# Know Code design

Status: agreed requirements baseline with a first working end-to-end slice. Architecture/release scope remains under development; see [actual implementation and limits](IMPLEMENTATION.md). Requirements below are not a claim that v0.0.1 is complete.

## Purpose

Give AI agents and other tools a fast, inspectable understanding of the code they are working on: what exists, how it connects, why an answer is supported, and whether that answer reflects the requested checkout.

## Confirmed user constraints

- Know Code is a new OSS product.
- Know Code uses Apache-2.0; see [LICENSE](../LICENSE). Reference repositories remain under their own upstream licenses.
- Operate locally after dependency restoration, with no hosted-service requirement. Widely published builds must contain zero telemetry. Debug builds and the user's personal install may capture unrestricted local diagnostic payloads, including full queries, results, source content, and relevant internal state, so future agents can inspect usage and investigate bugs. There is no metadata-only content restriction. Automatically rotate local diagnostics under a generous configurable disk budget; the initial design default is 10 GiB across workspaces, including payload bundles. See [diagnostics](DIAGNOSTICS.md).
- The common core is implemented in Rust, with thin .NET bindings using a C-compatible ABI/PInvoke and a native Node-API add-on for Node.js sharing that implementation.
- Begin with one Rust core crate using ordinary modules for facts/identity, storage, updates, and queries, plus thin host/native-binding targets. Keep CLI/MCP-only dependencies outside the embedded core's required dependency surface. Split further only when concrete dependency or reuse needs justify it; exact package/target names and build layout remain open.
- v0.0.1 analyzes every declared C# target framework by default. Preserve target-specific compilation context and report unavailable/failed targets as coverage gaps. Share identical source content and schedule bounded analysis work; do not reduce coverage to one target as an implicit performance shortcut.
- Use one selected C# build context per workspace across all target frameworks. Default to repository/MSBuild evaluation unless explicitly overridden, preserving solution-to-project mappings even when effective project configuration labels differ. Report the selected context and effective configurations as analysis scope; changes trigger affected reanalysis through the common update path. If several solution files are present, require one workspace solution selection for mappings; projects outside it still use their own evaluated defaults. Do not expand every solution context, impose a Know Code-wide Debug default or automatically index every configuration.
- Query results combine equivalent findings across target frameworks, identify the supporting targets, preserve target-specific differences, and support target filtering. Result grouping must preserve semantic identities and compatible compilation context along returned paths.
- Graph queries default to compact symbols, signatures, relationships, and source locations. Source bodies may be requested with bounded inclusion, using the served generation's exact source. Compact projection retains freshness, coverage, uncertainty, and provenance; source retrieval remains explicitly available.
- Include test projects/code in the default index, with query filters to include or exclude test code. This supports static test-impact exploration; it does not establish measured runtime test coverage or require running tests.
- Background indexing prioritizes repositories with active queries or recent file saves, while other queued repositories continue making progress within shared resource limits. Prioritization changes scheduling, not semantic coverage or the accepted freshness/stale-result policy.
- Manage resources automatically by default, with optional per-user CPU and memory caps. Shared budgets govern analysis concurrency, worker residency, and caches across registered repositories/providers. No manual tuning is required for normal use; numeric defaults and cap enforcement details remain to be designed and measured.
- Support optional checked-in workspace configuration with centrally stored personal overrides. Share project settings across collaborators while keeping machine-specific CPU/memory caps personal. Use one settings-resolution path across consumers; the file name, format, and exact schema remain open.
- If a project cannot be analyzed within a configured cap, stop its analysis and report resource-limited. Preserve any prior results as explicitly stale; no prior usable index remains not-ready. Do not silently exceed the configured cap or treat interrupted analysis as a completed partial graph.
- Upgrades requiring index rebuilds construct a replacement alongside the existing workspace index and switch only when ready. Compatible old data remains queryable with explicit freshness/coverage while rebuilding. Rebuild work uses shared resource limits and temporarily needs additional disk; cutover and reclamation details remain design work.
- For v0.0.x, compatible library/coordinator versions may coexist. Incompatible pairs return an actionable update-required error; broad backward compatibility is not a release requirement while APIs settle. Exact supported compatibility ranges remain to be defined.
- SQLite is the initial durable fact/text-index store, with bounded graph traversal in Rust, subject to performance gates. Use one index database per repository/worktree plus a small central registry, stored centrally per user outside source repositories. Detailed schema, publication, and cross-repository link layout remain open.
- The coordinator is the sole index writer per workspace. Compiler analysis runs outside write transactions; completed fact/source/coverage changes publish atomically while readers retain a coherent served generation. SQLite WAL mode is accepted, with bounded reader lifetimes and managed checkpoints. Exact staging, fencing, checkpoint/durability settings, and crash-recovery mechanisms remain under design.
- Rebuildable indexes may lose recent indexing progress after a power failure, provided startup recovery reconciles with saved source and freshness remains honest. Registrations and settings use stronger durability. Coherent facts/source/coverage are still required; exact durability settings and recovery mechanics remain open.
- Rebuild corrupted derived indexes automatically, preserving registrations/settings and reporting recovery progress/failures. Reuse the shared bounded rebuild mechanism. Data known to be corrupt is unavailable, not an eligible stale fallback; unaffected healthy workspaces remain usable.
- Embedding, local CLI/MCP, and shared service use cases must all use a common core.
- The product will encompass multiple programming languages.
- All language providers use five shared logical fact families: source artifacts, symbols, occurrences, relationships, and analysis reports. Semantic symbols are distinct from the places they are declared or used. Exact record fields, SQLite layout, and wire encoding remain design work; see [fact model](FACT_MODEL.md).
- Use UTF-8 byte ranges as the internal source coordinate system, tied to the exact indexed source version. Convert compiler-native coordinates at provider boundaries and line/column representations at API boundaries. Stale results must retrieve their matching indexed text, not apply old ranges to current disk contents.
- Initially store retained source text in the workspace SQLite database, sharing identical content across targets and generations while keeping artifact identities/provenance distinct. Publish source references coherently with facts and reclaim content only when serving/readers/recovery no longer need it. External blob storage waits for a measured need; this does not add a historical source archive.
- v0.0.1 must support TypeScript and C#; future languages must reuse the same core and adapter contracts.
- v0.0.1 supports macOS on Apple Silicon only (ARM64). Intel Macs and Windows/Linux are outside this release; later platform timing remains open. The user selected macOS 15+, Node 24 and a separately installed .NET 10 SDK for the first ARM64 candidate; the user accepts macOS 15 verification only if a viable VM can be obtained and run freely, because no separate macOS 15 machine is available. Distinguish the deployment target from actually tested OS versions; see [VM feasibility](VM_VALIDATION.md).
- v0.0.1 requires compiler-resolved symbols and references using TypeScript's compiler and Roslyn. Syntax-only extraction does not satisfy the release requirement. Report unresolved/incomplete analysis explicitly rather than silently substituting heuristics.
- Standalone TypeScript files without a tsconfig.json use an inferred compiler configuration in v0.0.1. Clearly expose inferred settings and coverage limits; keep synthetic configuration out of the repository. Compiler-backed resolution remains required, and broken explicit configuration is not silently replaced by inference.
- Include local variables and parameters as queryable symbols. Default graph overviews focus on types, members, and callables, with explicit filters/access for finer-grained symbols; presentation defaults do not reduce indexed semantic coverage.
- Include generated project source in v0.0.1, including source-generator output and available generated API clients. Label generated facts/source and preserve origin/mappings. Support compiler-integrated generation; report missing external codegen/build outputs rather than automatically running full builds or external generation commands.
- Publish valid partial graphs for saved code with compiler errors, with explicit compiler diagnostics, unresolved coverage, and incomplete regions. Do not require successful compilation before updating usable facts.
- Automatically restore missing project dependencies needed for analysis. For Node projects, 0.0.1 requires npm and pnpm restoration; Yarn restoration is backlog work without an assigned release. Honor repository-selected tooling and workspace/lockfile inputs. Automatically fetch missing repository-pinned npm/pnpm versions into a user-scoped cache, deduplicate downloads and reuse cached tools offline after setup, with visible effective versions/progress/failures. This package-tool setup does not install Node.js or the .NET SDK. npm/pnpm install scripts are disabled by default; enabling them requires an explicit personal opt-in per workspace, not a checked-in setting alone. Script-policy changes invalidate the corresponding preparation checkpoint. Expose progress and failures, deduplicate restores across consumers/projects where applicable, and avoid rerunning restores on ordinary saves with unchanged dependency inputs. Node.js and the .NET SDK must be installed separately; check for them and provide actionable missing/incompatible-tool errors. Know Code does not install these prerequisites.
- Honor each repository's TypeScript version and .NET SDK selection, with explicit missing/unsupported-version errors. Do not silently substitute a different compiler or SDK outside the repository's selection policy. Preserve effective toolchain provenance; supported ranges, unpinned defaults, and worker-version isolation remain design work.
- Third-party dependencies are represented by external symbol identities, signatures, and documentation where available by default. Compilers still consume restored dependencies for resolution. Full third-party source indexing is optional future scope without an assigned release.
- v0.0.1 must include callers, callees, and dependency-impact queries. Navigation alone does not meet the intended useful product scope; relationship results must carry source evidence and explicit resolution limits.
- Embedding-based semantic code search may wait beyond v0.0.1; no later release has been assigned. For 0.0.1, symbol/signature search and the compiler-backed graph are sufficient, without a required embedding model or vector index. The user explicitly put source-body string/comment search on the backlog without an assigned release; source retrieval and bounded context/body inclusion remain separate requirements.
- SQLite FTS5 is the initial lexical-search index in each workspace database, subject to code-search quality and performance checks. Shared Rust operators preserve query scope, served-generation evidence, and budgets. Exact tokenization/ranking and FTS layout remain open; textual matches do not establish semantic relationships.
- Bundle a pinned SQLite build with FTS5 across the coordinator and native library artifacts for consistent features/storage behavior. The macOS system SQLite is not the selected dependency source. Exact version, Rust binding crate, build flags, and static/shared linkage remain open; bundled native artifacts incur additional binary size.
- Angular template-to-component references belong in v0.0.2. v0.0.3 must add Swift and Rust source-language support through the common core and analysis contract.
- v0.0.1 requires in-process libraries for both .NET and Node.js. A client of a separate local service alone does not satisfy embedding; both runtime bindings must use the common core.
- v0.0.1 MCP runs as an agent-launched stdio process using the same core/indexes and persistent indexing coordinator. MCP HTTP transport is deferred without an assigned release. MCP process shutdown does not stop background indexing.
- v0.0.1 must support multiple local processes using the same repository index concurrently, including .NET and Node/MCP consumers. Coordination must preserve the in-process query requirement.
- The persistent local indexing coordinator starts automatically at user login and continues after all consumers disconnect. It reuses the Rust update engine and language workers; its lifetime is independent of consumer connections. The accepted user-scoped runtime layout uses ~/.local/share/know-code with a stable ~/.local/bin/know-code launcher, separate from retained central indexes/settings. Updates pause new jobs and wait up to 30 seconds for active analysis; a busy timeout resumes indexing without switching. Uninstall removes runtime/launcher/managed login job while retaining product data. See [installation behavior and remaining limits](INSTALLATION.md). Startup before login is not required by this decision.
- Support both explicit repository registration and parent-folder repository discovery. Newly discovered repositories beneath a registered parent automatically register and begin indexing, subject to discovery exclusions and shared resource limits. Both entry points feed one durable registry and common analysis/update path; overlapping registrations must not duplicate work. Exact discovery traversal/exclusions/timing remain under design.
- Parent-folder discovery skips directory symlinks by default; explicitly registering a linked repository remains supported. Resolve aliases through shared workspace identity to avoid duplicate indexing. This limits repository discovery traversal, not compiler-resolved linked source membership.
- If a registered repository disappears, preserve its index/settings until explicitly removed, report it unavailable, and automatically resume when the same workspace returns. Reconcile saved inputs before declaring freshness; missing roots must not be treated as deletion of every indexed source file.
- Start with current-saved-state queries only. Preserve a clear snapshot identity and fact ownership model so historical revision queries can be added later; they are not initially implemented or retained and have no assigned release.
- Renames/moves may produce new symbol IDs in v0.0.1. Refactoring continuity is required in v0.0.2. Keep identity continuity separate from historical graph queries and code editing.
- Queries wait up to 500 ms by default for fresh analysis, configurable per query and bounded by the caller deadline, then fall back to the last coherent index with an explicit stale flag. This budget covers freshness waiting, not total execution.
- All pages of a paginated result use one index generation. Cursors expire to bound retention; expired/invalidated cursors require explicit restart rather than silently switching views.
- Storage, queries, and graph traversal run in-process. Compiler-backed language analysis may run in workers through one shared contract; workers do not own a separate storage or query engine.
- Worker communication uses versioned Protobuf in bounded length-prefixed batches over local stdin/stdout. Exact schema/codecs and limits remain implementation work. The public embedded query API remains in-process.
- Consumer/coordinator control uses a local per-user Unix domain stream socket with shared versioned Protobuf framing/codec infrastructure. Control handles registration/jobs/refresh/freshness; graph execution stays in-process.
- It cannot become a bloated sprawl of code or validations.
- Exceptional speed and low resource use are explicit product priorities. The user's 64/128 GB workstations are evaluation environments, not required compute; Know Code should not rely on that level of hardware for baseline operation.
- Heavy concurrent video encoding is a normal supported workstation workload, confirmed by the user. Evaluate query responsiveness and ongoing indexing/freshness while the workstation is busy, along with Know Code's effect on the user's other work. Normal use must not require an idle/exclusive machine. Under contention, prioritize interactive query responsiveness over indexing freshness, while background indexing continues making progress within shared resource limits. Preserve fairness across queued workspaces and expose lag through accurate freshness metadata. Scheduling/enforcement mechanics and numeric guarantees remain to be selected and measured.
- The accepted first contention performance gate is p95 <=100 ms for warm, focused symbol/reference lookups returning <=100 results against an already-current index during encoding, initially evaluated on named TS/Polly fixtures through the native APIs. Preserve result semantics, source/coverage evidence and freshness. This gate does not change runtime timeouts or establish a guarantee for arbitrary queries/hardware; cold starts, pending-save waiting, indexing and broader workloads require separate measurements.
- Keep the graph as current as possible through continuous incremental updates after file saves. Saved on-disk state is the permanent freshness boundary, including saved uncommitted changes. Unsaved editor buffers are explicitly out of scope. Freshness and resource efficiency are simultaneous goals, not permission for perpetual full reindexing or unlimited compiler residency.
- Architecture and product exploration must proceed through focused Q&A, rather than large conversational documents requiring an open-ended response.
- Research clones and retained artifacts must live in durable project locations, not temporary or reboot-cleaned directories.

These are confirmed constraints. Other implementation techniques, provider details, storage choices, and numeric targets below remain proposals unless explicitly identified as accepted.

## Product direction

A graph plus MCP is already common. The proposed differentiation is a coherent combination of current-checkout correctness, explicit coverage and uncertainty, compact evidence-bearing answers, predictable operation, and an open reusable engine.

This is an inference from research, not evidence that competitors universally lack these capabilities. See [research](RESEARCH.md).

## Core jobs

1. Discover symbols and read exact definitions or source ranges.
2. Find references, callers/callees, imports, inheritance, and dependencies.
3. Trace bounded paths and explain potential change impact.
4. Assemble relevant source context within a caller-specified budget.
5. Identify the repository, snapshot, freshness, and analysis limits behind an answer.
6. Reuse the same capabilities from a library, local tools, and a shared service.

## Intended work-scale deployment

The user identified a work environment with hundreds of repositories, a C# service architecture, and an Angular frontend. Storage choice and scaling design must account for this workload now. Repository count alone does not establish graph size, update throughput, or query cost.

Whole-estate cross-repository queries and Angular template-to-component references target v0.0.2 and are not required in v0.0.1. The primary deployment is a local Apple M5 Max workstation: 64 GB unified memory here, 128 GB at work. Know Code's memory/storage budgets, concurrency/freshness targets, detailed template coverage, and service-boundary coverage still need clarification. SQLite is selected initially, subject to performance gates. See [scaling and upgrade discussion](SCALING.md).

## Release boundaries

The [release-readiness ledger](RELEASE_PLAN.md) records implemented evidence, remaining acceptance gaps and a proposed path to publishing 0.0.1, then 0.0.2. It does not waive or move the commitments below.

- **v0.0.1:** macOS/Apple-Silicon-only Rust common core; compiler-resolved TS+C# symbols/references using the TypeScript compiler and Roslyn; callers, callees, and dependency-impact queries; in-process .NET/Node.js bindings; agent-launched stdio MCP. Compiler workers are permitted; whole-estate query coordination and MCP HTTP transport are outside this release. Other detailed feature acceptance scope remains to be defined.
- **v0.0.2:** whole-estate cross-repository queries, Angular template-to-component references, and refactoring continuity across renames/moves, with a local Apple M5 Max workstation as the primary estate-scale use case. This does not by itself commit to a specific repository count/latency guarantee or complete runtime service-call resolution. Shared-service support remains an overall product goal, not the primary v0.0.2 deployment.
- **v0.0.3:** add Swift and Rust source analysis through the existing core/provider and query contracts. Swift project support proceeds **SwiftPM first, then Xcode**. Swift defaults to a compatible installed Xcode while honoring repository pins and explicit overrides; CLT is the fallback when Xcode is absent. Selection, compiler/SDK identity and coverage gaps stay visible; no global toolchain change or compiler installation is automatic. Xcode iOS indexing covers both device and simulator code paths as distinct compilation contexts, preserving target provenance and sharing source payloads. Detailed language capability coverage remains open; experimental provider progress is documented separately from release acceptance. Rust source analysis is separate from the already-selected Rust core implementation.
- **Design now:** repository/package/compilation identities, generation metadata, and versioned artifacts must permit cross-repository composition without requiring separate engines later.
- **Continuity preparation:** v0.0.1 may assign new IDs across renames/moves; preserve the scoped identity/evidence boundaries needed to link these changes in v0.0.2. Do not group continuity with indefinitely deferred optional features.
- **Deferred without a release:** embedding-based semantic code search, searchable historical revisions, optional full third-party dependency-source indexing, and MCP HTTP transport. Preserve sensible extension boundaries without implementing their infrastructure now.

## Feature priorities

P0 is the initial coherent product scope, not a promise to implement every item before any usable milestone. P1 follows core correctness; P2 is optional expansion. Explicit release commitments above take precedence over these broad priority labels.

| Priority | Capability | Expected behavior |
| --- | --- | --- |
| P0 | Canonical facts | Repositories/packages, files, symbols, occurrences, and typed relationships; retain unresolved targets. |
| P0 | Multiple languages | Shared adapter contract; publish parsing, definition, reference, call, and type capabilities independently. |
| P0 | Evidence | Source ranges, snapshot identity, extraction/resolution method, ambiguity and incompleteness. |
| P0 | Checkout identity | Worktrees, revisions, and uncommitted changes are explicit and isolated. |
| P0 | Incremental updates | Add/edit/delete/rename, dependency invalidation, reconciliation, and coherent publication. |
| P0 | Navigation | Symbols, definitions, references, callers/callees, dependencies, and bounded paths. |
| P0 | Dependency impact | v0.0.1 reverse dependency/call traversal with supporting paths, bounded expansion, and visible unresolved coverage. |
| P0 | Retrieval | Exact/lexical search and graph expansion; ranked context with explicit output limits. |
| P0 | Common interfaces | Shared request/result semantics for library, CLI JSON, MCP, and server. |
| P0 | Operations | Progress, cancellation, resource limits, pagination, truncation, status, and coverage. |
| P0 | Ownership | Core indexing/navigation needs no hosted account or model service; versioned export/import. |
| P1 | Reusable baselines | CI-produced index artifacts and isolated local/worktree overlays. |
| P1 | Cross-repository context | Package/version-aware identities and explicit dependency boundaries. |
| P1 | Angular template references | v0.0.2 template-to-component navigation through the shared fact model. |
| P1 | Swift and Rust providers | v0.0.3 source analysis through the existing provider contract. |
| P1 | Change understanding | Changed symbols, affected paths, and candidate tests with supporting evidence. |
| P2 | Enrichment | Optional embeddings, framework links, docs, runtime observations, deeper program analysis. |

## Hybrid query API

Accepted: focused navigation/search/impact tools plus a bounded structured read-only graph query, both backed by the same Rust query operators. SQL/storage internals remain private; do not build a custom textual query language or duplicate execution engine. Exact schemas, names, and operator coverage remain open. See [architecture](ARCHITECTURE.md#hybrid-agent-query-surface).

## Meaning of graph facts

[FACT_MODEL.md](FACT_MODEL.md) proposes logical records and ownership/identity boundaries. It is a discussion draft, not an accepted physical schema or new language feature commitment.

The common model should distinguish definitions, occurrences, references, calls, imports, containment, inheritance, and implementations. A reference is not automatically a call; a possible static call target is not proof of runtime execution.

Queryable symbols include local variables and parameters as well as types/members/callables. Retain scope/containment and compiler-bound references so shadowed names and same-named locals remain distinct. Local functions and lambdas used by call analysis also need usable identities/evidence. Do not imply complete data-flow analysis merely by indexing locals or parameter references.

Third-party dependency bodies are outside default graph coverage; record external targets and available signatures/docs with package/assembly/module origin and version evidence. Missing source/documents are explicit, not fabricated source locations. A path reaching an external target marks an analysis boundary rather than proving execution or dependency impact ends there. Registered first-party repositories remain eligible for normal source indexing; their cross-repository links arrive in v0.0.2.

Generated project source is within v0.0.1 graph coverage and uses the same symbol/reference/call model with generated provenance. Compiler-derived documents produced from saved project inputs are distinct from unsaved editor buffers; they may exist only in a compiler workspace. Their source content and mappings must remain retrievable for the served generation without requiring a compiler process to stay resident. Indexing generated project code does not enable full third-party dependency-source indexing or Angular template analysis ahead of their accepted scope.

For v0.0.1, extract invocation context, enclosing callable, compiler-resolved target where available, and callsite evidence. Distinguish direct/static targets, interface/virtual dispatch possibilities, and unresolved dynamic behavior. Dependency-impact queries traverse the recorded graph and explain supporting paths; they do not promise every runtime effect. Diff-to-symbol mapping and validated test selection remain separate from this required core impact capability.

Callers/callees include compiler-implied calls such as property accessors, overloaded operators and conversions by default, labeled implicit and filterable. See [call-graph implementation plan](CALL_GRAPH.md); this accepted default is not a claim that extraction or queries are implemented.

Use explicit, labeled initializer nodes for calls made during file loading or type initialization. Keep initializer execution separate from later field/function-value use so traversal does not invent execution paths.

Impact results return one supporting path per affected symbol by default. Additional paths can be requested explicitly within the same shared query budgets. Prefer a compiler-bound path over a shorter path containing possible dispatch. A single returned path does not imply uniqueness or exhaustive enumeration; preserve its uncertainty and compatible compilation provenance. Respect search budgets and expose limited search rather than imply that an unfound preferred path does not exist. Compiler binding remains distinct from proof of runtime execution.

Accepted default: include possible interface/virtual dispatch targets in impact analysis, clearly labeled as possible, with an exact-target-only filter available. Preserve the resolution category of each edge and propagate uncertainty through returned paths. Exact-only excludes inferred candidate dispatch edges; it does not turn a compiler-bound virtual/interface declaration into proof of which implementation runs. Scope, unknown implementations, and truncated candidate expansion must remain visible.

Impact traversal includes compiler-backed calls, value references (including field/constant reads), type references, inheritance and imports by default. Label each dependency kind and allow kind filtering through the same Rust operators. Preserve compiler/source evidence and external/unresolved boundaries; lexical matches alone do not establish dependencies. Callers/callees retain their call-specific meaning.

Proposed evidence categories include syntactically observed, semantically resolved, heuristic, and externally supplied observations/claims. Exact names and schema are undecided. Do not substitute arbitrary numerical confidence for a documented resolution method.

Tree-sitter supplies syntax parsing; SCIP supplies code-navigation facts. Neither alone guarantees complete program semantics or call graphs. Deeper analyses can contribute facts through explicit contracts.

Snapshot/generation identity initially describes the current indexed saved checkout, including saved uncommitted changes; it is not synonymous with a Git commit. Future historical support must fit the same logical fact/query model. Retaining an old generation for an active reader or failed-update recovery does not make it a user-queryable revision archive.

## Correctness requirements

- Every result identifies its requested/served snapshot and relevant freshness state.
- Every source-bearing result has a valid location for its declared snapshot.
- Unsupported or unresolved analysis is distinguishable from a complete query with no matches.
- Incremental indexing and clean indexing produce equivalent normalized facts for the same inputs and configuration.
- Readers observe coherent index generations; a failed refresh does not destroy the prior usable generation.
- Compiler errors in user source are not automatically an indexing failure. Publish facts the compiler/provider can establish in a coherent partial generation, and distinguish incomplete coverage from staleness. Analyzer crashes, cancelled/failed publication, and storage failures preserve the prior usable generation.
- Concurrent local consumers must not corrupt updates, observe partially published generations, or trigger redundant full indexing merely by opening the same workspace. Define reader lifetime, writer ownership, crash recovery, and cancellation at the workspace boundary.
- A query interrupted by limits reports incompleteness and supports continuation where applicable.
- Stale or lower-fidelity results cannot silently masquerade as fresh semantic results.

When relevant saved changes are still being analyzed, wait briefly for a suitable coherent generation. If the freshness wait expires, query the last usable generation and mark the response stale, with served-generation and known-pending-state information. If no usable generation exists, report not-ready rather than treating an empty result as complete. The freshness wait must honor the caller's overall deadline/cancellation; timing out one reader must not cancel shared background analysis.

The freshness-wait budget defaults to 500 ms and is configurable per query, capped by the remaining request deadline. Exact request/result field names remain open. Freshness metadata describes known observed inputs; it must not claim every concurrent filesystem change has been detected instantly. Returned source excerpts and ranges must belong to the served generation, including during stale fallback.

A newly published partial graph can satisfy the observed-input freshness target while still carrying incomplete-analysis flags. Do not wait indefinitely for compiler diagnostics to disappear. Report incomplete regions/unresolved sites instead of retaining obsolete relationships as if they were current or treating unavailable analysis as a definitive empty result.

Continuous freshness is preferred over requiring explicit refreshes or waiting for Git events. File saves, project/configuration changes, and branch operations must feed a common incremental update path. Library callers must be able to submit updates without starting a process-wide watcher. The host may supply file watching, with reconciliation for missed/coalesced events. Source contents and configuration must correspond to the generation being published, including changes that arrive during analysis.

Unsaved editor-buffer contents are permanently outside the product's freshness scope. Do not add editor-buffer synchronization or unsaved-content overlays in anticipation of a later release. Disk-backed working-tree/baseline overlays may still be useful for saved uncommitted changes; they are distinct from unsaved editor overlays. No fixed debounce interval or freshness latency guarantee has been selected.

Static analysis remains incomplete for many dynamic constructs. Impact results must explain their scope and supporting paths rather than claim all possible runtime effects.

## Simplicity requirements

- One engine owns identities, snapshots, facts, updates, queries, and result semantics.
- Language adapters contribute analysis; they do not each implement persistence or query engines.
- Transport adapters translate the common API; they do not fork navigation, ranking, or validation logic.
- Prefer one typed contract and derive bindings/documentation where practical.
- Validate external requests, imported artifacts, and adapter boundaries; avoid repeated validation of trusted internal values.
- Use a small set of meaningful invariant tests plus language fixtures. Avoid redundant validators and tests that mirror implementation.
- New dependencies and extension points must earn their maintenance cost through a concrete requirement.
- Available hardware must not excuse unnecessary memory residency, compiler work, reparsing, copying, or background CPU use. Distinguish the Rust core from language workers while measuring the total user-visible footprint.
- Start with several languages exercising the same abstraction; broader support must not mean parser counts disguised as semantic capability.

## Shared service requirements

The shared deployment must preserve the same engine semantics. Authentication, authorization context, scheduling, and transport concerns may differ from local operation.

Authorization must constrain traversal and derived answers, including paths, counts, snippets, and caches. Read-only querying and index administration should be distinct capabilities. Details remain open architecture work.

## Deferred scope

Agent orchestration, code editing, broad SDLC connector suites, a large graph visualization product, mandatory LLM extraction, and a full taint/security engine are deferred. They are independent product surfaces rather than prerequisites for useful code context.

## Unresolved decisions

See [architecture discussion](ARCHITECTURE.md). Rust is selected for the common core, C ABI/PInvoke and Node-API for native host integration, SQLite for initial durable facts/text indexes, TypeScript's compiler/Roslyn for TS/C# semantic analysis, and Apache-2.0 as the project license. Detailed binding ownership/async/packaging, storage schema, extraction/call-graph strategy, optional SCIP interchange, future-language providers, plugin mechanism, and deployment packaging remain open.

See [performance](PERFORMANCE.md) for provisional numeric goals; they are hypotheses to validate after a prototype, not established guarantees.

## Accepted resource-control defaults (2026-11-09)

The user accepted automatic shared ceilings of two CPU cores of background work and 2 GiB of memory, adjustable per user. These are tuning defaults, not hardware requirements. The user accepted 20 ms sampled macOS memory enforcement with possible brief overshoot, and workspace-wide indexing cancellation that resumes on the next observed save or explicit refresh. Closing a client or canceling a query does not cancel shared indexing. See [implementation, commands and limits](RESOURCE_CONTROL.md).

Cancellation first reports `cancelling` while the shared scheduler records a durable saved-input baseline without starting compilers. It reports `cancelled` only after successful capture; saves absorbed into that baseline remain cancelled, while later changed inputs or explicit refresh resume indexing. Pending intent and recorded baselines survive restart. The user accepted a snapshot boundary for v0.0.1: an edit racing final verification may count as a later save even if the last status read was `cancelling`. Unchanged delayed events must stay cancelled. This is not an atomic filesystem cutoff; see [the detailed contract and evidence](RESOURCE_CONTROL.md).

## Accepted query deadline outcome (2026-11-09)

An expired overall query deadline returns typed `deadline_exceeded` and releases its reader. It does not return a partly assembled successful page or cancel background indexing. A graph traversal's separate work limits may still produce explicitly partial findings before the overall deadline. The accepted 500 ms freshness wait remains a phase budget within the overall deadline. The overall default is 5 seconds, configurable per request up to 120 seconds, including waiting inside Know Code, freshness checks, execution and response encoding. Cursor details remain under discussion; see [query lifecycle](QUERY_LIFECYCLE.md).

## Accepted pagination portability (2026-11-09)

Continuation tokens are opaque and bound to the workspace, usable across local clients and separate CLI invocations. Queries remain in-process through the common Rust core. Every page retains the original generation, scope and provenance; expiration/invalidation requires an explicit restart. The user accepted 2 minutes idle and a 10-minute absolute lifetime; page activity never extends the absolute deadline. Coordinator restart invalidates outstanding tokens with typed cursor_invalidated and requires a new query. Under cursor-retention disk pressure, invalidate least recently used idle cursors with an explicit restart error so indexing can continue; active page reads remain coherent. The user accepted a configurable 1 GiB per-user budget for extra cursor-retained index data across all workspaces, excluding current serving data. This is not RAM allocation or a hard SQLite/WAL file-size cap. The configurable starting count limit is 128 live cursors per user, shared across workspaces, independently of marginal retained bytes. Retrying a valid token returns the same page/generation and next logical position without consuming it; freshness metadata may change. At the count limit, evict least recently used idle cursors and return cursor_limit only if all are busy. Under retention pressure with all cursors busy, defer publication until a bounded page read finishes, then evict; preserve active reads and flag prior results stale. Node/edge/path work limits span the entire cursor; each page gets a response-size budget and request deadline. Paging/retries do not reset traversal work or erase partial-coverage evidence. A valid cursor also supports matching source and symbol-context reads from its pinned generation until expiry/invalidation. For v0.0.1 graph pagination, compute one bounded result in-process and retain it locally for repeatable pages; the first page waits for the bounded traversal. Retained result storage must also be bounded and reclaimed, sharing the same 1 GiB pool with cursor-retained index data. The in-process core streams bounded result chunks to coordinator-owned files, with capacity reserved before retaining bytes; the coordinator manages acceptance/lifetime/cleanup while graph computation and page reads remain in-process. The accepted API keeps `limit` as the total logical result cap and adds `page_size` for rows/items per page; hitting the total cap stays explicitly partial even on the terminal page. The shared coordinator/core implementation and focused evidence are documented in [CURSOR_INTEGRATION.md](CURSOR_INTEGRATION.md); those checks do not imply final release certification. See [query lifecycle and the retention finding](QUERY_LIFECYCLE.md#accepted-cursor-portability).

## Accepted complete-response budget (2026-11-09)

Each page's shared JSON response defaults to 256 KiB and is configurable up to 2 MiB, including metadata, diagnostics, signatures and source. MCP's additional protocol framing needs its own bound. This is a complete encoded-response limit, not a claim about total query memory. See [query lifecycle](QUERY_LIFECYCLE.md#accepted-complete-response-byte-budget).

## Accepted parent discovery boundary (2026-11-10)

Automatic parent-folder discovery stops descending when it finds a repository. Nested repositories and submodules can still be registered explicitly through the shared identity/registration path. This complements the accepted default of skipping directory symlinks during discovery; it does not change compiler-resolved linked-source analysis or delete existing registrations.
