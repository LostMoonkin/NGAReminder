/**
 * Internationalization (i18n) module for NGA Reminder Standalone Extension
 */

export const TRANSLATIONS = {
    en: {
        // Header
        subtitle: 'Standalone Edition',
        tabSettings: '⚙️ Settings',
        tabUnseen: '📬 Unseen',

        // Language
        languageLabel: '🌐 Language',
        langEn: 'English',
        langZh: '中文',

        // Cookie status
        checkingCookies: 'Checking cookies...',
        loggedInAs: '✓ Logged in as UID',
        notLoggedIn: '✗ Not logged in to NGA',
        errorCheckingCookies: '✗ Error checking cookies',
        openNgaLogin: 'Open NGA to Login',

        // Bark Settings
        barkSettings: '🔕 Bark Settings',
        barkSave: 'Save',
        barkSaved: '✓ Saved',
        barkInfo: 'Configure Bark to receive notifications when browser is unfocused',
        barkServerLabel: 'Bark Server URL',
        barkServerPlaceholder: 'https://api.day.app',
        barkDeviceKeyLabel: 'Device Key *',
        barkDeviceKeyPlaceholder: 'Your Bark device key',
        barkDeviceKeyHint: 'Required for Bark notifications. Leave empty to disable.',
        barkPriorityLabel: 'Priority',
        barkPriorityActive: 'Active (default)',
        barkPriorityTimeSensitive: 'Time Sensitive',
        barkPriorityPassive: 'Passive',

        // Monitored Threads
        monitoredThreads: 'Monitored Threads',
        addThread: '+ Add',
        noThreads: 'No threads configured yet',

        // Thread Form
        addThreadTitle: 'Add Thread',
        editThreadTitle: 'Edit Thread',
        tidLabel: 'Thread ID (TID) *',
        tidPlaceholder: 'e.g., 45974302',
        authorNotificationLabel: 'Author UIDs to Notify (comma-separated)',
        authorNotificationPlaceholder: 'e.g., 150058,123456 (leave empty for all)',
        authorNotificationHint: 'Only send notifications for posts from these authors',
        lastSeenLabel: 'Start Monitoring From Post # (Optional)',
        lastSeenPlaceholder: '0 (start from beginning)',
        lastSeenHint: 'Set initial post number to skip earlier posts',
        checkIntervalLabel: 'Check Interval (seconds)',
        checkIntervalHint: 'How often to check for new posts (minimum 60 seconds)',
        useScheduleLabel: 'Use Time-based Schedule',
        scheduleHint: 'JSON array format. See examples in README.',
        saveBtn: 'Save',
        cancelBtn: 'Cancel',

        // Thread Card
        interval: 'Interval',
        lastSeen: 'Last Seen',
        dynamic: 'Dynamic',
        notChecked: 'Not checked',
        notifyUIDs: 'Notify UIDs',
        editBtn: 'Edit',
        deleteBtn: 'Delete',
        testBtn: 'Test',
        testingBtn: 'Testing...',

        // Config Management
        exportConfig: '📥 Export Config',
        importConfig: '📤 Import Config',

        // Alerts / Confirms
        deleteConfirm: 'Are you sure you want to delete this thread?',
        testSuccess: 'Test completed! Check console for details.',
        testFailed: 'Test failed: ',
        testError: 'Test error: ',
        exportSuccess: 'Configuration exported successfully!',
        exportError: 'Error exporting configuration: ',
        importInvalidFormat: 'Invalid configuration file format',
        importConfirm: (count) => `Import configuration with ${count} thread(s)?\n\nThis will replace your current configuration.`,
        importSuccess: (count) => `Configuration imported successfully!\n${count} thread(s) loaded.`,
        importError: 'Error importing configuration: ',
        invalidScheduleJson: 'Invalid schedule JSON format: ',
        clearAllConfirm: 'Clear all unseen posts?',

        // Unseen Posts
        unseenPosts: 'Unseen Posts',
        clearAll: 'Clear All',
        noUnseenPosts: 'No Unseen Posts',
        allCaughtUp: "You're all caught up!",
        posts: (count) => `${count} ${count === 1 ? 'post' : 'posts'}`,

        // Timestamp
        justNow: 'Just now',
        minutesAgo: (n) => `${n}m ago`,
        hoursAgo: (n) => `${n}h ago`,
        daysAgo: (n) => `${n}d ago`,

        // Footer
        footer: 'v1.0.0 | No server required',
    },

    zh: {
        // Header
        subtitle: '独立版',
        tabSettings: '⚙️ 设置',
        tabUnseen: '📬 未读',

        // Language
        languageLabel: '🌐 语言',
        langEn: 'English',
        langZh: '中文',

        // Cookie status
        checkingCookies: '检查登录状态...',
        loggedInAs: '✓ 已登录，UID',
        notLoggedIn: '✗ 未登录 NGA',
        errorCheckingCookies: '✗ 检查登录状态出错',
        openNgaLogin: '打开 NGA 登录',

        // Bark Settings
        barkSettings: '🔕 Bark 推送设置',
        barkSave: '保存',
        barkSaved: '✓ 已保存',
        barkInfo: '配置 Bark 以在浏览器未聚焦时接收推送通知',
        barkServerLabel: 'Bark 服务器地址',
        barkServerPlaceholder: 'https://api.day.app',
        barkDeviceKeyLabel: '设备密钥 *',
        barkDeviceKeyPlaceholder: '您的 Bark 设备密钥',
        barkDeviceKeyHint: '接收 Bark 推送必填，留空则禁用。',
        barkPriorityLabel: '推送优先级',
        barkPriorityActive: '活跃（默认）',
        barkPriorityTimeSensitive: '时效性',
        barkPriorityPassive: '被动',

        // Monitored Threads
        monitoredThreads: '监控主题',
        addThread: '+ 添加',
        noThreads: '尚未配置任何主题',

        // Thread Form
        addThreadTitle: '添加主题',
        editThreadTitle: '编辑主题',
        tidLabel: '主题 ID（TID）*',
        tidPlaceholder: '例如：45974302',
        authorNotificationLabel: '要通知的作者 UID（逗号分隔）',
        authorNotificationPlaceholder: '例如：150058,123456（留空则通知所有人）',
        authorNotificationHint: '仅在这些作者发帖时发送通知',
        lastSeenLabel: '从第 # 楼开始监控（可选）',
        lastSeenPlaceholder: '0（从头开始）',
        lastSeenHint: '设置初始楼层以跳过之前的帖子',
        checkIntervalLabel: '检查间隔（秒）',
        checkIntervalHint: '检查新帖的频率（最少 60 秒）',
        useScheduleLabel: '使用时间表',
        scheduleHint: 'JSON 数组格式，示例请参阅 README。',
        saveBtn: '保存',
        cancelBtn: '取消',

        // Thread Card
        interval: '间隔',
        lastSeen: '最后读到',
        dynamic: '动态',
        notChecked: '未检查',
        notifyUIDs: '通知 UID',
        editBtn: '编辑',
        deleteBtn: '删除',
        testBtn: '测试',
        testingBtn: '测试中...',

        // Config Management
        exportConfig: '📥 导出配置',
        importConfig: '📤 导入配置',

        // Alerts / Confirms
        deleteConfirm: '确认删除此监控主题？',
        testSuccess: '测试完成！详情请查看控制台。',
        testFailed: '测试失败：',
        testError: '测试出错：',
        exportSuccess: '配置导出成功！',
        exportError: '导出配置时出错：',
        importInvalidFormat: '无效的配置文件格式',
        importConfirm: (count) => `导入包含 ${count} 个主题的配置？\n\n这将替换您当前的配置。`,
        importSuccess: (count) => `配置导入成功！\n已加载 ${count} 个主题。`,
        importError: '导入配置时出错：',
        invalidScheduleJson: '无效的时间表 JSON 格式：',
        clearAllConfirm: '清除所有未读帖子？',

        // Unseen Posts
        unseenPosts: '未读帖子',
        clearAll: '清除全部',
        noUnseenPosts: '没有未读帖子',
        allCaughtUp: '您已全部阅读完毕！',
        posts: (count) => `${count} 条帖子`,

        // Timestamp
        justNow: '刚刚',
        minutesAgo: (n) => `${n} 分钟前`,
        hoursAgo: (n) => `${n} 小时前`,
        daysAgo: (n) => `${n} 天前`,

        // Footer
        footer: 'v1.0.0 | 无需服务器',
    }
};

// Internal state — populated on first call to getCurrentLang / initI18n
let _currentLang = null;

/**
 * Initialize i18n: load persisted language from storage.
 * Must be awaited before calling t() or applyTranslations().
 */
export async function initI18n() {
    const { lang } = await chrome.storage.local.get(['lang']);
    _currentLang = lang || 'zh';
}

/** Return the active language code ('en' | 'zh'). */
export function getCurrentLang() {
    return _currentLang || 'zh';
}

/** Persist language preference and update internal state. */
export async function setLang(lang) {
    _currentLang = lang;
    await chrome.storage.local.set({ lang });
}

/**
 * Look up a translation key.
 * If the value is a function (for plural/template strings), return the function.
 * Falls back to English, then the key itself.
 */
export function t(key) {
    const lang = getCurrentLang();
    const dict = TRANSLATIONS[lang] || TRANSLATIONS.en;
    return dict[key] !== undefined ? dict[key] : (TRANSLATIONS.en[key] !== undefined ? TRANSLATIONS.en[key] : key);
}

/**
 * Apply translations to all elements with data-i18n / data-i18n-placeholder attributes.
 * Also updates <html lang>.
 */
export function applyTranslations() {
    const lang = getCurrentLang();
    document.documentElement.lang = lang === 'zh' ? 'zh-CN' : 'en';

    document.querySelectorAll('[data-i18n]').forEach(el => {
        const key = el.getAttribute('data-i18n');
        const value = t(key);
        if (typeof value === 'string') {
            el.textContent = value;
        }
    });

    document.querySelectorAll('[data-i18n-placeholder]').forEach(el => {
        const key = el.getAttribute('data-i18n-placeholder');
        const value = t(key);
        if (typeof value === 'string') {
            el.placeholder = value;
        }
    });

    document.querySelectorAll('[data-i18n-title]').forEach(el => {
        const key = el.getAttribute('data-i18n-title');
        const value = t(key);
        if (typeof value === 'string') {
            el.title = value;
        }
    });
}
