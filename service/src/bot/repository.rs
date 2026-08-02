//! Database access for the bot runtime: inbound dedupe, binding lookup and
//! outbox persistence. No command payload text is ever persisted.

#![allow(dead_code)]
use sqlx::Row;
use uuid::Uuid;

use crate::{
    app::AppState,
    platform::integration::{BotRole, ConversationType},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboundDedup {
    /// Inserted as a new inbound event.
    New,
    /// The (integration_id, platform_message_id) pair already exists.
    Duplicate,
}

/// Resolve a binding for an actor in a conversation. A binding with a NULL
/// conversation_id applies to every conversation; an exact conversation match
/// wins over the global one.
#[derive(Clone, Debug)]
pub struct BotBindingInfo {
    pub id: String,
    pub integration_id: String,
    pub actor_id: String,
    pub role: BotRole,
    pub label: String,
}

pub async fn find_binding(
    state: &AppState,
    integration_id: &str,
    actor_id: &str,
    conversation_id: &str,
) -> Result<Option<BotBindingInfo>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, integration_id, actor_id, role, label
         FROM bot_bindings
         WHERE integration_id = $1 AND actor_id = $2
           AND enabled = 1
           AND (conversation_id IS NULL OR conversation_id = $3)
         ORDER BY CASE WHEN conversation_id IS NULL THEN 1 ELSE 0 END
         LIMIT 1",
    )
    .bind(integration_id)
    .bind(actor_id)
    .bind(conversation_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let role = BotRole::parse(&row.get::<String, _>("role"))
        .ok_or_else(|| sqlx::Error::Protocol("invalid_role".to_owned()))?;
    Ok(Some(BotBindingInfo {
        id: row.get("id"),
        integration_id: row.get("integration_id"),
        actor_id: row.get("actor_id"),
        role,
        label: row.get("label"),
    }))
}

/// Persist an inbound event for dedupe and audit. Raw text and command
/// arguments are intentionally not stored.
#[allow(clippy::too_many_arguments)]
pub async fn record_inbound_event(
    state: &AppState,
    integration_id: &str,
    platform_message_id: &str,
    platform_event_id: Option<&str>,
    actor_id: &str,
    conversation_id: &str,
    conversation_type: ConversationType,
    command_name: Option<&str>,
    status: &str,
) -> Result<(String, InboundDedup), sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let inserted = sqlx::query(
        "INSERT INTO bot_inbound_events
         (id, integration_id, platform_message_id, platform_event_id, actor_id,
          conversation_id, conversation_type, command_name, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (integration_id, platform_message_id) DO NOTHING",
    )
    .bind(&id)
    .bind(integration_id)
    .bind(platform_message_id)
    .bind(platform_event_id)
    .bind(actor_id)
    .bind(conversation_id)
    .bind(conversation_type.as_str())
    .bind(command_name)
    .bind(status)
    .execute(&state.pool)
    .await?;
    if inserted.rows_affected() == 1 {
        Ok((id, InboundDedup::New))
    } else {
        let existing: String = sqlx::query_scalar(
            "SELECT id FROM bot_inbound_events
             WHERE integration_id = $1 AND platform_message_id = $2",
        )
        .bind(integration_id)
        .bind(platform_message_id)
        .fetch_one(&state.pool)
        .await?;
        Ok((existing, InboundDedup::Duplicate))
    }
}

pub async fn mark_inbound_processed(
    state: &AppState,
    inbound_event_id: &str,
    status: &str,
    error_kind: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE bot_inbound_events SET status = $1, error_kind = $2,
         processed_at = CURRENT_TIMESTAMP WHERE id = $3",
    )
    .bind(status)
    .bind(error_kind)
    .bind(inbound_event_id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

pub async fn update_inbound_command(
    state: &AppState,
    inbound_event_id: &str,
    command_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE bot_inbound_events SET command_name = $1 WHERE id = $2")
        .bind(command_name)
        .bind(inbound_event_id)
        .execute(&state.pool)
        .await?;
    Ok(())
}
