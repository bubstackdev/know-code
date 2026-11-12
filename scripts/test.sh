#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# Bash 3-compatible catalog. Preserve the historical all-check order.
task_all_checks='build-record rust cli protocol jsonrpc provider-source rust-navigation native-header managed-paths typescript typescript-classification node-preparation preparation-safety swift-toolchain sdk-metadata csharp-context csharp-target-grouping csharp-classification project-trust provider-failures e2e query-admission wal-pressure test-classification recovery upgrades mcp cursors workspace-api query-deadlines query-cancellation response-budget live coordinator discovery resources preparation resolution call-graph project-links dispatch rust-provider swift-provider swiftpm-provider xcode-provider languages'
task_quick_checks='protocol provider-source native-header typescript'
task_core_checks="$task_quick_checks build-record rust managed-paths cli project-trust e2e query-admission wal-pressure test-classification recovery upgrades mcp cursors workspace-api query-deadlines query-cancellation response-budget live coordinator discovery resources preparation call-graph"
task_extended_checks='jsonrpc rust-navigation typescript-classification node-preparation preparation-safety swift-toolchain sdk-metadata csharp-context csharp-target-grouping csharp-classification provider-failures resolution project-links dispatch rust-provider swift-provider swiftpm-provider xcode-provider languages'

usage() {
  cat <<'USAGE'
Usage: bash scripts/test.sh [debug|release] [quick|core|extended|all|CHECK ...]
       bash scripts/test.sh [debug|release] --list

Default: debug quick (no matching native build required).
core: required public TS/C#/native release baseline; extended: retained matrices
and experimental adapters. Select relevant extra checks explicitly; see
 docs/VALIDATION.md. All existing checks remain available through all.

Examples:
  bash scripts/test.sh
  bash scripts/test.sh jsonrpc build-record
  bash scripts/test.sh debug rust cursors
  bash scripts/test.sh release core
  bash scripts/test.sh release all

Artifact consumers verify the existing matching producer before first use and
at the end. This script does not build product artifacts automatically.
USAGE
}

list_checks() {
  printf 'quick: %s\ncore: %s\nextended: %s\nall: %s\n' \
    "$task_quick_checks" "$task_core_checks" "$task_extended_checks" "$task_all_checks"
}
validate_check_name() {
  case "$1" in
    ''|*[!a-z0-9-]*) printf 'Unknown check: %s (use --list)\n' "$1" >&2; exit 2 ;;
  esac
  case " $task_all_checks " in
    *" $1 "*) ;;
    *) printf 'Unknown check: %s (use --list)\n' "$1" >&2; exit 2 ;;
  esac
}

# Parse and reject the complete selection before HOME/toolchain/producers/tests.
task_profile=debug
case "${1:-}" in debug|release) task_profile=$1; shift ;; esac
case "${1:-}" in
  -h|--help) usage; exit 0 ;;
  --list)
    if [ "$#" -ne 1 ]; then usage >&2; exit 2; fi
    list_checks; exit 0 ;;
esac
if [ "$#" -eq 0 ]; then
  task_selected=$task_quick_checks
elif [ "$#" -eq 1 ]; then
  case "$1" in
    quick) task_selected=$task_quick_checks ;;
    core) task_selected=$task_core_checks ;;
    extended) task_selected=$task_extended_checks ;;
    all) task_selected=$task_all_checks ;;
    *) validate_check_name "$1"; task_selected=$1 ;;
  esac
else
  for task_check in "$@"; do validate_check_name "$task_check"; done
  task_selected="$*"
fi

if [ -n "${HOME:-}" ]; then
  export PATH="$HOME/.cargo/bin:$HOME/.dotnet:$HOME/.local/bin:$PATH"
fi
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
export KNOW_CODE_BUILD_PROFILE="$task_profile"
case "$task_profile" in
  debug) task_cargo_profile=test; task_managed_profile=Debug ;;
  release) task_cargo_profile=release; task_managed_profile=Release ;;
esac

run_check() {
  case "$1" in
    build-record) python3 tests/build_record.py ;;
    rust) cargo test --workspace --locked --profile "$task_cargo_profile" ;;
    cli) python3 tests/cli.py ;;
    protocol) node tests/protocol.mjs ;;
    jsonrpc) node tests/jsonrpc.mjs ;;
    provider-source) node tests/provider_source.mjs ;;
    rust-navigation) node tests/rust_navigation.mjs ;;
    native-header)
      clang -Wall -Werror -fsyntax-only -x c tests/native_header.c
      clang++ -Wall -Werror -fsyntax-only -x c++ tests/native_header.c
      ;;
    managed-paths) dotnet "bindings/dotnet/Smoke/bin/$task_managed_profile/net10.0/Smoke.dll" --path-validation ;;
    typescript) node tests/typescript.mjs ;;
    typescript-classification) node tests/typescript_test_classification.mjs ;;
    node-preparation) node tests/node_preparation.mjs ;;
    preparation-safety) node tests/preparation_safety.mjs ;;
    swift-toolchain) node tests/swift_toolchain.mjs ;;
    sdk-metadata) node tests/sdk-metadata.mjs ;;
    csharp-context) node tests/csharp_context.mjs ;;
    csharp-target-grouping) node tests/csharp_target_grouping.mjs ;;
    csharp-classification) node tests/csharp_classification.mjs ;;
    project-trust) python3 tests/project_trust.py ;;
    provider-failures) python3 tests/provider_failures.py ;;
    e2e) python3 tests/e2e.py ;;
    query-admission) python3 tests/query_admission.py ;;
    wal-pressure) python3 tests/wal_pressure.py ;;
    test-classification) python3 tests/test_classification.py ;;
    recovery) python3 tests/recovery.py ;;
    upgrades) python3 tests/upgrades.py ;;
    mcp) python3 tests/mcp.py ;;
    cursors) python3 tests/cursors.py ;;
    workspace-api) python3 tests/workspace_api.py ;;
    query-deadlines) python3 tests/query_deadlines.py ;;
    query-cancellation) python3 tests/query_cancellation.py ;;
    response-budget) python3 tests/response_budget.py ;;
    live) python3 tests/live.py ;;
    coordinator) python3 tests/coordinator.py ;;
    discovery) python3 tests/discovery.py ;;
    resources) python3 tests/resources.py ;;
    preparation) python3 tests/preparation.py ;;
    resolution) python3 tests/resolution.py ;;
    call-graph) python3 tests/call_graph.py ;;
    project-links) python3 tests/project_links.py ;;
    dispatch) python3 tests/dispatch.py ;;
    rust-provider) python3 tests/rust_provider.py ;;
    swift-provider) python3 tests/swift_provider.py ;;
    swiftpm-provider) python3 tests/swiftpm_provider.py ;;
    xcode-provider) python3 tests/xcode_provider.py ;;
    languages) python3 tests/languages.py ;;
  esac
}

task_verified=0
for task_check in $task_all_checks; do
  case " $task_selected " in *" $task_check "*) ;; *) continue ;; esac
  # These checks load source or owned fixtures, not product build outputs.
  case "$task_check" in
    build-record|rust|protocol|jsonrpc|provider-source|rust-navigation|native-header|typescript|typescript-classification|node-preparation|preparation-safety|swift-toolchain|rust-provider) ;;
    *)
      if [ "$task_verified" -eq 0 ]; then
        printf '\n[producer] verify %s\n' "$task_profile"
        python3 scripts/build_record.py verify "$task_profile" >/dev/null
        task_verified=1
      fi
      ;;
  esac
  printf '\n[check] %s (%s)\n' "$task_check" "$task_profile"
  task_started=$SECONDS
  # Direct call: an if/! wrapper would disable errexit inside this function.
  run_check "$task_check"
  printf '[passed] %s (%ss)\n' "$task_check" "$((SECONDS - task_started))"
done
if [ "$task_verified" -eq 1 ]; then
  printf '\n[producer] verify %s after selected checks\n' "$task_profile"
  python3 scripts/build_record.py verify "$task_profile" >/dev/null
fi
printf '\nSelected checks passed (%s): %s\n' "$task_profile" "$task_selected"
