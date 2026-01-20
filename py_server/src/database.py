#!/usr/bin/env python3
"""
Database manager for NGA BBS crawler.
Handles SQLite database operations for storing threads and posts.
"""

import sqlite3
from typing import Dict, Any, List, Optional
from datetime import datetime
import os


class NGADatabase:
    """SQLite database manager for NGA BBS data."""
    
    def __init__(self, db_path: str = "nga_data.db"):
        """
        Initialize database connection.
        
        Args:
            db_path: Path to SQLite database file
        """
        self.db_path = db_path
        self.conn = None
        self.cursor = None
        self._connect()
        self._init_schema()
    
    def _connect(self):
        """Establish database connection."""
        self.conn = sqlite3.connect(self.db_path)
        self.conn.row_factory = sqlite3.Row  # Enable column access by name
        self.cursor = self.conn.cursor()
    
    def _init_schema(self):
        """Initialize database schema from schema.sql file."""
        # ../data/schema.sql relative to database.py
        schema_path = os.path.join(os.path.dirname(os.path.dirname(__file__)), 'data', 'schema.sql')
        
        if os.path.exists(schema_path):
            with open(schema_path, 'r', encoding='utf-8') as f:
                schema_sql = f.read()
                self.cursor.executescript(schema_sql)
                self.conn.commit()
        else:
            # Fallback: create tables inline if schema.sql doesn't exist
            self._create_tables_inline()
    
    def _create_tables_inline(self):
        """Create tables inline if schema.sql is not found."""
        self.cursor.executescript('''
            CREATE TABLE IF NOT EXISTS threads (
                tid INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                author_name TEXT NOT NULL,
                author_uid INTEGER NOT NULL,
                total_posts INTEGER DEFAULT 0,
                total_pages INTEGER DEFAULT 0,
                created_at TIMESTAMP DEFAULT (datetime('now', 'localtime')),
                updated_at TIMESTAMP DEFAULT (datetime('now', 'localtime'))
            );
            
            CREATE TABLE IF NOT EXISTS posts (
                pid INTEGER PRIMARY KEY,
                tid INTEGER NOT NULL,
                fid INTEGER NOT NULL,
                author_name TEXT NOT NULL,
                author_uid INTEGER NOT NULL,
                post_date TEXT NOT NULL,
                post_timestamp INTEGER NOT NULL,
                content TEXT,
                post_number INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT (datetime('now', 'localtime')),
                FOREIGN KEY (tid) REFERENCES threads(tid) ON DELETE CASCADE
            );
            
            CREATE INDEX IF NOT EXISTS idx_posts_tid ON posts(tid);
            CREATE INDEX IF NOT EXISTS idx_posts_author_uid ON posts(author_uid);
            CREATE INDEX IF NOT EXISTS idx_posts_timestamp ON posts(post_timestamp);
            CREATE INDEX IF NOT EXISTS idx_threads_author_uid ON threads(author_uid);
        ''')
        self.conn.commit()
    
    def save_thread(self, thread_data: Dict[str, Any]) -> bool:
        """
        Save or update thread information.
        
        Args:
            thread_data: Dictionary containing thread metadata
            
        Returns:
            True if successful, False otherwise
        """
        try:
            self.cursor.execute('''
                INSERT INTO threads (
                    tid, title, author_name, author_uid,
                    total_posts, total_pages, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?, datetime('now', 'localtime'))
                ON CONFLICT(tid) DO UPDATE SET
                    title = excluded.title,
                    total_posts = excluded.total_posts,
                    total_pages = excluded.total_pages,
                    updated_at = datetime('now', 'localtime')
            ''', (
                thread_data['tid'],
                thread_data['title'],
                thread_data['author_name'],
                thread_data['author_uid'],
                thread_data.get('total_posts', 0),
                thread_data.get('total_pages', 0)
            ))
            self.conn.commit()
            return True
        except Exception as e:
            print(f"Error saving thread: {e}")
            self.conn.rollback()
            return False
    
    def save_post(self, post_data: Dict[str, Any]) -> bool:
        """
        Save a single post.
        
        Args:
            post_data: Dictionary containing post data
            
        Returns:
            True if successful, False otherwise
        """
        try:
            self.cursor.execute('''
                INSERT OR REPLACE INTO posts (
                    pid, tid, fid, author_name, author_uid, post_date,
                    post_timestamp, content, post_number
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                post_data['pid'],
                post_data['tid'],
                post_data['fid'],
                post_data['author_name'],
                post_data['author_uid'],
                post_data['post_date'],
                post_data['post_timestamp'],
                post_data.get('content', ''),
                post_data.get('post_number', 0)
            ))
            self.conn.commit()
            return True
        except Exception as e:
            print(f"Error saving post {post_data.get('pid')}: {e}")
            self.conn.rollback()
            return False
    
    def save_posts_batch(self, posts: List[Dict[str, Any]]) -> int:
        """
        Save multiple posts in a batch.
        
        Args:
            posts: List of post dictionaries
            
        Returns:
            Number of posts successfully saved
        """
        saved_count = 0
        try:
            for post_data in posts:
                self.cursor.execute('''
                    INSERT OR REPLACE INTO posts (
                        pid, tid, fid, author_name, author_uid, post_date,
                        post_timestamp, content, post_number
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ''', (
                    post_data['pid'],
                    post_data['tid'],
                    post_data['fid'],
                    post_data['author_name'],
                    post_data['author_uid'],
                    post_data['post_date'],
                    post_data['post_timestamp'],
                    post_data.get('content', ''),
                    post_data.get('post_number', 0)
                ))
                saved_count += 1
            self.conn.commit()
        except Exception as e:
            print(f"Error in batch save: {e}")
            self.conn.rollback()
        
        return saved_count
    
    def get_thread(self, tid: int) -> Optional[Dict[str, Any]]:
        """
        Get thread information by TID.
        
        Args:
            tid: Thread ID
            
        Returns:
            Thread data dictionary or None
        """
        self.cursor.execute('SELECT * FROM threads WHERE tid = ?', (tid,))
        row = self.cursor.fetchone()
        return dict(row) if row else None
    
    def get_posts_by_thread(self, tid: int, limit: Optional[int] = None) -> List[Dict[str, Any]]:
        """
        Get all posts for a thread.
        
        Args:
            tid: Thread ID
            limit: Optional limit on number of posts
            
        Returns:
            List of post dictionaries
        """
        query = 'SELECT * FROM posts WHERE tid = ? ORDER BY post_timestamp'
        if limit:
            query += f' LIMIT {limit}'
        
        self.cursor.execute(query, (tid,))
        return [dict(row) for row in self.cursor.fetchall()]
    
    def get_posts_by_author(self, author_uid: int, limit: Optional[int] = None) -> List[Dict[str, Any]]:
        """
        Get posts by a specific author.
        
        Args:
            author_uid: Author's UID
            limit: Optional limit on number of posts
            
        Returns:
            List of post dictionaries
        """
        query = 'SELECT * FROM posts WHERE author_uid = ? ORDER BY post_timestamp DESC'
        if limit:
            query += f' LIMIT {limit}'
        
        self.cursor.execute(query, (author_uid,))
        return [dict(row) for row in self.cursor.fetchall()]
    
    def get_posts_after(self, tid: int, start_post_number: int, author_uid: Optional[int] = None) -> List[Dict[str, Any]]:
        """
        Get posts after a specific post number with optional author filter.
        
        Args:
            tid: Thread ID
            start_post_number: Minimum post number (exclusive)
            author_uid: Optional author UID filter
            
        Returns:
            List of post dictionaries
        """
        if author_uid:
            query = '''
                SELECT * FROM posts 
                WHERE tid = ? AND post_number > ? AND author_uid = ?
                ORDER BY post_number
            '''
            self.cursor.execute(query, (tid, start_post_number, author_uid))
        else:
            query = '''
                SELECT * FROM posts 
                WHERE tid = ? AND post_number > ?
                ORDER BY post_number
            '''
            self.cursor.execute(query, (tid, start_post_number))
        
        return [dict(row) for row in self.cursor.fetchall()]
    
    def get_thread_stats(self) -> List[Dict[str, Any]]:
        """
        Get statistics for all threads.
        
        Returns:
            List of thread statistics
        """
        self.cursor.execute('SELECT * FROM thread_stats')
        return [dict(row) for row in self.cursor.fetchall()]
    
    def search_posts(self, keyword: str, limit: int = 100) -> List[Dict[str, Any]]:
        """
        Search posts by keyword in content.
        
        Args:
            keyword: Search keyword
            limit: Maximum number of results
            
        Returns:
            List of matching posts
        """
        self.cursor.execute('''
            SELECT * FROM posts 
            WHERE content LIKE ? 
            ORDER BY post_timestamp DESC 
            LIMIT ?
        ''', (f'%{keyword}%', limit))
        return [dict(row) for row in self.cursor.fetchall()]
    
    def close(self):
        """Close database connection."""
        if self.conn:
            self.conn.close()
    
    def __enter__(self):
        """Context manager entry."""
        return self
    
    def __exit__(self, exc_type, exc_val, exc_tb):
        """Context manager exit."""
        self.close()


def parse_page_result(page_data: Dict[str, Any]) -> tuple[Dict[str, Any], List[Dict[str, Any]]]:
    """
    Parse API page result into thread and post data.
    
    Args:
        page_data: Raw API response for a page
        
    Returns:
        Tuple of (thread_dict, list_of_post_dicts)
    """
    # Extract thread information from top-level response
    # Note: tid is not at top level, need to get from first post
    first_post = page_data.get('result', [{}])[0] if page_data.get('result') else {}
    
    thread = {
        'tid': first_post.get('tid', 0),  # Get tid from first post
        'title': page_data.get('tsubject', ''),
        'author_name': page_data.get('tauthor', ''),
        'author_uid': page_data.get('tauthorid', 0),
        'total_posts': page_data.get('vrows', 0),
        'total_pages': page_data.get('totalPage', 0)
    }
    
    # Extract posts
    posts = []
    for post in page_data.get('result', []):
        author = post.get('author', {})
        post_dict = {
            'pid': post.get('pid', 0),
            'tid': post.get('tid', 0),
            'fid': post.get('fid', 0),
            'author_name': author.get('username', ''),
            'author_uid': author.get('uid', 0),
            'post_date': post.get('postdate', ''),
            'post_date': post.get('postdate', ''),
            'post_timestamp': post.get('postdatetimestamp', 0),
            'content': post.get('content', ''),
            'post_number': post.get('lou', 0)
        }
        posts.append(post_dict)
    
    return thread, posts


if __name__ == '__main__':
    # Example usage
    db = NGADatabase('test.db')
    
    # Example thread
    thread = {
        'tid': 12345,
        'title': 'Test Thread',
        'author_name': 'TestUser',
        'author_uid': 100,
        'total_posts': 100,
        'total_pages': 5
    }
    
    db.save_thread(thread)
    print("Thread saved!")
    
    # Example post
    post = {
        'pid': 1,
        'tid': 12345,
        'fid': 1,
        'author_name': 'TestUser',
        'author_uid': 100,
        'post_date': '2024-01-01 12:00',
        'post_timestamp': 1704096000,
        'content': 'This is a test post'
    }
    
    db.save_post(post)
    print("Post saved!")
    
    # Query
    retrieved = db.get_thread(12345)
    print(f"Retrieved thread: {retrieved}")
    
    db.close()
