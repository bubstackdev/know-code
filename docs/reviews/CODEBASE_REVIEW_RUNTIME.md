# Runtime review triage before v0.0.1

Assessment date: 2026-11-11. Read-only source review of main
`88f5bd3af1c9a981ac427bcf7e15e34fc8b35740`, including the working-tree documentation
and test changes visible during this review. This assesses the runtime subset of
`CODEBASE-REVIEW.md`; it does not adopt that report's severity or proposed fixes
automatically. No build, native probe, test suite or live-runtime operation was
performed for this assessment. The regression cases below are proposed gates,
not newly passing evidence.

## Recommendation

Fix H2, H7 and H8 before tagging. They have concrete source-level paths from an
ordinary control operation or an expected cleanup/corruption event to a shared
service panic or failure of unrelated workspaces. Add a scoped worker-unwind
outcome for H3; an internal panic must produce a terminal failure or deliberate
coordinator shutdown, rather than silently occupying the only analysis slot.

Treat M5/M19 as one pre-tag liveness problem: the global cursor mutex can be held
while background CPU throttling parks a publication, and other cursor requests
block acquiring that mutex before they can check their deadlines. Establish the
paused-publication regression below and fix that blocking path. This does not
require replacing the cursor subsystem or parallelizing SQLite writers.

M6/M8 are confirmed lock-coupling risks that need focused control-liveness
evidence before the tag. Do not infer an observed user-visible timeout from
source inspection alone. M18 needs conservative accounting/isolation evidence;
unknown retained debt cannot safely be discarded to make a sibling proceed.

H1/M11 are accepted finite transport behavior. M10 is already included in the
accepted transport budget. Changing those to queues would contradict the
current contract. The maximum-size M20 case is real bounded failure behavior,
not a silent generation switch or source-integrity defect.

## Confirmed pre-tag defects and gates

### H2: waiter can index a removed registration

In [coordinator.rs](../../crates/host/src/coordinator.rs), `serve` checks the
workspace once, then the `wait` loop indexes `entries[&request.database]` after
`Condvar::wait_timeout` reacquires the mutex (line 2455 at review time).
`unregister` may remove a queued workspace while that mutex is released. A
subsequent notification, including progress from another workspace, reaches the
index with no entry. The panic occurs while holding `entries`, poisoning the
shared map. The timed-out branch itself breaks before the next index; the race
does not require every unregister to notify directly.

Minimal remedy: use a checked lookup on every wake and return a defined
unregistered outcome. Capture/check registration identity as well, so
unregister followed by re-register cannot silently turn an old wait into a
wait on a different enrollment. Notify waiters when unregister completes.

Gate: hold a waiter on a genuinely queued epoch, unregister that enrollment,
wake it deterministically, and assert a controlled result plus successful
status/query/update for a healthy sibling. Also exercise re-registration at
the same database path. Avoid a sleep-only timing test.

### H7: cleanup after a failed initial borrow can panic

`CursorService::request_inner` admits a cursor and then calls
`borrow_controlled`; on failure it calls `retire(handle.id, ...)` (service.rs
lines 325–338). `CursorRegistry::borrow_inner` first sweeps. If the new pin is
marked BAD in that interval, the sweep may remove it. `CursorRegistry::retire`
then uses `expect("selected cursor")` at cursor.rs lines 582/584. That panic
poisons the service state mutex. This is not a request for unsafe stale reads:
the original corruption/invalidation outcome should be preserved.

Minimal remedy: make retirement of an already-removed entry safe, or check
existence on this cleanup path without introducing another race. Preserve
charges for entries whose actual cleanup failed.

Gate: a deterministic BAD transition between admission and first borrow;
original request fails with the intended typed outcome, no panic/poison, and
another workspace can admit, page and publish afterward.

### H8: one known cleanup failure stops unrelated progress

`CursorRegistry::sweep_inner` (cursor.rs lines 543–579) returns immediately on
the first healthy entry's retirement failure. Only BAD-pin errors become a
retirement warning. `request_inner` sweeps before serving normal actions, and
`prepare_selected` propagates sweeping errors during publication. A failed
unlink, protected result directory or busy pin-release transaction can therefore
fail unrelated requests/publications and skip subsequent reclaimable entries.
The existing `bad_pin_retirement_preserves_other_workspaces_and_failed_file_charges`
test covers the BAD special case; it does not cover this healthy-pin path.

Minimal remedy: retain the retiring entry and its charge, expose the cleanup
warning, continue sweeping independent entries, and let operations whose known
budget permits them proceed. Actual insufficient or unknown budget must still
fail closed. Never delete foreign entries or known-live files as a shortcut.

Gate: healthy A's result cleanup fails; A stays retiring/charged/warned; B can
read a valid page and publish a save with known headroom; a third reclaimable
entry is not skipped. Removing the obstruction later releases A's debt.

### H3: panic lacks an analysis-job terminal outcome

The dispatched closure at coordinator.rs line 1021 sends `Event::Finished`
only after `execute` returns normally. An unwind skips that send, leaving the
main loop's `running`, `analysis_running` and `active_job` ownership unreleased.
No join/outcome guard observes the detached thread's failure. This is a
confirmed conditional failure path, not evidence of a new independently
reproduced panic in ordinary compilation.

`CursorService::publish_with` also updates `pending_publications` without an
unwind guard. Qualify the original report: `stop` does **not necessarily spin
forever** after any publication panic. A panic while holding the state mutex
poisons it, and `stop`'s `if let Ok` may skip its drain instead. Both outcomes
need a defined failure policy; simply asserting an infinite spin overstates
the inspected source.

Minimal remedy: scope terminal job ownership so every unwind is observed and
reported. If a panic invalidates/poisons shared invariants, promptly retire the
coordinator rather than pretending healthy scheduling resumed. Preserve
writer/live-owner fencing and release permits. Do not blanket-ignore poisoned
mutexes or insert `catch_unwind` around only the final send.

Gate: inject an unwind before publication and at the publication ownership
boundary using test-only closures. Observe explicit failure or bounded clean
shutdown, no stranded analysis slot, no stale live certification, and no drain
hang. Production test knobs are unnecessary.

### M5/M19: CPU pause and deadline-insensitive cursor lock acquisition

`publish_with` holds the service state mutex while calling `Control::check`
(service.rs line 635). `Control::check` waits on its condition variable while
paused (resource.rs lines 159–165). It is called again within
`Writer::finish_publication`, including after the write transaction starts.
Other cursor requests use blocking `Mutex::lock` at service.rs line 157 before
their next `Execution::check`. The main coordinator loop also calls `sweep`, so
it can wait behind this mutex. A configured CPU throttle can thus suspend
interactive cursor work unrelated to that background publication. This
conflicts with the accepted priority for interactive queries under contention
and with bounded overall query deadlines; it is not evidence of a measured
encoding-performance miss.

The WAL slice already moved explicit checkpoint and Writer drop outside this
mutex (`CursorService::publish` owns the writer outside `publish_with`), and
disables automatic/last-close checkpoints on the relevant connections. Those
parts of the original M19 criticism are superseded. Commit, retention SQL and
up-to-16-MiB result verification still occur under the mutex. Their real hold
time needs a focused bound; their mere existence does not prove every request
violates its deadline. `publication_waiting` temporarily rejecting admission is
also intentional starvation prevention, although the unconditional flag can
make rejection conservative.

Minimal remedy: do not park for the background CPU budget while holding the
global cursor state lock; preserve atomic commit/cancellation and retention
selection. Make waiting to acquire cursor state observe the request's remaining
deadline. Do not move checkpoints back into the fence or expand the retained
result/query budgets to hide latency.

Gate: hold a publication at a real paused-control boundary, issue a healthy
sibling page with a short deadline, and require completion or typed
`deadline_exceeded` within a finite allowance. Status/cancel/stop must remain
usable. Repeat during result verification and a delayed commit to expose the
remaining lock waits. Use controlled unit barriers before expensive workloads.

## Remaining assigned findings

| Finding | Current assessment | Smallest useful pre-tag action |
| --- | --- | --- |
| H1 / M11 | Accepted bounded behavior. `Connection::register_request` limits pending IDs to four; `begin_frame` limits owned output futures to two. Saturation closes the connection and cancels its queries. [QUERY_ADMISSION.md](../QUERY_ADMISSION.md) explicitly accepts transport failure without a queue, including non-query output. | Preserve actual-SDK blocked-output/duplicate-ID/cancellation tests. Document the per-connection bounds clearly; do not promise that every excess call gets `busy`. No mandatory product change from these findings. |
| M6 | Confirmed lock chain: `Control::publish` holds resource state through commit to serialize cancellation; `serve` cancel holds `entries` and `active_job` while asking that control for cancellation/commit state. A slow commit can stall unrelated control operations. Commit-wins cancellation semantics are intentional. | Test delayed commit + cancel + sibling status/wait. If it blocks past the finite control bound, snapshot identity/control under the short map lock, perform the blocking operation outside it, then revalidate before mutation. Do not remove the commit/cancel linearization lock. |
| M8 | Confirmed broad lock scope: `register_workspace` holds `entries` across settings and `recovery::adopt`; `observe_bad`/report paths perform lifecycle and persistence work while entries are borrowed. Blocking lifecycle flock during cutover can delay unrelated control requests. | Hold a lifecycle operation with a unit barrier while registering/reporting A; check sibling status/cancel. Move only demonstrated blocking preparation outside the map lock, revalidating registration/incarnation before applying results. No new scheduler or registry abstraction. |
| M10 | Structured MCP output duplicates JSON in text and structured content. Already counted by the accepted 8-MiB **complete frame** bound and actual byte/deadline tests. Core `response_max_bytes` intentionally bounds the core JSON separately. | Retain frame-accounting regression, including escaping, ID and newline. Output compactness is optimization backlog; not a reason to drop required structured evidence before release. |
| M12 | Per-tool schemas reject extra properties in their advertised shape, but `call_tool` translates into shared `QueryRequest`, so a globally known field from another operator may be ignored. This is an API/schema mismatch, not a demonstrated trust bypass. | Cheap contract hygiene: shared operator-specific field rejection where semantics require it, with one valid known-but-inapplicable field test. Do not add an independent MCP query validator/engine. This alone does not establish a shipping blocker. |
| M13 input | SDK silently discards lines over 64 KiB. Explicitly documented at [MCP.md](../MCP.md), and tests/mcp.py deliberately checks no oversized-ID reply followed by successful ping/list. Different from the 256-KiB core request cap. | Keep/document the transport-specific ceiling and sender timeout consequence. An explicit close/error is future protocol polish; do not call the existing finite behavior a new regression. |
| M13 coverage schema | Concrete schema inconsistency: mcp.rs `coverage_schema` offers generation-only, whereas `ReadView::query` requires snapshot on managed generation guards. The tool is managed. | Before tag, require `snapshot` in the MCP coverage schema and leave generation optional as an additional guard; test advertised minimum succeeds and generation-only fails actionably. This is a small schema correction, not storage compatibility work. |
| M18 | `budget_usage` propagates selector/measurement failure from any live cursor family. Cross-workspace coupling is real; safe shared accounting sometimes requires it. An unknown liability must not become zero. BAD detection already has a special path. | Gate known-accounted A fault vs valid B page/publication, and separately truly unknown retained debt. Continue reads needing no new retention where safe; preserve conservative charges/refusal where debt is unknown. Reuse H8 work rather than declaring every measurement error ignorable. |
| M20 | Source and existing unit test `frozen_pages_use_request_budget_after_metadata_replacement` establish that fresh coordination metadata can make a frozen page exceed the caller's cap. Returned data remains pinned and the outcome is explicit `response_limit_exceeded`; a larger cap normally works. At 2 MiB there is no larger retry. | Add the maximum-cap metadata-growth case and document original-query restart if replay cannot fit. If release acceptance requires guaranteed replay at the maximum cap, reserve bounded metadata headroom or a bounded coordination projection. That stronger guarantee is not established here. Never strip required freshness/provenance or split atomic grouped findings silently. |

The accepted source of truth for H1/M10/M11 is
[QUERY_ADMISSION.md](../QUERY_ADMISSION.md) and
[QUERY_CANCELLATION.md](../QUERY_CANCELLATION.md): the latter explicitly counts
duplicated text/structured output and accepts a five-second output deadline.
Some older paragraphs in MCP.md still describe admission/output work as pending;
reconcile those stale paragraphs during release documentation cleanup.

## Related low-priority findings

| Finding | Disposition and boundary |
| --- | --- |
| L7 | Owner-only directory/socket hardening needs the separate security review. Existing-directory permissions and same-user trust are not independently exploit-tested here; do not infer a cross-user break just from absent peer credentials. Avoid chmod on arbitrary caller-owned paths. |
| L8 | Same entries-lock scale/liveness issue as M8. Hundreds of repositories is a scale goal; no hundreds-repository latency measurement was performed. Use bounded batches only if the focused lock-liveness gate demonstrates the need. |
| L9 | Blanket claim that registry/persistence errors should be recoverable is rejected. Durable registry failure may require safe coordinator exit. Separate workspace-local derived-index failure from lost durable-control integrity. A disk-full gate should verify no live freshness claim, retained settings, actionable error and safe restart; do not simply swallow `?`. |
| L10 | Actual 64-thread connection bound, with up-to-120-second waits, can exhaust control ingress; overflow closes the accepted socket. This is bounded overload, but status/cancel liveness is important. Gate saturation with idle waits and a cancel, then consider a small reserved control path only if required; do not add an unbounded queue or thread pool. |
| L11 | `Governor::configure` holds its limits mutex over durable save. Confirmed coupling, not a measured normal-path stall. Test failure leaves old effective limits intact and monitor/cancel remain usable; move save before short state replacement only with settings-update serialization preserved. |
| L12 | Invalid page position yields `CursorInvalidated`, and `Lease::finish` invalidates the shared token for a non-creator. Token callers share fate by design, but malformed position need not destroy a valid shared cursor. Backlog or small typed-invalid-request correction with valid-token replay regression; no wrong-generation result shown. |
| L13 | `ResultStore::reset` rejects foreign names and scans at most 4096 entries; the recorded startup `file_error` disables pagination until restart. This is deliberate filesystem safety but harsh availability for a foreign regular file. Pre-tag gate/document actionable repair. Never follow/delete foreign files or increase the scan without a bound. General pagination failure from ordinary healthy cleanup remains H8. |
| L14 | Deadline after finish may leave a tokenless cursor until bounded expiry. Existing count/byte/idle limits bound it; cleanup is not immediate-expiry certification. Backlog unless a repeated-expiry gate finds unbounded ownership. |
| L15 | Several encoded/parsed copies of a bounded 16-MiB materialization are plausible. Global four-slot admission limits multiplicity; it is not a hard RAM guarantee. Retain peak-RSS measurement as performance work; don't infer full-workspace residency. |
| L16 | Writer drop/reclaim remains storage-lane review work. The new outer-owned writer removes that work from `CursorService::publish`'s global mutex on both success and error. Any remaining uninterruptible cleanup should not be misreported as the old hidden checkpoint path. |
| L22 | Focused tools plus bounded `query_graph` are the explicitly accepted hybrid API, not duplicate engines. Distinct max-byte meanings deserve descriptions/defaults, not a forced unified budget. Use `source_max_bytes` for graph bodies; retain core-envelope/frame distinctions. Tool-name diagnostics and remaining descriptions are documentation polish. |
| L23 / L24 | Structured CLI error exit status, version/help/HOME and flag diagnostics are interface polish unless release CLI acceptance explicitly requires a different status contract. Existing automation reads the JSON envelope; changing exit semantics needs matching tests. No corruption/security evidence in these claims. |
| L25 | Synchronous admission before handler enqueue is intentional: capacity must reject before host-queue work. Moving it into `spawn_blocking` first violates that ordering. Measure slow filesystem policy/lock setup if concerned; retain nonblocking kernel slot acquisition and bounded input. No per-query RPC or admission queue is warranted. |

## Finite gate order

1. Source fixes and deterministic unit regressions for H2, H7, H8; scoped H3
   unwind/outcome ownership. Review their shared registration/cleanup invariants.
2. Paused-publication/deadline test for M5/M19; delayed commit/lifecycle tests for
   M6/M8; M18 known-vs-unknown debt isolation. This is one runtime-liveness batch,
   not repeated full Debug runs.
3. Small schema correction/gate for managed coverage, maximum-page metadata gate,
   and documentation reconciliation for accepted MCP saturation/input limits.
4. Run affected coordinator/cursor/MCP/WAL integration checks against one matching
   assembled build. Preserve current WAL preflight-auto-resume versus
   allocated-attempt-explicit-retry assertions; these findings do not authorize
   restore retries, stale BAD reads, cursor invalidation for WAL pressure or
   increased budgets.
5. Only then run the separate final release gates. This assessment does not
   certify full Debug, optimized artifacts, macOS 15, real encoding performance,
   signatures/notarization, installed archives, zero telemetry, licenses or
   publication readiness. Existing passing packets remain historical evidence;
   they do not certify fixes not yet made.
