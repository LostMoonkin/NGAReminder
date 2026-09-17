#!/usr/bin/env bash
set -euo pipefail

service_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

# 无参数时固定数据目录基准；显式参数保留调用者的工作目录和路径含义。
if [[ $# -eq 0 ]]; then
  cd -- "$service_dir"
  if [[ -f config.json ]]; then
    set -- -config "$service_dir/config.json"
  fi
fi

export CGO_ENABLED=0
go -C "$service_dir" build -o bin/nga-reminder ./cmd/server

# 替换 shell，使退出码和停止信号直接交给服务进程处理。
exec "$service_dir/bin/nga-reminder" "$@"
