#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$HOME/.dotnet:$HOME/.local/bin:$PATH"
exec python3 scripts/build_record.py build "${1:-debug}"
