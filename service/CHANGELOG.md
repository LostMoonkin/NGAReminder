# Changelog

## Unreleased

### 机器人交互与 Cookie 续期（M8）

- 平台模型重构：新增 `platform_integrations`（平台连接持有应用凭据，`bot_enabled` 归属连接），重建
  `notification_channels`（引用连接 + 独立通知目标），同平台至多一个机器人连接的 partial unique index，
  平台级原子切换 API。
- 通用机器人模块：`bot/` 平台 adapter trait、标准 `BotEvent`、入站有界队列、`(integration, message_id)`
  持久化去重、命令 router/dispatcher、基于角色与私聊的授权、`bot_outbox` 投递（含短 TTL 图片）。
- 飞书第一个 adapter：长连接按 integration 协调、`im.message.receive_v1` 解析、回复/发送消息、
  事件回调不访问数据库。
- 命令集：`/help`、`/status`、`/bind`、`/watch list|run`、`/login status|confirm|captcha|cancel`；
  敏感登录交互仅限 owner 私聊；一次性绑定码（SHA-256 存储，10 分钟过期）。
- NGA Cookie 续期：`nga_account_renewal_settings` + `nga_login_sessions`（唯一活动会话）、
  `watch_targets.pause_reason`（auth 与 user 暂停区分）、`on_auth_failure` 全暂停并通知 owner、
  登录状态机（confirm → challenge → captcha → 候选 Cookie 验证 → 原子替换并恢复 auth 暂停监控）、
  失败冷却与凭据失效停用。
- `nga_web_login_v1` 登录协议适配器：RSA PKCS#1 v1.5 密码加密、验证码获取、`data[3]` 候选 Cookie
  提取（array/object 兼容）、`window.script_muti_get_var_store=` 包装解析、脱敏 fixture 驱动测试。
- 凭据加密升级 v2：AEAD AAD 绑定字段上下文（`nga_account:{id}:renewal_password:v2` 等），
  防止密文跨字段互换。
- 管理页面：通知中心拆分为「平台连接 / 通知目标 / 机器人授权」（连接状态、测试连接、平台级机器人
  原子切换、一次性绑定码、绑定管理）；服务设置新增「Cookie 自动续期」配置区与活动登录会话展示
  （取消按钮），续期配置校验 owner 私聊绑定。

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
