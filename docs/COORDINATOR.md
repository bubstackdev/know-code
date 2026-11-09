# Saved-state coordinator

Status: implemented development slice, tested on macOS/Apple Silicon. This is part of the agreed v0.0.1 design, not a complete release. See [implementation limits](IMPLEMENTATION.md).

## Operation

```sh
target/debug/know-code watch /absolute/repository
target/debug/know-code coordinator status
target/debug/know-code index /absolute/repository --configuration Release
target/debug/know-code unwatch /absolute/repository
target/debug/know-code coordinator stop
```

`watch` durably registers a workspace and starts the coordinator when necessary. `index` requests an explicit full refresh through that same coordinator. The client exits independently; the coordinator continues watching and indexing with zero consumers. `unwatch` removes registration/settings and stops monitoring; derived index files are currently retained for explicit future cleanup. `stop` drains the active job and exits. The development executable locates workers in its build checkout.

The default per-user data root is `~/Library/Application Support/Know Code/`. `KNOW_CODE_DATA_DIR` can isolate development/tests. The root contains `registry.db`, `coordinator.lock`, `c.sock`, and per-workspace indexes. The socket must fit macOS's Unix-socket path limit; a too-long override returns an actionable startup error. No socket/state is placed in temporary storage.

## Ownership and control

One coordinator owns each data root through an exclusive file lock. It alone schedules workspace-index writes; the existing workspace writer lock also excludes independent writers. Consumers use versioned Protobuf control messages over a Unix domain stream socket. Registration, refresh, status, bounded freshness waits, unregister, and shutdown are supported. Graph queries never execute on the coordinator.

The native .NET and Node bindings call the same Rust control client and then open their own SQLite read view. Control and worker transport share framing/codec infrastructure. Protocol v2 adds explicit source compilation ownership, dependency/input metadata, and coordinator messages; incompatible protocol versions return update-required errors. Control connections are capped at 64, incoming events at 4,096, and frames at 8 MiB. Overflow requests reconciliation rather than silently losing freshness information.

Registration, configuration, workspace identity, and saved-input manifests use the central SQLite registry with FULL synchronous mode. Rebuildable workspace indexes retain NORMAL synchronous mode. Root-directory/Git metadata identifies the registered workspace; a different directory/repository at the same path remains unavailable until explicitly unregistered/re-registered. Identity is checked again before publication. Missing roots retain their state and resume when the original workspace returns.

## Save to publication

1. A native FSEvents watcher reports changes. Short save bursts coalesce before scheduling. Atomic-save replacement paths are reconciled; build/dependency directories are excluded from ordinary source discovery.
2. The shared update path fingerprints saved inputs. Unchanged metadata can reuse hashes; explicitly reported paths are rehashed. Missing explicitly tracked inputs keep a sentinel so their return remains observable. Known compiler configuration/additional/linked inputs outside the repository are watched too.
3. Saved source changes select owning projects and the transitive closure of recorded project dependents. C# target variants remain distinct and all affected targets are analyzed. Unmapped new source conservatively expands the language scope; structural/configuration changes can require a full workspace refresh.
4. Providers emit facts through the existing bounded worker contract. Analysis runs outside write transactions. Completed units stage under a new owner generation; unchanged units are referenced through a small generation manifest, without copying their symbols, occurrences, or source payloads.
5. Successful worker completion and reports permit atomic publication. A before/after input comparison and observed event epochs preserve saves arriving during analysis. Older work cannot acknowledge newer saves. Failed work preserves the previous publication and does not retry unchanged input on every query.

Index format v4 preserves generation-to-compilation ownership and unit-to-source membership, and uses private integer keys in occurrence/relationship storage, adding per-site evidence and capability reports. Formats v1/v2/v3 migrate transactionally without changing existing public identities. Formats v2/v3 stay queryable through the same symbol/reference/source operators until the next write upgrades them; graph capabilities in old data remain unknown, and legacy relationship triples retain legacy_unknown evidence; migration attempts compaction without invalidating pinned readers. Old WAL readers keep their original facts/source while obsolete unreferenced units are reclaimed. Retained owner generations are current-state implementation details, not a searchable history archive.

The coordinator runs one analysis job at a time across workspaces. Recently queried/saved workspaces receive priority, with queue aging for background progress. Providers/targets execute sequentially within a job. C# roots can reuse shared compiler state while moving between active dependency closures inside that job; all retained compiler state is released at job completion. Numeric CPU/memory enforcement and across-job compiler-session reuse are not implemented yet.

## Freshness

Native queries validate eligible live state in-process using the common Rust path in [LIVE_FRESHNESS.md](LIVE_FRESHNESS.md). Capable producers accept coalesced, best-effort activity notifications without a reply wait, including first requests. Older peers retain first/checkpoint RPCs. Missing, pending or uncertain live state and failed notification submissions use control RPC, waiting up to 500 ms by default for the request's captured observed target. `freshness_wait_ms` is configurable up to 120,000; `deadline_ms`, when supplied, caps the freshness-control wait. A later save does not extend that captured wait target. Overall SQL execution deadlines remain future work.

Results distinguish `current`, `stale`, and `unverified`, with coordinator phase, observed/published epochs, generation, availability, and the last actionable error. Current means the served generation matches the coordinator's acknowledged observed inputs; it does not prove instantaneous observation of every concurrent filesystem write. Pending/failed/unavailable work serves a coherent prior view flagged stale. No usable prior view returns not-ready. Coordinator loss never turns a retained managed index into fresh data. Compiler diagnostics and partial coverage remain separate from freshness.

On restart the registry restores registrations. Saved inputs and the recovered index generation are reconciled before freshness is acknowledged. Unchanged healthy indexes do not rebuild merely because the coordinator restarted. Offline edits queue affected work; a registry acknowledgement that outlived lost index progress cannot establish freshness.

Saved-input manifests are stored separately from registry status rows and loaded for the active workspace, rather than retaining every repository's file map in idle memory. Idle entries retain status, scheduling metadata, and exceptional watched input paths. Queries do not scan source trees or probe toolchains. Active reconciliation still scans a workspace's relevant input files; this is not yet a highly tuned large-estate scheduler.

## Login startup

```sh
target/debug/know-code coordinator login-plist
```

This emits a reviewable per-user LaunchAgent plist with RunAtLoad, KeepAlive, a restart throttle, an explicit toolchain PATH, and the chosen data root. The generator does not install or activate the service. The user subsequently approved activating this machine’s development LaunchAgent; see [local installation](LOCAL_INSTALL.md). Tests do not install a login service. The generated plist references the development executable/checkout, so it is not final distribution packaging.

## Verification and remaining scope

`tests/coordinator.py` exercises saves without consumers, atomic saves, unchanged-unit reuse, C# dependent/target invalidation, bounded stale fallback, saves during analysis, failed-job preservation, missing/replaced/returning roots, offline edits, unchanged restart, one-owner exclusion, native parity, and plist generation. Test roots and databases remain under `.artifacts/cd/`; the test stops its coordinator on exit. Local socket/FSEvents tests require an environment that permits those OS operations.

Remaining work includes across-save compiler-native incremental sessions, automatic resource caps, parent-folder enrollment/exclusion configuration, broader toolchain/package-change detection, source body search, cursor lifecycle, corruption rebuilds, upgrade cutover, MCP, and release installation. This implementation does not establish estate-scale throughput, memory ceilings, or semantic-freshness latency guarantees. Reference for the native watcher: [notify documentation](https://docs.rs/notify/8.2.0/notify/).

## Dependency preparation and reconciliation

Workers report preparation outcomes and bounded progress through the existing protocol. Rust persists preparation checkpoints next to each workspace index and supplies them to later workers; package/MSBuild commands stay in adapters. Status/query coordination metadata exposes the current stage. Completed compiler analysis may publish partial coverage after a restore failure; failed or truncated worker streams preserve the prior generation.

A watcher rescan/overflow means rehash and reconcile saved inputs, including tracked external inputs. It does not mean force analysis or retry an unchanged failed restore. Only explicit refresh requests unconditional preparation retries; dependency/configuration fingerprints and missing successful outputs also drive preparation. Filtering ignored dependency/build-output paths before event queueing prevents install noise from feeding repeated work; the OS watcher callback never blocks on coordinator state. The preparation regression includes a 6,000-file event flood and verifies no replacement generation or restore for unchanged inputs.

Broad traversal exclusions do not override explicit compiler input ownership. Saved generated C# compile files under `obj`, analyzer configuration, and additional inputs reported by a provider are tracked directly and can invalidate their owning project/dependents. Virtual source-generator documents remain derived compilation output.

## Failed analysis and unchanged-input reconciliation

A no-op watcher rescan cannot certify a failed refresh. The coordinator retains the analysis error and a SHA-256 fingerprint of the failed saved-input inventory separately from temporary availability/watch errors. The existing central workspace registry adds `failed_input_hash`; no source contents or usage records are gathered by this state. The error/hash commit together with FULL registry durability. Successful semantic publication clears them.

If reconciliation sees the same failed inputs, it retains the failure and stale prior generation without retrying compiler analysis or dependency restoration. A repair can restore the exact last-good bytes, so comparison must use the failed input fingerprint as well as the last successful manifest: changed failed inputs trigger real reanalysis, even when they equal the successful manifest. Explicit refresh can retry unchanged inputs. A failed input snapshot that could not be captured remains unverified and cannot clear the error through the no-op success path.

Returning the same workspace resumes normal reconciliation. If analysis was interrupted/failed, its return schedules actual reanalysis before clearing the failure; a missing path or temporary watcher error alone does not become a durable analyzer failure. Compiler-error partial publication remains a successful, explicitly partial result and is distinct from analyzer/storage failure. Tests cover a broken compiler with unchanged saved files, no-op saves, restoring valid configuration bytes and repairing an interrupted workspace while it is unavailable.

## Contended lookup gate

The initial RPC-based encoding run missed 22/24 cells with current/correct results. Connection pooling and thread-only QoS trials were not adopted. An isolated Interactive-policy trial improved the RPC tail but left interference/freshness tradeoffs; the normal LaunchAgent policy remains unchanged.

The shared live-state path has now been integrated into the common core. Its private owner layout v3 retires certificates with the writer fence, makes setup/resource failures fall back to RPC and contains no prototype instrumentation. The accepted one-way activity path now avoids first/checkpoint reply waits with capable peers; activity delivery is best effort under overload, independently of freshness. Older peers retain RPC checkpoints. See [current contract and tests](LIVE_FRESHNESS.md) and [historical measurements and limitations](QUERY_PERFORMANCE.md#shared-live-freshness-prototype). The real-encoding performance gate is not certified by synthetic measurements.

## Resource policy and explicit indexing cancellation

The coordinator now uses the common-core [resource controller](RESOURCE_CONTROL.md) across its one shared analysis slot. Defaults are two CPU cores and 2 GiB with personal central overrides, sampled memory enforcement, CPU throttling of owned worker groups, and cancellable staged publication. `cancel` affects the workspace job; client disposal does not. Status adds `outcome_code` and `cancelling`/`cancelled`/`resource_limited` phases. Saved changes/refresh resume cancelled work; unchanged failures remain stopped. Settings changes retry resource-limited work without invalidating healthy facts. Resource/cancel capability version 1 is checked explicitly; older coordinators return update-required through new clients. The graph/store protocol remains compatible.
