# Local development installation

The user explicitly approved activating the development coordinator on this machine. Its per-user LaunchAgent is installed, enabled, and running. This is a checkout-based development installation, not release packaging.

| Setting | Installed value |
| --- | --- |
| LaunchAgent label | `dev.bubstack.know-code` |
| User/domain | zakkeown, `gui/501` |
| Installed plist | `/Users/zakkeown/Library/LaunchAgents/dev.bubstack.know-code.plist` |
| Executable | `/Users/zakkeown/Code/bubstackdev/know-code/target/debug/know-code` |
| Arguments | `coordinator run` |
| Data root | `/Users/zakkeown/Library/Application Support/Know Code` |
| Lifecycle | RunAtLoad, KeepAlive, 10-second restart throttle |

The installed plist matches the [validated project artifact](../.artifacts/login/dev.bubstack.know-code.plist). Activation was verified through `launchctl print` reporting running and a successful coordinator status RPC. Socket permissions are 0600. At activation there were no registered workspaces; use live status rather than treating that initial count as current state.

```sh
target/debug/know-code coordinator status
target/debug/know-code watch /absolute/repository
```

Restart after rebuilding/updating the executable or changing worker/control compatibility:

```sh
launchctl kickstart -k gui/501/dev.bubstack.know-code
target/debug/know-code coordinator status
```

To stop/unload the managed service:

```sh
launchctl bootout gui/501/dev.bubstack.know-code
```

`coordinator stop` alone is not a durable stop for this service: KeepAlive restarts it. Removing the installed plist after unloading removes login startup; indexes/registry remain separate retained data. Do not disable/remove this service merely to run integration tests: tests use isolated data roots under `.artifacts/` and stop only their own coordinators.

The LaunchAgent supplies PATH entries for `~/.local/bin`, `~/.dotnet`, `~/.cargo/bin`, Homebrew/system bins, and disables .NET CLI telemetry. The Cargo path was added when restarting the updated service exposed a missing rust-analyzer executable during this checkout's own source indexing; no toolchain installation was needed. It adds no Know Code usage recorder or uploader. The binary and compiler workers still depend on this durable checkout and its built artifacts. See [coordinator behavior](COORDINATOR.md) and [implementation limits](IMPLEMENTATION.md).

Registered after activation: this Know Code checkout, `.research/repos/ts-pattern`, and `.research/repos/Polly`, each through explicit workspace registration. Clone revisions/setup and observed indexing coverage are recorded in [CORPUS_FINDINGS.md](CORPUS_FINDINGS.md) and the [corpus manifest](../.research/corpora.json). Use coordinator status for current progress; registration alone does not establish a usable or complete index.

The development coordinator now uses control/worker protocol v7 and writes store format v6. Compatible v2/v3/v4/v5 indexes remain queryable and migrate on their next write; old triples and missing graph capability reports do not become complete call graphs. Reference repositories were explicitly refreshed for the TS/C# graph slice. See [indexing measurements and migration behavior](INDEXING_PERFORMANCE.md); no registration or source removal was needed.

The TypeScript declaration-root/collection update is loaded too. The reference ts-pattern registration was explicitly refreshed to adopt new IDs; its generation 29 is an observation at this update, not a permanently current generation. Use live status for freshness. The previous LaunchAgent plist is retained under `.artifacts/login/previous-before-cargo-path.plist`.

The project-body update refreshed Polly (generation 10, 59/59 complete compiler reports, 10,260 definition pointers) and ts-pattern on the same registrations. A live Polly query followed linked bodies with current freshness and explicit traversal limits. Its CLI refresh wait expired before the background publication finished; this was not a failed/aborted publication. See [project-body validation](CALL_GRAPH.md#project-body-validation-closeout-2026-11-07) and `.artifacts/project-links/installed-summary.json`. Graph capability coverage remains partial.

The dispatch follow-on uses protocol v7/store v6 and refreshed TS (generation 33) and Polly (generation 11). Polly remains 59/59 compiler-complete; a live query returns a possible cross-project target with compiler-selected composition evidence. Graph capabilities remain partial. The coordinator also retains failed analysis through unchanged-input rescans and distinguishes a repaired input snapshot; a returning interrupted workspace must actually reanalyze before clearing that failure. See `.artifacts/dispatch/installed-summary.json` and [dispatch validation](CALL_GRAPH.md#dispatch-validation-closeout-2026-11-07).

The reader-planning/compact-evidence update is built in Debug/Release and loaded by a restart of the existing LaunchAgent. Protocol v7/store v6/native ABI v1 are unchanged; compiler facts do not need refreshing. TS generation 33 and Polly generation 11 remain available/current in the measured live queries, with all registrations idle/error-free. See [actual measurements, validation and search tradeoff](QUERY_PERFORMANCE.md#implementation-follow-on) and `.artifacts/query-optimization/1794091178124164000/installed-restart.json`.


The live-freshness integration is now loaded in this development service. Its plist, ProcessType, registrations, control protocol v7, store v6 and native ABI v1 are unchanged; private live-owner layout v3 is active. Optional state-page setup failures fall back to RPC, and no prototype counters/private operation were installed. Forty live native correctness calls match across Rust/.NET/Node with TS generation 33 and Polly generation 11 still current. This activation does not certify the real-encoding performance gate. See [current contract](LIVE_FRESHNESS.md) and `.artifacts/live/1794175698639990000/installed.json`.

The accepted activity-notification integration is loaded in the same development service. Its optional owner capability permits first/checkpoint reads without a scheduling reply when live freshness is valid; priority notifications are best effort under overload. The existing plist/ProcessType, registrations, protocol v7, store v6 and native ABI v1 remain unchanged. Eighty Rust/.NET/Node correctness calls match before/after reload, with TS generation 33 and Polly generation 11 current and no index rebuild. See `.artifacts/ai/1794235886885493000/installed.json` and [current contract](LIVE_FRESHNESS.md). This is a development installation, not a published package or real-encoding gate result.

The CLI/MCP development slice and common bounded source operator are now built in Debug/Release and loaded alongside the existing coordinator installation. Launch `know-code mcp --workspace PATH` from an agent to auto-register/index that explicit workspace; EOF leaves background indexing running. The installed LaunchAgent plist/ProcessType, store v6, worker/control v7 and native C ABI v1 are unchanged. Eighty native correctness calls pass before/after reload; real TS/Polly MCP searches and source excerpts match CLI. Source JSON now includes bounded excerpt coordinates/total length, with hash-required explicit ranges. See [workflow/limits](MCP.md) and `.artifacts/mcp/1794236901404139000/installed.json`. Agent-client setup and portable publication remain separate work.

The compact-context update is loaded in both profiles and this conversation discovers ten MCP tools. The LaunchAgent plist/ProcessType, store v6, worker/control v7 and native ABI v1 remain unchanged. New compiler declaration extents require reanalysis: the registered ts-pattern and Polly corpora were refreshed to generations 34 and 12, with current function-body results matching CLI/.NET/Node; this checkout served current Rust context at generation 95. These are observations, not fixed current generations. The unchanged project MCP configuration was briefly disabled/re-enabled through the supported local reload API to replace its cached old process, then restored byte-for-byte. See [contract, payload measurements and limits](SYMBOL_CONTEXT.md) and `.artifacts/context/1794240471405543000/validation.json`. The real-encoding gate remains open.

The native Workspace API update is loaded too. The LaunchAgent plist/ProcessType, store v6, control/worker v7 and C ABI v1 are unchanged; `kc_workspace_json` is an additive native export. Public .NET and Node clients open this checkout by repository root, read progress and return identical current symbol/context results without being given database paths. Installed generations remained ts-pattern 34, Polly 12 and this checkout 105, all idle/error-free at verification. See [workspace usage and remaining limits](WORKSPACE_API.md) and `.artifacts/workspace-api/1794243299585353000/installed.json`. Packaging, cancellation/resource limits and real-encoding acceptance remain separate release work.
