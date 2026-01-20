#!/usr/bin/env python3
"""
Test script to verify the API functionality.
"""
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from src.database import NGADatabase

def test_get_posts_after():
    """Test the get_posts_after method."""
    db = NGADatabase('data/nga_data.db')
    
    # Create test data
    thread = {
        'tid': 12345,
        'title': 'Test Thread',
        'author_name': 'TestUser',
        'author_uid': 100
    }
    db.save_thread(thread)
    
    # Create test posts
    posts = [
        {
            'pid': 1,
            'tid': 12345,
            'fid': 1,
            'author_name': 'User1',
            'author_uid': 100,
            'post_date': '2024-01-01 12:00',
            'post_timestamp': 1704096000,
            'content': 'Post 1',
            'post_number': 1
        },
        {
            'pid': 2,
            'tid': 12345,
            'fid': 1,
            'author_name': 'User2',
            'author_uid': 200,
            'post_date': '2024-01-01 12:01',
            'post_timestamp': 1704096060,
            'content': 'Post 2',
            'post_number': 2
        },
        {
            'pid': 3,
            'tid': 12345,
            'fid': 1,
            'author_name': 'User1',
            'author_uid': 100,
            'post_date': '2024-01-01 12:02',
            'post_timestamp': 1704096120,
            'content': 'Post 3',
            'post_number': 3
        },
    ]
    
    for post in posts:
        db.save_post(post)
    
    # Test 1: Get all posts after post_number 0
    result = db.get_posts_after(12345, 0)
    assert len(result) == 3, f"Expected 3 posts, got {len(result)}"
    print("✓ Test 1 passed: Get all posts after 0")
    
    # Test 2: Get posts after post_number 1
    result = db.get_posts_after(12345, 1)
    assert len(result) == 2, f"Expected 2 posts, got {len(result)}"
    assert result[0]['post_number'] == 2, "Expected first post to be post_number 2"
    print("✓ Test 2 passed: Get posts after 1")
    
    # Test 3: Get posts after post_number 1 filtered by author_uid 100
    result = db.get_posts_after(12345, 1, author_uid=100)
    assert len(result) == 1, f"Expected 1 post, got {len(result)}"
    assert result[0]['post_number'] == 3, "Expected post_number 3"
    assert result[0]['author_uid'] == 100, "Expected author_uid 100"
    print("✓ Test 3 passed: Get posts after 1 filtered by author_uid 100")
    
    # Test 4: Get posts after post_number 3 (should be empty)
    result = db.get_posts_after(12345, 3)
    assert len(result) == 0, f"Expected 0 posts, got {len(result)}"
    print("✓ Test 4 passed: Get posts after 3 (empty result)")
    
    db.close()
    print("\n✓ All tests passed!")

if __name__ == '__main__':
    test_get_posts_after()
