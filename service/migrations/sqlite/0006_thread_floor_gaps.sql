CREATE TABLE thread_floor_gaps (
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    floor_number INTEGER NOT NULL CHECK (floor_number > 0),
    page_hint INTEGER NOT NULL CHECK (page_hint > 0),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'resolved', 'expired')),
    retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
    first_detected_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    next_retry_at TEXT NOT NULL,
    last_attempt_at TEXT,
    expires_at TEXT NOT NULL,
    resolved_at TEXT,
    PRIMARY KEY (watch_id, floor_number)
);

CREATE INDEX thread_floor_gaps_due
    ON thread_floor_gaps (watch_id, status, next_retry_at);
