# Changelog

KnowCode follows [Semantic Versioning](https://semver.org/) and stays on 0.x while its surface settles.

## 0.2.0 (unreleased)

- Leaner MCP answers and tool list. A clean, current answer no longer lists its repos: `repos` and `provenance` are
  left out unless a repo is dirty or was answered from its last sync, or something is `stale`, `failed` or
  `missing_repo`. `complete` is said only when it is `false` (no `incomplete` means complete), and false flags, nulls,
  `offset: 0` and a search's `stats` are left out. `stale` is one `incomplete` entry naming repos by name, not path.
  `kc_search` and `kc_grep_ast` hits, and `kc_more` pages of them, are text rows (`repo/file:line kind symbol |
  text`) instead of objects; docs/answers.md has the format. On a three-repo sample, search answers are 40-60%
  smaller. The tool schemas and server instructions under profile `all` are 7,186 characters, down from 10,641:
  shorter descriptions and parameter lines, fewer advertised parameters (the rest are still accepted and in
  `kc_help <tool>`), and instructions that no longer repeat the descriptions. The instructions and the `kc_refs` and
  `kc_find` descriptions now send "what calls X" and "what breaks if X changes" to `kc_impact` (or `kc_trace`, depth
  2) with a `file:line` selector. The CLI's output and `--json` are unchanged.
- Answers whole enough to stop on. `find` on an exact name adds `declared`: one line per repo saying it has every
  declaration of the name there, with a C# `partial` type's parts (file:line each), a type's constructors (a record's
  primary one too), a method's overloads, and how many references in code name it (the other mentions are uses, for
  `refs`). `refs` groups references by caller (code before tests), marks rows in test files `test`, tallies callers
  and sites in `callers`, numbers the overloads the references resolve to (`overloads`, with signatures and the sites
  reaching each) and says which one each row calls (`overload`, in place of its `targets`). For an interface or base
  class, `refs` adds `hierarchy` (implementers or subclasses, those reaching it through another type, derived
  interfaces, DI registrations of it and of them) and `context` adds `conformers_indirect` and `di_implementers`; both
  say each empty list as "none found" with what was searched (`none_found`). A `context` on a partial type lists its
  `parts` instead of calling them other definitions, and lists constructors first among members.
- `knowcode fetch-deps --into DIR` clones the git repos a repo depends on (manifest dependencies written as git
  URLs, Terraform git module sources, GitLab CI includes of other projects, and repos whose checkout was deleted),
  shallow, one directory each, so cross-repo answers can reach them. `--dry-run` prints the plan. DIR must be outside
  every repo; only https and ssh remotes are cloned; a repo whose AI privacy file blocks it whole is skipped unless
  `--include-blocked`. `--add-root` adds DIR as a workspace root. CLI only: the MCP server never clones.
- `knowcode history deepen` fetches more history into a shallow clone (`--by N`, 1000 by default, or `--since DATE`)
  and reports how deep it is before and after, and whether `hotspots`, `coupled`, `ownership` and `reviewers` now
  have all the history they read.
- Each index is rebuilt once on the first query after updating (index schema 20): constructs KnowCode doesn't read
  are now kept in a table of their own. Answers are unchanged.
- Smaller MCP answers by default. `kc_refs` lists 20 references (was 25), `kc_trace` 25 nodes (was 40), `kc_impact`
  30 dependents (was 60), `kc_flow` 40 steps (was 60), `kc_unused` 25 findings (was 50). The `kc_context` budget is
  1200 tokens (was 2000), capped at 6000 characters, and the `kc_overview` map is 400 tokens (was 800). Every cut says
  what it left out, and the rest comes through `kc_more` or the parameter it names. MCP rows are leaner. A
  reference's `from` is the enclosing symbol's qualified name, with no column. What all rows of a reference list
  share (`targets`, `from`, `name`) is said once beside the list. Test files drop their ranking `signals`. Tree use
  sites in the symbol's own file are said by line. The CLI's output and `--json` are unchanged.
  `scripts/mcp_tokens.py` measures answer sizes, with `--count-tokens` and `--compare`.
- Smaller MCP tool schemas: each tool's schema lists only its common parameters, with short lines. `repo` and
  `max_chars`, which the server instructions state once, and the rarer parameters (`as_of`, `since`, `order`,
  `format`, `kinds`, `evidence`, `edges`, `cite`, …) are still accepted and documented in `kc_help <tool>`. Profile
  `all` costs about 4.5k tokens per turn for a client that loads every schema (was about 8.6k), and the four
  always-loaded tools about 0.8k (was about 1.8k); see eval/schema-cost.md.
- The MCP server's instructions, the descriptions of `kc_find`, `kc_locate`, `kc_refs`, `kc_context`, `kc_trace`,
  `kc_impact` and `kc_search`, and the plugin's skill descriptions now lead with the questions they answer ("who calls
  or uses", "where is", "what breaks if"). None is longer than before.
- The clean-machine acceptance report adds `knowcode-use.json`: KnowCode calls per case and session, by tool.
- Cited source: every row that gives a location (`trace` nodes, `impact` rows, `path` and `flow` steps, `refs` items,
  a `context`'s callers and callees) carries `cite`, the source lines it rests on, read from the working tree when
  the answer is built: an edge's site, else the line naming a declaration. By default (`cite: brief`) the first 12
  rows quote one line each, up to 120 characters, and the rest give where it is, with a `capped` entry saying so.
  `--cite lines` (MCP `cite: "lines"`) quotes every row, a declaration from its first line through its name (at most
  3), each line up to 160 characters; `cut` says what a cap left out. `--cite none` leaves them out. Text trees print
  the lines under each row. A `context`'s callers quote their line in `cite` instead of `code`, which remains under
  `cite: none`.
- Cites are checked at query time: each cited file is compared with the index the answer read, and `check` says
  `verified`, or names the drift: `changed` (the line still names what it cites), `moved` (quoted where it is now,
  with `was`), `gone`, or `unchecked`. Every cite is checked, past the brief quote cap too. Any drift makes the answer
  `stale`, naming the cites. `--cite check` (MCP `cite: "check"`) gives the check without the lines. A `context`'s own
  symbol carries a check-only `cite`.
- Walks that finish: every place a walk stops short gives the exact call going on from it. Each `frontier` stop of
  `trace` and `impact` has `next` (`kc_impact {"depth":3,"repo":"web","symbol":"OrderService.place"}`), and the
  `incomplete` hint ends with the first one. A `flow` step at the depth limit that leads on has `stop` `{reason,
  beyond, next}`, and the flow is incomplete (`depth`); a flow cut by its step or repo limit, and a path search cut by
  its node or repo budget, are `capped` with the call raising the limits; a path search finding nothing within its
  hops has `next`, the call looking further.
- `trace` and `impact` now walk the same symbol graph as `path` and `flow`, through the same gate. One visible change:
  a hop across a link matched by name, from code found `exact`, is now `unique_name` in `trace` and `impact` too, as
  `path` and `flow` always labelled it. That covers an SQL object matched by its name (evidence `sql_name`) and one of
  several HTTP handlers or SQL schemas preferred by a heuristic (`dominant`, evidence `name_dominant`). The evidence is
  as before; the hop now counts in `impact`'s `low_confidence`. Under the default evidence policy it is still followed;
  under `confirmed` it is still left out.

- When a name has definitions in code and in tests (a test helper named like the function it tests), the code's
  answers first, whatever the kinds: `context`, `trace`, `impact` and the other symbol lookups used to rank a test
  file's function above a source file's constant. The test's definition is listed among the alternatives.
- Razor files (`.razor`, `.cshtml`) aren't read. A C# `refs` or `impact` answer from a repo holding any is now
  incomplete with reason `unsupported`, naming one and saying their markup's uses aren't listed, with a `kc_search`
  hint, instead of looking complete.
- The pre-registered public agent study measured this release's candidate (commit 00aae5b): it missed 2 of its 4
  criteria (re-check rate, tokens) and met 2 (answer F1 and recall, unprompted use). See
  [eval/public-v1/results.md](eval/public-v1/results.md).

## 0.1.1 (2026-10-20)

- A workspace-wide answer no longer fails because one repo can't be read (a checkout deleted while the answer was
  worked out, a failing `git` call): it answers with the other repos and names the one left out as `missing_repo` or
  `failed`.

- Updating refreshes the Claude Code plugin: after `self-update`, and when `serve` starts with an older bundle,
  KnowCode rewrites the bundle and runs Claude Code's plugin update. If that can't finish, it prints the
  `knowcode setup claude` command that does. `setup claude` also moves an installed plugin to the current version.
- The Claude Code hook script exits without starting `knowcode` for Bash commands that can't be code searches.

- `status` and `kc_status` always show the AI privacy policy they apply, indexed or not: the file's state, the file
  name looked for, the global level, `block_at` and how many paths are blocked. The CLI text gives it one line.
- `uninstall` no longer asks Claude Code to keep the plugin's data directory, where KnowCode keeps nothing. Claude
  Code's orphaned cache copy of the plugin is left to Claude Code, for sessions still running.
- Tests: the shared `commit` test helper reports git's output when a fixture commit fails.

- Homebrew is back: `brew install bubstackdev/tap/knowcode`. For a Homebrew installation `self-update` refuses with
  "Homebrew manages this install; run `brew upgrade bubstackdev/tap/knowcode`", and the update notice and
  `self-update --check` name that command. After `brew upgrade`, the next `serve` start refreshes the Claude Code
  plugin.

### Release

- Releases publish a Homebrew formula to `bubstackdev/homebrew-tap` again. The publish job commits as "KnowCode
  release bot" with a noreply address, has read-only `GITHUB_TOKEN` permissions, and is the only job given the
  `HOMEBREW_TAP_TOKEN` secret, which reaches only its push step, through `env:`. On a tag push, CI first checks that
  the tap is public and that the token can push to it; pull requests and pushes to `main` need no secret.
- Release binaries no longer contain build-machine paths. The C code in tree-sitter and its grammars now gets the same
  path remapping as Rust code, and each release build fails if a binary contains the builder's home or checkout path.

### Release testing

- Clean-machine acceptance is one script in the repo, `scripts/acceptance/clean-machine.sh`, for macOS and Linux. It
  never starts an interactive shell, covers the installer adding `~/.local/bin` to PATH, compares client settings
  entry by entry from a snapshot taken right before uninstall, records two client sessions per case from the
  client's transcripts (recorded, not gated), and checks that a new shell starts cleanly after the removal steps.

## 0.1.0 (2026-10-19)

The first public release, for macOS on Apple silicon and Linux on x86_64 and arm64.

### Answers

- One answer contract for every command and MCP tool: `complete`, `incomplete` with closed-set reasons (`capped`,
  `budget`, `deadline`, `stale`, `failed`, `missing_repo`, `unsupported`, `policy`, `withheld`), `sampled`,
  `provenance` for every repo consulted, and cursors that continue a cut list exactly once (`knowcode more`,
  `kc_more`).
- Every edge carries a confidence (`exact`, `unique_name`, `ambiguous`, `external_likely`, `unresolved`) and evidence:
  how the target was found and what that did not check. An evidence policy (`--evidence`) chooses which edges a walk
  follows.

### Questions it answers

- Definitions by name (`find`), files for a concept (`locate`), symbols at a line (`at`), ranked text and regex search
  (`search`), and structural search with ast-grep patterns (`grep-ast`).
- One bundle per symbol (`context`), every reference (`refs`), call trees (`trace`), paths between symbols (`path`),
  and end-to-end flows from an entry point (`flow`), across repos where HTTP routes, the message bus, SQL,
  dependency injection, Terraform modules or Ansible roles link them.
- Before a change: `impact` by symbol, file or diff, with dependents by proximity and co-changed files; `tests` to
  run; `coupled` files; `reviewers`; dead-code candidates (`unused`) and rename previews (`rename`) for C# and
  TypeScript.
- History: `hotspots` with code-health biomarkers, `ownership`, and functional `areas`.
- How a repo is deployed, from its GitLab CI pipeline through shared templates to Ansible, Terraform, cloud CLIs,
  containers and releases (`context <repo name>`), with every include accounted for and every list continuing by
  cursor.
- Constructs an extractor recognises but doesn't read (DI assembly scans, run-time SQL table names, publishes of
  unknown message types, Jinja-named includes, Terraform `moved` blocks, unread CI include and Angular lazy-route
  forms) make the answers drawing on them incomplete (`unsupported`), naming each.
- Orientation: `overview`, `deps`, `routes`, `tables`, `events`, `docs`, `status`.
- Notes anchored to symbols, kept in the user data directory, with export and import.

### Languages

- C#, TypeScript, TSX and JavaScript, Swift, Python, Rust, ColdFusion, SQL, Terraform, Ansible and GitLab CI, with
  manifests from SwiftPM, XcodeGen, Cargo, MSBuild and npm. Angular, ASP.NET, Entity Framework, MassTransit,
  NServiceBus and common DI containers are modelled. Precise references are read from a SCIP index when one is
  present. See [languages](docs/languages.md) for what is and isn't covered.
- A code base's own conventions (message-type attributes, consumer bases and publish helpers, gateway calls and
  request contracts, DI scope values, CI and Ansible names) are configured under `[conventions.*]` in `config.toml`,
  so its messages and requests link as the libraries' do.

### Running it

- `knowcode setup claude`, `codex` or `cursor` connects a client; the Claude Code plugin adds a Grep/Glob/Bash hook and
  four workflow skills. `knowcode uninstall` removes what setup recorded, checks that the client no longer lists
  it, and ends with the exact steps to remove a shell-installer installation.
- In Claude Code, `kc_context`, `kc_refs`, `kc_impact` and `kc_locate` load up front (`anthropic/alwaysLoad`, about
  1k tokens); the other tools stay behind tool search.
- The first query indexes automatically. MCP queries wait about two seconds and otherwise answer from the last
  snapshot, marked stale, while a low-priority background worker finishes.
- Indexes live in the user cache directory, bounded by `cache.max_size` (2 GiB by default), with deleted checkouts
  pruned automatically. Nothing is written into indexed repos.
- One `config.toml` for settings, `knowcode config` to edit it, and workspace roots for multi-repo answers.
- Repos can block content from AI tools with a privacy file (`.knowcode-privacy.yml`, or the name and keys
  `[privacy]` configures); blocked content never reaches an answer or the cache.
- No telemetry. Indexing and queries make no network calls. A daily, notify-only update check can be turned off.
- Shell installer built with cargo-dist; `knowcode self-update` for shell installs. Homebrew follows in 0.1.1.
