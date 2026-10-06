#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
command -v docker >/dev/null || { echo 'Docker required' >&2; exit 1; }
command -v python3 >/dev/null || { echo 'Python 3 required' >&2; exit 1; }
exec python3 scripts/db-concurrency-test.py
