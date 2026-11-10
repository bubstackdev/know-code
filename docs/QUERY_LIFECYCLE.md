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

## Decisions still open

Explicit request-cancellation outcomes and host API; whole-response byte limits; admission bounds; cursor storage/ownership, expiration and retained-reader limits. These must use one Rust execution path across CLI/MCP/.NET/Node. Keep SQL/storage private and avoid a separate pagination engine or per-language implementation.

## Validation outline

Use focused core/native boundary checks during development. Cover SQLite work interruption, Rust traversal/projection interruption, freshness and admission consuming the same deadline, coherent pages, reader release, typed native parity, and indexing continuing after a query stops. Cursor tests must publish updates between pages, preserve exact source/provenance, enforce expiration with an explicit restart, and verify retained storage is reclaimed. Run the broader suite at the settled integration checkpoint; documented partial implementation is not release acceptance.

## Focused validation checkpoint

Implementation freeze `16448cf` passed 64 Rust workspace cases, 30 Workspace API checks, 23 MCP checks, both actual host-queue saturation tests, 11 live-freshness checks and 18 coordinator checks. Formatting and all-target Clippy pass. The stopped-coordinator case expires during freshness waiting, then verifies a new saved change still publishes. The retained-view SQL test confirms an expired read no longer blocks WAL truncation. An isolated child resets SIGPIPE to its default and survives peer-close with BrokenPipe.

A durable direct probe confirmed the macOS EINVAL/buffered-response condition; the initially failing SDK/MCP freshness runs and fixture compilation failures remain recorded. The final polling path and matching native artifacts pass the affected checks. Exact source/build/log hashes, test identities and read-only review are in `.artifacts/query-deadline/1794277314956542000/validation.json`; `.artifacts/query-deadline/current` points there.

This is focused Debug evidence, not a new full Debug, optimized-profile or packaged-candidate certification. Existing archives predate this deadline implementation. No live coordinator/login job changed and nothing was published. Remaining cancellation/admission/byte/cursor and outer-transport work is not waived.
