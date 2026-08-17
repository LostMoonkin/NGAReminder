# NGA Reminder

[English](README.en.md)

NGA Reminder 是一个面向 NGA 的主题与用户监控、内容持久化和通知服务，同时保留一个无需服务端的 Chromium 独立扩展版本。

当前第一版服务端核心功能已经完成 M0～M8 验收，包含 NGA 抓取、PostgreSQL/SQLite 持久化、Bark/飞书通知、Markdown/ZIP 导出、Web 管理台、飞书机器人交互和单机生产运维能力。

## 版本组成

| 版本 | 适用场景 | 主要特点 |
| --- | --- | --- |
| Rust 服务端 | 需要历史保存、持续运行和多渠道通知 | 主题/用户监控、数据库持久化、导出、管理台、Bark/飞书 |
| Standalone 扩展 | 只需要浏览器内监控，不部署服务 | 直接使用浏览器 NGA Cookie，配置简单，浏览器运行时工作 |

两者是相互独立的产品形态，不共享运行模式、存储和发布流程。

## 服务端能力

- 按 TID 监控 NGA 主题，可选择全量历史或仅新增；历史回溯支持有界并发。
- 按 UID 监控用户的新主题和回复，首次只建立当前水位，之后的新增内容落库用于重试和审计。
- PostgreSQL 或 SQLite 持久化，支持动态时间段间隔、游标和自然键去重。
- Bark 和飞书国内版企业应用机器人通知；渠道与 TID 事件作者 UID 白名单直接配置在监控目标中，未命中白名单的内容仍会落库，但不会创建事件或推送。
- Transactional outbox、投递 lease、重试、死信和通知去重。
- Markdown 与 ZIP 导出，支持图片资源元数据、本地下载和 SHA-256 内容寻址。
- 大主题分页流式 Markdown、磁盘临时 ZIP，以及资源缺失检查和过期孤儿/临时文件清理。
- 响应式 `/admin` 管理台：Cookie、监控目标与通知过滤、动态间隔、通知渠道、事件、
  UID 结果、安全富文本详情和导出管理。
- 通用机器人能力：平台连接与通知目标分离、飞书长连接机器人适配器、`/指令` 命令路由、
  角色授权与私聊限制、一次性绑定码、消息幂等与回复 outbox。
- NGA Cookie 自动续期（可选）：Cookie 失效自动暂停监控并通知 owner，owner 私聊确认 →
  图形验证码 → 服务端提交密码登录 → 候选 Cookie 验证 → 原子替换并恢复监控；失败冷却与
  人工回退管理台。
- `/health`、`/ready`、`/metrics`、结构化日志、请求 ID 和 NGA Cookie 失效告警。

## 快速启动服务端

要求：Rust 1.92 或更新版本；使用 PostgreSQL 时需要 Docker Compose。

```bash
cd service
cp .env.example .env
```

至少设置以下敏感配置，并使用随机长字符串或随机密钥：

```bash
export NGA_REMINDER__API_TOKEN='replace-with-a-long-random-token'
export NGA_REMINDER__ADMIN_PASSWORD='replace-with-a-long-random-password'
export NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY="$(openssl rand -base64 32)"
```

开发环境可直接使用 SQLite：

```bash
export NGA_REMINDER__DATABASE_BACKEND=sqlite
export NGA_REMINDER__SQLITE_PATH=./data/nga-reminder.db
cargo run -- all
```

PostgreSQL 模式：

```bash
docker compose -f compose.yml up -d postgres
cargo run -- all
```

服务角色：

```text
cargo run -- serve   # 仅 API
cargo run -- worker  # 仅抓取和通知 worker
cargo run -- all     # API + worker，单机默认模式
```

访问地址：

```text
GET http://127.0.0.1:8080/health
GET http://127.0.0.1:8080/ready
GET http://127.0.0.1:8080/metrics
GET http://127.0.0.1:8080/admin
```

生产环境建议使用 Nginx 终止 TLS，并让 Rust 服务只监听内部地址。详细备份、恢复、升级、回滚和反向代理说明见 [`service/docs/OPERATIONS.md`](service/docs/OPERATIONS.md)。

不需要 PostgreSQL 的单机生产部署可直接使用 [`service/compose.production.yml`](service/compose.production.yml)。该模板只启动一个 SQLite `all` 容器，并将数据库与资源保存在同一个 Docker 数据卷中。

## Docker 镜像发布

[`service-image.yml`](.github/workflows/service-image.yml) 会在涉及服务端代码的 pull request 上运行 Rust 质量门；推送到 `main`、推送非 `-standalone` 的 `v*` 版本 tag，或手动触发时才构建服务端镜像并发布到 GHCR：

```text
ghcr.io/<owner>/<repository>
```

镜像会生成分支/tag、语义化版本、commit SHA 标签；默认分支额外生成 `latest`。工作流使用 GitHub Actions 内置 `GITHUB_TOKEN`，不需要额外配置 Docker Hub 凭据。

## 版本发布

服务端和 Standalone 扩展使用独立版本号。使用 [`scripts/release.sh`](scripts/release.sh) 更新版本、运行质量检查、创建提交和 annotated tag：

```bash
# 下一次发布示例
scripts/release.sh service 0.1.2
scripts/release.sh extension 1.0.2
```

服务端 tag 为 `vX.Y.Z`，扩展 tag 为 `vX.Y.Z-standalone`。脚本默认只创建本地提交和 tag；确认无误后追加 `--push` 推送到远端。

## 常用 API

受保护 API 使用：

```text
Authorization: Bearer <NGA_REMINDER__API_TOKEN>
```

主要接口包括：

```text
POST   /api/v1/settings/nga-account
POST   /api/v1/watches/threads
POST   /api/v1/watches/users
GET    /api/v1/watches
GET/PATCH/DELETE /api/v1/watches/{id}
POST   /api/v1/watches/{id}/run
POST   /api/v1/watches/{id}/reset
GET    /api/v1/watches/{id}/runs
GET/POST/PATCH/DELETE /api/v1/channels...
GET    /api/v1/threads
GET    /api/v1/threads/{tid}/posts
GET    /api/v1/events
GET    /api/v1/exports/threads/{tid}?format=markdown|zip
GET    /api/v1/exports/users/{uid}?format=markdown|zip
```

TID 可选择全量静默基线或仅建立当前水位；UID 首次只读取主题/回复列表首页建立水位。后续抓取到的新内容才产生事件并进入通知 outbox。

## 独立扩展

Standalone 扩展位于 [`extension-standalone/`](extension-standalone/)，无需部署 Rust 服务或数据库。安装方式和配置说明见 [`extension-standalone/README.md`](extension-standalone/README.md)。

适合以下场景：

- 浏览器始终开启，只需要接收新回复通知。
- 不需要保存历史内容或从服务器持续运行。
- 希望快速安装，不配置数据库和服务端凭据。

## 当前状态

- M0：接口验证与规格冻结——完成。
- M1：Rust 服务骨架与双持久化后端——完成。
- M2：Thread 全量与增量持久化——完成。
- M3：User 监控——完成。
- M4：监控目标内嵌通知过滤、Bark 与飞书——验收完成。
- M5：Markdown/ZIP 导出与资源持久化——验收完成。
- M6：Web 管理页面——验收完成。
- M7：生产加固——按 homeserver 单机部署范围验收完成。
- M8：飞书机器人交互与 NGA Cookie 自动续期——已完成 dev 环境整体端到端验收，可用于
  homeserver 单机生产发布。

后续增强包括更完整的音频/视频附件提取、导出 golden fixtures，以及旧父帖楼中楼的独立变化检测。

## 开发与验证

在 `service/` 目录执行：

```bash
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

当前测试覆盖 NGA 解析器 fixture、GBK 资料解析、TID/UID 采集器、数据库幂等、通知重试、资源安全、Markdown/ZIP 导出和 API 鉴权。

## 安全注意事项

- 不要提交真实 NGA Cookie、API token、Bark device key 或飞书应用密钥。
- Cookie 和通知渠道配置会加密保存，API 列表不会回显明文凭据。
- 数据库和 `assets.storage_path` 必须作为一个整体备份与恢复。
- SQLite 适合单进程 `all` 模式；需要多 worker 或横向扩展时使用 PostgreSQL。

## 文档

- [项目计划](PROJECT_PLAN.md)
- [服务端说明](service/README.md)（[English](service/README.en.md)）
- [运维手册](service/docs/OPERATIONS.md)
- [NGA API 契约](service/docs/NGA_API_CONTRACT.md)
- [机器人交互与 Cookie 续期设计](service/docs/BOT_INTERACTION_AND_COOKIE_RENEWAL_DESIGN.md)
- [流式导出、资源维护与内容管理设计](service/docs/EXPORT_RESOURCE_AND_CONTENT_UI_DESIGN.md)
- [Standalone 扩展说明](extension-standalone/README.md)
- [更新日志](service/CHANGELOG.md)（[English](service/CHANGELOG.en.md)）

## 许可证

[MIT](LICENSE)
