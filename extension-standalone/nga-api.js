/**
 * NGA API Client for Chrome Extension
 * Handles authentication via browser cookies and API calls
 */

export class NGAClient {
    constructor() {
        this.baseUrl = 'https://bbs.nga.cn/app_api.php';
    }

    async getCookies() {
        const cookies = await chrome.cookies.getAll({
            domain: '.nga.cn'
        });

        const cookieMap = {};
        cookies.forEach(cookie => {
            cookieMap[cookie.name] = cookie.value;
        });

        return {
            uid: cookieMap.ngaPassportUid,
            cid: cookieMap.ngaPassportCid
        };
    }

    async fetchPage(tid, page = 1) {
        const cookies = await this.getCookies();

        if (!cookies.uid || !cookies.cid) {
            throw new Error('NGA cookies not found. Please log in to NGA website.');
        }

        // Match server's API call structure
        const params = new URLSearchParams({
            '__lib': 'post',
            '__act': 'list'
        });

        const formData = new URLSearchParams({
            'tid': tid,
            'page': page
        });

        const response = await fetch(`${this.baseUrl}?${params}`, {
            method: 'POST',
            // Cookie/Origin/User-Agent are browser-controlled request headers.
            // `include` makes Chrome attach the authenticated NGA cookie jar.
            credentials: 'include',
            headers: {
                'Content-Type': 'application/x-www-form-urlencoded',
                'Accept': 'application/json, text/javascript, */*; q=0.01',
                'Accept-Language': 'en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7'
            },
            body: formData
        });

        if (!response.ok) {
            throw new Error(`HTTP ${response.status}`);
        }

        const data = await response.json();
        if (Number(data?.code) !== 0) {
            throw new Error(`NGA business error ${String(data?.code ?? 'missing')}`);
        }
        return this.parsePageResult(data, Number(tid), Number(page));
    }

    parsePageResult(pageData, expectedTid, expectedPage) {
        if (!Array.isArray(pageData?.result) || pageData.result.length === 0) {
            throw new Error('Malformed NGA response: result must be a non-empty array');
        }
        const positiveInteger = value => Number.isSafeInteger(Number(value)) && Number(value) > 0;
        const nonNegativeInteger = value => Number.isSafeInteger(Number(value)) && Number(value) >= 0;
        const currentPage = Number(pageData.currentPage);
        const totalPages = Number(pageData.totalPage);
        const postsPerPage = Number(pageData.perPage);
        const totalPosts = Number(pageData.vrows);
        if (!positiveInteger(currentPage) || !positiveInteger(totalPages) ||
            !positiveInteger(postsPerPage) || !positiveInteger(totalPosts) ||
            currentPage > totalPages || currentPage !== expectedPage) {
            throw new Error('Malformed NGA pagination metadata');
        }

        // Validate every row before deriving metadata or notification payloads.
        // A successful response for another TID must never advance this watch.
        for (const post of pageData.result) {
            const floor = Number(post?.lou);
            const pid = Number(post?.pid);
            if (Number(post?.tid) !== expectedTid || !nonNegativeInteger(floor) ||
                !nonNegativeInteger(pid) || (floor > 0 && pid === 0)) {
                throw new Error('Malformed NGA post identity');
            }
        }
        const firstPost = pageData.result[0];

        // Thread info
        const thread = {
            tid: expectedTid,
            title: pageData.tsubject || '',
            author_name: pageData.tauthor || '',
            author_uid: pageData.tauthorid || 0,
            total_posts: totalPosts,
            total_pages: totalPages,
            posts_per_page: postsPerPage,
            currentPage
        };

        // Posts array
        const posts = [];
        for (const post of pageData.result) {
            const author = post.author || {};
            posts.push({
                pid: Number(post.pid),
                tid: expectedTid,
                fid: post.fid || 0,
                author_name: author.username || '',
                author_uid: author.uid || 0,
                post_date: post.postdate || '',
                post_timestamp: post.postdatetimestamp || 0,
                content: post.content || '',
                post_number: Number(post.lou),  // Floor number (楼层)
                page: currentPage  // Which page this post is on
            });
        }

        return { thread, posts };
    }

    async fetchNewPosts(tid, lastSeenPostNumber) {
        // Fetch page 1 to get thread info
        const { thread, posts: firstPagePosts } = await this.fetchPage(tid, 1);
        if (Number(thread.currentPage) !== 1) {
            throw new Error('NGA returned an unexpected first page');
        }

        // Calculate which pages to fetch based on lastSeenPostNumber
        const totalPosts = thread.total_posts;
        const postsPerPage = thread.posts_per_page;

        // If no new posts, return empty
        if (totalPosts <= Number(lastSeenPostNumber) + 1) {
            return { thread, newPosts: [] };
        }

        // Calculate page range containing new posts
        // lastSeenPostNumber is the highest we've seen (1-indexed floor number)
        // We need to fetch pages that contain posts > lastSeenPostNumber

        const startPage = Math.floor(lastSeenPostNumber / postsPerPage) + 1;
        const endPage = thread.total_pages;

        console.log(`[TID ${tid}] Fetching pages ${startPage}-${endPage} for new posts after #${lastSeenPostNumber}`);

        let allNewPosts = [];

        // Add new posts from first page if any
        const newPostsFromFirstPage = firstPagePosts.filter(p => p.post_number > lastSeenPostNumber);
        allNewPosts.push(...newPostsFromFirstPage);

        // Fetch every remaining page in the new-post range. Page 1 was fetched
        // above for metadata, so start at page 2 when startPage is 1.
        for (let page = Math.max(startPage, 2); page <= endPage; page++) {
            const { thread: pageInfo, posts } = await this.fetchPage(tid, page);
            if (Number(pageInfo.currentPage) !== page) {
                throw new Error(`NGA returned page ${pageInfo.currentPage} while page ${page} was requested`);
            }
            const newPosts = posts.filter(p => p.post_number > lastSeenPostNumber);
            allNewPosts.push(...newPosts);

            // Small delay to avoid rate limiting
            await this.delay(500);
        }

        console.log(`[TID ${tid}] Found ${allNewPosts.length} new posts`);
        return { thread, newPosts: allNewPosts };
    }

    delay(ms) {
        return new Promise(resolve => setTimeout(resolve, ms));
    }
}
