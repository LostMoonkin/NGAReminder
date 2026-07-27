use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::AppState,
    collector::thread::{self, CrawlSummary, ThreadCollectorError},
    repository::watch::{self, CreateWatchError, WatchTarget},
};

#[derive(Debug, Deserialize)]
pub struct CreateThreadWatchRequest {
    tid: i64,
    #[serde(default = "default_interval")]
    interval_seconds: i32,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWatchRequest {
    enabled: Option<bool>,
    interval_seconds: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct WatchResponse {
    id: String,
    target_type: String,
    target_id: i64,
    enabled: bool,
    interval_seconds: i32,
    status: String,
    baseline_completed: bool,
    next_run_at: String,
    last_completed_at: Option<String>,
    last_error_kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WatchListResponse {
    items: Vec<WatchResponse>,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    error: &'static str,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

pub async fn list(State(state): State<AppState>) -> ApiResult<WatchListResponse> {
    let watches = watch::list(&state.pool).await.map_err(internal_error)?;
    Ok(Json(WatchListResponse {
        items: watches.into_iter().map(WatchResponse::from).collect(),
    }))
}

pub async fn create_thread(
    State(state): State<AppState>,
    Json(request): Json<CreateThreadWatchRequest>,
) -> Result<(StatusCode, Json<WatchResponse>), (StatusCode, Json<ApiError>)> {
    if request.tid <= 0 || !valid_interval(request.interval_seconds) {
        return Err(bad_request());
    }
    let watch =
        watch::create_thread_watch(&state.pool, request.tid, request.interval_seconds).await;
    match watch {
        Ok(watch) => Ok((StatusCode::CREATED, Json(watch.into()))),
        Err(CreateWatchError::Conflict) => Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "watch_already_exists",
            }),
        )),
        Err(CreateWatchError::Database(error)) => Err(internal_error(error)),
    }
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateWatchRequest>,
) -> ApiResult<WatchResponse> {
    if request.enabled.is_none() && request.interval_seconds.is_none() {
        return Err(bad_request());
    }
    if request
        .interval_seconds
        .is_some_and(|interval| !valid_interval(interval))
    {
        return Err(bad_request());
    }
    let watch = watch::update(&state.pool, &id, request.enabled, request.interval_seconds)
        .await
        .map_err(internal_error)?
        .ok_or_else(not_found)?;
    Ok(Json(watch.into()))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    if watch::delete(&state.pool, &id)
        .await
        .map_err(internal_error)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found())
    }
}

pub async fn run(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<CrawlSummary> {
    let watch = watch::find(&state.pool, &id)
        .await
        .map_err(internal_error)?
        .ok_or_else(not_found)?;
    if watch.target_type != "thread" {
        return Err(bad_request());
    }
    let claimed = watch::claim_by_id(&state.pool, state.config.database_backend, &id)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "watch_already_running",
            }),
        ))?;

    thread::run(&state, claimed)
        .await
        .map(Json)
        .map_err(|error| {
            let (status, kind) = match error {
                ThreadCollectorError::Credentials => (
                    StatusCode::PRECONDITION_FAILED,
                    "nga_account_not_configured",
                ),
                ThreadCollectorError::Nga(crate::nga::NgaRequestError::NotFound) => {
                    (StatusCode::NOT_FOUND, "nga_thread_not_found")
                }
                ThreadCollectorError::Nga(crate::nga::NgaRequestError::Unauthorized) => {
                    (StatusCode::UNAUTHORIZED, "nga_unauthorized")
                }
                ThreadCollectorError::Nga(_) | ThreadCollectorError::Parse(_) => {
                    (StatusCode::BAD_GATEWAY, "nga_crawl_failed")
                }
                ThreadCollectorError::Database(_) | ThreadCollectorError::InvalidWatch => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
                }
            };
            (status, Json(ApiError { error: kind }))
        })
}

fn default_interval() -> i32 {
    60
}

fn valid_interval(value: i32) -> bool {
    (30..=86_400).contains(&value)
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
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: "watch_not_found",
        }),
    )
}

fn internal_error(_: sqlx::Error) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "internal_error",
        }),
    )
}

impl From<WatchTarget> for WatchResponse {
    fn from(value: WatchTarget) -> Self {
        Self {
            id: value.id,
            target_type: value.target_type,
            target_id: value.target_id,
            enabled: value.enabled,
            interval_seconds: value.interval_seconds,
            status: value.status,
            baseline_completed: value.baseline_completed,
            next_run_at: value.next_run_at,
            last_completed_at: value.last_completed_at,
            last_error_kind: value.last_error_kind,
        }
    }
}
