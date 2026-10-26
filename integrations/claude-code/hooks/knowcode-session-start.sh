#!/bin/sh
# KnowCode SessionStart hook for Claude Code: at the start of a session, after /clear and after a compaction, it adds
# a line for the agent saying whether KnowCode has this repo indexed, how many other repos are, and which tools answer
# callers, impact and cross-repo questions.
#
# It says nothing without the knowcode binary, outside an indexed repo (or one under a workspace root), in a repo
# whose privacy file blocks KnowCode, with `hooks.session_start = false` in KnowCode's config.toml, on any error, or
# after KNOWCODE_HOOK_MS milliseconds (default 1500). It reads only the config, the privacy file and the cache's
# listing: it never indexes and makes no network call.
#
# KNOWCODE_BIN names the binary when it isn't on PATH.

KC="${KNOWCODE_BIN:-knowcode}"
command -v "$KC" >/dev/null 2>&1 || exit 0
"$KC" hook session-start 2>/dev/null
exit 0
