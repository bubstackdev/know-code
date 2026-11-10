# Corrupt derived-index recovery

Status: frozen implementation contract; implementation and acceptance remain pending. This slice implements corruption recovery, not incompatible-version upgrade triggers.

## Accepted recovery policy

The user selected one automatic recovery attempt. If it fails, wait for explicit refresh; queries, saved changes, rescans and restart must not relaunch it. Ordinary compiler errors can produce a valid fresh partial replacement. Preserve registrations/settings, workspace identity, saved-source semantics and dependency-preparation identity. Registry corruption never authorizes deleting registrations/settings.

The user accepted a shared configurable2GiB temporary index allowance and512MiB free-space reserve. These are tuning defaults, not hardware requirements. Count candidates and retained/failed-cleanup index file families across workspaces, not per repository; the existing1GiB cursor pool remains a different logical-retention metric. Enforce growth/free-space checks cooperatively with explicit possible sampling overshoot/filesystem overhead; do not promise a hard filesystem quota. One recovery runs in the existing global analysis slot under the common CPU/memory controls. Check the old-baseline-to-retired charge before cutover too. Exhaustion stops recovery visibly as resource-limited; never raise a cap silently.

No damaged-index archive: retain damaged state only for in-progress/failed replacement or active reader leases, then delete after healthy cutover and safe last-reader release. Failed cleanup remains charged. Do not delete source, settings or preparation state.

## Identity, detection and read fencing

The public database path remains a logical workspace handle. One durable selector chooses a unique physical indexes/<nonce>/index.db incarnation. A missing selector initially means legacy index.db; only the coordinator adopts registered owned legacy data. Never rename an open DB/WAL/SHM family. Hold a shared per-incarnation reader lock for its lifetime; cleanup takes a nonblocking exclusive lock. No PID polling or timeout-based assumption that readers are gone.

IndexReadGuard::open(logical,execution), physical_path(), incarnation(), check() and note_corruption() form the shared read hook. It is a no-op for genuinely unmanaged indexes. Keep a permanent monotonic BAD latch per incarnation with cheap cross-process mapped checks, not a filesystem stat on every graph edge. A consumer can latch its exact incarnation even if coordinator RPC is unavailable; only the coordinator publishes replacements. Check admission, cooperative SQL/traversal work and before successful result delivery. Healthy retired incarnations remain coherent for future upgrades; BAD ones are never eligible stale fallback.

Only proven SQLite CORRUPT/NOTADB (including their extended codes) from a guarded index connection, or an explicit bounded integrity-check corruption result, can trigger recovery. Busy/locked, access/I/O/full/resource/dependency/tool/version failures and interrupted/timed-out integrity checks are not corruption. Retain typed fault provenance: logical workspace, incarnation and SQLite cause. Never infer an index fault from registry.db, a worker's own database or arbitrary caller paths. Native index faults report through existing coordinator control; stale/double reports cannot poison a new incarnation.

Durably fence BAD before recovery, revoke live freshness, invalidate all that index's cursor results/pins and prevent fresh/stale success from the damaged incarnation. Cursor retirement must abandon quarantined pin SQL without reopening damaged storage, while preserving failed file-cleanup charges. Pinned descriptors carry the exact index incarnation. Older managed clients must get actionable update_required, including when an older live-freshness fast path would otherwise bypass control. Worker protocol remains7; lifecycle capability1 and resource-control3 are explicit compatibility boundaries.

## Rebuild and restart

Persist attempt-start before analysis. Run the same full refresh/Writer/Governor into a unique candidate, preserving logical settings/preparation paths. Seed candidate generation above the trusted last published workspace generation; resetting to1 must not make old generation-only guards accidentally valid. Validate, close/checkpoint/fsync and mark the candidate ready before atomic durable selector switch. Reconcile the FULL registry after switch, then reclaim retired artifacts when safe. Restart never serves incomplete/unverified candidates or automatically repeats an interrupted failed attempt. It must reconcile crash windows before ready, before switch and after switch before registry completion.

One reusable selector/replacement mechanism must support future compatible-old-data upgrade cutover, but this wave must not claim S07 accepted or add automatic incompatible-version rebuild triggers.

## Owners and focused checks

Recovery owner: recovery.rs helper/health/lifetime/budgets, host coordinator integration, workspace/update destination plumbing, resource defaults and focused fixtures/docs. Root owns lib/Error/control/proto/live/cursors, and integrates store Writer hooks after grouping handoff. Grouping owns query/graph; root attaches the agreed health guard after handoff. No live install, original corpus, signing or publication changes.

Focused cases cover corruption vs false positives, healthy sibling progress, exact saved-source rebuild, monotonic replacement generations, old readers/tokens, stale old-incarnation reports, one failed attempt and explicit retry, disk/free-space/cleanup pressure, and each restart window. Use actual SQLite/provider/native boundaries; full Debug remains a final settled-candidate checkpoint.
