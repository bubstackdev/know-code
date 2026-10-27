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
| One answer for a goal in words: the files involved, what depends on them, the tests to run, what the index can't settle | `knowcode task "refunds fail for partial captures"` | `kc_task` |
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
| One symbol's history: the commits that changed its lines, its first and last change, who wrote its current lines, the files and direct callers changed in the same commits | `knowcode history SYMBOL` | `kc_history` |

These read local git history only. They measure history, not who understands the code today.

`history` reads a definition's history over the lines the index says it spans, with `git log -L`, so a method's
history is its own, not its file's: `git log -L :Name:file` needs a function-name rule that git's default doesn't
have for indented C# members, and `git log -S Name` counts calls and other overloads. It reads at most 200 commits;
in the MCP server it stops at the call's time budget and keeps what it read. Either is said in the answer.

A shallow clone (`git clone --depth 1`, common in CI and in `fetch-deps`) has little or no history to read.
`knowcode history deepen` fetches more: `git fetch --deepen=1000` by default, `--by N` for another number of
commits, or `--since DATE` (`git fetch --shallow-since`). It reports the commits before and after and, for each of
`hotspots`, `coupled` (their 365-day window), `ownership`, `reviewers` and `history` (all history), whether it now has
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
| Run it for a pipeline: bound repos, an op allowlist, its own cache | `knowcode serve --restricted --repo DIR --ops LIST --cache-dir DIR --config-dir DIR` |
| Every MCP op as JSON, for a client building its own tool surface | `knowcode capabilities [OP]` |
| Show or change settings | `knowcode config path\|list\|get\|set\|unset\|migrate` |
| Manage workspace roots | `knowcode workspace add\|remove\|list` |
| Clone the git repos this repo depends on, for cross-repo answers | `knowcode fetch-deps --into DIR [--dry-run]` |
| Fetch more history into a shallow clone | `knowcode history deepen [--by N \| --since DATE]` |
| Check for or install an update | `knowcode self-update [--check]` |

Installing, connecting a client, updating and uninstalling are described in [install](install.md).

## Tool profiles

By default `serve` advertises one tool, `kc` (profile `one`). Its `op` names the tool to run, without `kc_`:
`impact` for what calls a symbol or an HTTP endpoint, hop by hop and from any repo, or breaks if it changes; `find`
for where a name is defined; `refs` for every use; `context` for what a symbol is; `search` for text; `locate` for
where a concept lives; `history` for who changed a symbol, when and with what; `coupled` for the files that change
with a file; `more` for the next page of a cut list; `help` for the documentation; and every other tool by its name
(`trace`, `tests`, `reviewers`, …). `q` fills the op's main parameter, `qs` several, and any other parameter
of that tool is passed by its own name: `kc {op: impact, q: OrderService.Place, depth: 2}`. `kc_help kc` lists which
parameter `q` fills for each op. Answers, the server's instructions and the Claude Code plugin's notes, skills and
session-start line then name calls as ops: a cut list says `kc {op: more, cursor: …}`.

One tool is the default because every advertised schema, and the server's instructions, is paid on every turn: `kc`
and its instructions are about 500 tokens, where the eight core tools and their instructions were about 1,300 in a
client that loads every schema, and about 1,300 with the plugin's 25 tools in Claude Code, which defers most of them.

The separate tools are still there. `KNOWCODE_TOOLS=core` (or `serve --tools core`) advertises the core tools:
`kc_find`, `kc_locate`, `kc_context`, `kc_refs`, `kc_impact`, `kc_search`, `kc_more` and `kc_help`. `all` advertises
all 26 tools (every tool but `kc_capabilities`); a comma list adds tools by name (`core,kc_trace,kc_tests`). Under these, a tool left out is still
documented in `kc_help`, which says it isn't advertised, but can't be called until the server is started with it, and
answers name the separate tools (`kc_more {cursor: …}`). To keep the separate tools in Claude Code, run
`KNOWCODE_TOOLS=all knowcode setup claude`; for Codex or Cursor, set `KNOWCODE_TOOLS` in the environment `setup`
runs in, and it is recorded in the client's configuration.

Each tool's schema lists only the parameters most calls need, with a line only where the name doesn't say enough.
Every other parameter, `repo`, `max_chars` and `kc_impact`'s `file` among them, is accepted all the same and
documented in `kc_help <tool>`, so a client that loads every schema on every turn carries less. Some clients leave the
server's instructions out (Claude Code's bare mode does), so each core tool's description says on its own which
questions it answers.

Under `core` and `all`, in Claude Code, the core tools that answer a first question, `kc_find`, `kc_context`,
`kc_refs`, `kc_impact` and `kc_search`, are marked always loaded (`_meta` `anthropic/alwaysLoad`), so they are in
context from the start instead of waiting behind tool search. Every other tool stays deferred, one tool search away,
among them `kc_locate`, `kc_more` and `kc_help`: sessions rarely call them, and a schema loaded up front is paid on
every turn. A client that doesn't defer tools loads all eight core tools. `kc` is marked always loaded too. A
server-level `alwaysLoad` setting overrides the mark, and a profile that leaves a core tool out doesn't list it. The
Claude Code plugin that `knowcode setup claude` installs sets `KNOWCODE_TOOLS=one` unless you set it; a plugin
updated from 0.3, which wrote `all` as its default, takes `one` too.

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
| `kc_history` | `history` | `kc_history` |
| `kc_overview` | `overview`, `deps` | `kc_overview` |
| `kc_areas` | `areas` | `kc_areas` |
| `kc_notes` | `note list` | `kc_notes` |
| `kc_note_add` | `note add` | `kc_note_add` |
| `kc_search` | `search` | `kc_search` |
| `kc_grep_ast` | `grep-ast` | `kc_grep_ast` |
| `kc_status` | `status` | `kc_status` |
| `kc_more` | `more` | `kc_more`, `cursors` |
| `kc_help` | | `kc_help` |
| `kc_capabilities` | `capabilities` | `kc_capabilities` |
| `kc_task` | `task` | `kc_task` |

`kc_capabilities` isn't in `all`: it is the `kc` tool's `capabilities` op, the resource `knowcode://capabilities`,
and `knowcode capabilities`, and `--tools all,kc_capabilities` advertises it.

## Capabilities

A client that builds its own tool surface (an automated pipeline offering an agent a subset of the ops, say) reads
`kc {op: capabilities}` instead of copying this page. It describes every op as JSON: the full input schema, with the
bounds of each number (`minimum`, `maximum`, `default`) and the words a closed set takes (`enum`); what `q` and `qs`
fill and whether the op takes a query at all; whether it reads or writes (`note_add` is the only write); the
evidence presets it takes; and its answer's format, the keys a JSON answer always has, and the list it is about. A
number past its range is clamped, and a walk says how deep it went (`depth: {asked, walked}`); a word outside its set
or a value of the wrong type is refused. The document also names the server's version, the MCP protocol version and
the index schema. It is read from the same tables as help and the advertised schemas, and a test checks every op's
entry against the running server. `kc_help kc_capabilities` describes each field.

## Restricted mode

An automated pipeline that gives an agent KnowCode, rather than a person's session, can start the server bound to
what the job needs:

```sh
knowcode serve --restricted --repo /work/job-17/shop --ops find,refs,context,impact,more \
  --cache-dir /work/kc-cache --config-dir /work/kc-config
```

- **Bound repos.** It answers about the `--repo`s only (each a repo's root; repeat the flag for more). The workspace
  is those repos: nothing else is discovered or indexed, a workspace-wide call covers them alone, and a call naming
  another repo, or an absolute path outside them, is refused with code `not_allowed`.
- **An op allowlist.** `--ops` lists the ops it runs. Any other is refused with `not_allowed`, and under a profile
  of separate tools (`--tools all`) isn't listed. `capabilities` is
  always allowed. List `more` too if answers should continue, and `note_add` only if the job may write notes.
- **Explicit directories.** `--cache-dir` and `--config-dir` (or `KNOWCODE_CACHE_DIR` and `KNOWCODE_CONFIG_DIR`) are
  required, so nothing of the user's is read or written. Notes go to `--data-dir`, by default `data` in the cache.
  The server writes nothing outside the cache (and data) directory.
- **Nothing else.** No update checks, no Claude Code plugin refresh, and git kept off the network: a partial clone's
  missing objects aren't fetched (`GIT_NO_LAZY_FETCH`), and only local git protocols are allowed. The MCP server never
  clones or fetches in any mode.
- **Its scope, said.** The `initialize` reply carries it under `capabilities.experimental["knowcode/scope"]`: the
  repos, ops and directories, and `discovery`, `update_checks`, `plugin_refresh` and `git_network`, all false. The
  same is the first line on stderr and the capabilities' `scope`, beside each op's `allowed`.

A headless run, for example in a sandbox that allows writes to the cache only:

- Start it from a clean environment: `env -i HOME=… PATH=… TMPDIR=<cache>/tmp knowcode serve --restricted …`, with
  `TMPDIR` inside the cache.
- On macOS, put the real git first on `PATH`, not `/usr/bin/git`. That is a shim that runs `xcrun`, which may need to
  write its own cache files outside yours. The real one is in the directory above `git --exec-path`'s parent, `bin`
  (`/Library/Developer/CommandLineTools/usr/bin` with the Command Line Tools).
- Index the repos once before the jobs (`knowcode index DIR` with the same `KNOWCODE_CACHE_DIR` and
  `KNOWCODE_CONFIG_DIR`). The server also indexes a bound repo itself on its first question.

The acceptance check (`crates/knowcode/tests/restricted.rs`) starts it this way on macOS under a sandbox profile
that kills the server on any write outside its cache and on any network connection. It then checks that the server
says its scope, answers the ops allowed about its repo, refuses other ops, repos and paths with `not_allowed`, and is
still running at the end. Elsewhere it runs the same calls without the sandbox, and compares the files outside the
cache before and after.

Every tool takes `repo` and `max_chars`. Shared concepts have their own topics: `contract`, `evidence`, `cursors`,
`privacy` and `profiles`.

Some defaults differ between the two surfaces, to keep agent answers small: `kc_trace` and `kc_impact` go one hop
unless asked for more (`trace` defaults to 2, `impact` to 3), `kc_impact` and `kc_flow` list at most 60 rows by
default, and `kc_overview`'s map budget is 800 tokens (1000 on the CLI).

The server runs one call at a time, except that a call running longer than 2 seconds lets the next one start beside
it, and `kc_search` and `kc_grep_ast` run in a child process of their own. Cancelling a request
(`notifications/cancelled`) stops it and kills its child processes.

## Command flags

Every query command takes `-r/--repo`, `--no-sync`, `--json` and `--provenance` (the JSON envelope with a provenance
record, for durable records; see [answers](answers.md#the-provenance-record)); they are not repeated below.

| Command | Flags |
| --- | --- |
| `index` | `--workspace DIR`, `--threads N`, `--jobs N`, `--rebuild`, `--scip FILE`, `--verify`, `--phases` |
| `find` | `-k/--kind`, `-n/--limit` |
| `locate` | `-n/--limit` |
| `task` | `--files`, `--dependents`, `--tests` |
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
| `history` | `-n/--limit`, `--since DATE` |
| `history deepen` | `--by N`, `--since DATE` |
| `note add` | `--tag` |
| `note export`, `note import` | `--repo` |
| `more` | |
| `status` | `--verify` |
| `serve` | `--tools`; restricted mode: `--restricted`, `--ops`, `--cache-dir`, `--config-dir`, `--data-dir` |
| `capabilities` | `--tools` |
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

## Claude Code hooks

At the start of a Claude Code session (and after `/clear` or a compaction) the plugin adds one line saying what
KnowCode has indexed here and which tools answer callers, impact, HTTP and definitions:
`KnowCode indexes shop. Callers, impact, HTTP: kc {op: impact}; definitions: op find.` (under `core`
or `all`, `kc_impact` and `kc_find`) It says nothing in a directory with no
index or in a repo whose privacy file blocks KnowCode, reads no index and makes no network call. Turn it off with
`knowcode config set hooks.session_start false`.

The plugin also adds a `PreToolUse` hook for Grep, Glob and Bash (for Bash, only code searches such as `grep`,
`rg`, `git grep` or `find -name`). When the pattern names a symbol in an existing index, it adds a short note beside
the result: where the symbol is defined, its reference count by confidence, its top callers, and the call to make next
(`kc {op: context}` by default, `kc_context` under `core` or `all`). It never blocks the search and never indexes. The hook script decides first: a Bash command without a
search word in it exits in the shell without starting `knowcode`. See [integrations/claude-code](../integrations/claude-code/README.md).
