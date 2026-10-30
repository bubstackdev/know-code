#!/bin/bash
# Feeds the Grep/Glob/Bash hook sample PreToolUse events and checks that it adds context only when it should and fails
# open otherwise, and that calls which can't be searches exit in the shell script without starting the binary. On
# macOS it also reports the instructions each call retires (/usr/bin/time -l).
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

bash_event() { # command (no double quotes in it)
    printf '{"session_id":"t","hook_event_name":"PreToolUse","cwd":"%s","tool_name":"Bash","tool_input":{"command":"%s","description":"t"},"tool_use_id":"toolu_test"}' \
        "$repo" "$1"
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
check "Bash: grep for the symbol" context "$(bash_event "grep -rn '$symbol' --include=*.cs .")"
check "Bash: cd chain, rg" context "$(bash_event "cd . && rg -n '$symbol' | head")"
check "Bash: build" none "$(bash_event "cargo build")"
check "Bash: git status" none "$(bash_event "git status")"
check "Bash: sed -n" none "$(bash_event "sed -n 1,20p README.md")"
check "Bash: grep for a plain word" none "$(bash_event "cat x | grep -c foo")"
check "Read: not a search tool" none "$(event Read "$symbol")"
check "malformed JSON" none '{"tool_name": "Grep", "tool_input": '
check "missing binary" none "$(event Grep "$symbol")" KNOWCODE_BIN=/nonexistent/knowcode
check "timeout (1 ms budget)" none "$(event Grep "$symbol")" KNOWCODE_HOOK_MS=1
check "repo without an index" none "$(event Grep "$symbol")" KNOWCODE_CACHE_DIR="$tmp/empty-cache"

# The shell script decides first: a call that can't be a search exits without starting the binary. A KNOWCODE_BIN
# that records being called tells which calls reached it.
printf '#!/bin/sh\ncat >/dev/null\ntouch "%s/called"\n' "$tmp" >"$tmp/recorder"
chmod +x "$tmp/recorder"
starts() { # case name, expectation (binary|shell), event JSON
    rm -f "$tmp/called"
    printf '%s' "$3" | KNOWCODE_BIN="$tmp/recorder" "$hook" >/dev/null 2>&1
    local got=shell verdict=PASS
    [ -e "$tmp/called" ] && got=binary
    if [ "$got" != "$2" ]; then
        verdict=FAIL
        fails=$((fails + 1))
    fi
    printf '%-4s %-34s decided by the %s\n' "$verdict" "$1" "$got"
}
starts "Grep" binary "$(event Grep "$symbol")"
starts "Glob" binary "$(event Glob "**/$symbol*")"
starts "Bash: grep" binary "$(bash_event "grep -rn $symbol .")"
starts "Bash: egrep in a pipeline" binary "$(bash_event "cat x | egrep $symbol")"
starts "Bash: fgrep" binary "$(bash_event "fgrep -r $symbol src")"
starts "Bash: git grep" binary "$(bash_event "git grep -n $symbol")"
starts "Bash: cd chain, rg" binary "$(bash_event "cd src && rg $symbol")"
starts "Bash: ag" binary "$(bash_event "ag $symbol")"
starts "Bash: ack" binary "$(bash_event "ack $symbol")"
starts "Bash: find -name" binary "$(bash_event "find . -name '$symbol*'")"
starts "Bash: find -iname" binary "$(bash_event "find . -iname '$symbol*'")"
starts "Bash: find -path" binary "$(bash_event "find . -path '*/$symbol/*'")"
starts "Bash: find -wholename" binary "$(bash_event "find . -wholename '*/$symbol.cs'")"
starts "Bash: rg after a newline" binary "$(bash_event "cd src\\nrg $symbol")"
starts "Bash: cargo build" shell "$(bash_event "cargo build")"
starts "Bash: git status" shell "$(bash_event "git status")"
starts "Bash: sed -n" shell "$(bash_event "sed -n 1,20p README.md")"
starts "Bash: find -type f" shell "$(bash_event "find . -type f")"
starts "Bash: words containing rg, ag" shell "$(bash_event "cargo fmt && npm run tag-release")"
starts "Read" shell "$(event Read "$symbol")"
starts "empty input" shell ''

# Any index, in a build's own tree (empty-cache/v/<key>/repos) or at the top level (0.6.1 and older).
if [ -n "$(find "$tmp/empty-cache" -name '*.sqlite' 2>/dev/null)" ]; then
    echo "FAIL the hook created an index"
    fails=$((fails + 1))
fi
echo "$fails failed"
exit $((fails > 0))
