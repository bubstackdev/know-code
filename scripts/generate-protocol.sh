#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Reuse Cargo's pinned native ARM64 protoc; no Intel-only Grpc.Tools dependency.
task_protoc="$(cargo metadata --offline --locked --filter-platform aarch64-apple-darwin --format-version 1 | python3 -c 'import json,sys,pathlib; p=next(p for p in json.load(sys.stdin)["packages"] if p["name"]=="protoc-bin-vendored-macos-aarch_64"); print(pathlib.Path(p["manifest_path"]).parent/"bin/protoc")')"
mkdir -p providers/dotnet/obj/protocol
"$task_protoc" --proto_path=proto --csharp_out=providers/dotnet/obj/protocol proto/analysis.proto
