-- NGA BBS Crawler Database Schema
-- SQLite database for storing thread and post data

-- Threads table: stores thread metadata
CREATE TABLE IF NOT EXISTS threads (
    tid INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    author_name TEXT NOT NULL,
    author_uid INTEGER NOT NULL,
    total_posts INTEGER DEFAULT 0,
    total_pages INTEGER DEFAULT 0,
    created_at TIMESTAMP DEFAULT (datetime('now', 'localtime')),
    updated_at TIMESTAMP DEFAULT (datetime('now', 'localtime'))
);

-- Posts table: stores individual post data
CREATE TABLE IF NOT EXISTS posts (
    pid INTEGER PRIMARY KEY,
    tid INTEGER NOT NULL,
    fid INTEGER NOT NULL,
    author_name TEXT NOT NULL,
    author_uid INTEGER NOT NULL,
    post_date TEXT NOT NULL,
    post_timestamp INTEGER NOT NULL,
    post_number INTEGER NOT NULL,
    content TEXT,
    created_at TIMESTAMP DEFAULT (datetime('now', 'localtime')),
    FOREIGN KEY (tid) REFERENCES threads(tid) ON DELETE CASCADE
);

-- Indexes for better query performance
CREATE INDEX IF NOT EXISTS idx_posts_tid ON posts(tid);
CREATE INDEX IF NOT EXISTS idx_posts_author_uid ON posts(author_uid);
CREATE INDEX IF NOT EXISTS idx_posts_timestamp ON posts(post_timestamp);
CREATE INDEX IF NOT EXISTS idx_threads_author_uid ON threads(author_uid);

-- View: Latest posts by thread
CREATE VIEW IF NOT EXISTS latest_posts AS
SELECT 
    p.*,
    t.title as thread_title
FROM posts p
JOIN threads t ON p.tid = t.tid
ORDER BY p.post_timestamp DESC;

-- View: Thread statistics
CREATE VIEW IF NOT EXISTS thread_stats AS
SELECT 
    t.tid,
    t.title,
    t.author_name,
    t.total_posts,
    t.total_pages,
    COUNT(DISTINCT p.pid) as crawled_posts,
    MAX(p.post_timestamp) as last_post_time,
    MIN(p.post_timestamp) as first_post_time
FROM threads t
LEFT JOIN posts p ON t.tid = p.tid
GROUP BY t.tid;
