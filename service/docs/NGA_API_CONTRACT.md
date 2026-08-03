# NGA API 契约

本文冻结 Rust 服务使用的 NGA 请求和响应契约。契约基于真实认证账号执行的只读探测；仓库中的 fixture 只在结构上具有代表性且已完全脱敏，不包含真实 Cookie、用户名、帖子正文或探测得到的业务 ID。

## 通用请求配置

所有 NGA 数据请求都保留以下请求头：

```text
Content-Type: application/x-www-form-urlencoded
User-Agent: <configured user agent>
Accept: application/json, text/javascript, */*; q=0.01
Accept-Language: en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7
Cookie: <encrypted full browser Cookie for cross-user searches>
Origin: https://bbs.nga.cn
Referer: https://bbs.nga.cn/
```

主题抓取和登录账号自身的搜索只需要 `ngaPassportUid` 与 `ngaPassportCid`。2026-08-03 的用户侧与服务主机真实探测均确认：跨用户搜索使用显式 `page=1`、配置的 User-Agent，以及 `ngaPassportUid`、可选 `ngaPassportUrlencodedUname`、`ngaPassportCid` 三个 Cookie 字段即可稳定返回 JSON。附加的 `Accept`、`Accept-Language`、`C3VK`、`Sec-Fetch-*` 和 `sec-ch-ua*` 均不是必要条件。任何 Cookie 都绝不能出现在日志、fixture、API 响应或导出文件中。

凭据校验必须使用用户回复接口，而不是用户资料页面。即使 `ngaPassportCid` 无效，真实 UID 的资料页仍可能公开可读。在回复接口中，`code=2048` 且 `msg` 包含“必须登录”表示凭据无效；`code=2048` 且消息包含“服务器忙”则按后文重试策略处理。

每个响应按以下顺序检查：

1. HTTP 状态和响应体。用户主题/回复第一页的空体 HTTP 503 不能证明列表为空，按 2 秒间隔最多尝试 3 次；耗尽后归类为 `nga_user_search_unavailable`，并放弃本次事务中的全部游标更新。
2. `__output=12` 与 `app_api.php` 响应的 JSON 解码。
3. NGA 顶层业务 `code`。

HTTP 成功不代表 NGA 业务成功。

## 接口

| 用途 | 请求 | Fixture |
| --- | --- | --- |
| 主题页面 | `POST /app_api.php?__lib=post&__act=list`，表单参数 `tid`、`page` | `thread_page_success.json` |
| 按 PID 获取帖子 | 同一接口，表单参数 `tid`、`pid` | `post_by_pid_success.json` |
| 用户主题 | `GET /thread.php?authorid={uid}&__output=12&page={page}` | `user_topics_page_1.json`、`user_topics_page_2.json` |
| 用户回复 | `GET /thread.php?searchpost=1&authorid={uid}&__output=12&page={page}` | `user_replies_success.json` |
| 用户资料 | `GET /nuke.php?func=ucp&uid={uid}` | `user_profile_gbk.html` |

主题接口可能在目标帖子正在审核时返回 HTTP 200、`code=51` 以及“帖子正等待审核”等消息。这是暂时不可用，不是主题不存在或认证失败。当前抓取应记录为 `skipped`，错误类型为 `nga_pending_review`；不提交帖子、事件或游标，下一次定时运行继续重试。

## 主题页面

成功的主题响应是 `code=0` 的 UTF-8 JSON。重要顶层字段：

```text
currentPage, totalPage, perPage, vrows
fid, forum_name
tsubject, tauthor, tauthorid
attachPrefix, hot_post
result[]
```

重要帖子字段：

```text
tid, pid, fid, lou
postdate, postdatetimestamp
subject, content, type
author.uid, author.username
attches, comments
vote, vote_good, vote_bad
```

契约约定：

- `result` 按 `lou` 升序排列。
- 主题从 `lou=0` 开始，可能使用 `pid=0`。
- 当前 `perPage` 为 20，但最后一页可能更短。
- 新增回复后 `vrows` 包含主题帖并增长。
- 使用 `tid` 加非零 `pid` 请求时，只返回目标帖子。
- TID 监控保存所有可访问条目。
- 用户监控请求第 1 页完成主题补全时，只保存作者 UID 匹配的 `lou=0` 主楼。

### 楼中楼评论

楼中楼评论位于父帖的 `comments` 数组中。已观察到的契约如下：

- 评论的 `tid` 等于所属帖子的 TID。
- 评论的 `pid` 非零，在已验证样本中与普通结果 PID 不重复。
- 评论的 `lou` 为零，不是主题楼层号。
- 数据库中的父子关系来自 JSON 包含关系。
- `comment_to_id` 具有多态性，可能等于父帖 PID，也可能等于被回复用户 UID。应将其作为原始元数据保留，不得用作 `parent_post_id`。

回复和评论使用 `(tid, pid)` 作为自然键；主题使用 `(tid, 主题类型)` 作为自然键。

### 热门帖子

`hot_post` 可能是包含完整帖子对象的非空数组。已验证的热门帖子 PID 也会出现在普通 `result` 数组中。应将热门帖子解析为排名/元数据引用，再按 `(tid, pid)` 解析到规范帖子；绝不能插入第二份帖子或事件。

### 附件

`attches` 是数组，已观察到的字段如下：

```text
attachurl, path, name, ext, type, size, subid
thumb, dscp, hash, url_utf8_org_name
```

`attachurl` 和 `path` 是相对路径。远程资源 URL 由 `attachPrefix + attachurl` 构造。对已构造的样例 URL 执行 HEAD 探测返回 HTTP 200 和 `image/jpeg`；实际下载仍必须校验 scheme、主机策略、响应大小和内容类型后才能写入本地。

资源二进制不会存入 PostgreSQL `BYTEA` 或 SQLite `BLOB`。数据库保存源 URL、SHA-256、MIME 类型、字节数、下载状态以及相对于资源根目录的安全路径。就绪文件使用类似 `<sha256-prefix>/<sha256>.<safe-extension>` 的内容寻址布局，使来自不同 URL 的相同内容共享一个文件。下载通过临时文件流式执行，校验后原子重命名，再 upsert 元数据。数据库和资源目录必须作为一个逻辑数据集一起备份和恢复。

## 用户主题

成功响应使用以下字段：

```text
result.__T[]
result.__T__ROWS
result.__T__ROWS_PAGE
result.__ROWS
result.__F
result.__CU
result.__GLOBAL
```

已观察到 `__T__ROWS_PAGE` 为 35。最大页码计算方式为：

```text
ceil(parse_int(__ROWS) / __T__ROWS_PAGE)
```

不要因为 `__T` 数组较短就停止：无权访问的记录可能使返回数组短于服务端计数，而后续页面仍然存在。

`result.__T` 可能包含 `denied=true` 且 `error` 非空的无权访问占位记录，这类记录只用于诊断。只有在记录可访问且 `authorid` 等于被监控 UID 时，才能接受为主题候选。

用户主题监控只保存被监控用户的主楼，不创建主题监控，也不持久化该 TID 的其他回复。

## 用户回复

响应将主题摘要存放在 `result.__T` 中。被监控用户匹配到的回复嵌套在 `__P` 中，并包含：

```text
tid, pid, authorid, postdate, subject, content, type
```

已观察到 `result.__R__ROWS_PAGE` 的回复页容量为 20，但真实响应的 `result.__ROWS` 可以是 `null`。有总数时按总数计算末页；没有总数时，满页继续请求下一页，短页或成功页之后的空体 503 结束分页。相邻页面按 `postdate` 降序排列，PID 不重叠。

`result.__T` 还可能混入 `__P.postdate=""` 的占位记录。缺少有效 `tid`、`pid` 或 `postdate` 的 `__P` 必须跳过，不能令整页失败或成为水位。

采集器只接受 `__P.authorid == watched_uid` 的记录，然后使用 TID/PID 请求帖子接口，并在写入前校验 `result[0].author.uid == watched_uid`。它不会将发现的 TID 扩展为完整主题抓取。

## NGA 忙碌响应

用户主题和用户回复查询可能返回 HTTP 200 以及：

```json
{"code":2048,"msg":"服务器忙,请稍后重试"}
```

相同请求每秒重试一次，最多尝试 10 次。如果所有尝试都返回 2048，则将抓取标记为 `skipped_busy`，不写入帖子或事件，也不推进游标。

HTTP 错误、空响应、JSON 解析失败和其他 NGA 业务码使用各自的错误分类，不得视为 2048。

## 已确认的错误

NGA 会在 HTTP 200、空 `result` 数组中返回以下错误：

| 条件 | 业务码 | 消息 | Fixture |
| --- | ---: | --- | --- |
| 未知 TID | 14 | `找不到主题` | `invalid_tid_14.json` |
| Passport Cookie 缺失或无效 | 46 | `访客不能直接访问` | `missing_auth_46.json` |

Code 46 会暂停分配给该账号的任务，直到更新凭据并通过连通性测试。Code 14 对当前请求是永久错误，不会按普通临时错误路径重试。

## 用户资料

用户资料接口返回 `text/html; charset=GBK`，即使提供 `__output=12` 也是如此。应将 GBK 解码为 UTF-8，定位 `__UCPUSER = {...};` 赋值，只解析其中的 JSON 对象，绝不能执行页面 JavaScript。

可用字段包括 `uid`、`username`、`groupid`、`avatar`、`regdate`、`lastpost`、`posts` 和 `sign`。

对一个不存在的 UID 测试时，资料接口返回 HTTP 200 和不含 `__UCPUSER` 对象的 GBK 页面。UID 采集器先通过资料页确认目标存在，再分别请求主题和回复列表。空体 HTTP 503 既可能出现在不存在 UID 的列表请求，也可能出现在浏览器中实际有内容的有效 UID 跨用户搜索，因此不能将它解释为空列表。采集器必须记录 `nga_user_search_unavailable`，保持基线未完成，并且不提交主题或回复游标。带非空响应体的 503 仍按通用 HTTP 错误处理。

## 持久化与通知去重

- 主题帖按 TID 和主题类型唯一，因为其 PID 可能为零。
- 回复/评论按 `(tid, pid)` 使用自然键；父子关系单独保存。
- 实时写入使用 `INSERT ... ON CONFLICT DO NOTHING RETURNING id`。
- 实时发现会创建或复用一个 `post_event`，即使另一个监控已经保存过该帖子；基线导入不发送通知。
- `post_events` 按 `(post_id, event_type)` 唯一。
- 通知 outbox 按 `(post_event_id, channel_id)` 唯一。
- TID 和 UID 来源都可以记录在 `post_event_watch_matches` 中，但共用一次渠道投递。
- 发现顺序无论是 TID 后 UID、UID 后 TID 还是并发，都不能改变单次投递结果。

## 延后探测

无权访问的主题/帖子响应暂时延后，不阻塞 M0。未知 NGA 业务码仍然作为类型化错误处理，绝不能当作成功的空结果。
