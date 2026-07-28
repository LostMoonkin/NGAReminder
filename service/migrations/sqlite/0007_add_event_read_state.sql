ALTER TABLE post_events ADD COLUMN read_at TEXT;

CREATE INDEX post_events_unread ON post_events(read_at, occurred_at);
