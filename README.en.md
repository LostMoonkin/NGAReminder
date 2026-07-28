# NGA Reminder

[中文主 README](README.md)

NGA Reminder provides NGA thread and user monitoring, persistent storage, notifications, exports, and a web administration console. It also includes a standalone Chromium extension that runs without the server.

## Components

- **Rust service**: thread/user watches, PostgreSQL or SQLite persistence, incremental crawls, Bark and Feishu notifications, Markdown/ZIP exports, admin console, metrics, and operational documentation.
- **Standalone extension**: browser-only monitoring using the browser's NGA cookies; no server or database required.

The two modes are independent and do not share storage or release workflows.

## Server quick start

```bash
cd service
cp .env.example .env
export NGA_REMINDER__API_TOKEN='replace-with-a-long-random-token'
export NGA_REMINDER__ADMIN_PASSWORD='replace-with-a-long-random-password'
export NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY="$(openssl rand -base64 32)"
export NGA_REMINDER__DATABASE_BACKEND=sqlite
export NGA_REMINDER__SQLITE_PATH=./data/nga-reminder.db
cargo run -- all
```

Endpoints:

```text
GET /health
GET /ready
GET /metrics
GET /admin
```

For PostgreSQL, Docker Compose, Nginx TLS termination, backups, upgrades, and rollback, see the [Chinese documentation](README.md) and [`service/docs/OPERATIONS.md`](service/docs/OPERATIONS.md).

## Current status

The first server release has completed acceptance for milestones M0 through M7, including Bark push delivery and the M5 Markdown/ZIP and asset workflow. Remaining work is limited to optional production exercises and enhancements such as broader media attachment extraction, streaming exports, orphan cleanup, and richer admin views.

## Development

```bash
cd service
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

## Security

Never commit NGA cookies, API tokens, Bark device keys, or Feishu application secrets. Credentials are encrypted at rest and are not returned by list APIs. Back up the database and the configured assets directory together.

## License

[MIT](LICENSE)
