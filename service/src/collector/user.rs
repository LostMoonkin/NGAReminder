use std::collections::HashSet;

use secrecy::ExposeSecret;
use sqlx::{Any, Transaction};
use thiserror::Error;
use time::OffsetDateTime;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::AppState,
    collector::thread,
    config::DatabaseBackend,
    domain::{
        thread::{ParsedPost, PostKind, ThreadMetadata},
        user::{UserProfile, UserReplyCandidate, UserTopicCandidate},
    },
    nga::{NgaRequestError, thread_parser, user_parser},
    repository::watch::{self, UserCursor, WatchTarget},
    schedule,
};

#[derive(Debug, serde::Serialize)]
pub struct UserCrawlSummary {
    pub crawl_run_id: String,
    pub uid: i64,
    pub status: &'static str,
    pub baseline: bool,
    pub pages_requested: i32,
    pub posts_inserted: i32,
    pub events_created: i32,
}

#[derive(Debug, Error)]
pub enum UserCollectorError {
    #[error("watch is not a user watch")]
    InvalidWatch,
    #[error("NGA account is not configured or cannot be decrypted")]
    Credentials,
    #[error("NGA detail did not contain the watched user's post")]
    InvalidDetail,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Nga(#[from] NgaRequestError),
    #[error(transparent)]
    ThreadParse(#[from] thread_parser::ThreadParseError),
    #[error(transparent)]
    UserParse(#[from] user_parser::UserParseError),
}

struct UserDetail {
    metadata: ThreadMetadata,
    posts: Vec<ParsedPost>,
}

struct Discovery {
    profile: UserProfile,
    topics: Vec<UserTopicCandidate>,
    replies: Vec<UserReplyCandidate>,
    details: Vec<UserDetail>,
    pages_requested: i32,
}

pub async fn run(
    state: &AppState,
    watch_target: WatchTarget,
) -> Result<UserCrawlSummary, UserCollectorError> {
    if watch_target.target_type != "user" {
        return Err(UserCollectorError::InvalidWatch);
    }
    let baseline = !watch_target.baseline_completed;
    let run_id = Uuid::new_v4().to_string();
    create_crawl_run(state, &run_id, &watch_target.id, baseline).await?;

    match collect(state, &run_id, &watch_target, baseline).await {
        Err(UserCollectorError::Nga(NgaRequestError::Busy)) => {
            mark_skipped_busy(state, &run_id, &watch_target).await?;
            Ok(UserCrawlSummary {
                crawl_run_id: run_id,
                uid: watch_target.target_id,
                status: "skipped_busy",
                baseline,
                pages_requested: 0,
                posts_inserted: 0,
                events_created: 0,
            })
        }
        Err(UserCollectorError::Nga(NgaRequestError::PendingReview)) => {
            mark_skipped_pending_review(state, &run_id, &watch_target).await?;
            Ok(UserCrawlSummary {
                crawl_run_id: run_id,
                uid: watch_target.target_id,
                status: "skipped_pending_review",
                baseline,
                pages_requested: 0,
                posts_inserted: 0,
                events_created: 0,
            })
        }
        Err(error) => {
            record_failure(state, &run_id, &watch_target, &error).await;
            Err(error)
        }
        Ok(summary) => Ok(summary),
    }
}

async fn collect(
    state: &AppState,
    run_id: &str,
    watch_target: &WatchTarget,
    baseline: bool,
) -> Result<UserCrawlSummary, UserCollectorError> {
    let cursor = watch::user_cursor(&state.pool, &watch_target.id).await?;
    let (passport_uid, passport_cid) = thread::load_credentials(state)
        .await
        .map_err(|_| UserCollectorError::Credentials)?;
    let profile_bytes = state
        .nga_client
        .fetch_user_profile(
            passport_uid.expose_secret(),
            passport_cid.expose_secret(),
            watch_target.target_id,
        )
        .await?;
    let profile = user_parser::parse_profile_gbk(&profile_bytes, watch_target.target_id)?;

    let (topics, topic_pages) = discover_topics(
        state,
        passport_uid.expose_secret(),
        passport_cid.expose_secret(),
        watch_target.target_id,
        &cursor,
        baseline,
    )
    .await?;
    let (replies, reply_pages) = discover_replies(
        state,
        passport_uid.expose_secret(),
        passport_cid.expose_secret(),
        watch_target.target_id,
        &cursor,
        baseline,
    )
    .await?;

    let mut details = Vec::with_capacity(topics.len() + replies.len());
    for candidate in &topics {
        let value = state
            .nga_client
            .fetch_thread_page(
                passport_uid.expose_secret(),
                passport_cid.expose_secret(),
                candidate.tid,
                1,
            )
            .await?;
        let page = thread_parser::parse_thread_page(&value, candidate.tid)?;
        let Some(topic) = page
            .posts
            .iter()
            .find(|post| post.kind == PostKind::Topic && post.author_uid == watch_target.target_id)
            .cloned()
        else {
            return Err(UserCollectorError::InvalidDetail);
        };
        let mut posts = vec![topic];
        posts.extend(
            page.posts
                .into_iter()
                .filter(|post| post.kind == PostKind::Comment && post.parent_is_topic),
        );
        details.push(UserDetail {
            metadata: page.metadata,
            posts,
        });
    }
    for candidate in &replies {
        let value = state
            .nga_client
            .fetch_post_by_pid(
                passport_uid.expose_secret(),
                passport_cid.expose_secret(),
                candidate.tid,
                candidate.pid,
            )
            .await?;
        let page = thread_parser::parse_thread_page(&value, candidate.tid)?;
        let Some(reply) = page
            .posts
            .iter()
            .find(|post| {
                post.kind == PostKind::Reply
                    && post.pid == Some(candidate.pid)
                    && post.author_uid == watch_target.target_id
            })
            .cloned()
        else {
            return Err(UserCollectorError::InvalidDetail);
        };
        let mut posts = vec![reply];
        posts.extend(page.posts.into_iter().filter(|post| {
            post.kind == PostKind::Comment && post.parent_pid == Some(candidate.pid)
        }));
        details.push(UserDetail {
            metadata: page.metadata,
            posts,
        });
    }

    let discovery = Discovery {
        profile,
        topics,
        replies,
        pages_requested: 1
            + topic_pages
            + reply_pages
            + i32::try_from(details.len()).unwrap_or(i32::MAX),
        details,
    };
    persist(state, run_id, watch_target, &cursor, baseline, discovery).await
}

async fn discover_topics(
    state: &AppState,
    passport_uid: &str,
    passport_cid: &str,
    uid: i64,
    cursor: &UserCursor,
    baseline: bool,
) -> Result<(Vec<UserTopicCandidate>, i32), UserCollectorError> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut requested = 0;
    let mut page_number = 1;
    let mut total_pages = 1;
    let boundary = (cursor.newest_topic_at_unix, cursor.newest_topic_tid);
    while page_number <= total_pages {
        let value = state
            .nga_client
            .fetch_user_topics(passport_uid, passport_cid, uid, page_number)
            .await?;
        requested += 1;
        let page = user_parser::parse_topic_list(&value, uid)?;
        total_pages = page.total_pages;
        let reached_boundary = page
            .candidates
            .iter()
            .any(|item| (item.postdate, item.tid) <= boundary);
        for item in page.candidates {
            if (baseline || (item.postdate, item.tid) > boundary) && seen.insert(item.tid) {
                result.push(item);
            }
        }
        if !baseline && reached_boundary {
            break;
        }
        page_number += 1;
    }
    Ok((result, requested))
}

async fn discover_replies(
    state: &AppState,
    passport_uid: &str,
    passport_cid: &str,
    uid: i64,
    cursor: &UserCursor,
    baseline: bool,
) -> Result<(Vec<UserReplyCandidate>, i32), UserCollectorError> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut requested = 0;
    let mut page_number = 1;
    let mut total_pages = 1;
    let boundary = (cursor.newest_reply_at_unix, cursor.newest_reply_pid);
    while page_number <= total_pages {
        let value = state
            .nga_client
            .fetch_user_replies(passport_uid, passport_cid, uid, page_number)
            .await?;
        requested += 1;
        let page = user_parser::parse_reply_list(&value, uid)?;
        total_pages = page.total_pages;
        let reached_boundary = page
            .candidates
            .iter()
            .any(|item| (item.postdate, item.pid) <= boundary);
        for item in page.candidates {
            if (baseline || (item.postdate, item.pid) > boundary)
                && seen.insert((item.tid, item.pid))
            {
                result.push(item);
            }
        }
        if !baseline && reached_boundary {
            break;
        }
        page_number += 1;
    }
    Ok((result, requested))
}

async fn persist(
    state: &AppState,
    run_id: &str,
    watch_target: &WatchTarget,
    cursor: &UserCursor,
    baseline: bool,
    discovery: Discovery,
) -> Result<UserCrawlSummary, UserCollectorError> {
    let mut tx = state.pool.begin().await?;
    upsert_user(&mut tx, &discovery.profile).await?;
    let mut posts_inserted = 0;
    let mut events_created = 0;

    for detail in &discovery.details {
        thread::upsert_thread_partial(&mut tx, &detail.metadata).await?;
        let parent = detail
            .posts
            .iter()
            .find(|post| post.kind != PostKind::Comment)
            .ok_or(UserCollectorError::InvalidDetail)?;
        let (parent_id, inserted) = thread::insert_post(
            &mut tx,
            parent,
            None,
            state.config.persistence.store_raw_payload,
            state.config.assets.download_enabled,
        )
        .await?;
        if inserted {
            posts_inserted += 1;
            if !baseline
                && thread::insert_event(&mut tx, &parent_id, parent, &watch_target.id).await?
            {
                events_created += 1;
            }
        }
        for comment in detail
            .posts
            .iter()
            .filter(|post| post.kind == PostKind::Comment)
        {
            let (id, inserted) = thread::insert_post(
                &mut tx,
                comment,
                Some(&parent_id),
                state.config.persistence.store_raw_payload,
                state.config.assets.download_enabled,
            )
            .await?;
            if inserted {
                posts_inserted += 1;
                if !baseline
                    && thread::insert_event(&mut tx, &id, comment, &watch_target.id).await?
                {
                    events_created += 1;
                }
            }
        }
    }

    let topic_key = discovery
        .topics
        .iter()
        .map(|item| (item.postdate, item.tid))
        .max()
        .unwrap_or((cursor.newest_topic_at_unix, cursor.newest_topic_tid))
        .max((cursor.newest_topic_at_unix, cursor.newest_topic_tid));
    let reply_key = discovery
        .replies
        .iter()
        .map(|item| (item.postdate, item.pid))
        .max()
        .unwrap_or((cursor.newest_reply_at_unix, cursor.newest_reply_pid))
        .max((cursor.newest_reply_at_unix, cursor.newest_reply_pid));
    finish_success(
        &mut tx,
        state.config.database_backend,
        run_id,
        watch_target,
        topic_key,
        reply_key,
        discovery.pages_requested,
        posts_inserted,
        events_created,
        state.config.scheduler.timezone_offset,
    )
    .await?;
    tx.commit().await?;

    info!(
        crawl_run_id = run_id,
        watch_id = watch_target.id,
        uid = watch_target.target_id,
        baseline,
        posts_inserted,
        events_created,
        "user crawl completed"
    );
    Ok(UserCrawlSummary {
        crawl_run_id: run_id.to_owned(),
        uid: watch_target.target_id,
        status: "succeeded",
        baseline,
        pages_requested: discovery.pages_requested,
        posts_inserted,
        events_created,
    })
}

async fn upsert_user(
    tx: &mut Transaction<'_, Any>,
    profile: &UserProfile,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO nga_users
            (uid, username, group_id, avatar, registered_at_unix, last_post_at_unix,
             remote_post_count, signature, raw_payload)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (uid) DO UPDATE SET
            username = EXCLUDED.username, group_id = EXCLUDED.group_id,
            avatar = EXCLUDED.avatar, registered_at_unix = EXCLUDED.registered_at_unix,
            last_post_at_unix = EXCLUDED.last_post_at_unix,
            remote_post_count = EXCLUDED.remote_post_count,
            signature = EXCLUDED.signature, raw_payload = EXCLUDED.raw_payload,
            last_seen_at = CURRENT_TIMESTAMP",
    )
    .bind(profile.uid)
    .bind(&profile.username)
    .bind(profile.group_id)
    .bind(&profile.avatar)
    .bind(profile.registered_at_unix)
    .bind(profile.last_post_at_unix)
    .bind(profile.remote_post_count)
    .bind(&profile.signature)
    .bind(&profile.raw_payload)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn finish_success(
    tx: &mut Transaction<'_, Any>,
    backend: DatabaseBackend,
    run_id: &str,
    watch_target: &WatchTarget,
    topic_key: (i64, i64),
    reply_key: (i64, i64),
    pages_requested: i32,
    posts_inserted: i32,
    events_created: i32,
    timezone_offset: time::UtcOffset,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE user_watch_cursors SET newest_topic_at_unix = $1,
         newest_topic_tid = $2, newest_reply_at_unix = $3, newest_reply_pid = $4,
         updated_at = CURRENT_TIMESTAMP WHERE watch_id = $5",
    )
    .bind(topic_key.0)
    .bind(topic_key.1)
    .bind(reply_key.0)
    .bind(reply_key.1)
    .bind(&watch_target.id)
    .execute(&mut **tx)
    .await?;
    finish_watch(
        tx,
        backend,
        watch_target,
        "active",
        true,
        run_id,
        "succeeded",
        pages_requested,
        posts_inserted,
        events_created,
        timezone_offset,
    )
    .await
}

async fn create_crawl_run(
    state: &AppState,
    run_id: &str,
    watch_id: &str,
    baseline: bool,
) -> Result<(), UserCollectorError> {
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

async fn mark_skipped_busy(
    state: &AppState,
    run_id: &str,
    watch_target: &WatchTarget,
) -> Result<(), UserCollectorError> {
    let mut tx = state.pool.begin().await?;
    finish_watch(
        &mut tx,
        state.config.database_backend,
        watch_target,
        "active",
        false,
        run_id,
        "skipped_busy",
        0,
        0,
        0,
        state.config.scheduler.timezone_offset,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn mark_skipped_pending_review(
    state: &AppState,
    run_id: &str,
    watch_target: &WatchTarget,
) -> Result<(), UserCollectorError> {
    let mut tx = state.pool.begin().await?;
    let next_run = match state.config.database_backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + ($2 * INTERVAL '1 second')",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+' || $2 || ' seconds')",
    };
    let delay = schedule::next_delay_seconds(
        watch_target.schedule.as_ref(),
        watch_target.interval_seconds,
        OffsetDateTime::now_utc(),
        state.config.scheduler.timezone_offset,
    );
    let watch_query = format!(
        "UPDATE watch_targets SET status = 'active',
         next_run_at = {next_run}, lease_until = NULL,
         last_completed_at = CURRENT_TIMESTAMP, last_error_kind = 'nga_pending_review',
         last_error_message = 'nga_pending_review', updated_at = CURRENT_TIMESTAMP WHERE id = $1"
    );
    sqlx::query(&watch_query)
        .bind(&watch_target.id)
        .bind(delay)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE crawl_runs SET status = 'skipped', error_kind = 'nga_pending_review',
         error_message = 'nga_pending_review', completed_at = CURRENT_TIMESTAMP WHERE id = $1",
    )
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn finish_watch(
    tx: &mut Transaction<'_, Any>,
    backend: DatabaseBackend,
    watch_target: &WatchTarget,
    watch_status: &str,
    baseline_completed: bool,
    run_id: &str,
    run_status: &str,
    pages_requested: i32,
    posts_inserted: i32,
    events_created: i32,
    timezone_offset: time::UtcOffset,
) -> Result<(), sqlx::Error> {
    let next_run = match backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + ($4 * INTERVAL '1 second')",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+' || $4 || ' seconds')",
    };
    let delay = schedule::next_delay_seconds(
        watch_target.schedule.as_ref(),
        watch_target.interval_seconds,
        OffsetDateTime::now_utc(),
        timezone_offset,
    );
    let query = format!(
        "UPDATE watch_targets SET status = $1,
         baseline_completed = CASE WHEN $2 = 1 THEN 1 ELSE baseline_completed END,
         next_run_at = {next_run}, lease_until = NULL,
         last_completed_at = CURRENT_TIMESTAMP, last_error_kind = NULL,
         last_error_message = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = $3"
    );
    sqlx::query(&query)
        .bind(watch_status)
        .bind(i32::from(baseline_completed))
        .bind(&watch_target.id)
        .bind(delay)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "UPDATE crawl_runs SET status = $1, pages_requested = $2,
         posts_inserted = $3, events_created = $4,
         completed_at = CURRENT_TIMESTAMP WHERE id = $5",
    )
    .bind(run_status)
    .bind(pages_requested)
    .bind(posts_inserted)
    .bind(events_created)
    .bind(run_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn record_failure(
    state: &AppState,
    run_id: &str,
    watch_target: &WatchTarget,
    error: &UserCollectorError,
) {
    let (kind, status, disable) = match error {
        UserCollectorError::Credentials
        | UserCollectorError::Nga(NgaRequestError::Unauthorized) => {
            ("unauthorized", "paused", true)
        }
        UserCollectorError::UserParse(user_parser::UserParseError::ProfileNotFound)
        | UserCollectorError::UserParse(user_parser::UserParseError::UidMismatch) => {
            ("user_not_found", "not_found", true)
        }
        UserCollectorError::Nga(NgaRequestError::Http(_)) => ("nga_http_error", "error", false),
        UserCollectorError::Nga(NgaRequestError::Request(_)) => {
            ("nga_request_error", "error", false)
        }
        UserCollectorError::Nga(NgaRequestError::Decode(_))
        | UserCollectorError::ThreadParse(_)
        | UserCollectorError::UserParse(_) => ("nga_parse_error", "error", false),
        UserCollectorError::Nga(NgaRequestError::NotFound) | UserCollectorError::InvalidDetail => {
            ("post_not_found", "error", false)
        }
        UserCollectorError::Nga(NgaRequestError::PendingReview) => {
            ("nga_pending_review", "error", false)
        }
        UserCollectorError::Nga(NgaRequestError::Business { .. })
        | UserCollectorError::Nga(NgaRequestError::Busy) => ("nga_business_error", "error", false),
        UserCollectorError::Database(_) => ("database_error", "error", false),
        UserCollectorError::InvalidWatch => ("invalid_watch", "error", false),
    };
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
    let query = format!(
        "UPDATE watch_targets SET status = $1,
         enabled = CASE WHEN $2 = 1 THEN 0 ELSE enabled END,
         lease_until = NULL, next_run_at = {next_run},
         last_error_kind = $3, last_error_message = $3,
         updated_at = CURRENT_TIMESTAMP WHERE id = $4"
    );
    if let Err(db_error) = sqlx::query(&query)
        .bind(status)
        .bind(i32::from(disable))
        .bind(kind)
        .bind(&watch_target.id)
        .bind(delay)
        .execute(&state.pool)
        .await
    {
        warn!(watch_id = %watch_target.id, error = %db_error, "failed to record user watch failure");
    }
    if let Err(db_error) = sqlx::query(
        "UPDATE crawl_runs SET status = 'failed', error_kind = $1,
         error_message = $1, completed_at = CURRENT_TIMESTAMP WHERE id = $2",
    )
    .bind(kind)
    .bind(run_id)
    .execute(&state.pool)
    .await
    {
        warn!(crawl_run_id = run_id, error = %db_error, "failed to record user crawl failure");
    }
    if kind == "unauthorized" {
        if let Err(db_error) = sqlx::query(
            "UPDATE nga_accounts SET status = 'paused', last_auth_error_kind = 'unauthorized',
             last_auth_checked_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE label = 'default'",
        )
        .execute(&state.pool)
        .await
        {
            warn!(error = %db_error, "failed to pause rejected NGA account");
        }
        if let Err(db_error) =
            crate::notification::alerts::ensure_nga_credentials_invalid_alert(state).await
        {
            warn!(error = %db_error, "failed to enqueue NGA credential alert");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, net::SocketAddr, sync::Arc};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use serde_json::{Value, json};
    use sqlx::any::AnyPoolOptions;
    use tokio::sync::RwLock;

    use super::{Discovery, UserDetail, create_crawl_run, persist};
    use crate::{
        app::AppState,
        config::{
            AppConfig, DatabaseBackend, ObservabilityConfig, PersistenceConfig, SchedulerConfig,
        },
        crypto::CredentialCipher,
        domain::user::{UserReplyCandidate, UserTopicCandidate},
        nga::{NgaClient, thread_parser, user_parser},
        repository::watch,
    };

    #[tokio::test]
    async fn user_baseline_and_increment_share_global_post_deduplication() {
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

        let key = STANDARD.encode([11_u8; 32]);
        let state = AppState {
            pool: pool.clone(),
            config: Arc::new(AppConfig {
                bind_addr: "127.0.0.1:0"
                    .parse::<SocketAddr>()
                    .expect("address must parse"),
                database_backend: DatabaseBackend::Sqlite,
                database_url: SecretString::from("postgres://unused"),
                sqlite_path: ":memory:".into(),
                database_max_connections: 1,
                api_token: SecretString::from("test-token"),
                admin_password: SecretString::from("test-password"),
                credential_encryption_key: SecretString::from(key.clone()),
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
            }),
            credential_cipher: Arc::new(
                CredentialCipher::from_base64(&key).expect("cipher must build"),
            ),
            nga_client: NgaClient::new("test-agent".to_owned()).expect("client must build"),
            admin_sessions: Arc::new(RwLock::new(HashSet::new())),
            feishu_channel_updates: tokio::sync::watch::channel(()).0,
        };
        let watch = watch::create_user_watch(&pool, 2001, 60)
            .await
            .expect("watch must create");
        let profile = user_parser::parse_profile_gbk(&fixture_bytes("user_profile_gbk.html"), 2001)
            .expect("profile must parse");
        let topic_page =
            thread_parser::parse_thread_page(&fixture("thread_page_success.json"), 1001)
                .expect("topic detail must parse");
        let cursor = watch::user_cursor(&pool, &watch.id)
            .await
            .expect("cursor must load");
        create_crawl_run(&state, "user-baseline", &watch.id, true)
            .await
            .expect("run must create");
        let baseline = persist(
            &state,
            "user-baseline",
            &watch,
            &cursor,
            true,
            Discovery {
                profile: profile.clone(),
                topics: vec![UserTopicCandidate {
                    tid: 1001,
                    postdate: 1767225600,
                }],
                replies: vec![],
                details: vec![UserDetail {
                    metadata: topic_page.metadata,
                    posts: vec![topic_page.posts[0].clone()],
                }],
                pages_requested: 3,
            },
        )
        .await
        .expect("baseline must persist");
        assert_eq!(baseline.posts_inserted, 1);
        assert_eq!(baseline.events_created, 0);

        let mut reply_json = fixture("post_by_pid_success.json");
        reply_json["result"][0]["tid"] = json!(1003);
        reply_json["result"][0]["pid"] = json!(4002);
        reply_json["result"][0]["author"]["uid"] = json!(2001);
        let reply_page =
            thread_parser::parse_thread_page(&reply_json, 1003).expect("reply detail must parse");
        let current_watch = watch::find(&pool, &watch.id)
            .await
            .expect("watch query must succeed")
            .expect("watch must exist");
        let cursor = watch::user_cursor(&pool, &watch.id)
            .await
            .expect("cursor must load");
        create_crawl_run(&state, "user-increment", &watch.id, false)
            .await
            .expect("run must create");
        let discovery = Discovery {
            profile,
            topics: vec![],
            replies: vec![UserReplyCandidate {
                tid: 1003,
                pid: 4002,
                postdate: 1767225720,
            }],
            details: vec![UserDetail {
                metadata: reply_page.metadata.clone(),
                posts: vec![reply_page.posts[0].clone()],
            }],
            pages_requested: 4,
        };
        let increment = persist(
            &state,
            "user-increment",
            &current_watch,
            &cursor,
            false,
            discovery,
        )
        .await
        .expect("increment must persist");
        assert_eq!(increment.posts_inserted, 1);
        assert_eq!(increment.events_created, 1);

        let current_watch = watch::find(&pool, &watch.id)
            .await
            .expect("watch query must succeed")
            .expect("watch must exist");
        let cursor = watch::user_cursor(&pool, &watch.id)
            .await
            .expect("cursor must load");
        create_crawl_run(&state, "user-repeat", &watch.id, false)
            .await
            .expect("run must create");
        let repeat = persist(
            &state,
            "user-repeat",
            &current_watch,
            &cursor,
            false,
            Discovery {
                profile: user_parser::parse_profile_gbk(
                    &fixture_bytes("user_profile_gbk.html"),
                    2001,
                )
                .expect("profile must parse"),
                topics: vec![],
                replies: vec![UserReplyCandidate {
                    tid: 1003,
                    pid: 4002,
                    postdate: 1767225720,
                }],
                details: vec![UserDetail {
                    metadata: reply_page.metadata,
                    posts: vec![reply_page.posts[0].clone()],
                }],
                pages_requested: 4,
            },
        )
        .await
        .expect("repeat must persist");
        assert_eq!(repeat.posts_inserted, 0);
        assert_eq!(repeat.events_created, 0);

        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM post_events")
            .fetch_one(&pool)
            .await
            .expect("events must count");
        let partial_threads: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM threads WHERE coverage = 'partial'")
                .fetch_one(&pool)
                .await
                .expect("threads must count");
        assert_eq!(events, 1);
        assert_eq!(partial_threads, 2);
    }

    fn fixture(name: &str) -> Value {
        serde_json::from_slice(&fixture_bytes(name)).expect("fixture must be JSON")
    }

    fn fixture_bytes(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nga")
            .join(name);
        std::fs::read(path).expect("fixture must be readable")
    }
}
