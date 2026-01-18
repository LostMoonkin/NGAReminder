# Thread Monitoring Guide

## Overview

The thread monitor tracks NGA threads for new posts and can filter by specific authors.

---

## Quick Start

### 1. Add a Thread to Monitor

Monitor all new posts in a thread:
```bash
python monitor.py add --tid 45974302 --interval 300
```

Monitor only posts by specific author(s):
```bash
python monitor.py add --tid 45974302 --authors 150058 --interval 300
```

Monitor multiple authors:
```bash
python monitor.py add --tid 45974302 --authors 150058,42098303 --interval 300
```

### 2. Check for New Posts

Check a specific thread once:
```bash
python monitor.py check --tid 45974302
```

Check all monitored threads once:
```bash
python monitor.py check
```

### 3. Run Continuous Monitoring

Monitor continuously (checks every 60 seconds):
```bash
python monitor.py loop --interval 60
```

### 4. List Monitored Threads

```bash
python monitor.py list
```

### 5. View Monitoring Events

View all events:
```bash
python monitor.py events --limit 50
```

View events for specific thread:
```bash
python monitor.py events --tid 45974302 --limit 20
```

### 6. Remove Thread from Monitoring

```bash
python monitor.py remove --tid 45974302
```

---

## Features

### Author Filtering

You can monitor posts only from specific authors by providing their UIDs:

```bash
# Monitor only posts by user 150058
python monitor.py add --tid 45974302 --authors 150058

# Monitor posts by multiple users
python monitor.py add --tid 45974302 --authors 150058,42098303,39248719
```

When author filter is set, the monitor will only notify about new posts from those specific authors.

### Check Intervals

Control how often threads are checked (in seconds):

```bash
# Check every 5 minutes (300 seconds)
python monitor.py add --tid 45974302 --interval 300

# Check every minute (60 seconds)
python monitor.py add --tid 45974302 --interval 60

# Check every hour (3600 seconds)
python monitor.py add --tid 45974302 --interval 3600
```

### Event Logging

All monitoring activity is logged to the database:
- `new_post`: New posts were found
- `check`: Regular check with no new posts
- `error`: Error occurred during check

View the log:
```bash
python monitor.py events --limit 100
```

---

## Use Cases

### 1. Monitor Stock Discussion Thread for Specific User

Track a specific financial advisor's posts:

```bash
python monitor.py add --tid 45974302 --authors 150058
python monitor.py loop --interval 300
```

### 2. Track Multiple Threads

Add multiple threads to monitoring:

```bash
python monitor.py add --tid 45974302 --interval 300
python monitor.py add --tid 43098323 --interval 300
python monitor.py add --tid 45922084 --interval 600
```

Then run the loop to check all:

```bash
python monitor.py loop
```

### 3. One-Time Check

Quickly check if there are new posts without continuous monitoring:

```bash
python monitor.py check --tid 45974302
```

### 4. Monitor Announcements

Set up monitoring for important announcement threads with shorter intervals:

```bash
python monitor.py add --tid 12345 --interval 60
python monitor.py loop --interval 60
```

---

## Database Tables

### `monitored_threads`

Stores monitoring configuration:

| Field | Description |
|-------|-------------|
| `tid` | Thread ID |
| `author_filter` | Comma-separated author UIDs (NULL = all) |
| `check_interval` | Seconds between checks |
| `last_checked` | Last check timestamp |
| `last_post_timestamp` | Timestamp of last seen post |
| `is_active` | Whether monitoring is active |

### `monitoring_events`

Logs all monitoring activity:

| Field | Description |
|-------|-------------|
| `id` | Event ID |
| `tid` | Thread ID |
| `event_type` | Type: 'new_post', 'check', 'error' |
| `post_count` | Number of new posts found |
| `message` | Event description |
| `created_at` | Event timestamp |

---

## Example Workflow

```bash
# 1. Add thread to monitor (check every 5 minutes)
python monitor.py add --tid 45974302 --authors 150058 --interval 300

# 2. List monitored threads to confirm
python monitor.py list

# 3. Do a manual check first
python monitor.py check --tid 45974302

# 4. Start continuous monitoring
python monitor.py loop --interval 60

# In another terminal, view events as they happen:
watch -n 5 'python monitor.py events --limit 10'
```

---

## Integration with Other Tools

### Export New Posts

Get new posts from the database:

```python
from database import NGADatabase

db = NGADatabase()
new_posts = db.get_posts_by_author(150058, limit=10)
for post in new_posts:
    print(f"{post['post_date']}: {post['content'][:100]}")
```

### Custom Notifications

Extend `monitor.py` to send notifications:
- Email alerts
- Slack/Discord webhooks
- Desktop notifications
- SMS via Twilio

Example:

```python
def check_thread(self, tid: int):
    result = super().check_thread(tid)
    
    if result.get('new_posts', 0) > 0:
        # Send notification
        send_email(f"New posts in thread {tid}")
    
    return result
```

---

## Tips

1. **Politeness**: Don't set check intervals too low (< 60s) to avoid overwhelming the server
2. **Multiple monitors**: You can run multiple monitoring instances with different intervals
3. **Database cleanup**: Periodically clean old events to keep database size manageable
4. **Author UIDs**: Find author UIDs by checking posts in the database or API responses
5. **Recovery**: If monitoring is interrupted, it will resume from the last checked timestamp

---

## Troubleshooting

### Monitor not detecting new posts

Check the `last_post_timestamp` in monitored_threads:

```python
from database import NGADatabase
db = NGADatabase()
db.cursor.execute('SELECT * FROM monitored_threads WHERE tid = ?', (45974302,))
print(dict(db.cursor.fetchone()))
```

### Reset monitoring state

```python
from database import NGADatabase
db = NGADatabase()
db.cursor.execute('UPDATE monitored_threads SET last_post_timestamp = 0 WHERE tid = ?', (45974302,))
db.conn.commit()
```

### View recent monitoring activity

```bash
python monitor.py events --limit 20
```
