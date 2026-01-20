#!/usr/bin/env python3
"""
NGA BBS API Crawler
Crawls paginated posts from NGA forum threads using the app API.
Automatically fetches all pages using multi-threading.
"""

import argparse
import json
import sys
import time
import threading
from typing import Dict, Any, Optional, List
from concurrent.futures import ThreadPoolExecutor, as_completed
import requests


class NGACrawler:
    """Crawler for NGA BBS API with authentication and pagination support."""
    
    def __init__(self, config_path: str = "config/config.json"):
        """
        Initialize the NGA crawler.
        
        Args:
            config_path: Path to the configuration file containing auth cookies
        """
        self.base_url = "https://bbs.nga.cn/app_api.php"
        self.config = self._load_config(config_path)
        self.session = self._create_session()
        
        # Rate limiting setup
        self.rate_limit = self.config.get('rate_limit_per_minute', 30)
        self.min_interval = 60.0 / self.rate_limit  # Seconds between requests
        self.last_request_time = 0
        self.rate_limit_lock = threading.Lock()
    
    def _load_config(self, config_path: str) -> Dict[str, Any]:
        """
        Load authentication configuration from JSON file.
        
        Args:
            config_path: Path to config file
            
        Returns:
            Configuration dictionary
            
        Raises:
            SystemExit: If config file cannot be loaded
        """
        try:
            with open(config_path, 'r', encoding='utf-8') as f:
                config = json.load(f)
            
            # Validate required fields
            required_fields = ['ngaPassportUid', 'ngaPassportCid']
            
            # Set default values for optional fields
            if 'max_threads' not in config:
                config['max_threads'] = 5  # Default to 5 concurrent threads
            
            if 'rate_limit_per_minute' not in config:
                config['rate_limit_per_minute'] = 30  # Default to 30 requests/minute
            missing_fields = [field for field in required_fields if field not in config]
            
            if missing_fields:
                print(f"Error: Missing required fields in config: {', '.join(missing_fields)}", 
                      file=sys.stderr)
                sys.exit(1)
            
            return config
        except FileNotFoundError:
            print(f"Error: Config file not found: {config_path}", file=sys.stderr)
            print("Please create a config.json file based on config.example.json", file=sys.stderr)
            sys.exit(1)
        except json.JSONDecodeError as e:
            print(f"Error: Invalid JSON in config file: {e}", file=sys.stderr)
            sys.exit(1)
    
    def _create_session(self) -> requests.Session:
        """
        Create a requests session with authentication cookies and user agent.
        
        Returns:
            Configured requests Session
        """
        session = requests.Session()
        
        # Set user agent from config, or use Chrome as default
        default_user_agent = (
            'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
            '(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
        )
        user_agent = self.config.get('user_agent', default_user_agent)
        session.headers.update({'User-Agent': user_agent})
        
        # Set authentication cookies
        session.cookies.set('ngaPassportUid', self.config['ngaPassportUid'])
        session.cookies.set('ngaPassportCid', self.config['ngaPassportCid'])
        
        return session
    
    def _rate_limit(self):
        """Apply rate limiting before making a request."""
        with self.rate_limit_lock:
            current_time = time.time()
            time_since_last = current_time - self.last_request_time
            
            if time_since_last < self.min_interval:
                sleep_time = self.min_interval - time_since_last
                time.sleep(sleep_time)
            
            self.last_request_time = time.time()
    
    def fetch_page(self, tid: int, page: int) -> Optional[Dict[str, Any]]:
        """
        Fetch a single page of posts from a thread.
        Rate limited according to config.
        
        Args:
            tid: Thread ID
            page: Page number
            
        Returns:
            JSON response as dictionary, or None if request failed
        """
        # Apply rate limiting
        self._rate_limit()
        
        params = {
            '__lib': 'post',
            '__act': 'list'
        }
        
        data = {
            'tid': tid,
            'page': page
        }
        
        try:
            response = self.session.post(
                self.base_url,
                params=params,
                data=data,
                timeout=30
            )
            response.raise_for_status()
            
            return response.json()
        except requests.exceptions.RequestException as e:
            print(f"Error fetching page {page}: {e}", file=sys.stderr)
            return None
        except json.JSONDecodeError as e:
            print(f"Error decoding JSON response for page {page}: {e}", file=sys.stderr)
            return None
    
    def crawl_all_pages(self, tid: int) -> List[Dict[str, Any]]:
        """
        Automatically crawl all pages of a thread using multi-threading.
        
        Args:
            tid: Thread ID
            
        Returns:
            List of all page results
        """
        print(f"{'='*80}")
        print(f"Starting crawl for thread {tid}")
        print(f"{'='*80}\n")
        
        # Step 1: Fetch page 1 to get total page count
        print("[1/3] Fetching page 1 to determine total pages...")
        first_page = self.fetch_page(tid, 1)
        
        if first_page is None:
            print("Error: Failed to fetch page 1", file=sys.stderr)
            return []
        
        total_pages = first_page.get('totalPage', 1)
        current_page = first_page.get('currentPage', 1)
        total_posts = first_page.get('vrows', 0)
        thread_title = first_page.get('tsubject', 'Unknown')
        
        print(f"✓ Thread: {thread_title}")
        print(f"✓ Total pages: {total_pages}")
        print(f"✓ Total posts: {total_posts}")
        print(f"✓ Posts per page: {first_page.get('perPage', 20)}\n")
        
        # Store all results
        all_results = [first_page]
        
        if total_pages <= 1:
            print("Only 1 page found, crawl completed!\n")
            self._print_result(first_page, 1)
            return all_results
        
        # Step 2: Fetch remaining pages using thread pool
        remaining_pages = list(range(2, total_pages + 1))
        max_threads = self.config.get('max_threads', 5)
        
        print(f"[2/3] Fetching pages 2-{total_pages} using {max_threads} threads...")
        
        successful = 0
        failed = 0
        
        with ThreadPoolExecutor(max_workers=max_threads) as executor:
            # Submit all tasks
            future_to_page = {
                executor.submit(self.fetch_page, tid, page): page 
                for page in remaining_pages
            }
            
            # Collect results as they complete
            for future in as_completed(future_to_page):
                page_num = future_to_page[future]
                try:
                    result = future.result()
                    if result is not None:
                        all_results.append(result)
                        successful += 1
                        print(f"  ✓ Page {page_num}/{total_pages} completed")
                    else:
                        failed += 1
                        print(f"  ✗ Page {page_num}/{total_pages} failed", file=sys.stderr)
                except Exception as e:
                    failed += 1
                    print(f"  ✗ Page {page_num}/{total_pages} error: {e}", file=sys.stderr)
        
        print(f"\n[3/3] Crawl summary:")
        print(f"  ✓ Successful: {successful + 1}/{total_pages} pages")
        if failed > 0:
            print(f"  ✗ Failed: {failed}/{total_pages} pages")
        print()
        
        # Sort results by page number
        all_results.sort(key=lambda x: x.get('currentPage', 0))
        
        # Print all results
        for result in all_results:
            self._print_result(result, result.get('currentPage', 0))
        
        print(f"{'='*80}")
        print(f"Crawl completed: {len(all_results)}/{total_pages} pages retrieved")
        print(f"{'='*80}")
        
        return all_results
    
    def crawl_pages_range(self, tid: int, start_page: int, end_page: int) -> List[Optional[Dict[str, Any]]]:
        """
        Crawl a range of pages using multi-threading.
        Returns results in page order (may contain None for failed pages).
        
        Args:
            tid: Thread ID
            start_page: Starting page number (inclusive)
            end_page: Ending page number (inclusive)
            
        Returns:
            List of page results in order (same length as page range)
        """
        if start_page > end_page:
            return []
        
        pages = list(range(start_page, end_page + 1))
        max_threads = self.config.get('max_threads', 5)
        
        # Initialize results list with None
        results = [None] * len(pages)
        completed = 0
        total = len(pages)
        
        with ThreadPoolExecutor(max_workers=max_threads) as executor:
            # Submit all tasks
            future_to_index = {
                executor.submit(self.fetch_page, tid, page): i 
                for i, page in enumerate(pages)
            }
            
            # Collect results as they complete
            for future in as_completed(future_to_index):
                index = future_to_index[future]
                page_num = pages[index]
                try:
                    result = future.result()
                    results[index] = result
                    completed += 1
                    if result:
                        print(f"  ✓ Fetched page {page_num}/{end_page} ({completed}/{total})")
                    else:
                        print(f"  ✗ Failed to fetch page {page_num}/{end_page}")
                except Exception as e:
                    completed += 1
                    print(f"  ✗ Error fetching page {page_num}/{end_page}: {e}")
        
        return results
    
    def crawl_pages_range_with_callback(self, tid: int, start_page: int, end_page: int, callback, stop_event=None):
        """
        Crawl a range of pages and call callback immediately when each page completes.
        This allows processing (e.g., saving to DB) as pages arrive instead of waiting for all.
        
        Args:
            tid: Thread ID
            start_page: Starting page number (inclusive)
            end_page: Ending page number (inclusive)
            callback: Function(page_num, page_result) called when each page completes
            stop_event: Optional threading.Event to signal early stop
        """
        if start_page > end_page:
            return
        
        pages = list(range(start_page, end_page + 1))
        max_threads = self.config.get('max_threads', 5)
        
        completed = 0
        total = len(pages)
        
        with ThreadPoolExecutor(max_workers=max_threads) as executor:
            # Submit all tasks
            future_to_page = {
                executor.submit(self.fetch_page, tid, page): page 
                for page in pages
            }
            
            # Process results as they complete
            for future in as_completed(future_to_page):
                # Check if we should stop
                if stop_event and stop_event.is_set():
                    print(f"\n  ⚠ Fetch interrupted by stop signal ({completed}/{total} pages completed)")
                    # Cancel remaining futures
                    for f in future_to_page:
                        f.cancel()
                    return
                
                page_num = future_to_page[future]
                completed += 1
                try:
                    result = future.result()
                    print(f"  ✓ Fetched page {page_num}/{end_page} ({completed}/{total})")
                    # Call callback immediately with the result
                    callback(page_num, result)
                except Exception as e:
                    print(f"  ✗ Error fetching page {page_num}/{end_page}: {e}")
                    # Call callback with None to indicate failure
                    callback(page_num, None)
    
    def _print_result(self, result: Dict[str, Any], page_num: int):
        """Print a single page result."""
        print(f"{'='*80}")
        print(f"Page {page_num}")
        print(f"{'='*80}\n")
        print(json.dumps(result, indent=2, ensure_ascii=False))
        print()


def main():
    """Main entry point for the crawler."""
    parser = argparse.ArgumentParser(
        description='NGA BBS API Crawler - Automatically fetch all pages from NGA forum threads',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog='''
Examples:
  # Crawl all pages of a thread
  python nga_crawler.py --tid 12345
  
  # Use custom config file
  python nga_crawler.py --tid 12345 --config my_config.json
  
  # Crawl multiple threads
  python nga_crawler.py --tid 12345
  python nga_crawler.py --tid 67890
        '''
    )
    
    parser.add_argument(
        '--tid',
        type=int,
        required=True,
        help='Thread ID to crawl (automatically fetches all pages)'
    )
    
    parser.add_argument(
        '--config',
        type=str,
        default='config.json',
        help='Path to config file (default: config.json)'
    )
    
    args = parser.parse_args()
    
    # Initialize crawler and start crawling
    try:
        crawler = NGACrawler(config_path=args.config)
        results = crawler.crawl_all_pages(tid=args.tid)
        
        if not results:
            print("\nNo results retrieved.", file=sys.stderr)
            sys.exit(1)
    except KeyboardInterrupt:
        print("\n\nCrawl interrupted by user", file=sys.stderr)
        sys.exit(130)


if __name__ == '__main__':
    main()
