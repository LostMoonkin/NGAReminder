-- Add monitoring tables to track thread monitoring state

-- Monitored threads configuration
CREATE TABLE IF NOT EXISTS monitored_threads (
    tid INTEGER PRIMARY KEY,
    author_filter TEXT,  -- Comma-separated UIDs to monitor, or NULL for all
    author_notification TEXT,  -- Comma-separated UIDs to send notifications for, or NULL for none
    check_interval INTEGER DEFAULT 300,  -- Seconds between checks
    last_checked TIMESTAMP,
    last_post_timestamp INTEGER DEFAULT 0,
    is_active BOOLEAN DEFAULT 1,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (tid) REFERENCES threads(tid) ON DELETE CASCADE
);

-- Monitoring events log
CREATE TABLE IF NOT EXISTS monitoring_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tid INTEGER NOT NULL,
    event_type TEXT NOT NULL,  -- 'new_post', 'error', 'check'
    post_count INTEGER DEFAULT 0,
    message TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (tid) REFERENCES threads(tid) ON DELETE CASCADE
);

-- Create indexes
CREATE INDEX IF NOT EXISTS idx_monitored_active ON monitored_threads(is_active);
CREATE INDEX IF NOT EXISTS idx_monitoring_events_tid ON monitoring_events(tid);
CREATE INDEX IF NOT EXISTS idx_monitoring_events_created ON monitoring_events(created_at);
