CREATE TABLE threads (
    tid BIGINT PRIMARY KEY,
    fid BIGINT NOT NULL,
    title TEXT NOT NULL,
    forum_name TEXT NOT NULL,
    author_uid BIGINT NOT NULL,
    author_name TEXT NOT NULL,
    coverage TEXT NOT NULL DEFAULT 'full'
        CHECK (coverage IN ('partial', 'full')),
    remote_total_pages INTEGER NOT NULL DEFAULT 0,
    remote_vrows INTEGER NOT NULL DEFAULT 0,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE posts (
    id TEXT PRIMARY KEY,
    tid BIGINT NOT NULL REFERENCES threads(tid) ON DELETE CASCADE,
    pid BIGINT,
    floor_number INTEGER,
    post_kind TEXT NOT NULL CHECK (post_kind IN ('topic', 'reply', 'comment')),
    parent_post_id TEXT REFERENCES posts(id) ON DELETE CASCADE,
    author_uid BIGINT NOT NULL,
    author_name TEXT NOT NULL,
    subject TEXT NOT NULL DEFAULT '',
    content_raw TEXT NOT NULL,
    published_at_unix BIGINT,
    page_number INTEGER NOT NULL,
    raw_payload TEXT NOT NULL,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (post_kind = 'topic' AND floor_number = 0)
        OR (post_kind = 'reply' AND floor_number > 0 AND pid IS NOT NULL)
        OR (post_kind = 'comment' AND parent_post_id IS NOT NULL AND pid IS NOT NULL)
    )
);

CREATE UNIQUE INDEX posts_topic_natural_key
    ON posts (tid) WHERE post_kind = 'topic';
CREATE UNIQUE INDEX posts_reply_comment_natural_key
    ON posts (tid, pid) WHERE post_kind IN ('reply', 'comment');
CREATE INDEX posts_thread_floor ON posts (tid, floor_number);
CREATE INDEX posts_author ON posts (author_uid, published_at_unix);

CREATE TABLE watch_targets (
    id TEXT PRIMARY KEY,
    target_type TEXT NOT NULL CHECK (target_type IN ('thread', 'user')),
    target_id BIGINT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    interval_seconds INTEGER NOT NULL DEFAULT 60 CHECK (interval_seconds BETWEEN 30 AND 86400),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'active', 'paused', 'error', 'not_found')),
    baseline_completed INTEGER NOT NULL DEFAULT 0 CHECK (baseline_completed IN (0, 1)),
    next_run_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    lease_until TIMESTAMPTZ,
    last_started_at TIMESTAMPTZ,
    last_completed_at TIMESTAMPTZ,
    last_error_kind TEXT,
    last_error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (target_type, target_id)
);

CREATE INDEX watch_targets_due
    ON watch_targets (enabled, next_run_at);

CREATE TABLE watch_cursors (
    watch_id TEXT PRIMARY KEY REFERENCES watch_targets(id) ON DELETE CASCADE,
    last_floor INTEGER NOT NULL DEFAULT -1,
    remote_vrows INTEGER NOT NULL DEFAULT 0,
    remote_total_pages INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE crawl_runs (
    id TEXT PRIMARY KEY,
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    status TEXT NOT NULL
        CHECK (status IN ('running', 'succeeded', 'failed', 'skipped_busy')),
    baseline INTEGER NOT NULL CHECK (baseline IN (0, 1)),
    pages_requested INTEGER NOT NULL DEFAULT 0,
    posts_inserted INTEGER NOT NULL DEFAULT 0,
    events_created INTEGER NOT NULL DEFAULT 0,
    remote_vrows INTEGER,
    error_kind TEXT,
    error_message TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMPTZ
);

CREATE INDEX crawl_runs_watch_started
    ON crawl_runs (watch_id, started_at DESC);

CREATE TABLE post_events (
    id TEXT PRIMARY KEY,
    post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (event_type IN ('new_topic', 'new_reply')),
    discovered_by_watch_id TEXT REFERENCES watch_targets(id) ON DELETE SET NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (post_id, event_type)
);

CREATE INDEX post_events_occurred_at ON post_events (occurred_at);
