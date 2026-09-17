#!/usr/bin/env bash
set -euo pipefail

service_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export CGO_ENABLED=0
go -C "$service_dir/tools/pg-migrate" build -o "$service_dir/bin/pg-migrate" .
exec "$service_dir/bin/pg-migrate" "$@"
