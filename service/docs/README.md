# 服务端文档

服务端文档统一在本目录维护，使用中文，专业术语可保留英语。开发规范见 [AGENTS.md](../AGENTS.md)。

| 目录 | 内容 | 入口 |
| --- | --- | --- |
| `plan/` | 整体计划、阶段依赖与推进状态 | [Go 重构计划](plan/README.md) |
| `spec/` | 功能目标、行为、约束与验收标准 | [阶段 Spec 列表](plan/README.md#阶段与交付物) |
| `ticket/` | 确有拆分需要时的实施任务 | [Ticket 说明](ticket/README.md) |

计划引用 Spec，Ticket 引用对应 Spec；各处只维护自身内容，避免重复记录需求和状态。

## 本地运行

需要 Go 1.27.0 或更新版本。所有构建和检查设置 `CGO_ENABLED=0`。SQLite 使用 [libtnb/sqlite](https://github.com/libtnb/sqlite) 的 GORM 驱动，底层为纯 Go 的 modernc SQLite，无需安装 SQLite、C 编译器或数据库服务。

从仓库根目录准备配置：

```bash
cd service
umask 077
cp config.example.json config.json
```

编辑 `config.json`，替换 `api_token` 和 `encryption_key`。API token 可自定义，只要求不全为空白；部署密钥沿用旧 Rust 的格式要求，可用 `openssl rand -base64 32` 生成。示例密钥是占位符，需替换后启动。

```bash
./start.sh
```

也可从仓库根目录执行 [启动脚本](../start.sh)：`./service/start.sh`。脚本强制 `CGO_ENABLED=0`，每次先构建 `service/bin/nga-reminder` 再以前台进程运行。无参数时以 `service/` 为工作目录，自动读取该目录的 `config.json`；文件不存在时使用默认值和环境变量，缺少必要凭据会明确报错。脚本不生成或覆盖配置。

指定其他配置文件时，参数原样交给服务；相对配置路径以调用脚本时的工作目录为基准。例如从仓库根目录执行：

```bash
./service/start.sh -config ./service/config.local.json
```

服务默认监听 `0.0.0.0:8989`。在局域网其他机器上打开 `http://<服务端局域网 IP>:8989/admin`；服务端本机也可打开 [本地管理页](http://127.0.0.1:8989/admin)。管理页直接访问，无需登录，可配置 NGA 账号、管理 TID 监控、手动采集和浏览已保存帖子。启动配置更改在重启后生效。

已有配置文件需同步将 `listen_address` 改为 `0.0.0.0:8989`，或在启动时设置 `NGA_REMINDER_LISTEN_ADDRESS=0.0.0.0:8989`。旧配置中的 `admin_username`、`admin_password`、`cookie_secure` 已移除，升级时删除这些字段。

也可以编译后运行；管理页和时区数据已包含在二进制内：

```bash
CGO_ENABLED=0 go build -o bin/nga-reminder ./cmd/server
./bin/nga-reminder -config config.json
```

使用 Ctrl-C 或向服务进程发送 SIGTERM 停止。启动脚本通过 `exec` 运行二进制，脚本 PID 即服务 PID。服务取消正在执行的采集，最多等待 10 秒处理已有 HTTP 请求，保存中断结果后关闭网络和数据库连接；运行日志直接输出到当前终端。异常退出留下的 `running` 记录在下次启动时标为 `interrupted`，由用户手动重跑。

## 配置

优先级为 **环境变量 > JSON 文件 > 默认值**。环境变量名称是 `NGA_REMINDER_` 加字段的大写形式，例如 `NGA_REMINDER_DATABASE_PATH`。直接运行二进制并省略 `-config` 时仅使用默认值与环境变量；启动脚本无参数时会自动选择 `service/config.json`。沿用旧服务的行为，忽略尚未使用的配置字段，不额外限制配置文件大小。

| 字段 | 默认值 / 要求 |
| --- | --- |
| `listen_address` | `0.0.0.0:8989`，监听所有网卡，使用 `host:port`；端口 0 表示由系统分配 |
| `database_path` | `data/nga-reminder.db`，本地持久化 SQLite 文件，不能为 `:memory:` |
| `assets_path` | `data/assets`，本地资源目录 |
| `api_token` | 必填，不能全为空白，无最小长度要求；按原值匹配，不裁剪空白 |
| `encryption_key` | 必填，标准 Base64 编码，解码后为 32 字节，与数据库分开保管 |
| `timezone` | `Asia/Shanghai`，有效的 IANA 时区 |
| `nga_user_agent` | `Mozilla/5.0 (compatible; NGA-Reminder/0.1)`，沿用旧 Rust 默认值，供 NGA 请求使用 |
| `background_enabled` | `true`；设置 `false` 进入关闭后台任务的核验模式 |

文件中的相对数据路径以配置文件所在目录为基准；环境变量覆盖的相对路径也使用这个基准。没有配置文件时，以进程工作目录为基准。首次启动创建数据目录和当前所需表，启动过程不会写回配置文件。

临时关闭后台任务：

```bash
NGA_REMINDER_BACKGROUND_ENABLED=false ./start.sh
```

该开关仅改变本次进程的运行模式，不改写业务开关。管理页会显示“后台任务已关闭”；凭据校验和手动采集也返回 409，保证核验期间不访问 NGA。仍可查询和管理本地数据。后续阶段的自动任务同样受此开关控制。

启动失败会以非零退出码结束。日志指出配置字段或无法访问的路径，并保留错误原因和栈；常见原因是密钥编码不正确、端口被占用、数据目录不可写或数据库不属于 Go 服务。

## 管理入口与健康检查

| 请求 | 认证与行为 |
| --- | --- |
| `GET /` | 跳转 `/admin` |
| `GET /admin` | 无需认证，账号、监控、已保存主题和运行设置 |
| `GET /admin/watches/:id` | 监控配置、手动运行和最近 20 次结果；运行中每 3 秒刷新 |
| `GET /admin/threads/:tid?page=1` | 已保存帖子及纯文本正文，每页 50 条 |
| `GET /api/v1/settings` | `Authorization: Bearer <api_token>`；未认证返回 401 |
| `GET /healthz` | 无需认证，进程能处理请求时返回 200 |
| `GET /readyz` | 无需认证，SQLite 查询成功返回 200，不可访问返回 503 |

Web 管理用于可信内网，不设置后台用户名、密码或会话；管理页由服务端直接读取业务数据。API token 用于脚本等独立 API 调用，页面不包含 token。登录和退出接口已移除；来自其他站点的写请求仍会被拒绝。

`/api/v1/settings` 只返回监听地址、数据路径、时区、后台开关、当前时间和数据库状态；健康检查不返回运行配置。所有应用 HTTP 响应包含 `X-Request-ID`，错误 JSON 中也包含 `trace_id`。路径按表格精确匹配，多余的尾斜杠返回 404。

```bash
curl -i http://127.0.0.1:8989/healthz
curl -i http://127.0.0.1:8989/readyz
```

调用管理 API 时，将 `NGA_REMINDER_API_TOKEN` 设为与启动配置一致的值：

```bash
curl -i -H "Authorization: Bearer $NGA_REMINDER_API_TOKEN" http://127.0.0.1:8989/api/v1/settings
```

## NGA 账号与手动采集

1. 在管理页粘贴完整 Cookie（可带 `Cookie:` 前缀），或留空 Cookie、填写 passport UID/CID。点击“校验并保存凭据”。校验请求使用需要认证的用户回复接口；公开资料页可读不能证明 Cookie 有效。
2. 展开“新增 TID 监控”，填写 TID、可选备注和初始化模式。一个 TID 只保留一个监控。
3. 进入详情，点击“手动运行一次”。请求立即返回，采集在本进程中串行执行；页面展示页数、新增数量、错误摘要和运行关联 ID。运行时可浏览数据，其他采集、账号操作和监控修改返回 409，等待完成后重试。
4. 从详情或概览的“已保存主题”进入帖子列表。正文按转义后的纯文本显示，保留 BBCode、来源链接与资源地址；本阶段不下载资源，不发送通知。

账号只保留一份。Cookie 以 AES-256-GCM 加密后写入 SQLite；页面/API 只显示脱敏 UID、可用状态和最近校验结果。替换失败保留原凭据及其状态，并展示新凭据失败原因。“校验已保存凭据”确认失效或采集返回认证错误时，账号进入 `auth_paused`；重新校验成功或保存有效新凭据后解除认证暂停，保留用户的暂停选择。

初始化与重跑规则：

- **保存全部可访问历史（`full`）**：逐页保存主楼、回复和楼中楼，完成整轮后建立静默基线。热门引用不另存一份。
- **从现在开始（`from_now`）**：首次成功运行读取首页和末页，以实际楼层建立水位，不导入旧帖子。后续保存新楼层及新评论；旧楼层下的评论按 NGA 发帖时间与基线时间区分，若未提供时间则不把它当作新评论。
- 两种模式都不产生历史通知。后续手动采集逐页重读、按自然键仅插入新增内容，能发现旧楼层下新增的楼中楼；现阶段每轮请求量随主题页数增长。使用首页返回的页数快照，本轮期间新增的尾页留到下轮。
- 每页内容与本轮计数一并提交；整轮成功才提交最终水位和基线。中途失败可能已经保存部分页，但不会前移原水位或提前完成基线；重跑通过唯一键复用已存内容，不修改已有正文。
- 暂停影响后续自动调度，仍允许显式手动运行。阶段 02 尚无自动调度；主题不存在时标为 `missing`，后续不得自动抓取，可手动重试恢复。待审核标为 `skipped_pending`，保留原水位。
- 修改 TID 或初始化模式会重置基线；只改备注不影响水位。也可选择模式后单独重置基线。重置和删除监控都保留帖子及运行记录，已保存内容始终可从概览进入。

NGA 单次 HTTP 请求超时 15 秒，请求间隔至少 500 毫秒。账号校验整体最多等待 25 秒；回复接口返回“服务器忙”时每秒重试，最多 10 次，空 HTTP 503 每两秒重试、最多 3 次。主题采集失败由用户显式重跑，不运行后台重试队列。

独立 API 均要求 `Authorization: Bearer <api_token>`，写入 body 为 JSON：

| 请求 | 参数 / 返回 |
| --- | --- |
| `GET /api/v1/nga-account` | 脱敏账号状态；不返回 Cookie 或密文 |
| `PUT /api/v1/nga-account` | `{"cookie":"…"}` 或 `{"passport_uid":"…","passport_cid":"…"}`；校验成功才保存 |
| `POST /api/v1/nga-account/check` | 校验当前已保存凭据，无需 body |
| `GET /api/v1/watches` | 概览：`account`、`watches`、`threads` |
| `POST /api/v1/watches` | `{"tid":1001,"label":"备注","init_mode":"full"}`；创建返回 201 |
| `GET /api/v1/watches/:id` | `watch` 和该 TID 最近 20 次 `runs` |
| `PUT /api/v1/watches/:id` | 完整的 `tid`、`label`、`init_mode` 配置 |
| `POST /api/v1/watches/:id/pause`、`.../resume` | 暂停 / 恢复，无需 body |
| `POST /api/v1/watches/:id/reset` | `{"init_mode":"full"}` 或 `{"init_mode":"from_now"}` |
| `DELETE /api/v1/watches/:id` | 仅删除监控配置，保留帖子 |
| `POST /api/v1/watches/:id/run` | 返回 202、运行记录和 `Location: /api/v1/runs/:id` |
| `GET /api/v1/runs/:id` | 运行状态、时间、来源、页数、新增数量、错误摘要和关联 ID |
| `GET /api/v1/threads/:tid/posts?page=1` | `posts`、`total`、`page` 和最近 `runs`，每页 50 条 |

参数错误返回 400，记录不存在返回 404，已有任务或核验模式返回 409，NGA 认证拒绝返回 422。远端故障不会被当作空数据成功；错误响应包含 `trace_id`，详细原因和栈见日志。

## 数据与日志

当前创建 `accounts`、`watches`、`runs`、`posts` 四张业务表；GORM 启动时升级已有 Go 数据库。旧会话表不再读写，其他业务表随后续功能增加。帖子通过 `(tid, key)` 唯一键去重：主楼为 `main`，普通回复和楼中楼均为 `pid:<PID>`；楼中楼保留 JSON 嵌套确定的父键，`comment_to_id` 仅作为原始元数据保存。

持久化时间统一使用 UTC，页面按配置时区展示。部署密钥用于 NGA 凭据加密落盘，API token 保留在配置中；密钥丢失后不能解密已存 Cookie。资源目录当前只创建并检查可写性。

SQLite 使用 WAL、5 秒 busy timeout 和单连接。数据库通过 `application_id` 标记归属；非空且没有 Go 标记的 SQLite 会被拒绝打开，避免误用旧 Rust 数据库。Go 必须使用独立的数据路径，旧 PG 数据通过 [阶段 09](spec/09-data-migration.md) 显式迁移。

日志默认以 info 级别逐行输出 JSON 到 stdout。一次请求中的操作共用 `trace_id`，每层记录 `span_id`、`parent_span_id`、`operation`、开始和结束；结束记录 `result` 和 `duration_ms`。SQL 记录挂在对应 repository 操作下，使用占位符，不打印实参；HTTP 只记录匹配的路由，不记录 query、请求头或表单内容。

异步采集使用独立 trace：运行记录中的 `trace_id` 对应采集，`source_trace_id` 对应触发 HTTP 请求；请求日志也记录 `run_id` 和 `run_trace_id`。NGA HTTP 日志记录脱敏 URL、状态、耗时，采集业务日志记录 TID、页码、初始化模式和数量，Cookie 不写入日志。

错误由终止操作的入口记录一次，带 `error`、`causes`、`stack`；栈在错误创建或第三方边界捕获，包含函数、文件和行号。panic 恢复为通用 500 响应，原始栈留在日志中。`message`、`error`、`causes` 中的已知凭据会脱敏，关联 ID、数字及 JSON 结构保持完整；业务代码仍需按 [规范](../AGENTS.md#调用链与日志) 显式选择安全字段。

## 开发检查

在 `service/` 下执行：

```bash
gofmt -w cmd internal
CGO_ENABLED=0 go build ./...
CGO_ENABLED=0 go vet ./...
CGO_ENABLED=0 go test ./...
```

测试使用假凭据和临时 SQLite，覆盖配置、免登录管理页、API token、数据库故障、panic、完整调用链和脱敏，以及 Cookie 失效/替换、初始化、去重、增量、重置/删除、中断和重启。故障入口只存在于测试中，不进入生产路由。NGA fixture 位于 [testdata/nga](../internal/infrastructure/testdata/nga/)，复制自 [Rust 脱敏样本](../../archive/rust-service/service/tests/fixtures/nga/README.md)；测试内构造分页、新楼层和业务错误，只有远端 HTTP transport 被替换，Gin、NGA Client、service 与 SQLite 均实际执行。测试不访问真实 NGA、飞书或 Bark。
