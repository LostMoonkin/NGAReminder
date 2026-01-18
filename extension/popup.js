document.addEventListener('DOMContentLoaded', () => {
    let threads = [];

    // DOM Elements
    const apiUrlInput = document.getElementById('apiUrl');
    const intervalInput = document.getElementById('interval');
    const threadListDiv = document.getElementById('threadList');
    const addThreadBtn = document.getElementById('addThreadBtn');
    const addThreadForm = document.getElementById('addThreadForm');
    const confirmAddBtn = document.getElementById('confirmAddBtn');
    const cancelAddBtn = document.getElementById('cancelAddBtn');
    const saveBtn = document.getElementById('saveBtn');
    const statusDiv = document.getElementById('status');

    // Load settings
    chrome.storage.sync.get(
        ['apiUrl', 'interval', 'threads', 'tid', 'startPost', 'authorUid'],
        (items) => {
            apiUrlInput.value = items.apiUrl || 'http://127.0.0.1:8848';
            intervalInput.value = items.interval || 30;

            // Migration: If 'threads' doesn't exist but old single-thread config does
            if (!items.threads && items.tid) {
                threads = [{
                    tid: items.tid,
                    startPost: items.startPost || 0,
                    authorUid: items.authorUid || ''
                }];
                // Clear old keys
                chrome.storage.sync.remove(['tid', 'startPost', 'authorUid']);
            } else {
                threads = items.threads || [];
            }

            renderThreads();
        }
    );

    // Render Thread List
    function renderThreads() {
        threadListDiv.innerHTML = '';

        if (threads.length === 0) {
            threadListDiv.innerHTML = '<div style="color:#888; text-align:center; padding:10px;">No threads monitored</div>';
            return;
        }

        threads.forEach((thread, index) => {
            const item = document.createElement('div');
            item.className = 'thread-item';

            const info = document.createElement('div');
            info.className = 'thread-info';

            const title = document.createElement('div');
            title.className = 'thread-tid';
            title.textContent = `TID: ${thread.tid}`;

            const details = document.createElement('div');
            details.className = 'thread-details';
            details.textContent = `Start: ${thread.startPost}` + (thread.authorUid ? `, UID: ${thread.authorUid}` : '');

            info.appendChild(title);
            info.appendChild(details);

            const delBtn = document.createElement('button');
            delBtn.className = 'delete-btn';
            delBtn.innerHTML = '&times;';
            delBtn.onclick = () => removeThread(index);

            item.appendChild(info);
            item.appendChild(delBtn);
            threadListDiv.appendChild(item);
        });
    }

    // Add Thread UI Toggle
    addThreadBtn.addEventListener('click', () => {
        addThreadBtn.classList.add('hidden');
        addThreadForm.classList.remove('hidden');
    });

    cancelAddBtn.addEventListener('click', () => {
        addThreadForm.classList.add('hidden');
        addThreadBtn.classList.remove('hidden');
        clearAddForm();
    });

    // Add Thread Logic
    confirmAddBtn.addEventListener('click', () => {
        const tid = document.getElementById('newTid').value;
        const startPost = document.getElementById('newStartPost').value;
        const authorUid = document.getElementById('newAuthorUid').value;

        if (!tid) {
            alert('Thread ID is required');
            return;
        }

        // Check duplicate
        if (threads.some(t => t.tid === tid)) {
            alert('This Thread ID is already in the list');
            return;
        }

        threads.push({
            tid: tid,
            startPost: parseInt(startPost) || 0,
            authorUid: authorUid
        });

        renderThreads();
        clearAddForm();
        addThreadForm.classList.add('hidden');
        addThreadBtn.classList.remove('hidden');
    });

    function removeThread(index) {
        threads.splice(index, 1);
        renderThreads();
    }

    function clearAddForm() {
        document.getElementById('newTid').value = '';
        document.getElementById('newStartPost').value = '0';
        document.getElementById('newAuthorUid').value = '';
    }

    // Save Settings
    saveBtn.addEventListener('click', () => {
        const settings = {
            apiUrl: apiUrlInput.value.replace(/\/$/, ''),
            interval: parseInt(intervalInput.value),
            threads: threads
        };

        chrome.storage.sync.set(settings, () => {
            statusDiv.textContent = 'Settings saved!';
            statusDiv.className = 'status success';

            chrome.runtime.sendMessage({ type: 'UPDATE_SETTINGS' });

            setTimeout(() => {
                statusDiv.textContent = '';
                statusDiv.className = 'status';
            }, 2000);
        });
    });

    // Test Connection
    document.getElementById('testBtn').addEventListener('click', async () => {
        const status = document.getElementById('status');
        const apiUrl = apiUrlInput.value.replace(/\/$/, '');

        status.textContent = 'Testing connection...';
        status.className = 'status';

        try {
            const response = await fetch(`${apiUrl}/health`);
            if (response.ok) {
                status.textContent = 'Connection successful!';
                status.className = 'status success';
            } else {
                status.textContent = `Error: Server returned ${response.status}`;
                status.className = 'status error';
            }
        } catch (error) {
            status.textContent = 'Connection failed!';
            status.className = 'status error';
        }
    });
});
