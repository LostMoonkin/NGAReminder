# NGA Reminder

[中文主 README](README.md)

NGA Reminder provides NGA thread and user monitoring, persistent storage, notifications, exports, a web administration console, and Feishu bot interactions. It also includes a standalone Chromium extension that runs without the server.

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

## Docker image publishing

[`service-image.yml`](.github/workflows/service-image.yml) runs the Rust quality gates on pull requests that touch the service. It builds and publishes the service image to `ghcr.io/<owner>/<repository>` only on pushes to `main`, non-Standalone `v*` tags, or manual dispatch. It publishes branch/tag, semantic-version, commit-SHA, and `latest` (default branch) tags using the built-in GitHub Actions token.

## Version releases

The service and Standalone extension use independent versions. [`scripts/release.sh`](scripts/release.sh) updates the selected version, runs its quality gates, and creates an annotated tag:

```bash
scripts/release.sh service 0.1.2
scripts/release.sh extension 1.0.2
```

Service tags use `vX.Y.Z`; extension tags use `vX.Y.Z-standalone`. The script creates local commits and tags by default; add `--push` after review to push them to the remote.

## Current status

The first server release has completed acceptance for milestones M0 through M8, including Bark/Feishu delivery, Markdown/ZIP exports, the web console, Feishu bot interactions, and single-instance homeserver operations. Remaining enhancements include broader media attachment extraction, export golden fixtures, independent change detection for nested comments on old parent posts, PostgreSQL integration coverage, and ongoing production soak and restore exercises.

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
