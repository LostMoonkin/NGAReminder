CREATE TABLE uid_reply_backfill_jobs (
    id TEXT PRIMARY KEY,
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE RESTRICT,
    uid INTEGER NOT NULL,
    start_date TEXT NOT NULL,
    start_at_unix INTEGER NOT NULL,
    end_at_unix INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
    next_page INTEGER NOT NULL DEFAULT 1 CHECK (next_page > 0),
    pages_requested INTEGER NOT NULL DEFAULT 0 CHECK (pages_requested >= 0),
    candidates_processed INTEGER NOT NULL DEFAULT 0 CHECK (candidates_processed >= 0),
    posts_inserted INTEGER NOT NULL DEFAULT 0 CHECK (posts_inserted >= 0),
    lease_until TEXT,
    lease_token TEXT,
    error_kind TEXT,
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    started_at TEXT,
    completed_at TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (start_at_unix <= end_at_unix)
);

CREATE UNIQUE INDEX uid_reply_backfill_one_active
    ON uid_reply_backfill_jobs ((1))
    WHERE status IN ('pending', 'running');

CREATE INDEX uid_reply_backfill_created
    ON uid_reply_backfill_jobs (created_at DESC, id DESC);
