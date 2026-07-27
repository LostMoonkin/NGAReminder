# NGA fixtures

These fixtures are synthetic, sanitized equivalents of structures observed during authenticated,
read-only NGA probes.

Rules:

- Never store a Cookie header or credential value.
- Never store a real username, post body, signature, avatar URL, webhook, or device key.
- Replace TID, PID, UID, FID, timestamps, and URLs with internally consistent synthetic values.
- Preserve JSON types, nesting, optional/null fields, ordering, and pagination relationships.
- Keep raw probe responses only in a temporary directory and delete them after sanitization.
- Record a new fixture here before changing parser behavior.

Current fixtures:

| File | Contract |
| --- | --- |
| `thread_page_success.json` | Thread page, topic PID zero, ascending floors |
| `thread_comments_hot_post.json` | Nested comments and duplicate hot-post reference |
| `thread_attachments.json` | Relative attachment metadata |
| `post_by_pid_success.json` | TID/PID detail returns one post |
| `user_topics_page_1.json` | Accessible topic plus denied placeholder |
| `user_topics_page_2.json` | Final user-topic page |
| `user_replies_success.json` | Topic summary with watched reply in `__P` |
| `busy_2048.json` | HTTP-success NGA busy response |
| `invalid_tid_14.json` | Unknown TID business error |
| `missing_auth_46.json` | Missing Passport-cookie business error |
| `user_profile_gbk.html` | Synthetic GBK profile page with `__UCPUSER` |
| `invalid_uid_profile_gbk.html` | Synthetic GBK page without `__UCPUSER` |
| `invalid_uid_http_503.json` | Observed empty HTTP response envelope for an invalid UID list request |

Deferred:

- Permission-denied thread/post response.
