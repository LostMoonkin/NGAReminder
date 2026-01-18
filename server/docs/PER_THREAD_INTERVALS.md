# Per-Thread Check Intervals - Fixed!

## 🐛 Bug Fixed

The monitoring loop now properly respects **per-thread `check_interval`** settings from the database!

---

## What Was Wrong

**Before (Bug):**
- `python monitor.py loop --interval 60` checked ALL threads every 60 seconds
- Per-thread `check_interval` settings in database were **ignored**
- All threads checked at the same time regardless of configuration

**After (Fixed):**
- Each thread is checked according to its own `check_interval`
- Thread A with 300s interval checks every 5 minutes
- Thread B with 600s interval checks every 10 minutes
- They run independently!

---

## How It Works Now

### 1. Configuration

Each thread has its own `check_interval` in the database:

```json
{
  "monitored_threads": [
    {
      "tid": 45974302,
      "check_interval": 300,  // Check every 5 minutes
      "enabled": true
    },
    {
      "tid": 43098323,
      "check_interval": 600,  // Check every 10 minutes
      "enabled": true
    }
  ]
}
```

### 2. Monitoring Loop

```bash
python monitor.py loop
```

**What happens:**
1. Every 10 seconds (configurable), evaluates which threads are due
2. Checks each thread if `time_since_last_check >= check_interval`
3. Threads checked independently based on their own schedule

---

## Example Timeline

**Setup:**
- Thread A: check_interval = 300s (5 min)
- Thread B: check_interval = 600s (10 min)

**Timeline:**

```
t=0:00  → Check Thread A, Check Thread B
t=0:10  → Evaluate (nothing due)
t=0:20  → Evaluate (nothing due)
...
t=5:00  → Check Thread A (300s passed)
t=5:10  → Evaluate (nothing due)
...
t=10:00 → Check Thread A (300s passed), Check Thread B (600s passed)
t=10:10 → Evaluate (nothing due)
...
t=15:00 → Check Thread A (300s passed)
```

---

## CLI Arguments

### `loop` command

```bash
python monitor.py loop [--check-all-interval SECONDS]
```

**Arguments:**
- `--check-all-interval`: How often to evaluate which threads need checking (default: 10)

**Examples:**

```bash
# Default: evaluate every 10 seconds
python monitor.py loop

# Evaluate every 30 seconds (less frequent checks)
python monitor.py loop --check-all-interval 30

# Evaluate every 5 seconds (more responsive)
python monitor.py loop --check-all-interval 5
```

---

## Output Example

```
Starting monitoring loop
Each thread will be checked according to its own check_interval
Press Ctrl+C to stop

================================================================================
Checking 2 thread(s) due for update
================================================================================

Thread 45974302: 自立自强,科学技术打头阵
  Check interval: 300s
  Overdue by: 5s

================================================================================
Checking thread 45974302: 自立自强,科学技术打头阵
================================================================================
Last check: 2026-01-14 12:45:00
Stored total posts: 3615
Current total posts: 3635
...

Thread 43098323: Another Thread  
  Check interval: 600s
  Overdue by: 2s

================================================================================
Completed checking 2 thread(s)
================================================================================

Monitoring 2 thread(s):
  TID 45974302: 自立自强,科学技术打头阵
    Interval: 300s, Last checked: 2026-01-14 12:50:05
  TID 43098323: Another Thread
    Interval: 600s, Last checked: 2026-01-14 12:50:07

Waiting 10s until next evaluation...
```

---

## Benefits

### 1. **Flexible Scheduling**
- Important threads: check every 60s
- Normal threads: check every 300s
- Low priority: check every 3600s

### 2. **Efficient Resource Usage**
- Don't waste API calls on threads that update slowly
- Focus attention on active threads

### 3. **Independent Timing**
- Each thread runs on its own schedule
- Adding/removing threads doesn't affect others

### 4. **Rate Limit Friendly**
- Spreads out requests over time
- Avoids burst of simultaneous checks

---

## Configuration Best Practices

### High Priority Threads (Active Discussions)

```json
{
  "tid": 45974302,
  "check_interval": 60,
  "enabled": true
}
```

Check every minute for real-time updates.

### Normal Priority (Regular Monitoring)

```json
{
  "tid": 43098323,
  "check_interval": 300,
  "enabled": true
}
```

Check every 5 minutes - good balance.

### Low Priority (Slow Threads)

```json
{
  "tid": 45922084,
  "check_interval": 3600,
  "enabled": true
}
```

Check every hour - minimal overhead.

### Mixed Configuration

```json
{
  "monitored_threads": [
    {"tid": 45974302, "check_interval": 60},   // Hot thread
    {"tid": 43098323, "check_interval": 300},  // Normal
    {"tid": 45922084, "check_interval": 3600}, // Slow
    {"tid": 39248719, "check_interval": 1800}  // Medium
  ]
}
```

Each thread checked at its own optimal frequency!

---

## Technical Details

### Time Tracking

The monitor tracks:
- `last_checked`: Timestamp of last check (stored in DB)
- `check_interval`: Seconds between checks (stored in DB)
- Compares `current_time - last_checked >= check_interval`

### Evaluation Frequency

The `--check-all-interval` parameter (default: 10s) controls how often the system evaluates which threads need checking.

**Trade-offs:**
- **Lower** (5s): More responsive, but more CPU usage
- **Higher** (30s): Less CPU, but up to 30s delay before checking

**Recommendation**: Keep at 10s for good balance.

---

## Migration from Old Behavior

If you were using:

```bash
python monitor.py loop --interval 60
```

Now use:

```bash
python monitor.py loop
```

The per-thread intervals from your config will be respected automatically!

---

## Summary

✅ **Fixed**: Per-thread `check_interval` now works correctly  
✅ **Independent**: Each thread runs on its own schedule  
✅ **Efficient**: Only checks threads when due  
✅ **Configurable**: Set different intervals for different threads  
✅ **Rate-friendly**: Spreads out requests naturally  

Your monitoring is now much smarter! 🎉
