CREATE TABLE notification_channels (
    id TEXT PRIMARY KEY,
    channel_type TEXT NOT NULL CHECK (channel_type IN ('bark', 'feishu')),
    label TEXT NOT NULL UNIQUE,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    config_encrypted BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE notification_rules (
    id TEXT PRIMARY KEY,
    label TEXT NOT NULL UNIQUE,
    channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
    tid BIGINT,
    uid BIGINT,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (tid IS NOT NULL OR uid IS NOT NULL)
);

CREATE TABLE post_event_matches (
    id TEXT PRIMARY KEY,
    post_event_id TEXT NOT NULL REFERENCES post_events(id) ON DELETE CASCADE,
    rule_id TEXT NOT NULL REFERENCES notification_rules(id) ON DELETE CASCADE,
    matched_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (post_event_id, rule_id)
);

CREATE TABLE notification_outbox (
    id TEXT PRIMARY KEY,
    post_event_id TEXT NOT NULL REFERENCES post_events(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'sending', 'delivered', 'failed', 'dead')),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    lease_until TIMESTAMPTZ,
    last_error_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    delivered_at TIMESTAMPTZ,
    UNIQUE (post_event_id, channel_id)
);

CREATE INDEX notification_outbox_due
    ON notification_outbox(status, next_attempt_at);

CREATE TABLE notification_deliveries (
    id TEXT PRIMARY KEY,
    outbox_id TEXT NOT NULL REFERENCES notification_outbox(id) ON DELETE CASCADE,
    attempt INTEGER NOT NULL,
    success INTEGER NOT NULL CHECK (success IN (0, 1)),
    http_status INTEGER,
    response_summary TEXT,
    error_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (outbox_id, attempt)
);
