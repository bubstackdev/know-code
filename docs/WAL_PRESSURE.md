# Shared temporary disk and WAL pressure

Status: accepted policy; implementation in progress. The manual SQLite probe below is evidence for this design, not proof of automatic enforcement or v0.0.1 readiness.

## Accepted behavior

Use the existing configurable temporary-disk pool: initially 2 GiB, with a separate 512 MiB available-volume reserve. Charge selected serving WAL sidecars, pending replacement families and retained retired families once. Current serving main databases remain ordinary index storage. The persisted setting remains `rebuild_disk_mib`; `--temporary-disk-mib` is a CLI alias, not a second allowance. Accounting/enforcement is sampled and cannot promise an instantaneous filesystem quota. This pool spans workspaces owned by one coordinator/data root; independently configured coordinators do not aggregate it automatically.

An open raw Rust read view may prevent WAL reclamation. Preserve its coherent snapshot. Pause affected indexing, release compiler workers and the analysis slot, expose prior results as stale with a disk-pressure outcome, and resume after reclamation creates useful headroom. Maintenance must continue while indexing pauses, because dropping a raw reader sends no coordinator event. Preserve cancellation, saved-input failure fingerprints, queue fairness and dependency-preparation authorization. Automatic resumption must not grant a restore retry, clear an unrelated analyzer error or start a compile/abort/cleanup loop.

Rebuild preflight waits automatically for space **before allocating an attempt**. Once an active rebuild fails or is interrupted, preserve the existing explicit-refresh retry rule, including a rebuild stopped by its disk limit. Do not add a new automatic attempt by treating that failure as ordinary indexing pressure. Compatible old data remains queryable with accurate freshness; known corrupt data is never fallback.

## Common-core implementation boundaries

- Recovery owns one shared disk census and bounded sampling, using existing owned-family checks and temporary-disk settings. Full preflight/publication checks must discover newly retired or pending families; cached byte totals must not hide WAL growth.
- Resource control distinguishes disk pressure from ordinary memory/cancellation/analyzer failures. Filesystem accounting runs outside its settings mutex.
- Storage owns nonblocking checkpoint maintenance on the exact healthy incarnation, under the writer fence, without schema migration or query admission. Busy readers defer reclamation. Checkpoint failure after commit cannot turn a successful publication into an unpublished failure.
- Cursor publication retains the writer outside the global cursor mutex on both success and error. Explicit checkpoints, automatic checkpoints and last-connection-close work must not hide inside that mutex.
- Coordinator owns preflight deferral, durable ordinary-job pause/resume, bounded fair maintenance and the active-rebuild retry exception. Paused jobs release shared analysis capacity. No SQL, filesystem census or checkpoint runs under global entry/settings/cursor locks.

Reuse lifecycle capability 3, resource capability 3, storage 8, framing 7 and recovery receipt 2. No public DTO shape, new disk engine, per-language scheduler or background query RPC is required.

## Measurement motivating the policy

The owned manual probe used bundled SQLite 3.54.1, eight 1 MiB source revisions and a healthy sibling workspace. An idle logical generation pin permitted checkpoints. A held raw SQLite snapshot retained WAL growth from 1,277,232 to 3,806,912 bytes with zero frames checkpointed. After the raw view dropped, PASSIVE checkpointed 924/924 frames and explicit TRUNCATE reduced WAL bytes to zero while the logical pin stayed valid. Final source reuse did not extend the 3,317,760-byte main database. These are explicit probe interventions, not automatic maintenance results.

Evidence: [raw measurement](../.artifacts/qa/.artifacts/wal-pressure/d9168f72d8/evidence.json), [admission checkpoint](../.artifacts/query-admission/1794422334119/attempt-04/validation.json). Cold publication took 1,388.32 ms; later samples were 67.82–106.50 ms. These finite direct-fact samples establish neither compiler throughput nor a latency SLO.

## Focused acceptance

Exercise a held raw reader, idle logical cursor, ordinary saves and a healthy sibling under one small shared pool. Verify stale/coherent reads, bounded maintenance, slot/worker release, automatic resume after actual reclamation, and no repeat-analysis loop from rollback alone. Cancel and restart while paused; preserve cancellation and dependency failure/retry rules. Check both rebuild preflight deferral and active-attempt explicit retry. Include selected WAL plus pending/retired/cutover accounting, changed families, free-space reserve, unsupported/corrupt/missing paths and busy checkpoints. Keep full Debug, optimized/platform/encoding/signing/publication gates separate.
