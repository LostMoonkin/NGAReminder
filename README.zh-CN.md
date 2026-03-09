# NGA Reminder

监控 NGA 论坛帖子，实时接收新回复通知的综合工具集。

## 🎯 两种版本

### 🔌 独立扩展（推荐）
**无需服务器！** 完全在浏览器中运行。
- ✅ 快速上手 - 安装即用
- ✅ 自动使用浏览器的 NGA Cookies
- ✅ 集成 Bark，在浏览器不在前台时发送手机通知
- ✅ 通知 URL 可点击，直接跳转到对应帖子
- ⚠️ 需要保持浏览器运行

**[→ 查看独立扩展文档](extension-standalone/README.md)**

###  服务端 + 扩展
功能完整，支持数据持久化。
- ✅ 帖子数据保存至 SQLite 数据库
- ✅ 全天候独立运行（24/7）
- ✅ 提供 API 接口供自定义集成
- ⚠️ 需要部署 Python 服务端

**[→ 查看服务端文档](server/README.md)**
**[→ 查看扩展文档（服务端版）](extension/README.md)**

---

## 🚀 快速开始（独立版）

### 安装

#### 从 GitHub Releases 安装（简单）
1. 从 [Releases](https://github.com/YOUR_USERNAME/NGAReminder/releases) 下载最新的 `extension-standalone.zip`
2. 解压 ZIP 文件
3. 打开 Chrome → `chrome://extensions/`
4. 开启右上角的**开发者模式**
5. 点击**加载已解压的扩展程序**
6. 选择解压后的文件夹

#### 从源码安装
```bash
git clone https://github.com/YOUR_USERNAME/NGAReminder.git
cd NGAReminder
```

然后将 `extension-standalone` 文件夹作为已解压扩展加载到 Chrome 中。

### 配置

1. **登录 NGA**：点击扩展图标 → 点击"打开 NGA 登录" → 在 bbs.nga.cn 完成登录
2. **添加帖子**：点击"+ 添加" → 输入帖子 URL 中的帖子 ID（TID）
3. **配置 Bark（可选）**：添加您的 Bark 设备密钥以接收手机通知
4. **完成！** 扩展将自动检查新帖子

---

## ✨ 主要特性

### 独立扩展

| 特性 | 描述 |
|------|------|
| 🔐 **自动认证** | 使用浏览器 Cookies，无需手动配置 |
| 🔔 **智能通知** | 浏览器聚焦时弹出 Chrome 通知，失焦时发送 Bark 通知 |
| 🔗 **可点击链接** | 点击通知直接跳转到对应帖子 |
| ⏰ **时间计划** | 不同时间段和日期设置不同检查间隔 |
| 👤 **作者过滤** | 仅对特定用户的回复发送通知 |
| 📊 **多帖监控** | 同时监控无限数量的帖子 |
| 🎯 **起始位置** | 设置初始楼层，跳过之前的帖子 |

### 服务端版本

| 特性 | 描述 |
|------|------|
| 💾 **数据持久化** | 所有帖子保存至 SQLite 数据库 |
| 🔄 **持续监控** | 全天候独立运行 |
| 🐳 **Docker 支持** | 一键部署 |
| 🚦 **频率限制** | 可配置请求延迟，避免封禁 |
| 📡 **REST API** | 支持以编程方式查询历史数据 |

---

## 📊 版本对比

| 方面 | 独立版 | 服务端 + 扩展 |
|------|--------|--------------|
| **配置复杂度** | ⭐ 简单 | ⭐⭐⭐ 中等 |
| **运行条件** | 保持浏览器打开 | 服务端运行中 |
| **历史数据** | ❌ 无 | ✅ SQLite 数据库 |
| **手机通知** | ✅ Bark | ⚠️ 仅通过扩展 |
| **资源占用** | 低 | 中 |
| **离线监控** | ❌ 否 | ✅ 是 |
| **认证方式** | 浏览器 Cookies | 配置文件 |

---

## 🔧 高级配置

### 时间段检查间隔（独立版）

根据时间和星期配置不同的检查频率：

```json
[
  {
    "days": ["weekdays"],
    "start_time": "09:00",
    "end_time": "18:00",
    "interval": 300,
    "description": "工作日 - 每 5 分钟"
  },
  {
    "days": ["weekends"],
    "start_time": "00:00",
    "end_time": "23:59",
    "interval": 1800,
    "description": "周末 - 每 30 分钟"
  }
]
```

### Bark 手机通知

1. 在 iOS/Android 上安装 Bark
2. 从应用中获取设备密钥
3. 在扩展中：Bark 设置 → 输入设备密钥 → 保存
4. 当浏览器不在前台时，您将收到 Bark 通知

---

## 🏗️ 架构

### 独立扩展流程
```
浏览器 → NGA Cookies → 扩展后台 Worker → NGA API
                              ↓
                    Chrome Storage（帖子配置）
                              ↓
              Bark（后台）/ Chrome 通知（前台）
```

### 服务端版本流程
```
Python 服务端 → NGA API → SQLite 数据库
                    ↓
             REST API 接口
                    ↓
         Chrome 扩展 → 系统通知
```

---

## 🛠️ 开发

### 项目结构
```
NGAReminder/
├── extension-standalone/    # 独立 Chrome 扩展
│   ├── manifest.json
│   ├── background.js       # 后台 Service Worker
│   ├── nga-api.js          # NGA API 客户端
│   ├── popup.html/js       # 配置界面
│   └── README.md
├── extension/              # 服务端连接版扩展
│   └── ...
├── server/                 # Python FastAPI 服务端
│   ├── src/               # 源代码
│   ├── Dockerfile         # Docker 构建配置
│   └── README.md
└── .github/
    └── workflows/         # CI/CD 流水线
```

### 打包独立扩展

扩展会在 GitHub 发布时自动打包：

```bash
# 手动构建
cd extension-standalone
zip -r extension-standalone.zip . -x "*.git*" "README.md"
```

---

## 📝 许可证

MIT 许可证 - 详见 [LICENSE](LICENSE) 文件

---

## 🤖 致谢

本项目由 AI 助手通过迭代协作完整架构设计、编码并文档化。

**使用的技术：**
- Chrome Extension Manifest V3
- Python FastAPI
- SQLite
- Docker
- Bark 推送通知服务

---

## 🐛 常见问题

### 常见问题排查

**独立扩展：**
- **未登录**：确保您已在 bbs.nga.cn 登录
- **没有通知**：检查 Chrome 通知权限及帖子开关状态
- **Bark 不工作**：验证设备密钥是否正确

**服务端版本：**
- **连接失败**：确保服务端正在运行且 URL 正确
- **没有新帖子**：检查服务端日志和频率限制设置

详细排查方法请参阅各组件的 README：
- [独立扩展问题排查](extension-standalone/README.md#-troubleshooting)
- [服务端问题排查](server/README.md#troubleshooting)

---

## 📮 支持

- **问题反馈**：[GitHub Issues](https://github.com/YOUR_USERNAME/NGAReminder/issues)
- **讨论交流**：[GitHub Discussions](https://github.com/YOUR_USERNAME/NGAReminder/discussions)

---

**⚠️ 免责声明：** 这是一个非官方工具，请合理使用并遵守 NGA 的服务条款。扩展会读取您的认证 Cookies 以代您发起 API 请求。
