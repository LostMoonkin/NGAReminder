# Bark Notification System

## 📬 Overview

The monitoring system now supports **Bark notifications** for alerting you when specific authors post new content. The system uses an extensible notification architecture that can support multiple notification methods.

---

## 🧩 Architecture

### Interface-Based Design

```
NotificationSender (Abstract Base Class)
    ├─ BarkNotificationSender
    ├─ ConsoleNotificationSender  
    └─ (Easy to add more: Email, Webhook, etc.)
```

**Benefits:**
- Clean separation of concerns
- Easy to add new notification methods
- Each sender can be enabled/disabled independently

---

## ⚙️ Configuration

### Bark Settings

Add to your ` config.json`:

```json
{
  "bark_enabled": true,
  "bark_server_url": "https://api.day.app",
  "bark_device_key": "your_device_key_here",
  "bark_sound": "bell",
  "bark_group": "NGA Monitor",
  "bark_icon": "",
  "console_notification_enabled": true
}
```

**Bark Parameters:**
- `bark_enabled`: Enable/disable Bark notifications
- `bark_server_url`: Bark server URL (default: https://api.day.app)
- `bark_device_key`: Your Bark device key (get from Bark app)
- `bark_sound`: Notification sound (bell, alarm, etc.)
- `bark_group`: Group name in Bark app
- `bark_icon`: Optional custom icon URL
- `console_notification_enabled`: Show notifications in console (for debugging)

### Per-Thread Notification

For each monitored thread, specify which authors should trigger notifications:

```json
{
  "monitored_threads": [
    {
      "tid": 45974302,
      "author_filter": [150058, 42098303],
      "author_notification": [150058],
      "check_interval": 300,
      "enabled": true
    }
  ]
}
```

**Fields:**
- `author_filter`: Which authors' posts to **save** (or `null` for all)
- `author_notification`: Which authors' posts to **notify about** (or `null` for none)
- They can be different!

---

## 📊 Use Cases

### 1. Monitor Specific Author, Notify on Same

```json
{
  "tid": 45974302,
  "author_filter": [150058],
  "author_notification": [150058]
}
```

**Result:**
- Saves only posts by UID 150058
- Sends Bark notification for posts by UID 150058

### 2. Monitor All, Notify on Specific

```json
{
  "tid": 43098323,
  "author_filter": null,
  "author_notification": [150058, 42098303]
}
```

**Result:**
- Saves ALL posts (complete archive)
- Only sends notifications for UIDs 150058 and 42098303

### 3. Monitor Specific, Notify Different Subset

```json
{
  "tid": 45922084,
  "author_filter": [150058, 42098303, 39248719],
  "author_notification": [150058]
}
```

**Result:**
- Saves posts by 3 authors
- Only notifies about UID 150058 (VIP author)

### 4. Monitor All, No Notifications

```json
{
  "tid": 39248719,
  "author_filter": null,
  "author_notification": null
}
```

**Result:**
- Saves all posts
- No notifications (silent archiving)

---

## 🔔 Notification Format

### Bark Notification

**Title:** 📬 [Thread Title]  
**Message:** [Author Name]: [Post Content Preview (100 chars)]  
**URL:** Direct link to the post

**Example:**
```
Title: 📬 自立自强,科学技术打头阵
Message: -阿狼-: 今天科技股大涨，半导体板块领涨，建议关注...
URL: https://bbs.nga.cn/read.php?tid=45974302&pid=854599234
```

Tapping the notification opens the thread directly to that post!

---

## 🚀 Setup Guide

### Step 1: Install Bark

1. Download Bark app (iOS/Mac)
2. Get your device key from the app
3. Note the server URL (usually https://api.day.app)

### Step 2: Configure

Edit `config.json`:

```json
{
  "bark_enabled": true,
  "bark_device_key": "ABC123DEF456",  // Your actual key
  "bark_sound": "bell",
  "bark_group": "NGA"
}
```

### Step 3: Add Thread with Notifications

```json
{
  "monitored_threads": [
    {
      "tid": 45974302,
      "author_notification": [150058]
    }
  ]
}
```

### Step 4: Sync and Start

```bash
python monitor.py sync
python monitor.py loop
```

---

## 🧪 Testing

### Test Notification System

```bash
python notification.py
```

This sends a test notification to verify Bark is configured correctly.

### Monitor with Console Notifications

Keep `console_notification_enabled: true` to see notifications in the terminal for debugging:

```
================================================================================
📱 NOTIFICATION
================================================================================
Title: 📬 自立自强,科学技术打头阵
Message: -阿狼-: 今天科技股大涨...
URL: https://bbs.nga.cn/read.php?tid=45974302&pid=854599234
================================================================================
```

---

## 🔧 Advanced Configuration

### Multiple Notification Channels

The system supports multiple notification senders simultaneously:

- **Bark**: Mobile/desktop notifications
- **Console**: Terminal output (debugging)
- **Future**: Email, Webhook, Telegram, etc.

### Custom Sounds

Bark supports various sounds:
- `bell` - Default bell
- `alarm` - Alarm sound
- `anticipate` - Anticipation sound
- `birdsong` - Bird chirping
- `bloom` - Bloom sound
- ...and many more

Set in config:
```json
{
  "bark_sound": "alarm"  // For urgent notifications
}
```

### Custom Icons

Add a custom icon URL:
```json
{
  "bark_icon": "https://example.com/nga-icon.png"
}
```

---

## 📝 Notification Logic

### When Are Notifications Sent?

1. Monitor checks thread for new posts
2. Filters posts by `author_filter` (if set)
3. For each filtered post:
   - Check if author UID is in `author_notification`
   - If yes, send notification via all enabled senders

### Filtering Example

```
Thread has 5 new posts:
  - UID 150058: "Post A" ✅ matches author_filter [150058, 42098303]
  - UID 42098303: "Post B" ✅ matches author_filter [150058, 42098303]
  - UID 99999: "Post C" ❌ not in author_filter
  - UID 150058: "Post D" ✅ matches author_filter [150058, 42098303]
  - UID 150058: "Post E" ✅ matches author_filter [150058, 42098303]

Filtered posts: 4 (A, B, D, E)

Notification logic:
  - Post A (150058) ✅ in author_notification [150058] → NOTIFY
  - Post B (42098303) ❌ not in author_notification [150058] → SKIP
  - Post D (150058) ✅ in author_notification [150058] → NOTIFY
  - Post E (150058) ✅ in author_notification [150058] → NOTIFY

Total notifications: 3
```

---

## 🔐 Security

- **Device Key**: Keep your `bark_device_key` secret!
- **Config File**: Already in `.gitignore`, won't be committed
- **HTTPS**: Bark uses HTTPS by default

---

## 🐛 Troubleshooting

### Notifications Not Sent

**Check:**
1. `bark_enabled: true` in config?
2. Valid `bark_device_key`?
3. Author UID in `author_notification` list?
4. Bark app installed and configured?

**Debug:**
```bash
# Enable console notifications to see what's being sent
"console_notification_enabled": true

# Check logs when monitoring
python monitor.py loop
```

### Bark API Error

**Error:** `400 Bad Request`  
**Solution:** Check your device key is correct

**Error:** `Connection timeout`  
**Solution:** Check internet connection and bark_server_url

### No Notifications for New Posts

**Possible causes:**
1. Author UID not in ` author_notification` list
2. Posts filtered out by `author_filter`
3. No new posts from specified authors

---

## 🎯 Best Practices

1. **Start with console notifications** - Verify logic works
2. **Enable Bark later** - After confirming behavior
3. **Use specific author_notification** - Avoid notification spam
4. **Test with real data** - Add thread, wait for new post
5. **Monitor log output** - Check what's being detected

---

## 🔮 Future Enhancements

The interface-based design makes it easy to add:

- **Email notifications**
- **Webhook integrations** (Discord, Slack)
- **Telegram bot**
- **Custom notification scripts  **
- **Database-based notification queue**

---

## 📖 Code Example

### Adding a Custom Notification Sender

```python
from notification import NotificationSender

class EmailNotificationSender(NotificationSender):
    def __init__(self, config):
        self.smtp_server = config.get('smtp_server')
        self.to_email = config.get('notification_email')
        # ... more config
    
    def is_configured(self):
        return bool(self.smtp_server and self.to_email)
    
    def send(self, title, message, **kwargs):
        # Send email via SMTP
        # Implementation details...
        return True
```

Then add to `NotificationManager` initialization.

---

## ✅ Summary

✅ **Extensible** - Interface-based, easy to add new channels  
✅ **Flexible** - Different filter/notification settings per thread  
✅ **Powerful** - Bark push notifications to phone/desktop  
✅ **Debuggable** - Console notifications for testing  
✅ **Secure** - Config in gitignore, HTTPS by default  

Start getting instant notifications for important NGA posts! 📬
