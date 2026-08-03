# NGA Reminder Service

[中文](README.md)

An independent Rust service for NGA thread/user monitoring, PostgreSQL/SQLite persistence, notifications, and Markdown export.

## Local development

Requirements:

- Rust 1.92 or newer.
- Docker Compose.

Start PostgreSQL:

```bash
docker compose -f compose.yml up -d postgres
```

Alternatively, select SQLite:

```bash
export NGA_REMINDER__DATABASE_BACKEND=sqlite
export NGA_REMINDER__SQLITE_PATH=./data/nga-reminder.db
```

The parent directory is created automatically. SQLite uses WAL mode, foreign keys, and a five-second busy timeout. PostgreSQL is recommended for multiple worker processes; SQLite is intended for one `all` process.

Raw post JSON is disabled by default:

```text
NGA_REMINDER__PERSISTENCE__STORE_RAW_PAYLOAD=false
```

When disabled, `posts.raw_payload` is empty for new rows while normalized fields remain available. Enable it only for parser diagnostics that require the full NGA response. Changing the option does not erase or backfill existing rows.

Configure assets independently:

```text
NGA_REMINDER__ASSETS__DOWNLOAD_ENABLED=false
NGA_REMINDER__ASSETS__STORAGE_PATH=./data/assets
NGA_REMINDER__ASSETS__MAX_DOWNLOAD_BYTES=10485760
```

Inline NGA `[img]` resources are recorded even when downloads are disabled. Enabled downloads use trusted HTTPS NGA hosts, bounded downloads, and SHA-256 content-addressed paths. Back up the database and `assets.storage_path` together.

Configure and run the service:

```bash
cp .env.example .env
export NGA_REMINDER__API_TOKEN='replace-with-a-long-random-token'
export NGA_REMINDER__ADMIN_PASSWORD='replace-with-a-long-random-password'
export NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY="$(openssl rand -base64 32)"
cargo run -- all
```

If using `.env` directly with Cargo, load it first with `set -a; . ./.env; set +a`.

## Production without PostgreSQL

Use [`compose.production.yml`](compose.production.yml) for a single-server deployment. It runs one SQLite `all` container and stores the database and assets in the `nga-reminder-data` Docker volume.

```bash
export NGA_REMINDER__API_TOKEN="$(openssl rand -hex 32)"
export NGA_REMINDER__ADMIN_PASSWORD="$(openssl rand -base64 32)"
export NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY="$(openssl rand -base64 32)"
docker compose -f compose.production.yml up -d
```

The template binds `127.0.0.1:12888` and is intended to sit behind Nginx. Set `NGA_REMINDER_IMAGE` to an immutable GHCR tag for upgrades or rollbacks. Do not run `docker compose down -v`, which removes the SQLite data volume. See [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for backup and restore.

Roles and public probes:

```text
cargo run -- serve   # API only
cargo run -- worker  # worker only
cargo run -- all     # API + worker

GET /health
GET /ready
GET /metrics
```

Protected API calls require `Authorization: Bearer <NGA_REMINDER__API_TOKEN>`. The management page is available at `GET /admin`; it uses an HttpOnly, SameSite=Strict administrator session and never returns either NGA credential.

## Thread and user watches

Create a thread watch after configuring and testing the NGA account:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/watches/threads \
  -H "Authorization: Bearer $NGA_REMINDER__API_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{
    "tid": 12345678,
    "interval_seconds": 60,
    "history": {"mode": "full", "parallel_enabled": true, "parallelism": 2},
    "notification": {"channel_ids": ["channel-id"], "author_uids": []}
  }'
```

The first crawl imports accessible pages as a silent baseline. Later crawls append only floors above the stored cursor; existing posts are never updated or deleted. Watch endpoints are:

```text
GET    /api/v1/watches
POST   /api/v1/watches/threads
POST   /api/v1/watches/users
GET    /api/v1/watches/{id}
PATCH  /api/v1/watches/{id}
DELETE /api/v1/watches/{id}
POST   /api/v1/watches/{id}/run
POST   /api/v1/watches/{id}/reset
GET    /api/v1/watches/{id}/runs
```

`interval_seconds` defaults to the scheduler configuration and must be between 30 and 86400. A manual run returns `409` while another worker holds the lease. Code 14 disables a missing-thread watch; Thread `code=51` skips the current crawl without advancing the cursor. Rejected credentials pause the account and affected watches until corrected and explicitly enabled.

User watches do not import history. The first run records only current topic/reply watermarks. Later runs save newly discovered posts and replies for delivery retries and audit, without expanding a discovered TID into a full-thread crawl. Busy responses are retried up to ten times; exhaustion records `skipped_busy` without advancing cursors.

## Notifications

Channel secrets are encrypted at rest and never returned by list APIs. Supported channels are `bark` and `feishu`; matching is configured on each watch.

```text
GET/POST     /api/v1/channels
PATCH/DELETE /api/v1/channels/{id}
POST         /api/v1/channels/{id}/test
```

Feishu uses an enterprise custom application's bot capability, not a custom-bot webhook. The application needs message-send permission and must be available to the recipient; group delivery also requires adding it to the target group. Tokens are cached in memory and never persisted. Cards may embed up to three trusted NGA images; rejected or failed images fall back to source links.

Multiple watches matching one event and channel share one outbox row while retaining all source-watch relationships. Transient delivery failures are retried up to five times. Disabling a channel pauses new enqueue operations and existing retries.

## Markdown and ZIP exports

Protected export endpoints default to Markdown and accept `?format=markdown` or `?format=zip`:

```text
GET /api/v1/exports/threads/{tid}?format=markdown
GET /api/v1/exports/threads/{tid}?format=zip
GET /api/v1/exports/users/{uid}?format=markdown
GET /api/v1/exports/users/{uid}?format=zip
```

Thread exports contain all persisted posts for the TID. User exports contain persisted posts authored by the UID, grouped by TID. ZIP files contain Markdown, `metadata.json`, and ready local assets; missing or remote-only assets remain remote links.

Markdown is read with stable cursor pagination and streamed to the client. ZIP exports are built incrementally under `assets.storage_path/.tmp` and streamed from disk, so complete posts, assets, and archives are not retained in memory. Temporary ZIP files are removed when the response completes or is dropped.

Asset maintenance is available in the admin console and through:

```text
GET  /api/v1/assets/maintenance
POST /api/v1/assets/maintenance/cleanup
```

The read-only scan reports missing files, orphan metadata, orphan content files, and expired export temporary files. Cleanup requeues missing ready assets and removes only orphan/temporary files older than the retention window.

See [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for metrics, structured logging, Cookie-invalid alerts, backups, Docker release/rollback, and Nginx deployment. See [`docs/BOT_INTERACTION_AND_COOKIE_RENEWAL_DESIGN.md`](docs/BOT_INTERACTION_AND_COOKIE_RENEWAL_DESIGN.md) for bot architecture and Cookie renewal.

Never commit real NGA Cookies, API tokens, Bark keys, or Feishu application secrets.
