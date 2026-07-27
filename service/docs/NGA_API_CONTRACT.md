# NGA API Contract

This document freezes the NGA request and response contract used by the Rust service. It is based on
read-only probes performed with a real authenticated account. Repository fixtures are structurally
representative and fully sanitized; they do not contain real cookies, usernames, post bodies, or probed
business IDs.

## Common request profile

All NGA data requests keep the following headers:

```text
Content-Type: application/x-www-form-urlencoded
User-Agent: <configured user agent>
Accept: application/json, text/javascript, */*; q=0.01
Accept-Language: en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7
Cookie: ngaPassportUid=<secret>; ngaPassportCid=<secret>
Origin: https://bbs.nga.cn
Referer: https://bbs.nga.cn/
```

Only `ngaPassportUid` and `ngaPassportCid` are required from the browser Cookie string. They must never
appear in logs, fixtures, API responses, exports, or error payloads.

Credential validation must use the user-reply endpoint, not the user-profile page. A real UID profile
can remain publicly readable even when `ngaPassportCid` is invalid. On the reply endpoint,
`code=2048` with `msg` containing `必须登录` means invalid credentials, while `code=2048` with
`服务器忙` follows the retry policy below.

Every response is checked in this order:

1. HTTP status and non-empty body.
2. JSON decoding for `__output=12` and `app_api.php` responses.
3. NGA top-level business `code`.

HTTP success does not imply NGA business success.

## Endpoints

| Purpose | Request | Fixture |
| --- | --- | --- |
| Thread page | `POST /app_api.php?__lib=post&__act=list`, form `tid`, `page` | `thread_page_success.json` |
| One post by PID | Same endpoint, form `tid`, `pid` | `post_by_pid_success.json` |
| User topics | `GET /thread.php?authorid={uid}&__output=12&page={page}` | `user_topics_page_1.json`, `user_topics_page_2.json` |
| User replies | `GET /thread.php?searchpost=1&authorid={uid}&__output=12&page={page}` | `user_replies_success.json` |
| User profile | `GET /nuke.php?func=ucp&uid={uid}` | `user_profile_gbk.html` |

## Thread page

Successful thread responses are UTF-8 JSON with `code=0`. Important top-level fields:

```text
currentPage, totalPage, perPage, vrows
fid, forum_name
tsubject, tauthor, tauthorid
attachPrefix, hot_post
result[]
```

Important post fields:

```text
tid, pid, fid, lou
postdate, postdatetimestamp
subject, content, type
author.uid, author.username
attches, comments
vote, vote_good, vote_bad
```

Contract:

- `result` is ordered by ascending `lou`.
- The topic starts at `lou=0` and may use `pid=0`.
- `perPage` is currently 20, but the final page can be shorter.
- `vrows` includes the topic and grows when new replies are added.
- A `tid` plus non-zero `pid` request returns only the target post.
- Thread watches persist every accessible item.
- User watches requesting page 1 for topic completion persist only `lou=0` with a matching author UID.

### Nested comments

Nested comments are an array in the parent post's `comments` field. The observed contract:

- Comment `tid` equals the containing post's TID.
- Comment `pid` is non-zero and unique from normal result PIDs in the verified sample.
- Comment `lou` is zero and is not a thread floor number.
- The database parent relation comes from JSON containment.
- `comment_to_id` is polymorphic: it can equal a parent PID or a replied-to user UID. Preserve it as raw
  metadata; do not use it as `parent_post_id`.

Use `(tid, pid)` as the natural key for replies and comments. Use `(tid, topic kind)` for the topic.

### Hot posts

`hot_post` can be a non-empty array containing full post objects. Verified hot-post PIDs also occur in
the normal `result` array. Parse hot posts as metadata/ranking references and resolve them to the
canonical post by `(tid, pid)`; never insert a second post or event.

### Attachments

`attches` is an array. Observed fields:

```text
attachurl, path, name, ext, type, size, subid
thumb, dscp, hash, url_utf8_org_name
```

`attachurl` and `path` are relative. Construct the remote resource URL as `attachPrefix + attachurl`.
A HEAD probe of the constructed sample URL returned HTTP 200 with `image/jpeg`. Asset download still
validates scheme, host policy, response size, and content type before writing locally.

## User topics

Successful responses use:

```text
result.__T[]
result.__T__ROWS
result.__T__ROWS_PAGE
result.__ROWS
result.__F
result.__CU
result.__GLOBAL
```

The observed `__T__ROWS_PAGE` is 35. Calculate the maximum page as:

```text
ceil(parse_int(__ROWS) / __T__ROWS_PAGE)
```

Do not stop on a short `__T` array: inaccessible records can make the returned array shorter than the
server-side count while later pages still exist.

`result.__T` can contain inaccessible placeholders with `denied=true` and a non-empty `error`. Such
records are diagnostic only. A topic candidate is accepted only when it is accessible and its
`authorid` equals the watched UID.

User-topic monitoring saves only the watched user's topic post. It does not create a thread watch or
persist other replies from that TID.

## User replies

The response stores topic summaries in `result.__T`. The watched user's matching reply is nested in
`__P` and includes:

```text
tid, pid, authorid, postdate, subject, content, type
```

The observed reply page capacity in `result.__R__ROWS_PAGE` is 20. Adjacent pages are ordered by
descending `postdate` and have no overlapping PIDs.

The collector accepts only `__P.authorid == watched_uid`, then requests the post endpoint with TID/PID
and verifies `result[0].author.uid == watched_uid` before insertion. It never expands the discovered TID
into a full thread crawl.

## Busy response

User topic and reply queries can return HTTP 200 with:

```json
{"code":2048,"msg":"服务器忙,请稍后重试"}
```

Retry the same request once per second, with at most 10 total attempts. If all attempts return 2048,
mark the crawl as `skipped_busy`, write no posts or events, and do not advance the cursor.

HTTP errors, empty responses, JSON failures, and other NGA business codes use their own error
classification and must not be treated as 2048.

## Confirmed errors

NGA returns these errors with HTTP 200 and an empty `result` array:

| Condition | Business code | Message | Fixture |
| --- | ---: | --- | --- |
| Unknown TID | 14 | `找不到主题` | `invalid_tid_14.json` |
| Missing or invalid Passport cookies | 46 | `访客不能直接访问` | `missing_auth_46.json` |

Code 46 pauses tasks assigned to that account until credentials are updated and a connectivity test
succeeds. Code 14 is permanent for that request and is not retried on the normal transient-error path.

## User profile

The user profile endpoint returns `text/html; charset=GBK`, including when `__output=12` is supplied.
Decode GBK to UTF-8, locate the assignment `__UCPUSER = {...};`, and parse only the JSON object. Never
execute page JavaScript.

Useful fields include `uid`, `username`, `groupid`, `avatar`, `regdate`, `lastpost`, `posts`, and `sign`.

For a tested nonexistent UID, the profile endpoint returned HTTP 200 and a GBK page without a
`__UCPUSER` object. Validate a new UID watch with this endpoint before scheduling list requests.
The user-topic list endpoint returned an empty HTTP 503 for the nonexistent UID; this is recorded as an
observed transport outcome, not treated as an empty successful list or a stable NGA business code.

## Persistence and notification deduplication

- Topic posts are unique by TID and topic kind because their PID may be zero.
- Reply/comment natural keys use `(tid, pid)`; parent-child relations are separate.
- Live inserts use `INSERT ... ON CONFLICT DO NOTHING RETURNING id`.
- Only a successful live insert creates a `post_event`; baseline imports do not notify.
- `post_events` are unique by `(post_id, event_type)`.
- Notification outbox rows are unique by `(post_event_id, channel_id)`.
- TID and UID rules may both be recorded in `post_event_matches`, but they share one channel delivery.
- Discovery order—TID then UID, UID then TID, or concurrent—must not change the single-delivery result.

## Deferred probe

Permission-denied thread/post responses are intentionally deferred and do not block M0. Unknown NGA
business codes remain typed errors and are never treated as successful empty results.
