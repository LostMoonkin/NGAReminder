ALTER TABLE assets
    ADD COLUMN download_lease_until TIMESTAMPTZ;

ALTER TABLE assets
    ADD COLUMN download_claim_token TEXT;

ALTER TABLE watch_targets
    ADD COLUMN lease_token TEXT;

-- Give downloads already in flight during a rolling upgrade a grace period.
UPDATE assets
SET download_lease_until = CURRENT_TIMESTAMP + INTERVAL '10 minutes'
WHERE download_status = 'downloading';

CREATE INDEX assets_download_lease
    ON assets (download_status, download_lease_until, first_seen_at);
