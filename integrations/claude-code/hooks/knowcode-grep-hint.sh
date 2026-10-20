#!/bin/sh
# KnowCode PreToolUse hook for Claude Code's Grep, Glob and Bash tools (for Bash, only code searches: grep, rg, git grep,
# find -name and the like; any other command adds nothing).
#
# When the pattern names a symbol the KnowCode index knows, it adds a short note for the agent: where the symbol is
# defined, how many references resolve to it and with what confidence, its top callers, and the kc_context / kc_refs
# call that answers more. Otherwise it adds nothing.
#
# It decides here first, in the shell, whether the call can be a search: the tool is Grep or Glob, or the Bash command
# has a search word in it (grep, egrep, fgrep, rg, ag, ack, git grep, or find with -name, -iname, -path or
# -wholename). Anything else exits at once without starting the binary. The test is cheap and deliberately loose;
# the binary parses the event precisely.
#
# It fails open: without the knowcode binary, without an index for the repo, on any error, or after
# KNOWCODE_HOOK_MS milliseconds (default 1500), it prints nothing and exits 0, and the tool runs as usual. It never
# indexes, and reads the index from KNOWCODE_CACHE_DIR when set, like every knowcode command.
#
# KNOWCODE_BIN names the binary when it isn't on PATH.

input=$(cat)

# True when $1 appears in $rest as a word: not inside a longer name, and not run on after a JSON \n or \t escape.
word() {
    case $rest in
        *[!A-Za-z0-9_-]"$1"[!A-Za-z0-9_-]* | *'\n'"$1"[!A-Za-z0-9_-]* | *'\t'"$1"[!A-Za-z0-9_-]*) return 0 ;;
    esac
    return 1
}

case $input in
    *'"tool_name"'*'"Grep"'* | *'"tool_name"'*'"Glob"'*) ;;
    *'"tool_name"'*'"Bash"'*)
        # The command and what follows it; the working directory and other fields come before it.
        rest=${input#*'"command"'}
        case $rest in
            *grep*) ;;
            *)
                if word rg || word ag || word ack; then
                    :
                elif word find; then
                    case $rest in
                        *-name* | *-iname* | *-path* | *-wholename*) ;;
                        *) exit 0 ;;
                    esac
                else
                    exit 0
                fi
                ;;
        esac
        ;;
    *) exit 0 ;;
esac

KC="${KNOWCODE_BIN:-knowcode}"
command -v "$KC" >/dev/null 2>&1 || exit 0
printf '%s' "$input" | "$KC" hook claude-code 2>/dev/null
exit 0
