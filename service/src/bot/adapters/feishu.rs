//! Feishu bot adapter: one long connection per bot-enabled integration,
//! translating `im.message.receive_v1` into `BotEvent` and delivering replies
//! via the reply / create message APIs.

use std::sync::Arc;
use std::time::Duration;

use ::time::OffsetDateTime;
use anyhow::Context;
use async_trait::async_trait;
use openlark_client::ws_client::{
    EventDispatcherHandler, EventHandler, LarkWsClient, WsClientError,
};
use openlark_communication::im::v1::message::create::{CreateMessageBody, CreateMessageRequest};
use openlark_communication::im::v1::message::models::ReceiveIdType;
use openlark_communication::im::v1::message::reply::{ReplyMessageBody, ReplyMessageRequest};
use openlark_core::config::Config as OpenLarkConfig;
use serde::Deserialize;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    bot::adapter::{BotAdapter, BotDeliveryReceipt, BotEventSink, BotSendError},
    bot::domain::{
        BotEvent, BotMessageKind, BotOutboundMessage, BotPlatform, ImagePayload, TextPayload,
    },
    bot::parser::strip_self_mention,
    notification::sender::openlark_config,
    platform::feishu::FeishuImageUploader,
    platform::integration::{ConversationType, FeishuCredentials},
};

const MESSAGE_EVENT_TYPE: &str = "im.message.receive_v1";
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(2);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);

pub struct FeishuAdapter {
    integration_id: String,
    credentials: FeishuCredentials,
}

impl FeishuAdapter {
    pub fn new(integration_id: String, credentials: FeishuCredentials) -> Self {
        Self {
            integration_id,
            credentials,
        }
    }
}

#[async_trait]
impl BotAdapter for FeishuAdapter {
    fn integration_id(&self) -> &str {
        &self.integration_id
    }

    fn platform(&self) -> BotPlatform {
        BotPlatform::Feishu
    }

    async fn run(
        &self,
        sink: BotEventSink,
        cancellation: CancellationToken,
    ) -> Result<(), crate::bot::adapter::BotAdapterError> {
        let mut reconnect_delay = INITIAL_RECONNECT_DELAY;
        loop {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            info!(
                integration_id = %self.integration_id,
                app_id = %self.credentials.app_id,
                "connecting Feishu long connection"
            );
            let event_handler = EventDispatcherHandler::builder()
                .register_raw(
                    MESSAGE_EVENT_TYPE,
                    FeishuMessageEventHandler {
                        integration_id: self.integration_id.clone(),
                        sink: sink.clone(),
                    },
                )
                .map_err(|error| {
                    crate::bot::adapter::BotAdapterError::Connection(anyhow::anyhow!(
                        "failed to register Feishu message event handler: {error}"
                    ))
                })?
                .build();
            let openlark = openlark_config(&self.credentials);
            let result = LarkWsClient::open(Arc::new(openlark), event_handler).await;

            match result {
                Ok(()) => info!(
                    integration_id = %self.integration_id,
                    "Feishu long connection ended"
                ),
                Err(WsClientError::ConnectionClosed { reason }) => {
                    info!(
                        integration_id = %self.integration_id,
                        ?reason,
                        "Feishu long connection closed"
                    )
                }
                Err(error) => {
                    warn!(
                        integration_id = %self.integration_id,
                        error = %error,
                        "Feishu long connection failed"
                    )
                }
            }

            tokio::select! {
                _ = cancellation.cancelled() => return Ok(()),
                _ = time::sleep(reconnect_delay) => {}
            }
            reconnect_delay = (reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
        }
    }

    async fn deliver(
        &self,
        message: &BotOutboundMessage,
    ) -> Result<BotDeliveryReceipt, BotSendError> {
        if message.integration_id != self.integration_id {
            return Err(BotSendError::Platform("integration mismatch".to_owned()));
        }
        let payload_json =
            std::str::from_utf8(&message.payload).map_err(|_| BotSendError::InvalidPayload)?;
        let config = openlark_config(&self.credentials);
        let uuid = Uuid::new_v5(&Uuid::NAMESPACE_URL, message.dedupe_key.as_bytes()).to_string();
        match message.message_kind {
            BotMessageKind::Text => {
                let payload: TextPayload =
                    serde_json::from_str(payload_json).map_err(|_| BotSendError::InvalidPayload)?;
                send_text(&config, message, &uuid, &payload.text).await
            }
            BotMessageKind::Image => {
                let payload: ImagePayload =
                    serde_json::from_str(payload_json).map_err(|_| BotSendError::InvalidPayload)?;
                send_image(&config, &self.credentials, message, &uuid, payload).await
            }
            BotMessageKind::Card => Err(BotSendError::InvalidPayload),
        }
    }
}

async fn send_text(
    config: &OpenLarkConfig,
    message: &BotOutboundMessage,
    uuid: &str,
    text: &str,
) -> Result<BotDeliveryReceipt, BotSendError> {
    let content = serde_json::json!({ "text": text }).to_string();
    let response = if let Some(reply_to) = &message.reply_to_message_id {
        let body = ReplyMessageBody {
            content,
            msg_type: "text".to_owned(),
            reply_in_thread: Some(false),
            uuid: Some(uuid.to_owned()),
        };
        ReplyMessageRequest::new(config.clone())
            .message_id(reply_to.clone())
            .execute(body)
            .await
            .map_err(|error| BotSendError::Platform(error.to_string()))?
    } else {
        let body = CreateMessageBody {
            receive_id: message.conversation_id.clone(),
            msg_type: "text".to_owned(),
            content,
            uuid: Some(uuid.to_owned()),
        };
        CreateMessageRequest::new(config.clone())
            .receive_id_type(ReceiveIdType::ChatId)
            .execute(body)
            .await
            .map_err(|error| BotSendError::Platform(error.to_string()))?
    };
    let message_id = response
        .get("data")
        .and_then(|data| data.get("message_id"))
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    Ok(BotDeliveryReceipt {
        platform_message_id: message_id,
        response_summary: summarize(&response.to_string()),
    })
}

async fn send_image(
    config: &OpenLarkConfig,
    credentials: &FeishuCredentials,
    message: &BotOutboundMessage,
    uuid: &str,
    payload: ImagePayload,
) -> Result<BotDeliveryReceipt, BotSendError> {
    let file_name = format!("captcha.{}", mime_ext(&payload.mime_type));
    let uploader = FeishuImageUploader::new(credentials)
        .map_err(|error| BotSendError::ImageUpload(error.to_string()))?;
    let image_key = uploader
        .upload_message_image(payload.bytes, &payload.mime_type, &file_name)
        .await
        .map_err(|error| BotSendError::ImageUpload(error.to_string()))?;
    let content = serde_json::json!({ "image_key": image_key }).to_string();
    let response = if let Some(reply_to) = &message.reply_to_message_id {
        let body = ReplyMessageBody {
            content,
            msg_type: "image".to_owned(),
            reply_in_thread: Some(false),
            uuid: Some(uuid.to_owned()),
        };
        ReplyMessageRequest::new(config.clone())
            .message_id(reply_to.clone())
            .execute(body)
            .await
            .map_err(|error| BotSendError::Platform(error.to_string()))?
    } else {
        let body = CreateMessageBody {
            receive_id: message.conversation_id.clone(),
            msg_type: "image".to_owned(),
            content,
            uuid: Some(uuid.to_owned()),
        };
        CreateMessageRequest::new(config.clone())
            .receive_id_type(ReceiveIdType::ChatId)
            .execute(body)
            .await
            .map_err(|error| BotSendError::Platform(error.to_string()))?
    };
    Ok(BotDeliveryReceipt {
        platform_message_id: response
            .get("data")
            .and_then(|data| data.get("message_id"))
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        response_summary: summarize(&response.to_string()),
    })
}

fn mime_ext(mime: &str) -> &str {
    match mime {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/jpeg" => "jpg",
        _ => "jpg",
    }
}

fn summarize(value: &str) -> String {
    value.chars().take(256).collect()
}

// ---- event parsing --------------------------------------------------------

struct FeishuMessageEventHandler {
    integration_id: String,
    sink: BotEventSink,
}

impl EventHandler for FeishuMessageEventHandler {
    fn handle(&self, payload: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let envelope: MessageEventEnvelope =
            serde_json::from_slice(payload).context("failed to parse Feishu message event")?;
        if envelope.header.event_type != MESSAGE_EVENT_TYPE {
            return Ok(());
        }
        // Only user-sent text messages; ignore bot messages to avoid loops.
        if envelope.event.sender.sender_type.as_deref() != Some("user") {
            return Ok(());
        }
        if envelope.event.message.message_type != "text" {
            return Ok(());
        }
        let Some(actor_id) = envelope.event.sender.sender_id.open_id.clone() else {
            return Ok(());
        };
        let Some(message_id) = envelope.event.message.message_id.clone() else {
            return Ok(());
        };
        let Some(conversation_id) = envelope.event.message.chat_id.clone() else {
            return Ok(());
        };
        let content: TextMessageContent = serde_json::from_str(&envelope.event.message.content)
            .context("failed to parse Feishu text content")?;
        let conversation_type = match envelope.event.message.chat_type.as_str() {
            "p2p" => ConversationType::Private,
            "group" => ConversationType::Group,
            _ => ConversationType::Group,
        };

        // Group commands must start with a real Feishu mention placeholder.
        // Group delivery is scoped to @bot events; the payload itself does
        // not reliably identify which mentioned ID belongs to the app.
        let leading_mention =
            envelope
                .event
                .message
                .mentions
                .iter()
                .enumerate()
                .find_map(|(index, mention)| {
                    mention
                        .leading_token(&content.text)
                        .map(|token| (index, token))
                });
        if conversation_type == ConversationType::Group && leading_mention.is_none() {
            return Ok(());
        }
        let text = strip_self_mention(
            &content.text,
            leading_mention.as_ref().map(|(_, token)| token.as_str()),
        );
        if !text.starts_with('/') {
            return Ok(());
        }

        let occurred_at = envelope
            .header
            .create_time
            .parse::<i64>()
            .ok()
            .and_then(|millis| {
                OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000).ok()
            })
            .unwrap_or_else(OffsetDateTime::now_utc);

        let event = BotEvent {
            integration_id: self.integration_id.clone(),
            platform: BotPlatform::Feishu,
            platform_event_id: Some(envelope.header.event_id.clone()),
            platform_message_id: message_id,
            actor_id,
            actor_display_name: None,
            conversation_id,
            conversation_type,
            text,
            mentions: envelope
                .event
                .message
                .mentions
                .iter()
                .enumerate()
                .map(|(index, mention)| {
                    mention.to_bot_mention(
                        leading_mention
                            .as_ref()
                            .is_some_and(|(own, _)| *own == index),
                    )
                })
                .collect(),
            occurred_at,
        };
        self.sink
            .try_send(event)
            .map_err(|_| "bot inbound queue full; ask the platform to redeliver".into())
    }
}

#[derive(Debug, Deserialize)]
struct MessageEventEnvelope {
    header: EventHeader,
    event: MessageEvent,
}

#[derive(Debug, Deserialize)]
struct EventHeader {
    event_id: String,
    #[serde(default)]
    create_time: String,
    #[serde(default)]
    event_type: String,
}

#[derive(Debug, Deserialize)]
struct MessageEvent {
    sender: Sender,
    message: Message,
}

#[derive(Debug, Deserialize)]
struct Sender {
    sender_id: SenderId,
    #[serde(default)]
    sender_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SenderId {
    open_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Message {
    message_id: Option<String>,
    message_type: String,
    content: String,
    chat_type: String,
    chat_id: Option<String>,
    #[serde(default)]
    mentions: Vec<Mention>,
}

#[derive(Debug, Deserialize)]
struct Mention {
    key: Option<String>,
    id: Option<MentionId>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MentionId {
    open_id: Option<String>,
    user_id: Option<String>,
    union_id: Option<String>,
}

impl Mention {
    fn leading_token(&self, text: &str) -> Option<String> {
        let trimmed = text.trim_start();
        if let Some(key) = self.key.as_deref().filter(|key| trimmed.starts_with(key)) {
            return Some(key.to_owned());
        }
        self.name
            .as_deref()
            .map(|name| format!("@{name}"))
            .filter(|token| trimmed.starts_with(token))
    }

    fn to_bot_mention(&self, is_self: bool) -> crate::bot::domain::BotMention {
        let id = self
            .id
            .as_ref()
            .and_then(|id| {
                id.open_id
                    .clone()
                    .or_else(|| id.user_id.clone())
                    .or_else(|| id.union_id.clone())
            })
            .or_else(|| self.key.clone())
            .unwrap_or_default();
        crate::bot::domain::BotMention {
            id,
            name: self.name.clone().unwrap_or_default(),
            is_self,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TextMessageContent {
    text: String,
}

#[cfg(test)]
mod tests {
    use super::{Mention, MessageEventEnvelope, TextMessageContent};

    #[test]
    fn parses_feishu_message_event_shape() {
        let payload = serde_json::json!({
            "header": {
                "event_id": "evt_test",
                "create_time": "1700000000",
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": {"sender_id": {"open_id": "ou_test"}, "sender_type": "user"},
                "message": {
                    "message_id": "om_test",
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "chat_type": "p2p",
                    "chat_id": "oc_test",
                    "mentions": []
                }
            }
        });
        let envelope: MessageEventEnvelope =
            serde_json::from_value(payload).expect("message event must parse");
        assert_eq!(envelope.header.event_type, "im.message.receive_v1");
        assert_eq!(envelope.header.event_id, "evt_test");
        let content: TextMessageContent =
            serde_json::from_str(&envelope.event.message.content).expect("text must parse");
        assert_eq!(content.text, "hello");
    }

    #[test]
    fn group_mention_uses_real_feishu_placeholder_shape() {
        let mention: Mention = serde_json::from_value(serde_json::json!({
            "key": "@_user_1",
            "id": {"open_id": "ou_bot"},
            "name": "NGA Bot"
        }))
        .expect("mention must parse");
        assert_eq!(
            mention.leading_token("  @_user_1 /status").as_deref(),
            Some("@_user_1")
        );
        let normalized = mention.to_bot_mention(true);
        assert_eq!(normalized.id, "ou_bot");
        assert!(normalized.is_self);
    }
}
