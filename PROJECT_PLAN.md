# NGA Reminder 服务端项目计划

## 1. 项目目标

参考现有 `extension-standalone` Chromium 扩展中已验证的 NGA 请求方式，建设一个独立的
Rust 服务端，提供 NGA 内容抓取、持久化、通知、Markdown 导出和 Web 管理能力。持久化
支持 PostgreSQL 与 SQLite，由部署配置选择。

本计划不继承或恢复仓库历史中的旧服务端代码。新服务从独立的领域模型、数据库结构和
Rust 工程开始建设。服务端与 `extension-standalone` 是两个相互独立的工程，不共享运行
模式、存储或发布流程，也不规划扩展接入服务端。

### 1.1 核心需求

1. 按 TID 监控 NGA 主题，持久化主题、主楼、全部回复和可获取的楼中楼评论。
2. 按 UID 监控 NGA 用户，只持久化该用户自己的新主题、单条回复和可获取的楼中楼评论，
   不因该用户参与某个主题而回溯或持续保存主题内其他内容。
3. 新内容入库后按 TID、UID 或其组合匹配通知规则，并发送到 Bark、飞书国内版企业应用
   机器人。
4. 将持久化内容导出为 Markdown，支持单文件及带资源的 ZIP。
5. 提供独立 Web 管理页面，用于录入 NGA Cookie、管理监控目标、通知渠道和导出，并查看
   当前服务参数。

### 1.2 第一版范围

- 单实例、单用户，不提供用户注册、成员管理或租户隔离。
- 一套或多套 NGA 访问凭据，但所有凭据属于同一服务实例。
- PostgreSQL 或 SQLite 作为业务数据、任务状态和通知 outbox 的唯一持久化依赖；单个
  实例只能选择一种后端。
- HTTP 轮询 NGA；“实时通知”定义为一次轮询周期内发现并推送，目标周期 30～60 秒。
- 服务端提供 REST API；暂不依赖 Redis、Kafka 或独立任务队列。
- Rust 服务仅监听内部 HTTP，生产环境由前置 Nginx 负责 HTTPS 和反向代理。
- 是否将附件和正文图片保存到本地由服务端参数统一控制。
- Web 管理页面与 REST API 由同一服务提供。

### 1.3 非目标

- 不承诺恢复 NGA 已删除、隐藏或当前账号无权访问的内容。
- 不承诺恢复指定 UID 自注册以来的绝对完整历史。
- 不跟踪已入库回复的后续编辑或删除状态。
- 第一版不做 SaaS 多租户、公开注册、计费或复杂 RBAC。
- 第一版不提供全文搜索集群；先使用所选数据库的查询和索引。
- 不提供从 `extension-standalone` 导入配置、上传 Cookie 或迁移浏览器本地数据的能力。

## 2. 关键产品定义

### 2.1 “持久化所有回复”

对服务可访问的内容执行以下策略：

- 首次添加 TID 时回溯全部可访问页面。
- 持续抓取服务运行期间出现的新回复。
- 始终保存标准化字段；仅在 `persistence.store_raw_payload=true` 时额外保存原始响应。
- 已入库回复视为不可变历史记录，后续同步只 append 新发现的回复。
- 不主动重新抓取、更新或删除已持久化的回复。

### 2.2 “监控用户自己的发帖和回复”

分别抓取用户主题列表和用户回复列表：

- 用户主题列表中的可访问条目代表该 UID 新发布的主题，只补全并保存主楼。
- 用户回复列表中的 `__P` 代表该 UID 的单条回复，按 TID/PID 补全后保存。
- 不因发现某个 TID 而启动 Thread Collector，不抓取该主题的其他楼层或后续回复。
- 若同一帖子也被 TID watch 发现，复用同一 `posts` 记录和同一个新增事件。

由于 NGA 用户历史接口可能存在分页、时间窗口、权限和频率限制，产品承诺应表述为：

> 保存当前 NGA 凭据可以检索到的该用户历史主题和回复，并持续保存服务运行后发现的
> 该用户新主题与新回复；不承诺保存其参与主题中的其他内容。

用户监控功能进入开发前必须先完成真实账号接口探测。

### 2.3 通知匹配语义

通知过滤与内容持久化完全解耦。抓取器保存所有目标内容，通知规则只决定哪些事件发送到
哪些渠道。

支持的基础规则：

- `tid = X`：主题 X 的所有新内容。
- `uid = Y`：用户 Y 在所有主题中的新内容。
- `tid = X AND uid = Y`：用户 Y 在主题 X 中的新内容。
- 多个 TID、多个 UID。
- 第一版只有“新增主题”和“新增回复”事件。

这里的通知 `uid` 始终表示帖子作者 UID。一个帖子可能同时匹配 TID watch、UID watch 或
多条通知规则，但这些都是同一新增事件的多个触发来源，不是多条通知：

- 帖子自然键在全局唯一；只有实时阶段首次插入成功时创建一个 `post_event`。
- 每个 `(post_event_id, channel_id)` 最多创建一个 outbox 投递。
- 同一事件匹配到的全部 watch/rule 只记录为匹配来源，并合并为一次渠道通知。
- 无论 TID watch 还是 UID watch 先发现，后发现的一方都不得补发同一帖子。
- watch 首次历史回溯只持久化基线，不发送历史通知；完成基线后的新增内容才进入通知
  outbox。

## 3. 技术决策

### 3.1 技术栈

| 领域 | 选择 |
| --- | --- |
| 语言 | Rust stable |
| Web 框架 | Axum |
| 异步运行时 | Tokio |
| 持久化 | SQLx；PostgreSQL / SQLite |
| HTTP 客户端 | Reqwest |
| 序列化 | Serde / serde_json |
| 错误处理 | thiserror + anyhow |
| 日志与 tracing | tracing + tracing-subscriber |
| API 文档 | utoipa / OpenAPI |
| 生产入口 | Nginx 反向代理 |
| 编码转换 | encoding_rs |
| Secret 包装 | secrecy |
| 配置 | config + 环境变量 |
| 数据库迁移 | SQLx migrations |

### 3.2 部署形态

采用模块化单体，同一代码库提供三种运行角色：

```text
nga-reminder serve    # REST API
nga-reminder worker   # 抓取、通知、导出工作进程
nga-reminder all      # API + worker，默认单机模式
```

第一版使用 `all` 运行。PostgreSQL 部署需要横向扩展时，可把 API 与 worker 分开部署；
任务通过 PostgreSQL 行锁或 advisory lock 竞争。SQLite 部署限定为单个 `all` 进程，
不支持多 worker 横向扩展。

Rust 服务自身不终止 TLS。生产流量路径为：

```text
客户端 → HTTPS Nginx → HTTP Axum
```

### 3.3 持久化与任务协调

统一 repository 接口和领域模型，PostgreSQL/SQLite 使用各自 migration 目录。两套
migration 必须保持表语义、约束和版本一致。

```text
database_backend = postgres | sqlite
database_url = postgres://...        # PostgreSQL 时使用
sqlite_path = ./data/nga-reminder.db # SQLite 时使用
```

SQLite 自动创建数据库文件的父目录，启用 WAL、foreign keys 和 5 秒 busy timeout。
SQLite 面向单机单进程部署，不承诺多进程任务竞争。

PostgreSQL 的任务协调策略：

- `watch_targets.next_run_at` 表示下次抓取时间。
- worker 使用 `FOR UPDATE SKIP LOCKED` 领取到期任务。
- 抓取结果和 `post_events` 在同一事务提交。
- 通知采用 transactional outbox。
- 导出使用独立 `export_jobs` 表。
- 所有任务具备状态、重试次数、下次重试时间和最后错误。

## 4. 目标架构

```text
Web Management UI
    │
    │ REST API
    ▼
Axum API
    ├── Watch API
    ├── NGA Account / Settings API
    ├── 帖子/用户/主题查询 API
    ├── Notification Rule API
    ├── Export API
    └── 未读事件 API
              │
              ▼
    PostgreSQL / SQLite
              ▲
              │
Scheduler → Collector → NGA Client
              │
              ├── 主题采集器
              └── 用户采集器

帖子事件 Outbox → 规则匹配器 → Bark / 飞书
导出任务        → Markdown 渲染器 → .md / .zip
```

## 5. Rust 工程结构

第一阶段使用单 crate，避免过早拆分。模块边界稳定后可迁移为 Cargo workspace。

```text
service/
├── Cargo.toml
├── Dockerfile
├── migrations/
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── app.rs
│   ├── api/
│   ├── web/
│   ├── domain/
│   ├── nga/
│   │   ├── client.rs
│   │   ├── auth.rs
│   │   ├── thread_parser.rs
│   │   ├── user_parser.rs
│   │   └── fixtures/
│   ├── collector/
│   ├── repository/
│   ├── scheduler/
│   ├── notification/
│   │   ├── bark.rs
│   │   ├── feishu.rs
│   │   └── worker.rs
│   ├── export/
│   │   ├── markup.rs
│   │   ├── markdown.rs
│   │   └── assets.rs
│   └── observability/
└── tests/
```

## 6. 数据模型

### 6.1 核心实体

| 表 | 作用 |
| --- | --- |
| `nga_accounts` | NGA Cookie、账号状态、最近认证结果 |
| `nga_users` | UID、用户名和用户元数据 |
| `threads` | TID、标题、分区、作者、页数、回复数和 `partial/full` 采集覆盖状态 |
| `posts` | append-only 的主楼、回复和楼中楼记录 |
| `watch_targets` | thread/user 监控目标及检查间隔 |
| `watch_cursors` | 不同目标类型的增量游标 |
| `crawl_runs` | 抓取运行、耗时、请求数和错误 |
| `post_events` | 基线完成后新增主题/回复的入库事件 |
| `notification_channels` | Bark、飞书企业应用机器人配置 |
| `notification_rules` | TID/UID/事件类型匹配条件 |
| `post_event_matches` | 一个新增事件命中的 watch/rule 来源，用于解释去重后的通知 |
| `notification_outbox` | 待发送通知 |
| `notification_deliveries` | 每次投递和重试结果 |
| `export_jobs` | Markdown/ZIP 导出任务 |
| `assets` | 图片、音频、视频等资源的 URL、hash、MIME、大小、本地路径和下载状态 |
| `post_assets` | 帖子与资源的多对多关联、正文出现顺序和用途 |

### 6.2 帖子建议字段

```text
id                  UUID PRIMARY KEY
tid                 BIGINT NOT NULL
pid                 BIGINT
floor_number        INTEGER
post_kind           topic | reply | comment
parent_post_id      UUID
author_uid          BIGINT
author_name         TEXT
content_raw         TEXT
content_markdown    TEXT
published_at        TIMESTAMPTZ
raw_payload         TEXT NOT NULL DEFAULT '' # 仅配置开启时保存原始 JSON
first_seen_at       TIMESTAMPTZ
```

不能把 `pid` 单独作为主键，因为主楼可能出现 `pid = 0`。内部使用 UUID，并针对 NGA
自然标识建立组合唯一约束。

去重约束：

- 普通回复和楼中楼按经过 M0 fixture 冻结的 NGA 自然键全局唯一。
- 主楼按 `(tid, post_kind=topic)` 唯一，不能依赖 `pid=0`。
- `post_events` 按 `(post_id, event_type)` 唯一；第一版每个帖子只会产生一个新增事件。
- `notification_outbox` 按 `(post_event_id, channel_id)` 唯一，不包含 `rule_id`。
- `post_event_matches` 可记录多个 watch/rule 命中，但只关联到同一 outbox 投递。

原始响应存储由服务参数控制：

```text
persistence.store_raw_payload = false # 默认值
```

- `false`：`posts.raw_payload` 写入空字符串，只保存已标准化字段，减少数据库、WAL 和备份
  体积。
- `true`：保存该帖子对应的完整原始 JSON，主要用于接口诊断和 parser 兼容性分析。
- 环境变量为 `NGA_REMINDER__PERSISTENCE__STORE_RAW_PAYLOAD=true|false`。
- 参数只影响启用后的新插入记录；不会自动清空或回填历史 `raw_payload`。
- 无论是否保存原始 payload，附件元数据都应由 asset parser 写入结构化 `assets` /
  `post_assets`，不能依赖长期保留 `raw_payload`。

### 6.3 资源建议字段与存储边界

附件、正文图片、音频和视频的二进制内容不写入 PostgreSQL `BYTEA` 或 SQLite `BLOB`。
数据库只保存资源元数据、帖子关联和本地相对路径，二进制文件保存到
`assets.storage_path`。

```text
id                  UUID PRIMARY KEY
source_url          TEXT NOT NULL
content_hash        TEXT                 # SHA-256，下载并校验后写入
mime_type           TEXT
size_bytes          BIGINT
original_name       TEXT
local_relative_path TEXT                 # 只允许相对 assets.storage_path 的安全路径
download_status     pending | downloading | ready | failed | remote_only
http_status         INTEGER
last_error_kind     TEXT
first_seen_at       TIMESTAMPTZ
downloaded_at       TIMESTAMPTZ
```

存储约定：

- `assets.download_enabled=false` 时只保存 URL 和元数据，状态为 `remote_only`。
- `assets.download_enabled=true` 时下载到本地内容寻址目录，推荐布局为
  `<storage_path>/<sha256前2位>/<完整sha256>.<安全扩展名>`。
- `content_hash` 唯一；相同内容即使来自不同 URL 也只保存一份本地文件。
- `local_relative_path` 不保存绝对路径，不能包含 `..`，原始文件名不得直接作为磁盘路径。
- 下载必须限制协议、目标主机、重定向次数、响应大小和允许的 MIME 类型，防止 SSRF、
  路径穿越和磁盘耗尽。
- PostgreSQL 与 SQLite 使用相同资源模型和目录布局；切换数据库后端不改变资源文件。
- 极小的表情或缩略图也不做 BLOB 特例，保持备份、导出和存储行为一致。

文件落盘与数据库提交顺序：

1. 下载到 `assets.storage_path` 内的临时文件。
2. 流式计算 SHA-256，并校验响应大小、实际 MIME 和配置上限。
3. 使用同文件系统原子 rename 移动到内容寻址的最终路径；目标已存在时复用现有文件。
4. 在数据库事务中 upsert `assets` 和 `post_assets`。
5. 定期清理超时临时文件，并根据数据库引用计数清理孤儿文件。

数据库事务不能与文件系统组成真正的原子事务，因此 worker 必须允许幂等重试：文件已存在
但数据库未提交时重新 upsert；数据库为 `pending/downloading` 但文件缺失时重新下载。

备份和恢复必须同时覆盖数据库与整个 `assets.storage_path`。推荐先暂停资源下载 worker
或取得一致性快照，再备份数据库和资源目录；恢复后运行资源一致性检查，报告缺失文件和
孤儿文件。ZIP 导出只读取状态为 `ready` 且 hash/路径校验通过的本地资源。

### 6.4 密钥

- NGA Cookie、Bark device key、飞书 `app_secret` 不以明文出现在 API 响应或日志中。
- 数据库存储使用应用级加密，密钥仅由环境变量或 Secret Manager 注入。
- 配置导出不包含 Secret。
- NGA Cookie 通过 Web 管理页面录入；页面只显示是否已配置和脱敏摘要，不回显原值。

## 7. 抓取策略

### 7.1 主题采集器

首次回溯：

1. 请求第一页并验证 NGA 业务状态码。
2. 保存主题元数据和第一页内容。
3. 限流抓取剩余页面。
4. 批量插入 posts，唯一键冲突时忽略。
5. 提交游标和抓取统计。

增量同步：

1. 请求第一页取得当前页数、`vrows` 和主题状态。
2. 只有当前 `vrows` 大于已持久化楼层数时才进入新增回复抓取。
3. 根据最后持久化楼层计算覆盖新增楼层的最小页面范围。
4. 分页接口无法只返回页内新增部分时，边界页可能同时返回少量已存在回复；这些记录通过
   唯一键和 `ON CONFLICT DO NOTHING` 忽略，绝不更新原记录。
5. 事务内 append 新 posts、创建新增 events 并推进游标。

该策略不为发现编辑或删除而重拉历史页面，也不对已有回复执行 update。

### 7.2 用户采集器

验证并实现以下来源：

```text
GET /thread.php?authorid={uid}&__output=12&page={page}
GET /thread.php?searchpost=1&authorid={uid}&__output=12&page={page}
```

处理要求：

- `__output=12` 按 JSON 解析，并兼容 `text/json` Content-Type。
- 任何响应都先检查 HTTP 状态，再检查 NGA 顶层业务 `code`。
- 所有 NGA 数据请求统一保留以下已验证请求头，不再按接口自行删减：

```text
Content-Type: application/x-www-form-urlencoded
User-Agent: <configured user agent>
Accept: application/json, text/javascript, */*; q=0.01
Accept-Language: en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7
Cookie: <加密保存的完整浏览器 Cookie；仅主题抓取和账号自身查询可只用 Passport UID/CID>
Origin: https://bbs.nga.cn
Referer: https://bbs.nga.cn/
```

- 验证分页参数、最大历史范围和忙碌响应。
- 用户主题列表只接受可访问且 `authorid` 等于目标 UID 的条目，补全并保存该主题主楼。
- 用户回复列表只接受 `__P.authorid` 等于目标 UID 的单条回复，使用 TID/PID 补全。
- 不将用户列表发现的 TID 转换成 thread watch，也不请求主题全部历史页面。
- 仅由 user watch 产生的主题元数据标记为 `coverage=partial`；存在直接 TID watch 且完成
  回溯后升级为 `coverage=full`。
- 入库前再次校验补全结果的 `author.uid` 等于目标 UID，防止列表占位项或接口漂移导致
  保存其他用户内容。
- 用户抓取与 thread 抓取共享同一 `posts` 表和去重逻辑。
- 实时阶段只有 `INSERT ... ON CONFLICT DO NOTHING RETURNING id` 返回新记录时才创建
  `post_event`；基线回溯不创建通知事件，唯一键冲突也不再创建事件或通知。

用户主题或回复查询出现以下响应时，按 NGA 热点用户专用策略处理：

```text
(ERROR:2048) > 服务器忙,请稍后重试
```

- 对同一个请求固定间隔 1 秒重试，总尝试次数最多 10 次。
- 任意一次成功即继续处理本轮更新。
- 10 次全部返回 `ERROR:2048` 时，将本次 `crawl_run` 标记为 `skipped_busy` 并结束本轮。
- 跳过时不写入帖子、不生成通知事件、不推进用户游标。
- 等待该 watch 的正常下一个调度周期，不触发额外的通用指数退避重试。

### 7.3 限流与容错

- 全局账号级 token bucket。
- 每个请求配置 timeout。
- 429、5xx、网络错误使用指数退避和 jitter；空响应体也按 HTTP 层错误处理。
- HTTP 层错误与 HTTP 200 下的 NGA 业务码 `2048` 是两类错误，分别计数和重试。
- Thread 接口返回 `ERROR:51`（帖子正等待审核）时视为暂时不可用，跳过本轮抓取，不推进
  thread/user 游标、不写帖子或通知，并在 `crawl_run` 中记录 `skipped` 与
  `nga_pending_review`；下一调度周期自动重试。
- 认证失败暂停该账号任务，不做无限重试。
- 页面解析失败保存脱敏后的 fixture/摘要供诊断。
- 同一 TID/UID 同时只允许一个抓取任务。

### 7.4 NGA 接口契约

以下契约来自 `extension-standalone` 的现有调用、实际只读请求和
[`ngapost2md`](https://github.com/ludoux/ngapost2md) 的实现交叉验证。测试请求只需要
主题抓取和账号自身查询可只使用 `ngaPassportUid` 与 `ngaPassportCid`；跨用户搜索从保存的完整 Cookie 中提取 UID、可选编码用户名和 CID，并使用已验证的最小请求头。文档、fixture 和日志不得保存任何 Cookie 值。

| 用途 | 请求 | 当前验证状态 |
| --- | --- | --- |
| 主题分页 | `POST /app_api.php?__lib=post&__act=list`，form：`tid`、`page` | 已验证 HTTP 200、UTF-8 JSON、`code=0` |
| 按 PID 补全回复 | 同上，form：`tid`、`pid` | 已验证结果收敛为目标单条回复 |
| 用户主题列表 | `GET /thread.php?authorid={uid}&__output=12&page={page}` | 已验证成功响应、相邻页分页和不可访问占位项 |
| 用户回复列表 | `GET /thread.php?searchpost=1&authorid={uid}&__output=12&page={page}` | 已验证分页、字段和 `code=2048` 忙碌响应 |
| 用户资料 | `GET /nuke.php?func=ucp&uid={uid}` | 已验证为 GBK HTML；资料位于 `__UCPUSER` JSON 对象 |
| 附件资源 | 使用主题响应的 `attachPrefix`、帖子 `attches` 或正文资源 URL | 字段已确认，非空附件 fixture 已保存 |

主题分页成功响应的顶层关键字段：

```text
code, msg
currentPage, totalPage, perPage, vrows
fid, forum_name
tsubject, tauthor, tauthorid
attachPrefix, hot_post
result[]
```

`result[]` 每项表示主楼或普通回复，关键字段：

```text
tid, pid, fid, lou
postdate, postdatetimestamp
subject, content, type
author.uid, author.username
attches, comments
vote, vote_good, vote_bad
```

已验证的主题分页语义：

- `result` 按 `lou` 升序排列；第一页从 `lou=0` 的主楼开始。
- 主楼可出现 `pid=0`，因此持久化自然键不能只使用 `pid`。
- `perPage` 当前为 20；相邻完整页楼层连续且 PID 无重叠。
- 末页条数可以小于 `perPage`，必须以实际 `result.length` 为准。
- `vrows` 包含主楼，可在新回复出现后增长；`totalPage`、`vrows` 和末页内容应在同一次
  crawl 中作为一致的远端快照处理。
- 带 `tid`、`pid` 调用同一 `app_api` 接口时，已验证 `result` 仅返回目标回复，
  可用于用户回复列表发现 TID/PID 后的详情补全。

用户主题列表成功响应为 `{"code": 0, "result": {...}}`，关键字段：

```text
result.__T[]             # 主题摘要或不可访问占位项
result.__T__ROWS         # 当前页返回条数
result.__T__ROWS_PAGE    # 主题列表标称页容量，当前为 35
result.__ROWS            # 服务端报告的历史结果数
result.__F               # 关联分区数据
result.__CU              # 当前登录用户数据
result.__GLOBAL          # 页面级全局数据
```

用户主题分页和条目语义：

- 相邻两页 TID 无重叠；可访问主题按 `postdate` 降序排列。
- 可访问主题摘要包含 `tid`、`fid`、`authorid`、`subject`、`postdate`、`lastpost`、
  `replies`、`tpcurl` 等字段，但不包含 `__P` 回复详情。
- `__T` 可能混入 `denied=true` 且 `error` 非空的不可访问占位项。这类条目的作者和主题
  字段不可信，不作为帖子入库；只计入 crawl 诊断。
- 仅当条目可访问且 `authorid` 等于目标 UID 时，才请求该 TID 第一页补全主楼；响应中
  只保存 `lou=0` 且作者 UID 匹配的主楼，忽略同时返回的其他楼层。
- 最大页数按 `ceil(parse_int(__ROWS) / __T__ROWS_PAGE)` 计算并严格限制请求页码。
- 不能以 `__T__ROWS < __T__ROWS_PAGE` 判断末页：本次第一页只返回 30 条，但根据
  `__ROWS=41`、`__T__ROWS_PAGE=35` 仍存在第二页。
- `__ROWS` 可能包含未实际返回的隐藏或不可访问记录，只用于计算待请求页数，不能据此
  承诺绝对完整历史。

楼中楼与附件约定：

- 楼中楼由父回复内嵌的 `comments` 集合承载，不规划独立评论列表接口。
- 已验证 `comments` 为数组，评论 `pid` 非零且与普通回复 PID 不重叠，`lou` 固定为 0。
- `parent_post_id` 来自 JSON 包含关系；`comment_to_id` 既可能是父 PID，也可能是被回复
  用户 UID，只作为原始元数据保存。
- 主楼自然键为 `(tid, topic)`，普通回复和楼中楼评论自然键统一为 `(tid, pid)`。
- 附件元数据位于 `attches`，资源基址来自 `attachPrefix`；正文中的图片、音视频 URL
  也交给统一资源解析器处理。
- 已验证 `attches` 为数组，`attachurl` 和 `path` 为相对地址；
  `attachPrefix + attachurl` 的样本资源 HEAD 请求返回 HTTP 200、`image/jpeg`。
- 已验证非空 `hot_post` 与 `result` 存在重复 PID；hot post 只建立对规范 post 的引用，
  不重复插入帖子或事件。
- 服务启动参数 `assets.download_enabled` 控制是否将附件和正文图片保存到本地：
  - `false`：只保存资源元数据和远程 URL，Markdown 引用远程资源。
  - `true`：二进制下载到 `assets.storage_path` 的内容寻址目录；数据库仍只记录 URL、
    SHA-256、MIME、大小、本地相对路径、下载状态和错误，不保存 BLOB。
- `assets.download_enabled` 和 `assets.storage_path` 由配置文件或环境变量提供，管理页面
  只读展示当前生效值；从 `false` 切换为 `true` 并重启后，只保证新发现资源进入下载
  队列，历史资源回填通过独立维护任务显式触发。
- 非空 `comments`、`attches` 和 `hot_post` 已保存为脱敏 fixture。

用户资料不是发现发帖和回复的必要依赖，只用于补全用户元数据。页面需先从 GBK 转成
UTF-8，再提取 `__UCPUSER = {...};` 中的 JSON；禁止执行页面 JavaScript。已观察字段包括
`uid`、`username`、`groupid`、`avatar`、`regdate`、`lastpost`、`posts` 和 `sign`。

## 8. 通知系统

### 8.1 标准通知事件

```text
event_id
event_type
thread
post
author
content_preview
canonical_url
occurred_at
```

### 8.2 渠道接口

Rust 中定义统一 trait：

```rust
trait NotificationSender {
    async fn send(&self, notification: &Notification) -> Result<DeliveryReceipt>;
}
```

实现：

- Bark：使用 V2 `POST /push`，提供 title、body、group、level 和跳转 URL。
- 飞书：使用企业自建应用的 `app_id`、`app_secret` 获取 `tenant_access_token`，再通过
  `im/v1/messages` 向指定 `chat_id`、`open_id` 或 `user_id` 发送 interactive card。
- 飞书 token 按接口返回的有效期缓存在服务内存中并提前刷新；业务响应拒绝 token 时清除
  缓存并重新鉴权一次。token 不写入数据库。
- 后续渠道只新增 adapter，不修改规则引擎。

### 8.3 投递保证

- 数据库事务提交后才允许发送。
- `(post_event_id, channel_id)` 唯一，保证同一帖子在同一渠道只发送一次。
- TID、UID、TID+UID 规则可以同时匹配，但仅追加 `post_event_matches` 来源，不重复创建
  outbox。
- Thread Collector 与 User Collector 并发发现同一帖子时，由 `posts` 自然唯一约束决定
  唯一插入者；只有唯一插入者创建 `post_event`。
- 后续 collector 或新规则再次命中已经存在的帖子时不补发历史通知。
- 保存 HTTP 状态、响应摘要、attempt 和下次重试时间。
- 区分可重试错误和永久错误。
- 支持渠道级暂停和手工测试通知。

## 9. Markdown 导出

### 9.1 转换原则

参考 `ngapost2md` 的 NGA 标记处理能力，但用 Rust 重新实现并配套固定样例测试。

转换管线：

```text
content_raw
  → NGA 标记 tokenizer
  → 中间 AST
  → Markdown 渲染器
  → 资源解析器
```

优先支持：

- 换行、URL、图片、表情。
- 引用和引用楼层链接。
- 删除线、粗体等基础格式。
- 音频、视频降级链接。
- 楼中楼。
- 匿名和用户名展示。

数据库原始内容永远保留。`content_markdown` 是可重新生成的派生数据。

### 9.2 导出格式

- Thread 导出：`coverage=full` 时导出完整主题；`partial` 时明确标注仅包含已持久化内容。
- User 导出：只导出该 UID 自己的主题主楼和单条回复，可按时间或所属主题分组。
- `assets.download_enabled=false` 时 Markdown 引用原始资源 URL。
- `assets.download_enabled=true` 时优先引用本地资源，并可生成包含资源的 `.zip`。
- 导出器不得直接信任数据库路径；读取前必须确认规范化路径仍位于
  `assets.storage_path` 内，并可按 SHA-256 选择性校验文件。
- 附带 `metadata.json`，记录 TID、UID、导出时间和游标。

## 10. REST API 草案

```text
GET    /health
GET    /ready

GET    /api/v1/settings
GET    /api/v1/nga-account
PUT    /api/v1/nga-account
POST   /api/v1/nga-account/test

GET    /api/v1/watches
POST   /api/v1/watches/threads
POST   /api/v1/watches/users
PATCH  /api/v1/watches/{id}
DELETE /api/v1/watches/{id}
POST   /api/v1/watches/{id}/run

GET    /api/v1/threads
GET    /api/v1/threads/{tid}
GET    /api/v1/threads/{tid}/posts
GET    /api/v1/users/{uid}
GET    /api/v1/users/{uid}/authored-threads
GET    /api/v1/users/{uid}/authored-posts
GET    /api/v1/posts/{id}

GET    /api/v1/channels
POST   /api/v1/channels
PATCH  /api/v1/channels/{id}
POST   /api/v1/channels/{id}/test

GET    /api/v1/notification-rules
POST   /api/v1/notification-rules
PATCH  /api/v1/notification-rules/{id}
DELETE /api/v1/notification-rules/{id}

GET    /api/v1/events
POST   /api/v1/events/{id}/read
POST   /api/v1/events/read-all

POST   /api/v1/exports
GET    /api/v1/exports/{id}
GET    /api/v1/exports/{id}/download
```

第一版为单用户部署。Web 管理页面使用单一管理员登录态，脚本调用使用固定 API token 和
`Authorization: Bearer ...`。Axum 只提供内部 HTTP，生产环境由前置 Nginx 提供公网
HTTPS、反向代理和请求体大小等入口限制。

## 11. Web 管理页面

### 11.1 页面范围

- 登录和服务健康状态。
- NGA Cookie 录入、脱敏状态展示和连通性测试。
- TID/UID watch 的创建、暂停、立即运行和运行状态。
- 用户 watch 保存的新主题和单条回复列表。
- Bark、飞书国内版企业应用机器人和通知规则管理。
- 主题、用户、回复和未读事件查询。
- Markdown/ZIP 导出任务及下载。
- 只读展示 `assets.download_enabled`、`assets.storage_path` 等服务启动参数。

NGA Cookie 页面允许粘贴完整 Cookie 字符串，后端加密保存完整 Cookie，并单独提取
`ngaPassportUid`、`ngaPassportCid` 用于认证状态与兼容流程。保存响应和后续页面均不回显原值，连通性测试只返回
成功状态、账号 UID 和脱敏错误。

### 11.2 与 Chromium 扩展的边界

`extension-standalone` 与 Rust 服务是两个独立工程：

- 服务端不提供 extension/service 双模式。
- 扩展不调用本服务 API，本服务也不读取扩展的 `chrome.storage` 或 Cookie。
- 两个工程不共享配置迁移、发布包、版本号或回归测试。
- 扩展代码仅作为已验证 NGA 请求行为的参考，服务端实现拥有自己的 fixture 和测试。

### 11.3 NGA Cookie 失效告警与人工恢复

为避免浏览器脚本读取并上传认证 Cookie，服务端不增加 Cookie 自动同步脚本。服务端发现
NGA Passport 凭据失效时，采用“告警通知 + 管理台人工更新”的恢复流程：

1. thread/user worker 收到 NGA 未授权响应后，将 `nga_accounts.status` 标记为 `paused`，并暂停
   相关 watch；
2. 同一失效周期只生成一次 `nga_credentials_invalid` 系统告警，避免每个轮询周期重复通知；
3. 系统告警进入独立的持久化通知队列，沿用已配置的 Bark/飞书渠道和重试、租约、投递记录；
4. 通知正文只包含“Cookie 已失效，请登录管理台更新 Cookie”和管理台链接，不包含任何凭据；
5. 用户在管理台重新粘贴 Cookie 后点击“测试 NGA 连接”；验证成功后关闭当前系统告警并显示
   恢复提示；
6. 已暂停的 watch 不自动恢复，用户确认凭据有效后手动重新启用，避免错误凭据继续请求 NGA。

系统告警必须与帖子事件通知解耦，不能伪造 `post_events` 记录。告警必须支持跨 SQLite/PostgreSQL
迁移、重启后继续投递、失败重试和按告警类型去重。认证失败、告警入队和账号恢复均不得记录
Cookie 原文或可逆的日志摘要。

验收标准：

- 任一 worker 首次检测到 NGA 未授权时，账号和对应 watch 进入暂停状态，并为所有启用通知渠道
  生成一条系统告警；
- 后续轮询不重复生成同一未解决告警；
- 通知 worker 可将系统告警通过 Bark/飞书投递，失败时按现有退避规则重试；
- 管理台测试新 Cookie 成功后，账号状态恢复为 `valid`，系统告警标记为已解决；
- Cookie 失效期间、告警内容、API 响应和日志均不出现 Cookie 原值。

## 12. 里程碑

### M0：接口验证与规格冻结

目标：证明 NGA 数据源可用，冻结抓取契约。

已完成的初步验证：

- 主题分页接口为
  `POST /app_api.php?__lib=post&__act=list`，form 参数为 `tid`、`page`。
- 主题响应顶层包含分页和主题元数据，`result` 包含主楼、回复以及可选的内嵌
  `comments`；已验证第一页、相邻页和末页的楼层顺序及分页边界。
- 主楼已观察到 `lou=0`、`pid=0`；相邻完整页各 20 条且 PID 无重叠，末页可不足 20 条。
- 同一主题在两次探测间 `vrows` 从 8005 增长到 8006，末页出现新楼层，验证了
  append-only 增量判断所需的远端信号。
- 按 PID 补全接口复用主题分页接口，form 参数改为 `tid`、`pid`；已验证成功响应的
  `result` 仅含目标回复。
- 用户主题接口确定为
  `GET /thread.php?authorid={uid}&__output=12&page={page}`；已验证第一页和第二页成功响应、
  TID 无重叠并按 `postdate` 降序。
- 用户主题响应的 `result.__T` 同时包含可访问主题和 `denied=true` 的不可访问占位项；
  可访问项才可转换为 TID 候选。
- 用户主题列表不包含 `__P` 回复详情，发现 TID 后仍需调用主题分页接口补全主楼。
- 当前用户主题 `__ROWS=41`、`__T__ROWS_PAGE=35`，因此应抓取两页；第一页虽然只实际
  返回 30 项，也不能提前停止。
- 用户回复接口使用
  `GET /thread.php?searchpost=1&authorid={uid}&__output=12&page={page}`。用户提供的精确浏览器 cURL 证明 `page=1` 可稳定返回 JSON；服务必须同时对齐完整 Cookie 和浏览器导航请求头，不能把不等价请求的 503 归因于分页参数或限流。
- `__output=12` 返回 `text/json;charset=UTF-8`，响应可使用 gzip 压缩。
- HTTP 200 不代表业务成功，必须继续判断顶层 `code`。
- 成功响应为 `{"code": 0, "result": ...}`；`result.__T` 是结果数组。
- 每个 `result.__T` 元素包含主题摘要，命中的用户回复位于 `__P`，字段包括
  `tid`、`pid`、`authorid`、`postdate`、`subject`、`content` 和 `type`。
- `result.__R__ROWS_PAGE` 表示用户回复结果每页容量，当前验证值为 20。
- 真实回复响应可返回 `result.__ROWS=null`；此时以满页继续、短页停止的方式扫描到已保存边界。
- `result.__T` 可混入 `__P.postdate=""` 的占位记录；缺少有效 TID/PID/时间的条目直接跳过。
- `page=2` 能返回下一页结果；已验证相邻两页 PID 无重叠且按 `postdate` 降序排列。
- 用户列表请求只携带 `User-Agent` 和最小 `Cookie`；Cookie 仅保留 UID、可选编码用户名和 CID，
  不额外添加 `Accept`、`Accept-Language`、`Origin`、`Referer`、`Sec-Fetch-*` 或 `sec-ch-ua*`。
- 2026-08-03 复测确认：跨用户搜索只需 `ngaPassportUid`、可选 `ngaPassportUrlencodedUname`、
  `ngaPassportCid` 和 User-Agent；两个 Accept 头、`C3VK` 及其他浏览器导航头不影响结果。显式 `page=1` 是已验证的正确请求。
- 已实际观察到 HTTP 200 + `{"code": 2048, "msg": "服务器忙,请稍后重试"}`；
  一次测试中前三次返回 2048，第 4 次成功，符合每秒重试、最多 10 次的处理策略。
- 用户资料接口 `GET /nuke.php?func=ucp&uid={uid}` 返回 GBK HTML，可在转码后安全提取
  `__UCPUSER` JSON，不需要执行脚本。
- 已实际验证非空楼中楼、附件和 `hot_post`：
  - 楼中楼 `comments` 是父回复内嵌数组，评论 PID 非零且样本内唯一。
  - `comment_to_id` 具有父 PID/用户 UID 多态语义，不能作为父外键。
  - `attches` 是数组，相对 `attachurl` 可与 `attachPrefix` 拼接并成功访问图片。
  - `hot_post` 与普通 `result` PID 重叠，必须引用同一规范帖子。
- 无效 TID 返回 HTTP 200 + `code=14`；缺失 Passport Cookie 返回 HTTP 200 +
  `code=46`，均已保存脱敏 fixture。
- Thread 接口已验证存在 HTTP 200 + `code=51` 的待审核响应；该响应按跳过本轮策略处理，
  不应被当作永久不存在或认证失败。
- 失效 Passport Cookie 同样返回 HTTP 200 + `code=46`。
- 无效 UID 的资料页返回 HTTP 200、GBK HTML 但不包含 `__UCPUSER`；主题列表探测返回
  空体 HTTP 503，因此新增 UID watch 先通过资料页校验存在性。有效 UID 的跨用户回复搜索也可能返回相同 503，采集器必须报 `nga_user_search_unavailable` 且不推进游标。
- 已建立 `service/docs/NGA_API_CONTRACT.md` 和 `service/tests/fixtures/nga/`。
- 本次验证未将真实 Cookie 或原始用户内容写入仓库。

状态：完成。权限不足样本按产品决策延后，不阻塞 M0。

验收：

- 至少一个多页主题可完整解析。
- 至少一个 UID 的主题列表和回复列表可分页解析。
- 至少一个主题的楼中楼与附件可解析。
- fixture 不包含可恢复的 Cookie。
- 明确“历史全部”的实际可达边界。

预计：3～5 个开发日。

### M1：Rust 服务骨架与双持久化后端

状态：完成。

已实现并验证：

- `serve`、`worker`、`all` 三种运行角色，支持 SIGTERM/Ctrl-C 优雅退出。
- Axum health/readiness、固定 API token、单管理员 HttpOnly 会话。
- SQLx AnyPool、PostgreSQL/SQLite 独立 migration、Dockerfile、Compose 和 Nginx
  反向代理示例。
- 配置可选择 PostgreSQL URL 或 SQLite 文件路径；SQLite 已验证自动建目录、WAL 和
  migration。
- 管理页可粘贴完整 Cookie 或分别录入 Passport 字段；后端为跨用户监控加密保存完整 Cookie，
  并提取 `ngaPassportUid`、`ngaPassportCid`，全部使用 AES-256-GCM 和随机 nonce 加密保存。
- 自动续期收集本次登录的完整 cookie jar，验证成功后与旧 Cookie 合并并写入 `cookie_encrypted`；
  即使旧账号只有 UID/CID，也不得让完整 Cookie 列保持 `NULL`。
- Cookie 查询 API 只返回脱敏 UID、认证状态和检查时间，不回显凭据。
- 连通性测试使用用户回复接口判断认证状态；用户资料页不用于认证，因为已验证无效 CID
  仍可能正常取得公开资料。
- 用户回复接口 `code=2048` 已区分“服务器忙”和“必须登录”；忙碌时每秒重试，最多
  10 次。
- M0 thread/user/busy/comments/attachments fixtures 已纳入 Rust contract tests。

验证结果：

- `cargo fmt --check`、`cargo check`、严格 Clippy 和 12 项测试通过。
- Compose release 镜像构建、migration、health、ready、API token、管理员登录、
  Cookie 密文存储和脱敏读取均通过实测。
- PostgreSQL 与指定文件路径的 SQLite 均已实际启动并通过 readiness、migration、
  加密写入和脱敏读取；SQLite 测试文件可在停止服务后正常清理。
- 虚构 Passport CID 被连通性测试正确拒绝并记录为 `unauthorized`。

任务：

- 初始化 `service/`。
- Axum health/readiness。
- 配置、日志、优雅退出。
- SQLx pool 和 PostgreSQL/SQLite 两套首批 migrations。
- Dockerfile 和本地 compose。
- Nginx 反向代理示例配置；Axum 默认只绑定回环或内部网络地址。
- 单管理员登录态和 API token 鉴权。
- NGA Cookie 加密存储、录入 API、最小管理页面和连通性测试。

验收：

- PostgreSQL 模式可通过 Compose 一条命令启动；SQLite 模式无需外部数据库。
- migrations 自动执行或有明确命令。
- health、ready、鉴权和基础集成测试通过。
- 用户可从最小管理页面录入 NGA Cookie；服务端加密保存完整 Cookie 和两个 Passport 值且不回显。

预计：4～6 个开发日。

### M2：主题全量与增量持久化

状态：完成。

已实现并验证：

- PostgreSQL/SQLite 两套语义一致的 thread、append-only post、watch、cursor、crawl run
  和帖子事件 migration。
- 强类型 thread parser 覆盖主楼、普通回复、楼中楼、`hot_post` 去重边界及附件原始
  payload 保留。
- NGA thread HTTP client 统一使用已验证请求头；跨用户查询传完整 Cookie，其他请求兼容 Passport UID/CID，并实现
  账号级 500ms 请求间隔、超时、429/5xx/网络错误退避重试及业务码分类。
- 新 watch 首次抓取全部可访问页面并建立无事件基线；后续仅抓取覆盖新楼层的页面范围，
  通过楼层游标和自然唯一键追加新记录。
- `posts` 不执行 update/delete；旧楼远端内容即使变化也不会覆盖数据库历史。
- 只有实时阶段 `INSERT ... ON CONFLICT DO NOTHING` 真正成功时才创建唯一
  `post_event`；重复抓取不重复生成事件。
- worker 每 2 秒领取到期 watch；PostgreSQL 和 SQLite 均使用带过期时间的 lease，
  进程中断后可重新领取，遗留运行记录标记为 `lease_expired`。
- 已提供 thread watch 列表、新建、修改、删除和立即运行 API。
- 无效 TID 会停止并禁用 watch；认证拒绝会暂停账号和 watch，避免无限重试。

验证结果：

- SQLite 集成测试覆盖“基线 → 新增回复 → 重复轮询”，确认基线 0 事件、新回复单事件、
  旧内容不覆盖和自然键去重。
- watch CRUD、重复 TID 冲突、暂停/恢复和级联删除均有 SQLite 集成测试。
- 在用户提供的 PostgreSQL 实例上实际执行 M2 migration，并验证 create/list、
  duplicate 409、patch、delete；测试 watch 已清理。
- `cargo test --all-targets` 20 项测试通过，严格 Clippy 通过。

任务：

- NGA HTTP client、Cookie 和错误模型。
- Thread/page/post 解析。
- 全量回溯。
- 基于楼层游标的 append-only 增量同步。
- crawl_runs、post_events。
- watch CRUD 和手工 run API。

验收：

- 新增 TID 后可完成全量抓取。
- 首次全量回溯只建立基线，不发送历史回复通知。
- 重跑不会产生重复帖子。
- 新回复在一个轮询周期内入库。
- 已入库回复不会被后续抓取覆盖或删除。
- 中断后可继续同步。

预计：5～8 个开发日。

### M3：用户监控

状态：完成。

已实现并验证：

- PostgreSQL/SQLite 同步新增 `nga_users` 和独立 `user_watch_cursors` migration。
- 用户主题、用户回复列表强类型 parser；有 `__ROWS` 时按 `__ROWS/page_size` 计算，
  `__ROWS=null` 时按满页/短页继续扫描；`denied=true`、缺少有效时间的占位项和非目标 UID 条目不会成为候选。
- GBK 用户资料安全解码，只截取并解析 `__UCPUSER` JSON，不执行页面脚本。
- 用户主题候选仅补全并保存作者 UID 匹配的主楼；用户回复候选按 TID/PID 补全并二次
  校验详情作者，只保存该单条回复及其可获取楼中楼。
- user watch 基线完整遍历当前可检索列表但不产生事件；增量列表遇到已保存时间/PID/TID
  边界后停止，详情只请求新候选。
- 用户列表 `code=2048 + 服务器忙` 固定每秒重试，总次数 10；全部失败将 crawl 标记为
  `skipped_busy`，释放 lease，但不完成基线、不写帖子且不推进用户游标。
- thread/user 使用同一 `insert_post`、`insert_event` 和数据库自然唯一约束；用户发现的
  主题标记为 `coverage=partial`，不会启动整帖抓取。
- worker 可调度 thread/user 两类 watch；新增 user watch API，并复用修改、删除和立即
  运行接口。

验证结果：

- fixture tests 覆盖用户主题页数、不可访问项过滤、目标 UID 回复过滤和 GBK 资料解析。
- SQLite 事务测试覆盖“用户基线 → 实时单条回复 → 重复发现”：基线 0 事件，实时回复
  1 个事件，重复发现 0 插入/0 事件，主题保持 `coverage=partial`。
- 在 `nga_reminder_dev` 实际执行 M3 migration，并验证 user watch create/list/delete；
  临时 watch 已清理。
- `cargo test --all-targets` 25 项测试、严格 Clippy 和 release build 均通过。

任务：

- 用户主题和回复列表解析。
- 用户历史回溯游标。
- 用户新主题主楼补全和单条回复 TID/PID 补全。
- 补全结果作者 UID 二次校验。
- 与 Thread Collector 共用 posts/event 创建事务和自然键去重。
- user watch API。

验收：

- 新增 UID 后只保存当前凭据可检索到的该用户主题主楼和单条回复。
- 首次用户历史回溯只建立基线，不推送历史主题或回复。
- 用户新发主题或回复在一个轮询周期内入库。
- 不会因用户回复某个主题而保存该主题的其他楼层或后续回复。
- 同一帖子由 thread/user 两条路径发现时只保存一次。
- 无论 thread/user 哪条路径先发现，同一通知渠道只收到一次通知。
- 用户列表查询返回 `ERROR:2048` 时按 1 秒间隔最多尝试 10 次；全部失败后不推进游标。
- 用户主题列表中的不可访问占位项不会生成帖子或错误的 TID 候选。
- 繁忙、权限不足和历史截断有可观测状态。

预计：5～10 个开发日，取决于 M0 结果。

### M4：通知规则、Bark 与飞书

状态：验收完成。Bark 推送与飞书企业应用 OpenAPI 均已完成外部投递验收。

已实现并验证：

- PostgreSQL/SQLite 同步新增渠道、规则、事件匹配来源、outbox 和 delivery migration。
- 渠道配置使用现有 AES-256-GCM 加密后入库，列表 API 不返回 device key、`app_id`、
  `app_secret` 或接收目标。
- 新 `post_event` 在原数据库事务中匹配 TID、UID、TID+UID 规则，同时写入
  `post_event_matches` 和事务 outbox。
- `(post_event_id, rule_id)` 保留全部匹配来源，`(post_event_id, channel_id)` 唯一约束
  保证多条规则命中同一渠道时只投递一次。
- Bark V2 POST adapter 支持标题、正文、group 和帖子跳转 URL。
- 飞书国内版企业应用机器人 adapter 使用 `tenant_access_token` 和
  `im/v1/messages` 发送 interactive card，支持群聊和单聊接收 ID。
- 飞书 tenant token 依据服务端有效期在内存缓存并提前刷新，API 拒绝 token 时自动重新
  鉴权一次；持久化层只保存加密后的应用配置，不保存 token。
- 飞书卡片从完整回复中提取 `[img]`，最多下载并内嵌 3 张可信 NGA 图片；下载限制为
  10 MB，`image_key` 按应用与源 URL 缓存。下载、上传或域名校验失败时降级为原图链接，
  不阻塞文字通知。
- 回复跳转链接使用持久化页码生成
  `read.php?tid={tid}&page={page}#pid{pid}Anchor`，在完整主题页内定位目标回复。
- 通知 worker 使用 lease 领取 outbox；网络错误、429、5xx 最多重试 5 次，采用
  30 秒、2 分钟、10 分钟、30 分钟退避；4xx/配置错误直接进入 dead。
- 每次尝试记录 HTTP 状态、截断后的响应摘要和错误类型；成功记录 delivered 时间。
- 已提供渠道、规则的创建/列表/启停/删除 API，以及渠道测试通知 API。

验证结果：

- SQLite 幂等测试中，同一事件同时命中 TID 和 UID 两条规则：保留 2 条 match，但同一
  渠道只有 1 条 outbox；重复匹配仍保持 1 条 outbox。
- Bark 默认配置、飞书配置/卡片编码和 429/5xx/4xx/API 限流重试分类具备单元测试。
- 在 `nga_reminder_dev` 实际执行 M4 migration，验证加密渠道与规则
  create/list/cascade-delete；临时记录已清理。
- 已使用企业应用 `app_id`、`app_secret` 和目标群 `chat_id` 调用真实飞书 OpenAPI，
  获取 tenant access token 并成功发送群消息；测试密钥随后应轮换。
- Bark 推送已通过真实渠道测试和通知链路验收，包含跳转链接、规则匹配、outbox 和 delivery
  结果验证。

任务：

- notification rules。
- transactional outbox。
- Bark adapter。
- 飞书国内版企业应用机器人 OpenAPI adapter。
- 投递重试、幂等、测试通知。

验收：

- TID、UID、TID+UID 三类规则正确匹配。
- 同一事件同时匹配 TID 和 UID 规则时，每个渠道只生成一个 outbox 和一次投递。
- 先由 UID watch、后由 TID watch 发现以及反向顺序均不会重复通知。
- Bark 和飞书均可收到含跳转链接的通知。
- 模拟失败后可重试。
- worker 重启不会重复发送已成功通知。

预计：4～7 个开发日。

### M5：Markdown 导出

状态：验收完成。

已实现并验证：

- PostgreSQL/SQLite 新增 `assets`、`post_assets` migration，正文 `[img]` 以及 NGA
  `attachPrefix`/`attches` 附件进入结构化元数据。
- `assets.download_enabled`、内容大小限制和 `assets.storage_path` 配置已接入；受信任 HTTPS
  NGA 图片进入 pending 队列，下载后按 SHA-256 内容寻址落盘。
- 实现 NGA markup AST/Markdown renderer，覆盖加粗、斜体、下划线、删除线、引用、链接、
  图片、代码块和换行，并拒绝不安全链接。
- 新增 thread/user Markdown 与 ZIP 导出 API；ZIP 包含 Markdown、`metadata.json` 和已就绪
  的本地资源，数据库路径经过相对路径安全校验。
- 已通过资源幂等、路径穿越、渲染、Markdown/ZIP 导出和全量回归测试。

后续增强：

- 继续扩展 NGA `attches` 中音频、视频等附件元数据的覆盖范围。
- 大主题分页流式 Markdown、磁盘临时 ZIP、资源缺失检查和孤儿清理已于 2026-08-03
  完成；后续继续补充更多媒体类型和 golden fixtures。

任务：

- NGA markup tokenizer/AST。
- Markdown renderer。
- thread/user 导出。
- `assets.download_enabled` 和本地资源下载队列。
- SHA-256 内容寻址落盘、跨 URL 去重、临时文件原子 rename 和失败幂等恢复。
- `assets`/`post_assets` 元数据、缺失文件检查及临时/孤儿资源清理任务。
- 远程资源 Markdown 与本地资源 ZIP 两种输出。
- `ngapost2md` 行为 fixture/golden tests。

验收：

- 中文、引用、URL、图片、表情和楼中楼正确渲染。
- 关闭附件保存时不产生本地资源文件；开启时新资源进入下载队列并可打包导出。
- 附件二进制不进入 PostgreSQL/SQLite；重复内容只产生一个本地文件。
- 下载中断后可安全重试，资源检查可报告缺失文件并清理超过保留期的临时/孤儿文件。
- 大主题以流式方式导出，不把全部内容加载到内存。
- 相同数据库快照可重复生成一致结果。

预计：5～9 个开发日。

### M6：Web 管理页面

状态：验收完成。

任务：

- 单管理员登录页。
- NGA Cookie 录入、脱敏展示和连通性测试。
- thread/user watch、用户自身发帖回复和运行状态 UI。
- Bark、飞书企业应用机器人和通知规则 UI。
- 主题、回复、未读事件和导出 UI。
- 附件本地保存参数和其他服务配置的只读状态 UI。

已实现：

- `/admin` 已替换为响应式单页管理台，采用零构建依赖的原生 HTML/CSS/JavaScript，随 Axum
  服务一起发布。
- 已接入管理员会话登录、凭据脱敏展示、Cookie 加密保存和 NGA 连通性测试。
- 已接入 thread/user watch 的新增、启停、删除和立即运行，以及运行状态展示。
- 已接入 Bark/飞书渠道的新增、启停、测试、删除和通知规则的新增、启停、删除。
- 已接入主题列表、帖子预览、Markdown/ZIP 下载、事件收件箱和事件已读状态。
- 已接入 NGA Cookie 失效系统告警、独立告警队列、Bark/飞书投递重试，以及管理台人工更新
  Cookie 后的验证恢复流程。
- 新增 `/api/v1/overview`、主题/帖子查询和事件读状态 API；事件已读状态由 0007 migration
  持久化。
- 服务参数、附件本地保存开关、资源目录和已就绪资源数量以只读方式展示。
- NGA 账号缺失或凭据解密失败时，首页显示可恢复的重新配置提示，不再直接显示通用 500。

后续增强：

- 用户 watch 独立结果视图、用户导出、安全富文本详情、schedule 编辑和按来源/TID/UID
  事件筛选均已于 2026-08-03 完成。

验收：

- 可通过管理页面录入 Cookie，API 和日志均不回显原值。
- 可配置 thread/user watch 和通知规则。
- 可查看 user watch 保存的目标用户主题和单条回复。
- 可查看附件本地保存参数；修改配置并重启后，后续资源按新值处理。
- 未读、打开帖子、标记已读、创建导出和下载正常。

预计：5～8 个开发日。

### M7：生产加固

状态：已完成（按 homeserver 单机部署范围验收）。

任务：

- Prometheus 指标、结构化日志和 NGA Cookie 失效告警运维说明。
- PostgreSQL/SQLite 数据库与 `assets.storage_path` 的一致性备份/恢复说明。
- Docker 发布、升级和镜像标签回滚文档。
- Nginx TLS 终止、反向代理及 forwarded headers 部署文档。

已完成：

- 新增公开 `/metrics` Prometheus 文本端点，记录 HTTP、worker、抓取、资源和通知任务计数。
- 保留并文档化 JSON 结构化日志、`x-request-id` 请求关联和 NGA Cookie 失效告警流程。
- 新增 homeserver 运维手册，明确 PostgreSQL/SQLite 与资源目录必须成组备份和恢复。
- Compose 支持不可变镜像标签，运维手册包含发布、升级、健康检查和回滚流程。
- 修正 Nginx 上游端口为 `12888`，并补齐 `X-Real-IP`、`X-Forwarded-For`、
  `X-Forwarded-Host`、`X-Forwarded-Proto` 与 `X-Request-Id`。

验收：

- `/metrics` 可被 Prometheus 抓取，日志和告警不包含 Secret。
- 运维文档覆盖数据库、资源目录、Docker 发布升级回滚和 Nginx 反代。
- PostgreSQL/SQLite 的备份恢复流程明确，并要求恢复后检查 `/ready`。

预计：4～7 个开发日。

## 13. 总体依赖关系

```text
M0 → M1 → M2 ─┬→ M3
              ├→ M4
              └→ M5

M2 + M3 + M4 + M5 → M6 → M7
```

单人开发粗略总量：35～60 个开发日。M0 的用户历史接口验证是最大不确定项。

## 14. 测试策略

### 单元测试

- NGA JSON/GBK/parser fixtures。
- 页码和游标边界。
- 自然键去重和 append-only 插入。
- `ERROR:2048` 前 9 次失败后成功，以及连续 10 次失败跳过本轮。
- 用户主题列表可访问条目、`denied` 占位项过滤和基于 `__ROWS` 的页数计算。
- 用户主题只保存匹配 UID 的主楼，用户回复只保存匹配 UID 的 TID/PID 详情。
- User Collector 忽略补全响应中同时出现的其他作者楼层。
- HTTP 层错误、空响应体、5xx 退避和恢复后继续原游标。
- `__UCPUSER` GBK 转码和无脚本执行的 JSON 提取。
- 内嵌 `comments` 和 `attches` 非空 fixture。
- asset URL 规范化、内容 hash 去重、安全相对路径和大小/MIME 限制。
- 通知规则组合。
- NGA markup → Markdown golden tests。

### 数据库集成测试

- PostgreSQL/SQLite migrations 及两套 schema 语义一致性。
- append-only insert、游标和事件事务。
- asset/post_assets upsert、同 hash 去重及下载状态幂等恢复。
- thread/user 并发插入同一自然键时仅一个事务创建 `post_event`。
- user-only `coverage=partial` 在 TID 全量回溯完成后升级为 `full`，已有帖子不重复。
- `SKIP LOCKED` 多 worker 领取。
- SQLite WAL、busy timeout、单 worker 调度和文件路径配置。
- `(post_event_id, channel_id)` outbox 幂等、多个匹配来源合并和重试。
- API repository 查询。

### 端到端测试

- Mock NGA server → collector → PostgreSQL/SQLite。
- UID watch → 新主题主楼/单条回复补全 → 只保存目标 UID 内容。
- 同一帖子按 UID→TID 和 TID→UID 两种发现顺序执行，均只产生一次渠道投递。
- post event → Bark/飞书 mock endpoint。
- API → Web 管理页面。
- 导出任务 → Markdown/ZIP。

### 回归测试

- Web 管理页面的 Cookie、watch、通知、附件参数和导出流程。
- 服务端升级后数据库 migration 与已持久化游标兼容。

## 15. 可观测性

最低指标：

```text
nga_requests_total
nga_request_duration_seconds
nga_request_failures_total
nga_user_busy_retries_total
crawl_runs_total
crawl_runs_skipped_busy_total
crawl_lag_seconds
posts_discovered_total
notification_delivery_total
notification_delivery_failures_total
notification_duplicates_suppressed_total
notification_outbox_pending
export_jobs_total
```

日志必须包含 `crawl_run_id`、`watch_id`、`tid`、`uid` 和 `event_id`，但不得包含 Cookie、
设备密钥、飞书 `app_secret`、tenant access token 和 API token。

## 16. 风险与应对

| 风险 | 应对 |
| --- | --- |
| 用户历史接口受限 | M0 先验证；明确 best-effort 契约；从部署后持续采集 |
| 热点用户查询持续返回 `ERROR:2048` | 每秒一次、最多 10 次；全部失败则不推进游标并跳过本轮 |
| NGA 限流或接口变化 | 账号级限流、退避、fixture contract tests、解析错误告警 |
| 分页边界导致漏帖 | 根据最后楼层抓取最小必要页面；边界页重复项由唯一键忽略 |
| TID/UID collector 或多规则重复触发通知 | posts/event 全局去重，outbox 使用 `(post_event_id, channel_id)` 唯一键 |
| Secret 泄漏 | 应用级加密、日志脱敏、API 不回传明文 |
| Markdown 规则复杂 | 原始内容永久保存、AST 管线、golden tests、逐步覆盖 |
| SQLite 并发写入受限 | WAL + busy timeout；限定单个 `all` 进程；需要横向扩展时使用 PostgreSQL |

## 17. 开发工作流与完成定义

每个里程碑完成时必须满足：

- migration、代码和 API 文档同步。
- 新增逻辑具备单元或集成测试。
- `cargo fmt --check`、`cargo clippy`、`cargo test` 通过。
- 不提交真实 Cookie、Bark key、飞书 `app_secret` 或 API token。
- 错误具备可诊断上下文，但不泄漏 Secret。
- README 更新启动、配置、升级和验证步骤。
- 对外 API 变更记录在 changelog。

## 18. 已确认产品决策

1. 第一版为单用户部署，不建设多用户和多租户系统。
2. NGA Cookie 通过服务端 Web 管理页面录入，并加密保存。
3. 由服务端配置文件或环境变量中的全局参数控制是否将附件和正文图片保存到本地。
4. UID watch 只保存目标用户自己的主题主楼和单条回复，不扩展为参与主题监控。
5. `extension-standalone` 与 Rust 服务是两个相互独立的工程，不做接入、迁移或合并发布。
6. 飞书渠道使用飞书国内版企业应用机器人，通过 `app_id`、`app_secret` 鉴权并向指定
   `chat_id`/用户 ID 发送消息。
7. 持久化支持 PostgreSQL 和 SQLite；SQLite 文件位置由服务配置指定。
8. 资源二进制统一保存在 `assets.storage_path`，数据库只保存元数据和相对路径；不使用
   PostgreSQL `BYTEA` 或 SQLite `BLOB` 保存附件内容。

## 19. 下一步

第一版核心功能已完成 M0～M8 验收。后续工作以生产运营和增强项为主：

1. 提交当前生产加固、指标和运维文档改动。
2. 继续扩展附件音频、视频等媒体类型和导出 golden fixtures。
3. 持续进行真实部署、备份恢复和长期运行验证。
