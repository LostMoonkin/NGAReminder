# PG → SQLite 迁移操作

[Spec09](../spec/09-data-migration.md) 的交付入口是 [migrate-pg.sh](../../migrate-pg.sh)。在 Rust 停机后运行一次，完整导出 PG 的应用 schema，再转换为当前 Go 服务可直接打开的 SQLite 文件。**assets 继续挂载原目录，脚本不读取、复制或改动资源文件。**

## 准备

- 需要 Bash、Go 1.27.0，以及旧 PG 的读取权限；首次构建需要下载 Go 依赖。无需 CGO、C 编译器、系统 SQLite、Python 或 `pg_dump`。也可提前构建 `service/bin/pg-migrate`，迁移机只运行该二进制。
- 支持归档 Rust schema 的 0001～0007 迁移。目标是新的 SQLite 文件，不支持覆盖或合并已有库。
- 保留旧部署加密密钥、配置和 PG 备份。PG 读取用户须能读取所选 schema 的全部表；无权限、RLS 限制或不支持的 schema 会报错，不接受部分导出。
- 从旧部署确认时区和 `assets.download_enabled`。例如旧调度偏移为 `+08:00` 时使用 `Asia/Shanghai`；下载原来关闭则省略 `-download-enabled`。
- Go 制品、Compose 配置和副本演练在正式停机前准备完成。测试只操作副本；正式执行前停止 Rust HTTP、采集、通知、Bot 等所有写入进程，PG 保持运行。

## 一条命令导出并转换

在仓库根目录执行。以下均为占位值，密钥与连接串通过环境变量传递，不写进脚本或命令参数：

```bash
umask 077
export NGA_MIGRATE_PG_URL='postgres://USER:PASSWORD@HOST:5432/DATABASE?sslmode=require'
export NGA_REMINDER_ENCRYPTION_KEY='旧部署的 Base64 加密密钥'

./service/migrate-pg.sh \
  -output ./service/data/nga-reminder.db \
  -timezone Asia/Shanghai
```

默认用同一部署密钥解密 Rust 数据、重新加密 Go 数据。如果需要更换密钥，将 `NGA_MIGRATE_OLD_KEY` 设为旧密钥，`NGA_REMINDER_ENCRYPTION_KEY` 设为新密钥，并在 Go Compose 的 `environment` 中使用新密钥。两者都沿用 Base64 编码的 32 字节密钥格式。连接密码中的特殊字符按 URL 编码；`sslmode` 按实际 PG 配置选择。

参数：

| 参数 | 含义 |
| --- | --- |
| `-output PATH` | 必填，新的 SQLite 文件；已有文件、符号链接或同名报告均拒绝覆盖 |
| `-schema NAME` | PG 应用 schema，默认 `public` |
| `-source PATH` | 从本工具生成的完整 PG JSONL 快照转换；不连接 PG |
| `-timezone NAME` | Go 的 IANA 时区，默认 `Asia/Shanghai`；写入报告，上线环境必须一致 |
| `-download-enabled` | 旧部署允许资源下载时传入；转换为 Go 数据库中的资源下载开关 |

脚本从任何目录调用均可，输入输出相对路径以调用时的工作目录为准。它每次先以 `CGO_ENABLED=0` 构建独立的迁移工具；服务端正常运行的 module 和镜像不包含 PG 驱动。

成功后生成三个权限为 `0600` 的文件：

| 文件 | 内容 |
| --- | --- |
| `nga-reminder.db` | 当前 Go schema 的完整业务数据；WAL 已合并，可单独复制这个文件 |
| `nga-reminder.db.pg.jsonl` | 所选 PG schema 全部普通表的每一列、每一行，含未知表、旧审计数据、未恢复临时状态；附列名/类型、快照时间和完整行数尾记录 |
| `nga-reminder.db.report.json` | `ready`、源/目标表数量、ID 映射、转换计数、检查结果、仅归档表、快照 SHA-256 和上线环境要求；不含明文凭据 |

PG 导出使用只读 `REPEATABLE READ` 事务。JSONL 是本工具可重放的完整**表数据快照**，不替代含数据库角色、DDL 等信息的 PG 备份。它保留旧密文和原始字段，旧 Bot 表原有的收件地址也会原样保留，按敏感备份保管。大型库需要为快照、转换临时库和最终 SQLite 留出磁盘空间；资源无需额外复制空间。

转换中途失败返回非零退出码，不发布未完成的 SQLite；已成功导出的 PG 快照会保留。日志含阶段、表/字段、关联 ID、完整原因及栈，不输出凭据。最终 SQLite 和 `ready: true` 报告必须同时存在才进入核验。

无需重新连接 PG 即可重跑到另一个新文件：

```bash
./service/migrate-pg.sh \
  -source ./service/data/nga-reminder.db.pg.jsonl \
  -output ./service/data/retry.db \
  -timezone Asia/Shanghai
```

同一快照、密钥和参数产生相同业务数据与 ID；重新加密的随机 nonce 不要求相同。JSONL 必须有完整尾记录，截断或行数不一致会失败；保留报告中的 SHA-256 用于后续核对。断电留下的 `.pg-export-*`、`.sqlite-migrate-*` 等临时文件不是交付产物，重跑不读取它们。

## 映射与差异

| 旧数据 | Go 结果 |
| --- | --- |
| NGA 账号 / Cookie | 保留完整 Cookie 或 UID/CID，校验一致性并重新加密；`invalid/paused` 转为 `auth_paused`，`unchecked` 保留未校验状态 |
| TID / UID watch | 保留目标、名称、启停、初始化模式、基线、已提交水位、频率和免拉取；手动/错误暂停保持暂停，认证暂停保留为 `auth_paused`；已删除 watch 不复活，历史 ID 不被新 watch 复用 |
| TID 历史边界 | 保留楼层水位及远端回复总数/页数快照；缺少快照时首次只从水位补查尾部。以快照时刻屏蔽旧楼层下从未保存的历史评论，避免首次 Go 采集制造历史通知；原已知缺口仍可补采 |
| 时间规则 | 星期转换为 1～7；处理 Rust 的整天、跨午夜频率规则语义，保留规则优先级；时间戳保留 UTC |
| 内容 | 主楼/回复/评论、64 位 TID/PID/UID、正文、父子关系、时间和资源关联保留；全部主题独立迁入，含无帖子主题、标题、版面、楼主、coverage、远端分页与首次/最后观察时间；帖子页码和完整 raw payload 保留，新采集 raw 开关不影响历史迁移 |
| 资源 | URL、MIME、大小及已就绪资源的相对路径原样保留；未完成下载转为可手动处理状态。脚本不检查文件是否存在；Go 资源扫描与 ZIP 导出负责显示缺失文件或回退远程链接 |
| Bark / 飞书 | 解密并转换渠道配置，渠道开关取原渠道与 integration 开关的共同有效状态；单个飞书应用映射为 Go 全局应用 |
| Bot | 保留启用的私聊 `owner`，群授权需与这些管理员对应；保留全部旧消息的去重标识。禁用绑定、一次性绑定码、连接状态、待执行回复不恢复 |
| 系统告警 | 保留开启/已解除告警及所有渠道投递；已发送保持 sent，未确认的 pending/sending 转 failed，核对后手动重试；告警与内容投递分开映射 ID，不覆盖旧投递 |
| 续期 | 保留加密登录凭据和有效私聊绑定；原 `invalid/cooldown` 凭据保留但关闭自动续期，核对后手动启用；未完成登录中断，不恢复验证码或协议上下文 |
| 事件 / 投递 | 保留已读状态、来源和投递结果；已投递为 `sent`，旧未发/发送中为 `failed`，需核对后手动重试。同帖事件合并时保留未读、同渠道已发送结果优先，防止重发 |
| 运行 / 回填 / 缺口 | 保留结果摘要；未完成运行/回填为 `interrupted`。缺口保留提示页、原发现时间、截止时间和尝试次数，过期不延期，后续使用 Go 重试时间表 |

多个 NGA 账号、多个飞书 integration、Telegram/QQ、启用的低权限绑定、无私聊绑定的群 owner、没有具体会话的全局绑定，或不同管理员的授权群范围不同，都会拒绝转换。先在旧服务或演练副本中明确处理这些配置后重新导出；工具不会任意选一个应用或把低权限用户提升为管理员。TID 已开启的历史页面并发数按 [Spec11](../spec/11-thread-page-concurrency.md) 导入，未开启时为 1。旧错误正文、逐次投递响应和租约等运行时不用的字段完整保留在快照；系统告警及其投递已进入 Go 模型。原文件名也迁入，用于缺失资源重新下载时选择扩展名。

旧 `assets.max_download_bytes` 和 `persistence.store_raw_payload` 来自部署配置，不在 PG 中，无法从数据库自动迁移。上线前分别设置 `NGA_REMINDER_MAX_DOWNLOAD_BYTES`（默认 `10485760`）和 `NGA_REMINDER_STORE_RAW_PAYLOAD`（默认 `false`），沿用旧配置的实际值。

## 核验与上线

1. 检查退出码和 `report.json`：`ready` 为 `true`，源/目标数量、合并/归档计数及 ID 映射符合预期。报告内的 `asset_paths_preserved_unchecked` 表示只保留路径，不代表文件存在性已验收。
2. Compose 指向新 SQLite，`NGA_REMINDER_BACKGROUND_ENABLED: "false"`，使用转换后的密钥和相同时区，**继续挂载原 assets 路径**。例如原资源在 `/srv/nga/assets` 时，保留该目录，只增加 SQLite 数据目录挂载：

   ```yaml
   environment:
     NGA_REMINDER_DATABASE_PATH: /app/data/nga-reminder.db
     NGA_REMINDER_ASSETS_PATH: /app/data/assets
     NGA_REMINDER_BACKGROUND_ENABLED: "false"
     NGA_REMINDER_TIMEZONE: Asia/Shanghai
     # API token、密钥和其余配置仍按 Compose 模板的 environment 填写
   volumes:
     - ./data:/app/data
     - /srv/nga/assets:/app/data/assets
   ```

3. 启动 Go，访问 `http://<局域网 IP>:8989/admin` 与 `/readyz`；查看 TID/UID 水位、基线、暂停状态、收件箱与旧投递结果，打开中文帖子、评论及 Markdown/ZIP 导出。资源管理页执行扫描，检查本来就缺失的文件；不要把“能读取数据库”当作所有旧文件都存在。
4. 确认后按[阶段 10](../spec/10-single-host-delivery.md)启用后台，首次增量运行应从原水位继续。旧失败投递由用户核对后手动重试。运行时不使用 PG，Rust 保持停止。
5. Go 正常处理业务前如需退回，停止 Go，继续使用未改动的原 PG、原 assets 和旧密钥恢复 Rust。Go 已产生新业务数据后不直接切回旧库，本工具不做反向同步。

## 已执行的开发自测

使用一次性 PostgreSQL 17 容器，按真实 0001～0007 schema 构造假凭据及 TID/UID、评论、分层资源路径、通知、Bot、续期、运行、回填与缺口数据。Spec12 补充无帖主题、未知 raw 字段、评论页码、原文件名及开启/已解除系统告警与投递 fixture。验证了完整导出前后 PG 数据一致、离线重放结果一致、Go 解密/管理页/ZIP 读取、错误密钥与不兼容配置失败、拒绝覆盖、截断快照失败、旧 Bot 去重和增量采集。未连接实际业务 PG、NGA、飞书或 Bark，未执行实际停机与上线。

复跑命令（`NGA_MIGRATE_TEST_PG_URL` 指向可创建临时 schema 的测试 PG；测试创建并清理自身 schema）：

```bash
CGO_ENABLED=0 go -C service/tools/pg-migrate build -o /tmp/pg-migrate .
CGO_ENABLED=0 go -C service/tools/pg-migrate vet ./...
NGA_MIGRATE_TEST_PG_URL='postgres://USER:PASSWORD@127.0.0.1:PORT/TEST_DB?sslmode=disable' \
  CGO_ENABLED=0 go -C service/tools/pg-migrate test -count=1 -v ./...
CGO_ENABLED=0 go -C service test ./...
```

未提供测试 PG 时，迁移集成测试会明确跳过，不能据此宣称完成 PG 迁移验收。实际旧库的最终迁移与上线验收留给部署时执行。

本轮完整性 review 与验证结果见 [Spec12 数据迁移审查](rust-parity-migration-review.md)。
