use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::AppState,
    assets::{self, DEFAULT_MAINTENANCE_RETENTION_SECONDS},
};

#[derive(Debug, Default, Deserialize)]
pub struct MaintenanceOptions {
    retention_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    error: &'static str,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

pub async fn report(
    State(state): State<AppState>,
    Query(options): Query<MaintenanceOptions>,
) -> ApiResult<assets::MaintenanceReport> {
    assets::maintenance_report(&state, retention_seconds(options.retention_seconds))
        .await
        .map(Json)
        .map_err(internal)
}

pub async fn cleanup(
    State(state): State<AppState>,
    Json(options): Json<MaintenanceOptions>,
) -> ApiResult<assets::MaintenanceCleanupResult> {
    assets::cleanup_maintenance(&state, retention_seconds(options.retention_seconds))
        .await
        .map(Json)
        .map_err(internal)
}

fn retention_seconds(value: Option<u64>) -> u64 {
    value
        .unwrap_or(DEFAULT_MAINTENANCE_RETENTION_SECONDS)
        .clamp(60 * 60, 30 * 24 * 60 * 60)
}

fn internal(error: anyhow::Error) -> (StatusCode, Json<ApiError>) {
    tracing::warn!(error = %error, "asset maintenance request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "asset_maintenance_failed",
        }),
    )
}
