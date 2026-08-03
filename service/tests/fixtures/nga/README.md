# NGA 测试样例

这些 fixture 是在认证账号下执行只读 NGA 探测后，根据观察到的结构生成的合成、脱敏样本。

规则：

- 不得保存 Cookie 请求头或任何凭据值。
- 不得保存真实用户名、帖子正文、签名、头像 URL、webhook 或设备密钥。
- 将 TID、PID、UID、FID、时间戳和 URL 替换为内部一致的合成值。
- 保留 JSON 类型、嵌套关系、可选/null 字段、顺序和分页关系。
- 原始探测响应只能暂存在临时目录中，脱敏后立即删除。
- 修改解析器行为前，先在此处记录对应的新 fixture。

当前 fixture：

| File | Contract |
| --- | --- |
| `thread_page_success.json` | 主题页面、主题 PID 为零、楼层升序 |
| `thread_comments_hot_post.json` | 楼中楼评论和重复的热门帖子引用 |
| `thread_attachments.json` | 相对路径附件元数据 |
| `post_by_pid_success.json` | TID/PID 详情只返回一条帖子 |
| `user_topics_page_1.json` | 可访问主题和无权访问占位记录 |
| `user_topics_page_2.json` | 用户主题列表最后一页 |
| `user_replies_success.json` | 包含 `__P` 中目标回复的主题摘要，并覆盖真实响应中 `__ROWS=null` |
| `busy_2048.json` | HTTP 成功但 NGA 返回 busy 的响应 |
| `thread_pending_review_51.json` | 主题待审核业务错误 |
| `invalid_tid_14.json` | 未知 TID 业务错误 |
| `missing_auth_46.json` | Passport Cookie 缺失业务错误 |
| `user_profile_gbk.html` | 含 `__UCPUSER` 的合成 GBK 资料页 |
| `invalid_uid_profile_gbk.html` | 不含 `__UCPUSER` 的合成 GBK 页面 |
| `invalid_uid_http_503.json` | 用户列表观察到的空体 HTTP 503 响应封装；该响应不能证明列表为空，必须按搜索不可用处理 |

延后补充：

- 无权访问主题/帖子的响应。
