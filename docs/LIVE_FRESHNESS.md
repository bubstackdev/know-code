# Live freshness certification

Managed queries share this Rust path across the CLI, .NET and Node bindings. Graph and storage queries remain in-process. The normal control RPC remains available for every query; live-state acceleration is optional and does not change the 500 ms default freshness wait or generation matching.

## Current query path

Each managed query checks live state under the data root's private `live` directory. The producer publishes one immutable, bounded status file per database and a fixed-size owner file. A kernel lease, PID/start identity, incarnation token, protocol/layout versions, checksums and inode identity bind the answer to the live coordinator. Unknown, malformed, unavailable, stopped, dead or replaced state falls back to RPC. A valid page cannot override the existing served-generation comparison in `query_json`.

A producer advertising the optional `activity_hints` capability accepts bounded, one-way activity notifications on the existing control stream. Eligible current results can use this path on the first request and after idle periods without waiting for an activity reply. A 32-entry, five-second cache coalesces notifications across embedding threads/workspaces. Its advisory timestamps never establish freshness: the live evidence is checked on every query. With compatible older producers lacking that capability, first/five-second activity checkpoints still use ordinary RPC and intervening eligible queries can use live state.

The private owner layout remains version 3. Its checksummed JSON body carries the optional capability, defaulting to false when absent; older clients ignore the extra field and retain their checkpoint policy. Worker/control Protobuf remains v7, store format remains v6 and the native ABI remains v1. Unsupported page layouts use RPC. Package-version equality is not the compatibility test.

## Accepted activity-delivery contract

The user accepted best-effort activity delivery under overload. Notification submission sets nonblocking mode before connect, sends one complete frame of at most 4 KiB, and closes without waiting for a response. Failed/partial submissions do not create a cached checkpoint and fall back to ordinary RPC. A successful send is not an acknowledgement: existing connection admission or delayed processing can lose or postpone a priority update. A later query retries after five seconds. This cannot establish freshness or alter index facts.

The coordinator only updates the existing transient activity timestamp. It does not reset queued-job age, force analysis/restoration or bypass resource limits. Five-second queue aging and the existing active window remain. No background thread is added to embedding hosts. On Darwin, rejected timeout setup after a one-way peer closes triggers a nonblocking drain of its buffered frame; operations with responses retain their ordinary write timeout. RPC fallback retains its existing connection/deadline limits, so this is not a guarantee of nonblocking execution for every query.

## Writer and lifetime invariants

`LiveStatus` retires a workspace's certificate before exposing mutable status fields, including error/progress/availability changes. Successful completed-state publication creates an immutable replacement only after coherent index/registry publication. Unregistration retires the certificate through the same wrapper.

A watcher event carries a pending-input token from before enqueueing until handling completes. Pending inputs block accelerated certification even while the previous status says idle. Queue overflow records a sequence before releasing a dropped token; a completed rescan acknowledges only the observed sequence. Shared words use value/complement encoding and checked, nonwrapping updates. Uncertain counter state retires acceleration.

Readers collect the overflow sequence again after its acknowledgement and check pending inputs again. Their final gate check precedes the final status/owner inode checks. This order matters: an event can finish entirely between checks, returning the gate to clear while retiring or replacing the status file. The old open file must then fail identity validation rather than certify the retired generation.

The endpoint owns a reference to the publisher and disables its lease before releasing the coordinator writer fence. Retained callbacks cannot resurrect it. Kernel process/lease checks also handle abrupt termination without a destructor. A new incarnation cannot reuse an old certificate before startup reconciliation.

The producer never truncates a published owner inode. It creates a fixed 4 KiB backing file, maps aligned shared atomics, then publishes that inode by replacement. Consumers map read-only within a single check; there is no permanent per-workspace mapping cache. The OS rounds mapping size to its page size. Cleanup/restart must unlink or replace files, never truncate a live mapped inode. Cooperative private control-state ownership is part of this lifetime contract; this is not a security boundary against another process deliberately modifying the same user's files.

## Degraded operation and collection policy

Live-state setup, mapping or publication failure does not stop indexing or baseline RPC. Setup failure creates an inactive publisher; later I/O/integrity failure retires the lease. Failed staging files are removed. Acceleration can resume on a subsequent coordinator startup after the resource/path problem is repaired. No healthy index or registration is removed to repair optional control state.

Status files and input gates are required coordination state. No routing counters, usage collector, debug control operation, background consumer thread, upload path or hidden collection option is included. The diagnostic counters and wait-count operation from the isolated prototype were not promoted.

## Verification and measured scope

Rust tests cover lease retirement, crashes/stops/restarts, source-event/overflow gates, corrupt/oversized data, generation/owner mismatches and setup/publication/invalidation failures. The endpoint regression retains a publisher reference after dropping the endpoint and proves its certificate cannot outlive the writer fence. `tests/live.py` uses real, stateful Node/.NET consumers, proves the first read is current before a control observer has accepted or replied, checks burst coalescing, checks crash/offline-edit recovery and verifies RPC-only operation when live-state setup is blocked. It adds no diagnostic native-library exports.

A deterministic interleaving regression completes an input, replaces the status page and clears the gate between the reader's last gate check and its file-identity checks. The retired page is rejected and the next read sees the replacement generation. After this ordering fix, Debug/Release Rust tests, the Release native live checks and the saved-file coordinator regression were repeated; the complete Debug script had passed before that final fix. Build, formatting and all-target Clippy logs are retained under `.artifacts/live/1794175698639990000/`.

The original isolated synthetic measurements and their limits are in [QUERY_PERFORMANCE.md](QUERY_PERFORMANCE.md#shared-live-freshness-prototype). They are evidence for that prototype, not an integrated-build real-encoding certification. No OS priority or host-GC change is required. Full API first-call/checkpoint tails, broader arrival patterns and resource/fairness limits remain separate performance concerns.

The accepted activity-notification path is integrated in the main core. [Both synthetic prototype timing series](QUERY_PERFORMANCE.md#nonblocking-activity-notification-prototype-2026-11-09) remain scoped to their source copy; sparse-query gains are repeatable there while first-query results vary. They do not certify the integrated build under real encoding. Main integration evidence and checks are retained under `.artifacts/ai/1794235886885493000/`. See [release readiness](RELEASE_PLAN.md) for the remaining 0.0.1 work.

During installer verification an optimized parallel regression exposed a retirement race: closing the publisher's lease descriptor alone can leave its `flock` alive in a duplicate open-file description, such as one briefly inherited by a concurrent process spawn before close-on-exec. The same failure was reproduced deterministically by retaining a cloned descriptor across publisher disable. Retirement now invalidates the existing input gate and explicitly unlocks the lease before closing it; readers recheck that gate with final identity/ownership checks. This changes no protocol/store/owner version and older readers still observe the explicit unlock. Reproduction and verification are retained under `.artifacts/installer/lease-reproduction.log` and the final integration logs.
