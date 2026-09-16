use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime, Time, UtcOffset, format_description};

use crate::{
    app::AppState,
    repository::user_backfill::{self, CreateBackfillError, UserReplyBackfillJob},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBackfillRequest {
    uid: i64,
    start_date: String,
}

#[derive(Debug, Serialize)]
pub struct BackfillResponse {
    id: String,
    watch_id: String,
    uid: i64,
    start_date: String,
    start_at_unix: i64,
    end_at_unix: i64,
    status: String,
    next_page: i32,
    pages_requested: i32,
    candidates_processed: i32,
    posts_inserted: i32,
    error_kind: Option<String>,
    error_message: Option<String>,
    created_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BackfillListResponse {
    items: Vec<BackfillResponse>,
    scheduler_timezone_offset: String,
    current_date: String,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    error: &'static str,
}

pub async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateBackfillRequest>,
) -> Result<(StatusCode, Json<BackfillResponse>), (StatusCode, Json<ApiError>)> {
    if request.uid <= 0 {
        return Err(bad_request());
    }
    let now = OffsetDateTime::now_utc();
    let start_at_unix = start_boundary(
        &request.start_date,
        state.config.scheduler.timezone_offset,
        now,
    )
    .ok_or_else(bad_request)?;
    let created = user_backfill::create(
        &state.pool,
        request.uid,
        &request.start_date,
        start_at_unix,
        now.unix_timestamp(),
    )
    .await
    .map_err(map_create_error)?;
    Ok((StatusCode::CREATED, Json(created.into())))
}

pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<BackfillListResponse>, (StatusCode, Json<ApiError>)> {
    let items = user_backfill::list(&state.pool)
        .await
        .map_err(internal_error)?
        .into_iter()
        .map(BackfillResponse::from)
        .collect();
    let offset = state.config.scheduler.timezone_offset;
    Ok(Json(BackfillListResponse {
        items,
        scheduler_timezone_offset: format_timezone_offset(offset),
        current_date: OffsetDateTime::now_utc()
            .to_offset(offset)
            .date()
            .to_string(),
    }))
}

fn start_boundary(value: &str, offset: UtcOffset, now: OffsetDateTime) -> Option<i64> {
    let format = format_description::parse_borrowed::<2>("[year]-[month]-[day]").ok()?;
    let date = Date::parse(value, &format).ok()?;
    if date > now.to_offset(offset).date() {
        return None;
    }
    Some(OffsetDateTime::new_in_offset(date, Time::MIDNIGHT, offset).unix_timestamp())
}

fn map_create_error(error: CreateBackfillError) -> (StatusCode, Json<ApiError>) {
    match error {
        CreateBackfillError::WatchNotFound => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "uid_watch_not_found",
            }),
        ),
        CreateBackfillError::Conflict => (
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "uid_reply_backfill_active",
            }),
        ),
        CreateBackfillError::Database(error) => internal_error(error),
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

fn internal_error(_: sqlx::Error) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "internal_error",
        }),
    )
}

fn format_timezone_offset(offset: UtcOffset) -> String {
    let seconds = offset.whole_seconds();
    let sign = if seconds < 0 { '-' } else { '+' };
    let seconds = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", seconds / 3_600, (seconds % 3_600) / 60)
}

impl From<UserReplyBackfillJob> for BackfillResponse {
    fn from(value: UserReplyBackfillJob) -> Self {
        Self {
            id: value.id,
            watch_id: value.watch_id,
            uid: value.uid,
            start_date: value.start_date,
            start_at_unix: value.start_at_unix,
            end_at_unix: value.end_at_unix,
            status: value.status,
            next_page: value.next_page,
            pages_requested: value.pages_requested,
            candidates_processed: value.candidates_processed,
            posts_inserted: value.posts_inserted,
            error_kind: value.error_kind,
            error_message: value.error_message,
            created_at: value.created_at,
            started_at: value.started_at,
            completed_at: value.completed_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use time::{Date, Month, OffsetDateTime, Time, UtcOffset};

    use super::start_boundary;

    #[test]
    fn start_date_uses_service_timezone_and_is_inclusive() {
        let offset = UtcOffset::from_hms(8, 0, 0).expect("offset must be valid");
        let now = OffsetDateTime::new_in_offset(
            Date::from_calendar_date(2026, Month::September, 14).expect("date must be valid"),
            Time::from_hms(10, 0, 0).expect("time must be valid"),
            offset,
        );
        let boundary =
            start_boundary("2026-09-14", offset, now).expect("current local date must be accepted");
        let expected = OffsetDateTime::new_in_offset(
            Date::from_calendar_date(2026, Month::September, 14).expect("date must be valid"),
            Time::MIDNIGHT,
            offset,
        )
        .unix_timestamp();
        assert_eq!(boundary, expected);
    }

    #[test]
    fn malformed_and_future_service_dates_are_rejected() {
        let now =
            OffsetDateTime::from_unix_timestamp(1_789_344_000).expect("timestamp must be valid");
        assert!(start_boundary("not-a-date", UtcOffset::UTC, now).is_none());
        assert!(start_boundary("2099-01-01", UtcOffset::UTC, now).is_none());
    }
}
