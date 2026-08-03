# Changelog

[中文](CHANGELOG.md)

## Unreleased

### Streaming exports, asset maintenance, and content management

- Changed Markdown exports to stable cursor pagination and HTTP streaming; ZIP exports are built incrementally in temporary files and delete those files when the response completes or is dropped.
- Added asset consistency scan and explicit cleanup APIs/UI for missing ready files, orphan metadata, and expired orphan/content temporary files.
- Added watched-UID result summaries, user post queries, and user Markdown/ZIP actions in the admin console.
- Added server-side safe HTML rendering for NGA markup in thread and user details.

### Bot interaction and Cookie renewal (M8)

- Refactored the platform model with `platform_integrations` for application credentials and bot ownership, rebuilt `notification_channels` around integrations and independent targets, added a partial unique index allowing at most one bot connection per platform, and added platform-level atomic switching APIs.
- Added the generic bot module: the `bot/` platform adapter trait, normalized `BotEvent`, a bounded inbound queue, durable `(integration, message_id)` deduplication, command routing/dispatch, role- and private-chat-based authorization, and `bot_outbox` delivery with short-TTL image support.
- Added the first Feishu adapter with integration-coordinated long connections, `im.message.receive_v1` parsing, replies and outbound messages; event callbacks do not access the database.
- Added `/help`, `/status`, `/bind`, `/watch list|run`, and `/login status|confirm|captcha|cancel`. Sensitive login interaction is restricted to owner private chats; one-time pairing codes are stored as SHA-256 hashes and expire after 10 minutes.
- Added NGA Cookie renewal with renewal settings and login sessions, distinct authentication/user pause reasons, owner alerts on authentication failure, a confirm → challenge → captcha → candidate-Cookie validation → atomic replacement state machine, failure cooldowns, and credential-invalid deactivation.
- Added the `nga_web_login_v1` login protocol adapter with RSA PKCS#1 v1.5 password encryption, captcha retrieval, array/object-compatible `data[3]` candidate-Cookie extraction, `window.script_muti_get_var_store=` wrapper parsing, and sanitized-fixture tests.
- Upgraded credential encryption to v2 with field-context-bound AEAD AAD such as `nga_account:{id}:renewal_password:v2`, preventing ciphertexts from being exchanged across fields.
- Updated the management UI with separate platform connection, notification target, and bot authorization areas, plus Cookie renewal settings and active login sessions.

### Frozen design

- Asset persistence is defined as database metadata plus SHA-256 content-addressed local files; binaries are not stored in PostgreSQL `BYTEA` or SQLite `BLOB`.
- Added `persistence.store_raw_payload`, disabled by default, so new posts retain normalized fields without storing the full source JSON unless explicitly enabled.

### M5 (completed)

- Added SQLite/PostgreSQL asset metadata and post-to-asset association tables.
- Added bounded inline-image discovery, HTTPS NGA host validation, pending download processing, and SHA-256 content-addressed local storage.
- Added `attachPrefix`/`attches` parsing so NGA attachment metadata enters the same local resource queue as inline images.
- Added NGA markup parsing/rendering for Markdown, including links, images, quotes, formatting, code blocks, line breaks, and unsafe-link rejection.
- Added protected thread/user Markdown and ZIP export endpoints with metadata and ready local assets.
- Added export, asset safety, renderer, ZIP, and idempotency tests.
- Completed external acceptance of the full Markdown/ZIP export and asset persistence workflow.

### Reliability

- Added handling for NGA Thread `code=51` pending-review responses. The affected crawl is skipped for the current cycle, cursors and notifications remain unchanged, and the next scheduled run retries automatically.

### M3

- Added PostgreSQL/SQLite user metadata and independent topic/reply cursors.
- Added typed user-topic, user-reply, and GBK profile parsing with inaccessible-entry and author filtering.
- Added user baselines and incremental discovery that persist only the watched UID's topic posts, individual replies, and available nested comments.
- Added the ten-attempt NGA busy policy with `skipped_busy` cursor preservation.
- Added user watch API/scheduling and shared post/event insertion deduplication with thread watches.

### M4 (completed)

- Added encrypted Bark/Feishu channels, TID/UID rules, transactional event matching, and a channel-deduplicated outbox.
- Added Bark V2 and Feishu enterprise-application interactive-card adapters.
- Feishu obtains and caches `tenant_access_token` from `app_id`/`app_secret`, refreshes rejected tokens once, and sends through `im/v1/messages` to configured chat or user IDs.
- Feishu cards extract NGA image markup, upload up to three trusted images with bounded streaming downloads, cache `image_key` values, and fall back to source links without blocking text delivery.
- Notification links now open the stored thread page at `#pid{pid}Anchor` instead of NGA's isolated-reply view.
- Added leased delivery processing, retry/dead-letter classification, delivery history, channel test sends, and channel/rule management APIs.
- Completed external Bark push acceptance together with the previously verified Feishu delivery workflow, including routing, deep links, outbox, and delivery results.

### M2

- Added PostgreSQL and SQLite schemas for threads, append-only posts, watches, cursors, crawl runs, and post events.
- Added typed NGA thread parsing for topics, replies, nested comments, and preserved attachment payloads.
- Added authenticated thread-page requests, account-level request spacing, transient retries, and typed NGA business errors.
- Added full baseline imports, floor-cursor incremental collection, global natural-key deduplication, watch leases, and automatic scheduling.
- Added thread watch CRUD and manual-run APIs.

### M1

- Added the Rust/Axum service skeleton, PostgreSQL and SQLite support, API/admin authentication, and encrypted NGA Passport credential management.
- Added the first responsive `/admin` management console with session login, overview cards, watch controls, notification channel/rule forms, content browsing, and export downloads.
- Added protected overview, thread/post query, event inbox, and event read-state APIs.
- Added persistent `post_events.read_at` state for marking one or all events as read.
- Account decryption failures now return a recoverable configuration state instead of a generic internal error; the management page guides the administrator to re-enter the NGA Cookie.
