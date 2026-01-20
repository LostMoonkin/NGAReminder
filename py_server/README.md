# NGA BBS API Crawler

A Python 3 command-line tool for crawling paginated posts from NGA forum threads using the app API.

## Features

- ✅ Cookie-based authentication with NGA passport
- ✅ Pagination support (single page or page ranges)
- ✅ Chrome user agent for proper API access
- ✅ JSON output with pretty printing
- ✅ Error handling and request timeout
- ✅ Configurable via JSON config file

## Installation

1. **Navigate to the server directory**

```bash
cd /root/project/NGAReminder/server
```

2. **Create and activate a Python virtual environment**

```bash
# Create virtual environment
python3 -m venv venv

# Activate virtual environment
source venv/bin/activate  # On Linux/Mac
# or
venv\Scripts\activate     # On Windows
```

3. **Install dependencies**

```bash
pip install -r requirements.txt
```

Alternatively, install directly:

```bash
pip install requests
```

4. **Set up configuration**

Copy the example config file and add your NGA credentials:

```bash
cp config.example.json config.json
```

Edit `config.json` and replace the placeholder values with your actual NGA passport cookies:

```json
{
  "ngaPassportUid": "your_actual_uid",
  "ngaPassportCid": "your_actual_cid",
  "user_agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
  "max_threads": 5,
  "rate_limit_per_minute": 30
}
```

**Configuration Options**:
- `ngaPassportUid` (required): Your NGA passport user ID cookie
- `ngaPassportCid` (required): Your NGA passport credential cookie  
- `user_agent` (optional): Custom user agent string (defaults to Chrome if not specified)
- `max_threads` (optional): Number of concurrent threads for fetching pages (default: 5)
- `rate_limit_per_minute` (optional): Maximum API requests per minute (default: 30)

### How to get your NGA cookies

1. Log in to NGA BBS in your browser
2. Open Developer Tools (F12)
3. Go to the Application/Storage tab → Cookies
4. Find `ngaPassportUid` and `ngaPassportCid` values
5. Copy them to your `config.json`

## Usage

> **Note**: Make sure your virtual environment is activated before running the crawler:
> ```bash
> source venv/bin/activate
> ```

### How It Works

The crawler automatically:
1. Fetches page 1 to determine total page count
2. Uses multi-threading to fetch all remaining pages in parallel
3. Prints results in page order

### Basic Commands

**Crawl all pages of a thread:**

```bash
python nga_crawler.py --tid 12345
```

**Use a custom config file:**

```bash
python nga_crawler.py --tid 12345 --config my_config.json
```

### Command-line Options

- `--tid` (required): Thread ID to crawl (automatically fetches all pages)
- `--config`: Path to config file (default: `config.json`)

### Performance Tuning

Adjust `max_threads` in your config file to control concurrent requests:
- **Lower values (1-3)**: More polite to the server, slower crawling
- **Medium values (5-10)**: Balanced performance (recommended)
- **Higher values (10+)**: Faster but may trigger rate limiting

### Examples

```bash
# Crawl all pages of thread 37888657
python nga_crawler.py --tid 37888657

# Use alternative config with different thread count
python nga_crawler.py --tid 37888657 --config prod_config.json
```

### Sample Output

```
================================================================================
Starting crawl for thread 45974302
================================================================================

[1/3] Fetching page 1 to determine total pages...
✓ Thread: 自立自强,科学技术打头阵
✓ Total pages: 181
✓ Total posts: 3615
✓ Posts per page: 20

[2/3] Fetching pages 2-181 using 5 threads...
  ✓ Page 2/181 completed
  ✓ Page 3/181 completed
  ✓ Page 5/181 completed
  ✓ Page 4/181 completed
  ✓ Page 6/181 completed
  ...

[3/3] Crawl summary:
  ✓ Successful: 181/181 pages

================================================================================
Crawl completed: 181/181 pages retrieved
================================================================================
```

## Documentation

For detailed documentation, see the [`docs/`](docs/) directory:

### Getting Started
- **[GETTING_STARTED.md](docs/GETTING_STARTED.md)** - Complete overview and quick start guide
- **[PROJECT_SUMMARY.md](docs/PROJECT_SUMMARY.md)** - Project architecture and features

### Monitoring System
- **[MONITOR.md](docs/MONITOR.md)** - Monitoring features and commands
- **[CONFIG_MONITOR.md](docs/CONFIG_MONITOR.md)** - Configuration file format
- **[MONITORING_IMPROVEMENTS.md](docs/MONITORING_IMPROVEMENTS.md)** - Smart detection technical details
- **[PER_THREAD_INTERVALS.md](docs/PER_THREAD_INTERVALS.md)** - Per-thread check interval guide

### Technical Documentation
- **[DATABASE.md](docs/DATABASE.md)** - Database schema and queries
- **[API_ANALYSIS.md](docs/API_ANALYSIS.md)** - NGA API response structure
- **[RATE_LIMITING.md](docs/RATE_LIMITING.md)** - Rate limiting configuration
- **[CHANGELOG.md](docs/CHANGELOG.md)** - Multi-threading implementation notes

---

## Quick Reference

The crawler uses the NGA app API:

- **Base URL**: `https://bbs.nga.cn/app_api.php`
- **Library**: `post`
- **Action**: `list`
- **Method**: POST
- **Parameters**:
  - `tid` (form body): Thread ID (integer)
  - `page` (form body): Page number (integer)

## Output

Results are printed to stdout in pretty-printed JSON format. Each page is separated by dividers for readability.

Example output:

```
================================================================================
Fetching page 1...
================================================================================

{
  "code": 0,
  "data": {
    ...
  }
}

================================================================================
Crawl completed
================================================================================
```

## Security Notes

⚠️ **Important**: 
- Never commit `config.json` to version control (it's already in `.gitignore`)
- Keep your NGA passport cookies private
- Cookies may expire - update them if you get authentication errors

## Error Handling

The crawler includes error handling for:
- Missing or invalid config files
- Network request failures
- Invalid JSON responses
- Request timeouts (30 seconds)

## Project Structure

```
server/
├── nga_crawler.py       # Main crawler script
├── config.json          # Your credentials (create from example)
├── config.example.json  # Template config file
├── requirements.txt     # Python dependencies
└── README.md           # This file
```

## License

This tool is for personal use. Please respect NGA's terms of service and use responsibly.
