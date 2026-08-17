/**
 * Background Service Worker for NGA Reminder (Standalone)
 * Monitors threads and sends notifications
 */

import { NGAClient } from './nga-api.js';
import { ensureThreadIdentities } from './thread-config.mjs';

const ngaClient = new NGAClient();
const NOTIFICATION_URL_STORAGE_PREFIX = 'notificationUrl:';
const activeThreadChecks = new Set();

function notificationUrlStorageKey(notificationId) {
    return `${NOTIFICATION_URL_STORAGE_PREFIX}${notificationId}`;
}

async function storeNotificationUrl(notificationId, url) {
    await chrome.storage.local.set({
        [notificationUrlStorageKey(notificationId)]: url
    });
}

async function takeNotificationUrl(notificationId) {
    const key = notificationUrlStorageKey(notificationId);
    const stored = await chrome.storage.local.get(key);
    await chrome.storage.local.remove(key);
    return stored[key];
}

async function removeNotificationUrl(notificationId) {
    await chrome.storage.local.remove(notificationUrlStorageKey(notificationId));
}

// Check for cookies on startup
chrome.runtime.onStartup.addListener(async () => {
    setupAlarm();
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
    const threads = await loadStoredThreads();

    if (!threads || threads.length === 0) {
        return;
    }

    for (const thread of threads) {
        try {
            if (!thread?.enabled) continue;
            // A malformed imported schedule must not prevent later watches
            // from being checked in the same alarm cycle.
            if (shouldCheckThread(thread)) {
                await checkThread(thread);
            }
        } catch (error) {
            console.error(
                `[TID ${String(thread?.tid ?? 'unknown')}] Invalid schedule or check configuration:`,
                error
            );
        }
    }
}

async function loadStoredThreads() {
    return withStorageLock('nga-threads', async () => {
        const { threads = [] } = await chrome.storage.local.get(['threads']);
        const migrated = ensureThreadIdentities(threads);
        const identityChanged = migrated.some((thread, index) =>
            thread?.watchId !== threads[index]?.watchId ||
            thread?.revision !== threads[index]?.revision
        );
        if (identityChanged) {
            await chrome.storage.local.set({ threads: migrated });
        }
        return migrated;
    });
}

void setupAlarmIfMissing();

async function setupAlarmIfMissing() {
    const alarm = await chrome.alarms.get('checkThreads');
    if (!alarm) {
        setupAlarm();
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
    const checkKey = thread.watchId || String(thread.tid);
    if (activeThreadChecks.has(checkKey)) {
        return { success: false, error: `TID ${thread.tid} is already being checked` };
    }
    activeThreadChecks.add(checkKey);

    try {
        console.log(`[TID ${thread.tid}] Checking thread...`);

        // Fetch new posts (automatically handles multi-page fetching)
        const { thread: threadInfo, newPosts } = await ngaClient.fetchNewPosts(
            thread.tid,
            thread.lastSeenPostNumber || 0
        );

        // Persist a newly discovered or changed title independently of post
        // delivery so a later notification failure does not lose it.
        if (threadInfo.title && thread.title !== threadInfo.title) {
            thread.title = threadInfo.title;
            if (!await updateThreadProgress(thread, { title: thread.title })) {
                return { success: false, error: `TID ${thread.tid} configuration changed` };
            }
        }

        if (newPosts.length > 0) {
            console.log(`[TID ${thread.tid}] Found ${newPosts.length} new posts`);

            // Process in floor order and commit progress one post at a time.
            // If delivery throws, the current and all later posts remain above
            // the stored watermark and will be retried on the next check.
            const orderedPosts = [...newPosts].sort(comparePostsByFloor);
            for (const post of orderedPosts) {
                const currentThread = await getEnabledThread(thread);
                if (!currentThread) {
                    return { success: false, error: `TID ${thread.tid} is no longer enabled` };
                }
                thread = { ...thread, ...currentThread };
                const floor = postFloor(post);
                if (floor <= Number(thread.lastSeenPostNumber || 0)) {
                    continue;
                }

                await deliverPostIfNeeded(thread, post);

                thread.lastSeenPostNumber = floor;
                if (!await updateThreadProgress(thread, { lastSeenPostNumber: floor })) {
                    return { success: false, error: `TID ${thread.tid} configuration changed` };
                }
            }

            thread.lastChecked = Date.now();
            if (!await updateThreadProgress(thread, { lastChecked: thread.lastChecked })) {
                return { success: false, error: `TID ${thread.tid} configuration changed` };
            }
        } else {
            console.log(`[TID ${thread.tid}] No new posts`);
            // No new posts, just update last checked time
            thread.lastChecked = Date.now();
            if (!await updateThreadProgress(thread, { lastChecked: thread.lastChecked })) {
                return { success: false, error: `TID ${thread.tid} configuration changed` };
            }
        }
        return { success: true };
    } catch (error) {
        console.error(`[TID ${thread.tid}] Error checking thread:`, error);
        return { success: false, error: error instanceof Error ? error.message : String(error) };
    } finally {
        activeThreadChecks.delete(checkKey);
    }
}

async function getEnabledThread(expected) {
    const { threads = [] } = await chrome.storage.local.get(['threads']);
    return threads.find(candidate =>
        candidate.watchId === expected.watchId &&
        candidate.revision === expected.revision &&
        candidate.enabled === true
    ) || null;
}

function postFloor(post) {
    const floor = Number(post.post_number);
    if (!Number.isFinite(floor)) {
        throw new Error(`Invalid post floor for PID ${post.pid}`);
    }
    return floor;
}

function comparePostsByFloor(left, right) {
    const floorDifference = postFloor(left) - postFloor(right);
    if (floorDifference !== 0) {
        return floorDifference;
    }
    return Number(left.pid || 0) - Number(right.pid || 0);
}

function shouldNotifyPost(thread, post) {
    return !thread.authorNotification ||
        thread.authorNotification.length === 0 ||
        thread.authorNotification.some(uid => String(uid) === String(post.author_uid));
}

async function deliverPostIfNeeded(thread, post) {
    if (shouldNotifyPost(thread, post)) {
        await sendNotification(thread, post);
    }
}

async function sendNotification(thread, post) {
    const title = thread.title || `Thread ${thread.tid}`;
    const content = String(post.content || '').replace(/<[^>]*>/g, '').substring(0, 100); // Strip HTML tags

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
    // A retry may overlap the close event from a previous notification for the
    // same post. A per-delivery id keeps that event from deleting this URL.
    const notificationId = `nga-${thread.tid}-${post.pid}-${crypto.randomUUID()}`;
    try {
        // Store first so an immediate click can always resolve its destination.
        await storeNotificationUrl(notificationId, jumpUrl);
        await chrome.notifications.create(notificationId, {
            type: 'basic',
            iconUrl: 'icons/icon128.png',
            title: `New Post: ${title}`,
            message: `${post.author_name} (#${post.post_number}):\n${content}...`,
            priority: 2,
            requireInteraction: true
        });
    } catch (error) {
        await removeNotificationUrl(notificationId);
        throw error;
    }
}

async function checkBrowserFocus() {
    try {
        const window = await chrome.windows.getLastFocused();
        // Consider browser focused only if the window is actually focused AND not minimized
        const isFocused = window.focused === true && window.state !== 'minimized';
        console.log(`[Focus Check] Browser focused: ${isFocused}, window state: ${window.state}`);
        return isFocused;
    } catch (error) {
        console.error('[Focus Check] Error checking browser focus:', error);
        // Default to false (use Bark/system notification) on error
        return false;
    }
}

async function addUnseenPost(postData) {
    const { unseenPosts = [] } = await chrome.storage.local.get(['unseenPosts']);

    // Check if post already exists by its stable thread/post identity.
    const exists = unseenPosts.some(p => p.tid === postData.tid && p.pid === postData.pid);
    if (exists) {
        console.log(`[Unseen] Post ${postData.pid} already in unseen list`);
        return;
    }

    // Chrome storage has no compare-and-swap primitive. Serialize writers
    // across extension contexts with Web Locks when available; the fallback
    // keeps compatibility with Chrome's extension service worker runtime.
    await withStorageLock('nga-unseen-posts', async () => {
        const { unseenPosts: currentPosts = [] } = await chrome.storage.local.get(['unseenPosts']);
        const alreadyStored = currentPosts.some(post =>
            String(post.tid) === String(postData.tid) && String(post.pid) === String(postData.pid)
        );
        if (!alreadyStored) {
            currentPosts.push(postData);
            await chrome.storage.local.set({ unseenPosts: currentPosts });
        }
    });

    // Update badge
    await updateBadge();
    console.log(`[Unseen] Added post ${postData.pid}`);
}

async function withStorageLock(name, operation) {
    if (globalThis.navigator?.locks) {
        return navigator.locks.request(name, operation);
    }
    return operation();
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

        const server = new URL(serverUrl);
        if (server.protocol !== 'https:') {
            console.error('[Bark] Refusing a non-HTTPS server');
            return false;
        }
        const originPermission = `${server.origin}/*`;
        const allowed = await chrome.permissions.contains({ origins: [originPermission] });
        if (!allowed) {
            console.error(`[Bark] Host permission has not been granted for ${server.origin}`);
            return false;
        }

        // Construct Bark notification
        const title = `New Post: ${threadTitle}`;
        const body = `${post.author_name} (#${post.post_number}):\n${content}...`;

        // Add URL parameter for clickable notification
        const baseUrl = server.href.replace(/\/+$/, '');
        const barkUrl = new URL(
            `${baseUrl}/${encodeURIComponent(deviceKey)}/${encodeURIComponent(title)}/${encodeURIComponent(body)}`
        );
        barkUrl.searchParams.set('level', priority);
        barkUrl.searchParams.set('url', jumpUrl);

        // Send request to Bark
        const response = await fetch(barkUrl.href, {
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

async function updateThreadProgress(expected, updates) {
    return withStorageLock('nga-threads', async () => {
        const { threads = [] } = await chrome.storage.local.get(['threads']);
        const index = threads.findIndex(thread =>
            thread.watchId === expected.watchId &&
            thread.revision === expected.revision &&
            thread.enabled === true
        );
        if (index !== -1) {
            // Merge only worker-owned progress fields. Replacing the whole object
            // can undo a concurrent popup edit or re-enable a thread the user just
            // disabled while its request was in flight.
            threads[index] = { ...threads[index], ...updates };
            await chrome.storage.local.set({ threads });
            return true;
        }
        return false;
    });
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
        if (scheduleRuleMatches(rule, currentDay, currentTime)) {
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

function previousDay(day) {
    const days = [
        'monday',
        'tuesday',
        'wednesday',
        'thursday',
        'friday',
        'saturday',
        'sunday'
    ];
    const index = days.indexOf(day);
    return index === -1 ? day : days[(index + days.length - 1) % days.length];
}

function scheduleRuleMatches(rule, currentDay, currentTime) {
    if (!isTimeInRange(currentTime, rule.start_time, rule.end_time)) {
        return false;
    }

    if (!rule.days || rule.days.length === 0) {
        return true;
    }

    // The after-midnight portion of a wrapping range belongs to the day on
    // which that range started. For example, Tuesday 01:00 is part of a
    // Monday 22:00-06:00 rule.
    const wrapsPastMidnight = rule.start_time > rule.end_time;
    const scheduleDay = wrapsPastMidnight && currentTime < rule.end_time
        ? previousDay(currentDay)
        : currentDay;

    return expandDays(rule.days).includes(scheduleDay);
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
    void (async () => {
        try {
            const url = await takeNotificationUrl(notificationId);
            if (url) {
                await chrome.tabs.create({ url });
            }
        } catch (error) {
            console.error(`[Notification] Failed to handle click for ${notificationId}:`, error);
        } finally {
            await chrome.notifications.clear(notificationId);
        }
    })();
});

chrome.notifications.onClosed.addListener((notificationId) => {
    void removeNotificationUrl(notificationId).catch(error => {
        console.error(`[Notification] Failed to clean URL for ${notificationId}:`, error);
    });
});

// Listen for messages from popup
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (message.type === 'CHECK_COOKIES') {
        ngaClient.getCookies().then(cookies => {
            sendResponse({ cookies });
        });
        return true;  // Keep channel open for async response
    } else if (message.type === 'TEST_THREAD') {
        checkThread(message.thread).then(result => {
            sendResponse(result);
        });
        return true;
    } else if (message.type === 'UPDATE_BADGE') {
        updateBadge().then(() => {
            sendResponse({ success: true });
        });
        return true;
    }
});
