/**
 * Popup UI Controller
 */

let currentEditIndex = null;

// Initialize on load
document.addEventListener('DOMContentLoaded', async () => {
    await checkCookieStatus();
    await loadBarkSettings();
    await loadUnseenCount();
    await loadThreads();
    setupEventListeners();
});

async function checkCookieStatus() {
    const statusBox = document.getElementById('cookie-status');
    const loginBtn = document.getElementById('login-btn');

    try {
        const response = await chrome.runtime.sendMessage({ type: 'CHECK_COOKIES' });
        const { cookies } = response;

        if (cookies.uid && cookies.cid) {
            statusBox.className = 'status-box status-ok';
            statusBox.innerHTML = `✓ Logged in as UID ${cookies.uid}`;
            loginBtn.style.display = 'none';
        } else {
            statusBox.className = 'status-box status-error';
            statusBox.innerHTML = '✗ Not logged in to NGA';
            loginBtn.style.display = 'block';
        }
    } catch (error) {
        statusBox.className = 'status-box status-error';
        statusBox.innerHTML = '✗ Error checking cookies';
    }
}

async function loadUnseenCount() {
    const { unseenPosts = [] } = await chrome.storage.local.get(['unseenPosts']);
    document.getElementById('unseen-count').textContent = unseenPosts.length;
}

async function loadBarkSettings() {
    const { barkConfig } = await chrome.storage.local.get(['barkConfig']);

    if (barkConfig) {
        document.getElementById('bark-server').value = barkConfig.serverUrl || 'https://api.day.app';
        document.getElementById('bark-device-key').value = barkConfig.deviceKey || '';
        document.getElementById('bark-priority').value = barkConfig.priority || 'active';
    } else {
        document.getElementById('bark-server').value = 'https://api.day.app';
    }
}

async function saveBarkSettings() {
    const serverUrl = document.getElementById('bark-server').value.trim();
    const deviceKey = document.getElementById('bark-device-key').value.trim();
    const priority = document.getElementById('bark-priority').value;

    const barkConfig = {
        serverUrl: serverUrl || 'https://api.day.app',
        deviceKey: deviceKey,
        priority: priority
    };

    await chrome.storage.local.set({ barkConfig });

    // Show feedback
    const saveBtn = document.getElementById('save-bark-btn');
    const originalText = saveBtn.textContent;
    saveBtn.textContent = '✓ Saved';
    saveBtn.disabled = true;

    setTimeout(() => {
        saveBtn.textContent = originalText;
        saveBtn.disabled = false;
    }, 2000);
}

async function loadThreads() {
    const { threads } = await chrome.storage.local.get(['threads']);
    const threadsList = document.getElementById('threads-list');

    if (!threads || threads.length === 0) {
        threadsList.innerHTML = '<p class="empty-state">No threads configured yet</p>';
        return;
    }

    threadsList.innerHTML = '';
    threads.forEach((thread, index) => {
        const threadCard = createThreadCard(thread, index);
        threadsList.appendChild(threadCard);
    });
}

function createThreadCard(thread, index) {
    const card = document.createElement('div');
    card.className = 'thread-card';

    const interval = thread.checkIntervalSchedule ? 'Dynamic' : `${thread.checkInterval}s`;
    const lastSeen = thread.lastSeenPostNumber ? `#${thread.lastSeenPostNumber}` : 'Not checked';

    card.innerHTML = `
        <div class="thread-header">
            <span class="thread-title">${thread.title || `Thread ${thread.tid}`}</span>
            <label class="toggle">
                <input type="checkbox" class="thread-toggle" data-index="${index}" ${thread.enabled ? 'checked' : ''}>
                <span class="slider"></span>
            </label>
        </div>
        <div class="thread-details">
            <p><strong>TID:</strong> ${thread.tid}</p>
            <p><strong>Interval:</strong> ${interval}</p>
            <p><strong>Last Seen:</strong> ${lastSeen}</p>
            ${thread.authorNotification && thread.authorNotification.length > 0 ?
            `<p><strong>Notify UIDs:</strong> ${thread.authorNotification.join(', ')}</p>` : ''}
        </div>
        <div class="thread-actions">
            <button class="btn btn-small btn-edit" data-index="${index}">Edit</button>
            <button class="btn btn-small btn-danger btn-delete" data-index="${index}">Delete</button>
            <button class="btn btn-small btn-test" data-index="${index}">Test</button>
        </div>
    `;

    return card;
}

function setupEventListeners() {
    document.getElementById('login-btn').addEventListener('click', () => {
        chrome.tabs.create({ url: 'https://bbs.nga.cn/' });
    });

    document.getElementById('view-posts-btn').addEventListener('click', () => {
        chrome.tabs.create({ url: chrome.runtime.getURL('posts.html') });
    });

    document.getElementById('save-bark-btn').addEventListener('click', async () => {
        await saveBarkSettings();
    });

    document.getElementById('add-thread-btn').addEventListener('click', () => {
        showThreadForm();
    });

    document.getElementById('cancel-btn').addEventListener('click', () => {
        hideThreadForm();
    });

    document.getElementById('use-schedule').addEventListener('change', (e) => {
        document.getElementById('schedule-config').style.display = e.target.checked ? 'block' : 'none';
    });

    document.getElementById('thread-config-form').addEventListener('submit', async (e) => {
        e.preventDefault();
        await saveThread();
    });

    // Export/Import config
    document.getElementById('export-config-btn').addEventListener('click', async () => {
        await exportConfig();
    });

    document.getElementById('import-config-btn').addEventListener('click', () => {
        document.getElementById('import-file-input').click();
    });

    document.getElementById('import-file-input').addEventListener('change', async (e) => {
        const file = e.target.files[0];
        if (file) {
            await importConfig(file);
            e.target.value = ''; // Reset file input
        }
    });

    // Event delegation for thread card buttons
    document.getElementById('threads-list').addEventListener('click', async (e) => {
        const target = e.target;

        // Toggle checkbox
        if (target.classList.contains('thread-toggle')) {
            const index = parseInt(target.dataset.index);
            await toggleThread(index, target.checked);
        }

        // Edit button
        if (target.classList.contains('btn-edit')) {
            const index = parseInt(target.dataset.index);
            await editThread(index);
        }

        // Delete button
        if (target.classList.contains('btn-delete')) {
            const index = parseInt(target.dataset.index);
            await deleteThread(index);
        }

        // Test button
        if (target.classList.contains('btn-test')) {
            const index = parseInt(target.dataset.index);
            await testThread(index, target);
        }
    });
}

function showThreadForm(thread = null, index = null) {
    currentEditIndex = index;
    const form = document.getElementById('thread-form');
    const formTitle = document.getElementById('form-title');

    if (thread) {
        formTitle.textContent = 'Edit Thread';
        document.getElementById('tid').value = thread.tid;
        document.getElementById('author-notification').value =
            thread.authorNotification ? thread.authorNotification.join(',') : '';
        document.getElementById('last-seen').value = thread.lastSeenPostNumber || 0;
        document.getElementById('check-interval').value = thread.checkInterval || 300;

        if (thread.checkIntervalSchedule) {
            document.getElementById('use-schedule').checked = true;
            document.getElementById('schedule-config').style.display = 'block';
            document.getElementById('schedule-json').value =
                JSON.stringify(thread.checkIntervalSchedule, null, 2);
        }
    } else {
        formTitle.textContent = 'Add Thread';
        document.getElementById('thread-config-form').reset();
        document.getElementById('schedule-config').style.display = 'none';
    }

    form.style.display = 'block';
    document.querySelector('.threads-list').parentElement.style.display = 'none';
}

function hideThreadForm() {
    document.getElementById('thread-form').style.display = 'none';
    document.querySelector('.threads-list').parentElement.style.display = 'block';
    document.getElementById('thread-config-form').reset();
    currentEditIndex = null;
}

async function saveThread() {
    const tid = parseInt(document.getElementById('tid').value);
    const authorNotificationStr = document.getElementById('author-notification').value.trim();
    const lastSeenInput = document.getElementById('last-seen').value.trim();
    const lastSeenPostNumber = lastSeenInput ? parseInt(lastSeenInput) : 0;
    const checkInterval = parseInt(document.getElementById('check-interval').value);
    const useSchedule = document.getElementById('use-schedule').checked;

    // Parse author notification
    const authorNotification = authorNotificationStr
        ? authorNotificationStr.split(',').map(uid => parseInt(uid.trim())).filter(uid => !isNaN(uid))
        : [];

    // Parse schedule
    let checkIntervalSchedule = null;
    if (useSchedule) {
        try {
            const scheduleJson = document.getElementById('schedule-json').value.trim();
            if (scheduleJson) {
                checkIntervalSchedule = JSON.parse(scheduleJson);
                if (!Array.isArray(checkIntervalSchedule)) {
                    checkIntervalSchedule = [checkIntervalSchedule];
                }
            }
        } catch (error) {
            alert('Invalid schedule JSON format: ' + error.message);
            return;
        }
    }

    const thread = {
        tid,
        title: null,  // Will be fetched on first check
        lastSeenPostNumber,
        authorNotification,
        checkInterval,
        checkIntervalSchedule,
        enabled: true,
        lastChecked: 0
    };

    const { threads = [] } = await chrome.storage.local.get(['threads']);

    if (currentEditIndex !== null) {
        // Preserve some existing data when editing
        thread.title = threads[currentEditIndex].title;
        // Only preserve lastSeenPostNumber if user didn't explicitly change it
        if (!lastSeenInput && threads[currentEditIndex].lastSeenPostNumber) {
            thread.lastSeenPostNumber = threads[currentEditIndex].lastSeenPostNumber;
        }
        threads[currentEditIndex] = thread;
    } else {
        threads.push(thread);
    }

    await chrome.storage.local.set({ threads });
    hideThreadForm();
    await loadThreads();
}

async function toggleThread(index, enabled) {
    const { threads } = await chrome.storage.local.get(['threads']);
    threads[index].enabled = enabled;
    await chrome.storage.local.set({ threads });
}

async function editThread(index) {
    const { threads } = await chrome.storage.local.get(['threads']);
    showThreadForm(threads[index], index);
}

async function deleteThread(index) {
    if (!confirm('Are you sure you want to delete this thread?')) {
        return;
    }

    const { threads } = await chrome.storage.local.get(['threads']);
    threads.splice(index, 1);
    await chrome.storage.local.set({ threads });
    await loadThreads();
}

async function testThread(index, btn) {
    const { threads } = await chrome.storage.local.get(['threads']);
    const thread = threads[index];

    btn.textContent = 'Testing...';
    btn.disabled = true;

    try {
        const response = await chrome.runtime.sendMessage({
            type: 'TEST_THREAD',
            thread
        });

        if (response.success) {
            alert('Test completed! Check console for details.');
        } else {
            alert('Test failed: ' + response.error);
        }
    } catch (error) {
        alert('Test error: ' + error.message);
    } finally {
        btn.textContent = 'Test';
        btn.disabled = false;
    }
}

async function exportConfig() {
    try {
        // Get all configuration data
        const { threads, barkConfig } = await chrome.storage.local.get(['threads', 'barkConfig']);

        const config = {
            version: '1.0',
            exportDate: new Date().toISOString(),
            threads: threads || [],
            barkConfig: barkConfig || null
        };

        // Create downloadable file
        const blob = new Blob([JSON.stringify(config, null, 2)], { type: 'application/json' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `nga-reminder-config-${new Date().toISOString().split('T')[0]}.json`;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        URL.revokeObjectURL(url);

        alert('Configuration exported successfully!');
    } catch (error) {
        alert('Error exporting configuration: ' + error.message);
    }
}

async function importConfig(file) {
    try {
        const text = await file.text();
        const config = JSON.parse(text);

        // Validate config structure
        if (!config.version || !Array.isArray(config.threads)) {
            throw new Error('Invalid configuration file format');
        }

        // Confirm before importing
        const threadCount = config.threads.length;
        const message = `Import configuration with ${threadCount} thread(s)?\n\nThis will replace your current configuration.`;

        if (!confirm(message)) {
            return;
        }

        // Import data
        const importData = {
            threads: config.threads
        };

        if (config.barkConfig) {
            importData.barkConfig = config.barkConfig;
        }

        await chrome.storage.local.set(importData);

        // Reload UI
        await loadBarkSettings();
        await loadThreads();
        await loadUnseenCount();

        alert(`Configuration imported successfully!\n${threadCount} thread(s) loaded.`);
    } catch (error) {
        alert('Error importing configuration: ' + error.message);
    }
}
