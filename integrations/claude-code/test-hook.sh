#!/bin/bash
# Feeds the Grep/Glob hook sample PreToolUse events and checks that it adds context only when it should and fails
# open otherwise. On macOS it also reports the instructions each call retires (/usr/bin/time -l).
#
#   integrations/claude-code/test-hook.sh <indexed repo> <a symbol name defined in it>
#
# Uses `knowcode` from PATH, or KNOWCODE_BIN; reads the index from KNOWCODE_CACHE_DIR like knowcode does.
set -u
repo=$(cd "${1:?usage: test-hook.sh <indexed repo> <symbol>}" && pwd)
symbol=${2:?usage: test-hook.sh <indexed repo> <symbol>}
here=$(cd "$(dirname "$0")" && pwd)
hook="$here/hooks/knowcode-grep-hint.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

event() { # tool pattern
    printf '{"session_id":"t","hook_event_name":"PreToolUse","cwd":"%s","tool_name":"%s","tool_input":{"pattern":"%s"},"tool_use_id":"toolu_test"}' \
        "$repo" "$1" "$2"
}

# case name, expectation (context|none), event JSON, then any VAR=value for the hook's environment
check() {
    local name=$1 want=$2 json=$3
    shift 3
    printf '%s' "$json" >"$tmp/in"
    if [ "$(uname)" = Darwin ]; then
        env "$@" /usr/bin/time -l "$hook" <"$tmp/in" >"$tmp/out" 2>"$tmp/time"
    else
        env "$@" "$hook" <"$tmp/in" >"$tmp/out" 2>"$tmp/time"
    fi
    local code=$? bytes
    bytes=$(wc -c <"$tmp/out" | tr -d ' ')
    local instr
    instr=$(awk '/instructions retired/ {print $1}' "$tmp/time")
    local got=none
    grep -q '"additionalContext"' "$tmp/out" && got=context
    local verdict=PASS
    if [ "$code" != 0 ] || [ "$got" != "$want" ]; then
        verdict=FAIL
        fails=$((fails + 1))
    fi
    printf '%-4s %-34s exit %s, %s, %5s bytes, %s instructions\n' "$verdict" "$name" "$code" "$got" "$bytes" "${instr:-?}"
    if [ "$got" = context ]; then
        sed 's/^/       /' "$tmp/out" | cut -c1-400
    fi
}

check "Grep: known symbol" context "$(event Grep "$symbol")"
check "Grep: \\b-wrapped known symbol" context "$(event Grep "\\\\b$symbol\\\\b")"
check "Glob: file named for the symbol" context "$(event Glob "**/$symbol*")"
check "Grep: unknown name" none "$(event Grep NoSuchSymbolAnywhere)"
check "Grep: regex" none "$(event Grep "foo.*Bar|baz\\\\d+")"
check "Grep: plain word" none "$(event Grep error)"
check "Read: not a search tool" none "$(event Read "$symbol")"
check "malformed JSON" none '{"tool_name": "Grep", "tool_input": '
check "missing binary" none "$(event Grep "$symbol")" KNOWCODE_BIN=/nonexistent/knowcode
check "timeout (1 ms budget)" none "$(event Grep "$symbol")" KNOWCODE_HOOK_MS=1
check "repo without an index" none "$(event Grep "$symbol")" KNOWCODE_CACHE_DIR="$tmp/empty-cache"
if [ -e "$tmp/empty-cache/repos" ] && [ -n "$(ls -A "$tmp/empty-cache/repos")" ]; then
    echo "FAIL the hook created an index"
    fails=$((fails + 1))
fi
echo "$fails failed"
exit $((fails > 0))
