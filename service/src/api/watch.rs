use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::AppState,
    collector::{
        thread::{self, ThreadCollectorError},
        user::{self, UserCollectorError},
    },
    repository::watch::{self, CreateWatchError, WatchTarget},
    schedule::{self, Schedule},
};

#[derive(Debug, Deserialize)]
pub struct CreateThreadWatchRequest {
    tid: i64,
    interval_seconds: Option<i32>,
    #[serde(default)]
    schedule: Option<Schedule>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserWatchRequest {
    uid: i64,
    interval_seconds: Option<i32>,
    #[serde(default)]
    schedule: Option<Schedule>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWatchRequest {
    enabled: Option<bool>,
    interval_seconds: Option<i32>,
    #[serde(default)]
    schedule: Option<Schedule>,
}

#[derive(Debug, Serialize)]
pub struct WatchResponse {
    id: String,
    target_type: String,
    target_id: i64,
    enabled: bool,
    interval_seconds: i32,
    schedule: Option<Schedule>,
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
    let interval_seconds = request
        .interval_seconds
        .unwrap_or(state.config.scheduler.default_interval_seconds);
    if request.tid <= 0
        || !valid_interval(interval_seconds)
        || !valid_schedule(request.schedule.as_ref())
    {
        return Err(bad_request());
    }
    let watch = watch::create_thread_watch_with_schedule(
        &state.pool,
        request.tid,
        interval_seconds,
        request.schedule.as_ref(),
    )
    .await;
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

pub async fn create_user(
    State(state): State<AppState>,
    Json(request): Json<CreateUserWatchRequest>,
) -> Result<(StatusCode, Json<WatchResponse>), (StatusCode, Json<ApiError>)> {
    let interval_seconds = request
        .interval_seconds
        .unwrap_or(state.config.scheduler.default_interval_seconds);
    if request.uid <= 0
        || !valid_interval(interval_seconds)
        || !valid_schedule(request.schedule.as_ref())
    {
        return Err(bad_request());
    }
    match watch::create_user_watch_with_schedule(
        &state.pool,
        request.uid,
        interval_seconds,
        request.schedule.as_ref(),
    )
    .await
    {
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
    if request.enabled.is_none() && request.interval_seconds.is_none() && request.schedule.is_none()
    {
        return Err(bad_request());
    }
    if request
        .interval_seconds
        .is_some_and(|interval| !valid_interval(interval))
        || !valid_schedule(request.schedule.as_ref())
    {
        return Err(bad_request());
    }
    let schedule_update = request.schedule.as_ref().map(Some);
    let watch = watch::update_with_schedule(
        &state.pool,
        &id,
        request.enabled,
        request.interval_seconds,
        schedule_update,
    )
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

pub async fn run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    let _watch = watch::find(&state.pool, &id)
        .await
        .map_err(internal_error)?
        .ok_or_else(not_found)?;
    let claimed = watch::claim_by_id(&state.pool, state.config.database_backend, &id)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "watch_already_running",
            }),
        ))?;

    match claimed.target_type.as_str() {
        "thread" => thread::run(&state, claimed)
            .await
            .map(|summary| Json(serde_json::to_value(summary).expect("summary must serialize")))
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
            }),
        "user" => user::run(&state, claimed)
            .await
            .map(|summary| Json(serde_json::to_value(summary).expect("summary must serialize")))
            .map_err(|error| {
                let (status, kind) = match error {
                    UserCollectorError::Credentials => (
                        StatusCode::PRECONDITION_FAILED,
                        "nga_account_not_configured",
                    ),
                    UserCollectorError::Nga(crate::nga::NgaRequestError::Unauthorized) => {
                        (StatusCode::UNAUTHORIZED, "nga_unauthorized")
                    }
                    UserCollectorError::UserParse(
                        crate::nga::user_parser::UserParseError::ProfileNotFound
                        | crate::nga::user_parser::UserParseError::UidMismatch,
                    ) => (StatusCode::NOT_FOUND, "nga_user_not_found"),
                    UserCollectorError::Nga(_)
                    | UserCollectorError::ThreadParse(_)
                    | UserCollectorError::UserParse(_)
                    | UserCollectorError::InvalidDetail => {
                        (StatusCode::BAD_GATEWAY, "nga_crawl_failed")
                    }
                    UserCollectorError::Database(_) | UserCollectorError::InvalidWatch => {
                        (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
                    }
                };
                (status, Json(ApiError { error: kind }))
            }),
        _ => Err(bad_request()),
    }
}

fn valid_interval(value: i32) -> bool {
    schedule::validate_interval(value)
}

fn valid_schedule(schedule: Option<&Schedule>) -> bool {
    schedule.is_none_or(|items| schedule::validate_schedule(items).is_ok())
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
            schedule: value.schedule,
            status: value.status,
            baseline_completed: value.baseline_completed,
            next_run_at: value.next_run_at,
            last_completed_at: value.last_completed_at,
            last_error_kind: value.last_error_kind,
        }
    }
}
