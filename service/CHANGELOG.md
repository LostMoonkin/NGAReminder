# 更新日志

[English](CHANGELOG.en.md)

## 未发布

### 机器人交互与 Cookie 续期（M8）

- 重构平台模型：新增 `platform_integrations`（平台连接持有应用凭据，`bot_enabled` 归属连接），重建 `notification_channels`（引用连接并独立保存通知目标），通过 partial unique index 保证同一平台最多一个机器人连接，并提供平台级原子切换 API。
- 新增通用机器人模块：`bot/` 平台适配器 trait、标准 `BotEvent`、有界入站队列、`(integration, message_id)` 持久化去重、命令路由/分发、基于角色和私聊的授权，以及带短 TTL 图片支持的 `bot_outbox` 投递。
- 新增首个飞书适配器：按 integration 协调长连接，解析 `im.message.receive_v1`，支持回复和主动发送；事件回调不访问数据库。
- 新增命令：`/help`、`/status`、`/bind`、`/watch list|run`、`/login status|confirm|captcha|cancel`。敏感登录交互仅限 owner 私聊；一次性绑定码使用 SHA-256 保存，10 分钟过期。
- 支持 NGA Cookie 续期：新增 `nga_account_renewal_settings`、`nga_login_sessions`（唯一活动会话）和 `watch_targets.pause_reason`（区分认证暂停与用户暂停）；认证失败时暂停全部相关监控并通知 owner；登录状态机依次执行 confirm → challenge → captcha → 候选 Cookie 验证 → 原子替换，并恢复因认证失败暂停的监控；支持失败冷却和凭据失效停用。
- 新增 `nga_web_login_v1` 登录协议适配器：RSA PKCS#1 v1.5 密码加密、验证码获取、兼容 array/object 的 `data[3]` 候选 Cookie 提取、`window.script_muti_get_var_store=` 包装解析，以及脱敏 fixture 驱动测试。
- 凭据加密升级到 v2：使用绑定字段上下文的 AEAD AAD（例如 `nga_account:{id}:renewal_password:v2`），防止密文跨字段互换。
- 更新管理页面：通知中心拆分为“平台连接 / 通知目标 / 机器人授权”，支持连接状态、测试连接、平台级机器人原子切换、一次性绑定码和绑定管理；服务设置新增“Cookie 自动续期”配置区及活动登录会话展示和取消按钮；续期配置校验 owner 私聊绑定。

### 已冻结的设计

- 资源持久化确定为“数据库元数据 + SHA-256 内容寻址本地文件”；资源二进制不会存储在 PostgreSQL `BYTEA` 或 SQLite `BLOB` 中。
- 新增 `persistence.store_raw_payload`，默认关闭。新帖子保存标准化字段，只有显式开启时才保存完整源 JSON。

### M5（已完成）

- 新增 SQLite/PostgreSQL 资源元数据和帖子-资源关联表。
- 新增有界的正文图片发现、HTTPS NGA 主机校验、待下载任务和 SHA-256 内容寻址本地存储。
- 新增 `attachPrefix`/`attches` 解析，使 NGA 附件元数据进入与正文图片相同的本地资源队列。
- 新增 NGA 标记到 Markdown 的解析和渲染，支持链接、图片、引用、格式、代码块、换行和不安全链接拒绝。
- 新增受保护的主题/用户 Markdown 和 ZIP 导出接口，包含元数据和已就绪的本地资源。
- 新增导出、资源安全、渲染器、ZIP 和幂等性测试。
- 完成 Markdown/ZIP 导出及资源持久化工作流的外部验收。

### 稳定性

- 新增对 NGA 主题 `code=51` 待审核响应的处理。受影响的抓取只跳过当前周期，游标和通知保持不变，下一次定时运行自动重试。

### M3

- 新增 PostgreSQL/SQLite 用户元数据以及相互独立的主题/回复游标。
- 新增类型化的用户主题、用户回复和 GBK 资料解析，支持无权条目和作者过滤。
- 新增用户基线和增量发现，只持久化被监控 UID 的主题帖、单条回复及可用楼中楼评论。
- 新增十次尝试的 NGA busy 策略，并在 `skipped_busy` 时保留游标。
- 新增用户监控 API/调度，以及与主题监控共享的帖子/事件去重写入。

### M4（已完成）

- 新增加密的 Bark/飞书渠道、TID/UID 规则、事务化事件匹配和按渠道去重的 outbox。
- 新增 Bark V2 与飞书企业应用交互卡片适配器。
- 飞书根据 `app_id`/`app_secret` 获取并缓存 `tenant_access_token`，在 token 被拒时刷新一次，并通过 `im/v1/messages` 向配置的群聊或用户 ID 发送消息。
- 飞书卡片现在会提取 NGA 图片标记，使用有界流式下载上传最多三张受信任图片，缓存 `image_key`，失败时回退为源链接，不阻塞文本投递。
- 通知链接现在打开带 `#pid{pid}Anchor` 的已保存主题页面，而不是 NGA 的孤立回复视图。
- 新增带租约的投递处理、重试/死信分类、投递历史、渠道测试发送以及渠道/规则管理 API。
- 完成 Bark 推送的外部验收，同时完成此前已验证的飞书投递工作流验收，覆盖路由、深链接、outbox 和投递结果。

### M2

- 新增主题、append-only 帖子、监控、游标、抓取运行和帖子事件的 PostgreSQL/SQLite schema。
- 新增对主题、回复、楼中楼评论和附件原始 payload 的类型化 NGA 解析。
- 新增认证主题页请求、账号级请求间隔、临时错误重试和类型化 NGA 业务错误。
- 新增全量基线导入、楼层游标增量采集、全局自然键去重、监控租约和自动调度。
- 新增主题监控 CRUD 和手动运行 API。

### M1

- 新增 Rust/Axum 服务骨架、PostgreSQL/SQLite 支持、API/管理台鉴权以及加密 NGA Passport 凭据管理。
- 新增首个响应式 `/admin` 管理台，支持 session 登录、概览卡片、监控控制、通知渠道/规则表单、内容浏览和导出下载。
- 新增受保护的概览、主题/帖子查询、事件收件箱和事件已读状态 API。
- 新增持久化的 `post_events.read_at` 状态，用于标记单条或全部事件为已读。
- 账号解密失败现在返回可恢复的配置状态，而不是通用内部错误；管理页面会引导管理员重新录入 NGA Cookie。
