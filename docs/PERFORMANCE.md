# Performance and evaluation

Status: provisional planning budgets. A [first public-corpus performance pass](INDEXING_PERFORMANCE.md) now records Debug indexing measurements and a compact-key storage improvement. It is not an estate-scale or release benchmark. See [implementation evidence and limits](IMPLEMENTATION.md).

## Resource-efficiency requirement

Widely published builds have zero telemetry. Debug/personal diagnostic builds may gather extensive local traces, but diagnostic overhead must be measured separately; do not report instrumented runs as identical to public-release cost or conceal collection by calling it logging. Test public artifact/profile behavior for absence of usage collection, sinks, exporters, and instrumentation-only background work. Preserve ordinary errors/status/index functionality.

Local Debug/personal payloads are unrestricted and may include complete source/analysis/request/result snapshots. Measure serialization, disk write volume, and retention overhead separately. Content permission does not establish a storage budget or require copying every full workspace on every event; preserve useful replay context with measured capture costs.

Automatic diagnostic rotation is required, with an initial configurable 10 GiB budget per user across workspaces, including payload/reproduction bundles. Exercise active writes during pruning, oversized events, disk-full outcomes, and incomplete trace reporting. Confirm pruning is restricted to diagnostic-owned files and never deletes live index/registry/source state. This disk budget does not allocate resident memory or justify unbounded collection queues.

SQLite is the accepted initial fact/text-index store with bounded graph traversal in Rust, subject to these performance gates. Initial smoke timings are not release performance evidence. Exercise actual schema/query plans, concurrent readers with coordinator writes, checkpoint/generation lifetime, cold disk-backed traversal, and aggregate memory; store selection does not waive the low-footprint or estate-scale requirements.

SQLite FTS5 is accepted for initial lexical search. Evaluate TS/C# symbol names, qualified names, identifier styles, source text, punctuation, Unicode, and representative agent retrieval tasks before selecting tokenizer/ranking/layout. Measure search latency/quality, FTS size/write amplification, duplicated text storage, incremental maintenance, and cold/filtered queries. Verify served-generation scope and exact source evidence during updates/retention; text hits must not become fabricated semantic edges. Engine selection does not promise substring/regex support, finished ranking, or measured quality.

WAL mode is accepted, with bounded readers and managed checkpoints. Pin SQLite build/version/options and measure checkpoint latency, write/query tails, and aggregate WAL growth under slow/abandoned cursors and mixed readers/writes. Verify cleanup honors served source/generation lifetimes and expiry releases retained state. Exact checkpoint policy, reader mechanism, and durability settings remain open; do not infer bounded disk use or a finished pagination mechanism from WAL mode alone.

SQLite is bundled at a shared pinned version/build-feature policy across coordinator and native libraries, including FTS5. Verify actual loaded SQLite identity/features for packaged .NET/Node/coordinator artifacts and measure native package size/startup overhead. Keep tests of intentionally compatible mixed Know Code versions separate from the same-release build pin. Exact SQLite version, binding crate, flags, and linkage remain open.

Recent rebuildable index progress may be lost after power failure; registrations/settings use stronger durability. Evaluate recovered-generation coherence, source artifact availability, reconciliation before freshness claims, stale/not-ready behavior, and durable registry acknowledgements referring to lost index progress. Report recovery work/time and publication sync cost separately. Distinguish process termination from power-loss durability evidence; exact synchronous/fsync mechanisms remain unselected and untested.

Corrupted derived indexes rebuild automatically with visible progress/failures while registrations/settings remain preserved. Exercise corruption detected during opening/querying, simultaneous consumer reports, interruption during recovery, and an unrelated healthy workspace. Verify known-corrupt data is never returned as stale evidence, recovery is deduplicated/bounded, and unsupported formats/access/resource errors do not trigger corruption-rebuild loops. Measure detection/rebuild cost and retained damaged-artifact footprint; exact detection and retention mechanisms remain open.

The accepted layout is one index database per repository/worktree plus a small central registry. Measure registry contention, per-database connection/page-cache overhead, open/close costs, and total file-handle/memory residency across hundreds of registered indexes. One query or consumer must not eagerly open every database. Validate independent workspace updates and mixed-language results within the same workspace index.

The user explicitly requires exceptionally light and performant operation. Neither the available M5 Max workstations nor the proposed reference machines define minimum/recommended hardware. No minimum hardware requirement or numeric memory ceiling has been accepted. Earlier numeric budgets are provisional ceilings/goals to refine, not permission to consume those resources or evidence that they are necessary.

Automatic resource management is accepted, with optional per-user CPU and memory caps. Evaluate shared limits across concurrent repositories/providers, worker startup and peak residency, staging/caches, and multiple embedded clients. Report the controlled scope, accounting metric/window, any overshoot, and resource-limited outcomes; host-runtime/application allocations outside Know Code must be distinguished. Numeric defaults and enforcement mechanics remain open. Verify normal default operation requires no tuning and configured budgets do not multiply with repository count.

If a project cannot fit a configured cap, its analysis stops with a resource-limited outcome and prior results remain explicitly stale; no prior usable view means not-ready. Exercise cap exhaustion during analysis/staging, cleanup, retained source/generation consistency, useful errors, and independent work continuing within budget. Verify interrupted facts never become a completed partial publication, limits are not silently raised, and repeated queries do not create an unchanged-failure retry loop. Distinguish ordinary capacity waits/CPU throttling from an analysis that cannot fit; record measured enforcement overshoot while refining the mechanism rather than claiming unproven hard-limit guarantees.

Report core resident memory, language-worker memory, aggregate peak memory, idle CPU, update work, disk footprint, and query latency separately. Resource efficiency must hold alongside correctness; reducing footprint by silently omitting semantic analysis is not success. Large estate throughput may scale with workload, but baseline operation must not require workstation-class resources.

Design directions to evaluate include disk-backed facts, bounded working sets, lazy project loading, reusable analysis results, minimal source copying, and controlled worker residency/concurrency. These are mechanisms to test, not claims that arbitrary projects fit a fixed memory limit.

## Reference workload

- Pin a machine with 8 modern CPU cores, 16 GiB RAM, and local NVMe storage; publish exact CPU, OS, and configuration.
- The preceding machine is a proposed portability baseline, not the user's confirmed hardware. The primary v0.0.2 target is a local Apple M5 Max workstation: 64 GB unified memory here, 128 GB at work. Pin storage, OS, exact machine configuration, and Know Code's own resource budget before establishing estate-scale targets. Do not transfer the baseline budgets to that hardware as measured guarantees.

The 64 GB machine is the user's available local development/evaluation platform; the 128 GB machine is the work-scale platform. Measure both without making 128 GB a universal product requirement. Memory accounting includes the Rust core, caches, and compiler workers; report idle serving separately from active indexing. Higher machine memory should permit larger caches or more analysis concurrency without changing graph semantics or forcing all compilers resident.
- Freeze corpora at approximately 100K, 1M, and 10M indexed source LOC after exclusions.
- v0.0.1 fixtures must include TypeScript, C#, and mixed workspaces. Measure both .NET and Node.js in-process bindings separately from MCP; report binding overhead and any analysis worker costs.
- v0.0.1 supports macOS on Apple Silicon only (ARM64). Pin macOS, toolchain, and actual machine configuration in measurements. Intel Macs and Windows/Linux are not acceptance platforms; minimum macOS version remains open.
- Concurrent multi-process access to one repository index is required in v0.0.1. Measure simultaneous .NET/Node/MCP readers during updates, aggregate memory across consumers and language workers, publication/lock waits, and duplicate analysis suppression. Do not infer process-sharing efficiency from a single-client run.
- Background indexing continues with zero consumers. Measure coordinator idle CPU/memory, save-to-semantic freshness without connected clients, worker idle reclamation, and resource use when many workspaces are registered but unchanged. Client disconnect must not stop required updates or strand uncommitted work.
- The coordinator starts automatically at user login. Measure time to usable existing indexes, startup reconciliation, and background resource contention after login. A login must not unconditionally trigger full reindexing or eagerly load every compiler workspace.
- Registration supports individual repositories and parent-folder discovery, automatically enrolling/indexing newly discovered repositories under registered parents subject to exclusions/shared limits. Measure discovery/startup cost for overlapping roots and large trees, newly cloned repositories, additions while stopped, and excluded paths; registration overlap must not cause duplicate indexing. Directory discovery must not become a full source scan before each query or bypass analysis budgets. Exact traversal/exclusion rules and discovery timing remain open.
- Missing repositories retain their index/settings until explicit removal and resume automatically on return. Exercise unmount/reappearance, disappearance during analysis, coordinator restart while missing, and a different repository occupying the same path. Verify unavailable status, no empty/deletion publication for a missing root, bounded retries, retained state, identity checks, and reconciliation before freshness claims. Measure return-to-queryable time and retained disk footprint.
- Parent discovery skips directory symlinks by default, with explicit registration of linked repositories supported. Include symlink cycles, an alias plus real-path registration, and a retargeted link. Verify discovery stays bounded, aliases do not duplicate indexes/workers, and the discovery rule does not suppress compiler-resolved linked source. Exact canonicalization/retarget handling remains under design.
- Extend fixtures for Angular template-to-component references in v0.0.2 and Swift/Rust source analysis in v0.0.3. Publish coverage, freshness, and worker/core resource costs per provider; do not equate additional parsers with equivalent semantic support.
- Publish source bytes, file count, language mix, maximum file size, node/edge counts, and excluded paths.
- Embedding-based semantic code search is deferred beyond v0.0.1; exclude embeddings/generative extraction from that release's baseline and dependencies. Measure symbol/text retrieval and compiler-backed graph queries directly.
- Do not infer hardware equivalence between ARM and x86 or performance equivalence from LOC alone.

## Normal workstation contention

The user explicitly confirmed that heavy concurrent video encoding is normal during development. Include it as an acceptance scenario alongside a quiet baseline. A quiet rerun helps isolate causes; it does not replace validation of ordinary busy-workstation use. This confirmation accepts the workload, not arbitrary load-average values as a guaranteed operating envelope or any new latency/CPU/memory ceiling.

The accepted priority is **interactive query responsiveness over indexing freshness** when Know Code's work competes for resources. Background indexing should make slower, steady progress within the shared limits, with fairness across queued repositories. Preserve the existing 500 ms freshness-wait policy and explicit stale fallback. Do not claim current results merely to meet a latency target. This selects priority, not a particular scheduler, CPU reservation, preemption mechanism or numeric SLA.

Use reproducible encoding inputs/settings, concurrency and priority where possible; record actual CPU/memory/I/O pressure and cache state. Compare matched runs with and without Know Code and record both effects: what contention does to Know Code and what Know Code does to encoding/foreground work. Do not stop unrelated workloads or change their priority without explicit authorization.

| Gate | Evidence |
| --- | --- |
| Interactive queries | End-to-end latency distribution, deadline misses, result budgets and semantic parity through each native API. Preserve freshness/coverage/source evidence. |
| Saved updates and indexing | Queue age, actual progress, save-to-publication latency, worker residency, and startup/restore/planning/compiler time separately. Report stale fallback honestly. |
| Resource coexistence | Aggregate Know Code CPU/physical footprint/I/O and the change in foreground/encoding throughput or latency. Shared budgets must not multiply per repository. |
| Functional integration | Compiler setup completes and checks run under representative contention. Keep setup/liveness waits distinct from query-latency goals; retain timeouts and failures rather than marking unexecuted checks passed. |

The symbol-search run's compiler-setup failures remain unresolved integration evidence in this supported scenario. Its core/native search checks passed; that narrower success is not full compiler validation. Diagnose scheduling, repeated work and preparation cost before deciding whether any setup waits are inappropriate. Do not merely raise deadlines or relax responsiveness goals to obtain a green run. Other numeric contended-workload targets and scheduling/enforcement mechanics remain open.

## Accepted first lookup gate

The user accepted **p95 <=100 ms** for warm, focused symbol/reference lookups returning **at most 100 results**, served from an **already-current index during concurrent encoding**. Initial scope is named TS/Polly fixtures through the native APIs. Preserve coverage, source/version identity, targets and result budgets. This accepts a benchmark target, not a runtime cancellation deadline, a new freshness policy or a guarantee across arbitrary queries and hardware.

Time the actual public .NET/Node API boundary, including normal serialization, common-core work, coordination and result conversion; retain direct Rust measurements separately with their known serialization scope. Use a stable served generation and no pending saved changes; require observed current freshness. Report stale/not-ready results as unmet preconditions, never as faster passing lookups. First-call/JIT, cold storage, pending-save freshness waiting and compiler preparation have separate measurements.

For each named query/runtime, predeclare warmups and sample count, compute empirical p95 consistently, retain every measured tail and verify results/limits. Do not pool fast queries to hide a failing case or discard scheduling/GC outliers after inspection. Record actual concurrent workload and its interference; load average alone does not define a reproducible encoding fixture. Small local runs are evidence, not complete release certification.

The preceding snapshot benchmark has one observed Node TS exact-name p95 around 148 ms, and no reference-query timings. Its unmanaged views report unverified freshness. Those results calibrate query execution but do not establish this managed current-index gate. Full compiler-suite setup failures also remain open independently of lookup latency. MCP/CLI transport budgets below remain proposals; this accepted gate applies to the native lookup slice.

## Proposed budgets

| Metric | Initial release target | Later target |
| --- | --- | --- |
| Complete structural index, 100K LOC | <=10 s | <=3 s |
| Complete structural index, 1M LOC | <=60 s | <=15 s |
| Complete structural index, 10M LOC | <=10 min | <=2 min |
| Single-file edit to published structural generation | p95 <=1 s | <=250 ms |
| 100-file edit batch to published structural generation | p95 <=10 s | <=3 s |
| Warm symbol/reference query, <=100 results | MCP p95 <=100 ms | <=25 ms |
| Bounded traversal | MCP p95 <=250 ms | <=75 ms |
| Context assembly, <=4K output tokens | p95 <=1 s | <=300 ms |
| Peak indexing RSS, 1M LOC fixture | <=2 GiB | <=1 GiB |
| Idle serving RSS, same fixture | <=512 MiB | <=256 MiB |
| Graph plus lexical index size | <=3x filtered source bytes | <=1.5x |
| Readiness with an existing index | <=1 s | <=250 ms |

Single-file latency assumes a file <=100 KiB and bounded affected dependencies; publish the dependency bound for each fixture. Traversals are depth <=3, <=1,000 visited nodes, <=100 returned nodes. Earlier fact/text-index size budgets exclude retained source payloads; report their contribution separately and publish the total workspace database plus WAL/staging footprint. Optional embedding artifacts are outside v0.0.1. Memory is measured across the relevant process tree, not just the parent process.

Retained source text initially lives in SQLite, sharing identical content across targets/generations. Measure payload deduplication, large generated-source ingestion, bounded source retrieval, publication/write amplification, and safe reclamation after reader/cursor release. Verify sharing does not merge distinct artifact provenance or retain an unbounded history archive. Exact content keys/schema/compression remain open; external blob storage requires a measured need.

Rebase these targets after a prototype. Structural indexing completion is not compiler-complete semantic enrichment.

## Semantic enrichment

Worker messages use Protobuf batches over local stdio. Measure encode/decode, allocations, bytes transferred, staging backpressure, worker startup, and compiler time separately on actual TS/C# fact sets. Bounded batches must prevent full-project materialization in the transport. Source-heavy/generated artifacts and cancellation while blocked on output are important workload cases. Do not infer throughput from codec reputation or conflate worker transfer with in-process query latency.

Compiler-resolved TypeScript/C# symbols and references are required in v0.0.1, using the TypeScript compiler and Roslyn. Treat their complete, queryable semantic generation as a release-critical measurement, not an optional enrichment outside reported performance. Structural timings remain useful for partial readiness but cannot stand in for required semantic results.

Saved code with compiler errors must still publish valid partial semantic graphs when possible. Measure save-to-partial publication and coverage separately from successful-fixture full analysis. Verify fresh/incomplete and stale/complete states are represented independently; the 500 ms freshness wait must not wait for a clean build after a usable current partial generation has published. Test errors introduced then fixed, syntax/binding failures affecting dependents, deletion of previously resolved targets, and source diagnostics versus analyzer/storage failure.

Establish language-specific budgets on pinned buildable fixtures. Report dependency/build preparation, indexer execution, import, and resolution separately. Provide progress, cancellation, coverage, and resource limits. Never silently downgrade semantic results to meet a structural deadline.

Automatic dependency restoration is required behavior when needed for analysis. Report cold restore/network preparation separately from compiler indexing while also publishing total time to usable results. Exercise dependency-present, dependency-missing, lockfile/project-input changes, offline/failed restore, and concurrent consumers requesting the same workspace. Verify ordinary source saves do not trigger repeated restores and failures do not create an unbounded retry/resource loop.

Node.js and the .NET SDK are external prerequisites. Verify useful errors for missing/incompatible versions, .NET runtime without an SDK, and tools unavailable to the login-started coordinator. Probe at the shared setup boundary and cache/invalidate appropriately; warm graph queries must not spawn toolchain-detection subprocesses. No auto-install work is part of readiness/performance accounting.

Repository-selected TypeScript versions and .NET SDK selection must be honored. Include repositories using different supported versions, an unsupported selection, missing SDK/package cases, and a selection change. Verify effective compiler/SDK provenance, actionable failures, toolchain-scoped cache invalidation, and no silent substitution outside repository selection policy. Measure worker isolation/startup/churn and aggregate memory under the shared budget. Keep compiler compatibility distinct from worker-host runtime compatibility; supported ranges and unpinned defaults remain open.

Continuous freshness after file saves is a confirmed goal and permanent product boundary. Measure save/change-to-queryable structural and semantic generations separately, including event detection, queue/debounce time, worker startup, dependent resolution, and publication. Include rapid edit bursts, atomic-save rename patterns, edits arriving during analysis, idle-to-active transitions, and configuration/branch changes. Report freshness together with idle/active resource consumption. Unsaved editor buffers are out of scope; numerical semantic freshness targets remain undecided.

Indexing prioritizes repositories with active queries or recent saves, while other queued repositories continue progressing under shared resource limits. Evaluate an initial-index backlog alongside sustained queries/saves in a favored repository. Report favored-repository freshness, background progress/maximum wait, worker churn, and aggregate resource use. Verify fresh repeated queries do not trigger redundant compiler work, prioritization does not bypass caps, and continuous favored activity does not starve other work. Exact scheduling algorithm and numeric fairness targets remain open.

Queries use a bounded freshness wait defaulting to 500 ms, configurable per query and capped by the remaining caller deadline, then coherent stale fallback if needed. The budget bounds waiting, not total query latency, and fresh queries must not incur an unconditional wait. Measure wait time, query execution time, and total caller-visible latency separately. Warm fresh-query budgets exclude unnecessary waiting, but publish mixed-workload latency including actual waits. Report fallback rate, stale age, not-ready outcomes, and errors so low latency cannot hide poor freshness. Exercise a continuously changing workspace, initial indexing with no prior generation, and cancellation by one of several readers.

## Estate-scale evaluation

The user's work environment includes hundreds of repositories, C# services, and an Angular frontend. The single-workspace budgets above do not establish adequate estate-wide performance.

Add a representative multi-repository corpus and measure total source bytes/symbols/edges, project/target-framework counts, package/service links, transient generation retention, and graph density. Initial queries are current-state-only, not a retained historical archive. Measure mixed ingestion/query load, update queues, skew between large and small repositories, cross-repository fanout/hops, global search/ranking, cold shards, memory ceilings, and latency under concurrent clients. Report authorization and partial-result behavior for cross-shard queries.

Whole-estate cross-repository queries target v0.0.2 on a local Apple M5 Max workstation. Numeric estate-scale targets remain open until memory/storage budgets and workload assumptions are clarified. Include bounded-cache/cold-index queries, compiler-worker churn, and competing foreground workload. Do not promise that hundreds of repositories fit in RAM or that single-workspace p95 applies to estate-wide traversal. See [scaling](SCALING.md).

## Quality goals

Internal source ranges use UTF-8 bytes tied to the indexed source version. Include non-ASCII identifiers, combining characters, astral characters, CRLF text, provider-coordinate conversions, generated mappings, and source edits after publication. Verify converted source evidence retrieves the exact served text and does not confuse UTF-8 byte offsets with host-native string indices or offsets in differently encoded physical files. Measure conversion/index overhead; exact endpoint and API line/column conventions remain open.

For v0.0.x compatibility, exercise supported mixed library/coordinator versions and deliberately incompatible pairs. Verify .NET/Node expose equivalent actionable update-required errors, no unsafe index interpretation occurs, and an incompatible client does not start a second indexing owner. Keep protocol compatibility, index readability, and native ABI loading checks at their actual boundaries; exact supported ranges remain open.

Index-rebuild upgrades construct a replacement alongside the current workspace index before cutover. Exercise queries against compatible old data during rebuilding, saves arriving during rebuild/cutover, old pinned cursors, interruption/restart, and failed replacement creation. Measure aggregate CPU/memory, temporary disk amplification, rebuild progress across many workspaces, and time to safe reclamation. Verify failures preserve the prior usable index, served freshness/coverage stays honest, and retired source/index data is not deleted beneath readers. Compatibility ranges and exact cutover mechanics are not yet implemented guarantees.

Standalone TypeScript files without a tsconfig.json are analyzed with explicitly inferred settings. Include inferred-project files/imports, unresolved packages, and a later explicit configuration with different bindings. Verify inferred provenance/coverage, compiler-backed facts, affected reanalysis, and absence of synthetic configuration writes into the source repository. A broken explicit tsconfig must remain visible instead of silently becoming inferred analysis. Measure project grouping and compiler-state costs without one unnecessary resident compiler program per file.

Default indexing includes test projects/code. Include mixed production/test projects in TS/C# evaluation, report their contribution to index size and analysis cost, and verify include/exclude filters behave consistently across both query surfaces. Keep unknown classification visible; static test-impact paths must not be presented as measured execution coverage.

C# analysis covers every declared target framework by default. Include multi-target fixtures with conditional declarations/calls, different dependency bindings, generated documents, and a target that fails to load. Measure aggregate analysis time, peak process-tree memory, shared source storage, and incremental invalidation across targets. Verify coverage identifies missing targets and one target's facts do not overwrite another's. Bound concurrency and worker residency without reducing the default target scope; all build-configuration/runtime-identifier combinations are not implied.

Use one configurable C# build configuration per workspace across those targets. Include a configuration switch with conditional symbols/generated output, queries during reanalysis, and old pinned cursors. Verify configuration scope is visible and old-configuration facts cannot appear current under the new choice. Measure switch cost without multiplying routine analysis by every available configuration; the default configuration and runtime-identifier handling remain open.

Cross-target query results combine equivalent findings with supporting-target provenance, preserve differences, and support target filters. Verify consistent behavior across focused/structured entry points, bounded pagination, and target-specific results. Include a path whose edges exist in mutually incompatible target contexts and ensure it is not reported as a supported end-to-end path. Measure grouping/provenance overhead without materializing an unbounded duplicate graph.

Pagination is generation-pinned with expiring cursors. Test updates between pages, stable ordering/scope, known stale status, explicit expiry/invalidation errors, process exit, and cleanup under active reads. Measure retained views, file handles, source bytes, and WAL/checkpoint progress with slow or abandoned consumers. An expired cursor must not silently return data from a new generation.

v0.0.1 may create new symbol IDs after rename/move; refactoring continuity is required in v0.0.2. Establish deterministic unchanged-input identity checks now. Continuity evaluation must measure matching precision/coverage and update cost, including ambiguous similar symbols, overloads, scope/project changes, and generated/local symbols, without counting forced merges as success. Do not let continuity matching trigger an unbounded whole-estate rescan or require a retained historical graph archive.

Generated project source is required in v0.0.1. Include on-disk generated clients, compiler-only generated documents, handwritten/generated partial declarations, mappings, generated call targets, changed generator inputs, disappearing outputs, and unavailable generation in fixtures. Measure generation cost and derived source retention separately. Verify source evidence is retrievable after worker teardown, and generated output events do not create repeated self-triggered indexing. This does not add full third-party dependency-source indexing or Angular template analysis to v0.0.1.

Support compiler-integrated generation without automatically running full builds or external codegen commands to repair missing artifacts. Verify missing outputs yield actionable coverage/partial results, analysis does not repeatedly attempt a full build, and dependency restoration remains distinct from source-generation readiness.

Third-party dependency source is not fully indexed by default. Verify compiler-bound external references and available signatures/docs, distinct package/assembly/version provenance, explicit missing-source markers, and boundary-aware traversal/impact. Measure external metadata enrichment and guard against recursive expansion across entire framework type graphs. Source-body coverage statistics must identify this deliberate boundary rather than imply whole-package analysis.

Local variables and parameters are required queryable symbols. Include shadowing, nested scopes, captures, destructuring/pattern bindings where supported, local functions/lambdas, and parameter references in TS/C# fixtures. Measure their actual fact/index overhead and fine-grained lookup latency; don't publish size/performance figures for an artificially reduced symbol set. Verify default graph presentation filters do not remove indexed facts or misrepresent the applied query scope.

The hybrid query interface is accepted. Compare equivalent focused and structured queries for results, evidence, freshness, budgets, and truncation. Measure mixed filter/traversal workloads and verify the generic surface reuses the common engine rather than materializing an unbounded whole-workspace graph. This decision does not add embedding search or cross-repository queries to v0.0.1.

Callers/callees and dependency impact are required in v0.0.1. Evaluate both forward and reverse relationships, source-evidence validity, impact-path correctness, and incremental maintenance after target deletion/rename or public-signature changes. Include constructors, overloads, aliases, interface/virtual dispatch, extension methods, generics, callbacks, and unresolved/dynamic callsites in named TS/C# fixtures. Publish supported subsets and unresolved denominators rather than hiding omissions behind aggregate navigation accuracy. Bounded traversal latency must be measured on compiler-backed relationship data, not a graph of arbitrary textual references.

Impact defaults to possible dispatch candidates included, with an exact-target-only filter. Evaluate default and exact-only results separately, including candidate precision/coverage, high-fanout interface implementations, mixed exact/possible paths, and truncation under budgets. Verify focused/structured entry points apply identical filtering and preserve uncertainty; do not count a possible implementation as a proven runtime call.

Correctness invariants are specified in [design](DESIGN.md). Proposed initial quality targets for named Tier 1 languages and labeled fixtures:

| Capability | Precision | Recall |
| --- | --- | --- |
| Definitions and source spans | >=99% | >=98% |
| Cross-file references in precise mode | >=99% | >=95% |
| Statically resolvable calls in structural mode | >=95% | >=85% |

Define ground truth, supported constructs, and denominators before making claims. Report unresolved/dynamic sites separately, with their share of all callsites; do not hide unsupported cases by shrinking the evaluation denominator. Indexer-backed comparisons measure agreement with that indexer, not universal semantic truth.

## Agent usefulness

Graph responses default to compact symbols/signatures, relationships, and source locations, with optional bounded source-body inclusion. Measure compact and source-inclusive payload bytes, serialization/copy/allocation costs, end-to-end agent tokens/tool calls, and final-task quality. Verify source inclusion uses the served generation and output limits expose truncation while preserving required evidence/coverage. Avoid reporting smaller graph responses as a win if extra source fetches or missing context reduce task quality; exact budget units/defaults remain open.

Initial aspiration: >=30% median reduction in exploration tokens and >=25% fewer tool calls, with final-task quality within a predefined 2 percentage point non-inferiority margin against the same agent using file tools. Later aspiration: >=50% token reduction at maintained quality.

Report task success, answer completeness, time, and token costs together. Adequate sample sizes and uncertainty estimates are necessary before claiming non-inferiority. Preserve source access when graph context is insufficient.

The Codebase-Memory preprint reports 83% answer quality versus 92% for file exploration despite using ten times fewer tokens. This is motivation for quality-gated evaluation, not a target to copy: https://arxiv.org/abs/2603.27277

## Measurement protocol

1. Pin repository commits, product builds, parser/indexer versions, toolchains, and configuration.
2. Separate cold indexing, warm-filesystem indexing, warm queries, and startup with existing indexes.
3. Time indexing through persisted, queryable publication, not just parsing. Report time to first partial result separately.
4. Exercise add/edit/delete/rename, no-op scans, signature/dependency changes, branch/worktree switches, offline edits, interrupted refreshes, and restart recovery.
5. Measure queries during indexing and concurrent clients, including a small multi-agent workload.
6. Use sufficient query samples for tails, for example >=1,000 requests for p95; publish sample counts. For a few cold-index runs, report median/range rather than unsupported p99 claims.
7. Agent experiments fix model/prompt/tool budgets, isolate sessions, randomize ordering, and use blinded grading where possible. Account for tool schemas, cached tokens, failures, and source fallbacks.
8. Publish raw measurements and coverage gaps. Keep the harness centered on real invariants and user tasks rather than building a second validation framework.

## Coordinator slice evidence

The saved-state implementation reuses unchanged compilation-owned facts via generation manifests; it does not copy the whole graph per save. Its input manifests stay in a separate registry table and load for active work rather than every idle repository. Native queries share control/freshness semantics while executing SQLite in-process. The integration suite exercises real FSEvents, dependent C# target invalidation, stale fallback, failure retention, external inputs, root identity, and restart/no-op behavior. These are correctness checks, not resource-budget or estate-throughput evidence. Project reanalysis still starts fresh compiler processes, and active reconciliation scans workspace inputs; quantify those costs before claiming latency/footprint targets. See [coordinator details](COORDINATOR.md).

## Query measurements after dispatch linking

[Query latency and memory evidence](QUERY_PERFORMANCE.md) covers ten real TS/Polly requests across the three Release in-process APIs, separate repeated-call memory measurements, and managed live freshness probes. It exposes planner/temporary-allocation costs that structural query-plan checks did not catch. The subsequent shared-core implementation is validated with paired controls and actual native API measurements; a small combined-search tradeoff remains explicit. The measurements do not establish an SLO, hardware floor or estate-scale acceptance. See the report for exact budgets, returned/truncated work, warm-cache limits, immutable artifacts and the next validation gates.

The [symbol/text lookup implementation](QUERY_PERFORMANCE.md#symbol-and-text-lookup-follow-on) adds plan gates for all filter-presence combinations and measures VM work plus paired native API latency on real snapshots. Measurements distinguish selective lookup from large indexed scans, preserve case-dependent combined-filter costs, and retain compiler-setup timeout failures. Full compiler-suite validation in the supported contended-workstation scenario remains outstanding.

The [first managed lookup-gate run](QUERY_PERFORMANCE.md#accepted-current-index-lookup-gate-first-run) is an observed failure: 22/24 cells miss 100 ms, despite current/correct results. It adds reference-query coverage and separates freshness-RPC time from actual read-view/query work. Bounded connection reuse is a proposed experiment, with a coordinator freshness check on every query; no cached-current fast path or production policy change has been accepted or implemented.
