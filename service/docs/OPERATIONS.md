# NGA Reminder 运维说明

本文面向部署在家庭服务器（homeserver）上的单机实例。推荐使用 PostgreSQL、Docker Compose 和 Nginx；SQLite
适合不需要独立数据库容器的单进程部署。

## 观测、日志和告警

服务提供以下无需鉴权的探针：

```text
GET /health   进程存活探针
GET /ready    数据库连接探针
GET /metrics  Prometheus 文本格式指标
```

`/metrics` 只返回计数和运行时长，不包含 Cookie、API token、通知渠道配置或帖子正文。当前指标
包括 HTTP 请求/状态码、worker 周期、抓取成功/失败、资源任务和通知任务数量。

生产环境建议将结构化日志打开：

```dotenv
NGA_REMINDER__OBSERVABILITY__LOG_JSON=true
NGA_REMINDER__OBSERVABILITY__LOG_FILTER=info,sqlx=warn
```

HTTP 请求会带有 `x-request-id`，该 ID 会回传给客户端并出现在请求追踪日志中。日志过滤器可按
需要调整，但不要把 `sqlx=debug` 或包含请求头/响应体的调试日志用于长期运行。

NGA Cookie 失效时，worker 会暂停相关账号和 watch，并通过已启用的 Bark/飞书渠道发送去重后的
系统告警。通知只包含“Cookie 已失效”和管理台地址；管理员在 `/admin` 更新并测试 Cookie 后，
告警会被解决，watch 仍需由管理员手动重新启用。

## PostgreSQL 备份与恢复

备份必须同时包含数据库和 `NGA_REMINDER__ASSETS__STORAGE_PATH` 指向的资源目录。为了让两者处于
一致快照，备份前先停止 app（PostgreSQL 容器保持运行即可）：

```bash
cd service
mkdir -p ./backups/$(date +%Y%m%d-%H%M%S)
BACKUP_DIR="./backups/$(date +%Y%m%d-%H%M%S)"
docker compose stop app
docker compose exec -T postgres pg_dump -U nga_reminder -d nga_reminder --format=custom \
  > "$BACKUP_DIR/nga_reminder.dump"
tar --xattrs --same-owner -czf "$BACKUP_DIR/assets.tar.gz" -C ./data assets
docker compose start app
```

实际操作时应只计算一次时间戳并复用同一个目录名，例如：

```bash
BACKUP_DIR="./backups/20260728-020000"
mkdir -p "$BACKUP_DIR"
```

恢复前停止 app，并将待恢复备份和资源目录放在同一备份目录中。以下恢复会覆盖目标数据库，
只应对明确指定的 `nga_reminder` 数据库执行：

```bash
docker compose stop app
docker compose exec -T postgres pg_restore -U nga_reminder -d nga_reminder \
  --clean --if-exists --no-owner < "$BACKUP_DIR/nga_reminder.dump"
rm -rf ./data/assets
mkdir -p ./data
tar -xzf "$BACKUP_DIR/assets.tar.gz" -C ./data
docker compose start app
```

恢复后检查：

```bash
curl -fsS http://127.0.0.1:12888/ready
docker compose logs --tail=100 app
```

管理台的资源统计和导出 ZIP 可用于抽样验证。若数据库中的 `assets.local_relative_path` 有值但
对应文件不存在，导出会保留远程 URL；这类文件缺失应从同一备份重新恢复，而不是修改数据库指针。

建议至少保留 7 份日备份和 4 份周备份，并将备份目录复制到另一块磁盘或另一台设备。备份文件
包含帖子内容和加密后的凭据，必须按敏感数据保护，不能提交到 Git 或公开文件服务。

## SQLite 备份与恢复

SQLite 运行期间不要直接复制数据库文件。停止服务后，将数据库和资源目录作为一个整体归档：

```bash
cd service
docker compose stop app  # 或停止 cargo run -- all
mkdir -p ./backups/20260728-020000
tar --xattrs --same-owner -czf ./backups/20260728-020000/sqlite-data.tar.gz \
  -C ./data nga-reminder.db assets
```

恢复时停止服务，将归档解压回同一个 `SQLITE_PATH` 的父目录和同一个 `ASSETS__STORAGE_PATH`，
然后启动服务并检查 `/ready`。SQLite 的 `-wal`/`-shm` 文件不应在服务停止后作为独立备份对象；
若停机时仍存在，应连同数据库目录一起保留，避免只恢复主数据库文件。

## Docker 发布、升级与回滚

### SQLite 单容器生产模板

不需要单独部署 PostgreSQL 时，使用 [`../compose.production.yml`](../compose.production.yml)。该模板
只启动一个 `all` 容器，数据库和资源统一保存在 Docker 命名卷 `nga-reminder-data` 中：

```bash
cd service
export NGA_REMINDER__API_TOKEN="$(openssl rand -hex 32)"
export NGA_REMINDER__ADMIN_PASSWORD="$(openssl rand -base64 32)"
export NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY="$(openssl rand -base64 32)"
docker compose -f compose.production.yml up -d
curl -fsS http://127.0.0.1:12888/ready
```

模板默认只绑定宿主机 `127.0.0.1:12888`，由 Nginx 对外提供 HTTPS。SQLite 只支持单个 `all`
进程，不要通过启动多个容器来扩容。备份前先停止 app，且不要使用 `docker compose down -v`，
否则会删除包含数据库和资源文件的 Docker 命名卷。

首次启动：

```bash
cd service
cp .env.example .env
# 编辑 .env，至少设置 API_TOKEN、ADMIN_PASSWORD、CREDENTIAL_ENCRYPTION_KEY 和 DATABASE_URL
docker compose up -d --build
docker compose ps
curl -fsS http://127.0.0.1:12888/ready
```

Compose 启动时会等待 PostgreSQL 健康检查，应用启动阶段自动执行 SQLx migrations。升级前先做一次
备份，然后使用不可变镜像标签构建新版本：

```bash
docker compose stop app
docker compose exec -T postgres pg_dump -U nga_reminder -d nga_reminder --format=custom \
  > ./backups/pre-upgrade.dump
docker build -t nga-reminder:20260728-020000 .
NGA_REMINDER_IMAGE=nga-reminder:20260728-020000 docker compose up -d app
curl -fsS http://127.0.0.1:12888/ready
docker compose logs --tail=100 app
```

如果新版本无法启动或 `/ready` 不恢复，先停止 app，切回上一个镜像标签并启动：

```bash
docker compose stop app
NGA_REMINDER_IMAGE=nga-reminder:previous docker compose up -d app
curl -fsS http://127.0.0.1:12888/ready
```

数据库 migration 只向前兼容时，代码回滚前必须确认新版本没有执行不可逆 migration；本项目当前
migrations 由应用启动自动执行，生产升级前应保留数据库备份并先在副本上验证回滚路径。

发布包只需要服务目录中的 Dockerfile、Cargo 清单/锁文件、源码、migrations、config、部署和
运维文档；`.env`、数据库文件、assets、真实导出文件和真实日志不属于发布包。

## Nginx TLS、反向代理和转发请求头

`deploy/nginx.conf` 是 server/location 级配置示例。Nginx 负责公网 TLS，Rust 服务只监听 Docker
内部网络的 `12888` 端口：

```text
浏览器 -- HTTPS --> Nginx -- HTTP/内部网络 --> nga-reminder:12888
```

部署时替换证书路径、域名和上游服务名，并确保 `X-Forwarded-Proto` 只由可信 Nginx 设置。应用
使用该请求头判断管理员 session cookie 是否附加 `Secure` 属性，同时保留 `X-Request-Id` 供日志
关联。不要让公网直接暴露应用容器端口；Compose 的端口映射应改为仅绑定 homeserver 本机，或
移除映射并只让 Nginx 所在网络访问应用。

证书更新后执行：

```bash
nginx -t
systemctl reload nginx
```

修改 Nginx 或应用上游后，依次检查 `/health`、`/ready`、管理台登录和一条受保护 API 请求。
