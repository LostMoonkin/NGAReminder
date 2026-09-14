use sqlx::{Any, AnyPool, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::config::DatabaseBackend;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserReplyBackfillJob {
    pub id: String,
    pub watch_id: String,
    pub uid: i64,
    pub start_date: String,
    pub start_at_unix: i64,
    pub end_at_unix: i64,
    pub status: String,
    pub next_page: i32,
    pub pages_requested: i32,
    pub candidates_processed: i32,
    pub posts_inserted: i32,
    pub lease_token: Option<String>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Error)]
pub enum CreateBackfillError {
    #[error("UID watch does not exist")]
    WatchNotFound,
    #[error("another UID reply backfill is active")]
    Conflict,
    #[error("database error")]
    Database(#[source] sqlx::Error),
}

pub async fn create(
    pool: &AnyPool,
    uid: i64,
    start_date: &str,
    start_at_unix: i64,
    end_at_unix: i64,
) -> Result<UserReplyBackfillJob, CreateBackfillError> {
    let mut tx = pool.begin().await.map_err(CreateBackfillError::Database)?;
    let watch_id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM watch_targets
         WHERE target_type = 'user' AND target_id = $1 AND deleted_at IS NULL",
    )
    .bind(uid)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CreateBackfillError::Database)?
    .ok_or(CreateBackfillError::WatchNotFound)?;
    let id = Uuid::new_v4().to_string();
    let result = sqlx::query(
        "INSERT INTO uid_reply_backfill_jobs
            (id, watch_id, uid, start_date, start_at_unix, end_at_unix)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&id)
    .bind(&watch_id)
    .bind(uid)
    .bind(start_date)
    .bind(start_at_unix)
    .bind(end_at_unix)
    .execute(&mut *tx)
    .await;
    match result {
        Ok(_) => {}
        Err(error) if is_unique_violation(&error) => return Err(CreateBackfillError::Conflict),
        Err(error) => return Err(CreateBackfillError::Database(error)),
    }
    tx.commit().await.map_err(CreateBackfillError::Database)?;
    find(pool, &id)
        .await
        .map_err(CreateBackfillError::Database)?
        .ok_or_else(|| CreateBackfillError::Database(sqlx::Error::RowNotFound))
}

pub async fn list(pool: &AnyPool) -> Result<Vec<UserReplyBackfillJob>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, watch_id, uid, start_date, start_at_unix, end_at_unix,
                status, next_page, pages_requested, candidates_processed, posts_inserted,
                lease_token, error_kind, error_message,
                CAST(created_at AS TEXT) AS created_at,
                CAST(started_at AS TEXT) AS started_at,
                CAST(completed_at AS TEXT) AS completed_at
         FROM uid_reply_backfill_jobs
         ORDER BY created_at DESC, id DESC
         LIMIT 50",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(map_job).collect())
}

pub async fn claim(
    pool: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Option<UserReplyBackfillJob>, sqlx::Error> {
    let token = Uuid::new_v4().to_string();
    let lease_until = lease_expression(backend);
    let query = format!(
        "UPDATE uid_reply_backfill_jobs
         SET status = 'running', lease_until = {lease_until}, lease_token = $1,
             started_at = COALESCE(started_at, CURRENT_TIMESTAMP),
             error_kind = NULL, error_message = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = (
             SELECT id FROM uid_reply_backfill_jobs
             WHERE status = 'pending'
                OR (status = 'running' AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP))
             ORDER BY created_at, id LIMIT 1
         )
           AND (status = 'pending'
                OR (status = 'running' AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)))
         RETURNING id"
    );
    let row = sqlx::query(&query)
        .bind(&token)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    find(pool, &row.get::<String, _>("id")).await
}

pub async fn renew(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    lease_token: &str,
) -> Result<bool, sqlx::Error> {
    let query = format!(
        "UPDATE uid_reply_backfill_jobs
         SET lease_until = {}, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND status = 'running' AND lease_token = $2",
        lease_expression(backend)
    );
    Ok(sqlx::query(&query)
        .bind(id)
        .bind(lease_token)
        .execute(pool)
        .await?
        .rows_affected()
        == 1)
}

#[allow(clippy::too_many_arguments)]
pub async fn advance_page(
    tx: &mut Transaction<'_, Any>,
    backend: DatabaseBackend,
    id: &str,
    lease_token: &str,
    next_page: i32,
    pages_requested: i32,
    candidates_processed: i32,
    posts_inserted: i32,
) -> Result<bool, sqlx::Error> {
    let query = format!(
        "UPDATE uid_reply_backfill_jobs
         SET next_page = $1,
             pages_requested = pages_requested + $2,
             candidates_processed = candidates_processed + $3,
             posts_inserted = posts_inserted + $4,
             lease_until = {}, updated_at = CURRENT_TIMESTAMP
         WHERE id = $5 AND status = 'running' AND lease_token = $6",
        lease_expression(backend)
    );
    Ok(sqlx::query(&query)
        .bind(next_page)
        .bind(pages_requested)
        .bind(candidates_processed)
        .bind(posts_inserted)
        .bind(id)
        .bind(lease_token)
        .execute(&mut **tx)
        .await?
        .rows_affected()
        == 1)
}

pub async fn finish(pool: &AnyPool, id: &str, lease_token: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE uid_reply_backfill_jobs
         SET status = 'succeeded', lease_until = NULL, lease_token = NULL,
             completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND status = 'running' AND lease_token = $2",
    )
    .bind(id)
    .bind(lease_token)
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}

pub async fn release(pool: &AnyPool, id: &str, lease_token: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE uid_reply_backfill_jobs
         SET status = 'pending', lease_until = NULL, lease_token = NULL,
             updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND status = 'running' AND lease_token = $2",
    )
    .bind(id)
    .bind(lease_token)
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}

pub async fn fail(
    pool: &AnyPool,
    id: &str,
    lease_token: &str,
    error_kind: &str,
    error_message: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE uid_reply_backfill_jobs
         SET status = 'failed', lease_until = NULL, lease_token = NULL,
             error_kind = $3, error_message = $4,
             completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND status = 'running' AND lease_token = $2",
    )
    .bind(id)
    .bind(lease_token)
    .bind(error_kind)
    .bind(error_message)
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}

async fn find(pool: &AnyPool, id: &str) -> Result<Option<UserReplyBackfillJob>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, watch_id, uid, start_date, start_at_unix, end_at_unix,
                status, next_page, pages_requested, candidates_processed, posts_inserted,
                lease_token, error_kind, error_message,
                CAST(created_at AS TEXT) AS created_at,
                CAST(started_at AS TEXT) AS started_at,
                CAST(completed_at AS TEXT) AS completed_at
         FROM uid_reply_backfill_jobs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(map_job))
}

fn map_job(row: &sqlx::any::AnyRow) -> UserReplyBackfillJob {
    UserReplyBackfillJob {
        id: row.get("id"),
        watch_id: row.get("watch_id"),
        uid: row.get("uid"),
        start_date: row.get("start_date"),
        start_at_unix: row.get("start_at_unix"),
        end_at_unix: row.get("end_at_unix"),
        status: row.get("status"),
        next_page: row.get("next_page"),
        pages_requested: row.get("pages_requested"),
        candidates_processed: row.get("candidates_processed"),
        posts_inserted: row.get("posts_inserted"),
        lease_token: row.get("lease_token"),
        error_kind: row.get("error_kind"),
        error_message: row.get("error_message"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
    }
}

fn lease_expression(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '5 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+5 minutes')",
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use sqlx::any::AnyPoolOptions;

    use super::{CreateBackfillError, advance_page, claim, create, finish, list};
    use crate::{config::DatabaseBackend, repository::watch};

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

    #[tokio::test]
    async fn create_requires_a_watch_and_serializes_active_jobs() {
        let pool = test_pool().await;
        let missing = create(&pool, 2001, "2026-09-01", 1, 2).await;
        assert!(matches!(missing, Err(CreateBackfillError::WatchNotFound)));

        watch::create_user_watch(&pool, 2001, 60)
            .await
            .expect("user watch must create");
        let created = create(&pool, 2001, "2026-09-01", 1, 2)
            .await
            .expect("backfill must create");
        assert_eq!(created.status, "pending");
        let conflict = create(&pool, 2001, "2026-09-02", 2, 3).await;
        assert!(matches!(conflict, Err(CreateBackfillError::Conflict)));
    }

    #[tokio::test]
    async fn stale_claim_is_recoverable_and_progress_is_lease_guarded() {
        let pool = test_pool().await;
        watch::create_user_watch(&pool, 2001, 60)
            .await
            .expect("user watch must create");
        let created = create(&pool, 2001, "2026-09-01", 1, 2)
            .await
            .expect("backfill must create");
        let first = claim(&pool, DatabaseBackend::Sqlite)
            .await
            .expect("claim must query")
            .expect("job must be claimable");
        let first_token = first.lease_token.expect("claim must have a token");

        sqlx::query(
            "UPDATE uid_reply_backfill_jobs
             SET lease_until = datetime(CURRENT_TIMESTAMP, '-1 minute') WHERE id = $1",
        )
        .bind(&created.id)
        .execute(&pool)
        .await
        .expect("lease must expire");
        let second = claim(&pool, DatabaseBackend::Sqlite)
            .await
            .expect("reclaim must query")
            .expect("stale job must be reclaimed");
        let second_token = second.lease_token.expect("reclaim must have a token");
        assert_ne!(first_token, second_token);

        let mut tx = pool.begin().await.expect("transaction must begin");
        assert!(
            !advance_page(
                &mut tx,
                DatabaseBackend::Sqlite,
                &created.id,
                &first_token,
                2,
                3,
                1,
                1,
            )
            .await
            .expect("stale progress update must query")
        );
        assert!(
            advance_page(
                &mut tx,
                DatabaseBackend::Sqlite,
                &created.id,
                &second_token,
                2,
                3,
                1,
                1,
            )
            .await
            .expect("owned progress update must query")
        );
        tx.commit().await.expect("progress must commit");
        assert!(
            finish(&pool, &created.id, &second_token)
                .await
                .expect("finish must query")
        );
        let jobs = list(&pool).await.expect("jobs must list");
        assert_eq!(jobs[0].status, "succeeded");
        assert_eq!(jobs[0].next_page, 2);
        assert_eq!(jobs[0].pages_requested, 3);
        assert_eq!(jobs[0].candidates_processed, 1);
        assert_eq!(jobs[0].posts_inserted, 1);
    }
}
