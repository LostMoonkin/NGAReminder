// background.js

const DEFAULT_SETTINGS = {
    apiUrl: 'http://127.0.0.1:8848',
    interval: 30,
    threads: []
};

// Initialize on install
chrome.runtime.onInstalled.addListener(() => {
    console.log('NGA Reminder Extension Installed');
    setupAlarm();
});

// Listen for settings updates
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (message.type === 'UPDATE_SETTINGS') {
        console.log('Settings updated, resetting alarm...');
        setupAlarm();
    }
});

// Setup periodic alarm
function setupAlarm() {
    chrome.storage.sync.get(['interval'], (items) => {
        const interval = items.interval || DEFAULT_SETTINGS.interval;
        chrome.alarms.create('pollApi', {
            periodInMinutes: interval / 60
        });
        console.log(`Alarm set for every ${interval} seconds`);
    });
}

// Handle alarm
chrome.alarms.onAlarm.addListener((alarm) => {
    if (alarm.name === 'pollApi') {
        pollAllThreads();
    }
});

// Poll all threads
async function pollAllThreads() {
    chrome.storage.sync.get(
        ['apiUrl', 'threads', 'tid', 'startPost', 'authorUid'],
        async (items) => {
            const apiUrl = (items.apiUrl || DEFAULT_SETTINGS.apiUrl).replace(/\/$/, '');

            // Backward compatibility / Migration fallback
            let threads = items.threads || [];
            if (threads.length === 0 && items.tid) {
                threads = [{
                    tid: items.tid,
                    startPost: items.startPost || 0,
                    authorUid: items.authorUid || ''
                }];
            }

            if (threads.length === 0) {
                console.log('No threads configured, skipping poll');
                return;
            }

            console.log(`Polling ${threads.length} threads...`);

            // Poll each thread concurrently
            const promises = threads.map((thread, index) =>
                pollSingleThread(apiUrl, thread, index)
            );

            const results = await Promise.allSettled(promises);

            // Check if any threads need updating (startPost changed)
            let needsUpdate = false;
            results.forEach((result, index) => {
                if (result.status === 'fulfilled' && result.value) {
                    // result.value is the new startPost
                    threads[index].startPost = result.value;
                    needsUpdate = true;
                }
            });

            if (needsUpdate) {
                chrome.storage.sync.set({ threads: threads });
                console.log('Updated thread states in storage');
            }
        }
    );
}

// Poll a single thread
async function pollSingleThread(apiUrl, thread, index) {
    try {
        let url = `${apiUrl}/api/v1/posts?tid=${thread.tid}&start_post_number=${thread.startPost}`;
        if (thread.authorUid) {
            url += `&author_uid=${thread.authorUid}`;
        }

        const response = await fetch(url);
        if (!response.ok) throw new Error(`Status ${response.status}`);

        const data = await response.json();
        const posts = Array.isArray(data) ? data : data.posts;

        if (posts && posts.length > 0) {
            // Handle new posts
            posts.sort((a, b) => a.post_number - b.post_number);

            const latestPost = posts[posts.length - 1];

            // Send notification
            const notificationId = `nga-${thread.tid}-${latestPost.post_number}`;
            chrome.notifications.create(notificationId, {
                type: 'basic',
                iconUrl: 'icons/icon128.png',
                title: `New Post in Thread ${thread.tid}`,
                message: `${posts.length} new post(s). Latest from UID ${latestPost.author_uid}:\n${latestPost.content.substring(0, 100)}...`,
                priority: 2
            });

            console.log(`Thread ${thread.tid}: Found ${posts.length} new posts. New start: ${latestPost.post_number}`);

            // Return new startPost to update storage
            return latestPost.post_number;
        }

        return null; // No change
    } catch (error) {
        console.error(`Error polling thread ${thread.tid}:`, error);
        return null;
    }
}

chrome.notifications.onClicked.addListener((notificationId) => {
    chrome.notifications.clear(notificationId);
});
