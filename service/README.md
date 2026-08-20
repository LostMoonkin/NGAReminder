# NGA Reminder 服务端

[English](README.en.md)

独立的 Rust 服务端，提供 NGA 主题/用户监控、PostgreSQL/SQLite 持久化、通知和 Markdown 导出。

## 本地开发

依赖：

- Rust 1.92 或更新版本。
- Docker Compose。

启动 PostgreSQL：

```bash
docker compose -f compose.yml up -d postgres
```

也可以选择 SQLite，在不启动 PostgreSQL 的情况下运行：

```bash
export NGA_REMINDER__DATABASE_BACKEND=sqlite
export NGA_REMINDER__SQLITE_PATH=./data/nga-reminder.db
```

SQLite 会自动创建父目录，启动时启用 WAL、外键和 5 秒 busy timeout。需要运行多个 worker 进程时推荐使用 PostgreSQL；SQLite 只面向单个 `all` 进程。

默认不保存原始帖子 JSON：

```text
NGA_REMINDER__PERSISTENCE__STORE_RAW_PAYLOAD=false
```

关闭时，新写入的 `posts.raw_payload` 为空字符串，但标准化帖子字段仍然可用。只有在确实需要完整 NGA 原始响应来诊断解析器时才设为 `true`。修改该选项不会删除或回填已有记录。

资源持久化单独配置：

```text
NGA_REMINDER__ASSETS__DOWNLOAD_ENABLED=false
NGA_REMINDER__ASSETS__STORAGE_PATH=./data/assets
NGA_REMINDER__ASSETS__MAX_DOWNLOAD_BYTES=10485760
```

即使关闭下载，正文中的 NGA `[img]` 资源仍会记录到数据库。关闭下载时，导出保留远程 URL；开启下载时，受信任的 HTTPS NGA 图片主机会进入有界下载队列，并保存到按 SHA-256 内容寻址的路径中。数据库和 `assets.storage_path` 必须一起备份。

配置服务端：

```bash
cp .env.example .env
export NGA_REMINDER__API_TOKEN='replace-with-a-long-random-token'
export NGA_REMINDER__ADMIN_PASSWORD='replace-with-a-long-random-password'
export NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY="$(openssl rand -base64 32)"
```

如果要让 Cargo 直接使用已填写的 `.env` 文件，应先将其加载到当前 shell：

```bash
set -a
. ./.env
set +a
```

启动 API 和 worker：

```bash
cargo run -- all
```

## 不使用 PostgreSQL 的生产部署

单机部署可使用 [`compose.production.yml`](compose.production.yml)。它只启动一个使用 SQLite 的 `all` 容器，并将数据库与下载的资源保存在 Docker 数据卷 `nga-reminder-data` 中，不会启动 PostgreSQL。

启动前设置三个必需的密钥：

```bash
export NGA_REMINDER__API_TOKEN="$(openssl rand -hex 32)"
export NGA_REMINDER__ADMIN_PASSWORD="$(openssl rand -base64 32)"
export NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY="$(openssl rand -base64 32)"
docker compose -f compose.production.yml up -d
```

模板绑定 `127.0.0.1:12888`，预期由 Nginx 反向代理。容器内服务固定以 UID/GID `999:999` 运行；若将 `/data` 改为宿主机或 NFS 绑定目录，必须预先授予该数值用户读写权限。升级或回滚时，在执行 `docker compose ... up -d` 前将 `NGA_REMINDER_IMAGE` 设置为不可变的 GHCR 标签；不要执行 `docker compose down -v`，否则会删除 SQLite 数据卷。备份和恢复流程见 [`docs/OPERATIONS.md`](docs/OPERATIONS.md)。

运行角色：

```text
cargo run -- serve   # 仅 API
cargo run -- worker  # 仅 worker
cargo run -- all     # API + worker
```

公开探针：

```text
GET /health
GET /ready
GET /metrics
```

受保护 API 需要：

```text
Authorization: Bearer <NGA_REMINDER__API_TOKEN>
```

最小管理页面位于 `GET /admin`。页面会建立带 HttpOnly、SameSite=Strict 属性的管理员 session，并可加密保存完整 NGA Cookie 或单独保存两个 Passport 值。跨用户 UID 搜索需要完整浏览器 Cookie 和对应的导航请求头；只配置 Passport UID/CID 仍可用于主题监控和账号自身查询。API 响应只暴露脱敏后的 UID 与完整 Cookie 是否已配置，绝不会返回凭据。

自动续期成功时会收集登录流程返回的完整 cookie jar，与账号已有 Cookie 合并后加密写入 `cookie_encrypted`；候选中的新字段覆盖旧值，旧的其他字段保留，并强制以已验证的新 UID/CID 为准。即使账号此前只有两个 Passport 字段，续期后也会建立完整 Cookie 凭据。

## 主题和用户监控

配置并测试 NGA 账号后，创建主题监控：

```bash
curl -X POST http://127.0.0.1:8080/api/v1/watches/threads \
  -H "Authorization: Bearer $NGA_REMINDER__API_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{
    "tid": 12345678,
    "interval_seconds": 60,
    "history": {
      "mode": "full",
      "parallel_enabled": true,
      "parallelism": 2
    },
    "notification": {
      "channel_ids": ["channel-id"],
      "author_uids": []
    }
  }'
```

worker 会立即领取新监控。第一次抓取会将所有可访问页面导入为静默基线，不创建通知事件；之后只追加高于已保存游标的楼层。已有帖子内容不会更新或删除。

监控接口：

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

`interval_seconds` 可选，默认使用 `config/default.toml` 中的 `scheduler.default_interval_seconds`（或 `NGA_REMINDER__SCHEDULER__DEFAULT_INTERVAL_SECONDS`），范围必须为 30～86400。其他 worker 持有监控租约时，手动运行返回 `409`。NGA code 14 会停用不存在主题的监控。主题 `code=51`（帖子正在审核）只跳过本轮抓取，不推进游标，下一次定时运行会自动重试。凭据被拒后会暂停账号及受影响监控，修复凭据并显式启用监控后才会恢复。

每个监控还可以配置 `schedule` 数组。规则按顺序使用 `scheduler.timezone_offset` 评估；`days` 支持 `weekdays`、`weekends` 或单个星期名称（`monday` 至 `sunday`，也支持常见的三/四字母缩写）。`start_time` 和 `end_time` 使用 `HH:MM`，`end_time` 可以是 `24:00`。所有规则都未命中时，回退到监控的 `interval_seconds`。

```bash
curl -X POST http://127.0.0.1:8080/api/v1/watches/threads \
  -H "Authorization: Bearer $NGA_REMINDER__API_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{
    "tid": 12345678,
    "schedule": [
      {
        "days": ["weekdays"],
        "description": "工作日工作时间 - 每 2 分钟",
        "start_time": "09:00",
        "end_time": "16:00",
        "interval": 120
      },
      {
        "days": ["weekends"],
        "description": "周末 - 每小时",
        "start_time": "00:00",
        "end_time": "23:59",
        "interval": 3600
      }
    ],
    "history": {
      "mode": "incremental",
      "parallel_enabled": false,
      "parallelism": 2
    },
    "notification": {
      "channel_ids": ["channel-id"],
      "author_uids": [150058]
    }
  }'
```

临近规则边界时，调度器会保证不晚于边界执行，因此较长的间隔不会跳过更快或更慢的规则切换。使用 `schedule: null` 的 `PATCH` 会清除计划并回到回退间隔。

免拉取时段通过独立的 `no_fetch_periods` 配置，不改变 `schedule` 的拉取间隔语义。规则同样使用 `scheduler.timezone_offset`，支持 `weekdays`、`weekends` 和星期名称；开始时间包含、结束时间不包含，跨午夜规则归属于开始日，`00:00`～`24:00` 表示全天。每个目标最多配置 128 条规则，规则不得为空、不得使用相同的开始和结束时间，且整周必须至少保留一分钟可拉取时间。

```json
{
  "no_fetch_periods": [
    {
      "days": ["weekdays"],
      "start_time": "00:00",
      "end_time": "08:00",
      "description": "夜间"
    }
  ]
}
```

免拉取期间只有自动运行会在访问 NGA 前记录一条 `status=skipped`、`error_kind=no_fetch_period` 的零计数运行，并将下次运行推到连续区间结束；游标和基线不会推进。手动 API `/run` 和机器人 `/watch run` 不受免拉取限制。`PATCH` 中字段缺失表示不修改，`null` 清空配置，非空数组完整替换，空数组会返回 `400 invalid_request`。读取 watch 时可使用 `no_fetch_active`、`no_fetch_until` 和 `scheduler_timezone_offset`，运行列表中的 `trigger_kind` 为 `unknown`、`scheduled` 或 `manual`。

创建用户监控：

```bash
curl -X POST http://127.0.0.1:8080/api/v1/watches/users \
  -H "Authorization: Bearer $NGA_REMINDER__API_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{
    "uid": 150058,
    "interval_seconds": 60,
    "notification": {
      "channel_ids": ["channel-id"]
    }
  }'
```

用户监控不会导入历史。回复列表使用 `GET /thread.php?searchpost=1&authorid={uid}&__output=12&page={page}`；请求只携带配置的 User-Agent，以及从完整 Cookie 中提取的 `ngaPassportUid`、可选 `ngaPassportUrlencodedUname` 和 `ngaPassportCid`。第一次运行只记录当前主题列表和回复列表的水位，不创建帖子或通知。后续运行保存该 UID 新发现的主题和单条回复，以支持可靠的投递重试与审计，但不会因为用户参与某个 TID 就扩展为完整主题抓取。跨用户查询必须先在服务设置中粘贴完整浏览器 Cookie；仅有 Passport UID/CID 时会返回 `nga_full_cookie_required`。若 NGA 对跨用户列表返回空体 HTTP 503，则按 2 秒间隔最多尝试 3 次，耗尽后记录 `nga_user_search_unavailable`。这两种失败都不会建立错误的空水位或推进任一游标。列表扫描在已保存的 `(postdate, tid/pid)` 边界处停止，只请求新候选的详情。用户列表遇到 NGA busy 响应时每秒重试一次，最多十次；耗尽后记录 `skipped_busy`，且不推进两个游标。如果主题详情返回 `code=51`，本次用户抓取记录为 `skipped_pending_review`，游标保持不变，等待下一次定时运行。

## 通知

通知渠道密钥会静态加密保存，列表接口不会返回明文。支持的渠道类型为 `bark` 和 `feishu`。通知匹配直接配置在每个监控中，通知中心只管理渠道。

```text
GET/POST     /api/v1/channels
PATCH/DELETE /api/v1/channels/{id}
POST         /api/v1/channels/{id}/test
```

Bark 渠道配置示例：

```json
{"device_key":"...","server_url":"https://api.day.app","group":"NGA Reminder"}
```

飞书渠道配置示例：

```json
{
  "app_id": "cli_...",
  "app_secret": "...",
  "receive_id_type": "chat_id",
  "receive_id": "oc_..."
}
```

飞书适配器使用企业自建应用的机器人能力，不使用自定义机器人 webhook。`receive_id_type` 默认为 `chat_id`，也接受 `open_id`、`user_id`、`union_id` 和 `email`。应用必须发布并拥有发送消息权限，接收者必须可用；群聊发送还需要将应用加入目标群。租户访问令牌（tenant access token）会按返回的有效期缓存在内存中，不会持久化。

飞书卡片会从完整帖子正文中提取 NGA `[img]...[/img]` 标记。最多下载三张来自受支持 NGA 图片主机的 HTTPS 图片，大小限制为 10 MB，通过 `im/v1/images` 上传并嵌入返回的 `image_key`。每个应用和源 URL 的 image key 会缓存在内存中。主机不受信任、下载失败、上传失败或超过前三张的图片会改为源链接，因此图片处理不会阻塞文本通知。飞书应用还需要图片/文件资源上传权限。

通知匹配与帖子事件创建在同一事务中完成。TID 监控可按作者 UID 过滤新帖子；UID 监控始终匹配目标 UID。多个监控命中同一事件和渠道时共用一条 outbox 记录，同时保留所有来源监控关系用于审计。临时失败最多重试五次，每次都会记录，且不会记录渠道密钥。停用渠道会暂停新任务入队和已有任务重试；被监控或 outbox 引用的渠道必须先解除引用才能删除。

回复动作使用持久化页码和 NGA 楼内锚点格式：
`read.php?tid={tid}&page={page}#pid{pid}Anchor`。打开通知后会定位到完整主题页面中的对应回复，而不是 NGA 的孤立回复页面。

## Markdown 和 ZIP 导出

受保护的导出接口默认返回 Markdown，也接受 `?format=markdown` 或 `?format=zip`：

```text
GET /api/v1/exports/threads/{tid}?format=markdown
GET /api/v1/exports/threads/{tid}?format=zip
GET /api/v1/exports/users/{uid}?format=markdown
GET /api/v1/exports/users/{uid}?format=zip
```

主题导出包含该 TID 已持久化的全部帖子。用户导出包含该 UID 发布的已持久化帖子，并按 TID 分组。ZIP 包含 Markdown 文件、`metadata.json` 以及下载状态为 `ready` 的本地资源；缺失或仅有远程地址的资源仍保留远程链接。

Markdown 按稳定游标分页读取并直接流式响应。ZIP 在 `assets.storage_path/.tmp` 中生成临时
文件后流式返回，正常完成或客户端下载中断时都会删除临时 ZIP，因此大主题导出不会把
完整帖子集合、资源内容或 ZIP 全部加载到服务内存。

资源一致性维护接口：

```text
GET  /api/v1/assets/maintenance
POST /api/v1/assets/maintenance/cleanup
```

扫描只读地报告缺失文件、孤儿元数据、孤儿内容文件和过期导出临时文件。清理默认只删除
超过 24 小时的孤儿/临时文件；缺失 ready 文件会重新进入下载队列，关闭资源下载时则恢复
为仅远程状态。管理台“服务设置 → 资源一致性维护”提供相同操作。

## 资源持久化设计

资源 worker 会将附件、图片、音频和视频的元数据保存到所选数据库，并将二进制内容保存到 `assets.storage_path`；不会使用 PostgreSQL `BYTEA` 或 SQLite `BLOB`。关闭下载时只保留远程 URL 和元数据。

下载文件使用 SHA-256 内容寻址路径：

```text
<assets.storage_path>/<前两个 SHA-256 字符>/<完整 SHA-256>.<安全扩展名>
```

这样可以去重内容，并控制数据库、WAL 和备份大小。数据库与资源目录是一个逻辑备份单元，必须一起备份和恢复。本节描述的是已冻结的设计；资源下载器、流式导出和显式资源维护已作为 M5 后续增强实现。更广泛的音频/视频附件覆盖仍属于可选增强。

生产部署应让应用只监听内部地址，并以 [`deploy/nginx.conf`](deploy/nginx.conf) 为 TLS 终止配置起点。

Prometheus 指标、结构化日志、Cookie 失效告警、PostgreSQL/SQLite 与资源备份恢复、Docker 发布/回滚以及 Nginx TLS/反向代理说明见 [`docs/OPERATIONS.md`](docs/OPERATIONS.md)。通用机器人架构、斜杠命令协议、飞书适配器重构、身份绑定和用户确认的 NGA Cookie 续期设计见 [`docs/BOT_INTERACTION_AND_COOKIE_RENEWAL_DESIGN.md`](docs/BOT_INTERACTION_AND_COOKIE_RENEWAL_DESIGN.md)。

流式导出、临时文件生命周期、资源维护安全边界和 UID 内容管理设计见
[`docs/EXPORT_RESOURCE_AND_CONTENT_UI_DESIGN.md`](docs/EXPORT_RESOURCE_AND_CONTENT_UI_DESIGN.md)。

不要提交真实 NGA Cookie、API token、Bark key 或飞书应用密钥。
