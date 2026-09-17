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

服务默认监听 `0.0.0.0:8989`。在局域网其他机器上打开 `http://<服务端局域网 IP>:8989/admin`；服务端本机也可打开 [本地管理页](http://127.0.0.1:8989/admin)。管理页直接访问，无需登录，可配置 NGA 账号、管理 TID/UID 监控、配置自动频率与免拉取时段、手动采集和浏览已保存帖子。启动配置更改在重启后生效。

已有配置文件需同步将 `listen_address` 改为 `0.0.0.0:8989`，或在启动时设置 `NGA_REMINDER_LISTEN_ADDRESS=0.0.0.0:8989`。旧配置中的 `admin_username`、`admin_password`、`cookie_secure` 已移除，升级时删除这些字段。

也可以编译后运行；管理页和时区数据已包含在二进制内：

```bash
CGO_ENABLED=0 go build -o bin/nga-reminder ./cmd/server
./bin/nga-reminder -config config.json
```

使用 Ctrl-C 或向服务进程发送 SIGTERM 停止。启动脚本通过 `exec` 运行二进制，脚本 PID 即服务 PID。服务取消正在执行的采集，最多等待 10 秒处理已有 HTTP 请求，保存中断结果后关闭网络和数据库连接；运行日志直接输出到当前终端。异常退出留下的 `running` 记录在下次启动时标为 `interrupted`，保留已提交水位；未暂停的监控到期后重新运行，也可手动重跑，不补造离线期间的每次任务。

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

该开关仅改变本次进程的运行模式，不改写业务开关。管理页会显示“后台任务已关闭”；凭据校验和手动采集也返回 409，保证核验期间不访问 NGA。仍可查询和管理本地数据。自动调度和后续阶段的后台任务同样受此开关控制。

启动失败会以非零退出码结束。日志指出配置字段或无法访问的路径，并保留错误原因和栈；常见原因是密钥编码不正确、端口被占用、数据目录不可写或数据库不属于 Go 服务。

## 管理入口与健康检查

| 请求 | 认证与行为 |
| --- | --- |
| `GET /` | 跳转 `/admin` |
| `GET /admin` | 无需认证，账号、监控、已保存主题和运行设置 |
| `GET /admin/watches/:id` | 监控配置、手动运行和最近 20 次结果；运行中每 3 秒刷新 |
| `GET /admin/threads/:tid?page=1` | 已保存帖子及安全富文本，每页 50 条 |
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

## NGA 账号与监控

1. 在管理页粘贴完整 Cookie（可带 `Cookie:` 前缀），或留空 Cookie、填写 passport UID/CID。点击“校验并保存凭据”。校验请求使用需要认证的用户回复接口；公开资料页可读不能证明 Cookie 有效。
2. 展开“新增 TID 监控”或“新增 UID 监控”，填写目标、备注和采集频率。每个 TID、每个 UID 各只保留一个监控。创建后进入自动调度，未配置或未验证账号时等待账号恢复。TID 可选择初始化模式，UID 固定从首次成功运行的水位开始。
3. 进入详情，可调整星期/时间段覆盖间隔和免拉取时段，也可点击“手动运行一次”。手动请求立即返回，自动和手动采集在本进程中串行执行；页面展示来源、开始/结束时间、页数、新增数量、错误摘要和运行关联 ID。运行时可浏览数据，其他采集、账号操作和监控修改返回 409，等待完成后重试。
4. 从详情或概览的“已保存主题”进入帖子列表。正文按安全富文本显示，支持常用 NGA 标记、来源链接与资源地址；UID watch 详情可进入该用户内容。资源下载和导出见下文。

账号只保留一份。Cookie 以 AES-256-GCM 加密后写入 SQLite；页面/API 只显示脱敏 UID、可用状态和最近校验结果。替换失败保留原凭据及其状态，并展示新凭据失败原因。“校验已保存凭据”确认失效或采集返回认证错误时，账号进入 `auth_paused`；重新校验成功或保存有效新凭据后解除认证暂停，保留用户的暂停选择。

TID 初始化与重跑规则：

- **保存全部可访问历史（`full`）**：逐页保存主楼、回复和楼中楼，完成整轮后建立静默基线。热门引用不另存一份。
- **从现在开始（`from_now`）**：首次成功运行读取首页和末页，以实际楼层建立水位，不导入旧帖子。后续保存新楼层及新评论；旧楼层下的评论按 NGA 发帖时间与基线时间区分，若未提供时间则不把它当作新评论。
- 两种模式都不产生历史通知。后续采集逐页重读、按自然键仅插入新增内容，能发现旧楼层下新增的楼中楼；现阶段每轮请求量随主题页数增长。使用首页返回的页数快照，本轮期间新增的尾页留到下轮。
- 每页内容与本轮计数一并提交；整轮成功才提交最终水位和基线。中途失败可能已经保存部分页，但不会前移原水位或提前完成基线；重跑通过唯一键复用已存内容，不修改已有正文。
- 手动暂停同时阻止自动和手动运行；恢复后继续使用原水位。主题不存在时标为 `missing`，后续不得自动抓取，可手动重试恢复。待审核标为 `skipped_pending`，保留原水位。
- 修改 TID 或初始化模式会重置基线；只改备注或调度配置不影响水位。也可选择模式后单独重置基线。重置和删除监控都保留帖子及运行记录，已保存内容始终可从概览进入。

NGA 单次 HTTP 请求超时 15 秒，请求间隔至少 500 毫秒。账号校验整体最多等待 25 秒；回复接口返回“服务器忙”时每秒重试，最多 10 次，空 HTTP 503 每两秒重试、最多 3 次。单轮采集失败保留已提交水位；可手动重跑，或等待下一次调度。认证暂停与主题不存在会停止自动抓取，待审核或临时失败则按间隔再次运行。

UID 初始化与增量规则：

- 首次成功读取主题/回帖列表，分别记录 `(发布时间, TID)` 与 `(发布时间, PID)` 水位，不访问历史帖子详情、不导入历史。主题列表按服务端总数分页，不能因无权记录使页面较短而提前结束；回复列表按发布时间倒序读取。
- 后续只补全新候选：主题只请求第 1 页并保存目标 UID 的主楼；回帖只按 TID/PID 请求目标内容。详情作者必须仍为目标 UID；按 PID 获取的 `lou=0` 仍保存为回复，不会覆盖主楼或另一个 PID。
- 主题列表可能受旧帖回复活动影响排序，因此每轮按总页数扫描；回复读到旧水位即可停止，同秒内容继续按 PID 区分。未知总数的回复列表满页继续，短页或成功页后的空体 503 结束；第一页空体 503、业务失败、缺少分页信息均不能当作空列表。
- 两份列表和所需详情全部成功后，内容、两份水位和运行结果在一个 SQLite 事务中提交。失败不写入本轮部分内容，也不推进任意一份水位。明确的成功空列表可以建立空水位。
- 重置或修改 UID 后重新静默建立水位；已有帖子保留并按自然键去重。详情页面显示两份水位，内容可在概览的“已保存主题”浏览。符合条件的增量由阶段 04 通知链路处理。

## 自动调度与免拉取

每个监控的基础间隔默认 60 秒，范围 30～86400 秒；可以添加多个星期/时间段覆盖规则，按配置顺序取第一条命中规则，否则用基础间隔。正常运行结束后以结束时刻的规则计算下次执行时间。单进程每秒检查到期监控，按到期时间串行运行；前一个任务耗时较长时，后面的任务会顺延。

星期为 `1`～`7`（周一至周日），时间使用严格 `HH:MM`。开始包含、结束不包含；例如周一 `23:00`～`02:00` 包含周二凌晨，归属于周一。起止相同表示开始日该时刻至次日同一时刻。所有判断使用服务配置时区。

免拉取与间隔覆盖分别维护，自动到期时先判断免拉取；命中后不访问 NGA，记录 `skipped_no_fetch`，将下次时间移到相交或首尾相接时段的共同结束，不每分钟重复跳过。每周全天免拉取时，记录一次后等待配置变更，`next_run_at` 和 `no_fetch_until` 为 `null`。手动运行绕过免拉取，且不破坏已经记录的本段跳过；手动暂停、认证暂停和正在采集仍会阻止运行。

创建、恢复或保存监控配置后重新进入到期检查；自动与手动使用同一个运行入口。启动时将遗留 `running` 记录标记为 `interrupted`，到期后只重新运行一次，不追补离线的定时次数。账号修复仅解除认证暂停，不改变手动暂停。

调度字段可同时用于创建和修改，例如：

```json
{
  "kind": "uid",
  "uid": 2001,
  "label": "关注用户",
  "interval_seconds": 60,
  "interval_rules": [
    {"weekdays": [1, 2, 3, 4, 5], "start": "09:00", "end": "18:00", "interval_seconds": 300}
  ],
  "no_fetch_periods": [
    {"weekdays": [1, 2, 3, 4, 5, 6, 7], "start": "23:00", "end": "07:00"}
  ]
}
```

修改时省略调度字段保留原值；传 `[]` 清空对应规则。兼容阶段 02 的 TID 请求：省略 `kind` 时默认 `tid`，原监控升级后使用 60 秒基础间隔，原暂停状态和基线保持。

独立 API 均要求 `Authorization: Bearer <api_token>`，写入 body 为 JSON：

| 请求 | 参数 / 返回 |
| --- | --- |
| `GET /api/v1/nga-account` | 脱敏账号状态；不返回 Cookie 或密文 |
| `PUT /api/v1/nga-account` | `{"cookie":"…"}` 或 `{"passport_uid":"…","passport_cid":"…"}`；校验成功才保存 |
| `POST /api/v1/nga-account/check` | 校验当前已保存凭据，无需 body |
| `GET /api/v1/watches` | 概览：`account`、`watches`、`threads`；监控附带 `last_run`、`next_run_at`、`no_fetch`、`no_fetch_until` |
| `POST /api/v1/watches` | TID：`{"kind":"tid","tid":1001,"label":"备注","init_mode":"full"}`；UID：`{"kind":"uid","uid":2001,"label":"备注"}`；创建返回 201 |
| `GET /api/v1/watches/:id` | `watch` 和该 watch 最近 20 次 `runs`，以及 `timezone`、`background_enabled` |
| `PUT /api/v1/watches/:id` | 完整目标配置：`kind`、对应的 `tid` 或 `uid`、`label`；TID 还需 `init_mode`，可带调度字段 |
| `POST /api/v1/watches/:id/pause`、`.../resume` | 暂停 / 恢复，无需 body |
| `POST /api/v1/watches/:id/reset` | TID：`{"init_mode":"full"}` 或 `{"init_mode":"from_now"}`；UID 可省略 body，重新静默建立水位 |
| `DELETE /api/v1/watches/:id` | 仅删除监控配置，保留帖子 |
| `POST /api/v1/watches/:id/run` | 返回 202、运行记录和 `Location: /api/v1/runs/:id` |
| `GET /api/v1/runs/:id` | 运行状态、时间、来源、页数、新增数量、错误摘要和关联 ID |
| `GET /api/v1/threads/:tid/posts?page=1` | `posts`、`total`、`page` 和最近 `runs`，每页 50 条 |

参数错误返回 400，记录不存在返回 404，已有任务、手动暂停或核验模式返回 409，NGA 认证拒绝返回 422。远端故障不会被当作空数据成功；错误响应包含 `trace_id`，详细原因和栈见日志。

## 数据与日志

基础业务表为 `accounts`、`watches`、`runs`、`posts`，阶段 04～07 增加通知、Bot、续期与资源元数据表；GORM 启动时升级已有 Go 数据库，保留阶段 02 的 TID 水位、内容和暂停状态，并将旧 TID 唯一索引替换为分别约束 TID/UID 的索引。旧会话表不再读写，其他业务表随后续功能增加。帖子通过 `(tid, key)` 唯一键去重：主楼为 `main`，普通回复和楼中楼均为 `pid:<PID>`；楼中楼保留 JSON 嵌套确定的父键，`comment_to_id` 仅作为原始元数据保存。

持久化时间统一使用 UTC，页面按配置时区展示。部署密钥用于 NGA 凭据加密落盘，API token 保留在配置中；密钥丢失后不能解密已存 Cookie。资源文件与导出临时文件保存在资源目录；备份时与数据库一同保存。

SQLite 使用 WAL、5 秒 busy timeout 和单连接。数据库通过 `application_id` 标记归属；非空且没有 Go 标记的 SQLite 会被拒绝打开，避免误用旧 Rust 数据库。Go 必须使用独立的数据路径，旧 PG 数据通过 [阶段 09](spec/09-data-migration.md) 显式迁移。

日志默认以 info 级别逐行输出 JSON 到 stdout。一次请求中的操作共用 `trace_id`，每层记录 `span_id`、`parent_span_id`、`operation`、开始和结束；结束记录 `result` 和 `duration_ms`。SQL 记录挂在对应 repository 操作下，使用占位符，不打印实参；HTTP 只记录匹配的路由，不记录 query、请求头或表单内容。

异步采集使用独立 trace：运行记录中的 `trace_id` 对应采集，`source_trace_id` 对应触发 HTTP 请求或调度 tick（每个 tick 独立 trace）；请求日志也记录 `run_id` 和 `run_trace_id`。NGA HTTP 日志记录脱敏 URL、状态、耗时，采集业务日志记录 watch ID、TID/UID、PID、页码、触发来源、初始化模式、水位和数量，Cookie 不写入日志。

错误由终止操作的入口记录一次，带 `error`、`causes`、`stack`；栈在错误创建或第三方边界捕获，包含函数、文件和行号。panic 恢复为通用 500 响应，原始栈留在日志中。`message`、`error`、`causes` 中的已知凭据会脱敏，关联 ID、数字及 JSON 结构保持完整；业务代码仍需按 [规范](../AGENTS.md#调用链与日志) 显式选择安全字段。

## 开发检查

在 `service/` 下执行：

```bash
gofmt -w cmd internal
CGO_ENABLED=0 go build ./...
CGO_ENABLED=0 go vet ./...
CGO_ENABLED=0 go test ./...
```

测试使用假凭据和临时 SQLite，覆盖配置、免登录管理页、API token、数据库故障、panic、完整调用链和脱敏，以及 Cookie 失效/替换、TID/UID 初始化与增量、详情作者核对、PID 回复身份、重置/删除、中断与重启、旧 Go SQLite 升级、调度边界和免拉取。故障入口只存在于测试中，不进入生产路由。NGA fixture 位于 [testdata/nga](../internal/infrastructure/testdata/nga/)，复制自 [Rust 脱敏样本](../../archive/rust-service/service/tests/fixtures/nga/README.md)；测试内构造分页、新楼层和业务错误，只有远端 HTTP transport 被替换，Gin、NGA Client、service 与 SQLite 均实际执行。测试不访问真实 NGA、飞书或 Bark。

## 通知与收件箱（阶段 04）

从概览进入 `/admin/notifications`。先配置飞书自建应用（所有通知目标共用），再添加 Bark 或飞书接收者；Bark 默认服务地址 `https://api.day.app`，支持自定义 HTTP/HTTPS 地址、设备 key 和分组。飞书需开启机器人并授予消息发送与图片上传权限，SDK 使用 [官方 Go SDK](https://github.com/larksuite/oapi-sdk-go)。凭据与完整接收地址加密保存，不在页面或 API 回显；修改时敏感字段留空保留，分组按本次输入更新。

在 watch 详情中选择多个通知渠道；TID 可配置作者 UID 白名单，留空表示所有作者，UID 仅匹配目标用户。初始化、重置后的基线、白名单外内容保持静默。匹配的新内容进入收件箱，未选渠道也可查看及标记已读。一个帖子只有一条事件，每个渠道只有一条投递，多个监控命中时追加来源。

投递异步执行，失败不回滚采集进度；最多尝试五次，重试间隔依次为 1、4、9、16 分钟，耗尽后可手动重试。关闭渠道阻止新增投递和自动重试；被 watch 引用时不能删除，解除引用后删除会保留历史并取消未成功投递。飞书最多附带三张可用 NGA 图片，下载或上传失败保留文本和原帖链接。重启保留事件、已读状态与投递结果；外部已接收但本地尚未记成功时可能重发。

API 均使用 Bearer token，写入 JSON：

| 请求 | 参数 / 行为 |
| --- | --- |
| `GET /api/v1/notifications?page=1` | 脱敏应用状态、渠道和每页 50 条收件箱事件，包含来源与投递结果 |
| `POST /api/v1/feishu-app` | `app_id`、`app_secret`，留空保留原值 |
| `POST /api/v1/channels`、`POST /api/v1/channels/:id` | `name`、`kind`（`bark`/`feishu`）、`enabled`；Bark 使用 `server_url`、`device_key`、`group`；飞书使用 `receive_id`、`receive_id_type`（默认 `chat_id`，也支持 `open_id`/`user_id`/`union_id`/`email`） |
| `POST /api/v1/channels/:id/test` | 对已启用渠道发送测试通知，核验模式禁用 |
| `POST /api/v1/channels/:id/enable`、`.../disable`、`.../delete` | 启停 / 删除渠道 |
| `POST /api/v1/inbox/:id/read`、`.../unread` | 修改已读状态 |
| `POST /api/v1/deliveries/:id/retry` | 将未成功投递重新加入发送，计数归零；渠道须存在且启用 |

watch 创建/更新额外接受 `channel_ids` 与 `author_uids` 数组；省略保留原配置，空数组清空。阶段 03 数据升级时已有 TID 内容建立静默观察记录，不会把历史帖子重新当作通知。


## 飞书 Bot（阶段 05）

在飞书自建应用中启用机器人、消息接收事件 `im.message.receive_v1` 和长连接订阅，授予私聊/群内 @机器人消息读取及消息发送权限。应用凭据与通知共用，在 `/admin/bot` 独立启停 Bot、查看连接状态和错误；应用配置改变后自动重连，连接启动失败每 30 秒再试。`background_enabled=false` 同时阻止 Bot 接入与发送。

管理页生成十分钟有效的一次性绑定码，管理员私聊 `/bind <code>` 完成绑定。绑定码只展示一次，数据库仅存摘要；身份和私聊地址加密保存。页面可撤销绑定，之后立即失去操作权限。

已绑定管理员支持 `/help`、`/status`、`/watch list`、`/watch run <watch_id>`。运行遵循管理页的暂停、账号和串行限制，接受后回复运行编号，不代表采集已经成功。群内需显式配置允许的 chat ID，发送者仍须已绑定；绑定仅支持私聊。群地址不回显，替换时勾选“更新允许群”。

消息 ID 在 SQLite 中先登记再执行业务，重复推送与正常重启重放不重复运行。异常退出发生在接收登记后时，用户需重新发出命令；不回放未完成副作用。日志记录独立消息 trace、命令类别和业务/回复调用，不记录原始私聊内容、绑定码或收件地址。SDK 内部日志关闭，网络请求统一由项目的 HTTP 日志记录。

| API（Bearer token） | 参数 / 行为 |
| --- | --- |
| `GET /api/v1/bot` | 脱敏设置、连接状态和绑定编号 |
| `POST /api/v1/bot` | `enabled`，可选 `groups`（空白分隔 chat ID 字符串）；省略保留，空串清空 |
| `POST /api/v1/bot/bind-code` | 返回一次性 `code`、`expires_at`，新码使旧码失效 |
| `POST /api/v1/bot/bindings/:id/revoke` | 撤销管理员身份 |


## Cookie 续期（阶段 06）

在 `/admin/renewal` 启用续期，填写 NGA 登录名/密码，选择已绑定管理员；敏感字段留空保留，不回显。需先保存一份 NGA Cookie 确定预期 UID。配置变更取消活动交互，重新选择管理员后再发起。

采集或校验已保存 Cookie 时，只有从可用转为明确认证失败才发送一次确认；网络错误、繁忙不触发登录。也可在页面手动发起，有活动交互时复用，不提前停用有效 Cookie。

指定管理员在原绑定私聊中使用 `/login status`、`/login confirm <request_id>`、`/login captcha <request_id> <code>`、`/login cancel <request_id>`。确认后建立独立 NGA 登录会话，上传并实际发送图形验证码后才等待六位答案；提交时按 NGA 页面公钥进行 RSA 加密。候选 Cookie 通过认证接口校验且 UID 与原账号一致后，在同一事务中替换并解除认证暂停，保留手动暂停。

交互有效期十分钟；密码/验证码/图片失败、取消、过期、配置或账号变更都需重新发起。短信、手机或腾讯挑战提示手动更新 Cookie。登录上下文仅在内存中，结束立即丢弃；启动时将未完成记录标记中断，不恢复密码或验证码提交。应用凭据改变也使原交互无法继续。不自动重试密码。

| API（Bearer token） | 参数 / 行为 |
| --- | --- |
| `GET /api/v1/renewal` | 脱敏配置、管理员绑定列表与最近交互 |
| `POST /api/v1/renewal` | `enabled`、`binding_id`、`name`、`password`；敏感字段留空保留 |
| `POST /api/v1/renewal/start` | 向指定私聊发送确认，返回活动交互；核验模式禁用 |

页面和普通 API 不返回密码、验证码或 Cookie；结果带可理解的阶段与错误，内部日志保留调用链及错误栈。


## 内容、导出与资源（阶段 07）

TID 内容在 `/admin/threads/:tid`，UID watch 详情中的“查看用户帖子”进入 `/admin/users/:uid`，每页 50 条。UID 页面和导出只读取该 UID 已保存内容，按 TID 分组，不额外访问 NGA。Web、Markdown 和通知共用标记解析：段落、强调、引用、代码、链接、图片及折叠；未知标记保留文本，HTML 只输出允许标签和 HTTP(S) 链接。相对图片路径通过已存资源元数据解析。

页面提供 Markdown 和 ZIP 下载。以开始时的最大帖子 ID 固定内容范围，每批最多 200 条、按 TID/楼层/父子关系/内部 ID 稳定排序。Markdown 包含标题、作者、时间、楼层/父帖和原帖链接。ZIP 包含 `content.md`、`metadata.json` 和 `assets/` 中已保存的相关资源，共享文件只打包一次，缺失项使用远程地址。服务生成临时文件后流式发送，完成、客户端断开或生成失败都会清理；异常退出遗留项可从资源页清理。

`/admin/resources` 管理下载开关及维护。默认关闭下载，仅保留资源地址；开启后采集在正文提交后下载，单个失败不会回滚正文、水位或通知。也可重新下载已有内容缺失的资源。下载仅允许 HTTPS 的 `img.nga.cn`、`img.nga.178.com`、`img4.nga.178.com`，禁止跳转，检查实际文件类型，单文件最多 20 MiB；支持 PNG/JPEG/GIF/WebP/BMP、PDF、ZIP/RAR/7z。文件名使用内容 SHA-256 与类型扩展名，二进制不写入 SQLite。

资源页的扫描只读，显示缺失/未下载、无引用元数据、无引用普通文件和临时文件。勾选确认再清理时重新扫描，只删除超过 24 小时且没有正文引用的普通文件和临时文件；保护同内容共享文件，不跟随文件或目录符号链接，不删除正文。资源下载期间清理会等待；有采集或账号操作时返回忙碌提示。元数据保留，便于定位和显式重新下载。

| API（Bearer token） | 行为 |
| --- | --- |
| `GET /api/v1/users/:uid/posts?page=1` | 目标用户的已保存内容及分页信息 |
| `GET /api/v1/exports/threads/:id?format=markdown`、`.../users/:id?format=zip` | `format` 为 `markdown` 或 `zip`，下载已有内容 |
| `GET /api/v1/resources` | 只读资源扫描及下载设置 |
| `POST /api/v1/resources` | `{"download_enabled":true}`，修改下载开关 |
| `POST /api/v1/resources/redownload` | 下载缺失项，返回成功数；失败项保留错误并写日志 |
| `POST /api/v1/resources/cleanup` | `{"confirm":true}`，重新扫描并清理符合条件的旧文件 |
| `GET /api/v1/assets/:name` | 下载资源目录内的普通文件，拒绝越界和符号链接 |
