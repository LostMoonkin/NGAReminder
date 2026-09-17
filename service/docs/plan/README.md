# Go 服务端重构计划

阶段 01～04 已完成实现与自测，其余阶段待实现。这里统一维护新服务端的阶段安排与进度，具体需求和验收见相应 Spec；[历史项目计划](../../../archive/rust-service/PROJECT_PLAN.md) 保留在归档中。

## 已确认的方向

- 个人 side project，人工可维护性优先；Go + Gin + GORM + SQLite + zerolog，飞书接入官方 Go SDK。
- 不启用 CGO，SQLite 驱动与构建、自测、发布流程均按 `CGO_ENABLED=0` 验证。
- 按 handler、service、repository 分层，外部服务接入 infrastructure；具体规则见 [服务端 AGENTS.md](../../AGENTS.md)。
- 保留 TID/UID 监控、调度、通知、Bot 指令、Cookie 续期、Web 管理、内容保存与导出。
- Go 运行时仅支持 SQLite 和单机单进程，移除 PostgreSQL、多实例协调及为未来扩展预留的框架；旧 PG 数据通过一次停机迁移导入。
- 项目文档使用中文；不要求 TDD、覆盖率门槛或商业化稳定性设施。

## 阶段与交付物

默认按下表顺序推进。功能阶段交付可操作的功能及对应管理页，迁移和交付阶段提供相应工具与说明；依赖列说明完成条件，不表示要提前拆 tickets 或并行开发。

| 阶段 | Spec | 完成后可做什么 | 依赖 | 状态 |
| --- | --- | --- | --- | --- |
| 01 | [运行基础与日志](../spec/01-runtime-and-logging.md) | 启动服务、进入管理页、查询健康状态并追踪请求和错误 | 无 | 已完成 |
| 02 | [NGA 账号与 TID 监控](../spec/02-nga-and-thread-monitoring.md) | 配置 Cookie，手动保存主题历史和新增楼层 | 01 | 已完成 |
| 03 | [UID 监控与统一调度](../spec/03-user-monitoring-and-scheduling.md) | 自动监控主题和用户，配置拉取频率与免拉取时段 | 02 | 已完成 |
| 04 | [通知与收件箱](../spec/04-notifications.md) | 匹配新内容，通过 Bark/飞书发送通知并查看结果 | 03 | 已完成 |
| 05 | [飞书机器人](../spec/05-feishu-bot.md) | 绑定管理员，在飞书查询状态、查看监控和手动运行 | 04 | 待实现 |
| 06 | [Cookie 续期](../spec/06-cookie-renewal.md) | 在飞书确认登录、提交验证码并恢复因认证暂停的监控 | 05 | 待实现 |
| 07 | [内容浏览、资源与导出](../spec/07-content-and-export.md) | 浏览富文本，导出 Markdown/ZIP，维护本地资源 | 03 | 待实现 |
| 08 | [历史回填与楼层补偿](../spec/08-history-and-gap-recovery.md) | 回填指定日期后的用户回帖，补收审核后恢复的楼层 | 04、07 | 待实现 |
| 09 | [数据迁移](../spec/09-data-migration.md) | Rust 停机后将 PG 数据和资源迁入 Go SQLite，核验数据与进度 | 01～08 | 待实现 |
| 10 | [单机交付与上线](../spec/10-single-host-delivery.md) | 使用迁入数据启用 Go，并按中文说明部署、备份和恢复 | 01～09 | 待实现 |

实际切换顺序：**Rust server 停机 → PG 迁移 SQLite → 核验 → Go 服务上线**。Go 制品、迁移工具、部署配置和副本演练在正式停机前准备完成；阶段编号不是停机期间的开发顺序。

## 本轮的简化边界

- Web 管理面向可信内网，直接访问，不设后台账号、密码或会话；管理 API 使用 API token，Bot 保留可信身份绑定和敏感操作私聊限制。
- 后台任务由一个进程负责。中断任务显示为中断并允许重跑，不要求从每个内部步骤自动恢复。
- 通知保留发送结果和有限重试；不承诺服务崩溃与第三方接收之间的严格 exactly-once。
- 新服务使用独立的 SQLite 文件和资源目录。阶段 09 负责旧 Rust PG 数据与资源的一次离线迁移，保留源库和备份；旧 API 逐字段兼容仍不在范围内，不能覆盖、清空或原地升级旧库。
- 管理页随功能实现，不预先引入独立前端工程。具体页面与接口形状在实现对应 Spec 时确定并记录。
- 每个 Spec 只定义目标、行为、约束和验收；数据库表结构、详细接口和文件划分不在这里提前铺开。

## 术语与历史参考

- **监控（watch）**：一个 TID 或 UID 的采集目标及其配置。
- **基线（baseline）**：监控第一次成功初始化的位置；初始化期间不产生新内容通知。
- **游标/水位（cursor）**：已成功采集到的位置；失败和跳过不推进未完成位置。
- **运行（run）**：一次手动或自动采集及其结果。
- **免拉取时段**：只阻止自动访问 NGA 的时间段；手动运行仍可执行。

协议解析优先参考 [NGA 契约](../../../archive/rust-service/service/docs/NGA_API_CONTRACT.md) 和 [脱敏 fixture](../../../archive/rust-service/service/tests/fixtures/nga/README.md)。其他历史设计由各 Spec 按需引用；新 Spec 中明确的简化取代旧实现约束，不要求搬迁其内部架构。
