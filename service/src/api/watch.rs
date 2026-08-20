use std::collections::HashSet;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::Row;

use crate::{
    app::AppState,
    collector::{
        thread::{self, ThreadCollectorError},
        user::{self, UserCollectorError},
    },
    metrics,
    no_fetch::{self, NoFetchPeriods},
    repository::watch::{self, CreateWatchError, ResetWatchError, WatchTarget},
    schedule::{self, Schedule},
};

#[derive(Debug, Deserialize)]
pub struct HistoryRequest {
    #[serde(default = "default_history_mode")]
    mode: String,
    #[serde(default)]
    parallel_enabled: bool,
    #[serde(default = "default_parallelism")]
    parallelism: i32,
}

#[derive(Debug, Deserialize)]
pub struct NotificationRequest {
    channel_ids: Vec<String>,
    #[serde(default)]
    author_uids: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateThreadWatchRequest {
    tid: i64,
    interval_seconds: Option<i32>,
    #[serde(default)]
    schedule: Option<Schedule>,
    #[serde(default)]
    no_fetch_periods: Option<NoFetchPeriods>,
    #[serde(default)]
    history: Option<HistoryRequest>,
    notification: NotificationRequest,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateUserWatchRequest {
    uid: i64,
    interval_seconds: Option<i32>,
    #[serde(default)]
    schedule: Option<Schedule>,
    #[serde(default)]
    no_fetch_periods: Option<NoFetchPeriods>,
    notification: NotificationRequest,
}

#[derive(Debug, Default)]
enum PatchField<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

fn deserialize_patch_field<'de, D, T>(deserializer: D) -> Result<PatchField<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(match Option::<T>::deserialize(deserializer)? {
        Some(value) => PatchField::Value(value),
        None => PatchField::Null,
    })
}

#[derive(Debug, Deserialize)]
pub struct UpdateWatchRequest {
    enabled: Option<bool>,
    interval_seconds: Option<i32>,
    #[serde(default, deserialize_with = "deserialize_patch_field")]
    schedule: PatchField<Schedule>,
    #[serde(default, deserialize_with = "deserialize_patch_field")]
    no_fetch_periods: PatchField<NoFetchPeriods>,
    history: Option<HistoryRequest>,
    notification: Option<NotificationRequest>,
}

#[derive(Debug, Deserialize)]
pub struct ResetWatchRequest {
    history_mode: Option<String>,
    parallel_enabled: Option<bool>,
    parallelism: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct WatchResponse {
    id: String,
    target_type: String,
    target_id: i64,
    target_name: String,
    enabled: bool,
    interval_seconds: i32,
    schedule: Option<Schedule>,
    no_fetch_periods: Option<NoFetchPeriods>,
    no_fetch_active: bool,
    no_fetch_until: Option<String>,
    scheduler_timezone_offset: String,
    status: String,
    baseline_completed: bool,
    next_run_at: String,
    last_completed_at: Option<String>,
    last_error_kind: Option<String>,
    history: Option<HistoryResponse>,
    notification: NotificationResponse,
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    mode: String,
    parallel_enabled: bool,
    parallelism: i32,
}

#[derive(Debug, Serialize)]
pub struct NotificationResponse {
    channel_ids: Vec<String>,
    author_uids: Vec<i64>,
}

#[derive(Debug, Serialize)]
pub struct WatchListResponse {
    items: Vec<WatchResponse>,
}

#[derive(Debug, Serialize)]
pub struct RunView {
    id: String,
    status: String,
    baseline: bool,
    sync_mode: String,
    trigger_kind: String,
    pages_requested: i32,
    posts_inserted: i32,
    events_created: i32,
    matches_created: i32,
    outbox_enqueued: i32,
    remote_vrows: Option<i32>,
    error_kind: Option<String>,
    error_message: Option<String>,
    started_at: String,
    completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunListResponse {
    items: Vec<RunView>,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    error: &'static str,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

pub async fn list(State(state): State<AppState>) -> ApiResult<WatchListResponse> {
    let watches = watch::list(&state.pool).await.map_err(internal_error)?;
    Ok(Json(WatchListResponse {
        items: watches
            .into_iter()
            .map(|watch| WatchResponse::from_target(watch, state.config.scheduler.timezone_offset))
            .collect(),
    }))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<WatchResponse> {
    watch::find(&state.pool, &id)
        .await
        .map_err(internal_error)?
        .map(|watch| WatchResponse::from_target(watch, state.config.scheduler.timezone_offset))
        .map(Json)
        .ok_or_else(not_found)
}

pub async fn create_thread(
    State(state): State<AppState>,
    Json(request): Json<CreateThreadWatchRequest>,
) -> Result<(StatusCode, Json<WatchResponse>), (StatusCode, Json<ApiError>)> {
    let interval_seconds = request
        .interval_seconds
        .unwrap_or(state.config.scheduler.default_interval_seconds);
    let history = request.history.unwrap_or(HistoryRequest {
        mode: default_history_mode(),
        parallel_enabled: false,
        parallelism: default_parallelism(),
    });
    if request.tid <= 0
        || !valid_interval(interval_seconds)
        || !valid_schedule(request.schedule.as_ref())
        || !valid_no_fetch_periods(request.no_fetch_periods.as_ref())
        || !valid_history(&history)
        || !valid_notification(&request.notification, true)
    {
        return Err(bad_request());
    }
    let created = watch::create_thread_watch_with_no_fetch_config(
        &state.pool,
        request.tid,
        interval_seconds,
        request.schedule.as_ref(),
        request.no_fetch_periods.as_ref(),
        &history.mode,
        history.parallel_enabled,
        history.parallelism,
        request
            .notification
            .author_uids
            .as_deref()
            .unwrap_or_default(),
        &request.notification.channel_ids,
    )
    .await
    .map_err(map_create_error)?;
    let created = refresh_after_configuration_change(&state, created.id).await?;
    Ok((
        StatusCode::CREATED,
        Json(WatchResponse::from_target(
            created,
            state.config.scheduler.timezone_offset,
        )),
    ))
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
        || !valid_no_fetch_periods(request.no_fetch_periods.as_ref())
        || !valid_notification(&request.notification, false)
    {
        return Err(bad_request());
    }
    let created = watch::create_user_watch_with_no_fetch_config(
        &state.pool,
        request.uid,
        interval_seconds,
        request.schedule.as_ref(),
        request.no_fetch_periods.as_ref(),
        &request.notification.channel_ids,
    )
    .await
    .map_err(map_create_error)?;
    let created = refresh_after_configuration_change(&state, created.id).await?;
    Ok((
        StatusCode::CREATED,
        Json(WatchResponse::from_target(
            created,
            state.config.scheduler.timezone_offset,
        )),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateWatchRequest>,
) -> ApiResult<WatchResponse> {
    let has_schedule = !matches!(request.schedule, PatchField::Missing);
    let has_no_fetch_periods = !matches!(request.no_fetch_periods, PatchField::Missing);
    if request.enabled.is_none()
        && request.interval_seconds.is_none()
        && !has_schedule
        && !has_no_fetch_periods
        && request.history.is_none()
        && request.notification.is_none()
    {
        return Err(bad_request());
    }
    if request
        .interval_seconds
        .is_some_and(|interval| !valid_interval(interval))
        || matches!(&request.schedule, PatchField::Value(value) if !valid_schedule(Some(value)))
        || matches!(&request.no_fetch_periods, PatchField::Value(value) if !valid_no_fetch_periods(Some(value)))
        || request
            .history
            .as_ref()
            .is_some_and(|value| !valid_history(value))
    {
        return Err(bad_request());
    }
    let current = watch::find(&state.pool, &id)
        .await
        .map_err(internal_error)?
        .ok_or_else(not_found)?;
    if current.target_type == "user" && request.history.is_some() {
        return Err(bad_request());
    }
    if current.baseline_completed && request.history.is_some() {
        return Err(conflict("history_requires_reset"));
    }
    if let Some(notification) = &request.notification
        && !valid_notification(notification, current.target_type == "thread")
    {
        return Err(bad_request());
    }

    let schedule = match &request.schedule {
        PatchField::Missing => None,
        PatchField::Null => Some(None),
        PatchField::Value(value) => Some(Some(value)),
    };
    let no_fetch_periods = match &request.no_fetch_periods {
        PatchField::Missing => None,
        PatchField::Null => Some(None),
        PatchField::Value(value) => Some(Some(value)),
    };
    let should_refresh_no_fetch = has_no_fetch_periods || request.enabled == Some(true);
    let history_mode = request.history.as_ref().map(|value| value.mode.as_str());
    let history_parallel_enabled = request.history.as_ref().map(|value| value.parallel_enabled);
    let history_parallelism = request.history.as_ref().map(|value| value.parallelism);
    let author_uids = request
        .notification
        .as_ref()
        .filter(|_| current.target_type == "thread")
        .map(|value| value.author_uids.as_deref().unwrap_or_default());
    let channel_ids = request
        .notification
        .as_ref()
        .map(|value| value.channel_ids.as_slice());

    let updated = watch::update_with_no_fetch_config(
        &state.pool,
        &id,
        request.enabled,
        request.interval_seconds,
        schedule,
        no_fetch_periods,
        history_mode,
        history_parallel_enabled,
        history_parallelism,
        author_uids,
        channel_ids,
    )
    .await
    .map_err(map_create_error)?
    .ok_or_else(not_found)?;
    let updated = if should_refresh_no_fetch {
        refresh_after_configuration_change(&state, updated.id).await?
    } else {
        updated
    };
    Ok(Json(WatchResponse::from_target(
        updated,
        state.config.scheduler.timezone_offset,
    )))
}

pub async fn reset(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ResetWatchRequest>,
) -> ApiResult<WatchResponse> {
    let current = watch::find(&state.pool, &id)
        .await
        .map_err(internal_error)?
        .ok_or_else(not_found)?;
    if current.target_type == "thread" {
        if !matches!(
            request.history_mode.as_deref(),
            Some("full" | "incremental")
        ) {
            return Err(bad_request());
        }
        if request
            .parallelism
            .is_some_and(|value| !(1..=16).contains(&value))
            || (request.history_mode.as_deref() == Some("incremental")
                && request.parallel_enabled == Some(true))
        {
            return Err(bad_request());
        }
    } else if request.history_mode.is_some()
        || request.parallel_enabled.is_some()
        || request.parallelism.is_some()
    {
        return Err(bad_request());
    }
    match watch::reset(
        &state.pool,
        &id,
        request.history_mode.as_deref(),
        request.parallel_enabled,
        request.parallelism,
    )
    .await
    {
        Ok(Some(value)) => {
            let value = refresh_after_configuration_change(&state, value.id).await?;
            Ok(Json(WatchResponse::from_target(
                value,
                state.config.scheduler.timezone_offset,
            )))
        }
        Ok(None) => Err(not_found()),
        Err(ResetWatchError::Busy) => Err(conflict("watch_running")),
        Err(ResetWatchError::Database(error)) => Err(internal_error(error)),
    }
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

pub async fn runs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<RunListResponse> {
    watch::find(&state.pool, &id)
        .await
        .map_err(internal_error)?
        .ok_or_else(not_found)?;
    let rows = sqlx::query(
        "SELECT id, status, baseline, sync_mode, pages_requested, posts_inserted,
         events_created, matches_created, outbox_enqueued, remote_vrows,
         trigger_kind,
         error_kind, error_message, CAST(started_at AS TEXT) AS started_at,
         CAST(completed_at AS TEXT) AS completed_at
         FROM crawl_runs WHERE watch_id = $1 ORDER BY started_at DESC LIMIT 100",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal_error)?;
    Ok(Json(RunListResponse {
        items: rows
            .iter()
            .map(|row| RunView {
                id: row.get("id"),
                status: row.get("status"),
                baseline: row.get::<i32, _>("baseline") == 1,
                sync_mode: row.get("sync_mode"),
                trigger_kind: row.get("trigger_kind"),
                pages_requested: row.get("pages_requested"),
                posts_inserted: row.get("posts_inserted"),
                events_created: row.get("events_created"),
                matches_created: row.get("matches_created"),
                outbox_enqueued: row.get("outbox_enqueued"),
                remote_vrows: row.get("remote_vrows"),
                error_kind: row.get("error_kind"),
                error_message: row.get("error_message"),
                started_at: row.get("started_at"),
                completed_at: row.get("completed_at"),
            })
            .collect(),
    }))
}

pub async fn run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    watch::find(&state.pool, &id)
        .await
        .map_err(internal_error)?
        .ok_or_else(not_found)?;
    let Some(claimed) =
        watch::claim_by_id_with_trigger(&state.pool, state.config.database_backend, &id, "manual")
            .await
            .map_err(internal_error)?
    else {
        let error = watch::manual_run_conflict(&state.pool, &id)
            .await
            .map_err(internal_error)?;
        return Err(match error {
            "watch_not_found" => not_found(),
            "watch_already_running" | "watch_run_already_requested" => conflict(error),
            _ => conflict("watch_not_runnable"),
        });
    };

    let run_context = match watch::prepare_crawl_run(
        &state.pool,
        state.config.database_backend,
        &claimed,
        state.config.scheduler.timezone_offset,
    )
    .await
    .map_err(internal_error)?
    {
        watch::RunPreparation::Skipped(skip) => {
            metrics::crawl_skipped_no_fetch();
            return Ok(Json(serde_json::json!({
                "crawl_run_id": skip.crawl_run_id,
                "status": "skipped",
                "baseline": skip.baseline,
                "sync_mode": skip.sync_mode,
                "pages_requested": 0,
                "posts_inserted": 0,
                "events_created": 0,
                "matches_created": 0,
                "outbox_enqueued": 0,
                "no_fetch_until": no_fetch::format_rfc3339(skip.no_fetch_until),
            })));
        }
        watch::RunPreparation::Collect(context) => context,
    };

    match claimed.target_type.as_str() {
        "thread" => thread::run_with_run_id(&state, claimed, run_context.crawl_run_id)
            .await
            .map(|summary| Json(serde_json::to_value(summary).expect("summary must serialize")))
            .map_err(map_thread_error),
        "user" => user::run_with_run_id(&state, claimed, run_context.crawl_run_id)
            .await
            .map(|summary| Json(serde_json::to_value(summary).expect("summary must serialize")))
            .map_err(map_user_error),
        _ => Err(bad_request()),
    }
}

async fn refresh_after_configuration_change(
    state: &AppState,
    id: String,
) -> Result<WatchTarget, (StatusCode, Json<ApiError>)> {
    if let Some(skip) = watch::reevaluate_no_fetch_period(
        &state.pool,
        state.config.database_backend,
        &id,
        state.config.scheduler.timezone_offset,
    )
    .await
    .map_err(internal_error)?
    {
        metrics::crawl_skipped_no_fetch();
        tracing::info!(
            crawl_run_id = %skip.crawl_run_id,
            watch_id = %id,
            trigger_kind = "scheduled",
            no_fetch_until = %no_fetch::format_rfc3339(skip.no_fetch_until),
            "automatic watch run skipped during a no-fetch period"
        );
    }
    watch::find(&state.pool, &id)
        .await
        .map_err(internal_error)?
        .ok_or_else(not_found)
}

fn map_create_error(error: CreateWatchError) -> (StatusCode, Json<ApiError>) {
    match error {
        CreateWatchError::Conflict => conflict("watch_already_exists"),
        CreateWatchError::InvalidChannel => (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid_notification_channel",
            }),
        ),
        CreateWatchError::Database(error) => internal_error(error),
    }
}

fn map_thread_error(error: ThreadCollectorError) -> (StatusCode, Json<ApiError>) {
    let (status, kind) = match error {
        ThreadCollectorError::Credentials => (
            StatusCode::PRECONDITION_FAILED,
            "nga_account_not_configured",
        ),
        ThreadCollectorError::Nga(crate::nga::NgaRequestError::NotFound) => {
            (StatusCode::NOT_FOUND, "nga_thread_not_found")
        }
        ThreadCollectorError::Nga(crate::nga::NgaRequestError::Unauthorized) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "nga_credentials_invalid")
        }
        ThreadCollectorError::Nga(_) | ThreadCollectorError::Parse(_) => {
            (StatusCode::BAD_GATEWAY, "nga_crawl_failed")
        }
        ThreadCollectorError::Database(_) | ThreadCollectorError::InvalidWatch => {
            (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
    };
    (status, Json(ApiError { error: kind }))
}

fn map_user_error(error: UserCollectorError) -> (StatusCode, Json<ApiError>) {
    let (status, kind) = match error {
        UserCollectorError::Credentials => (
            StatusCode::PRECONDITION_FAILED,
            "nga_account_not_configured",
        ),
        UserCollectorError::FullCookieRequired => {
            (StatusCode::PRECONDITION_FAILED, "nga_full_cookie_required")
        }
        UserCollectorError::Nga(crate::nga::NgaRequestError::Unauthorized) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "nga_credentials_invalid")
        }
        UserCollectorError::UserParse(
            crate::nga::user_parser::UserParseError::ProfileNotFound
            | crate::nga::user_parser::UserParseError::UidMismatch,
        ) => (StatusCode::NOT_FOUND, "nga_user_not_found"),
        UserCollectorError::Nga(_)
        | UserCollectorError::ThreadParse(_)
        | UserCollectorError::UserParse(_)
        | UserCollectorError::InvalidDetail => (StatusCode::BAD_GATEWAY, "nga_crawl_failed"),
        UserCollectorError::Database(_) | UserCollectorError::InvalidWatch => {
            (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
    };
    (status, Json(ApiError { error: kind }))
}

fn valid_interval(value: i32) -> bool {
    schedule::validate_interval(value)
}

fn valid_schedule(value: Option<&Schedule>) -> bool {
    value.is_none_or(|items| schedule::validate_schedule(items).is_ok())
}

fn valid_no_fetch_periods(value: Option<&NoFetchPeriods>) -> bool {
    value.is_none_or(|items| no_fetch::validate_no_fetch_periods(items).is_ok())
}

fn valid_history(value: &HistoryRequest) -> bool {
    matches!(value.mode.as_str(), "full" | "incremental")
        && (1..=16).contains(&value.parallelism)
        && (value.mode == "full" || !value.parallel_enabled)
}

fn valid_notification(value: &NotificationRequest, allow_authors: bool) -> bool {
    !value.channel_ids.is_empty()
        && unique(&value.channel_ids)
        && (allow_authors || value.author_uids.is_none())
        && value
            .author_uids
            .as_deref()
            .unwrap_or_default()
            .iter()
            .all(|uid| *uid > 0)
        && unique(value.author_uids.as_deref().unwrap_or_default())
}

fn unique<T: Eq + std::hash::Hash>(values: &[T]) -> bool {
    values.iter().collect::<HashSet<_>>().len() == values.len()
}

fn default_history_mode() -> String {
    "full".to_owned()
}

fn default_parallelism() -> i32 {
    2
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

fn conflict(error: &'static str) -> (StatusCode, Json<ApiError>) {
    (StatusCode::CONFLICT, Json(ApiError { error }))
}

fn internal_error(_: sqlx::Error) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "internal_error",
        }),
    )
}

impl WatchResponse {
    fn from_target(value: WatchTarget, timezone_offset: time::UtcOffset) -> Self {
        let no_fetch_window = no_fetch::current_window(
            value.no_fetch_periods.as_ref(),
            time::OffsetDateTime::now_utc(),
            timezone_offset,
        );
        Self {
            id: value.id,
            target_type: value.target_type,
            target_id: value.target_id,
            target_name: if value.target_name.trim().is_empty() {
                value.target_id.to_string()
            } else {
                value.target_name
            },
            enabled: value.enabled,
            interval_seconds: value.interval_seconds,
            schedule: value.schedule,
            no_fetch_periods: value.no_fetch_periods,
            no_fetch_active: no_fetch_window.is_some(),
            no_fetch_until: no_fetch_window.map(|window| no_fetch::format_rfc3339(window.until)),
            scheduler_timezone_offset: format_timezone_offset(timezone_offset),
            status: value.status,
            baseline_completed: value.baseline_completed,
            next_run_at: value.next_run_at,
            last_completed_at: value.last_completed_at,
            last_error_kind: value.last_error_kind,
            history: value.history_mode.map(|mode| HistoryResponse {
                mode,
                parallel_enabled: value.history_parallel_enabled,
                parallelism: value.history_parallelism,
            }),
            notification: NotificationResponse {
                channel_ids: value.channel_ids,
                author_uids: value.author_uids,
            },
        }
    }
}

fn format_timezone_offset(offset: time::UtcOffset) -> String {
    let seconds = offset.whole_seconds();
    let sign = if seconds < 0 { '-' } else { '+' };
    let seconds = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", seconds / 3_600, (seconds % 3_600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_create_rejects_tid_only_fields() {
        let history = serde_json::from_str::<CreateUserWatchRequest>(
            r#"{
                "uid": 150058,
                "history": {"mode": "full"},
                "notification": {"channel_ids": ["channel-id"]}
            }"#,
        );
        assert!(history.is_err());

        let request = serde_json::from_str::<CreateUserWatchRequest>(
            r#"{
                "uid": 150058,
                "notification": {
                    "channel_ids": ["channel-id"],
                    "author_uids": []
                }
            }"#,
        )
        .expect("notification body should deserialize before semantic validation");
        assert!(!valid_notification(&request.notification, false));
    }

    #[test]
    fn thread_notification_defaults_to_all_authors() {
        let request = serde_json::from_str::<CreateThreadWatchRequest>(
            r#"{
                "tid": 47264819,
                "notification": {"channel_ids": ["channel-id"]}
            }"#,
        )
        .expect("thread request should deserialize");
        assert!(valid_notification(&request.notification, true));
        assert!(request.notification.author_uids.is_none());
    }

    #[test]
    fn no_fetch_patch_has_three_state_replacement_semantics() {
        let missing: UpdateWatchRequest =
            serde_json::from_str(r#"{"interval_seconds":120}"#).expect("patch must parse");
        assert!(matches!(missing.no_fetch_periods, PatchField::Missing));

        let clear: UpdateWatchRequest =
            serde_json::from_str(r#"{"no_fetch_periods":null}"#).expect("null patch must parse");
        assert!(matches!(clear.no_fetch_periods, PatchField::Null));

        let replacement: UpdateWatchRequest = serde_json::from_str(
            r#"{"no_fetch_periods":[{"days":["weekdays"],"start_time":"00:00","end_time":"08:00"}]}"#,
        )
        .expect("replacement patch must parse");
        let PatchField::Value(periods) = replacement.no_fetch_periods else {
            panic!("replacement must carry a value");
        };
        assert!(valid_no_fetch_periods(Some(&periods)));
        assert!(!valid_no_fetch_periods(Some(&Vec::new())));
    }

    #[test]
    fn collector_auth_failure_is_not_reported_as_admin_unauthorized() {
        let (thread_status, thread_error) = map_thread_error(ThreadCollectorError::Nga(
            crate::nga::NgaRequestError::Unauthorized,
        ));
        let (user_status, user_error) = map_user_error(UserCollectorError::Nga(
            crate::nga::NgaRequestError::Unauthorized,
        ));

        assert_eq!(thread_status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(thread_error.error, "nga_credentials_invalid");
        assert_eq!(user_status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(user_error.error, "nga_credentials_invalid");
    }
}
