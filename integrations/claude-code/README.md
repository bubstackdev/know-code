# KnowCode for Claude Code

The plugin bundles the local MCP server, a Grep/Glob hook, and four workflow skills.
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
- `hooks/hooks.json` registers a `PreToolUse` hook for Grep and Glob.
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

`test-hook.sh` feeds the hook sample events and prints the instructions each call retires on macOS.

The manifest follows the [Claude plugin reference](https://code.claude.com/docs/en/plugins-reference)
and [plugin MCP configuration](https://code.claude.com/docs/en/mcp#plugin-provided-mcp-servers).
Other client registrations use the [Codex MCP format](https://developers.openai.com/codex/mcp/)
and [Cursor MCP format](https://cursor.com/help/customization/mcp).
