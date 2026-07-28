use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{
    Client, StatusCode, Url,
    header::{CONTENT_LENGTH, CONTENT_TYPE},
    multipart::{Form, Part},
    redirect,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::warn;

const FEISHU_API_BASE_URL: &str = "https://open.feishu.cn";
const FEISHU_TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60);
const FEISHU_MAX_CARD_IMAGES: usize = 3;
const FEISHU_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BarkConfig {
    #[serde(default = "default_bark_server")]
    pub server_url: String,
    pub device_key: String,
    #[serde(default = "default_group")]
    pub group: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default = "default_feishu_receive_id_type")]
    pub receive_id_type: String,
    pub receive_id: String,
}

impl FeishuConfig {
    pub fn is_valid(&self) -> bool {
        self.app_id.starts_with("cli_")
            && !self.app_secret.trim().is_empty()
            && !self.receive_id.trim().is_empty()
            && matches!(
                self.receive_id_type.as_str(),
                "chat_id" | "open_id" | "user_id" | "union_id" | "email"
            )
    }
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
    Api { code: i64, summary: String },
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
            Self::Image { .. } => "image_error",
            Self::InvalidConfig => "invalid_config",
        }
    }
}

#[async_trait]
pub trait NotificationSender: Send + Sync {
    async fn send(&self, notification: &Notification) -> Result<DeliveryReceipt, SendError>;
}

pub async fn send_configured(
    channel_type: &str,
    config_json: &str,
    notification: &Notification,
) -> Result<DeliveryReceipt, SendError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(SendError::Request)?;
    match channel_type {
        "bark" => {
            let config = serde_json::from_str(config_json).map_err(|_| SendError::InvalidConfig)?;
            BarkSender::new(client, config)?.send(notification).await
        }
        "feishu" => {
            let config = serde_json::from_str(config_json).map_err(|_| SendError::InvalidConfig)?;
            FeishuSender::new(client, config)?.send(notification).await
        }
        _ => Err(SendError::InvalidConfig),
    }
}

pub struct BarkSender {
    client: Client,
    config: BarkConfig,
}

impl BarkSender {
    pub fn new(client: Client, config: BarkConfig) -> Result<Self, SendError> {
        if config.device_key.is_empty() || !config.server_url.starts_with("http") {
            return Err(SendError::InvalidConfig);
        }
        Ok(Self { client, config })
    }
}

#[async_trait]
impl NotificationSender for BarkSender {
    async fn send(&self, notification: &Notification) -> Result<DeliveryReceipt, SendError> {
        let endpoint = format!("{}/push", self.config.server_url.trim_end_matches('/'));
        let body = truncate_chars(&notification.body, 500);
        let response = self
            .client
            .post(endpoint)
            .json(&serde_json::json!({
                "device_key": self.config.device_key,
                "title": notification.title,
                "body": body,
                "group": self.config.group,
                "url": notification.url
            }))
            .send()
            .await
            .map_err(SendError::Request)?;
        receipt(response).await
    }
}

pub struct FeishuSender {
    client: Client,
    image_client: Client,
    config: FeishuConfig,
}

impl FeishuSender {
    pub fn new(client: Client, config: FeishuConfig) -> Result<Self, SendError> {
        if !config.is_valid() {
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
        Ok(Self {
            client,
            image_client,
            config,
        })
    }

    async fn access_token(&self) -> Result<String, SendError> {
        let cache = feishu_token_cache();
        let mut cache = cache.lock().await;
        if let Some(cached) = cache.get(&self.config.app_id)
            && cached.expires_at > Instant::now() + FEISHU_TOKEN_REFRESH_MARGIN
        {
            return Ok(cached.value.clone());
        }

        let response = self
            .client
            .post(format!(
                "{FEISHU_API_BASE_URL}/open-apis/auth/v3/tenant_access_token/internal"
            ))
            .json(&serde_json::json!({
                "app_id": self.config.app_id,
                "app_secret": self.config.app_secret
            }))
            .send()
            .await
            .map_err(SendError::Request)?;
        let status = response.status();
        let text = response.text().await.map_err(SendError::Request)?;
        if !status.is_success() {
            return Err(SendError::Http {
                status,
                summary: summarize(&text),
            });
        }
        let token_response: FeishuTokenResponse =
            serde_json::from_str(&text).map_err(|_| SendError::Api {
                code: -1,
                summary: "invalid token response".to_owned(),
            })?;
        if token_response.code != 0 {
            return Err(SendError::Api {
                code: token_response.code,
                summary: summarize(&token_response.msg),
            });
        }
        let token = token_response
            .tenant_access_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| SendError::Api {
                code: -1,
                summary: "token response omitted tenant_access_token".to_owned(),
            })?;
        let ttl = Duration::from_secs(token_response.expire.unwrap_or(7200).max(1));
        cache.insert(
            self.config.app_id.clone(),
            CachedFeishuToken {
                value: token.clone(),
                expires_at: Instant::now() + ttl,
            },
        );
        Ok(token)
    }

    async fn invalidate_token(&self, rejected_token: &str) {
        let mut cache = feishu_token_cache().lock().await;
        if cache
            .get(&self.config.app_id)
            .is_some_and(|cached| cached.value == rejected_token)
        {
            cache.remove(&self.config.app_id);
        }
    }

    async fn send_with_token(
        &self,
        notification: &Notification,
        token: &str,
    ) -> Result<DeliveryReceipt, SendError> {
        let body = self.feishu_message_body(notification, token).await;
        let response = self
            .client
            .post(format!("{FEISHU_API_BASE_URL}/open-apis/im/v1/messages"))
            .query(&[("receive_id_type", &self.config.receive_id_type)])
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(SendError::Request)?;
        feishu_receipt(response).await
    }

    async fn feishu_message_body(
        &self,
        notification: &Notification,
        token: &str,
    ) -> serde_json::Value {
        let parsed = parse_nga_images(&notification.body);
        let mut uploaded = Vec::new();
        let mut fallback = Vec::new();

        for (index, url) in parsed.image_urls.iter().enumerate() {
            if index >= FEISHU_MAX_CARD_IMAGES {
                fallback.push(url.clone());
                continue;
            }
            match self.upload_nga_image(url, token).await {
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

        let text = truncate_chars(&parsed.text, 3_000);
        feishu_message_body(&self.config, notification, &text, &uploaded, &fallback)
    }

    async fn upload_nga_image(&self, value: &str, token: &str) -> Result<String, SendError> {
        let url = Url::parse(value).map_err(|_| SendError::Image {
            summary: "invalid image URL",
        })?;
        if !is_trusted_nga_image_url(&url) {
            return Err(SendError::Image {
                summary: "untrusted image host",
            });
        }

        let cache_key = format!("{}\0{}", self.config.app_id, url.as_str());
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

        let part = Part::bytes(bytes)
            .file_name("nga-image")
            .mime_str(&content_type)
            .map_err(SendError::Request)?;
        let response = self
            .client
            .post(format!("{FEISHU_API_BASE_URL}/open-apis/im/v1/images"))
            .bearer_auth(token)
            .multipart(
                Form::new()
                    .text("image_type", "message")
                    .part("image", part),
            )
            .send()
            .await
            .map_err(SendError::Request)?;
        let status = response.status();
        let text = response.text().await.map_err(SendError::Request)?;
        if !status.is_success() {
            return Err(SendError::Http {
                status,
                summary: summarize(&text),
            });
        }
        let upload: FeishuImageResponse =
            serde_json::from_str(&text).map_err(|_| SendError::Api {
                code: -1,
                summary: "invalid image upload response".to_owned(),
            })?;
        if upload.code != 0 {
            return Err(SendError::Api {
                code: upload.code,
                summary: summarize(&upload.msg),
            });
        }
        let image_key = upload
            .data
            .and_then(|data| data.image_key)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SendError::Api {
                code: -1,
                summary: "image upload response omitted image_key".to_owned(),
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
        let token = self.access_token().await?;
        match self.send_with_token(notification, &token).await {
            Err(SendError::Api { .. })
            | Err(SendError::Http {
                status: StatusCode::UNAUTHORIZED,
                ..
            }) => {
                self.invalidate_token(&token).await;
                invalidate_feishu_images(&self.config.app_id).await;
                let refreshed = self.access_token().await?;
                self.send_with_token(notification, &refreshed).await
            }
            result => result,
        }
    }
}

#[derive(Deserialize)]
struct FeishuTokenResponse {
    code: i64,
    #[serde(default)]
    msg: String,
    tenant_access_token: Option<String>,
    expire: Option<u64>,
}

#[derive(Deserialize)]
struct FeishuApiResponse {
    code: i64,
    #[serde(default)]
    msg: String,
}

#[derive(Deserialize)]
struct FeishuImageResponse {
    code: i64,
    #[serde(default)]
    msg: String,
    data: Option<FeishuImageData>,
}

#[derive(Deserialize)]
struct FeishuImageData {
    image_key: Option<String>,
}

struct CachedFeishuToken {
    value: String,
    expires_at: Instant,
}

fn feishu_token_cache() -> &'static Mutex<HashMap<String, CachedFeishuToken>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedFeishuToken>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn feishu_image_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn invalidate_feishu_images(app_id: &str) {
    let prefix = format!("{app_id}\0");
    feishu_image_cache()
        .lock()
        .await
        .retain(|key, _| !key.starts_with(&prefix));
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
    config: &FeishuConfig,
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
            "title": {"tag": "plain_text", "content": notification.title}
        },
        "elements": elements
    });
    serde_json::json!({
        "receive_id": config.receive_id,
        "msg_type": "interactive",
        "content": serde_json::to_string(&card).expect("JSON value must serialize")
    })
}

async fn feishu_receipt(response: reqwest::Response) -> Result<DeliveryReceipt, SendError> {
    let status = response.status();
    let text = response.text().await.map_err(SendError::Request)?;
    let summary = summarize(&text);
    if !status.is_success() {
        return Err(SendError::Http { status, summary });
    }
    let api_response: FeishuApiResponse =
        serde_json::from_str(&text).map_err(|_| SendError::Api {
            code: -1,
            summary: "invalid message response".to_owned(),
        })?;
    if api_response.code != 0 {
        return Err(SendError::Api {
            code: api_response.code,
            summary: summarize(&api_response.msg),
        });
    }
    Ok(DeliveryReceipt {
        http_status: status.as_u16(),
        response_summary: summary,
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

fn default_bark_server() -> String {
    "https://api.day.app".to_owned()
}

fn default_group() -> String {
    "NGA Reminder".to_owned()
}

fn default_feishu_receive_id_type() -> String {
    "chat_id".to_owned()
}

#[cfg(test)]
mod tests {
    use reqwest::{StatusCode, Url};

    use super::{
        BarkConfig, FeishuConfig, Notification, SendError, feishu_message_body,
        is_trusted_nga_image_url, parse_nga_images, send_configured,
    };

    #[test]
    fn bark_defaults_are_stable() {
        let config: BarkConfig =
            serde_json::from_str(r#"{"device_key":"test"}"#).expect("config must parse");
        assert_eq!(config.server_url, "https://api.day.app");
        assert_eq!(config.group, "NGA Reminder");
    }

    #[test]
    fn feishu_defaults_to_group_delivery() {
        let config: FeishuConfig = serde_json::from_str(
            r#"{"app_id":"cli_test","app_secret":"secret","receive_id":"oc_test"}"#,
        )
        .expect("config must parse");
        assert_eq!(config.receive_id_type, "chat_id");
        assert!(config.is_valid());
    }

    #[test]
    fn old_feishu_webhook_config_is_rejected() {
        assert!(
            serde_json::from_str::<FeishuConfig>(
                r#"{"webhook_url":"https://example.test/hook","secret":"secret"}"#
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn configured_sender_reports_old_webhook_as_invalid_config() {
        let error = send_configured(
            "feishu",
            r#"{"webhook_url":"https://example.test/hook","secret":"secret"}"#,
            &Notification {
                title: "title".to_owned(),
                body: "body".to_owned(),
                url: "https://bbs.nga.cn/".to_owned(),
            },
        )
        .await
        .expect_err("old webhook config must fail");
        assert!(matches!(error, SendError::InvalidConfig));
    }

    #[test]
    fn feishu_card_is_encoded_as_content_string() {
        let config = FeishuConfig {
            app_id: "cli_test".to_owned(),
            app_secret: "secret".to_owned(),
            receive_id_type: "chat_id".to_owned(),
            receive_id: "oc_test".to_owned(),
        };
        let body = feishu_message_body(
            &config,
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
}
