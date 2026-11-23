# Spec21：帖子导出按 UID 过滤

## Goal

当前主题（TID）导出只能导出整帖全部作者或按时间段筛选，在十几万楼的长帖中无法只导出指定几位用户的发言记录。

完成后，用户导出主题帖子时可提供作者 UID 列表，仅导出匹配该列表 UID 的已保存内容，适用于 Markdown 与 ZIP 导出，并兼容同步与异步导出。

## Behavior

- 主题（TID）的 Markdown 与 ZIP 导出新增可选的作者 UID 列表筛选条件（管理页与 API 均支持）：
  - 省略或留空时保持原有行为，导出该主题下全部作者的已保存内容。
  - 提供一个或多个 UID 时，仅导出该主题下 `author_uid` 命中列表的已保存帖子（包含匹配作者的主楼、普通回复与楼中楼评论），排除其他作者的内容。
- 支持输入多个正整数 UID：文本参数与管理页输入框支持逗号（含中文逗号）、空白或换行分隔，API 同时支持逗号分隔列表或重复查询参数；服务端自动去重，且 UID 输入顺序不影响导出内容的原有排序。
- 列表中包含非数字、0、负数或其他非法值时返回 400 参数错误，不生成导出文件或异步任务。
- UID 过滤可与按时段导出（`start_at`、`end_at`）组合生效，取交集；所有有效筛选条件共同决定匹配帖子总数，并据此判定走同步导出或异步导出（见 [Spec20](20-async-content-export.md)）。
- 筛选后无匹配帖子时，沿用现有“没有可导出的已保存内容”反馈，不生成空文件或异步任务。
- Markdown 文内引用链接遵守当次导出范围：被引用的帖子未命中 UID 过滤（不在本次导出内容中）时，自动回退为 NGA 原帖链接，不产生失效锚点。
- ZIP 导出仅打包命中 UID 过滤条件的帖子所引用的已下载本地资源，不打包被排除帖子的资源；`metadata.json` 记录实际导出的帖子数量及本次过滤的 UID 列表。
- 管理页在 TID 内容页的“导出内容”悬浮窗及无 JavaScript 基础表单中新增可选的“作者 UID 过滤”输入项，提示可填写多个 UID（留空表示全部作者）；UID 内容页（`/admin/users/:uid`）不显示该输入项。
- 对 UID 导出接口（`/admin/exports/users/:uid`、`/api/v1/exports/users/:uid`）传入 UID 过滤参数时返回 400 参数错误，明确提示仅主题导出支持 UID 过滤，不静默忽略。

## Constraints

- 复用现有 `/admin/exports/threads/:id`、`/api/v1/exports/threads/:id` 与异步导出流程，仅查询 SQLite 中已保存的帖子，不额外访问 NGA。
- 保持既有排序规则、快照边界、Markdown 渲染（[Spec13](13-markdown-rendering.md)）、时段筛选（[Spec16](16-time-ranged-content-export.md)）及资源安全约束。
- 管理页沿用 [Spec15](15-admin-ui.md) 的原生模板、CSS 与渐进增强 JavaScript，不引入前端依赖。
- 仅修改 Go 服务端与管理后台，不改动独立 Chromium 扩展。

## Out of Scope

- 主题在线浏览页（`/admin/threads/:tid`）的 UID 筛选或分页变更。
- 按作者昵称过滤、UID 黑名单（排除模式）或跨多个主题的联合 UID 过滤导出。
- 自动附带非目标 UID 的被引用原楼层或父楼层上下文。
- 持久化保存常用 UID 分组模板。

## Acceptance Criteria

- 省略或留空 UID 过滤参数时，TID 的 Markdown 与 ZIP 导出结果与既有行为完全一致。
- 指定单个或多个 UID（含重复值及逗号/空白分隔）导出 TID 时，Markdown 与 ZIP 仅包含 `author_uid` 命中列表的已保存主楼、回复与楼中楼，且保持原有排序。
- UID 过滤与时段筛选组合使用时，仅导出同时满足时间闭区间和 UID 列表的帖子；筛选为空时返回现有无内容提示，不产生文件或任务。
- 命中帖子引用了被过滤掉作者的帖子时，Markdown 渲染为 NGA 原帖链接而非失效文内锚点；ZIP 仅打包命中帖子引用的已下载资源。
- 传入非法 UID（非数字、0、负数）或对 UID 导出接口传入 UID 过滤参数时，返回 400 且不创建文件或任务。
- TID 内容页的导出悬浮窗与无 JavaScript 基础表单均支持填写 UID 过滤列表，UID 内容页不显示该选项；与异步导出组合时，筛选后条数正确参与阈值判断并在任务摘要中展示 UID 过滤条件。
- `node --check web/static/admin.js`、`CGO_ENABLED=0 go build ./...`、`CGO_ENABLED=0 go vet ./...`、`CGO_ENABLED=0 go test -count=1 -timeout=30s ./...` 和 `git diff --check` 通过。
