# 监控与通知模型改造设计

## 1. 目标

本次改造围绕四项需求展开：

1. TID 监控可选择是否拉取历史数据；历史回溯可选择并发抓取及并发数；不回溯时只从当前水位之后抓取新增内容。
2. UID 监控不拉取历史数据；从监控启用后发现的新主题、新回复落库，用于通知重试和后续审计。
3. 通知过滤配置并入监控目标；通知中心只管理 Bark、飞书等通知渠道。
4. 管理页面、API、数据库和采集器使用同一套配置语义，避免“后端支持、页面不可配置”。

本设计基于当前实现：

- `watch_targets` 当前区分 `thread` 和 `user`，保存固定间隔、状态、租约和游标。
- 服务端已经支持 `schedule`，但管理台未暴露 schedule 编辑。
- 当前 TID 采集会保存主题全部内容；当前 UID 采集会保存 `nga_users`、主题、帖子和事件。
- 当前通知规则独立存储 `tid`、`uid`，匹配逻辑为可选条件 AND。

## 2. 目标行为

### 2.1 TID 监控

TID 监控配置包括：

- 目标 TID。
- 初始同步模式：
  - `full`：首次运行拉取当前可访问的全部历史页面，建立静默基线；后续只拉取新增内容。
  - `incremental`：首次请求第 1 页取得元数据，并在总页数大于 1 时只探测最后一页以确认当前最大楼层，记录水位但不保存页面内容、不发送历史通知；后续只拉取水位之后的新内容。
- 历史回溯并发：
  - `parallel_enabled`：是否并发请求历史页面。
  - `parallelism`：最大同时请求数，范围 `1..16`，默认 `2`。
- 检查间隔：固定 fallback 间隔或时间段 schedule。
- 通知过滤：通知渠道、作者 UID 白名单。每条本轮新发现的内容只判断其作者 UID 是否命中白名单。

并发数只表示抓取任务的最大 in-flight 数，不绕过 NGA 的账号级请求限速。所有请求仍必须经过现有请求节流器和重试策略。

TID 增量的强保证范围是 NGA `result` 中新增的主题楼层回复。楼中楼位于父帖的 `comments` 数组，且新增楼中楼不一定推动 `vrows`；因此本设计只保存并过滤“本次因新楼层而读取到的楼中楼”，不承诺发现后来追加到旧父帖的楼中楼。若未来要求严格监控所有旧父帖的新楼中楼，需要 NGA 提供独立变更水位，或增加高成本的历史页周期复扫策略，不能复用当前 `vrows` 增量语义假装实现。

### 2.2 UID 监控

UID 监控不执行历史回溯。首次运行只请求用户主题列表和回复列表的第 1 页，以页面中最大的 `(postdate, tid)` 和 `(postdate, pid)` 建立两个水位；不继续翻历史页、不请求帖子详情、不保存历史内容、不发送历史通知。该行为依赖列表按时间倒序返回，必须通过解析器 fixture 和集成测试固化；如果接口不满足倒序保证，则首次初始化失败，不能退化为遍历历史列表。

首次运行完成后，新增候选按 TID/PID 请求详情并再次校验作者 UID。满足条件的新主题或新回复需要写入现有 `threads`、`posts` 和 `post_events`，并进入持久化 `notification_outbox`，从而支持通知失败重试和审计查询。

UID watch 只保存监控启动后发现的目标用户新增内容，不因发现一个 TID 而回溯该主题的其他历史楼层。对于新增目标帖子详情中随响应返回的楼中楼，可作为该条新增内容的审计上下文保存，但不因为“随详情返回”就创建事件或通知；只有列表中被独立识别为目标 UID 新增候选的那条内容才创建 UID 事件。这样不会把上下文误判为本轮新增。

允许持久化的内容包括：

- `watch_targets` 的启停、schedule、租约和运行状态。
- 用户主题列表和用户回复列表的增量游标。
- 监控启动后新发现的目标用户主题/回复、必要的主题元数据和审计所需的楼中楼。
- `post_events`、`notification_outbox`、投递重试状态和去重来源。

不保存 UID 的历史主题、历史回复或用户资料。新 schema 直接删除当前仅被 UID collector 写入的 `nga_users` 表；如果仍需调用用户资料接口校验 UID，只解析响应而不持久化。UID 新内容和 TID 新内容统一使用帖子自然键去重，并统一进入通知投递链路。

### 2.3 通知过滤

通知规则不再独立于监控目标存在。每个监控目标包含一个通知配置：

- `channel_ids`：一个或多个通知渠道。
- TID 监控唯一的内容过滤条件是 `author_uids`：
  - 空列表表示主题内所有作者。
  - 非空表示只通知这些 UID 在该主题下产生的新内容。
- 不再配置 `post_kinds`；每条本轮新发现的内容统一按作者 UID 判断是否通知。
- UID 监控不需要配置作者 UID，目标 UID 本身就是作者过滤条件。

创建监控目标时 `channel_ids` 至少包含一个已存在渠道，列表内不允许重复。渠道可以处于停用状态，但停用渠道不会为新事件创建 outbox；以后重新启用也不补发停用期间的事件。通知过滤和渠道变更只影响变更提交后的新事件，已经进入 outbox 的任务继续按原配置重试；如需停止重试，应停用对应渠道。

例如：

```json
{
  "target_type": "thread",
  "tid": 47264819,
  "notification": {
    "channel_ids": ["bark-main", "feishu-team"],
    "author_uids": [150058, 123456]
  }
}
```

这表示：主题内容按 TID 规则采集和保存；每条新增内容只要其作者 UID 是 `150058` 或 `123456`，就进入指定通知渠道。作者过滤不影响 TID 内容落库。

UID 监控示例：

```json
{
  "target_type": "user",
  "uid": 150058,
  "notification": {
    "channel_ids": ["bark-main", "feishu-team"]
  }
}
```

这表示：首次只建立 UID `150058` 的当前水位；之后发现该 UID 的新主题或新回复时保存新增内容并推送到指定渠道，不回溯其历史内容。

## 3. 数据库设计

### 3.1 TID 专属配置

新增 `thread_watch_options`，避免把仅适用于 TID 的字段塞入 `watch_targets`：

```sql
CREATE TABLE thread_watch_options (
    watch_id TEXT PRIMARY KEY REFERENCES watch_targets(id) ON DELETE CASCADE,
    history_mode TEXT NOT NULL DEFAULT 'full'
        CHECK (history_mode IN ('full', 'incremental')),
    history_parallel_enabled INTEGER NOT NULL DEFAULT 0
        CHECK (history_parallel_enabled IN (0, 1)),
    history_parallelism INTEGER NOT NULL DEFAULT 2
        CHECK (history_parallelism BETWEEN 1 AND 16),
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

新建 TID watch 默认使用 `history_mode = 'full'`、`history_parallel_enabled = 0`、`history_parallelism = 2`。并发关闭时仍保存 `parallelism = 2`，但采集器按单请求顺序执行；再次开启并发时直接恢复该配置。

`baseline_completed` 继续表示“初始水位已建立”。`crawl_runs` 增加 `sync_mode`，取值限定为 `tid_full_baseline`、`tid_incremental_baseline`、`uid_baseline`、`incremental`，并增加 `matches_created`、`outbox_enqueued` 统计，用于区分运行语义、已有事件被新 watch 命中的情况以及页面排查。

`history_mode` 和历史并发配置只作用于 TID 初始化或 reset 后的下一次初始化。基线完成后修改 `history_mode` 不应静默改变当前运行状态：API 必须拒绝该修改并提示先 reset，或由一个原子 reset 请求同时更新配置和游标；管理页面采用后者。

### 3.2 通知配置

使用关系表支持多个渠道和多个 TID 作者 UID：

```sql
CREATE TABLE watch_notification_authors (
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    author_uid BIGINT NOT NULL,
    PRIMARY KEY (watch_id, author_uid)
);

CREATE TABLE watch_notification_channels (
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE RESTRICT,
    PRIMARY KEY (watch_id, channel_id)
);
```

约定：`watch_notification_authors` 为空表示 TID 监控通知所有新增内容；UID 监控不写入该表，目标 UID 自动作为作者条件。

`notification_channels` 保留，旧的 `notification_rules` 直接删除，不再作为新配置入口。`notification_outbox.channel_id` 同样改为 `ON DELETE RESTRICT`，避免删除渠道时连带删除重试和审计记录。被任一 watch 或 outbox 引用的渠道不可物理删除，渠道删除 API 返回 `409 channel_in_use`；用户需要先解除引用，日常停用使用 `enabled = false`。

### 3.3 统一持久化通知链路

UID 新内容直接复用当前 TID 的持久化链路：

```text
新增 UID 内容
  → threads/posts 自然键去重
  → post_event
  → notification_outbox
  → 渠道投递与重试
```

不再新增 `notification_dispatches` 或 transient payload 表。`notification_outbox` 继续从 `post_events -> posts -> threads` 读取通知内容，保证重试期间内容仍可用，并支持后续审计。

现有 `post_events.discovered_by_watch_id` 只能表示单一来源，不适用于 TID/UID 共同发现。开发环境直接删除该列和旧 `post_event_matches`，统一重建来源关系表：

```sql
CREATE TABLE post_event_watch_matches (
    post_event_id TEXT NOT NULL REFERENCES post_events(id) ON DELETE CASCADE,
    watch_id TEXT NOT NULL REFERENCES watch_targets(id) ON DELETE CASCADE,
    matched_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (post_event_id, watch_id)
);
```

该表记录“哪个 watch 发现了事件”，无论作者过滤是否命中都要写入；outbox 只在该 watch 的作者条件命中时，按 `watch_notification_channels` 创建。

`notification_outbox` 保持 `UNIQUE (post_event_id, channel_id)`。如果同一新增内容同时被 TID watch 和 UID watch 发现，帖子和 `post_event` 只保留一份，来源 watch 可以保留多条匹配记录；相同渠道不重复推送，不同渠道各生成一条 outbox。这个约束同时覆盖主题、普通回复和楼中楼，避免仅以可空的 PID 描述去重键。

`watch_targets` 增加 `deleted_at`，管理 API 的删除操作改为软删除：停用调度、释放租约并从默认列表隐藏，但保留 watch、通知配置和事件来源关系用于审计。唯一约束改为仅约束未删除记录的部分唯一索引 `(target_type, target_id) WHERE deleted_at IS NULL`；同一个 TID 或 UID 在同一时刻只能有一个有效监控，删除后可以重新创建新的监控实例。后台调度和普通查询必须统一过滤 `deleted_at IS NULL`。

### 3.4 开发环境数据库处理

当前项目仍处于开发阶段，不做旧数据迁移、不保留旧 API 兼容层，也不生成孤儿规则迁移报告。

- 删除 `notification_rules` 及其相关索引和查询代码。
- 删除 `nga_users` 表、相关索引和 UID collector 的用户资料写入路径；保留可选的只读 UID 有效性校验。
- 删除当前 UID collector 的历史回溯路径；保留监控启动后的新增主题/回复写入 `threads`、`posts`、`post_events` 的路径。
- 直接修改 SQLite/PostgreSQL 基础 schema 并重建开发数据库，不新增数据迁移脚本、不尝试保留当前开发数据。
- 新 schema 只保留新的监控目标通知配置，并让 TID/UID 共用 `post_event` 和 `notification_outbox`。

## 4. 采集器改造

### 4.1 TID collector

首次运行：

1. 请求第 1 页，取得 `vrows`、`totalPage`、`per_page` 和主题元数据。
2. `history_mode = incremental` 时，如果存在多页，只额外请求最后一页并计算当前最大 `floor_number`；随后仅保存水位，不写主题/帖子、不生成通知事件。最后一页探测失败时初始化失败，不能用 `vrows` 猜测楼层。
3. `history_mode = full` 时，计算所有需要请求的页面。
4. 根据 `history_parallel_enabled` 和 `history_parallelism` 使用有界并发请求页面。
5. 将结果按页码排序后，在单个数据库事务内写入主题、帖子、楼中楼和游标。
6. 初始 full backfill 不生成通知；backfill 完成后才进入实时增量阶段。

TID full 完成后将 `threads.coverage` 升级为 `full`。incremental 初始化只建立 watch 水位，不创建空的主题内容记录；以后保存增量帖子时可创建或更新 `coverage = partial` 的主题元数据，且 partial upsert 不得把已有 `full` 降级。

后续运行：

- 先请求第 1 页获取最新元数据。
- 当 `vrows`、总页数和可用的最新楼层标识都没有变化时，只更新运行状态和下一次调度时间。
- 当 `vrows` 增长时，根据 `last_floor` 和 `per_page` 请求覆盖新增楼层的页面。
- 增量阶段默认不使用历史并发配置；如果一次新增跨越多页，可使用固定的低并发上限，但不能超过账号级请求预算。

一致性要求：

- 任一页面失败时，本次事务不提交，游标不推进。
- 并发请求只发生在 HTTP 获取阶段，数据库写入仍按页码确定性排序。
- 并发任务必须有界，单页失败后取消尚未开始的页面请求；已经完成的响应只保留在本次运行内，不写入数据库。
- 运行租约必须支持续租。历史回溯可能超过当前固定 5 分钟 lease，应增加 heartbeat 或根据页数延长 lease。
- full backfill 期间暂停或删除 watch 时，未提交的内存结果直接丢弃；已提交的数据保持一致。

### 4.2 UID collector

1. 读取用户主题和用户回复游标。
2. 首次运行分别只请求 topic/reply 列表第 1 页并记录水位，不翻页、不请求详情、不保存历史内容、不发送历史通知。两个列表必须在同一次成功运行中完成初始化，任一失败都不设置 `baseline_completed`。
3. 增量运行只处理水位之后的候选。
4. 对候选按 TID/PID 请求详情并再次校验作者 UID。
5. 只保存监控启动后发现的目标用户新主题/新回复，以及审计所需的最小主题元数据和随目标详情返回的楼中楼；不保存该主题的其他历史楼层。上下文楼中楼只写 `posts`，不在本次候选处理中创建 `post_event` 或 outbox。
6. 复用 `insert_post`、`upsert_thread`、`insert_event` 和现有通知 outbox；不调用用户资料写入路径。
7. 帖子、post event、通知 outbox 和游标在同一事务中提交。通知任务进入 outbox 后才推进游标；投递失败由现有 outbox 重试。
8. UID 新内容的通知 URL 可以直接使用 NGA 的 TID/PID 地址，内容保留在 `posts` 中供通知重试和审计查询。

UID collector 不因发现候选 TID 而启动该主题的历史回溯，也不保存该主题中与目标用户无关的楼层。

增量列表按 `(postdate, tid)` 和 `(postdate, pid)` 与游标比较，并持续翻页直到命中旧水位；列表项和详情必须再次校验 UID、TID/PID。一个批次中任一候选详情失败时，整批内容和游标都不提交，避免跳过失败项。重复失败会使 watch 保持 error 并等待人工检查，不能静默越过该候选。

UID 写入的 `threads.coverage` 固定为 `partial`，只维护通知渲染和审计所需的最小元数据；如果该主题此前已由 TID full 保存，更新时必须保留 `coverage = full` 和完整统计。

### 4.3 TID/UID 共同发现时的事件处理

- `posts` 按现有自然键全局去重：主题按 TID，普通回复和楼中楼按 `(tid, pid)`；同一内容被两个 collector 发现时只保存一份。
- 如果实时 collector 发现的帖子已经存在，但没有 `post_event`，应补建一个实时事件；不能因为帖子已存在就跳过通知。
- 如果 `post_event` 已存在，只新增 `post_event_watch_matches` 里的来源 watch，并按该 watch 的渠道配置补充 outbox；不能重复创建事件或重复投递。
- 初始 full/incremental 基线写入的内容不创建事件，UID 首次水位同样不创建事件。

实时事件处理固定为同一事务内的幂等顺序：upsert post → insert/select post event → insert watch match → 计算该 watch 的作者过滤 → 对每个启用渠道 insert outbox on conflict。`events_created` 只统计新事件，`matches_created` 和 `outbox_enqueued` 分别统计本次新增来源关系和通知任务。

## 5. API 改造

### 5.1 创建 TID watch

```http
POST /api/v1/watches/threads
```

```json
{
  "tid": 47264819,
  "interval_seconds": 300,
  "schedule": [
    {
      "days": ["weekdays"],
      "start_time": "09:00",
      "end_time": "18:00",
      "interval": 120
    }
  ],
  "history": {
    "mode": "full",
    "parallel_enabled": true,
    "parallelism": 2
  },
  "notification": {
    "channel_ids": ["channel-id"],
    "author_uids": [150058, 123456]
  }
}
```

### 5.2 创建 UID watch

```http
POST /api/v1/watches/users
```

```json
{
  "uid": 150058,
  "interval_seconds": 300,
  "schedule": null,
  "notification": {
    "channel_ids": ["channel-id"]
  }
}
```

UID 请求中拒绝 `history` 和 `author_uids`，目标 UID 自动作为作者匹配条件。

创建和更新 watch 必须在一个数据库事务中同时写入目标、TID 选项、作者过滤和渠道关系。任一渠道不存在、ID 重复、作者 UID 非正数或字段不适用于当前目标类型时，整个请求失败，不留下半配置 watch。

### 5.3 查询与更新

保留并扩展：

```http
GET   /api/v1/watches
GET   /api/v1/watches/{id}
PATCH /api/v1/watches/{id}
DELETE /api/v1/watches/{id}
POST  /api/v1/watches/{id}/run
POST  /api/v1/watches/{id}/reset
GET   /api/v1/watches/{id}/runs
```

`WatchResponse` 必须返回：

- `target_type`、`target_id`、`enabled`、`status`。
- `interval_seconds`、`schedule`、`next_run_at`。
- TID 的 `history.mode`、`parallel_enabled`、`parallelism`、初始化状态。
- 嵌套的 `notification.channel_ids`，以及 TID 的 `author_uids`。
- 最近运行结果、页数、保存数量、通知数量和错误类型。

`PATCH` 要支持显式清空 schedule，例如使用三态字段区分：

- 字段缺失：不修改。
- `schedule: null`：清空 schedule，恢复固定间隔。
- `schedule: [...]`：替换 schedule。

当前 `Option<Schedule>` 无法区分字段缺失和显式 null，需要改用三态反序列化结构。

嵌套字段同样使用明确的替换语义：`notification` 缺失表示不修改；传入 `notification` 时，`channel_ids` 和 TID 的 `author_uids` 都是完整替换，空 `author_uids` 表示通知所有作者。`channel_ids` 不允许为空。API 不接受只更新嵌套对象一部分的含糊请求。

schedule 的执行语义与现有调度器保持一致：使用服务配置时区；命中时间段时使用该规则的 `interval`，未命中任何规则时使用顶层 `interval_seconds`；跨午夜规则按起始日归属；规则重叠时按数组顺序取第一条。页面必须展示 fallback 含义、时区和规则优先级，并支持拖动排序，不能只让用户手写 JSON。

### 5.4 重置与重新初始化

TID reset：

```http
POST /api/v1/watches/{id}/reset
```

```json
{
  "history_mode": "incremental"
}
```

- `history_mode = full`：原子更新 TID 初始化配置并清空 watch 游标，下一轮重新执行全量静默回溯。
- `history_mode = incremental`：原子更新配置并清空 watch 游标，下一轮只建立新的当前水位。
- reset 不删除已有 TID 内容；full 重跑通过自然键去重，incremental 只改变后续水位。

UID reset 不接受 `history_mode`，请求体为空对象：

```http
POST /api/v1/watches/{id}/reset
```

```json
{}
```

UID reset 只清空 topic/reply 游标并将目标恢复为待初始化，下一轮仍只请求两个列表的第 1 页，不产生历史通知。

两类 reset 都必须先取得该 watch 的排他租约：正在运行时返回 `409 watch_running`；成功后将 watch 置为 `pending` 并立即调度下一轮。reset 和已有内容删除是两个独立操作，本设计不提供通过 reset 删除内容的参数。

`DELETE /api/v1/watches/{id}` 执行软删除，不删除已保存内容、事件、outbox 或来源关系。已进入 outbox 的任务继续重试；若需要立即停止该 watch 相关渠道的所有投递，应先停用渠道。物理清理只允许通过独立的开发维护命令执行，不属于管理页面能力。

## 6. 管理页面改造

### 6.1 监控目标页面

监控目标表单根据类型动态展示：

TID：

- TID。
- 初始同步：全量历史 / 仅新增。
- 并发抓取开关。
- 并发抓取数，默认 2、范围 1–16；仅在“全量历史 + 并发开启”时启用，并提示这是最大 in-flight 请求数而非 OS 线程数。
- 固定间隔和可视化 schedule 编辑器。
- 通知渠道多选。
- 作者 UID 白名单，可添加、删除 UID；为空表示所有作者。

UID：

- UID。
- 固定间隔和 schedule 编辑器。
- 通知渠道多选。
- 明确提示“首次不拉取历史；监控启动后的新增内容会保存，用于通知重试和审计”。

不再在监控页面之外配置 TID/UID 通知规则。

### 6.2 监控列表

列表至少显示：

- 类型和目标 ID。
- 同步模式及初始化状态。
- schedule 摘要或“固定 N 秒”。
- 通知渠道数和过滤摘要。
- 状态、下次运行、最近运行结果。
- 编辑、暂停/启用、立即运行、重置、删除。

已完成基线的 TID 在编辑历史模式时，页面必须明确提示该操作会执行 reset，并将“配置更新 + 游标重置”作为一个确认动作提交；普通的渠道、作者和 schedule 编辑不触发 reset。

### 6.3 通知中心

通知中心只保留：

- Bark 渠道新增、编辑、启停、测试、删除。
- 飞书渠道新增、编辑、启停、测试、删除。

删除通知规则表单、通知规则列表和相关文案。

### 6.4 内容与事件页面

- 展示 TID watch 保存的主题和事件，也展示 UID watch 启动后捕获的新内容。
- 内容列表支持按来源 watch、TID、UID 和时间筛选，明确标记“UID 监控新增内容”。
- UID watch 不展示历史回溯内容，因为 UID 监控不会拉取历史。

## 7. 实施顺序

当前项目仍处于开发阶段，允许破坏性调整，不设计旧数据迁移和兼容发布流程：

1. 直接重建开发数据库，创建 TID 专属配置、监控通知配置，并复用 `post_event` 和 `notification_outbox`。
2. 删除旧 `notification_rules` API、repository 查询和管理页面。
3. 实现 TID full/incremental 两种初始化模式、有界历史并发和租约续期。
4. 改造 UID collector 为“无历史、增量内容落库”，并通过帖子自然键、事件唯一键和 `(post_event_id, channel_id)` outbox 唯一键完成全局去重。
5. 管理页面接入新的监控目标 API，通知中心只保留渠道管理。

## 8. 测试与验收

### TID

- full 模式首次保存所有可访问页，零历史通知。
- full + 并发 1 与顺序抓取结果一致。
- full + 并发 N 不超过配置并发数，且不绕过账号限速。
- 某一历史页失败时不提交部分结果，游标不推进，重试后可恢复。
- incremental 首次只建立水位，不保存旧回复；下一轮只保存新增回复。
- incremental 多页主题首次只请求第 1 页和最后一页，建立的楼层水位不会把最后一页旧回复误判为新增。
- 新增跨页时只请求覆盖新增楼层的页面。
- 主题每条新增内容按作者 UID 白名单正确过滤，但不影响主题内容落库。
- 新普通回复携带的楼中楼按各自作者 UID 过滤；追加到旧父帖但未触发页面重读的楼中楼不在强保证范围内。
- 同一回复同时被 TID watch 和 UID watch 命中时，同一渠道只发送一次，不同渠道各发送一次。

### UID

- 首次 UID 运行只建立游标，不保存历史内容、不产生历史通知。
- 首次 UID 运行每个列表只请求第 1 页；任一列表初始化失败时两个水位都不提交。
- 后续新主题、新回复会写入 `threads`、`posts`、`post_events`，并可以通知。
- UID watch 不写入历史数据；只有监控启动后的目标用户新增内容进入内容表。
- 随目标详情返回的上下文楼中楼可以写入 `posts`，但不会误建事件或通知；只有被列表独立识别的新候选才建事件。
- UID 详情作者校验失败时不通知、不推进错误候选。
- 同一批次中间候选失败时，已解析内容、事件、outbox 和游标全部回滚。
- 通知失败可通过持久化 outbox 重试，内容可供后续审计。
- 同一回复被 UID watch 和 TID watch 同时发现时，同一渠道只发送一次。

### API 和页面

- TID/UID 创建、编辑、启停、删除、立即运行和重置行为一致。
- schedule 新增、替换、清空均可通过 API 和页面完成。
- schedule 的时区、fallback、跨午夜和重叠优先级在 API 与页面展示一致。
- TID 页面能保存并回显历史模式、并发开关和并发抓取数。
- 已初始化 TID 修改历史模式时通过一次确认原子完成配置更新和 reset，普通通知/schedule 编辑不重置游标。
- UID 页面显示“首次不回溯，监控启动后的新增内容会落库”提示，并隐藏 TID 专属历史配置。
- 通知中心只能管理渠道，不能再创建独立通知规则。
- 被 watch 或 outbox 引用的渠道删除返回 409，停用渠道不会删除已有审计和重试记录。
- 删除 watch 后默认列表不再显示，但事件审计仍能回显历史目标类型、目标 ID 和配置；同一 TID/UID 可以重新创建新 watch。

## 9. 风险

主要风险：

- NGA 请求限速可能使并发抓取数不能线性提升；该值必须定义为 in-flight 上限，而不是性能承诺。
- full backfill 可能超过当前 lease，需要先实现续租，否则会出现重复采集。
- UID 新增内容会进入持久化帖子、事件和 outbox；需要控制保存范围，确保不会因 UID watch 回溯或保存无关历史楼层。
- `vrows` 不表示旧父帖楼中楼变化；若产品要求严格监控所有楼中楼，需要新增独立方案和请求预算，不能仅修改作者过滤页面。
- UID 首次水位依赖用户列表按时间倒序；若 NGA 接口排序语义变化，必须让初始化显式失败并报警，不能通过全量翻页规避。
- 单个永久失败的 UID 候选会阻塞该 watch 的水位推进；这是“不丢数据”的有意取舍，需要在运行详情中暴露候选 TID/PID 和错误原因，供人工处理。
- 开发数据库会被重建，不能把该方案直接用于已有生产数据。
