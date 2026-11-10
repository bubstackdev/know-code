# Corrupt derived-index recovery

Status: frozen implementation contract; implementation and acceptance remain pending. This slice implements corruption recovery, not incompatible-version upgrade triggers.

## Accepted recovery policy

The user selected one automatic recovery attempt. If it fails, wait for explicit refresh; queries, saved changes, rescans and restart must not relaunch it. Ordinary compiler errors can produce a valid fresh partial replacement. Preserve registrations/settings, workspace identity, saved-source semantics and dependency-preparation identity. Registry corruption never authorizes deleting registrations/settings.

The user accepted a shared configurable 2 GiB temporary index allowance and 512 MiB free-space reserve. These are tuning defaults, not hardware requirements. Count candidates and retained/failed-cleanup index file families across workspaces, not per repository; the existing 1 GiB cursor pool remains a different logical-retention metric. Enforce growth/free-space checks cooperatively with explicit possible sampling overshoot/filesystem overhead; do not promise a hard filesystem quota. Configure the guards through `know-code resources --rebuild-disk-mib MIB --min-free-disk-mib MIB`. One recovery runs in the existing global analysis slot under the common CPU/memory controls. Check the old-baseline-to-retired charge before cutover too. Exhaustion stops recovery visibly as resource-limited; never raise a cap silently.

No damaged-index archive: retain damaged state only for in-progress/failed replacement or active reader leases, then delete after healthy cutover and safe last-reader release. Failed cleanup remains charged. Do not delete source, settings or preparation state.

## Identity, detection and read fencing

The public database path remains a logical workspace handle. One durable selector chooses a unique physical indexes/<nonce>/index.db incarnation. A missing selector initially means legacy index.db; only the coordinator adopts registered owned legacy data. Never rename an open DB/WAL/SHM family. Hold a shared per-incarnation reader lock for its lifetime; cleanup takes a nonblocking exclusive lock. No PID polling or timeout-based assumption that readers are gone. A pre-lifecycle legacy reader may lack this lease. Retain and charge its legacy family conservatively after replacement until safe retirement is established; do not claim a lease proves unknown old handles closed. New registrations use guarded physical incarnations from their first publication.

IndexReadGuard::open(logical,execution), physical_path(), incarnation(), check() and note_corruption() form the shared read hook. It is a no-op for genuinely unmanaged indexes. Keep a permanent monotonic BAD latch per incarnation with cheap cross-process mapped checks, not a filesystem stat on every graph edge. A consumer can latch its exact incarnation even if coordinator RPC is unavailable; only the coordinator publishes replacements. Check admission, cooperative SQL/traversal work and before successful result delivery. Healthy retired incarnations remain coherent for future upgrades; BAD ones are never eligible stale fallback.

Only proven SQLite CORRUPT/NOTADB (including their extended codes) from a guarded index connection, or an explicit bounded integrity-check corruption result, can trigger recovery. Busy/locked, access/I/O/full/resource/dependency/tool/version failures and interrupted/timed-out integrity checks are not corruption. Retain typed fault provenance: logical workspace, incarnation and SQLite cause. Never infer an index fault from registry.db, a worker's own database or arbitrary caller paths. Native index faults report through existing coordinator control; stale/double reports cannot poison a new incarnation.

Durably fence BAD before recovery, revoke live freshness, invalidate all that index's cursor results/pins and prevent fresh/stale success from the damaged incarnation. Cursor retirement must abandon quarantined pin SQL without reopening damaged storage, while preserving failed file-cleanup charges. Pinned descriptors carry the exact index incarnation. Older managed clients must get actionable update_required, including when an older live-freshness fast path would otherwise bypass control. Worker protocol remains7; lifecycle capability1 and resource-control3 are explicit compatibility boundaries.

## Snapshot guards

The user accepted an opaque snapshot token for guarded follow-up reads. The same token binds the logical workspace, exact physical incarnation and served generation; it never selects historical state or pins a generation. Ordinary unguarded current reads remain possible. Managed generation-only requests require a fresh snapshot token, since an unreadable legacy index cannot prove its historical generation high-water. Cursor-bound reads still use their original pin and cannot accept another incarnation's token.

Persist a generation high-water reservation before every new managed Writer can publish, under its existing logical writer fence. Gaps are valid. Legacy registry generation is not a trustworthy bound: repeated commit-before-registry crashes, or failed saved-input reconciliation after publication, can accumulate unacknowledged generations. Use known floors where available but never claim numeric uniqueness for unreadable legacy data; the snapshot guard makes that transition safe.

## Rebuild and restart

Persist attempt-start before analysis. Run the same full refresh/Writer/Governor into a unique candidate, preserving logical settings/preparation paths. Seed candidate generation above the durable high-water when available; snapshot checks reject replaced-incarnation data independently of generation numbers. Validate, close/checkpoint/fsync and mark the candidate ready before atomic durable selector switch. Reconcile the FULL registry after switch, then reclaim retired artifacts when safe. Restart never serves incomplete/unverified candidates or automatically repeats an interrupted failed attempt. It must reconcile crash windows before ready, before switch and after switch before registry completion.

One reusable selector/replacement mechanism must support future compatible-old-data upgrade cutover, but this wave must not claim S07 accepted or add automatic incompatible-version rebuild triggers.

## Owners and focused checks

Recovery owner: recovery.rs helper/health/lifetime/budgets, host coordinator integration, workspace/update destination plumbing, resource defaults and focused fixtures/docs. Root owns lib/Error/control/proto/live/cursors, and integrates store Writer hooks after grouping handoff. Grouping owns query/graph; root attaches the agreed health guard after handoff. No live install, original corpus, signing or publication changes.

Focused cases cover corruption vs false positives, healthy sibling progress, exact saved-source rebuild, monotonic replacement generations, old readers/tokens, stale old-incarnation reports, one failed attempt and explicit retry, disk/free-space/cleanup pressure, and each restart window. Use actual SQLite/provider/native boundaries; full Debug remains a final settled-candidate checkpoint.
