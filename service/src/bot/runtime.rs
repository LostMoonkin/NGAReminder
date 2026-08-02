//! Bot runtime: reconciles bot-enabled integrations, runs one adapter task
//! per connection, and feeds normalized events into a bounded queue consumed
//! by the command dispatcher.

use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use sqlx::Row;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    app::AppState,
    bot::adapter::{BotAdapter, BotEventSink},
    bot::adapters::FeishuAdapter,
    bot::dispatcher,
    platform::integration::{FeishuCredentials, PlatformKind},
};

const CONFIG_REFRESH_INTERVAL: time::Duration = time::Duration::from_secs(30);
const INBOUND_QUEUE_CAPACITY: usize = 1024;

/// Start the bot runtime: connection reconciliation plus the inbound queue
/// consumer. Returns when cancelled.
pub async fn run(state: AppState, cancellation: CancellationToken) -> anyhow::Result<()> {
    let (sink, receiver) = BotEventSink::new(INBOUND_QUEUE_CAPACITY);
    let mut platform_updates = state.platform_updates.subscribe();
    let mut refresh = time::interval(CONFIG_REFRESH_INTERVAL);
    refresh.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut active: HashMap<String, ActiveAdapter> = HashMap::new();

    let consumer = tokio::spawn(consume(state.clone(), receiver));

    reconcile(&state, &sink, &cancellation, &mut active).await?;
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                stop_all(&mut active).await;
                consumer.abort();
                return Ok(());
            }
            changed = platform_updates.changed() => {
                if changed.is_err() {
                    stop_all(&mut active).await;
                    consumer.abort();
                    return Ok(());
                }
                reconcile(&state, &sink, &cancellation, &mut active).await?;
            }
            _ = refresh.tick() => {
                reconcile(&state, &sink, &cancellation, &mut active).await?;
            }
        }
    }
}

async fn consume(state: AppState, mut receiver: mpsc::Receiver<crate::bot::domain::BotEvent>) {
    while let Some(event) = receiver.recv().await {
        if let Err(error) = dispatcher::dispatch(&state, &event).await {
            warn!(
                integration_id = %event.integration_id,
                error = %error,
                "bot inbound dispatch failed"
            );
        }
    }
}

struct ActiveAdapter {
    credentials_hash: String,
    cancellation: CancellationToken,
    task: JoinHandle<anyhow::Result<()>>,
}

async fn reconcile(
    state: &AppState,
    sink: &BotEventSink,
    cancellation: &CancellationToken,
    active: &mut HashMap<String, ActiveAdapter>,
) -> anyhow::Result<()> {
    let desired = load_bot_integrations(state).await?;

    let stale: Vec<String> = active
        .keys()
        .filter(|id| !desired.contains_key(*id))
        .cloned()
        .collect();
    for id in stale {
        if let Some(connection) = active.remove(&id) {
            stop_connection(&id, connection).await;
        }
    }

    for (integration_id, credentials) in desired {
        let needs_restart = active.get(&integration_id).is_some_and(|connection| {
            connection.credentials_hash != credentials_hash(&credentials)
                || connection.task.is_finished()
        });
        if needs_restart && let Some(connection) = active.remove(&integration_id) {
            stop_connection(&integration_id, connection).await;
        }
        if !active.contains_key(&integration_id) {
            // Connection state is best-effort bookkeeping; ignore DB failures.
            let _ = crate::platform::integration::mark_connection_state(
                state,
                &integration_id,
                "connecting",
                None,
            )
            .await;
            start_connection(sink, integration_id, credentials, cancellation, active);
        }
    }

    if active.is_empty() {
        info!("bot runtime waiting for a bot-enabled integration");
    }
    Ok(())
}

async fn stop_all(active: &mut HashMap<String, ActiveAdapter>) {
    let connections = std::mem::take(active);
    for (id, connection) in connections {
        stop_connection(&id, connection).await;
    }
}

async fn stop_connection(integration_id: &str, connection: ActiveAdapter) {
    connection.cancellation.cancel();
    connection.task.abort();
    match connection.task.await {
        Ok(Ok(())) => info!(integration_id = %integration_id, "bot connection stopped"),
        Ok(Err(error)) => {
            warn!(integration_id = %integration_id, error = %error, "bot connection stopped with error")
        }
        Err(error) if !error.is_cancelled() => {
            warn!(integration_id = %integration_id, error = %error, "bot connection task failed")
        }
        Err(_) => {}
    }
}

fn start_connection(
    sink: &BotEventSink,
    integration_id: String,
    credentials: FeishuCredentials,
    cancellation: &CancellationToken,
    active: &mut HashMap<String, ActiveAdapter>,
) {
    let adapter: Arc<dyn BotAdapter> = Arc::new(FeishuAdapter::new(
        integration_id.clone(),
        credentials.clone(),
    ));
    let child_cancellation = cancellation.child_token();
    let task = tokio::spawn(run_adapter(
        adapter,
        sink.clone(),
        child_cancellation.clone(),
    ));
    active.insert(
        integration_id,
        ActiveAdapter {
            credentials_hash: credentials_hash(&credentials),
            cancellation: child_cancellation,
            task,
        },
    );
}

async fn run_adapter(
    adapter: Arc<dyn BotAdapter>,
    sink: BotEventSink,
    cancellation: CancellationToken,
) -> anyhow::Result<()> {
    let result = adapter.run(sink, cancellation).await;
    match result {
        Ok(()) => Ok(()),
        Err(error) => Err(anyhow::anyhow!("bot adapter ended: {error}")),
    }
}

async fn load_bot_integrations(
    state: &AppState,
) -> anyhow::Result<HashMap<String, FeishuCredentials>> {
    let rows = sqlx::query(
        "SELECT id, platform, credentials_encrypted
         FROM platform_integrations
         WHERE bot_enabled = 1 AND enabled = 1",
    )
    .fetch_all(&state.pool)
    .await
    .context("failed to load bot-enabled integrations")?;
    let mut integrations = HashMap::new();
    for row in rows {
        let platform: String = row.get("platform");
        let kind = match PlatformKind::parse(&platform) {
            Some(kind) => kind,
            None => continue,
        };
        if kind != PlatformKind::Feishu {
            // Telegram/QQ adapters arrive later; skip their connections so a
            // config cannot silently half-start.
            continue;
        }
        let id: String = row.get("id");
        let encrypted: Vec<u8> = row.get("credentials_encrypted");
        let plaintext = match state.credential_cipher.decrypt(&encrypted) {
            Ok(value) => value,
            Err(_) => {
                warn!(integration_id = %id, "skipping bot integration that cannot be decrypted");
                continue;
            }
        };
        let credentials = match serde_json::from_str::<FeishuCredentials>(&plaintext) {
            Ok(value) => value,
            Err(error) => {
                warn!(integration_id = %id, error = %error, "skipping malformed bot integration credentials");
                continue;
            }
        };
        integrations.insert(id, credentials);
    }
    Ok(integrations)
}

fn credentials_hash(credentials: &FeishuCredentials) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!(
        "{}\0{}",
        credentials.app_id, credentials.app_secret
    ));
    format!("{digest:x}")
}
