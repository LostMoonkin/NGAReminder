# Monitoring Configuration

Define threads to monitor in your `config.json` file.

## Configuration Format

Add a `monitored_threads` array to your config.json:

```json
{
  "ngaPassportUid": "your_uid_here",
  "ngaPassportCid": "your_cid_here",
  "user_agent": "...",
  "max_threads": 5,
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
      "enabled": false
    }
  ]
}
```

## Field Descriptions

### monitored_threads (array)

Each object in the array configures one thread:

- **tid** (required, integer): Thread ID to monitor
- **author_filter** (optional, array or null): 
  - Array of author UIDs to monitor: `[150058, 42098303]`
  - `null` or omit to monitor all authors
- **check_interval** (optional, integer): Seconds between checks (default: 300)
- **enabled** (optional, boolean): Whether to monitor this thread (default: true)

## Examples

### Monitor specific author in one thread

```json
{
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

### Monitor multiple authors

```json
{
  "monitored_threads": [
    {
      "tid": 45974302,
      "author_filter": [150058, 42098303, 39248719],
      "check_interval": 300,
      "enabled": true
    }
  ]
}
```

### Monitor all posts in a thread

```json
{
  "monitored_threads": [
    {
      "tid": 45974302,
      "author_filter": null,
      "check_interval": 600,
      "enabled": true
    }
  ]
}
```

### Multiple threads with different settings

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
      "author_filter": [150058, 42098303],
      "check_interval": 600,
      "enabled": true
    },
    {
      "tid": 45922084,
      "author_filter": null,
      "check_interval": 900,
      "enabled": false
    }
  ]
}
```

## Usage

### Sync from config file

Load all threads defined in config.json:

```bash
python monitor.py sync
```

Use a different config file:

```bash
python monitor.py sync --config prod_config.json
```

### What sync does

The `sync` command will:
1. Read `monitored_threads` from config file
2. Add new threads to monitoring
3. Update existing threads with new settings
4. Skip threads where `enabled: false`
5. Show a summary of changes

### Example output

```
Syncing 3 thread(s) from config...

  + TID 45974302: Adding to monitoring
Initializing thread 45974302...
✓ Thread 45974302 added to monitoring
  Title: 自立自强,科学技术打头阵
  Author: -阿狼-
  Filter: [150058]
  Check interval: 300s

  ↻ TID 43098323: Updating configuration
✓ Thread 43098323 added to monitoring
  Title: Another Thread
  Author: Username
  Filter: None
  Check interval: 600s

  ⊘ TID 45922084: Skipped (disabled in config)

================================================================================
Sync complete:
  Added: 1
  Updated: 1
  Skipped: 1
================================================================================
```

## Workflow

1. **Create/edit config.json**:
   ```bash
   cp config.example.json config.json
   # Edit config.json with your threads
   ```

2. **Sync threads**:
   ```bash
   python monitor.py sync
   ```

3. **Verify**:
   ```bash
   python monitor.py list
   ```

4. **Start monitoring**:
   ```bash
   python monitor.py loop
   ```

## Tips

- **Disabling temporarily**: Set `"enabled": false` instead of removing from config
- **Different intervals**: Use shorter intervals for important threads
- **Author filter**: You can find author UIDs from the database or API responses
- **Re-sync anytime**: Run `sync` again to update settings from config
- **Config backup**: Keep your config.json in version control (but gitignored!)
