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
pub struct CreateRule {
    label: String,
    channel_id: String,
    tid: Option<i64>,
    uid: Option<i64>,
}

#[derive(Deserialize)]
pub struct SetEnabled {
    enabled: bool,
}

#[derive(Serialize)]
pub struct ChannelView {
    id: String,
    label: String,
    channel_type: String,
    enabled: bool,
}

#[derive(Serialize)]
pub struct RuleView {
    id: String,
    label: String,
    channel_id: String,
    tid: Option<i64>,
    uid: Option<i64>,
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
        items: rows
            .iter()
            .map(|row| ChannelView {
                id: row.get("id"),
                label: row.get("label"),
                channel_type: row.get("channel_type"),
                enabled: row.get::<i32, _>("enabled") == 1,
            })
            .collect(),
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
    Json(request): Json<SetEnabled>,
) -> ApiResult<ChannelView> {
    sqlx::query(
        "UPDATE notification_channels SET enabled = $1, updated_at = CURRENT_TIMESTAMP
         WHERE id = $2",
    )
    .bind(i32::from(request.enabled))
    .bind(&id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    let row = sqlx::query(
        "SELECT id, label, channel_type, enabled FROM notification_channels WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;
    let _ = state.feishu_channel_updates.send(());
    Ok(Json(ChannelView {
        id: row.get("id"),
        label: row.get("label"),
        channel_type: row.get("channel_type"),
        enabled: row.get::<i32, _>("enabled") == 1,
    }))
}

pub async fn delete_channel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let result = delete_row(&state, "notification_channels", &id).await;
    if result.is_ok() {
        let _ = state.feishu_channel_updates.send(());
    }
    result
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

pub async fn list_rules(State(state): State<AppState>) -> ApiResult<ListResponse<RuleView>> {
    let rows = sqlx::query(
        "SELECT id, label, channel_id, tid, uid, enabled
         FROM notification_rules ORDER BY created_at",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    Ok(Json(ListResponse {
        items: rows.iter().map(map_rule).collect(),
    }))
}

pub async fn create_rule(
    State(state): State<AppState>,
    Json(request): Json<CreateRule>,
) -> Result<(StatusCode, Json<RuleView>), (StatusCode, Json<ApiError>)> {
    if request.label.trim().is_empty() || (request.tid.is_none() && request.uid.is_none()) {
        return Err(bad_request());
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO notification_rules (id, label, channel_id, tid, uid)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&id)
    .bind(&request.label)
    .bind(&request.channel_id)
    .bind(request.tid)
    .bind(request.uid)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    Ok((
        StatusCode::CREATED,
        Json(RuleView {
            id,
            label: request.label,
            channel_id: request.channel_id,
            tid: request.tid,
            uid: request.uid,
            enabled: true,
        }),
    ))
}

pub async fn update_rule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<SetEnabled>,
) -> ApiResult<RuleView> {
    sqlx::query(
        "UPDATE notification_rules SET enabled = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2",
    )
    .bind(i32::from(request.enabled))
    .bind(&id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    let row = sqlx::query(
        "SELECT id, label, channel_id, tid, uid, enabled
         FROM notification_rules WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;
    Ok(Json(map_rule(&row)))
}

pub async fn delete_rule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    delete_row(&state, "notification_rules", &id).await
}

fn validate_config(channel_type: &str, value: &serde_json::Value) -> bool {
    match channel_type {
        "bark" => serde_json::from_value::<BarkConfig>(value.clone()).is_ok(),
        "feishu" => serde_json::from_value::<FeishuConfig>(value.clone())
            .is_ok_and(|config| config.is_valid()),
        _ => false,
    }
}

fn map_rule(row: &sqlx::any::AnyRow) -> RuleView {
    RuleView {
        id: row.get("id"),
        label: row.get("label"),
        channel_id: row.get("channel_id"),
        tid: row.get("tid"),
        uid: row.get("uid"),
        enabled: row.get::<i32, _>("enabled") == 1,
    }
}

async fn delete_row(
    state: &AppState,
    table: &str,
    id: &str,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let query = format!("DELETE FROM {table} WHERE id = $1");
    if sqlx::query(&query)
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(internal)?
        .rows_affected()
        == 1
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found())
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
