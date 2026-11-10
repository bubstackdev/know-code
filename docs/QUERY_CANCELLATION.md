# Query cancellation and MCP transport bounds

Status: assembled implementation under focused validation. This wave combines the accepted parent-discovery and compiler-acceptance packets with cancellation in the common Rust execution path. It does not close all 0.0.1 gates.

## Accepted API contract

The user selected normal host cancellation: .NET query methods accept a trailing optional CancellationToken and report OperationCanceledException; Node Index/Workspace query accepts an optional `{signal}` and reports AbortError (ABORT_ERR). Options are separate from query JSON. The shared Rust/JSON boundary reports `cancelled`. Indexing cancellation remains a different operation; query cancellation never cancels the coordinator job.

A pre-cancelled request does not serialize/open an index. Cancellation requested while work is queued is propagated to the native task, which must exit before its wrapper completes cancellation and releases its handle/registration. Completion can win a race with a late cancellation: successful results are not retroactively changed. Once execution observes both cancellation and deadline exhaustion, cancellation takes precedence. Ordinary invalid requests may already have failed before a later cancellation arrives.

## Shared implementation and ownership

QueryCancellation uses an owned Arc flag. Execution retains/clones that flag through control waits, SQLite progress/busy callbacks, Rust traversal, bounded JSON encoding and page reads. No global ID registry, cancel RPC for graph execution, unsafe borrowed flag or timer thread per query is introduced. Cancel-capable Unix polling checks at bounded intervals; noncancellable requests retain their ordinary deadline polling. SQL cancellation rolls back a retained reader and keeps its typed stop reason; a new reader can use the unchanged coherent publication.

The native Node task owns a cloned flag independently of the JS callback object. .NET uses SafeHandle parameters for native queries and cancel callbacks; registration is disposed before the handle, after awaited native return. The C header documents opaque handle lifetime and explicitly forbids freeing a handle concurrently with any call using it. Direct C consumers own those synchronization obligations. ABI stays1; additive native query contract becomes3. Worker/control protocol7, fact store6 and cursor-control capability1 remain unchanged; parent discovery has its own additive capability1.

Interrupted cursor work uses a separate, uncancelled cleanup budget of at most200ms, capped by the original coordinator read-lease deadline. Creator uploads request discard; ordinary page failures finish the borrow without consuming token position. Failed cleanup still falls back to the coordinator's bounded lease/periodic cleanup, retaining charged files/pins until safe reclamation. Unknown lost acknowledgements cannot authorize unbounded state or cancellation of shared indexing.

## MCP connection and output

The user accepted an **8 MiB complete serialized JSON-RPC frame ceiling**, including request ID, newline, escaping, duplicated text and structured content, and a **5-second output deadline**. This is separate from core response_max_bytes (default256KiB/max2MiB) and its query deadline. The existing SDK codec is retained. Preflight counts the actual envelope without another allocated JSON copy; oversized responses become a small explicit transport error before their bytes are written. The writer also counts actual bounded chunks and fences queued sends after failure. A stalled or partially written connection closes; it cannot append a fabricated complete result after a partial frame. SDK-internal error writes share the writer deadline.

Each query has a child cancellation flag linked to its connection. EOF or I/O/output failure cancels active children immediately, before the SDK's drain period; cancelling one request leaves its parent/siblings running. The permit stays in blocking core work until it exits. Disconnect leaves the coordinator and saved-file indexing running. Incoming lines remain bounded at64KiB; concurrent MCP operations remain bounded at two. No HTTP transport is added.

Enforcement remains cooperative. OS scheduling/filesystem calls, one host parse/allocation or an embedding thread-pool delay can postpone observation/delivery. No hard wall-clock return, whole-host memory ceiling or global admission guarantee is claimed. Physical WAL/checkpoint acceptance, general query admission/peak memory and the other release semantics/recovery/distribution gates remain separate work.

## Focused evidence plan

Reuse existing Rust/core/native/coordinator tests and owned fixtures. Verify queued/pre-aborted native requests, actual SQL and control waits, exact source/coverage after recovery, saved indexing continuing, creator file/pin cleanup, late-cancel isolation, complete-frame accounting and queued-write fencing. Actual MCP process tests must prove active SQL before EOF and real stdout backpressure while stdin stays open. Only a matching producer build and combined checks establish assembled evidence; individual agent or older-native fixture runs remain development evidence. Full Debug remains a settled candidate checkpoint, not a routine development loop.
