#![allow(dead_code)] // design-contract APIs; wired by login command and renewal API
//! NGA login session repository and the auth-failure / renewal-success flows.
//! Raw passwords, captcha answers, candidate cookies and protocol contexts are
//! never persisted in plaintext; contexts are v2-encrypted with field AAD.

use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{app::AppState, bot::outbox, notification};

pub const CONFIRMATION_TTL: time::Duration = time::Duration::seconds(15 * 60);
pub const MAX_CAPTCHA_ATTEMPTS: i32 = 3;
pub const FAILURE_COOLDOWN: time::Duration = time::Duration::seconds(15 * 60);
pub const MAX_COOLDOWN: time::Duration = time::Duration::seconds(30 * 60);

#[derive(Clone, Debug)]
pub struct LoginSession {
    pub id: String,
    pub account_id: String,
    pub bot_binding_id: String,
    pub integration_id: String,
    pub actor_id: String,
    pub conversation_id: String,
    pub status: LoginSessionStatus,
    pub captcha_attempt_count: i32,
    pub last_error_kind: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginSessionStatus {
    AwaitingConfirmation,
    Starting,
    AwaitingCaptcha,
    Submitting,
    ValidatingCookie,
    Succeeded,
    Failed,
    Cancelled,
    Expired,
    UnsupportedChallenge,
}

impl LoginSessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Starting => "starting",
            Self::AwaitingCaptcha => "awaiting_captcha",
            Self::Submitting => "submitting",
            Self::ValidatingCookie => "validating_cookie",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::UnsupportedChallenge => "unsupported_challenge",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginProtocolContext {
    pub protocol_version: String,
    pub created_at: String,
    pub expires_at: String,
    pub rid: String,
    pub prid: String,
    pub public_key_pem: String,
    pub cookie_jar: Vec<CookiePair>,
    pub captcha_revision: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CookiePair {
    pub name: String,
    pub value: String,
}

impl LoginProtocolContext {
    pub fn aad(session_id: &str) -> String {
        format!("nga_login_session:{session_id}:protocol_context:v2")
    }
}

/// Triggered by the collectors on a clear `Unauthorized`: pause the account
/// and auth-affected watches, raise the alert, and — when renewal is
/// configured with a valid owner binding — create an awaiting-confirmation
/// login session and notify the owner.
pub async fn on_auth_failure(state: &AppState) -> Result<(), sqlx::Error> {
    // 1. Pause the account.
    sqlx::query(
        "UPDATE nga_accounts SET status = 'paused', last_auth_error_kind = 'unauthorized',
         last_auth_checked_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE label = 'default'",
    )
    .execute(&state.pool)
    .await?;

    // 2. Pause every currently-enabled watch with reason 'auth' (user-paused
    //    watches keep their own pause_reason and are never auto-restored).
    sqlx::query(
        "UPDATE watch_targets SET enabled = 0, status = 'paused', pause_reason = 'auth',
         updated_at = CURRENT_TIMESTAMP
         WHERE enabled = 1 AND deleted_at IS NULL",
    )
    .execute(&state.pool)
    .await?;

    // 3. Create or reopen the alert.
    notification::alerts::ensure_nga_credentials_invalid_alert(state).await?;

    // 4. Try to open a renewal session for the owner.
    let renewal = sqlx::query(
        "SELECT r.account_id, r.bot_binding_id, r.enabled,
                b.integration_id, b.actor_id, b.conversation_id, b.enabled AS binding_enabled
         FROM nga_account_renewal_settings r
         JOIN bot_bindings b ON b.id = r.bot_binding_id
         JOIN nga_accounts a ON a.id = r.account_id
         WHERE a.label = 'default'",
    )
    .fetch_optional(&state.pool)
    .await?;
    let Some(renewal) = renewal else {
        return Ok(());
    };
    let renewal_enabled: i32 = renewal.get("enabled");
    let binding_enabled: i32 = renewal.get("binding_enabled");
    if renewal_enabled == 0 || binding_enabled == 0 {
        return Ok(());
    }
    let conversation_id: Option<String> = renewal.get("conversation_id");
    let Some(conversation_id) = conversation_id else {
        return Ok(());
    };
    let account_id: String = renewal.get("account_id");
    let bot_binding_id: String = renewal.get("bot_binding_id");
    let integration_id: String = renewal.get("integration_id");
    let actor_id: String = renewal.get("actor_id");

    // 5. Only one active session per account (partial unique index guards).
    let id = Uuid::new_v4().to_string();
    let expires_sql = match state.config.database_backend {
        crate::config::DatabaseBackend::Postgres => {
            format!(
                "CURRENT_TIMESTAMP + INTERVAL '{} seconds'",
                CONFIRMATION_TTL.whole_seconds()
            )
        }
        crate::config::DatabaseBackend::Sqlite => {
            format!(
                "datetime(CURRENT_TIMESTAMP, '+{} seconds')",
                CONFIRMATION_TTL.whole_seconds()
            )
        }
    };
    let inserted = sqlx::query(&format!(
        "INSERT INTO nga_login_sessions
         (id, account_id, bot_binding_id, integration_id, actor_id, conversation_id,
          trigger_kind, status, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, 'cookie_invalid', 'awaiting_confirmation', {expires_sql})"
    ))
    .bind(&id)
    .bind(&account_id)
    .bind(&bot_binding_id)
    .bind(&integration_id)
    .bind(&actor_id)
    .bind(&conversation_id)
    .execute(&state.pool)
    .await;
    match inserted {
        Ok(result) if result.rows_affected() == 1 => {}
        // The one-active-session partial unique index fired: an active
        // session already exists, nothing else to do.
        Ok(_) => return Ok(()),
        Err(error) if is_unique_violation(&error) => return Ok(()),
        Err(error) => return Err(error),
    }

    // 6. Notify the owner (dedupe key makes repeats idempotent).
    let dedupe_key = format!("login:{id}:confirmation");
    outbox::enqueue_text_reply(
        state,
        &integration_id,
        None,
        &conversation_id,
        None,
        &dedupe_key,
        &format!(
            "NGA Cookie 已失效，所有监控已暂停。\n确认续期请回复：\n`/login confirm {id}`\n取消请回复：\n`/login cancel {id}`\n（15 分钟内有效）"
        ),
        None,
    )
    .await?;
    Ok(())
}

pub async fn get_session(state: &AppState, id: &str) -> Result<Option<LoginSession>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, account_id, bot_binding_id, integration_id, actor_id,
                conversation_id, status, captcha_attempt_count, last_error_kind
         FROM nga_login_sessions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    row.map(session_from_row).transpose()
}

pub async fn active_session_for_account(
    state: &AppState,
    account_id: &str,
) -> Result<Option<LoginSession>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, account_id, bot_binding_id, integration_id, actor_id,
                conversation_id, status, captcha_attempt_count, last_error_kind
         FROM nga_login_sessions
         WHERE account_id = $1 AND status IN
           ('awaiting_confirmation','starting','awaiting_captcha','submitting','validating_cookie')",
    )
    .bind(account_id)
    .fetch_optional(&state.pool)
    .await?;
    row.map(session_from_row).transpose()
}

/// Conditional status transition; returns whether exactly one row changed.
pub async fn transition(
    state: &AppState,
    id: &str,
    from: &[LoginSessionStatus],
    to: LoginSessionStatus,
    last_error_kind: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let from_list = from
        .iter()
        .map(|status| format!("'{}'", status.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "UPDATE nga_login_sessions SET status = $1, last_error_kind = $2,
         updated_at = CURRENT_TIMESTAMP,
         completed_at = CASE WHEN $1 IN ('succeeded','failed','cancelled','expired','unsupported_challenge')
                             THEN CURRENT_TIMESTAMP ELSE completed_at END
         WHERE id = $3 AND status IN ({from_list})"
    );
    let affected = sqlx::query(&query)
        .bind(to.as_str())
        .bind(last_error_kind)
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

pub async fn increment_captcha_attempts(state: &AppState, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE nga_login_sessions SET captcha_attempt_count = captcha_attempt_count + 1,
         updated_at = CURRENT_TIMESTAMP WHERE id = $1",
    )
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

pub async fn save_protocol_context(
    state: &AppState,
    session_id: &str,
    context: &LoginProtocolContext,
) -> Result<(), sqlx::Error> {
    let json = serde_json::to_string(context).map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;
    let encrypted = state
        .credential_cipher
        .encrypt_v2(&json, LoginProtocolContext::aad(session_id).as_bytes())
        .map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;
    sqlx::query(
        "UPDATE nga_login_sessions SET protocol_context_encrypted = $1,
         updated_at = CURRENT_TIMESTAMP WHERE id = $2",
    )
    .bind(encrypted)
    .bind(session_id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

pub async fn load_protocol_context(
    state: &AppState,
    session_id: &str,
) -> Result<Option<LoginProtocolContext>, sqlx::Error> {
    let encrypted: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT protocol_context_encrypted FROM nga_login_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(encrypted) = encrypted else {
        return Ok(None);
    };
    let json = state
        .credential_cipher
        .decrypt_v2(&encrypted, LoginProtocolContext::aad(session_id).as_bytes())
        .map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;
    let context = serde_json::from_str(&json).map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;
    Ok(Some(context))
}

pub async fn clear_protocol_context(state: &AppState, session_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE nga_login_sessions SET protocol_context_encrypted = NULL,
         updated_at = CURRENT_TIMESTAMP WHERE id = $1",
    )
    .bind(session_id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => database.is_unique_violation(),
        _ => false,
    }
}

/// Expire stale sessions (confirmation/captcha TTL) and clear their contexts.
pub async fn expire_stale_sessions(state: &AppState) -> Result<usize, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id FROM nga_login_sessions
         WHERE status IN ('awaiting_confirmation','starting','awaiting_captcha','submitting','validating_cookie')
           AND expires_at <= CURRENT_TIMESTAMP",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut count = 0;
    for row in rows {
        let id: String = row.get("id");
        if transition(
            state,
            &id,
            &[
                LoginSessionStatus::AwaitingConfirmation,
                LoginSessionStatus::Starting,
                LoginSessionStatus::AwaitingCaptcha,
                LoginSessionStatus::Submitting,
                LoginSessionStatus::ValidatingCookie,
            ],
            LoginSessionStatus::Expired,
            Some("expired"),
        )
        .await?
        {
            clear_protocol_context(state, &id).await?;
            count += 1;
        }
    }
    Ok(count)
}

/// The verified candidate cookie replaces the old one and auth-paused watches
/// are restored — all in one transaction. Never called with an unverified
/// candidate.
pub async fn complete_success(
    state: &AppState,
    session_id: &str,
    account_id: &str,
    passport_uid: &str,
    passport_cid: &str,
) -> Result<usize, sqlx::Error> {
    let uid_encrypted = state
        .credential_cipher
        .encrypt(passport_uid)
        .map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;
    let cid_encrypted = state
        .credential_cipher
        .encrypt(passport_cid)
        .map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;

    let mut tx = state.pool.begin().await?;
    // 1. Replace the Cookie and mark the account valid.
    sqlx::query(
        "UPDATE nga_accounts SET passport_uid_encrypted = $1, passport_cid_encrypted = $2,
         status = 'valid', last_auth_checked_at = CURRENT_TIMESTAMP,
         last_auth_error_kind = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = $3",
    )
    .bind(uid_encrypted)
    .bind(cid_encrypted)
    .bind(account_id)
    .execute(&mut *tx)
    .await?;
    // 2. Reset renewal bookkeeping.
    sqlx::query(
        "UPDATE nga_account_renewal_settings SET credential_status = 'ready',
         consecutive_failure_count = 0, cooldown_until = NULL, last_renewal_at = CURRENT_TIMESTAMP,
         last_error_kind = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE account_id = $1",
    )
    .bind(account_id)
    .execute(&mut *tx)
    .await?;
    // 3. Resolve the credential alert.
    notification::alerts::resolve_nga_credentials_invalid_alert_tx(&mut tx).await?;
    // 4. Restore only auth-paused watches; user-paused watches stay paused.
    let restored = sqlx::query(
        "UPDATE watch_targets SET enabled = 1, status = 'pending',
         pause_reason = NULL, next_run_at = CURRENT_TIMESTAMP,
         updated_at = CURRENT_TIMESTAMP
         WHERE deleted_at IS NULL AND pause_reason = 'auth'",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // 5. Mark the session succeeded and clear the protocol context.
    sqlx::query(
        "UPDATE nga_login_sessions SET status = 'succeeded', protocol_context_encrypted = NULL,
         last_error_kind = NULL, completed_at = CURRENT_TIMESTAMP,
         updated_at = CURRENT_TIMESTAMP WHERE id = $1",
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(restored as usize)
}

fn session_from_row(row: sqlx::any::AnyRow) -> Result<LoginSession, sqlx::Error> {
    Ok(LoginSession {
        id: row.get("id"),
        account_id: row.get("account_id"),
        bot_binding_id: row.get("bot_binding_id"),
        integration_id: row.get("integration_id"),
        actor_id: row.get("actor_id"),
        conversation_id: row.get("conversation_id"),
        status: LoginSessionStatus::from_str(&row.get::<String, _>("status"))
            .ok_or_else(|| sqlx::Error::Protocol("invalid_session_status".to_owned()))?,
        captcha_attempt_count: row.get("captcha_attempt_count"),
        last_error_kind: row.get("last_error_kind"),
    })
}

impl LoginSessionStatus {
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "awaiting_confirmation" => Some(Self::AwaitingConfirmation),
            "starting" => Some(Self::Starting),
            "awaiting_captcha" => Some(Self::AwaitingCaptcha),
            "submitting" => Some(Self::Submitting),
            "validating_cookie" => Some(Self::ValidatingCookie),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "expired" => Some(Self::Expired),
            "unsupported_challenge" => Some(Self::UnsupportedChallenge),
            _ => None,
        }
    }
}

/// Renewal credentials decrypted only inside a login flow, never persisted or
/// logged.
#[derive(Clone, Debug)]
pub struct RenewalCredentials {
    pub login_name: String,
    pub password: secrecy::SecretString,
}

pub async fn load_renewal_credentials(
    state: &AppState,
    account_id: &str,
) -> Result<Option<RenewalCredentials>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT login_name_encrypted, password_encrypted
         FROM nga_account_renewal_settings WHERE account_id = $1 AND enabled = 1",
    )
    .bind(account_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let login_encrypted: Vec<u8> = row.get("login_name_encrypted");
    let password_encrypted: Vec<u8> = row.get("password_encrypted");
    let login_name = state
        .credential_cipher
        .decrypt_v2(
            &login_encrypted,
            format!("nga_account:{account_id}:renewal_login:v2").as_bytes(),
        )
        .map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;
    let password = state
        .credential_cipher
        .decrypt_v2(
            &password_encrypted,
            format!("nga_account:{account_id}:renewal_password:v2").as_bytes(),
        )
        .map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;
    Ok(Some(RenewalCredentials {
        login_name,
        password: secrecy::SecretString::from(password),
    }))
}

/// Record a login failure on the renewal settings.
/// `permanent_invalid` (wrong password/account) sets `credential_status =
/// 'invalid'`, which requires admin replacement to recover; everything else
/// backs off with an exponential cooldown.
pub async fn mark_renewal_failure(
    state: &AppState,
    account_id: &str,
    error_kind: &str,
    permanent_invalid: bool,
) -> Result<(), sqlx::Error> {
    if permanent_invalid {
        sqlx::query(
            "UPDATE nga_account_renewal_settings SET credential_status = 'invalid',
             consecutive_failure_count = consecutive_failure_count + 1,
             last_error_kind = $1, updated_at = CURRENT_TIMESTAMP
             WHERE account_id = $2",
        )
        .bind(error_kind)
        .bind(account_id)
        .execute(&state.pool)
        .await?;
        return Ok(());
    }
    // Exponential cooldown: 15min, 30min, then capped at 30min per failure.
    let next = sqlx::query(
        "SELECT consecutive_failure_count FROM nga_account_renewal_settings
         WHERE account_id = $1",
    )
    .bind(account_id)
    .fetch_optional(&state.pool)
    .await?;
    let failures: i32 = next
        .as_ref()
        .map(|row| row.get::<i32, _>("consecutive_failure_count"))
        .unwrap_or(0);
    let minutes = (FAILURE_COOLDOWN.whole_seconds() / 60)
        .min(MAX_COOLDOWN.whole_seconds() / 60)
        .max(15)
        .min(MAX_COOLDOWN.whole_seconds() / 60);
    let cooldown_minutes = minutes * i64::from(failures.max(1)).min(2);
    let expires_sql = match state.config.database_backend {
        crate::config::DatabaseBackend::Postgres => {
            format!("CURRENT_TIMESTAMP + INTERVAL '{cooldown_minutes} minutes'")
        }
        crate::config::DatabaseBackend::Sqlite => {
            format!("datetime(CURRENT_TIMESTAMP, '+{cooldown_minutes} minutes')")
        }
    };
    sqlx::query(&format!(
        "UPDATE nga_account_renewal_settings SET credential_status = 'cooldown',
         consecutive_failure_count = consecutive_failure_count + 1,
         cooldown_until = {expires_sql},
         last_error_kind = $1, updated_at = CURRENT_TIMESTAMP
         WHERE account_id = $2"
    ))
    .bind(error_kind)
    .bind(account_id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Summary of the renewal configuration for /login status and the API.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RenewalSettingView {
    pub enabled: bool,
    pub credentials_configured: bool,
    pub credential_status: String,
    pub cooldown_until: Option<String>,
    pub last_renewal_at: Option<String>,
    pub last_error_kind: Option<String>,
}

pub async fn renewal_setting_view(
    state: &AppState,
    account_id: &str,
) -> Result<Option<RenewalSettingView>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT enabled, credential_status,
                CAST(cooldown_until AS TEXT) AS cooldown_until,
                CAST(last_renewal_at AS TEXT) AS last_renewal_at,
                last_error_kind
         FROM nga_account_renewal_settings WHERE account_id = $1",
    )
    .bind(account_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(RenewalSettingView {
        enabled: row.get::<i32, _>("enabled") == 1,
        credentials_configured: true,
        credential_status: row.get("credential_status"),
        cooldown_until: row.get("cooldown_until"),
        last_renewal_at: row.get("last_renewal_at"),
        last_error_kind: row.get("last_error_kind"),
    }))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, net::SocketAddr, sync::Arc};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use sqlx::Row;
    use tokio::sync::RwLock;

    use super::{LoginSessionStatus, complete_success, on_auth_failure, transition};
    use crate::{
        app::AppState,
        config::{
            AppConfig, AssetsConfig, DatabaseBackend, ObservabilityConfig, PersistenceConfig,
            SchedulerConfig,
        },
        crypto::CredentialCipher,
        nga::NgaClient,
        platform::integration::{BotRole, ConversationType},
    };

    async fn test_state() -> AppState {
        sqlx::any::install_default_drivers();
        let pool = sqlx::any::AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("db must connect");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("fk must enable");
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
                nga_user_agent: "test".to_owned(),
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
            nga_client: NgaClient::new("test".to_owned()).unwrap(),
            admin_sessions: Arc::new(RwLock::new(HashSet::new())),
            platform_updates: tokio::sync::watch::channel(()).0,
        }
    }

    async fn seed_owner_binding(state: &AppState) -> String {
        sqlx::query(
            "INSERT INTO platform_integrations
             (id, platform, label, credentials_encrypted, bot_enabled)
             VALUES ('integration', 'feishu', 'app', X'00', 1)",
        )
        .execute(&state.pool)
        .await
        .expect("integration must insert");
        crate::platform::integration::insert_binding(
            state,
            "integration",
            "ou_owner",
            Some("oc_private"),
            ConversationType::Private,
            BotRole::Owner,
            "owner",
        )
        .await
        .expect("binding must insert")
    }

    #[tokio::test]
    async fn auth_failure_pauses_watches_and_notifies_owner_once() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO nga_accounts (id, label, passport_uid_encrypted, passport_cid_encrypted)
             VALUES ('acct', 'default', X'00', X'00')",
        )
        .execute(&state.pool)
        .await
        .expect("account must insert");
        for (index, (id, reason)) in [("w-auth", None), ("w-user", Some("user"))]
            .into_iter()
            .enumerate()
        {
            let watch_enabled = if id == "w-user" { 0 } else { 1 };
            sqlx::query(
                "INSERT INTO watch_targets (id, target_type, target_id, enabled, pause_reason)
                 VALUES ($1, 'thread', $3, $4, $2)",
            )
            .bind(id)
            .bind(reason)
            .bind(1001_i64 + i64::try_from(index).unwrap())
            .bind(watch_enabled)
            .execute(&state.pool)
            .await
            .expect("watch must insert");
        }
        let binding_id = seed_owner_binding(&state).await;
        sqlx::query(
            "INSERT INTO nga_account_renewal_settings
             (account_id, enabled, login_name_encrypted, password_encrypted, bot_binding_id)
             VALUES ('acct', 1, X'00', X'00', $1)",
        )
        .bind(&binding_id)
        .execute(&state.pool)
        .await
        .expect("renewal settings must insert");

        on_auth_failure(&state)
            .await
            .expect("auth failure must succeed");
        on_auth_failure(&state)
            .await
            .expect("second call must be idempotent");

        // Auth watch paused with reason auth; user-paused watch untouched.
        let row =
            sqlx::query("SELECT enabled, pause_reason FROM watch_targets WHERE id = 'w-auth'")
                .fetch_one(&state.pool)
                .await
                .expect("watch must exist");
        assert_eq!(row.get::<i32, _>("enabled"), 0);
        assert_eq!(row.get::<String, _>("pause_reason"), "auth");
        let row =
            sqlx::query("SELECT enabled, pause_reason FROM watch_targets WHERE id = 'w-user'")
                .fetch_one(&state.pool)
                .await
                .expect("watch must exist");
        assert_eq!(row.get::<i32, _>("enabled"), 0);
        assert_eq!(row.get::<String, _>("pause_reason"), "user");

        // Exactly one active session and one confirmation outbox row.
        let sessions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nga_login_sessions WHERE status = 'awaiting_confirmation'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("count must work");
        assert_eq!(sessions, 1);
        let outbox: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM bot_outbox WHERE message_kind = 'text'")
                .fetch_one(&state.pool)
                .await
                .expect("count must work");
        assert_eq!(outbox, 1);
    }

    #[tokio::test]
    async fn complete_success_restores_only_auth_paused_watches() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO nga_accounts (id, label, passport_uid_encrypted, passport_cid_encrypted, status)
             VALUES ('acct', 'default', X'00', X'00', 'paused')",
        )
        .execute(&state.pool)
        .await
        .expect("account must insert");
        let binding_id = seed_owner_binding(&state).await;
        for (index, (id, reason)) in [("w-auth", Some("auth")), ("w-user", Some("user"))]
            .into_iter()
            .enumerate()
        {
            sqlx::query(
                "INSERT INTO watch_targets (id, target_type, target_id, enabled, pause_reason)
                 VALUES ($1, 'thread', $3, 0, $2)",
            )
            .bind(id)
            .bind(reason)
            .bind(1001_i64 + i64::try_from(index).unwrap())
            .execute(&state.pool)
            .await
            .expect("watch must insert");
        }
        sqlx::query(
            "INSERT INTO nga_login_sessions
             (id, account_id, bot_binding_id, integration_id, actor_id, conversation_id,
              trigger_kind, status, expires_at)
             VALUES ('sess', 'acct', $1, 'integration', 'ou_owner', 'oc_private',
                      'cookie_invalid', 'validating_cookie', '2099-01-01 00:00:00')",
        )
        .bind(&binding_id)
        .execute(&state.pool)
        .await
        .expect("session must insert");

        let restored = complete_success(&state, "sess", "acct", "123456", "new-cid")
            .await
            .expect("success must commit");
        assert_eq!(restored, 1);

        let row =
            sqlx::query("SELECT enabled, pause_reason FROM watch_targets WHERE id = 'w-auth'")
                .fetch_one(&state.pool)
                .await
                .expect("watch must exist");
        assert_eq!(row.get::<i32, _>("enabled"), 1);
        assert!(row.get::<Option<String>, _>("pause_reason").is_none());
        let row =
            sqlx::query("SELECT enabled, pause_reason FROM watch_targets WHERE id = 'w-user'")
                .fetch_one(&state.pool)
                .await
                .expect("watch must exist");
        assert_eq!(row.get::<i32, _>("enabled"), 0);
        assert_eq!(row.get::<String, _>("pause_reason"), "user");

        let status: String =
            sqlx::query_scalar("SELECT status FROM nga_login_sessions WHERE id = 'sess'")
                .fetch_one(&state.pool)
                .await
                .expect("session must exist");
        assert_eq!(status, "succeeded");
        let account_status: String =
            sqlx::query_scalar("SELECT status FROM nga_accounts WHERE id = 'acct'")
                .fetch_one(&state.pool)
                .await
                .expect("account must exist");
        assert_eq!(account_status, "valid");
    }

    #[tokio::test]
    async fn status_transition_is_conditional_and_once_only() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO nga_accounts (id, label, passport_uid_encrypted, passport_cid_encrypted)
             VALUES ('acct', 'default', X'00', X'00')",
        )
        .execute(&state.pool)
        .await
        .expect("account must insert");
        let binding_id = seed_owner_binding(&state).await;
        sqlx::query(
            "INSERT INTO nga_login_sessions
             (id, account_id, bot_binding_id, integration_id, actor_id, conversation_id,
              trigger_kind, status, expires_at)
             VALUES ('sess', 'acct', $1, 'integration', 'ou_owner', 'oc_private',
                      'cookie_invalid', 'awaiting_confirmation', '2099-01-01 00:00:00')",
        )
        .bind(&binding_id)
        .execute(&state.pool)
        .await
        .expect("session must insert");

        assert!(
            transition(
                &state,
                "sess",
                &[LoginSessionStatus::AwaitingConfirmation],
                LoginSessionStatus::Starting,
                None,
            )
            .await
            .expect("transition must work")
        );
        // Repeating the same transition must not move a session that already
        // left `awaiting_confirmation`.
        assert!(
            !transition(
                &state,
                "sess",
                &[LoginSessionStatus::AwaitingConfirmation],
                LoginSessionStatus::Starting,
                None,
            )
            .await
            .expect("transition must work")
        );
        let status: String =
            sqlx::query_scalar("SELECT status FROM nga_login_sessions WHERE id = 'sess'")
                .fetch_one(&state.pool)
                .await
                .expect("session must exist");
        assert_eq!(status, "starting");
    }
}
