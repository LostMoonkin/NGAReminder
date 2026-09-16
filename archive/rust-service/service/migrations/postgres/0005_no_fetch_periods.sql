ALTER TABLE watch_targets
    ADD COLUMN no_fetch_periods_json TEXT;

ALTER TABLE watch_targets
    ADD COLUMN pending_trigger_kind TEXT
        CHECK (pending_trigger_kind IS NULL OR pending_trigger_kind IN ('manual'));

ALTER TABLE watch_targets
    ADD COLUMN lease_trigger_kind TEXT
        CHECK (lease_trigger_kind IS NULL OR lease_trigger_kind IN ('scheduled', 'manual'));

ALTER TABLE crawl_runs
    ADD COLUMN trigger_kind TEXT NOT NULL DEFAULT 'unknown'
        CHECK (trigger_kind IN ('unknown', 'scheduled', 'manual'));
