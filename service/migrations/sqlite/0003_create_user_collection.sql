CREATE TABLE nga_users (
    uid INTEGER PRIMARY KEY,
    username TEXT NOT NULL,
    group_id INTEGER,
    avatar TEXT,
    registered_at_unix INTEGER,
    last_post_at_unix INTEGER,
    remote_post_count INTEGER,
    signature TEXT,
    raw_payload TEXT NOT NULL,
    first_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE user_watch_cursors (
    watch_id TEXT PRIMARY KEY REFERENCES watch_targets(id) ON DELETE CASCADE,
    newest_topic_at_unix INTEGER NOT NULL DEFAULT 0,
    newest_topic_tid INTEGER NOT NULL DEFAULT 0,
    newest_reply_at_unix INTEGER NOT NULL DEFAULT 0,
    newest_reply_pid INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX nga_users_last_post ON nga_users(last_post_at_unix);
