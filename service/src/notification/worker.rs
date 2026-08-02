use sqlx::Row;
use uuid::Uuid;

use crate::{
    app::AppState,
    bot::adapter::{BotAdapter, BotSendError},
    bot::adapters::FeishuAdapter,
    bot::domain::{BotMessageKind, BotOutboundMessage},
    config::DatabaseBackend,
    notification::alerts,
    notification::sender::{Notification, SendError, send_configured},
    platform::integration::FeishuCredentials,
};

const BOT_MAX_ATTEMPTS: i32 = 3;

pub async fn process_one(state: &AppState) -> anyhow::Result<bool> {
    if process_bot_one(state).await? {
        return Ok(true);
    }
    if process_system_alert_one(state).await? {
        return Ok(true);
    }
    process_post_one(state).await
}

async fn process_bot_one(state: &AppState) -> anyhow::Result<bool> {
    // Expired rows are dropped without delivery and their payload cleared.
    let clear_payload = match state.config.database_backend {
        DatabaseBackend::Postgres => "'\\x00'::bytea",
        DatabaseBackend::Sqlite => "X'00'",
    };
    let expired = format!(
        "UPDATE bot_outbox SET status = 'dead', payload_encrypted = {clear_payload},
         last_error_kind = 'expired', delivered_at = CURRENT_TIMESTAMP
         WHERE status IN ('pending', 'failed')
           AND expires_at IS NOT NULL AND expires_at <= CURRENT_TIMESTAMP"
    );
    sqlx::query(&expired).execute(&state.pool).await?;

    let lease = match state.config.database_backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '2 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+2 minutes')",
    };
    let claim = format!(
        "UPDATE bot_outbox SET status = 'sending',
         attempt_count = attempt_count + 1, lease_until = {lease}
         WHERE id = (
           SELECT o.id FROM bot_outbox o
           WHERE o.status IN ('pending', 'failed')
             AND o.next_attempt_at <= CURRENT_TIMESTAMP
             AND (o.expires_at IS NULL OR o.expires_at > CURRENT_TIMESTAMP)
             AND (o.lease_until IS NULL OR o.lease_until <= CURRENT_TIMESTAMP)
           ORDER BY o.next_attempt_at, o.created_at LIMIT 1
         )
         AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
         RETURNING id"
    );
    let Some(row) = sqlx::query(&claim).fetch_optional(&state.pool).await? else {
        return Ok(false);
    };
    let outbox_id: String = row.get("id");
    let row = sqlx::query(
        "SELECT o.attempt_count, o.integration_id, o.conversation_id,
                o.reply_to_message_id, o.message_kind, o.payload_encrypted,
                o.dedupe_key, o.inbound_event_id, i.platform, i.credentials_encrypted
         FROM bot_outbox o
         JOIN platform_integrations i ON i.id = o.integration_id
         WHERE o.id = $1",
    )
    .bind(&outbox_id)
    .fetch_one(&state.pool)
    .await?;
    let attempt: i32 = row.get("attempt_count");
    let integration_id: String = row.get("integration_id");
    let conversation_id: String = row.get("conversation_id");
    let reply_to_message_id: Option<String> = row.get("reply_to_message_id");
    let message_kind = match row.get::<String, _>("message_kind").as_str() {
        "text" => BotMessageKind::Text,
        "image" => BotMessageKind::Image,
        "card" => BotMessageKind::Card,
        _ => BotMessageKind::Text,
    };
    let encrypted: Vec<u8> = row.get("payload_encrypted");
    let payload = state
        .credential_cipher
        .decrypt(&encrypted)
        .map_err(|_| anyhow::anyhow!("bot outbox payload decryption failed"))?;
    let credentials_encrypted: Vec<u8> = row.get("credentials_encrypted");
    let credentials_json = state
        .credential_cipher
        .decrypt(&credentials_encrypted)
        .map_err(|_| anyhow::anyhow!("integration credentials decryption failed"))?;
    let dedupe_key: String = row.get("dedupe_key");
    let inbound_event_id: Option<String> = row.get("inbound_event_id");
    let platform: String = row.get("platform");

    let message = BotOutboundMessage {
        integration_id: integration_id.clone(),
        conversation_id,
        reply_to_message_id,
        message_kind,
        payload: payload.into_bytes(),
        dedupe_key: dedupe_key.clone(),
    };

    let delivery = match platform.as_str() {
        "feishu" => {
            let credentials: FeishuCredentials = serde_json::from_str(&credentials_json)
                .map_err(|_| anyhow::anyhow!("invalid Feishu credentials"))?;
            let adapter = FeishuAdapter::new(integration_id.clone(), credentials);
            adapter.deliver(&message).await
        }
        other => {
            tracing::warn!(
                platform = other,
                "no bot adapter for platform; dropping message"
            );
            Err(BotSendError::Platform("unsupported_platform".to_owned()))
        }
    };

    match delivery {
        Ok(receipt) => {
            sqlx::query(
                "UPDATE bot_outbox SET status = 'delivered', lease_until = NULL,
                 delivered_at = CURRENT_TIMESTAMP, last_error_kind = NULL,
                 payload_encrypted = $2 WHERE id = $1",
            )
            .bind(&outbox_id)
            .bind(Vec::<u8>::new())
            .execute(&state.pool)
            .await?;
            if let Some(event_id) = inbound_event_id {
                sqlx::query("UPDATE bot_inbound_events SET status = 'succeeded' WHERE id = $1")
                    .bind(event_id)
                    .execute(&state.pool)
                    .await?;
            }
            let _ = receipt;
        }
        Err(error) => {
            let retryable = bot_error_retryable(&error) && attempt < BOT_MAX_ATTEMPTS;
            let kind = bot_error_kind(&error);
            let next = match state.config.database_backend {
                DatabaseBackend::Postgres => {
                    "CURRENT_TIMESTAMP + (CASE attempt_count WHEN 1 THEN 30 WHEN 2 THEN 120 ELSE 600 END * INTERVAL '1 second')"
                }
                DatabaseBackend::Sqlite => {
                    "datetime(CURRENT_TIMESTAMP, '+' || CASE attempt_count WHEN 1 THEN 30 WHEN 2 THEN 120 ELSE 600 END || ' seconds')"
                }
            };
            let clear_payload = match state.config.database_backend {
                DatabaseBackend::Postgres => "'\\x00'::bytea",
                DatabaseBackend::Sqlite => "X'00'",
            };
            let query = format!(
                "UPDATE bot_outbox SET status = $1, lease_until = NULL,
                 next_attempt_at = {next}, last_error_kind = $2,
                 payload_encrypted = CASE WHEN $1 = 'dead' THEN {clear_payload} ELSE payload_encrypted END
                 WHERE id = $3"
            );
            sqlx::query(&query)
                .bind(if retryable { "failed" } else { "dead" })
                .bind(kind)
                .bind(&outbox_id)
                .execute(&state.pool)
                .await?;
        }
    }
    Ok(true)
}

fn bot_error_retryable(error: &BotSendError) -> bool {
    !matches!(error, BotSendError::InvalidPayload)
}

fn bot_error_kind(error: &BotSendError) -> &'static str {
    match error {
        BotSendError::InvalidPayload => "invalid_payload",
        BotSendError::Platform(_) => "platform_error",
        BotSendError::ImageUpload(_) => "image_upload_error",
    }
}

async fn process_system_alert_one(state: &AppState) -> anyhow::Result<bool> {
    alerts::enqueue_open_alert_channels(state).await?;
    let lease = match state.config.database_backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '2 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+2 minutes')",
    };
    let claim = format!(
        "UPDATE system_alert_outbox SET status = 'sending',
         attempt_count = attempt_count + 1, lease_until = {lease}
         WHERE id = (
           SELECT o.id FROM system_alert_outbox o
           JOIN system_alerts a ON a.id = o.alert_id
           JOIN notification_channels c ON c.id = o.channel_id
           JOIN platform_integrations i ON i.id = c.integration_id
           WHERE a.resolved_at IS NULL AND c.enabled = 1
             AND i.enabled = 1 AND i.delivery_enabled = 1
             AND o.status IN ('pending', 'failed')
             AND o.next_attempt_at <= CURRENT_TIMESTAMP
             AND (o.lease_until IS NULL OR o.lease_until <= CURRENT_TIMESTAMP)
           ORDER BY o.next_attempt_at, o.created_at LIMIT 1
         )
         AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
         RETURNING id"
    );
    let Some(row) = sqlx::query(&claim).fetch_optional(&state.pool).await? else {
        return Ok(false);
    };
    let outbox_id: String = row.get("id");
    let row = sqlx::query(
        "SELECT o.attempt_count, c.target_encrypted, i.platform, i.credentials_encrypted,
                a.title, a.body, a.url
         FROM system_alert_outbox o
         JOIN system_alerts a ON a.id = o.alert_id
         JOIN notification_channels c ON c.id = o.channel_id
         JOIN platform_integrations i ON i.id = c.integration_id
         WHERE o.id = $1",
    )
    .bind(&outbox_id)
    .fetch_one(&state.pool)
    .await?;
    let attempt: i32 = row.get("attempt_count");
    let encrypted: Vec<u8> = row.get("credentials_encrypted");
    let credentials = state
        .credential_cipher
        .decrypt(&encrypted)
        .map_err(|_| anyhow::anyhow!("integration credentials decryption failed"))?;
    let target_encrypted: Vec<u8> = row.get("target_encrypted");
    let target = state
        .credential_cipher
        .decrypt(&target_encrypted)
        .map_err(|_| anyhow::anyhow!("notification target decryption failed"))?;
    let notification = Notification {
        title: row.get("title"),
        body: row.get("body"),
        url: row.get("url"),
    };
    let platform: String = row.get("platform");
    match send_configured(&platform, &credentials, &target, &notification).await {
        Ok(receipt) => {
            record_system_alert_delivery(
                state,
                &outbox_id,
                attempt,
                true,
                Some(receipt.http_status as i32),
                Some(&receipt.response_summary),
                None,
            )
            .await?;
            sqlx::query(
                "UPDATE system_alert_outbox SET status = 'delivered', lease_until = NULL,
                 delivered_at = CURRENT_TIMESTAMP, last_error_kind = NULL WHERE id = $1",
            )
            .bind(&outbox_id)
            .execute(&state.pool)
            .await?;
        }
        Err(error) => {
            let retryable = error.retryable() && attempt < 5;
            let (status, summary) = error_details(&error);
            record_system_alert_delivery(
                state,
                &outbox_id,
                attempt,
                false,
                status,
                summary,
                Some(error.kind()),
            )
            .await?;
            let next = match state.config.database_backend {
                DatabaseBackend::Postgres => {
                    "CURRENT_TIMESTAMP + (CASE attempt_count WHEN 1 THEN 30 WHEN 2 THEN 120 WHEN 3 THEN 600 ELSE 1800 END * INTERVAL '1 second')"
                }
                DatabaseBackend::Sqlite => {
                    "datetime(CURRENT_TIMESTAMP, '+' || CASE attempt_count WHEN 1 THEN 30 WHEN 2 THEN 120 WHEN 3 THEN 600 ELSE 1800 END || ' seconds')"
                }
            };
            let query = format!(
                "UPDATE system_alert_outbox SET status = $1, lease_until = NULL,
                 next_attempt_at = {next}, last_error_kind = $2 WHERE id = $3"
            );
            sqlx::query(&query)
                .bind(if retryable { "failed" } else { "dead" })
                .bind(error.kind())
                .bind(&outbox_id)
                .execute(&state.pool)
                .await?;
        }
    }
    Ok(true)
}

async fn process_post_one(state: &AppState) -> anyhow::Result<bool> {
    let lease = match state.config.database_backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '2 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+2 minutes')",
    };
    let claim = format!(
        "UPDATE notification_outbox SET status = 'sending',
         attempt_count = attempt_count + 1, lease_until = {lease}
         WHERE id = (
           SELECT o.id FROM notification_outbox o
           JOIN notification_channels c ON c.id = o.channel_id
           JOIN platform_integrations i ON i.id = c.integration_id
           WHERE c.enabled = 1 AND i.enabled = 1 AND i.delivery_enabled = 1
             AND o.status IN ('pending', 'failed')
             AND next_attempt_at <= CURRENT_TIMESTAMP
             AND (o.lease_until IS NULL OR o.lease_until <= CURRENT_TIMESTAMP)
           ORDER BY o.next_attempt_at, o.created_at LIMIT 1
         )
         AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
         RETURNING id"
    );
    let Some(row) = sqlx::query(&claim).fetch_optional(&state.pool).await? else {
        return Ok(false);
    };
    let outbox_id: String = row.get("id");
    let row = sqlx::query(
        "SELECT o.attempt_count, c.target_encrypted, i.platform, i.credentials_encrypted,
         p.tid, p.pid, p.floor_number, p.page_number, p.author_name, p.content_raw, t.title
         FROM notification_outbox o
         JOIN notification_channels c ON c.id = o.channel_id
         JOIN platform_integrations i ON i.id = c.integration_id
         JOIN post_events e ON e.id = o.post_event_id
         JOIN posts p ON p.id = e.post_id
         JOIN threads t ON t.tid = p.tid
         WHERE o.id = $1",
    )
    .bind(&outbox_id)
    .fetch_one(&state.pool)
    .await?;
    let attempt: i32 = row.get("attempt_count");
    let encrypted: Vec<u8> = row.get("credentials_encrypted");
    let credentials = state
        .credential_cipher
        .decrypt(&encrypted)
        .map_err(|_| anyhow::anyhow!("integration credentials decryption failed"))?;
    let target_encrypted: Vec<u8> = row.get("target_encrypted");
    let target = state
        .credential_cipher
        .decrypt(&target_encrypted)
        .map_err(|_| anyhow::anyhow!("notification target decryption failed"))?;
    let tid: i64 = row.get("tid");
    let pid: Option<i64> = row.get("pid");
    let page: i32 = row.get("page_number");
    let author: String = row.get("author_name");
    let content: String = row.get("content_raw");
    let title: String = row.get("title");
    let floor: Option<i32> = row.get("floor_number");
    let notification = Notification {
        title,
        body: format!("{} · #{}\n\n{}", author, floor.unwrap_or_default(), content),
        url: post_url(tid, page, pid),
    };
    let platform: String = row.get("platform");
    match send_configured(&platform, &credentials, &target, &notification).await {
        Ok(receipt) => {
            record_delivery(
                state,
                &outbox_id,
                attempt,
                true,
                Some(receipt.http_status as i32),
                Some(&receipt.response_summary),
                None,
            )
            .await?;
            sqlx::query(
                "UPDATE notification_outbox SET status = 'delivered', lease_until = NULL,
                 delivered_at = CURRENT_TIMESTAMP, last_error_kind = NULL WHERE id = $1",
            )
            .bind(&outbox_id)
            .execute(&state.pool)
            .await?;
        }
        Err(error) => {
            let retryable = error.retryable() && attempt < 5;
            let (status, summary) = error_details(&error);
            record_delivery(
                state,
                &outbox_id,
                attempt,
                false,
                status,
                summary,
                Some(error.kind()),
            )
            .await?;
            let next = match state.config.database_backend {
                DatabaseBackend::Postgres => {
                    "CURRENT_TIMESTAMP + (CASE attempt_count WHEN 1 THEN 30 WHEN 2 THEN 120 WHEN 3 THEN 600 ELSE 1800 END * INTERVAL '1 second')"
                }
                DatabaseBackend::Sqlite => {
                    "datetime(CURRENT_TIMESTAMP, '+' || CASE attempt_count WHEN 1 THEN 30 WHEN 2 THEN 120 WHEN 3 THEN 600 ELSE 1800 END || ' seconds')"
                }
            };
            let query = format!(
                "UPDATE notification_outbox SET status = $1, lease_until = NULL,
                 next_attempt_at = {next}, last_error_kind = $2 WHERE id = $3"
            );
            sqlx::query(&query)
                .bind(if retryable { "failed" } else { "dead" })
                .bind(error.kind())
                .bind(&outbox_id)
                .execute(&state.pool)
                .await?;
        }
    }
    Ok(true)
}

async fn record_system_alert_delivery(
    state: &AppState,
    outbox_id: &str,
    attempt: i32,
    success: bool,
    http_status: Option<i32>,
    summary: Option<&str>,
    error_kind: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO system_alert_deliveries
         (id, outbox_id, attempt, success, http_status, response_summary, error_kind)
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(outbox_id)
    .bind(attempt)
    .bind(i32::from(success))
    .bind(http_status)
    .bind(summary)
    .bind(error_kind)
    .execute(&state.pool)
    .await?;
    Ok(())
}

async fn record_delivery(
    state: &AppState,
    outbox_id: &str,
    attempt: i32,
    success: bool,
    http_status: Option<i32>,
    summary: Option<&str>,
    error_kind: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO notification_deliveries
         (id, outbox_id, attempt, success, http_status, response_summary, error_kind)
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(outbox_id)
    .bind(attempt)
    .bind(i32::from(success))
    .bind(http_status)
    .bind(summary)
    .bind(error_kind)
    .execute(&state.pool)
    .await?;
    Ok(())
}

fn error_details(error: &SendError) -> (Option<i32>, Option<&str>) {
    match error {
        SendError::Http { status, summary } => (Some(status.as_u16() as i32), Some(summary)),
        SendError::Api { summary, .. } => (None, Some(summary)),
        _ => (None, None),
    }
}

fn post_url(tid: i64, page: i32, pid: Option<i64>) -> String {
    match pid {
        Some(pid) => {
            format!(
                "https://bbs.nga.cn/read.php?tid={tid}&page={}#pid{pid}Anchor",
                page.max(1)
            )
        }
        None => format!("https://bbs.nga.cn/read.php?tid={tid}"),
    }
}

#[cfg(test)]
mod tests {
    use super::post_url;

    #[test]
    fn reply_url_opens_thread_page_at_post_anchor() {
        assert_eq!(
            post_url(47_264_819, 3, Some(876_581_704)),
            "https://bbs.nga.cn/read.php?tid=47264819&page=3#pid876581704Anchor"
        );
    }

    #[test]
    fn topic_url_opens_thread_without_reply_anchor() {
        assert_eq!(
            post_url(47_264_819, 1, None),
            "https://bbs.nga.cn/read.php?tid=47264819"
        );
    }
}
