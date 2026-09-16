use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sqlx::Row;

use crate::app::AppState;

static STARTED_AT: OnceLock<Instant> = OnceLock::new();
static HTTP_REQUESTS: AtomicU64 = AtomicU64::new(0);
static HTTP_2XX: AtomicU64 = AtomicU64::new(0);
static HTTP_4XX: AtomicU64 = AtomicU64::new(0);
static HTTP_5XX: AtomicU64 = AtomicU64::new(0);
static WORKER_CYCLES: AtomicU64 = AtomicU64::new(0);
static CRAWL_SUCCEEDED: AtomicU64 = AtomicU64::new(0);
static CRAWL_FAILED: AtomicU64 = AtomicU64::new(0);
static CRAWL_SKIPPED_NO_FETCH: AtomicU64 = AtomicU64::new(0);
static ASSET_JOBS: AtomicU64 = AtomicU64::new(0);
static NOTIFICATION_JOBS: AtomicU64 = AtomicU64::new(0);
static BOT_INBOUND_HANDLED: AtomicU64 = AtomicU64::new(0);
static BOT_INBOUND_REJECTED: AtomicU64 = AtomicU64::new(0);
static BOT_INBOUND_DUPLICATE: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    STARTED_AT.get_or_init(Instant::now);
}

pub async fn record_http(request: Request, next: Next) -> Response {
    HTTP_REQUESTS.fetch_add(1, Ordering::Relaxed);
    let response = next.run(request).await;
    match response.status().as_u16() {
        200..=399 => {
            HTTP_2XX.fetch_add(1, Ordering::Relaxed);
        }
        400..=499 => {
            HTTP_4XX.fetch_add(1, Ordering::Relaxed);
        }
        _ => {
            HTTP_5XX.fetch_add(1, Ordering::Relaxed);
        }
    }
    response
}

pub async fn endpoint(State(state): State<AppState>) -> impl IntoResponse {
    init();
    let uptime = STARTED_AT
        .get()
        .map_or(0, |started| started.elapsed().as_secs());
    let runtime = sqlx::query(
        "SELECT
          (SELECT COUNT(*) FROM platform_integrations WHERE bot_enabled = 1 AND enabled = 1) AS bot_connections,
          (SELECT COUNT(*) FROM bot_outbox WHERE status IN ('pending','failed','sending')) AS bot_outbox_pending,
          (SELECT COUNT(*) FROM nga_login_sessions WHERE status IN
            ('awaiting_confirmation','starting','awaiting_captcha','submitting','validating_cookie')) AS login_sessions_active,
          (SELECT COUNT(*) FROM watch_targets WHERE pause_reason = 'auth' AND deleted_at IS NULL) AS auth_paused_watches",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    let value = |column| runtime.as_ref().map_or(0_i64, |row| row.get(column));
    let body = format!(
        "# HELP nga_reminder_uptime_seconds Service uptime in seconds.\n\
# TYPE nga_reminder_uptime_seconds gauge\n\
nga_reminder_uptime_seconds {uptime}\n\
# HELP nga_reminder_http_requests_total Total HTTP requests received.\n\
# TYPE nga_reminder_http_requests_total counter\n\
nga_reminder_http_requests_total {}\n\
# HELP nga_reminder_http_responses_total HTTP responses by status class.\n\
# TYPE nga_reminder_http_responses_total counter\n\
nga_reminder_http_responses_total{{class=\"2xx\"}} {}\n\
nga_reminder_http_responses_total{{class=\"4xx\"}} {}\n\
nga_reminder_http_responses_total{{class=\"5xx\"}} {}\n\
# HELP nga_reminder_worker_cycles_total Worker scheduler cycles.\n\
# TYPE nga_reminder_worker_cycles_total counter\n\
nga_reminder_worker_cycles_total {}\n\
# HELP nga_reminder_crawls_total Completed crawl runs by outcome.\n\
# TYPE nga_reminder_crawls_total counter\n\
nga_reminder_crawls_total{{outcome=\"succeeded\"}} {}\n\
nga_reminder_crawls_total{{outcome=\"failed\"}} {}\n\
# HELP crawl_runs_skipped_no_fetch_total Automatic crawl runs skipped by a no-fetch period.\n\
# TYPE crawl_runs_skipped_no_fetch_total counter\n\
crawl_runs_skipped_no_fetch_total {}\n\
# HELP nga_reminder_asset_jobs_total Asset jobs processed.\n\
# TYPE nga_reminder_asset_jobs_total counter\n\
nga_reminder_asset_jobs_total {}\n\
# HELP nga_reminder_notification_jobs_total Notification jobs processed.\n\
# TYPE nga_reminder_notification_jobs_total counter\n\
nga_reminder_notification_jobs_total {}\n\
# HELP nga_reminder_bot_inbound_events_total Bot inbound command outcomes.\n\
# TYPE nga_reminder_bot_inbound_events_total counter\n\
nga_reminder_bot_inbound_events_total{{status=\"handled\"}} {}\n\
nga_reminder_bot_inbound_events_total{{status=\"rejected\"}} {}\n\
nga_reminder_bot_inbound_events_total{{status=\"duplicate\"}} {}\n\
# HELP nga_reminder_bot_connections Active bot-enabled platform connections.\n\
# TYPE nga_reminder_bot_connections gauge\n\
nga_reminder_bot_connections {}\n\
# HELP nga_reminder_bot_outbox_pending Pending, failed or leased bot replies.\n\
# TYPE nga_reminder_bot_outbox_pending gauge\n\
nga_reminder_bot_outbox_pending {}\n\
# HELP nga_reminder_nga_login_sessions_active Active NGA renewal sessions.\n\
# TYPE nga_reminder_nga_login_sessions_active gauge\n\
nga_reminder_nga_login_sessions_active {}\n\
# HELP nga_reminder_nga_auth_paused_watches Watches paused by authentication failure.\n\
# TYPE nga_reminder_nga_auth_paused_watches gauge\n\
nga_reminder_nga_auth_paused_watches {}\n",
        HTTP_REQUESTS.load(Ordering::Relaxed),
        HTTP_2XX.load(Ordering::Relaxed),
        HTTP_4XX.load(Ordering::Relaxed),
        HTTP_5XX.load(Ordering::Relaxed),
        WORKER_CYCLES.load(Ordering::Relaxed),
        CRAWL_SUCCEEDED.load(Ordering::Relaxed),
        CRAWL_FAILED.load(Ordering::Relaxed),
        CRAWL_SKIPPED_NO_FETCH.load(Ordering::Relaxed),
        ASSET_JOBS.load(Ordering::Relaxed),
        NOTIFICATION_JOBS.load(Ordering::Relaxed),
        BOT_INBOUND_HANDLED.load(Ordering::Relaxed),
        BOT_INBOUND_REJECTED.load(Ordering::Relaxed),
        BOT_INBOUND_DUPLICATE.load(Ordering::Relaxed),
        value("bot_connections"),
        value("bot_outbox_pending"),
        value("login_sessions_active"),
        value("auth_paused_watches"),
    );
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

pub fn worker_cycle() {
    WORKER_CYCLES.fetch_add(1, Ordering::Relaxed);
}

pub fn asset_job() {
    ASSET_JOBS.fetch_add(1, Ordering::Relaxed);
}

pub fn notification_job() {
    NOTIFICATION_JOBS.fetch_add(1, Ordering::Relaxed);
}

pub fn crawl_succeeded() {
    CRAWL_SUCCEEDED.fetch_add(1, Ordering::Relaxed);
}

pub fn crawl_failed() {
    CRAWL_FAILED.fetch_add(1, Ordering::Relaxed);
}

pub fn crawl_skipped_no_fetch() {
    CRAWL_SKIPPED_NO_FETCH.fetch_add(1, Ordering::Relaxed);
}

pub fn bot_inbound_handled() {
    BOT_INBOUND_HANDLED.fetch_add(1, Ordering::Relaxed);
}

pub fn bot_inbound_rejected() {
    BOT_INBOUND_REJECTED.fetch_add(1, Ordering::Relaxed);
}

pub fn bot_inbound_duplicate() {
    BOT_INBOUND_DUPLICATE.fetch_add(1, Ordering::Relaxed);
}
