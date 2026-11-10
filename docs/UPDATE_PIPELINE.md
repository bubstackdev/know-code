# Incremental update and publication pipeline

Status: coordinator-only writes, analysis outside write transactions, atomic publication, and WAL with bounded readers are accepted. The [first slice](IMPLEMENTATION.md) implements one-shot writer ownership, generation-tagged staging, atomic current-generation selection, failed-job cleanup, and read transactions. The [coordinator slice](COORDINATOR.md) adds native watching, durable registration, project/dependent invalidation, unchanged-unit references, shared freshness waiting, and restart reconciliation. Compiler-session reuse, broader recovery, cursor expiry, and tuned resource/checkpoint policy remain pending.

## Constraints

Saved-state updates, compiler-backed partial graphs, multiple in-process readers, background indexing after disconnect/login, 500 ms default freshness wait then stale fallback, one SQLite index per workspace, and low resource use are accepted. See [design](DESIGN.md) and [fact model](FACT_MODEL.md).

## Proposed flow

```text
Saved-file/project events or explicit update
  -> coalesced workspace change set
  -> scoped compiler analysis and invalidation
  -> staged replacement facts + source artifacts + coverage
  -> atomic publication and generation notification
  -> in-process readers select a committed generation
```

These are steps in one engine, not separate services or a workflow framework. Language providers share scheduling/publication contracts while retaining compiler-specific analysis behavior.

## 1. Observe and queue

The coordinator owns monitoring for registered workspaces; library callers can submit equivalent change sets. Normalize adds/edits/deletes/renames/configuration changes through one path. Coalesce redundant saves without losing updates arriving during analysis. Treat filesystem notifications as hints requiring reconciliation, including atomic-save renames and startup gaps.

Track observed input versions/fingerprints rather than assuming a Git commit or mtime alone identifies source. A query captures an observed freshness target; subsequent edits cannot extend its wait forever. File watching cannot prove instantaneous knowledge of all disk changes.

Optional checked-in workspace settings and central personal overrides resolve through the same shared settings path. Changes to effective analysis settings feed this queue and affected-compilation invalidation. Distinguish semantic configuration from operational CPU/memory tuning; changing a resource cap alone must not require rebuilding unchanged semantic facts. Exact configuration file/schema and reload/error behavior remain open; see [workspace configuration](ARCHITECTURE.md#workspace-configuration).

## 2. Determine and analyze affected units

Providers identify affected project/compilation/document units and semantic inputs, including configuration, dependency surfaces, and generated output. Reuse compiler state where it helps, with bounded concurrency/residency. Do not apply a universal one-hop invalidation rule to TS inference or Roslyn projects.

Accepted scheduling policy: prioritize repositories with active queries or recent saved-file changes while ensuring other queued repositories continue making progress under shared resource limits. One provider-neutral scheduler supplies this policy; providers supply their analysis units and dependencies. Query activity may raise priority for already-needed analysis, but must not force redundant analysis for fresh inputs or create a compiler workspace on every query.

Priorities do not expand resource caps, omit required target frameworks, extend a caller's freshness deadline, or let one consumer cancel shared work. Fairness must cover initial indexing/backlogs as well as updates, including sustained activity in a favored repository. Priority decay, aging/quotas, task granularity, and cooperative interruption remain implementation choices to measure; no latency bound or preemption mechanism has been selected.

Restore missing dependencies when necessary, but do not reinstall runtimes, run external full builds, or repeat unchanged preparation on every source save. Compiler-integrated generation is supported. Missing outputs and source errors produce valid partial analysis when possible.

Accepted: compiler analysis runs without holding a write transaction across it. Workers emit bounded, length-prefixed Protobuf batches over local stdin/stdout with explicit ownership/input context and a completion/report marker; they do not write the serving index. Short coordinator-owned staging transactions are a possible mechanism, not a transaction held open for a full compiler job. Detailed schema and limits remain open. A worker stream ending unexpectedly is not a completed partial graph: distinguish source diagnostics from incomplete protocol/job execution.

When inputs change during analysis, preserve their versions and keep newer work pending. Do not label superseded results current; whether to publish an intermediate explicitly stale-but-useful result or discard/rebase it is a scheduling detail to validate. Ownership must prevent obsolete jobs from publishing over newer results.

## 3. Stage replacements

Proposed: materialize changed-unit facts and required source/coverage into bounded workspace-owned staging state before live publication. Avoid holding an entire estate graph in RAM or a live write transaction across compiler work. Staging may use SQLite tables or another small durable workspace artifact; exact mechanics are open.

First-slice mechanism: full-refresh facts/source/reports are inserted into generation-tagged tables in the workspace SQLite database, in short transactions after each bounded frame arrives. Queries select only the published generation. Failed jobs drop their staging generation, and the next lock-owning writer removes abandoned staging. The coordinator slice adds project/compilation-owned replacement with generation manifests, so unchanged facts remain in place. Compiler-native incremental sessions remain future work.

Facts are owned by analysis unit, not blindly by target symbol. Replacing a callee must not erase unchanged caller evidence through an indiscriminate cascade. Invalidate obsolete bindings/dependents correctly; do not preserve old relationships simply because they are expensive to recompute.

Generated source and evidence must be ready before publication. Accepted initial source storage is the workspace SQLite database, sharing identical content across targets/generations while preserving distinct artifact provenance and ownership. Staging must make source payloads/references available before publishing their facts. Cleanup follows generation/read/recovery ownership; external source sidecars are not part of the initial serving design.

## 4. Publish atomically

Accepted ownership/publication: the coordinator alone writes each workspace index and atomically publishes completed changes. Embedded consumers submit update requests through the shared coordinator-control client and retain in-process coherent readers; they do not write the index independently. Proposed SQLite mechanism: apply changed facts, source references, analysis reports, derived search/adjacency changes, and generation metadata in one final transaction. Keep the central registry out of the critical fact-publication transaction where possible; do not assume separate databases commit atomically together. Exact writer fencing and replacement-owner recovery remain open.

Implemented first-slice selection: after successful worker completion/exit and compilation reports, a final transaction changes the current-generation pointer and reclaims old facts/source. Existing read transactions still see their original coherent generation through WAL. A per-workspace file lock excludes another one-shot writer; the persistent coordinator now owns those locks and exposes the shared control channel. See [coordinator operation](COORDINATOR.md) for registry/manifest durability and actual limits.

The transaction must publish a coherent analysis view, including explicit unavailable/invalidated regions where required. A partial graph with compiler errors is a successful coherent publication. Half a failed worker stream, stale facts relabeled current, or mismatched source ranges are not.

The accepted FTS5 lexical index lives in the same workspace database and is derived search state. Maintain its changes coherently with facts/source and ensure hits are scoped to the served generation/compilation rather than exposing staged, retired, or incompatible-target content. Reclaim entries consistently with source/read lifetimes. Exact FTS table/content layout and maintenance mechanics remain open; workers and wrappers do not implement separate search stores.

After commit, notify waiting consumers. Readers establish or refresh a local committed view; notifications are hints, while database generation state remains authoritative. Never advance acknowledged input baselines solely because a worker started or events were observed.

Rollback/cancellation/storage failure preserves the prior usable publication. Startup recovery distinguishes unpublished staging from committed state and requeues outstanding input changes. Exact crash recovery, fencing, durability settings, and transaction size require prototype evidence.

Accepted power-failure policy: a rebuildable index may lose recent indexing progress, while registrations/settings use stronger durability. On restart, reconcile the recovered index/input state with saved source before claiming freshness; an older coherent view can be served explicitly stale under the existing query policy. No recovered usable view means not-ready while indexing/recovery proceeds. This permits rolled-back progress, not a mixed generation, mismatched source evidence, or loss of user configuration.

Central durable registry/job metadata can survive a publication that the weaker-durability index loses. Therefore registry acknowledgements cannot substitute for verifying the recovered workspace generation/input manifest. Retained source payloads in SQLite and replacement-index cutover must obey the same coherence requirement. Exact SQLite synchronous settings, file/directory syncing, and reconciliation/recovery implementation remain open; do not infer power-failure guarantees from process-crash tests alone.

Accepted corruption recovery: automatically rebuild a corrupted derived workspace index while preserving registrations/settings and exposing recovery progress/failures through the existing job/status model. Reuse coordinator ownership, scheduling/resource limits, and replacement/cutover infrastructure. Data known to be corrupt cannot serve as stale fallback; a separate known-healthy retained view may qualify, otherwise return unavailable/not-ready with the recovery reason. Other healthy workspaces remain usable.

Distinguish corruption from unsupported format/version, permissions, disk exhaustion, and compiler/resource failures; those do not automatically establish a corrupted index. Detect/coordinate recovery once across consumers, avoid unchanged-failure rebuild loops, and bound any retained damaged artifacts under a deliberate storage policy. Detection/check timing, damaged-state isolation/retention, retry/backoff, and recovery mechanics remain open. Automatic repair of the durable registry/settings is not implied by permission to rebuild derived indexes.

## 5. Read with explicit lifetime

Accepted initial SQLite mode: WAL with bounded reader lifetimes and managed checkpoints. Proposed reader mechanism: isolated connections/transactions retain one committed view during a query while the coordinator publishes new work. Do not enable dirty reads or share a mutable SQLite handle concurrently across consumers.

Checkpoint policy must account for active/abandoned readers, query/publication latency, and aggregate WAL disk use across workspaces. Use the accepted cursor expiry policy to bound retained state; cleanup cannot discard source/index data beneath a valid reader. SQLite is a pinned bundled build with FTS5 across coordinator/native artifacts; exact version/build flags/linkage, checkpoint policy, cursor implementation/TTL, and synchronous settings remain open. WAL selection does not by itself implement generation-pinned pagination across separate requests or index replacements.

Wait for freshness before pinning the final read view, or reopen the view after notification; a reader that keeps its old transaction open will not discover later commits in that snapshot. Apply the accepted 500 ms configurable wait and overall deadline, then serve coherent stale fallback if necessary. No prior usable view means not-ready, not fabricated empty success.

Read/source evidence uses the same served generation. A request's cancellation must release its resources without cancelling shared indexing. Old views, source artifacts, and WAL/checkpoint progress must be bounded by explicit reader lifetimes; retention across page requests is limited to a valid expiring cursor, not an entire idle agent conversation.

Accepted pagination policy: all pages stay on the same served generation while updates publish. Cursors expire to bound retained views/state. Continuations preserve the initial query scope/evidence/budgets and do not silently advance generations; report known staleness when applicable. Expiration or lost state requires an explicit restart response.

The user accepted opaque workspace-bound cursor tokens usable across local clients and separate CLI invocations, with page queries remaining in-process. The retained-generation mechanism, concurrency limits, idle/absolute lifetimes, restart behavior and safe cleanup remain open; see [query lifecycle](QUERY_LIFECYCLE.md#accepted-cursor-portability). SQLite snapshot behavior alone does not make cursors portable between processes or recoverable after restart. Expiry must release resources safely and stop abandoned readers from blocking checkpoint/source reclamation indefinitely.

## Scope and resource implications

- One writer per workspace does not require serializing analysis across unrelated workspaces.
- Compiler concurrency, staging buffers, database connections/caches, source retention, and query expansion need explicit budgets rather than unlimited per-repository defaults.
- Accepted resource policy: automatic management by default, with optional per-user CPU/memory caps shared across coordinator-managed workspaces/providers. The scheduler accounts for workers and shared cache/staging costs when admitting work; priorities do not grant extra budget. Numeric defaults, CPU accounting windows, memory metrics, and enforcement mechanisms remain open.
- Accepted oversized-project policy: if analysis cannot fit a configured cap, stop the affected analysis and report resource-limited with the project/target, configured limit, and available measured context. Preserve prior usable results as explicitly stale; without a usable prior view, report not-ready with the resource-limit reason. Never publish an interrupted stream as a completed partial graph or silently raise the cap. Independent work can continue within budget; dependent coverage remains explicit. Ordinary queuing for capacity or CPU throttling is not itself proof that a project cannot fit. Avoid repeated retries of unchanged failing work on every query; exact retry triggers and termination/cleanup mechanics remain implementation work.
- Embedded consumers must use bounded Know Code query/cache allocations, but the coordinator does not own their host-runtime heaps or arbitrary application work. Define budget accounting/coordination for multiple in-process consumers before claiming an enforced aggregate per-user cap; do not advertise a scheduling budget as a proven OS-level hard limit.
- Current-state publication must not duplicate the full database per save or keep every compiler workspace resident.
- Continuity in v0.0.2 can compare relevant old/new facts during updates and store evidence links; it need not enable full historical graph queries.
- Cross-repository queries in v0.0.2 need a selected generation set and coverage policy. Per-database atomic commits do not create one atomic estate-wide snapshot.

## Prototype evidence needed

Use the existing focused invariant fixtures: incremental/full equivalence, source/coverage correctness, concurrent readers during updates, missed-event reconciliation, new saves during analysis, cancelled/crashed workers, rollback/restart, and bounded WAL/source retention. Measure publication cost independently of compiler time. Avoid creating a second validation framework.

## Primary references

- [SQLite isolation](https://www.sqlite.org/isolation.html): separate connections see committed transactions; WAL readers retain their starting snapshot and must end/restart it to see later writes.
- [SQLite WAL](https://www.sqlite.org/wal.html): reader/writer concurrency, checkpoint/reader-lifetime implications, and lack of atomic cross-database WAL commits as a set.
- [Source review](SOURCE_REVIEW.md): staged publication, input baselines, fact ownership, dependency invalidation, and ambient-state gotchas in the pinned reference implementations.

These references establish building-block behavior, not Know Code's implementation, correctness, or performance results.
