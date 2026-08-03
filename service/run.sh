#!/usr/bin/env bash

set -Eeuo pipefail

service_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
env_file="${service_dir}/.env"

if [[ ! -f "${env_file}" ]]; then
  echo "Error: environment file not found: ${env_file}" >&2
  exit 1
fi

set -a
# shellcheck disable=SC1090
source "${env_file}"
set +a

cd "${service_dir}"
exec cargo run -- all
