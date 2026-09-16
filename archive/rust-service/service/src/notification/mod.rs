pub mod alerts;
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

pub(crate) async fn watch_matches_post(
    tx: &mut Transaction<'_, Any>,
    post: &ParsedPost,
    watch_id: &str,
) -> Result<bool, sqlx::Error> {
    let watch = sqlx::query(
        "SELECT target_type, target_id FROM watch_targets
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(watch_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(watch) = watch else {
        return Ok(false);
    };
    let target_type: String = watch.get("target_type");
    let target_id: i64 = watch.get("target_id");
    if target_type == "user" {
        return Ok(target_id == post.author_uid);
    }
    if target_type != "thread" || target_id != post.tid {
        return Ok(false);
    }

    let author_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM watch_notification_authors WHERE watch_id = $1")
            .bind(watch_id)
            .fetch_one(&mut **tx)
            .await?;
    if author_count == 0 {
        return Ok(true);
    }
    let author_matches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM watch_notification_authors
         WHERE watch_id = $1 AND author_uid = $2",
    )
    .bind(watch_id)
    .bind(post.author_uid)
    .fetch_one(&mut **tx)
    .await?;
    Ok(author_matches == 1)
}

pub(crate) async fn enqueue_confirmed_match(
    tx: &mut Transaction<'_, Any>,
    event_id: &str,
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

    let channels = sqlx::query(
        "SELECT wc.channel_id FROM watch_notification_channels wc
         JOIN notification_channels c ON c.id = wc.channel_id
         JOIN platform_integrations i ON i.id = c.integration_id
         WHERE wc.watch_id = $1 AND c.enabled = 1
           AND i.enabled = 1 AND i.delivery_enabled = 1",
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

    use crate::{collector::thread::insert_event, nga::thread_parser::parse_thread_page};

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
            "INSERT INTO platform_integrations
             (id, platform, label, credentials_encrypted)
             VALUES ('integration', 'bark', 'test', X'00')",
        )
        .execute(&pool)
        .await
        .expect("integration must insert");
        sqlx::query(
            "INSERT INTO notification_channels
             (id, integration_id, label, target_encrypted)
             VALUES ('channel', 'integration', 'test', X'00')",
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
        let thread_match = insert_event(&mut tx, "post", post, "watch-thread")
            .await
            .expect("thread match must succeed");
        assert_eq!(thread_match.outbox_enqueued, 0);
        insert_event(&mut tx, "post", post, "watch-user")
            .await
            .expect("user match must succeed");
        tx.commit().await.expect("transaction must commit");

        sqlx::query(
            "INSERT INTO posts
             (id, tid, pid, floor_number, post_kind, author_uid, author_name,
              content_raw, page_number, raw_payload)
             VALUES ('filtered-post', 1001, 4002, 2, 'reply', 2002,
                     'reply author', 'filtered body', 1, '')",
        )
        .execute(&pool)
        .await
        .expect("filtered post must insert");
        let mut filtered_post = post.clone();
        filtered_post.pid = Some(4002);
        filtered_post.floor_number = 2;
        let mut tx = pool.begin().await.expect("transaction must begin");
        let filtered = insert_event(&mut tx, "filtered-post", &filtered_post, "watch-thread")
            .await
            .expect("filtered event insert must succeed");
        tx.commit().await.expect("transaction must commit");
        assert!(!filtered.event_created);
        assert_eq!(filtered.matches_created, 0);
        assert_eq!(filtered.outbox_enqueued, 0);

        let matches: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM post_event_watch_matches")
            .fetch_one(&pool)
            .await
            .expect("matches must count");
        let outbox: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
            .fetch_one(&pool)
            .await
            .expect("outbox must count");
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM post_events")
            .fetch_one(&pool)
            .await
            .expect("events must count");
        assert_eq!(events, 1);
        assert_eq!(matches, 1);
        assert_eq!(outbox, 1);
    }
}
