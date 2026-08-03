use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    app::AppState,
    notification::sender::{Notification, send_configured},
    platform::integration::{PlatformKind, validate_target},
};

#[derive(Deserialize)]
pub struct CreateChannel {
    integration_id: String,
    label: String,
    #[serde(default = "default_true")]
    enabled: bool,
    target: serde_json::Value,
}

#[derive(Deserialize)]
pub struct UpdateChannel {
    enabled: Option<bool>,
    label: Option<String>,
    target: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct ChannelView {
    id: String,
    integration_id: String,
    platform: String,
    label: String,
    enabled: bool,
}

#[derive(Serialize)]
pub struct ListResponse<T> {
    items: Vec<T>,
}

#[derive(Serialize)]
pub struct ApiError {
    error: &'static str,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

pub async fn list_channels(State(state): State<AppState>) -> ApiResult<ListResponse<ChannelView>> {
    let rows = sqlx::query(
        "SELECT c.id, c.integration_id, i.platform, c.label, c.enabled
         FROM notification_channels c
         JOIN platform_integrations i ON i.id = c.integration_id
         ORDER BY c.created_at",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    Ok(Json(ListResponse {
        items: rows.iter().map(map_channel).collect(),
    }))
}

pub async fn create_channel(
    State(state): State<AppState>,
    Json(request): Json<CreateChannel>,
) -> Result<(StatusCode, Json<ChannelView>), (StatusCode, Json<ApiError>)> {
    if request.label.trim().is_empty() {
        return Err(bad_request());
    }
    let platform_row = sqlx::query("SELECT platform FROM platform_integrations WHERE id = $1")
        .bind(&request.integration_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(internal)?
        .ok_or_else(bad_request)?;
    let platform: String = platform_row.get("platform");
    let kind = PlatformKind::parse(&platform).ok_or_else(bad_request)?;
    if !validate_target(kind, &request.target) {
        return Err(bad_request());
    }
    let target_raw = serde_json::json!({
        "platform": platform,
        "target": request.target,
    })
    .to_string();
    let encrypted = state
        .credential_cipher
        .encrypt(&target_raw)
        .map_err(|_| internal_api())?;
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO notification_channels
         (id, integration_id, label, enabled, target_encrypted) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&id)
    .bind(&request.integration_id)
    .bind(&request.label)
    .bind(i32::from(request.enabled))
    .bind(encrypted)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    notify_platform_change(&state);
    Ok((
        StatusCode::CREATED,
        Json(ChannelView {
            id,
            integration_id: request.integration_id,
            platform,
            label: request.label,
            enabled: request.enabled,
        }),
    ))
}

pub async fn update_channel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateChannel>,
) -> ApiResult<ChannelView> {
    if request.enabled.is_none() && request.label.is_none() && request.target.is_none() {
        return Err(bad_request());
    }
    if request
        .label
        .as_ref()
        .is_some_and(|label| label.trim().is_empty())
    {
        return Err(bad_request());
    }
    let current = sqlx::query(
        "SELECT c.integration_id, i.platform
         FROM notification_channels c
         JOIN platform_integrations i ON i.id = c.integration_id
         WHERE c.id = $1",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;
    let platform: String = current.get("platform");
    let kind = PlatformKind::parse(&platform).ok_or_else(bad_request)?;
    let encrypted = if let Some(target) = request.target {
        if !validate_target(kind, &target) {
            return Err(bad_request());
        }
        let raw = serde_json::json!({
            "platform": platform,
            "target": target,
        })
        .to_string();
        Some(
            state
                .credential_cipher
                .encrypt(&raw)
                .map_err(|_| internal_api())?,
        )
    } else {
        None
    };
    sqlx::query(
        "UPDATE notification_channels SET
         enabled = COALESCE($1, enabled),
         label = COALESCE($2, label),
         target_encrypted = COALESCE($3, target_encrypted),
         updated_at = CURRENT_TIMESTAMP WHERE id = $4",
    )
    .bind(request.enabled.map(i32::from))
    .bind(request.label)
    .bind(encrypted)
    .bind(&id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    let row = sqlx::query(
        "SELECT c.id, c.integration_id, i.platform, c.label, c.enabled
         FROM notification_channels c
         JOIN platform_integrations i ON i.id = c.integration_id
         WHERE c.id = $1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;
    notify_platform_change(&state);
    Ok(Json(map_channel(&row)))
}

pub async fn delete_channel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let references: i64 = sqlx::query_scalar(
        "SELECT
           (SELECT COUNT(*) FROM watch_notification_channels WHERE channel_id = $1)
         + (SELECT COUNT(*) FROM notification_outbox WHERE channel_id = $1)
         + (SELECT COUNT(*) FROM system_alert_outbox WHERE channel_id = $1)",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;
    if references > 0 {
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "channel_in_use",
            }),
        ));
    }
    if sqlx::query("DELETE FROM notification_channels WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(internal)?
        .rows_affected()
        == 0
    {
        return Err(not_found());
    }
    notify_platform_change(&state);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test_channel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let row = sqlx::query(
        "SELECT i.platform, i.credentials_encrypted, c.target_encrypted
         FROM notification_channels c
         JOIN platform_integrations i ON i.id = c.integration_id
         WHERE c.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;
    let credentials_encrypted: Vec<u8> = row.get("credentials_encrypted");
    let credentials = state
        .credential_cipher
        .decrypt(&credentials_encrypted)
        .map_err(|_| internal_api())?;
    let target_encrypted: Vec<u8> = row.get("target_encrypted");
    let target = state
        .credential_cipher
        .decrypt(&target_encrypted)
        .map_err(|_| internal_api())?;
    send_configured(
        &row.get::<String, _>("platform"),
        &credentials,
        &target,
        &Notification {
            title: "NGA Reminder 测试".to_owned(),
            body: "通知渠道配置成功".to_owned(),
            url: "https://bbs.nga.cn/".to_owned(),
        },
    )
    .await
    .map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            Json(ApiError {
                error: "notification_send_failed",
            }),
        )
    })?;
    Ok(StatusCode::NO_CONTENT)
}

fn map_channel(row: &sqlx::any::AnyRow) -> ChannelView {
    ChannelView {
        id: row.get("id"),
        integration_id: row.get("integration_id"),
        platform: row.get("platform"),
        label: row.get("label"),
        enabled: row.get::<i32, _>("enabled") == 1,
    }
}

fn notify_platform_change(state: &AppState) {
    let _ = state.platform_updates.send(());
}

fn default_true() -> bool {
    true
}

fn bad_request() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "invalid_request",
        }),
    )
}

fn not_found() -> (StatusCode, Json<ApiError>) {
    (StatusCode::NOT_FOUND, Json(ApiError { error: "not_found" }))
}

fn internal(_: sqlx::Error) -> (StatusCode, Json<ApiError>) {
    internal_api()
}

fn internal_api() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "internal_error",
        }),
    )
}
