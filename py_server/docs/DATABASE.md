# Database Schema Documentation

## Overview

The NGA BBS crawler uses SQLite to store thread and post data for persistence and easy querying.

---

## Tables

### `threads`

Stores thread metadata.

| Column | Type | Description |
|--------|------|-------------|
| `tid` | INTEGER PRIMARY KEY | Thread ID (unique) |
| `title` | TEXT | Thread subject/title |
| `author_name` | TEXT | Thread author's username |
| `author_uid` | INTEGER | Thread author's UID |
| `total_posts` | INTEGER | Total number of posts in thread |
| `total_pages` | INTEGER | Total number of pages |
| `created_at` | TIMESTAMP | When thread was first crawled |
| `updated_at` | TIMESTAMP | When thread was last updated |

### `posts`

Stores individual post data.

| Column | Type | Description |
|--------|------|-------------|
| `pid` | INTEGER PRIMARY KEY | Post ID (unique) |
| `tid` | INTEGER | Thread ID (foreign key) |
| `fid` | INTEGER | Forum ID |
| `author_name` | TEXT | Post author's username |
| `author_uid` | INTEGER | Post author's UID |
| `post_date` | TEXT | Formatted post date |
| `post_timestamp` | INTEGER | Unix timestamp |
| `content` | TEXT | Post content (HTML/BBCode) |
| `created_at` | TIMESTAMP | When post was crawled |

---

## Indexes

Performance indexes for common queries:

- `idx_posts_tid` - Fast lookup of posts by thread
- `idx_posts_author_uid` - Fast lookup of posts by author
- `idx_posts_timestamp` - Fast chronological queries
- `idx_threads_author_uid` - Fast lookup of threads by author

---

## Views

### `latest_posts`

Shows recent posts with thread information.

```sql
SELECT * FROM latest_posts LIMIT 10;
```

### `thread_stats`

Aggregated statistics for each thread.

```sql
SELECT * FROM thread_stats WHERE tid = 12345;
```

Shows:
- Total posts crawled vs. total posts in thread
- Latest and earliest post timestamps
- Thread metadata

---

## Common Queries

### Get all posts in a thread

```sql
SELECT * FROM posts 
WHERE tid = 12345 
ORDER BY post_timestamp;
```

### Find posts by specific author

```sql
SELECT * FROM posts 
WHERE author_uid = 150058 
ORDER BY post_timestamp DESC;
```

### Search posts by keyword

```sql
SELECT * FROM posts 
WHERE content LIKE '%科技%' 
ORDER BY post_timestamp DESC;
```

### Get thread statistics

```sql
SELECT 
    t.title,
    COUNT(p.pid) as crawled_posts,
    t.total_posts,
    MAX(p.post_timestamp) as latest_post
FROM threads t
LEFT JOIN posts p ON t.tid = p.tid
WHERE t.tid = 12345
GROUP BY t.tid;
```

### Find most active authors

```sql
SELECT 
    author_name,
    author_uid,
    COUNT(*) as post_count,
    SUM(vote_good) as total_upvotes
FROM posts
GROUP BY author_uid
ORDER BY post_count DESC
LIMIT 10;
```

---

## Python API

### Initialize Database

```python
from database import NGADatabase

db = NGADatabase('nga_data.db')
```

### Save Thread

```python
thread_data = {
    'tid': 12345,
    'title': 'Thread Title',
    'author_name': 'Username',
    'author_uid': 100,
    'total_posts': 100,
    'total_pages': 5
}

db.save_thread(thread_data)
```

### Save Posts

```python
# Single post
post_data = {
    'pid': 1,
    'tid': 12345,
    'fid': 1,
    'author_name': 'Username',
    'author_uid': 100,
    'post_date': '2024-01-01 12:00',
    'post_timestamp': 1704096000,
    'content': 'Post content'
}

db.save_post(post_data)

# Batch save
posts = [post_data1, post_data2, ...]
count = db.save_posts_batch(posts)
```

### Query Data

```python
# Get thread info
thread = db.get_thread(12345)

# Get all posts in thread
posts = db.get_posts_by_thread(12345)

# Get posts by author
author_posts = db.get_posts_by_author(150058, limit=50)

# Search posts
results = db.search_posts('科技', limit=100)

# Get thread statistics
stats = db.get_thread_stats()
```

### Parse API Response

```python
from database import parse_page_result

# Parse API response into DB format
thread_data, posts_data = parse_page_result(api_response)

db.save_thread(thread_data)
db.save_posts_batch(posts_data)
```

---

## Database File

- **Location**: `nga_data.db` (configurable)
- **Format**: SQLite 3
- **Size**: Depends on data volume (estimate: ~1KB per post)
- **Backup**: Simply copy the `.db` file

---

## Maintenance

### Vacuum Database (Optimize Size)

```sql
VACUUM;
```

### Check Database Integrity

```sql
PRAGMA integrity_check;
```

### View Table Info

```sql
PRAGMA table_info(posts);
```

---

## Migration Notes

When schema changes are needed:

1. Update `schema.sql`
2. Create migration script for existing databases
3. Use `ALTER TABLE` for additive changes
4. For breaking changes, create new tables and migrate data

Example migration:

```sql
-- Add new column
ALTER TABLE posts ADD COLUMN is_deleted BOOLEAN DEFAULT 0;

-- Create index
CREATE INDEX idx_posts_deleted ON posts(is_deleted);
```
