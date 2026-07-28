PRAGMA foreign_keys = OFF;

CREATE TABLE crawl_runs_new (
    id TEXT PRIMARY KEY,
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    status TEXT NOT NULL
        CHECK (status IN ('running', 'succeeded', 'failed', 'skipped_busy', 'skipped')),
    baseline INTEGER NOT NULL CHECK (baseline IN (0, 1)),
    pages_requested INTEGER NOT NULL DEFAULT 0,
    posts_inserted INTEGER NOT NULL DEFAULT 0,
    events_created INTEGER NOT NULL DEFAULT 0,
    remote_vrows INTEGER,
    error_kind TEXT,
    error_message TEXT,
    started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TEXT
);

INSERT INTO crawl_runs_new (
    id, watch_id, status, baseline, pages_requested, posts_inserted, events_created,
    remote_vrows, error_kind, error_message, started_at, completed_at
)
SELECT
    id, watch_id, status, baseline, pages_requested, posts_inserted, events_created,
    remote_vrows, error_kind, error_message, started_at, completed_at
FROM crawl_runs;

DROP TABLE crawl_runs;
ALTER TABLE crawl_runs_new RENAME TO crawl_runs;

CREATE INDEX crawl_runs_watch_started
    ON crawl_runs (watch_id, started_at DESC);

PRAGMA foreign_keys = ON;
