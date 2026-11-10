# Query deadlines, cancellation and pagination

Status: overall deadline and complete core response-cap slices have passed focused Debug validation. Generation-retention and lifecycle-policy primitives are implemented; coordinator/control/page/retained-byte wiring remains unfinished. Accepted decisions are requirements, not real-time guarantees. See [release criteria Q11/Q12 and S04](RELEASE_ACCEPTANCE.md).

## Accepted outcome at the overall deadline

On expiration of a query's overall deadline, return the typed error `deadline_exceeded` and release its reader and request resources. Do not return a partly assembled successful page. This is independent of graph traversal limits: a traversal may still return explicitly partial findings with its existing limit/evidence metadata when its own bounded search finishes before the overall deadline. Query expiry never cancels shared background indexing.

The accepted freshness wait remains 500 ms by default, configurable per request and capped by its remaining deadline. It is a phase budget, not the overall deadline. Freshness fallback remains a coherent stale result, or not-ready without a usable publication, if the overall deadline still permits executing the query.

## Accepted overall budget

The user accepted a 5,000 ms default overall deadline, configurable per request from 0 through 120,000 ms. Zero means immediately expired. Time spent waiting inside Know Code, freshness checks, query execution and response encoding consumes this one budget. These are stopping budgets, not latency SLOs or a hard real-time scheduling guarantee.

## Shared implementation

`execution.rs` carries one monotonic clock through asynchronous native dispatch, freshness control, read-view setup, SQLite statements/lock waits, Rust projection/traversal and core JSON encoding. There is no timer thread per query. SQLite uses progress and busy callbacks; ordinary Rust loops cooperate at work boundaries. The encoder writes bounded chunks, checks final UTF-8 construction and checks again after dropping the materialized result. Expiry discards partial success.

A deadline-aware local socket uses nonblocking connect/read/write and polling against remaining time, including trickling frames. The coordinator receives only the remaining freshness wait. On this tested macOS, setting a read timeout after the peer closes returns EINVAL even while response bytes remain readable; the polling path drains those bytes. Raw owned macOS sockets set SO_NOSIGPIPE, preserving an embedding application's signal policy.

The per-call native read view is dropped before response encoding. If a low-level Rust caller retains a ReadView whose query expires, the query rolls back its read transaction and marks that view unusable; open a new view rather than silently switching generations. Successful explicit Rust read views retain their existing ownership behavior. Expiring cursors and a general maximum retained-reader policy remain unfinished.

Node records time before request serialization/native dispatch and before libuv queueing. .NET records time before serialization/Task.Run and passes elapsed milliseconds through additive `kc_query_json_elapsed`; Node uses additive `queryJsonElapsed`. Current SDKs reject native artifacts lacking those exports. Older low-level C/Node entry points remain available and apply the same defaults from native admission. C ABI 1, worker/control protocol 7 and store format 6 are unchanged. No coordinator protocol feature is needed to send a smaller existing wait_ms value.

MCP captures its clock before blocking dispatch and advertises the shared constants. Its two permits remain held until core work exits; expiry does not detach still-running work. CancellationToken/AbortSignal and prompt underlying cancellation on MCP cancellation remain unfinished.

Enforcement is cooperative: OS scheduling, filesystem calls, allocation, a single compiler-report JSON parse or other noninterruptible work can delay observing expiry. Millisecond host elapsed-time transfer has sub-millisecond rounding/marshalling gaps. A saturated host thread pool may delay delivering the error until it runs the expired task; that task must not begin index reading. Core JSON encoding is covered; host JSON parsing, outer MCP framing/output backpressure and caller processing are outside this implemented stopping boundary. No hard wall-clock response-delivery promise, peak-query-memory cap or outer-transport byte cap is claimed. The complete core JSON response cap below is separate. These limits do not change the accepted overall-budget objective or close the remaining release gates.

## Accepted cursor portability

The user accepted opaque, workspace-bound continuation tokens that work across local clients and separate CLI invocations. They are not tied to one .NET/Node object, MCP connection or process. Page queries still execute in the caller's process through the Rust core; portable tokens do not authorize moving graph execution into the coordinator. All pages preserve one generation and the original query scope/provenance. Expiration or invalidation requires an explicit restart, never a silent switch to current data.

Portable tokens do not permit changing query scope on continuation. Pagination is not implemented by this documentation step.

### Accepted replay behavior

The user accepted replay-safe continuation tokens. Reusing a valid token returns the same page from the same generation, without advancing or skipping results. Freshness/coordination metadata can change as indexing progresses; indexed items, their order, source, provenance and the next logical position stay fixed. A new page token or a retry must not reset the cursor's original creation time. Ordinary request failures such as deadline expiry do not consume the continuation position. Token validity still depends on expiry, coordinator incarnation and accepted pressure invalidation.

Successful concurrent retries must agree on the page/next position. The implementation may use deterministic continuation or bounded retained page outcomes, but re-running a time-limited graph traversal is not sufficient proof of stable replay. Page/frontier state needs explicit accounting and bounds; the accepted index-retention budget does not imply an unlimited page cache or a resident graph.

### Accepted lifetimes and restart behavior

The user accepted **2 minutes idle** and a **10-minute absolute lifetime**. Page activity may renew the idle window but never resets the creation time or extends the total lifetime. An expired token requires a new query; it cannot be renewed or silently moved to the current generation. Expiry checks must reject stale tokens even if physical cleanup has not run yet. Detailed activity/renewal timing and active-page cleanup follow the shared reader/deadline contract; timeouts are not a promise that disk cleanup runs at the exact expiry instant.

The user accepted invalidating all outstanding cursors on coordinator restart in v0.0.1. Return a clear typed `cursor_invalidated` error requiring a new query. Tokens remain portable across clients/processes while that coordinator instance runs. This does not require persisting/recovering cursor leases or continuation state across restart. Loss of optional live-freshness acceleration alone is not a coordinator restart and must not invalidate cursors.

### Accepted retention-pressure behavior

The user accepted invalidating the least recently used idle cursors under cursor-retention disk pressure, returning an explicit restart error so indexing can continue. Invalidation must not silently advance a cursor or discard facts/source beneath an active page read. This policy allows a token to become invalid before its idle/absolute timeout. Shared retention accounting and safe active-query cleanup remain implementation work. A budget must not silently grow with workspace count or available workstation RAM.

### Accepted admission when capacity is full

At the shared cursor-count limit, the user accepted the same least-recently-used idle eviction policy as retention pressure. Reject a new cursor with typed `cursor_limit` when every existing cursor is busy; do not evict an active page read to admit it. The configurable starting count limit remains 128 per user across workspaces.

When retention space is needed and every cursor is serving a page, the user accepted deferring publication until a bounded active page read finishes, then evicting an idle cursor. Active reads remain coherent and prior serving results remain explicitly stale during the delay. This does not authorize increasing budgets or keeping a serving write transaction open while waiting. Implementation must prevent further admissions from starving a pending publication and reclaim abandoned page borrows within their deadline/lifetime bounds.

### Accepted shared retention budget

The user accepted a configurable **1 GiB (1,073,741,824 bytes)** starting budget for extra cursor-retained index data, shared per user across all workspaces. Count data retained solely for cursor generations; exclude data still needed by the current serving generation. Shared units/source must not be charged repeatedly for each token. This is a disk-retention policy, not RAM reservation/preallocation, a per-workspace allowance, or a hard SQLite database/WAL file-size cap.

The budget is personal, with one effective value enforced by the coordinator across workspaces/providers/clients. Exact settings encoding and retained-byte accounting remain implementation decisions to validate. Physical database, freelist and WAL size remain separate measurements; reclaimed pages can be reused without shrinking the file. The accepted idle-cursor pressure policy applies when this shared retention allowance is exhausted. The user also accepted a configurable starting limit of **128 live cursors per user**, shared across all workspaces, providers and local clients. Count cursors independently of marginal retained bytes: shared or current-generation pins cannot create unlimited metadata. This is an upper bound, not a preallocated pool or a multiplied per-workspace allowance. Count-limit admission/eviction is accepted as specified above; per-cursor continuation-state bounds remain under design.

### Accepted source/context use through a cursor

The user accepted fetching source and symbol context through a valid cursor's pinned generation while indexing advances. Locations from a page remain useful until that cursor expires or is invalidated. These are separate bounded reads through the snapshot; they do not advance or rewrite the original pagination scope. Returned source, hashes, extents, compilation provenance and coverage must match that generation. No current disk text or newer index generation may be substituted. Invalid/expired cursors require an explicit restart rather than a latest-generation fallback.

### Accepted compute-once graph pagination

The user accepted computing a bounded graph result once, retaining it locally and serving repeatable pages from it for v0.0.1. The first page waits for that bounded traversal; subsequent pages and retries reuse its indexed items, order, provenance, uncertainty and work-limit outcome. Computation remains in the common in-process Rust operators, not the coordinator. This does not authorize computing the entire workspace graph or removing the overall deadline/node/edge/path limits. Retained result data needs finite per-cursor and shared bounds and cleanup on expiry/invalidation, alongside generation/source retention. Exact spool encoding, page packing and first-page retry identity remain implementation work.

### Retention finding and proposed mechanism

Before the storage foundation, `Writer::open_controlled` recovery and `Writer::finish_publication` deleted noncurrent manifests without cursor retention. They now preserve explicitly pinned manifests (foundation below). A token containing only a generation number cannot recover those deleted rows in a new process. Current long-lived ReadViews preserve an SQLite transaction in one process; they cannot supply the accepted portable token by themselves.

Proposed mechanism: let the sole coordinator/writer maintain bounded leases over logical generation manifests. Publication/cleanup retains compilation ownership and exact source while a lease is valid; individual page requests open short SQLite transactions against that retained generation through the same operators. No SQLite transaction should stay open between pages, and no unbounded full result set/index should be copied just to enable pagination. Bounded page outcomes may be retained for replay under explicit state budgets. Lease changes must serialize with staging recovery, publication and reclamation; accepting a pin after its generation was reclaimed must fail explicitly. Temporary pins remain reader retention, not searchable history.

Bind token validation to workspace identity, index incarnation/compatibility, immutable query scope and continuation position. Reuse existing control framing for lease coordination and shared query operators for execution. Use a coordinator incarnation that exists independently of optional live-freshness acceleration. Startup may retire the previous incarnation's cursor leases after acquiring sole-writer ownership; existing SQLite reader transactions still need coherent cleanup. Exact storage/encoding, retained-byte accounting, admission/publication synchronization, graph continuation state and cleanup remain under design. This is an implementation proposal derived from the inspected code, not a claim that the lease path exists or that portable tokens have passed tests.

### Read-only sizing observation

The existing frozen store-v6 ts-pattern snapshot occupies 36.4 MiB on disk with 19.3 MiB allocated pages; Polly occupies 681.5 MiB with 362.6 MiB allocated pages. Measured read-only without touching live indexes; raw page/file accounting is `.artifacts/cursor-design/snapshot-storage.json`, tied to the saved datasets in `.artifacts/query-performance/1794075844642436000/datasets.json`. These are historical corpus snapshots, not current estate-size acceptance or measurements of marginal cursor retention. Shared unchanged units and source deduplication mean cursor count cannot simply be multiplied by full index size.

Cursor-only logical retention and physical database/WAL size need distinct accounting. Reclaiming facts makes pages reusable; it does not promise immediate file shrinkage. The accepted 1 GiB starting budget applies to extra cursor-retained data across workspaces; it is not inferred to be a measured hard physical file limit. Measure this proposed lease path before claiming a hard physical disk bound or that a pinned generation costs nothing.

### Storage foundation under focused validation

`cursor::GenerationRetention` is a coordinator-owned storage primitive, not a consumer pagination API. Callers must hold the coordinator's sole-writer fence. It atomically pins the current published generation; `GenerationPin` is a read-only descriptor whose drop performs no writes. Pinned readers check owner/pin in their SQLite snapshot and use existing query operators. Explicit release removes only the released generation's unused manifest, preserving shared source/compilation ownership and already-open coherent WAL readers. A new owner invalidates old descriptors; same-owner activation is idempotent.

The optional retention metadata has its own version 1 contract while base fact store format remains 6. Unknown retention metadata stops writer mutation with update_required; ordinary current-state readers remain compatible. No old coordinator may advertise cursor support or create pins. A coordinator restart invalidates cursors, so an older coordinator taking sole ownership does not promise continued cursor support. Uncoordinated writers remain outside the managed-index contract.

The writer records active staging generations so cursor cleanup cannot reclaim an empty stage before its first compilation frame. Recovery clears abandoned stages after acquiring workspace writer ownership. Short staging/publication write transactions now begin IMMEDIATE: pin/release transactions cannot commit between a writer's read and later write and cause SQLITE_BUSY_SNAPSHOT. Compiler execution and publication-pressure waiting do not hold these write transactions.

### Cursor lifecycle foundation

`cursor::CursorRegistry` adds a coordinator-owned policy primitive over generation pins: one registry spans workspaces, with a configurable positive count limit (starting default 128), 2-minute idle and 10-minute absolute lifetimes, workspace/owner validation, LRU idle eviction and bounded active-read leases. Count admission returns `cursor_limit` when all candidates are busy. A pending-publication gate blocks new cursor/read admissions so existing reads can finish or expire before idle eviction; no serving write transaction is held while waiting. Expired/evicted descriptors cannot reopen retained generations. Active read bookkeeping is additionally bounded by the same configured count; concurrency saturation or the publication gate returns `resource_limited`, not an assertion that the index is incomplete. This internal guard is not a final general query admission/memory policy.

Borrow deadlines are capped by the remaining request budget, the 120-second request maximum and the cursor's absolute lifetime. Finishing a live read refreshes idle activity without changing creation time. A lost client cannot retain an active borrow indefinitely. Host integration must periodically sweep, serialize access and own the sole-writer fence; dropping a descriptor performs no writes. The primitive is not yet connected to coordinator startup, control, publication, user settings, byte accounting or native query entry points. Consequently this does not advertise working portable pagination, automatic lifecycle enforcement or a 1 GiB retention guarantee.

The shared budget clock now uses macOS `CLOCK_MONOTONIC_RAW`, which includes machine sleep and ignores wall-clock corrections. The pinned Rust standard library's `Instant` uses `CLOCK_UPTIME_RAW`, which pauses during sleep; that was unsuitable for cursor expiry and overall query budgets. Query admission/phase/execution and cursor policy share the continuous clock. Unavailable or reversed clock readings fail closed rather than extending a query. Evidence comes from local primary sources: Rust 1.99.1 `library/std/src/sys/time/unix.rs` and macOS `clock_gettime(3)`. No test suspends the user's machine; deterministic lifecycle tests advance policy time across exact idle/absolute/deadline boundaries. Other kernel/host scheduling and host-side pre-admission timing remain outside a hard real-time guarantee.

This foundation does not yet enforce manager TTL/count/retention budgets, publish-wait admission or replay/portable tokens. Those remain required before exposing pagination. The focused storage tests include actual overlap at publication and definition-reference validation; this is stronger than sequential interleaving alone.

## Accepted complete-response byte budget

The user accepted **256 KiB (262,144 bytes) by default**, configurable per request up to **2 MiB (2,097,152 bytes)** for each page's shared UTF-8 JSON response. Count the complete core result envelope, including required metadata, diagnostics, signatures and source; existing source/detail/graph sub-budgets do not grant additional envelope bytes. MCP's extra protocol framing/duplication requires a separate transport bound and is not counted as if it were free. This cap is an output budget, not a whole-query peak-memory guarantee.

The common Rust encoder now enforces `response_max_bytes`: 1,024–2,097,152 bytes, default 262,144. Count actual UTF-8 JSON bytes after escaping, including the `{result: ...}` envelope and appended coordination metadata. An over-budget encoding returns small typed `response_limit_exceeded`, with no broken/partial successful JSON or silent removal of required evidence. Invalid budgets return `invalid_request`. Before a request's budget is validated, rejection errors are bounded at the 1 KiB minimum; this respects every valid requested budget without parsing the request twice. Oversized error details use an explicit small limit error.

Native CLI/.NET/Node/MCP query responses use one bounded serializer. Public Rust ReadView queries measure the same encoded envelope with a sink instead of allocating another JSON buffer; deadline expiry during that check still releases/invalidates the reader. The encoder limits its output growth, not the already materialized result's entire memory footprint. Source/detail/graph sub-budgets still apply independently; requesting a large source/graph budget does not automatically increase the envelope budget.

Current SDKs require additive native query contract version 1 (C `kc_query_contract_version`, Node `queryContractVersion`), covering overall deadlines and complete JSON caps. Older/mismatched native artifacts raise typed update_required. C ABI 1, worker/control protocol 7 and store 6 remain unchanged. Existing coordinator versions satisfying analysis/resource contracts can still coordinate these in-process queries; no new query RPC engine is added.

Page packing and portable cursor implementation remain pending. Current oversized nonpaginated results fail explicitly; narrow the query or raise response_max_bytes within its maximum. A required metadata envelope or individual item that cannot fit must not be silently stripped to manufacture success. MCP outer framing/duplication is still separately unbounded by this core field.

## Decisions still open

Explicit request-cancellation outcomes and host API; outer transport limits and pagination packing; admission bounds; cursor retention mechanism, detailed renewal timing, pressure accounting/synchronization and continuation-state bounds. These must use one Rust execution path across CLI/MCP/.NET/Node. Keep SQL/storage private and avoid a separate pagination engine or per-language implementation.

### Continuation implementation findings

Symbol search already has a stable explicit ordering (`name`, opaque symbol ID, compilation ID). References order by source path/range/compilation; coverage orders by compilation ID. These can reuse the existing operators with generation-bound continuation keys, subject to tie-breaker tests. Do not implement continuation by repeatedly increasing an offset over a changing generation.

Graph queries additionally hold traversal frontier, visited/context selections, evidence paths and global work limits. Their own time budget can stop a search at different points; reissuing a query against a fixed generation alone does not establish replay-identical pages. The accepted replay semantics require deterministic page outcomes or bounded retained results/frontiers; that implementation remains open. Paging cannot silently reset a logical traversal's node/edge/context limits or combine incompatible compilation paths. Keep extraction/provider logic out of pagination.

### Accepted traversal budgets across pages

The user accepted node/edge/path work limits spanning the entire logical cursor. Each page request gets its own complete-response byte limit and overall request deadline. Paging/retries must not reset traversal counters, context-compatibility evidence or path limits. Idle/lifetime clocks remain separate from traversal computation time. Exhausting a logical work limit remains explicit partial coverage; reaching the end of materialized pages alone is not proof of exhaustive results.

## Validation outline

Use focused core/native boundary checks during development. Cover SQLite work interruption, Rust traversal/projection interruption, freshness and admission consuming the same deadline, coherent pages, reader release, typed native parity, and indexing continuing after a query stops. Cursor tests must publish updates between pages, preserve exact source/provenance, enforce idle and absolute expiration with an explicit restart, reject tokens after coordinator restart, preserve tokens across mere client reconnects, and verify retained storage is reclaimed. Run the broader suite at the settled integration checkpoint; documented partial implementation is not release acceptance.

## Focused validation checkpoint

### Lifecycle policy and continuous-clock foundation

Focused Debug evidence: [.artifacts/cursor-lifecycle/1794323847861175000/validation.json](../.artifacts/cursor-lifecycle/1794323847861175000/validation.json). All 77 Rust tests, both actual Node/.NET queue-deadline checks, 18 isolated coordinator checks, formatting and locked all-target Clippy passed. The five new lifecycle cases use actual SQLite publication/pin/source cleanup and deterministic policy time: shared cross-workspace count/LRU, active-reader protection, idle/absolute boundaries despite activity, abandoned borrows, publication admission fencing, concurrent retry bookkeeping, clock reversal and failed-cleanup accounting. The additional query-clock case checks unavailable/reversed time and overflow without a fail-open budget. No test suspends the user's machine.

The manager remains an unwired common-core primitive; portable control/tokens, retained-byte accounting, saved graph results, page packing/replay, periodic coordinator cleanup and cursor source/context entry points remain pending. This is not full Debug, optimized-profile, refreshed distribution or installed acceptance. No new dependency, live coordinator/login change or publishing occurred. No independent subagent review was performed for this increment.

Implementation freeze `16448cf` passed 64 Rust workspace cases, 30 Workspace API checks, 23 MCP checks, both actual host-queue saturation tests, 11 live-freshness checks and 18 coordinator checks. Formatting and all-target Clippy pass. The stopped-coordinator case expires during freshness waiting, then verifies a new saved change still publishes. The retained-view SQL test confirms an expired read no longer blocks WAL truncation. An isolated child resets SIGPIPE to its default and survives peer-close with BrokenPipe.

A durable direct probe confirmed the macOS EINVAL/buffered-response condition; the initially failing SDK/MCP freshness runs and fixture compilation failures remain recorded. The final polling path and matching native artifacts pass the affected checks. Exact source/build/log hashes, test identities and read-only review are in `.artifacts/query-deadline/1794277314956542000/validation.json`; `.artifacts/query-deadline/current` points there.

This is focused Debug evidence, not a new full Debug, optimized-profile or packaged-candidate certification. Existing archives predate this deadline implementation. No live coordinator/login job changed and nothing was published. Remaining cancellation/admission/byte/cursor and outer-transport work is not waived.

## Complete-response validation checkpoint

Freeze `1994770` passed 66 Rust workspace tests, 31 Workspace SDK checks, 24 MCP checks, five cross-API response-budget checks (18 generated-fixture requests through CLI/Node/.NET) and both real host-queue tests. Formatting and all-target Clippy pass. Tests cover exact encoding boundaries, mid-Unicode cutoffs, escaping/envelope/metadata bytes, bounded errors before request validation, direct Rust reader reuse and deadline cleanup, typed SDK recovery, and explicit larger budgets without dropping evidence.

Read-only review caught the pre-validation error edge; the final 1 KiB conservative policy and regression close it without a second parse. Exact source/build hashes, final test identities/log hashes and review are in `.artifacts/response-budget/1794282495361761000/validation.json`; `.artifacts/response-budget/current` points there. Earlier diagnostic logs remain retained.

This is focused Debug evidence. Full Debug, optimized-profile, refreshed archives/installed acceptance, outer MCP limits, query peak-memory/admission policy and portable cursor/replay implementation remain open. No live installation/login job changed and nothing was published.
