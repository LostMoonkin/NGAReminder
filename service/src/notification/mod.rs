pub mod alerts;
pub mod receiver;
pub mod sender;
pub mod worker;

use sqlx::{Any, Row, Transaction};
use uuid::Uuid;

use crate::domain::thread::ParsedPost;

#[derive(Clone, Copy, Debug, Default)]
pub struct MatchStats {
    pub matches_created: i32,
    pub outbox_enqueued: i32,
}

pub async fn enqueue_matches(
    tx: &mut Transaction<'_, Any>,
    event_id: &str,
    post: &ParsedPost,
    watch_id: &str,
) -> Result<MatchStats, sqlx::Error> {
    let matched = sqlx::query(
        "INSERT INTO post_event_watch_matches (post_event_id, watch_id)
         VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(event_id)
    .bind(watch_id)
    .execute(&mut **tx)
    .await?;

    let watch = sqlx::query(
        "SELECT target_type, target_id FROM watch_targets
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(watch_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(watch) = watch else {
        return Ok(MatchStats {
            matches_created: i32::from(matched.rows_affected() == 1),
            outbox_enqueued: 0,
        });
    };
    let target_type: String = watch.get("target_type");
    let target_id: i64 = watch.get("target_id");
    let content_matches = if target_type == "user" {
        target_id == post.author_uid
    } else if target_type == "thread" && target_id == post.tid {
        let author_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM watch_notification_authors WHERE watch_id = $1",
        )
        .bind(watch_id)
        .fetch_one(&mut **tx)
        .await?;
        author_count == 0
            || sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM watch_notification_authors
                 WHERE watch_id = $1 AND author_uid = $2",
            )
            .bind(watch_id)
            .bind(post.author_uid)
            .fetch_one(&mut **tx)
            .await?
                == 1
    } else {
        false
    };
    if !content_matches {
        return Ok(MatchStats {
            matches_created: i32::from(matched.rows_affected() == 1),
            outbox_enqueued: 0,
        });
    }

    let channels = sqlx::query(
        "SELECT wc.channel_id FROM watch_notification_channels wc
         JOIN notification_channels c ON c.id = wc.channel_id
         WHERE wc.watch_id = $1 AND c.enabled = 1",
    )
    .bind(watch_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut outbox_enqueued = 0;
    for channel in channels {
        let result = sqlx::query(
            "INSERT INTO notification_outbox (id, post_event_id, channel_id)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(event_id)
        .bind(channel.get::<String, _>("channel_id"))
        .execute(&mut **tx)
        .await?;
        outbox_enqueued += i32::from(result.rows_affected() == 1);
    }
    Ok(MatchStats {
        matches_created: i32::from(matched.rows_affected() == 1),
        outbox_enqueued,
    })
}

#[cfg(test)]
mod tests {
    use sqlx::any::AnyPoolOptions;

    use super::enqueue_matches;
    use crate::nga::thread_parser::parse_thread_page;

    #[tokio::test]
    async fn thread_and_user_watch_share_channel_outbox() {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("database must connect");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("foreign keys must enable");
        sqlx::migrate!("./migrations/sqlite")
            .run(&pool)
            .await
            .expect("migrations must run");
        sqlx::query(
            "INSERT INTO threads
             (tid, fid, title, forum_name, author_uid, author_name)
             VALUES (1001, 3001, 'title', 'forum', 2001, 'author')",
        )
        .execute(&pool)
        .await
        .expect("thread must insert");
        sqlx::query(
            "INSERT INTO posts
             (id, tid, pid, floor_number, post_kind, author_uid, author_name,
              content_raw, page_number, raw_payload)
             VALUES ('post', 1001, 4001, 1, 'reply', 2002, 'reply author', 'body', 1, '')",
        )
        .execute(&pool)
        .await
        .expect("post must insert");
        sqlx::query(
            "INSERT INTO post_events (id, post_id, event_type)
             VALUES ('event', 'post', 'new_reply')",
        )
        .execute(&pool)
        .await
        .expect("event must insert");
        sqlx::query(
            "INSERT INTO notification_channels
             (id, channel_type, label, config_encrypted)
             VALUES ('channel', 'bark', 'test', X'00')",
        )
        .execute(&pool)
        .await
        .expect("channel must insert");
        for (id, target_type, target_id) in [
            ("watch-thread", "thread", 1001_i64),
            ("watch-user", "user", 2002_i64),
        ] {
            sqlx::query(
                "INSERT INTO watch_targets (id, target_type, target_id)
                 VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(target_type)
            .bind(target_id)
            .execute(&pool)
            .await
            .expect("watch must insert");
            sqlx::query(
                "INSERT INTO watch_notification_channels (watch_id, channel_id)
                 VALUES ($1, 'channel')",
            )
            .bind(id)
            .execute(&pool)
            .await
            .expect("watch channel must insert");
        }
        sqlx::query(
            "INSERT INTO watch_notification_authors (watch_id, author_uid)
             VALUES ('watch-thread', 9999)",
        )
        .execute(&pool)
        .await
        .expect("thread author filter must insert");
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/nga/thread_page_success.json"),
            )
            .expect("fixture must read"),
        )
        .expect("fixture must parse");
        let page = parse_thread_page(&value, 1001).expect("page must parse");
        let post = &page.posts[1];
        let mut tx = pool.begin().await.expect("transaction must begin");
        let thread_match = enqueue_matches(&mut tx, "event", post, "watch-thread")
            .await
            .expect("thread match must succeed");
        assert_eq!(thread_match.outbox_enqueued, 0);
        enqueue_matches(&mut tx, "event", post, "watch-user")
            .await
            .expect("user match must succeed");
        tx.commit().await.expect("transaction must commit");

        let matches: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM post_event_watch_matches")
            .fetch_one(&pool)
            .await
            .expect("matches must count");
        let outbox: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
            .fetch_one(&pool)
            .await
            .expect("outbox must count");
        assert_eq!(matches, 2);
        assert_eq!(outbox, 1);
    }
}
