# 流式导出、资源维护与内容管理设计

## 1. 范围

本设计面向 homeserver 单机 `nga-reminder all` 部署，完成以下能力：

1. Markdown 导出按固定大小从数据库分页读取，避免一次加载全部帖子。
2. ZIP 导出先在资源目录的 `.tmp` 子目录生成临时文件，再以文件流返回；响应结束或中断后删除临时文件。
3. 提供资源一致性扫描和显式清理操作，检查数据库记录、本地内容寻址文件和临时文件。
4. 管理台增加 UID 监控结果、用户 Markdown/ZIP 导出和安全的 NGA 富文本详情。

不增加异步 `export_jobs`，不增加独立服务或队列，也不为多实例协调设计分布式锁。

## 2. 导出设计

### 2.1 Markdown

- API 路径保持不变：
  - `GET /api/v1/exports/threads/{tid}?format=markdown`
  - `GET /api/v1/exports/users/{uid}?format=markdown`
- 帖子使用稳定复合游标分页：`tid`、楼层排序值、页码、PID 排序值、内部 ID。
- 每批最多读取固定数量的帖子，渲染后立即作为 HTTP body chunk 返回。
- 用户导出跨批次保留当前 TID，确保主题分组标题只输出一次。

### 2.2 ZIP

- ZIP 格式需要文件尾部 central directory，不能直接复用普通 HTTP chunk 流。
- 服务在 `assets.storage_path/.tmp` 中分页生成 Markdown 临时文件，再逐文件复制到 ZIP，整个过程不把完整主题、资源或 ZIP 放入内存。
- ZIP 完成后通过文件流响应；流对象持有删除守卫，正常完成、客户端断开或响应被丢弃时都删除 ZIP 临时文件。
- ZIP 内仍包含 Markdown、`metadata.json` 和状态为 `ready` 且路径检查通过的资源。
- `.tmp` 中异常退出遗留的文件由资源维护功能按保留时间清理。

## 3. 资源一致性与清理

新增受保护 API：

```text
GET  /api/v1/assets/maintenance
POST /api/v1/assets/maintenance/cleanup
```

扫描报告包括：

- 数据库资源数和 ready 资源数；
- ready 记录缺失或路径非法的本地文件；
- 没有 `post_assets` 引用的资源元数据；
- 本地存在、但没有任何 ready 记录引用的内容文件；
- `.tmp` 中超过保留时间的导出或下载临时文件。

清理使用以下安全边界：

- 默认保留时间为 24 小时，避免删除正在完成数据库提交的下载文件。
- 只处理 `assets.storage_path` 内扫描得到的普通文件，不跟随符号链接。
- 缺失 ready 文件在启用下载时重置为 `pending`，关闭下载时重置为 `remote_only`。
- 删除无帖子引用且不处于 `downloading` 状态的资源元数据。
- 删除超过保留时间的无引用文件和临时文件；共享内容路径只要仍被一个 ready 记录引用就保留。
- 管理台必须先扫描，再由管理员确认执行清理，不在 worker 周期中自动删除文件。

## 4. 用户结果与富文本

新增查询 API：

```text
GET /api/v1/users
GET /api/v1/users/{uid}/posts
```

- 用户列表以未删除的 UID watch 为入口，汇总已持久化帖子数、主题数和最近发帖时间。
- 用户详情只返回该 UID 已持久化的帖子，不扩展抓取范围。
- 帖子响应同时返回纯文本预览和服务端生成的安全 HTML。
- HTML 渲染器复用 NGA markup AST，只输出固定标签，并对文本和属性转义；链接和图片只接受解析器认可的 HTTP(S) URL。
- 管理台不解析原始 NGA HTML，也不直接把 `content_raw` 写入 DOM。

管理台内容页增加用户结果表、用户 Markdown/ZIP 导出入口，并让主题和用户详情共用富文本对话框。

## 5. 验收

- 大于单批大小的主题和用户导出内容完整、顺序稳定。
- Markdown 响应和 ZIP 构建不持有完整帖子集合或完整资源字节。
- ZIP 响应完成后临时文件被删除，遗留临时文件可由维护 API 清理。
- 扫描不会修改数据库或文件；清理不会越过资源目录或删除仍被引用的共享文件。
- 缺失资源可重新进入下载队列，孤儿元数据与过期孤儿文件可清理。
- 管理台可浏览 UID 结果、导出用户内容，并安全显示格式、引用、链接、代码和图片。

