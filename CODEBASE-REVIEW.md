# Codebase Review — 2026-11-11

This is a multi-lens, read-only review of the whole repository. Nine parallel reviewers each covered one area:

- FFI/native-binding safety
- process/filesystem/IPC security
- coordinator concurrency
- SQLite storage/recovery
- query/graph performance
- the cursor subsystem
- MCP/CLI host surface
- language providers
- architecture/tech debt

Each reviewer checked its findings against the code. Items marked **✔ spot-checked** were checked again by hand when the findings were put together.

**Caveats**
- Another session was committing to `main` during the review (for example `coordinator.rs` grew from 2,932 to 3,430 lines), so line numbers may have drifted.
- Severity is the reviewers' judgement of impact × likelihood. It is not proof that a bug can be exploited or reproduced.
- Nothing here has been fixed. No full test suite was run.

Severity scale: **Critical** means data loss or a security break in default use; none were found. **High** means a crash, wedge, exploitable trust gap or measured large cost in plausible use. **Medium** means wrong or degraded results, or a narrower failure. **Low** means hardening, polish or minor inefficiency.

---

## Critical

None found.

---

## High

### H1. MCP: a 5th concurrent request tears down the whole session ✔ spot-checked
`crates/host/src/mcp_transport.rs:63-67, 475-477`
- **Problem:** When `register_request` fails at `MAX_PENDING_RESPONSES` (4), it calls `connection.fail()`, which cancels every in-flight query and closes stdio. The request should get a `busy` error instead.
- **Scenario:** An agent issues 5 parallel tool calls, or 4 calls plus a ping or `tools/list`. The session dies with "MCP connection closed". Agents that call tools in parallel will hit this routinely.

### H2. Coordinator: a `wait` racing `unregister` panics while holding the `entries` lock ✔ spot-checked
`crates/host/src/coordinator.rs:~2380` (`&entries[&request.database]` after `wait_timeout`)
- **Problem:** `unregister` can remove a *queued* entry while another client is blocked in `wait`. When the waiter wakes, the map index panics, which poisons `entries`. Every later `entries.lock().unwrap()` then panics too.
- **Scenario:** Client A waits on a queued workspace, client B unregisters it, and the coordinator crashes.

### H3. Coordinator: a job-thread panic wedges the scheduler forever
`crates/host/src/coordinator.rs:~1001-1056`, `crates/core/src/cursor/service.rs` (`publish_with`)
- **Problem:** There is no `catch_unwind` and no drop guard, so `Event::Finished` is never sent. `running`, `active_job` and `analysis_running` stay set, and no workspace is ever scheduled again.
- **Also:** A panic inside `publish_with` skips `pending_publications -= 1`, so `CursorService::stop` spins forever.

### H4. Node binding: a panic aborts the host process ✔ spot-checked
`bindings/node/native/src/lib.rs:112-132` (and every `#[napi]` fn)
- **Problem:** `compute` calls `query_json_reserved` without `catch_unwind`. napi-rs adds one only with `#[napi(catch_unwind)]`.
- **Effect:** Any `unwrap`, poisoned lock or out-of-bounds slice in core aborts the Node or MCP host. The C ABI does catch panics (`response_owned`), so the two bindings behave differently.

### H5. Automatic indexing runs repository-controlled code without personal opt-in
- **TypeScript** (`providers/typescript/worker.mjs:12-27`): `require.resolve('typescript')` from the project directory loads whatever `node_modules/typescript` is checked in or present in a parent directory. The 5.9 version check runs only after that code has executed.
- **C#** (`providers/dotnet/Program.cs:188`, `SdkMetadata.cs:166-176`): `dotnet restore` and `BuildManager` evaluation run MSBuild `Exec`/`InitialTargets`/`BeforeTargets="Restore"` hooks from `Directory.Build.*`. A repo-supplied SDK via `global.json` `sdk.paths` may be a further route.
- **Scenario:** A hostile repository cloned under a registered parent folder is auto-discovered and executes code at login, with no prompt.
- **Assessment:** Compiler fidelity may genuinely require the repository's TypeScript and MSBuild evaluation. Even so, this conflicts with the stated rule that only personal settings grant script/hook execution. It needs an explicit, documented trust decision: for example, auto-discovered repos stay untrusted until a personal grant.

### H6. Storage: missing index makes `reclaim` quadratic on every publish ✔ spot-checked
`crates/core/src/store.rs:274` (artifacts DELETE); schema at `store.rs:176/220`
- **Problem:** `unit_artifacts` has PK `(owner_generation, compilation_id, artifact_id)` but no index on `(owner_generation, artifact_id)`. Both the `NOT EXISTS` probe and the foreign-key check on each deleted artifact scan it.
- **Measured:** 40k artifacts × 40 compilations took **78 s wall** for one statement. `reclaim` runs inside every publish transaction, on Writer open and on cursor activate/release.
- **Knock-on:** A concurrent cursor release holds the write lock past the Writer's 2 s `busy_timeout`, so `stage()` fails with SQLITE_BUSY, which is reported as a storage failure.
- **Fix:** `CREATE INDEX … ON unit_artifacts(owner_generation, artifact_id)`. See also M14 and M15.

### H7. Cursor: `retire` `expect` panic permanently poisons the cursor service
`crates/core/src/cursor/service.rs:337` → `crates/core/src/cursor.rs:582`
- **Problem:** During `admit`, `borrow_controlled` runs `sweep_inner`. If the pin's BAD latch flips in that window, the sweep removes the new entry. The service then calls `retire(id)`, whose `expect("selected cursor")` panics while holding the `State` mutex.
- **Effect:** Pagination stays broken until the coordinator is restarted.

### H8. Cursor: one cleanup failure blocks index publication for every workspace
`crates/core/src/cursor.rs:523, 571-575`; `service.rs:314, 637-642`
- **Problem:** `sweep_inner` returns `Err` on the first failed `retire` (an unlink check, `check_directory`, or SQLite busy on release). It skips the remaining entries. Every `request_inner` sweeps first, and `prepare_selected` propagates the error with `?` in `publish_with`.
- **Effect:** Saved-file publication fails coordinator-wide, not only under disk pressure.

### H9. Providers: one bad project fails the whole language job
`providers/typescript/worker.mjs:29,59`, `analysis.mjs:34`; `providers/dotnet/Program.cs:173` → `SdkMetadata.cs:65`
- **Problem:** Any of these throws to the top level, so no TS or C# facts are published for the workspace until it is fixed:
  - one tsconfig with a JSON syntax error;
  - one package pinned to TS 5.8;
  - one `.csproj` that fails evaluation.
- **Fix:** Scope the failure to that compilation's coverage.

### H10. Graph: frontier nodes at max depth are fully expanded and charged
`crates/core/src/graph.rs:769, 803-837`
- **Problem:** Nodes popped at `distance == depth` still fetch their whole adjacency, run dispatch-proof SQL and charge `examined_edges`, and only then is every neighbor rejected. They also emit `unresolved_target` and `dispatch_specialization_unproven` boundaries for edges outside the requested depth.
- **Scenario:** `callers` at depth 1 on a method with 500 callers that each have 50 callers runs 501 adjacency queries and about 25k edge checks. That trips `edge_budget`/`time_budget` and returns spurious boundaries.
- **Fix:** A `LIMIT 1` existence probe is enough to set `depth_budget`.

### H11. Graph: relationship filters run after the SQL `LIMIT`, so the budget is spent on rejected rows
`crates/core/src/graph/dispatch.rs:183-195`, `graph.rs:445`
- **Problem:** `adjacency_sql` filters only by endpoint. Kind, `include_implicit` and `exact_only` are applied in Rust after `LIMIT remaining+1`, and every rejected row still costs budget. There is no `ORDER BY`, so which rows survive truncation depends on the query plan.
- **Scenario:** A member with 12k references and 5 calls exhausts the 10k edge budget before it reaches the calls.

---

## Medium

### Security / trust boundary
- **M1. Repo `.npmrc` / `pnpm-workspace.yaml` settings still apply during restore.** `providers/typescript/preparation.mjs:196-209`
  - Only scripts and pnpmfile are forced off. For example, `git=./tools/x.sh` together with a git dependency in the lockfile could run repository code. (Reasoned from npm's documented `git` setting; not tested.)
- **M2. Downloaded npm/pnpm integrity is circular.** `providers/typescript/package-tools.mjs:54-103`
  - The sha512 comes from the same registry response, with no signature or provenance check and no pinned hash.
  - Cache reuse re-hashes only the entry file, not the unpacked tree.
- **M3. Installer copies the shell PATH verbatim into the LaunchAgent plist.** `crates/host/src/install.rs:805-809`, `crates/core/src/update.rs:116-118`
  - Relative entries such as `.`, `node_modules/.bin` or empty are kept, and workers run with the workspace root as their working directory. A repo-committed `node` or `dotnet` binary can then win.
- **M4. Out-of-root source enters the index unlabeled.** `providers/typescript/analysis.mjs:53, 108-110`
  - A tsconfig `include: ["../../**"]` or a symlinked file pulls user files from outside the repo into the index, and MCP tools serve them to agents. This is partly by design, but the boundary is not labeled.

### Concurrency / liveness
- **M5. A CPU-throttle pause can hold the cursor mutex and the SQLite write lock together.** `cursor/service.rs:625-643` with `resource.rs:161-165`
  - Effects:
    - The main loop blocks in `cursors.sweep()`.
    - The 4096-slot event channel overflows, forcing full rescans.
    - Pages stall for every workspace.
  - Cursor-service lock-hold issues (head-of-line blocking, spurious `resource_limited`) are in M19.
- **M6. `Control::publish` holds `inner.state` across `tx.commit()` and fsync.** `resource.rs:204`
  - A cancel during a large commit holds `entries`, which blocks every `status`/`wait`.
- **M7. A daemonized worker descendant wedges the single analysis slot.** `update.rs:169, 265`
  - The read loops wait for pipe EOF. A child that leaves the process group (for example from an opted-in install script) keeps the pipes open, and even an explicit cancel cannot free the slot.
- **M8. `entries` is held across blocking calls.** `coordinator.rs:~282, 1986`; `recovery.rs:1053-1067`
  - `register_workspace`, `observe_bad` and `report_corruption` hold it across `flock`, `settings::save` and registry commits.
  - Registering during a replacement cutover therefore blocks all control RPCs.
- **M9. Native Node queries run on libuv's 4-thread pool.** `bindings/node/native/src/lib.rs:150, 201`
  - Four slow queries (up to 120 s each), or one `wait` plus three queries, starve the host's `fs`, `dns` and `crypto` work.
  - Workspace `wait` calls are not admission-controlled.

### MCP / CLI surface
- **M10. Each result is sent twice.** `crates/host/src/mcp.rs:195, 210-212`
  - `CallToolResult::structured` also copies the JSON into a text block. Wire size is 2–3× the core response, and `response_max_bytes` understates the real cost.
- **M11. Concurrent rmcp response sends can exhaust output capacity.** `mcp_transport.rs:140-153`
  - A third frame queued while two large frames are still draining kills the connection.
- **M12. `additionalProperties:false` is advertised but not enforced per tool.** `mcp.rs:259`, `query.rs:17-57`
  - A field that belongs to a different tool is silently ignored. For example, `find_references {"include_source":true}`.
- **M13. Bad requests produce no response or an unusable schema.**
  - Input lines over 64 KiB are dropped silently: no JSON-RPC error, and the client hangs (`mcp.rs:30`, rmcp `async_rw.rs:282`).
  - The `coverage_details` schema offers `{generation}` alone, which the core always rejects (`mcp.rs:336-339` vs `query.rs:650`).

### Storage
- **M14. Deleting `contents` scans `artifacts` once per deleted row.** `store.rs:277`
  - The FK child `artifacts.content_hash` has no index. Measured: 5k deleted rows against 40k artifacts took 4.9 s, with quadratic growth.
- **M15. No index on `generation_units(owner_generation, compilation_id)`.** `store.rs:176, 272`
  - Each compilation delete scans the table, which grows with retained generations and cursor pins.
- **M16. Staging WAL vs. disk pressure can livelock.** `store.rs:20-80`, `recovery.rs:1316`
  - During staging, `wal_autocheckpoint=0` and the Writer holds the lock, so nothing checkpoints. A large first index hits disk pressure, discards its work, re-analyzes and hits the limit again. (Medium confidence; the resume path was not traced end to end.)
- **M17. `validate_candidate` runs `integrity_check` under a fixed 120 s deadline.** `recovery.rs:974-985`
  - Under contention a multi-GB candidate gets interrupted, and that uses up the single automatic recovery attempt even though nothing is corrupt.

### Cursors
- **M18. One workspace's measurement error fails other workspaces' publication or admission.** `cursor.rs:470-475`
  - `budget_usage` opens every database with live cursors, and any error from one of them aborts the whole check.
- **M19. The coordinator-wide cursor mutex is held across commit, `reclaim` and 16 MiB SHA-256.** `service.rs:417, 622-643`
  - Every `next_page` waits behind it, and the wait ignores request deadlines.
  - `publication_waiting` is set unconditionally, so admissions fail with spurious `resource_limited` during relock windows.
- **M20. Frozen pages can become unreadable.** `pages.rs:206, 354, 748-758`; `client.rs:422`
  - Pages are packed with zero headroom. A larger current status at read time pushes a stored page over the same `response_max_bytes`, and at the 2 MiB cap the request cannot be retried.

### Graph / query performance
- **M21. No per-node adjacency memo.** `graph.rs:769`
  - Re-expansion per `(node, possible, context)` up to `paths_per_symbol` (8) re-runs the SQL and re-charges the budget, about 8× work.
- **M22. Graph `max_bytes` (1 MiB default) exceeds the 256 KiB response cap.** `graph.rs:514`, `query.rs:563`
  - Graph truncation can never trigger at defaults. Large impact results fail with `response_limit_exceeded` instead of returning a labeled partial result.
- **M23. Graph responses skip the relevant-coverage filter.** `query.rs:829`
  - Response size grows with the workspace, not with the result.
- **M24. Hot-path SQL is re-prepared without the statement cache.** `graph.rs:684, 699`, `testing.rs:93`, `grouping.rs:240`, `query.rs:981`
  - Uses `query_row` without `prepare_cached`, about 5 prepares per finding.
  - `references` runs an uncached `testing::origin` query per row even though the CTE already joins the mask.
- **M25. `source_line` streams the content blob from offset 0, twice per declaration.** `query.rs:1128, 1331`
  - This happens even when no source is requested.
- **M26. `symbols` materializes and sorts every match before `LIMIT`.** `query.rs:1480`
  - `search_symbols {}`, with no name or query, is also accepted (`mcp.rs:364`), which sorts the whole generation.

### Providers
- **M27. Each source file goes in a single ≤8 MiB frame.** `providers/shared/protocol.mjs:36`, `dotnet/Program.cs:102`
  - A generated file of about 8.5 MiB kills the whole workspace job. There is no chunking and no per-artifact "too large" gap.
- **M28. Self-recursive calls are dropped.** ✔ spot-checked `typescript/graph.mjs:7`, `dotnet/Graph.cs:55`
  - The `from == to` guard written for `contains` edges also drops calls, so `callers(f)` omits `f` calling itself. No recursion fixture exists.
- **M29. Implicit accessor read/write direction is wrong for some writes.** `dotnet/Graph.cs:186-189`, `typescript/graph.mjs:106-108`
  - Tuple and destructuring assignment and `??=` record only the getter.
- **M30. The Rust worker makes one serial `definition` round trip per semantic token and caches every external file for the job.** `rust/worker.mjs:151-182`
  - Experimental, but costly.
- **M31. TypeScript and Swift toolchain resolution climbs above the workspace root.** `typescript/worker.mjs:14-27`, `swift/toolchain.mjs:39-44`
  - `~/node_modules/typescript` gets reported as selected.
  - `~/.tool-versions` gets labeled `repository_pin`.

---

## Low

### Native / FFI
- **L1. Live/health pages are length-checked before `mmap`.** `crates/core/src/live.rs:434-456`, `recovery.rs:389-400`
  - A later truncation raises SIGBUS in the embedding host. Requires same-user tampering or corruption.
- **L2. Coordinator client socket is not atomically CLOEXEC.** `execution.rs:229-252`, `control.rs:673-680`
  - A host `fork` or `exec` in that window inherits the socket.
- **L3. Embedded NUL in a .NET `Index` path silently truncates it.** `bindings/dotnet/KnowCode/Index.cs:190-195`, `ffi/src/lib.rs:226-236`
  - `"a.db\0x"` opens `a.db`. Unpaired surrogates become U+FFFD.
- **L4. `know_code.h` has no include guard and no `extern "C"` block.** `bindings/ffi/know_code.h`

### Security hardening
- **L5. Unbounded `readFileSync` of repository-selected paths.** `preparation.mjs:14-23, 130-141`
  - A `.npmrc` with `pnpmfile=/dev/zero`, or a symlink to it, makes the worker read until it is killed, and this repeats on every reconcile.
- **L6. Runtime `dotnet restore` leaves MSBuild node reuse and the build server on.** `providers/dotnet/Program.cs:130-133`
  - It does not set `-nodeReuse:false` or `DOTNET_CLI_USE_MSBUILD_SERVER=0`, so nodes can outlive the worker and the resource caps.
  - `DOTNET_CLI_WORKLOAD_UPDATE_NOTIFY_DISABLE` is unset, which may cause network calls.
  - Note: the providers reviewer says the host sets `MSBUILDDISABLENODEREUSE` for the worker, which partly covers the first point. Verify before fixing.
- **L7. Existing directories are never tightened to 0700, and the socket has no `getpeereid` check.** `coordinator.rs:376-396`, `control.rs:478-482`
  - Applies to an existing data directory or `KNOW_CODE_DATA_DIR`.

### Concurrency
- **L8. The 2 s presence pass and job dispatch hold `entries` across per-workspace I/O.** `coordinator.rs:755-760, 933-970`
  - Lock hold time grows with the number of registered repositories.
- **L9. Recoverable errors exit `run()` and kill the coordinator.** `coordinator.rs:751, 933, 1261, 1918`
  - Covers registry, persist and slot errors. `persist_record` under a full disk is the most likely trigger.
- **L10. Connections are capped at 64 serve threads, each of which a `wait` can hold for 120 s.** `coordinator.rs:670`
  - Overflow silently drops `cancel`, `shutdown` and `resume`.
- **L11. `Governor::configure` holds the limits mutex across file I/O.** `resource.rs:334`

### Cursors
- **L12. An out-of-range position invalidates the shared cursor for every holder.** `client.rs:177-183`, `service.rs:465-476`
  - A typo or truncation in a token is enough.
- **L13. Startup reset hard-fails on any foreign entry and is never retried.** `result_store.rs:494-556`, `service.rs:129-138`
  - A `.DS_Store` or more than 4096 entries is enough to disable pagination for the coordinator's lifetime.
- **L14. A late deadline after `finish` orphans a tokenless cursor until idle expiry.** `service.rs:258, 437-451`
- **L15. Paged-query peak host memory is a multiple of 16 MiB.** `client.rs:367-396`, `pages.rs:360-406`
  - It is held as an encoded Vec, a `Value`, per-item Vecs and chunk copies.

### Storage
- **L16. `Writer::drop` runs a full `reclaim` with no interrupt or progress handler.** `store.rs:858-868`
  - Combined with H6, a cancel can hold the write lock for minutes.
- **L17. `symbol_test_mask` index is never usable.** `store.rs:409`
  - The only predicate is bitwise, so the index only adds write cost.
- **L18. `compilations(project_path)` has no index for incremental replacement.** `store.rs:432-434`

### Graph
- **L19. `depth_budget` is pushed once per rejected neighbor.** `graph.rs:837, 973`
  - The list can reach 100k entries, and it is scanned after every pop.
- **L20. `Contexts::load` reparses every unit's `references_json` on each graph query.** `graph.rs:572`
- **L21. `symbol_full` has no declaration `LIMIT`.** `query.rs:788`, `graph.rs:358`
  - The bounded path's `locations_limited` behavior is lost.

### MCP / CLI
- **L22. Schema clarity for agents.** `mcp.rs:226-335, 395`
  - `max_bytes` means three different budgets across tools.
  - Many params lack descriptions and defaults.
  - `query_graph` duplicates `callers`/`callees`/`impact`.
  - Errors use internal op names rather than tool names.
- **L23. `know-code query`/`queries` print `{"error":…}` but exit 0.** `crates/host/src/main.rs:79-81, 121`
- **L24. CLI polish.** `main.rs`
  - `--version` needs `HOME`.
  - Numeric parse errors don't name the flag.
  - Repeated flags silently take the last value.
  - Usage omits several commands, and there is no `--help`.
- **L25. Synchronous admission filesystem work runs on the single-threaded MCP runtime.** `mcp.rs:157`

### Providers
- **L26. Quadratic receive buffering and ignored backpressure.** `shared/jsonrpc.mjs:19, 67`
  - `Buffer.concat` runs on every chunk, and `stdin.write` backpressure is ignored.
- **L27. Any `obj` path segment is treated as generated.** `dotnet/Program.cs:321`
- **L28. C# diagnostic paths are absolute, unlike other providers.** `dotnet/Program.cs:305-306`
  - `diagnostics_truncated` also counts hidden and info diagnostics.
- **L29. Unresolved TS/C# name references are dropped without a coverage count.** `analysis.mjs:120`, `Program.cs:290`
  - The Rust worker does count them.
- **L30. Primary-constructor base calls produce no call edge.** `dotnet/Graph.cs:144-151`
  - For example, `class D(int x) : B(Compute(x))`.

---

## Architecture & tech debt (ranked by impact)

1. **Capability and version checks are hand-copied at about 20 call sites.** (High)
   - Where: `core/src/control.rs`, `cursor/client.rs`, `live.rs`, `host/src/runtime.rs`.
   - Each site checks a different subset, so a new capability can be silently ignored by an old peer.
   - Use one typed `Capabilities` value with `require(&[…])`.
2. **Version constants are re-typed across languages.** (High)
   - Query contract `7` appears in `Index.cs:120` and `index.mjs:11`. Protocol `7` appears in Rust, `protocol.mjs` and `Program.cs`. Two different `CONTROL_VERSION`s exist.
   - Neither SDK's Workspace control path checks a version.
   - Generate them from one source.
3. **`coordinator.rs` is a god-module.** (High)
   - About 3.4k lines; `run()` is about 845 lines.
   - `Entry` carries 10+ interacting booleans, which is the root shape behind R02-class bugs and H2/H3.
   - Split it into registry, a scheduler state machine (enum), disk-pause handling and the control server.
4. **The durable registry schema is inline host SQL with no `user_version` or migration ledger.** (High)
   - Where: `coordinator.rs:~485-520`, plus raw SQL in `discovery.rs`, `install.rs` and `runtime.rs`.
   - This is the data that most needs durability. Use a small versioned registry module in core.
5. **`recovery.rs` mixes the incarnation lifecycle with temporary-disk accounting.** (Medium)
   - It exposes about 30 free functions keyed by path.
   - Move disk accounting into its own module and put incarnations behind a typed handle.
6. **Legacy native query entry points remain in three layers.** (Medium)
   - Five core variants are mirrored in FFI and Node, and pre-flight checks are repeated.
   - Since broad backward compatibility isn't required yet, keep one reserved entry point.
7. **Query operations are plain strings.** (Medium)
   - Op lists are re-matched in 5 places.
   - The `120000` literal appears about 8 times despite `MAX_DEADLINE_MS`.
   - MCP schemas are hand-written.
   - Use an `Op` enum and named constants.
8. **AGENTS.md is a ~67 KB changelog.** (Medium)
   - It costs about 17k tokens per session and contradicts itself (storage "v6" vs code v8; query contracts 2, 3, 4, 6 and 7).
   - Keep standing rules and pointers; move status and evidence to `docs/RELEASE_PLAN.md` and the feature docs.
9. **Docs have drifted from the code.** (Medium)
   - `docs/IMPLEMENTATION.md` says format v6, query contract 6 and lifecycle 2; the code is at 8, 7 and 3.
   - Link to the constants instead of restating numbers.
10. **Test harness.** (Medium)
    - About 30 scripts each re-implement run-directory, coordinator and wait setup.
    - Fixed-sleep negative assertions (`resources.py:145, 228`, `coordinator.py:124, 133`) are flaky under the CPU contention the product treats as normal.
    - `csharp_classification.mjs` and `typescript_test_classification.mjs` are never run by `scripts/test.sh`.
11. **Provider duplication.** (Low)
    - The UTF-16→UTF-8 offset map, fact batching, `relative()` and diagnostic capping are duplicated across the Node workers; they belong in `providers/shared`.
12. **Host dependency duplication.** (Low)
    - The `rmcp =3.9.0` pin pulls a second sha2/digest, and there are two `syn` majors. Host only; the core surface is clean.

---

## What looked solid

- **Core dependency surface:** about 45 crates, with no MCP, tokio or notify in core. SQLite is pinned and bundled, and MCP tracing is compiled off.
- **.NET bindings:** SafeHandle, cancellation and reservation lifetimes are correct. The C ABI catches panics.
- **Single-use reservations:** a reservation can be submitted only once, enforced by a compare-and-swap in core.
- **Lock order:** consistent overall (discovery → entries → active_job → Control). Condvar waits loop on their predicates with deadlines, and clocks are monotonic.
- **Migrations:** each runs in a single IMMEDIATE transaction.
- **Checkpoints:** never block readers.
- **Corruption:** mapping is narrow (CORRUPT/NOTADB only).
- **Retired-family deletion:** allowlisted, so it never touches settings or registrations.
- **Cursor tokens:** 128-bit random, bound to an incarnation and workspace.
- **Result files:** `openat`/`O_NOFOLLOW` with uid, nlink and mode checks.
- **Cursor retirement:** files are removed before pins are released, and failed cleanup stays charged.
- **Settings trust:** `.know-code.json` cannot grant script execution (`deny_unknown_fields`, personal-only grant).
- **Install/rollback:** refuses symlinks and unmanaged plists.
- **Protocol frames:** bounds are checked before allocation in Rust and C#.
- **Worker stdout:** stays clean in every provider.
- **Definition links:** compiler-selected throughout, never matched by name.
- **Coordinate conversion:** UTF-16→UTF-8 is consistent for surrogates.
- **Budget enforcement:** per-row edge and time budgets plus the SQLite progress-handler deadline. The tests filter is applied before limits without pruning traversal.

## Suggested first fixes (cheap, high leverage)

1. **H6, M14, M15:** add the three missing indexes, in one migration.
2. **H1:** reply `busy` per request instead of failing the connection.
3. **H2, H3, H7:** replace `entries[&…]`/`expect` with fallible lookups, and add a job-thread drop guard so `Finished` is always sent.
4. **H4:** `#[napi(catch_unwind)]`.
5. **H10, H11:** skip expansion at the depth frontier, and push kind and implicit filters into SQL with a deterministic `ORDER BY`.
6. **M28:** limit the `from == to` guard to `contains` edges, and add a recursion fixture.
7. **H5:** record an explicit trust decision for repo-resolved toolchains and MSBuild evaluation in `docs/`.
