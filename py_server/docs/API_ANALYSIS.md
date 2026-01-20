# NGA BBS API Response Structure Analysis

## Overview

The NGA BBS API returns a rich JSON structure containing thread posts, author information, pagination metadata, and additional features like comments, votes, and hot posts.

---

## Top-Level Structure

```json
{
  "code": 0,                    // Status code (0 = success)
  "msg": "操作成功",             // Status message
  "result": [...],              // Array of posts (main content)
  "totalPage": 181,             // Total number of pages
  "currentPage": 1,             // Current page number
  "perPage": 20,                // Posts per page
  "vrows": 3615,                // Total number of rows/posts
  "attachPrefix": "...",        // Base URL for attachments
  "tsubject": "...",            // Thread subject/title
  "tauthorid": 150058,          // Thread author's UID
  "tauthor": "-阿狼-",          // Thread author's username
  "fid": 706,                   // Forum ID
  "forum_name": "大时代",        // Forum name
  "forum_bit": 33554568,
  "tmisc_bit1": 0,
  "hot_post": [...],            // Array of highlighted/hot posts
  "is_forum_admin": 0           // Whether user is forum admin
}
```

---

## Post Object Structure

Each post in the `result` array contains:

### Basic Post Info
- **pid** (int): Post ID (unique identifier, 0 for the first post/OP)
- **fid** (int): Forum ID
- **tid** (int): Thread ID
- **type** (int): Post type (0 = normal, 1 = comment, 33554468 = OP)
- **lou** (int): Floor number (0 for OP, 1+ for replies)
- **subject** (string): Post subject (usually empty for replies)
- **content** (string): Post content with HTML tags (`<br/>`, `[url]`, `[img]`, etc.)
- **postdate** (string): Formatted date "YYYY-MM-DD HH:MM"
- **postdatetimestamp** (int): Unix timestamp
- **from_client** (string): Client info ("0 /", "8 Android", "7 iOS")

### Engagement Metrics
- **vote_good** (int): Number of upvotes
- **vote_bad** (int): Number of downvotes
- **vote** (string): Vote details (usually empty)

### Special Features
- **isTieTiao** (bool): Whether post is pinned/sticky
- **is_highquality** (int): High quality flag (1 = yes)
- **is_user_quote** (int): Whether current user quoted this
- **follow** (int): Follow status
- **comment_to_id** (string/int): ID of post being commented on
- **alterinfo** (string): Modification/reward info (HTML)
- **attches** (null/array): Attachments
- **address** (object): Location info
- **gifts** (array): Virtual gifts received

### Author Information

Each post has an `author` object with extensive user details:

```json
{
  "uid": 150058,                    // User ID
  "username": "-阿狼-",              // Username
  "nickname": null,                 // Nickname (if different)
  "avatar": "...",                  // Avatar URL
  "credit": 135537154,              // Credit score
  "postnum": 37735,                 // Total post count
  "money": 100765,                  // Virtual currency
  "member": "学徒",                  // Member level
  "memberid": 73,
  "groupid": -1,
  "gender": 0,                      // Gender (0 = unspecified)
  "regdate": 1123989606,            // Registration timestamp
  "thisvisit": 1768351943,          // Last visit timestamp
  "mute_time": 0,                   // Mute expiration
  "mute_status": 0,
  "signature": "...",               // User signature (HTML)
  "yz": 4,                          // Unknown flag
  "rvrc": "2.9",                    // Reputation score
  "reputation": "...",              // Reputation details
  "bit_data": 135537154,
  "conferred_title": "",            // Special title
  "honor": null,                    // Special honor
  "medal": [...],                   // Array of medals
  "buffs": null/object,             // Active buffs/status effects
  "site": null,
  "__initialized__": true
}
```

### Medals

Users can have multiple medals:

```json
{
  "id": 272,
  "name": "棒棒糖",
  "dscp": "叔叔请你吃棒棒糖",
  "icon": "http://img4.nga.178.com/ngabbs/medal/272.gif"
}
```

### Comments/Nested Replies

The first post (OP) can have a `comments` array containing nested replies with the same structure as regular posts.

---

## Hot Posts Feature

The `hot_post` array contains highlighted posts with the same structure as regular posts, plus optional `gifts` field for virtual gifts received.

---

## Content Formatting

Post content uses NGA's custom BBCode-like syntax:

- `<br/>` - Line breaks
- `[url]...[/url]` - Links
- `[img]...[/img]` - Images
- `[quote]...[/quote]` - Quotes
- `[s:ac:茶]` - Emoji/stickers
- `[uid=...]username[/uid]` - User mentions
- `[tid=...]Topic[/tid]` - Thread references

---

## Pagination Logic

- **Total pages**: `totalPage` (181 in this example)
- **Current page**: `currentPage` (1-indexed)
- **Posts per page**: `perPage` (typically 20)
- **Total posts**: `vrows` (3615 in this example)

To crawl all pages: iterate from `1` to `totalPage`

---

## Key Data Points for Extraction

### For Post Monitoring/Reminders

1. **Post ID** (`pid`) - Unique identifier
2. **Author** (`author.username`, `author.uid`) - Who posted
3. **Content** (`content`) - The actual message
4. **Timestamp** (`postdatetimestamp`) - When posted
5. **Floor number** (`lou`) - Position in thread
6. **Votes** (`vote_good`) - Popularity metric

### For Author Filtering

1. **UID** (`author.uid`) - Filter by specific users
2. **Username** (`author.username`) - Display name
3. **Post count** (`author.postnum`) - Activity level

### For Content Analysis

1. **Content HTML** (`content`) - May need parsing
2. **Is OP** (`isTieTiao` or `pid == 0` or `type == 33554468`)
3. **Has attachments** (`attches != null`)
4. **Client type** (`from_client`) - Mobile vs desktop

---

## Potential Use Cases

### 1. **Post Reminder System**
- Monitor specific threads for new posts by target authors
- Send notifications when specific users post
- Track high-voted posts

### 2. **Content Archival**
- Download all pages of a thread
- Save posts with attachments
- Build searchable archives

### 3. **User Analysis**
- Track post frequency
- Analyze upvote patterns
- Monitor author activity

### 4. **Content Filtering**
- Extract only OP posts
- Filter by vote threshold
- Show only posts from specific users

### 5. **Data Export**
- Convert to readable formats (Markdown, HTML)
- Export to database
- Generate reports

---

## Recommended Next Steps

1. **Add data processing functions**:
   - Parse HTML content to plain text
   - Extract images and attachments
   - Clean BBCode formatting

2. **Implement filtering**:
   - Filter by author UID
   - Filter by vote threshold
   - Filter by date range

3. **Add export options**:
   - Save to JSON files
   - Export to CSV
   - Generate HTML/Markdown

4. **Build monitoring features**:
   - Check for new posts since last crawl
   - Detect posts by specific authors
   - Send notifications/reminders

5. **Create a database schema**:
   - Store posts persistently
   - Track crawl history
   - Enable searches and queries
