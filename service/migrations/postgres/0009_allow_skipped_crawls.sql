ALTER TABLE crawl_runs DROP CONSTRAINT IF EXISTS crawl_runs_status_check;

ALTER TABLE crawl_runs
    ADD CONSTRAINT crawl_runs_status_check
    CHECK (status IN ('running', 'succeeded', 'failed', 'skipped_busy', 'skipped'));
