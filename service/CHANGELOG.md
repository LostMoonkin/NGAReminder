# Changelog

## Unreleased

### M2

- Added PostgreSQL and SQLite schemas for threads, append-only posts, watches, cursors, crawl runs,
  and post events.
- Added typed NGA thread parsing for topics, replies, nested comments, and preserved attachment
  payloads.
- Added authenticated thread-page requests, account-level request spacing, transient retries, and
  typed NGA business errors.
- Added full baseline imports, floor-cursor incremental collection, global natural-key
  deduplication, watch leases, and automatic scheduling.
- Added thread watch CRUD and manual-run APIs.

### M1

- Added the Rust/Axum service skeleton, PostgreSQL and SQLite support, API/admin authentication, and
  encrypted NGA Passport credential management.
