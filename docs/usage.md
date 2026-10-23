# Using KnowCode

KnowCode answers the same questions two ways: as MCP tools for a coding agent (`knowcode serve`, set up by
`knowcode setup`), and as CLI commands for you and for scripts. Both read the same index and return the same
[answer contract](answers.md). This page is organised by task. For every parameter of an MCP tool, ask the server:
`kc_help {topic: "kc_impact"}` (the topics are also MCP resources, `knowcode://help/<topic>`). For every flag of a
command, run `knowcode <command> --help`.

## Common to every query

- **Which repo.** A query answers for the repo containing the current directory (for MCP, the server's working
  directory), or the one named by `-r/--repo` (MCP `repo`: a path or an indexed repo's name). `--repo` repeats.
  Outside any repo and without `--repo`, a query covers the workspace (see
  [configuration](configuration.md#workspace-roots)).
- **Freshness.** CLI queries reindex changed files first; `--no-sync` answers from the index as it is. MCP queries
  share a wait of about two seconds for indexing; past it they answer from the last committed snapshot and say so
  (`stale`), while a background worker finishes.
- **Output.** Text by default; `--json` prints the full JSON envelope. MCP answers are compact JSON, or text trees
  for `kc_trace`, `kc_impact`, `kc_path` and `kc_flow` (`format: "json"` for the envelope), capped at `max_chars`;
  `kc_search` and `kc_grep_ast` hits are text rows (`repo/file:line kind symbol | text`). See
  [MCP answers](answers.md#mcp-answers).
- **Picking one of several definitions.** Anything taking a symbol accepts a name, `Type.member`, a name qualified by
  namespace or module (`Acme.Billing.Invoice.Total`, `src/worker.Worker.run`), `path/to/file#name`, or `file:line`
  (the innermost definition spanning the line).
- **Cut lists.** A cut list says so in `incomplete` and carries a cursor: `knowcode more <cursor>`, or `kc_more`.

## Orient in a repo

| Task | CLI | MCP |
| --- | --- | --- |
| What is this repo? Languages, packages, dependencies and dependents, entry points, key types, hotspots, a ranked map of the central files | `knowcode overview` | `kc_overview` |
| The parts of a repo, clustered from references, co-change and directories | `knowcode areas` | `kc_areas` |
| A deterministic Markdown summary of a repo, printed | `knowcode docs` | (use `kc_overview`) |
| Packages, targets and repo-to-repo edges from manifests | `knowcode deps` | `kc_overview` |
| Index coverage, freshness, schema, config, SCIP and privacy state | `knowcode status` | `kc_status` |

## Find code

| Task | CLI | MCP |
| --- | --- | --- |
| Definitions by name, prefix or camelCase words (`settings view`, `Store.load`) | `knowcode find` | `kc_find` |
| The files for a concept, bug report or ticket title, when you don't know the names | `knowcode locate "refund approval"` | `kc_locate` |
| The symbols enclosing a `file:line` (a stack frame) | `knowcode at src/app/filter.service.ts:419` | `kc_symbol_at` |
| Text or regex over every file, ranked definitions first | `knowcode search` | `kc_search` |
| Code patterns over syntax trees ([ast-grep](https://ast-grep.github.io) patterns) | `knowcode grep-ast 'new HttpClient($$$)' -l cs` | `kc_grep_ast` |

`search` matches per line, like `git grep`. Hits are ranked `def` (the match covers a declared name), `decl`
(elsewhere on a declaration line), `code`, then `test`, `comment` and `doc`. It stops at 50,000 matching lines,
always at the same file, and says `capped`.

`grep-ast` takes one language (`csharp`/`cs`, `typescript`/`ts`, `javascript`/`js`, `python`/`py`, `rust`/`rs`,
`swift`). `$X` matches one node, `$$$ARGS` any number of nodes, `$_` one node without capturing it.
`kind:<node kind>` matches every node of a kind and `kind:<node kind>~<regex>` those whose text matches. A pattern
that doesn't parse alone goes in context with `--selector`:
`'try { $$$ } catch $$$ { }' --selector catch_clause` finds empty catch blocks.

## Understand a symbol

| Task | CLI | MCP |
| --- | --- | --- |
| One bundle: source (a skeleton when long), members, conformances, callers and callees with confidence, notes, and the sections that apply (HTTP, DI, tables, bus, SQL, Angular, Ansible, Terraform) | `knowcode context` | `kc_context` (topics `kc_context.*`) |
| How a repo is deployed: its CI deploy chains, playbooks and roles, and every CI include | `knowcode context <repo name> [--limit N]` | `kc_context` with `limit` (topic `kc_context.deploy`) |
| Every reference to a name, each resolved with confidence and evidence | `knowcode refs` | `kc_refs` |
| The call and relation tree around a symbol | `knowcode trace` | `kc_trace` |
| How one symbol reaches another, possibly across repos | `knowcode path A B` | `kc_path` |
| What happens end to end from an entry point (a route, component, action, method or `file:line`) | `knowcode flow` | `kc_flow` |
| HTTP routes a repo requests, with the actions serving them | `knowcode routes` | `kc_context` (topic `kc_context.http`) |
| Entity Framework and SQL tables, who reads and writes each | `knowcode tables [NAME]` | `kc_context` (topic `kc_context.tables`) |
| Messages a repo publishes and consumes, with the other side | `knowcode events` | `kc_context` (topic `kc_context.bus`) |

`kc_context`, `kc_refs` and `kc_locate` take several inputs per call (`symbols`, `names`, `queries`, up to 20).

`trace`, `impact`, `path` and `flow` filter edges with `--edges` (calls, instantiates, uses, reads, writes, http, bus,
sql, …) and evidence with `--evidence` ([evidence policy](answers.md#evidence-policy)). `trace` and `impact` filter
listed symbols with `--kinds` and walk through the rest.

## Before and after a change

| Task | CLI | MCP |
| --- | --- | --- |
| What depends on a symbol, a file or a diff, by proximity: `direct` (hop 1), `near` (2), `far` (3+), `test`; plus files history couples to it | `knowcode impact [SYMBOL] \| --file F \| --diff working` | `kc_impact` (topics `kc_impact.crossings`, `kc_impact.history`) |
| The tests to run, ranked, with reasons and a confidence | `knowcode tests FILE... \| --diff REV` | `kc_tests` |
| Files that usually change in the same commits | `knowcode coupled FILE` | `kc_coupled` |
| Who could review a change | `knowcode reviewers FILE... \| --diff REV` | `kc_reviewers` |
| Dead-code candidates, `unused` or `possibly unused` with reasons (C#, TypeScript) | `knowcode unused` | `kc_unused` |
| Every site a rename would edit, with collisions; exits 2 when unsure (C#, TypeScript) | `knowcode rename SYMBOL NEW_NAME` | `kc_rename_preview` |

Proximity is graph distance, not risk. `impact --diff` also reads definitions the change deletes or renames from the
base commit (`removed`), and warns about usual partners the diff leaves alone (`missed_partners`). `unused` and
`rename` are previews: nothing is edited.

## History

| Task | CLI | MCP |
| --- | --- | --- |
| Files that keep hurting: churn, fix commits, complexity, coupling, health; one file's code-health biomarkers | `knowcode hotspots [FILE]` | `kc_hotspots` |
| Historical contribution: top contributor, truck factor, share of lines by former authors; surviving lines for a file | `knowcode ownership [PATH]` | `kc_ownership` |

These read local git history only. They measure history, not who understands the code today.

A shallow clone (`git clone --depth 1`, common in CI and in `fetch-deps`) has little or no history to read.
`knowcode history deepen` fetches more: `git fetch --deepen=1000` by default, `--by N` for another number of
commits, or `--since DATE` (`git fetch --shallow-since`). It reports the commits before and after and, for each of
`hotspots`, `coupled` (their 365-day window), `ownership` and `reviewers` (all history), whether it now has
everything it reads (`complete`), some (`partial`) or nothing (`none`). It writes nothing but what that fetch writes,
and leaves a clone that isn't shallow alone. It is a CLI command only: the MCP server never fetches.

## Fetching dependency repos

Cross-repo answers reach only the repos you have cloned. `knowcode fetch-deps --into DIR` clones the git repos the
current repo (or `--repo`) depends on into DIR, one directory per repo, so you can add them to the workspace. Running
it clones; `--dry-run` prints what would be cloned from where and clones nothing. It is a CLI command only, using
your git credentials: the MCP server never clones.

- **What it finds.** Manifest dependencies written as git URLs (SwiftPM `Package.swift`, XcodeGen `project.yml`,
  Cargo `git =`, npm `git+https:`, `git+ssh:` and `github:` specs), Terraform module sources from git
  (`git::https://…`, `github.com/…`), GitLab CI includes of another project (cloned from the host of the repo's own
  origin), and dependencies on a repo whose checkout was deleted (`missing_repo`), by that repo's origin. Registry
  dependencies (NuGet, npm by version) name no repository: they are counted, not fetched. A repo with a checkout
  already indexed is listed as `present`.
- **How it clones.** Shallow by default (`--full` for all history; `knowcode history deepen` later), with your git
  credentials. Only `https://`, `ssh://` and `user@host:path` remotes are cloned; others (`http://`, `file://`,
  `git://`, local paths) are listed as `refused`.
- **Where.** DIR is required and must be outside every git repo and indexed checkout; KnowCode never writes into an
  existing repo. A dependency whose directory under DIR is taken by something else is `skipped`; one cloned there
  before is `present`. When workspace roots are configured and DIR is under none of them, it says so, and
  `--add-root` adds DIR as a root. With no roots configured it suggests `knowcode index --workspace DIR` instead,
  since adding a root would narrow the workspace.
- **Privacy.** Each repo is cloned without a checkout first and its AI privacy file read from the commit. A repo
  blocked whole is removed again and listed as `blocked` (`withheld` in `incomplete`); `--include-blocked` keeps it,
  though KnowCode still indexes and answers nothing from it.
- **Nothing to fetch.** When no dependency names a git repo, it says so and changes nothing.

## Notes

Short notes anchored to symbols, kept in the user data directory, never in the repo or the cache.

| Task | CLI | MCP |
| --- | --- | --- |
| Add a note (`--tag decision\|gotcha\|todo`) | `knowcode note add SYMBOL "text"` | `kc_note_add` (writes local state) |
| Read notes, each `current`, `stale` or `orphaned` | `knowcode note list [SYMBOL]` | `kc_notes` |
| Remove a note | `knowcode note rm ID` | |
| Export and import notes as JSON | `knowcode note export > notes.json`, `knowcode note import notes.json` | |

A note records the declaration's identity and a hash of its body: it goes `stale` when the body changes and
`orphaned` when no declaration has its identity, and re-attaches when one does again.

## Indexes and the cache

| Task | CLI |
| --- | --- |
| Index repos now (queries index on their own) | `knowcode index [PATHS]` |
| Remove one checkout's index | `knowcode forget PATH` |
| Remove the indexes of checkouts that no longer exist | `knowcode prune` |
| Remove every index (notes are kept) | `knowcode purge` |

## Setup, settings and updates

| Task | CLI |
| --- | --- |
| Connect Claude Code, Codex or Cursor | `knowcode setup claude\|codex\|cursor` |
| Remove client registrations and cached indexes | `knowcode uninstall` |
| Run the MCP server on stdio | `knowcode serve` |
| Show or change settings | `knowcode config path\|list\|get\|set\|unset\|migrate` |
| Manage workspace roots | `knowcode workspace add\|remove\|list` |
| Clone the git repos this repo depends on, for cross-repo answers | `knowcode fetch-deps --into DIR [--dry-run]` |
| Fetch more history into a shallow clone | `knowcode history deepen [--by N \| --since DATE]` |
| Check for or install an update | `knowcode self-update [--check]` |

`setup claude` installs the bundled Claude Code plugin (MCP server, Grep/Glob hook and skills) through the local
`claude` CLI. `setup codex` and `setup cursor` add the server to the client's user settings and keep unrelated
settings. `--print` prints the configuration without changing anything, `--client-dir DIR` targets an isolated
profile, and `--replace` replaces a different server already registered as `knowcode` (Codex, Cursor). Start a new
client session afterwards.

`self-update` updates a shell-installer installation. For a Homebrew installation it refuses and names
`brew upgrade bubstackdev/tap/knowcode`, which updates it instead.

Updating keeps the Claude Code plugin in step with the binary. After `self-update` installs a release, and whenever
`serve` starts and finds that the plugin bundle setup wrote is older than the binary, KnowCode rewrites the bundle
(manifest, hook and skills, keeping the binary path and overrides setup recorded) and runs
`claude plugin update knowcode@knowcode-local --scope user` for each Claude profile it was set up in, so Claude Code
picks up the new version in its next session. It touches only the bundle at the path setup recorded. If the `claude`
CLI is missing or the update fails, it says so once on stderr with the `knowcode setup claude` command that finishes
it; the update and the server go on regardless. `brew upgrade` doesn't run `self-update`, so for a Homebrew
installation the next `serve` start does the refresh. Codex and Cursor registrations name only the binary, so an update
needs nothing there.

`uninstall` removes the registrations setup recorded, the bundled plugin files, the cached indexes, KnowCode's lock
and update-check files and the directories they leave empty, and keeps unrelated client settings and files. In a terminal it offers to remove configuration and notes (default no); otherwise pass
`--delete-config` or `--delete-notes`. It checks each removal against the client's own listing: a registration still
listed is reported, the command exits non-zero, and the registration stays recorded so running `uninstall` again
finishes it. Running clients are not stopped.

For Claude Code, the plugin is uninstalled with its data directory (`~/.claude/plugins/data/knowcode-knowcode-local`),
where KnowCode keeps nothing. Claude Code keeps its cached copy of the plugin, marked orphaned (`.orphaned_at`), so
sessions still running keep working, and deletes it itself later. KnowCode leaves that copy alone: it belongs to
Claude Code, and deleting it under a running session would break that session.

`uninstall` does not remove the binary. A package manager removes what it installed. The shell installer has no
uninstaller; for an installation it made, an `uninstall` that completes ends with the exact steps for that machine
(keep the binary until then). They are, in order:

1. Delete the line `. "$HOME/.config/knowcode/env.sh"` from each of `~/.profile`, `~/.bashrc`, `~/.bash_profile`,
   `~/.bash_login`, `~/.zshrc` and `~/.zshenv` that has it (`$ZDOTDIR` for the zsh files when set), and delete
   `~/.config/fish/conf.d/knowcode.env.fish`. Do this first: once `env.sh` is gone, every new shell that still
   sources it prints an error.
2. Delete the binary, `~/.local/bin/knowcode` by default.
3. Delete the installer's directory, `~/.config/knowcode` (`$XDG_CONFIG_HOME/knowcode` when set): its receipt
   `knowcode-receipt.json` and `env.sh` and `env.fish`.

KnowCode's own configuration directory is a different one; `--delete-config` removes the files KnowCode wrote there.

By default `serve` advertises the core tools: `kc_find`, `kc_locate`, `kc_context`, `kc_refs`, `kc_impact`,
`kc_search`, `kc_more` and `kc_help`. They answer where a name is defined, every use of it, what a symbol is, where a
concept lives, text search, and what calls a symbol or breaks if it changes. `serve --tools all` (or
`KNOWCODE_TOOLS=all`) advertises all 25 tools; a comma list adds tools by name (`core,kc_trace,kc_tests`). A tool left
out is still documented in `kc_help`, which says it isn't advertised, but can't be called until the server is
started with it.

The default is the small set because a client that doesn't defer tools loads every advertised schema, and the
server's instructions, on every turn. The core tools and the instructions are about 3.5k characters; `all` is about
7.4k. kc_trace is not in the core set: kc_impact answers callers by hop, kc_context a symbol's callers and callees, and
kc_trace's schema is one of the largest.

Each tool's schema lists only the parameters most calls need. Every other parameter, `repo` and `max_chars` among
them, is accepted all the same and documented in `kc_help <tool>`, so a client that loads every schema on every turn
carries less.

In Claude Code, the core tools are marked always loaded (`_meta` `anthropic/alwaysLoad`), so they are in context from
the start instead of waiting behind tool search; with `all`, the other tools stay deferred. A server-level
`alwaysLoad` setting overrides the mark, and a profile that leaves a core tool out doesn't list it. The Claude Code
plugin that `knowcode setup claude` installs sets `KNOWCODE_TOOLS=all` (unless you set it): Claude Code defers the
non-core tools, and the plugin's skills use them.

## MCP tools

| Tool | CLI equivalent | Help topic |
| --- | --- | --- |
| `kc_find` | `find` | `kc_find` |
| `kc_locate` | `locate` | `kc_locate` |
| `kc_symbol_at` | `at` | `kc_symbol_at` |
| `kc_refs` | `refs` | `kc_refs`, `kc_refs.languages` |
| `kc_context` | `context`, `routes`, `tables`, `events` | `kc_context` and its sections |
| `kc_trace` | `trace` | `kc_trace` |
| `kc_path` | `path` | `kc_path` |
| `kc_flow` | `flow` | `kc_flow` |
| `kc_impact` | `impact` | `kc_impact`, `kc_impact.crossings`, `kc_impact.history` |
| `kc_rename_preview` | `rename` | `kc_rename_preview` |
| `kc_unused` | `unused` | `kc_unused` |
| `kc_tests` | `tests` | `kc_tests` |
| `kc_coupled` | `coupled` | `kc_coupled` |
| `kc_hotspots` | `hotspots` | `kc_hotspots` |
| `kc_ownership` | `ownership` | `kc_ownership` |
| `kc_reviewers` | `reviewers` | `kc_reviewers` |
| `kc_overview` | `overview`, `deps` | `kc_overview` |
| `kc_areas` | `areas` | `kc_areas` |
| `kc_notes` | `note list` | `kc_notes` |
| `kc_note_add` | `note add` | `kc_note_add` |
| `kc_search` | `search` | `kc_search` |
| `kc_grep_ast` | `grep-ast` | `kc_grep_ast` |
| `kc_status` | `status` | `kc_status` |
| `kc_more` | `more` | `kc_more`, `cursors` |
| `kc_help` | | `kc_help` |

Every tool takes `repo` and `max_chars`. Shared concepts have their own topics: `contract`, `evidence`, `cursors`,
`privacy` and `profiles`.

Some defaults differ between the two surfaces, to keep agent answers small: `kc_trace` and `kc_impact` go one hop
unless asked for more (`trace` defaults to 2, `impact` to 3), `kc_impact` and `kc_flow` list at most 60 rows by
default, and `kc_overview`'s map budget is 800 tokens (1000 on the CLI).

The server runs one call at a time, except that a call running longer than 2 seconds lets the next one start beside
it, and `kc_search` and `kc_grep_ast` run in a child process of their own. Cancelling a request
(`notifications/cancelled`) stops it and kills its child processes.

## Command flags

Every query command takes `-r/--repo`, `--no-sync` and `--json`; they are not repeated below.

| Command | Flags |
| --- | --- |
| `index` | `--workspace DIR`, `--threads N`, `--rebuild`, `--scip FILE`, `--verify`, `--phases` |
| `find` | `-k/--kind`, `-n/--limit` |
| `locate` | `-n/--limit` |
| `at` | |
| `refs` | `-k/--kind` (alias `--edges`), `-n/--limit`, `--cite brief\|lines\|check\|none` |
| `context` | `--budget`, `--source full\|skeleton\|none\|auto`, `--limit` (rows per list of a repo's deploy answer), `--cite brief\|lines\|check\|none` |
| `trace` | `-d/--direction in\|out\|both`, `--depth`, `-n/--limit`, `--edges`, `--kinds`, `--evidence`, `--cite brief\|lines\|check\|none` |
| `path` | `--to-repo`, `--max-hops`, `-n/--paths`, `--max-nodes`, `--max-repos`, `--allow-ambiguous`, `--edges`, `--evidence`, `--cite brief\|lines\|check\|none` |
| `flow` | `--depth`, `-n/--limit`, `--max-repos`, `--edges`, `--children`, `--evidence`, `--cite brief\|lines\|check\|none` |
| `impact` | `--file`, `--diff`, `--depth`, `-n/--limit`, `--order rank\|hop`, `--as-of`, `--since`, `--edges`, `--kinds`, `--evidence`, `--cite brief\|lines\|check\|none` |
| `unused` | `-k/--kind`, `--tier unused\|possibly\|all`, `-n/--limit` |
| `rename` | `--allow-ambiguous` |
| `coupled` | `-n/--limit`, `--as-of`, `--since` |
| `tests` | `--diff`, `-n/--limit`, `--evidence` |
| `hotspots` | `-n/--top`, `--as-of` |
| `ownership` | `--without`, `--depth`, `-n/--top`, `--as-of` |
| `reviewers` | `--diff`, `-n/--limit`, `--as-of` |
| `overview` | `--focus`, `--map-budget` |
| `areas` | `--area` |
| `docs` | `--map-budget` |
| `search` | `-e/--regex`, `-i/--ignore-case`, `-S/--smart-case`, `-w/--word`, `-l/--lang`, `-g/--glob`, `-C/--context`, `-n/--limit`, `--offset`, `--files`, `-a/--all` |
| `grep-ast` | `-l/--lang`, `--selector`, `--inside`, `--has`, `--not-inside`, `--not-has`, `--text`, `-g/--glob`, `-n/--limit`, `--offset`, `-a/--all` |
| `tables` | `--all-callers` |
| `routes`, `events`, `deps` | |
| `fetch-deps` | `--into DIR` (required), `--dry-run`, `--full`, `--add-root`, `--include-blocked` |
| `history deepen` | `--by N`, `--since DATE` |
| `note add` | `--tag` |
| `note export`, `note import` | `--repo` |
| `more` | |
| `status` | `--verify` |
| `serve` | `--tools` |
| `setup` | `--client-dir`, `--print`, `--replace` |
| `uninstall` | `--delete-config`, `--delete-notes` |
| `self-update` | `--check` |

`--cite` (MCP `cite`) says how much rows quote of the source lines they rest on, each checked against the index
([cited source](answers.md#cited-source)): `brief` (the default), `lines`, `check` (the check without the lines) or
`none`. The `--evidence` flag is also accepted as `--min-evidence`. `--as-of` takes a commit or date for `coupled` and
`impact`, and unix seconds, `YYYY-MM-DD` or `head` for `hotspots`, `ownership` and `reviewers`.

## Precise references from SCIP

KnowCode never runs a compiler or indexer, but when a repo has an `index.scip` at its root (or one named once with
`knowcode index --scip FILE`), it reads it whenever it changes. A reference whose site matches a SCIP occurrence takes
the compiler's answer (`precise: true`, evidence `scip`); where the heuristics pointed elsewhere it says so
(`corrected_from`). A SCIP index applies only to files and projects unchanged since it was written; `status` shows
its coverage. To make one: `npx @sourcegraph/scip-typescript index` in a TypeScript repo, or
`dotnet tool install -g scip-dotnet && scip-dotnet index` in a .NET one.

## Claude Code hook

The Claude Code plugin adds a `PreToolUse` hook for Grep, Glob and Bash (for Bash, only code searches such as `grep`,
`rg`, `git grep` or `find -name`). When the pattern names a symbol in an existing index, it adds a short note beside
the result: where the symbol is defined, its reference count by confidence, its top callers, and the `kc_context` call
to make next. It never blocks the search and never indexes. The hook script decides first: a Bash command without a
search word in it exits in the shell without starting `knowcode`. See [integrations/claude-code](../integrations/claude-code/README.md).
