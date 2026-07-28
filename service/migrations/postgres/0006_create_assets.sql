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

CREATE UNIQUE INDEX assets_content_hash_unique
    ON assets(content_hash) WHERE content_hash IS NOT NULL;
CREATE INDEX assets_download_queue
    ON assets(download_status, first_seen_at);

CREATE TABLE post_assets (
    post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    appearance_order INTEGER NOT NULL,
    usage TEXT NOT NULL DEFAULT 'inline',
    PRIMARY KEY (post_id, asset_id, appearance_order)
);

CREATE INDEX post_assets_asset ON post_assets(asset_id);
