#!/bin/sh
# Check the installed (`dist` profile) binary against the budget in size-budget.txt.
#
# Usage: scripts/check-size.sh [--no-build [BINARY]]
#
# Builds `target/dist/knowcode` (unless --no-build; then BINARY, if given, is measured instead, as CI measures the
# binary it packaged), then exits 1 if it is more than 1% over the budget. If it is
# more than 1% under, it passes and prints the new budget to commit, so the budget only ever ratchets down.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
budget_file="$root/size-budget.txt"
bin="${2:-${CARGO_TARGET_DIR:-$root/target}/dist/knowcode}"
tolerance_pct=1

if [ "${1:-}" != "--no-build" ]; then
    cargo build --profile dist --quiet -p knowcode --manifest-path "$root/Cargo.toml"
fi
[ -f "$bin" ] || { echo "check-size: $bin not found; build it or drop --no-build" >&2; exit 2; }

budget=$(grep -v '^#' "$budget_file" | tr -d ' \t' | grep -m1 .)
size=$(wc -c < "$bin" | tr -d ' ')
over=$((budget + budget * tolerance_pct / 100))
under=$((budget - budget * tolerance_pct / 100))
mb() { awk -v b="$1" 'BEGIN { printf "%.2f MB", b / 1e6 }'; }
delta=$((size - budget))

if [ "$size" -gt "$over" ]; then
    echo "check-size: FAIL: knowcode is $size bytes ($(mb "$size")), budget $budget ($(mb "$budget")), +$delta bytes." >&2
    echo "  Shrink it, or raise size-budget.txt to $size in the commit that grew it, with the reason in the message." >&2
    exit 1
elif [ "$size" -lt "$under" ]; then
    echo "check-size: ok, and $((-delta)) bytes under budget: lower size-budget.txt to $size ($(mb "$size"))."
else
    echo "check-size: ok: $size bytes ($(mb "$size")), budget $budget ($(mb "$budget")), ${delta} bytes."
fi
