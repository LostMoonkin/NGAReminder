use axum::{Json, extract::State, http::StatusCode};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{
    app::AppState,
    bot::session::{self, LoginSessionStatus},
    nga::AuthCheckError,
    notification,
};

#[derive(Debug, Deserialize)]
pub struct SaveAccountRequest {
    #[serde(default)]
    passport_uid: Option<String>,
    #[serde(default)]
    passport_cid: Option<String>,
    #[serde(default)]
    cookie: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AccountResponse {
    configured: bool,
    passport_uid_masked: Option<String>,
    full_cookie_configured: bool,
    status: String,
    last_auth_checked_at: Option<String>,
    last_auth_error_kind: Option<String>,
    renewal_enabled: bool,
    renewal_credentials_configured: bool,
    renewal_bot_binding_configured: bool,
    renewal_bot_binding_id: Option<String>,
    renewal_credential_status: Option<String>,
    renewal_cooldown_until: Option<String>,
    last_renewal_at: Option<String>,
    last_renewal_error_kind: Option<String>,
    active_login_session: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRenewalRequest {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    login_name: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    bot_binding_id: Option<String>,
    #[serde(default)]
    clear_credentials: bool,
}

#[derive(Debug, Serialize)]
pub struct TestAccountResponse {
    valid: bool,
    uid: i64,
}

#[derive(Debug, Serialize)]
pub struct RenewalTestResponse {
    ok: bool,
    detail: String,
}

#[derive(Debug, Serialize)]
pub struct RenewalTriggerResponse {
    session_id: String,
    status: &'static str,
    created: bool,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    error: &'static str,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

const MAX_COOKIE_HEADER_LENGTH: usize = 16_384;

#[derive(Debug, PartialEq, Eq)]
struct SavedCredentials {
    passport_uid: String,
    passport_cid: String,
    cookie_header: Option<String>,
}

pub async fn get(State(state): State<AppState>) -> ApiResult<AccountResponse> {
    let row = sqlx::query(
        "SELECT id, passport_uid_encrypted, passport_cid_encrypted, cookie_encrypted, status,
         CAST(last_auth_checked_at AS TEXT) AS last_auth_checked_at, last_auth_error_kind
         FROM nga_accounts WHERE label = 'default'",
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(internal_error)?;

    let Some(row) = row else {
        return Ok(Json(unconfigured_response().await));
    };
    let encrypted: Vec<u8> = row.get("passport_uid_encrypted");
    let cid_encrypted: Vec<u8> = row.get("passport_cid_encrypted");
    let Ok(uid) = state.credential_cipher.decrypt(&encrypted) else {
        return Ok(Json(needs_configuration_response().await));
    };
    if state.credential_cipher.decrypt(&cid_encrypted).is_err() {
        return Ok(Json(needs_configuration_response().await));
    }
    let cookie_encrypted: Option<Vec<u8>> = row.get("cookie_encrypted");
    let full_cookie_configured = match cookie_encrypted {
        Some(value) => {
            if state.credential_cipher.decrypt(&value).is_err() {
                return Ok(Json(needs_configuration_response().await));
            }
            true
        }
        None => false,
    };
    let account_id: String = row.get("id");
    let renewal = renewal_summary(&state, &account_id).await;
    let active_session = active_login_session(&state, &account_id).await;

    Ok(Json(AccountResponse {
        configured: true,
        passport_uid_masked: Some(mask_uid(&uid)),
        full_cookie_configured,
        status: row.get("status"),
        last_auth_checked_at: row.get("last_auth_checked_at"),
        last_auth_error_kind: row.get("last_auth_error_kind"),
        renewal_enabled: renewal.0,
        renewal_credentials_configured: renewal.1,
        renewal_bot_binding_configured: renewal.2,
        renewal_bot_binding_id: renewal.3,
        renewal_credential_status: renewal.4,
        renewal_cooldown_until: renewal.5,
        last_renewal_at: renewal.6,
        last_renewal_error_kind: renewal.7,
        active_login_session: active_session,
    }))
}

pub async fn save(
    State(state): State<AppState>,
    Json(request): Json<SaveAccountRequest>,
) -> ApiResult<AccountResponse> {
    let credentials = extract_credentials(request).ok_or((
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "invalid_nga_credentials",
        }),
    ))?;
    if credentials.passport_uid.parse::<i64>().is_err()
        || credentials.passport_uid.len() > 20
        || credentials.passport_cid.trim().is_empty()
        || credentials.passport_cid.len() > 512
        || credentials
            .cookie_header
            .as_ref()
            .is_some_and(|cookie| cookie.len() > MAX_COOKIE_HEADER_LENGTH)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid_nga_credentials",
            }),
        ));
    }
    let uid_encrypted = state
        .credential_cipher
        .encrypt(&credentials.passport_uid)
        .map_err(|_| internal_api_error())?;
    let cid_encrypted = state
        .credential_cipher
        .encrypt(&credentials.passport_cid)
        .map_err(|_| internal_api_error())?;
    let cookie_encrypted = credentials
        .cookie_header
        .as_deref()
        .map(|cookie| state.credential_cipher.encrypt(cookie))
        .transpose()
        .map_err(|_| internal_api_error())?;

    sqlx::query(
        "INSERT INTO nga_accounts
            (label, passport_uid_encrypted, passport_cid_encrypted, cookie_encrypted)
         VALUES ('default', $1, $2, $3)
         ON CONFLICT (label) DO UPDATE SET
            passport_uid_encrypted = EXCLUDED.passport_uid_encrypted,
            passport_cid_encrypted = EXCLUDED.passport_cid_encrypted,
            cookie_encrypted = EXCLUDED.cookie_encrypted,
            status = 'unchecked',
            last_auth_checked_at = NULL,
            last_auth_error_kind = NULL,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(uid_encrypted)
    .bind(cid_encrypted)
    .bind(cookie_encrypted)
    .execute(&state.pool)
    .await
    .map_err(internal_error)?;

    let account_id: String =
        sqlx::query_scalar("SELECT id FROM nga_accounts WHERE label = 'default'")
            .fetch_one(&state.pool)
            .await
            .map_err(internal_error)?;
    let renewal = renewal_summary(&state, &account_id).await;
    let active_session = active_login_session(&state, &account_id).await;

    Ok(Json(AccountResponse {
        configured: true,
        passport_uid_masked: Some(mask_uid(&credentials.passport_uid)),
        full_cookie_configured: credentials.cookie_header.is_some(),
        status: "unchecked".to_owned(),
        last_auth_checked_at: None,
        last_auth_error_kind: None,
        renewal_enabled: renewal.0,
        renewal_credentials_configured: renewal.1,
        renewal_bot_binding_configured: renewal.2,
        renewal_bot_binding_id: renewal.3,
        renewal_credential_status: renewal.4,
        renewal_cooldown_until: renewal.5,
        last_renewal_at: renewal.6,
        last_renewal_error_kind: renewal.7,
        active_login_session: active_session,
    }))
}

/// PATCH /api/v1/settings/nga-account/renewal
pub async fn update_renewal(
    State(state): State<AppState>,
    Json(request): Json<UpdateRenewalRequest>,
) -> ApiResult<AccountResponse> {
    let account_row = sqlx::query("SELECT id FROM nga_accounts WHERE label = 'default'")
        .fetch_optional(&state.pool)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::PRECONDITION_FAILED,
            Json(ApiError {
                error: "nga_account_needs_configuration",
            }),
        ))?;
    let account_id: String = account_row.get("id");

    // Validate the binding when provided.
    if let Some(binding_id) = &request.bot_binding_id {
        let binding = sqlx::query(
            "SELECT b.id, b.role, b.conversation_type, b.conversation_id,
                    b.enabled AS binding_enabled, i.bot_enabled, i.enabled, i.platform
             FROM bot_bindings b
             JOIN platform_integrations i ON i.id = b.integration_id
             WHERE b.id = $1",
        )
        .bind(binding_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(internal_error)?;
        let Some(binding) = binding else {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "invalid_bot_binding",
                }),
            ));
        };
        let role: String = binding.get("role");
        let conversation_type: Option<String> = binding.get("conversation_type");
        let conversation_id: Option<String> = binding.get("conversation_id");
        let bot_enabled: i32 = binding.get("bot_enabled");
        let integration_enabled: i32 = binding.get("enabled");
        let binding_enabled: i32 = binding.get("binding_enabled");
        let platform: String = binding.get("platform");
        if role != "owner"
            || conversation_type.as_deref() != Some("private")
            || conversation_id.is_none()
            || bot_enabled == 0
            || integration_enabled == 0
            || binding_enabled == 0
            || platform != "feishu"
        {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "invalid_bot_binding",
                }),
            ));
        }
    }

    let existing =
        sqlx::query("SELECT enabled FROM nga_account_renewal_settings WHERE account_id = $1")
            .bind(&account_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal_error)?;
    let exists = existing.is_some();
    let currently_enabled = existing
        .as_ref()
        .map(|row| row.get::<i32, _>("enabled") == 1)
        .unwrap_or(false);
    let desired_enabled = request.enabled.unwrap_or(currently_enabled);

    // Clearing credentials requires renewal to be disabled at the same time.
    if request.clear_credentials && desired_enabled {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "cannot_clear_while_enabled",
            }),
        ));
    }
    if request.clear_credentials {
        if let Some(active) = session::active_session_for_account(&state, &account_id)
            .await
            .map_err(internal_error)?
        {
            let cancelled = session::transition(
                &state,
                &active.id,
                &[
                    LoginSessionStatus::AwaitingConfirmation,
                    LoginSessionStatus::Starting,
                    LoginSessionStatus::AwaitingCaptcha,
                    LoginSessionStatus::Submitting,
                    LoginSessionStatus::ValidatingCookie,
                ],
                LoginSessionStatus::Cancelled,
                Some("renewal_credentials_deleted"),
            )
            .await
            .map_err(internal_error)?;
            if cancelled {
                session::clear_protocol_context(&state, &active.id)
                    .await
                    .map_err(internal_error)?;
            }
        }
        sqlx::query("DELETE FROM nga_account_renewal_settings WHERE account_id = $1")
            .bind(&account_id)
            .execute(&state.pool)
            .await
            .map_err(internal_error)?;
        return get(State(state)).await;
    }

    if request
        .login_name
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
        || request
            .password
            .as_ref()
            .is_some_and(|value| value.is_empty())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid_renewal_credentials",
            }),
        ));
    }

    if !exists {
        // Creating the setting requires credentials + binding at once.
        if request.login_name.is_none()
            || request.password.is_none()
            || request.bot_binding_id.is_none()
        {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "renewal_requires_credentials_and_binding",
                }),
            ));
        }
    }

    let login_encrypted = request.login_name.as_ref().map(|value| {
        state
            .credential_cipher
            .encrypt_v2(
                value,
                format!("nga_account:{account_id}:renewal_login:v2").as_bytes(),
            )
            .map_err(|_| internal_api_error())
    });
    let password_encrypted = request.password.as_ref().map(|value| {
        state
            .credential_cipher
            .encrypt_v2(
                value,
                format!("nga_account:{account_id}:renewal_password:v2").as_bytes(),
            )
            .map_err(|_| internal_api_error())
    });
    let login_encrypted = login_encrypted.transpose()?;
    let password_encrypted = password_encrypted.transpose()?;

    if !exists {
        let login = login_encrypted.expect("validated above");
        let password = password_encrypted.expect("validated above");
        sqlx::query(
            "INSERT INTO nga_account_renewal_settings
             (account_id, enabled, login_name_encrypted, password_encrypted, bot_binding_id)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&account_id)
        .bind(i32::from(desired_enabled))
        .bind(login)
        .bind(password)
        .bind(request.bot_binding_id.as_deref())
        .execute(&state.pool)
        .await
        .map_err(internal_error)?;
    } else {
        let binding = request.bot_binding_id.as_deref();
        sqlx::query(
            "UPDATE nga_account_renewal_settings SET
             enabled = COALESCE($1, enabled),
             login_name_encrypted = COALESCE($2, login_name_encrypted),
             password_encrypted = COALESCE($3, password_encrypted),
             bot_binding_id = COALESCE($4, bot_binding_id),
             credential_status = CASE
                WHEN $2 IS NOT NULL OR $3 IS NOT NULL THEN 'ready'
                ELSE credential_status END,
             consecutive_failure_count = CASE
                WHEN $2 IS NOT NULL OR $3 IS NOT NULL THEN 0
                ELSE consecutive_failure_count END,
             cooldown_until = CASE
                WHEN $2 IS NOT NULL OR $3 IS NOT NULL THEN NULL ELSE cooldown_until END,
             last_error_kind = CASE
                WHEN $2 IS NOT NULL OR $3 IS NOT NULL THEN NULL ELSE last_error_kind END,
             updated_at = CURRENT_TIMESTAMP
             WHERE account_id = $5",
        )
        .bind(request.enabled.map(i32::from))
        .bind(login_encrypted)
        .bind(password_encrypted)
        .bind(binding)
        .bind(&account_id)
        .execute(&state.pool)
        .await
        .map_err(internal_error)?;
    }

    if desired_enabled {
        let account_status: String =
            sqlx::query_scalar("SELECT status FROM nga_accounts WHERE id = $1")
                .bind(&account_id)
                .fetch_one(&state.pool)
                .await
                .map_err(internal_error)?;
        if matches!(account_status.as_str(), "paused" | "invalid") {
            // Authentication may have failed before renewal credentials or a
            // bot owner binding were configured. Re-entering the idempotent
            // auth-failure flow here fills that notification gap.
            session::on_auth_failure(&state)
                .await
                .map_err(internal_error)?;
        }
    }

    get(State(state)).await
}

/// POST /api/v1/settings/nga-account/renewal/cancel
/// Admin-side cancellation of the active login session (mirrors the bot
/// `/login cancel` command).
pub async fn cancel_renewal(
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let account_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM nga_accounts WHERE label = 'default'")
            .fetch_optional(&state.pool)
            .await
            .map_err(internal_error)?;
    let Some(account_id) = account_id else {
        return Err((
            StatusCode::PRECONDITION_FAILED,
            Json(ApiError {
                error: "nga_account_needs_configuration",
            }),
        ));
    };
    let session = session::active_session_for_account(&state, &account_id)
        .await
        .map_err(internal_error)?;
    let Some(session) = session else {
        return Ok(StatusCode::NO_CONTENT);
    };
    let cancelled = session::transition(
        &state,
        &session.id,
        &[
            LoginSessionStatus::AwaitingConfirmation,
            LoginSessionStatus::Starting,
            LoginSessionStatus::AwaitingCaptcha,
            LoginSessionStatus::Submitting,
            LoginSessionStatus::ValidatingCookie,
        ],
        LoginSessionStatus::Cancelled,
        None,
    )
    .await
    .map_err(internal_error)?;
    if cancelled {
        session::clear_protocol_context(&state, &session.id)
            .await
            .map_err(internal_error)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/v1/settings/nga-account/renewal/test
/// Never submits credentials; verifies decryption + binding validity and reads
/// the account page to check the RSA public key structure.
pub async fn test_renewal(
    State(state): State<AppState>,
) -> Result<Json<RenewalTestResponse>, (StatusCode, Json<ApiError>)> {
    let account_row = sqlx::query("SELECT id FROM nga_accounts WHERE label = 'default'")
        .fetch_optional(&state.pool)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::PRECONDITION_FAILED,
            Json(ApiError {
                error: "nga_account_needs_configuration",
            }),
        ))?;
    let account_id: String = account_row.get("id");
    let credentials = session::load_renewal_credentials(&state, &account_id)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::PRECONDITION_FAILED,
            Json(ApiError {
                error: "renewal_not_configured",
            }),
        ))?;
    if credentials.login_name.trim().is_empty() || credentials.password.expose_secret().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid_renewal_credentials",
            }),
        ));
    }
    // The binding is validated again here so stale references fail fast.
    let binding = sqlx::query(
        "SELECT b.role, b.conversation_type, b.conversation_id, i.bot_enabled, i.enabled
         FROM nga_account_renewal_settings r
         JOIN bot_bindings b ON b.id = r.bot_binding_id
         JOIN platform_integrations i ON i.id = b.integration_id
         WHERE r.account_id = $1",
    )
    .bind(&account_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal_error)?;
    let binding_valid = binding.as_ref().is_some_and(|row| {
        row.get::<String, _>("role") == "owner"
            && row.get::<Option<String>, _>("conversation_type").as_deref() == Some("private")
            && row.get::<Option<String>, _>("conversation_id").is_some()
            && row.get::<i32, _>("bot_enabled") == 1
            && row.get::<i32, _>("enabled") == 1
    });
    if !binding_valid {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid_bot_binding",
            }),
        ));
    }

    // Read the account page to verify the RSA key structure without sending
    // any credential.
    let mut adapter = crate::nga::login::NgaWebLoginV1::new(state.config.nga_user_agent.as_str())
        .map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            Json(ApiError {
                error: "nga_login_http_error",
            }),
        )
    })?;
    match adapter.prepare_challenge().await {
        Ok(challenge) => {
            let detail = format!(
                "协议正常（{}，验证码类型 {}）",
                challenge.context.protocol_version, challenge.image_mime
            );
            Ok(Json(RenewalTestResponse { ok: true, detail }))
        }
        Err(error) => {
            tracing::warn!(
                error_kind = error.kind(),
                error = %error,
                "NGA renewal protocol probe failed"
            );
            Err((
                StatusCode::BAD_GATEWAY,
                Json(ApiError {
                    error: error.kind(),
                }),
            ))
        }
    }
}

/// POST /api/v1/settings/nga-account/renewal/trigger
/// Opens a bot confirmation session without declaring the current Cookie
/// invalid or pausing watches.
pub async fn trigger_renewal(State(state): State<AppState>) -> ApiResult<RenewalTriggerResponse> {
    let confirmation = session::request_manual_renewal(&state)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "renewal_not_ready",
            }),
        ))?;

    let status = if confirmation.created {
        "awaiting_confirmation"
    } else {
        "active"
    };
    Ok(Json(RenewalTriggerResponse {
        session_id: confirmation.session_id,
        status,
        created: confirmation.created,
    }))
}

pub async fn test(State(state): State<AppState>) -> ApiResult<TestAccountResponse> {
    let row = sqlx::query(
        "SELECT passport_uid_encrypted, passport_cid_encrypted, cookie_encrypted
         FROM nga_accounts WHERE label = 'default'",
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(internal_error)?
    .ok_or({
        (
            StatusCode::PRECONDITION_FAILED,
            Json(ApiError {
                error: "nga_account_needs_configuration",
            }),
        )
    })?;
    let uid = decrypt_column(&state, row.get("passport_uid_encrypted"))?;
    let cid = decrypt_column(&state, row.get("passport_cid_encrypted"))?;
    let cookie_encrypted: Option<Vec<u8>> = row.get("cookie_encrypted");
    let request_cookie = match cookie_encrypted {
        Some(value) => decrypt_column(&state, value)?,
        None => format!("ngaPassportUid={uid}; ngaPassportCid={cid}"),
    };

    match state
        .nga_client
        .check_credentials(&uid, &request_cookie)
        .await
    {
        Ok(check) => {
            update_auth_status(&state, "valid", None).await?;
            notification::alerts::resolve_nga_credentials_invalid_alert(&state)
                .await
                .map_err(internal_error)?;
            Ok(Json(TestAccountResponse {
                valid: check.valid,
                uid: check.uid,
            }))
        }
        Err(error) => {
            let (kind, status, http_status) = map_auth_check_error(&error);
            if matches!(error, AuthCheckError::Unauthorized) {
                // A deliberate connection test is also a definitive auth
                // check. Route it through the same pause/alert/bot flow as a
                // collector rejection so renewal does not depend on waiting
                // for the next scheduled crawl.
                session::on_auth_failure(&state)
                    .await
                    .map_err(internal_error)?;
            } else {
                update_auth_status(&state, status, Some(kind)).await?;
            }
            Err((http_status, Json(ApiError { error: kind })))
        }
    }
}

fn map_auth_check_error(error: &AuthCheckError) -> (&'static str, &'static str, StatusCode) {
    match error {
        // A rejected NGA Cookie is an invalid external credential, not a failure of
        // this API's administrator authentication. Keep 401 reserved for the API
        // authentication middleware so browser clients do not discard their UI session.
        AuthCheckError::Unauthorized => (
            "nga_credentials_invalid",
            "invalid",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        AuthCheckError::Busy => ("nga_busy", "unchecked", StatusCode::SERVICE_UNAVAILABLE),
        AuthCheckError::Http(_) => ("nga_http_error", "unchecked", StatusCode::BAD_GATEWAY),
        AuthCheckError::Request(_) => ("nga_request_error", "unchecked", StatusCode::BAD_GATEWAY),
    }
}

fn decrypt_column(
    state: &AppState,
    value: Vec<u8>,
) -> Result<String, (StatusCode, Json<ApiError>)> {
    state.credential_cipher.decrypt(&value).map_err(|_| {
        (
            StatusCode::PRECONDITION_FAILED,
            Json(ApiError {
                error: "nga_account_needs_configuration",
            }),
        )
    })
}

async fn unconfigured_response() -> AccountResponse {
    AccountResponse {
        configured: false,
        passport_uid_masked: None,
        full_cookie_configured: false,
        status: "unconfigured".to_owned(),
        last_auth_checked_at: None,
        last_auth_error_kind: None,
        renewal_enabled: false,
        renewal_credentials_configured: false,
        renewal_bot_binding_configured: false,
        renewal_bot_binding_id: None,
        renewal_credential_status: None,
        renewal_cooldown_until: None,
        last_renewal_at: None,
        last_renewal_error_kind: None,
        active_login_session: None,
    }
}

async fn needs_configuration_response() -> AccountResponse {
    let mut response = unconfigured_response().await;
    response.status = "needs_configuration".to_owned();
    response.last_auth_error_kind = Some("credential_decryption_failed".to_owned());
    response
}

/// (enabled, credentials_configured, binding_configured, binding_id,
///  credential_status, cooldown_until, last_renewal_at, last_error_kind)
async fn renewal_summary(
    state: &AppState,
    account_id: &str,
) -> (
    bool,
    bool,
    bool,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let row = sqlx::query(
        "SELECT r.enabled, r.credential_status,
                CAST(r.cooldown_until AS TEXT) AS cooldown_until,
                CAST(r.last_renewal_at AS TEXT) AS last_renewal_at,
                r.last_error_kind, b.id AS binding_id
         FROM nga_account_renewal_settings r
         LEFT JOIN bot_bindings b ON b.id = r.bot_binding_id
         WHERE r.account_id = $1",
    )
    .bind(account_id)
    .fetch_optional(&state.pool)
    .await;
    match row {
        Ok(Some(row)) => {
            let enabled = row.get::<i32, _>("enabled") == 1;
            let binding_id: Option<String> = row.get("binding_id");
            (
                enabled,
                true,
                binding_id.is_some(),
                binding_id,
                Some(row.get("credential_status")),
                row.get("cooldown_until"),
                row.get("last_renewal_at"),
                row.get("last_error_kind"),
            )
        }
        _ => (false, false, false, None, None, None, None, None),
    }
}

async fn active_login_session(state: &AppState, account_id: &str) -> Option<serde_json::Value> {
    let row = sqlx::query(
        "SELECT id, status, trigger_kind,
                CAST(expires_at AS TEXT) AS expires_at
         FROM nga_login_sessions
         WHERE account_id = $1 AND status IN
           ('awaiting_confirmation','starting','awaiting_captcha','submitting','validating_cookie')
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(account_id)
    .fetch_optional(&state.pool)
    .await
    .ok()?;
    row.map(|row| {
        serde_json::json!({
            "id": row.get::<String, _>("id"),
            "status": row.get::<String, _>("status"),
            "trigger_kind": row.get::<String, _>("trigger_kind"),
            "expires_at": row.get::<Option<String>, _>("expires_at"),
        })
    })
}

async fn update_auth_status(
    state: &AppState,
    status: &str,
    error_kind: Option<&str>,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    sqlx::query(
        "UPDATE nga_accounts SET status = $1, last_auth_checked_at = CURRENT_TIMESTAMP,
         last_auth_error_kind = $2, updated_at = CURRENT_TIMESTAMP WHERE label = 'default'",
    )
    .bind(status)
    .bind(error_kind)
    .execute(&state.pool)
    .await
    .map_err(internal_error)?;
    Ok(())
}

fn mask_uid(uid: &str) -> String {
    if uid.len() <= 4 {
        return "*".repeat(uid.len());
    }
    format!("{}***{}", &uid[..2], &uid[uid.len() - 2..])
}

fn extract_credentials(request: SaveAccountRequest) -> Option<SavedCredentials> {
    if let Some(cookie) = request.cookie {
        let cookie = cookie.trim();
        let cookie = cookie.strip_prefix("Cookie:").unwrap_or(cookie).trim();
        if cookie.is_empty()
            || cookie.len() > MAX_COOKIE_HEADER_LENGTH
            || cookie.chars().any(char::is_control)
        {
            return None;
        }
        let mut uid = None;
        let mut cid = None;
        let mut parts = Vec::new();
        for part in cookie.split(';') {
            let Some((name, value)) = part.trim().split_once('=') else {
                continue;
            };
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() || value.is_empty() {
                continue;
            }
            match name {
                "ngaPassportUid" => uid = Some(value.to_owned()),
                "ngaPassportCid" => cid = Some(value.to_owned()),
                _ => {}
            }
            parts.push(format!("{name}={value}"));
        }
        return Some(SavedCredentials {
            passport_uid: uid?,
            passport_cid: cid?,
            cookie_header: Some(parts.join("; ")),
        });
    }
    Some(SavedCredentials {
        passport_uid: request.passport_uid?,
        passport_cid: request.passport_cid?,
        cookie_header: None,
    })
}

fn internal_error(_: sqlx::Error) -> (StatusCode, Json<ApiError>) {
    internal_api_error()
}

fn internal_api_error() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "internal_error",
        }),
    )
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::{
        SaveAccountRequest, SavedCredentials, extract_credentials, map_auth_check_error, mask_uid,
    };
    use crate::nga::AuthCheckError;

    #[test]
    fn masks_uid() {
        assert_eq!(mask_uid("7654321"), "76***21");
        assert_eq!(mask_uid("1234"), "****");
    }

    #[test]
    fn extracts_credentials_and_preserves_the_full_cookie() {
        let request = SaveAccountRequest {
            passport_uid: None,
            passport_cid: None,
            cookie: Some(
                "other=value; ngaPassportUid=123456; ngaPassportCid=secret; ignored=1".to_owned(),
            ),
        };

        assert_eq!(
            extract_credentials(request),
            Some(SavedCredentials {
                passport_uid: "123456".to_owned(),
                passport_cid: "secret".to_owned(),
                cookie_header: Some(
                    "other=value; ngaPassportUid=123456; ngaPassportCid=secret; ignored=1"
                        .to_owned()
                ),
            })
        );
    }

    #[test]
    fn rejected_nga_cookie_is_not_reported_as_admin_unauthorized() {
        let (kind, account_status, http_status) =
            map_auth_check_error(&AuthCheckError::Unauthorized);

        assert_eq!(kind, "nga_credentials_invalid");
        assert_eq!(account_status, "invalid");
        assert_eq!(http_status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
