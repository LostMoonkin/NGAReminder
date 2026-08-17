import test from 'node:test';
import assert from 'node:assert/strict';

import { NGAClient } from './nga-api.js';
import {
    nextThreadFence,
    normalizeThreadConfig,
    normalizeThreadList
} from './thread-config.mjs';

test('imported thread configs are normalized and receive fresh fences', () => {
    const [thread] = normalizeThreadList([{
        watchId: 'untrusted-export-id',
        revision: 99,
        tid: '123',
        authorNotification: ['456', 456]
    }], { preserveIdentity: false });

    assert.equal(thread.tid, 123);
    assert.equal(thread.revision, 0);
    assert.notEqual(thread.watchId, 'untrusted-export-id');
    assert.deepEqual(thread.authorNotification, [456]);
    assert.equal(thread.checkInterval, 300);
});

test('editing a thread advances the existing fence', () => {
    assert.deepEqual(
        nextThreadFence({ watchId: 'watch', revision: 4 }),
        { watchId: 'watch', revision: 5 }
    );
});

test('malformed imported fields are rejected', () => {
    assert.throws(
        () => normalizeThreadConfig({ tid: 1, authorNotification: '2' }),
        /authorNotification/
    );
    assert.throws(
        () => normalizeThreadConfig({ tid: 1, enabled: 'yes' }),
        /enabled/
    );
});

const validPage = () => ({
    code: 0,
    currentPage: 1,
    totalPage: 1,
    perPage: 20,
    vrows: 2,
    result: [
        { tid: 123, pid: 0, lou: 0, author: { uid: 1, username: 'topic' } },
        { tid: 123, pid: 456, lou: 1, author: { uid: 2, username: 'reply' } }
    ]
});

test('NGA page parsing rejects an unexpected thread identity', () => {
    const page = validPage();
    page.result[1].tid = 999;
    assert.throws(
        () => new NGAClient().parsePageResult(page, 123, 1),
        /post identity/
    );
});

test('NGA page parsing requires non-empty, matching pagination', () => {
    const empty = validPage();
    empty.result = [];
    assert.throws(
        () => new NGAClient().parsePageResult(empty, 123, 1),
        /non-empty array/
    );

    const wrongPage = validPage();
    wrongPage.currentPage = 2;
    wrongPage.totalPage = 2;
    assert.throws(
        () => new NGAClient().parsePageResult(wrongPage, 123, 1),
        /pagination metadata/
    );
});
