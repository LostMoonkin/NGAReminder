/**
 * Background Service Worker for NGA Reminder (Standalone)
 * Monitors threads and sends notifications
 */

import { NGAClient } from './nga-api.js';

const ngaClient = new NGAClient();
// Map to store notification URLs for click handling
const notificationUrlMap = new Map();

// Check for cookies on startup
chrome.runtime.onStartup.addListener(async () => {
    const cookies = await ngaClient.getCookies();
    if (!cookies.uid || !cookies.cid) {
        console.warn('NGA cookies not found');
    } else {
        console.log(`NGA authentication ready (UID: ${cookies.uid})`);
    }
});

// Also check on install
chrome.runtime.onInstalled.addListener(async () => {
    console.log('NGA Reminder (Standalone) installed');
    setupAlarm();
});

// Setup monitoring alarm
function setupAlarm() {
    chrome.alarms.create('checkThreads', {
        periodInMinutes: 1  // Check every minute, apply per-thread intervals
    });
    console.log('Monitoring alarm set (every 1 minute)');
}

chrome.alarms.onAlarm.addListener(async (alarm) => {
    if (alarm.name === 'checkThreads') {
        await checkAllThreads();
    }
});

async function checkAllThreads() {
    const { threads } = await chrome.storage.local.get(['threads']);

    if (!threads || threads.length === 0) {
        return;
    }

    for (const thread of threads) {
        if (!thread.enabled) continue;

        // Check if enough time has passed for this thread
        if (shouldCheckThread(thread)) {
            await checkThread(thread);
        }
    }
}

function shouldCheckThread(thread) {
    // Calculate current interval (simple or time-based)
    const currentInterval = getCurrentCheckInterval(
        thread.checkInterval,
        thread.checkIntervalSchedule
    );

    const now = Date.now();
    const lastCheck = thread.lastChecked || 0;
    const timeSinceCheck = (now - lastCheck) / 1000;  // seconds

    return timeSinceCheck >= currentInterval;
}

async function checkThread(thread) {
    try {
        console.log(`[TID ${thread.tid}] Checking thread...`);

        // Fetch new posts (automatically handles multi-page fetching)
        const { thread: threadInfo, newPosts } = await ngaClient.fetchNewPosts(
            thread.tid,
            thread.lastSeenPostNumber || 0
        );

        // Update cached thread title if needed
        if (!thread.title && threadInfo.title) {
            thread.title = threadInfo.title;
        }

        if (newPosts.length > 0) {
            console.log(`[TID ${thread.tid}] Found ${newPosts.length} new posts`);

            // Update last seen post number
            thread.lastSeenPostNumber = Math.max(...newPosts.map(p => p.post_number));
            thread.lastChecked = Date.now();
            await updateThread(thread);

            // Send notifications for posts matching authorNotification
            for (const post of newPosts) {
                // If no filter specified, notify for all; otherwise check if author matches
                if (!thread.authorNotification ||
                    thread.authorNotification.length === 0 ||
                    thread.authorNotification.includes(post.author_uid)) {
                    await sendNotification(thread, post);
                }
            }
        } else {
            console.log(`[TID ${thread.tid}] No new posts`);
            // No new posts, just update last checked time
            thread.lastChecked = Date.now();
            await updateThread(thread);
        }
    } catch (error) {
        console.error(`[TID ${thread.tid}] Error checking thread:`, error);
    }
}

async function sendNotification(thread, post) {
    const title = thread.title || `Thread ${thread.tid}`;
    const content = post.content.replace(/<[^>]*>/g, '').substring(0, 100); // Strip HTML tags

    // Use page number from post object
    const jumpUrl = `https://nga.178.com/read.php?tid=${thread.tid}&page=${post.page}#pid${post.pid}Anchor`;

    // Add to unseen posts
    await addUnseenPost({
        tid: thread.tid,
        pid: post.pid,
        threadTitle: title,
        authorName: post.author_name,
        postNumber: post.post_number,
        content: content,
        timestamp: post.post_timestamp,
        page: post.page,
        url: jumpUrl
    });

    // Check if browser is focused
    const isBrowserFocused = await checkBrowserFocus();
    console.log(`[Notification] TID ${thread.tid}, Post #${post.post_number}, Browser focused: ${isBrowserFocused}`);

    if (!isBrowserFocused) {
        // Browser not focused, try Bark notification
        const barkSent = await sendBarkNotification(thread, post, title, content, jumpUrl);
        if (barkSent) {
            console.log(`[Bark] Sent notification for TID ${thread.tid}, Post #${post.post_number}`);
            return;
        }
        console.log(`[Bark] Failed or not configured, falling back to Chrome notification`);
        // If Bark fails or not configured, fall through to Chrome notification
    }

    // Browser is focused or Bark not available, use Chrome notification
    console.log(`[Chrome] Creating notification for TID ${thread.tid}, Post #${post.post_number}`);
    const notificationId = `nga-${thread.tid}-${post.pid}`;
    chrome.notifications.create(notificationId, {
        type: 'basic',
        iconUrl: 'icons/icon128.png',
        title: `New Post: ${title}`,
        message: `${post.author_name} (#${post.post_number}):\n${content}...`,
        priority: 2,
        requireInteraction: true
    });

    // Store URL for click handler
    notificationUrlMap.set(notificationId, jumpUrl);
}

async function checkBrowserFocus() {
    try {
        const window = await chrome.windows.getLastFocused();
        const isFocused = window.focused === true;
        console.log(`[Focus Check] Browser focused: ${isFocused}, window state: ${window.state}`);
        return isFocused;
    } catch (error) {
        console.error('[Focus Check] Error checking browser focus:', error);
        // Default to true (show Chrome notification) on error
        return true;
    }
}

async function addUnseenPost(postData) {
    const { unseenPosts = [] } = await chrome.storage.local.get(['unseenPosts']);

    // Check if post already exists (by pid)
    const exists = unseenPosts.some(p => p.pid === postData.pid);
    if (exists) {
        console.log(`[Unseen] Post ${postData.pid} already in unseen list`);
        return;
    }

    // Add new post
    unseenPosts.push(postData);
    await chrome.storage.local.set({ unseenPosts });

    // Update badge
    await updateBadge();
    console.log(`[Unseen] Added post ${postData.pid}, total unseen: ${unseenPosts.length}`);
}

async function updateBadge() {
    const { unseenPosts = [] } = await chrome.storage.local.get(['unseenPosts']);
    const count = unseenPosts.length;

    if (count > 0) {
        const badgeText = count > 99 ? '99+' : count.toString();
        chrome.action.setBadgeText({ text: badgeText });
        chrome.action.setBadgeBackgroundColor({ color: '#E74C3C' });
    } else {
        chrome.action.setBadgeText({ text: '' });
    }
}

async function sendBarkNotification(thread, post, threadTitle, content, jumpUrl) {
    try {
        // Get Bark configuration
        const { barkConfig } = await chrome.storage.local.get(['barkConfig']);

        if (!barkConfig || !barkConfig.deviceKey) {
            console.log('[Bark] Not configured, skipping');
            return false;
        }

        const serverUrl = barkConfig.serverUrl || 'https://api.day.app';
        const deviceKey = barkConfig.deviceKey;
        const priority = barkConfig.priority || 'active';

        // Construct Bark notification
        const title = `New Post: ${threadTitle}`;
        const body = `${post.author_name} (#${post.post_number}):\n${content}...`;

        // Add URL parameter for clickable notification
        const barkUrl = `${serverUrl}/${deviceKey}/${encodeURIComponent(title)}/${encodeURIComponent(body)}?level=${priority}&url=${encodeURIComponent(jumpUrl)}`;

        // Send request to Bark
        const response = await fetch(barkUrl, {
            method: 'GET'
        });

        if (response.ok) {
            return true;
        } else {
            console.error('[Bark] Failed to send notification:', response.status);
            return false;
        }
    } catch (error) {
        console.error('[Bark] Error sending notification:', error);
        return false;
    }
}

async function updateThread(thread) {
    const { threads } = await chrome.storage.local.get(['threads']);
    const index = threads.findIndex(t => t.tid === thread.tid);
    if (index !== -1) {
        threads[index] = thread;
        await chrome.storage.local.set({ threads });
    }
}

// Helper function for time-based intervals (reuse from server logic)
function getCurrentCheckInterval(baseInterval, schedule) {
    if (!schedule || schedule.length === 0) {
        return baseInterval;
    }

    const now = new Date();
    const currentDay = now.toLocaleDateString('en-US', { weekday: 'long' }).toLowerCase();
    const currentTime = now.toTimeString().substring(0, 5);  // HH:MM format

    for (const rule of schedule) {
        // Check day match
        if (rule.days && rule.days.length > 0) {
            const expandedDays = expandDays(rule.days);
            if (!expandedDays.includes(currentDay)) {
                continue;
            }
        }

        // Check time match
        if (isTimeInRange(currentTime, rule.start_time, rule.end_time)) {
            return rule.interval;
        }
    }

    return baseInterval;  // Fallback
}

function expandDays(days) {
    const weekdays = ['monday', 'tuesday', 'wednesday', 'thursday', 'friday'];
    const weekends = ['saturday', 'sunday'];

    const expanded = [];
    for (const day of days) {
        const dayLower = day.toLowerCase();
        if (dayLower === 'weekdays' || dayLower === 'weekday') {
            expanded.push(...weekdays);
        } else if (dayLower === 'weekends' || dayLower === 'weekend') {
            expanded.push(...weekends);
        } else {
            expanded.push(dayLower);
        }
    }

    return [...new Set(expanded)];  // Remove duplicates
}

function isTimeInRange(currentTime, startTime, endTime) {
    if (startTime <= endTime) {
        // Normal range (e.g., 09:00 to 18:00)
        return currentTime >= startTime && currentTime < endTime;
    } else {
        // Wrap-around (e.g., 22:00 to 06:00)
        return currentTime >= startTime || currentTime < endTime;
    }
}

chrome.notifications.onClicked.addListener((notificationId) => {
    // Open the associated URL if available
    const url = notificationUrlMap.get(notificationId);
    if (url) {
        chrome.tabs.create({ url });
        notificationUrlMap.delete(notificationId);
    }
    chrome.notifications.clear(notificationId);
});

// Listen for messages from popup
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (message.type === 'CHECK_COOKIES') {
        ngaClient.getCookies().then(cookies => {
            sendResponse({ cookies });
        });
        return true;  // Keep channel open for async response
    } else if (message.type === 'TEST_THREAD') {
        checkThread(message.thread).then(() => {
            sendResponse({ success: true });
        }).catch(error => {
            sendResponse({ success: false, error: error.message });
        });
        return true;
    } else if (message.type === 'UPDATE_BADGE') {
        updateBadge().then(() => {
            sendResponse({ success: true });
        });
        return true;
    }
});
