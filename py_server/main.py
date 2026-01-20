#!/usr/bin/env python3
"""
Main entry point for NGA Reminder.
"""
import sys
import os
import argparse

# Ensure src is in python path
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


def main():
    """Main entry point with command routing."""
    parser = argparse.ArgumentParser(
        description='NGA Reminder - Monitor NGA forum threads',
        formatter_class=argparse.RawDescriptionHelpFormatter
    )

    subparsers = parser.add_subparsers(dest='mode', help='Operation mode')

    # CLI mode (existing monitor commands)
    cli_parser = subparsers.add_parser('cli', help='Run CLI monitor commands')

    # Server mode (new API server)
    server_parser = subparsers.add_parser('server', help='Run API server')
    server_parser.add_argument('--host', type=str, default='127.0.0.1', help='Host to bind to')
    server_parser.add_argument('--port', type=int, default=8000, help='Port to bind to')
    server_parser.add_argument('--reload', action='store_true', help='Enable auto-reload')

    args, remaining = parser.parse_known_args()

    if args.mode == 'server':
        # Run FastAPI server
        try:
            import uvicorn
            import json
        except ImportError:
            print("Error: uvicorn not installed. Run: pip install -r requirements.txt", file=sys.stderr)
            sys.exit(1)

        # Read server config from config file
        config_path = 'config/config.json'
        default_host = '127.0.0.1'
        default_port = 8000

        if os.path.exists(config_path):
            try:
                with open(config_path, 'r', encoding='utf-8') as f:
                    config = json.load(f)
                    default_host = config.get('server_host', default_host)
                    default_port = config.get('server_port', default_port)
            except Exception as e:
                print(f"Warning: Could not read config file: {e}", file=sys.stderr)

        # Command-line args can override config file
        host = args.host if args.host != '127.0.0.1' else default_host
        port = args.port if args.port != 8000 else default_port

        print(f"Starting NGA Reminder API Server on {host}:{port}")
        print(f"API Documentation: http://{host}:{port}/docs")

        uvicorn.run(
            "src.api:app",
            host=host,
            port=port,
            reload=args.reload
        )
    else:
        # Default to CLI mode (existing monitor functionality)
        try:
            from src.monitor import main as monitor_main
        except ImportError as e:
            print(f"Error importing src.monitor: {e}", file=sys.stderr)
            print("Ensure you are running this script from the server directory.", file=sys.stderr)
            sys.exit(1)

        # Pass remaining args to monitor
        sys.argv = [sys.argv[0]] + remaining
        monitor_main()


if __name__ == '__main__':
    main()
