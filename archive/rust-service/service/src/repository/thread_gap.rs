use sqlx::{Any, AnyPool, Row, Transaction};

use crate::config::DatabaseBackend;

pub const FIRST_RETRY_MINUTES: i32 = 5;
pub const RETRY_WINDOW_MINUTES: i32 = 120;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadFloorGap {
    pub floor_number: i32,
    pub page_hint: i32,
    pub retry_count: i32,
    pub due: bool,
}

pub async fn list_open(pool: &AnyPool, watch_id: &str) -> Result<Vec<ThreadFloorGap>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT floor_number, page_hint, retry_count,
                CASE WHEN status = 'pending' AND next_retry_at <= CURRENT_TIMESTAMP
                     THEN 1 ELSE 0 END AS due
         FROM thread_floor_gaps
         WHERE watch_id = $1 AND status IN ('pending', 'expired')
         ORDER BY floor_number",
    )
    .bind(watch_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| ThreadFloorGap {
            floor_number: row.get("floor_number"),
            page_hint: row.get("page_hint"),
            retry_count: row.get("retry_count"),
            due: row.get::<i32, _>("due") == 1,
        })
        .collect())
}

pub async fn insert_pending(
    tx: &mut Transaction<'_, Any>,
    backend: DatabaseBackend,
    watch_id: &str,
    floor_number: i32,
    page_hint: i32,
) -> Result<bool, sqlx::Error> {
    let (next_retry, expires) = match backend {
        DatabaseBackend::Postgres => (
            format!("CURRENT_TIMESTAMP + INTERVAL '{FIRST_RETRY_MINUTES} minutes'"),
            format!("CURRENT_TIMESTAMP + INTERVAL '{RETRY_WINDOW_MINUTES} minutes'"),
        ),
        DatabaseBackend::Sqlite => (
            format!("datetime(CURRENT_TIMESTAMP, '+{FIRST_RETRY_MINUTES} minutes')"),
            format!("datetime(CURRENT_TIMESTAMP, '+{RETRY_WINDOW_MINUTES} minutes')"),
        ),
    };
    let query = format!(
        "INSERT INTO thread_floor_gaps
            (watch_id, floor_number, page_hint, next_retry_at, expires_at)
         VALUES ($1, $2, $3, {next_retry}, {expires})
         ON CONFLICT (watch_id, floor_number) DO NOTHING"
    );
    Ok(sqlx::query(&query)
        .bind(watch_id)
        .bind(floor_number)
        .bind(page_hint)
        .execute(&mut **tx)
        .await?
        .rows_affected()
        == 1)
}

pub async fn mark_resolved(
    tx: &mut Transaction<'_, Any>,
    watch_id: &str,
    floor_number: i32,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE thread_floor_gaps
         SET status = 'resolved', resolved_at = CURRENT_TIMESTAMP
         WHERE watch_id = $1 AND floor_number = $2 AND status <> 'resolved'",
    )
    .bind(watch_id)
    .bind(floor_number)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1)
}

pub async fn record_missed_attempt(
    tx: &mut Transaction<'_, Any>,
    backend: DatabaseBackend,
    watch_id: &str,
    gap: &ThreadFloorGap,
) -> Result<(), sqlx::Error> {
    let delay_minutes = next_retry_delay_minutes(gap.retry_count);
    let next_retry = match backend {
        DatabaseBackend::Postgres => {
            "LEAST(expires_at, CURRENT_TIMESTAMP + ($3 * INTERVAL '1 minute'))"
        }
        DatabaseBackend::Sqlite => {
            "MIN(expires_at, datetime(CURRENT_TIMESTAMP, '+' || $3 || ' minutes'))"
        }
    };
    let query = format!(
        "UPDATE thread_floor_gaps
         SET retry_count = retry_count + 1,
             last_attempt_at = CURRENT_TIMESTAMP,
             status = CASE
                 WHEN retry_count >= 5 OR CURRENT_TIMESTAMP >= expires_at
                 THEN 'expired' ELSE 'pending' END,
             next_retry_at = {next_retry}
         WHERE watch_id = $1 AND floor_number = $2 AND status = 'pending'"
    );
    sqlx::query(&query)
        .bind(watch_id)
        .bind(gap.floor_number)
        .bind(delay_minutes)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn schedule_earliest_pending(
    tx: &mut Transaction<'_, Any>,
    watch_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE watch_targets
         SET next_run_at = (
             SELECT MIN(next_retry_at) FROM thread_floor_gaps
             WHERE watch_id = $1 AND status = 'pending'
         )
         WHERE id = $1
           AND EXISTS (
               SELECT 1 FROM thread_floor_gaps
               WHERE watch_id = $1 AND status = 'pending'
           )
           AND next_run_at > (
               SELECT MIN(next_retry_at) FROM thread_floor_gaps
               WHERE watch_id = $1 AND status = 'pending'
           )",
    )
    .bind(watch_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn schedule_earliest_pending_pool(
    pool: &AnyPool,
    watch_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE watch_targets
         SET next_run_at = (
             SELECT MIN(next_retry_at) FROM thread_floor_gaps
             WHERE watch_id = $1 AND status = 'pending'
         )
         WHERE id = $1
           AND EXISTS (
               SELECT 1 FROM thread_floor_gaps
               WHERE watch_id = $1 AND status = 'pending'
           )
           AND next_run_at > (
               SELECT MIN(next_retry_at) FROM thread_floor_gaps
               WHERE watch_id = $1 AND status = 'pending'
           )",
    )
    .bind(watch_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn clear(tx: &mut Transaction<'_, Any>, watch_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM thread_floor_gaps WHERE watch_id = $1")
        .bind(watch_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn next_retry_delay_minutes(retry_count: i32) -> i32 {
    match retry_count {
        i32::MIN..=0 => 10,
        1 => 20,
        _ => 30,
    }
}

#[cfg(test)]
mod tests {
    use sqlx::any::AnyPoolOptions;

    use super::{
        insert_pending, list_open, next_retry_delay_minutes, record_missed_attempt,
        schedule_earliest_pending,
    };
    use crate::config::DatabaseBackend;

    #[test]
    fn retry_delays_back_off_and_cap_at_thirty_minutes() {
        assert_eq!(next_retry_delay_minutes(0), 10);
        assert_eq!(next_retry_delay_minutes(1), 20);
        assert_eq!(next_retry_delay_minutes(2), 30);
        assert_eq!(next_retry_delay_minutes(5), 30);
    }

    #[tokio::test]
    async fn sixth_missed_attempt_expires_the_gap() {
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
        sqlx::query(
            "INSERT INTO watch_targets (id, target_type, target_id, next_run_at)
             VALUES ('watch', 'thread', 1001, '2099-01-01 00:00:00')",
        )
        .execute(&pool)
        .await
        .expect("watch must insert");
        let mut tx = pool.begin().await.expect("transaction must begin");
        assert!(
            insert_pending(&mut tx, DatabaseBackend::Sqlite, "watch", 3, 1)
                .await
                .expect("gap must insert")
        );
        tx.commit().await.expect("transaction must commit");
        let original: (String, i32) = sqlx::query_as(
            "SELECT first_detected_at, page_hint FROM thread_floor_gaps
             WHERE watch_id = 'watch' AND floor_number = 3",
        )
        .fetch_one(&pool)
        .await
        .expect("gap must query");
        let mut tx = pool.begin().await.expect("transaction must begin");
        assert!(
            !insert_pending(&mut tx, DatabaseBackend::Sqlite, "watch", 3, 9)
                .await
                .expect("duplicate gap insert must query")
        );
        schedule_earliest_pending(&mut tx, "watch")
            .await
            .expect("gap must affect scheduling");
        tx.commit().await.expect("transaction must commit");
        let unchanged: (String, i32) = sqlx::query_as(
            "SELECT first_detected_at, page_hint FROM thread_floor_gaps
             WHERE watch_id = 'watch' AND floor_number = 3",
        )
        .fetch_one(&pool)
        .await
        .expect("gap must query");
        assert_eq!(unchanged, original);
        let schedule_matches: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM watch_targets w
             JOIN thread_floor_gaps g ON g.watch_id = w.id
             WHERE w.id = 'watch' AND w.next_run_at = g.next_retry_at",
        )
        .fetch_one(&pool)
        .await
        .expect("schedule must query");
        assert_eq!(schedule_matches, 1);

        for expected_count in 1..=6 {
            sqlx::query(
                "UPDATE thread_floor_gaps SET next_retry_at = CURRENT_TIMESTAMP
                 WHERE watch_id = 'watch' AND floor_number = 3",
            )
            .execute(&pool)
            .await
            .expect("gap must become due");
            let gap = list_open(&pool, "watch")
                .await
                .expect("gaps must list")
                .into_iter()
                .next()
                .expect("gap must exist");
            assert!(gap.due);
            let mut tx = pool.begin().await.expect("transaction must begin");
            record_missed_attempt(&mut tx, DatabaseBackend::Sqlite, "watch", &gap)
                .await
                .expect("attempt must record");
            tx.commit().await.expect("transaction must commit");

            let state: (String, i32) = sqlx::query_as(
                "SELECT status, retry_count FROM thread_floor_gaps
                 WHERE watch_id = 'watch' AND floor_number = 3",
            )
            .fetch_one(&pool)
            .await
            .expect("gap state must query");
            assert_eq!(state.1, expected_count);
            assert_eq!(
                state.0,
                if expected_count == 6 {
                    "expired"
                } else {
                    "pending"
                }
            );
        }
    }
}
