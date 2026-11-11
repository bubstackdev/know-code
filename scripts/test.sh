#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$HOME/.dotnet:$HOME/.local/bin:$PATH"
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
export KNOW_CODE_BUILD_PROFILE="${1:-debug}"
case "$KNOW_CODE_BUILD_PROFILE" in
  debug) task_cargo_profile=test ;;
  release) task_cargo_profile=release ;;
  *) echo "Usage: bash scripts/test.sh [debug|release]" >&2; exit 2 ;;
esac
python3 tests/build_record.py
cargo test --workspace --locked --profile "$task_cargo_profile"
node tests/protocol.mjs
node tests/jsonrpc.mjs
node tests/typescript.mjs
node tests/node_preparation.mjs
node tests/swift_toolchain.mjs
node tests/sdk-metadata.mjs
node tests/csharp_context.mjs
node tests/csharp_target_grouping.mjs
python3 tests/e2e.py
python3 tests/query_admission.py
python3 tests/test_classification.py
python3 tests/recovery.py
python3 tests/upgrades.py
python3 tests/mcp.py
python3 tests/cursors.py
python3 tests/workspace_api.py
python3 tests/query_deadlines.py
python3 tests/query_cancellation.py
python3 tests/response_budget.py
python3 tests/live.py
python3 tests/coordinator.py
python3 tests/discovery.py
python3 tests/resources.py
python3 tests/preparation.py
python3 tests/resolution.py
python3 tests/call_graph.py
python3 tests/project_links.py
python3 tests/dispatch.py
python3 tests/rust_provider.py
python3 tests/swift_provider.py
python3 tests/swiftpm_provider.py
python3 tests/xcode_provider.py
python3 tests/languages.py
