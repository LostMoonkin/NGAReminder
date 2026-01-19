/**
 * Unseen Posts Page Controller
 */

// Load and display unseen posts on page load
document.addEventListener('DOMContentLoaded', async () => {
    await loadUnseenPosts();
    setupEventListeners();
});

function setupEventListeners() {
    document.getElementById('clear-all-btn').addEventListener('click', async () => {
        if (confirm('Clear all unseen posts?')) {
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
    postsCount.textContent = `${count} ${count === 1 ? 'post' : 'posts'}`;

    if (unseenPosts.length === 0) {
        postsList.innerHTML = `
            <div class="empty-state">
                <div class="empty-state-icon">📭</div>
                <h2>No Unseen Posts</h2>
                <p>You're all caught up!</p>
            </div>
        `;
        return;
    }

    // Sort by timestamp (newest first)
    unseenPosts.sort((a, b) => b.timestamp - a.timestamp);

    // Display posts
    postsList.innerHTML = '';
    unseenPosts.forEach((post, index) => {
        const postCard = createPostCard(post, index);
        postsList.appendChild(postCard);
    });
}

function createPostCard(post, index) {
    const card = document.createElement('div');
    card.className = 'post-item';
    card.dataset.index = index;

    const timestamp = new Date(post.timestamp * 1000);
    const timeStr = formatTimestamp(timestamp);

    card.innerHTML = `
        <div class="post-thread-title">${post.threadTitle || `Thread ${post.tid}`}</div>
        <div class="post-meta">
            <span>👤 ${post.authorName}</span>
            <span>#${post.postNumber}</span>
            <span>🕒 ${timeStr}</span>
        </div>
        <div class="post-content">${post.content}</div>
    `;

    // Click to open and remove
    card.addEventListener('click', async () => {
        // Open the URL
        chrome.tabs.create({ url: post.url });

        // Remove from unseen list
        await removePost(index);

        // Reload the list
        await loadUnseenPosts();
    });

    return card;
}

async function removePost(index) {
    const { unseenPosts = [] } = await chrome.storage.local.get(['unseenPosts']);
    unseenPosts.splice(index, 1);
    await chrome.storage.local.set({ unseenPosts });

    // Update badge
    await chrome.runtime.sendMessage({ type: 'UPDATE_BADGE' });
}

async function clearAllPosts() {
    await chrome.storage.local.set({ unseenPosts: [] });
    await chrome.runtime.sendMessage({ type: 'UPDATE_BADGE' });
    await loadUnseenPosts();
}

function formatTimestamp(date) {
    const now = new Date();
    const diffMs = now - date;
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMs / 3600000);
    const diffDays = Math.floor(diffMs / 86400000);

    if (diffMins < 1) return 'Just now';
    if (diffMins < 60) return `${diffMins}m ago`;
    if (diffHours < 24) return `${diffHours}h ago`;
    if (diffDays < 7) return `${diffDays}d ago`;

    return date.toLocaleDateString();
}
