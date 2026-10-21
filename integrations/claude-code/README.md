# KnowCode for Claude Code

The plugin bundles the local MCP server, a Grep/Glob/Bash search hook, and four workflow skills.
It requires an installed `knowcode` binary and the Claude Code CLI. The first MCP
query indexes its repository automatically; no manual `index` command is needed.

## Install

Run:

```sh
knowcode setup claude
```

Setup extracts the bundled plugin into KnowCode's user data directory and installs
it through a local marketplace named `knowcode-local`. This works without cloning
the KnowCode repository or downloading plugin files. It records the executable's
path and any explicit `KNOWCODE_CACHE_DIR`, `KNOWCODE_CONFIG_DIR`, `KNOWCODE_DATA_DIR`,
and `KNOWCODE_TOOLS` overrides. Start a new Claude Code session in your repository.
Each Claude registration command has a deadline of 120 seconds, configurable with
`KNOWCODE_SETUP_DEADLINE`. A failed or partial attempt can be retried.

For an isolated Claude profile, pass `--client-dir DIR`. `knowcode setup claude
--print` prints a plain MCP configuration snippet without installing anything.

After `knowcode self-update`, or when `knowcode serve` starts with a bundle older
than the binary, KnowCode rewrites the bundle and runs `claude plugin update
knowcode@knowcode-local` for each profile setup recorded, so the next session uses
the new hook and skills. When that can't finish, it prints the `knowcode setup
claude` command that does.

The repository is also a plugin marketplace for an installation that uses
`knowcode` from `PATH`:

```sh
claude plugin install knowcode --marketplace bubstackdev/knowcode
```

Choose one installation method. An older manual MCP registration or manually
copied hook/skills can otherwise coexist with the plugin. Remove the older
registration and copies when migrating; setup does not delete them.

## Components

- `.mcp.json` starts `knowcode serve` on stdio.
- `hooks/hooks.json` registers a `PreToolUse` hook for Grep, Glob and Bash.
- `skills/knowcode-explore` covers overview, locate, context, and flow.
- `skills/knowcode-before-edit` covers impact, tests, coupled files, and reviewers.
- `skills/knowcode-debug-stack-trace` maps file/line frames to symbols and callers.
- `skills/knowcode-safe-refactor` covers unused-code investigation and rename previews.

Claude may namespace plugin MCP tools. The skills refer to the advertised `kc_*`
tool names. Each tells the agent to check `complete` on every answer, read every
`incomplete` entry, follow `hint`s and cursors (`kc_more`) when the rest of a list
matters, and never treat a missing, stale, sampled or withheld result as proof of
absence. The answer contract is in [docs/answers.md](../../docs/answers.md).

## Hook behavior

The hook adds a short context note when a Grep or Glob pattern names a symbol in
an existing index and its definition still matches the working tree. It does not
block the search. It never creates or refreshes an index; the MCP query does that.
The note names `kc_context`, `kc_refs`, `kc_impact` and `kc_locate` both by
their short names and as Claude sees them from the plugin
(`mcp__plugin_knowcode_knowcode__kc_context`). The server marks those four always
loaded (about 1.6k tokens up front), and the full name lets tool search load one in
a single step if a client still defers it.

For Bash, it reads the command and acts only on code searches: `grep`, `egrep`,
`rg`, `ag`, `ack`, `git grep` and `find -name`, also in pipelines and in
`cd dir && ...` or `;` chains. It takes the patterns (`-e`, quoted, and top-level
alternations such as `A|B` or `A\|B`) and keeps the ones that look like symbols.
Any other command, such as a build, `git status`, `sed -n` or `cat`, adds nothing,
decided from the command string before any index is opened.

The hook script makes a cheap, deliberately loose test first, in POSIX `sh`: unless
the tool is Grep or Glob, or the Bash command contains `grep`, `egrep`, `fgrep`,
`rg`, `ag`, `ack`, `git grep`, or `find` with `-name`, `-iname`, `-path` or
`-wholename`, it exits without starting the `knowcode` binary. The binary then
parses what passes precisely.

Plain words, general regexes, unknown symbols, missing indexes, stale definitions,
malformed input, and unavailable binaries get no note. On errors or after
`KNOWCODE_HOOK_MS` (default 1500 milliseconds), it prints nothing and exits zero.
`KNOWCODE_BIN` selects the binary when installing the source plugin manually.

For manual integration without the plugin, `hooks/knowcode-grep-hint.sh` can still
be registered in Claude settings and the skill directories copied individually.
Use `knowcode setup claude --print` for the accompanying MCP registration.

## Validate

```sh
claude plugin validate integrations/claude-code --strict
python3 scripts/check-first-run.py target/debug/knowcode
integrations/claude-code/test-hook.sh /path/to/indexed/repo SymbolName
```

The protocol smoke check uses an isolated synthetic checkout. Clean-machine
installation and public-repository acceptance are separate release gates.

`test-hook.sh` feeds the hook sample events and prints the instructions each call retires on macOS (those of the
hook's shell, not of the binary it starts). It also checks, with a `KNOWCODE_BIN` that records being called, that only
calls which can be searches start the binary.

The manifest follows the [Claude plugin reference](https://code.claude.com/docs/en/plugins-reference)
and [plugin MCP configuration](https://code.claude.com/docs/en/mcp#plugin-provided-mcp-servers).
Other client registrations use the [Codex MCP format](https://developers.openai.com/codex/mcp/)
and [Cursor MCP format](https://cursor.com/help/customization/mcp).
