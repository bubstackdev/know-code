# Parent-folder repository discovery

The accepted discovery policy stops at the first repository boundary. A `.git` directory or file identifies that boundary, including a Git worktree or submodule. Nested repositories/submodules require explicit registration. Discovery skips directory symlinks; an explicitly selected symlink parent or repository is canonicalized. All enrolled workspaces use the existing registry, compiler preparation, saved-file scheduler, resource limits and cursor lifecycle.

## Public contract

- `know-code parents add PATH [--exclude RELATIVE_PATH ...]` durably adds/re-registers a canonical parent and replaces its exclusions as one object. Exclusions are exact relative path prefixes, not globs; absolute paths, empty paths and `.`/`..` components are rejected. Built-in dependency/build/state directories remain excluded. Exclusions affect future enrollment only.
- `know-code parents list` reports registrations, exclusions, availability, scan progress and bounded errors.
- `know-code parents scan PATH` requests a new reconciliation without clearing explicit child suppressions.
- `know-code parents remove PATH` stops discovery for that parent. Already-enrolled workspaces, settings, indexes and other overlapping parents are retained.

Explicit `unwatch` durably suppresses that canonical repository path from future automatic enrollment, even under overlapping parents. Explicit `watch`/`index` clears its suppression after successful registration. Changing parent exclusions, scanning or removing/re-adding a parent does not clear a child's explicit suppression.

Missing parents retain their registration and become unavailable. A different filesystem identity at the same path is rejected until explicit parent re-registration; the original identity resumes when it returns. Discovered missing workspaces retain the existing unavailable/reconciliation behavior. Overlapping parents and explicit aliases converge on the existing canonical workspace registry; distinct Git worktrees retain distinct indexes.

The additive local Protobuf operation is `parents`, with an action/root/exclusions request, repeated parent statuses and discovery capability version 1. Clients must reject peers without that capability. This is enrollment control, never a query or separate analysis engine.

Management requests choose an exact retained parent key before resolving a current filesystem alias. A missing parent or a parent replaced by a symlink can therefore still be scanned/removed by its original registered path. Ordinary aliases resolve when there is no exact registration. `add` always canonicalizes the explicitly selected current directory.

## Execution and evidence

The host's ordinary `discovery` module owns traversal/parent configuration; the coordinator invokes its existing registration helper for both explicit and discovered repositories. Existing registrations are reused without reanalysis or dependency preparation. Missing/failed analysis remains a workspace status, distinct from a parent traversal error. No new analysis pool, query engine, dependency, SDK implementation or source-filter policy is introduced. STOP is an enrollment boundary; compiler-selected or otherwise eligible nested source inside an indexed workspace retains its existing behavior.

Scans use cooperative bounded slices (initial tuning: at most 256 entries/about 10 ms), resumable directory iterators and round-robin scheduling. Completed scans wait 30 seconds before reconciling again; newly created repositories need no explicit scan request. Active workspace saves retain the existing native watcher path. These are tuning defaults, not hard filesystem-call deadlines or a guaranteed enrollment latency. Discovery pauses during runtime maintenance.

| Shared implementation guard | Behavior when exhausted |
| --- | --- |
| 64 durable parents | Reject another registration without changing existing settings. |
| Four retained directory iterators across all parents | Other parents wait for a slot; slices rotate among admitted scans. |
| 4,096 pending directory paths across all parents | Report an explicit partial scan and continue bounded work; narrow parents or add exclusions. |
| 64 directory levels / 16,384 entries in one directory | Report an explicit partial scan and release traversal state; narrow the selected parent. |
| 128 exclusion paths / 16 KiB per parent | Reject the replacement atomically. |

These bounds do not preallocate their maximum memory or multiply iterator/queue limits per parent. Inaccessible entries and guards preserve a bounded error plus error count; the parent never reports an exhaustive idle scan after omitted work. Phase is `queued`, `scanning`, `idle`, `partial` or `unavailable`. Counters describe the latest scan; `enrolled` includes eligible workspaces already in the registry, and `suppressed` records explicit unenrollment decisions. Registrations/exclusions/suppression are durable in the existing FULL-synchronous registry; transient iterators/counters restart after coordinator restart. Discovery skips symlinks by default but is ordinary same-user filesystem traversal, not a security boundary against hostile concurrent mutation.

No live coordinator or original corpus is used for validation. Root owns final assembly with the other release work; helper/unit tests alone do not certify discovery integration or the release.

## Focused validation

Implementation revision `82460f8` passes five host unit groups, isolated host compilation, locked host Clippy with warnings denied and formatting. A fresh owned TS/coordinator fixture passes 16 integration groups, including automatic future-child enrollment without a manual scan, overlapping parents/aliases, `.git` file boundaries, durable suppression/restart, saved compiler-backed updates, unavailable/replaced parent/child identities, exclusions/removal retention and management of a parent replaced by a symlink. Exact source/output/harness hashes, logs and captured responses are retained in [.artifacts/parent-discovery/1794339914835104000/handoff.json](../.artifacts/parent-discovery/1794339914835104000/handoff.json).

The test uses an isolated Debug host and protocol-7 provider resources from the frozen `a24d1ce` candidate. It does not certify a newly assembled archive, SDK/MCP parent-management surface, all languages, hostile filesystem races, whole-suite correctness or performance. The integrator must regenerate matching protocol artifacts/builds and validate its final combined candidate.

Retained unsuccessful attempts remain failures: two concurrent timestamp-named unit fixtures collided, causing one to encounter the other's intentional depth-limit tree; an atomic sequence now makes fixture roots unique. The first TS run correctly returned an absent old symbol with empty relevant coverage, exposing an overstrict fixture assertion; the corrected helper requires a successful current generation and complete workspace summary while preserving nonempty relevant compiler coverage for positive findings. Its assertion-only replay includes five rejection controls, and the final fresh runtime run passes. Independent review also caught the replaced-symlink management bug; the exact-key fix and real control regression are included in `82460f8`.
