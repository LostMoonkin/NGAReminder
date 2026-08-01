use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Context;
use open_lark::ws_client::{EventDispatcherHandler, EventHandler, LarkWsClient, WsClientError};
use serde::Deserialize;
use sqlx::Row;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    app::AppState,
    notification::sender::{FeishuConfig, openlark_config},
};

const MESSAGE_EVENT_TYPE: &str = "im.message.receive_v1";
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(2);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);
const CONFIG_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const MAX_LOGGED_MESSAGE_CHARS: usize = 2_000;

pub async fn run(state: AppState, cancellation: CancellationToken) -> anyhow::Result<()> {
    info!("Feishu long connection receiver started");
    let mut channel_updates = state.feishu_channel_updates.subscribe();
    let mut refresh = time::interval(CONFIG_REFRESH_INTERVAL);
    refresh.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut active = HashMap::new();

    reconcile_connections(&state, &cancellation, &mut active).await?;
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                stop_all_connections(&mut active).await;
                return Ok(());
            }
            changed = channel_updates.changed() => {
                if changed.is_err() {
                    stop_all_connections(&mut active).await;
                    return Ok(());
                }
                reconcile_connections(&state, &cancellation, &mut active).await?;
            }
            _ = refresh.tick() => {
                reconcile_connections(&state, &cancellation, &mut active).await?;
            }
        }
    }
}

async fn reconcile_connections(
    state: &AppState,
    cancellation: &CancellationToken,
    active: &mut HashMap<String, ActiveConnection>,
) -> anyhow::Result<()> {
    let desired = load_feishu_configs(state).await?;

    let stale_app_ids: Vec<String> = active
        .keys()
        .filter(|app_id| !desired.contains_key(*app_id))
        .cloned()
        .collect();
    for app_id in stale_app_ids {
        if let Some(connection) = active.remove(&app_id) {
            stop_connection(app_id, connection).await;
        }
    }

    for (app_id, config) in desired {
        let needs_restart = active.get(&app_id).is_some_and(|connection| {
            connection.config.app_secret != config.app_secret || connection.task.is_finished()
        });
        if needs_restart && let Some(connection) = active.remove(&app_id) {
            stop_connection(app_id.clone(), connection).await;
        }
        if !active.contains_key(&app_id) {
            start_connection(config, cancellation, active);
        }
    }

    if active.is_empty() {
        info!("Feishu long connection receiver waiting for an enabled Feishu channel");
    }
    Ok(())
}

async fn stop_all_connections(active: &mut HashMap<String, ActiveConnection>) {
    let connections = std::mem::take(active);
    for (app_id, connection) in connections {
        stop_connection(app_id, connection).await;
    }
}

async fn stop_connection(app_id: String, connection: ActiveConnection) {
    connection.cancellation.cancel();
    connection.task.abort();
    match connection.task.await {
        Ok(Ok(())) => info!(app_id = %app_id, "Feishu long connection stopped"),
        Ok(Err(error)) => {
            warn!(app_id = %app_id, error = %error, "Feishu long connection stopped with error")
        }
        Err(error) if !error.is_cancelled() => {
            warn!(app_id = %app_id, error = %error, "Feishu long connection task failed")
        }
        Err(_) => {}
    }
}

fn start_connection(
    config: FeishuConfig,
    cancellation: &CancellationToken,
    active: &mut HashMap<String, ActiveConnection>,
) {
    let app_id = config.app_id.clone();
    let child_cancellation = cancellation.child_token();
    let task = tokio::spawn(run_connection(config.clone(), child_cancellation.clone()));
    active.insert(
        app_id,
        ActiveConnection {
            config,
            cancellation: child_cancellation,
            task,
        },
    );
}

struct ActiveConnection {
    config: FeishuConfig,
    cancellation: CancellationToken,
    task: JoinHandle<anyhow::Result<()>>,
}

async fn load_feishu_configs(state: &AppState) -> anyhow::Result<HashMap<String, FeishuConfig>> {
    let rows = sqlx::query(
        "SELECT config_encrypted
         FROM notification_channels
         WHERE channel_type = 'feishu' AND enabled = 1",
    )
    .fetch_all(&state.pool)
    .await
    .context("failed to load Feishu receiver configurations")?;

    let mut configs: HashMap<String, FeishuConfig> = HashMap::new();
    for row in rows {
        let encrypted: Vec<u8> = row
            .try_get("config_encrypted")
            .context("failed to read Feishu channel configuration")?;
        let plaintext = match state.credential_cipher.decrypt(&encrypted) {
            Ok(value) => value,
            Err(_) => {
                warn!("skipping Feishu receiver configuration that cannot be decrypted");
                continue;
            }
        };
        let config: FeishuConfig = match serde_json::from_str(&plaintext) {
            Ok(value) => value,
            Err(error) => {
                warn!(error = %error, "skipping malformed Feishu receiver configuration");
                continue;
            }
        };
        if !config.is_valid() {
            warn!("skipping invalid Feishu receiver configuration");
            continue;
        }
        if let Some(existing) = configs.get(&config.app_id) {
            if existing.app_secret != config.app_secret {
                warn!(
                    app_id = %config.app_id,
                    "multiple Feishu channels use the same app_id with different secrets; using the first configuration"
                );
            }
        } else {
            configs.insert(config.app_id.clone(), config);
        }
    }

    Ok(configs)
}

async fn run_connection(
    config: FeishuConfig,
    cancellation: CancellationToken,
) -> anyhow::Result<()> {
    let mut reconnect_delay = INITIAL_RECONNECT_DELAY;
    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }

        info!(app_id = %config.app_id, "connecting Feishu long connection");
        let event_handler = EventDispatcherHandler::builder()
            .register_raw(MESSAGE_EVENT_TYPE, FeishuMessageEventHandler)
            .map_err(|error| {
                anyhow::anyhow!("failed to register Feishu message event handler: {error}")
            })?
            .build();
        let result = LarkWsClient::open(Arc::new(openlark_config(&config)), event_handler).await;

        match result {
            Ok(()) => info!(app_id = %config.app_id, "Feishu long connection ended"),
            Err(WsClientError::ConnectionClosed { reason }) => {
                info!(app_id = %config.app_id, ?reason, "Feishu long connection closed")
            }
            Err(error) => {
                warn!(app_id = %config.app_id, error = %error, "Feishu long connection failed")
            }
        }

        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            _ = time::sleep(reconnect_delay) => {}
        }
        reconnect_delay = (reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
    }
}

struct FeishuMessageEventHandler;

impl EventHandler for FeishuMessageEventHandler {
    fn handle(&self, payload: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let envelope: MessageEventEnvelope =
            serde_json::from_slice(payload).context("failed to parse Feishu message event")?;
        let text = if envelope.event.message.message_type == "text" {
            serde_json::from_str::<TextMessageContent>(&envelope.event.message.content)
                .map(|content| truncate_for_log(&content.text))
                .unwrap_or_else(|_| "<invalid text content>".to_owned())
        } else {
            format!("<{} message>", envelope.event.message.message_type)
        };

        info!(
            event_type = %envelope.header.event_type,
            message_id = ?envelope.event.message.message_id,
            chat_type = %envelope.event.message.chat_type,
            chat_id = ?envelope.event.message.chat_id,
            sender_open_id = ?envelope.event.sender.sender_id.open_id,
            text = %text,
            "received Feishu message"
        );
        Ok(())
    }
}

fn truncate_for_log(value: &str) -> String {
    let mut chars = value.chars();
    let result: String = chars.by_ref().take(MAX_LOGGED_MESSAGE_CHARS).collect();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}

#[derive(Debug, Deserialize)]
struct MessageEventEnvelope {
    header: EventHeader,
    event: MessageEvent,
}

#[derive(Debug, Deserialize)]
struct EventHeader {
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
}

#[derive(Debug, Deserialize)]
struct TextMessageContent {
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_log_text_by_characters() {
        let text = "你好".repeat(MAX_LOGGED_MESSAGE_CHARS);
        let truncated = truncate_for_log(&text);

        assert_eq!(
            truncated.chars().count(),
            MAX_LOGGED_MESSAGE_CHARS + "…".chars().count()
        );
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn parses_feishu_message_event_shape() {
        let payload = serde_json::json!({
            "header": {"event_type": MESSAGE_EVENT_TYPE},
            "event": {
                "sender": {"sender_id": {"open_id": "ou_test"}},
                "message": {
                    "message_id": "om_test",
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "chat_type": "group",
                    "chat_id": "oc_test"
                }
            }
        });
        let envelope: MessageEventEnvelope =
            serde_json::from_value(payload).expect("message event must parse");

        assert_eq!(envelope.header.event_type, MESSAGE_EVENT_TYPE);
        assert_eq!(
            envelope.event.message.message_id.as_deref(),
            Some("om_test")
        );
        assert_eq!(
            envelope.event.sender.sender_id.open_id.as_deref(),
            Some("ou_test")
        );
        let content: TextMessageContent =
            serde_json::from_str(&envelope.event.message.content).expect("text must parse");
        assert_eq!(content.text, "hello");
    }
}
