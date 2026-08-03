//! (dead_code allowed: platform domain model contract; wired by APIs and adapters)
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::app::AppState;

/// Platforms the service knows about. `Bark` only notifies; the rest may also
/// host a bot adapter (telegram/qq arrive in a later phase).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlatformKind {
    Bark,
    Feishu,
    Telegram,
    Qq,
}

impl PlatformKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bark => "bark",
            Self::Feishu => "feishu",
            Self::Telegram => "telegram",
            Self::Qq => "qq",
        }
    }

    pub fn supports_bot(&self) -> bool {
        !matches!(self, Self::Bark)
    }

    pub fn bot_adapter_available(&self) -> bool {
        matches!(self, Self::Feishu)
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "bark" => Some(Self::Bark),
            "feishu" => Some(Self::Feishu),
            "telegram" => Some(Self::Telegram),
            "qq" => Some(Self::Qq),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotRole {
    Owner,
    Operator,
    ReadOnly,
}

impl BotRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Operator => "operator",
            Self::ReadOnly => "read_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Self::Owner),
            "operator" => Some(Self::Operator),
            "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationType {
    Private,
    Group,
    Channel,
}

impl ConversationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Group => "group",
            Self::Channel => "channel",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "private" => Some(Self::Private),
            "group" => Some(Self::Group),
            "channel" => Some(Self::Channel),
            _ => None,
        }
    }
}

// ---- Connection credentials ---------------------------------------------

/// Credentials stored on `platform_integrations.credentials_encrypted`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "platform", content = "credentials", rename_all = "lowercase")]
pub enum IntegrationCredentials {
    Bark(BarkCredentials),
    Feishu(FeishuCredentials),
    Telegram(serde_json::Value),
    Qq(serde_json::Value),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeishuCredentials {
    pub app_id: String,
    pub app_secret: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BarkCredentials {
    #[serde(default = "default_bark_server")]
    pub server_url: String,
    #[serde(default = "default_bark_group")]
    pub group: String,
}

// ---- Notification target -------------------------------------------------

/// Per-recipient target stored on `notification_channels.target_encrypted`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "platform", content = "target", rename_all = "lowercase")]
pub enum NotificationTarget {
    Bark(BarkTarget),
    Feishu(FeishuTarget),
    Telegram(serde_json::Value),
    Qq(serde_json::Value),
}

/// Decode the exact tagged representation persisted in
/// `platform_integrations.credentials_encrypted`.
pub fn parse_stored_credentials(json: &str) -> Result<IntegrationCredentials, serde_json::Error> {
    serde_json::from_str(json)
}

/// Decode the exact tagged representation persisted in
/// `notification_channels.target_encrypted`.
pub fn parse_stored_target(json: &str) -> Result<NotificationTarget, serde_json::Error> {
    serde_json::from_str(json)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeishuTarget {
    #[serde(default = "default_feishu_receive_id_type")]
    pub receive_id_type: String,
    pub receive_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BarkTarget {
    pub device_key: String,
}

// ---- Views ---------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct IntegrationView {
    pub id: String,
    pub platform: String,
    pub label: String,
    pub enabled: bool,
    pub delivery_enabled: bool,
    pub bot_enabled: bool,
    pub credentials_configured: bool,
    pub capabilities: Vec<&'static str>,
    pub connection_status: String,
    pub last_error_kind: Option<String>,
}

impl IntegrationView {
    pub fn capabilities_for(platform: PlatformKind) -> Vec<&'static str> {
        match platform {
            PlatformKind::Bark => vec!["notification_send"],
            PlatformKind::Feishu => vec![
                "notification_send",
                "bot_receive",
                "bot_reply",
                "image_send",
            ],
            PlatformKind::Telegram | PlatformKind::Qq => vec![
                "notification_send",
                "bot_receive",
                "bot_reply",
                "image_send",
            ],
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BotBindingView {
    pub id: String,
    pub integration_id: String,
    pub actor_id_masked: String,
    pub conversation_id: Option<String>,
    pub conversation_type: Option<String>,
    pub role: String,
    pub label: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PairingTokenView {
    pub code: String,
    pub expires_at: String,
}

pub struct PlatformIntegration {
    pub id: String,
    pub platform: PlatformKind,
    pub label: String,
    pub enabled: bool,
    pub delivery_enabled: bool,
    pub bot_enabled: bool,
    pub credentials: IntegrationCredentials,
    pub connection_status: String,
    pub last_error_kind: Option<String>,
}

// ---- Validation ----------------------------------------------------------

pub fn validate_credentials(platform: PlatformKind, value: &serde_json::Value) -> bool {
    match platform {
        PlatformKind::Bark => serde_json::from_value::<BarkCredentials>(value.clone())
            .is_ok_and(|c| !c.server_url.trim().is_empty() && c.server_url.starts_with("http")),
        PlatformKind::Feishu => serde_json::from_value::<FeishuCredentials>(value.clone())
            .is_ok_and(|c| c.app_id.starts_with("cli_") && !c.app_secret.trim().is_empty()),
        PlatformKind::Telegram | PlatformKind::Qq => false, // adapters arrive later
    }
}

pub fn validate_target(platform: PlatformKind, value: &serde_json::Value) -> bool {
    match platform {
        PlatformKind::Bark => serde_json::from_value::<BarkTarget>(value.clone())
            .is_ok_and(|t| !t.device_key.trim().is_empty()),
        PlatformKind::Feishu => {
            serde_json::from_value::<FeishuTarget>(value.clone()).is_ok_and(|t| {
                !t.receive_id.trim().is_empty()
                    && matches!(
                        t.receive_id_type.as_str(),
                        "chat_id" | "open_id" | "user_id" | "union_id" | "email"
                    )
            })
        }
        PlatformKind::Telegram | PlatformKind::Qq => false,
    }
}

// ---- Repository ----------------------------------------------------------

pub async fn list_integrations(state: &AppState) -> Result<Vec<IntegrationView>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, platform, label, enabled, delivery_enabled, bot_enabled,
                connection_status, last_error_kind
         FROM platform_integrations ORDER BY created_at",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        views.push(integration_view(&row));
    }
    Ok(views)
}

pub async fn get_integration(
    state: &AppState,
    id: &str,
) -> Result<Option<PlatformIntegration>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, platform, label, enabled, delivery_enabled, bot_enabled,
                credentials_encrypted, connection_status, last_error_kind
         FROM platform_integrations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let encrypted: Vec<u8> = row.get("credentials_encrypted");
    let plaintext = decrypt(state, &encrypted)?;
    let credentials: IntegrationCredentials = serde_json::from_str(&plaintext)
        .map_err(|error| sqlx::Error::Protocol(format!("{error}")))?;
    Ok(Some(PlatformIntegration {
        id: row.get("id"),
        platform: PlatformKind::parse(&row.get::<String, _>("platform"))
            .ok_or_else(|| sqlx::Error::Protocol("unknown platform".to_owned()))?,
        label: row.get("label"),
        enabled: row.get::<i32, _>("enabled") == 1,
        delivery_enabled: row.get::<i32, _>("delivery_enabled") == 1,
        bot_enabled: row.get::<i32, _>("bot_enabled") == 1,
        credentials,
        connection_status: row.get("connection_status"),
        last_error_kind: row.get("last_error_kind"),
    }))
}

pub async fn insert_integration(
    state: &AppState,
    platform: PlatformKind,
    label: &str,
    enabled: bool,
    delivery_enabled: bool,
    bot_enabled: bool,
    credentials: &serde_json::Value,
) -> Result<PlatformIntegration, sqlx::Error> {
    let raw = serde_json::json!({
        "platform": platform.as_str(),
        "credentials": credentials,
    })
    .to_string();
    let encrypted = state
        .credential_cipher
        .encrypt(&raw)
        .map_err(|error| sqlx::Error::Protocol(format!("{error}")))?;
    let id = Uuid::new_v4().to_string();
    // Pre-check the one-bot-per-platform rule so the caller can map the
    // conflict precisely (the partial unique index remains the final guard).
    if bot_enabled && platform.bot_adapter_available() {
        let others: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM platform_integrations
             WHERE platform = $1 AND bot_enabled = 1",
        )
        .bind(platform.as_str())
        .fetch_one(&state.pool)
        .await?;
        if others > 0 {
            return Err(sqlx::Error::Protocol(
                "bot_already_enabled_for_platform".to_owned(),
            ));
        }
    }
    sqlx::query(
        "INSERT INTO platform_integrations
         (id, platform, label, enabled, delivery_enabled, bot_enabled, credentials_encrypted)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(&id)
    .bind(platform.as_str())
    .bind(label)
    .bind(i32::from(enabled))
    .bind(i32::from(delivery_enabled))
    .bind(i32::from(bot_enabled))
    .bind(encrypted)
    .execute(&state.pool)
    .await?;
    // The request body carries bare credentials (no platform tag); build the
    // tagged variant from the validated platform instead of re-parsing it.
    let parsed = credentials_from_value(platform, credentials.clone())
        .map_err(|error| sqlx::Error::Protocol(format!("{error}")))?;
    Ok(PlatformIntegration {
        id,
        platform,
        label: label.to_owned(),
        enabled,
        delivery_enabled,
        bot_enabled,
        credentials: parsed,
        connection_status: "disconnected".to_owned(),
        last_error_kind: None,
    })
}

fn credentials_from_value(
    platform: PlatformKind,
    value: serde_json::Value,
) -> Result<IntegrationCredentials, serde_json::Error> {
    match platform {
        PlatformKind::Bark => Ok(IntegrationCredentials::Bark(serde_json::from_value(value)?)),
        PlatformKind::Feishu => Ok(IntegrationCredentials::Feishu(serde_json::from_value(
            value,
        )?)),
        PlatformKind::Telegram | PlatformKind::Qq => Err(serde_json::Error::io(
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "unsupported platform"),
        )),
    }
}

pub async fn update_integration(
    state: &AppState,
    id: &str,
    enabled: Option<bool>,
    delivery_enabled: Option<bool>,
    bot_enabled: Option<bool>,
    label: Option<&str>,
    credentials: Option<&serde_json::Value>,
) -> Result<Option<PlatformIntegration>, sqlx::Error> {
    let current =
        sqlx::query("SELECT platform, bot_enabled FROM platform_integrations WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let platform: String = current.get("platform");
    let current_bot: i32 = current.get("bot_enabled");
    let desired_bot = bot_enabled.unwrap_or(current_bot == 1);

    // Enabling bot on this connection requires it to be the platform's only
    // bot connection. The partial unique index remains the final guard; this
    // check produces a stable error before touching the row.
    if desired_bot
        && current_bot != 1
        && PlatformKind::parse(&platform).is_some_and(|kind| kind.bot_adapter_available())
    {
        let others: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM platform_integrations
             WHERE platform = $1 AND id <> $2 AND bot_enabled = 1",
        )
        .bind(&platform)
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
        if others > 0 {
            return Err(sqlx::Error::Protocol(
                "bot_already_enabled_for_platform".to_owned(),
            ));
        }
    }

    let encrypted = if let Some(credentials) = credentials {
        let raw = serde_json::json!({
            "platform": platform,
            "credentials": credentials,
        })
        .to_string();
        Some(
            state
                .credential_cipher
                .encrypt(&raw)
                .map_err(|error| sqlx::Error::Protocol(format!("{error}")))?,
        )
    } else {
        None
    };
    sqlx::query(
        "UPDATE platform_integrations SET
         enabled = COALESCE($1, enabled),
         delivery_enabled = COALESCE($2, delivery_enabled),
         bot_enabled = COALESCE($3, bot_enabled),
         label = COALESCE($4, label),
         credentials_encrypted = COALESCE($5, credentials_encrypted),
         updated_at = CURRENT_TIMESTAMP
         WHERE id = $6",
    )
    .bind(enabled.map(i32::from))
    .bind(delivery_enabled.map(i32::from))
    .bind(bot_enabled.map(i32::from))
    .bind(label)
    .bind(encrypted)
    .bind(id)
    .execute(&state.pool)
    .await?;
    get_integration(state, id).await
}

/// Atomically make `integration_id` the only bot-enabled connection for its
/// platform.
pub async fn set_bot_integration(
    state: &AppState,
    integration_id: &str,
) -> Result<PlatformIntegration, sqlx::Error> {
    let current = sqlx::query("SELECT platform FROM platform_integrations WHERE id = $1")
        .bind(integration_id)
        .fetch_optional(&state.pool)
        .await?;
    let Some(row) = current else {
        return Err(sqlx::Error::Protocol("integration_not_found".to_owned()));
    };
    let platform: String = row.get("platform");
    let kind = PlatformKind::parse(&platform)
        .ok_or_else(|| sqlx::Error::Protocol("unsupported_platform".to_owned()))?;
    if !kind.bot_adapter_available() {
        return Err(sqlx::Error::Protocol("unsupported_platform".to_owned()));
    }

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE platform_integrations SET bot_enabled = 0
         WHERE platform = $1 AND bot_enabled = 1",
    )
    .bind(&platform)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE platform_integrations SET bot_enabled = 1, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1",
    )
    .bind(integration_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    get_integration(state, integration_id).await.map(|value| {
        value.ok_or_else(|| sqlx::Error::Protocol("integration_not_found".to_owned()))
    })?
}

pub async fn clear_bot_integration(
    state: &AppState,
    integration_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE platform_integrations SET bot_enabled = 0, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1",
    )
    .bind(integration_id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Delete an integration unless it still has notification targets, bot
/// bindings, active login sessions or undelivered bot outbox rows.
pub async fn delete_integration(state: &AppState, id: &str) -> Result<bool, sqlx::Error> {
    let references: i64 = sqlx::query_scalar(
        "SELECT
           (SELECT COUNT(*) FROM notification_channels WHERE integration_id = $1)
         + (SELECT COUNT(*) FROM bot_bindings WHERE integration_id = $1)
         + (SELECT COUNT(*) FROM bot_outbox WHERE integration_id = $1)
         + (SELECT COUNT(*) FROM nga_login_sessions WHERE integration_id = $1 AND completed_at IS NULL)",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    if references > 0 {
        return Err(sqlx::Error::Protocol("integration_in_use".to_owned()));
    }
    let affected = sqlx::query("DELETE FROM platform_integrations WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

pub async fn mark_connection_state(
    state: &AppState,
    id: &str,
    connection_status: &str,
    last_error_kind: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE platform_integrations SET connection_status = $1, last_error_kind = $2,
         last_connected_at = CASE WHEN $1 = 'connected' THEN CURRENT_TIMESTAMP ELSE last_connected_at END,
         updated_at = CURRENT_TIMESTAMP WHERE id = $3",
    )
    .bind(connection_status)
    .bind(last_error_kind)
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

// ---- Bot bindings --------------------------------------------------------

pub async fn list_bindings(
    state: &AppState,
    integration_id: &str,
) -> Result<Vec<BotBindingView>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, integration_id, actor_id, conversation_id, conversation_type,
                role, label, enabled
         FROM bot_bindings WHERE integration_id = $1 ORDER BY created_at",
    )
    .bind(integration_id)
    .fetch_all(&state.pool)
    .await?;
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        views.push(bot_binding_view(&row));
    }
    Ok(views)
}

pub async fn update_binding(
    state: &AppState,
    id: &str,
    role: Option<&str>,
    enabled: Option<bool>,
    label: Option<&str>,
) -> Result<bool, sqlx::Error> {
    if enabled == Some(false) || role.is_some_and(|value| value != "owner") {
        let references: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nga_account_renewal_settings WHERE bot_binding_id = $1",
        )
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
        if references > 0 {
            return Err(sqlx::Error::Protocol("binding_in_use".to_owned()));
        }
    }
    let role = match role {
        Some(value) => Some(
            BotRole::parse(value)
                .ok_or_else(|| sqlx::Error::Protocol("invalid_role".to_owned()))?
                .as_str(),
        ),
        None => None,
    };
    let affected = sqlx::query(
        "UPDATE bot_bindings SET
         role = COALESCE($1, role),
         enabled = COALESCE($2, enabled),
         label = COALESCE($3, label),
         updated_at = CURRENT_TIMESTAMP
         WHERE id = $4",
    )
    .bind(role)
    .bind(enabled.map(i32::from))
    .bind(label)
    .bind(id)
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

pub async fn delete_binding(state: &AppState, id: &str) -> Result<bool, sqlx::Error> {
    let references: i64 = sqlx::query_scalar(
        "SELECT
           (SELECT COUNT(*) FROM nga_account_renewal_settings WHERE bot_binding_id = $1)
         + (SELECT COUNT(*) FROM nga_login_sessions WHERE bot_binding_id = $1 AND completed_at IS NULL)",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    if references > 0 {
        return Err(sqlx::Error::Protocol("binding_in_use".to_owned()));
    }
    let affected = sqlx::query("DELETE FROM bot_bindings WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

// ---- Pairing tokens ------------------------------------------------------

/// Create a one-time pairing token. The returned code is shown once in the
/// admin UI; only its SHA-256 is stored.
pub async fn create_pairing_token(
    state: &AppState,
    integration_id: &str,
    role: &str,
    expires_in_seconds: i64,
) -> Result<Option<PairingTokenView>, sqlx::Error> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM platform_integrations
         WHERE id = $1 AND platform = 'feishu' AND enabled = 1 AND bot_enabled = 1",
    )
    .bind(integration_id)
    .fetch_one(&state.pool)
    .await?;
    if exists == 0 {
        return Ok(None);
    }
    let role =
        BotRole::parse(role).ok_or_else(|| sqlx::Error::Protocol("invalid_role".to_owned()))?;
    let code = format!("bind-{}", random_token(24));
    let token_hash = hash_token(&code);
    let id = Uuid::new_v4().to_string();
    let expires_sql = match state.config.database_backend {
        crate::config::DatabaseBackend::Postgres => {
            format!("CURRENT_TIMESTAMP + INTERVAL '{expires_in_seconds} seconds'")
        }
        crate::config::DatabaseBackend::Sqlite => {
            format!("datetime(CURRENT_TIMESTAMP, '+{expires_in_seconds} seconds')")
        }
    };
    sqlx::query(&format!(
        "INSERT INTO bot_pairing_tokens (id, integration_id, token_hash, requested_role, expires_at)
         VALUES ($1, $2, $3, $4, {expires_sql})"
    ))
    .bind(&id)
    .bind(integration_id)
    .bind(token_hash)
    .bind(role.as_str())
    .execute(&state.pool)
    .await?;
    let row = sqlx::query(
        "SELECT CAST(expires_at AS TEXT) AS expires_at FROM bot_pairing_tokens WHERE id = $1",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await?;
    Ok(Some(PairingTokenView {
        code,
        expires_at: row.get("expires_at"),
    }))
}

/// Consume a pairing token and create its private-chat binding in one
/// transaction. A failed/duplicate binding insert rolls token consumption
/// back, and concurrent consumers cannot both claim the same token.
pub async fn consume_pairing_token_and_insert_binding(
    state: &AppState,
    integration_id: &str,
    code: &str,
    actor_id: &str,
    conversation_id: &str,
    label: &str,
) -> Result<Option<(String, BotRole)>, sqlx::Error> {
    let token_hash = hash_token(code);
    let mut tx = state.pool.begin().await?;
    let row = sqlx::query(
        "UPDATE bot_pairing_tokens SET used_at = CURRENT_TIMESTAMP
         WHERE id = (
             SELECT id FROM bot_pairing_tokens
             WHERE integration_id = $1 AND token_hash = $2
               AND used_at IS NULL AND expires_at > CURRENT_TIMESTAMP
             LIMIT 1
         )
         AND used_at IS NULL
         RETURNING requested_role",
    )
    .bind(integration_id)
    .bind(&token_hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(None);
    };
    let role = BotRole::parse(&row.get::<String, _>("requested_role"))
        .ok_or_else(|| sqlx::Error::Protocol("invalid_role".to_owned()))?;
    let binding_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO bot_bindings
         (id, integration_id, actor_id, conversation_id, conversation_type, role, label)
         VALUES ($1, $2, $3, $4, 'private', $5, $6)",
    )
    .bind(&binding_id)
    .bind(integration_id)
    .bind(actor_id)
    .bind(conversation_id)
    .bind(role.as_str())
    .bind(label)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some((binding_id, role)))
}

pub async fn insert_binding(
    state: &AppState,
    integration_id: &str,
    actor_id: &str,
    conversation_id: Option<&str>,
    conversation_type: ConversationType,
    role: BotRole,
    label: &str,
) -> Result<String, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO bot_bindings
         (id, integration_id, actor_id, conversation_id, conversation_type, role, label)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(&id)
    .bind(integration_id)
    .bind(actor_id)
    .bind(conversation_id)
    .bind(conversation_type.as_str())
    .bind(role.as_str())
    .bind(label)
    .execute(&state.pool)
    .await?;
    Ok(id)
}

// ---- Helpers -------------------------------------------------------------

fn decrypt(state: &AppState, encrypted: &[u8]) -> Result<String, sqlx::Error> {
    state
        .credential_cipher
        .decrypt(encrypted)
        .map_err(|error| sqlx::Error::Protocol(format!("{error}")))
}

fn integration_view(row: &sqlx::any::AnyRow) -> IntegrationView {
    let platform =
        PlatformKind::parse(&row.get::<String, _>("platform")).unwrap_or(PlatformKind::Feishu);
    IntegrationView {
        id: row.get("id"),
        platform: row.get("platform"),
        label: row.get("label"),
        enabled: row.get::<i32, _>("enabled") == 1,
        delivery_enabled: row.get::<i32, _>("delivery_enabled") == 1,
        bot_enabled: row.get::<i32, _>("bot_enabled") == 1,
        credentials_configured: true,
        capabilities: IntegrationView::capabilities_for(platform),
        connection_status: row.get("connection_status"),
        last_error_kind: row.get("last_error_kind"),
    }
}

fn bot_binding_view(row: &sqlx::any::AnyRow) -> BotBindingView {
    let actor_id: String = row.get("actor_id");
    BotBindingView {
        id: row.get("id"),
        integration_id: row.get("integration_id"),
        actor_id_masked: mask_actor(&actor_id),
        conversation_id: row.get("conversation_id"),
        conversation_type: row.get("conversation_type"),
        role: row.get("role"),
        label: row.get("label"),
        enabled: row.get::<i32, _>("enabled") == 1,
    }
}

fn mask_actor(value: &str) -> String {
    if value.len() <= 4 {
        "*".repeat(value.len())
    } else {
        format!("{}***{}", &value[..2], &value[value.len() - 2..])
    }
}

fn hash_token(code: &str) -> String {
    let digest = Sha256::digest(code.as_bytes());
    format!("{digest:x}")
}

fn random_token(length: usize) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let mut bytes = Vec::with_capacity(length);
    while bytes.len() < length {
        bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    URL_SAFE_NO_PAD.encode(&bytes[..length])
}

fn default_bark_server() -> String {
    "https://api.day.app".to_owned()
}

fn default_bark_group() -> String {
    "NGA Reminder".to_owned()
}

fn default_feishu_receive_id_type() -> String {
    "chat_id".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{BotRole, ConversationType, hash_token, mask_actor};

    #[test]
    fn roles_round_trip() {
        assert_eq!(
            BotRole::parse(BotRole::Owner.as_str()),
            Some(BotRole::Owner)
        );
        assert_eq!(BotRole::parse("admin"), None);
    }

    #[test]
    fn conversation_types_round_trip() {
        assert_eq!(
            ConversationType::parse(ConversationType::Private.as_str()),
            Some(ConversationType::Private)
        );
    }

    #[test]
    fn token_hash_is_deterministic_and_not_plaintext() {
        let hash = hash_token("bind-abc");
        assert_eq!(hash, hash_token("bind-abc"));
        assert!(!hash.contains("bind-abc"));
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn actor_mask_hides_middle() {
        assert_eq!(mask_actor("ou_123456789"), "ou***89");
        assert_eq!(mask_actor("abcd"), "****");
    }
}
