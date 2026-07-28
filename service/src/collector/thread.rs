use std::collections::{HashMap, HashSet};

use secrecy::ExposeSecret;
use sqlx::{Any, Row, Transaction};
use thiserror::Error;
use time::OffsetDateTime;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::AppState,
    config::DatabaseBackend,
    domain::thread::{ParsedPost, PostKind, ThreadMetadata, ThreadPage},
    nga::{NgaRequestError, thread_parser},
    notification,
    repository::watch::{self, ThreadCursor, WatchTarget},
    schedule,
};

#[derive(Debug, serde::Serialize)]
pub struct CrawlSummary {
    pub crawl_run_id: String,
    pub tid: i64,
    pub baseline: bool,
    pub pages_requested: i32,
    pub posts_inserted: i32,
    pub events_created: i32,
    pub remote_vrows: i32,
    pub last_floor: i32,
}

#[derive(Debug, Error)]
pub enum ThreadCollectorError {
    #[error("watch is not a thread watch")]
    InvalidWatch,
    #[error("NGA account is not configured or cannot be decrypted")]
    Credentials,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Nga(#[from] NgaRequestError),
    #[error(transparent)]
    Parse(#[from] thread_parser::ThreadParseError),
}

pub async fn run(
    state: &AppState,
    watch_target: WatchTarget,
) -> Result<CrawlSummary, ThreadCollectorError> {
    if watch_target.target_type != "thread" {
        return Err(ThreadCollectorError::InvalidWatch);
    }

    let baseline = !watch_target.baseline_completed;
    let run_id = Uuid::new_v4().to_string();
    create_crawl_run(state, &run_id, &watch_target.id, baseline).await?;

    let result = collect(state, &run_id, &watch_target, baseline).await;
    if let Err(error) = &result {
        record_failure(state, &run_id, &watch_target, error).await;
    }
    result
}

async fn collect(
    state: &AppState,
    run_id: &str,
    watch_target: &WatchTarget,
    baseline: bool,
) -> Result<CrawlSummary, ThreadCollectorError> {
    let cursor = watch::thread_cursor(&state.pool, &watch_target.id).await?;
    let (passport_uid, passport_cid) = load_credentials(state).await?;
    let first_value = state
        .nga_client
        .fetch_thread_page(
            passport_uid.expose_secret(),
            passport_cid.expose_secret(),
            watch_target.target_id,
            1,
        )
        .await?;
    let first_page = thread_parser::parse_thread_page(&first_value, watch_target.target_id)?;
    if first_page.current_page != 1 {
        return Err(thread_parser::ThreadParseError::Pagination.into());
    }

    let mut pages = vec![first_page];
    if baseline {
        fetch_pages(
            state,
            &passport_uid,
            &passport_cid,
            watch_target.target_id,
            2,
            pages[0].metadata.total_pages,
            &mut pages,
        )
        .await?;
    } else if pages[0].metadata.vrows > cursor.remote_vrows {
        let start_page = cursor.last_floor.div_euclid(pages[0].metadata.per_page) + 1;
        fetch_pages(
            state,
            &passport_uid,
            &passport_cid,
            watch_target.target_id,
            start_page.max(2),
            pages[0].metadata.total_pages,
            &mut pages,
        )
        .await?;
    }

    persist_pages(state, run_id, watch_target, &cursor, baseline, pages).await
}

async fn fetch_pages(
    state: &AppState,
    passport_uid: &secrecy::SecretString,
    passport_cid: &secrecy::SecretString,
    tid: i64,
    start_page: i32,
    end_page: i32,
    pages: &mut Vec<ThreadPage>,
) -> Result<(), ThreadCollectorError> {
    if start_page > end_page {
        return Ok(());
    }
    for page_number in start_page..=end_page {
        let value = state
            .nga_client
            .fetch_thread_page(
                passport_uid.expose_secret(),
                passport_cid.expose_secret(),
                tid,
                page_number,
            )
            .await?;
        let page = thread_parser::parse_thread_page(&value, tid)?;
        if page.current_page != page_number {
            return Err(thread_parser::ThreadParseError::Pagination.into());
        }
        pages.push(page);
    }
    Ok(())
}

async fn persist_pages(
    state: &AppState,
    run_id: &str,
    watch_target: &WatchTarget,
    cursor: &ThreadCursor,
    baseline: bool,
    pages: Vec<ThreadPage>,
) -> Result<CrawlSummary, ThreadCollectorError> {
    let metadata = pages[0].metadata.clone();
    let pages_requested = i32::try_from(pages.len()).unwrap_or(i32::MAX);
    let selected = select_posts(&pages, cursor.last_floor, baseline);
    let last_floor = pages
        .iter()
        .flat_map(|page| page.posts.iter())
        .filter(|post| post.kind != PostKind::Comment)
        .map(|post| post.floor_number)
        .max()
        .unwrap_or(cursor.last_floor)
        .max(cursor.last_floor);

    let mut tx = state.pool.begin().await?;
    upsert_thread(&mut tx, &metadata).await?;

    let mut canonical_ids = HashMap::new();
    let mut inserted_count = 0_i32;
    let mut event_count = 0_i32;

    for post in selected
        .iter()
        .filter(|post| post.kind != PostKind::Comment)
    {
        let (id, inserted) = insert_post(
            &mut tx,
            post,
            None,
            state.config.persistence.store_raw_payload,
            state.config.assets.download_enabled,
        )
        .await?;
        canonical_ids.insert(natural_key(post), id.clone());
        if inserted {
            inserted_count += 1;
            if !baseline && insert_event(&mut tx, &id, post, &watch_target.id).await? {
                event_count += 1;
            }
        }
    }

    for post in selected
        .iter()
        .filter(|post| post.kind == PostKind::Comment)
    {
        let parent_key = if post.parent_is_topic {
            "topic".to_owned()
        } else {
            format!(
                "pid:{}",
                post.parent_pid.ok_or(ThreadCollectorError::InvalidWatch)?
            )
        };
        let parent_id = if let Some(id) = canonical_ids.get(&parent_key) {
            id.clone()
        } else {
            find_post_id(&mut tx, post.tid, post.parent_pid, post.parent_is_topic).await?
        };
        let (id, inserted) = insert_post(
            &mut tx,
            post,
            Some(&parent_id),
            state.config.persistence.store_raw_payload,
            state.config.assets.download_enabled,
        )
        .await?;
        if inserted {
            inserted_count += 1;
            if !baseline && insert_event(&mut tx, &id, post, &watch_target.id).await? {
                event_count += 1;
            }
        }
    }

    update_cursor_and_finish(
        &mut tx,
        state.config.database_backend,
        run_id,
        watch_target,
        &metadata,
        last_floor,
        pages_requested,
        inserted_count,
        event_count,
        state.config.scheduler.timezone_offset,
    )
    .await?;
    tx.commit().await?;

    info!(
        crawl_run_id = run_id,
        watch_id = watch_target.id,
        tid = watch_target.target_id,
        baseline,
        pages_requested,
        posts_inserted = inserted_count,
        events_created = event_count,
        "thread crawl completed"
    );

    Ok(CrawlSummary {
        crawl_run_id: run_id.to_owned(),
        tid: watch_target.target_id,
        baseline,
        pages_requested,
        posts_inserted: inserted_count,
        events_created: event_count,
        remote_vrows: metadata.vrows,
        last_floor,
    })
}

fn select_posts(pages: &[ThreadPage], last_floor: i32, baseline: bool) -> Vec<ParsedPost> {
    let mut parent_keys = HashSet::new();
    let mut seen = HashSet::new();
    let mut selected = Vec::new();

    for post in pages.iter().flat_map(|page| page.posts.iter()) {
        if post.kind == PostKind::Comment {
            continue;
        }
        if baseline || post.floor_number > last_floor {
            let key = natural_key(post);
            if seen.insert(key.clone()) {
                parent_keys.insert(key);
                selected.push(post.clone());
            }
        }
    }
    for post in pages.iter().flat_map(|page| page.posts.iter()) {
        if post.kind != PostKind::Comment {
            continue;
        }
        let parent_key = if post.parent_is_topic {
            "topic".to_owned()
        } else if let Some(parent_pid) = post.parent_pid {
            format!("pid:{parent_pid}")
        } else {
            continue;
        };
        if parent_keys.contains(&parent_key) && seen.insert(natural_key(post)) {
            selected.push(post.clone());
        }
    }
    selected
}

fn natural_key(post: &ParsedPost) -> String {
    match post.kind {
        PostKind::Topic => "topic".to_owned(),
        PostKind::Reply | PostKind::Comment => format!("pid:{}", post.pid.unwrap_or_default()),
    }
}

pub(crate) async fn load_credentials(
    state: &AppState,
) -> Result<(secrecy::SecretString, secrecy::SecretString), ThreadCollectorError> {
    let row = sqlx::query(
        "SELECT passport_uid_encrypted, passport_cid_encrypted
         FROM nga_accounts WHERE label = 'default'",
    )
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ThreadCollectorError::Credentials)?;
    let uid: Vec<u8> = row.get("passport_uid_encrypted");
    let cid: Vec<u8> = row.get("passport_cid_encrypted");
    let uid = state
        .credential_cipher
        .decrypt(&uid)
        .map_err(|_| ThreadCollectorError::Credentials)?;
    let cid = state
        .credential_cipher
        .decrypt(&cid)
        .map_err(|_| ThreadCollectorError::Credentials)?;
    Ok((uid.into(), cid.into()))
}

async fn create_crawl_run(
    state: &AppState,
    run_id: &str,
    watch_id: &str,
    baseline: bool,
) -> Result<(), ThreadCollectorError> {
    sqlx::query(
        "UPDATE crawl_runs SET status = 'failed', error_kind = 'lease_expired',
         error_message = 'lease_expired', completed_at = CURRENT_TIMESTAMP
         WHERE watch_id = $1 AND status = 'running'",
    )
    .bind(watch_id)
    .execute(&state.pool)
    .await?;
    sqlx::query(
        "INSERT INTO crawl_runs (id, watch_id, status, baseline)
         VALUES ($1, $2, 'running', $3)",
    )
    .bind(run_id)
    .bind(watch_id)
    .bind(i32::from(baseline))
    .execute(&state.pool)
    .await?;
    Ok(())
}

async fn upsert_thread(
    tx: &mut Transaction<'_, Any>,
    metadata: &ThreadMetadata,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO threads
            (tid, fid, title, forum_name, author_uid, author_name, coverage,
             remote_total_pages, remote_vrows)
         VALUES ($1, $2, $3, $4, $5, $6, 'full', $7, $8)
         ON CONFLICT (tid) DO UPDATE SET
            fid = EXCLUDED.fid,
            title = EXCLUDED.title,
            forum_name = EXCLUDED.forum_name,
            author_uid = EXCLUDED.author_uid,
            author_name = EXCLUDED.author_name,
            coverage = 'full',
            remote_total_pages = EXCLUDED.remote_total_pages,
            remote_vrows = EXCLUDED.remote_vrows,
            last_seen_at = CURRENT_TIMESTAMP",
    )
    .bind(metadata.tid)
    .bind(metadata.fid)
    .bind(&metadata.title)
    .bind(&metadata.forum_name)
    .bind(metadata.author_uid)
    .bind(&metadata.author_name)
    .bind(metadata.total_pages)
    .bind(metadata.vrows)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn upsert_thread_partial(
    tx: &mut Transaction<'_, Any>,
    metadata: &ThreadMetadata,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO threads
            (tid, fid, title, forum_name, author_uid, author_name, coverage,
             remote_total_pages, remote_vrows)
         VALUES ($1, $2, $3, $4, $5, $6, 'partial', 0, 0)
         ON CONFLICT (tid) DO UPDATE SET
            fid = EXCLUDED.fid,
            title = CASE WHEN threads.title = '' THEN EXCLUDED.title ELSE threads.title END,
            forum_name = CASE WHEN threads.forum_name = '' THEN EXCLUDED.forum_name
                              ELSE threads.forum_name END,
            author_uid = EXCLUDED.author_uid,
            author_name = EXCLUDED.author_name,
            last_seen_at = CURRENT_TIMESTAMP",
    )
    .bind(metadata.tid)
    .bind(metadata.fid)
    .bind(&metadata.title)
    .bind(&metadata.forum_name)
    .bind(metadata.author_uid)
    .bind(&metadata.author_name)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn insert_post(
    tx: &mut Transaction<'_, Any>,
    post: &ParsedPost,
    parent_post_id: Option<&str>,
    store_raw_payload: bool,
    download_assets: bool,
) -> Result<(String, bool), sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let result = sqlx::query(
        "INSERT INTO posts
            (id, tid, pid, floor_number, post_kind, parent_post_id, author_uid,
             author_name, subject, content_raw, published_at_unix, page_number, raw_payload)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT DO NOTHING",
    )
    .bind(&id)
    .bind(post.tid)
    .bind(post.pid)
    .bind(post.floor_number)
    .bind(post.kind.as_str())
    .bind(parent_post_id)
    .bind(post.author_uid)
    .bind(&post.author_name)
    .bind(&post.subject)
    .bind(&post.content_raw)
    .bind(post.published_at_unix)
    .bind(post.page_number)
    .bind(raw_payload_for_storage(post, store_raw_payload))
    .execute(&mut **tx)
    .await?;

    if result.rows_affected() == 1 {
        crate::assets::record_post_assets(tx, &id, post, download_assets).await?;
        return Ok((id, true));
    }
    let existing = find_post_id(tx, post.tid, post.pid, post.kind == PostKind::Topic).await?;
    Ok((existing, false))
}

fn raw_payload_for_storage(post: &ParsedPost, store_raw_payload: bool) -> &str {
    if store_raw_payload {
        &post.raw_payload
    } else {
        ""
    }
}

pub(crate) async fn find_post_id(
    tx: &mut Transaction<'_, Any>,
    tid: i64,
    pid: Option<i64>,
    topic: bool,
) -> Result<String, sqlx::Error> {
    let row = if topic {
        sqlx::query("SELECT id FROM posts WHERE tid = $1 AND post_kind = 'topic'")
            .bind(tid)
            .fetch_one(&mut **tx)
            .await?
    } else {
        sqlx::query(
            "SELECT id FROM posts
             WHERE tid = $1 AND pid = $2 AND post_kind IN ('reply', 'comment')",
        )
        .bind(tid)
        .bind(pid)
        .fetch_one(&mut **tx)
        .await?
    };
    Ok(row.get("id"))
}

pub(crate) async fn insert_event(
    tx: &mut Transaction<'_, Any>,
    post_id: &str,
    post: &ParsedPost,
    watch_id: &str,
) -> Result<bool, sqlx::Error> {
    let event_id = Uuid::new_v4().to_string();
    let result = sqlx::query(
        "INSERT INTO post_events
            (id, post_id, event_type, discovered_by_watch_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING",
    )
    .bind(&event_id)
    .bind(post_id)
    .bind(post.kind.event_type())
    .bind(watch_id)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 1 {
        notification::enqueue_matches(tx, &event_id, post).await?;
    }
    Ok(result.rows_affected() == 1)
}

#[allow(clippy::too_many_arguments)]
async fn update_cursor_and_finish(
    tx: &mut Transaction<'_, Any>,
    backend: DatabaseBackend,
    run_id: &str,
    watch_target: &WatchTarget,
    metadata: &ThreadMetadata,
    last_floor: i32,
    pages_requested: i32,
    posts_inserted: i32,
    events_created: i32,
    timezone_offset: time::UtcOffset,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE watch_cursors SET last_floor = $1, remote_vrows = $2,
         remote_total_pages = $3, updated_at = CURRENT_TIMESTAMP
         WHERE watch_id = $4",
    )
    .bind(last_floor)
    .bind(metadata.vrows)
    .bind(metadata.total_pages)
    .bind(&watch_target.id)
    .execute(&mut **tx)
    .await?;

    let next_run = match backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + ($2 * INTERVAL '1 second')",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+' || $2 || ' seconds')",
    };
    let delay = schedule::next_delay_seconds(
        watch_target.schedule.as_ref(),
        watch_target.interval_seconds,
        OffsetDateTime::now_utc(),
        timezone_offset,
    );
    let query = format!(
        "UPDATE watch_targets SET status = 'active', baseline_completed = 1,
         next_run_at = {next_run}, lease_until = NULL,
         last_completed_at = CURRENT_TIMESTAMP, last_error_kind = NULL,
         last_error_message = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = $1"
    );
    sqlx::query(&query)
        .bind(&watch_target.id)
        .bind(delay)
        .execute(&mut **tx)
        .await?;

    sqlx::query(
        "UPDATE crawl_runs SET status = 'succeeded', pages_requested = $1,
         posts_inserted = $2, events_created = $3, remote_vrows = $4,
         completed_at = CURRENT_TIMESTAMP WHERE id = $5",
    )
    .bind(pages_requested)
    .bind(posts_inserted)
    .bind(events_created)
    .bind(metadata.vrows)
    .bind(run_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn record_failure(
    state: &AppState,
    run_id: &str,
    watch_target: &WatchTarget,
    error: &ThreadCollectorError,
) {
    let (kind, status) = match error {
        ThreadCollectorError::Nga(NgaRequestError::NotFound) => ("not_found", "not_found"),
        ThreadCollectorError::Nga(NgaRequestError::Unauthorized)
        | ThreadCollectorError::Credentials => ("unauthorized", "paused"),
        ThreadCollectorError::Nga(NgaRequestError::Http(_)) => ("nga_http_error", "error"),
        ThreadCollectorError::Nga(NgaRequestError::Request(_)) => ("nga_request_error", "error"),
        ThreadCollectorError::Nga(NgaRequestError::Decode(_)) => ("nga_decode_error", "error"),
        ThreadCollectorError::Nga(NgaRequestError::Business { .. }) => {
            ("nga_business_error", "error")
        }
        ThreadCollectorError::Nga(NgaRequestError::Busy) => ("nga_busy", "error"),
        ThreadCollectorError::Parse(_) => ("nga_parse_error", "error"),
        ThreadCollectorError::Database(_) => ("database_error", "error"),
        ThreadCollectorError::InvalidWatch => ("invalid_watch", "error"),
    };
    let safe_message = kind;

    let next_run = match state.config.database_backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + ($5 * INTERVAL '1 second')",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+' || $5 || ' seconds')",
    };
    let delay = schedule::next_delay_seconds(
        watch_target.schedule.as_ref(),
        watch_target.interval_seconds,
        OffsetDateTime::now_utc(),
        state.config.scheduler.timezone_offset,
    );
    let watch_query = format!(
        "UPDATE watch_targets SET status = $1,
         enabled = CASE WHEN $1 IN ('paused', 'not_found') THEN 0 ELSE enabled END,
         lease_until = NULL,
         next_run_at = {next_run}, last_error_kind = $2, last_error_message = $3,
         updated_at = CURRENT_TIMESTAMP WHERE id = $4"
    );
    if let Err(db_error) = sqlx::query(&watch_query)
        .bind(status)
        .bind(kind)
        .bind(safe_message)
        .bind(&watch_target.id)
        .bind(delay)
        .execute(&state.pool)
        .await
    {
        warn!(watch_id = %watch_target.id, error = %db_error, "failed to record watch failure");
    }
    if let Err(db_error) = sqlx::query(
        "UPDATE crawl_runs SET status = 'failed', error_kind = $1,
         error_message = $2, completed_at = CURRENT_TIMESTAMP WHERE id = $3",
    )
    .bind(kind)
    .bind(safe_message)
    .bind(run_id)
    .execute(&state.pool)
    .await
    {
        warn!(crawl_run_id = run_id, error = %db_error, "failed to record crawl failure");
    }
    if kind == "unauthorized"
        && let Err(db_error) = sqlx::query(
            "UPDATE nga_accounts SET status = 'paused', last_auth_error_kind = 'unauthorized',
             last_auth_checked_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE label = 'default'",
        )
        .execute(&state.pool)
        .await
    {
        warn!(error = %db_error, "failed to pause rejected NGA account");
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, net::SocketAddr, sync::Arc};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use serde_json::{Value, json};
    use sqlx::{Row, any::AnyPoolOptions};
    use tokio::sync::RwLock;

    use super::{create_crawl_run, persist_pages, raw_payload_for_storage, select_posts};
    use crate::{
        app::AppState,
        config::{
            AppConfig, DatabaseBackend, ObservabilityConfig, PersistenceConfig, SchedulerConfig,
        },
        crypto::CredentialCipher,
        domain::thread::PostKind,
        nga::{NgaClient, thread_parser::parse_thread_page},
        repository::watch,
    };

    #[test]
    fn incremental_selection_only_keeps_new_parent_and_its_comments() {
        let page = parse_thread_page(&fixture("thread_comments_hot_post.json"), 1001)
            .expect("fixture must parse");

        let selected = select_posts(&[page], 0, false);
        assert_eq!(selected.len(), 3);
        assert_eq!(
            selected
                .iter()
                .filter(|post| post.kind == PostKind::Reply)
                .count(),
            1
        );
        assert_eq!(
            selected
                .iter()
                .filter(|post| post.kind == PostKind::Comment)
                .count(),
            2
        );
    }

    #[test]
    fn incremental_selection_does_not_revisit_existing_parent_comments() {
        let page = parse_thread_page(&fixture("thread_comments_hot_post.json"), 1001)
            .expect("fixture must parse");
        assert!(select_posts(&[page], 1, false).is_empty());
    }

    #[test]
    fn raw_payload_storage_is_controlled_by_configuration() {
        let page = parse_thread_page(&fixture("thread_attachments.json"), 1004)
            .expect("fixture must parse");
        let post = &page.posts[0];
        assert_eq!(raw_payload_for_storage(post, false), "");
        assert!(raw_payload_for_storage(post, true).contains("asset-1.jpg"));
    }

    #[tokio::test]
    async fn baseline_then_increment_is_append_only_and_deduplicated() {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database must connect");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("foreign keys must enable");
        sqlx::migrate!("./migrations/sqlite")
            .run(&pool)
            .await
            .expect("migrations must run");

        let key = STANDARD.encode([9_u8; 32]);
        let cipher = Arc::new(CredentialCipher::from_base64(&key).expect("cipher must build"));
        let config = Arc::new(AppConfig {
            bind_addr: "127.0.0.1:0"
                .parse::<SocketAddr>()
                .expect("address must parse"),
            database_backend: DatabaseBackend::Sqlite,
            database_url: SecretString::from("postgres://unused"),
            sqlite_path: ":memory:".into(),
            database_max_connections: 1,
            api_token: SecretString::from("test-token"),
            admin_password: SecretString::from("test-password"),
            credential_encryption_key: SecretString::from(key),
            nga_user_agent: "test-agent".to_owned(),
            run_migrations: false,
            persistence: PersistenceConfig {
                store_raw_payload: false,
            },
            assets: crate::config::AssetsConfig {
                download_enabled: false,
                storage_path: "./data/test-assets".into(),
                max_download_bytes: 10 * 1024 * 1024,
            },
            scheduler: SchedulerConfig {
                default_interval_seconds: 60,
                timezone_offset: time::UtcOffset::UTC,
            },
            observability: ObservabilityConfig {
                log_filter: "info".to_owned(),
                log_json: false,
            },
        });
        let state = AppState {
            pool: pool.clone(),
            config,
            credential_cipher: cipher,
            nga_client: NgaClient::new("test-agent".to_owned()).expect("test client must build"),
            admin_sessions: Arc::new(RwLock::new(HashSet::new())),
        };

        let created = watch::create_thread_watch(&pool, 1001, 60)
            .await
            .expect("watch must create");
        let cursor = watch::thread_cursor(&pool, &created.id)
            .await
            .expect("cursor must load");
        create_crawl_run(&state, "baseline-run", &created.id, true)
            .await
            .expect("crawl run must create");
        let baseline_page =
            parse_thread_page(&fixture("thread_page_success.json"), 1001).expect("page must parse");
        let baseline = persist_pages(
            &state,
            "baseline-run",
            &created,
            &cursor,
            true,
            vec![baseline_page],
        )
        .await
        .expect("baseline must succeed");
        assert!(baseline.baseline);
        assert_eq!(baseline.posts_inserted, 2);
        assert_eq!(baseline.events_created, 0);

        let mut live = fixture("thread_page_success.json");
        live["vrows"] = json!(3);
        live["result"][1]["content"] = json!("edited content must not overwrite");
        live["result"]
            .as_array_mut()
            .expect("result must be an array")
            .push(json!({
                "tid": 1001,
                "pid": 4002,
                "fid": 3001,
                "lou": 2,
                "postdatetimestamp": 1767225720_i64,
                "subject": "",
                "content": "new reply",
                "type": 0,
                "author": {"uid": 2003, "username": "new author"},
                "attches": null
            }));
        let live_page = parse_thread_page(&live, 1001).expect("live page must parse");
        let current_watch = watch::find(&pool, &created.id)
            .await
            .expect("watch query must succeed")
            .expect("watch must exist");
        let cursor = watch::thread_cursor(&pool, &created.id)
            .await
            .expect("cursor must load");
        create_crawl_run(&state, "increment-run", &created.id, false)
            .await
            .expect("crawl run must create");
        let increment = persist_pages(
            &state,
            "increment-run",
            &current_watch,
            &cursor,
            false,
            vec![live_page.clone()],
        )
        .await
        .expect("increment must succeed");
        assert!(!increment.baseline);
        assert_eq!(increment.posts_inserted, 1);
        assert_eq!(increment.events_created, 1);
        assert_eq!(increment.last_floor, 2);

        let original: String =
            sqlx::query("SELECT content_raw FROM posts WHERE tid = 1001 AND pid = 4001")
                .fetch_one(&pool)
                .await
                .expect("original reply must exist")
                .get("content_raw");
        assert_eq!(original, "[redacted reply content]");

        let current_watch = watch::find(&pool, &created.id)
            .await
            .expect("watch query must succeed")
            .expect("watch must exist");
        let cursor = watch::thread_cursor(&pool, &created.id)
            .await
            .expect("cursor must load");
        create_crawl_run(&state, "repeat-run", &created.id, false)
            .await
            .expect("crawl run must create");
        let repeat = persist_pages(
            &state,
            "repeat-run",
            &current_watch,
            &cursor,
            false,
            vec![live_page],
        )
        .await
        .expect("repeat must succeed");
        assert_eq!(repeat.posts_inserted, 0);
        assert_eq!(repeat.events_created, 0);

        let post_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts")
            .fetch_one(&pool)
            .await
            .expect("posts must count");
        let event_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM post_events")
            .fetch_one(&pool)
            .await
            .expect("events must count");
        assert_eq!(post_count, 3);
        assert_eq!(event_count, 1);
        let payload_bytes: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(LENGTH(raw_payload)), 0) FROM posts")
                .fetch_one(&pool)
                .await
                .expect("payload bytes must count");
        assert_eq!(payload_bytes, 0);
    }

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nga")
            .join(name);
        serde_json::from_slice(&std::fs::read(path).expect("fixture must be readable"))
            .expect("fixture must be JSON")
    }
}
