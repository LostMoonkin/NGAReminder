# NGA Reminder service

Independent Rust service for NGA thread/user monitoring, PostgreSQL/SQLite persistence,
notifications, and Markdown export.

## Local development

Requirements:

- Rust 1.92 or newer.
- Docker with Compose.

Start PostgreSQL:

```bash
docker compose -f compose.yml up -d postgres
```

Alternatively, run without PostgreSQL by selecting SQLite and setting its file path:

```bash
export NGA_REMINDER__DATABASE_BACKEND=sqlite
export NGA_REMINDER__SQLITE_PATH=./data/nga-reminder.db
```

The parent directory is created automatically. SQLite starts in WAL mode with foreign keys enabled
and a 5-second busy timeout. PostgreSQL remains the recommended backend for multiple worker
processes; SQLite is intended for a single `all` process.

Raw post JSON is disabled by default:

```text
NGA_REMINDER__PERSISTENCE__STORE_RAW_PAYLOAD=false
```

When disabled, new `posts.raw_payload` values are stored as an empty string while normalized post
fields remain available. Set it to `true` only when full NGA payloads are needed for parser
diagnostics. Changing the option does not erase or backfill existing rows.

Asset persistence is configured independently:

```text
NGA_REMINDER__ASSETS__DOWNLOAD_ENABLED=false
NGA_REMINDER__ASSETS__STORAGE_PATH=./data/assets
NGA_REMINDER__ASSETS__MAX_DOWNLOAD_BYTES=10485760
```

Inline NGA `[img]` resources are recorded in the database even when downloads are disabled. With
downloads disabled, exports keep the remote URL. With downloads enabled, trusted HTTPS NGA image
hosts are queued for bounded download and stored under a SHA-256 content-addressed path. The
database and `assets.storage_path` must be backed up together.

Configure the service:

```bash
cp .env.example .env
export NGA_REMINDER__API_TOKEN='replace-with-a-long-random-token'
export NGA_REMINDER__ADMIN_PASSWORD='replace-with-a-long-random-password'
export NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY="$(openssl rand -base64 32)"
```

When using a populated `.env` file directly with Cargo, load it into the shell first:

```bash
set -a
. ./.env
set +a
```

Run API and workers:

```bash
cargo run -- all
```

Roles:

```text
cargo run -- serve
cargo run -- worker
cargo run -- all
```

Public probes:

```text
GET /health
GET /ready
GET /metrics
```

Protected API calls require:

```text
Authorization: Bearer <NGA_REMINDER__API_TOKEN>
```

The minimal management page is available at `GET /admin`. It establishes an HttpOnly,
SameSite=Strict administrator session and can save or test the two NGA Passport values. API
responses expose only a masked UID and never return either credential.

## Thread and user watches

Create a thread watch after configuring and testing the NGA account:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/watches/threads \
  -H "Authorization: Bearer $NGA_REMINDER__API_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{"tid":12345678,"interval_seconds":60}'
```

The worker picks up a new watch immediately. Its first crawl imports every accessible page as a
baseline and does not create notification events. Later crawls append only floors above the stored
cursor. Existing post content is never updated or deleted.

Available watch endpoints:

```text
GET    /api/v1/watches
POST   /api/v1/watches/threads
POST   /api/v1/watches/users
PATCH  /api/v1/watches/{id}
DELETE /api/v1/watches/{id}
POST   /api/v1/watches/{id}/run
```

`interval_seconds` is optional and defaults to `scheduler.default_interval_seconds` in
`config/default.toml` (or `NGA_REMINDER__SCHEDULER__DEFAULT_INTERVAL_SECONDS`). It must be between
30 and 86400. A manual run returns `409` while another worker holds the watch lease. NGA code 14
disables a missing-thread watch. Thread `code=51` (the post is pending review) skips the current
crawl without advancing the cursor and is retried on the next scheduled run. Rejected credentials
pause the account and affected watch until credentials are corrected and the watch is explicitly
enabled.

Each watch can also provide a `schedule` array. Rules are evaluated in order using the configured
`scheduler.timezone_offset`; `days` accepts `weekdays`, `weekends`, or individual weekday names
(`monday` through `sunday`, with common three/four-letter abbreviations). `start_time` and
`end_time` use `HH:MM`, and `end_time` may be `24:00`. The watch's `interval_seconds` remains the
fallback outside all rules.

```bash
curl -X POST http://127.0.0.1:8080/api/v1/watches/threads \
  -H "Authorization: Bearer $NGA_REMINDER__API_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{
    "tid": 12345678,
    "schedule": [
      {
        "days": ["weekdays"],
        "description": "Weekday business hours - every 2 min",
        "start_time": "09:00",
        "end_time": "16:00",
        "interval": 120
      },
      {
        "days": ["weekends"],
        "description": "Weekends - every hour",
        "start_time": "00:00",
        "end_time": "23:59",
        "interval": 3600
      }
    ]
  }'
```

When a rule boundary is approaching, the scheduler runs no later than that boundary, so a long
interval cannot skip a faster/slower rule transition. A `PATCH` with `schedule: []` clears the
schedule and returns the watch to its fallback interval.

Create a user watch:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/watches/users \
  -H "Authorization: Bearer $NGA_REMINDER__API_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{"uid":150058,"interval_seconds":60}'
```

A user watch stores only that UID's topic post and individual replies; it never expands a
participated TID into a full-thread crawl. The baseline is silent. Later list scans stop at the
stored `(postdate, tid/pid)` boundary and fetch details only for new candidates. User-list NGA busy
responses are retried once per second for ten total attempts; exhaustion records `skipped_busy`
without advancing either cursor. If a thread detail returns `code=51`, the user crawl is recorded as
`skipped_pending_review` and its cursors remain unchanged for the next scheduled run.

## Notifications

Notification channel secrets are encrypted at rest and never returned by list APIs. Supported
channel types are `bark` and `feishu`. Rules require at least one of `tid` or `uid`; providing both
uses AND semantics.

```text
GET/POST    /api/v1/channels
PATCH/DELETE /api/v1/channels/{id}
POST        /api/v1/channels/{id}/test
GET/POST    /api/v1/notification-rules
PATCH/DELETE /api/v1/notification-rules/{id}
```

Example Bark channel config:

```json
{"device_key":"...","server_url":"https://api.day.app","group":"NGA Reminder"}
```

Example Feishu channel config:

```json
{
  "app_id": "cli_...",
  "app_secret": "...",
  "receive_id_type": "chat_id",
  "receive_id": "oc_..."
}
```

The Feishu adapter uses an enterprise custom application's bot capability, not a custom-bot
webhook. `receive_id_type` defaults to `chat_id`; `open_id`, `user_id`, `union_id`, and `email` are
also accepted. The application must be published with message-send permission, be available to the
recipient, and be added to the target group for group delivery. Tenant access tokens are cached in
memory according to their returned lifetime and are never persisted.

Feishu cards extract NGA `[img]...[/img]` markup from the full post body. Up to three HTTPS images
from the supported NGA image hosts are downloaded with a 10 MB limit, uploaded through
`im/v1/images`, and embedded using the returned `image_key`. Image keys are cached in memory per
application and source URL. A rejected host, failed download, failed upload, or any image after the
first three is rendered as a source link instead, so image handling never blocks the text
notification. The Feishu application therefore also needs the image/file resource upload scope.

Matching is transactional with post-event creation. Multiple TID/UID rules targeting the same
channel share one outbox row. Delivery retries transient failures up to five attempts and records
each attempt without logging channel secrets.

Reply actions use the persisted page number and NGA's in-thread anchor format:
`read.php?tid={tid}&page={page}#pid{pid}Anchor`. Opening a notification therefore shows the full
thread page positioned at the matching reply instead of NGA's isolated-reply view.

## Markdown and ZIP exports

Protected export endpoints default to Markdown and accept `?format=markdown` or `?format=zip`:

```text
GET /api/v1/exports/threads/{tid}?format=markdown
GET /api/v1/exports/threads/{tid}?format=zip
GET /api/v1/exports/users/{uid}?format=markdown
GET /api/v1/exports/users/{uid}?format=zip
```

Thread exports include all persisted posts for the TID. User exports include persisted posts
authored by the UID and group them by TID. ZIP exports contain the Markdown file, `metadata.json`,
and local assets whose download status is `ready`; missing or remote-only resources remain remote
links.

## Asset persistence design

The planned asset worker stores attachment/image/audio/video metadata in the selected database and
stores binary content under `assets.storage_path`; it does not use PostgreSQL `BYTEA` or SQLite
`BLOB`. With downloads disabled, only remote URLs and metadata are retained.

Downloaded files use SHA-256 content-addressed paths:

```text
<assets.storage_path>/<first two SHA-256 characters>/<full SHA-256>.<safe extension>
```

This allows content deduplication and keeps database/WAL/backup size under control. The database and
asset directory form one logical backup unit and must be backed up and restored together. This
section describes the frozen design; the first asset downloader and export path are implemented and
accepted as part of M5. Broader attachment payload coverage and resource cleanup remain optional
post-M5 enhancements.

Production deployment should bind the application to an internal interface and use the example
[`deploy/nginx.conf`](deploy/nginx.conf) as the starting point for TLS termination.

See [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for Prometheus metrics, structured logging, Cookie
失效告警、PostgreSQL/SQLite plus assets backup and restore, Docker release/rollback, and Nginx
TLS/reverse-proxy deployment instructions.

Do not commit real NGA Cookies, API tokens, Bark keys, or Feishu application secrets.
