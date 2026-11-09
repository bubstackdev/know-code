#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
profile="${1:-debug}"
case "$profile" in debug) optimization=-O0 ;; release) optimization=-O2 ;; *) echo 'profile must be debug or release' >&2; exit 1 ;; esac
output=".artifacts/build/$profile/swift"
mkdir -p "$output"
xcrun clang -mmacosx-version-min=15.0 -std=c11 -Wall -Wextra -Werror "$optimization" providers/swift/indexstore-reader.c -o "$output/indexstore-reader"
