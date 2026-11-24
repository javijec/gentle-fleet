#!/usr/bin/env bash
set -eu
exec python3 "$(dirname "$0")/local_ci.py" --profile "${1:-full}"
