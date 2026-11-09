# First indexing performance pass

Measured on Apple M5 Max, 18 logical CPUs, 64 GiB unified memory, macOS 27.2 (26B123), with Debug artifacts for the early passes and matched Debug/Release builds for the later comparison below. These are small public-corpus observations, not minimum hardware requirements, estate-scale acceptance, or latency guarantees. Numeric product resource ceilings remain open.

## Workloads and accounting

[benchmark-index.py](../scripts/benchmark-index.py) runs a separate coordinator/data root against pinned, detached corpus worktrees under `.artifacts/perf/corpus/`. Registered development repositories stay separate. Each save probe appends a comment, checks the published normalized source and current query result, restores the original bytes, and waits for restoration to publish. No upstream source changes are retained.

Cold means an empty workspace index with shared dependency/filesystem caches left warm. Restoration and compiler setup are included. Source-save measurements use the same established index. Compilation coverage is retained in every result; TS partials are reported rather than excluded. No dependency DLL builds were added for these probes.

The external sampler reads macOS `proc_pid_rusage` and the coordinator's descendant process tree every 250 ms. It reports sampled aggregate physical footprint and RSS separately. CPU includes the coordinator and accounted/reaped descendants. Mach CPU ticks are converted using the machine's 125/3 timebase, calibrated against process CPU time. Sampled memory peaks can miss shorter peaks, process counts can miss short-lived children, and sums are not a hard physical-memory cap. Phase time comes from sampled status and is approximate. Benchmark-client/sampler CPU is outside the coordinator tree. No profiling collector was added to product builds.

## Polly baseline

The corpus has 59 declared project/target contexts, all complete, with 132,383 symbol rows, 627,749 occurrences, and approximately 4.4 MiB of deduplicated indexed source text. Core-file saves recompile 53 contexts; a leaf sample save recompiles one. Whole-workspace coverage counts do not imply every context was recompiled.

| Measurement | Initial observation |
| --- | --- |
| Empty-index setup through current/queryable state | 294.75 s |
| Leaf sample save / restore | 3.69 / 3.20 s |
| Core source save / restore | 118.92 / 116.31 s |
| Core update accounted CPU | 152.54 / 157.53 CPU s |
| Cold sampled peak aggregate footprint | about 1.42 GiB |
| Coordinator idle footprint | about 9.1 MiB |
| Coordinator idle CPU over 3 s | about 0.001 CPU s |

Cold setup performed two analysis passes as newly discovered compiler inputs entered reconciliation. It sampled over 400 dotnet process identities across the run. About 111 s was attributed to preparation and 56 s to restoration; the remaining time was largely analysis/publication. These facts motivate setup/session reuse and better observed-input evidence, not weaker coverage/freshness checks.

Raw [cold baseline](../.artifacts/perf/0fc53c4d00/result.json) and [saved-file baseline](../.artifacts/perf/836d803670/result.json) are retained. The initial harness later hit BOM normalization during a save probe; the cold result was valid, and saved-file probes were rerun after correcting the harness. The result records that distinction.

## Compact-key storage change

Long symbol, artifact, and compilation IDs were copied into every occurrence and multiple indexes. Average occurrence key lengths were roughly 207, 121, and 51 characters respectively. This made the live database about 834 MiB and its post-update file high-water mark about 1.62 GiB.

Storage format v3 keeps public IDs unchanged and uses integer symbol/artifact references in occurrences and relationship endpoints. Generation/compilation ownership derives from the referenced symbol. The occurrence primary key also serves symbol/role lookups; compact artifact and incoming-relationship indexes support cleanup and reverse access. All languages and native entry points use the same Rust implementation.

| Polly measurement | Text-key baseline | Compact keys with cleanup indexes |
| --- | --- | --- |
| Post-update database file | about 1,655 MiB | about 307 MiB |
| Live allocated database pages | about 834 MiB | about 165 MiB |
| Core save / restore | 118.92 / 116.31 s | 93.02 / 94.77 s |
| Accounted CPU | 152.54 / 157.53 CPU s | 134.79 / 135.51 CPU s |
| Complete compilation contexts | 59 | 59 |
| Symbol / occurrence rows | 132,383 / 627,749 | unchanged |

This pair shows roughly 81% lower file footprint, 80% lower live-page footprint, and 20% lower core-update wall time. There are only two steady paired samples per configuration; these are diagnostic results, not confidence intervals or percentile claims. Active compiler memory did not materially improve. Rust coordinator CPU was approximately 15 s of each optimized core update; about 47–48 s of wall time was still attributed to compiler/MSBuild preparation.

The first compact implementation omitted the artifact child-key index. SQLite then scanned all occurrences while reclaiming old artifacts, making updates slower (about 157 s). That regression was found by the workload, fixed with an integer-key index, and rerun. The [first attempt](../.artifacts/perf/94df92fd58/result.json), [corrected measurement](../.artifacts/perf/08771b9b58/result.json), and [storage accounting](../.artifacts/perf/storage-after.json) remain available. The 135 MiB file immediately after migration is not presented as the steady update footprint.

## TypeScript observations

The ts-pattern baseline completed empty-index setup in 12.60 s and paired saves in 5.85/6.19 s. It retained three complete and three explicitly partial contexts. Sampled worker footprint was roughly 1.5–1.8 GiB. The [baseline](../.artifacts/perf/0e08757ef8/result.json) is retained.

The first compact-key run reduced the post-update file from about 51 MiB to about 14 MiB. Its saved-file timings were noisier/slower (7.26/15.37 s, with migration included in the first save), so it does not establish a TypeScript latency win. See [compact-key observations](../.artifacts/perf/809c394700/result.json). A [quiet steady repeat](../.artifacts/perf/3f9fcbca18/result.json) measured 6.17/5.86 s, similar to baseline, with the file stable around 14 MiB. The first noisy pair is retained rather than hidden. There is no demonstrated TypeScript latency improvement; compiler/type-checker memory and latency need separate profiling.

## Compatibility and migration

The same query operators can read formats 2/3 through an internal location projection. A compatible v2 index remains queryable until its next write. Writers transactionally recode v1/v2 facts to v3, preserve logical IDs, ranges, source, relationships, and row counts, and attempt compaction. Pinned WAL readers retain their pre-migration view. A busy compaction is deferred rather than invalidating readers. This is physical recoding, not a compiler rebuild or a historical-index feature.

Protocol v4 distinguishes the new library/coordinator contract from older peers; the native ABI remains v1. The migration regression checks both legacy formats, format-v2 reads before upgrade, old pinned reads after migration, public IDs, references, source membership, relationships, and foreign keys. Compiler/native parity, coordinator, preparation, and compiler-resolution suites remain required.

The rollout uses the same tested coordinator/native artifacts. Existing compatible indexes remain usable and upgrade on their next write; the [installed status snapshot](../.artifacts/perf/installed-status.json) records all registered workspaces idle/current, with Polly still 59/59 complete. This is separate from benchmark data.

## C# SDK and loader startup pass

The accepted change keeps SDK property/identity evaluation isolated in the selected SDK's CLI. SDK selection checks run once per distinct nearest `global.json` scope in a worker request, including nested pins. Every target still receives its own SDK identity evaluation.

Roslyn input loading now combines a project's targets only when all evaluated identity properties match. The pinned Roslyn 5.9 `basename(TFM)` discriminator must give a complete, unique root mapping; otherwise the adapter loads targets separately. Differing metadata always uses separate loads. A load also supplies discovery evidence: previously unknown dependencies are prepared with their independent identities/targets, then the root is reloaded against that catalog. Already-known dependencies do not require a separate discovery pass. Compiler closures still compile one target at a time and are released afterward. No private Roslyn APIs, compiler pools, new dependencies, or graph/query engines were introduced.

A fresh [original-loader control](../.artifacts/perf/8c7d43c5c8/result.json) and the adjacent [optimized run](../.artifacts/perf/c23e119c78/result.json) used the same corpus, established compact-key index, edit, and restore sequence:

| Polly core update | Original-loader control | Consolidated loading/discovery |
| --- | --- | --- |
| Save / restore wall time | 104.90 / 107.35 s | 85.08 / 70.78 s |
| Accounted process-tree CPU | 146.01 / 145.80 CPU s | 124.32 / 110.23 CPU s |
| Peak sampled aggregate footprint | 1.36 / 1.29 GiB | 1.13 / 1.14 GiB |
| Sampled dotnet process identities, including exited-name samples | 136 / 141 | 99 / 95 |
| Recompiled contexts per update | 53 | 53 |
| Whole-workspace complete contexts | 59 | 59 |

Across these two observations, mean wall time was about 27% lower, CPU about 20% lower, and sampled footprint about 15% lower. The control was slower than the earlier 93/95 s compact-key run, and the optimized pair itself varied considerably. This is evidence of improvement in this diagnostic workload, not a percentile, confidence interval, general throughput promise, or proof of performance on hundreds of repositories. Debug builds and warm shared caches remain the test conditions. Process identities are sampled observations, not exact launch counts.

An untimed [control fingerprint](../.artifacts/perf/facts-control.json) and [optimized fingerprint](../.artifacts/perf/facts-optimized.json) match for 132,383 symbol rows, 627,749 occurrence rows, 3,763 compilation/source-artifact memberships, and 59 compilation/coverage rows. The comparison includes public IDs, signatures, locations, source hashes/origins, compiler options, status, and diagnostics while ignoring private integer keys and generation numbers. The developer script canonicalizes JSON and compares counts plus an order-independent sum of per-row SHA-256 digests; it is a regression fingerprint, not a cryptographic equality proof. No equality claim is made for unimplemented relationships or query operators. The corpus worktree was restored cleanly.

The [resolution regression](../tests/resolution.py) additionally checks nested unavailable SDK pins, per-project startup-directory state, SDK-target failure evidence and prior-graph preservation, saved identity changes, and different identity properties across targets. Existing aliases, nearest frameworks, generated inputs, Release configuration, native parity, and no-built-DLL checks remain required. The final build/full test suite, Rust formatting/Clippy, and binding declaration checks passed. The [validation snapshot](../.artifacts/perf/sdk-startup-validation.json) confirms that the final worker matches the timed binary and all registered workspaces remain idle/current, with Polly 59/59 complete; existing TS partial coverage remains explicit.

### Rejected experiments and SDK gotchas

The direct in-process SDK API [first trial](../.artifacts/perf/5beb350678/result.json) measured 84/88 s but raised sampled footprint to about 2.1 GiB. Explicit project unloading, discarded build results, and node shutdown [recovered memory](../.artifacts/perf/a65663d6c8/result.json), but measured 92/91 s and did not establish a compelling latency improvement. `ProjectCollection.Dispose` does not unload projects or clear its project-root cache; unloading is a distinct lifetime operation. The prototype API path was removed.

The isolated [SDK probe](../.artifacts/msbuild-api/README.md) also showed that reserved `MSBuildStartupDirectory` remains anchored to the first invocation directory in a shared SDK process, despite changing the current directory for later projects. Overriding it through ordinary global properties fails with MSB4177. Keeping isolated CLI evaluation avoids private SDK reflection and preserves the existing per-project invocation semantics. Do not reintroduce the API optimization without resolving that behavior and measuring its full memory/CPU cost.

An intermediate [batch-only trial](../.artifacts/perf/202ce52818/result.json) reduced memory but measured 114/99 s; it did not establish a latency win. Removing redundant discovery loads produced the accepted result above. Rejected trials remain available rather than being omitted from the record.

Primary SDK references: [MSBuild Locator ordering and runtime ownership](https://learn.microsoft.com/en-us/visualstudio/msbuild/find-and-use-msbuild-versions), [project collection lifetime implementation](https://github.com/dotnet/msbuild/blob/main/src/Build/Definition/ProjectCollection.cs), [process-wide startup-directory implementation](https://github.com/dotnet/msbuild/blob/main/src/Build/BackEnd/BuildManager/BuildParameters.cs). Installed-SDK behavior was checked directly by the retained probe; upstream `main` is explanatory evidence, not the installed source pin.

## Optimized Release comparison

`bash scripts/build.sh release` builds the Rust host/core/native bindings and bundled SQLite with Cargo's stock Release profile, plus the C# worker and managed smoke consumer with .NET Release. The SDK reports `Optimize=true`; the TypeScript worker and compiler are the same JS/Node artifacts in both profiles. Debug remains the default build/test profile. Both profiles coexist, with separate native smoke packages and matching worker selection; the installed login coordinator remains on its existing Debug path. This adds development build staging, not release distribution packaging or diagnostic collection.

The build profile is independent of analyzed C# configuration: Polly remained **Debug** in all four updates. Each update recompiled 53 contexts and retained all 59 complete contexts. The same established index, pinned corpus, appended comment, restored bytes, warm shared caches, and sampler were used sequentially. No compiler tests overlapped timing.

| Polly core update | Debug build | Release build |
| --- | --- | --- |
| Save / restore wall time | 102.60 / 70.78 s | 74.34 / 85.53 s |
| Coordinator CPU | 17.40 / 15.20 CPU s | 8.29 / 8.81 CPU s |
| Accounted process-tree CPU | 117.39 / 110.57 CPU s | 109.84 / 117.82 CPU s |
| Peak sampled aggregate footprint | 1.13 / 1.13 GiB | 1.16 / 1.15 GiB |

The coordinator, including native SQLite work, used about **48% less CPU** on average. Whole-job CPU was effectively unchanged. Mean wall time was about 8% lower, but save/restore results reversed direction and overlapped substantially; this pair does **not establish a consistent end-to-end speedup**. Peak memory did not improve. Most accounted CPU remains in descendant analysis processes, so a Rust-only optimization is insufficient to address the remaining workload. This is attribution by process accounting, not a managed-code hotspot profile.

The TypeScript cross-check used the same ts-pattern source save and restored bytes, recompiled four contexts, and retained the existing three complete/three explicitly partial contexts:

| ts-pattern update | Debug build | Release build |
| --- | --- | --- |
| Save / restore wall time | 8.31 / 7.65 s | 7.65 / 7.62 s |
| Coordinator CPU | 1.16 / 1.13 CPU s | 0.57 / 0.55 CPU s |
| Accounted process-tree CPU | 13.56 / 13.07 CPU s | 12.47 / 12.56 CPU s |
| Peak sampled aggregate footprint | 1.58 / 1.53 GiB | 1.57 / 1.82 GiB |

Coordinator CPU fell about **51%**; elapsed times were similar and memory did not show a reduction. The TypeScript compiler/runtime did not receive a separate optimization mode. Two observations per profile, sampled memory, warm caches, and uncontrolled host variation cannot establish tail latency, memory limits, cold-setup performance, or estate-scale throughput.

Raw results: [Polly Debug](../.artifacts/perf/b5c0a57418/result.json), [Polly Release](../.artifacts/perf/df8d08c518/result.json), [TypeScript Debug](../.artifacts/perf/21356ce3d8/result.json), [TypeScript Release](../.artifacts/perf/25c7f84c70/result.json). Cross-profile logical fingerprints match for [Polly](../.artifacts/release/facts-release.json) and [TypeScript](../.artifacts/release/facts-ts-release.json), including the same public identities, signatures, ranges, source hashes/origins, compiler options, status, and diagnostics. Both corpus worktrees were restored cleanly. The [artifact manifest](../.artifacts/release/artifacts.json) records binary sizes/hashes and matching native staging; for example, the host executable is about 7.31 MiB Debug versus 3.47 MiB Release. Smaller files are not a worker-memory improvement.

Both full build/test profiles passed: Rust publication/protocol/migration tests, native Rust/.NET/Node equivalence, coordinator, preparation, and resolution suites. Rust formatting/Clippy and TS declaration checks also passed. Logs and compiler settings are retained under `.artifacts/release/`; the [validation record](../.artifacts/release/validation.json) identifies the tested artifacts. No ABI, store-format, or protocol version change was needed.

Reproduce after building the selected profile:

```sh
bash scripts/build.sh release
bash scripts/test.sh release
python3 scripts/benchmark-index.py --profile release \
  --workspace .artifacts/perf/corpus/Polly --label polly-release \
  --symbol ResiliencePipeline --mode updates \
  --data .artifacts/perf/0fc53c4d00/data \
  --edit src/Polly.Core/ResiliencePipeline.cs
```

Use `--profile debug` for the corresponding control. These paths refer to the retained local corpus/index; a new checkout must first prepare the pinned corpus and run the harness's `--mode all` setup. The next useful profile is inside compiler/SDK analysis, with worker memory tracked alongside CPU; these observations do not justify adding a resident compiler workspace for every repository.

The subsequent [C# component profile](CSHARP_PROFILING.md) separates SDK/context loading, semantic work, allocations, and protocol costs without adding product instrumentation.

## Batched SDK metadata per project

The follow-up to [C# profiling](CSHARP_PROFILING.md) batches SDK metadata evaluation in one short-lived process per project. It uses an internal `--sdk-metadata` mode of the same provider binary, the shared bounded Protobuf codec, and the repository-selected installed SDK. Each target still gets independent properties and `GetAssemblyVersion` results. The process's invocation directory stays fixed to the project, preserving reserved `MSBuildStartupDirectory` semantics. No new package, crate, service, graph engine, or persistent compiler pool was added; SDK assembly references are compile-only.

All required restoration now finishes before complete metadata or Roslyn project inputs are cached. Planning requests only discover frameworks/assets; they do not execute identity hooks. Successful complete metadata is cached for the current worker job, and shared between the analysis loop and lazy catalog. Source-only updates reuse existing preparation checkpoints and request one complete metadata batch per project. Restoration retries remain explicit/invalidated through the existing preparation path, including failed-restore deduplication and bounded error details.

A regression probe caught an SDK caching trap: a `GetAssemblyVersion` hook wrote an imported props file during the first target, but the next target reused its old value. Explicitly unloading project/import state between targets restored the original isolated-CLI behavior. The SDK process and loaded task code can remain shared within that project; target evaluation/results and imported XML must not leak. The new [SDK regression](../tests/sdk-metadata.mjs) verifies the version sequence 1 then 2, fresh sessions, planning without identity hooks, and no compilation target, in both build profiles. Existing startup-directory, SDK-pin, error, aliases, differing-target identity, and no-built-DLL regressions remain passing.

The adjacent Release [control](../.artifacts/perf/44e6f16ad0/result.json) temporarily used the preserved original worker; the [batched worker](../.artifacts/perf/6d046ceb98/result.json) was restored automatically afterward. Both used the same established Polly index, saved comment/restore sequence, Debug analysis configuration, and 53 recompiled contexts:

| Polly core update | Original SDK CLI calls | Per-project SDK batches |
| --- | --- | --- |
| Save / restore wall time | 79.72 / 86.58 s | 64.40 / 83.46 s |
| Accounted process-tree CPU | 112.91 / 122.50 CPU s | 99.70 / 127.77 CPU s |
| Coordinator CPU | 8.37 / 9.29 CPU s | 8.68 / 11.71 CPU s |
| Peak sampled aggregate footprint | 1.14 / 1.17 GiB | 1.15 / 1.17 GiB |
| Sampled dotnet identities, including exited-name samples | 99 / 107 | 50 / 55 |
| Complete workspace contexts | 59 | 59 |

Mean wall time was about **11% lower** across this pair. Mean total CPU was only about 3% lower; the second CPU observation was worse, and memory was essentially unchanged. These are two diagnostic observations per implementation, with substantial run variation, not a percentile or a whole-estate performance guarantee. Fewer sampled process identities support the intended startup reduction but are not exact launch counts. Semantic extraction remains expensive.

[Control facts](../.artifacts/sdk-batch/facts-control-fresh.json) and [batched facts](../.artifacts/sdk-batch/facts-batched.json) match the established production logical fingerprints: 132,383 symbols, 627,749 occurrences, 3,763 compilation/artifact memberships, and all 59 compilation/coverage rows, including public IDs, ranges, source hashes/origins, signatures, options, status, and diagnostics. The saved worktree was restored cleanly. Both full build/test profiles, formatting, Clippy, and binding declaration checks passed; the [validation record](../.artifacts/sdk-batch/validation.json) pins the tested/measured artifacts and installed status.

The three internal SDK DTOs are optional additions to the shared v4 envelope and use the existing codec; the regular worker/coordinator contract, store format 3, and native ABI 1 are unchanged. They carry provider tooling metadata, not a new logical fact family or query API. See [worker contract](WORKER_PROTOCOL.md).

## C# symbol-ID allocation pass

The emitter previously recomputed a documentation ID, fallback source anchor, assembly-qualified ID, and deduplication lookup for every bound occurrence. It now caches the resulting ID by **symbol object reference for one source file**, clearing symbol references after that file. It computes the location-based fallback only when Roslyn has no documentation ID. The existing string-ID deduplication still governs emitted symbols, including distinct objects representing the same public identity. No semantic-equality cache, permanent compiler pool, new dependency, or protocol/store/ABI change was added. Compiler binding and diagnostics still run unchanged.

### Allocation evidence

Private provider copies under `.artifacts/extraction/` wrap synchronous semantic calls with allocation/timestamp probes. `GC.GetAllocatedBytesForCurrentThread` measures allocations during each call; process-wide allocation counters measure entire phases/jobs. The readonly probe scope avoids allocating an object for every measured call. Nested documentation-ID allocations are part of symbol-ID allocations, so these rows must not be added together. Background Roslyn allocations appear in process-wide totals, not necessarily in calling-thread counts. No collector was added to product artifacts.

Both variants made 509,001 binding queries, 124,834 declaration queries, and 626,232 emitter symbol-ID requests per core update. Only repeated ID work was avoided:

| Polly core update | Original emitter | Per-file reference cache and lazy anchor |
| --- | --- | --- |
| Documentation-ID computations | 626,232 | 209,146 |
| Calling-thread symbol-ID allocation, save / restore | 657.6 / 658.4 MiB | 262.4 / 262.4 MiB |
| Process-wide extraction allocation | 3.97 / 3.98 GiB | 3.60 / 3.60 GiB |
| Entire worker managed allocation | 7.78 / 7.82 GiB | 7.43 / 7.44 GiB |

This is about **60% less ID allocation**, **9.4% less extraction allocation**, and **356–388 MiB less allocation per worker job**. These are cumulative allocated bytes, not resident memory savings. Binding still allocated roughly 2.6 GiB on the calling thread. The measured private candidate cleared the cache at the start of each file; the final production implementation clears at the end of each file, also releasing the last file's symbol references before diagnostics. Both limit retained symbols to one file. The final implementation received the uninstrumented measurements and full tests below.

The [allocation summary](../.artifacts/extraction/allocation-summary.json) retains all four phase logs and counters. [Baseline](../.artifacts/perf/19047350a8/result.json) and [candidate](../.artifacts/perf/5b96e9fb48/result.json) metadata identify the private executed assemblies. Their timing was highly variable and probe-affected; it is **not** used to claim product latency improvement. Both private variants preserved the [baseline facts](../.artifacts/extraction/facts-baseline.json) and [candidate facts](../.artifacts/extraction/facts-candidate.json).

### Uninstrumented Release comparison

The adjacent [control](../.artifacts/perf/d288a1ca08/result.json) and [optimized run](../.artifacts/perf/f14028c320/result.json) used ordinary Release artifacts, the same established Polly index and comment-save/restore probe, warm shared caches, and no concurrent compiler tests. The previous worker DLL/PDB were temporarily substituted for the control and the tested optimized artifacts restored through a `finally` block. Indexed C# configuration remained Debug. Each update recompiled 53 contexts; all 59 workspace contexts remained complete.

| Polly core update | Original emitter | Optimized emitter |
| --- | --- | --- |
| Save / restore wall time | 64.59 / 61.68 s | 55.36 / 56.55 s |
| Accounted process-tree CPU | 100.17 / 101.10 CPU s | 89.52 / 91.24 CPU s |
| Peak sampled aggregate footprint | 1.13 / 1.15 GiB | 1.11 / 1.15 GiB |
| Complete workspace contexts | 59 | 59 |

Mean wall time was **11.4% lower** and mean accounted CPU **10.2% lower** across this pair. Peak memory was effectively unchanged. Two samples per implementation, sequential ordering, warm caches, and uncontrolled host variation do not establish percentiles, a general speedup, cold-setup performance, or estate-scale throughput. The allocation counters give more direct evidence of the intended mechanism than whole-update latency does.

[Control facts](../.artifacts/extraction/facts-control-release.json) and [optimized facts](../.artifacts/extraction/facts-optimized-release.json) match the established production fingerprints: 132,383 symbols, 627,749 occurrences, 3,763 compilation/artifact memberships, and 59 compilation/coverage rows. Public identities/signatures, source hashes/origins, occurrence ranges, compiler options, status, and diagnostics match. The comparison has the same fingerprint limitations described above and makes no claim about unimplemented relationships. The corpus worktree was restored cleanly.

Both full Debug/Release build and test suites passed, including compiler/native parity, SDK metadata, coordination, preparation, and compiler resolution. New fixture queries exercise repeated references and distinct same-named locals/parameters across two files and both C# targets through Rust, .NET, and Node. Rust formatting/Clippy and TS binding declaration checks passed. The [validation record](../.artifacts/extraction/validation.json) identifies final artifacts, logs, benchmark paths, and installed coverage. The installed coordinator remains on Debug; all registered workspaces are idle/current, with Polly 59/59 complete and existing TS partials explicit.

## C# invocation reference lookup

The [binding allocation profile](CSHARP_BINDING_PROFILE.md) identified method-group construction inside name-based `GetSymbolInfo`. The worker now recognizes a simple name used as a direct invocation, a member-call name, or a conditional-member-call name. It queries the invocation and accepts only an exact ordinary, reduced-extension, or local-function method. Everything else uses the original name query. Delegate variables/properties/fields/events, explicit delegate `Invoke`, dynamic/function-pointer calls, method groups used as values, unsupported syntax, and unresolved/ambiguous calls therefore retain their existing handling.

This is one small helper in the existing C# adapter, with no retained state or new dependency. It keeps the same normalization, source-name range, emission path, compiler settings, and diagnostics. Querying a call node here does **not** implement call relationships or callers/callees queries. Protocol v4, store format 3, and native ABI 1 are unchanged.

### Correctness before adoption

A [private differential pair](../.artifacts/perf/3ec2f85920/result.json) compared the candidate with the original name query at every analyzed simple name: **509,001 names per update, 1,018,002 total, zero mismatches**. It checks normalized symbol equality including nullability, the existing ID key/assembly identity and display signature, and candidate reason. Each update reanalyzed 53 contexts; all 59 workspace contexts remained complete. This is exhaustive for the visited workload names, not a proof for every C# program.

A separate [compiler probe](../.artifacts/invocation/cases.log) checks valid and deliberately partial code with nullable analysis enabled and disabled. It covers direct/member/conditional calls, generic methods and extensions, interface calls, local functions, delegate locals/properties/fields/events, explicit `Invoke`, parenthesized delegates, `nameof` method groups, dynamic receivers/arguments, function pointers, ambiguous overloads, missing methods, and bad arguments. All 156 visited names match the original algorithm, with 44 marker assertions across the four compilations.

The permanent [native integration regression](../tests/e2e.py) and [fixture](../tests/fixtures/csharp/Invocations.cs) verify overload/generic/extension/local-function identities and references, callable locals/properties/fields, and both C# targets through Rust, .NET, and Node. A saved ambiguous call produces fresh partial C# graphs: overloaded declarations stay queryable, while exact-reference results remain empty. Compiler errors are not converted into analyzer failure or guessed edges.

### Matching allocation probes

The private candidate and a fresh original control use the same refined [probe](../.artifacts/binding-profile/Probe.cs), without stack collection or differential comparisons during measurement. Counters bracket the entire reference-lookup request, including any invocation query and fallback query. There are 509,001 **lookup requests** per update in both variants; this is not a claim that the candidate makes exactly that many underlying Roslyn API calls.

| Polly core update | Original name lookup | Invocation-aware lookup |
| --- | --- | --- |
| Calling-thread binding allocation, save / restore | 2,678.7 / 2,679.4 MiB | 2,029.6 / 2,028.4 MiB |
| Entire worker managed allocation | 7.44 / 7.43 GiB | 6.79 / 6.81 GiB |
| Complete workspace contexts | 59 | 59 |

Mean binding allocation fell **24.3%**, about **650 MiB per update**. Entire worker allocation fell **8.6%**, about **656 MiB per update**. These are cumulative allocated bytes, not resident-memory savings. The whole-job counter includes probe bookkeeping and background work. The [summary](../.artifacts/invocation/allocation-summary.json) retains all four original counter paths; the [fresh control](../.artifacts/perf/dc7ef3bd20/result.json) and candidate metadata identify private executed assemblies separately from the harness's product assembly hash. Their timings are not uninstrumented product benchmarks.

The measured private candidate keeps the helper in a separate static class; production places the same algorithm inside the existing adapter. The [validation](../.artifacts/invocation/validation.json) checks matching helper bodies, and the final product artifacts receive the uninstrumented tests below. The saved allocation is for the complete query strategy, not an exclusive attribution to extension-method reduction. An earlier sampled stack share is not an upper bound on savings from selecting a different public API path.

### Uninstrumented Release checks, including reverse order

Ordinary Release builds used the same established Polly index and saved comment/restore sequence, warm shared caches, Debug analysis configuration, and no concurrent compiler tests. The first pair ran the preserved original worker, then the tested optimized worker. The second pair reversed that order because the first suggested an apparent regression. DLL/PDB substitution uses `finally` restoration; the final optimized worker is restored and hash-checked.

| Ordered run | Save / restore wall time | Accounted process-tree CPU | Peak sampled aggregate footprint |
| --- | --- | --- | --- |
| First: original control | 56.46 / 53.73 s | 91.15 / 88.94 CPU s | 1.14 / 1.12 GiB |
| Second: optimized | 58.60 / 66.72 s | 93.74 / 104.59 CPU s | 1.17 / 1.16 GiB |
| Repeat first: optimized | 55.14 / 58.55 s | 89.83 / 93.68 CPU s | 1.17 / 1.16 GiB |
| Repeat second: original control | 67.94 / 72.81 s | 104.58 / 102.74 CPU s | 1.17 / 1.19 GiB |

The first candidate pair was about **14% slower**; the reverse-order pair reversed the direction. Across all four samples per implementation, mean wall time was 4.8% lower and mean CPU only 1.4% lower for the candidate, with substantial overlap and run variation. **These results establish neither a consistent end-to-end speedup nor a repeatable timing regression. Peak memory did not improve.** The accepted benefit is reduced managed allocation churn with preserved output, not a claimed latency or resident-memory win. Four observations, sequential ordering, warm caches, and uncontrolled host variation do not establish percentiles, general throughput, cold performance, or estate-scale acceptance.

Raw runs: [initial control](../.artifacts/perf/f76bc6f4b8/result.json), [initial optimized](../.artifacts/perf/126108fc90/result.json), [repeat optimized](../.artifacts/perf/3defb36400/result.json), and [repeat control](../.artifacts/perf/59b75ce6a0/result.json). Every update recompiled 53 contexts and retained all 59 complete. Logical fingerprints after verification, both allocation variants, and all four ordinary runs match the established 132,383 symbols, 627,749 occurrences, 3,763 compilation/artifact memberships, and 59 compilation/coverage rows. Public identities/signatures, ranges, source hashes/origins, options, status, and diagnostics match, with the previously documented fingerprint limitations. The worktree is restored cleanly.

Both full Debug/Release build/test profiles passed, including the new native regressions and existing SDK metadata, coordinator, preparation, and resolution suites. Rust formatting/Clippy and TS declarations passed. The [validation record](../.artifacts/invocation/validation.json) pins source/artifact hashes, all seven fact comparisons, private runs, complete test logs, and installed status. All registered workspaces are idle/current; installed Polly remains 59/59 complete and existing TS partials remain explicit. No product telemetry/profiling collector was added.

## C# compiler reuse within one job

The [dependency fan-out investigation](INCREMENTAL_ANALYSIS.md) found repeated compiler work behind the 53 affected root contexts. The user selected reuse confined to one indexing job, with compiler state released afterward. Invalidation scope remains unchanged: the worker still emits fresh facts, generated source, and diagnostics for every affected target.

The C# adapter now loads requested project definitions before analysis and chooses the next root by greatest intersection with the current dependency closure, preserving request order for ties. One active `AdhocWorkspace` removes projects outside the next closure and adds missing projects from the existing lazy catalog. Unchanged shared projects retain Roslyn compiler state. The intersection of two dependency-closed sets is itself closed, so the retained project references, aliases, and identities do not need replacement or emitted dependency DLLs. Every target keeps its original compilation ID and fact extraction path.

The active workspace and lazy catalog are disposed at job completion; the worker still exits after each job. No compiler session remains between saves. This changes provider orchestration, with no new dependency, query engine, Rust scheduling policy, protocol version, store format, or native ABI. Logical closure size is not a byte cap; numeric resource enforcement remains unfinished.

### Observed reuse and correctness

Private provider copies count actual `Compilation` object identities using weak keys that do not keep compilations alive. They traverse only root/reference compilation objects already supplied by Roslyn. Both paired observations gave:

| Polly core update | Independent root workspaces | Active closure reuse |
| --- | --- | --- |
| Analyzed root contexts | 53 | 53 |
| Root/reference compilation visits | 157 | 157 |
| Distinct compilation objects observed | 157 | 66 |
| Maximum projects in the active solution | 6 | 6 |
| Complete workspace contexts | 59 | 59 |

This is **58% fewer distinct compiler objects**, not 58% fewer target analyses, compiler executions, CPU, or live objects. Old solution state can survive until garbage collection; six projects in the active solution does not establish a six-object memory bound. The [summary](../.artifacts/compiler-reuse/summary.json) retains both control and candidate logs, [private control run](../.artifacts/perf/b5e81d4b10/result.json), and [private candidate run](../.artifacts/perf/8732307120/result.json). These probes precede the loader-diagnostic ownership fix below; the closure/reuse algorithm is identical to the final worker. Probe-run latency is not a product benchmark, and no collector is compiled into product artifacts.

Preloading exposed a diagnostic ownership issue during review: an unscoped load diagnostic from a shared-output-path project could mark unrelated healthy contexts partial. A retained [reproduction](../.artifacts/compiler-reuse/warning-repro.log) demonstrates the leak. Diagnostics now retain the owning load's root IDs; a diagnostic with an explicit project ID uses that ID, while an unscoped diagnostic applies only to closures containing its owners. The [regression](../tests/resolution.py) deliberately overlaps artifact pivots across targets and verifies that unrelated healthy contexts remain complete. The two problematic contexts remain visibly partial. Initial timing artifacts from before this fix are retained under [initial benchmark manifest](../.artifacts/compiler-reuse/benchmark-initial.json) and `.artifacts/compiler-reuse/initial-optimized/`; they are not the final measured artifact.

The same regression builds a test generator outside the indexed workspace and attaches it to consumers of a source project. A dependency comment changes generated methods, invalidates the unchanged consumer call, and produces fresh partial coverage. Removing the comment restores the generated source and complete coverage. This exercises the [semantic-version counterexample](INCREMENTAL_ANALYSIS.md#reproduced-limit-of-semanticapi-equality) through the real provider, shared publication path, and query operators. Product analysis still does not build dependency assemblies. Existing aliases, nearest frameworks, per-target SDK identities, SDK hooks, generated inputs, partial graphs, and saves during analysis remain required checks.

### Exact final Release comparison

After fixing diagnostic ownership, both full build/test profiles passed and a new ordinary Release comparison used the exact final worker. The same established Polly index, saved comment/restore probe, Debug analysis configuration, warm shared caches, and sampler were used sequentially with no concurrent compiler tests. The preserved original DLL/PDB were substituted for the control, then the tested final worker restored and hash-checked.

| Polly core update | Original control | Job-scoped compiler reuse |
| --- | --- | --- |
| Save / restore wall time | 55.94 / 55.04 s | 56.21 / 50.90 s |
| Accounted process-tree CPU | 88.92 / 91.36 CPU s | 85.83 / 79.56 CPU s |
| Peak sampled aggregate footprint | 1.17 / 1.20 GiB | 1.10 / 1.03 GiB |
| Reanalyzed contexts | 53 | 53 |
| Whole-workspace complete contexts | 59 | 59 |

Mean accounted CPU was **8.3% lower** and mean sampled peak footprint **9.7% lower** in this pair. Mean wall time was only **3.5% lower**, with one update slightly slower; a consistent elapsed-time improvement is not established. Two observations per implementation, sampled peaks, warm caches, sequential ordering, and uncontrolled host variation do not establish percentiles, a memory ceiling, cold performance, or whole-estate throughput. The active workspace does not have an accepted numeric budget. These observations support keeping the reuse change without treating high-end test hardware as a product requirement.

Raw [final control](../.artifacts/perf/2a19fe0b68/result.json) and [final reuse](../.artifacts/perf/455589be28/result.json) are retained. The final idle samples after both optimized updates contain only the coordinator, at approximately 13.3 MiB physical footprint, with no sampled compiler descendants. Worker exit and workspace disposal release compiler state after the job; no across-save cache was added. The benchmark's separate three-second idle window occurs before edits, so it is not used as post-job evidence.

[Control probe facts](../.artifacts/compiler-reuse/facts-control-probed.json), [reuse probe facts](../.artifacts/compiler-reuse/facts-candidate-probed.json), [final control facts](../.artifacts/compiler-reuse/facts-control-release.json), and [final reuse facts](../.artifacts/compiler-reuse/facts-optimized-release.json) match the preceding production fingerprints: 132,383 symbols, 627,749 occurrences, 3,763 compilation/artifact memberships, and 59 compilation/coverage rows. Public IDs/signatures, ranges, source hashes/origins, compiler options, statuses, and diagnostics match, with the existing fingerprint limitations. The corpus was restored cleanly.

Both complete Debug/Release suites passed on the final diagnostic-scoped version, including the generator and loader-diagnostic regressions. Rust formatting/Clippy and binding declaration checks passed. The [validation record](../.artifacts/compiler-reuse/validation.json) identifies final source/binary hashes, private observations, all four final fact comparisons, post-job process samples, logs, and installed state. Installed Polly remains 59/59 complete; all registered workspaces are idle/current and existing TypeScript partials stay explicit. This remains a development slice, not complete v0.0.1 capability or an estate-scale acceptance result.

## Next bottlenecks

The [TypeScript profile](TYPESCRIPT_PROFILING.md) finds compiler-backed signature work and generic type relationships dominate the measured worker. Private allocation samples estimate roughly 6.4 GiB of heap churn per update; program-boundary collection demonstrates considerable collectible heap. A diagnostics-first experiment changes three reference identities through the compiler's `Transient` flag, so it is rejected as an output-compatible optimization. Product code remains unchanged in that investigation.

The subsequent [TypeScript implementation](TYPESCRIPT_PROVIDER.md) canonicalizes single-root compiler views to declaration symbols and applies the user-approved collection policy between completed compilations in the isolated worker. All 38,510 corpus occurrences remain; 91 duplicate view symbols consolidate. Ordinary compiler-process observations show about 27% lower maximum RSS with about 2% more elapsed time and 7% more CPU; these exclude coordinator/store costs and are not performance gates. Multiple-root view identity and signature-display ordering remain explicit limits.

The [dependency fan-out investigation](INCREMENTAL_ANALYSIS.md) traces a core save to six direct targets and 53 dependent contexts. Its job-scoped reuse candidate is implemented above. A reproduced generator counterexample still rules out semantic-version/public-signature equality alone as a general skip rule; required target analysis and invalidation scope have not been reduced.

The [follow-up binding profile](CSHARP_BINDING_PROFILE.md) describes the preceding 2.6 GiB binding-allocation baseline. The invocation change above reduces that to roughly 2.0 GiB without establishing a further latency or resident-memory win. Initial body/nullable setup remains substantial; more allocation-only changes should not be mistaken for proof of whole-update performance.

1. Batched SDK evaluation and remaining Roslyn build-host startup still cost time. Further reuse must preserve invocation-directory state, per-target metadata, dependency identities, and bounded lifetimes; the rejected direct API path is not an accepted shortcut. Release measurements halve coordinator CPU without establishing a consistent whole-update win; profile compiler/SDK work before choosing the next hot path.
2. Cold setup repeats analysis for newly observed compiler inputs. Actual compiler-consumed input hashes could avoid redundant work; conservative reconciliation must remain until that evidence is sound.
3. TS type checking and active worker memory need profiling independently of the Rust core. Worker reuse requires an explicit bounded-residency policy, not one permanently resident compiler workspace per repository.
4. Further physical high-water reclamation, FTS maintenance, query tails, and large-estate scheduling need their own measurements. This pass does not validate hundreds of repositories or automatic memory/CPU-cap enforcement.

Primary accounting references: [Apple resource-accounting structures](https://github.com/apple/darwin-xnu/blob/main/bsd/sys/resource.h), [Apple memory footprint guidance](https://developer.apple.com/library/archive/technotes/tn2434/_index.html).

## First call/dependency slice

The [first graph slice](CALL_GRAPH.md) adds facts, not an alternative execution engine. Protocol v5/store v4 add per-site source evidence, nullable unresolved targets, dispatch/implicit distinctions and capability reports while retaining compact integer keys. Old triples remain legacy_unknown. All endpoint/artifact child foreign keys are indexed for reclamation. Current TS/C# graph capabilities are explicitly partial, independently of compiler diagnostics.

The pinned TypeScript corpus now has **16,995 symbols, 41,624 occurrences, 47,829 relationships, 124 compilation/artifact memberships and six reports**. All **38,510 original occurrences remain exactly present**; 3,114 additional occurrences supply initializer/lambda/accessor/constructor evidence. [Original-occurrence comparison](../.artifacts/call-graph/original-occurrences.json). Compiler coverage remains three complete/three partial; no broken examples/tests were removed. The copied worktree was restored cleanly.

The first ingestion path redundantly checked relationship existence after every successful, already validated SQL insert. That can repeatedly scan a large callable's adjacency. The final path checks only ignored inserts, preserving validation of duplicates and malformed endpoints/source ranges. The [fact fingerprints](../.artifacts/call-graph/corpus-facts.json) match across all symbols, occurrences, relationships, artifact memberships, compilation options and reports. The fingerprint is a regression check, not mathematical equality proof. Malformed evidence and duplicate identity cases also have Rust regressions.

| Saved update / restoration | Initial graph ingestion | Lookup optimization |
| --- | --- | --- |
| Whole update elapsed | 10.42 / 11.70 s | 7.79 / 6.83 s |
| Coordinator CPU | 5.28 / 5.53 s | 2.06 / 1.95 s |
| Accounted process-tree CPU | 18.24 / 18.84 s | 12.84 / 11.85 s |
| Sampled aggregate RSS | 1.111 / 1.104 GiB | 1.107 / 1.102 GiB |

These are Debug builds, two observations per path, warm dependency/filesystem caches and uncontrolled concurrent machine activity. They do not establish tail latency, resource ceilings or estate-scale gates. The earlier graph-free measurements indexed less information; they are not an output-equivalent control. Raw [initial run](../.artifacts/perf/55d5db6ed8/result.json) and [optimized run](../.artifacts/perf/cfe4425de0/result.json). Database high-water/WAL sizes include staging/reclamation and should not be confused with live fact bytes.

Five warmed ordinary native Node observations on this corpus gave median callers **9.80 ms**, callees **1.75 ms**, and impact **9.90 ms**. Callers/impact returned the requested first 20 findings with explicit result-limit truncation; callees returned one finding. Requests bounded depth, edges, nodes and traversal work. This includes the in-process addon/core/JSON path; the stopped benchmark coordinator makes freshness explicitly stale and does not measure live freshness waiting. [Exact requests, measurements and limits](../.artifacts/call-graph/query-benchmark.json). No host GC flag, debugger/profiler or product usage recorder was introduced. [Query plans](../.artifacts/call-graph/query-plan.json) confirm indexed adjacency in both directions without a temporary sort before row limits.

The already registered reference corpora were refreshed on the development coordinator. Polly retains **59/59 complete compiler reports** and now has **788,603 relationship records**; its graph capability reports are partial. The installed TS corpus has 47,829 relationships with its existing three complete/three partial compiler contexts. All three registrations were idle/error-free at closeout; a Polly graph smoke query returned current results with explicit limits. [Installed evidence](../.artifacts/call-graph/installed-summary.json). This proves the measured slice works on those repositories, not completion of cross-project linking, all dispatch/implicit forms, hundreds-of-repository operation or v0.0.1 release acceptance.

### Project-body linking follow-on

The [project-body slice](CALL_GRAPH.md#compiler-selected-project-bodies) adds selected-context/snapshot metadata and indexed exact definition pointers in store v5. Occurrence and relationship endpoints remain compact private integers. An initial nested union could scan the entire relationship store for incoming links; four explicit union branches and ordered lookup joins removed that plan. A core `EXPLAIN QUERY PLAN` regression checks that neither direction scans relationship tables, builds an automatic adjacency index or sorts a temporary tree before LIMIT. The final diagnostic plan is retained under `.artifacts/project-links/query-plan.json`.

The installed Polly graph has 10,260 pointers and 59/59 complete compiler reports after refresh; this is functionality evidence, not a controlled performance comparison. No indexing throughput, resident-memory improvement or latency guarantee is claimed from this run. The complete Debug suite, focused Debug/Release project-link/native checks, optimized core tests and code checks are recorded in `.artifacts/project-links/validation.json`.

### Cross-project dispatch follow-on

Dispatch now reads exact receiver-specialized implementation proofs through indexed lookups under the graph's existing global work budgets. Compiler adapters emit verified ancestry; Rust does not construct an unbounded recursive SQL closure or cache the whole graph. Both endpoint directions retain the no-scan/no-materialization query-plan gate. Nongeneric keys are small constants; closed generic keys are fixed-size SHA-256 digests, with per-file/per-program compiler caches. Store v6 preserves key distinctions in evidence uniqueness and keeps interned occurrence/relationship endpoint IDs.

Polly's refreshed index has 149,816 keyed relationships and 59/59 complete compiler reports; TS has 135 keyed relationships and its existing three complete/three partial compiler contexts. A real current Polly query produced a compiler-selected cross-project candidate. These are functional observations, not controlled throughput, latency or memory results. See [dispatch evidence and limits](CALL_GRAPH.md#cross-project-interface-and-virtual-dispatch) and `.artifacts/dispatch/validation.json`.

### Query latency and memory follow-up

The [query investigation](QUERY_PERFORMANCE.md) measures the refreshed TS/Polly graphs through Release Rust, native Node and native .NET APIs. Bounded impact requests expose repeated SQLite planning and large temporary edge records; isolated shared-core prototypes preserve all ten full responses and substantially reduce the observed latency/footprint. Both changes are now implemented in the common core. The report retains the original baseline, separate prototype evidence, actual Rust/Node/.NET measurements, paired controls, a small combined-search tradeoff, and exact regression outcomes including isolated setup-timeout retries. Indexing throughput and published resource budgets are unchanged.

The [symbol/text follow-on](QUERY_PERFORMANCE.md#symbol-and-text-lookup-follow-on) removes compilation-wide symbol scans through fixed bound SQL shapes and join ordering, using existing indexes. Real Rust/Node/.NET results remain equivalent across 55 controlled probes; physical-plan/work and busy-machine latency evidence are retained separately. Full compiler integration hit initial-setup timeouts twice and remains explicitly unverified in that run; no indexing throughput or release-readiness claim follows.
