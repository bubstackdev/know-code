# Changelog

KnowCode follows [Semantic Versioning](https://semver.org/) and stays on 0.x while its surface settles.

## 0.1.1 (unreleased)

### Release

- Release binaries no longer contain build-machine paths. The C code in tree-sitter and its grammars now gets the same
  path remapping as Rust code, and each release build fails if a binary contains the builder's home or checkout path.

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
