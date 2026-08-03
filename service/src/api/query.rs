use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::app::AppState;

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct EventQuery {
    limit: Option<i64>,
    watch_id: Option<String>,
    tid: Option<i64>,
    uid: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct OverviewResponse {
    pub service: &'static str,
    pub version: &'static str,
    pub threads: i64,
    pub posts: i64,
    pub watches: i64,
    pub active_watches: i64,
    pub unread_events: i64,
    pub channels: i64,
    pub ready_assets: i64,
    pub assets_download_enabled: bool,
    pub assets_storage_path: String,
}

#[derive(Debug, Serialize)]
pub struct ThreadView {
    pub tid: i64,
    pub title: String,
    pub forum_name: String,
    pub author_name: String,
    pub author_uid: i64,
    pub post_count: i64,
    pub last_seen_at: String,
}

#[derive(Debug, Serialize)]
pub struct PostView {
    pub id: String,
    pub tid: i64,
    pub pid: Option<i64>,
    pub floor_number: Option<i32>,
    pub post_kind: String,
    pub author_name: String,
    pub author_uid: i64,
    pub subject: String,
    pub preview: String,
    pub published_at_unix: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct EventView {
    pub id: String,
    pub event_type: String,
    pub post_id: String,
    pub tid: i64,
    pub pid: Option<i64>,
    pub thread_title: String,
    pub author_name: String,
    pub preview: String,
    pub occurred_at: String,
    pub read_at: Option<String>,
    pub uid_watch_source: bool,
}

#[derive(Debug, Serialize)]
pub struct ListResponse<T> {
    pub items: Vec<T>,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    error: &'static str,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

pub async fn overview(State(state): State<AppState>) -> ApiResult<OverviewResponse> {
    let threads = count(&state, "SELECT COUNT(*) FROM threads").await?;
    let posts = count(&state, "SELECT COUNT(*) FROM posts").await?;
    let watches = count(
        &state,
        "SELECT COUNT(*) FROM watch_targets WHERE deleted_at IS NULL",
    )
    .await?;
    let active_watches = count(
        &state,
        "SELECT COUNT(*) FROM watch_targets WHERE enabled = 1 AND deleted_at IS NULL",
    )
    .await?;
    let unread_events = count(
        &state,
        "SELECT COUNT(*) FROM post_events WHERE read_at IS NULL",
    )
    .await?;
    let channels = count(&state, "SELECT COUNT(*) FROM notification_channels").await?;
    let ready_assets = count(
        &state,
        "SELECT COUNT(*) FROM assets WHERE download_status = 'ready'",
    )
    .await?;

    Ok(Json(OverviewResponse {
        service: "nga-reminder",
        version: env!("CARGO_PKG_VERSION"),
        threads,
        posts,
        watches,
        active_watches,
        unread_events,
        channels,
        ready_assets,
        assets_download_enabled: state.config.assets.download_enabled,
        assets_storage_path: state.config.assets.storage_path.display().to_string(),
    }))
}

pub async fn threads(
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> ApiResult<ListResponse<ThreadView>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let rows = sqlx::query(
        "SELECT t.tid, t.title, t.forum_name, t.author_name, t.author_uid,
         CAST(COUNT(p.id) AS BIGINT) AS post_count,
         CAST(t.last_seen_at AS TEXT) AS last_seen_at
         FROM threads t LEFT JOIN posts p ON p.tid = t.tid
         GROUP BY t.tid, t.title, t.forum_name, t.author_name, t.author_uid, t.last_seen_at
         ORDER BY t.last_seen_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    Ok(Json(ListResponse {
        items: rows.iter().map(map_thread).collect(),
    }))
}

pub async fn posts(
    State(state): State<AppState>,
    Path(tid): Path<i64>,
    Query(query): Query<LimitQuery>,
) -> ApiResult<ListResponse<PostView>> {
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let rows = sqlx::query(
        "SELECT id, tid, pid, floor_number, post_kind, author_name, author_uid,
         subject, content_raw, published_at_unix
         FROM posts WHERE tid = $1 ORDER BY COALESCE(floor_number, 0), published_at_unix NULLS LAST
         LIMIT $2",
    )
    .bind(tid)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    Ok(Json(ListResponse {
        items: rows.iter().map(map_post).collect(),
    }))
}

pub async fn events(
    State(state): State<AppState>,
    Query(query): Query<EventQuery>,
) -> ApiResult<ListResponse<EventView>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let watch_id = query.watch_id.unwrap_or_default();
    let tid = query.tid.unwrap_or_default();
    let uid = query.uid.unwrap_or_default();
    let rows = sqlx::query(
        "SELECT e.id, e.event_type, e.post_id, p.tid, p.pid, t.title AS thread_title,
         p.author_name, p.content_raw, CAST(e.occurred_at AS TEXT) AS occurred_at,
         CAST(e.read_at AS TEXT) AS read_at,
         CASE WHEN EXISTS (
             SELECT 1 FROM post_event_watch_matches source
             JOIN watch_targets source_watch ON source_watch.id = source.watch_id
             WHERE source.post_event_id = e.id AND source_watch.target_type = 'user'
         ) THEN 1 ELSE 0 END AS uid_watch_source
         FROM post_events e
         JOIN posts p ON p.id = e.post_id
         JOIN threads t ON t.tid = p.tid
         WHERE ($1 = '' OR EXISTS (
             SELECT 1 FROM post_event_watch_matches filter_source
             WHERE filter_source.post_event_id = e.id AND filter_source.watch_id = $1
         ))
           AND ($2 = 0 OR p.tid = $2)
           AND ($3 = 0 OR p.author_uid = $3)
         ORDER BY e.occurred_at DESC LIMIT $4",
    )
    .bind(watch_id)
    .bind(tid)
    .bind(uid)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    Ok(Json(ListResponse {
        items: rows.iter().map(map_event).collect(),
    }))
}

pub async fn mark_event_read(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let changed = sqlx::query(
        "UPDATE post_events SET read_at = CURRENT_TIMESTAMP WHERE id = $1 AND read_at IS NULL",
    )
    .bind(&id)
    .execute(&state.pool)
    .await
    .map_err(internal)?
    .rows_affected();
    if changed == 0 {
        let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM post_events WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?;
        if exists.is_none() {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: "event_not_found",
                }),
            ));
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn mark_all_events_read(
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    sqlx::query("UPDATE post_events SET read_at = CURRENT_TIMESTAMP WHERE read_at IS NULL")
        .execute(&state.pool)
        .await
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn count(state: &AppState, query: &str) -> Result<i64, (StatusCode, Json<ApiError>)> {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .map_err(internal)
}

fn map_thread(row: &sqlx::any::AnyRow) -> ThreadView {
    ThreadView {
        tid: row.get("tid"),
        title: row.get("title"),
        forum_name: row.get("forum_name"),
        author_name: row.get("author_name"),
        author_uid: row.get("author_uid"),
        post_count: row.get("post_count"),
        last_seen_at: row.get("last_seen_at"),
    }
}

fn map_post(row: &sqlx::any::AnyRow) -> PostView {
    PostView {
        id: row.get("id"),
        tid: row.get("tid"),
        pid: row.get("pid"),
        floor_number: row.get("floor_number"),
        post_kind: row.get("post_kind"),
        author_name: row.get("author_name"),
        author_uid: row.get("author_uid"),
        subject: row.get("subject"),
        preview: preview(row.get("content_raw")),
        published_at_unix: row.get("published_at_unix"),
    }
}

fn map_event(row: &sqlx::any::AnyRow) -> EventView {
    EventView {
        id: row.get("id"),
        event_type: row.get("event_type"),
        post_id: row.get("post_id"),
        tid: row.get("tid"),
        pid: row.get("pid"),
        thread_title: row.get("thread_title"),
        author_name: row.get("author_name"),
        preview: preview(row.get("content_raw")),
        occurred_at: row.get("occurred_at"),
        read_at: row.get("read_at"),
        uid_watch_source: row.get::<i32, _>("uid_watch_source") == 1,
    }
}

fn preview(content: String) -> String {
    let mut output = String::with_capacity(content.len().min(180));
    let mut inside_tag = false;
    for character in content.chars() {
        match character {
            '<' => inside_tag = true,
            '>' if inside_tag => inside_tag = false,
            _ if !inside_tag => output.push(character),
            _ => {}
        }
        if output.chars().count() >= 160 {
            break;
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn internal(_: sqlx::Error) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "internal_error",
        }),
    )
}
