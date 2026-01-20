#!/usr/bin/env python3
"""
FastAPI server for NGA Reminder.
Provides REST API for querying posts with background monitoring.
"""
from fastapi import FastAPI, Query, HTTPException
from fastapi.responses import JSONResponse
from typing import Optional, List, Dict, Any
import threading
import time
from contextlib import asynccontextmanager

from .monitor import ThreadMonitor
from .database import NGADatabase


# Global monitor instance and stop event
monitor: Optional[ThreadMonitor] = None
monitor_thread: Optional[threading.Thread] = None
monitor_stop_event: Optional[threading.Event] = None


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Lifespan context manager for startup and shutdown events."""
    global monitor, monitor_thread, monitor_stop_event
    
    # Startup: Initialize monitor and start background thread
    print("Starting NGA Monitor background thread...")
    
    # Create stop event
    monitor_stop_event = threading.Event()
    
    def run_monitor():
        """Background task to run the monitor loop."""
        global monitor
        try:
            # Create ThreadMonitor instance inside the thread to avoid SQLite threading issues
            monitor = ThreadMonitor()
            
            # Sync monitored threads from config file
            print("Syncing monitored threads from config...")
            sync_result = monitor.load_from_config(stop_event=monitor_stop_event)
            if 'error' in sync_result:
                print(f"Warning: {sync_result['error']}")
            else:
                print(f"Synced {sync_result.get('added', 0) + sync_result.get('updated', 0)} thread(s)")
            
            # Run with a 30-second check interval (default from monitor.py)
            monitor.run_loop(check_all_interval=30, stop_event=monitor_stop_event)
        except Exception as e:
            print(f"Monitor error: {e}")
            import traceback
            traceback.print_exc()
    
    monitor_thread = threading.Thread(target=run_monitor, daemon=True)
    monitor_thread.start()
    
    # Wait a bit for monitor to initialize
    time.sleep(0.5)
    print("Monitor started.")
    
    yield
    
    # Shutdown: Signal monitor to stop
    print("Shutting down monitor...")
    if monitor_stop_event:
        monitor_stop_event.set()
    
    # Wait for monitor thread to finish (with timeout)
    if monitor_thread and monitor_thread.is_alive():
        monitor_thread.join(timeout=10.0)
        if monitor_thread.is_alive():
            print("Monitor thread did not stop gracefully (daemon will terminate)")
        else:
            print("Monitor stopped gracefully.")


app = FastAPI(
    title="NGA Reminder API",
    description="REST API for querying NGA forum posts with background monitoring",
    version="1.0.0",
    lifespan=lifespan
)


@app.get("/")
async def root():
    """Root endpoint."""
    return {
        "message": "NGA Reminder API",
        "version": "1.0.0",
        "endpoints": {
            "posts": "/api/v1/posts",
            "docs": "/docs"
        }
    }


@app.get("/api/v1/posts")
async def get_posts(
    tid: int = Query(..., description="Thread ID"),
    start_post_number: int = Query(..., description="Start post number (exclusive)"),
    author_uid: Optional[int] = Query(None, description="Filter by author UID")
) -> List[Dict[str, Any]]:
    """
    Get posts from a thread after a specific post number.
    
    Args:
        tid: Thread ID
        start_post_number: Minimum post number (posts with post_number > this value)
        author_uid: Optional. Filter posts by author UID
        
    Returns:
        List of posts matching the criteria
    """
    try:
        db = NGADatabase('data/nga_data.db')
        posts = db.get_posts_after(tid, start_post_number, author_uid)
        db.close()
        
        return posts
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Database error: {str(e)}")


@app.get("/api/v1/threads")
async def list_monitored_threads() -> List[Dict[str, Any]]:
    """
    Get list of currently monitored threads.
    
    Returns:
        List of monitored threads with their configuration
    """
    if not monitor:
        raise HTTPException(status_code=503, detail="Monitor not initialized")
    
    try:
        threads = monitor.list_monitored()
        return threads
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Error: {str(e)}")


@app.get("/health")
async def health_check():
    """Health check endpoint."""
    return {
        "status": "healthy",
        "monitor_running": monitor is not None and monitor_thread is not None and monitor_thread.is_alive()
    }
