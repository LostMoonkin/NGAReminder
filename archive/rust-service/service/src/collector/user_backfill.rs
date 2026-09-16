use std::collections::HashSet;

use secrecy::ExposeSecret;
use thiserror::Error;
use tracing::{info, warn};

use crate::{
    app::AppState,
    collector::thread,
    config::DatabaseBackend,
    domain::{
        thread::{ParsedPost, PostKind, ThreadMetadata},
        user::UserReplyCandidate,
    },
    nga::{NgaRequestError, thread_parser, user_parser},
    repository::user_backfill::{self, UserReplyBackfillJob},
};

#[derive(Debug, Error)]
enum BackfillRunError {
    #[error("NGA account is not configured or cannot be decrypted")]
    Credentials,
    #[error("a full NGA Cookie is required for cross-user monitoring")]
    FullCookieRequired,
    #[error("UID reply list is not ordered newest first")]
    InvalidOrder,
    #[error("NGA detail did not contain the watched user's reply")]
    InvalidDetail,
    #[error("backfill lease was lost")]
    LeaseLost,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Nga(#[from] NgaRequestError),
    #[error(transparent)]
    ThreadParse(#[from] thread_parser::ThreadParseError),
    #[error(transparent)]
    UserParse(#[from] user_parser::UserParseError),
}

impl BackfillRunError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Credentials => "nga_account_not_configured",
            Self::FullCookieRequired => "nga_full_cookie_required",
            Self::InvalidOrder => "nga_reply_order_invalid",
            Self::InvalidDetail => "nga_detail_invalid",
            Self::LeaseLost => "lease_lost",
            Self::Database(_) => "database_error",
            Self::Nga(NgaRequestError::Unauthorized) => "nga_credentials_invalid",
            Self::Nga(NgaRequestError::Busy) => "nga_busy",
            Self::Nga(NgaRequestError::UserSearchUnavailable) => "nga_user_search_unavailable",
            Self::Nga(_) => "nga_request_failed",
            Self::ThreadParse(_) | Self::UserParse(_) => "nga_parse_failed",
        }
    }
}

#[derive(Clone)]
struct BackfillDetail {
    metadata: ThreadMetadata,
    posts: Vec<ParsedPost>,
}

/// Process at most one persistent UID reply history job.
///
/// Acquisition or parsing failures are terminal for this job and are stored
/// on the job row. Database failures are returned only when even that durable
/// bookkeeping cannot be completed.
pub async fn process_one(state: &AppState) -> Result<bool, sqlx::Error> {
    let Some(job) = user_backfill::claim(&state.pool, state.config.database_backend).await? else {
        return Ok(false);
    };
    let token = job
        .lease_token
        .as_deref()
        .expect("a claimed backfill must have a lease token")
        .to_owned();
    match process_page(state, &job, &token).await {
        Ok(true) => {
            if user_backfill::finish(&state.pool, &job.id, &token).await? {
                info!(job_id = %job.id, uid = job.uid, "UID reply history backfill completed");
            } else {
                warn!(job_id = %job.id, uid = job.uid, "UID reply history backfill lost its lease before completion");
            }
        }
        Ok(false) => {
            if !user_backfill::release(&state.pool, &job.id, &token).await? {
                warn!(job_id = %job.id, uid = job.uid, "UID reply history backfill lost its lease after page completion");
            }
        }
        Err(error) => {
            let kind = error.kind();
            let message = error.to_string();
            if user_backfill::fail(&state.pool, &job.id, &token, kind, &message).await? {
                warn!(job_id = %job.id, uid = job.uid, error_kind = kind, error = %error, "UID reply history backfill failed");
            }
        }
    }
    Ok(true)
}

async fn process_page(
    state: &AppState,
    job: &UserReplyBackfillJob,
    lease_token: &str,
) -> Result<bool, BackfillRunError> {
    let (passport_uid, passport_cid, full_cookie_configured) = thread::load_credentials(state)
        .await
        .map_err(|_| BackfillRunError::Credentials)?;
    if passport_uid.expose_secret().parse::<i64>().ok() != Some(job.uid) && !full_cookie_configured
    {
        return Err(BackfillRunError::FullCookieRequired);
    }

    let page_number = job.next_page;
    renew(state, job, lease_token).await?;
    let value = state
        .nga_client
        .fetch_user_replies(
            passport_uid.expose_secret(),
            passport_cid.expose_secret(),
            job.uid,
            page_number,
        )
        .await?;
    let Some(value) = value else {
        persist_page(
            &state.pool,
            state.config.database_backend,
            job,
            lease_token,
            page_number.saturating_add(1),
            1,
            0,
            &[],
            state.config.persistence.store_raw_payload,
            state.config.assets.download_enabled,
        )
        .await?;
        return Ok(true);
    };
    let page = user_parser::parse_reply_list(&value, job.uid, page_number)?;
    let selection = select_page(&page.candidates, job.start_at_unix, job.end_at_unix)?;
    let mut details = Vec::with_capacity(selection.candidates.len());
    for candidate in &selection.candidates {
        renew(state, job, lease_token).await?;
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
        let Some(reply) = page.posts.iter().find(|post| {
            post.kind != PostKind::Comment
                && post.pid == Some(candidate.pid)
                && post.author_uid == job.uid
        }) else {
            return Err(BackfillRunError::InvalidDetail);
        };
        let mut posts = vec![reply.clone()];
        posts.extend(page.posts.into_iter().filter(|post| {
            post.kind == PostKind::Comment && post.parent_pid == Some(candidate.pid)
        }));
        details.push(BackfillDetail {
            metadata: page.metadata,
            posts,
        });
    }

    let request_count =
        1_i32.saturating_add(i32::try_from(selection.candidates.len()).unwrap_or(i32::MAX));
    persist_page(
        &state.pool,
        state.config.database_backend,
        job,
        lease_token,
        page_number.saturating_add(1),
        request_count,
        i32::try_from(selection.candidates.len()).unwrap_or(i32::MAX),
        &details,
        state.config.persistence.store_raw_payload,
        state.config.assets.download_enabled,
    )
    .await?;

    Ok(selection.reached_start || page_number >= page.total_pages)
}

async fn renew(
    state: &AppState,
    job: &UserReplyBackfillJob,
    lease_token: &str,
) -> Result<(), BackfillRunError> {
    if user_backfill::renew(
        &state.pool,
        state.config.database_backend,
        &job.id,
        lease_token,
    )
    .await?
    {
        Ok(())
    } else {
        Err(BackfillRunError::LeaseLost)
    }
}

struct PageSelection {
    candidates: Vec<UserReplyCandidate>,
    reached_start: bool,
}

fn select_page(
    candidates: &[UserReplyCandidate],
    start_at_unix: i64,
    end_at_unix: i64,
) -> Result<PageSelection, BackfillRunError> {
    if candidates
        .windows(2)
        .any(|items| (items[0].postdate, items[0].pid) < (items[1].postdate, items[1].pid))
    {
        return Err(BackfillRunError::InvalidOrder);
    }
    let reached_start = candidates
        .iter()
        .any(|candidate| candidate.postdate < start_at_unix);
    let mut seen = HashSet::new();
    let candidates = candidates
        .iter()
        .filter(|candidate| {
            (start_at_unix..=end_at_unix).contains(&candidate.postdate)
                && seen.insert((candidate.tid, candidate.pid))
        })
        .cloned()
        .collect();
    Ok(PageSelection {
        candidates,
        reached_start,
    })
}

#[allow(clippy::too_many_arguments)]
async fn persist_page(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    job: &UserReplyBackfillJob,
    lease_token: &str,
    next_page: i32,
    pages_requested: i32,
    candidates_processed: i32,
    details: &[BackfillDetail],
    store_raw_payload: bool,
    download_assets: bool,
) -> Result<(), BackfillRunError> {
    let mut tx = pool.begin().await?;
    let mut posts_inserted = 0_i32;
    for detail in details {
        thread::upsert_thread_partial(&mut tx, &detail.metadata).await?;
        let parent = detail
            .posts
            .iter()
            .find(|post| post.kind != PostKind::Comment)
            .ok_or(BackfillRunError::InvalidDetail)?;
        let (parent_id, inserted) =
            thread::insert_post(&mut tx, parent, None, store_raw_payload, download_assets).await?;
        posts_inserted = posts_inserted.saturating_add(i32::from(inserted));
        for comment in detail
            .posts
            .iter()
            .filter(|post| post.kind == PostKind::Comment)
        {
            let (_, inserted) = thread::insert_post(
                &mut tx,
                comment,
                Some(&parent_id),
                store_raw_payload,
                download_assets,
            )
            .await?;
            posts_inserted = posts_inserted.saturating_add(i32::from(inserted));
        }
    }
    if !user_backfill::advance_page(
        &mut tx,
        backend,
        &job.id,
        lease_token,
        next_page,
        pages_requested,
        candidates_processed,
        posts_inserted,
    )
    .await?
    {
        return Err(BackfillRunError::LeaseLost);
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::{Row, any::AnyPoolOptions};

    use super::{BackfillDetail, persist_page, select_page};
    use crate::{
        config::DatabaseBackend,
        domain::{
            thread::{ParsedPost, PostKind, ThreadMetadata},
            user::UserReplyCandidate,
        },
        repository::{user_backfill, watch},
    };

    async fn test_pool() -> sqlx::AnyPool {
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
        pool
    }

    #[test]
    fn date_window_is_inclusive_and_stops_after_older_reply() {
        let selected = select_page(
            &[
                UserReplyCandidate {
                    tid: 10,
                    pid: 4,
                    postdate: 201,
                },
                UserReplyCandidate {
                    tid: 10,
                    pid: 3,
                    postdate: 200,
                },
                UserReplyCandidate {
                    tid: 10,
                    pid: 2,
                    postdate: 100,
                },
                UserReplyCandidate {
                    tid: 10,
                    pid: 1,
                    postdate: 99,
                },
            ],
            100,
            200,
        )
        .expect("ordered candidates must select");
        assert_eq!(selected.candidates.len(), 2);
        assert_eq!(selected.candidates[0].pid, 3);
        assert_eq!(selected.candidates[1].pid, 2);
        assert!(selected.reached_start);
    }

    #[test]
    fn unordered_reply_page_is_rejected_before_early_stop() {
        let result = select_page(
            &[
                UserReplyCandidate {
                    tid: 10,
                    pid: 1,
                    postdate: 100,
                },
                UserReplyCandidate {
                    tid: 10,
                    pid: 2,
                    postdate: 101,
                },
            ],
            1,
            200,
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn page_persistence_is_silent_idempotent_and_does_not_move_uid_cursor() {
        let pool = test_pool().await;
        let watch = watch::create_user_watch(&pool, 2001, 60)
            .await
            .expect("watch must create");
        sqlx::query("UPDATE watch_targets SET baseline_completed = 1 WHERE id = $1")
            .bind(&watch.id)
            .execute(&pool)
            .await
            .expect("baseline must update");
        sqlx::query(
            "UPDATE user_watch_cursors
             SET newest_topic_at_unix = 11, newest_topic_tid = 12,
                 newest_reply_at_unix = 13, newest_reply_pid = 14
             WHERE watch_id = $1",
        )
        .bind(&watch.id)
        .execute(&pool)
        .await
        .expect("cursor must update");
        let created = user_backfill::create(&pool, 2001, "1970-01-01", 1, 200)
            .await
            .expect("job must create");
        let job = user_backfill::claim(&pool, DatabaseBackend::Sqlite)
            .await
            .expect("claim must query")
            .expect("job must claim");
        let token = job.lease_token.clone().expect("claim must have token");
        let detail = BackfillDetail {
            metadata: ThreadMetadata {
                tid: 1001,
                fid: 7,
                title: "topic".to_owned(),
                forum_name: "forum".to_owned(),
                author_uid: 9,
                author_name: "author".to_owned(),
                total_pages: 1,
                per_page: 20,
                vrows: 1,
            },
            posts: vec![ParsedPost {
                tid: 1001,
                pid: Some(4002),
                floor_number: 1,
                kind: PostKind::Reply,
                parent_pid: None,
                parent_is_topic: false,
                author_uid: 2001,
                author_name: "watched".to_owned(),
                subject: String::new(),
                content_raw: "reply".to_owned(),
                published_at_unix: Some(100),
                page_number: 1,
                raw_payload: "raw".to_owned(),
                asset_refs: Vec::new(),
            }],
        };
        persist_page(
            &pool,
            DatabaseBackend::Sqlite,
            &job,
            &token,
            2,
            2,
            1,
            std::slice::from_ref(&detail),
            false,
            false,
        )
        .await
        .expect("first page must persist");
        persist_page(
            &pool,
            DatabaseBackend::Sqlite,
            &job,
            &token,
            3,
            2,
            1,
            std::slice::from_ref(&detail),
            false,
            false,
        )
        .await
        .expect("duplicate page must be safe");
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts")
            .fetch_one(&pool)
            .await
            .expect("posts must count");
        assert_eq!(stored, 1);
        for table in [
            "post_events",
            "post_event_watch_matches",
            "notification_outbox",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&pool)
                .await
                .expect("silent tables must count");
            assert_eq!(count, 0, "{table} must stay empty");
        }
        let cursor = sqlx::query(
            "SELECT newest_topic_at_unix, newest_topic_tid,
                    newest_reply_at_unix, newest_reply_pid
             FROM user_watch_cursors WHERE watch_id = $1",
        )
        .bind(&watch.id)
        .fetch_one(&pool)
        .await
        .expect("cursor must query");
        assert_eq!(cursor.get::<i64, _>("newest_topic_at_unix"), 11);
        assert_eq!(cursor.get::<i64, _>("newest_topic_tid"), 12);
        assert_eq!(cursor.get::<i64, _>("newest_reply_at_unix"), 13);
        assert_eq!(cursor.get::<i64, _>("newest_reply_pid"), 14);
        let baseline: i32 =
            sqlx::query_scalar("SELECT baseline_completed FROM watch_targets WHERE id = $1")
                .bind(&watch.id)
                .fetch_one(&pool)
                .await
                .expect("baseline must query");
        assert_eq!(baseline, 1);

        let job = user_backfill::list(&pool)
            .await
            .expect("jobs must list")
            .into_iter()
            .find(|item| item.id == created.id)
            .expect("job must remain visible");
        assert_eq!(job.posts_inserted, 1);
    }
}
