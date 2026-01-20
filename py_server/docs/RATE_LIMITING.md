# Rate Limiting Configuration

## Overview

The crawler now includes configurable rate limiting to prevent overwhelming the NGA server.

---

## Configuration

Add `rate_limit_per_minute` to your `config.json`:

```json
{
  "ngaPassportUid": "your_uid",
  "ngaPassportCid": "your_cid",
  "max_threads": 5,
  "rate_limit_per_minute": 30
}
```

---

## Settings

### `rate_limit_per_minute`

Maximum number of API requests per minute.

**Default**: 30 requests/minute (1 request every 2 seconds)

**Recommended values**:
- **Conservative**: 20-30 requests/minute (polite, slower)
- **Balanced**: 30-40 requests/minute (default, recommended)
- **Aggressive**: 50-60 requests/minute (faster, may trigger rate limiting)

---

## How It Works

### Rate Limiting Algorithm

Uses a simple **token bucket** algorithm:

1. Calculates minimum interval between requests: `60 seconds / rate_limit`
2. Before each request, checks time since last request
3. If interval is too short, sleeps until the minimum interval has passed
4. Thread-safe using locks (works with multi-threading)

### Example

With `rate_limit_per_minute: 30`:

```
Minimum interval = 60 / 30 = 2 seconds

Request 1 at t=0.0s  → proceeds immediately
Request 2 at t=0.5s  → sleeps 1.5s, proceeds at t=2.0s
Request 3 at t=2.1s  → proceeds immediately (> 2s since last)
Request 4 at t=3.0s  → proceeds immediately
```

---

## Performance Impact

### Without Rate Limiting (old)
- 5 threads × unlimited speed = very fast but risky
- Could trigger server-side rate limiting
- Risk of IP ban

### With Rate Limiting (30 req/min)
- 5 threads with controlled speed
- Maximum 30 requests/minute regardless of thread count
- Safe and polite to server

### Crawl Time Examples

**Thread with 181 pages, 30 requests/minute:**

```
Page 1: 0 seconds
Pages 2-181 (180 pages):
  180 requests / 30 per minute = 6 minutes

Total: ~6 minutes
```

**Comparison:**

| rate_limit_per_minute | Time for 181 pages |
|-----------------------|-------------------|
| 20 | ~9 minutes |
| 30 | ~6 minutes |
| 40 | ~4.5 minutes |
| 60 | ~3 minutes |
| Unlimited (risky) | ~30 seconds |

---

## Thread Pool vs Rate Limit

Both settings work together:

### `max_threads`
- Controls **parallelism** (how many requests at once)
- Higher = more concurrent worker threads

### `rate_limit_per_minute`
- Controls **throughput** (total requests per minute)
- Lower = more polite to server

**Example Configuration:**

```json
{
  "max_threads": 5,
  "rate_limit_per_minute": 30
}
```

- 5 threads try to fetch pages in parallel
- But rate limiter ensures max 30 requests/minute total
- Threads will wait when rate limit is reached

---

## Use Cases

### 1. Initial Full Crawl (Less Urgent)

```json
{
  "max_threads": 3,
  "rate_limit_per_minute": 20
}
```

- Polite and slow
- Good for archiving old threads
- Respectful to server

### 2. Regular Monitoring (Balanced)

```json
{
  "max_threads": 5,
  "rate_limit_per_minute": 30
}
```

- Default configuration
- Good balance of speed and politeness
- Recommended for most users

### 3. Urgent Updates (Faster)

```json
{
  "max_threads": 10,
  "rate_limit_per_minute": 50
}
```

- Faster crawling
- Use sparingly
- May trigger rate limiting

---

##Examples

### Conservative (Very Polite)

```json
{
  "rate_limit_per_minute": 20
}
```

**Result**: 1 request every 3 seconds

### Default (Recommended)

```json
{
  "rate_limit_per_minute": 30
}
```

**Result**: 1 request every 2 seconds

### Fast (Use with Caution)

```json
{
  "rate_limit_per_minute": 60
}
```

**Result**: 1 request every second

---

## Monitoring Rate Limits

The crawler automatically:
- Tracks request times
- Enforces minimum intervals
- Works across all threads

You don't need to do anything special - just set your preferred rate in the config!

---

## Best Practices

1. **Start conservative**: Begin with 20-30 requests/minute
2. **Monitor for errors**: If you see many failed requests, lower the rate
3. **Be respectful**: The server hosts free content, don't abuse it
4. **Adjust based on time**: Lower rates during peak hours, higher during off-peak
5. **One config for all**: Rate limit applies to both crawler and monitor

---

## Troubleshooting

### Getting rate limited by server

**Symptoms**: HTTP 429 errors, connection refused

**Solution**: Lower `rate_limit_per_minute` to 15-20

### Crawling too slow

**Symptoms**: Takes too long to crawl

**Solution**: 
1. Increase `rate_limit_per_minute` to 40-50
2. Increase `max_threads` to 8-10
3. Monitor for errors

### Inconsistent timing

**Symptoms**: Some requests very slow

**Note**: This is expected with rate limiting - threads wait for their turn

---

##Summary

✅ **Configurable** - Set your preferred rate  
✅ **Thread-safe** - Works with multi-threading  
✅ **Automatic** - No manual intervention needed  
✅ **Polite** - Respects server resources  
✅ **Effective** - Prevents rate limit bans  

Default 30 requests/minute is recommended for most users!
