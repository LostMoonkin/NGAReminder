import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from src.database import NGADatabase

def verify():
    db = NGADatabase('data/nga_data.db')
    
    # Check schema
    db.cursor.execute("PRAGMA table_info(posts)")
    columns = [row[1] for row in db.cursor.fetchall()]
    if 'post_number' in columns:
        print("PASS: post_number column exists")
    else:
        print("FAIL: post_number column missing")
        return

    # Check data insert
    post = {
        'pid': 999999,
        'tid': 12345,
        'fid': 1,
        'author_name': 'TestUser',
        'author_uid': 100,
        'post_date': '2024-01-01 12:00',
        'post_timestamp': 1704096000,
        'content': 'Test content',
        'post_number': 42
    }
    
    # Mock thread for foreign key constraint
    thread = {
        'tid': 12345,
        'title': 'Test Thread',
        'author_name': 'TestUser',
        'author_uid': 100
    }
    db.save_thread(thread)

    db.save_post(post)
    
    db.cursor.execute("SELECT post_number FROM posts WHERE pid = 999999")
    row = db.cursor.fetchone()
    if row and row['post_number'] == 42:
        print("PASS: post_number saved correctly")
    else:
        print(f"FAIL: post_number not saved correctly. Got: {row['post_number'] if row else 'None'}")
        
    db.close()

if __name__ == '__main__':
    verify()
