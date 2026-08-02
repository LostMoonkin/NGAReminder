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
    status: String,
    last_auth_checked_at: Option<String>,
    last_auth_error_kind: Option<String>,
    renewal_enabled: bool,
    renewal_credentials_configured: bool,
    renewal_bot_binding_configured: bool,
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
pub struct ApiError {
    error: &'static str,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

pub async fn get(State(state): State<AppState>) -> ApiResult<AccountResponse> {
    let row = sqlx::query(
        "SELECT id, passport_uid_encrypted, passport_cid_encrypted, status,
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
    let account_id: String = row.get("id");
    let renewal = renewal_summary(&state, &account_id).await;
    let active_session = active_login_session(&state, &account_id).await;

    Ok(Json(AccountResponse {
        configured: true,
        passport_uid_masked: Some(mask_uid(&uid)),
        status: row.get("status"),
        last_auth_checked_at: row.get("last_auth_checked_at"),
        last_auth_error_kind: row.get("last_auth_error_kind"),
        renewal_enabled: renewal.0,
        renewal_credentials_configured: renewal.1,
        renewal_bot_binding_configured: renewal.2,
        renewal_credential_status: renewal.3,
        renewal_cooldown_until: renewal.4,
        last_renewal_at: renewal.5,
        last_renewal_error_kind: renewal.6,
        active_login_session: active_session,
    }))
}

pub async fn save(
    State(state): State<AppState>,
    Json(request): Json<SaveAccountRequest>,
) -> ApiResult<AccountResponse> {
    let (passport_uid, passport_cid) = extract_credentials(request).ok_or((
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "invalid_nga_credentials",
        }),
    ))?;
    if passport_uid.parse::<i64>().is_err()
        || passport_uid.len() > 20
        || passport_cid.trim().is_empty()
        || passport_cid.len() > 512
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
        .encrypt(&passport_uid)
        .map_err(|_| internal_api_error())?;
    let cid_encrypted = state
        .credential_cipher
        .encrypt(&passport_cid)
        .map_err(|_| internal_api_error())?;

    sqlx::query(
        "INSERT INTO nga_accounts
            (label, passport_uid_encrypted, passport_cid_encrypted)
         VALUES ('default', $1, $2)
         ON CONFLICT (label) DO UPDATE SET
            passport_uid_encrypted = EXCLUDED.passport_uid_encrypted,
            passport_cid_encrypted = EXCLUDED.passport_cid_encrypted,
            status = 'unchecked',
            last_auth_checked_at = NULL,
            last_auth_error_kind = NULL,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(uid_encrypted)
    .bind(cid_encrypted)
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
        passport_uid_masked: Some(mask_uid(&passport_uid)),
        status: "unchecked".to_owned(),
        last_auth_checked_at: None,
        last_auth_error_kind: None,
        renewal_enabled: renewal.0,
        renewal_credentials_configured: renewal.1,
        renewal_bot_binding_configured: renewal.2,
        renewal_credential_status: renewal.3,
        renewal_cooldown_until: renewal.4,
        last_renewal_at: renewal.5,
        last_renewal_error_kind: renewal.6,
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
                    i.bot_enabled, i.enabled
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
        if role != "owner"
            || conversation_type.as_deref() != Some("private")
            || conversation_id.is_none()
            || bot_enabled == 0
            || integration_enabled == 0
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
        sqlx::query("DELETE FROM nga_account_renewal_settings WHERE account_id = $1")
            .bind(&account_id)
            .execute(&state.pool)
            .await
            .map_err(internal_error)?;
        return get(State(state)).await;
    }

    let has_new_credentials = request.login_name.is_some() || request.password.is_some();
    let new_login = request.login_name.as_deref().unwrap_or("");
    let new_password = request.password.as_deref().unwrap_or("");
    if has_new_credentials && (new_login.trim().is_empty() || new_password.is_empty()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid_renewal_credentials",
            }),
        ));
    }

    if !exists {
        // Creating the setting requires credentials + binding at once.
        if !has_new_credentials || request.bot_binding_id.is_none() {
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
                WHEN $2 IS NOT NULL OR $3 IS NOT NULL OR $4 IS NOT NULL THEN 'ready'
                ELSE credential_status END,
             consecutive_failure_count = CASE
                WHEN $2 IS NOT NULL OR $3 IS NOT NULL OR $4 IS NOT NULL THEN 0
                ELSE consecutive_failure_count END,
             cooldown_until = NULL,
             last_error_kind = NULL,
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
        Err(error) => Err((
            StatusCode::BAD_GATEWAY,
            Json(ApiError {
                error: error.kind(),
            }),
        )),
    }
}

pub async fn test(State(state): State<AppState>) -> ApiResult<TestAccountResponse> {
    let row = sqlx::query(
        "SELECT passport_uid_encrypted, passport_cid_encrypted
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

    match state.nga_client.check_credentials(&uid, &cid).await {
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
            let (kind, status, http_status) = match error {
                AuthCheckError::Unauthorized => {
                    ("unauthorized", "invalid", StatusCode::UNAUTHORIZED)
                }
                AuthCheckError::Busy => ("nga_busy", "unchecked", StatusCode::SERVICE_UNAVAILABLE),
                AuthCheckError::Http(_) => ("nga_http_error", "unchecked", StatusCode::BAD_GATEWAY),
                AuthCheckError::Request(_) => {
                    ("nga_request_error", "unchecked", StatusCode::BAD_GATEWAY)
                }
            };
            update_auth_status(&state, status, Some(kind)).await?;
            Err((http_status, Json(ApiError { error: kind })))
        }
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
        status: "unconfigured".to_owned(),
        last_auth_checked_at: None,
        last_auth_error_kind: None,
        renewal_enabled: false,
        renewal_credentials_configured: false,
        renewal_bot_binding_configured: false,
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

/// (enabled, credentials_configured, binding_configured, credential_status,
///  cooldown_until, last_renewal_at, last_error_kind)
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
            let binding_configured: Option<String> = row.get("binding_id");
            (
                enabled,
                true,
                binding_configured.is_some(),
                Some(row.get("credential_status")),
                row.get("cooldown_until"),
                row.get("last_renewal_at"),
                row.get("last_error_kind"),
            )
        }
        _ => (false, false, false, None, None, None, None),
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

fn extract_credentials(request: SaveAccountRequest) -> Option<(String, String)> {
    if let Some(cookie) = request.cookie {
        let mut uid = None;
        let mut cid = None;
        for part in cookie.split(';') {
            let Some((name, value)) = part.trim().split_once('=') else {
                continue;
            };
            match name {
                "ngaPassportUid" => uid = Some(value.to_owned()),
                "ngaPassportCid" => cid = Some(value.to_owned()),
                _ => {}
            }
        }
        return uid.zip(cid);
    }
    request.passport_uid.zip(request.passport_cid)
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
    use super::{SaveAccountRequest, extract_credentials, mask_uid};

    #[test]
    fn masks_uid() {
        assert_eq!(mask_uid("7654321"), "76***21");
        assert_eq!(mask_uid("1234"), "****");
    }

    #[test]
    fn extracts_only_required_values_from_full_cookie() {
        let request = SaveAccountRequest {
            passport_uid: None,
            passport_cid: None,
            cookie: Some(
                "other=value; ngaPassportUid=123456; ngaPassportCid=secret; ignored=1".to_owned(),
            ),
        };

        assert_eq!(
            extract_credentials(request),
            Some(("123456".to_owned(), "secret".to_owned()))
        );
    }
}
