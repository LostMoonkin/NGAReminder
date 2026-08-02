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
    notification::sender::{BarkConfig, FeishuConfig, Notification, send_configured},
};

#[derive(Deserialize)]
pub struct CreateChannel {
    label: String,
    channel_type: String,
    config: serde_json::Value,
}

#[derive(Deserialize)]
pub struct UpdateChannel {
    enabled: Option<bool>,
    label: Option<String>,
    config: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct ChannelView {
    id: String,
    label: String,
    channel_type: String,
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
        "SELECT id, label, channel_type, enabled FROM notification_channels ORDER BY created_at",
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
    if request.label.trim().is_empty() || !validate_config(&request.channel_type, &request.config) {
        return Err(bad_request());
    }
    let config = serde_json::to_string(&request.config).map_err(|_| bad_request())?;
    let encrypted = state
        .credential_cipher
        .encrypt(&config)
        .map_err(|_| internal_api())?;
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO notification_channels
         (id, channel_type, label, config_encrypted) VALUES ($1, $2, $3, $4)",
    )
    .bind(&id)
    .bind(&request.channel_type)
    .bind(&request.label)
    .bind(encrypted)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    let _ = state.feishu_channel_updates.send(());
    Ok((
        StatusCode::CREATED,
        Json(ChannelView {
            id,
            label: request.label,
            channel_type: request.channel_type,
            enabled: true,
        }),
    ))
}

pub async fn update_channel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateChannel>,
) -> ApiResult<ChannelView> {
    if request.enabled.is_none() && request.label.is_none() && request.config.is_none() {
        return Err(bad_request());
    }
    if request
        .label
        .as_ref()
        .is_some_and(|label| label.trim().is_empty())
    {
        return Err(bad_request());
    }
    let current = sqlx::query("SELECT channel_type FROM notification_channels WHERE id = $1")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let channel_type: String = current.get("channel_type");
    let encrypted = if let Some(config) = request.config {
        if !validate_config(&channel_type, &config) {
            return Err(bad_request());
        }
        let raw = serde_json::to_string(&config).map_err(|_| bad_request())?;
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
         config_encrypted = COALESCE($3, config_encrypted),
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
        "SELECT id, label, channel_type, enabled FROM notification_channels WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;
    let _ = state.feishu_channel_updates.send(());
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
    let _ = state.feishu_channel_updates.send(());
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test_channel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let row = sqlx::query(
        "SELECT channel_type, config_encrypted FROM notification_channels WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;
    let encrypted: Vec<u8> = row.get("config_encrypted");
    let config = state
        .credential_cipher
        .decrypt(&encrypted)
        .map_err(|_| internal_api())?;
    send_configured(
        &row.get::<String, _>("channel_type"),
        &config,
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

fn validate_config(channel_type: &str, value: &serde_json::Value) -> bool {
    match channel_type {
        "bark" => serde_json::from_value::<BarkConfig>(value.clone()).is_ok(),
        "feishu" => serde_json::from_value::<FeishuConfig>(value.clone())
            .is_ok_and(|config| config.is_valid()),
        _ => false,
    }
}

fn map_channel(row: &sqlx::any::AnyRow) -> ChannelView {
    ChannelView {
        id: row.get("id"),
        label: row.get("label"),
        channel_type: row.get("channel_type"),
        enabled: row.get::<i32, _>("enabled") == 1,
    }
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
