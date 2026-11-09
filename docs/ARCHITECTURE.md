# Architecture exploration

Status: agreed architectural direction with an implemented first slice covering compiler extraction, staged/atomic SQLite publication, and in-process native queries. See [implementation and current limits](IMPLEMENTATION.md). Persistent saved-file coordination and project-scoped storage reuse now work; compiler-session incrementality, full graph operations, and release packaging remain unfinished; initial schema/binding choices are provisional.

## Fixed constraints

An embeddable product, a local CLI/MCP service, and a shared service must use a common core. Multiple languages must fit that core without duplicating engines or proliferating validations. See [design](DESIGN.md).

v0.0.1 must index TypeScript and C# and provide in-process libraries for both .NET and Node.js. Storage, queries, and graph traversal remain in-process. Compiler-backed analysis may use worker processes through one shared contract; a subprocess-only client for the core is insufficient.

## Candidate logical boundaries

```text
Language analysis adapters
          |
          v
Canonical facts and evidence
          |
          v
Snapshot/index store and incremental updates
          |
          v
Query and context engine
          |
          +------ Library
          +------ CLI / MCP
          +------ Shared service
```

These are logical responsibilities, not a requirement for separate packages, processes, plugins, services, or abstraction layers.

| Responsibility | Candidate ownership |
| --- | --- |
| Analysis | Adapters emit definitions, occurrences, relationships, and diagnostics. |
| Identity | Core owns repositories, packages/versions, files, snapshots, and symbol identities. |
| Updates | Core schedules analysis, tracks invalidation, persists facts, publishes generations. |
| Queries | Core owns search, navigation, traversal, context assembly, and result budgets. |
| Operations | Shared job state, cancellation, limits, progress, coverage, and diagnostics. |
| Integration | Thin entry points translate the common API and supply deployment context. |

## Initial Rust packaging

Accepted: keep facts/identity, storage, update scheduling/publication, and query operators/projections as ordinary modules in one Rust core crate. They share typed contracts and ownership rules; do not make each logical responsibility a crate, plugin interface, validator, or dependency-injection framework. The initial Cargo workspace implements one core crate plus a host target and C ABI/Node native targets; exact release packaging remains open.

Use thin host and native-binding targets where packaging/runtime integration requires them. Keep CLI/MCP-specific transport and host dependencies outside the embedded core's required dependency surface. Compiler providers remain language-specific workers under the accepted shared contract. Exact package names/count, native build layout, and module APIs remain open; split the core only when a concrete dependency or reuse constraint justifies it.

## Decisions to explore through Q&A

1. **Storage details:** SQLite index database per repository/worktree plus a small central registry is selected, with bounded Rust graph traversal. Define schema, indexing, generation publication, cross-repository linking, and cross-process readers within performance gates.
2. **Analysis worker contract:** versioned, bounded length-prefixed Protobuf batches over local stdin/stdout are accepted. Review [the worker contract](WORKER_PROTOCOL.md) for schema, codec, readiness, compatibility, cancellation, and lifecycle details still to define.
3. **Fact model:** five shared logical families and internal UTF-8 byte-range source coordinates are accepted. Source ranges refer to the exact indexed version, with coordinate conversion at provider/API boundaries. Review [detailed fact-model proposals](FACT_MODEL.md) for exact fields, endpoint/line-column conventions, identity keys, and physical representation. Renames/moves may change IDs in v0.0.1; refactoring continuity is required in v0.0.2, using additive old/new identity evidence.
4. **Snapshot model:** immutable baselines, working-tree overlays, content addressing, and publication consistency.
5. **Runtime bindings:** C ABI/PInvoke for .NET and a Node-API add-on are accepted, both using the Rust core. Review [runtime binding details](RUNTIME_BINDINGS.md); ownership/async/packaging need design.
6. **Extraction details:** TypeScript compiler and Roslyn are selected for v0.0.1 symbols/references, callers/callees, and dependency impact; Angular template references arrive in v0.0.2 and Swift/Rust providers in v0.0.3. Define invocation/dispatch extraction, project loading, additional provider choices, and whether SCIP import adds value without duplicating live analysis.
7. **Incremental pipeline:** review [the proposed update/publication mechanism](UPDATE_PIPELINE.md), including provider invalidation, staging, atomic publication, reader lifetime, and recovery.
8. **Public API:** focused tools plus bounded structured graph queries over shared Rust operators are accepted. Define exact schemas/operator coverage, context assembly, pagination, budgets, and cancellation.
9. **Shared service:** authorization during traversal, concurrency, isolation, resource control, and artifact distribution.
10. **Extension policy:** which differences justify adapters; when an internal module should become a public plugin contract.

## Hybrid agent query surface

Accepted: focused tools plus a bounded structured read-only graph-query operation backed by the same Rust operators. Exact schemas/tool names/operator coverage remain design work:

- Focused operations for common tasks: symbol lookup, source retrieval, references, callers, callees, impact, and symbol/text search. Exact tool names remain undecided.
- One read-only `query_graph` operation using a small structured request: select a starting symbol/set, filter by supported properties, traverse typed edges with a direction/depth limit, and project bounded results/paths.
- Both surfaces invoke the same Rust query operators. Focused tools are convenient parameterizations/compositions, not separate traversal engines or repeated semantic validators.
- Keep SQL, SQLite table names, and database row IDs private. A structured graph request is not arbitrary SQL or an embedded scripting interpreter, and it should not grow a custom textual language/parser.
- Use one typed request contract across .NET/Node.js, CLI, and MCP; derive transport schemas where practical. Validate external request shape once and enforce operation invariants/budgets in the common core.
- Apply workspace/generation scope, the accepted freshness policy, visited-node/edge limits, output limits, deadlines/cancellation, cycle handling, and explicit truncation to both entry points.
- v0.0.1 queries stay within the selected repository/worktree; cross-repository execution arrives in v0.0.2 using the same semantic model.
- Combine equivalent findings across target frameworks while retaining supporting-target provenance and target-specific differences; expose target filtering through the same operators. Traversals must preserve compatible compilation contexts, avoiding paths that exist only by mixing incompatible targets. See [compilation context](FACT_MODEL.md#context).
- Include test projects/code in the default index. Both query surfaces support including/excluding test code through shared filters with explicit classification evidence; exact detection/override rules remain open. Test-impact paths are static evidence, not measured execution coverage.
- Accepted graph-result default: compact symbols/signatures, relationships, and source locations. Source bodies are optional bounded inclusion through the common projection/context path, with explicit truncation/omission and source matching the served generation. Compact output must retain required freshness, coverage, resolution evidence, and target provenance. Dedicated source retrieval remains available; exact request fields and budget units/defaults remain open.

Example capability: within the Payments namespace/path scope, find call paths to a selected publish method up to three hops, excluding test paths, and return source-backed paths. The compiler-derived relationships, completeness, possible dispatch, and unresolved sites retain their original meaning through either query surface.

Start with select/filter/walk/project rather than a generalized relational/graph algebra. Add operators when concrete agent tasks justify them. Exact schema/operator set remains open. Pagination is accepted as generation-pinned with expiring cursors; exact cursor mechanism/expiry remain design work.

Continuation uses the original query scope, filters, evidence policy, budgets, and served generation. It must not restart the freshness wait against newer edits or silently advance generations. Report known staleness if publication advances while the cursor remains valid. Expiration, restart/invalidation, or lost process-local view must produce an explicit restart-required outcome. Cursor portability across processes and lifetime limits are not promised before mechanism selection.

## MCP transport

Accepted v0.0.1 scope: an agent launches a stdio MCP process. MCP HTTP transport is deferred without an assigned release; no HTTP listener is required for the initial release. The entry point maps focused tools and structured graph queries into the same Rust operators, using the shared workspace registry/indexes and Rust coordinator-control client.

Queries execute in the MCP consumer process; indexing remains owned by the persistent coordinator. Several agents may launch consumers without creating duplicate compiler pools/watchers. Disconnecting or exiting an MCP consumer releases its query/cursor resources and does not stop background indexing. MCP's agent-facing transport is distinct from the accepted Protobuf stdio analysis-worker channel; sharing the word stdio does not make those wire protocols interchangeable. Exact MCP SDK/version, packaging, and tool schemas remain open.

## Coordinator control channel

The worker protocol is accepted as Protobuf over stdio. Consumer-to-coordinator control is a separate boundary: registration/discovery, enqueueing refreshes, setup/job status, and waiting for publication. Graph search/traversal still executes inside each consumer's Rust core against its SQLite read view.

Accepted local control transport: a per-user Unix domain stream socket, reusing versioned Protobuf framing/codec infrastructure for typed control messages. Keep worker-specific analysis/fact payloads separate from consumer control operations without adding a second generic RPC framework.

The binding layer uses the common Rust control client, not separate .NET/Node coordinator protocol implementations. Accepted v0.0.x policy: compatible library/coordinator versions may coexist; incompatible pairs return an actionable update-required error. Broad backward compatibility is not required while APIs settle. Exact compatibility ranges/handshake, endpoint discovery, local access permissions, restart/reconnect, bounded waits, and socket path constraints remain implementation work. Connection loss must not make a coherent cached graph silently appear fresh or cause a consumer to create a second indexing owner. See [early-version compatibility](SCALING.md#early-version-compatibility).

Use a local endpoint owned by the central application state; no temporary/reboot-cleaned location for retained artifacts. Socket lifecycle/discovery is distinct from persisted indexes/registry. Reference: [Rust UnixStream](https://doc.rust-lang.org/std/os/unix/net/struct.UnixStream.html).

## Concurrent local consumers

Accepted for v0.0.1: multiple processes may use the same repository index simultaneously. Each embedded consumer must retain in-process query execution through the Rust core; concurrent access does not imply routing all queries through a daemon.

Accepted lifecycle: background indexing continues after all consumers disconnect. Use a persistent local indexing coordinator whose lifetime is independent of .NET/Node/MCP consumer connections. It hosts the same Rust update engine and analysis-worker contract; embedded consumers query the index in-process through the same core.

Accepted ownership model: the coordinator is the sole workspace-index writer. Compiler analysis runs without holding a write transaction across it; completed changes publish atomically while consumers retain coherent generation-pinned readers. Consumers submit updates through the shared coordinator-control client; workers emit facts rather than writing the serving index. Do not let each consumer start its own watcher/compiler pool or promote itself to writer after a connection failure. Exact fencing/owner recovery, staging, reader mechanism, and startup installation remain open; see [publication pipeline](UPDATE_PIPELINE.md#4-publish-atomically).

Required design questions include duplicate watcher/worker suppression, generation pinning and reclamation, locking across processes, writer failure/handoff, and isolation between worktrees. A stale process must not publish a generation after ownership is lost. One consumer closing or cancelling its own request must not invalidate another consumer's active reads.

Accepted startup behavior: the coordinator starts automatically at user login and resumes registered workspace monitoring independently of consumer connections. It does not need to run before login. The development coordinator persists independently of consumers, and its reviewed per-user LaunchAgent has been explicitly activated at the user’s request. RunAtLoad/KeepAlive and a restart throttle are configured. Release installation/update packaging and broader failure policy remain under design; see [local installation](LOCAL_INSTALL.md) and [login setup](COORDINATOR.md#login-startup).

One coordinator per user is a proposed way to share workspace registrations, watcher ownership, and bounded analysis resources across repositories. The Unix socket/Protobuf control transport is accepted; process multiplicity, endpoint discovery, and shutdown/recovery details remain open. Idle monitoring must remain light and must not imply keeping every compiler project resident.

## Workspace registration and discovery

Accepted: users can register individual repositories or parent folders beneath which repositories are discovered. Both mechanisms feed one persistent registry; startup restores that registry without requiring each consumer to rediscover/re-register workspaces.

Accepted ongoing discovery: repositories newly discovered beneath a registered parent automatically register and begin indexing, subject to discovery exclusions and shared resource limits. This uses the same registry, setup/restore jobs, and scheduler as explicit registration; no consumer-specific crawler or parallel indexing owner is introduced. Discovery must resume after coordinator restarts, including repositories added while it was stopped. Event monitoring/reconciliation details and discovery latency remain open; ordinary graph queries must not trigger a full parent-tree scan.

Use the same identity, exclusion/discovery, and indexing implementation for both entry points. Overlapping roots and explicit registrations must not create duplicate watchers/compiler work or collapse distinct Git worktrees. Keep registration/discovery separate from whole-estate query coordination, which arrives in v0.0.2.

Accepted symlink discovery policy: skip directory symlinks when traversing a registered parent by default. Users can explicitly register a linked repository; resolve its target through the same workspace identity/deduplication path used for ordinary registration. A target reached through both a real directory and an explicit symlink alias must not create duplicate indexing owners. This is a repository-discovery boundary, not a ban on compiler-resolved linked source files or dependencies. Exact canonical/display path handling, alias persistence, and retarget detection remain design work.

Accepted missing-workspace behavior: retain the registration, index, and settings until explicit removal; expose unavailable status and automatically resume when the same repository/worktree returns. Missing/unmounted roots do not produce a publication deleting every source artifact. Stop futile analysis/restore work while unavailable and reconcile saved inputs after return before claiming freshness. Verify workspace identity before reusing a retained index at an occupied path; a different repository appearing at the same pathname must not inherit its graph. Exact identity checks, retry/reconciliation timing, and explicit removal controls remain design work.

Detailed traversal beyond the accepted directory-symlink default, submodule policy, exclusions, discovery timing, and explicit removal/parent-unregistration semantics remain open. Retention on disappearance is accepted. Avoid building a second crawler for each transport or registration mode.

## Central per-user state

Accepted: indexes and durable coordinator/workspace-registry state live centrally per user, outside source repositories. Individual consumers resolve the same registered index location; opening a .NET/Node/MCP consumer must not create a private duplicate index by default.

Use a durable application-data location on macOS, with `~/Library/Application Support/Know Code/` as the proposed default. Do not put retained indexes, registration state, or referenced artifacts in temporary or automatically evictable cache locations. Detailed subdirectory layout, path overrides, permissions, cleanup controls, and migration mechanics remain to be designed; no directory outside this repository is created during design work.

Accepted physical boundary: one index database per repository/worktree, plus a small central registry. All TS/C# projects and later language providers in the same workspace feed that workspace's index; do not create separate graph stores per language, consumer, or compiler project. Keep project/compilation identity inside the fact model and worktree identity explicit rather than deriving it solely from a filename or mutable absolute path.

The registry owns workspace lookup and registration metadata; it is not an estate-wide duplicate of every workspace graph. Cross-repository link/search catalog layout for v0.0.2 remains to be designed. Lazy opening, bounded connection/cache residency, coordinated publication per workspace, and worktree sharing optimizations require measurement. No initial history database per revision is implied.

Accepted durability policy: registrations and settings use stronger durability than rebuildable indexes. Recent index progress may be lost on power failure; startup must reconcile recovered state with saved source and expose stale/not-ready state until appropriate. Recovered facts/source remain coherent, and a durable registry entry cannot prove a weaker-durability index publication survived. Exact SQLite/file-sync settings and recovery mechanisms remain open; see [publication and recovery](UPDATE_PIPELINE.md#4-publish-atomically).

Accepted corruption policy: automatically rebuild a corrupted derived index, preserve registrations/settings, and report recovery progress/failures through shared status/jobs. Reuse the bounded coordinator rebuild path. Known-corrupt data cannot be a stale fallback; unaffected healthy workspaces remain available. Detection/isolation, damaged-artifact retention, and retry details remain open, and unsupported versions/access errors must not be mislabeled as corruption.

Accepted upgrade behavior: when a rebuild is necessary, construct a replacement beside the existing index and cut over only when ready, keeping compatible old data queryable with accurate freshness/coverage. Shared scheduling/resource limits also apply to rebuilds. Temporary extra disk use, reader-safe reclamation, and version compatibility require explicit handling; see [rebuild behavior](SCALING.md#accepted-rebuild-behavior). Exact cutover/writer handover mechanics remain open.

## Workspace configuration

Accepted: workspace settings may live in an optional checked-in configuration file, with personal overrides stored centrally per user. CPU/memory caps are personal settings; shared project configuration cannot raise them. A repository configuration file is not required for basic registration/indexing. Central index/registry storage remains unchanged.

Resolve defaults, checked-in workspace values, and personal overrides through one shared Rust path used by the coordinator and all consumers. Personal overrides take precedence for settings that permit them. Preserve the effective analysis settings and their origin for useful status/errors; differing consumers must not independently reinterpret the same workspace settings.

Analysis-affecting changes, including the selected C# build configuration, feed the existing update/invalidation path. Resource-limit changes affect admission/residency policy and do not inherently change semantic facts. Keep semantic-setting fingerprints distinct from operational tuning so a CPU-cap adjustment does not force a full reindex. File name, format/schema, override storage representation, and reload/error details remain open; no config file or new framework is created during architecture exploration.

Discuss one decision at a time. Record accepted decisions with their rationale here or in short decision records once there is an actual decision; do not create empty bureaucracy.

## Design cautions

- A common core does not require identical process topology in every deployment.
- Do not choose a graph database merely because the public model is a graph.
- Do not require a generalized plugin system before several concrete adapters expose the actual variation.
- Content caching alone is not semantic invalidation: unchanged files can require new resolution after dependency changes.
- Source locations and symbol identities have different stability requirements across edits and revisions.
- Keep factual navigation separable from optional model-assisted retrieval.
- Treat opaque CI artifacts as compatibility/versioning boundaries; their validation need not be duplicated throughout the engine.

## Local operation and diagnostic profiles

Accepted: runtime operation is local after project dependencies are restored. Core indexing/querying does not depend on a hosted service. Widely published builds have zero telemetry; Debug builds and the user's personal install may collect extensive local diagnostics for future agent-driven bug investigation.

The user permits unrestricted local diagnostic payloads, including full requests/results, source, worker inputs/outputs, and relevant internal state. Metadata-only restrictions are not required. Automatically rotate records and payload/reproduction bundles under a configurable per-user disk budget; initial design default is 10 GiB across workspaces. Retention implementation and capture triggers remain open; no automatic telemetry upload is part of the product.

Use one implementation with a small optional instrumentation boundary, not separate debug/public engines. Proposed enforcement: compile collection and sinks out of public distribution artifacts across the core, coordinator, bindings, and workers. A runtime flag alone must not turn a zero-telemetry public artifact into a collector. A personal diagnostic build may retain release optimizations; diagnostic availability and optimization are separate concerns. Exact build flags and packaging mechanics are undecided.

Ordinary explicit errors, requested status/progress, and persisted index/registry state remain product functions. Do not rename persistent usage collection as logging to evade the zero-telemetry requirement. There is no telemetry uploader or automatic remote export. See [diagnostics](DIAGNOSTICS.md) for the proposed local event/reproduction model and open questions.

## Continuous freshness

Accepted goal: keep the graph as current as possible after file saves without sacrificing the low-footprint requirement. Saved on-disk state, including uncommitted files, is the permanent boundary. Unsaved editor-buffer support is out of scope; exact timing remains open.

Accepted resource policy: automatic management with optional per-user CPU/memory caps. Shared scheduling and residency/cache budgets cover coordinator-managed repositories/providers; normal operation requires no tuning. A project that cannot fit a configured cap stops analysis with an explicit resource-limited outcome, retaining prior results as stale when available. Numeric defaults and cap accounting/enforcement remain open. Embedded consumers retain bounded core allocations, but their host-runtime heaps are outside coordinator control; aggregate multi-process accounting requires design. See [resource implications](UPDATE_PIPELINE.md#scope-and-resource-implications).

Proposed implementation direction:

- Host file events and explicit library updates feed the same workspace change queue and analysis/commit path.
- Coalesce redundant changes, retain updates arriving during analysis, and reconcile after missed events or branch switches. A watcher event is not proof that indexing completed.
- Prioritize active changes and affected dependencies; avoid repeated full-estate scans on the query path.
- Reuse compiler state where beneficial while bounding idle worker residency and analysis concurrency. Tune worker lifetime from measured update latency and memory, not a blanket keep-everything-warm policy.
- Prioritize indexing for actively queried or recently saved repositories, while other queued repositories continue progressing under shared limits. Use the [common update scheduler](UPDATE_PIPELINE.md#2-determine-and-analyze-affected-units); exact fairness/priority mechanics remain open.
- Bind each published result to coherent input/generation metadata. While new semantic analysis is pending, mark prior results stale or incomplete rather than relabeling them current.
- Analyze saved file contents and configuration. Do not add unsaved editor-buffer synchronization or unsaved-content overlay infrastructure. Keep any baseline/working-tree overlay grounded in saved on-disk content.

## Bounded freshness wait and stale fallback

Accepted policy: when analysis is catching up with saved changes, wait up to 500 ms by default and then return a coherent prior result explicitly marked stale. The freshness-wait budget is configurable per query and capped by the remaining request deadline. It is not a 500 ms total-query latency guarantee. Fresh requests should not incur an unconditional delay.

Capture an observed target/input generation for the request so continuous edits cannot extend the wait indefinitely. Pin the served generation for the query and preserve matching source/range evidence. Report whether the freshness target was met and any known newer pending inputs; don't equate current Git HEAD with analyzed saved state.

If no usable index exists, return not-ready after the wait rather than successful empty data. Request cancellation and freshness timeout must not abort analysis needed by other clients. Freshness waiting does not require routing graph execution through the coordinator: the local core can wait for publication and then query locally. Notification/control mechanics remain open.

## Current state and future historical revisions

Accepted: initial queries cover current saved state only. Searchable historical revisions are a future capability to accommodate, not a current indexing/retention requirement or a scheduled release.

Design direction: make workspace, compilation, and logical snapshot/generation identity explicit in fact ownership, query internals, and artifact metadata. Resolve current state through a current-generation selection rather than treating currentness as an intrinsic property of a symbol. Public symbol identity and snapshot-specific occurrence/declaration identity have different lifetimes.

This supports a later revision selector and retention layer without defining a second fact/query engine. It does not justify building a historical index manager now, duplicating every graph per save, or requiring every table/API to expose speculative historical details. Future schema and artifact migrations may still be needed; easy extension is a design goal, not a no-migration guarantee.

Initial retention is limited to current serving state plus bounded staging/read-pinned/recovery generations required by the chosen publication model. Reclaim obsolete data when readers and recovery no longer need it; exact storage/reclamation mechanics remain open. Historical queries must not be added implicitly by retaining these internal generations.

## Accepted architecture decisions

Confirmed for v0.0.1: common core across deployment modes, in-process .NET and Node.js libraries, and TypeScript plus C# indexing. The process boundary below is accepted; other implementation details remain proposals.

### In-process core with compiler-analysis workers

Accepted by the user: storage, queries, and graph traversal execute in the embedding process; compiler analysis may execute in workers. Both host-runtime bindings use the same core.

- Workers emit analysis through one shared contract. They do not contain independent graph stores, query engines, or transport-specific domain logic.
- Ordinary graph queries do not require a compiler round trip. Missing analysis is handled through explicit readiness/update behavior.
- TypeScript and Roslyn can use their natural toolchains without requiring both compiler runtimes to be embedded in every host.
- Tradeoff: worker startup, communication, packaging, cancellation, and lifecycle must be managed. Keeping workers warm and batching updates are candidate optimizations, not additional accepted requirements.
- This decision permits workers; it does not require an out-of-process path for every analysis provider or dictate the wire format.

## Compiler-backed symbols and references

Accepted for v0.0.1: TypeScript's compiler and Roslyn supply resolved symbols and references through the common analysis contract. Syntax-only analysis or heuristics cannot substitute for that release requirement. Compiler errors, missing dependencies, unresolved bindings, and partial project loading must be surfaced as coverage/readiness information.

Accepted standalone TypeScript behavior: when files have no tsconfig.json, analyze them with an inferred TypeScript compiler configuration. Expose the inferred origin, effective settings, and resulting coverage limits. This remains compiler-backed semantic analysis; inference describes project settings, not heuristic symbol resolution. Do not write synthetic configuration into the source repository. A broken explicit configuration remains a visible project-loading problem, not permission to silently substitute an inferred project. Inferred-project grouping/default options and transition to explicit configuration remain implementation design work.

Accepted C# scope: analyze every declared target framework by default using one configurable build configuration per workspace, retaining distinct compilation context for bindings, generated source, and diagnostics. Share identical source content and bound concurrent/resident compiler state instead of silently selecting one target. A target that cannot load/analyze must be reported as a coverage gap. Query results combine equivalent findings with target provenance, explicit differences, and target filters. Expose the active configuration in coverage; setting changes trigger affected reanalysis through the shared update path. The default configuration and runtime-identifier handling remain open. See [compilation context](FACT_MODEL.md#context).

Accepted granularity: local variables and parameters are first-class queryable symbols. Preserve enclosing scope, declarations/occurrences, source evidence, and binding identity to distinguish shadowed names. Graph overviews may default to types/members/callables but must expose filters for detailed symbols. Local functions/lambdas still participate in call extraction.

Keep local symbol identity scoped to its workspace/compilation and enclosing source/scope; do not promote same-named locals into global package identities or expose raw compiler AST objects to the core. Exact identity stability across edits remains design work. A common symbol-kind/scope model should accommodate all planned languages without implementing their binding rules in Rust.

Accepted identity boundary: renames/moves may create new IDs in v0.0.1. Refactoring continuity is required in v0.0.2. Proposed extension: explicit old/new symbol links plus change/evidence and generation context, preserving ambiguity rather than forcing identity reuse. This requires neither a historical query archive nor a code-editing subsystem. Detailed matcher and old-ID query behavior remain open.

Accepted publication policy: saved code with compiler errors may produce a valid partial graph, and that graph should be published with explicit diagnostics/coverage rather than blocked until successful compilation. Preserve facts the compiler/provider actually establishes; do not invent targets to fill gaps or silently carry outdated facts forward as current.

Keep analysis completeness distinct from publication coherence and freshness. A partial graph can be atomically published and fresh for its observed inputs. Known-unavailable regions and unresolved sites must survive into query evidence, including impact analysis where missing edges cannot mean no impact. Ordinary source diagnostics differ from worker crashes, cancelled publication, or storage failure; those failure paths preserve the previous usable generation.

Accepted setup policy: restore missing project dependencies automatically using the project's applicable package tooling (including Node package managers and NuGet/.NET restore). The coordinator schedules and deduplicates this preparation; language-specific command/project semantics belong to the provider/setup adapter, not separate transport implementations. Package-manager detection, exact commands, authentication/environment handling, and toolchain discovery/loading mechanics remain implementation work.

Track dependency/configuration inputs and restoration status so unchanged source saves do not repeatedly invoke package installation. A restore failure must be visible in readiness/coverage, preserve the previous usable index, and avoid a tight retry loop. Restore output/elapsed time belongs in job diagnostics; a query's short freshness timeout must not be confused with restore completion or cancel work shared by other consumers.

Restoration is explicit product setup behavior, not an implicit side effect of constructing the Rust library or importing a binding. Provider setup and analysis share the same coordinator/job lifecycle.

Accepted prerequisite policy: Node.js and the .NET SDK are installed separately by the user/environment. Know Code checks availability and compatibility and returns actionable errors; it does not install these toolchains. Specific supported versions remain implementation decisions.

Accepted toolchain-selection policy: honor the repository's TypeScript version and .NET SDK selection. Use the project's resolved TypeScript package after normal dependency restoration; missing or unsupported versions are explicit setup/coverage failures. Honor the repository's SDK selection policy, including its allowed resolution behavior, without silently choosing an unrelated installed SDK. Missing SDK installation remains the user's/environment's responsibility. Exact support ranges, discovery/loading mechanisms, and behavior for unpinned repositories remain open.

Record the requested selection, effective compiler/SDK identity, and relevant analysis inputs. Keep these separate from the runtime needed to host a Know Code worker; the selected SDK, Roslyn analysis components, and managed worker runtime are not interchangeable version labels. Worker compatibility/isolation must support differing repository toolchains within shared resource budgets. Do not claim support for every compiler version or approximate unsupported semantics silently. Changes in the effective analysis toolchain invalidate affected compilation facts through the common update path.

Prerequisite diagnostics should identify the affected provider/workspace, required tool/version when known, the detected state/path/version, and a concrete resolution. Distinguish a missing .NET SDK from a machine with only the runtime. Report discovery failures in the actual coordinator/login environment rather than assuming an interactive shell's setup is available. Surface the same structured failure through status/jobs, CLI/MCP, and bindings. Share one provider setup-check path; avoid redundant probes on every file or graph query.

Also accepted for v0.0.1: callers, callees, and dependency-impact queries. Workers must emit invocation/dependency facts with source evidence; Rust provides the shared forward/reverse traversal and impact semantics. Ordinary reference occurrences must not be relabeled CALLS. Use language-specific compiler/syntax/operation context for real extraction differences while sharing fact representation and query logic.

Evaluate TypeScript call/new-expression resolution and Roslyn operation-based extraction as implementation candidates. Preserve compiler-resolved callsite targets separately from inferred possible dispatch targets, and keep unresolved sites visible. Support bounded impact traversal with supporting paths. Complete dynamic runtime reachability is not implied by these static graph capabilities.

Accepted impact policy: include possible interface/virtual dispatch candidates by default, labeled as possible, and offer an exact-target-only filter through both focused and structured queries. Keep the bound declaration, implementation/override relationship, and possible dispatch edge distinct in the model. A path containing a possible edge must retain that evidence rather than be presented as wholly exact.

Candidate expansion uses known implementations in the analyzed scope and existing work/depth/output limits. Missing/external implementations and truncated candidates reduce completeness; they are not evidence of no impact. Exact-only filtering excludes inferred dispatch candidates but does not establish actual runtime execution. Provider-specific candidate derivation belongs in analysis; shared filtering/traversal/evidence semantics belong in Rust.

This decision does not establish a complete runtime call graph, mandate LSP servers, or require wrapping batch SCIP executables. Angular template-to-component references are separately committed for v0.0.2; Swift/Rust source analysis is committed for v0.0.3. Source findings favor deliberate project/compilation identity, caller-controlled toolchain/setup behavior, and reusable compiler state with bounded lifetime. The worker implementations should keep language semantics local while Rust owns common storage/query behavior.

The provider contract must accommodate TS, C#, Swift, and Rust without making the common core implement each compiler's semantic rules. Use these concrete planned languages to evaluate shared abstractions; do not prebuild a generalized plugin framework. Swift/Rust extraction backends and precise capability coverage remain undecided.

## Generated project source

Accepted for v0.0.1: index generated project source, including compiler/source-generator output and available generated API clients, with explicit generated provenance. Use the same language fact/query contracts as handwritten source. Compiler-integrated generation is supported; missing external build/codegen outputs produce actionable coverage gaps rather than automatically invoking full project builds or external generation scripts.

Normal project/compilation loading and compiler-integrated generator execution remain part of provider analysis; this is not a ban on necessary compiler/design-time project evaluation. Automatic dependency restoration remains accepted separately. Avoid turning the coordinator into a general build runner. A missing generated client should identify affected analysis and known remediation without silently keeping obsolete generated facts as current.

Providers must deliberately enumerate compiler-produced documents as well as ordinary project files; a filesystem-only scan or one document collection cannot be assumed complete. Preserve generated source identity, generator/origin when available, content fingerprint, compilation/input generation, and physical/mapped source ranges where applicable. Missing output or unavailable generation must be visible in coverage.

Compiler-generated documents may not have saved physical files. They are derived from saved inputs and do not require an unsaved-editor-buffer subsystem. Retain the source artifacts needed for query evidence in durable central workspace/index storage, with generation ownership and reclamation, rather than unstable temporary paths or permanent compiler residency. Storage format and deduplication details remain open.

Invalidation must account for generator/configuration inputs and changes to emitted documents. Generated output must not create a file-watch/reindex feedback loop. Measure generation separately from compiler extraction and avoid rerunning unchanged generators just to answer a query. Framework-specific template semantics remain v0.0.2 scope.

## Third-party dependency scope

Accepted default: represent external dependencies using symbol identities, signatures, documentation where available, and origin/version evidence. Full third-party dependency-source indexing is deferred, optional future functionality with no release assigned. This does not remove the need to restore/load compiler dependencies for correct local bindings.

Providers should emit the external symbols needed to explain indexed code, with bounded enrichment rather than recursively materializing whole framework/package type graphs. Preserve assembly/module identity and package/version provenance distinctly when they differ; do not guess a package identity from a display name. External definitions may lack source locations or bodies, and the model must represent that honestly.

Default traversals stop body expansion at these external boundaries and report coverage accordingly. Incoming references from indexed source to external targets remain useful for lookup/impact. External-boundary termination is not evidence that an external method calls nothing or cannot affect other code. Future optional source indexing can attach additional evidence under the same identity/provider model without duplicating query semantics.

Registered first-party source is not excluded merely because another project consumes it as a dependency. v0.0.2 linking must resolve compatible package/repository versions rather than assume every current branch corresponds to an installed package.

## Rust core and runtime bindings

Accepted: Rust implements the common core. Thin .NET and Node.js bindings expose the same storage/query/update implementation in-process. Compiler workers remain permitted. Source-language support and library host-runtime support are different dimensions.

v0.0.1 supports macOS on Apple Silicon only (ARM64), with `aarch64-apple-darwin` as the Rust target. Intel Macs and Windows/Linux are outside the release; minimum macOS version and later platform timing remain open. Keep OS-specific filesystem watching, paths, and native loading at narrow boundaries; do not implement unsupported-platform infrastructure ahead of need. Native bindings and compiler worker packaging must match the supported architecture.

Open binding concerns: native packaging, ABI design, handle/memory ownership, async/cancellation bridging, error propagation, and resource disposal. These are boundary concerns; domain behavior stays in Rust.

Do not build a separate core in each host language. Accepted integration mechanisms are a C-compatible Rust ABI with P/Invoke for .NET and a native Node-API add-on for Node.js. Exact helper libraries, ABI functions, ownership, async, and packaging designs remain open; see [runtime bindings](RUNTIME_BINDINGS.md).

Rust supports system-library artifacts for other languages; Node-API supports native add-ons, and .NET supports native calls through P/Invoke. These mechanisms establish integration paths, not completed packaging or lifecycle designs: [Rust linkage](https://doc.rust-lang.org/reference/linkage.html), [Node-API](https://nodejs.org/api/n-api.html), [.NET P/Invoke](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke).

## SQLite storage and bounded Rust graph traversal

Embedding-based semantic code search is deferred beyond v0.0.1. Initial retrieval requires symbol/text search and graph navigation/impact. Do not add a mandatory embedding model, vector index, model service, or generalized retrieval framework to support speculative later features. Future embedding retrieval should consume the same identities and source evidence without redefining graph facts or compiler semantics.

Accepted initial design, subject to performance gates: SQLite stores durable facts and text indexes in one database per repository/worktree, plus a small central registry; Rust implements bounded graph traversal. Keep tables/index access inside the Rust core and expose typed operations, not SQL, through the bindings. Schema, Rust binding crate, transaction/publication strategy, and v0.0.2 cross-repository catalog/link details remain open.

Accepted initial SQLite mode: WAL, with bounded reader lifetimes and managed checkpointing. Bundle a pinned SQLite build with FTS5 across the coordinator and native libraries, using a shared release pin/build-feature policy. This trades additional native artifact size for consistent available features and storage behavior. Exact pinned version, Rust binding crate, build flags/linkage, reader/cursor mechanism, checkpoint policy, and synchronous settings remain open; see [reader lifetime](UPDATE_PIPELINE.md#5-read-with-explicit-lifetime).

Accepted initial source storage: retained source text lives in the workspace SQLite database, sharing identical payloads across targets/generations while preserving distinct artifact identities and provenance. Source publication/cleanup obey generation/read/recovery lifetimes. External blob storage waits for a measured need; exact content keys, schema, compression, and retrieval strategy remain open. See [source artifacts](FACT_MODEL.md#source-artifacts-and-spans).

Accepted lexical-search engine: SQLite FTS5 in the workspace database, subject to code-search quality/performance gates. Use the common Rust search operators and served-generation/project/target filters; the search index is derived from retained facts/source and is not independent semantic truth. Include FTS maintenance in coherent publication/reclamation. Tokenization, ranking, exact-match behavior, and content/row layout remain open; do not infer substring/regex capability or code-aware ranking from engine selection. The accepted pinned bundled build must provide FTS5; exact version/flags/linkage remain open. Primary reference: [SQLite FTS5](https://www.sqlite.org/fts5.html).

Use indexed adjacency lookups and bounded working sets; do not load the entire estate graph into every consumer. Caching is optional and must be measured rather than become a second authoritative graph store. SQLite's concurrent-reader/single-writer-per-file behavior fits coordinated publication, but snapshot lifetime/checkpointing, query plans, write contention, and estate-wide fanout still require validation. SQL may support lookups and set operations; graph semantics stay owned by Rust.

Approval of this initial store does not establish that performance goals have passed. Validate meaningful symbol/reference lookups, forward/reverse traversal, incremental publication during reads, cold working sets, and bounded memory at representative scale. Do not create several backend adapters preemptively; retain the typed fact/artifact migration boundary for a measured need to change persistence later. Sources: [SQLite use cases](https://www.sqlite.org/whentouse.html), [WAL](https://www.sqlite.org/wal.html).

The user's intended work deployment spans hundreds of repositories with C# services and an Angular frontend. SQLite is accepted initially with performance gates; partitioning, query planning, and upgrade mechanics remain design work. [Scaling](SCALING.md) describes candidate partitioning, federation, compatibility, and migration boundaries.

Whole-estate cross-repository queries target v0.0.2 on a local Apple M5 Max workstation. v0.0.1 must retain composable repository/compilation identities and versioned facts/artifacts. Prioritize disk-backed local query execution, bounded caches, independent updates, and limited compiler-worker residency; distributed query infrastructure is not required for this target. RAM/storage budgets remain open.

The [source review](SOURCE_REVIEW.md) records compiler, project identity, incremental update, lifecycle, and embedding findings from pinned reference clones.
