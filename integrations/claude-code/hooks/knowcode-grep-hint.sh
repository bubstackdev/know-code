#!/bin/sh
# KnowCode PreToolUse hook for Claude Code's Grep, Glob and Bash tools (for Bash, only code searches: grep, rg, git grep,
# find -name and the like; any other command adds nothing).
#
# When the pattern names a symbol the KnowCode index knows, it adds a short note for the agent: where the symbol is
# defined, how many references resolve to it and with what confidence, its top callers, and the kc_context / kc_refs
# call that answers more. Otherwise it adds nothing.
#
# It fails open: without the knowcode binary, without an index for the repo, on any error, or after
# KNOWCODE_HOOK_MS milliseconds (default 1500), it prints nothing and exits 0, and the tool runs as usual. It never
# indexes, and reads the index from KNOWCODE_CACHE_DIR when set, like every knowcode command.
#
# KNOWCODE_BIN names the binary when it isn't on PATH.

KC="${KNOWCODE_BIN:-knowcode}"
command -v "$KC" >/dev/null 2>&1 || exit 0
"$KC" hook claude-code 2>/dev/null
exit 0
