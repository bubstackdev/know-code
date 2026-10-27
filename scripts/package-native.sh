#!/usr/bin/env bash
# Build a native installer preview, preserving the local manifest exactly as release CI does.
set -euo pipefail
cd "$(dirname "$0")/.."
knowcode_dist_bin=${1:-dist}
knowcode_dist_host=$(rustc -vV | sed -n 's/^host: //p')
case "$knowcode_dist_host" in
    aarch64-apple-darwin|x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;;
    *) echo "unsupported release host: $knowcode_dist_host" >&2; exit 1 ;;
esac
knowcode_dist_tmp=$(mktemp -d)
trap 'rm -rf "$knowcode_dist_tmp"' EXIT
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} bash scripts/build-release "$knowcode_dist_bin" --artifacts=local --target="$knowcode_dist_host" --output-format=json --no-local-paths > "$knowcode_dist_tmp/native.json"
cp "$knowcode_dist_tmp/native.json" "target/distrib/$knowcode_dist_host-dist-manifest.json"
bash scripts/build-release "$knowcode_dist_bin" --artifacts=global --output-format=json --no-local-paths > "$knowcode_dist_tmp/global.json"
