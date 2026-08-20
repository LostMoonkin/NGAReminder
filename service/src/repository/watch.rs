use std::collections::HashSet;

use sqlx::{Any, AnyPool, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::config::DatabaseBackend;
use crate::no_fetch::{
    NoFetchPeriods, current_window, format_database_timestamp, format_postgres_timestamp,
};
use crate::schedule::Schedule;
use time::{OffsetDateTime, UtcOffset};

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
    pub no_fetch_periods: Option<NoFetchPeriods>,
    pub status: String,
    pub baseline_completed: bool,
    pub next_run_at: String,
    pub last_completed_at: Option<String>,
    pub last_error_kind: Option<String>,
    pub pending_trigger_kind: Option<String>,
    pub lease_trigger_kind: Option<String>,
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

#[allow(clippy::too_many_arguments, dead_code)]
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
    create_thread_watch_with_no_fetch_config(
        pool,
        tid,
        interval_seconds,
        schedule,
        None,
        history_mode,
        history_parallel_enabled,
        history_parallelism,
        author_uids,
        channel_ids,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn create_thread_watch_with_no_fetch_config(
    pool: &AnyPool,
    tid: i64,
    interval_seconds: i32,
    schedule: Option<&Schedule>,
    no_fetch_periods: Option<&NoFetchPeriods>,
    history_mode: &str,
    history_parallel_enabled: bool,
    history_parallelism: i32,
    author_uids: &[i64],
    channel_ids: &[String],
) -> Result<WatchTarget, CreateWatchError> {
    let id = Uuid::new_v4().to_string();
    let mut tx = pool.begin().await.map_err(CreateWatchError::Database)?;
    validate_channels(&mut tx, channel_ids).await?;
    insert_watch_target(
        &mut tx,
        &id,
        "thread",
        tid,
        interval_seconds,
        schedule,
        no_fetch_periods,
    )
    .await?;
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

#[allow(dead_code)]
pub async fn create_user_watch_with_config(
    pool: &AnyPool,
    uid: i64,
    interval_seconds: i32,
    schedule: Option<&Schedule>,
    channel_ids: &[String],
) -> Result<WatchTarget, CreateWatchError> {
    create_user_watch_with_no_fetch_config(pool, uid, interval_seconds, schedule, None, channel_ids)
        .await
}

pub async fn create_user_watch_with_no_fetch_config(
    pool: &AnyPool,
    uid: i64,
    interval_seconds: i32,
    schedule: Option<&Schedule>,
    no_fetch_periods: Option<&NoFetchPeriods>,
    channel_ids: &[String],
) -> Result<WatchTarget, CreateWatchError> {
    let id = Uuid::new_v4().to_string();
    let mut tx = pool.begin().await.map_err(CreateWatchError::Database)?;
    validate_channels(&mut tx, channel_ids).await?;
    insert_watch_target(
        &mut tx,
        &id,
        "user",
        uid,
        interval_seconds,
        schedule,
        no_fetch_periods,
    )
    .await?;
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
    no_fetch_periods: Option<&NoFetchPeriods>,
) -> Result<(), CreateWatchError> {
    let result = sqlx::query(
        "INSERT INTO watch_targets
            (id, target_type, target_id, interval_seconds, schedule_json, no_fetch_periods_json)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(target_type)
    .bind(target_id)
    .bind(interval_seconds)
    .bind(schedule_json(schedule))
    .bind(no_fetch_json(no_fetch_periods))
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
        "SELECT id, lease_token, target_type, target_id, target_name, enabled, interval_seconds, schedule_json,
         no_fetch_periods_json, pending_trigger_kind, lease_trigger_kind, status,
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
        "SELECT id, lease_token, target_type, target_id, target_name, enabled, interval_seconds, schedule_json,
         no_fetch_periods_json, pending_trigger_kind, lease_trigger_kind, status,
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

#[allow(clippy::too_many_arguments, dead_code)]
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
    update_with_no_fetch_config(
        pool,
        id,
        enabled,
        interval_seconds,
        schedule,
        None,
        history_mode,
        history_parallel_enabled,
        history_parallelism,
        author_uids,
        channel_ids,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn update_with_no_fetch_config(
    pool: &AnyPool,
    id: &str,
    enabled: Option<bool>,
    interval_seconds: Option<i32>,
    schedule: Option<Option<&Schedule>>,
    no_fetch_periods: Option<Option<&NoFetchPeriods>>,
    history_mode: Option<&str>,
    history_parallel_enabled: Option<bool>,
    history_parallelism: Option<i32>,
    author_uids: Option<&[i64]>,
    channel_ids: Option<&[String]>,
) -> Result<Option<WatchTarget>, CreateWatchError> {
    let mut tx = pool.begin().await.map_err(CreateWatchError::Database)?;
    if enabled == Some(false) {
        sqlx::query(
            "UPDATE crawl_runs SET status = 'failed', error_kind = 'watch_paused',
             error_message = 'watch_paused', completed_at = CURRENT_TIMESTAMP
             WHERE watch_id = $1 AND status = 'running'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
        sqlx::query(
            "UPDATE watch_targets SET enabled = 0, status = 'paused', pause_reason = 'user',
             lease_until = NULL, lease_token = NULL,
             pending_trigger_kind = NULL, lease_trigger_kind = NULL,
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
             status = CASE WHEN $1 = 1 AND status <> 'running' THEN 'pending'
                           WHEN $1 = 0 THEN 'paused' ELSE status END,
             pause_reason = CASE WHEN $1 = 1 THEN NULL ELSE 'user' END,
             pending_trigger_kind = CASE WHEN $1 = 1 THEN pending_trigger_kind ELSE NULL END,
             lease_until = CASE WHEN $1 = 1 AND status = 'running' THEN lease_until ELSE NULL END,
             lease_token = CASE WHEN $1 = 1 AND status = 'running' THEN lease_token ELSE NULL END,
             lease_trigger_kind = CASE WHEN $1 = 1 AND status = 'running' THEN lease_trigger_kind ELSE NULL END,
             next_run_at = CASE WHEN $1 = 1 AND status <> 'running' THEN CURRENT_TIMESTAMP ELSE next_run_at END,
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
             next_run_at = CASE WHEN enabled = 1 AND status <> 'running' THEN CURRENT_TIMESTAMP ELSE next_run_at END,
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
             next_run_at = CASE WHEN enabled = 1 AND status <> 'running' THEN CURRENT_TIMESTAMP ELSE next_run_at END,
             updated_at = CURRENT_TIMESTAMP
             WHERE id = $2 AND deleted_at IS NULL",
        )
        .bind(schedule_json(schedule))
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(CreateWatchError::Database)?;
    }
    if let Some(no_fetch_periods) = no_fetch_periods {
        sqlx::query(
            "UPDATE watch_targets SET no_fetch_periods_json = $1,
             next_run_at = CASE WHEN enabled = 1 AND status <> 'running' THEN CURRENT_TIMESTAMP ELSE next_run_at END,
             updated_at = CURRENT_TIMESTAMP
             WHERE id = $2 AND deleted_at IS NULL",
        )
        .bind(no_fetch_json(no_fetch_periods))
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
         pending_trigger_kind = NULL, lease_trigger_kind = NULL,
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
         lease_token = NULL, pending_trigger_kind = NULL, lease_trigger_kind = NULL,
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
    claim(pool, backend, None, None).await
}

#[allow(dead_code)]
pub async fn claim_by_id(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<Option<WatchTarget>, sqlx::Error> {
    claim(pool, backend, Some(id), None).await
}

pub async fn claim_by_id_with_trigger(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    trigger_kind: &str,
) -> Result<Option<WatchTarget>, sqlx::Error> {
    if !matches!(trigger_kind, "scheduled" | "manual") {
        return Ok(None);
    }
    claim(pool, backend, Some(id), Some(trigger_kind)).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualRunRequest {
    Requested,
    AlreadyRunning,
    AlreadyRequested,
    NotRunnable,
}

#[derive(Clone, Debug)]
pub struct NoFetchSkip {
    pub crawl_run_id: String,
    pub baseline: bool,
    pub sync_mode: String,
    pub no_fetch_until: OffsetDateTime,
}

#[derive(Clone, Debug)]
pub struct CrawlRunContext {
    pub crawl_run_id: String,
    pub trigger_kind: String,
}

#[derive(Clone, Debug)]
pub enum RunPreparation {
    Collect(CrawlRunContext),
    Skipped(NoFetchSkip),
}

pub fn valid_trigger_kind(value: Option<&str>) -> Option<&str> {
    value.filter(|kind| matches!(*kind, "scheduled" | "manual" | "unknown"))
}

/// Complete an automatic run that is covered by a no-fetch window.
///
/// The lease is the serialization point: after it is claimed, this
/// transaction creates the audit row and releases the lease together with
/// the boundary-based next run. No collector has an opportunity to perform
/// NGA I/O before this function returns.
pub async fn skip_no_fetch_period(
    pool: &AnyPool,
    backend: DatabaseBackend,
    watch_target: &WatchTarget,
    timezone_offset: UtcOffset,
) -> Result<Option<NoFetchSkip>, sqlx::Error> {
    if watch_target.lease_trigger_kind.as_deref() != Some("scheduled")
        || watch_target.pending_trigger_kind.is_some()
    {
        return Ok(None);
    }
    let Some(window) = current_window(
        watch_target.no_fetch_periods.as_ref(),
        OffsetDateTime::now_utc(),
        timezone_offset,
    ) else {
        return Ok(None);
    };
    let baseline = !watch_target.baseline_completed;
    let sync_mode = match watch_target.target_type.as_str() {
        "thread" => match (baseline, watch_target.history_mode.as_deref()) {
            (true, Some("incremental")) => "tid_incremental_baseline",
            (true, _) => "tid_full_baseline",
            (false, _) => "incremental",
        },
        "user" => {
            if baseline {
                "uid_baseline"
            } else {
                "incremental"
            }
        }
        _ => return Ok(None),
    };
    let run_id = Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE crawl_runs SET status = 'failed', error_kind = 'lease_expired',
         error_message = 'lease_expired', completed_at = CURRENT_TIMESTAMP
         WHERE watch_id = $1 AND status = 'running'",
    )
    .bind(&watch_target.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO crawl_runs
            (id, watch_id, status, baseline, sync_mode, trigger_kind, error_kind, error_message)
         VALUES ($1, $2, 'skipped', $3, $4, 'scheduled', 'no_fetch_period', 'no_fetch_period')",
    )
    .bind(&run_id)
    .bind(&watch_target.id)
    .bind(i32::from(baseline))
    .bind(sync_mode)
    .execute(&mut *tx)
    .await?;
    let next_run = match backend {
        DatabaseBackend::Postgres => format_postgres_timestamp(window.until),
        DatabaseBackend::Sqlite => format_database_timestamp(window.until),
    };
    let next_run_expression = match backend {
        DatabaseBackend::Postgres => "CAST($2 AS TIMESTAMPTZ)",
        DatabaseBackend::Sqlite => "$2",
    };
    let query = format!(
        "UPDATE watch_targets SET status = 'active',
         next_run_at = {next_run_expression}, lease_until = NULL, lease_token = NULL,
         lease_trigger_kind = NULL, last_completed_at = CURRENT_TIMESTAMP,
         last_error_kind = 'no_fetch_period', last_error_message = 'no_fetch_period',
         updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND lease_token = $3 AND status = 'running'
           AND deleted_at IS NULL"
    );
    let updated = sqlx::query(&query)
        .bind(&watch_target.id)
        .bind(next_run)
        .bind(watch_target.lease_token.as_deref().unwrap_or_default())
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(None);
    }
    tx.commit().await?;
    Ok(Some(NoFetchSkip {
        crawl_run_id: run_id,
        baseline,
        sync_mode: sync_mode.to_owned(),
        no_fetch_until: window.until,
    }))
}

/// Create the run record after a lease has been claimed. Automatic runs that
/// are covered by a no-fetch window use the same transaction as
/// `skip_no_fetch_period`; all other runs receive a context that collectors
/// consume without creating another audit row.
pub async fn prepare_crawl_run(
    pool: &AnyPool,
    backend: DatabaseBackend,
    watch_target: &WatchTarget,
    timezone_offset: UtcOffset,
) -> Result<RunPreparation, sqlx::Error> {
    if watch_target.lease_trigger_kind.as_deref() == Some("scheduled")
        && current_window(
            watch_target.no_fetch_periods.as_ref(),
            OffsetDateTime::now_utc(),
            timezone_offset,
        )
        .is_some()
        && let Some(skipped) =
            skip_no_fetch_period(pool, backend, watch_target, timezone_offset).await?
    {
        return Ok(RunPreparation::Skipped(skipped));
    }
    Ok(RunPreparation::Collect(
        begin_crawl_run(pool, backend, watch_target).await?,
    ))
}

pub async fn begin_crawl_run(
    pool: &AnyPool,
    backend: DatabaseBackend,
    watch_target: &WatchTarget,
) -> Result<CrawlRunContext, sqlx::Error> {
    let renewed_lease = lease_expression(backend);
    let trigger_kind = valid_trigger_kind(watch_target.lease_trigger_kind.as_deref())
        .unwrap_or("unknown")
        .to_owned();
    let baseline = !watch_target.baseline_completed;
    let sync_mode = match watch_target.target_type.as_str() {
        "thread" => match (baseline, watch_target.history_mode.as_deref()) {
            (true, Some("incremental")) => "tid_incremental_baseline",
            (true, _) => "tid_full_baseline",
            (false, _) => "incremental",
        },
        "user" => {
            if baseline {
                "uid_baseline"
            } else {
                "incremental"
            }
        }
        _ => return Err(sqlx::Error::RowNotFound),
    };
    let run_id = Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;
    let owned = sqlx::query(&format!(
        "UPDATE watch_targets SET status = 'running', lease_until = {renewed_lease},
         updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND lease_token = $2 AND status = 'running'
           AND lease_until > CURRENT_TIMESTAMP AND deleted_at IS NULL"
    ))
    .bind(&watch_target.id)
    .bind(watch_target.lease_token.as_deref().unwrap_or_default())
    .execute(&mut *tx)
    .await?;
    if owned.rows_affected() != 1 {
        tx.rollback().await?;
        return Err(sqlx::Error::RowNotFound);
    }
    sqlx::query(
        "UPDATE crawl_runs SET status = 'failed', error_kind = 'lease_expired',
         error_message = 'lease_expired', completed_at = CURRENT_TIMESTAMP
         WHERE watch_id = $1 AND status = 'running'",
    )
    .bind(&watch_target.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO crawl_runs (id, watch_id, status, baseline, sync_mode, trigger_kind)
         VALUES ($1, $2, 'running', $3, $4, $5)",
    )
    .bind(&run_id)
    .bind(&watch_target.id)
    .bind(i32::from(baseline))
    .bind(sync_mode)
    .bind(&trigger_kind)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(CrawlRunContext {
        crawl_run_id: run_id,
        trigger_kind,
    })
}

/// Re-evaluate an enabled, idle watch after a configuration change. This is
/// intentionally a claim-and-skip operation so an API request cannot create
/// a second audit row concurrently with the regular worker.
pub async fn reevaluate_no_fetch_period(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    timezone_offset: UtcOffset,
) -> Result<Option<NoFetchSkip>, sqlx::Error> {
    let Some(target) = find(pool, id).await? else {
        return Ok(None);
    };
    if !target.enabled
        || target.status == "running"
        || target.pending_trigger_kind.is_some()
        || current_window(
            target.no_fetch_periods.as_ref(),
            OffsetDateTime::now_utc(),
            timezone_offset,
        )
        .is_none()
    {
        return Ok(None);
    }
    let Some(claimed) = claim_by_id_with_trigger(pool, backend, id, "scheduled").await? else {
        return Ok(None);
    };
    skip_no_fetch_period(pool, backend, &claimed, timezone_offset).await
}

/// Keep a manual run inside a no-fetch window from either duplicating an
/// existing automatic audit row or postponing that audit until an ordinary
/// interval. If this window has not been audited yet, return a short delay so
/// the next automatic evaluation records it.
pub async fn adjust_next_delay_for_manual(
    tx: &mut Transaction<'_, Any>,
    backend: DatabaseBackend,
    watch_target: &WatchTarget,
    timezone_offset: UtcOffset,
    base_delay: i64,
) -> Result<i64, sqlx::Error> {
    if watch_target.lease_trigger_kind.as_deref() != Some("manual") {
        return Ok(base_delay);
    }
    let now = OffsetDateTime::now_utc();
    let Some(window) = current_window(watch_target.no_fetch_periods.as_ref(), now, timezone_offset)
    else {
        return Ok(base_delay);
    };
    if !matches!(watch_target.target_type.as_str(), "thread" | "user") {
        return Ok(base_delay);
    }
    let (start, end, start_expression, end_expression) = match backend {
        DatabaseBackend::Postgres => (
            format_postgres_timestamp(window.start),
            format_postgres_timestamp(window.until),
            "CAST($2 AS TIMESTAMPTZ)",
            "CAST($3 AS TIMESTAMPTZ)",
        ),
        DatabaseBackend::Sqlite => (
            format_database_timestamp(window.start),
            format_database_timestamp(window.until),
            "$2",
            "$3",
        ),
    };
    let query = format!(
        "SELECT COUNT(*) FROM crawl_runs
         WHERE watch_id = $1 AND status = 'skipped' AND error_kind = 'no_fetch_period'
           AND started_at >= {start_expression} AND started_at < {end_expression}"
    );
    let count: i64 = sqlx::query_scalar(&query)
        .bind(&watch_target.id)
        .bind(&start)
        .bind(&end)
        .fetch_one(&mut **tx)
        .await?;
    if count > 0 {
        let remaining = window.until - now;
        let seconds = remaining.whole_seconds() + i64::from(remaining.subsec_nanoseconds() > 0);
        Ok(seconds.max(1))
    } else {
        Ok(1)
    }
}

/// Read-only wrapper for failure paths that update the watch outside the
/// collector's normal completion transaction.
pub async fn adjust_next_delay_for_manual_pool(
    pool: &AnyPool,
    backend: DatabaseBackend,
    watch_target: &WatchTarget,
    timezone_offset: UtcOffset,
    base_delay: i64,
) -> Result<i64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let delay =
        adjust_next_delay_for_manual(&mut tx, backend, watch_target, timezone_offset, base_delay)
            .await?;
    tx.rollback().await?;
    Ok(delay)
}

pub async fn request_manual_run(pool: &AnyPool, id: &str) -> Result<ManualRunRequest, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let requested = sqlx::query(
        "UPDATE watch_targets SET pending_trigger_kind = 'manual',
         next_run_at = CURRENT_TIMESTAMP,
         status = CASE WHEN status = 'running' THEN status ELSE 'pending' END,
         updated_at = CURRENT_TIMESTAMP
         WHERE id = $1 AND enabled = 1 AND deleted_at IS NULL
           AND status NOT IN ('paused', 'not_found')
           AND pending_trigger_kind IS NULL
           AND (status <> 'running' OR lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if requested {
        tx.commit().await?;
        return Ok(ManualRunRequest::Requested);
    }

    let running: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM watch_targets
         WHERE id = $1 AND status = 'running'
           AND lease_until IS NOT NULL AND lease_until > CURRENT_TIMESTAMP
           AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if running > 0 {
        tx.rollback().await?;
        return Ok(ManualRunRequest::AlreadyRunning);
    }
    let queued: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM watch_targets
         WHERE id = $1 AND pending_trigger_kind = 'manual' AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.rollback().await?;
    if queued > 0 {
        Ok(ManualRunRequest::AlreadyRequested)
    } else {
        Ok(ManualRunRequest::NotRunnable)
    }
}

pub async fn manual_run_conflict(pool: &AnyPool, id: &str) -> Result<&'static str, sqlx::Error> {
    let Some(row) = sqlx::query(
        "SELECT enabled, CAST(deleted_at AS TEXT) AS deleted_at, status,
                CAST(lease_until AS TEXT) AS lease_until, pending_trigger_kind
         FROM watch_targets WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok("watch_not_found");
    };
    let enabled: i32 = row.get("enabled");
    let deleted_at: Option<String> = row.get("deleted_at");
    if enabled != 1 || deleted_at.is_some() {
        return Ok("watch_not_runnable");
    }
    let status: String = row.get("status");
    let lease_until: Option<String> = row.get("lease_until");
    if status == "running" && lease_until.is_some() {
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM watch_targets
             WHERE id = $1 AND lease_until > CURRENT_TIMESTAMP",
        )
        .bind(id)
        .fetch_one(pool)
        .await?;
        if active > 0 {
            return Ok("watch_already_running");
        }
    }
    let pending_trigger_kind: Option<String> = row.get("pending_trigger_kind");
    if pending_trigger_kind.as_deref() == Some("manual") {
        return Ok("watch_run_already_requested");
    }
    Ok("watch_not_runnable")
}

/// Ask the regular worker to run a watch as soon as possible without doing
/// collector I/O in the caller (notably the single bot inbound consumer).
#[allow(dead_code)]
pub async fn request_run(pool: &AnyPool, id: &str) -> Result<bool, sqlx::Error> {
    Ok(matches!(
        request_manual_run(pool, id).await?,
        ManualRunRequest::Requested
    ))
}

async fn claim(
    pool: &AnyPool,
    backend: DatabaseBackend,
    id: Option<&str>,
    explicit_trigger_kind: Option<&str>,
) -> Result<Option<WatchTarget>, sqlx::Error> {
    let lease_expression = lease_expression(backend);
    let lease_token = Uuid::new_v4().to_string();
    let query = if explicit_trigger_kind.is_some() {
        debug_assert!(id.is_some());
        format!(
            "UPDATE watch_targets SET status = 'running', lease_until = {lease_expression},
             lease_token = $2, lease_trigger_kind = $3, pending_trigger_kind = NULL,
             last_started_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND enabled = 1 AND deleted_at IS NULL
               AND status NOT IN ('paused', 'not_found')
               AND pending_trigger_kind IS NULL
               AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
             RETURNING id"
        )
    } else if id.is_some() {
        format!(
            "UPDATE watch_targets SET status = 'running', lease_until = {lease_expression},
             lease_token = $2, lease_trigger_kind = COALESCE(pending_trigger_kind, 'scheduled'),
             pending_trigger_kind = NULL,
             last_started_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1 AND enabled = 1 AND deleted_at IS NULL
               AND status NOT IN ('paused', 'not_found')
               AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
             RETURNING id"
        )
    } else {
        format!(
            "UPDATE watch_targets SET status = 'running', lease_until = {lease_expression},
             lease_token = $1, lease_trigger_kind = COALESCE(pending_trigger_kind, 'scheduled'),
             pending_trigger_kind = NULL,
             last_started_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = (
                 SELECT id FROM watch_targets
                 WHERE enabled = 1 AND deleted_at IS NULL
                   AND next_run_at <= CURRENT_TIMESTAMP
                   AND status NOT IN ('paused', 'not_found')
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
        if explicit_trigger_kind.is_some() {
            query = query.bind(explicit_trigger_kind);
        }
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

/// Reload only the scheduling fields that may be edited while a collector is
/// running. The collector keeps its original target snapshot for acquisition,
/// while completion uses the persisted configuration for the next run.
pub async fn refresh_run_scheduling(
    tx: &mut Transaction<'_, Any>,
    watch_target: &WatchTarget,
) -> Result<WatchTarget, sqlx::Error> {
    let row = sqlx::query(
        "SELECT interval_seconds, schedule_json, no_fetch_periods_json,
                lease_trigger_kind
         FROM watch_targets
         WHERE id = $1 AND lease_token = $2 AND status = 'running'
           AND deleted_at IS NULL",
    )
    .bind(&watch_target.id)
    .bind(watch_target.lease_token.as_deref().unwrap_or_default())
    .fetch_one(&mut **tx)
    .await?;
    let schedule_json: Option<String> = row.get("schedule_json");
    let no_fetch_periods_json: Option<String> = row.get("no_fetch_periods_json");
    let mut refreshed = watch_target.clone();
    refreshed.interval_seconds = row.get("interval_seconds");
    refreshed.schedule = schedule_json.and_then(|value| serde_json::from_str(&value).ok());
    refreshed.no_fetch_periods =
        no_fetch_periods_json.and_then(|value| parse_no_fetch_json(&value));
    refreshed.lease_trigger_kind = row.get("lease_trigger_kind");
    Ok(refreshed)
}

fn lease_expression(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '5 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+5 minutes')",
    }
}

fn map_watch(row: &sqlx::any::AnyRow) -> WatchTarget {
    let schedule_json: Option<String> = row.get("schedule_json");
    let no_fetch_periods_json: Option<String> = row.get("no_fetch_periods_json");
    WatchTarget {
        id: row.get("id"),
        lease_token: row.get("lease_token"),
        target_type: row.get("target_type"),
        target_id: row.get("target_id"),
        target_name: row.get("target_name"),
        enabled: row.get::<i32, _>("enabled") == 1,
        interval_seconds: row.get("interval_seconds"),
        schedule: schedule_json.and_then(|value| serde_json::from_str(&value).ok()),
        no_fetch_periods: no_fetch_periods_json.and_then(|value| parse_no_fetch_json(&value)),
        status: row.get("status"),
        baseline_completed: row.get::<i32, _>("baseline_completed") == 1,
        next_run_at: row.get("next_run_at"),
        last_completed_at: row.get("last_completed_at"),
        last_error_kind: row.get("last_error_kind"),
        pending_trigger_kind: row.get("pending_trigger_kind"),
        lease_trigger_kind: row.get("lease_trigger_kind"),
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

fn no_fetch_json(periods: Option<&NoFetchPeriods>) -> Option<String> {
    periods.filter(|items| !items.is_empty()).map(|items| {
        serde_json::to_string(&StoredNoFetchPeriods {
            version: 1,
            periods: items,
        })
        .expect("no-fetch periods must serialize")
    })
}

#[derive(serde::Serialize)]
struct StoredNoFetchPeriods<'a> {
    version: u32,
    periods: &'a NoFetchPeriods,
}

fn parse_no_fetch_json(value: &str) -> Option<NoFetchPeriods> {
    if let Ok(stored) = serde_json::from_str::<serde_json::Value>(value)
        && stored.get("version").and_then(serde_json::Value::as_u64) == Some(1)
    {
        return serde_json::from_value(stored.get("periods")?.clone()).ok();
    }
    // Be liberal when reading a hand-edited pre-release value; all new
    // writes use the versioned representation above.
    serde_json::from_str(value).ok()
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
        CreateWatchError, ManualRunRequest, ResetWatchError, RunPreparation, claim_by_id,
        claim_due, create_thread_watch, create_thread_watch_with_no_fetch_config,
        create_user_watch_with_config, delete, list, prepare_crawl_run, renew_lease,
        request_manual_run, reset, skip_no_fetch_period, update,
    };
    use crate::config::DatabaseBackend;
    use crate::no_fetch::NoFetchPeriod;

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
        let paused_run: (String, String, String) = sqlx::query_as(
            "SELECT status, error_kind, trigger_kind FROM crawl_runs WHERE id = 'paused-run'",
        )
        .fetch_one(&pool)
        .await
        .expect("invalidated crawl run must query");
        assert_eq!(
            paused_run,
            ("failed".into(), "watch_paused".into(), "unknown".into())
        );

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

    #[tokio::test]
    async fn no_fetch_skip_is_a_zero_io_scheduled_run_and_manual_requests_survive_claiming() {
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

        let day = match time::OffsetDateTime::now_utc().weekday() {
            time::Weekday::Monday => "monday",
            time::Weekday::Tuesday => "tuesday",
            time::Weekday::Wednesday => "wednesday",
            time::Weekday::Thursday => "thursday",
            time::Weekday::Friday => "friday",
            time::Weekday::Saturday => "saturday",
            time::Weekday::Sunday => "sunday",
        };
        let periods = vec![NoFetchPeriod {
            days: vec![day.to_owned()],
            description: Some("test".to_owned()),
            end_time: "24:00".to_owned(),
            start_time: "00:00".to_owned(),
        }];
        let scheduled = create_thread_watch_with_no_fetch_config(
            &pool,
            5001,
            60,
            None,
            Some(&periods),
            "full",
            false,
            2,
            &[],
            &[],
        )
        .await
        .expect("scheduled watch must create");
        let claimed = claim_due(&pool, DatabaseBackend::Sqlite)
            .await
            .expect("scheduled watch must claim")
            .expect("scheduled watch must be due");
        assert_eq!(claimed.lease_trigger_kind.as_deref(), Some("scheduled"));
        let skipped = skip_no_fetch_period(
            &pool,
            DatabaseBackend::Sqlite,
            &claimed,
            time::UtcOffset::UTC,
        )
        .await
        .expect("no-fetch skip must succeed")
        .expect("scheduled watch must be skipped");
        assert!(skipped.baseline);
        let run: (String, String, String, i32, i32, i32, i32, i32) = sqlx::query_as(
            "SELECT status, error_kind, trigger_kind, pages_requested,
                    posts_inserted, events_created, matches_created, outbox_enqueued
             FROM crawl_runs WHERE id = $1",
        )
        .bind(&skipped.crawl_run_id)
        .fetch_one(&pool)
        .await
        .expect("skip run must be queryable");
        assert_eq!(
            run,
            (
                "skipped".to_owned(),
                "no_fetch_period".to_owned(),
                "scheduled".to_owned(),
                0,
                0,
                0,
                0,
                0
            )
        );
        let scheduled_after = list(&pool)
            .await
            .expect("watch list must load")
            .into_iter()
            .find(|watch| watch.id == scheduled.id)
            .expect("scheduled watch must remain active");
        assert_eq!(scheduled_after.status, "active");
        assert!(scheduled_after.lease_token.is_none());

        let manual = create_thread_watch_with_no_fetch_config(
            &pool,
            5002,
            60,
            None,
            Some(&periods),
            "full",
            false,
            2,
            &[],
            &[],
        )
        .await
        .expect("manual watch must create");
        assert_eq!(
            request_manual_run(&pool, &manual.id)
                .await
                .expect("manual request must persist"),
            ManualRunRequest::Requested
        );
        assert_eq!(
            request_manual_run(&pool, &manual.id)
                .await
                .expect("duplicate manual request must query"),
            ManualRunRequest::AlreadyRequested
        );
        let claimed_manual = claim_due(&pool, DatabaseBackend::Sqlite)
            .await
            .expect("manual request must claim")
            .expect("manual request must be due");
        assert_eq!(claimed_manual.id, manual.id);
        assert_eq!(claimed_manual.lease_trigger_kind.as_deref(), Some("manual"));
        assert!(
            skip_no_fetch_period(
                &pool,
                DatabaseBackend::Sqlite,
                &claimed_manual,
                time::UtcOffset::UTC,
            )
            .await
            .expect("manual no-fetch evaluation must query")
            .is_none()
        );
        let context = match prepare_crawl_run(
            &pool,
            DatabaseBackend::Sqlite,
            &claimed_manual,
            time::UtcOffset::UTC,
        )
        .await
        .expect("manual run preparation must succeed")
        {
            RunPreparation::Collect(context) => context,
            RunPreparation::Skipped(_) => panic!("manual run must not be skipped"),
        };
        let trigger: String =
            sqlx::query_scalar("SELECT trigger_kind FROM crawl_runs WHERE id = $1")
                .bind(&context.crawl_run_id)
                .fetch_one(&pool)
                .await
                .expect("manual run context must have an audit row");
        assert_eq!(trigger, "manual");
    }
}
