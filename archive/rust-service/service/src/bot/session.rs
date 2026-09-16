#![allow(dead_code)] // design-contract APIs; wired by login command and renewal API
//! NGA login session repository and the auth-failure / renewal-success flows.
//! Raw passwords, captcha answers, candidate cookies and protocol contexts are
//! never persisted in plaintext; contexts are v2-encrypted with field AAD.

use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    app::AppState,
    bot::domain::{ImagePayload, TextPayload},
    notification,
};

pub const CONFIRMATION_TTL: time::Duration = time::Duration::seconds(15 * 60);
pub const MAX_CAPTCHA_ATTEMPTS: i32 = 3;
pub const FAILURE_COOLDOWN: time::Duration = time::Duration::seconds(15 * 60);
pub const MAX_COOLDOWN: time::Duration = time::Duration::seconds(30 * 60);
const MAX_COOKIE_HEADER_LENGTH: usize = 16_384;

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
    pub expires_at: String,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginTriggerKind {
    CookieInvalid,
    Manual,
}

impl LoginTriggerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::CookieInvalid => "cookie_invalid",
            Self::Manual => "manual",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenewalConfirmation {
    pub session_id: String,
    pub created: bool,
}

impl LoginProtocolContext {
    pub fn aad(session_id: &str) -> String {
        format!("nga_login_session:{session_id}:protocol_context:v2")
    }

    pub fn is_current_and_unexpired(&self) -> bool {
        if self.protocol_version != "nga_web_login_v1" {
            return false;
        }
        time::OffsetDateTime::parse(
            &self.expires_at,
            &time::format_description::well_known::Rfc3339,
        )
        .is_ok_and(|expires_at| expires_at > time::OffsetDateTime::now_utc())
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

    // 2. Pause every currently-enabled watch with reason 'auth'. Collectors
    //    record the watch that observed Unauthorized before calling this
    //    function, so also adopt that just-disabled watch when it has no
    //    explicit pause reason. User-paused watches keep their own reason and
    //    are never auto-restored.
    sqlx::query(
        "UPDATE watch_targets SET enabled = 0, status = 'paused', pause_reason = 'auth',
         lease_until = NULL, lease_token = NULL,
         updated_at = CURRENT_TIMESTAMP
         WHERE deleted_at IS NULL AND (
             enabled = 1 OR (
                 enabled = 0 AND status = 'paused' AND pause_reason IS NULL
                 AND last_error_kind = 'unauthorized'
             )
         )",
    )
    .execute(&state.pool)
    .await?;

    // 3. Create or reopen the alert.
    notification::alerts::ensure_nga_credentials_invalid_alert(state).await?;

    // 4. Try to open a renewal session for the owner. Authentication failure
    // handling remains successful when renewal is not configured; the alert
    // and paused state above still need to be committed.
    let _ = enqueue_renewal_confirmation(state, LoginTriggerKind::CookieInvalid).await?;
    Ok(())
}

/// Explicitly request a renewal confirmation without marking the current
/// Cookie invalid or pausing watches. Returns `None` when renewal is disabled,
/// cooling down, or no valid owner-private binding is available.
pub async fn request_manual_renewal(
    state: &AppState,
) -> Result<Option<RenewalConfirmation>, sqlx::Error> {
    enqueue_renewal_confirmation(state, LoginTriggerKind::Manual).await
}

async fn enqueue_renewal_confirmation(
    state: &AppState,
    trigger: LoginTriggerKind,
) -> Result<Option<RenewalConfirmation>, sqlx::Error> {
    sqlx::query(
        "UPDATE nga_account_renewal_settings SET credential_status = 'ready',
         cooldown_until = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE credential_status = 'cooldown' AND cooldown_until <= CURRENT_TIMESTAMP",
    )
    .execute(&state.pool)
    .await?;
    let renewal = sqlx::query(
        "SELECT r.account_id, r.bot_binding_id,
                b.integration_id, b.actor_id, b.conversation_id
         FROM nga_account_renewal_settings r
         JOIN bot_bindings b ON b.id = r.bot_binding_id
         JOIN platform_integrations i ON i.id = b.integration_id
         JOIN nga_accounts a ON a.id = r.account_id
         WHERE a.label = 'default' AND r.enabled = 1 AND r.credential_status = 'ready'
           AND b.enabled = 1 AND b.role = 'owner'
           AND b.conversation_type = 'private' AND b.conversation_id IS NOT NULL
           AND i.enabled = 1 AND i.bot_enabled = 1 AND i.platform = 'feishu'",
    )
    .fetch_optional(&state.pool)
    .await?;
    let Some(renewal) = renewal else {
        return Ok(None);
    };
    let conversation_id: Option<String> = renewal.get("conversation_id");
    let Some(conversation_id) = conversation_id else {
        return Ok(None);
    };
    let account_id: String = renewal.get("account_id");
    let bot_binding_id: String = renewal.get("bot_binding_id");
    let integration_id: String = renewal.get("integration_id");
    let actor_id: String = renewal.get("actor_id");

    if let Some(existing_id) = sqlx::query_scalar::<_, String>(
        "SELECT id FROM nga_login_sessions
         WHERE account_id = $1 AND status IN
           ('awaiting_confirmation','starting','awaiting_captcha','submitting','validating_cookie')
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&account_id)
    .fetch_optional(&state.pool)
    .await?
    {
        return Ok(Some(RenewalConfirmation {
            session_id: existing_id,
            created: false,
        }));
    }

    // Only one active session per account (partial unique index guards races).
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
    let reason = match trigger {
        LoginTriggerKind::CookieInvalid => "NGA Cookie 已失效，所有监控已暂停。",
        LoginTriggerKind::Manual => "已从管理台手动发起 NGA Cookie 续期。",
    };
    let confirmation = serde_json::to_string(&TextPayload {
        text: format!(
            "{reason}\n确认续期请回复：\n`/login confirm {id}`\n取消请回复：\n`/login cancel {id}`\n（15 分钟内有效）"
        ),
    })
    .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let confirmation_encrypted = state
        .credential_cipher
        .encrypt(&confirmation)
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let mut tx = state.pool.begin().await?;
    let inserted = sqlx::query(&format!(
        "INSERT INTO nga_login_sessions
         (id, account_id, bot_binding_id, integration_id, actor_id, conversation_id,
          trigger_kind, status, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'awaiting_confirmation', {expires_sql})"
    ))
    .bind(&id)
    .bind(&account_id)
    .bind(&bot_binding_id)
    .bind(&integration_id)
    .bind(&actor_id)
    .bind(&conversation_id)
    .bind(trigger.as_str())
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(result) if result.rows_affected() == 1 => {}
        // The one-active-session partial unique index fired: an active
        // session already exists, nothing else to do.
        Ok(_) => {
            tx.rollback().await?;
            return active_confirmation(state, &account_id).await;
        }
        Err(error) if is_unique_violation(&error) => {
            tx.rollback().await?;
            return active_confirmation(state, &account_id).await;
        }
        Err(error) => return Err(error),
    }

    // Persist the owner notification in the same transaction.
    let dedupe_key = format!("login:{id}:confirmation");
    sqlx::query(&format!(
        "INSERT INTO bot_outbox
         (id, dedupe_key, integration_id, conversation_id, message_kind,
          payload_encrypted, expires_at)
         VALUES ($1, $2, $3, $4, 'text', $5, {expires_sql})"
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(dedupe_key)
    .bind(&integration_id)
    .bind(&conversation_id)
    .bind(confirmation_encrypted)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(RenewalConfirmation {
        session_id: id,
        created: true,
    }))
}

async fn active_confirmation(
    state: &AppState,
    account_id: &str,
) -> Result<Option<RenewalConfirmation>, sqlx::Error> {
    let id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM nga_login_sessions
         WHERE account_id = $1 AND status IN
           ('awaiting_confirmation','starting','awaiting_captcha','submitting','validating_cookie')
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(account_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(id.map(|session_id| RenewalConfirmation {
        session_id,
        created: false,
    }))
}

pub async fn get_session(state: &AppState, id: &str) -> Result<Option<LoginSession>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, account_id, bot_binding_id, integration_id, actor_id,
                conversation_id, status, captcha_attempt_count, last_error_kind,
                CAST(expires_at AS TEXT) AS expires_at
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
                conversation_id, status, captcha_attempt_count, last_error_kind,
                CAST(expires_at AS TEXT) AS expires_at
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
         WHERE id = $3 AND status IN ({from_list})
           AND ($1 = 'expired' OR expires_at > CURRENT_TIMESTAMP)"
    );
    let affected = sqlx::query(&query)
        .bind(to.as_str())
        .bind(last_error_kind)
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if affected > 0
        && matches!(
            to,
            LoginSessionStatus::Succeeded
                | LoginSessionStatus::Failed
                | LoginSessionStatus::Cancelled
                | LoginSessionStatus::Expired
                | LoginSessionStatus::UnsupportedChallenge
        )
    {
        invalidate_login_outbox(state, id).await?;
    }
    Ok(affected > 0)
}

/// Persist a captcha protocol context, enqueue its encrypted image and move
/// the session to `awaiting_captcha` atomically. This prevents a crash from
/// leaving an image without context (or context without a deliverable image).
pub async fn store_challenge_and_enqueue(
    state: &AppState,
    session: &LoginSession,
    from: LoginSessionStatus,
    context: &LoginProtocolContext,
    image: ImagePayload,
    expires_at: time::OffsetDateTime,
) -> Result<bool, sqlx::Error> {
    let context_json =
        serde_json::to_string(context).map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let context_encrypted = state
        .credential_cipher
        .encrypt_v2(
            &context_json,
            LoginProtocolContext::aad(&session.id).as_bytes(),
        )
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let image_json =
        serde_json::to_string(&image).map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let image_encrypted = state
        .credential_cipher
        .encrypt(&image_json)
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let expires = expires_at
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let expires_sql = match state.config.database_backend {
        crate::config::DatabaseBackend::Postgres => format!("'{expires}'::timestamptz"),
        crate::config::DatabaseBackend::Sqlite => format!("'{expires}'"),
    };
    let dedupe_key = format!("login:{}:captcha:{}", session.id, context.captcha_revision);
    let mut tx = state.pool.begin().await?;
    let updated = sqlx::query(&format!(
        "UPDATE nga_login_sessions SET status = 'awaiting_captcha',
         challenge_kind = 'image', protocol_context_encrypted = $1,
         expires_at = {expires_sql}, last_error_kind = NULL,
         updated_at = CURRENT_TIMESTAMP
         WHERE id = $2 AND status = $3 AND expires_at > CURRENT_TIMESTAMP"
    ))
    .bind(context_encrypted)
    .bind(&session.id)
    .bind(from.as_str())
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query(&format!(
        "INSERT INTO bot_outbox
         (id, dedupe_key, integration_id, conversation_id, message_kind,
          payload_encrypted, expires_at)
         VALUES ($1, $2, $3, $4, 'image', $5, {expires_sql})
         ON CONFLICT (dedupe_key) DO NOTHING"
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(dedupe_key)
    .bind(&session.integration_id)
    .bind(&session.conversation_id)
    .bind(image_encrypted)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn invalidate_login_outbox(
    state: &AppState,
    session_id: &str,
) -> Result<(), sqlx::Error> {
    let clear_payload = match state.config.database_backend {
        crate::config::DatabaseBackend::Postgres => "'\\x00'::bytea",
        crate::config::DatabaseBackend::Sqlite => "X'00'",
    };
    sqlx::query(&format!(
        "UPDATE bot_outbox SET status = 'dead', payload_encrypted = {clear_payload},
         lease_until = NULL, last_error_kind = 'login_session_closed'
         WHERE dedupe_key LIKE $1 AND status IN ('pending','failed','sending')"
    ))
    .bind(format!("login:{session_id}:%"))
    .execute(&state.pool)
    .await?;
    Ok(())
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
/// are restored — all in one transaction. The session is claimed first, so a
/// concurrent cancellation/expiry that wins the race prevents every credential
/// write. Returns `None` when the session is no longer eligible. Never called
/// with an unverified candidate.
pub async fn complete_success(
    state: &AppState,
    session_id: &str,
    account_id: &str,
    passport_uid: &str,
    passport_cid: &str,
    candidate_cookie_header: &str,
) -> Result<Option<usize>, sqlx::Error> {
    let uid_encrypted = state
        .credential_cipher
        .encrypt(passport_uid)
        .map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;
    let cid_encrypted = state
        .credential_cipher
        .encrypt(passport_cid)
        .map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;
    let mut tx = state.pool.begin().await?;
    let claimed = sqlx::query(
        "UPDATE nga_login_sessions SET status = 'succeeded',
         protocol_context_encrypted = NULL, last_error_kind = NULL,
         completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND account_id = $2 AND status = 'validating_cookie'
           AND expires_at > CURRENT_TIMESTAMP",
    )
    .bind(session_id)
    .bind(account_id)
    .execute(&mut *tx)
    .await?;
    if claimed.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(None);
    }

    let existing_cookie: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT cookie_encrypted FROM nga_accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(&mut *tx)
            .await?;
    let existing_cookie = existing_cookie
        .map(|value| {
            state
                .credential_cipher
                .decrypt(&value)
                .map_err(|e| sqlx::Error::Protocol(format!("{e}")))
        })
        .transpose()?;
    let cookie = merge_cookie_headers(
        existing_cookie.as_deref(),
        candidate_cookie_header,
        passport_uid,
        passport_cid,
    );
    if cookie.is_empty()
        || cookie.len() > MAX_COOKIE_HEADER_LENGTH
        || cookie.chars().any(char::is_control)
    {
        return Err(sqlx::Error::Protocol(
            "invalid renewed cookie header".to_owned(),
        ));
    }
    let cookie_encrypted = state
        .credential_cipher
        .encrypt(&cookie)
        .map_err(|e| sqlx::Error::Protocol(format!("{e}")))?;

    // 1. Replace the Cookie and mark the account valid.
    sqlx::query(
        "UPDATE nga_accounts SET passport_uid_encrypted = $1, passport_cid_encrypted = $2,
         cookie_encrypted = $3,
         status = 'valid', last_auth_checked_at = CURRENT_TIMESTAMP,
         last_auth_error_kind = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = $4",
    )
    .bind(uid_encrypted)
    .bind(cid_encrypted)
    .bind(cookie_encrypted)
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
    // 5. Clear queued protocol messages for the claimed session.
    let clear_payload = match state.config.database_backend {
        crate::config::DatabaseBackend::Postgres => "'\\x00'::bytea",
        crate::config::DatabaseBackend::Sqlite => "X'00'",
    };
    sqlx::query(&format!(
        "UPDATE bot_outbox SET status = 'dead', payload_encrypted = {clear_payload},
         lease_until = NULL, last_error_kind = 'login_session_closed'
         WHERE dedupe_key LIKE $1 AND status IN ('pending','failed','sending')"
    ))
    .bind(format!("login:{session_id}:%"))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(restored as usize))
}

fn merge_cookie_headers(
    existing_cookie: Option<&str>,
    candidate_cookie: &str,
    passport_uid: &str,
    passport_cid: &str,
) -> String {
    let mut parts = Vec::<(String, String)>::new();
    for source in existing_cookie.into_iter().chain([candidate_cookie]) {
        for part in source.split(';') {
            let Some((name, value)) = part.trim().split_once('=') else {
                continue;
            };
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() || value.is_empty() {
                continue;
            }
            if let Some(existing) = parts.iter_mut().find(|(saved, _)| saved == name) {
                existing.1 = value.to_owned();
            } else {
                parts.push((name.to_owned(), value.to_owned()));
            }
        }
    }
    for (name, value) in [
        ("ngaPassportUid", passport_uid),
        ("ngaPassportCid", passport_cid),
    ] {
        if let Some(existing) = parts.iter_mut().find(|(saved, _)| saved == name) {
            existing.1 = value.to_owned();
        } else {
            parts.push((name.to_owned(), value.to_owned()));
        }
    }
    parts
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
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
        expires_at: row.get("expires_at"),
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

    use super::{
        LoginSessionStatus, complete_success, merge_cookie_headers, on_auth_failure,
        request_manual_renewal, transition,
    };

    #[test]
    fn cookie_renewal_replaces_passport_values_and_preserves_other_cookies() {
        assert_eq!(
            merge_cookie_headers(
                Some("session=keep; ngaPassportUid=old; ngaPassportCid=old-cid"),
                "login_session=fresh; ngaPassportCid=candidate-cid",
                "new",
                "new-cid",
            ),
            "session=keep; ngaPassportUid=new; ngaPassportCid=new-cid; login_session=fresh"
        );
    }

    #[test]
    fn cookie_renewal_builds_a_full_cookie_without_an_existing_cookie() {
        assert_eq!(
            merge_cookie_headers(
                None,
                "login_session=fresh; ngaPassportUid=candidate; ngaPassportCid=candidate-cid",
                "new",
                "new-cid",
            ),
            "login_session=fresh; ngaPassportUid=new; ngaPassportCid=new-cid"
        );
    }
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
        sqlx::query(
            "INSERT INTO watch_targets
             (id, target_type, target_id, enabled, status, pause_reason, last_error_kind)
             VALUES ('w-collector', 'thread', 1003, 0, 'paused', NULL, 'unauthorized')",
        )
        .execute(&state.pool)
        .await
        .expect("collector-paused watch must insert");
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
            sqlx::query("SELECT enabled, pause_reason FROM watch_targets WHERE id = 'w-collector'")
                .fetch_one(&state.pool)
                .await
                .expect("collector-paused watch must exist");
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
    async fn auth_failure_notification_can_be_retried_after_renewal_is_configured() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO nga_accounts (id, label, passport_uid_encrypted, passport_cid_encrypted)
             VALUES ('acct', 'default', X'00', X'00')",
        )
        .execute(&state.pool)
        .await
        .expect("account must insert");

        on_auth_failure(&state)
            .await
            .expect("auth failure without renewal must still be recorded");
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nga_login_sessions")
            .fetch_one(&state.pool)
            .await
            .expect("session count must work");
        assert_eq!(before, 0);

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
            .expect("auth failure must be retryable after configuration");

        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nga_login_sessions")
            .fetch_one(&state.pool)
            .await
            .expect("session count must work");
        let outbox: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bot_outbox")
            .fetch_one(&state.pool)
            .await
            .expect("outbox count must work");
        assert_eq!(sessions, 1);
        assert_eq!(outbox, 1);
    }

    #[tokio::test]
    async fn manual_renewal_notifies_once_without_pausing_watches() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO nga_accounts (id, label, passport_uid_encrypted, passport_cid_encrypted)
             VALUES ('acct', 'default', X'00', X'00')",
        )
        .execute(&state.pool)
        .await
        .expect("account must insert");
        sqlx::query(
            "INSERT INTO watch_targets (id, target_type, target_id, enabled)
             VALUES ('watch', 'thread', 1001, 1)",
        )
        .execute(&state.pool)
        .await
        .expect("watch must insert");
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

        let first = request_manual_renewal(&state)
            .await
            .expect("manual renewal must succeed")
            .expect("renewal must be available");
        let second = request_manual_renewal(&state)
            .await
            .expect("repeated manual renewal must succeed")
            .expect("active session must be returned");

        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.session_id, second.session_id);
        let trigger: String =
            sqlx::query_scalar("SELECT trigger_kind FROM nga_login_sessions WHERE id = $1")
                .bind(&first.session_id)
                .fetch_one(&state.pool)
                .await
                .expect("session must exist");
        assert_eq!(trigger, "manual");
        let watch_enabled: i32 =
            sqlx::query_scalar("SELECT enabled FROM watch_targets WHERE id = 'watch'")
                .fetch_one(&state.pool)
                .await
                .expect("watch must exist");
        assert_eq!(watch_enabled, 1);
        let outbox: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bot_outbox")
            .fetch_one(&state.pool)
            .await
            .expect("outbox count must work");
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

        let restored = complete_success(
            &state,
            "sess",
            "acct",
            "123456",
            "new-cid",
            "login_session=fresh; ngaPassportUid=123456; ngaPassportCid=new-cid",
        )
        .await
        .expect("success must commit");
        assert_eq!(restored, Some(1));

        let encrypted_cookie: Vec<u8> =
            sqlx::query_scalar("SELECT cookie_encrypted FROM nga_accounts WHERE id = 'acct'")
                .fetch_one(&state.pool)
                .await
                .expect("renewed full cookie must be stored");
        let saved_cookie = state
            .credential_cipher
            .decrypt(&encrypted_cookie)
            .expect("saved cookie must decrypt");
        assert_eq!(
            saved_cookie,
            "login_session=fresh; ngaPassportUid=123456; ngaPassportCid=new-cid"
        );

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
    async fn complete_success_rejects_cancelled_expired_or_wrong_account_without_writes() {
        let state = test_state().await;
        let original_cookie = "session=keep; ngaPassportUid=old; ngaPassportCid=old-cid";
        let encrypted_cookie = state
            .credential_cipher
            .encrypt(original_cookie)
            .expect("old cookie must encrypt");
        sqlx::query(
            "INSERT INTO nga_accounts
             (id, label, passport_uid_encrypted, passport_cid_encrypted, cookie_encrypted, status)
             VALUES ('acct', 'default', X'00', X'00', $1, 'paused')",
        )
        .bind(encrypted_cookie)
        .execute(&state.pool)
        .await
        .expect("account must insert");
        sqlx::query(
            "INSERT INTO watch_targets
             (id, target_type, target_id, enabled, status, pause_reason)
             VALUES ('w-auth', 'thread', 1001, 0, 'paused', 'auth')",
        )
        .execute(&state.pool)
        .await
        .expect("watch must insert");
        let binding_id = seed_owner_binding(&state).await;
        sqlx::query(
            "INSERT INTO nga_login_sessions
             (id, account_id, bot_binding_id, integration_id, actor_id, conversation_id,
              trigger_kind, status, expires_at)
             VALUES ('sess', 'acct', $1, 'integration', 'ou_owner', 'oc_private',
                     'cookie_invalid', 'cancelled', '2099-01-01 00:00:00')",
        )
        .bind(binding_id)
        .execute(&state.pool)
        .await
        .expect("session must insert");

        let candidate = "login_session=fresh; ngaPassportUid=123456; ngaPassportCid=new-cid";
        let cancelled = complete_success(&state, "sess", "acct", "123456", "new-cid", candidate)
            .await
            .expect("cancelled session must be rejected cleanly");
        assert_eq!(cancelled, None);

        sqlx::query(
            "UPDATE nga_login_sessions SET status = 'validating_cookie',
             expires_at = '2000-01-01 00:00:00' WHERE id = 'sess'",
        )
        .execute(&state.pool)
        .await
        .expect("session must become expired in-flight");
        let expired = complete_success(&state, "sess", "acct", "123456", "new-cid", candidate)
            .await
            .expect("expired session must be rejected cleanly");
        assert_eq!(expired, None);

        sqlx::query(
            "UPDATE nga_login_sessions SET expires_at = '2099-01-01 00:00:00'
             WHERE id = 'sess'",
        )
        .execute(&state.pool)
        .await
        .expect("session must become live for account ownership check");
        let wrong_account = complete_success(
            &state,
            "sess",
            "different-account",
            "123456",
            "new-cid",
            candidate,
        )
        .await
        .expect("wrong account must be rejected cleanly");
        assert_eq!(wrong_account, None);

        let encrypted_cookie: Vec<u8> =
            sqlx::query_scalar("SELECT cookie_encrypted FROM nga_accounts WHERE id = 'acct'")
                .fetch_one(&state.pool)
                .await
                .expect("account must exist");
        let saved_cookie = state
            .credential_cipher
            .decrypt(&encrypted_cookie)
            .expect("saved cookie must decrypt");
        assert_eq!(saved_cookie, original_cookie);
        let account_status: String =
            sqlx::query_scalar("SELECT status FROM nga_accounts WHERE id = 'acct'")
                .fetch_one(&state.pool)
                .await
                .expect("account must exist");
        assert_eq!(account_status, "paused");
        let watch_enabled: i32 =
            sqlx::query_scalar("SELECT enabled FROM watch_targets WHERE id = 'w-auth'")
                .fetch_one(&state.pool)
                .await
                .expect("watch must exist");
        assert_eq!(watch_enabled, 0);
        let session_status: String =
            sqlx::query_scalar("SELECT status FROM nga_login_sessions WHERE id = 'sess'")
                .fetch_one(&state.pool)
                .await
                .expect("session must exist");
        assert_eq!(session_status, "validating_cookie");
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

    #[tokio::test]
    async fn expired_session_cannot_be_confirmed() {
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
             VALUES ('expired', 'acct', $1, 'integration', 'ou_owner', 'oc_private',
                     'cookie_invalid', 'awaiting_confirmation', '2000-01-01 00:00:00')",
        )
        .bind(binding_id)
        .execute(&state.pool)
        .await
        .expect("session must insert");

        assert!(
            !transition(
                &state,
                "expired",
                &[LoginSessionStatus::AwaitingConfirmation],
                LoginSessionStatus::Starting,
                None,
            )
            .await
            .expect("transition query must work")
        );
    }

    #[tokio::test]
    async fn invalid_or_cooling_credentials_do_not_open_login_session() {
        for (credential_status, cooldown_until) in
            [("invalid", None), ("cooldown", Some("2099-01-01 00:00:00"))]
        {
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
                "INSERT INTO nga_account_renewal_settings
                 (account_id, enabled, login_name_encrypted, password_encrypted,
                  bot_binding_id, credential_status, cooldown_until)
                 VALUES ('acct', 1, X'00', X'00', $1, $2, $3)",
            )
            .bind(binding_id)
            .bind(credential_status)
            .bind(cooldown_until)
            .execute(&state.pool)
            .await
            .expect("renewal settings must insert");

            on_auth_failure(&state)
                .await
                .expect("auth failure flow must complete");
            let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nga_login_sessions")
                .fetch_one(&state.pool)
                .await
                .expect("count must work");
            assert_eq!(sessions, 0, "status {credential_status} must block renewal");
        }
    }

    #[tokio::test]
    async fn pairing_token_consumption_and_binding_are_one_shot() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO platform_integrations
             (id, platform, label, credentials_encrypted, bot_enabled)
             VALUES ('integration', 'feishu', 'app', X'00', 1)",
        )
        .execute(&state.pool)
        .await
        .expect("integration must insert");
        let token =
            crate::platform::integration::create_pairing_token(&state, "integration", "owner", 600)
                .await
                .expect("token creation must work")
                .expect("integration supports pairing");
        let first = crate::platform::integration::consume_pairing_token_and_insert_binding(
            &state,
            "integration",
            &token.code,
            "ou_owner",
            "oc_private",
            "owner",
        )
        .await
        .expect("first consumption must work");
        assert!(first.is_some());
        let second = crate::platform::integration::consume_pairing_token_and_insert_binding(
            &state,
            "integration",
            &token.code,
            "ou_other",
            "oc_other",
            "other",
        )
        .await
        .expect("second consumption must not fail");
        assert!(second.is_none());
        let bindings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bot_bindings")
            .fetch_one(&state.pool)
            .await
            .expect("count must work");
        assert_eq!(bindings, 1);
    }

    #[tokio::test]
    async fn expired_bot_outbox_lease_is_reclaimed_and_terminally_classified() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO platform_integrations
             (id, platform, label, credentials_encrypted, bot_enabled)
             VALUES ('integration', 'feishu', 'app', X'00', 1)",
        )
        .execute(&state.pool)
        .await
        .expect("integration must insert");
        sqlx::query(
            "INSERT INTO bot_outbox
             (id, dedupe_key, integration_id, conversation_id, message_kind,
              payload_encrypted, status, lease_until)
             VALUES ('outbox', 'test:lease', 'integration', 'oc_private', 'text',
                     X'00', 'sending', '2000-01-01 00:00:00')",
        )
        .execute(&state.pool)
        .await
        .expect("outbox must insert");

        assert!(
            crate::notification::worker::process_one(&state)
                .await
                .expect("worker must reclaim the row")
        );
        let row = sqlx::query(
            "SELECT status, attempt_count, last_error_kind FROM bot_outbox WHERE id = 'outbox'",
        )
        .fetch_one(&state.pool)
        .await
        .expect("outbox must remain auditable");
        assert_eq!(row.get::<String, _>("status"), "dead");
        assert_eq!(row.get::<i32, _>("attempt_count"), 1);
        assert_eq!(
            row.get::<String, _>("last_error_kind"),
            "payload_decryption_failed"
        );
    }
}
