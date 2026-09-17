# 服务端文档

服务端文档统一在本目录维护，使用中文，专业术语可保留英语。开发规范见 [AGENTS.md](../AGENTS.md)。

| 目录 | 内容 | 入口 |
| --- | --- | --- |
| `plan/` | 整体计划、阶段依赖与推进状态 | [Go 重构计划](plan/README.md) |
| `spec/` | 功能目标、行为、约束与验收标准 | [阶段 Spec 列表](plan/README.md#阶段与交付物) |
| `ticket/` | 确有拆分需要时的实施任务 | [Ticket 说明](ticket/README.md) |

计划引用 Spec，Ticket 引用对应 Spec；各处只维护自身内容，避免重复记录需求和状态。

## 本地运行

需要 Go 1.26 或更新版本。所有构建和检查设置 `CGO_ENABLED=0`。SQLite 使用 [libtnb/sqlite](https://github.com/libtnb/sqlite) 的 GORM 驱动，底层为纯 Go 的 modernc SQLite，无需安装 SQLite、C 编译器或数据库服务。

从仓库根目录准备配置：

```bash
cd service
umask 077
cp config.example.json config.json
```

编辑 `config.json`，替换 `admin_password`、`api_token` 和 `encryption_key`。示例中的值只是占位符，不能直接启动；部署密钥可用 `openssl rand -base64 32` 生成，API token 可用 `openssl rand -hex 32` 生成。

```bash
CGO_ENABLED=0 go run ./cmd/server -config config.json
```

打开 [本地管理页](http://127.0.0.1:8080/admin)，使用配置中的用户名和密码登录。当前页面展示运行设置、时区、数据库状态，并支持退出登录。配置更改在重启后生效。

也可以编译后运行；管理页和时区数据已包含在二进制内：

```bash
CGO_ENABLED=0 go build -o bin/nga-reminder ./cmd/server
./bin/nga-reminder -config config.json
```

使用 Ctrl-C 或向服务进程发送 SIGTERM 停止。服务停止接收请求，最多等待 10 秒处理已有请求，然后关闭 HTTP 和数据库连接。`go run` 调试时使用 Ctrl-C；需要管理进程或发送 SIGTERM 时直接运行二进制。

## 配置

优先级为 **环境变量 > JSON 文件 > 默认值**。环境变量名称是 `NGA_REMINDER_` 加字段的大写形式，例如 `NGA_REMINDER_DATABASE_PATH`。省略 `-config` 时仅使用默认值与环境变量，不自动寻找配置文件。JSON 不接受未知字段。

| 字段 | 默认值 / 要求 |
| --- | --- |
| `listen_address` | `127.0.0.1:8080`，使用 `host:port`，端口 1～65535 |
| `database_path` | `data/nga-reminder.db`，本地持久化 SQLite 文件，不能为 `:memory:` |
| `assets_path` | `data/assets`，本地资源目录 |
| `admin_username` | `admin`，不能为空 |
| `admin_password` | 必填，12～72 字节 |
| `api_token` | 必填，至少 32 字节的随机值 |
| `encryption_key` | 必填，32 个随机字节的标准 Base64 编码，与数据库分开保管 |
| `timezone` | `Asia/Shanghai`，有效的 IANA 时区 |
| `background_enabled` | `true`；设置 `false` 进入关闭后台任务的核验模式 |
| `cookie_secure` | `false`，便于本地 HTTP；经 HTTPS 反向代理访问时设为 `true` |

文件中的相对数据路径以配置文件所在目录为基准；环境变量覆盖的相对路径也使用这个基准。没有配置文件时，以进程工作目录为基准。首次启动创建数据目录和当前所需表，启动过程不会写回配置文件。

临时关闭后台任务：

```bash
NGA_REMINDER_BACKGROUND_ENABLED=false CGO_ENABLED=0 go run ./cmd/server -config config.json
```

该开关仅改变本次进程的运行模式，不改写业务开关。管理页会显示“后台任务已关闭”。阶段 01 尚无采集、通知、Bot 或续期任务；后续阶段的任务启动统一受此开关控制。

启动失败会以非零退出码结束。日志指出配置字段或无法访问的路径，并保留错误原因和栈；常见原因是密钥编码不正确、端口被占用、数据目录不可写或数据库不属于 Go 服务。

## 管理入口与健康检查

| 请求 | 认证与行为 |
| --- | --- |
| `GET /` | 跳转 `/admin` |
| `GET /admin/login` | 登录表单 |
| `POST /admin/login` | 表单字段 `username`、`password`；成功设置 Cookie 并跳转管理页，凭据错误返回 401 |
| `GET /admin` | 登录 Cookie；未登录跳转登录页，成功显示当前运行设置 |
| `POST /admin/logout` | 撤销当前会话、清除 Cookie，跳转登录页 |
| `GET /api/v1/settings` | 登录 Cookie 或 `Authorization: Bearer <api_token>`；未认证返回 401 |
| `GET /healthz` | 无需认证，进程能处理请求时返回 200 |
| `GET /readyz` | 无需认证，SQLite 查询成功返回 200，不可访问返回 503 |

API token 只用于 API，管理页使用登录 Cookie。Cookie 为 HttpOnly、SameSite=Strict，有效期 12 小时；会话在正常重启后保留，退出或修改管理员用户名、密码、部署密钥会使对应旧会话失效。来自其他站点的表单提交被拒绝。

`/api/v1/settings` 只返回监听地址、数据路径、时区、后台开关、Cookie 安全设置、当前时间和数据库状态；健康检查不返回运行配置。所有应用 HTTP 响应包含 `X-Request-ID`，错误 JSON 中也包含 `trace_id`。路径按表格精确匹配，多余的尾斜杠返回 404。

```bash
curl -i http://127.0.0.1:8080/healthz
curl -i http://127.0.0.1:8080/readyz
```

调用管理 API 时，将 `NGA_REMINDER_API_TOKEN` 设为与启动配置一致的值：

```bash
curl -i -H "Authorization: Bearer $NGA_REMINDER_API_TOKEN" http://127.0.0.1:8080/api/v1/settings
```

## 数据与日志

当前数据库只保存管理员会话的不可逆校验值和 UTC Unix 秒到期时间，不保存原始 Cookie、密码或 API token。会话校验绑定部署密钥和管理员凭据；后续需要落盘的业务凭据再使用部署密钥加密。资源目录当前只创建并检查可写性。

SQLite 使用 WAL、5 秒 busy timeout 和单连接。数据库通过 `application_id` 标记归属；非空且没有 Go 标记的 SQLite 会被拒绝打开，避免误用旧 Rust 数据库。Go 必须使用独立的数据路径，旧 PG 数据通过 [阶段 09](spec/09-data-migration.md) 显式迁移。

日志默认以 info 级别逐行输出 JSON 到 stdout。一次请求中的操作共用 `trace_id`，每层记录 `span_id`、`parent_span_id`、`operation`、开始和结束；结束记录 `result` 和 `duration_ms`。SQL 记录挂在对应 repository 操作下，使用占位符，不打印实参；HTTP 只记录匹配的路由，不记录 query、请求头或表单内容。

错误由终止操作的入口记录一次，带 `error`、`causes`、`stack`；栈在错误创建或第三方边界捕获，包含函数、文件和行号。panic 恢复为通用 500 响应，原始栈留在日志中。启动凭据统一脱敏；业务代码仍需按 [规范](../AGENTS.md#调用链与日志) 显式选择安全字段。

## 开发检查

在 `service/` 下执行：

```bash
gofmt -w cmd internal
CGO_ENABLED=0 go build ./...
CGO_ENABLED=0 go vet ./...
CGO_ENABLED=0 go test ./...
```

测试使用假凭据和临时 SQLite，覆盖配置覆盖与校验、登录和退出、重启保留会话、API 认证、数据库故障、panic、完整调用链及脱敏。故障入口只存在于测试中，不进入生产路由。测试不访问 NGA、飞书或 Bark。
