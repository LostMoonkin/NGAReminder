use sqlx::Row;
use uuid::Uuid;

use crate::{
    app::AppState,
    config::DatabaseBackend,
    notification::sender::{Notification, SendError, send_configured},
};

pub async fn process_one(state: &AppState) -> anyhow::Result<bool> {
    let lease = match state.config.database_backend {
        DatabaseBackend::Postgres => "CURRENT_TIMESTAMP + INTERVAL '2 minutes'",
        DatabaseBackend::Sqlite => "datetime(CURRENT_TIMESTAMP, '+2 minutes')",
    };
    let claim = format!(
        "UPDATE notification_outbox SET status = 'sending',
         attempt_count = attempt_count + 1, lease_until = {lease}
         WHERE id = (
           SELECT id FROM notification_outbox
           WHERE status IN ('pending', 'failed')
             AND next_attempt_at <= CURRENT_TIMESTAMP
             AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
           ORDER BY next_attempt_at, created_at LIMIT 1
         )
         AND (lease_until IS NULL OR lease_until <= CURRENT_TIMESTAMP)
         RETURNING id"
    );
    let Some(row) = sqlx::query(&claim).fetch_optional(&state.pool).await? else {
        return Ok(false);
    };
    let outbox_id: String = row.get("id");
    let row = sqlx::query(
        "SELECT o.attempt_count, c.channel_type, c.config_encrypted,
         p.tid, p.pid, p.floor_number, p.page_number, p.author_name, p.content_raw, t.title
         FROM notification_outbox o
         JOIN notification_channels c ON c.id = o.channel_id
         JOIN post_events e ON e.id = o.post_event_id
         JOIN posts p ON p.id = e.post_id
         JOIN threads t ON t.tid = p.tid
         WHERE o.id = $1",
    )
    .bind(&outbox_id)
    .fetch_one(&state.pool)
    .await?;
    let attempt: i32 = row.get("attempt_count");
    let encrypted: Vec<u8> = row.get("config_encrypted");
    let config = state
        .credential_cipher
        .decrypt(&encrypted)
        .map_err(|_| anyhow::anyhow!("notification config decryption failed"))?;
    let tid: i64 = row.get("tid");
    let pid: Option<i64> = row.get("pid");
    let page: i32 = row.get("page_number");
    let author: String = row.get("author_name");
    let content: String = row.get("content_raw");
    let title: String = row.get("title");
    let floor: Option<i32> = row.get("floor_number");
    let notification = Notification {
        title,
        body: format!("{} (#{}): {}", author, floor.unwrap_or_default(), content),
        url: post_url(tid, page, pid),
    };
    let channel_type: String = row.get("channel_type");
    match send_configured(&channel_type, &config, &notification).await {
        Ok(receipt) => {
            record_delivery(
                state,
                &outbox_id,
                attempt,
                true,
                Some(receipt.http_status as i32),
                Some(&receipt.response_summary),
                None,
            )
            .await?;
            sqlx::query(
                "UPDATE notification_outbox SET status = 'delivered', lease_until = NULL,
                 delivered_at = CURRENT_TIMESTAMP, last_error_kind = NULL WHERE id = $1",
            )
            .bind(&outbox_id)
            .execute(&state.pool)
            .await?;
        }
        Err(error) => {
            let retryable = error.retryable() && attempt < 5;
            let (status, summary) = error_details(&error);
            record_delivery(
                state,
                &outbox_id,
                attempt,
                false,
                status,
                summary,
                Some(error.kind()),
            )
            .await?;
            let next = match state.config.database_backend {
                DatabaseBackend::Postgres => {
                    "CURRENT_TIMESTAMP + (CASE attempt_count WHEN 1 THEN 30 WHEN 2 THEN 120 WHEN 3 THEN 600 ELSE 1800 END * INTERVAL '1 second')"
                }
                DatabaseBackend::Sqlite => {
                    "datetime(CURRENT_TIMESTAMP, '+' || CASE attempt_count WHEN 1 THEN 30 WHEN 2 THEN 120 WHEN 3 THEN 600 ELSE 1800 END || ' seconds')"
                }
            };
            let query = format!(
                "UPDATE notification_outbox SET status = $1, lease_until = NULL,
                 next_attempt_at = {next}, last_error_kind = $2 WHERE id = $3"
            );
            sqlx::query(&query)
                .bind(if retryable { "failed" } else { "dead" })
                .bind(error.kind())
                .bind(&outbox_id)
                .execute(&state.pool)
                .await?;
        }
    }
    Ok(true)
}

async fn record_delivery(
    state: &AppState,
    outbox_id: &str,
    attempt: i32,
    success: bool,
    http_status: Option<i32>,
    summary: Option<&str>,
    error_kind: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO notification_deliveries
         (id, outbox_id, attempt, success, http_status, response_summary, error_kind)
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(outbox_id)
    .bind(attempt)
    .bind(i32::from(success))
    .bind(http_status)
    .bind(summary)
    .bind(error_kind)
    .execute(&state.pool)
    .await?;
    Ok(())
}

fn error_details(error: &SendError) -> (Option<i32>, Option<&str>) {
    match error {
        SendError::Http { status, summary } => (Some(status.as_u16() as i32), Some(summary)),
        SendError::Api { summary, .. } => (None, Some(summary)),
        _ => (None, None),
    }
}

fn post_url(tid: i64, page: i32, pid: Option<i64>) -> String {
    match pid {
        Some(pid) => {
            format!(
                "https://bbs.nga.cn/read.php?tid={tid}&page={}#pid{pid}Anchor",
                page.max(1)
            )
        }
        None => format!("https://bbs.nga.cn/read.php?tid={tid}"),
    }
}

#[cfg(test)]
mod tests {
    use super::post_url;

    #[test]
    fn reply_url_opens_thread_page_at_post_anchor() {
        assert_eq!(
            post_url(47_264_819, 3, Some(876_581_704)),
            "https://bbs.nga.cn/read.php?tid=47264819&page=3#pid876581704Anchor"
        );
    }

    #[test]
    fn topic_url_opens_thread_without_reply_anchor() {
        assert_eq!(
            post_url(47_264_819, 1, None),
            "https://bbs.nga.cn/read.php?tid=47264819"
        );
    }
}
