# NGA Reminder Chrome Extension

A Chrome extension that monitors NGA threads via your local API server and sends browser notifications for new posts.

## Installation

1.  Open Chrome and navigate to `chrome://extensions/`
2.  Enable **Developer mode** (toggle in the top right corner)
3.  Click **Load unpacked**
4.  Select the `extension` folder inside this project: `/root/project/NGAReminder/extension`

## Configuration

1.  Click the extension icon in your browser toolbar
2.  **API Server URL**: Default is `http://127.0.0.1:8848` (Make sure your server is running!)
3.  **Thread ID**: The ID of the NGA thread you want to monitor (e.g. `45974302`)
4.  **Start Post Number**: Set this to the last post number you've seen (e.g. `0` to start from beginning, or a higher number to only see new ones)
5.  **Polling Interval**: How often to check for new posts (in seconds)
6.  Click **Save Settings**

## Usage

- The extension runs in the background.
- When new posts are detected, a native Chrome notification will appear.
- The notification will show the number of new posts and a snippet of the latest one.
