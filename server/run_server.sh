#!/bin/bash
# Convenience script to run the server with venv

cd "$(dirname "$0")"
source venv/bin/activate
python main.py server "$@"
