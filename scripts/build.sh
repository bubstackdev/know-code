#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$HOME/.dotnet:$HOME/.local/bin:$PATH"
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
export MACOSX_DEPLOYMENT_TARGET=15.0
task_build_profile="${1:-debug}"
case "$task_build_profile" in
  debug) task_dotnet_configuration=Debug; task_cargo_profile=dev ;;
  release) task_dotnet_configuration=Release; task_cargo_profile=release ;;
  *) echo "Usage: bash scripts/build.sh [debug|release]" >&2; exit 2 ;;
esac
for tool in cargo dotnet node npm; do
  command -v "$tool" >/dev/null || { echo "Missing $tool: install it and put it on PATH." >&2; exit 1; }
done
npm ci --ignore-scripts --no-audit --no-fund
cargo build --workspace --locked --profile "$task_cargo_profile"
bash providers/swift/build.sh "$task_build_profile"
bash scripts/generate-protocol.sh
dotnet restore providers/dotnet/KnowCode.Provider.DotNet.csproj --locked-mode --nologo
dotnet build providers/dotnet/KnowCode.Provider.DotNet.csproj --no-restore --nologo --configuration "$task_dotnet_configuration"
dotnet restore bindings/dotnet/Smoke/Smoke.csproj --locked-mode --nologo
dotnet build bindings/dotnet/Smoke/Smoke.csproj --no-restore --nologo --configuration "$task_dotnet_configuration"
task_node_stage=".artifacts/build/$task_build_profile/node"
mkdir -p "$task_node_stage"
cp bindings/node/{index.mjs,index.d.ts,smoke.mjs,package.json} "$task_node_stage/"
cp "target/$task_build_profile/libknow_code_node.dylib" "$task_node_stage/know_code.node"
cp "target/$task_build_profile/libknow_code_ffi.dylib" "bindings/dotnet/Smoke/bin/$task_dotnet_configuration/net10.0/"
if [[ "$task_build_profile" == debug ]]; then
  cp target/debug/libknow_code_node.dylib bindings/node/know_code.node
fi
