# NGA Reminder

[🇨🇳 中文](README.zh-CN.md)

A comprehensive toolset to monitor NGA forum threads and receive real-time notifications for new posts.

## 🎯 Two Versions Available

### 🔌 Standalone Extension (Recommended)
**No server required!** Works entirely in your browser.
- ✅ Quick setup - just install and configure
- ✅ Uses your browser's NGA cookies automatically
- ✅ Bark integration for mobile notifications when browser is unfocused
- ✅ Clickable notification URLs to jump directly to posts
- ⚠️ Requires browser to be running

**[→ View Standalone Extension Documentation](extension-standalone/README.md)**

###  Server + Extension
Full-featured setup with data persistence.
- ✅ Historical post storage in SQLite database
- ✅ Works 24/7 independent of browser
- ✅ API access for custom integrations
- ⚠️ Requires Python server setup

**[→ View Server Documentation](server/README.md)**
**[→ View Server Extension Documentation](extension/README.md)**

---

## 🚀 Quick Start (Standalone)

### Installation

#### From GitHub Releases (Easy)
1. Download the latest `extension-standalone.zip` from [Releases](https://github.com/LostMoonkin/NGAReminder/releases)
2. Extract the ZIP file
3. Open Chrome → `chrome://extensions/`
4. Enable **Developer mode** (top right)
5. Click **Load unpacked**
6. Select the extracted folder

#### From Source
```bash
git clone https://github.com/LostMoonkin/NGAReminder.git
cd NGAReminder
```

Then load `extension-standalone` folder as unpacked extension in Chrome.

### Configuration

1. **Login to NGA**: Click extension icon → Click "Open NGA to Login" → Log in at bbs.nga.cn
2. **Add Thread**: Click "+ Add" → Enter Thread ID (TID) from thread URL
3. **Configure Bark (Optional)**: Add your Bark device key for mobile notifications
4. **Done!** Extension will check for new posts automatically

---

## ✨ Key Features

### Standalone Extension

| Feature | Description |
|---------|-------------|
| 🔐 **Auto Authentication** | Uses browser cookies - no manual config |
| 🔔 **Smart Notifications** | Chrome popups when browser focused, Bark when unfocused |
| 🔗 **Clickable URLs** | Click notification to jump to exact post |
| ⏰ **Time Schedules** | Different check intervals for different times/days |
| 👤 **Author Filters** | Get notified only for specific users |
| 📊 **Multi-thread** | Monitor unlimited threads simultaneously |
| 🎯 **Start Position** | Set initial post number to skip earlier posts |

### Server Version

| Feature | Description |
|---------|-------------|
| 💾 **Data Persistence** | All posts saved to SQLite database |
| 🔄 **Continuous Monitoring** | Runs 24/7 independently |
| 🐳 **Docker Ready** | One-command deployment |
| 🚦 **Rate Limiting** | Configurable delays to avoid bans |
| 📡 **REST API** | Query historical data programmatically |

---

## 📊 Version Comparison

| Aspect | Standalone | Server + Extension |
|--------|------------|--------------------|
| **Setup Complexity** | ⭐ Simple | ⭐⭐⭐ Moderate |
| **Running Requirements** | Browser open | Server running |
| **Historical Data** | ❌ No | ✅ SQLite DB |
| **Mobile Notifications** | ✅ Bark | ⚠️ Via extension only |
| **Resource Usage** | Low | Medium |
| **Offline Monitoring** | ❌ No | ✅ Yes |
| **Authentication** | Browser cookies | Config file |

---

## 🔧 Advanced Configuration

### Time-based Check Intervals (Standalone)

Configure different check frequencies based on time and day:

```json
[
  {
    "days": ["weekdays"],
    "start_time": "09:00",
    "end_time": "18:00",
    "interval": 300,
    "description": "Work hours - every 5 min"
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

### Bark Mobile Notifications

1. Install Bark on iOS/Android
2. Get your device key from the app
3. In extension: Bark Settings → Enter device key → Save
4. When browser is unfocused, you'll get Bark notifications instead

---

## 🏗️ Architecture

### Standalone Extension Flow
```
Browser → NGA Cookies → Extension Background Worker → NGA API
                                    ↓
                        Chrome Storage (thread config)
                                    ↓
                    Bark (unfocused) / Chrome Notification (focused)
```

### Server Version Flow
```
Python Server → NGA API → SQLite Database
                    ↓
            REST API Endpoints
                    ↓
        Chrome Extension → System Notifications
```

---

## 🛠️ Development

### Project Structure
```
NGAReminder/
├── extension-standalone/    # Standalone Chrome extension
│   ├── manifest.json
│   ├── background.js       # Background service worker
│   ├── nga-api.js          # NGA API client
│   ├── popup.html/js       # Configuration UI
│   └── README.md
├── extension/              # Server-connected extension
│   └── ...
├── server/                 # Python FastAPI server
│   ├── src/               # Source code
│   ├── Dockerfile         # Docker build config
│   └── README.md
└── .github/
    └── workflows/         # CI/CD pipelines
```

### Building Standalone Extension

The extension is automatically packaged on GitHub releases:

```bash
# Manual build
cd extension-standalone
zip -r extension-standalone.zip . -x "*.git*" "README.md"
```

---

## 📝 License

MIT License - See [LICENSE](LICENSE) file for details

---

## 🤖 Credits

This project was fully architected, coded, and documented by AI Assistants through iterative collaboration.

**Technologies Used:**
- Chrome Extension Manifest V3
- Python FastAPI
- SQLite
- Docker
- Bark Push Notification Service

---

## 🐛 Troubleshooting

### Common Issues

**Standalone Extension:**
- **Not logged in**: Make sure you're logged in at bbs.nga.cn
- **No notifications**: Check Chrome notification permissions and thread toggle
- **Bark not working**: Verify device key is correct

**Server Version:**
- **Connection failed**: Ensure server is running and URL is correct
- **No new posts**: Check server logs and rate limiting settings

For detailed troubleshooting, see individual READMEs:
- [Standalone Extension Issues](extension-standalone/README.md#-troubleshooting)
- [Server Issues](server/README.md#troubleshooting)

---

## 📮 Support

- **Issues**: [GitHub Issues](https://github.com/LostMoonkin/NGAReminder/issues)
- **Discussions**: [GitHub Discussions](https://github.com/LostMoonkin/NGAReminder/discussions)

---

**⚠️ Disclaimer:** This is an unofficial tool. Use responsibly and respect NGA's terms of service. The extension reads your authentication cookies to make API calls on your behalf.
