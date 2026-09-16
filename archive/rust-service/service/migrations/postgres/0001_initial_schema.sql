CREATE TABLE nga_accounts (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    label TEXT NOT NULL UNIQUE,
    passport_uid_encrypted BYTEA NOT NULL,
    passport_cid_encrypted BYTEA NOT NULL,
    encryption_version SMALLINT NOT NULL DEFAULT 1 CHECK (encryption_version > 0),
    status TEXT NOT NULL DEFAULT 'unchecked'
        CHECK (status IN ('unchecked', 'valid', 'invalid', 'paused')),
    last_auth_checked_at TIMESTAMPTZ,
    last_auth_error_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON TABLE nga_accounts IS
    'Encrypted NGA Passport credentials for the single-user service instance';
COMMENT ON COLUMN nga_accounts.passport_uid_encrypted IS
    'Versioned application-encrypted payload; never plaintext';
COMMENT ON COLUMN nga_accounts.passport_cid_encrypted IS
    'Versioned application-encrypted payload; never plaintext';

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
    pause_reason TEXT
        CHECK (pause_reason IN ('user', 'auth', 'error')),
    interval_seconds INTEGER NOT NULL DEFAULT 60 CHECK (interval_seconds BETWEEN 30 AND 86400),
    schedule_json TEXT,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'active', 'paused', 'error', 'not_found')),
    baseline_completed INTEGER NOT NULL DEFAULT 0 CHECK (baseline_completed IN (0, 1)),
    next_run_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    lease_until TIMESTAMPTZ,
    last_started_at TIMESTAMPTZ,
    last_completed_at TIMESTAMPTZ,
    last_error_kind TEXT,
    last_error_message TEXT,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX watch_targets_active_target
    ON watch_targets (target_type, target_id) WHERE deleted_at IS NULL;
CREATE INDEX watch_targets_due
    ON watch_targets (enabled, next_run_at) WHERE deleted_at IS NULL;

CREATE TABLE thread_watch_options (
    watch_id TEXT PRIMARY KEY REFERENCES watch_targets(id) ON DELETE CASCADE,
    history_mode TEXT NOT NULL DEFAULT 'full'
        CHECK (history_mode IN ('full', 'incremental')),
    history_parallel_enabled INTEGER NOT NULL DEFAULT 0
        CHECK (history_parallel_enabled IN (0, 1)),
    history_parallelism INTEGER NOT NULL DEFAULT 2
        CHECK (history_parallelism BETWEEN 1 AND 16),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE watch_cursors (
    watch_id TEXT PRIMARY KEY REFERENCES watch_targets(id) ON DELETE CASCADE,
    last_floor INTEGER NOT NULL DEFAULT -1,
    remote_vrows INTEGER NOT NULL DEFAULT 0,
    remote_total_pages INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE user_watch_cursors (
    watch_id TEXT PRIMARY KEY REFERENCES watch_targets(id) ON DELETE CASCADE,
    newest_topic_at_unix BIGINT NOT NULL DEFAULT 0,
    newest_topic_tid BIGINT NOT NULL DEFAULT 0,
    newest_reply_at_unix BIGINT NOT NULL DEFAULT 0,
    newest_reply_pid BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE crawl_runs (
    id TEXT PRIMARY KEY,
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    status TEXT NOT NULL
        CHECK (status IN ('running', 'succeeded', 'failed', 'skipped_busy', 'skipped')),
    baseline INTEGER NOT NULL CHECK (baseline IN (0, 1)),
    sync_mode TEXT NOT NULL DEFAULT 'incremental'
        CHECK (sync_mode IN ('tid_full_baseline', 'tid_incremental_baseline', 'uid_baseline', 'incremental')),
    pages_requested INTEGER NOT NULL DEFAULT 0,
    posts_inserted INTEGER NOT NULL DEFAULT 0,
    events_created INTEGER NOT NULL DEFAULT 0,
    matches_created INTEGER NOT NULL DEFAULT 0,
    outbox_enqueued INTEGER NOT NULL DEFAULT 0,
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
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    read_at TIMESTAMPTZ,
    UNIQUE (post_id, event_type)
);

CREATE INDEX post_events_occurred_at ON post_events (occurred_at);
CREATE INDEX post_events_unread ON post_events (read_at, occurred_at);

CREATE TABLE platform_integrations (
    id TEXT PRIMARY KEY,
    platform TEXT NOT NULL
        CHECK (platform IN ('bark', 'feishu', 'telegram', 'qq')),
    label TEXT NOT NULL UNIQUE,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    delivery_enabled INTEGER NOT NULL DEFAULT 1 CHECK (delivery_enabled IN (0, 1)),
    bot_enabled INTEGER NOT NULL DEFAULT 0 CHECK (bot_enabled IN (0, 1)),
    credentials_encrypted BYTEA NOT NULL,
    connection_status TEXT NOT NULL DEFAULT 'disconnected'
        CHECK (connection_status IN
            ('disconnected', 'connecting', 'connected', 'error')),
    last_connected_at TIMESTAMPTZ,
    last_error_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (platform <> 'bark' OR bot_enabled = 0)
);

COMMENT ON TABLE platform_integrations IS
    'A platform connection holds app credentials (e.g. Feishu App ID/Secret)';
COMMENT ON COLUMN platform_integrations.credentials_encrypted IS
    'Versioned application-encrypted JSON; notification targets live in notification_channels';

CREATE UNIQUE INDEX platform_integrations_one_bot_per_platform
    ON platform_integrations (platform)
    WHERE bot_enabled = 1;

CREATE TABLE notification_channels (
    id TEXT PRIMARY KEY,
    integration_id TEXT NOT NULL
        REFERENCES platform_integrations(id) ON DELETE RESTRICT,
    label TEXT NOT NULL UNIQUE,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    target_encrypted BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON COLUMN notification_channels.target_encrypted IS
    'Versioned application-encrypted per-recipient target (e.g. Feishu chat/open id, Bark device key)';

CREATE TABLE bot_bindings (
    id TEXT PRIMARY KEY,
    integration_id TEXT NOT NULL
        REFERENCES platform_integrations(id) ON DELETE CASCADE,
    actor_id TEXT NOT NULL,
    conversation_id TEXT,
    conversation_type TEXT
        CHECK (conversation_type IN ('private', 'group', 'channel')),
    role TEXT NOT NULL CHECK (role IN ('owner', 'operator', 'read_only')),
    label TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (integration_id, actor_id, conversation_id)
);

CREATE TABLE bot_pairing_tokens (
    id TEXT PRIMARY KEY,
    integration_id TEXT NOT NULL
        REFERENCES platform_integrations(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    requested_role TEXT NOT NULL
        CHECK (requested_role IN ('owner', 'operator', 'read_only')),
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE bot_inbound_events (
    id TEXT PRIMARY KEY,
    integration_id TEXT NOT NULL
        REFERENCES platform_integrations(id) ON DELETE CASCADE,
    platform_message_id TEXT NOT NULL,
    platform_event_id TEXT,
    actor_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    conversation_type TEXT NOT NULL,
    command_name TEXT,
    status TEXT NOT NULL DEFAULT 'received'
        CHECK (status IN
            ('received', 'processing', 'succeeded', 'rejected', 'failed')),
    error_kind TEXT,
    received_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    processed_at TIMESTAMPTZ,
    UNIQUE (integration_id, platform_message_id)
);

CREATE TABLE bot_outbox (
    id TEXT PRIMARY KEY,
    dedupe_key TEXT NOT NULL UNIQUE,
    integration_id TEXT NOT NULL
        REFERENCES platform_integrations(id) ON DELETE RESTRICT,
    inbound_event_id TEXT REFERENCES bot_inbound_events(id) ON DELETE SET NULL,
    conversation_id TEXT NOT NULL,
    reply_to_message_id TEXT,
    message_kind TEXT NOT NULL
        CHECK (message_kind IN ('text', 'image', 'card')),
    payload_encrypted BYTEA NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'sending', 'delivered', 'failed', 'dead')),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    lease_until TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    last_error_kind TEXT,
    delivered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX bot_outbox_due
    ON bot_outbox (status, next_attempt_at);

CREATE TABLE watch_notification_authors (
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    author_uid BIGINT NOT NULL,
    PRIMARY KEY (watch_id, author_uid)
);

CREATE TABLE watch_notification_channels (
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE RESTRICT,
    PRIMARY KEY (watch_id, channel_id)
);

CREATE TABLE post_event_watch_matches (
    post_event_id TEXT NOT NULL REFERENCES post_events(id) ON DELETE CASCADE,
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    matched_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (post_event_id, watch_id)
);

CREATE TABLE notification_outbox (
    id TEXT PRIMARY KEY,
    post_event_id TEXT NOT NULL REFERENCES post_events(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE RESTRICT,
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
    ON notification_outbox (status, next_attempt_at);

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

CREATE TABLE assets (
    id TEXT PRIMARY KEY,
    source_url TEXT NOT NULL UNIQUE,
    content_hash TEXT,
    mime_type TEXT,
    size_bytes BIGINT,
    original_name TEXT,
    local_relative_path TEXT,
    download_status TEXT NOT NULL DEFAULT 'remote_only'
        CHECK (download_status IN ('pending', 'downloading', 'ready', 'failed', 'remote_only')),
    http_status INTEGER,
    last_error_kind TEXT,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    downloaded_at TIMESTAMPTZ
);

-- Multiple NGA source URLs may resolve to the same bytes. Keep source records
-- distinct while the content-addressed filesystem reuses the same local file.
CREATE INDEX assets_content_hash
    ON assets (content_hash) WHERE content_hash IS NOT NULL;
CREATE INDEX assets_download_queue
    ON assets (download_status, first_seen_at);

CREATE TABLE post_assets (
    post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    appearance_order INTEGER NOT NULL,
    usage TEXT NOT NULL DEFAULT 'inline',
    PRIMARY KEY (post_id, asset_id, appearance_order)
);

CREATE INDEX post_assets_asset ON post_assets (asset_id);

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
    channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE RESTRICT,
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
    ON system_alert_outbox (status, next_attempt_at);

CREATE TABLE system_alert_deliveries (
    id TEXT PRIMARY KEY,
    outbox_id TEXT NOT NULL REFERENCES system_alert_outbox(id) ON DELETE CASCADE,
    attempt INTEGER NOT NULL,
    success INTEGER NOT NULL CHECK (success IN (0, 1)),
    http_status INTEGER,
    response_summary TEXT,
    error_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (outbox_id, attempt)
);

CREATE TABLE nga_account_renewal_settings (
    account_id TEXT PRIMARY KEY REFERENCES nga_accounts(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    login_name_encrypted BYTEA NOT NULL,
    password_encrypted BYTEA NOT NULL,
    bot_binding_id TEXT NOT NULL REFERENCES bot_bindings(id) ON DELETE RESTRICT,
    credential_status TEXT NOT NULL DEFAULT 'ready'
        CHECK (credential_status IN ('ready', 'invalid', 'cooldown')),
    consecutive_failure_count INTEGER NOT NULL DEFAULT 0,
    cooldown_until TIMESTAMPTZ,
    last_renewal_at TIMESTAMPTZ,
    last_error_kind TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON COLUMN nga_account_renewal_settings.login_name_encrypted IS
    'Optional renewal-only NGA login name; never used for crawling';
COMMENT ON COLUMN nga_account_renewal_settings.password_encrypted IS
    'Optional renewal-only NGA password; decrypted only inside a login flow';

CREATE TABLE nga_login_sessions (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES nga_accounts(id) ON DELETE CASCADE,
    bot_binding_id TEXT NOT NULL REFERENCES bot_bindings(id) ON DELETE RESTRICT,
    integration_id TEXT NOT NULL
        REFERENCES platform_integrations(id) ON DELETE RESTRICT,
    actor_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    trigger_kind TEXT NOT NULL CHECK (trigger_kind IN ('cookie_invalid', 'manual')),
    status TEXT NOT NULL
        CHECK (status IN (
            'awaiting_confirmation',
            'starting',
            'awaiting_captcha',
            'submitting',
            'validating_cookie',
            'succeeded',
            'failed',
            'cancelled',
            'expired',
            'unsupported_challenge'
        )),
    challenge_kind TEXT
        CHECK (challenge_kind IN ('none', 'image', 'tencent', 'match_phone')),
    protocol_context_encrypted BYTEA,
    captcha_attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error_kind TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX nga_login_sessions_one_active_account
    ON nga_login_sessions (account_id)
    WHERE status IN (
        'awaiting_confirmation', 'starting', 'awaiting_captcha',
        'submitting', 'validating_cookie'
    );
