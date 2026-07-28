CREATE TABLE system_alerts (
    id TEXT PRIMARY KEY,
    alert_key TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    url TEXT NOT NULL,
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE system_alert_outbox (
    id TEXT PRIMARY KEY,
    alert_id TEXT NOT NULL REFERENCES system_alerts(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'sending', 'delivered', 'failed', 'dead')),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    lease_until TIMESTAMPTZ,
    last_error_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    delivered_at TIMESTAMPTZ,
    UNIQUE (alert_id, channel_id)
);

CREATE INDEX system_alert_outbox_due
    ON system_alert_outbox(status, next_attempt_at);

CREATE TABLE system_alert_deliveries (
    id TEXT PRIMARY KEY,
    outbox_id TEXT NOT NULL REFERENCES system_alert_outbox(id) ON DELETE CASCADE,
    attempt INTEGER NOT NULL,
    success INTEGER NOT NULL,
    http_status INTEGER,
    response_summary TEXT,
    error_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (outbox_id, attempt)
);
