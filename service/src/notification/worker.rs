use sqlx::Row;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::AppState,
    bot::{
        adapter::{BotAdapter, BotSendError},
        adapters::FeishuAdapter,
        domain::{BotMessageKind, BotOutboundMessage},
        outbox,
        session::{self, LoginSessionStatus},
    },
    config::DatabaseBackend,
    notification::alerts,
    notification::sender::{Notification, SendError, send_configured},
    platform::integration::{IntegrationCredentials, parse_stored_credentials},
};

const BOT_MAX_ATTEMPTS: i32 = 3;
const CREDENTIAL_DECRYPTION_FAILED: &str = "credential_decryption_failed";
const TARGET_DECRYPTION_FAILED: &str = "target_decryption_failed";

#[cfg(test)]
pub async fn process_one(state: &AppState) -> anyhow::Result<bool> {
    if process_bot_one(state).await? {
        return Ok(true);
    }
    if process_system_alert_one(state).await? {
        return Ok(true);
    }
    process_post_one(state).await
}

/// Process at most one row from each independent notification class.
///
/// `process_one` intentionally keeps its historical priority order for direct
/// callers. The scheduler uses this fair batch so a continuously replenished
/// bot reply queue cannot starve system alerts or post notifications.
pub async fn process_fair_batch(state: &AppState) -> anyhow::Result<usize> {
    let mut processed = 0;
    if process_bot_one(state).await? {
        processed += 1;
    }
    if process_system_alert_one(state).await? {
        processed += 1;
    }
    if process_post_one(state).await? {
        processed += 1;
    }
    Ok(processed)
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
         WHERE expires_at IS NOT NULL AND expires_at <= CURRENT_TIMESTAMP
           AND (status IN ('pending', 'failed')
                OR (status = 'sending' AND lease_until <= CURRENT_TIMESTAMP))"
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
           WHERE o.status IN ('pending', 'failed', 'sending')
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
    let payload = match state.credential_cipher.decrypt(&encrypted) {
        Ok(payload) => payload,
        Err(_) => {
            fail_claimed_bot(
                state,
                &outbox_id,
                attempt,
                "payload_decryption_failed",
                false,
            )
            .await?;
            return Ok(true);
        }
    };
    let credentials_encrypted: Vec<u8> = row.get("credentials_encrypted");
    let credentials_json = match state.credential_cipher.decrypt(&credentials_encrypted) {
        Ok(credentials) => credentials,
        Err(_) => {
            fail_claimed_bot(
                state,
                &outbox_id,
                attempt,
                "credential_decryption_failed",
                true,
            )
            .await?;
            return Ok(true);
        }
    };
    let dedupe_key: String = row.get("dedupe_key");
    let inbound_event_id: Option<String> = row.get("inbound_event_id");
    let platform: String = row.get("platform");

    let message = BotOutboundMessage {
        integration_id: integration_id.clone(),
        conversation_id: conversation_id.clone(),
        reply_to_message_id,
        message_kind,
        payload: payload.into_bytes(),
        dedupe_key: dedupe_key.clone(),
    };

    let delivery = match platform.as_str() {
        "feishu" => {
            let credentials = match parse_stored_credentials(&credentials_json) {
                Ok(IntegrationCredentials::Feishu(credentials)) => credentials,
                Ok(_) | Err(_) => {
                    fail_claimed_bot(state, &outbox_id, attempt, "invalid_credentials", true)
                        .await?;
                    return Ok(true);
                }
            };
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
            if message_kind == BotMessageKind::Image {
                // Renew the exact attempt before creating its idempotent
                // follow-up. A reclaimed or cancelled attempt cannot create
                // session side effects.
                if !renew_claimed_bot(state, &outbox_id, attempt).await? {
                    return Ok(true);
                }
                enqueue_captcha_instruction(state, &integration_id, &conversation_id, &dedupe_key)
                    .await?;
            }
            let completed = sqlx::query(
                "UPDATE bot_outbox SET status = 'delivered', lease_until = NULL,
                 delivered_at = CURRENT_TIMESTAMP, last_error_kind = NULL,
                 payload_encrypted = $2
                 WHERE id = $1 AND status = 'sending' AND attempt_count = $3",
            )
            .bind(&outbox_id)
            .bind(Vec::<u8>::new())
            .bind(attempt)
            .execute(&state.pool)
            .await?
            .rows_affected()
                == 1;
            if completed && let Some(event_id) = inbound_event_id {
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
            warn!(
                outbox_id = %outbox_id,
                integration_id = %integration_id,
                message_kind = message_kind.as_str(),
                attempt,
                retryable,
                error_kind = kind,
                error_detail = %bot_error_detail(&error),
                "bot message delivery failed"
            );
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
            let completed = sqlx::query(&format!(
                "{query} AND status = 'sending' AND attempt_count = $4"
            ))
            .bind(if retryable { "failed" } else { "dead" })
            .bind(kind)
            .bind(&outbox_id)
            .bind(attempt)
            .execute(&state.pool)
            .await?
            .rows_affected()
                == 1;
            if completed && !retryable && message_kind == BotMessageKind::Image {
                fail_captcha_image_session(state, &integration_id, &conversation_id, &dedupe_key)
                    .await?;
            }
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

fn bot_error_detail(error: &BotSendError) -> String {
    let detail = match error {
        BotSendError::InvalidPayload => "invalid outbound message payload",
        BotSendError::Platform(detail) | BotSendError::ImageUpload(detail) => detail,
    };
    detail
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect()
}

fn captcha_session_id(dedupe_key: &str) -> Option<&str> {
    let value = dedupe_key.strip_prefix("login:")?;
    let (session_id, revision) = value.split_once(":captcha:")?;
    if session_id.is_empty() || revision.parse::<u32>().is_err() {
        return None;
    }
    Some(session_id)
}

async fn enqueue_captcha_instruction(
    state: &AppState,
    integration_id: &str,
    conversation_id: &str,
    image_dedupe_key: &str,
) -> Result<(), sqlx::Error> {
    let Some(session_id) = captcha_session_id(image_dedupe_key) else {
        return Ok(());
    };
    let Some(login_session) = session::get_session(state, session_id).await? else {
        return Ok(());
    };
    if login_session.status != LoginSessionStatus::AwaitingCaptcha {
        return Ok(());
    }
    let text =
        format!("验证码图片已发送。请在 10 分钟内回复：\n`/login captcha {session_id} <验证码>`");
    outbox::enqueue_text_reply(
        state,
        integration_id,
        None,
        conversation_id,
        None,
        &format!("{image_dedupe_key}:instruction"),
        &text,
        Some(time::OffsetDateTime::now_utc() + time::Duration::minutes(10)),
    )
    .await?;
    Ok(())
}

async fn fail_captcha_image_session(
    state: &AppState,
    integration_id: &str,
    conversation_id: &str,
    image_dedupe_key: &str,
) -> Result<(), sqlx::Error> {
    let Some(session_id) = captcha_session_id(image_dedupe_key) else {
        return Ok(());
    };
    let failed = session::transition(
        state,
        session_id,
        &[LoginSessionStatus::AwaitingCaptcha],
        LoginSessionStatus::Failed,
        Some("captcha_image_delivery_failed"),
    )
    .await?;
    if !failed {
        return Ok(());
    }
    session::clear_protocol_context(state, session_id).await?;
    outbox::enqueue_text_reply(
        state,
        integration_id,
        None,
        conversation_id,
        None,
        &format!("{image_dedupe_key}:delivery-failed"),
        "验证码图片上传失败，本次续期已终止。请查看服务端日志中的飞书错误码，检查请求参数、`im:resource` 权限和应用发布状态后重新发起续期。",
        None,
    )
    .await?;
    Ok(())
}

async fn fail_claimed_bot(
    state: &AppState,
    outbox_id: &str,
    attempt: i32,
    kind: &str,
    retryable: bool,
) -> Result<(), sqlx::Error> {
    let should_retry = retryable && attempt < BOT_MAX_ATTEMPTS;
    let next = match state.config.database_backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '30 seconds'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+30 seconds')",
    };
    let clear_payload = match state.config.database_backend {
        DatabaseBackend::Postgres => "'\\x00'::bytea",
        DatabaseBackend::Sqlite => "X'00'",
    };
    sqlx::query(&format!(
        "UPDATE bot_outbox SET status = $1, lease_until = NULL,
         next_attempt_at = {next}, last_error_kind = $2,
         payload_encrypted = CASE WHEN $1 = 'dead' THEN {clear_payload} ELSE payload_encrypted END
         WHERE id = $3 AND status = 'sending' AND attempt_count = $4"
    ))
    .bind(if should_retry { "failed" } else { "dead" })
    .bind(kind)
    .bind(outbox_id)
    .bind(attempt)
    .execute(&state.pool)
    .await?;
    Ok(())
}

async fn renew_claimed_bot(
    state: &AppState,
    outbox_id: &str,
    attempt: i32,
) -> Result<bool, sqlx::Error> {
    let lease = match state.config.database_backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '2 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+2 minutes')",
    };
    Ok(sqlx::query(&format!(
        "UPDATE bot_outbox SET lease_until = {lease}
         WHERE id = $1 AND status = 'sending' AND attempt_count = $2"
    ))
    .bind(outbox_id)
    .bind(attempt)
    .execute(&state.pool)
    .await?
    .rows_affected()
        == 1)
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
             AND o.status IN ('pending', 'failed', 'sending')
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
    let credentials = match state.credential_cipher.decrypt(&encrypted) {
        Ok(credentials) => credentials,
        Err(_) => {
            dead_letter_system_alert(state, &outbox_id, attempt, CREDENTIAL_DECRYPTION_FAILED)
                .await?;
            return Ok(true);
        }
    };
    let target_encrypted: Vec<u8> = row.get("target_encrypted");
    let target = match state.credential_cipher.decrypt(&target_encrypted) {
        Ok(target) => target,
        Err(_) => {
            dead_letter_system_alert(state, &outbox_id, attempt, TARGET_DECRYPTION_FAILED).await?;
            return Ok(true);
        }
    };
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
                 delivered_at = CURRENT_TIMESTAMP, last_error_kind = NULL
                 WHERE id = $1 AND status = 'sending' AND attempt_count = $2",
            )
            .bind(&outbox_id)
            .bind(attempt)
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
                 next_attempt_at = {next}, last_error_kind = $2
                 WHERE id = $3 AND status = 'sending' AND attempt_count = $4"
            );
            sqlx::query(&query)
                .bind(if retryable { "failed" } else { "dead" })
                .bind(error.kind())
                .bind(&outbox_id)
                .bind(attempt)
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
             AND o.status IN ('pending', 'failed', 'sending')
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
         p.tid, p.pid, p.floor_number, p.author_uid, p.author_name, p.content_raw, t.title,
         (SELECT COUNT(*) FROM post_event_watch_matches m
          JOIN watch_targets w ON w.id = m.watch_id
          WHERE m.post_event_id = e.id AND w.target_type = 'user') AS user_watch_count
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
    let credentials = match state.credential_cipher.decrypt(&encrypted) {
        Ok(credentials) => credentials,
        Err(_) => {
            dead_letter_post(state, &outbox_id, attempt, CREDENTIAL_DECRYPTION_FAILED).await?;
            return Ok(true);
        }
    };
    let target_encrypted: Vec<u8> = row.get("target_encrypted");
    let target = match state.credential_cipher.decrypt(&target_encrypted) {
        Ok(target) => target,
        Err(_) => {
            dead_letter_post(state, &outbox_id, attempt, TARGET_DECRYPTION_FAILED).await?;
            return Ok(true);
        }
    };
    let tid: i64 = row.get("tid");
    let pid: Option<i64> = row.get("pid");
    let author_uid: i64 = row.get("author_uid");
    let author: String = row.get("author_name");
    let content: String = row.get("content_raw");
    let title: String = row.get("title");
    let user_watch_count: i64 = row.get("user_watch_count");
    let floor: Option<i32> = row.get("floor_number");
    let notification = Notification {
        title: notification_title(&title, &author, author_uid, user_watch_count),
        body: format!("{} · #{}\n\n{}", author, floor.unwrap_or_default(), content),
        url: post_url(tid, pid),
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
                 delivered_at = CURRENT_TIMESTAMP, last_error_kind = NULL
                 WHERE id = $1 AND status = 'sending' AND attempt_count = $2",
            )
            .bind(&outbox_id)
            .bind(attempt)
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
                 next_attempt_at = {next}, last_error_kind = $2
                 WHERE id = $3 AND status = 'sending' AND attempt_count = $4"
            );
            sqlx::query(&query)
                .bind(if retryable { "failed" } else { "dead" })
                .bind(error.kind())
                .bind(&outbox_id)
                .bind(attempt)
                .execute(&state.pool)
                .await?;
        }
    }
    Ok(true)
}

async fn dead_letter_system_alert(
    state: &AppState,
    outbox_id: &str,
    attempt: i32,
    error_kind: &'static str,
) -> Result<(), sqlx::Error> {
    if let Err(error) = record_system_alert_delivery(
        state,
        outbox_id,
        attempt,
        false,
        None,
        None,
        Some(error_kind),
    )
    .await
    {
        warn!(
            outbox_id,
            error_kind, error = %error,
            "failed to record system alert decryption failure"
        );
    }
    sqlx::query(
        "UPDATE system_alert_outbox SET status = 'dead', lease_until = NULL,
         last_error_kind = $1
         WHERE id = $2 AND status = 'sending' AND attempt_count = $3",
    )
    .bind(error_kind)
    .bind(outbox_id)
    .bind(attempt)
    .execute(&state.pool)
    .await?;
    Ok(())
}

async fn dead_letter_post(
    state: &AppState,
    outbox_id: &str,
    attempt: i32,
    error_kind: &'static str,
) -> Result<(), sqlx::Error> {
    if let Err(error) = record_delivery(
        state,
        outbox_id,
        attempt,
        false,
        None,
        None,
        Some(error_kind),
    )
    .await
    {
        warn!(
            outbox_id,
            error_kind, error = %error,
            "failed to record notification decryption failure"
        );
    }
    sqlx::query(
        "UPDATE notification_outbox SET status = 'dead', lease_until = NULL,
         last_error_kind = $1
         WHERE id = $2 AND status = 'sending' AND attempt_count = $3",
    )
    .bind(error_kind)
    .bind(outbox_id)
    .bind(attempt)
    .execute(&state.pool)
    .await?;
    Ok(())
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

fn post_url(tid: i64, pid: Option<i64>) -> String {
    match pid {
        Some(pid) => format!("https://bbs.nga.cn/read.php?tid={tid}&pid={pid}"),
        None => format!("https://bbs.nga.cn/read.php?tid={tid}"),
    }
}

fn notification_title(
    thread_title: &str,
    author_name: &str,
    author_uid: i64,
    user_watch_count: i64,
) -> String {
    if user_watch_count > 0 {
        let nickname = if author_name.trim().is_empty() {
            format!("UID {author_uid}")
        } else {
            author_name.to_owned()
        };
        format!("用户监控：{nickname}")
    } else {
        thread_title.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, net::SocketAddr, sync::Arc};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use sqlx::any::AnyPoolOptions;
    use tokio::sync::RwLock;

    use super::{
        CREDENTIAL_DECRYPTION_FAILED, TARGET_DECRYPTION_FAILED, bot_error_detail,
        captcha_session_id, fail_claimed_bot, notification_title, post_url, process_one,
    };
    use crate::{
        app::AppState,
        bot::adapter::BotSendError,
        config::{
            AppConfig, AssetsConfig, DatabaseBackend, ObservabilityConfig, PersistenceConfig,
            SchedulerConfig,
        },
        crypto::CredentialCipher,
        nga::NgaClient,
    };

    async fn test_state() -> AppState {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database must connect");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("foreign keys must enable");
        sqlx::migrate!("./migrations/sqlite")
            .run(&pool)
            .await
            .expect("migrations must run");
        AppState {
            pool,
            config: Arc::new(AppConfig {
                bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
                database_backend: DatabaseBackend::Sqlite,
                database_url: SecretString::from("postgres://unused"),
                sqlite_path: ":memory:".into(),
                database_max_connections: 1,
                api_token: SecretString::from("test-token"),
                admin_password: SecretString::from("test-password"),
                credential_encryption_key: SecretString::from(STANDARD.encode([7_u8; 32])),
                nga_user_agent: "test-agent".to_owned(),
                run_migrations: false,
                persistence: PersistenceConfig {
                    store_raw_payload: false,
                },
                assets: AssetsConfig {
                    download_enabled: false,
                    storage_path: "./data/test-assets".into(),
                    max_download_bytes: 10 * 1024 * 1024,
                },
                scheduler: SchedulerConfig {
                    default_interval_seconds: 60,
                    timezone_offset: time::UtcOffset::UTC,
                },
                observability: ObservabilityConfig {
                    log_filter: "info".to_owned(),
                    log_json: false,
                },
            }),
            credential_cipher: Arc::new(
                CredentialCipher::from_base64(&STANDARD.encode([7_u8; 32])).unwrap(),
            ),
            nga_client: NgaClient::new("test-agent".to_owned()).unwrap(),
            admin_sessions: Arc::new(RwLock::new(HashSet::new())),
            platform_updates: tokio::sync::watch::channel(()).0,
        }
    }

    #[test]
    fn reply_url_opens_post_detail_by_tid_and_pid() {
        assert_eq!(
            post_url(47_264_819, Some(876_581_704)),
            "https://bbs.nga.cn/read.php?tid=47264819&pid=876581704"
        );
    }

    #[test]
    fn topic_url_opens_thread_without_reply_anchor() {
        assert_eq!(
            post_url(47_264_819, None),
            "https://bbs.nga.cn/read.php?tid=47264819"
        );
    }

    #[test]
    fn user_watch_title_highlights_author_nickname() {
        assert_eq!(
            notification_title("原主题标题", "铁锤狂砸盘", 24_252_407, 1),
            "用户监控：铁锤狂砸盘"
        );
        assert_eq!(
            notification_title("原主题标题", "", 24_252_407, 1),
            "用户监控：UID 24252407"
        );
        assert_eq!(
            notification_title("原主题标题", "铁锤狂砸盘", 24_252_407, 0),
            "原主题标题"
        );
    }

    #[test]
    fn captcha_dedupe_key_yields_only_valid_session_ids() {
        assert_eq!(
            captcha_session_id("login:session-id:captcha:2"),
            Some("session-id")
        );
        assert_eq!(captcha_session_id("login:session-id:captcha:nope"), None);
        assert_eq!(captcha_session_id("command:captcha:2"), None);
    }

    #[test]
    fn bot_error_detail_is_single_line_and_bounded() {
        let detail = format!("permission denied\n{}", "x".repeat(600));
        let sanitized = bot_error_detail(&BotSendError::ImageUpload(detail));
        assert!(!sanitized.contains('\n'));
        assert_eq!(sanitized.chars().count(), 512);
    }

    #[tokio::test]
    async fn stale_bot_attempt_cannot_overwrite_a_newer_claim() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO platform_integrations
             (id, platform, label, credentials_encrypted)
             VALUES ('integration', 'feishu', 'test', X'00')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO bot_outbox
             (id, dedupe_key, integration_id, conversation_id, message_kind,
              payload_encrypted, status, attempt_count)
             VALUES ('outbox', 'dedupe', 'integration', 'chat', 'text', X'00', 'sending', 2)",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        fail_claimed_bot(&state, "outbox", 1, "stale", false)
            .await
            .unwrap();
        let stale_result: (String, i32, Option<String>) = sqlx::query_as(
            "SELECT status, attempt_count, last_error_kind FROM bot_outbox WHERE id = 'outbox'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(stale_result, ("sending".into(), 2, None));

        fail_claimed_bot(&state, "outbox", 2, "current", false)
            .await
            .unwrap();
        let current_result: (String, Option<String>) =
            sqlx::query_as("SELECT status, last_error_kind FROM bot_outbox WHERE id = 'outbox'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(current_result, ("dead".into(), Some("current".into())));
    }

    #[tokio::test]
    async fn corrupt_notification_secrets_are_dead_lettered_and_do_not_stop_the_queue() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO platform_integrations
             (id, platform, label, credentials_encrypted)
             VALUES ('integration', 'bark', 'test', X'00')",
        )
        .execute(&state.pool)
        .await
        .expect("integration must insert");
        sqlx::query(
            "INSERT INTO notification_channels
             (id, integration_id, label, target_encrypted)
             VALUES ('channel', 'integration', 'test', X'00')",
        )
        .execute(&state.pool)
        .await
        .expect("channel must insert");
        sqlx::query(
            "INSERT INTO system_alerts (id, alert_key, title, body, url)
             VALUES ('alert', 'test-alert', 'title', 'body', '/admin')",
        )
        .execute(&state.pool)
        .await
        .expect("alert must insert");
        sqlx::query(
            "INSERT INTO system_alert_outbox (id, alert_id, channel_id)
             VALUES ('alert-outbox', 'alert', 'channel')",
        )
        .execute(&state.pool)
        .await
        .expect("alert outbox must insert");
        sqlx::query(
            "INSERT INTO threads
             (tid, fid, title, forum_name, author_uid, author_name)
             VALUES (1001, 3001, 'thread', 'forum', 2001, 'author')",
        )
        .execute(&state.pool)
        .await
        .expect("thread must insert");
        sqlx::query(
            "INSERT INTO posts
             (id, tid, pid, floor_number, post_kind, author_uid, author_name,
              content_raw, page_number, raw_payload)
             VALUES ('post', 1001, 4001, 1, 'reply', 2002, 'reply author', 'body', 1, '')",
        )
        .execute(&state.pool)
        .await
        .expect("post must insert");
        sqlx::query(
            "INSERT INTO post_events (id, post_id, event_type)
             VALUES ('event', 'post', 'new_reply')",
        )
        .execute(&state.pool)
        .await
        .expect("event must insert");
        sqlx::query(
            "INSERT INTO notification_outbox (id, post_event_id, channel_id)
             VALUES ('post-outbox', 'event', 'channel')",
        )
        .execute(&state.pool)
        .await
        .expect("post outbox must insert");

        assert!(
            process_one(&state)
                .await
                .expect("bad integration ciphertext must be row-local")
        );
        let alert_status: String =
            sqlx::query_scalar("SELECT status FROM system_alert_outbox WHERE id = 'alert-outbox'")
                .fetch_one(&state.pool)
                .await
                .expect("alert outbox must exist");
        let alert_error: String = sqlx::query_scalar(
            "SELECT last_error_kind FROM system_alert_outbox WHERE id = 'alert-outbox'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("alert failure kind must exist");
        let alert_lease: Option<String> = sqlx::query_scalar(
            "SELECT CAST(lease_until AS TEXT) FROM system_alert_outbox WHERE id = 'alert-outbox'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("alert lease must query");
        assert_eq!(alert_status, "dead");
        assert_eq!(alert_error, CREDENTIAL_DECRYPTION_FAILED);
        assert!(alert_lease.is_none());
        let alert_delivery_error: String = sqlx::query_scalar(
            "SELECT error_kind FROM system_alert_deliveries WHERE outbox_id = 'alert-outbox'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("alert failure delivery must exist");
        assert_eq!(alert_delivery_error, CREDENTIAL_DECRYPTION_FAILED);

        let valid_credentials = state
            .credential_cipher
            .encrypt("{}")
            .expect("test credentials must encrypt");
        sqlx::query(
            "UPDATE platform_integrations SET credentials_encrypted = $1
             WHERE id = 'integration'",
        )
        .bind(valid_credentials)
        .execute(&state.pool)
        .await
        .expect("credentials must become decryptable");

        assert!(
            process_one(&state)
                .await
                .expect("bad target ciphertext must be row-local")
        );
        let post_status: String =
            sqlx::query_scalar("SELECT status FROM notification_outbox WHERE id = 'post-outbox'")
                .fetch_one(&state.pool)
                .await
                .expect("post outbox must exist");
        let post_error: String = sqlx::query_scalar(
            "SELECT last_error_kind FROM notification_outbox WHERE id = 'post-outbox'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("post failure kind must exist");
        let post_lease: Option<String> = sqlx::query_scalar(
            "SELECT CAST(lease_until AS TEXT) FROM notification_outbox WHERE id = 'post-outbox'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("post lease must query");
        assert_eq!(post_status, "dead");
        assert_eq!(post_error, TARGET_DECRYPTION_FAILED);
        assert!(post_lease.is_none());
        let post_delivery_error: String = sqlx::query_scalar(
            "SELECT error_kind FROM notification_deliveries WHERE outbox_id = 'post-outbox'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("post failure delivery must exist");
        assert_eq!(post_delivery_error, TARGET_DECRYPTION_FAILED);

        assert!(
            !process_one(&state)
                .await
                .expect("queue must continue and become empty")
        );
    }
}
