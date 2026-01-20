/**
 * NGA API Client for Chrome Extension
 * Handles authentication via browser cookies and API calls
 */

export class NGAClient {
    constructor() {
        this.baseUrl = 'https://bbs.nga.cn/app_api.php';
        this.userAgent = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36';
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
            headers: {
                'Content-Type': 'application/x-www-form-urlencoded',
                'User-Agent': this.userAgent,
                'Accept': 'application/json, text/javascript, */*; q=0.01',
                'Accept-Language': 'en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7',
                'Cookie': `ngaPassportUid=${cookies.uid}; ngaPassportCid=${cookies.cid}`,
                'Origin': 'https://bbs.nga.cn',
                'Referer': 'https://bbs.nga.cn/'
            },
            body: formData
        });

        if (!response.ok) {
            throw new Error(`HTTP ${response.status}`);
        }

        const data = await response.json();
        return this.parsePageResult(data);
    }

    parsePageResult(pageData) {
        // Based on server's parse_page_result function
        const firstPost = pageData.result?.[0] || {};

        // Thread info
        const thread = {
            tid: firstPost.tid || 0,
            title: pageData.tsubject || '',
            author_name: pageData.tauthor || '',
            author_uid: pageData.tauthorid || 0,
            total_posts: pageData.vrows || 0,
            total_pages: pageData.totalPage || 0,
            posts_per_page: pageData.perPage || 20,
            currentPage: pageData.currentPage || 1
        };

        // Posts array
        const posts = [];
        const currentPage = pageData.currentPage || 1;
        for (const post of (pageData.result || [])) {
            const author = post.author || {};
            posts.push({
                pid: post.pid || 0,
                tid: post.tid || 0,
                fid: post.fid || 0,
                author_name: author.username || '',
                author_uid: author.uid || 0,
                post_date: post.postdate || '',
                post_timestamp: post.postdatetimestamp || 0,
                content: post.content || '',
                post_number: post.lou || 0,  // Floor number (楼层)
                page: currentPage  // Which page this post is on
            });
        }

        return { thread, posts };
    }

    async fetchNewPosts(tid, lastSeenPostNumber) {
        // Fetch page 1 to get thread info
        const { thread, posts: firstPagePosts } = await this.fetchPage(tid, 1);

        // Calculate which pages to fetch based on lastSeenPostNumber
        const totalPosts = thread.total_posts;
        const postsPerPage = thread.posts_per_page;

        // If no new posts, return empty
        if (totalPosts <= lastSeenPostNumber) {
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

        // Fetch remaining pages if needed
        if (startPage > 1) {
            for (let page = startPage; page <= endPage; page++) {
                if (page === 1) continue; // Already fetched

                const { posts } = await this.fetchPage(tid, page);
                const newPosts = posts.filter(p => p.post_number > lastSeenPostNumber);
                allNewPosts.push(...newPosts);

                // Small delay to avoid rate limiting
                await this.delay(500);
            }
        }

        console.log(`[TID ${tid}] Found ${allNewPosts.length} new posts`);
        return { thread, newPosts: allNewPosts };
    }

    delay(ms) {
        return new Promise(resolve => setTimeout(resolve, ms));
    }
}
