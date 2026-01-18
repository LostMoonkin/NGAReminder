#!/usr/bin/env python3
"""
Thread monitor for NGA BBS.
Periodically checks monitored threads for new posts.
"""

import json
import time
import argparse
from datetime import datetime
from typing import List, Dict, Any, Optional, Set
from .database import NGADatabase, parse_page_result
from .nga_crawler import NGACrawler
from .notification import NotificationManager


class ThreadMonitor:
    """Monitor NGA threads for new posts."""
    
    def __init__(self, db_path: str = "data/nga_data.db", config_path: str = "config/config.json"):
        """
        Initialize thread monitor.
        
        Args:
            db_path: Path to SQLite database
            config_path: Path to crawler config file
        """
        self.db = NGADatabase(db_path)
        self.crawler = NGACrawler(config_path)
        self.config_path = config_path
        self._init_monitor_tables()
        
        # Initialize notification system
        with open(config_path, 'r', encoding='utf-8') as f:
            config = json.load(f)
        self.notification_manager = NotificationManager(config)
    
    def _init_monitor_tables(self):
        """Initialize monitoring tables if they don't exist."""
        self.db.cursor.executescript('''
            CREATE TABLE IF NOT EXISTS monitored_threads (
                tid INTEGER PRIMARY KEY,
                author_filter TEXT,
                author_notification TEXT,
                check_interval INTEGER DEFAULT 300,
                last_checked TIMESTAMP,
                last_post_timestamp INTEGER DEFAULT 0,
                is_active BOOLEAN DEFAULT 1,
                created_at TIMESTAMP DEFAULT (datetime('now', 'localtime')),
                FOREIGN KEY (tid) REFERENCES threads(tid) ON DELETE CASCADE
            );
            
            CREATE TABLE IF NOT EXISTS monitoring_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                tid INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                post_count INTEGER DEFAULT 0,
                message TEXT,
                created_at TIMESTAMP DEFAULT (datetime('now', 'localtime')),
                FOREIGN KEY (tid) REFERENCES threads(tid) ON DELETE CASCADE
            );
            
            CREATE INDEX IF NOT EXISTS idx_monitored_active ON monitored_threads(is_active);
            CREATE INDEX IF NOT EXISTS idx_monitoring_events_tid ON monitoring_events(tid);
        ''')
        self.db.conn.commit()
    
    def add_thread(self, tid: int, author_filter: Optional[List[int]] = None, 
                   check_interval: int = 300, author_notification: Optional[List[int]] = None, 
                   stop_event=None) -> bool:
        """
        Add a thread to monitoring list.
        Fetches ALL pages initially and stores posts in batches.
        
        Args:
            tid: Thread ID
            author_filter: List of author UIDs to monitor (None = all authors)
            check_interval: Seconds between checks
            author_notification: List of author UIDs to notify (None = all authors)
            stop_event: Optional threading.Event to signal early stop
            
        Returns:
            True if successful
        """
        try:
            print(f"Fetching thread {tid}...")
            
            # Fetch first page to get thread info
            first_page = self.crawler.fetch_page(tid, 1)
            if not first_page:
                print("Failed to fetch thread info")
                return False
            
            # Get thread data
            thread_data, first_page_posts = parse_page_result(first_page)
            
            # Save thread info immediately
            self.db.save_thread(thread_data)
            
            total_pages = thread_data['total_pages']
            total_posts = thread_data['total_posts']
            
            print(f"✓ Thread: {thread_data['title']}")
            print(f"✓ Total pages: {total_pages}")
            print(f"✓ Total posts: {total_posts}")
            
            # Save first page posts
            if first_page_posts:
                saved = self.db.save_posts_batch(first_page_posts)
                print(f"  Saved page 1: {saved} posts")
            
            latest_timestamp = max((p['post_timestamp'] for p in first_page_posts), default=0)
            
            print(f"DEBUG: After page 1, total_pages={total_pages}")
            
            # Fetch and save remaining pages in batches
            if total_pages > 1:
                print(f"DEBUG: Entering batch fetch for pages 2-{total_pages}")
                print(f"Fetching pages 2-{total_pages}...")
                
                # Use callback to save posts immediately as each page completes
                def save_page_callback(page_num, page_result):
                    """Callback to save posts immediately when a page is fetched."""
                    nonlocal latest_timestamp
                    if page_result:
                        _, posts_data = parse_page_result(page_result)
                        if posts_data:
                            saved = self.db.save_posts_batch(posts_data)
                            print(f"  ✓ Saved page {page_num}: {saved} posts")
                            
                            # Update latest timestamp
                            page_latest = max((p['post_timestamp'] for p in posts_data), default=0)
                            if page_latest > latest_timestamp:
                                latest_timestamp = page_latest
                        else:
                            print(f"  ⚠ Page {page_num}: No posts found")
                    else:
                        print(f"  ✗ Page {page_num}: Failed to fetch")
                
                # Fetch with callback
                self.crawler.crawl_pages_range_with_callback(tid, 2, total_pages, save_page_callback, stop_event=stop_event)
            
            # Count total saved posts
            self.db.cursor.execute('SELECT COUNT(*) FROM posts WHERE tid = ?', (tid,))
            saved_count = self.db.cursor.fetchone()[0]
            print(f"✓ Total saved: {saved_count} posts")
            
            # Add to monitoring
            author_filter_str = ','.join(map(str, author_filter)) if author_filter else None
            author_notification_str = ','.join(map(str, author_notification)) if author_notification else None
            
            self.db.cursor.execute('''
                INSERT INTO monitored_threads (
                    tid, author_filter, author_notification, check_interval, 
                    last_checked, last_post_timestamp
                ) VALUES (?, ?, ?, ?, datetime('now', 'localtime'), ?)
                ON CONFLICT(tid) DO UPDATE SET
                    author_filter = excluded.author_filter,
                    author_notification = excluded.author_notification,
                    check_interval = excluded.check_interval,
                    is_active = 1,
                    last_checked = datetime('now', 'localtime'),
                    last_post_timestamp = excluded.last_post_timestamp
            ''', (tid, author_filter_str, author_notification_str, check_interval, latest_timestamp))
            
            self.db.conn.commit()
            
            print(f"✓ Thread {tid} added to monitoring")
            print(f"  Title: {thread_data['title']}")
            print(f"  Author: {thread_data['author_name']}")
            print(f"  Filter: {author_filter if author_filter else 'All authors'}")
            print(f"  Notify: {author_notification if author_notification else 'All authors'}")
            print(f"  Check interval: {check_interval}s")
            
            return True
        except Exception as e:
            print(f"Error adding thread: {e}")
            import traceback
            traceback.print_exc()
            return False
    
    def remove_thread(self, tid: int) -> bool:
        """Remove thread from monitoring."""
        try:
            self.db.cursor.execute(
                'UPDATE monitored_threads SET is_active = 0 WHERE tid = ?',
                (tid,)
            )
            self.db.conn.commit()
            print(f"✓ Thread {tid} removed from monitoring")
            return True
        except Exception as e:
            print(f"Error removing thread: {e}")
            return False
    
    def list_monitored(self) -> List[Dict[str, Any]]:
        """Get list of monitored threads."""
        self.db.cursor.execute('''
            SELECT 
                m.*,
                t.title,
                t.author_name,
                t.total_posts
            FROM monitored_threads m
            JOIN threads t ON m.tid = t.tid
            WHERE m.is_active = 1
            ORDER BY m.last_checked DESC
        ''')
        return [dict(row) for row in self.db.cursor.fetchall()]
    
    def load_from_config(self, config_path: Optional[str] = None, stop_event=None) -> Dict[str, Any]:
        """
        Load and sync monitored threads from config file.
        
        Args:
            config_path: Path to config file (defaults to self.config_path)
            stop_event: Optional threading.Event to signal early stop
            
        Returns:
            Summary of sync operation
        """
        config_path = config_path or self.config_path
        
        try:
            with open(config_path, 'r', encoding='utf-8') as f:
                config = json.load(f)
        except FileNotFoundError:
            return {'error': f'Config file not found: {config_path}'}
        except json.JSONDecodeError as e:
            return {'error': f'Invalid JSON in config file: {e}'}
        
        monitored_threads = config.get('monitored_threads', [])
        
        if not monitored_threads:
            return {'error': 'No monitored_threads defined in config file'}
        
        added = 0
        updated = 0
        skipped = 0
        errors = []
        
        print(f"\nSyncing {len(monitored_threads)} thread(s) from config...\n")
        
        for thread_config in monitored_threads:
            # Check if we should stop
            if stop_event and stop_event.is_set():
                print("\n⚠ Sync interrupted by stop signal")
                break
            
            tid = thread_config.get('tid')
            if not tid:
                errors.append('Thread missing tid field')
                continue
            
            enabled = thread_config.get('enabled', True)
            
            if not enabled:
                print(f"  ⊘ TID {tid}: Skipped (disabled in config)")
                skipped += 1
                continue
            
            author_filter = thread_config.get('author_filter')
            check_interval = thread_config.get('check_interval', 300)
            
            # Check if already monitored
            self.db.cursor.execute(
                'SELECT tid FROM monitored_threads WHERE tid = ?',
                (tid,)
            )
            existing = self.db.cursor.fetchone()
            
            # Check if posts already exist and get max post_number
            self.db.cursor.execute(
                'SELECT COUNT(*), MAX(post_number) FROM posts WHERE tid = ?',
                (tid,)
            )
            row = self.db.cursor.fetchone()
            post_count = row[0]
            max_post_number = row[1] if row[1] is not None else -1
            
            if post_count > 0:
                # We have some posts - check if we need to fetch more
                print(f"  ○ TID {tid}: Found {post_count} existing posts (up to post #{max_post_number})")
                
                # Fetch page 1 to check current state
                first_page = self.crawler.fetch_page(tid, 1)
                if not first_page:
                    print(f"    ✗ Failed to fetch page 1, skipping sync")
                    errors.append(f'Failed to fetch thread {tid}')
                    continue
                
                thread_data, _ = parse_page_result(first_page)
                current_total_posts = thread_data['total_posts']
                current_total_pages = thread_data['total_pages']
                
                if max_post_number >= current_total_posts - 1:
                    # We have all posts (post_number is 0-indexed)
                    print(f"    ✓ Already up to date ({current_total_posts} posts)")
                    
                    # Update or insert into monitored_threads
                    author_filter_str = ','.join(map(str, author_filter)) if author_filter else None
                    author_notification_str = ','.join(map(str, thread_config.get('author_notification', []))) if thread_config.get('author_notification') else None
                    
                    # Get latest post timestamp
                    self.db.cursor.execute(
                        'SELECT MAX(post_timestamp) FROM posts WHERE tid = ?',
                        (tid,)
                    )
                    latest_timestamp = self.db.cursor.fetchone()[0] or 0
                    
                    if existing:
                        self.db.cursor.execute('''
                            UPDATE monitored_threads SET
                                author_filter = ?,
                                author_notification = ?,
                                check_interval = ?,
                                is_active = 1
                            WHERE tid = ?
                        ''', (author_filter_str, author_notification_str, check_interval, tid))
                        updated += 1
                    else:
                        self.db.cursor.execute('''
                            INSERT INTO monitored_threads (
                                tid, author_filter, author_notification, check_interval,
                                last_checked, last_post_timestamp
                            ) VALUES (?, ?, ?, ?, datetime('now', 'localtime'), ?)
                        ''', (tid, author_filter_str, author_notification_str, check_interval, latest_timestamp))
                        added += 1
                    
                    self.db.conn.commit()
                else:
                    # Need to fetch missing posts
                    missing_posts = current_total_posts - (max_post_number + 1)
                    print(f"    ⟳ Need to fetch {missing_posts} new posts (current total: {current_total_posts})")
                    
                    # Calculate which pages to fetch
                    posts_per_page = first_page.get('perPage', 20)
                    # Start from page containing post after max_post_number
                    start_page = (max_post_number // posts_per_page) + 1
                    if start_page < 1:
                        start_page = 1
                    
                    print(f"    Fetching pages {start_page}-{current_total_pages}...")
                    
                    # Update thread info
                    self.db.save_thread(thread_data)
                    
                    # Fetch missing pages with callback
                    def save_page_callback(page_num, page_result):
                        if page_result:
                            _, posts_data = parse_page_result(page_result)
                            if posts_data:
                                # Only save posts we don't have yet
                                new_posts = [p for p in posts_data if p['post_number'] > max_post_number]
                                if new_posts:
                                    saved = self.db.save_posts_batch(new_posts)
                                    print(f"      ✓ Saved page {page_num}: {saved} new posts")
                    
                    self.crawler.crawl_pages_range_with_callback(tid, start_page, current_total_pages, save_page_callback, stop_event=stop_event)
                    
                    # Update monitored_threads
                    author_filter_str = ','.join(map(str, author_filter)) if author_filter else None
                    author_notification_str = ','.join(map(str, thread_config.get('author_notification', []))) if thread_config.get('author_notification') else None
                    
                    self.db.cursor.execute(
                        'SELECT MAX(post_timestamp) FROM posts WHERE tid = ?',
                        (tid,)
                    )
                    latest_timestamp = self.db.cursor.fetchone()[0] or 0
                    
                    if existing:
                        self.db.cursor.execute('''
                            UPDATE monitored_threads SET
                                author_filter = ?,
                                author_notification = ?,
                                check_interval = ?,
                                is_active = 1
                            WHERE tid = ?
                        ''', (author_filter_str, author_notification_str, check_interval, tid))
                        updated += 1
                    else:
                        self.db.cursor.execute('''
                            INSERT INTO monitored_threads (
                                tid, author_filter, author_notification, check_interval,
                                last_checked, last_post_timestamp
                            ) VALUES (?, ?, ?, ?, datetime('now', 'localtime'), ?)
                        ''', (tid, author_filter_str, author_notification_str, check_interval, latest_timestamp))
                        added += 1
                    
                    self.db.conn.commit()
            else:
                # No posts (new thread OR empty monitored thread) - fetch all pages
                if existing:
                    print(f"  ⚠ TID {tid}: In monitoring but no posts found, re-fetching...")
                else:
                    print(f"  + TID {tid}: Adding to monitoring (fetch all pages)")
                
                success = self.add_thread(tid, author_filter, check_interval, 
                                        author_notification=thread_config.get('author_notification'),
                                        stop_event=stop_event)
                if success:
                    added += 1
                else:
                    errors.append(f'Failed to add thread {tid}')

        
        print(f"\n{'='*80}")
        print(f"Sync complete:")
        print(f"  Added: {added}")
        print(f"  Updated: {updated}")
        print(f"  Skipped: {skipped}")
        if errors:
            print(f"  Errors: {len(errors)}")
            for err in errors:
                print(f"    - {err}")
        print(f"{'='*80}")
        
        return {
            'added': added,
            'updated': updated,
            'skipped': skipped,
            'errors': errors
        }
    
    def check_thread(self, tid: int, verbose: bool = True) -> Dict[str, Any]:
        """
        Check a single thread for new posts.
        Compares vrows (total posts) and fetches only new pages if needed.
        
        Args:
            tid: Thread ID
            verbose: Print detailed output
            
        Returns:
            Dictionary with check results
        """
        # Get monitoring config and thread data
        self.db.cursor.execute(
            'SELECT * FROM monitored_threads WHERE tid = ? AND is_active = 1',
            (tid,)
        )
        monitor_config = self.db.cursor.fetchone()
        
        if not monitor_config:
            return {'error': f'Thread {tid} not monitored'}
        
        monitor_config = dict(monitor_config)
        author_filter = monitor_config['author_filter']
        
        # Parse author filter
        author_uids = set()
        if author_filter:
            author_uids = set(int(uid) for uid in author_filter.split(','))
        
        # Get stored thread data
        thread = self.db.get_thread(tid)
        if not thread:
            return {'error': f'Thread {tid} not found in database'}
        
        old_total_posts = thread['total_posts']
        
        if verbose:
            print(f"\n{'='*80}")
            print(f"Checking thread {tid}: {thread['title']}")
            print(f"{'='*80}")
            print(f"Last check: {monitor_config.get('last_checked', 'Never')}")
            print(f"Stored total posts: {old_total_posts}")
        
        try:
            # Fetch page 1 to get current total post count
            first_page = self.crawler.fetch_page(tid, 1)
            
            if not first_page:
                self._log_event(tid, 'error', 0, 'Failed to fetch thread')
                return {'error': 'Failed to fetch thread'}
            
            # Get current thread stats
            current_total_posts = first_page.get('vrows', 0)
            current_total_pages = first_page.get('totalPage', 0)
            posts_per_page = first_page.get('perPage', 20)
            
            if verbose:
                print(f"Current total posts: {current_total_posts}")
                print(f"Current total pages: {current_total_pages}")
            
            # Check if there are new posts
            new_post_count = current_total_posts - old_total_posts
            
            if new_post_count <= 0:
                # No new posts
                self.db.cursor.execute(
                    'UPDATE monitored_threads SET last_checked = datetime(\'now\', \'localtime\') WHERE tid = ?',
                    (tid,)
                )
                self._log_event(tid, 'check', 0, 'No new posts')
                
                # Update thread stats
                thread_data, _ = parse_page_result(first_page)
                self.db.save_thread(thread_data)
                self.db.conn.commit()
                
                if verbose:
                    print(f"\n✓ No new posts")
                
                return {
                    'tid': tid,
                    'new_posts': 0,
                    'total_posts': current_total_posts,
                    'posts': []
                }
            
            if verbose:
                print(f"\n🔔 Found {new_post_count} new post(s)!")
                print(f"Fetching new pages...")
            
            # Calculate which pages to fetch
            # Old last page
            old_last_page = (old_total_posts + posts_per_page - 1) // posts_per_page
            
            # Pages that might contain new posts
            # We need to fetch from the page where new posts might start
            # Start from the page that would contain post at position (old_total_posts + 1)
            start_page = max(1, (old_total_posts // posts_per_page) + 1)
            if old_total_posts % posts_per_page == 0 and old_total_posts > 0:
                start_page = old_last_page + 1
            
            end_page = current_total_pages
            
            if verbose:
                print(f"Fetching pages {start_page} to {end_page}...")
            
            # Fetch new pages
            all_new_posts = []
            pages_to_fetch = list(range(start_page, end_page + 1))
            
            for page_num in pages_to_fetch:
                page_result = self.crawler.fetch_page(tid, page_num)
                if page_result:
                    _, posts_data = parse_page_result(page_result)
                    all_new_posts.extend(posts_data)
                    if verbose:
                        print(f"  ✓ Fetched page {page_num}: {len(posts_data)} posts")
                else:
                    if verbose:
                        print(f"  ✗ Failed to fetch page {page_num}")
            
            # Filter posts that are actually new (by timestamp or by not existing in DB)
            # Also apply author filter
            new_posts_to_save = []
            filtered_new_posts = []
            
            for post in all_new_posts:
                # Check if post already exists
                existing = self.db.cursor.execute(
                    'SELECT pid FROM posts WHERE pid = ?',
                    (post['pid'],)
                ).fetchone()
                
                if not existing:
                    new_posts_to_save.append(post)
                    
                    # Check author filter for notification
                    if not author_uids or post['author_uid'] in author_uids:
                        filtered_new_posts.append(post)
            
            # Save new posts
            if new_posts_to_save:
                saved_count = self.db.save_posts_batch(new_posts_to_save)
                if verbose:
                    print(f"\n✓ Saved {saved_count} new posts to database")
            
            # Update thread data
            thread_data, _ = parse_page_result(first_page)
            self.db.save_thread(thread_data)
            
            # Update monitoring state
            self.db.cursor.execute(
                'UPDATE monitored_threads SET last_checked = datetime(\'now\', \'localtime\') WHERE tid = ?',
                (tid,)
            )
            
            # Log event
            if filtered_new_posts:
                self._log_event(tid, 'new_post', len(filtered_new_posts), 
                              f'Found {len(filtered_new_posts)} new posts (filtered by author)')
            else:
                self._log_event(tid, 'check', len(new_posts_to_save), 
                              f'Found {len(new_posts_to_save)} new posts (none match filter)')
            
            self.db.conn.commit()
            
            # Send notifications for posts matching author_notification
            author_notification = monitor_config.get('author_notification')
            if author_notification:
                notification_uids = set(int(uid) for uid in author_notification.split(','))
                
                for post in filtered_new_posts:
                    if post['author_uid'] in notification_uids:
                        # Send notification
                        title = f"📬 {thread['title']}"
                        message = f"{post['author_name']}: {post['content'][:100]}"
                        url = f"https://bbs.nga.cn/read.php?tid={tid}&pid={post['pid']}"
                        
                        self.notification_manager.send(
                            title=title,
                            message=message,
                            url=url
                        )
            
            # Display filtered new posts
            if verbose and filtered_new_posts:
                print(f"\n🎯 New posts matching filter ({len(filtered_new_posts)}):")
                for post in filtered_new_posts[:10]:  # Show first 10
                    print(f"\n  Post #{post['pid']} by {post['author_name']} ({post['post_date']})")
                    content_preview = post['content'][:100].replace('\n', ' ').replace('<br/>', ' ')
                    print(f"  {content_preview}...")
                if len(filtered_new_posts) > 10:
                    print(f"\n  ... and {len(filtered_new_posts) - 10} more")
            elif verbose and new_posts_to_save and not filtered_new_posts:
                print(f"\n  ℹ️  {len(new_posts_to_save)} new posts found, but none match author filter")
            
            return {
                'tid': tid,
                'new_posts': len(filtered_new_posts),
                'total_new_posts': len(new_posts_to_save),
                'total_posts': current_total_posts,
                'posts': filtered_new_posts
            }
            
        except Exception as e:
            error_msg = f'Error checking thread: {e}'
            self._log_event(tid, 'error', 0, error_msg)
            if verbose:
                print(f"\n✗ {error_msg}")
                import traceback
                traceback.print_exc()
            return {'error': error_msg}
    
    def check_all(self, verbose: bool = True) -> Dict[str, Any]:
        """
        Check all active monitored threads.
        
        Args:
            verbose: Print detailed output
            
        Returns:
            Summary of checks
        """
        monitored = self.list_monitored()
        
        if not monitored:
            print("No threads being monitored")
            return {'total': 0, 'checked': 0, 'new_posts': 0}
        
        print(f"\nChecking {len(monitored)} monitored thread(s)...")
        
        total_new = 0
        checked = 0
        
        for thread in monitored:
            result = self.check_thread(thread['tid'], verbose=verbose)
            if 'new_posts' in result:
                total_new += result['new_posts']
                checked += 1
            
            # Small delay between checks
            time.sleep(1)
        
        print(f"\n{'='*80}")
        print(f"Summary: Checked {checked} threads, found {total_new} new post(s)")
        print(f"{'='*80}")
        
        return {
            'total': len(monitored),
            'checked': checked,
            'new_posts': total_new
        }
    
    def run_loop(self, check_all_interval: int = 10, stop_event=None):
        """
        Run monitoring loop continuously.
        Respects per-thread check_interval from database.
        
        Args:
            check_all_interval: Seconds between checking which threads need updates (default: 10)
            stop_event: Optional threading.Event to signal loop to stop
        """
        print(f"Starting monitoring loop")
        print("Each thread will be checked according to its own check_interval")
        if stop_event is None:
            print("Press Ctrl+C to stop\n")
        else:
            print("Monitoring will stop when server shuts down\n")
        
        try:
            while True:
                # Check if we should stop
                if stop_event and stop_event.is_set():
                    print("\nMonitoring loop stopped by signal")
                    break
                # Get all monitored threads with their configuration
                monitored = self.list_monitored()
                
                if not monitored:
                    print("No threads being monitored. Waiting...")
                    time.sleep(check_all_interval)
                    continue
                
                # Check each thread if its interval has passed
                current_time = time.time()
                threads_to_check = []
                
                for thread in monitored:
                    tid = thread['tid']
                    check_interval = thread['check_interval']
                    last_checked = thread['last_checked']
                    
                    # Parse last_checked time
                    if last_checked:
                        try:
                            # SQLite datetime format: 'YYYY-MM-DD HH:MM:SS'
                            from datetime import datetime
                            last_check_dt = datetime.strptime(last_checked, '%Y-%m-%d %H:%M:%S')
                            last_check_timestamp = last_check_dt.timestamp()
                            time_since_check = current_time - last_check_timestamp
                        except:
                            # If parsing fails, check immediately
                            time_since_check = check_interval + 1
                    else:
                        # Never checked, check immediately
                        time_since_check = check_interval + 1
                    
                    # Should we check this thread?
                    if time_since_check >= check_interval:
                        threads_to_check.append({
                            'tid': tid,
                            'title': thread['title'],
                            'check_interval': check_interval,
                            'overdue_by': time_since_check - check_interval
                        })
                
                # Check threads that are due
                if threads_to_check:
                    print(f"\n{'='*80}")
                    print(f"Checking {len(threads_to_check)} thread(s) due for update")
                    print(f"{'='*80}")
                    
                    for thread_info in threads_to_check:
                        print(f"\nThread {thread_info['tid']}: {thread_info['title']}")
                        print(f"  Check interval: {thread_info['check_interval']}s")
                        if thread_info['overdue_by'] > 0:
                            print(f"  Overdue by: {thread_info['overdue_by']:.0f}s")
                        
                        self.check_thread(thread_info['tid'], verbose=True)
                        
                        # Small delay between threads
                        time.sleep(1)
                    
                    print(f"\n{'='*80}")
                    print(f"Completed checking {len(threads_to_check)} thread(s)")
                    print(f"{'='*80}")
                
                # Show status of all threads
                print(f"\nMonitoring {len(monitored)} thread(s):")
                for thread in monitored:
                    last_checked = thread['last_checked'] or 'Never'
                    print(f"  TID {thread['tid']}: {thread['title']}")
                    print(f"    Interval: {thread['check_interval']}s, Last checked: {last_checked}")
                
                # Wait before next check cycle
                print(f"\nWaiting {check_all_interval}s until next evaluation...")
                
                if stop_event:
                    # Sleep in small chunks to respond quickly to stop signal
                    for _ in range(check_all_interval):
                        if stop_event.is_set():
                            print("\nMonitoring loop stopped by signal")
                            return
                        time.sleep(1)
                else:
                    time.sleep(check_all_interval)
                
        except KeyboardInterrupt:
            print("\n\nMonitoring stopped by user")
    
    def _log_event(self, tid: int, event_type: str, post_count: int, message: str):
        """Log a monitoring event."""
        self.db.cursor.execute('''
            INSERT INTO monitoring_events (tid, event_type, post_count, message)
            VALUES (?, ?, ?, ?)
        ''', (tid, event_type, post_count, message))
    
    def get_events(self, tid: Optional[int] = None, limit: int = 50) -> List[Dict[str, Any]]:
        """Get monitoring event history."""
        if tid:
            query = '''
                SELECT * FROM monitoring_events 
                WHERE tid = ? 
                ORDER BY created_at DESC 
                LIMIT ?
            '''
            self.db.cursor.execute(query, (tid, limit))
        else:
            query = '''
                SELECT e.*, t.title 
                FROM monitoring_events e
                JOIN threads t ON e.tid = t.tid
                ORDER BY e.created_at DESC 
                LIMIT ?
            '''
            self.db.cursor.execute(query, (limit,))
        
        return [dict(row) for row in self.db.cursor.fetchall()]
    
    def close(self):
        """Close database connection."""
        self.db.close()


def main():
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description='NGA Thread Monitor - Track threads for new posts',
        formatter_class=argparse.RawDescriptionHelpFormatter
    )
    
    subparsers = parser.add_subparsers(dest='command', help='Commands')
    
    # Add thread
    add_parser = subparsers.add_parser('add', help='Add thread to monitoring')
    add_parser.add_argument('--tid', type=int, required=True, help='Thread ID')
    add_parser.add_argument('--authors', type=str, help='Comma-separated author UIDs to monitor')
    add_parser.add_argument('--interval', type=int, default=300, help='Check interval in seconds')
    
    # Remove thread
    remove_parser = subparsers.add_parser('remove', help='Remove thread from monitoring')
    remove_parser.add_argument('--tid', type=int, required=True, help='Thread ID')
    
    # List monitored threads
    subparsers.add_parser('list', help='List monitored threads')
    
    # Check thread once
    check_parser = subparsers.add_parser('check', help='Check thread for new posts')
    check_parser.add_argument('--tid', type=int, help='Thread ID (omit to check all)')
    
    # Run monitoring loop
    loop_parser = subparsers.add_parser('loop', help='Run continuous monitoring')
    loop_parser.add_argument('--check-all-interval', type=int, default=10, 
                            help='Seconds between evaluating which threads need checking (default: 10)')
    
    # View events
    events_parser = subparsers.add_parser('events', help='View monitoring events')
    events_parser.add_argument('--tid', type=int, help='Thread ID (omit for all)')
    events_parser.add_argument('--limit', type=int, default=50, help='Number of events to show')
    
    # Sync from config
    sync_parser = subparsers.add_parser('sync', help='Load monitored threads from config file')
    sync_parser.add_argument('--config', type=str, default='config.json', help='Config file path')
    
    args = parser.parse_args()
    
    if not args.command:
        parser.print_help()
        return
    
    monitor = ThreadMonitor()
    
    try:
        if args.command == 'add':
            author_filter = [int(uid) for uid in args.authors.split(',')] if args.authors else None
            monitor.add_thread(args.tid, author_filter, args.interval)
        
        elif args.command == 'remove':
            monitor.remove_thread(args.tid)
        
        elif args.command == 'list':
            threads = monitor.list_monitored()
            if threads:
                print(f"\nMonitored threads ({len(threads)}):\n")
                for t in threads:
                    print(f"  TID {t['tid']}: {t['title']}")
                    print(f"    Author: {t['author_name']}")
                    print(f"    Filter: {t['author_filter'] or 'All authors'}")
                    print(f"    Interval: {t['check_interval']}s")
                    print(f"    Last checked: {t['last_checked'] or 'Never'}")
                    print()
            else:
                print("\nNo threads being monitored")
        
        elif args.command == 'check':
            if args.tid:
                monitor.check_thread(args.tid)
            else:
                monitor.check_all()
        
        elif args.command == 'loop':
            monitor.run_loop(args.check_all_interval)
        
        elif args.command == 'events':
            events = monitor.get_events(args.tid, args.limit)
            if events:
                print(f"\nMonitoring events ({len(events)}):\n")
                for e in events:
                    title_info = f" - {e.get('title', '')}" if 'title' in e else ""
                    print(f"  [{e['created_at']}] TID {e['tid']}{title_info}")
                    print(f"    {e['event_type']}: {e['message']} ({e['post_count']} posts)")
            else:
                print("\nNo events found")
        
        elif args.command == 'sync':
            monitor.load_from_config(args.config)
    
    finally:
        monitor.close()


if __name__ == '__main__':
    main()
