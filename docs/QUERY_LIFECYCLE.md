# Query deadlines, cancellation and pagination

Status: the overall deadline slice has passed focused Debug validation; cancellation/admission/response-byte/cursor contracts remain under focused Q&A. Accepted decisions are requirements, not real-time guarantees. See [release criteria Q11/Q12 and S04](RELEASE_ACCEPTANCE.md).

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

Enforcement is cooperative: OS scheduling, filesystem calls, allocation, a single compiler-report JSON parse or other noninterruptible work can delay observing expiry. Millisecond host elapsed-time transfer has sub-millisecond rounding/marshalling gaps. A saturated host thread pool may delay delivering the error until it runs the expired task; that task must not begin index reading. Core JSON encoding is covered; host JSON parsing, outer MCP framing/output backpressure and caller processing are outside this implemented stopping boundary. No hard wall-clock response-delivery promise or total-response memory/byte cap is claimed. These limits do not change the accepted overall-budget objective or close the remaining release gates.

## Accepted cursor portability

The user accepted opaque, workspace-bound continuation tokens that work across local clients and separate CLI invocations. They are not tied to one .NET/Node object, MCP connection or process. Page queries still execute in the caller's process through the Rust core; portable tokens do not authorize moving graph execution into the coordinator. All pages preserve one generation and the original query scope/provenance. Expiration or invalidation requires an explicit restart, never a silent switch to current data.

Portable tokens do not permit changing query scope on continuation. Pagination is not implemented by this documentation step.

### Accepted lifetimes and restart behavior

The user accepted **2 minutes idle** and a **10-minute absolute lifetime**. Page activity may renew the idle window but never resets the creation time or extends the total lifetime. An expired token requires a new query; it cannot be renewed or silently moved to the current generation. Expiry checks must reject stale tokens even if physical cleanup has not run yet. Detailed activity/renewal timing and active-page cleanup follow the shared reader/deadline contract; timeouts are not a promise that disk cleanup runs at the exact expiry instant.

The user accepted invalidating all outstanding cursors on coordinator restart in v0.0.1. Return a clear typed `cursor_invalidated` error requiring a new query. Tokens remain portable across clients/processes while that coordinator instance runs. This does not require persisting/recovering cursor leases or continuation state across restart. Loss of optional live-freshness acceleration alone is not a coordinator restart and must not invalidate cursors.

### Accepted retention-pressure behavior

The user accepted invalidating the least recently used idle cursors under cursor-retention disk pressure, returning an explicit restart error so indexing can continue. Invalidation must not silently advance a cursor or discard facts/source beneath an active page read. This policy allows a token to become invalid before its idle/absolute timeout. Active-query cleanup, admission when no idle candidate can free sufficient space, shared retention accounting and numeric budgets remain design work. A budget must not silently grow with workspace count or available workstation RAM.

### Retention finding and proposed mechanism

Current `Writer::open_controlled` staging recovery and `Writer::finish_publication` delete noncurrent generation manifests, then reclaim unreferenced facts/source. A token containing only a generation number cannot recover those deleted rows in a new process. Current long-lived ReadViews preserve an SQLite transaction in one process; they cannot supply the accepted portable token by themselves.

Proposed mechanism: let the sole coordinator/writer maintain bounded leases over logical generation manifests. Publication/cleanup retains compilation ownership and exact source while a lease is valid; individual page requests open short SQLite transactions against that retained generation through the same operators. No SQLite transaction stays open between pages, and no complete result set/index is copied just to enable pagination. Lease changes must serialize with staging recovery, publication and reclamation; accepting a pin after its generation was reclaimed must fail explicitly. Temporary pins remain reader retention, not searchable history.

Bind token validation to workspace identity, index incarnation/compatibility, immutable query scope and continuation position. Reuse existing control framing for lease coordination and shared query operators for execution. Use a coordinator incarnation that exists independently of optional live-freshness acceleration. Startup may retire the previous incarnation's cursor leases after acquiring sole-writer ownership; existing SQLite reader transactions still need coherent cleanup. Exact storage/encoding, admission/retention budgets, graph continuation state and cleanup remain under design. This is an implementation proposal derived from the inspected code, not a claim that the lease path exists or that portable tokens have passed tests.

### Read-only sizing observation

The existing frozen store-v6 ts-pattern snapshot occupies 36.4 MiB on disk with 19.3 MiB allocated pages; Polly occupies 681.5 MiB with 362.6 MiB allocated pages. Measured read-only without touching live indexes; raw page/file accounting is `.artifacts/cursor-design/snapshot-storage.json`, tied to the saved datasets in `.artifacts/query-performance/1794075844642436000/datasets.json`. These are historical corpus snapshots, not current estate-size acceptance or measurements of marginal cursor retention. Shared unchanged units and source deduplication mean cursor count cannot simply be multiplied by full index size.

Cursor-only logical retention and physical database/WAL size need distinct accounting. Reclaiming facts makes pages reusable; it does not promise immediate file shrinkage. Numeric retention defaults remain under discussion. Measure this proposed lease path before claiming a hard physical disk bound or that a pinned generation costs nothing.

## Decisions still open

Explicit request-cancellation outcomes and host API; whole-response byte limits; admission bounds; cursor retention mechanism, detailed renewal timing, pressure accounting/admission and retained-state budgets. These must use one Rust execution path across CLI/MCP/.NET/Node. Keep SQL/storage private and avoid a separate pagination engine or per-language implementation.

## Validation outline

Use focused core/native boundary checks during development. Cover SQLite work interruption, Rust traversal/projection interruption, freshness and admission consuming the same deadline, coherent pages, reader release, typed native parity, and indexing continuing after a query stops. Cursor tests must publish updates between pages, preserve exact source/provenance, enforce idle and absolute expiration with an explicit restart, reject tokens after coordinator restart, preserve tokens across mere client reconnects, and verify retained storage is reclaimed. Run the broader suite at the settled integration checkpoint; documented partial implementation is not release acceptance.

## Focused validation checkpoint

Implementation freeze `16448cf` passed 64 Rust workspace cases, 30 Workspace API checks, 23 MCP checks, both actual host-queue saturation tests, 11 live-freshness checks and 18 coordinator checks. Formatting and all-target Clippy pass. The stopped-coordinator case expires during freshness waiting, then verifies a new saved change still publishes. The retained-view SQL test confirms an expired read no longer blocks WAL truncation. An isolated child resets SIGPIPE to its default and survives peer-close with BrokenPipe.

A durable direct probe confirmed the macOS EINVAL/buffered-response condition; the initially failing SDK/MCP freshness runs and fixture compilation failures remain recorded. The final polling path and matching native artifacts pass the affected checks. Exact source/build/log hashes, test identities and read-only review are in `.artifacts/query-deadline/1794277314956542000/validation.json`; `.artifacts/query-deadline/current` points there.

This is focused Debug evidence, not a new full Debug, optimized-profile or packaged-candidate certification. Existing archives predate this deadline implementation. No live coordinator/login job changed and nothing was published. Remaining cancellation/admission/byte/cursor and outer-transport work is not waived.
