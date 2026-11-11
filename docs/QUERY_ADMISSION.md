# Shared query admission

Status: implemented development slice under focused validation. This precedes the physical WAL/reader-pressure checkpoint and does not certify v0.0.1. The assembled focused Debug checkpoint passes 207 Rust cases and native/integration checks; archive and final release acceptance remain pending.

## Accepted behavior

- Fail immediately with typed `busy` at capacity; Know Code adds no waiting queue.
- Default to four shared slots across local clients, native images and repositories for the account, independent of workspace/data-directory selection. Embedding/server hosts may explicitly select one shared scope of their own.
- Capacity is centrally configurable. A public Rust `ReadView` holds one slot from open until Drop, including idle time; its queries borrow that existing charge. Ordinary managed/native queries close their views automatically.

## Common core and ownership

Add one ordinary Rust admission module. Use stable, independently opened nonblocking file-lock slots under a scope directory. The default account directory is `~/Library/Application Support/Know Code/query-admission`; an explicit `QueryLimitScope`/Rust scope or `KNOW_CODE_QUERY_SCOPE` selects an owned host/test scope. Never derive a default scope from a database or data directory. Never unlink/replace slot or policy-lock inodes. Share one reservation with Arc only to extend that same request's ownership; dup/try_clone is not a new reservation. The last owner explicitly unlocks its lease before closing it, so a forked child's inherited descriptor cannot prolong completed work. Close-on-exec applies; kernel locks disappear when their remaining descriptors close. A post-fork child must exec/exit before using the Rust core.

Reservation takes the policy guard shared, reads the authoritative versioned policy, then acquires a slot before releasing the guard. Configuration takes that guard exclusive and acquires every previously authorized slot; active work returns `busy` without changing policy. Replace/fsync only the policy contents after complete quiescence. Do not cache divergent capacities or implement PID counters/a broker. Initial implementation allows 1–64 slots; the supported setter performs reconfiguration, not uncoordinated edits of policy files. Unknown versions, malformed state, access errors and lock contention remain distinct errors.

Reserve synchronously before SDK serialization/cancellation allocations and before libuv/Task.Run dispatch. Public reservations submit exactly once; internal Execution/read-view/cursor calls share one charge without reacquiring. Native work retains ownership until actual completion even after caller cancellation; SDK ownership extends through JSON parsing. Serialize/enqueue/native/parse/panic failures release ownership. Direct Rust/C result delivery transfers owned results to the caller; caller-retained returned objects are outside admission accounting.

For MCP, preserve a query's reservation through its actual response-send completion/drop, with a bounded request-ID handoff if required by the existing SDK model. Also bound per-connection output ownership, including status/busy/error responses; a stalled output cannot accumulate unbounded owned send futures. Saturation closes/fails the transport without a queue, preserving existing output deadlines and indexing independence. Status/cancel/control liveness does not require a query slot.

Use a 256 KiB UTF-8 serialized query request guard before core JSON parsing and before enqueue/marshaling where the host permits it. Reuse one core constant through native metadata; do not add per-language field validators. Existing complete-response/materialization/work budgets still apply. Admission bounds concurrent pending work, not hard RSS: caller inputs, serialization temporaries, runtime GC, delivered results and unrelated embedding allocations remain outside a hard memory promise. Measure actual peaks separately.

## Shared Rust seams

- `admission::AdmissionScope::{local,at,policy,configure,try_reserve}` selects a scope, reads/sets policy and returns a `Reservation`. `configure(max_active_queries)` requires quiescence.
- `Reservation::submit` is crate-private and single-use, yielding a cloneable crate-private `Permit` for one execution. Public host wrappers retain the original reservation through parse/send. Do not expose a reusable public execution token.
- Public scoped JSON entry points and `ReadView::open_in_scope` support embedded hosts and owned fixtures. Existing default entry points use the account scope. Controlled nested opens borrow Execution's permit. Internal coordinator maintenance has an explicit narrow path outside consumer admission; do not accidentally make cancellation/status depend on free query slots.
- Thin FFI/Node reservation handles retain the same Rust reservation; `.NET Index` and Node `Index` accept optional `QueryLimitScope`, and Workspace retains/passes it locally rather than adding it to workspace analysis JSON.
- `know-code queries` reads policy; `--max-active` configures it and `--scope` selects an explicit host scope. It performs no query or coordinator RPC.

## Compatibility

Advance native query contract 6→7 and control/live lifecycle capability 2→3. Existing lifecycle checks cover ordinary and cursor RPCs and live owner records, so no separate admission field, live magic, worker, store, resource or cursor format bump is needed. Durable recovery receipts remain format 2, with catalog read versions `[1,2]`; lifecycle 3 is a safety capability, not receipt format 3.

The current installer accepts lifecycle 3 targets and recognizes lifecycle 1/2 only as known sources for restricted status/maintenance/resume/shutdown bridging. An older lifecycle-2 installer cannot quiesce a lifecycle-3 coordinator. New libraries reject old coordinators; old managed clients reaching the new coordinator fail actionably. Arbitrary old libraries/direct SQLite readers cannot be retroactively controlled: old native6 may still perform stale fallback reads when its control connection fails. Do not claim account-wide enforcement across unsupported historical clients or pre-cutover work.

## Finite validation and work ownership

Core lane owns kernel slots/policy/single-use reservation and focused unit tests. SDK lane owns FFI/Node/.NET early admission, scope and parse lifetimes. MCP lane owns bounded operation-to-output ownership. Integrator owns shared entry-point/ReadView/Execution wiring, compatibility/installer/CLI, fixture isolation and combined acceptance. Keep these lanes out of one another's files; review cross-lane seams before building one frozen combined candidate.

Validate cross-process/cross-native-image capacity, explicit scope isolation, distinct data roots sharing the default scope, quiescent reconfiguration and stable inodes; early rejection before serialization/host scheduling; single submission, failure/cancellation/panic release and slow parse/send ownership; raw-view open/query/drop; one charge for cursor/page/source/context paths; exact UTF-8 request boundaries; maintained certified warm path; real prior-native/coordinator refusal and installer source/target direction. Current fixtures use explicit owned scopes. The first combined attempt exposed older integration fixtures that created five empty account-scope lock files (policy.lock and slots 00–03); no policy, settings or live runtime changed. They were corrected to explicit owned scopes. Stable lock names were retained rather than unsafely unlinking potentially shared inodes. Bound test/compiler work and run focused checks before one combined checkpoint.

The following WAL phase measures bundled SQLite publication/read coherence, checkpoint frame progress, physical main/WAL allocation and reuse after readers release. Logical cursor retention is not a physical WAL ceiling. No numeric WAL ceiling or latency SLO is accepted by this contract; start with finite owned workloads, preserving failures and adding policy only when measurements justify it.

## Admission checkpoint — 2026-11-11

Product source `a22e0876605538fc15c83d309b577f5a88706694`, Debug producer `61703230de034a9fac31b741e45438e1`, passes 207 Rust tests, formatting, strict all-target Clippy, 18 provenance fault checks, nine admission groups and the manual WAL probe. Focused compiler/native end-to-end, MCP, Workspace, cursors, cancellation, deadlines, response bounds, live freshness, coordinator, upgrades, test classification, call graph and project-link checks pass. [Source-bound raw evidence](../.artifacts/query-admission/1794422334119/attempt-04/validation.json). The Workspace test required the recorded test-only lifecycle mock correction `ddab070`; its original failed result remains in the packet. This is segmented validation, not an uninterrupted full-suite pass.

Earlier failures exposed inherited-descriptor lock lifetime and null SafeHandle marshaling defects; both are corrected with regressions. Earlier failed packets are retained. No optimized, archive, minimum-OS, encoding, signing, publication or live-activation claim follows. Automatic WAL pressure is the next independently validated implementation wave; see [WAL policy](WAL_PRESSURE.md).
