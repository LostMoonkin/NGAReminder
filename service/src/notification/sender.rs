use std::{collections::HashMap, sync::OnceLock, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use open_lark::Config as OpenLarkConfig;
use open_lark::auth::AuthTokenProvider;
use open_lark::communication::im::v1::message::create::{CreateMessageBody, CreateMessageRequest};
use open_lark::communication::im::v1::message::models::ReceiveIdType;
use reqwest::{
    Client, StatusCode, Url,
    header::{CONTENT_LENGTH, CONTENT_TYPE},
    redirect,
};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::warn;

use crate::{
    markup,
    platform::feishu::FeishuImageUploader,
    platform::integration::{
        BarkCredentials, BarkTarget, FeishuCredentials, FeishuTarget, IntegrationCredentials,
        NotificationTarget, parse_stored_credentials, parse_stored_target,
    },
};

const FEISHU_API_BASE_URL: &str = "https://open.feishu.cn";
const FEISHU_MAX_CARD_IMAGES: usize = 3;
const FEISHU_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const FEISHU_MAX_CARD_TITLE: usize = 80;
const FEISHU_MAX_CARD_TEXT: usize = 2_000;

pub(crate) fn openlark_config(credentials: &FeishuCredentials) -> OpenLarkConfig {
    let base_config = OpenLarkConfig::builder()
        .app_id(credentials.app_id.clone())
        .app_secret(credentials.app_secret.clone())
        .base_url(FEISHU_API_BASE_URL)
        .build();
    base_config.with_token_provider(AuthTokenProvider::new(base_config.clone()))
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub url: String,
}

#[derive(Debug)]
pub struct DeliveryReceipt {
    pub http_status: u16,
    pub response_summary: String,
}

#[derive(Debug, Error)]
pub enum SendError {
    #[error("notification request failed")]
    Request(#[source] reqwest::Error),
    #[error("notification endpoint returned HTTP {status}")]
    Http { status: StatusCode, summary: String },
    #[error("notification API returned code {code}")]
    #[allow(dead_code)] // error-model contract; produced by future adapters
    Api { code: i64, summary: String },
    #[error("OpenLark request failed: {summary}")]
    OpenLark { summary: String },
    #[error("notification image processing failed: {summary}")]
    Image { summary: &'static str },
    #[error("invalid notification configuration")]
    InvalidConfig,
}

impl SendError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Request(_))
            || matches!(self, Self::Http { status, .. } if status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS)
            || matches!(self, Self::Api { code, .. } if *code == 99991400)
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Request(_) => "request_error",
            Self::Http { status, .. } if status.is_server_error() => "remote_5xx",
            Self::Http { status, .. } if *status == StatusCode::TOO_MANY_REQUESTS => "rate_limited",
            Self::Http { .. } => "remote_4xx",
            Self::Api { code, .. } if *code == 99991400 => "rate_limited",
            Self::Api { .. } => "remote_api_error",
            Self::OpenLark { .. } => "openlark_error",
            Self::Image { .. } => "image_error",
            Self::InvalidConfig => "invalid_config",
        }
    }
}

#[async_trait]
pub trait NotificationSender: Send + Sync {
    async fn send(&self, notification: &Notification) -> Result<DeliveryReceipt, SendError>;
}

/// Send a notification through a platform connection + notification target.
/// `credentials_json` is the decrypted `platform_integrations.credentials_encrypted`
/// (tagged `{"platform": ..., "credentials": {...}}`), `target_json` is the
/// decrypted `notification_channels.target_encrypted` (tagged
/// `{"platform": ..., "target": {...}}`).
pub async fn send_configured(
    platform: &str,
    credentials_json: &str,
    target_json: &str,
    notification: &Notification,
) -> Result<DeliveryReceipt, SendError> {
    match platform {
        "bark" => {
            let IntegrationCredentials::Bark(credentials) =
                parse_stored_credentials(credentials_json).map_err(|_| SendError::InvalidConfig)?
            else {
                return Err(SendError::InvalidConfig);
            };
            let NotificationTarget::Bark(target) =
                parse_stored_target(target_json).map_err(|_| SendError::InvalidConfig)?
            else {
                return Err(SendError::InvalidConfig);
            };
            let client = Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .map_err(SendError::Request)?;
            BarkSender::new(client, credentials, target)?
                .send(notification)
                .await
        }
        "feishu" => {
            let IntegrationCredentials::Feishu(credentials) =
                parse_stored_credentials(credentials_json).map_err(|_| SendError::InvalidConfig)?
            else {
                return Err(SendError::InvalidConfig);
            };
            let NotificationTarget::Feishu(target) =
                parse_stored_target(target_json).map_err(|_| SendError::InvalidConfig)?
            else {
                return Err(SendError::InvalidConfig);
            };
            FeishuSender::new(credentials, target)?
                .send(notification)
                .await
        }
        _ => Err(SendError::InvalidConfig),
    }
}

pub struct BarkSender {
    client: Client,
    credentials: BarkCredentials,
    target: BarkTarget,
}

impl BarkSender {
    pub fn new(
        client: Client,
        credentials: BarkCredentials,
        target: BarkTarget,
    ) -> Result<Self, SendError> {
        if target.device_key.is_empty() || !credentials.server_url.starts_with("http") {
            return Err(SendError::InvalidConfig);
        }
        Ok(Self {
            client,
            credentials,
            target,
        })
    }
}

#[async_trait]
impl NotificationSender for BarkSender {
    async fn send(&self, notification: &Notification) -> Result<DeliveryReceipt, SendError> {
        let endpoint = format!("{}/push", self.credentials.server_url.trim_end_matches('/'));
        let body = truncate_chars(&notification.body, 500);
        let response = self
            .client
            .post(endpoint)
            .json(&serde_json::json!({
                "device_key": self.target.device_key,
                "title": notification.title,
                "body": body,
                "group": self.credentials.group,
                "url": notification.url
            }))
            .send()
            .await
            .map_err(SendError::Request)?;
        receipt(response).await
    }
}

pub struct FeishuSender {
    image_client: Client,
    credentials: FeishuCredentials,
    target: FeishuTarget,
    openlark_config: OpenLarkConfig,
    image_uploader: FeishuImageUploader,
}

impl FeishuSender {
    pub fn new(credentials: FeishuCredentials, target: FeishuTarget) -> Result<Self, SendError> {
        if !credentials.app_id.starts_with("cli_")
            || credentials.app_secret.trim().is_empty()
            || target.receive_id.trim().is_empty()
            || !matches!(
                target.receive_id_type.as_str(),
                "chat_id" | "open_id" | "user_id" | "union_id" | "email"
            )
        {
            return Err(SendError::InvalidConfig);
        }
        let image_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(redirect::Policy::custom(|attempt| {
                if attempt.previous().len() < 5 && is_trusted_nga_image_url(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .build()
            .map_err(SendError::Request)?;
        let openlark_config = openlark_config(&credentials);
        let image_uploader =
            FeishuImageUploader::new(&credentials).map_err(|error| SendError::OpenLark {
                summary: summarize(&error.to_string()),
            })?;
        Ok(Self {
            image_client,
            credentials,
            target,
            openlark_config,
            image_uploader,
        })
    }

    async fn send_message(
        &self,
        notification: &Notification,
    ) -> Result<DeliveryReceipt, SendError> {
        let body = self.feishu_message_body(notification).await;
        let message = CreateMessageBody {
            receive_id: self.target.receive_id.clone(),
            msg_type: body["msg_type"]
                .as_str()
                .ok_or_else(|| SendError::OpenLark {
                    summary: "message body omitted msg_type".to_owned(),
                })?
                .to_owned(),
            content: body["content"]
                .as_str()
                .ok_or_else(|| SendError::OpenLark {
                    summary: "message body omitted content".to_owned(),
                })?
                .to_owned(),
            uuid: None,
        };
        let response = CreateMessageRequest::new(self.openlark_config.clone())
            .receive_id_type(receive_id_type(&self.target.receive_id_type)?)
            .execute(message)
            .await
            .map_err(|error| SendError::OpenLark {
                summary: summarize(&error.to_string()),
            })?;
        Ok(DeliveryReceipt {
            http_status: 200,
            response_summary: summarize(&response.to_string()),
        })
    }

    async fn feishu_message_body(&self, notification: &Notification) -> serde_json::Value {
        let parsed = parse_nga_images(&notification.body);
        let mut uploaded = Vec::new();
        let mut fallback = Vec::new();

        for (index, url) in parsed.image_urls.iter().enumerate() {
            if index >= FEISHU_MAX_CARD_IMAGES {
                fallback.push(url.clone());
                continue;
            }
            match self.upload_nga_image(url).await {
                Ok(image_key) => uploaded.push(image_key),
                Err(error) => {
                    warn!(
                        image_index = index + 1,
                        error_kind = error.kind(),
                        "Feishu image upload failed; using source link"
                    );
                    fallback.push(url.clone());
                }
            }
        }

        let text = render_feishu_text(&parsed.text);
        feishu_message_body(&self.target, notification, &text, &uploaded, &fallback)
    }

    async fn upload_nga_image(&self, value: &str) -> Result<String, SendError> {
        let url = Url::parse(value).map_err(|_| SendError::Image {
            summary: "invalid image URL",
        })?;
        if !is_trusted_nga_image_url(&url) {
            return Err(SendError::Image {
                summary: "untrusted image host",
            });
        }

        let cache_key = format!("{}\0{}", self.credentials.app_id, url.as_str());
        if let Some(image_key) = feishu_image_cache().lock().await.get(&cache_key).cloned() {
            return Ok(image_key);
        }

        let response = self
            .image_client
            .get(url)
            .send()
            .await
            .map_err(SendError::Request)?;
        if !response.status().is_success() {
            return Err(SendError::Image {
                summary: "image download returned non-success status",
            });
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|size| size > FEISHU_MAX_IMAGE_BYTES)
        {
            return Err(SendError::Image {
                summary: "image exceeds 10 MB",
            });
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .filter(|value| value.starts_with("image/"))
            .ok_or(SendError::Image {
                summary: "download is not an image",
            })?
            .to_owned();
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(SendError::Request)?;
            if bytes.len() + chunk.len() > FEISHU_MAX_IMAGE_BYTES {
                return Err(SendError::Image {
                    summary: "image exceeds 10 MB",
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Err(SendError::Image {
                summary: "image is empty",
            });
        }

        let image_key = self
            .image_uploader
            .upload_message_image(
                bytes,
                &content_type,
                &format!("nga-image.{}", content_type.trim_start_matches("image/")),
            )
            .await
            .map_err(|error| SendError::OpenLark {
                summary: summarize(&error.to_string()),
            })?;
        feishu_image_cache()
            .lock()
            .await
            .insert(cache_key, image_key.clone());
        Ok(image_key)
    }
}

#[async_trait]
impl NotificationSender for FeishuSender {
    async fn send(&self, notification: &Notification) -> Result<DeliveryReceipt, SendError> {
        self.send_message(notification).await
    }
}

fn receive_id_type(value: &str) -> Result<ReceiveIdType, SendError> {
    match value {
        "chat_id" => Ok(ReceiveIdType::ChatId),
        "open_id" => Ok(ReceiveIdType::OpenId),
        "user_id" => Ok(ReceiveIdType::UserId),
        "union_id" => Ok(ReceiveIdType::UnionId),
        "email" => Ok(ReceiveIdType::Email),
        _ => Err(SendError::InvalidConfig),
    }
}

fn feishu_image_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

struct ParsedImageBody {
    text: String,
    image_urls: Vec<String>,
}

fn parse_nga_images(body: &str) -> ParsedImageBody {
    let mut text = String::with_capacity(body.len());
    let mut image_urls = Vec::new();
    let mut remaining = body;

    while let Some(start) = remaining.find("[img]") {
        text.push_str(&remaining[..start]);
        let after_open = &remaining[start + "[img]".len()..];
        let Some(end) = after_open.find("[/img]") else {
            text.push_str(&remaining[start..]);
            remaining = "";
            break;
        };
        let url = after_open[..end].trim();
        if !url.is_empty() {
            image_urls.push(url.to_owned());
        }
        remaining = &after_open[end + "[/img]".len()..];
    }
    text.push_str(remaining);

    ParsedImageBody {
        text: text.trim().to_owned(),
        image_urls,
    }
}

fn render_feishu_text(value: &str) -> String {
    let rendered = markup::render_compact_markdown(value, &HashMap::new());
    truncate_notification_text(&rendered, FEISHU_MAX_CARD_TEXT)
}

fn is_trusted_nga_image_url(url: &Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    matches!(
        url.host_str(),
        Some("img.nga.cn" | "img.nga.178.com" | "img4.nga.178.com")
    )
}

fn feishu_message_body(
    target: &FeishuTarget,
    notification: &Notification,
    text: &str,
    image_keys: &[String],
    fallback_urls: &[String],
) -> serde_json::Value {
    let mut elements = Vec::new();
    if !text.is_empty() {
        elements.push(serde_json::json!({
            "tag": "div",
            "text": {"tag": "lark_md", "content": text}
        }));
    }
    for image_key in image_keys {
        elements.push(serde_json::json!({
            "tag": "img",
            "img_key": image_key,
            "alt": {"tag": "plain_text", "content": "NGA 帖子图片"}
        }));
    }
    if !fallback_urls.is_empty() {
        let links = fallback_urls
            .iter()
            .enumerate()
            .map(|(index, url)| format!("[图片 {}]({url})", index + 1))
            .collect::<Vec<_>>()
            .join(" · ");
        elements.push(serde_json::json!({
            "tag": "div",
            "text": {"tag": "lark_md", "content": links}
        }));
    }
    elements.push(serde_json::json!({
        "tag": "action",
        "actions": [
            {"tag": "button", "text": {"tag": "plain_text", "content": "查看帖子"},
             "url": notification.url, "type": "primary"}
        ]
    }));
    let card = serde_json::json!({
        "header": {
            "template": "blue",
            "title": {"tag": "plain_text", "content": truncate_chars(&notification.title, FEISHU_MAX_CARD_TITLE)}
        },
        "elements": elements
    });
    serde_json::json!({
        "receive_id": target.receive_id,
        "msg_type": "interactive",
        "content": serde_json::to_string(&card).expect("JSON value must serialize")
    })
}

async fn receipt(response: reqwest::Response) -> Result<DeliveryReceipt, SendError> {
    let status = response.status();
    let text = response.text().await.map_err(SendError::Request)?;
    let summary = summarize(&text);
    if !status.is_success() {
        return Err(SendError::Http { status, summary });
    }
    Ok(DeliveryReceipt {
        http_status: status.as_u16(),
        response_summary: summary,
    })
}

fn summarize(value: &str) -> String {
    value.chars().take(512).collect()
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn truncate_notification_text(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }

    let suffix = "\n\n内容较长，点击“查看帖子”查看完整内容。";
    let prefix_limit = limit.saturating_sub(suffix.chars().count() + 1);
    format!("{}…{suffix}", take_chars(value, prefix_limit))
}

fn take_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use reqwest::{StatusCode, Url};

    use super::{
        FEISHU_MAX_CARD_TEXT, FeishuTarget, Notification, SendError, feishu_message_body,
        is_trusted_nga_image_url, parse_nga_images, render_feishu_text, send_configured,
    };
    use crate::platform::integration::{BarkCredentials, BarkTarget, FeishuCredentials};

    #[test]
    fn feishu_defaults_to_group_delivery() {
        let target: FeishuTarget =
            serde_json::from_str(r#"{"receive_id":"oc_test"}"#).expect("target must parse");
        assert_eq!(target.receive_id_type, "chat_id");
        let credentials: FeishuCredentials =
            serde_json::from_str(r#"{"app_id":"cli_test","app_secret":"secret"}"#)
                .expect("credentials must parse");
        assert_eq!(credentials.app_id, "cli_test");
    }

    #[tokio::test]
    async fn configured_sender_accepts_the_persisted_tagged_format() {
        let error = send_configured(
            "feishu",
            r#"{"platform":"feishu","credentials":{"app_id":"cli_test","app_secret":"secret"}}"#,
            r#"{"platform":"feishu","target":{"receive_id":"oc_test"}}"#,
            &Notification {
                title: "title".to_owned(),
                body: "body".to_owned(),
                url: "https://bbs.nga.cn/".to_owned(),
            },
        )
        .await
        .expect_err("send must fail without a live endpoint");
        // A valid config reaches the network layer (connection refused /
        // timeout), not InvalidConfig.
        assert!(!matches!(error, SendError::InvalidConfig));
    }

    #[tokio::test]
    async fn configured_sender_rejects_legacy_bare_and_cross_platform_values() {
        let notification = Notification {
            title: "title".to_owned(),
            body: "body".to_owned(),
            url: "https://bbs.nga.cn/".to_owned(),
        };
        let bare = send_configured(
            "feishu",
            r#"{"app_id":"cli_test","app_secret":"secret"}"#,
            r#"{"receive_id":"oc_test"}"#,
            &notification,
        )
        .await;
        assert!(matches!(bare, Err(SendError::InvalidConfig)));

        let mismatch = send_configured(
            "feishu",
            r#"{"platform":"bark","credentials":{"server_url":"https://api.day.app","group":"nga"}}"#,
            r#"{"platform":"feishu","target":{"receive_id":"oc_test"}}"#,
            &notification,
        )
        .await;
        assert!(matches!(mismatch, Err(SendError::InvalidConfig)));
    }

    #[test]
    fn feishu_card_is_encoded_as_content_string() {
        let target = FeishuTarget {
            receive_id_type: "chat_id".to_owned(),
            receive_id: "oc_test".to_owned(),
        };
        let body = feishu_message_body(
            &target,
            &Notification {
                title: "title".to_owned(),
                body: "body".to_owned(),
                url: "https://bbs.nga.cn/".to_owned(),
            },
            "body",
            &["img_v3_test".to_owned()],
            &["https://img.nga.cn/fallback.jpg".to_owned()],
        );
        assert_eq!(body["receive_id"], "oc_test");
        assert_eq!(body["msg_type"], "interactive");
        let content: serde_json::Value =
            serde_json::from_str(body["content"].as_str().expect("content must be string"))
                .expect("content must contain JSON");
        assert_eq!(content["header"]["title"]["content"], "title");
        assert_eq!(content["elements"][1]["tag"], "img");
        assert_eq!(content["elements"][1]["img_key"], "img_v3_test");
        assert!(
            content["elements"][2]["text"]["content"]
                .as_str()
                .expect("fallback must be text")
                .contains("fallback.jpg")
        );
    }

    #[test]
    fn nga_image_tags_are_extracted_without_leaking_markup() {
        let parsed = parse_nga_images(
            "before [img]https://img.nga.cn/a.jpg[/img] middle \
             [img]https://img4.nga.178.com/b.webp[/img] after",
        );
        assert_eq!(parsed.text, "before  middle  after");
        assert_eq!(
            parsed.image_urls,
            [
                "https://img.nga.cn/a.jpg",
                "https://img4.nga.178.com/b.webp"
            ]
        );
    }

    #[test]
    fn feishu_text_uses_export_markup_and_keeps_forum_emoticons() {
        let text = render_feishu_text("[b]重要[/b][code]\n无意义的围栏\n[/code]正文[s:ac:瞎]");
        assert!(text.contains("**重要**"));
        assert!(text.contains("无意义的围栏"));
        assert!(text.contains("正文[s:ac:瞎]"));
        assert!(!text.contains("```"));
    }

    #[test]
    fn feishu_text_adds_notice_when_content_is_too_long() {
        let text = render_feishu_text(&"正文".repeat(2_000));
        assert!(text.chars().count() <= FEISHU_MAX_CARD_TEXT);
        assert!(text.contains("点击“查看帖子”查看完整内容"));
    }

    #[test]
    fn only_https_nga_image_hosts_are_trusted() {
        assert!(is_trusted_nga_image_url(
            &Url::parse("https://img.nga.cn/a.jpg").expect("URL must parse")
        ));
        assert!(!is_trusted_nga_image_url(
            &Url::parse("http://img.nga.cn/a.jpg").expect("URL must parse")
        ));
        assert!(!is_trusted_nga_image_url(
            &Url::parse("https://img.nga.cn.example.test/a.jpg").expect("URL must parse")
        ));
    }

    #[test]
    fn only_transient_http_and_api_errors_are_retryable() {
        assert!(
            SendError::Http {
                status: StatusCode::TOO_MANY_REQUESTS,
                summary: String::new()
            }
            .retryable()
        );
        assert!(
            SendError::Http {
                status: StatusCode::BAD_GATEWAY,
                summary: String::new()
            }
            .retryable()
        );
        assert!(
            SendError::Api {
                code: 99991400,
                summary: String::new()
            }
            .retryable()
        );
        assert!(
            !SendError::Http {
                status: StatusCode::BAD_REQUEST,
                summary: String::new()
            }
            .retryable()
        );
    }

    #[test]
    fn bark_credentials_and_target_split() {
        let credentials: BarkCredentials =
            serde_json::from_str(r#"{"server_url":"https://api.day.app","group":"NGA Reminder"}"#)
                .expect("credentials must parse");
        let target: BarkTarget =
            serde_json::from_str(r#"{"device_key":"device"}"#).expect("target must parse");
        assert_eq!(credentials.server_url, "https://api.day.app");
        assert_eq!(target.device_key, "device");
    }
}
