# NGA Reminder

A comprehensive toolset to monitor NGA forum threads and receive notifications for new posts. This project consists of a Python backend server that crawls NGA and provides an API, and a Chrome extension that polls this API to deliver browser notifications.

## Project Structure

- **`server/`**: Python FastAPI backend. Handles NGA crawling, data storage (SQLite), and provides a REST API.
- **`extension/`**: Chrome Extension (Manifest V3). Polls the server API and displays system notifications.
- **`.github/`**: CI/CD workflows for building Docker images.

---

## 🚀 Server

The server is the core component. It monitors configured threads, saves posts to a local database to prevent data loss, and exposes an API for clients.

### Features
- **Smart Monitoring**: Intelligent sync logic that resumes interrupted fetches.
- **Rate Limiting**: Configurable request delays to avoid NGA bans.
- **Local Database**: SQLite storage for all fetched threads and posts.
- **API Access**: REST endpoints to query post data.
- **Docker Support**: Ready-to-use Alpine-based Docker image.

### Usage

#### Option 1: Docker (Recommended)

```bash
cd server
docker-compose up -d
```
This will start the server on port `8848`. Data and config are persisted in `./data` and `./config`.

#### Option 2: Local Python

1.  **Install Dependencies**:
    ```bash
    cd server
    pip install -r requirements.txt
    ```
2.  **Configuration**:
    Copy `config.example.json` to `config/config.json` and edit it (add your NGA UID/CID if needed).
    ```bash
    mkdir config
    cp config/config.example.json config/config.json
    ```
3.  **Run**:
    ```bash
    python main.py server
    ```

---

## 🧩 Chrome Extension

The extension connects to your running server instance to notify you of new posts in real-time.

### Features
- **Multi-Thread Support**: Monitor multiple threads simultaneously.
- **Filters**: Notify only for specific authors if desired.
- **Configurable Polling**: Set how often to check for updates.
- **System Notifications**: Native Chrome notifications with post previews.

### Installation

1.  Open Chrome and navigate to `chrome://extensions/`.
2.  Enable **Developer mode** in the top right corner.
3.  Click **Load unpacked**.
4.  Select the `extension` folder from this project (`/path/to/NGAReminder/extension`).

### Configuration

1.  Click the extension icon in your toolbar.
2.  **Server URL**: Default is `http://127.0.0.1:8848` (ensure your server is running!).
3.  **Add Thread**: Enter the Thread ID (TID) you want to watch.
    - *Start Post*: Set to `0` to see new posts from now on, or the last post number you read.
4.  Click **Save All Settings**.
5.  Use the **Test Server Connection** button to verify connectivity.

---

## Development

- **Server**: Built with FastAPI, SQLite, and Requests.
- **Extension**: Built with HTML/CSS/JS (Manifest V3).

To build the server Docker image locally:
```bash
cd server

---

## 🤖 AI Development

This project was fully architected, coded, and documented by AI Assistants.
