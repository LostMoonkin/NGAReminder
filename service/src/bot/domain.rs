//! Standardized bot domain types. Platform adapters translate their native
//! events into these types; command handlers never see platform-specific
//! structures.

#![allow(dead_code)]
use time::OffsetDateTime;

use crate::platform::integration::{BotRole, ConversationType};

/// Platforms a bot adapter can exist on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotPlatform {
    Feishu,
    Telegram,
    Qq,
}

impl BotPlatform {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Feishu => "feishu",
            Self::Telegram => "telegram",
            Self::Qq => "qq",
        }
    }
}

/// A normalized inbound event from a platform adapter.
#[derive(Clone, Debug)]
pub struct BotEvent {
    pub integration_id: String,
    pub platform: BotPlatform,
    pub platform_event_id: Option<String>,
    /// Idempotency key; a repeated message with the same id is dropped.
    pub platform_message_id: String,
    pub actor_id: String,
    pub actor_display_name: Option<String>,
    pub conversation_id: String,
    pub conversation_type: ConversationType,
    pub text: String,
    pub mentions: Vec<BotMention>,
    pub occurred_at: OffsetDateTime,
}

#[derive(Clone, Debug)]
pub struct BotMention {
    pub id: String,
    pub name: String,
    pub is_self: bool,
}

/// Outbound message queued in `bot_outbox` and delivered by the adapter.
#[derive(Clone, Debug)]
pub struct BotOutboundMessage {
    pub integration_id: String,
    pub conversation_id: String,
    pub reply_to_message_id: Option<String>,
    pub message_kind: BotMessageKind,
    /// Encrypted JSON payload; the adapter decrypts and interprets it.
    pub payload: Vec<u8>,
    /// Stable dedupe key derived by the producer.
    pub dedupe_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotMessageKind {
    Text,
    Image,
    Card,
}

impl BotMessageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Card => "card",
        }
    }
}

/// Plain text payload for text replies.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TextPayload {
    pub text: String,
}

/// Image payload for captcha images (short TTL, owner-only private chat).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ImagePayload {
    pub mime_type: String,
    /// Raw image bytes (captcha). Never persisted in logs or audit.
    pub bytes: Vec<u8>,
}

/// A command declaration used for routing, authorization and help output.
#[derive(Clone, Debug)]
pub struct CommandDescriptor {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub min_role: BotRole,
    pub private_only: bool,
    pub has_side_effects: bool,
    pub usage: &'static str,
    pub help: &'static str,
}

/// Successful command outcome; replies are enqueued to the bot outbox.
#[derive(Clone, Debug, Default)]
pub struct CommandResult {
    pub replies: Vec<String>,
    /// When true the inbound event is marked failed instead of succeeded
    /// (used for transient errors that should still be audit-visible).
    pub failed: bool,
    pub error_kind: Option<String>,
}
