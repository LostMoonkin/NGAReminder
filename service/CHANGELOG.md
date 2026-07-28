# Changelog

## Unreleased

### Design

- Frozen asset persistence as database metadata plus SHA-256 content-addressed local files. Asset
  binaries will not be stored in PostgreSQL `BYTEA` or SQLite `BLOB`.
- Added `persistence.store_raw_payload`, disabled by default, so new posts retain normalized fields
  without storing full source JSON unless explicitly enabled.

### M5 (completed)

- Added SQLite/PostgreSQL asset metadata and post-to-asset association tables.
- Added bounded inline-image discovery, HTTPS NGA host validation, pending download processing, and
  SHA-256 content-addressed local storage.
- Added `attachPrefix`/`attches` parsing so NGA attachment metadata enters the same local resource
  queue as inline images.
- Added NGA markup parsing/rendering for Markdown, including links, images, quotes, formatting,
  code blocks, line breaks, and unsafe-link rejection.
- Added protected thread/user Markdown and ZIP export endpoints with metadata and ready local assets.
- Added export, asset safety, renderer, ZIP, and idempotency tests.
- Completed external acceptance of the full Markdown/ZIP export and asset persistence workflow.

### Reliability

- Added handling for NGA Thread `code=51` pending-review responses. The affected crawl is skipped
  for the current cycle, cursors and notifications remain unchanged, and the next scheduled run
  retries automatically.

### M3

- Added PostgreSQL/SQLite user metadata and independent topic/reply cursors.
- Added typed user-topic, user-reply, and GBK profile parsing with inaccessible-entry and author
  filtering.
- Added user baselines and incremental discovery that persist only the watched UID's topic posts,
  individual replies, and available nested comments.
- Added the ten-attempt NGA busy policy with `skipped_busy` cursor preservation.
- Added user watch API/scheduling and shared post/event insertion deduplication with thread watches.

### M4 (completed)

- Added encrypted Bark/Feishu channels, TID/UID rules, transactional event matching, and a
  channel-deduplicated outbox.
- Added Bark V2 and Feishu enterprise-application interactive-card adapters.
- Feishu obtains and caches `tenant_access_token` from `app_id`/`app_secret`, refreshes rejected
  tokens once, and sends through `im/v1/messages` to configured chat or user IDs.
- Feishu cards now extract NGA image markup, upload up to three trusted images with bounded
  streaming downloads, cache `image_key` values, and fall back to source links without blocking
  text delivery.
- Notification links now open the stored thread page at `#pid{pid}Anchor` instead of opening NGA's
  isolated-reply view.
- Added leased delivery processing, retry/dead-letter classification, delivery history, channel
  test sends, and channel/rule management APIs.
- Completed external Bark push acceptance together with the previously verified Feishu delivery
  workflow, including routing, deep links, outbox, and delivery results.

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
## Unreleased

- Added the first responsive `/admin` management console with session login, overview cards,
  watch controls, notification channel/rule forms, content browsing, and export downloads.
- Added protected overview, thread/post query, event inbox, and event read-state APIs.
- Added persistent `post_events.read_at` state for marking one or all events as read.
- Account decryption failures now return a recoverable configuration state instead of a generic
  internal error; the management page guides the administrator to re-enter the NGA Cookie.
