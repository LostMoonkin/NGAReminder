# NGA Reminder (Standalone)

A Chrome extension that monitors NGA threads and sends notifications - **No server required!**

This standalone version works entirely within your browser by reading your NGA cookies directly, eliminating the need for a separate Python server.

## ✨ Features

- 🔐 **Automatic Authentication** - Uses your browser's NGA cookies
- 🔔 **Real-time Notifications** - Get notified of new posts instantly
- ⏰ **Time-based Intervals** - Different check frequencies for different times/days
- 👤 **Author Filtering** - Only get notified for specific authors
- 📊 **Multi-thread Monitoring** - Monitor multiple threads simultaneously
- 🚀 **Zero Setup** - Just install and configure threads

## 🆚 vs Server Version

| Feature | Standalone | Server-based |
|---------|------------|--------------|
| Setup Required | Extension only | Server + Extension |
| Authentication | Browser cookies | Config file |
| Historical Data | No | Yes (SQLite) |
| Resource Usage | Low | Medium |
| Works When | Browser open | Server running |

**Choose Standalone if:** You want simple setup and only need new posts  
**Choose Server if:** You want historical data and server-based automation

## 📦 Installation

1. Open Chrome and go to `chrome://extensions/`
2. Enable **Developer mode** (toggle in top right)
3. Click **Load unpacked**
4. Select the `extension-standalone` folder

## 🔧 Setup

### Step 1: Login to NGA

1. Click the extension icon
2. If you see "Not logged in to NGA", click **Open NGA to Login**
3. Log in to bbs.nga.cn
4. Return to extension - should show "✓ Logged in as UID XXXXX"

### Step 2: Add Threads

1. Click **+ Add** button
2. Fill in thread configuration:
   - **Thread ID (TID)**: Get from thread URL (e.g., `45974302`)
   - **Author UIDs to Notify**: Optional, comma-separated (e.g., `150058,123456`)
   - **Check Interval**: How often to check (in seconds, minimum 60)

3. *Optional:* Configure time-based schedule

### Step 3: Configure Time-based Intervals (Optional)

Check "Use Time-based Schedule" and add JSON configuration:

**Example 1: Weekday/Weekend Schedule**
```json
[
  {
    "days": ["weekdays"],
    "start_time": "09:00",
    "end_time": "18:00",
    "interval": 300,
    "description": "Weekday business hours - every 5 min"
  },
  {
    "days": ["weekdays"],
    "start_time": "18:00",
    "end_time": "09:00",
    "interval": 900,
    "description": "Weekday nights - every 15 min"
  },
  {
    "days": ["weekends"],
    "start_time": "00:00",
    "end_time": "23:59",
    "interval": 1800,
    "description": "Weekends - every 30 min"
  }
]
```

**Example 2: Day/Night Only**
```json
[
  {
    "start_time": "08:00",
    "end_time": "22:00",
    "interval": 300,
    "description": "Daytime - every 5 min"
  },
  {
    "start_time": "22:00",
    "end_time": "08:00",
    "interval": 1800,
    "description": "Nighttime - every 30 min"
  }
]
```

**Day Aliases:**
- `weekdays` = Monday - Friday
- `weekends` = Saturday - Sunday

## 📖 Usage

### Managing Threads

- **Toggle On/Off**: Use the switch next to each thread
- **Edit**: Click "Edit" to modify configuration
- **Delete**: Click "Delete" to remove thread
- **Test**: Click "Test" to manually trigger a check

### Notifications

- Notifications appear when new posts are found
- Click notification to dismiss
- Only posts from specified authors (if configured) trigger notifications
- If no authors specified, all new posts trigger notifications

## 🔍 How It Works

1. **Cookie Reading**: Extension uses Chrome's privileged `cookies` API to read `ngaPassportUid` and `ngaPassportCid` (even though they're httpOnly)
2. **API Calls**: Makes direct requests to NGA's API with your cookies
3. **Page Calculation**: Automatically calculates which pages contain new posts
4. **Multi-page Fetching**: Fetches all necessary pages to get complete new posts
5. **Notifications**: Sends Chrome notifications for new posts

## 🔒 Privacy & Security

- ✅ All data stays in your browser (chrome.storage.local)
- ✅ No external servers contacted except NGA
- ✅ No data collection or analytics
- ✅ Open source - verify the code yourself

## ⚠️ Limitations

- Browser must be open for monitoring to work
- No historical post storage (only tracks last seen post number)
- Limited by chrome.storage quota (should be fine for typical use)
- Per-browser configuration (doesn't sync across devices)

## 🐛 Troubleshooting

### "Not logged in to NGA"
- Make sure you're logged in at bbs.nga.cn
- Try logging out and back in
- Check if cookies are enabled for nga.cn

### No notifications appearing
- Check if Chrome notifications are enabled for the extension
- Verify thread is toggled ON
- Check if enough time has passed since last check
- Use "Test" button to trigger manual check

### "Error checking thread"
- Check browser console (F12) for detailed errors
- Verify thread ID is correct
- Check if you're still logged in to NGA

## 🛠️ Development

### File Structure
```
extension-standalone/
├── manifest.json       # Extension configuration
├── nga-api.js         # NGA API client
├── background.js      # Background service worker
├── popup.html         # Popup UI
├── popup.js           # Popup logic
├── styles.css         # Popup styles
├── icons/             # Extension icons
└── README.md          # This file
```

### Testing
1. Load extension in developer mode
2. Check background service worker console for logs
3. Use "Test" button to trigger checks manually
4. Monitor background logs for API calls and errors

## 📝 License

Same as parent project

## 🤝 Contributing

Contributions welcome! Please test thoroughly before submitting PRs.

## 📮 Support

For issues or questions, please open an issue on the main project repository.

---

**Note:** This extension reads your NGA authentication cookies to make API calls. This is safe and necessary for the extension to work, as it's making requests on your behalf. However, be aware that any extension with cookie access permissions has this capability.
