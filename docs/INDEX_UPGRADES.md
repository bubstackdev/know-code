# Bounded index upgrade rebuilds

Status: accepted wave contract; implementation and assembled acceptance pending. Reuse the corruption replacement lifecycle and common query/storage/scheduler. No new physical store format or parallel migration engine.

## Accepted policy

The user selected managed formats1–2 for side-by-side rebuild instead of the large private-reference conversion; formats3–7 keep their compatible additive migration to8. Format2 remains readable, including bounded paging/source/context through the common retention implementation; format1 has no usable current read view and reports not-ready during rebuilding. Unmanaged low-level Writer migrations remain supported. Unknown, future and negative formats never authorize replacement, including explicit refresh. Probe owned selected SQLite under its exact guard; distinguish unsupported versions from corruption, I/O and resource failures. Read the actual saved generation as a high-water floor, not just the registry acknowledgement.

The user accepted one automatic attempt, then explicit refresh after failure/interruption. Compatible prior facts remain coherent and explicitly stale throughout queued/running/failed upgrade. Saves, queries, rescan or restart do not retry a consumed attempt. A later explicit refresh may retry; accepted cancellation revokes earlier retry authorization. Ordinary compiler errors may produce a fresh partial replacement. Preserve settings, registrations, identity, dependency-preparation checkpoints and save races.

Reuse the shared two-core/2GiB analysis defaults, 2GiB additional-index allowance and512MiB free-space reserve, with existing configurability/sampled overshoot. One shared compiler job runs at a time. Charge healthy retired physical families too, including ongoing cursor metadata/WAL writes; do not assume other-workspace retired bytes stop growing. Legacy physical families may lack trustworthy reader leases: retain/charge conservatively rather than claiming automatic safe deletion of unknown old readers.

## Common lifecycle and compatibility

Persist a typed replacement cause (corruption or known source/target upgrade) before expensive work; healthy upgrade never sets BAD. Existing staged destination, generation reservation, integrity/checkpoint/fsync, atomic selector and safe reader-lease cleanup stay shared. Ready-but-unselected interrupted work consumes the attempt. Selected-but-unacknowledged cutover reconciles before claiming freshness. Never restore an old index selector as runtime rollback.

Advance the explicit lifecycle capability when adding durable receipt semantics; read prior lifecycle receipts deliberately, reject unknown future receipt versions before interpreting their fields, and preserve source/target evidence through failure. Native query ABI/JSON and worker framing stay unchanged unless the implementation demonstrates a concrete need. Runtime metadata exposes the shared directional store/lifecycle catalog. Installer switches only when target supports source and actual registered contracts; unsafe rollback stays actionable and leaves runtime/index selections unchanged. A bounded installer-only control bridge may quiesce the known prior lifecycle peer; normal clients remain strict. Failed-start rollback is allowed only while the old runtime can still read durable state.

## Healthy readers and cursor publication

Keep healthy old tokens/views pinned to their exact incarnation across cutover, with original source/hash/context. New current reads select replacement and reject old snapshot tokens; snapshot tokens do not pin history. Reset cursor initialization for the newly selected incarnation without invalidating healthy old pins. Proposed generations belong to one physical incarnation. After retirement no old serving generation is a free baseline: charge all old pinned facts plus result files. Candidate cutover uses the same serialized cursor admission/eviction/read-wait policy as ordinary publication. Active page reads remain coherent; only existing pressure/expiry/restart policies invalidate tokens.

## Work ownership and acceptance

Root owns store compatibility catalog, coordinator/control/protocol, installer/runtime/package metadata and assembled native tests. Lifecycle lane owns recovery.rs and focused lifecycle/disk tests. Cursor lane owns cursor.rs, cursor/accounting.rs, cursor/service.rs and focused cursor/legacy retention tests. One read-only reviewer audits contracts, implementation and exact evidence. Worktrees and results remain durable; no live runtime replacement, original-corpus mutation, signing or publication.

Focused checks must exercise actual format1/2 data, readable stale2 source and pages during rebuilding/failure, healthy old pages/context after cutover and subsequent publication, aggregate retained/physical pressure, one failed attempt and explicit retry, future-version refusal without mutation, cancellation/fairness and restart windows. Then freeze one combined source for affected native/CLI/MCP and installed tests. Preserve failures; do not substitute lane results or historical archives for assembled evidence. Full Debug/optimized/platform/real-encoding/signing gates remain separate release work.
