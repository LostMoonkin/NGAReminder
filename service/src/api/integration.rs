use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{
    app::AppState,
    platform::integration::{BotRole, PlatformKind, validate_credentials},
};

#[derive(Deserialize)]
pub struct CreateIntegration {
    platform: String,
    label: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_true")]
    delivery_enabled: bool,
    #[serde(default)]
    bot_enabled: bool,
    credentials: serde_json::Value,
}

#[derive(Deserialize)]
pub struct UpdateIntegration {
    enabled: Option<bool>,
    delivery_enabled: Option<bool>,
    bot_enabled: Option<bool>,
    label: Option<String>,
    credentials: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct SetBotIntegration {
    integration_id: String,
}

#[derive(Deserialize)]
pub struct CreatePairingToken {
    #[serde(default = "default_owner_role")]
    role: String,
    #[serde(default = "default_pairing_ttl")]
    expires_in_seconds: i64,
}

#[derive(Deserialize)]
pub struct UpdateBinding {
    role: Option<String>,
    enabled: Option<bool>,
    label: Option<String>,
}

#[derive(Serialize)]
pub struct ListResponse<T> {
    items: Vec<T>,
}

#[derive(Serialize)]
pub struct ApiError {
    error: String,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

pub async fn list(
    State(state): State<AppState>,
) -> ApiResult<ListResponse<crate::platform::integration::IntegrationView>> {
    let items = crate::platform::integration::list_integrations(&state)
        .await
        .map_err(internal)?;
    Ok(Json(ListResponse { items }))
}

pub async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateIntegration>,
) -> Result<
    (
        StatusCode,
        Json<crate::platform::integration::IntegrationView>,
    ),
    (StatusCode, Json<ApiError>),
> {
    let platform = PlatformKind::parse(&request.platform).ok_or_else(bad_request)?;
    if request.label.trim().is_empty() || !validate_credentials(platform, &request.credentials) {
        return Err(bad_request());
    }
    if request.bot_enabled && !platform.bot_adapter_available() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "bot_not_supported".to_owned(),
            }),
        ));
    }
    let integration = match crate::platform::integration::insert_integration(
        &state,
        platform,
        &request.label,
        request.enabled,
        request.delivery_enabled,
        request.bot_enabled,
        &request.credentials,
    )
    .await
    {
        Ok(integration) => integration,
        Err(error) => {
            let message = error.to_string().to_lowercase();
            if message.contains("bot_already_enabled_for_platform") {
                return Err(conflict("bot_already_enabled_for_platform"));
            }
            if message.contains("unique") {
                return Err(conflict("label_already_exists"));
            }
            return Err(internal_api_error());
        }
    };
    notify_platform_change(&state);
    Ok((StatusCode::CREATED, Json(integration_view(&integration))))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<crate::platform::integration::IntegrationView> {
    let integration = crate::platform::integration::get_integration(&state, &id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    Ok(Json(integration_view(&integration)))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateIntegration>,
) -> ApiResult<crate::platform::integration::IntegrationView> {
    if request.enabled.is_none()
        && request.delivery_enabled.is_none()
        && request.bot_enabled.is_none()
        && request.label.is_none()
        && request.credentials.is_none()
    {
        return Err(bad_request());
    }
    if request
        .label
        .as_ref()
        .is_some_and(|label| label.trim().is_empty())
    {
        return Err(bad_request());
    }
    let current = crate::platform::integration::get_integration(&state, &id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    if request
        .credentials
        .as_ref()
        .is_some_and(|value| !validate_credentials(current.platform, value))
    {
        return Err(bad_request());
    }
    if request.bot_enabled == Some(true) && !current.platform.bot_adapter_available() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "bot_not_supported".to_owned(),
            }),
        ));
    }
    let integration = match crate::platform::integration::update_integration(
        &state,
        &id,
        request.enabled,
        request.delivery_enabled,
        request.bot_enabled,
        request.label.as_deref(),
        request.credentials.as_ref(),
    )
    .await
    {
        Ok(Some(integration)) => integration,
        Ok(None) => return Err(not_found()),
        Err(error)
            if error
                .to_string()
                .contains("bot_already_enabled_for_platform") =>
        {
            return Err(conflict("bot_already_enabled_for_platform"));
        }
        Err(error) if error.to_string().to_lowercase().contains("unique") => {
            return Err(conflict("label_already_exists"));
        }
        Err(_) => return Err(internal_api_error()),
    };
    notify_platform_change(&state);
    Ok(Json(integration_view(&integration)))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    match crate::platform::integration::delete_integration(&state, &id).await {
        Ok(true) => {}
        Ok(false) => return Err(not_found()),
        Err(error) if error.to_string().contains("integration_in_use") => {
            return Err(conflict("integration_in_use"));
        }
        Err(_) => return Err(internal_api_error()),
    }
    notify_platform_change(&state);
    Ok(StatusCode::NO_CONTENT)
}

/// Verify platform credentials without touching any notification target.
pub async fn test(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let integration = crate::platform::integration::get_integration(&state, &id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    match test_credentials(&integration.platform, &integration.credentials).await {
        Ok(()) => {
            crate::platform::integration::mark_connection_state(&state, &id, "connected", None)
                .await
                .map_err(internal)?;
            Ok(StatusCode::NO_CONTENT)
        }
        Err(kind) => {
            crate::platform::integration::mark_connection_state(&state, &id, "error", Some(kind))
                .await
                .map_err(internal)?;
            Err((
                StatusCode::BAD_GATEWAY,
                Json(ApiError {
                    error: kind.to_owned(),
                }),
            ))
        }
    }
}

pub async fn set_bot(
    State(state): State<AppState>,
    Path(platform): Path<String>,
    Json(request): Json<SetBotIntegration>,
) -> ApiResult<crate::platform::integration::IntegrationView> {
    let kind = PlatformKind::parse(&platform).ok_or_else(bad_request)?;
    if !kind.bot_adapter_available() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "bot_not_supported".to_owned(),
            }),
        ));
    }
    let selected = crate::platform::integration::get_integration(&state, &request.integration_id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    if selected.platform != kind {
        return Err(bad_request());
    }
    let integration =
        crate::platform::integration::set_bot_integration(&state, &request.integration_id)
            .await
            .map_err(internal)?;
    notify_platform_change(&state);
    Ok(Json(integration_view(&integration)))
}

pub async fn clear_bot(
    State(state): State<AppState>,
    Path(platform): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let kind = PlatformKind::parse(&platform).ok_or_else(bad_request)?;
    if !kind.bot_adapter_available() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "bot_not_supported".to_owned(),
            }),
        ));
    }
    let rows = sqlx::query(
        "SELECT id FROM platform_integrations
         WHERE platform = $1 AND bot_enabled = 1",
    )
    .bind(platform)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    for row in rows {
        crate::platform::integration::clear_bot_integration(&state, &row.get::<String, _>("id"))
            .await
            .map_err(internal)?;
    }
    notify_platform_change(&state);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn bot_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<crate::platform::integration::IntegrationView> {
    get(State(state), Path(id)).await
}

pub async fn create_pairing_token(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<CreatePairingToken>,
) -> Result<
    (
        StatusCode,
        Json<crate::platform::integration::PairingTokenView>,
    ),
    (StatusCode, Json<ApiError>),
> {
    let integration = crate::platform::integration::get_integration(&state, &id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    if BotRole::parse(&request.role).is_none() {
        return Err(bad_request());
    }
    if !integration.enabled
        || !integration.bot_enabled
        || !integration.platform.bot_adapter_available()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "bot_not_enabled".to_owned(),
            }),
        ));
    }
    let ttl = request.expires_in_seconds.clamp(60, 3600);
    let token = crate::platform::integration::create_pairing_token(&state, &id, &request.role, ttl)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    Ok((StatusCode::CREATED, Json(token)))
}

pub async fn list_bindings(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<ListResponse<crate::platform::integration::BotBindingView>> {
    let items = crate::platform::integration::list_bindings(&state, &id)
        .await
        .map_err(internal)?;
    Ok(Json(ListResponse { items }))
}

pub async fn update_binding(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateBinding>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let updated = match crate::platform::integration::update_binding(
        &state,
        &id,
        request.role.as_deref(),
        request.enabled,
        request.label.as_deref(),
    )
    .await
    {
        Ok(updated) => updated,
        Err(error) if error.to_string().contains("binding_in_use") => {
            return Err(conflict("binding_in_use"));
        }
        Err(error) if error.to_string().contains("invalid_role") => return Err(bad_request()),
        Err(_) => return Err(internal_api_error()),
    };
    if !updated {
        return Err(not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_binding(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    match crate::platform::integration::delete_binding(&state, &id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(not_found()),
        Err(error) if error.to_string().contains("binding_in_use") => {
            Err(conflict("binding_in_use"))
        }
        Err(_) => Err(internal_api_error()),
    }
}

// ---- helpers -------------------------------------------------------------

fn integration_view(
    integration: &crate::platform::integration::PlatformIntegration,
) -> crate::platform::integration::IntegrationView {
    crate::platform::integration::IntegrationView {
        id: integration.id.clone(),
        platform: integration.platform.as_str().to_owned(),
        label: integration.label.clone(),
        enabled: integration.enabled,
        delivery_enabled: integration.delivery_enabled,
        bot_enabled: integration.bot_enabled,
        credentials_configured: true,
        capabilities: crate::platform::integration::IntegrationView::capabilities_for(
            integration.platform,
        ),
        connection_status: integration.connection_status.clone(),
        last_error_kind: integration.last_error_kind.clone(),
    }
}

async fn test_credentials(
    platform: &PlatformKind,
    credentials: &crate::platform::integration::IntegrationCredentials,
) -> Result<(), &'static str> {
    match (platform, credentials) {
        (
            PlatformKind::Feishu,
            crate::platform::integration::IntegrationCredentials::Feishu(creds),
        ) => {
            let client = Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .map_err(|_| "request_error")?;
            let response = client
                .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
                .json(&serde_json::json!({
                    "app_id": creds.app_id,
                    "app_secret": creds.app_secret,
                }))
                .send()
                .await
                .map_err(|_| "request_error")?;
            let status = response.status();
            let body: serde_json::Value = response.json().await.map_err(|_| "request_error")?;
            if status.is_success() && body["code"].as_i64() == Some(0) {
                Ok(())
            } else {
                Err("invalid_credentials")
            }
        }
        (PlatformKind::Bark, crate::platform::integration::IntegrationCredentials::Bark(creds)) => {
            // Bark has no server-side auth; a reachable push endpoint is enough.
            let client = Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|_| "request_error")?;
            client
                .get(format!("{}/ping", creds.server_url.trim_end_matches('/')))
                .send()
                .await
                .map_err(|_| "request_error")?;
            Ok(())
        }
        _ => Err("unsupported_platform"),
    }
}

fn notify_platform_change(state: &AppState) {
    let _ = state.platform_updates.send(());
}

fn default_true() -> bool {
    true
}

fn default_owner_role() -> String {
    "owner".to_owned()
}

fn default_pairing_ttl() -> i64 {
    600
}

fn bad_request() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "invalid_request".to_owned(),
        }),
    )
}

fn not_found() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: "not_found".to_owned(),
        }),
    )
}

fn conflict(kind: &str) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::CONFLICT,
        Json(ApiError {
            error: kind.to_owned(),
        }),
    )
}

fn internal(_: sqlx::Error) -> (StatusCode, Json<ApiError>) {
    internal_api_error()
}

fn internal_api_error() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "internal_error".to_owned(),
        }),
    )
}
