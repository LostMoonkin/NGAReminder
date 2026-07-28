pub mod alerts;
pub mod sender;
pub mod worker;

use sqlx::{Any, Row, Transaction};
use uuid::Uuid;

use crate::domain::thread::ParsedPost;

pub async fn enqueue_matches(
    tx: &mut Transaction<'_, Any>,
    event_id: &str,
    post: &ParsedPost,
) -> Result<(), sqlx::Error> {
    let rules = sqlx::query(
        "SELECT r.id, r.channel_id
         FROM notification_rules r
         JOIN notification_channels c ON c.id = r.channel_id
         WHERE r.enabled = 1 AND c.enabled = 1
           AND (r.tid IS NULL OR r.tid = $1)
           AND (r.uid IS NULL OR r.uid = $2)",
    )
    .bind(post.tid)
    .bind(post.author_uid)
    .fetch_all(&mut **tx)
    .await?;

    for rule in rules {
        let rule_id: String = rule.get("id");
        let channel_id: String = rule.get("channel_id");
        sqlx::query(
            "INSERT INTO post_event_matches (id, post_event_id, rule_id)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(event_id)
        .bind(&rule_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "INSERT INTO notification_outbox (id, post_event_id, channel_id)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(event_id)
        .bind(channel_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::any::AnyPoolOptions;

    use super::enqueue_matches;
    use crate::nga::thread_parser::parse_thread_page;

    #[tokio::test]
    async fn multiple_rules_share_one_channel_outbox() {
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
        for (id, tid, uid) in [
            ("rule-tid", Some(1001_i64), None),
            ("rule-uid", None, Some(2002_i64)),
        ] {
            sqlx::query(
                "INSERT INTO notification_rules (id, label, channel_id, tid, uid)
                 VALUES ($1, $1, 'channel', $2, $3)",
            )
            .bind(id)
            .bind(tid)
            .bind(uid)
            .execute(&pool)
            .await
            .expect("rule must insert");
        }
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
        enqueue_matches(&mut tx, "event", post)
            .await
            .expect("matching must succeed");
        enqueue_matches(&mut tx, "event", post)
            .await
            .expect("repeated matching must succeed");
        tx.commit().await.expect("transaction must commit");

        let matches: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM post_event_matches")
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
