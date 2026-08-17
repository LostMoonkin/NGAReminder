/**
 * Unseen Posts Page Controller
 */

import { initI18n, t, applyTranslations } from './i18n.js';

const HTML_ESCAPE_MAP = {
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;'
};

function escapeHtml(value) {
    return String(value ?? '').replace(/[&<>"']/g, character => HTML_ESCAPE_MAP[character]);
}

function postIdentity(post) {
    return `${String(post.tid)}:${String(post.pid)}`;
}

async function withStorageLock(name, operation) {
    if (globalThis.navigator?.locks) {
        return navigator.locks.request(name, operation);
    }
    return operation();
}

// Load and display unseen posts on page load
document.addEventListener('DOMContentLoaded', async () => {
    await initI18n();
    applyTranslations();
    await loadUnseenPosts();
    setupEventListeners();
});

function setupEventListeners() {
    document.getElementById('clear-all-btn').addEventListener('click', async () => {
        if (confirm(t('clearAllConfirm'))) {
            await clearAllPosts();
        }
    });
}

async function loadUnseenPosts() {
    const { unseenPosts = [] } = await chrome.storage.local.get(['unseenPosts']);
    const postsList = document.getElementById('posts-list');
    const postsCount = document.getElementById('posts-count');

    // Update count
    const count = unseenPosts.length;
    const postsFn = t('posts');
    postsCount.textContent = typeof postsFn === 'function' ? postsFn(count) : `${count}`;

    if (unseenPosts.length === 0) {
        postsList.innerHTML = `
            <div class="empty-state">
                <div class="empty-state-icon">📭</div>
                <h2>${t('noUnseenPosts')}</h2>
                <p>${t('allCaughtUp')}</p>
            </div>
        `;
        return;
    }

    // Sort by timestamp (newest first)
    const sortedPosts = [...unseenPosts].sort((a, b) => b.timestamp - a.timestamp);

    // Display posts
    postsList.innerHTML = '';
    sortedPosts.forEach(post => {
        const postCard = createPostCard(post);
        postsList.appendChild(postCard);
    });
}

function createPostCard(post) {
    const card = document.createElement('div');
    card.className = 'post-item';

    const timestamp = new Date(post.timestamp * 1000);
    const timeStr = formatTimestamp(timestamp);
    const threadTitle = post.threadTitle || `Thread ${post.tid}`;

    card.innerHTML = `
        <div class="post-thread-title">${escapeHtml(threadTitle)}</div>
        <div class="post-meta">
            <span>👤 ${escapeHtml(post.authorName)}</span>
            <span>#${escapeHtml(post.postNumber)}</span>
            <span>🕒 ${escapeHtml(timeStr)}</span>
        </div>
        <div class="post-content">${escapeHtml(post.content)}</div>
    `;

    // Click to open and remove
    card.addEventListener('click', async () => {
        // Open the URL
        chrome.tabs.create({ url: post.url });

        // Remove from unseen list
        await removePost(post);

        // Reload the list
        await loadUnseenPosts();
    });

    return card;
}

async function removePost(post) {
    await withStorageLock('nga-unseen-posts', async () => {
        const { unseenPosts = [] } = await chrome.storage.local.get(['unseenPosts']);
        const identity = postIdentity(post);
        const remainingPosts = unseenPosts.filter(candidate => postIdentity(candidate) !== identity);
        await chrome.storage.local.set({ unseenPosts: remainingPosts });
    });

    // Update badge
    await chrome.runtime.sendMessage({ type: 'UPDATE_BADGE' });
}

async function clearAllPosts() {
    await withStorageLock('nga-unseen-posts', () =>
        chrome.storage.local.set({ unseenPosts: [] })
    );
    await chrome.runtime.sendMessage({ type: 'UPDATE_BADGE' });
    await loadUnseenPosts();
}

function formatTimestamp(date) {
    const now = new Date();
    const diffMs = now - date;
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMs / 3600000);
    const diffDays = Math.floor(diffMs / 86400000);

    if (diffMins < 1) return t('justNow');
    if (diffMins < 60) return t('minutesAgo')(diffMins);
    if (diffHours < 24) return t('hoursAgo')(diffHours);
    if (diffDays < 7) return t('daysAgo')(diffDays);

    return date.toLocaleDateString();
}
