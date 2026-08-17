use std::collections::HashSet;

use sqlx::{Any, AnyPool, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::config::DatabaseBackend;
use crate::schedule::Schedule;

#[derive(Clone, Debug)]
pub struct WatchTarget {
    pub id: String,
    pub lease_token: Option<String>,
    pub target_type: String,
    pub target_id: i64,
    pub target_name: String,
    pub enabled: bool,
    pub interval_seconds: i32,
    pub schedule: Option<Schedule>,
    pub status: String,
    pub baseline_completed: bool,
    pub next_run_at: String,
    pub last_completed_at: Option<String>,
    pub last_error_kind: Option<String>,
    pub history_mode: Option<String>,
    pub history_parallel_enabled: bool,
    pub history_parallelism: i32,
    pub author_uids: Vec<i64>,
    pub channel_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ThreadCursor {
    pub last_floor: i32,
    pub remote_vrows: i32,
    pub remote_total_pages: i32,
}

#[derive(Clone, Debug)]
pub struct UserCursor {
    pub newest_topic_at_unix: i64,
    pub newest_topic_tid: i64,
    pub newest_reply_at_unix: i64,
    pub newest_reply_pid: i64,
}

#[derive(Debug, Error)]
pub enum CreateWatchError {
    #[error("watch already exists")]
    Conflict,
    #[error("notification channel does not exist")]
    InvalidChannel,
    #[error("database error")]
    Database(#[source] sqlx::Error),
}

#[derive(Debug, Error)]
pub enum ResetWatchError {
    #[error("watch is running")]
    Busy,
    #[error("database error")]
    Database(#[from] sqlx::Error),
}

#[cfg(test)]
pub async fn create_thread_watch(
    pool: &AnyPool,
    tid: i64,
    interval_seconds: i32,
) -> Result<WatchTarget, CreateWatchError> {
    create_thread_watch_with_config(
        pool,
        tid,
        interval_seconds,
        None,
        "full",
        false,
        2,
        &[],
        &[],
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn create_thread_watch_with_config(
    pool: &AnyPool,
    tid: i64,
    interval_seconds: i32,
    schedule: Option<&Schedule>,
    history_mode: &str,
    history_parallel_enabled: bool,
    history_parallelism: i32,
    author_uids: &[i64],
    channel_ids: &[String],
) -> Result<WatchTarget, CreateWatchError> {
    let id = Uuid::new_v4().to_string();
    let mut tx = pool.begin().await.map_err(CreateWatchError::Database)?;
    validate_channels(&mut tx, channel_ids).await?;
    insert_watch_target(&mut tx, &id, "thread", tid, interval_seconds, schedule).await?;
    sqlx::query(
        "INSERT INTO thread_watch_options
            (watch_id, history_mode, history_parallel_enabled, history_parallelism)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&id)
    .bind(history_mode)
    .bind(i32::from(history_parallel_enabled))
    .bind(history_parallelism)
    .execute(&mut *tx)
    .await
    .map_err(CreateWatchError::Database)?;
    sqlx::query("INSERT INTO watch_cursors (watch_id, last_floor) VALUES ($1, -1)")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
    insert_notification_config(&mut tx, &id, author_uids, channel_ids)
        .await
        .map_err(CreateWatchError::Database)?;
    tx.commit().await.map_err(CreateWatchError::Database)?;
    find(pool, &id)
        .await
        .map_err(CreateWatchError::Database)?
        .ok_or_else(|| CreateWatchError::Database(sqlx::Error::RowNotFound))
}

#[cfg(test)]
pub async fn create_user_watch(
    pool: &AnyPool,
    uid: i64,
    interval_seconds: i32,
) -> Result<WatchTarget, CreateWatchError> {
    create_user_watch_with_config(pool, uid, interval_seconds, None, &[]).await
}

pub async fn create_user_watch_with_config(
    pool: &AnyPool,
    uid: i64,
    interval_seconds: i32,
    schedule: Option<&Schedule>,
    channel_ids: &[String],
) -> Result<WatchTarget, CreateWatchError> {
    let id = Uuid::new_v4().to_string();
    let mut tx = pool.begin().await.map_err(CreateWatchError::Database)?;
    validate_channels(&mut tx, channel_ids).await?;
    insert_watch_target(&mut tx, &id, "user", uid, interval_seconds, schedule).await?;
    sqlx::query("INSERT INTO user_watch_cursors (watch_id) VALUES ($1)")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
    insert_notification_config(&mut tx, &id, &[], channel_ids)
        .await
        .map_err(CreateWatchError::Database)?;
    tx.commit().await.map_err(CreateWatchError::Database)?;
    find(pool, &id)
        .await
        .map_err(CreateWatchError::Database)?
        .ok_or_else(|| CreateWatchError::Database(sqlx::Error::RowNotFound))
}

async fn insert_watch_target(
    tx: &mut Transaction<'_, Any>,
    id: &str,
    target_type: &str,
    target_id: i64,
    interval_seconds: i32,
    schedule: Option<&Schedule>,
) -> Result<(), CreateWatchError> {
    let result = sqlx::query(
        "INSERT INTO watch_targets
            (id, target_type, target_id, interval_seconds, schedule_json)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(target_type)
    .bind(target_id)
    .bind(interval_seconds)
    .bind(schedule_json(schedule))
    .execute(&mut **tx)
    .await;
    match result {
        Ok(_) => Ok(()),
        Err(error) if is_unique_violation(&error) => Err(CreateWatchError::Conflict),
        Err(error) => Err(CreateWatchError::Database(error)),
    }
}

pub async fn list(pool: &AnyPool) -> Result<Vec<WatchTarget>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, lease_token, target_type, target_id, target_name, enabled, interval_seconds, schedule_json, status,
         baseline_completed, CAST(next_run_at AS TEXT) AS next_run_at,
         CAST(last_completed_at AS TEXT) AS last_completed_at, last_error_kind
         FROM watch_targets WHERE deleted_at IS NULL
         ORDER BY created_at, id",
    )
    .fetch_all(pool)
    .await?;
    let mut watches = Vec::with_capacity(rows.len());
    for row in &rows {
        watches.push(load_config(pool, map_watch(row)).await?);
    }
    Ok(watches)
}

pub async fn find(pool: &AnyPool, id: &str) -> Result<Option<WatchTarget>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, lease_token, target_type, target_id, target_name, enabled, interval_seconds, schedule_json, status,
         baseline_completed, CAST(next_run_at AS TEXT) AS next_run_at,
         CAST(last_completed_at AS TEXT) AS last_completed_at, last_error_kind
         FROM watch_targets WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    match row {
        Some(row) => Ok(Some(load_config(pool, map_watch(&row)).await?)),
        None => Ok(None),
    }
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
        remote_total_pages: row.get("remote_total_pages"),
    })
}

pub async fn user_cursor(pool: &AnyPool, watch_id: &str) -> Result<UserCursor, sqlx::Error> {
    let row = sqlx::query(
        "SELECT newest_topic_at_unix, newest_topic_tid,
         newest_reply_at_unix, newest_reply_pid
         FROM user_watch_cursors WHERE watch_id = $1",
    )
    .bind(watch_id)
    .fetch_one(pool)
    .await?;
    Ok(UserCursor {
        newest_topic_at_unix: row.get("newest_topic_at_unix"),
        newest_topic_tid: row.get("newest_topic_tid"),
        newest_reply_at_unix: row.get("newest_reply_at_unix"),
        newest_reply_pid: row.get("newest_reply_pid"),
    })
}

#[cfg(test)]
pub async fn update(
    pool: &AnyPool,
    id: &str,
    enabled: Option<bool>,
    interval_seconds: Option<i32>,
) -> Result<Option<WatchTarget>, CreateWatchError> {
    update_with_config(
        pool,
        id,
        enabled,
        interval_seconds,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn update_with_config(
    pool: &AnyPool,
    id: &str,
    enabled: Option<bool>,
    interval_seconds: Option<i32>,
    schedule: Option<Option<&Schedule>>,
    history_mode: Option<&str>,
    history_parallel_enabled: Option<bool>,
    history_parallelism: Option<i32>,
    author_uids: Option<&[i64]>,
    channel_ids: Option<&[String]>,
) -> Result<Option<WatchTarget>, CreateWatchError> {
    let mut tx = pool.begin().await.map_err(CreateWatchError::Database)?;
    let changes_runtime_config = enabled.is_some()
        || interval_seconds.is_some()
        || schedule.is_some()
        || history_mode.is_some()
        || history_parallel_enabled.is_some()
        || history_parallelism.is_some()
        || author_uids.is_some()
        || channel_ids.is_some();
    if changes_runtime_config {
        let reason = if enabled == Some(false) {
            "watch_paused"
        } else {
            "watch_updated"
        };
        sqlx::query(
            "UPDATE crawl_runs SET status = 'failed', error_kind = $1,
             error_message = $1, completed_at = CURRENT_TIMESTAMP
             WHERE watch_id = $2 AND status = 'running'",
        )
        .bind(reason)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
        sqlx::query(
            "UPDATE watch_targets SET
             status = CASE WHEN enabled = 1 THEN 'pending' ELSE 'paused' END,
             lease_until = NULL, lease_token = NULL,
             next_run_at = CASE WHEN enabled = 1 THEN CURRENT_TIMESTAMP ELSE next_run_at END,
             updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
    }
    if let Some(channel_ids) = channel_ids {
        validate_channels(&mut tx, channel_ids).await?;
    }
    if let Some(enabled) = enabled {
        sqlx::query(
            "UPDATE watch_targets SET enabled = $1,
             status = CASE WHEN $1 = 1 THEN 'pending' ELSE 'paused' END,
             pause_reason = CASE WHEN $1 = 1 THEN NULL ELSE 'user' END,
             lease_until = NULL, lease_token = NULL,
             next_run_at = CASE WHEN $1 = 1 THEN CURRENT_TIMESTAMP ELSE next_run_at END,
             updated_at = CURRENT_TIMESTAMP
             WHERE id = $2 AND deleted_at IS NULL",
        )
        .bind(i32::from(enabled))
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
    }
    if let Some(interval_seconds) = interval_seconds {
        sqlx::query(
            "UPDATE watch_targets SET interval_seconds = $1,
             next_run_at = CASE WHEN enabled = 1 THEN CURRENT_TIMESTAMP ELSE next_run_at END,
             updated_at = CURRENT_TIMESTAMP
             WHERE id = $2 AND deleted_at IS NULL",
        )
        .bind(interval_seconds)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
    }
    if let Some(schedule) = schedule {
        sqlx::query(
            "UPDATE watch_targets SET schedule_json = $1,
             next_run_at = CASE WHEN enabled = 1 THEN CURRENT_TIMESTAMP ELSE next_run_at END,
             updated_at = CURRENT_TIMESTAMP
             WHERE id = $2 AND deleted_at IS NULL",
        )
        .bind(schedule_json(schedule))
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
    }
    if history_mode.is_some() || history_parallel_enabled.is_some() || history_parallelism.is_some()
    {
        sqlx::query(
            "UPDATE thread_watch_options SET
             history_mode = COALESCE($1, history_mode),
             history_parallel_enabled = COALESCE($2, history_parallel_enabled),
             history_parallelism = COALESCE($3, history_parallelism),
             updated_at = CURRENT_TIMESTAMP WHERE watch_id = $4",
        )
        .bind(history_mode)
        .bind(history_parallel_enabled.map(i32::from))
        .bind(history_parallelism)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
    }
    if let Some(author_uids) = author_uids {
        sqlx::query("DELETE FROM watch_notification_authors WHERE watch_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(CreateWatchError::Database)?;
        insert_authors(&mut tx, id, author_uids)
            .await
            .map_err(CreateWatchError::Database)?;
    }
    if let Some(channel_ids) = channel_ids {
        sqlx::query("DELETE FROM watch_notification_channels WHERE watch_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(CreateWatchError::Database)?;
        insert_channels(&mut tx, id, channel_ids)
            .await
            .map_err(CreateWatchError::Database)?;
    }
    tx.commit().await.map_err(CreateWatchError::Database)?;
    find(pool, id).await.map_err(CreateWatchError::Database)
}

pub async fn reset(
    pool: &AnyPool,
    id: &str,
    history_mode: Option<&str>,
    history_parallel_enabled: Option<bool>,
    history_parallelism: Option<i32>,
) -> Result<Option<WatchTarget>, ResetWatchError> {
    let mut tx = pool.begin().await?;
    // Atomically take the row before touching its cursors. This closes the
    // SELECT-then-reset race with the regular worker claim path.
    let row = sqlx::query(
        "UPDATE watch_targets SET status = 'running', lease_until = CURRENT_TIMESTAMP,
         lease_token = NULL,
         updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND deleted_at IS NULL
           AND (status <> 'running' OR lease_until IS NULL
                OR lease_until <= CURRENT_TIMESTAMP)
         RETURNING target_type",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM watch_targets WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        return if exists == 0 {
            Ok(None)
        } else {
            Err(ResetWatchError::Busy)
        };
    };
    let target_type: String = row.get("target_type");
    sqlx::query(
        "UPDATE crawl_runs SET status = 'failed', error_kind = 'watch_reset',
         error_message = 'watch_reset', completed_at = CURRENT_TIMESTAMP
         WHERE watch_id = $1 AND status = 'running'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if target_type == "thread" {
        if history_mode.is_some()
            || history_parallel_enabled.is_some()
            || history_parallelism.is_some()
        {
            sqlx::query(
                "UPDATE thread_watch_options SET
                 history_mode = COALESCE($1, history_mode),
                 history_parallel_enabled = COALESCE($2, history_parallel_enabled),
                 history_parallelism = COALESCE($3, history_parallelism),
                 updated_at = CURRENT_TIMESTAMP WHERE watch_id = $4",
            )
            .bind(history_mode)
            .bind(history_parallel_enabled.map(i32::from))
            .bind(history_parallelism)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE watch_cursors SET last_floor = -1, remote_vrows = 0,
             remote_total_pages = 0, updated_at = CURRENT_TIMESTAMP WHERE watch_id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE user_watch_cursors SET newest_topic_at_unix = 0, newest_topic_tid = 0,
             newest_reply_at_unix = 0, newest_reply_pid = 0,
             updated_at = CURRENT_TIMESTAMP WHERE watch_id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE watch_targets SET baseline_completed = 0, status = 'pending',
         enabled = 1, pause_reason = NULL, lease_until = NULL, lease_token = NULL,
         next_run_at = CURRENT_TIMESTAMP,
         last_error_kind = NULL, last_error_message = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(find(pool, id).await?)
}

pub async fn delete(pool: &AnyPool, id: &str) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE crawl_runs SET status = 'failed', error_kind = 'watch_deleted',
         error_message = 'watch_deleted', completed_at = CURRENT_TIMESTAMP
         WHERE watch_id = $1 AND status = 'running'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let deleted = sqlx::query(
        "UPDATE watch_targets SET enabled = 0, status = 'paused', lease_until = NULL,
         lease_token = NULL,
         deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    tx.commit().await?;
    Ok(deleted)
}

pub async fn claim_due(
    pool: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Option<WatchTarget>, sqlx::Error> {
    claim(pool, backend, None).await
}

pub async fn claim_by_id(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<Option<WatchTarget>, sqlx::Error> {
    claim(pool, backend, Some(id)).await
}

/// Ask the regular worker to run a watch as soon as possible without doing
/// collector I/O in the caller (notably the single bot inbound consumer).
pub async fn request_run(pool: &AnyPool, id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE watch_targets SET next_run_at = CURRENT_TIMESTAMP,
         status = CASE WHEN status = 'running' THEN status ELSE 'pending' END,
         updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND enabled = 1 AND deleted_at IS NULL
           AND (status <> 'running' OR lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)",
    )
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}

async fn claim(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: Option<&str>,
) -> Result<Option<WatchTarget>, sqlx::Error> {
    let lease_expression = lease_expression(backend);
    let lease_token = Uuid::new_v4().to_string();
    let query = if id.is_some() {
        format!(
            "UPDATE watch_targets SET status = 'running', lease_until = {lease_expression},
             lease_token = $2,
             last_started_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND deleted_at IS NULL
               AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
             RETURNING id"
        )
    } else {
        format!(
            "UPDATE watch_targets SET status = 'running', lease_until = {lease_expression},
             lease_token = $1,
             last_started_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = (
                 SELECT id FROM watch_targets
                 WHERE enabled = 1 AND deleted_at IS NULL
                   AND next_run_at <= CURRENT_TIMESTAMP
                   AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
                 ORDER BY next_run_at, created_at LIMIT 1
             )
             AND deleted_at IS NULL
             AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
             RETURNING id"
        )
    };
    let mut query = sqlx::query(&query);
    if let Some(id) = id {
        query = query.bind(id).bind(&lease_token);
    } else {
        query = query.bind(&lease_token);
    }
    let claimed = query.fetch_optional(pool).await?;
    let Some(claimed) = claimed else {
        return Ok(None);
    };
    find(pool, &claimed.get::<String, _>("id")).await
}

pub async fn renew_lease(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    lease_token: &str,
) -> Result<bool, sqlx::Error> {
    let query = format!(
        "UPDATE watch_targets SET lease_until = {}, updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND lease_token = $2 AND status = 'running' AND deleted_at IS NULL",
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

pub async fn update_target_name(
    tx: &mut Transaction<'_, Any>,
    watch_id: &str,
    lease_token: &str,
    candidate: &str,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        "UPDATE watch_targets
         SET target_name = COALESCE(NULLIF(TRIM($2), ''), CAST(target_id AS TEXT)),
             updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND lease_token = $3 AND status = 'running'
           AND deleted_at IS NULL",
    )
    .bind(watch_id)
    .bind(candidate)
    .bind(lease_token)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() == 1 {
        Ok(())
    } else {
        Err(sqlx::Error::RowNotFound)
    }
}

fn lease_expression(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '5 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+5 minutes')",
    }
}

fn map_watch(row: &sqlx::any::AnyRow) -> WatchTarget {
    let schedule_json: Option<String> = row.get("schedule_json");
    WatchTarget {
        id: row.get("id"),
        lease_token: row.get("lease_token"),
        target_type: row.get("target_type"),
        target_id: row.get("target_id"),
        target_name: row.get("target_name"),
        enabled: row.get::<i32, _>("enabled") == 1,
        interval_seconds: row.get("interval_seconds"),
        schedule: schedule_json.and_then(|value| serde_json::from_str(&value).ok()),
        status: row.get("status"),
        baseline_completed: row.get::<i32, _>("baseline_completed") == 1,
        next_run_at: row.get("next_run_at"),
        last_completed_at: row.get("last_completed_at"),
        last_error_kind: row.get("last_error_kind"),
        history_mode: None,
        history_parallel_enabled: false,
        history_parallelism: 2,
        author_uids: Vec::new(),
        channel_ids: Vec::new(),
    }
}

async fn load_config(pool: &AnyPool, mut watch: WatchTarget) -> Result<WatchTarget, sqlx::Error> {
    if watch.target_type == "thread"
        && let Some(row) = sqlx::query(
            "SELECT history_mode, history_parallel_enabled, history_parallelism
             FROM thread_watch_options WHERE watch_id = $1",
        )
        .bind(&watch.id)
        .fetch_optional(pool)
        .await?
    {
        watch.history_mode = Some(row.get("history_mode"));
        watch.history_parallel_enabled = row.get::<i32, _>("history_parallel_enabled") == 1;
        watch.history_parallelism = row.get("history_parallelism");
    }
    watch.author_uids = sqlx::query(
        "SELECT author_uid FROM watch_notification_authors
         WHERE watch_id = $1 ORDER BY author_uid",
    )
    .bind(&watch.id)
    .fetch_all(pool)
    .await?
    .iter()
    .map(|row| row.get("author_uid"))
    .collect();
    watch.channel_ids = sqlx::query(
        "SELECT channel_id FROM watch_notification_channels
         WHERE watch_id = $1 ORDER BY channel_id",
    )
    .bind(&watch.id)
    .fetch_all(pool)
    .await?
    .iter()
    .map(|row| row.get("channel_id"))
    .collect();
    Ok(watch)
}

async fn validate_channels(
    tx: &mut Transaction<'_, Any>,
    channel_ids: &[String],
) -> Result<(), CreateWatchError> {
    let mut unique = HashSet::new();
    for channel_id in channel_ids {
        if !unique.insert(channel_id) {
            return Err(CreateWatchError::InvalidChannel);
        }
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM notification_channels WHERE id = $1")
                .bind(channel_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(CreateWatchError::Database)?;
        if exists != 1 {
            return Err(CreateWatchError::InvalidChannel);
        }
    }
    Ok(())
}

async fn insert_notification_config(
    tx: &mut Transaction<'_, Any>,
    watch_id: &str,
    author_uids: &[i64],
    channel_ids: &[String],
) -> Result<(), sqlx::Error> {
    insert_authors(tx, watch_id, author_uids).await?;
    insert_channels(tx, watch_id, channel_ids).await
}

async fn insert_authors(
    tx: &mut Transaction<'_, Any>,
    watch_id: &str,
    author_uids: &[i64],
) -> Result<(), sqlx::Error> {
    for uid in author_uids {
        sqlx::query(
            "INSERT INTO watch_notification_authors (watch_id, author_uid) VALUES ($1, $2)",
        )
        .bind(watch_id)
        .bind(uid)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn insert_channels(
    tx: &mut Transaction<'_, Any>,
    watch_id: &str,
    channel_ids: &[String],
) -> Result<(), sqlx::Error> {
    for channel_id in channel_ids {
        sqlx::query(
            "INSERT INTO watch_notification_channels (watch_id, channel_id) VALUES ($1, $2)",
        )
        .bind(watch_id)
        .bind(channel_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

fn schedule_json(schedule: Option<&Schedule>) -> Option<String> {
    schedule
        .filter(|items| !items.is_empty())
        .map(|items| serde_json::to_string(items).expect("schedule must serialize"))
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

    use super::{
        CreateWatchError, ResetWatchError, claim_by_id, create_thread_watch,
        create_user_watch_with_config, delete, list, renew_lease, reset, update,
    };
    use crate::config::DatabaseBackend;

    #[tokio::test]
    async fn thread_watch_crud_and_soft_delete() {
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
        assert_eq!(created.history_parallelism, 2);
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
        let pause_reason: Option<String> =
            sqlx::query_scalar("SELECT pause_reason FROM watch_targets WHERE id = $1")
                .bind(&created.id)
                .fetch_one(&pool)
                .await
                .expect("pause reason must query");
        assert_eq!(pause_reason.as_deref(), Some("user"));

        update(&pool, &created.id, Some(true), None)
            .await
            .expect("watch must resume")
            .expect("watch must exist");
        let claimed = claim_by_id(&pool, DatabaseBackend::Sqlite, &created.id)
            .await
            .expect("watch claim must succeed")
            .expect("watch must be claimable");
        let stale_token = claimed
            .lease_token
            .clone()
            .expect("claim must have a token");
        sqlx::query(
            "INSERT INTO crawl_runs (id, watch_id, status, baseline, sync_mode)
             VALUES ('paused-run', $1, 'running', 0, 'incremental')",
        )
        .bind(&created.id)
        .execute(&pool)
        .await
        .expect("crawl run must seed");
        update(&pool, &created.id, Some(false), None)
            .await
            .expect("running watch must pause")
            .expect("watch must exist");
        assert!(
            !renew_lease(&pool, DatabaseBackend::Sqlite, &created.id, &stale_token)
                .await
                .expect("stale renewal must query")
        );
        let paused_state: (String, String, Option<String>) = sqlx::query_as(
            "SELECT status, pause_reason, lease_token FROM watch_targets WHERE id = $1",
        )
        .bind(&created.id)
        .fetch_one(&pool)
        .await
        .expect("paused watch must query");
        assert_eq!(paused_state, ("paused".into(), "user".into(), None));
        let paused_run: (String, String) =
            sqlx::query_as("SELECT status, error_kind FROM crawl_runs WHERE id = 'paused-run'")
                .fetch_one(&pool)
                .await
                .expect("invalidated crawl run must query");
        assert_eq!(paused_run, ("failed".into(), "watch_paused".into()));

        sqlx::query(
            "UPDATE watch_targets SET enabled = 0, status = 'paused', pause_reason = 'auth'
             WHERE id = $1",
        )
        .bind(&created.id)
        .execute(&pool)
        .await
        .expect("auth pause must seed");
        let resumed = update(&pool, &created.id, Some(true), None)
            .await
            .expect("watch must resume")
            .expect("watch must exist");
        assert!(resumed.enabled);
        let pause_reason: Option<String> =
            sqlx::query_scalar("SELECT pause_reason FROM watch_targets WHERE id = $1")
                .bind(&created.id)
                .fetch_one(&pool)
                .await
                .expect("pause reason must query");
        assert!(pause_reason.is_none());

        update(&pool, &created.id, Some(false), None)
            .await
            .expect("watch must pause again")
            .expect("watch must exist");
        let pause_reason: Option<String> =
            sqlx::query_scalar("SELECT pause_reason FROM watch_targets WHERE id = $1")
                .bind(&created.id)
                .fetch_one(&pool)
                .await
                .expect("pause reason must query");
        assert_eq!(pause_reason.as_deref(), Some("user"));

        sqlx::query(
            "UPDATE watch_targets SET status = 'running',
             lease_until = '2099-01-01 00:00:00' WHERE id = $1",
        )
        .bind(&created.id)
        .execute(&pool)
        .await
        .expect("live lease must seed");
        assert!(matches!(
            reset(&pool, &created.id, Some("full"), Some(false), Some(2)).await,
            Err(ResetWatchError::Busy)
        ));
        sqlx::query("UPDATE watch_targets SET lease_until = '2000-01-01 00:00:00' WHERE id = $1")
            .bind(&created.id)
            .execute(&pool)
            .await
            .expect("stale lease must seed");
        let reset_watch = reset(&pool, &created.id, Some("full"), Some(false), Some(2))
            .await
            .expect("stale lease must be resettable")
            .expect("watch must exist");
        assert!(reset_watch.enabled);
        assert_eq!(reset_watch.status, "pending");

        assert!(delete(&pool, &created.id).await.expect("watch must delete"));
        assert!(list(&pool).await.expect("watches must list").is_empty());
        create_thread_watch(&pool, 12345, 60)
            .await
            .expect("soft-deleted target can be recreated");

        sqlx::query(
            "INSERT INTO platform_integrations
             (id, platform, label, credentials_encrypted)
             VALUES ('integration', 'bark', 'integration', X'00')",
        )
        .execute(&pool)
        .await
        .expect("integration must create");
        sqlx::query(
            "INSERT INTO notification_channels
             (id, integration_id, label, target_encrypted)
             VALUES ('channel', 'integration', 'channel', X'00')",
        )
        .execute(&pool)
        .await
        .expect("channel must create");
        let user = create_user_watch_with_config(&pool, 2001, 60, None, &["channel".to_owned()])
            .await
            .expect("configured user watch must create");
        assert_eq!(user.channel_ids, vec!["channel"]);
        assert!(user.author_uids.is_empty());
    }
}
