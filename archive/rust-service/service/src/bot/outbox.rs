//! Bot reply outbox. Producers enqueue stable-dedupe-key messages; the
//! notification worker claims and delivers them through the platform adapter.

#![allow(dead_code)]
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    app::AppState,
    bot::domain::{BotMessageKind, ImagePayload, TextPayload},
};

/// Queue a plain-text reply. `sequence` disambiguates multiple replies to the
/// same inbound message.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_text_reply(
    state: &AppState,
    integration_id: &str,
    inbound_event_id: Option<&str>,
    conversation_id: &str,
    reply_to_message_id: Option<&str>,
    dedupe_key: &str,
    text: &str,
    expires_at: Option<OffsetDateTime>,
) -> Result<bool, sqlx::Error> {
    let payload = TextPayload {
        text: text.to_owned(),
    };
    let payload_json = serde_json::to_string(&payload)
        .map_err(|error| sqlx::Error::Protocol(format!("{error}")))?;
    enqueue_payload(
        state,
        integration_id,
        inbound_event_id,
        conversation_id,
        reply_to_message_id,
        BotMessageKind::Text,
        &payload_json,
        dedupe_key,
        expires_at,
    )
    .await
}

/// Queue an image (captcha) with a short TTL.
pub async fn enqueue_image(
    state: &AppState,
    integration_id: &str,
    inbound_event_id: Option<&str>,
    conversation_id: &str,
    dedupe_key: &str,
    image: ImagePayload,
    expires_at: OffsetDateTime,
) -> Result<bool, sqlx::Error> {
    let payload =
        serde_json::to_string(&image).map_err(|error| sqlx::Error::Protocol(format!("{error}")))?;
    enqueue_payload(
        state,
        integration_id,
        inbound_event_id,
        conversation_id,
        None,
        BotMessageKind::Image,
        &payload,
        dedupe_key,
        Some(expires_at),
    )
    .await
}

/// Core insert with dedupe on `dedupe_key`. Returns whether a new row was
/// inserted (false = duplicate).
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_payload(
    state: &AppState,
    integration_id: &str,
    inbound_event_id: Option<&str>,
    conversation_id: &str,
    reply_to_message_id: Option<&str>,
    message_kind: BotMessageKind,
    payload_json: &str,
    dedupe_key: &str,
    expires_at: Option<OffsetDateTime>,
) -> Result<bool, sqlx::Error> {
    let encrypted = state
        .credential_cipher
        .encrypt(payload_json)
        .map_err(|error| sqlx::Error::Protocol(format!("{error}")))?;
    let expires_sql = match expires_at {
        Some(when) => match state.config.database_backend {
            crate::config::DatabaseBackend::Postgres => {
                format!("'{}'::timestamptz", format_sql_timestamp(when))
            }
            crate::config::DatabaseBackend::Sqlite => {
                format!("'{}'", format_sql_timestamp(when))
            }
        },
        None => "NULL".to_owned(),
    };
    let inserted = sqlx::query(&format!(
        "INSERT INTO bot_outbox
         (id, dedupe_key, integration_id, inbound_event_id, conversation_id,
          reply_to_message_id, message_kind, payload_encrypted, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, {expires_sql})
         ON CONFLICT (dedupe_key) DO NOTHING"
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(dedupe_key)
    .bind(integration_id)
    .bind(inbound_event_id)
    .bind(conversation_id)
    .bind(reply_to_message_id)
    .bind(message_kind.as_str())
    .bind(encrypted)
    .execute(&state.pool)
    .await?;
    Ok(inserted.rows_affected() == 1)
}

fn format_sql_timestamp(when: OffsetDateTime) -> String {
    when.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
