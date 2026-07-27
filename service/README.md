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

Configure the service:

```bash
cp .env.example .env
export NGA_REMINDER__API_TOKEN='replace-with-a-long-random-token'
export NGA_REMINDER__ADMIN_PASSWORD='replace-with-a-long-random-password'
export NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY="$(openssl rand -base64 32)"
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
```

Protected API calls require:

```text
Authorization: Bearer <NGA_REMINDER__API_TOKEN>
```

The minimal management page is available at `GET /admin`. It establishes an HttpOnly,
SameSite=Strict administrator session and can save or test the two NGA Passport values. API
responses expose only a masked UID and never return either credential.

## Thread watches

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

Available M2 endpoints:

```text
GET    /api/v1/watches
POST   /api/v1/watches/threads
PATCH  /api/v1/watches/{id}
DELETE /api/v1/watches/{id}
POST   /api/v1/watches/{id}/run
```

`interval_seconds` must be between 30 and 86400. A manual run returns `409` while another worker
holds the watch lease. NGA code 14 disables a missing-thread watch; rejected credentials pause the
account and affected watch until credentials are corrected and the watch is explicitly enabled.

Production deployment should bind the application to an internal interface and use the example
[`deploy/nginx.conf`](deploy/nginx.conf) as the starting point for TLS termination.

Do not commit real NGA Cookies, API tokens, Bark keys, or Feishu webhooks.
