//! Platform bot adapters: each adapter owns one platform connection for one
//! integration, translates native events into `BotEvent`, and delivers
//! `BotOutboundMessage` replies.

#![allow(dead_code)]
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::bot::domain::{BotEvent, BotOutboundMessage, BotPlatform};

/// Bounded inbound queue shared by all adapters of a runtime. Adapters use
/// `try_send`; a full queue makes the platform redeliver.
#[derive(Clone)]
pub struct BotEventSink {
    sender: mpsc::Sender<BotEvent>,
}

impl BotEventSink {
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<BotEvent>) {
        let (sender, receiver) = mpsc::channel(capacity);
        (Self { sender }, receiver)
    }

    pub fn try_send(&self, event: BotEvent) -> Result<(), BotEventQueueFull> {
        self.sender.try_send(event).map_err(|_| BotEventQueueFull)
    }
}

#[derive(Debug)]
pub struct BotEventQueueFull;

#[derive(Debug, thiserror::Error)]
pub enum BotAdapterError {
    #[error("adapter connection failed")]
    Connection(#[source] anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum BotSendError {
    #[error("invalid outbound message payload")]
    InvalidPayload,
    #[error("platform reply/send failed")]
    Platform(String),
    #[error("image upload failed")]
    ImageUpload(String),
}

#[derive(Debug)]
pub struct BotDeliveryReceipt {
    pub platform_message_id: Option<String>,
    pub response_summary: String,
}

/// A single platform connection for a single integration.
#[async_trait]
pub trait BotAdapter: Send + Sync {
    fn integration_id(&self) -> &str;

    fn platform(&self) -> BotPlatform;

    /// Connect (with reconnect/backoff), translate native events and push them
    /// into the sink. Runs until cancelled.
    async fn run(
        &self,
        sink: BotEventSink,
        cancellation: CancellationToken,
    ) -> Result<(), BotAdapterError>;

    /// Deliver one outbound message (reply to a message or send to a chat).
    async fn deliver(
        &self,
        message: &BotOutboundMessage,
    ) -> Result<BotDeliveryReceipt, BotSendError>;
}
