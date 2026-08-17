const MIN_CHECK_INTERVAL_SECONDS = 60;
const MAX_CHECK_INTERVAL_SECONDS = 86_400;
const MAX_SCHEDULE_RULES = 128;

const ALLOWED_DAYS = new Set([
    'monday', 'tuesday', 'wednesday', 'thursday', 'friday',
    'saturday', 'sunday', 'weekday', 'weekdays', 'weekend', 'weekends'
]);

function isPlainObject(value) {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) {
        return false;
    }
    const prototype = Object.getPrototypeOf(value);
    return prototype === Object.prototype || prototype === null;
}

function integerValue(value, field, minimum, maximum) {
    const candidate = typeof value === 'string' && /^-?\d+$/.test(value.trim())
        ? Number(value.trim())
        : value;
    if (!Number.isSafeInteger(candidate) || candidate < minimum || candidate > maximum) {
        throw new Error(`${field} must be an integer between ${minimum} and ${maximum}`);
    }
    return candidate;
}

function optionalInteger(value, fallback, field, minimum, maximum) {
    return value === undefined || value === null
        ? fallback
        : integerValue(value, field, minimum, maximum);
}

function isValidWatchId(value) {
    return typeof value === 'string' && value.length > 0 && value.length <= 128;
}

export function createWatchId() {
    if (!globalThis.crypto?.randomUUID) {
        throw new Error('This browser does not support secure watch identifiers');
    }
    return globalThis.crypto.randomUUID();
}

export function ensureThreadIdentities(threads) {
    if (!Array.isArray(threads)) {
        return [];
    }

    const usedIds = new Set();
    return threads.map(thread => {
        if (!isPlainObject(thread)) {
            return thread;
        }

        let watchId = thread.watchId;
        if (!isValidWatchId(watchId) || usedIds.has(watchId)) {
            watchId = createWatchId();
        }
        usedIds.add(watchId);

        const revision = Number.isSafeInteger(thread.revision) && thread.revision >= 0
            ? thread.revision
            : 0;
        return { ...thread, watchId, revision };
    });
}

export function nextThreadFence(thread) {
    if (!isPlainObject(thread) || !isValidWatchId(thread.watchId)) {
        return { watchId: createWatchId(), revision: 0 };
    }
    if (!Number.isSafeInteger(thread.revision) || thread.revision < 0 ||
        thread.revision === Number.MAX_SAFE_INTEGER) {
        return { watchId: createWatchId(), revision: 0 };
    }
    return { watchId: thread.watchId, revision: thread.revision + 1 };
}

export function normalizeSchedule(schedule) {
    if (schedule === undefined || schedule === null) {
        return null;
    }
    if (!Array.isArray(schedule) || schedule.length === 0 || schedule.length > MAX_SCHEDULE_RULES) {
        throw new Error(`schedule must contain between 1 and ${MAX_SCHEDULE_RULES} rules`);
    }

    return schedule.map((rule, index) => {
        if (!isPlainObject(rule)) {
            throw new Error(`schedule rule ${index} must be an object`);
        }
        const validTime = value => typeof value === 'string' &&
            /^(?:[01]\d|2[0-3]):[0-5]\d$/.test(value);
        if (!validTime(rule.start_time) || !validTime(rule.end_time)) {
            throw new Error(`schedule rule ${index} must use HH:MM start and end times`);
        }

        const days = rule.days === undefined ? [] : rule.days;
        if (!Array.isArray(days) || !days.every(day =>
            typeof day === 'string' && ALLOWED_DAYS.has(day.toLowerCase()))) {
            throw new Error(`schedule rule ${index} contains an unsupported day`);
        }

        const normalized = {
            days: [...new Set(days.map(day => day.toLowerCase()))],
            start_time: rule.start_time,
            end_time: rule.end_time,
            interval: integerValue(
                rule.interval,
                `schedule rule ${index} interval`,
                MIN_CHECK_INTERVAL_SECONDS,
                MAX_CHECK_INTERVAL_SECONDS
            )
        };
        if (rule.description !== undefined) {
            if (typeof rule.description !== 'string') {
                throw new Error(`schedule rule ${index} description must be a string`);
            }
            normalized.description = rule.description;
        }
        return normalized;
    });
}

export function normalizeThreadConfig(thread, { preserveIdentity = true } = {}) {
    if (!isPlainObject(thread)) {
        throw new Error('thread must be an object');
    }

    const tid = integerValue(thread.tid, 'tid', 1, Number.MAX_SAFE_INTEGER);
    const enabled = thread.enabled === undefined ? true : thread.enabled;
    if (typeof enabled !== 'boolean') {
        throw new Error('enabled must be a boolean');
    }

    const title = thread.title === undefined || thread.title === null ? null : thread.title;
    if (title !== null && typeof title !== 'string') {
        throw new Error('title must be a string or null');
    }

    const authors = thread.authorNotification === undefined
        ? []
        : thread.authorNotification;
    if (!Array.isArray(authors)) {
        throw new Error('authorNotification must be an array');
    }
    const authorNotification = [...new Set(authors.map((uid, index) =>
        integerValue(uid, `authorNotification[${index}]`, 1, Number.MAX_SAFE_INTEGER)
    ))];

    let watchId = preserveIdentity && isValidWatchId(thread.watchId)
        ? thread.watchId
        : createWatchId();
    let revision = preserveIdentity
        ? optionalInteger(thread.revision, 0, 'revision', 0, Number.MAX_SAFE_INTEGER)
        : 0;
    if (!isValidWatchId(watchId)) {
        watchId = createWatchId();
        revision = 0;
    }

    return {
        watchId,
        revision,
        tid,
        title,
        lastSeenPostNumber: optionalInteger(
            thread.lastSeenPostNumber,
            0,
            'lastSeenPostNumber',
            0,
            Number.MAX_SAFE_INTEGER
        ),
        authorNotification,
        checkInterval: optionalInteger(
            thread.checkInterval,
            300,
            'checkInterval',
            MIN_CHECK_INTERVAL_SECONDS,
            MAX_CHECK_INTERVAL_SECONDS
        ),
        checkIntervalSchedule: normalizeSchedule(thread.checkIntervalSchedule),
        enabled,
        lastChecked: optionalInteger(
            thread.lastChecked,
            0,
            'lastChecked',
            0,
            Number.MAX_SAFE_INTEGER
        )
    };
}

export function normalizeThreadList(threads, { preserveIdentity = true } = {}) {
    if (!Array.isArray(threads)) {
        throw new Error('threads must be an array');
    }

    const tids = new Set();
    const watchIds = new Set();
    return threads.map((thread, index) => {
        const normalized = normalizeThreadConfig(thread, { preserveIdentity });
        if (tids.has(normalized.tid)) {
            throw new Error(`duplicate TID ${normalized.tid}`);
        }
        tids.add(normalized.tid);

        if (watchIds.has(normalized.watchId)) {
            if (!preserveIdentity) {
                throw new Error(`duplicate watch identity at index ${index}`);
            }
            normalized.watchId = createWatchId();
            normalized.revision = 0;
        }
        watchIds.add(normalized.watchId);
        return normalized;
    });
}
