# Query latency and memory investigation

Original investigation measured 2026-11-07. Product source and binaries were left unchanged during that investigation. Both changes have subsequently been implemented; see the [implementation follow-on](#implementation-follow-on). These observations are not shipping performance guarantees. All experiments live under `.artifacts/query-performance/`; no usage collector, host GC policy, or allocator hook was added to Know Code.

The original shared query path was inexpensive for many small graph requests, but two bounded 100-result impact queries take roughly 140–230 ms and leave roughly 98–110 MiB of physical footprint in a minimal Rust process after 30 calls. Isolated prototypes reduce those observations to roughly 10–18 ms and 12–17 MiB while preserving every response byte on all ten measured requests. Those trials motivated the two common-core changes now implemented and validated below.

## Reproducible evidence

The immutable measurement run is [1794075844642436000](../.artifacts/query-performance/1794075844642436000/). Start with its [summary](../.artifacts/query-performance/1794075844642436000/summary.json), [validation manifest](../.artifacts/query-performance/1794075844642436000/validation.json), [metadata](../.artifacts/query-performance/1794075844642436000/metadata.json), [exact requests](../.artifacts/query-performance/1794075844642436000/cases.json), and [dataset paths](../.artifacts/query-performance/1794075844642436000/datasets.json). Raw per-process results, samples, plans and stderr are under `observations/`; profiles are separate under `profiles/`. Snapshot databases, original harness sources, private mirrored core sources and frozen probe binaries remain in that same run directory. They are local artifacts, not required product dependencies or distributable benchmark fixtures.

The ordinary matrix contains **111 fresh processes and 3,192 API calls** across Rust, native Node and native .NET entry points. These totals exclude separate profiling, planning and memory prototypes. Snapshot-backed probes query the real production Rust core and native bindings. The private diagnostic binaries build copies of that core; they never replace the installed library or coordinator. The manifest pins all retained inputs/outputs and checks the previous dispatch manifest's 24 source files and eight product binaries are unchanged.

| Environment | Value |
| --- | --- |
| Machine | Apple M5 Max, 18 logical CPUs, 64 GiB unified memory |
| OS | macOS 27.2, build 26B123 |
| Query build | Release; pinned bundled SQLite 3.54.1, store v6 |
| Rust | rustc 1.99.1 (58cf8f9c3, 2026-10-13) |
| Node | v24.22.2 |
| .NET | SDK 10.0.403, runtime 10.0.14 |
| C# analysis configuration | Debug; distinct from the Release query build |
| Freshness | Immutable backup probes: unverified; managed live probes: current |

| Corpus | Git revision | Served generation | Snapshot bytes | Compiler coverage |
| --- | --- | ---: | ---: | --- |
| ts-pattern | f11be73acb18e1d653d4f252e7d75981d873a2e6 | 33 | 38,125,568 | Three complete, three partial contexts |
| Polly | 289f29808f70f49d3d46de57f42fcd1139bc2bd0 | 11 | 714,608,640 | 59/59 complete reports |

Graph capability reports remain explicitly partial in both corpora; compiler completeness is separate. Polly is multi-target and includes tests. The TS clone has an untracked dependency-restoration lockfile at `examples/one-file-demo/package-lock.json`; its tracked files are clean. Benchmarking does not edit source/configuration. SQLite's backup API captured the existing live indexes into durable local snapshots, preserving generation/source coherence. Snapshot file sizes are allocated database files, not live fact bytes.

## Measurement method and limits

- Two independent latency processes per host/request; 35 calls each, discard the first call plus four warmups, leaving **60 steady observations**. Host order rotates and case order is seeded. Tables give median and empirical p95; these samples do not establish tail SLOs.
- Rust times `query_json` through returned JSON text, excluding the harness's later response parsing. Node times public native `Index.query`, including its normal result conversion. .NET times typed `Index.QueryAsync`, including its normal JSON/PInvoke/result parsing. Host timings describe actual entry points, not identical serialization work or pure FFI overhead. Verification and JSONL reporting happen outside each timer.
- Warm OS/file/runtime caches are uncontrolled; the machine was not purged or rebooted. Each production query opens and releases its own read view/SQLite connection with the existing 2 MiB connection cache. Warm calls do not reuse one persistent database connection. A fresh process is not a cold-storage test.
- Default graph requests bound depth (one for callers/callees, five for impact), 10,000 examined edges, 2,000 nodes, 1 MiB output and 1,000 ms traversal work, with 100 findings. Exact request JSON is retained. Truncation and boundaries remain visible; returning 100 results is not exhaustive whole-graph traversal.
- Memory uses **separate** processes, 30 calls per host/request, retaining only the final response until an explicit release. It skips per-call response verification/reserialization. Physical footprint and RSS are sampled every 5 ms with macOS `proc_pid_rusage`, flavor 4, using the installed SDK's 296-byte `rusage_info_v4`. Reported query peak is the larger of sampled footprint and the kernel lifetime high-water available at query completion. Raw counters are retained. RSS is not interchangeable with physical footprint.
- Baseline is after host startup but before queries. Idle is 0.6 seconds after the final result is released; Node/.NET may retain runtime-managed memory. Neither host receives GC flags or forced collection. One memory process per cell is an observation, not a memory ceiling. Baseline includes the harness/runtime, and differences do not isolate pure core allocation.
- CPU counters remain raw alongside a calibration of about 24 million units per CPU second on this machine; their values must not be treated as nanoseconds. Profiling/instrumented diagnostic processes are excluded from ordinary latency/memory tables.
- The coordinator and three registrations are idle/error-free throughout the ordinary matrix, with unchanged generations and observed/published epochs. The coordinator's footprint moves from 7.594 to 7.625 MiB. There is no concurrent indexing in this query test.

## Original baseline: warm latency

Milliseconds, median / observed p95:

| Request | Rust JSON median / observed p95 | Node API median / observed p95 | .NET API median / observed p95 | Findings; reported limits |
| --- | ---: | ---: | ---: | --- |
| ts_symbol | 3.25 / 3.55 | 3.32 / 3.63 | 3.50 / 3.91 | 94; none |
| ts_match_callers | 134.59 / 138.77 | 136.53 / 141.04 | 136.75 / 142.65 | 100; depth_budget, result_limit |
| ts_match_callees | 1.71 / 2.11 | 1.80 / 2.09 | 1.70 / 2.14 | 1; none |
| ts_with_impact | 137.55 / 143.38 | 140.30 / 148.28 | 140.16 / 144.92 | 100; result_limit |
| polly_symbol | 46.58 / 47.59 | 46.82 / 48.22 | 47.66 / 50.33 | 21; none |
| polly_fanout_callers | 20.04 / 20.70 | 20.50 / 21.25 | 20.70 / 21.62 | 11; none |
| polly_dispatch_callees | 7.51 / 7.77 | 7.94 / 8.43 | 8.04 / 8.69 | 10; depth_budget |
| polly_dispatch_exact | 4.15 / 4.61 | 4.53 / 4.85 | 4.42 / 4.91 | 7; depth_budget |
| polly_impl_callers | 13.54 / 15.38 | 13.45 / 13.99 | 13.78 / 14.75 | 6; none |
| polly_impl_impact | 226.22 / 233.89 | 228.01 / 235.70 | 229.68 / 235.51 | 100; depth_budget, result_limit |

`ts_symbol` looks up `match` (94 symbols), and `polly_symbol` looks up `Guard` (21). The TS graph roots are the compiler Function declaration `match` and `with`, not similarly named properties. Polly cases cover a high-site-count callable, possible dispatch versus exact-only callees, and the implementation's reverse callers/impact. The original pilot's mistaken TS root was corrected before the full matrix; discarded pilot observations are not included.

The two impact queries examine 2,509 TS / 3,438 Polly edges. Possible-dispatch callees examine 168 edges and return ten findings versus 119 / seven for exact-only. Six reverse implementation callers require 24 examined edges. Complete per-case counts, limits, boundaries, coverage and generation are retained in the summary. Ordinary host/iteration results agree on those checks; the later private prototype additionally checks the full returned JSON hash.

Six fresh-process dispatch trials per host, with OS caches already warm, give median first-API-call times of **12.50 ms Rust, 11.36 ms Node, 14.06 ms .NET**. Their observed ranges are 11.51–13.22, 10.67–11.70 and 13.25–16.00 ms. Process-to-ready medians are 4.00, 34.17 and 44.20 ms, measured separately. An earlier setup pilot produced a roughly **402 ms first .NET call**; subsequent fresh processes were much faster. This is retained in `pilot2.log`, and neither series establishes cold-boot/runtime-first-use latency.

Managed live queries also exercise the real coordinator freshness path, with a 500 ms allowed freshness wait and no pending changes. Median TS callers are 134.56 / 139.39 / 139.61 ms (Rust/Node/.NET); Polly dispatch is 7.73 / 8.01 / 8.00 ms and impact 225.44 / 229.97 / 230.01 ms. All return **current** results. Their similarity to snapshot timings does not establish a precise IPC overhead; order/cache variation remains. These are not saved-change wait or indexing measurements.

## Original baseline: physical memory

MiB of physical footprint, separate processes, 30 repeated calls:

| Host | Idle before queries | Dispatch peak / after release | TS impact peak / after release | Polly impact peak / after release |
| --- | ---: | ---: | ---: | ---: |
| rust | 1.3 | 7.4 / 7.4 | 98.1 / 98.1 | 110.0 / 110.0 |
| node | 13.9 | 20.9 / 20.9 | 112.6 / 112.6 | 127.0 / 127.0 |
| dotnet | 26.6 | 36.3 / 36.5 | 131.6 / 131.8 | 145.8 / 146.0 |

The small dispatch path stays relatively light. Repeated impact traversals cause much larger resident growth, including in the minimal Rust process. This is not justified by the test machine's available RAM and is not a product resource budget.

A separate ordinary Rust TS-impact run reaches **195.70 MiB peak / 192.28 MiB idle after 100 calls**. Longer reader-planning diagnostic runs reach about 197 MiB peak / 193–194 MiB idle at 100 and 250 calls, suggesting a plateau under that workload. These observations do not prove an unbounded leak or establish a general bound.

Separate 20-call `vmmap`/`leaks`/`lsof` probes find no open snapshot descriptors after calls and zero leaked blocks/bytes in the inspected processes. Their live malloc allocations are modest (roughly 278 KiB TS and 1,046 KiB Polly) while tens of MiB remain dirty in **Malloc Large (empty)** regions. This supports allocator-retained temporary buffers as a major cause, rather than a retained graph or reader. It is not proof that no leak can exist elsewhere. A private post-measurement `malloc_zone_pressure_relief(NULL, 0)` experiment returns zero bytes and does not recover the footprint; it was never introduced into a product or embedding host.

## Diagnosis and isolated prototypes

### Repeated SQL planning

Three-second Apple `sample` profiles at 1 ms intervals show impact traversals spending much of their main-thread time beneath:

```text
dispatch::neighbors -> Rows::advance -> sqlite3_step
  -> sqlite3Reprepare -> sqlite3Prepare -> wherePathSolver / whereLoopInsert
```

One reprepare branch accounts for 1,526 of 2,248 TS main-thread samples, and 1,427 of 2,250 Polly samples. This is repeated planning during stepping, not merely one initial statement prepare. The structural query-plan gate still correctly rules out full relationship scans and adjacency materialization, but does not bound the planner's own cost. Neither snapshot has SQLite statistics tables; the pinned build enables STAT4. These facts alone do not establish the exact reprepare trigger.

The first private mirrored-core probe enables `SQLITE_DBCONFIG_ENABLE_QPSG` on the **reader connection only**, before preparing queries. This avoids parameter-sensitive plan changes in the trial. It does not alter the writer, store format, host allocator or GC. The off/on probe runs 12 calls per case, discards three, and compares SHA-256 of the entire returned JSON outside the timer. Every response on all ten requests is byte-identical. For example, TS impact moves from 139.93 to 10.06 ms, Polly impact 220.93 to 18.27 ms, and TS callers 131.96 to 7.71 ms. Memory retention is essentially unchanged.

This setting can trade away beneficial parameter-sensitive plans. The measured `Guard` lookup shows no improvement (45.05 to 47.55 ms in the small private trial); broad query distributions and all existing query-plan/behavior gates must be exercised before adoption. This is evidence for a focused implementation experiment, not a global SQLite-tuning prescription.

### Temporary edge representation

On this ARM64 build, Rust `Edge` occupies **1,144 bytes**, including large optional provenance records even when absent. A second private mirror boxes just four optional payloads: `dispatch_proof`, `definition_link`, `binding_definition_link`, and `dispatch_evidence`. The inline `Edge` falls to **400 bytes**. Existing evidence and JSON are preserved; no graph fields are dropped or cached between calls. This mirror keeps reader QPSG enabled.

Every returned JSON response on all ten requests again matches the first prototype byte-for-byte. The combined trial gives:

| Request | Current ordinary Rust median | QPSG + compact optional payloads median |
| --- | ---: | ---: |
| TS callers | 134.59 ms | 7.52 ms |
| TS impact | 137.55 ms | 9.57 ms |
| Polly high-site callers | 20.04 ms | 5.21 ms |
| Polly dispatch callees | 7.51 ms | 4.64 ms |
| Polly exact-only callees | 4.15 ms | 2.84 ms |
| Polly implementation callers | 13.54 ms | 4.34 ms |
| Polly implementation impact | 226.22 ms | 17.50 ms |
| Polly `Guard` symbol lookup | 46.58 ms | 47.33 ms |

| Rust-process workload | Current peak | Combined prototype peak |
| --- | ---: | ---: |
| TS impact, 30 calls | 98.11 MiB | 12.50 MiB |
| Polly impact, 30 calls | 110.02 MiB | 16.80 MiB |
| TS impact, 100 calls | 195.70 MiB | 12.36 MiB |

Prototype memory remains at the shown values after response release/idle. Small variations between independent processes are expected. These prototype latency figures have only nine steady samples per request in one process, and the memory figures one process per workload. They are directional trials against fixed snapshots, not a controlled shipping before/after or a guarantee for arbitrary graphs. Only the ordinary baseline is measured through all three hosts. The Rust field-type change also needs review even though serialized responses and native ABI shapes are unchanged in the prototype.

### Separate symbol lookup follow-up

Polly's `Guard` lookup remains about 47 ms. Its independent profile is dominated by B-tree traversal and `pread`; this does not prove physical cold-disk latency. `profiles/symbol-plan.json`, obtained with the actual bundled SQLite build, shows symbol lookup constrained by generation/compilation, but not by name, plus temporary ordering. The existing `(?2 IS NULL OR s.name=?2)` predicate prevents a focused exact-name seek in that plan. A later separate exact-name query shape is worth testing; no production predicate/index change was made in this investigation.

## Implementation gates identified by the investigation

Implement the two demonstrated changes inside the existing shared Rust reader/graph path, then rerun the actual Debug/Release core, graph, dispatch, project-link, native SDK, pagination, generation/update, formatting and clippy checks. Preserve all evidence, target/context compatibility, budget accounting, ordering and source-generation behavior. Rerun equivalent real-corpus requests through Rust/Node/.NET, including memory after repeated calls. Retain the original snapshot/baseline binaries for a balanced comparison. Expand query-plan checks to parameter distributions that could regress with QPSG, including exact symbols, target filters and uncommon adjacency sizes.

Do not add a global allocator, host-GC policy, persistent full-graph cache, new query engine or dependency solely to address these observations. The compact optional payloads and reader planning choice are narrow common-core experiments. Byte equality on ten fixed requests is a useful guard, not a substitute for product regression coverage or independent correctness evidence.

This report does not establish hundreds-of-repository behavior, concurrent-query admission, resource-cap enforcement, cold-storage tails, arbitrary high-fanout traversal, historical queries, Swift/Rust performance, or release acceptance. It does not change proposed performance goals in [PERFORMANCE.md](PERFORMANCE.md).

## Local rerun notes

The explicit measurement tools are private durable artifacts, not automatic product telemetry. Ordinary probe sources are frozen in the run's `harness-sources/`; diagnostic sources in `diagnostic-sources/`; combined prototype sources are frozen in the run's `boxed-sources/` (the working mirror remains under `.artifacts/query-performance/diagnostic-boxed/`). The validation manifest pins them. Full snapshot hashes tie reruns to the measured generations. Source clones are not changed or reindexed by a query rerun.

From the repository, with user-scoped Rust/Node/.NET on PATH:

```sh
python3 .artifacts/query-performance/report.py
```

This recomputes tables/summary from retained JSON and checks diagnostic response hashes without issuing queries. To collect new timings, select a new durable run directory and copy/pin snapshots, cases, environment, harness sources and binaries first; keep the baseline run immutable. `controller.py` implements the bounded child-process protocol, `matrix.py` the ordinary workloads, `diagnostic.py` and `boxed.py` the isolated comparisons, `retention.py` longer repetitions, and `profile.py` separate instrumented runs. Inspect their pinned paths before reuse. macOS process inspection and coordinator Unix sockets may require supported sandbox escalation. Never merge instrumented timings into the ordinary series or infer cold-cache performance from a fresh child process.

## Implementation follow-on

The approved changes now live in the shared Rust core: `ReadView::open` enables SQLite QPSG on each reader before statement preparation, and four large optional `Edge` evidence payloads use `Box`. No dependencies, compiler adapters, store/protocol versions, writer settings or native ABI changed. Serialized evidence remains intact. Direct Rust consumers constructing these public fields now supply `Box`; the Node/.NET and JSON contracts retain their shapes. The actual Release Rust probe reports **400-byte edges**, down from 1,144. No global allocator hook or embedding-host GC policy was added.

Retained [implementation summary](../.artifacts/query-optimization/1794091178124164000/summary.json), [checks](../.artifacts/query-optimization/1794091178124164000/checks.json), [validation manifest](../.artifacts/query-optimization/1794091178124164000/validation.json), and [three-file patch](../.artifacts/query-optimization/1794091178124164000/changes.patch) are separate from the original research run. The original snapshots/baseline binaries remain pinned.

### Validation and operating conditions

Debug/Release builds, both Rust workspace test profiles, formatting and all-target Clippy with warnings denied pass. The complete Debug script passed every step before its final four-language integration hit its 120-second initial-analysis deadline; that unchanged integration subsequently passed alone, including all saved-update/error-recovery checks. Release native/compiler, coordinator, call-graph, project-link and dispatch suites pass. Its first coordinator attempt hit the 45-second SDK-setup deadline during overlap with Debug compiler work; the unchanged isolated rerun passed. Original failure logs remain retained; the full Debug script's nonzero exit is not rewritten as a successful run. No test deadlines were raised.

Process inspection found no leftover Know Code compilers after cleanup. Other video encoders were consuming substantial CPU; system load was above 60 on 18 logical CPUs during diagnosis. This is a likely contributor to the setup failures, not a proven exclusive cause. During the later ordinary query matrix, load averages move from **26.24 / 39.32 / 44.76** to **24.36 / 34.41 / 42.16**. No Know Code compiler tests/indexing overlap that timed matrix. Absolute timings differ from the earlier prototype and must not be read as an equally controlled hardware-only before/after. No unrelated processes were stopped.

The strengthened existing SQL-plan regression uses a real read view and verifies both base/proof endpoint directions still avoid full relationship scans, automatic adjacency indexes and temporary ordering. Rebinding generation/key/LIMIT parameters causes **zero statement reprepares**. A private control with the old reader configuration fails that assertion with **three reprepares**, demonstrating the gate detects the defect. Existing publication tests preserve pinned readers/source through updates and migration. Generation-pinned cursor pagination remains unimplemented and is not claimed validated.

All ten original real-corpus responses match their old complete JSON hashes; native Node/.NET results equal the Release Rust result. Expanded checks cover **33 requests** with byte-identical old/new responses: exact/missing names, browsing 1/1,000 rows, FTS, combined name/text filters, absent targets, real net10/netstandard roots, exact/implicit/kind filtering, and edge/node/byte/depth/result limits. Six new filter cases additionally receive full native SDK parity checks. These fixed-request comparisons complement fixture regression coverage; they do not prove arbitrary semantic correctness.

### Implemented performance

The ordinary matrix repeats the original protocol: **111 processes, 3,192 API calls**, 60 steady latency samples per host/request, separate 30-call memory processes, fresh-process dispatch trials and managed live queries. Requests use the identical generation-33 TS / generation-11 Polly snapshots. Findings, boundaries, coverage and work limits match the original matrix. All managed live results are **current**. Snapshot results remain **unverified**.

Milliseconds, median / empirical p95, on the busy machine:

| Request | Rust median / p95 | Node median / p95 | .NET median / p95 |
| --- | ---: | ---: | ---: |
| ts_symbol | 6.85 / 8.02 | 6.85 / 9.54 | 6.51 / 8.73 |
| ts_match_callers | 15.04 / 38.47 | 15.50 / 18.99 | 15.65 / 26.12 |
| ts_match_callees | 1.84 / 4.37 | 1.82 / 2.19 | 1.84 / 3.68 |
| ts_with_impact | 17.95 / 21.63 | 18.84 / 23.56 | 18.15 / 26.95 |
| polly_symbol | 104.82 / 210.31 | 97.85 / 123.43 | 101.80 / 159.59 |
| polly_fanout_callers | 8.51 / 17.00 | 9.45 / 14.39 | 9.20 / 11.01 |
| polly_dispatch_callees | 5.37 / 5.90 | 6.13 / 9.96 | 5.77 / 8.18 |
| polly_dispatch_exact | 5.04 / 6.07 | 5.14 / 11.02 | 4.96 / 6.08 |
| polly_impl_callers | 6.35 / 7.27 | 6.50 / 9.31 | 6.10 / 7.26 |
| polly_impl_impact | 38.25 / 61.76 | 31.93 / 41.86 | 40.29 / 51.80 |

The smaller paired comparison alternates old/new process order by case, uses eight calls per process with three discarded, and holds the snapshot/request fixed. The old binary is the frozen mirrored-core diagnostic with its planning flag disabled; the new binary uses the actual updated core. Full response hashes match. Its five steady observations per side are directional comparisons, not an SLO:

| Request | Old median | Implemented median |
| --- | ---: | ---: |
| TS callers | 218.97 ms | 12.41 ms |
| TS impact | 227.39 ms | 14.22 ms |
| Polly impact | 425.02 ms | 29.34 ms |
| Polly `Guard` symbols | 101.50 ms | 94.62 ms |

These paired impact observations are roughly **14–16 times faster**. A combined TS exact-name/FTS filter showed a small cost: a focused three-pair follow-up (90 steady samples per side) measures **3.51 ms old / 4.17 ms implemented**, about 0.67 ms more. QPSG trades parameter-sensitive plans for stability; it does not improve every query. This tradeoff is retained explicitly. Exact-name/text query shaping is the recommended next optimization, including the remaining expensive Polly lookup. No search predicate/index change is included here.

Physical footprint in MiB, one fresh process per cell, peak through 30 repeated impact calls:

| Host | TS original / implemented | Polly original / implemented |
| --- | ---: | ---: |
| rust | 98.11 / 13.59 | 110.02 / 19.78 |
| node | 112.61 / 29.88 | 126.97 / 38.25 |
| dotnet | 131.59 / 43.92 | 145.84 / 57.95 |

The original/implemented memory columns are separate runs, not allocator-only accounting. Host/runtime baselines and final idle values remain in the summary. At 100 calls, separate ordinary Rust processes peak/idle at **16.33 MiB TS / 21.41 MiB Polly**. This is a large reduction from the original repeated-call footprint, not proof of a universal memory ceiling or a plateau at every repetition count. The coordinator footprint moves from **9.63 to 8.23 MiB** across the matrix; no whole graph is retained there.

The existing development LaunchAgent was restarted after checks. TS and Polly remain at generations 33 and 11; this checkout is generation 62 at the measured closeout. All three registrations are available, idle and error-free. Managed live queries retain current freshness. No compiler-fact refresh/index rebuild was required. Public-build telemetry policy, automatic resource-cap implementation limits and the remaining release scope are unchanged.

## Symbol and text lookup follow-on

Exact-name, FTS and combined symbol queries now select a small set of fixed SQL shapes in the shared Rust `symbols` operator. Every caller value remains a bound parameter. Name, target and text semantics, source locations, coverage, ordering, truncation and native contracts are unchanged. No index, schema/protocol version, dependency, compiler adapter, ranking policy or tokenizer change is required. Existing indexes remain usable.

The initial experiment showed that removing nullable `OR` predicates alone was insufficient: SQLite still reordered joins to scan each compilation's symbol index slice. The implemented join order is deliberate:

- Exact names: visible generation units → symbols using the existing `(generation,name)` index → owning compilations. An additional text predicate checks FTS against each candidate rowid.
- Text only: FTS postings → symbol primary rowid → visible generation/compilation ownership → compilation metadata.
- Browsing: existing ordinary joins, with an explicit target predicate when supplied.

The joins retain both owner generation and compilation identity. FTS postings from a hidden generation cannot enter the result merely because they match text. `ORDER BY name,id,compilation_id` and the extra limit row retain existing stable ordering/truncation. Broad text matches still require work proportional to the match set and sorting; this is not a universal constant-time query guarantee.

Artifacts: [summary](../.artifacts/symbol-search/1794102821693350000/summary.json), [exact requests](../.artifacts/symbol-search/1794102821693350000/cases.json), [checks including failures](../.artifacts/symbol-search/1794102821693350000/checks.json), [validation manifest](../.artifacts/symbol-search/1794102821693350000/validation.json), [review patch](../.artifacts/symbol-search/1794102821693350000/changes.patch), [plans emitted by the actual tested helper](../.artifacts/symbol-search/1794102821693350000/tested-plans.json), and [work counters](../.artifacts/symbol-search/1794102821693350000/work-counters.json). Original/current full responses, baseline binaries and measurement sources are retained in that durable run.

### Execution work

Measured using the same pinned SQLite build and identical Polly snapshot/parameters. These counts cover the symbol-selection statement, excluding coverage loading, declaration hydration, serialization and internal FTS execution. Fullscan-step counters are zero before and after: an indexed compilation slice can still read far too many entries. The useful distinction is constrained name/rowid lookup, not merely the word `INDEX` in a plan.

| Filter | Rows | SQL VM steps before | SQL VM steps after |
| --- | ---: | ---: | ---: |
| Exact name | 21 | 1,531,907 | 8,761 |
| Text | 42 | 1,685,834 | 2,331 |
| Name + text | 21 | 1,685,456 | 8,888 |

### Paired native API observations

Two process rounds per case/host/version, 20 calls per process, first five discarded: **30 steady observations** per cell. Old/new order reverses in the second round. Rust, Node and typed .NET use the same respective driver across versions; API serialization differences between runtimes remain, so this is not a host ranking. Startup is excluded from the API timers. The old core includes the preceding QPSG/compact-edge changes. Both versions query the same immutable TS generation-33 and Polly generation-11 databases.

There are **84 processes / 1,800 API calls**, including separate 30-call memory probes. The machine is heavily contended by unrelated video encoding. Recorded load averages move from **183.70 / 207.20 / 220.61** to **177.01 / 197.21 / 213.71** on 18 logical CPUs. Scheduling/I/O variation is substantial; medians, empirical tails and raw samples are retained, but these are not quiet-machine SLOs or portable speedup guarantees. No unrelated jobs were stopped and no host GC flags/forced collection were used.

Milliseconds, observed median before → after:

| Request | Rust before → after | Node before → after | .NET before → after |
| --- | ---: | ---: | ---: |
| ts_symbol | 5.83 → 2.49 | 6.41 → 3.00 | 7.97 → 2.53 |
| ts_fts | 5.61 → 1.85 | 7.27 → 2.51 | 3.91 → 2.82 |
| ts_name_and_fts | 3.25 → 3.21 | 3.35 → 3.75 | 4.30 → 4.43 |
| polly_symbol | 194.19 → 1.90 | 624.37 → 2.21 | 182.81 → 2.26 |
| polly_fts | 43.81 → 2.01 | 66.71 → 3.25 | 41.46 → 2.97 |
| polly_name_and_fts | 37.81 → 2.53 | 105.66 → 2.97 | 56.20 → 3.92 |

The large Polly exact/text/combined paths improve substantially. TS exact-name/text paths improve too; its combined name/text path is roughly unchanged. Node's combined TS sample is about 0.4 ms slower, and .NET's about 0.13 ms slower in this run. The earlier small combined-filter cost is not claimed universally eliminated. Exact candidates with FTS point checks avoid materializing a potentially large global posting list, but repeated FTS setup can cost time for many same-named candidates. Adaptive planning/cardinality heuristics have not been added.

Physical footprint in MiB, separate processes, peak through 30 repeated queries:

| Query / runtime | Before | After |
| --- | ---: | ---: |
| TS name + text, rust | 6.50 | 5.30 |
| TS name + text, node | 19.71 | 19.16 |
| TS name + text, dotnet | 34.67 | 33.27 |
| Polly exact, rust | 6.69 | 5.61 |
| Polly exact, node | 20.21 | 18.56 |
| Polly exact, dotnet | 37.13 | 33.13 |

These are whole-process observations including the driver/runtime, not isolated core allocation or resource ceilings. Memory does not increase in these measured cells; broader workload/cap enforcement remains separate.

### Validation and remaining integration gap

Debug and Release builds pass. Debug core tests and the Release Rust workspace pass; formatting and all-target Clippy with warnings denied pass. The search-plan gate checks all eight name/text/target-presence combinations, requiring name-index lookup, FTS-driven primary-rowid lookup or FTS rowid point checks as appropriate. The existing incremental-publication regression additionally verifies FTS across reused owner generations, old pinned readers, target intersections, ordering and truncation.

**55 controlled probes** preserve the old Rust response bytes, with full parsed response equality through real native Node/.NET APIs. They cover all filter combinations, missing/empty names, Unicode, quoted literals, invalid blank text, row limits, target filters, compiler coverage/provenance, and existing graph operators. The first captured default-budget net10 impact baseline hit its one-second work budget under load; immediate old/current reruns produced the same complete work counts. For deterministic parity checks, graph requests then explicitly used the existing ten-second work-budget option. The original requests/results remain retained. No product work/freshness default changed; graph execution itself is untouched.

**The full compiler suite is not green in this run.** `bash scripts/test.sh` stops during `e2e.py` initial indexing, before search assertions, because the coordinator does not settle inside the existing setup wait. One isolated retry reaches the same setup timeout. Earlier framing, TypeScript/toolchain and SDK-metadata checks completed. Setup deadlines were not raised and remaining full-suite steps are not marked passed. This SQL change does not execute in compiler preparation, and system load is extreme; load is a likely contributor, not a proven exclusive cause. A full compiler-suite rerun remains required before treating this as complete release integration validation; expected workstation contention must be covered as well as an isolating quiet baseline.

At live closeout, the registered TS and Polly indexes still serve generations 33 and 11 with **current** freshness. Rust, native Node and native .NET return equal exact-name results (94 TS `match` symbols / 21 Polly `Guard` symbols). All three workspace registrations are available, idle and error-free; this checkout's observed generation is 63. Native consumers load the updated query library; query-plan improvements do not require compiler-fact refreshes or index rebuilds. See [live evidence](../.artifacts/symbol-search/1794102821693350000/live-closeout.json).

### Workload clarification

After these measurements, the user explicitly confirmed that heavy concurrent video encoding is a normal development workload. The observed contention is relevant product evidence. A quiet rerun alone cannot close the integration gap for this supported scenario. The existing failures and narrower passing checks remain as recorded; no timeout, performance goal or scheduler behavior changed with this clarification. See [normal workstation contention gates](PERFORMANCE.md#normal-workstation-contention). The user subsequently selected query responsiveness over indexing freshness under contention, with slower ongoing background progress and honest stale metadata. Numeric contended-operation goals and enforcement mechanics remain undecided; this clarification changes requirements, not the measured implementation or failed-test outcomes.

## Accepted current-index lookup gate: first run

The accepted first gate is **p95 <=100 ms** for warm focused lookups returning at most 100 results on the named TS/Polly fixtures during encoding. It is a measurement target, not a changed runtime timeout. See [scope and method](PERFORMANCE.md#accepted-first-lookup-gate).

The first managed run is an **observed failure: 22 of 24 query/runtime cells miss the gate**. All 1,200 calls, including predeclared warmups, return current freshness and the expected generation, row count and truncation. There is no stale fallback counted as a fast pass. Reference fixtures use real callable symbols with nonempty compiler-backed references; each returns the requested 100 rows. TypeScript callable kind tags are `Function`, while the C# adapter uses `method`; fixture selection preserves those provider tags.

Eight queries (exact name, text, combined name/text and references in each language), three runtimes, two process rounds, 25 calls per process, first five discarded give **40 steady samples per cell**, 48 processes total. Empirical p95 uses nearest rank `ceil(0.95*n)` separately per query/runtime. Eight SVT AV1 encoding processes are observed active before the run; exact encoding inputs/settings remain unpinned. Results are local evidence rather than release certification. The old snapshot run's 148 ms Node tail remains retained; the managed path reveals misses across all three runtimes.

Artifacts: [gate result](../.artifacts/lookup-gate/1794107110655840000/gate.json), [method/workload](../.artifacts/lookup-gate/1794107110655840000/method.json), [requests](../.artifacts/lookup-gate/1794107110655840000/cases.json), [raw results](../.artifacts/lookup-gate/1794107110655840000/results.json), [phase summaries](../.artifacts/lookup-gate/1794107110655840000/phase-summary.json), [validation manifest](../.artifacts/lookup-gate/1794107110655840000/validation.json). Measured sources/binaries and individual process samples remain in the same durable run. All benchmark entry points use the existing production core; no performance policy, product code or diagnostic collector was added.

Milliseconds, observed p95; bold misses exceed 100 ms:

| Query | Rust p95 | Node p95 | .NET p95 |
| --- | ---: | ---: | ---: |
| ts_exact | 132.51 **miss** | 140.82 **miss** | 192.81 **miss** |
| ts_text | 163.79 **miss** | 154.05 **miss** | 158.35 **miss** |
| ts_combined | 229.04 **miss** | 111.50 **miss** | 156.07 **miss** |
| ts_references | 144.42 **miss** | 141.66 **miss** | 153.68 **miss** |
| polly_exact | 137.58 **miss** | 154.13 **miss** | 195.70 **miss** |
| polly_text | 158.17 **miss** | 25.53 | 176.32 **miss** |
| polly_combined | 201.43 **miss** | 89.33 | 110.10 **miss** |
| polly_references | 113.24 **miss** | 152.10 **miss** | 128.85 **miss** |

### Common-path diagnosis

A separate read-only Rust diagnostic times the same current-index control check and existing `ReadView` operators, with 40 steady samples for each exact-name case. This instrumented diagnostic is kept separate from the native gate. It does not serve user results or replace freshness certification. Its result encoding omits the coordinator attachment, so it is not byte-for-byte full-API timing. Phase percentiles are not additive.

| Diagnostic phase | TS p95 | Polly p95 |
| --- | ---: | ---: |
| Freshness RPC | 118.76 | 87.59 |
| Open read view | 2.15 | 1.54 |
| Execute query | 3.91 | 2.25 |
| Close view | 0.36 | 0.19 |
| Encode result | 0.07 | 0.07 |
| Diagnostic total | 122.07 | 90.19 |

The freshness round trip dominates the measured TS tail while query execution stays below 4 ms in these phase probes. The later Polly phase window is faster; that variation does not erase the earlier managed-gate failures. The common control path is the next place to investigate rather than treating this as a Node-only problem.

The current Rust client creates one Unix stream per request, and the coordinator accepts it on a new handler thread serving one request. A proposed next experiment is a **small bounded pool of persistent control connections shared across workspaces in each embedding process**, reusing the existing framed Protobuf path. Continue to obtain fresh coordinator evidence for each query. Do not cache a prior current status, weaken generation matching, skip the coordinator or change the 500 ms freshness policy to meet the latency target.

Connection reuse is a hypothesis, not an implemented fix or proof that thread creation alone causes the delay. An experiment must also inspect control-handler scheduling/lock contention and cover idle expiry, disconnect/restart, incompatible peers, concurrent waits and client limits. It must retain in-process queries and bounded resources without multiplying pools per repository. The compiler-setup integration failures remain a separate unresolved gap.


## Bounded control-connection reuse experiment: not adopted

On November 8, 2026, the authorized prototype retained one fresh coordinator exchange per query, using additive explicitly acknowledged keep-alive flags on protocol v7. A shared Rust client pool held at most four idle sockets across all roots, expired them after one second, and never held its mutex during I/O. Only read-only freshness waits could reconnect once after a cached-channel I/O failure, discarding partial frames and clipping the remaining wait budget. The coordinator served negotiated frames on one handler with a three-second idle timeout; old peers stayed one-shot. Existing 64-client and frame limits remained. Storage, queries, source facts, generation checks, native ABI, dependencies, and freshness defaults did not change.

**Decision: preserve the experiment, restore the simpler one-shot implementation.** The measured latency benefit is insufficient to justify the additional pool, retry, negotiation and lifecycle code. This rejects this particular prototype as an optimization; it does not establish that connection reuse can never help. The accepted 100 ms gate remains unmet.

### Paired managed lookup results

Durable run: [control-reuse evidence](../.artifacts/control-reuse/1794156665200579000/). Old and prototype clients used the **same prototype coordinator**, with old clients taking its one-shot path. The same eight requests read live TS generation 33 / Polly generation 11. Rotated host order and reversed second-round order provided 96 processes and **2,400 API calls**, with five discarded warmups per 25-call process: 40 steady observations per query/runtime/version. Direct Rust JSON, native Node, and typed .NET APIs were measured. Every call, including warmups, returned current status, the expected generation, row count and truncation. These checks do not establish full response-payload equality.

Three FFmpeg encoders were observed active before and after; encoding inputs/settings and OS scheduling were not pinned. Load averages moved from 74.08 / 80.97 / 81.61 to 68.71 / 77.67 / 80.29. No other applications were stopped or reprioritized. The previous gate's 22 misses occurred in a different, more heavily contended window; comparing 22 directly with this experiment's two misses would falsely attribute the improvement to reuse.

Milliseconds, empirical p95, **one-shot / reuse**; bold values miss the 100 ms gate:

| Query | Rust | Node | .NET |
|---|---:|---:|---:|
| ts exact | 43.97 / 78.25 | 77.97 / 69.73 | 74.12 / 91.30 |
| ts text | 81.45 / 80.58 | 8.53 / 71.62 | 12.50 / 9.12 |
| ts combined | 19.49 / **126.01** | 92.50 / 96.67 | **157.36** / 86.72 |
| ts references | 95.77 / 76.63 | 82.63 / 90.95 | **119.58** / 90.97 |
| polly exact | 41.98 / 74.29 | **181.12** / 94.72 | 78.77 / 81.25 |
| polly text | 5.41 / 78.30 | 82.82 / **134.04** | 91.73 / 7.83 |
| polly combined | 93.62 / 78.02 | 34.13 / 4.58 | 8.34 / 42.76 |
| polly references | 66.31 / 69.78 | 13.26 / 18.24 | 8.50 / 24.70 |

The paired one-shot path missed **3/24 cells**, reuse **2/24**. Improvements and regressions occur in different cells; two rounds under varying contention do not establish a dependable tail-latency improvement. Summed API wall time over all calls was **27.542 s / 27.473 s**; summed consumer CPU was **5.768 s / 5.709 s**. Summed wall time is descriptive only, not a pooled p95 or a release gate.

A separate read-only TS exact-name CPU trial alternated one-shot/reuse/reuse/one-shot, 200 calls per process. For 400 calls per version, coordinator CPU fell **0.0516 s to 0.0268 s**, while client CPU rose **0.8486 s to 0.9231 s**. Combined CPU was **0.9002 s / 0.9499 s**. Server footprint stayed approximately 7 MiB. The roughly halved coordinator cost is small in absolute terms and did not lower total observed CPU. Server accounting spans the child-process measurement interval, including its short holds; it is not request-level tracing. Raw macOS counters were calibrated at about 24 million units per CPU second. This separate trial is not included in the gate samples.

### Verification and retained work

Prototype Debug/Release builds, Debug/Release Rust workspace tests, formatting, all-target Clippy with warnings denied, and compiler/native integration passed. Nine added socket tests covered acknowledged reuse/one-shot fallback, fresh response changes, partial-channel discard, remaining budgets, non-retryable errors, shared bounds/expiry, concurrent waits, state-lock release between frames, clean idle EOF, stopping, and incompatible peers. Actual old-client/new-server and new-client/old-server negotiation were also exercised. These tests live with the preserved prototype rather than adding unused implementation or validation to the restored core.

The full Debug suite **did not pass**: it stopped at the coordinator fixture's existing 45-second initial-registration deadline during npm preparation. An isolated retry stopped at that deadline during configured TS analysis. Generation 1 had published, but newer observed inputs were still pending. Subsequent suites were not run by that stopped script. No test deadlines were raised and these setup failures remain unresolved under the concurrent workload.

The run retains before/prototype source, patches, compiled hosts, native modules, harnesses, raw observations, negotiation checks, logs and SHA-256 manifests. `prototype-sources/` is experimental evidence, not active product code. The first restoration attempt preserved older source timestamps and Cargo reused prototype artifacts; a validation run exposed that cached test binary (socket tests failed in the restricted sandbox). Those logs are explicitly retained as `cached-restored-*`. Sources were then timestamp-invalidated and actually rebuilt. See `validation.json` and `restored-checks.json` for final restoration checks. The development LaunchAgent is returned to the simpler path after healthy-index reconciliation. Final Debug/Release recompilation, Rust workspace tests, formatting and all-target Clippy pass. A prototype-capable probe confirms the restored server does not acknowledge reuse. All eight cases return current expected results with full response equality across freshly built Rust, Node and .NET hosts (24 calls); these are correctness checks, not another timing-gate run.

The next useful diagnostic is to separate client scheduling, accept-to-handler scheduling, lock acquisition, handler execution and response wakeup under encoding. The phase trace identifies the control round trip, not which of those waits dominates. Keep such instrumentation outside ordinary gate timings; do not add pools/executors, raise process priority, cache freshness, or weaken generation matching without evidence.


## Control-path scheduling trace: synthetic contention

On November 8, 2026, the user authorized tracing scheduling and lock waits in the freshness RPC. The real encoders had finished; the user explicitly selected bounded synthetic CPU contention for this diagnosis. **This is not a new encoding-gate run or a performance fix.** It does not supersede the failed real-encoding gate. The evidence points toward control-path scheduling/wakeup delays in the measured workload, rather than the workspace-state mutex or computation inside the handler.

Durable evidence: [control traces and validation](../.artifacts/ct/1794160061265145000/). All instrumentation lives in an ignored diagnostic source copy under that run. Root product sources, dependencies, protocol v7/store v6/native ABI v1, defaults and source facts are unchanged. The existing development LaunchAgent temporarily ran a pinned diagnostic executable, then returned to its exact previous binary. Registrations and live indexes were retained. No QoS, niceness, process priority, compiler-resource policy or unrelated application was changed.

### Method and instrumentation sensitivity

Client and server events use macOS `clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW)` for a shared nanosecond timebase, and `CLOCK_THREAD_CPUTIME_ID` for thread CPU. Relevant SDK definitions are retained in `sdk-clock-extract.txt`; these nanosecond values differ from the earlier raw `proc_pid_rusage` counters. Existing envelope job IDs correlate one request with one handler; no schema field was added. The instrumented shared Rust control client sends the same per-query freshness request and preserves socket timeouts, wait policy, generation checks and stale behavior. The Rust diagnostic driver calls the real `query_json` entry point; there is no alternate graph/query engine.

Events cover connect, request write, accept, pre-spawn, handler entry, request read/decode, state-mutex acquisition, condition-variable waits, response construction/write and client read/decode completion. A `try_lock` before the ordinary blocking acquisition records actual contention. Time between pre-spawn and handler entry includes thread-creation work and scheduling; it does not isolate those two costs. Client completion includes socket reading/decoding and scheduling, rather than proving an exact kernel wakeup time.

The first trace wrote each completed server record to a file. Under load, later one-shot controls differed considerably from earlier instrumented phases. A baseline/traced/traced/baseline repetition also gave the unmodified server a roughly 108 ms RPC tail, showing the large delays were not unique to instrumentation. This does not establish zero observer effect. To reduce it, the final collector stores at most **4,096 records** in a preallocated in-memory vector and flushes between timed phases through a diagnostic-only control operation. There is no per-request server trace-file write in that final series. Flush asserts that no records were dropped. The source copy and binaries for both collectors are retained; the diagnostic operation is absent from the product.

Synthetic load is one process with **36 runnable arithmetic threads**, each checking a shared two-minute termination deadline. Three separate bounded runs were stopped after their measurement phases; final verification confirms all three processes are gone. No encoder was started, stopped or reprioritized. The final load uses a ten-second ramp, with baseline A / traced A / traced B / baseline B ordering on two exact-name cases. Server transitions retain the same LaunchAgent environment and reconcile saved inputs before queries start. Existing root data and sources are used; startup/restore is outside the timed phases. The load's few-MiB resident size and CPU/process observations are retained alongside each phase. This runnable-thread load does not recreate an encoder's instruction mix, I/O, thread groups or scheduling classification.

Across the initial series and two sensitivity checks, **3,840 Rust API calls** returned current freshness, the expected served generation, row count and truncation. The initial eight cases include exact/text/combined/reference queries. The final contention series focuses on TS `match` (94 rows, generation 33) and Polly `Guard` (21 rows, generation 11): two rounds of 60 calls per case per phase, discard five per round, **220 steady calls per phase**, 110 per case. Its idle series retains all eight cases, 400 steady calls. Timing uses Debug coordinator and Debug Rust client; it does not measure .NET/Node async queues or replace their Release native-API gates.

### Final buffered trace

Milliseconds, empirical p95, pooled within each diagnostic phase. These are phase distributions, not per-query/native acceptance results; percentiles are not additive.

| Phase | Idle | Synthetic A | Synthetic B |
|---|---:|---:|---:|
| Rust API, Debug diagnostic client | 7.07946 | 126.80737 | 112.84321 |
| Freshness RPC | 0.15908 | 107.76346 | 74.40054 |
| Request written to accept (clipped) | 0.01554 | 74.89825 | 68.14250 |
| Before spawn to handler start | 0.05350 | 35.13150 | 11.39742 |
| Workspace-state mutex acquisition | 0.00067 | 0.00196 | 0.00208 |
| Handler thread CPU | 0.05862 | 0.11338 | 0.11567 |
| Response write start to client read/decode completion | 0.03213 | 14.32433 | 21.54533 |

The final unmodified-server control phases, using the same instrumented client under synthetic load, have RPC p95 **45.02 ms / 77.85 ms** and maxima **152.52 ms / 158.61 ms**. The traced phases have RPC maxima **182.13 ms / 181.51 ms**. The spread and different phase populations prevent treating the tables as an unbiased before/after optimization measurement. Removing per-request server file writes does not remove scheduling variability or all observer effects.

For the **440 steady buffered contention calls**, the state mutex was actually contended **4 times**; no call entered the freshness condition-variable wait. These are already-current indexes with no pending analysis, not measurements of lock behavior while indexing or across hundreds of repositories. Mutex acquisition p95 is about **2 microseconds**, and handler CPU p95 about **0.11 ms**, while wall-clock RPC tails reach tens to hundreds of milliseconds.

A non-overlapping partition of total observed RPC wall time assigns approximately **64.0% before accept**, **15.1% from pre-spawn to handler entry**, and **14.0% after server write completion to client read/decode completion**. State-mutex acquisition accounts for approximately **0.05%**. This is a descriptive sum of elapsed intervals, not a percentile, an OS scheduling trace, or a precise CPU utilization attribution. Server events are clipped between client request-write completion and client response-read completion so overlapping client/server activity is not counted twice. The partition sums exactly to each observed RPC; raw timestamps and derived intervals are retained in `joined.json`.

One current TS lookup spent **182.13 ms** in control: **178.30 ms** elapsed after its request was written before the listener accepted it, then **3.57 ms** from pre-spawn to handler entry. The handler consumed **0.087 ms** CPU and acquired the state mutex in **0.0012 ms**. This is evidence against solving that sample by changing symbol SQL or the state-lock implementation. It is consistent with delayed scheduling/wakeup/dispatch at the control boundary; distinguishing kernel scheduling, thread creation and process-policy effects needs a narrower follow-up. It does not prove the exact cause of prior real-encoding tails.

### Restoration, limits and next experiment

Final verification matches the four product source hashes and both Debug/Release executable hashes to their pre-experiment values. The service is running, all three registrations are idle/error-free, and 24 final native queries give current expected results with full Rust/.NET/Node response equality. Both diagnostic builds pass with the pinned offline Rust dependencies. The unchanged product's full integration suite was not rerun; its earlier coordinator-setup timeout remains unresolved. No release gate or compiler-completeness claim is added.

The next proposed experiment is to **inspect and test macOS QoS hints only for the control listener and request handlers**. Record their requested/effective scheduling class before assuming the default is wrong. Keep graph queries in-process and preserve fresh evidence on every query; do not infer that a pool or broader priority increase is needed. Evaluate latency together with competing-work throughput and indexing progress/fairness. Adoption still needs real-encoding native-API evidence. This is a proposal, not an accepted or installed scheduling change.


## Control-thread QoS trial: not adopted

On November 8, 2026, the user authorized testing macOS interactive scheduling hints on the control listener/handlers together with their impact on competing work. **The thread-only hint is not adopted.** It successfully changed the requested QoS class, but observed Mach scheduling priority and maximum stayed at 20. Query latency and competing arithmetic throughput did not improve in the measured sequence. The exact baseline service is restored; product sources, dependencies, index facts and settings remain unchanged. The real-encoding gate remains unmet.

Evidence: [QoS trial](../.artifacts/cq/1794161154481776000/). There were no real encoders running, so this uses the previously user-authorized bounded synthetic load. The trial is a diagnostic comparison, not native Release encoding certification.

### Requested class versus observed scheduling priority

Only the isolated source copy calls `pthread_set_qos_class_self_np(QOS_CLASS_USER_INITIATED, 0)`, at listener entry and handler entry. This is the **user initiated** class for short requested work, not the stronger UI-interaction class. Both sides retain the same buffered traces and observation calls. `pthread_get_qos_class_np` records the requested class; public SDK documentation does not define that result as the effective class after overrides. `thread_info(THREAD_EXTENDED_INFO)` independently records `pth_curpri`, `pth_priority` and `pth_maxpriority`. These observed priority values are not substituted for an effective QoS-class API.

| Role | Unhinted requested class | Hinted requested class | Observed priority / maximum |
|---|---|---|---|
| Coordinator main | Utility (`0x11`) | Utility (`0x11`), no setter call | 20 / 20 |
| Control listener | Default (`0x15`) | User initiated (`0x19`) | 20 / 20 |
| Control handler | Default (`0x15`) | User initiated (`0x19`) | 20 / 20 |

All snapshot API calls succeeded. The setter succeeded for the hinted listener and **524 observed handlers**, but every recorded current/priority/maximum value stayed at 20. New handler threads were observed requesting Default even after their creator requested User initiated, so changing a creator/main thread alone must not be assumed to classify newly spawned threads or child processes. No QoS calls were added to embedding hosts or compiler workers.

The installed LaunchAgent omits `ProcessType`. The local `launchd.plist(5)` manual documents that unspecified/Standard jobs receive light CPU and I/O resource limits, while Interactive removes those process-wide limits. This is **consistent with** the observed priority ceiling; the trial did not change the job policy and therefore does not establish that policy as the exclusive cause. Requested QoS alone is not evidence of an effective CPU-priority increase.

### Latency and competing-work measurements

An observe A / hint A / hint B / observe B sequence ran under one 36-thread arithmetic load process with a **120-second self-termination deadline** and a ten-second ramp. Each query batch contains two rounds of 60 exact-name calls per TS/Polly case; five per case/round are discarded from timing statistics. Each phase runs complete batches for at least ten seconds, capped at eight batches, so call counts differ. Build, startup and flush work occur outside the timed query phases. Both coordinator and diagnostic Rust API client use Debug; the client calls the existing full `query_json` path and preserves freshness/socket limits.

The arithmetic workers update one atomic progress counter per thread per 5,000 iterations. A separate monitor emits cumulative counts every 250 ms. Competing-work throughput uses the first/last samples wholly inside each query phase; sample times and boundary offsets are retained. This is a small arithmetic workload, not video encoding, compiler indexing, a fixed-rate arrival workload or a complete interference benchmark. The QPS columns describe this sequential driver; they are not service saturation throughput.

Milliseconds, empirical p95 pooled within the phase, not a per-native-query acceptance result. Competing throughput is millions of arithmetic iterations per second.

| Phase | Calls / steady | Rust API p95 | RPC p95 | Queries/s | Competing M iterations/s |
|---|---:|---:|---:|---:|---:|
| observe-a | 480 / 440 | 78.25 | 69.92 | 41.92 | 1,944.9 |
| hint-a | 240 / 220 | 108.54 | 91.28 | 20.08 | 1,561.9 |
| hint-b | 240 / 220 | 156.21 | 145.04 | 13.59 | 1,165.0 |
| observe-b | 480 / 440 | 89.25 | 79.22 | 27.21 | 2,033.7 |

All **1,440 measured queries** returned current freshness, expected generation, row counts and truncation: TS generation 33 / 94 `match` rows, Polly generation 11 / 21 `Guard` rows. The traced state-lock/condition-variable evidence and per-case statistics remain in `joined.json` / `summary.json`.

The hinted phases have higher observed latency and lower competing-work progress in this sequence. One ordered trial under varying system conditions does not establish a causal regression or a precise effect size. It provides no performance/interference basis for adopting the hint, especially while its observed CPU priority is unchanged. No test or product freshness deadlines were raised to improve these numbers.

### Diagnostic failure, restoration and validation

The first attempt completed 480 observer queries but its **untimed diagnostic flush** exceeded the driver's 500 ms control-response timeout. Cleanup stopped that load process and restored the original service; the incomplete trace and logs are retained under `attempt-1/` and excluded from the table. The final collector batches flush writes with `BufWriter` and the diagnostic-only flush request permits ten seconds. Timed graph-query freshness/socket policy is unchanged. The successful sequence completed under its hard load deadline, then stopped its own load process and restored the exact executable.

Both isolated, pinned offline builds pass. Final verification confirms original product source hashes and Debug/Release executable hashes, all three registered workspaces idle/error-free, and 24 current native queries with full response equality across Rust/.NET/Node. There is no shipping collector, extra dependency or installed scheduling policy. The unchanged product's full suite was not rerun; the prior coordinator-setup timeout is still unresolved.

### Next architecture decision

A job-policy experiment is broader than a control-thread hint: `ProcessType=Interactive` removes limits for the **whole coordinator process**, potentially affecting indexing and descendants. A [reviewable, uninstalled plist](../.artifacts/cq/1794161154481776000/proposal/interactive-test.plist) adds only that key. Testing it responsibly requires explicit background policy for indexing/worker processes and verification of their actual inherited/effective settings; a utility request on the main thread alone is insufficient evidence.

Do not silently switch the production job to Interactive. Adaptive is documented around XPC message activity; `xpc_transaction_begin/end` counts activity but does not by itself establish a priority donation from the existing Unix socket. Do not replace the accepted socket transport or add a second transport to obtain a scheduling boost without an architecture decision. A scoped job-policy experiment, with throughput/freshness/fairness checks and automatic restoration, is proposed rather than accepted or installed.


## LaunchAgent policy with explicit background analysis: experimental only

The user authorized a broader process-policy experiment after the thread-only hint remained capped. **Interactive job policy lets the control hint take effect**, but it is not installed in the normal service. At fixed 10 queries/s the measured freshness RPC p95 drops from about 90 ms to below 0.4 ms. Competing arithmetic progress is also lower in the Interactive phases, and saved-analysis publication was not observed until after the load ended. These tradeoffs and the lack of real-encoding validation do not justify automatic adoption.

Evidence: [isolated job-policy trial](../.artifacts/lp/1794165402943418000/). A separately named test LaunchAgent uses an isolated registry and APFS-cloned immutable generation-33 TS / generation-11 Polly indexes. Its registry paths point only to those copies. Startup reconciles the saved inputs. The normal LaunchAgent, plist, executable, settings, source repositories and indexes are never replaced or reconfigured. The test job is unloaded on exit. Artifact data roots are test isolation, not a change to the product's central per-user layout.

### Explicit separation and actual observations

The same diagnostic binary runs under Standard and Interactive test plists; only ProcessType differs. The listener and handlers request User initiated. Index-owned threads (coordinator main loop, analysis threads and stderr drainers) enter Darwin thread-background mode. Compiler launches in the **one shared Rust update path** run through `/usr/sbin/taskpolicy -b -c background`, which backgrounds and clamps the process, including descendants. No language adapter gets separate scheduling logic. This is an experimental use of a macOS utility, not an adopted prerequisite or resource-budget implementation.

| Role | Standard test job | Interactive test job |
|---|---|---|
| Listener/handler requested class | User initiated (`0x19`) | User initiated (`0x19`) |
| Listener/handler observed priority / maximum | 20 / 20 | 37 / 63 |
| Main/index thread observed priority | 4, Darwin background flag set | 4, Darwin background flag set |
| Sampled compiler/package/SDK-child thread priority / maximum | 4 / 4 | 4 / 4 |

The main loop enters background mode after creating the listener/control thread. This avoids assuming that a background creator can later grant its children a working QoS override. Control threads assert that their Darwin background flag is clear before requesting QoS. The initial preflight caught an incompatible sequence: applying Darwin background mode then requesting a QoS class returns EPERM. That failed test job was unloaded before timing; logs remain in `attempt-1/`. The corrected index-owned path uses background mode alone. These APIs/observations must not be conflated with a public effective-QoS-class getter.

Both saved-fixture preflights completed with semantic publication. Across preflight and the later contention check, **28 distinct descendant processes** and **11,708 child-thread observations** were sampled, covering Node, npm, Roslyn/.NET and SDK helpers. Every captured child priority and maximum is 4; no sample buffer was truncated. This verifies the sampled processes and interval, not every future compiler subprocess, resource cap or estate-scale fairness property. Framework-owned coordinator threads can still have their framework's scheduling policy; no claim is made that every thread in the process is priority 4.

### Contention comparisons

All phases use Debug coordinator/diagnostic Rust client, the same full Rust query path, buffered records flushed outside timing, existing freshness/socket limits, and 36 arithmetic load threads with a hard two-minute deadline. No real encoders were running. Startup and configuration transitions occur outside query phases. The first Standard A / Interactive A / Interactive B / Standard B sequence runs complete query batches for at least ten seconds, subject to the retained batch bound. Its two exact-name cases have the same expected positive findings as before.

Empirical p95 in milliseconds, pooled within diagnostic phase; worker throughput is millions of arithmetic iterations/s using interior progress samples. These are not the native Release encoding gate.

| Saturated phase | Calls / steady | Rust API p95 | RPC p95 | Competing M iterations/s |
|---|---:|---:|---:|---:|
| observe-a | 480 / 440 | 109.64 | 92.454 | 1,938.4 |
| hint-a | 480 / 440 | 69.26 | 30.094 | 1,173.8 |
| hint-b | 720 / 660 | 87.34 | 24.469 | 1,692.4 |
| observe-b | 720 / 660 | 63.68 | 24.892 | 1,747.4 |

Because a faster serial query loop can create more work, the follow-up holds the TS exact-name request stream to a nominal **10 requests/s**, 150 sequential calls per phase, first five discarded. All four phases complete at approximately 10.05 calls/s. The pacing serializes requests and can catch up after a late call; it is not an open-loop concurrent-arrival or queueing-latency benchmark.

| Paced phase | Calls / steady | Rust API p95 | RPC p95 | Competing M iterations/s |
|---|---:|---:|---:|---:|
| paced-observe-a | 150 / 145 | 103.11 | 92.426 | 2,092.7 |
| paced-hint-a | 150 / 145 | 13.48 | 0.381 | 1,437.7 |
| paced-hint-b | 150 / 145 | 91.22 | 0.354 | 1,856.0 |
| paced-observe-b | 150 / 145 | 96.57 | 90.279 | 2,109.5 |

The paced RPC improvement is consistent across both Interactive windows. Full API latency still varies because it includes SQLite/result work and caller scheduling. Competing arithmetic throughput is about **31% lower** in the first Interactive window and **12% lower** in the second than their corresponding Standard windows. This remains one short ordered sequence under changing system conditions, not a proven causal slowdown or a prediction of encoder performance. It does establish that the current evidence cannot claim interference-free responsiveness. The 3,000 comparative calls all return current freshness, expected generation/counts/truncation. Source/graph compiler-completeness claims are unchanged.

### Saved changes and background progress

A separate Interactive run saves both owned TS and C# inputs under a one-minute 36-thread load while queries continue against the unchanged corpus copies. Worker/SDK process transitions and analysis-stage changes demonstrate progress. **600 corpus queries** remain current with expected results, and the owned fixture advances from generation 3 to 4. Its last under-load status sample is still analyzing; publication is confirmed afterward. Because status is checked between six-second query batches, this does **not** pinpoint whether publication occurred just before or after the exact load termination. `published_while_load_running=false` means it was not observed published while the load process was alive, not proof of an exact cutover timestamp or absolute starvation. Completion was confirmed roughly 59 seconds after the save. The long lag remains material even though eventual coherent publication succeeds.

This is an intentionally small compiler/native fixture, including ordinary partial compiler inputs inherited from the prior e2e corpus, not a compiler-completeness or large-estate acceptance test. Background priority/clamping is not the agreed future automatic CPU/memory budget, nor proof that a hundred queued repositories make fair progress.

### Validation and disposition

Both offline pinned builds pass. Product source and Debug/Release executable hashes and the normal installed plist match their pre-experiment values. All test load processes are gone and the isolated LaunchAgent is unloaded. The normal service remains running and healthy. **24 actual native queries against the experimental Interactive service**, followed by 24 against the normal service, return current expected data with full Rust/.NET/Node response equality within each service context. These are correctness checks, not a native timing-gate rerun. The unchanged product full suite was not rerun; the earlier coordinator setup timeout remains unresolved.

The experiment and patch are preserved; normal product scheduling is unchanged. Real-encoding latency/interference, broader background progress, numeric resource enforcement and release packaging are not certified. No usage collector, extra runtime framework or public API/store/protocol change is added to the product.

### Proposed alternative for the next Q&A

A **shared live freshness page** could let the common Rust core validate an already-current workspace without waking the coordinator. The coordinator remains sole writer; storage/graph queries remain in-process, and registration, jobs, waits and notifications retain the existing framed Unix-socket contract. Pending/failed/unknown state falls back to the existing bounded RPC path, including the 500 ms default wait and explicit stale/not-ready outcomes. This is a proposal, not implemented behavior or an accepted replacement for per-query certification.

It must be live evidence rather than a prior-status cache: validate coordinator ownership/incarnation/liveness, page version and exact served generation; detect crashed/restarted writers, replaced files and torn updates. Preserve fresh-partial coverage, compiler evidence, native metadata and pinned readers. Missing/incompatible/uncertain pages must not fabricate freshness. Bound mappings/state across workspaces and reuse one Rust implementation across .NET/Node. A prototype must prove those failure cases and measure that its ownership/coherence checks actually avoid the scheduling tail; mmap alone does not establish safety or speed. No new query engine, source-history store, telemetry path or per-language cache is proposed.


## Shared live freshness prototype

The user authorized a shared live freshness prototype after the process-policy trial exposed an interference tradeoff. **The prototype's synthetic warm lookup matrix has zero misses in 24 cells, versus 11 for the RPC baseline; the largest prototype p95 is 4.01 ms.** No process/thread priority changes are used. This is a working isolated prototype, not installed behavior or a real-encoding gate pass. The normal service and product sources remain unchanged.

Evidence: [live freshness run](../.artifacts/fp/1794167245346412000/). The same isolated Standard-policy producer serves baseline and prototype consumers against separate, reconciled clones of the original TS generation 33 / Polly generation 11 indexes. No second writer attaches to the normal indexes. Compiler workers and all graph/storage/query operators retain their existing behavior.

### Mechanism and boundaries

The common Rust control client reads **immutable per-workspace status files through the OS page cache**, rather than retaining an old status answer. A single private, fixed-size owner file supplies a small shared atomic gate. Consumers map that gate read-only for the duration of a check; they do not map the graph or maintain permanent mappings per repository. The producer holds an exclusive kernel file lock on its current owner inode. The owner records a random incarnation token, PID/start identity and private layout/protocol version. Each read checks lock ownership, active OS process identity, current path/inode identity, bounded length, digests, protocol/payload shape and exact requested database. Stopped, exiting, crashed or replaced owners cannot establish a current result.

A producer-side status wrapper retires the workspace's status file **before any mutation** of its freshness-relevant fields. A completed coherent publication creates an immutable replacement, only for idle/available/error-free state with acknowledged observed inputs and a nonzero generation. Reader file identities are checked again at the end, so an already-open retired file cannot silently establish a later current result. The existing read-view generation comparison remains authoritative: a well-formed mismatched page yields explicitly stale results, never a fabricated matching graph. Compiler-error partial coverage remains separate from freshness and is not upgraded to complete.

There is also an **input gate before event enqueueing**. Source callbacks acquire a pending-input token; the token remains pending through main-loop event handling. Readers fall back while any relevant input is pending. Queue failure records an overflow sequence before dropping the pending token; only a corresponding completed rescan acknowledges it. Nested pending inputs, later overflows and counter corruption cannot be mistaken for a clear gate. Each shared atomic word uses a value/complement encoding, checked loads and nonwrapping updates; uncertainty disables or bypasses the fast path. This closes the queued-save/idle-status race rather than assuming an idle status means no save has arrived.

The owner backing inode has a fixed 4 KiB length, never truncated in place by the producer; replacements get a new inode. The OS rounds mappings to its page size. Per-workspace status files are bounded at 64 KiB; ordinary payloads are much smaller. Existing versioned Protobuf framing/status types are reused, including protocol v7 checks. The private owner layout is experimental version 2, with a separate status-page marker. Store v6 and native ABI v1 are unchanged. `libc` becomes a direct dependency **only in the source-copy prototype**, using the already locked 0.2.191 package; no new resolved package version or hosted service is introduced.

This is a cooperative private control-state contract. Source, nonce/hash and lease checks are not protection against the same user deliberately forging all control state. Concurrent external truncation of the fixed-size mapped owner inode is not covered by the tests and is outside the writer's lifetime contract. Private-file fault/lifecycle hardening remains necessary before promotion.

### Fallback, scheduling and compatibility

Missing, malformed, corrupt, uncertain, unsupported or pending state uses the existing RPC. The default 500 ms freshness wait, caller deadline behavior, stale/not-ready outcomes, exact served-generation check and query projection remain unchanged. Query execution still occurs in-process through the common Rust operators in .NET and Node; this is not another graph engine or an RPC graph service.

The prototype preserves the existing query-activity scheduler with a **bounded advisory activity cache**, not a freshness-status cache. The first request and then a request after five seconds use normal RPC to refresh coordinator activity. The local cache holds at most 32 database paths per core instance; each fast query still validates live state anew. Its recent checkpoint is inside the coordinator's existing 30-second active window. Consequently **first calls and sparse queries can retain the old RPC tail**, even when the index is already current. No claim is made that every warm arrival pattern benefits. Pending/failed state always falls back, regardless of the local activity stamp.

Compatible older coordinators have no live-state support; the actual new-client/old-coordinator test returns current results through two ordinary RPCs and accepts no leftover page. Old consumers work with the prototype producer through the original RPC. Unknown page layout/protocol metadata falls back to the control handshake rather than inferring compatibility from package versions.

### Safety and native integration

Eight new Rust test entries (including the subprocess helper) cover live lease and retirement, nested queued inputs, overflow acknowledgement/counter damage, stale/error/zero-generation exclusions, truncated/oversized/corrupt status data, wrong owner identity/versions, invalidation or replacement during a read, and actual SIGSTOP/SIGKILL without relying on destructors. Existing Rust workspace graph, publication, migration and framing tests pass. Debug/Release builds, formatting and all-target Clippy with warnings denied pass for the source copy; the initial missing copied SQL fixture and a test borrow error were corrected and their logs retained.

The compiler/native integration additionally proves:

- Two normal queries in **each real Node and .NET host** cause one RPC followed by one live read. The server's diagnostic wait count is sampled independently of the consumer, rather than inferring the route from timing.
- All eight named cases have complete parsed-response equality across the old Rust path and prototype Rust/Node/.NET paths, including both queries from each native pair.
- Corrupt status content falls back to current RPC evidence; a deliberately forged well-formed generation mismatch is served explicitly stale.
- A stopped owner times out through the existing policy and serves retained data stale. A killed owner cannot certify retained pages; restart reconciles before publishing a new incarnation.
- A saved unavailable .NET SDK pin invalidates the page and preserves the previous graph stale. Removing it causes real repair/publication and current results again.

These are focused prototype checks, not a claim that the unchanged product's full integration suite is green. The earlier coordinator setup timeout remains unresolved. Experimental route counters and the private test-only wait-count operation must be removed before published-build integration; they exist only in this ignored source copy.

### Synthetic native lookup matrix

The ordinary matrix uses the same eight exact/text/combined/reference cases, three Release native entry points, two process rounds and 25 calls per process, discarding five: **40 steady observations per cell**, 96 processes and **2,400 calls**. Baseline and prototype order rotates/reverses. Two bounded 36-thread load windows each have a hard two-minute self-termination deadline. There is no concurrent compilation during measurement and no Node/.NET GC/allocator override. Every call, including initial/warmup calls, is current with expected generation, positive row counts and truncation. Per-call timing summaries are not full-payload parity checks; full payload parity is checked separately above.

Milliseconds, empirical p95, **RPC baseline / live prototype**; bold values exceed 100 ms:

| Query | Rust | Node | .NET |
|---|---:|---:|---:|
| ts exact | **135.49** / 1.89 | **115.44** / 2.29 | **116.53** / 2.29 |
| ts text | **108.75** / 1.86 | **115.57** / 2.62 | **114.13** / 2.30 |
| ts combined | 89.25 / 3.26 | 89.11 / 4.01 | **101.58** / 3.74 |
| ts references | 96.72 / 1.36 | 82.49 / 1.92 | 89.87 / 1.63 |
| polly exact | 94.56 / 1.97 | 99.97 / 2.48 | **118.99** / 2.48 |
| polly text | 80.79 / 1.94 | 90.32 / 2.23 | 82.08 / 2.53 |
| polly combined | 82.35 / 2.15 | **104.00** / 3.00 | **135.65** / 3.05 |
| polly references | **104.03** / 1.79 | 89.26 / 2.67 | 80.97 / 2.59 |

The maximum prototype cell p95 is **4.0069 ms**. Summed API wall time over all ordinary calls is **53.898 s / 5.190 s**; summed consumer CPU is **4.125 s / 2.654 s**. These sums describe this series, not pooled tail percentiles or total-system/producer CPU. macOS process-rusage counters are calibrated separately, not assumed to be nanoseconds.

The first call still uses RPC. Across 48 processes per version, observed first-call p95 is **141.48 ms / 156.67 ms**, with medians **43.26 ms / 50.38 ms**. These are fresh-process API calls with warm storage and uncontrolled OS/runtime caches, not cold-boot results. They must not be hidden behind the much better steady burst result.

### Paced requests, activity checkpoints and competing work

Separate fixed-rate Rust series add **600 calls**, 150 per phase at nominal 10 requests/s, with baseline/prototype order reversed in the second load window. After five discarded calls, each phase has 145 steady observations. Pacing is serial and may catch up after a slow request, not an open-loop concurrency benchmark.

| Round/path | Full Rust API p95 | Maximum steady API time | Live reads / RPCs, all 150 | Competing M iterations/s |
|---|---:|---:|---:|---:|
| 0 baseline | 160.41 ms | 239.41 ms | not instrumented / 150 by baseline design | 1,947.7 |
| 0 prototype | 2.93 ms | 43.20 ms | 147 / 3 | 2,283.8 |
| 1 prototype | 3.04 ms | 7.77 ms | 147 / 3 | 2,351.4 |
| 1 baseline | 126.89 ms | 281.17 ms | not instrumented / 150 by baseline design | 2,428.0 |

The three prototype RPCs include first-call/activity checkpoints; those checkpoint tails remain in the raw measurements and steady maximum. The baseline driver's zero route-counter fields mean **uninstrumented**, not zero RPCs. Its unchanged control client sends a freshness RPC for each managed query.

Arithmetic throughput in prototype windows is approximately **17% higher** in round 0 and **3% lower** in round 1 than the corresponding baseline windows. These short varying windows do not establish a causal throughput gain or prove zero interference. They do not show the repeated large drop observed in the prior Interactive-policy trial. No real encoder throughput is measured.

### Memory and remaining work

Median sampled query-peak process footprint in this series is **5.51 / 5.20 MiB Rust**, **18.87 / 18.75 MiB Node**, and **35.24 / 33.32 MiB .NET**. These include the host and result, not just the reader, and are not accepted memory ceilings. The 5 ms sampler can miss brief peaks; kernel lifetime-peak evidence is retained too. On the same three-workspace cloned registry, one separate idle-producer sample is **9.56 MiB legacy / 9.50 MiB prototype**. These small differences are descriptive process variation, not proof of an allocation reduction, cold-cache behavior or whole-estate resource bounds.

The approach is promising for the accepted burst shape, but is **not integrated or installed**. Before promotion: make optional live-state initialization/resource failures fall back without breaking baseline coordinator startup, harden/document ownership and mapping-file lifetime/fault behavior, remove all experimental instrumentation, extend the coordinator regression coverage, and run the real-encoding native gate. Current experimental startup requires successful page initialization. First-call/sparse traffic and the remaining activity-checkpoint tail also need an explicit scheduling design decision; coalesced, nonblocking activity hints over the existing stream could avoid waiting for those replies without adding another graph engine or host background thread. No such hint mechanism is implemented or accepted yet.


## Live freshness integration closeout

The user authorized hardening/integration after the isolated prototype. The common core now contains the live-state path described in [LIVE_FRESHNESS.md](LIVE_FRESHNESS.md). Owner layout v3 is published only within the coordinator writer-fence lifetime; optional initialization, mapping and publication failures retain ordinary RPC/indexing. Failed staging artifacts are cleaned up. An inactive publisher still supports the unchanged source-event/scheduler path. The activity cache is bounded and evicts its oldest entry rather than leaving newly active workspaces uncached indefinitely. First/five-second activity-checkpoint RPCs remain.

No prototype routing counters, diagnostic control operation or telemetry switch was promoted. Control protocol v7, store v6 and native ABI v1 are unchanged; libc is an already resolved/pinned dependency now used directly for the macOS process/atomic-mapping boundary. No priority, compiler-GC or allocator policy was changed.

Both final Debug/Release builds pass. The **full Debug test script passes**, including compiler/native, the new stateful Node/.NET live tests, coordinator, preparation/flood, SDK resolution, call graph, project links/dispatch, Rust, Swift, SwiftPM, explicit-partial Xcode and four-language saved-update regressions. The previous coordinator setup timeout does not occur in this idle run with its original deadlines; this does not resolve expected-contention setup performance by itself. Optimized Rust workspace and live native tests also pass. Formatting and all-target Clippy with warnings denied pass.

New hardening regressions cover optional setup failure, publication/invalidation failure, staging cleanup, malformed owner size before mapping and lease retirement while callback references remain after endpoint teardown. The stateful native test proves second-query live certification with the control socket temporarily unavailable, then verifies crash/offline-edit recovery and RPC-only operation when the optional live directory is blocked. It requires no diagnostic API exports.

Final review added a deterministic completed-input interleaving regression and tightened read validation: double-collect the overflow sequence, check pending inputs again, then perform the final inode checks after the gate check. An event that completes and clears the gate cannot make a retired open status page current again. Debug/Release Rust tests, the Release native live test and the coordinator saved-input regression were repeated after this fix, alongside both builds, formatting and Clippy. The full Debug script above ran before this last ordering change.

The previously authorized development LaunchAgent was restarted after validation. Its plist/ProcessType and registrations remain unchanged; owner layout 3 is loaded. TS generation 33 and Polly generation 11 remain current without an index rebuild. Forty installed Rust/Node/.NET calls across the eight named cases have expected data and full parsed-response equality. This is correctness/activation evidence, not another performance matrix. Run evidence is under `.artifacts/live/1794175698639990000/`.

The earlier synthetic prototype timing table remains scoped to that source copy. No integrated-build real-encoding gate pass is claimed: no encoder is running during the integration checks. A normal encoding workload is still needed for that validation. First/sparse-query tails and the optional future nonblocking activity-hint design remain separate work; no hint mechanism was silently added.


## Nonblocking activity notification prototype (2026-11-09)

The user authorized a prototype after postponing the real-encoding run. It lives only under `.artifacts/ah/1794234120472172000/source`; the installed common core, native artifacts and normal LaunchAgent are unchanged. [Source diff](../.artifacts/ah/1794234120472172000/source.diff), [final comparison](../.artifacts/ah/1794234120472172000/comparison.json), [earlier comparison](../.artifacts/ah/1794234120472172000/before-drain-guard/comparison.json), [wire/native checks](../.artifacts/ah/1794234120472172000/native-checks.json) and [test evidence](../.artifacts/ah/1794234120472172000/README.md) are retained.

### Mechanism and tradeoff

A validated owner certificate advertises an optional `activity_hints` capability. The existing layout-v3 JSON envelope/checksum carries it; a missing field defaults to false. The prototype obtains fresh live certification on each query, then uses a complete, one-way `activity` request over the existing framed Protobuf Unix stream instead of waiting for an activity-checkpoint reply. It reuses protocol v7 and changes neither store v6 nor native ABI v1. Older consumers ignore the additional private owner field; newer consumers preserve first/five-second RPC checkpoints with older producers. Actual wire captures verify both directions.

Submission sets nonblocking mode before connecting, bounds the hint at 4 KiB, attempts one write, and closes its descriptor without awaiting a response. Setup failure, queue pressure or an incomplete write does not establish a local checkpoint; the ordinary wait/RPC path remains the fallback. Unsupported or uncertain live state uses that path too. The inherited RPC path still has its existing connection/deadline behavior; this experiment does not make every fallback or whole query nonblocking.

The existing 32-entry, five-second activity cache coalesces notifications across embedding threads/workspaces; its mutex covers only short nonblocking submission and cache work, with no reply wait. No consumer background thread, socket pool, new crate, native export, telemetry counter or diagnostic operation is added. Activity timestamps are transient scheduler state, not a retained usage record. Storage and graph queries remain in-process.

A successful write is **not an acknowledgement**. Existing admission limits can discard a notification after connection, and processing can be delayed. A subsequent query retries after the checkpoint expires. Consequently activity priority is best effort under overload. Live-owner/input/generation evidence still independently establishes freshness; losing an activity hint cannot certify stale data as current. The scheduler's existing five-second queue-aging precedence and 30-second active window remain; notifications cannot reset queue age, enqueue reanalysis, retry restoration, or bypass indexing limits. This is unchanged aging/fairness logic, not a proof of whole-estate fairness or under-load indexing progress.

The experiment exposed a Darwin gotcha: a peer that writes and closes can leave a buffered frame while timeout setup on the accepted socket returns EINVAL. The prototype explicitly switches to a nonblocking drain in that case, and sets write timeouts only for operations that send replies. This bounds unexpected timeout-setup failures too. A real one-way scheduler regression caught the original dropped-notification bug before measurement.

### Measurements

Both series use the same isolated, ordinary-policy Release producer and reconciled copies of TS generation 33 / Polly generation 11. Current installed Release clients are compared with prototype clients through real Rust, Node and .NET entry points. No priority, host-GC, compiler, storage or query-operator change is introduced. All measured calls retain expected generation, freshness, positive row counts and truncation.

Each series contains **318 timed calls**: six idle sanity calls; 144 first-call observations across eight named cases, three hosts, two versions and three counterbalanced rounds; and 168 calls from twelve paired long-lived consumers across two rounds. Those consumers issue seven calls apiece, 5.2 seconds apart. Sparse statistics include the six later calls per process, yielding 24 samples per host/version; their first calls remain in the raw results. First-call statistics also have 24 samples per host/version, pooled over the eight cases/three rounds, with no discarded first calls. Process startup, imports and Index construction precede timing; first-query wrapper/JIT/native work is included. Storage is warm and OS caches are uncontrolled, so these are not cold-boot timings or per-case acceptance percentiles.

Synthetic load uses 36 arithmetic threads with a hard 120-second self-termination bound per load process; every window is stopped in cleanup. Sparse runs deliberately synchronize twelve clients. This is a diagnostic arrival pattern, not a representative whole-estate or real-encoding workload.

Empirical p95 milliseconds, **baseline / prototype**:

| Arrival | Host | Initial series | Final bounded-drain series |
|---|---|---:|---:|
| first | rust | 129.34 / 3.27 | 7.81 / 6.49 |
| first | node | 102.38 / 8.99 | 12.43 / 6.07 |
| first | dotnet | 118.24 / 22.27 | 13.07 / 18.37 |
| sparse | rust | 190.56 / 6.24 | 93.11 / 5.03 |
| sparse | node | 112.48 / 6.80 | 116.84 / 5.46 |
| sparse | dotnet | 112.41 / 7.34 | 117.02 / 12.63 |

Infrequent-query tails improve in both series. First-query behavior varies substantially: the initial baseline has large RPC tails, while the repeat has much smaller ones, including a modest .NET first-query regression. These short series cannot distinguish all scheduling/cache effects or establish a universal first-query improvement. Both runs are retained; the initial one used the preceding closed-peer drain behavior and the final one includes the nonblocking guard. Neither certifies the real-encoding gate.

Phase-boundary resource records include each consumer/runtime and exclude producer/compiler/load CPU. Final first-call consumer CPU totals differ by roughly -0.4% to +2.2%; sparse consumer CPU differs by roughly -0.9% to +10.8%, depending on host. Final median complete-process footprint is approximately 4.5 MiB Rust, 17.2 MiB Node and 30.3 MiB .NET for first-call runs; these are observations, not memory ceilings. Sparse runtime-background work is included. No sampled-peak, whole-system CPU, competing-work throughput or causal allocation benefit is claimed. Raw [resource comparisons](../.artifacts/ah/1794234120472172000/resource-comparison.json) and load counters are retained.

### Verification and adoption status

Optimized workspace tests, build, formatting and all-target Clippy pass. New regressions exercise concurrent coalescing without a reply, expiration, a saturated listen queue, failed/oversized submissions, absent capability and actual one-way scheduler delivery. Notifications preserve the status certificate, saved-input epochs, restoration flags and queue age. Existing crash/stopped-owner, overflow, page-retirement, generation and source-publication tests remain green.

Real native checks compare 48 complete responses across the eight named cases. A temporary external endpoint captures first/burst requests while sending **no replies**: all three prototype hosts still return current results and submit one activity request for two burst queries. Proxied captures prove old consumers/new producer and new consumers/old producer each retain one ordinary wait followed by a live read. This adds no instrumentation to product libraries.

Adapted copies of the existing native live and saved-file coordinator regressions pass against isolated prototype data: crash/offline-edit recovery, optional-state RPC fallback, saved changes/dependent scopes, failures/repair, workspace identity, old protocol and external configuration. The full saved-file regression ran before the final drain guard; optimized workspace tests, native live recovery, full response parity and captured old/new wire checks were repeated after it. The complete product test script was not rerun for this source-copy experiment.

The prototype is ready for an adoption decision, including acceptance of best-effort activity priority. No main implementation was promoted or normal service reloaded. The real-encoding gate and broader resource/fairness validation remain open.

## Activity notification integration closeout (2026-11-09)

The user accepted best-effort activity delivery under overload and authorized integration. The reviewed source-copy changes are now in the common core and coordinator, with durable test roots resolved from the checkout rather than a research-machine path. The supported-capability path can certify a first query without an activity reply; failed submissions and older peers retain RPC. Freshness evidence, exact served-generation matching, queue aging, store v6, protocol v7, native ABI v1 and process policy remain unchanged. The accepted current contract is in [LIVE_FRESHNESS.md](LIVE_FRESHNESS.md).

Both main Debug/Release builds pass. The full Debug script passes, including the first-query/no-reply and burst-coalescing checks in actual Node/.NET hosts, crashes/offline edits/optional-state fallback, saved-file coordination, preparation/flood, resolution, calls, project links/dispatch and all existing experimental-language regressions. Optimized Rust workspace and native live tests, formatting and all-target Clippy with warnings denied also pass. No diagnostic counter/control operation, collector, new crate or embedding-host background thread was promoted.

The previously authorized normal development LaunchAgent was reloaded with unchanged plist/ProcessType. Its checksummed layout-v3 owner now advertises activity support. Forty correctness calls before the restart exercised new consumers against the previous producer; forty after it checked the installed supported path. All have current expected data and complete Rust/Node/.NET response parity; TS generation 33 and Polly generation 11 remain available without an index rebuild. These are correctness/activation checks, not timing acceptance. Evidence and final source snapshots are under `.artifacts/ai/1794235886885493000/`.

Both synthetic prototype timing series remain preserved under their original source-copy scope. First-query results vary, including the modest .NET regression in the repeat; sparse-query gains do not establish a universal first-query improvement. No integrated-build real-encoding gate pass is claimed.

The user selected CLI/MCP as the next workflow toward publishing 0.0.1, then 0.0.2. [RELEASE_PLAN.md](RELEASE_PLAN.md) records concrete remaining acceptance and distribution gaps without moving existing requirements or promoting experimental Swift/Rust into 0.0.1.
