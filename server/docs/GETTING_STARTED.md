# Thread Monitoring System - Complete Guide

## ✅ What's Been Built

A complete NGA BBS thread monitoring system with:

1. **Database storage** (SQLite)
2. **Multi-threaded crawler** 
3. **Thread monitoring** with author filtering
4. **Configuration-based setup**
5. **Event logging**

---

## 🚀 Quick Start

### 1. Setup Configuration

Edit `config.json`:

```json
{
  "ngaPassportUid": "your_uid",
  "ngaPassportCid": "your_cid",
  "user_agent": "...",
  "max_threads": 5,
  "monitored_threads": [
    {
      "tid": 45974302,
      "author_filter": [150058],
      "check_interval": 300,
      "enabled": true
    }
  ]
}
```

### 2. Sync Threads from Config

```bash
python monitor.py sync
```

### 3. Start Monitoring

```bash
python monitor.py loop --interval 60
```

---

## 📋 Features

### Config-Based Monitoring

Define all threads in `config.json`:
- **tid**: Thread ID to monitor
- **author_filter**: Array of UIDs to track (or `null` for all)
- **check_interval**: Seconds between checks
- **enabled**: Enable/disable without removing

### Commands

| Command | Description |
|---------|-------------|
| `sync` | Load threads from config file |
| `add` | Manually add a thread |
| `remove` | Stop monitoring a thread |
| `list` | Show all monitored threads |
| `check` | Check for new posts once |
| `loop` | Continuous monitoring |
| `events` | View event history |

### Author Filtering

Monitor only specific authors:

```json
{
  "tid": 45974302,
  "author_filter": [150058, 42098303],
  "check_interval": 300
}
```

Or monitor everyone:

```json
{
  "tid": 45974302,
  "author_filter": null,
  "check_interval": 600
}
```

---

## 📖 Usage Examples

### Example 1: Monitor Multiple Threads

`config.json`:
```json
{
  "monitored_threads": [
    {
      "tid": 45974302,
      "author_filter": [150058],
      "check_interval": 300,
      "enabled": true
    },
    {
      "tid": 43098323,
      "author_filter": null,
      "check_interval": 600,
      "enabled": true
    },
    {
      "tid": 45922084,
      "author_filter": [150058, 42098303],
      "check_interval": 900,
      "enabled": false
    }
  ]
}
```

Sync and start:
```bash
python monitor.py sync
python monitor.py list
python monitor.py loop
```

### Example 2: One-Time Check

```bash
python monitor.py check --tid 45974302
```

### Example 3: View Activity

```bash
python monitor.py events --limit 20
python monitor.py events --tid 45974302 --limit 10
```

---

## 🗂️ File Structure

```
server/
├── nga_crawler.py          # Multi-threaded crawler
├── database.py             # Database management
├── monitor.py              # Thread monitoring
├── schema.sql              # Threads & posts tables
├── schema_monitor.sql      # Monitoring tables
├── config.json             # Your configuration
├── config.example.json     # Configuration template
├── nga_data.db             # SQLite database
├── README.md               # Crawler documentation
├── DATABASE.md             # Database schema docs
├── MONITOR.md              # Monitoring guide
└── CONFIG_MONITOR.md       # Config reference
```

---

## 💾 Database Tables

### Core Tables
- **threads**: Thread metadata
- **posts**: Post content and authors

### Monitoring Tables
- **monitored_threads**: Monitoring configuration
- **monitoring_events**: Activity log

---

## 🔄 Workflow

### Initial Setup
```bash
# 1. Copy example config
cp config.example.json config.json

# 2. Edit config with your credentials and threads
nano config.json

# 3. Sync threads
python monitor.py sync
```

### Daily Use
```bash
# Start monitoring loop
python monitor.py loop --interval 60

# In another terminal, watch events:
watch -n 5 'python monitor.py events --limit 10'
```

### Adding New Threads
```bash
# Option 1: Edit config.json and sync
nano config.json
python monitor.py sync

# Option 2: Add manually
python monitor.py add --tid 12345 --authors 150058 --interval 300
```

---

## 📊 Output Examples

### Sync Output
```
Syncing 3 thread(s) from config...

  + TID 45974302: Adding to monitoring
Initializing thread 45974302...
✓ Thread 45974302 added to monitoring
  Title: 自立自强,科学技术打头阵
  Author: -阿狼-
  Filter: [150058]
  Check interval: 300s

================================================================================
Sync complete:
  Added: 1
  Updated: 0
  Skipped: 0
================================================================================
```

### Check Output
```
================================================================================
Checking thread 45974302
================================================================================
Last check: 2026-01-14 12:10:00
Last post timestamp: 1768226718

✓ Found 2 new post(s)!

  Post #854544859 by -阿狼- (2026-01-14 13:19)
  早上我不想和你们交流那么低级的问题 我的操作值多少钱你们自己说...
```

---

## 🎯 Next Steps

### Potential Enhancements

1. **Notifications**
   - Email alerts for new posts
   - Desktop notifications
   - Webhook integration (Slack, Discord)

2. **Analytics**
   - Post frequency analysis
   - Author activity trends
   - Content keywords tracking

3. **Filtering**
   - Content-based filters
   - Vote threshold filters
   - Time-based filters

4. **Export**
   - Generate daily summaries
   - Export to HTML/PDF
   - RSS feed generation

---

## 🔧 Troubleshooting

### No new posts detected

Check monitoring state:
```bash
python monitor.py list
python monitor.py events --tid 45974302
```

### Reset monitoring

```python
from database import NGADatabase
db = NGADatabase()
db.cursor.execute('UPDATE monitored_threads SET last_post_timestamp = 0')
db.conn.commit()
```

### View raw database

```bash
sqlite3 nga_data.db
.tables
SELECT * FROM monitored_threads;
.quit
```

---

## 📝 Notes

- **Politeness**: Don't set check intervals too low
- **Reliability**: The system tracks state, so restarts are safe
- **Scaling**: Can monitor dozens of threads simultaneously
- **Flexibility**: Mix config-based and manual thread management
