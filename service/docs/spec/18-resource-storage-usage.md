# 资源磁盘占用统计

## Goal

资源维护页目前只显示缺失、无引用和临时文件，无法直接判断当前 SQLite 与 assets 的磁盘占用。

完成后，管理员打开资源维护页即可查看两项当前统计，用于日常维护和备份容量判断。

## Behavior

- 每次打开资源维护页时计算一次当前 SQLite 和 assets 占用，页面加载后不自动刷新。
- SQLite 占用为主数据库文件、WAL 文件和 SHM 文件当前逻辑大小之和；当前不存在的 WAL 或 SHM 按 0 计算。
- Assets 占用为资源目录扫描到的所有普通文件逻辑大小之和，文件总数为同一范围内的普通文件数量；两项统计均包括已引用、无引用和临时文件。
- 资源维护 GET API 返回 SQLite 字节数、assets 字节数和 assets 文件总数；管理页展示同一次请求的结果。
- 统计失败时，请求按现有错误处理返回可定位的失败结果，不展示不完整的占用量。

## Constraints

- 继续使用现有资源维护页和 GET API，不新增定时任务。
- Assets 统计复用现有只读扫描，不跟随符号链接，不将非普通文件计入结果。
- 统计只读，不执行 SQLite checkpoint、`VACUUM` 或资源清理。

## Out of Scope

- 文件系统实际分配块数、所在磁盘的总容量与剩余空间。
- 统计缓存、持久化计数器、后台定时更新和页面自动轮询。
- 日志目录、临时目录或其他服务文件的占用统计。

## Acceptance Criteria

- 资源维护页能显示 SQLite 当前字节数、assets 当前字节数及文件总数。
- `GET /api/v1/resources` 返回 SQLite 字节数、assets 字节数和 assets 文件总数，且与当次文件状态一致。
- SQLite 统计正确累加已存在的主库、WAL 和 SHM，并允许 WAL 或 SHM 不存在。
- Assets 容量和文件总数均包含普通资源和临时文件，不计入符号链接。
- 扫描和统计不修改 SQLite 或 assets 中的文件。
- `CGO_ENABLED=0 go build ./...`、`CGO_ENABLED=0 go vet ./...`、`CGO_ENABLED=0 go test -count=1 -timeout=30s ./...` 和 `git diff --check` 通过。
