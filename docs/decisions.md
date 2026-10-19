# Decisions

Release and first-run decisions for 0.1.0, made October 15, 2026, with the public-release gates clarified
October 16, 2026. This records intended behavior; it is not a claim that every item is implemented. What is
implemented is described in [usage](usage.md), [configuration](configuration.md) and [architecture](architecture.md).

## Audience, platforms, and installation

KnowCode is a local code graph for coding agents. Version 0.1.0 targets macOS
Apple silicon and Linux x86_64 and arm64. Windows waits for hardware on which to
validate it. Claude Code and other MCP clients are the primary interface; an editor
extension and a human-first CLI are outside this release.

Use cargo-dist for versioned GitHub releases and a shell installer, with Homebrew
distribution. Releases follow SemVer, remaining on 0.x while the surface settles.
The license is MIT OR Apache-2.0. There is no telemetry.

## Runtime and indexing

There is no daemon. The MCP server watches repositories and keeps their indexes
warm while it is alive. A per-repository writer file lock coordinates processes;
readers can continue using a committed snapshot during indexing.

The first query automatically indexes its repository, defaulting to the MCP
server's startup repository. A query waits roughly two seconds for freshness;
after that it answers from the available snapshot with explicit stale or incomplete
status while indexing finishes in the background. A missing index must never
produce a confidently complete empty answer. Index-version changes similarly
require stale disclosure and background rebuilding.

Background indexing runs at low priority with bounded concurrency and visible
progress. There is no battery-specific pause. Incomplete, stale, sampled, and
withheld answers retain their distinct meanings.

## Workspace and storage

`knowcode workspace add` registers roots. A light background scan discovers
repositories and their manifests and name summaries; full indexing happens lazily
when a repository is touched. Deleted checkouts are pruned automatically and a
least-recently-used size cap bounds derived cache storage. Notes are durable user
data, separate from disposable indexes.

The default index budget is 2 GiB (`cache.max_size`, with `0` disabling size
eviction). Retain current and in-use indexes even if they exceed the budget.
Size eviction keeps the last workspace summary for discovery, marks answers
that use it stale, and restores the full index on demand. Cleanup removes only
recognized index files, preserving notes, unrelated files, and stable lock files.

Settings live in one platform-user-config `config.toml`, editable through
`knowcode config set`. Legacy `cache_dir` and `ignore` settings migrate without
requiring repository-local configuration.

Fetching missing dependency repositories uses the user's Git credentials and
requires an explicit CLI command (`fetch-deps`); MCP does not clone repositories.
Deepening shallow history also requires an explicit CLI command (`history deepen`).
These decisions do not authorize adding either feature to the 0.1.0 scope.

## Client setup and removal

Provide a Claude Code plugin containing the MCP registration, hook, and skills.
`knowcode setup claude|codex|cursor` provides client setup, including configuration
snippets where appropriate. First use must not require manual index commands or
configuration edits.

`knowcode uninstall` removes KnowCode's registrations and cache, and asks before
removing configuration or notes. Preserve unrelated client settings and user data.

## Updates and network access

A daily update check only notifies; installing an update is manual through
`self-update`. The check is opt-out and is the only automatic network operation.
Indexing and queries stay local. Installer, explicit update, and explicit Git
operations necessarily access the network when the user invokes them.

## Publication gates

Publish a fresh repository from one audited snapshot, without private development
history. All snapshot files, including binary files, fixtures, tests, scripts,
comments, and documentation, pass a private-vocabulary scan and a full diff
review. The user reviews the exact snapshot before its first public push. Keep
private inventories and evaluation inputs outside the snapshot. Automated scanning
alone cannot guarantee that no identifying material remains.

Version 0.1.0 may ship after first-run acceptance, the scrub, the approved public
repository, and the distribution pipeline. The release goal remains open until a
reproducible evaluation on public repositories is published, including failures
and cases where KnowCode loses.

Until that evaluation is published, public material describes capabilities and
says that evaluation is coming, without comparative claims or evaluation numbers.
No public claim of being better than grep or any other tool may appear before a
published evaluation supports that claim. Narrow the claims if the results do
not support a cross-repository benefit.

## First-run acceptance

Measure a fresh macOS user account on Apple silicon when the machine is quiet and
a clean Linux x86_64 container. Include installation, `knowcode setup claude`, and
the first correct `kc_context` answer, without manual indexing or configuration
edits. Start with a public API-handler question in dotnet/eShop and measure its
indexing cost before finalizing the latency bar. Aim for five minutes overall; if
full indexing cannot fit, return an explicitly stale answer from the two-second
budget and require a correct, complete answer within five minutes. Check a small
public TypeScript repository as well.

Windows, the single graph abstraction, new languages, and new cross-repository
inference features are outside 0.1.0.
