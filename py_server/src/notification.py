#!/usr/bin/env python3
"""
Notification interface and implementations for NGA monitor.
"""

from abc import ABC, abstractmethod
from typing import Dict, Any, List
import requests


class NotificationSender(ABC):
    """Abstract base class for notification senders."""
    
    @abstractmethod
    def send(self, title: str, message: str, **kwargs) -> bool:
        """
        Send a notification.
        
        Args:
            title: Notification title
            message: Notification message
            **kwargs: Additional sender-specific parameters
            
        Returns:
            True if sent successfully, False otherwise
        """
        pass
    
    @abstractmethod
    def is_configured(self) -> bool:
        """
        Check if the sender is properly configured.
        
        Returns:
            True if configured and ready to send
        """
        pass


class BarkNotificationSender(NotificationSender):
    """Bark notification sender implementation."""
    
    def __init__(self, config: Dict[str, Any]):
        """
        Initialize Bark sender.
        
        Args:
            config: Configuration dictionary with bark settings
        """
        self.server_url = config.get('bark_server_url', '')
        self.device_key = config.get('bark_device_key', '')
        self.sound = config.get('bark_sound', 'default')
        self.group = config.get('bark_group', 'NGA')
        self.icon = config.get('bark_icon', '')
        self.timeout = config.get('bark_timeout', 10)
    
    def is_configured(self) -> bool:
        """Check if Bark is configured."""
        return bool(self.server_url and self.device_key)
    
    def send(self, title: str, message: str, **kwargs) -> bool:
        """
        Send notification via Bark.
        
        Args:
            title: Notification title
            message: Notification message
            **kwargs: Optional parameters:
                - url: URL to open when notification is tapped
                - sound: Override default sound
                - group: Override default group
                - icon: Override default icon
                
        Returns:
            True if sent successfully
        """
        if not self.is_configured():
            print("Bark not configured, skipping notification")
            return False
        
        try:
            # Build Bark API URL
            # Format: http://server/device_key/title/message
            api_url = f"{self.server_url.rstrip('/')}/{self.device_key}"
            
            # Prepare parameters
            params = {
                'title': title,
                'body': message,
                'sound': kwargs.get('sound', self.sound),
                'group': kwargs.get('group', self.group)
            }
            
            # Optional parameters
            if kwargs.get('url'):
                params['url'] = kwargs['url']
            
            icon = kwargs.get('icon', self.icon)
            if icon:
                params['icon'] = icon
            
            # Send request
            response = requests.get(api_url, params=params, timeout=self.timeout)
            response.raise_for_status()
            
            # Check response
            result = response.json()
            if result.get('code') == 200:
                return True
            else:
                print(f"Bark API error: {result}")
                return False
                
        except requests.exceptions.RequestException as e:
            print(f"Failed to send Bark notification: {e}")
            return False
        except Exception as e:
            print(f"Error sending Bark notification: {e}")
            return False


class ConsoleNotificationSender(NotificationSender):
    """Console notification sender for testing/debugging."""
    
    def __init__(self, config: Dict[str, Any] = None):
        """Initialize console sender."""
        self.enabled = config.get('console_notification_enabled', True) if config else True
    
    def is_configured(self) -> bool:
        """Console sender is always configured."""
        return self.enabled
    
    def send(self, title: str, message: str, **kwargs) -> bool:
        """
        Print notification to console.
        
        Args:
            title: Notification title
            message: Notification message
            **kwargs: Ignored for console sender
            
        Returns:
            Always True
        """
        if not self.enabled:
            return False
        
        print(f"\n{'='*80}")
        print(f"📱 NOTIFICATION")
        print(f"{'='*80}")
        print(f"Title: {title}")
        print(f"Message: {message}")
        if kwargs.get('url'):
            print(f"URL: {kwargs['url']}")
        print(f"{'='*80}\n")
        return True


class NotificationManager:
    """Manages multiple notification senders."""
    
    def __init__(self, config: Dict[str, Any]):
        """
        Initialize notification manager.
        
        Args:
            config: Configuration dictionary
        """
        self.senders: List[NotificationSender] = []
        
        # Initialize Bark sender if configured
        if config.get('bark_enabled', False):
            bark_sender = BarkNotificationSender(config)
            if bark_sender.is_configured():
                self.senders.append(bark_sender)
        
        # Always add console sender for debugging (can be disabled in config)
        console_sender = ConsoleNotificationSender(config)
        if console_sender.is_configured():
            self.senders.append(console_sender)
    
    def send(self, title: str, message: str, **kwargs) -> int:
        """
        Send notification via all configured senders.
        
        Args:
            title: Notification title
            message: Notification message
            **kwargs: Additional parameters passed to senders
            
        Returns:
            Number of successful sends
        """
        success_count = 0
        for sender in self.senders:
            if sender.send(title, message, **kwargs):
                success_count += 1
        return success_count
    
    def has_senders(self) -> bool:
        """Check if any senders are configured."""
        return len(self.senders) > 0


if __name__ == '__main__':
    # Example usage
    config = {
        'bark_enabled': True,
        'bark_server_url': 'https://api.day.app',
        'bark_device_key': 'your_device_key_here',
        'bark_sound': 'bell',
        'bark_group': 'NGA Monitor'
    }
    
    manager = NotificationManager(config)
    
    # Send test notification
    manager.send(
        title="New Post Alert",
        message="用户 -阿狼- 发表了新帖子",
        url="https://bbs.nga.cn/read.php?tid=12345"
    )
