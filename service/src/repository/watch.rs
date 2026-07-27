use sqlx::{AnyPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::config::DatabaseBackend;

#[derive(Clone, Debug)]
pub struct WatchTarget {
    pub id: String,
    pub target_type: String,
    pub target_id: i64,
    pub enabled: bool,
    pub interval_seconds: i32,
    pub status: String,
    pub baseline_completed: bool,
    pub next_run_at: String,
    pub last_completed_at: Option<String>,
    pub last_error_kind: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ThreadCursor {
    pub last_floor: i32,
    pub remote_vrows: i32,
}

#[derive(Debug, Error)]
pub enum CreateWatchError {
    #[error("watch already exists")]
    Conflict,
    #[error("database error")]
    Database(#[source] sqlx::Error),
}

pub async fn create_thread_watch(
    pool: &AnyPool,
    tid: i64,
    interval_seconds: i32,
) -> Result<WatchTarget, CreateWatchError> {
    let id = Uuid::new_v4().to_string();
    let mut tx = pool.begin().await.map_err(CreateWatchError::Database)?;
    let result = sqlx::query(
        "INSERT INTO watch_targets
            (id, target_type, target_id, interval_seconds)
         VALUES ($1, 'thread', $2, $3)",
    )
    .bind(&id)
    .bind(tid)
    .bind(interval_seconds)
    .execute(&mut *tx)
    .await;

    match result {
        Ok(_) => {}
        Err(error) if is_unique_violation(&error) => return Err(CreateWatchError::Conflict),
        Err(error) => return Err(CreateWatchError::Database(error)),
    }

    sqlx::query(
        "INSERT INTO watch_cursors (watch_id, last_floor)
         VALUES ($1, -1)",
    )
    .bind(&id)
    .execute(&mut *tx)
    .await
    .map_err(CreateWatchError::Database)?;
    tx.commit().await.map_err(CreateWatchError::Database)?;

    find(pool, &id)
        .await
        .map_err(CreateWatchError::Database)?
        .ok_or_else(|| CreateWatchError::Database(sqlx::Error::RowNotFound))
}

pub async fn list(pool: &AnyPool) -> Result<Vec<WatchTarget>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, target_type, target_id, enabled, interval_seconds, status,
         baseline_completed, CAST(next_run_at AS TEXT) AS next_run_at,
         CAST(last_completed_at AS TEXT) AS last_completed_at, last_error_kind
         FROM watch_targets
         ORDER BY created_at, id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(map_watch).collect())
}

pub async fn find(pool: &AnyPool, id: &str) -> Result<Option<WatchTarget>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, target_type, target_id, enabled, interval_seconds, status,
         baseline_completed, CAST(next_run_at AS TEXT) AS next_run_at,
         CAST(last_completed_at AS TEXT) AS last_completed_at, last_error_kind
         FROM watch_targets WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(map_watch))
}

pub async fn thread_cursor(pool: &AnyPool, watch_id: &str) -> Result<ThreadCursor, sqlx::Error> {
    let row = sqlx::query(
        "SELECT last_floor, remote_vrows, remote_total_pages
         FROM watch_cursors WHERE watch_id = $1",
    )
    .bind(watch_id)
    .fetch_one(pool)
    .await?;
    Ok(ThreadCursor {
        last_floor: row.get("last_floor"),
        remote_vrows: row.get("remote_vrows"),
    })
}

pub async fn update(
    pool: &AnyPool,
    id: &str,
    enabled: Option<bool>,
    interval_seconds: Option<i32>,
) -> Result<Option<WatchTarget>, sqlx::Error> {
    if let Some(enabled) = enabled {
        sqlx::query(
            "UPDATE watch_targets SET enabled = $1,
             status = CASE WHEN $1 = 1 THEN 'pending' ELSE 'paused' END,
             next_run_at = CASE WHEN $1 = 1 THEN CURRENT_TIMESTAMP ELSE next_run_at END,
             updated_at = CURRENT_TIMESTAMP
             WHERE id = $2",
        )
        .bind(i32::from(enabled))
        .bind(id)
        .execute(pool)
        .await?;
    }
    if let Some(interval_seconds) = interval_seconds {
        sqlx::query(
            "UPDATE watch_targets SET interval_seconds = $1, updated_at = CURRENT_TIMESTAMP
             WHERE id = $2",
        )
        .bind(interval_seconds)
        .bind(id)
        .execute(pool)
        .await?;
    }
    find(pool, id).await
}

pub async fn delete(pool: &AnyPool, id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM watch_targets WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected()
        == 1)
}

pub async fn claim_due(
    pool: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Option<WatchTarget>, sqlx::Error> {
    let lease_expression = match backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '5 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+5 minutes')",
    };
    let query = format!(
        "UPDATE watch_targets
         SET status = 'running', lease_until = {lease_expression},
             last_started_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE id = (
             SELECT id FROM watch_targets
             WHERE target_type = 'thread' AND enabled = 1
               AND next_run_at <= CURRENT_TIMESTAMP
               AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
             ORDER BY next_run_at, created_at LIMIT 1
         )
         AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
         RETURNING id"
    );
    let claimed = sqlx::query(&query).fetch_optional(pool).await?;
    let Some(claimed) = claimed else {
        return Ok(None);
    };
    let id: String = claimed.get("id");
    find(pool, &id).await
}

pub async fn claim_by_id(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<Option<WatchTarget>, sqlx::Error> {
    let lease_expression = match backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '5 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+5 minutes')",
    };
    let query = format!(
        "UPDATE watch_targets
         SET status = 'running', lease_until = {lease_expression},
             last_started_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND target_type = 'thread'
           AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
         RETURNING id"
    );
    let claimed = sqlx::query(&query).bind(id).fetch_optional(pool).await?;
    if claimed.is_none() {
        return Ok(None);
    }
    find(pool, id).await
}

fn map_watch(row: &sqlx::any::AnyRow) -> WatchTarget {
    WatchTarget {
        id: row.get("id"),
        target_type: row.get("target_type"),
        target_id: row.get("target_id"),
        enabled: row.get::<i32, _>("enabled") == 1,
        interval_seconds: row.get("interval_seconds"),
        status: row.get("status"),
        baseline_completed: row.get::<i32, _>("baseline_completed") == 1,
        next_run_at: row.get("next_run_at"),
        last_completed_at: row.get("last_completed_at"),
        last_error_kind: row.get("last_error_kind"),
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "23505" || code == "2067")
}

#[cfg(test)]
mod tests {
    use sqlx::any::AnyPoolOptions;

    use super::{CreateWatchError, create_thread_watch, delete, list, update};

    #[tokio::test]
    async fn thread_watch_crud_and_conflict() {
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

        let created = create_thread_watch(&pool, 12345, 60)
            .await
            .expect("watch must create");
        assert_eq!(created.target_id, 12345);
        assert_eq!(list(&pool).await.expect("watches must list").len(), 1);

        assert!(matches!(
            create_thread_watch(&pool, 12345, 60).await,
            Err(CreateWatchError::Conflict)
        ));

        let paused = update(&pool, &created.id, Some(false), Some(120))
            .await
            .expect("watch must update")
            .expect("watch must exist");
        assert!(!paused.enabled);
        assert_eq!(paused.interval_seconds, 120);
        assert_eq!(paused.status, "paused");

        let resumed = update(&pool, &created.id, Some(true), None)
            .await
            .expect("watch must update")
            .expect("watch must exist");
        assert!(resumed.enabled);
        assert_eq!(resumed.status, "pending");

        assert!(delete(&pool, &created.id).await.expect("watch must delete"));
        assert!(list(&pool).await.expect("watches must list").is_empty());
    }
}
