use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{app::AppState, nga::AuthCheckError, notification};

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
}

#[derive(Debug, Serialize)]
pub struct TestAccountResponse {
    valid: bool,
    uid: i64,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    error: &'static str,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

pub async fn get(State(state): State<AppState>) -> ApiResult<AccountResponse> {
    let row = sqlx::query(
        "SELECT passport_uid_encrypted, passport_cid_encrypted, status,
         CAST(last_auth_checked_at AS TEXT) AS last_auth_checked_at, last_auth_error_kind
         FROM nga_accounts WHERE label = 'default'",
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(internal_error)?;

    let Some(row) = row else {
        return Ok(Json(AccountResponse {
            configured: false,
            passport_uid_masked: None,
            status: "unconfigured".to_owned(),
            last_auth_checked_at: None,
            last_auth_error_kind: None,
        }));
    };
    let encrypted: Vec<u8> = row.get("passport_uid_encrypted");
    let cid_encrypted: Vec<u8> = row.get("passport_cid_encrypted");
    let Ok(uid) = state.credential_cipher.decrypt(&encrypted) else {
        return Ok(Json(needs_configuration_response()));
    };
    if state.credential_cipher.decrypt(&cid_encrypted).is_err() {
        return Ok(Json(needs_configuration_response()));
    }

    Ok(Json(AccountResponse {
        configured: true,
        passport_uid_masked: Some(mask_uid(&uid)),
        status: row.get("status"),
        last_auth_checked_at: row.get("last_auth_checked_at"),
        last_auth_error_kind: row.get("last_auth_error_kind"),
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

    Ok(Json(AccountResponse {
        configured: true,
        passport_uid_masked: Some(mask_uid(&passport_uid)),
        status: "unchecked".to_owned(),
        last_auth_checked_at: None,
        last_auth_error_kind: None,
    }))
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

fn needs_configuration_response() -> AccountResponse {
    AccountResponse {
        configured: false,
        passport_uid_masked: None,
        status: "needs_configuration".to_owned(),
        last_auth_checked_at: None,
        last_auth_error_kind: Some("credential_decryption_failed".to_owned()),
    }
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
