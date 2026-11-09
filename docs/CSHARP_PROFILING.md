# C# analysis profile

The Polly profile identifies two main costs: SDK/project setup takes about **57% of worker wall time**, and semantic extraction takes about **30%**. Protobuf encoding takes roughly a quarter-second. The next candidate is batching SDK metadata evaluation per project; replacing the codec is not supported by these measurements.

This is a component profile of the existing Release implementation on the same M5 Max environment as [the indexing measurements](INDEXING_PERFORMANCE.md). Polly's analyzed configuration remained **Debug**. Each core save/restore recompiled 53 contexts and retained all 59 complete contexts. Test code and every declared target remained represented; the six unaffected contexts were retained through the ordinary update path.

## Measurement method

All profiling code lives in the durable ignored `.artifacts/csharp-profile/` directory. `prepare.py` copies the current provider and generated protocol source, inserts phase probes, and builds a separate Release assembly with the same locked dependencies. An isolated launcher substitutes that assembly only in the benchmark coordinator's process environment. It restores the worker's ordinary PATH for SDK subprocesses. Production source, binaries, protocols, and installed-service configuration are unchanged. No product collector was added.

Two full updates collected external `dotnet-trace` data; two more used the same phase probes without stack sampling. The latter pair supplies the table below. Stack collection measurably adds overhead, so trace-run latency and peak memory are not product performance results. A one-target leaf probe first checked startup tracing and fact equality.

The private probes record wall time, worker-process CPU, process-wide managed allocations, GC counts, endpoint RSS, frame counts/bytes, encoding time, and pipe-write time. CPU/allocation deltas cover all worker threads while a phase is active, including background work; they are not exclusive attribution to that API and exclude SDK/build-host child CPU. A child-only CPU calibration measured about 0.019 CPU-seconds in the parent while its child burned one CPU-second. Allocations are cumulative churn, not resident memory or retained heap. RSS snapshots are not sampled physical-footprint peaks.

## Phase results without stack sampling

| Stage | Calls per update | Save wall time | Restore wall time | Managed allocation per update |
| --- | --- | --- | --- | --- |
| SDK CLI commands | 85 | 27.08 s | 32.66 s | about 6 MiB in the worker; child allocations excluded |
| Roslyn project/context loading | 18 | 15.65 s | 18.36 s | 107–112 MiB |
| GetCompilationAsync | 53 | 5.29 s | 5.82 s | 1.78–1.79 GiB |
| Load/hash/emit indexed source | 53 | 0.25 s | 0.34 s | about 68 MiB |
| Semantic queries and fact extraction | 53 | 22.23 s | 26.58 s | about 3.97–3.98 GiB |
| Compiler diagnostics/reporting | 53 | 3.50 s | 3.95 s | 1.55–1.57 GiB |
| Entire worker job | 1 | 74.78 s | 89.08 s | 7.68–7.72 GiB |

The job total contains the individual phases; do not add it to them. Bookkeeping, workspace lifetimes, and probe overhead account for the remainder. The post-compilation `GetSourceGeneratedDocumentsAsync` retrieval was near zero; generator work can already occur inside compilation, so this is not evidence that source generation is free.

SDK calls break down into 53 `GetAssemblyVersion` evaluations, 15 requested-project framework evaluations, 15 catalog definitions, and two SDK-selection/listing commands. Assembly identity alone consumed 21.07/25.32 seconds. The two sets of framework/definition evaluations consumed another 5.87/7.23 seconds. These are actual selected-SDK calls, including repository versioning hooks; no metadata was guessed or compiler diagnostics suppressed.

All frames together encoded in **0.226/0.261 seconds**. Pipe writes took **2.69/4.65 seconds**, nested within the phase times above, and can include backpressure. Each update sent 8,852 frames and roughly 273 MiB. The long IDs make the stream substantial, but the measured codec cost is small compared with setup and binding. Storage/WAL work after the worker exits is outside its job total.

The three Polly.Specs target contexts were the largest extraction cases. Together they consumed about 8.0/10.3 seconds of extraction wall time and roughly 1.7 GiB of extraction allocations. Removing tests or targets would change required coverage and is not a performance fix.

## Managed stack observations

Pinned `dotnet-trace` **10.0.750501** collected `dotnet-common` and `dotnet-sampled-thread-time` with a 64 MiB trace buffer. Its installed profile describes approximately 100 Hz thread-stack sampling. The target starts suspended for trace attachment and the private entry point clears that setting before launching SDK children. The tool and phase-probe sources are not part of published artifacts.

These are **thread wall samples, not kernel CPU samples**. Summing samples across threads can greatly exceed elapsed time. Default top-N output is dominated by waits and unresolved/synthetic frames. The Speedscope exports label managed/unmanaged intervals with `CPU_TIME` and `UNMANAGED_CODE_TIME`; those names do not establish processor residency. Roughly 94% of summed thread time was marked unmanaged, which can include native waits, JIT, and GC work. It must not be interpreted as 94% CPU in native code.

The resolvable managed stacks show compilation tracking, Roslyn invocation binding/`GetSymbolInfo`, declaration binding, diagnostics, and some documentation-ID generation. They do not establish our ID formatter as the dominant CPU hotspot. Finalizer and monitor frames are prominent too. The phase measurements provide a stronger basis for the next change than percentages from the default all-thread top-N table.

Raw `.nettrace` files, top-N reports, and portable Speedscope JSON are retained for both full traces. The [summary](../.artifacts/csharp-profile/summary.json) contains phase totals, per-context ranks, SDK-command categories, and explicitly qualified stack statistics. Its managed categories overlap and are descriptive, not additive CPU shares.

## Evidence and correctness

- [Traced core updates](../.artifacts/perf/764e7a7330/result.json), [phase-only core updates](../.artifacts/perf/f6b74d5230/result.json), and [leaf check](../.artifacts/perf/4feb762c00/result.json). Their metadata records the executed private assembly hash separately from the unchanged production-provider hash.
- [Source/tool manifest](../.artifacts/csharp-profile/source-manifest.json), [phase/stack summary](../.artifacts/csharp-profile/summary.json), and [CPU calibration](../.artifacts/csharp-profile/cpu-check/result.txt).
- Logical fingerprints after both full pairs match the earlier production Release result: 132,383 symbols, 627,749 occurrences, 3,763 compilation/artifact memberships, and 59 compilation/coverage rows. Public identities, signatures, source hashes/origins, ranges, compiler options, status, and diagnostics match. See [traced facts](../.artifacts/csharp-profile/facts-core.json), [phase-only facts](../.artifacts/csharp-profile/facts-phases-only.json), and [validation](../.artifacts/csharp-profile/validation.json).
- Saved source bytes were restored and the corpus worktree is clean. Production artifacts match the previously tested Debug/Release manifest. The private instrumented provider built with zero warnings/errors; unchanged product code did not require another full integration-suite run.

## Follow-up

The subsequent [per-project SDK batching change and measurements](INDEXING_PERFORMANCE.md#batched-sdk-metadata-per-project) implement this candidate, including a regression for version hooks changing imports between targets. The profile above describes the original implementation; its source/artifact manifests are historical evidence.

The later [symbol-ID allocation pass](INDEXING_PERFORMANCE.md#c-symbol-id-allocation-pass) measures synchronous binding/declaration/ID allocations in private provider copies after SDK batching. A per-file reference cache and lazy fallback anchors reduce ID allocation without skipping semantic queries or diagnostics. Its independent, uninstrumented Release comparison preserves every Polly context and the existing logical fact fingerprints. Allocation churn and sampled resident footprint remain separate measurements.

The subsequent [binding allocation investigation](CSHARP_BINDING_PROFILE.md) separates signatures from bodies and uses zero-loss allocation traces to identify member-body setup, nullable analysis, generic substitution, and method-group construction. Its proposed invocation-query experiment remains unimplemented; product artifacts are unchanged.

## Candidate selected from this profile

Investigate one short-lived SDK metadata session **per project**, evaluating all its declared targets with independent properties. A fixed project invocation directory can preserve `MSBuildStartupDirectory`, the process-wide reserved state that invalidated the earlier whole-worker API experiment. Use the selected SDK's public evaluation APIs, keep restoration and compiler loading semantics intact, and release the session after the project. No per-repository resident compiler pool or second graph engine is required for this experiment.

Also consider sharing framework-definition results within a job when restoration has not changed evaluation inputs. Do not reuse pre-restore evaluation blindly, persist metadata without sound invalidation, substitute SDK versions, or collapse target-specific identities. Batching is a candidate to measure, not a promised elimination of the entire setup cost. It must beat the current control while preserving source/fact equality, all targets, SDK errors, and memory behavior.

After setup work, profile semantic binding/allocation inside the largest contexts. Potential reuse must stay bounded and preserve diagnostics and target-specific semantics. These observations do not justify caching every compiled context across hundreds of repositories.

Primary references: [Microsoft diagnostic CLI and profile semantics](https://learn.microsoft.com/en-us/dotnet/core/diagnostics/dotnet-trace), [pinned Roslyn loader](https://github.com/dotnet/roslyn/blob/35d9211b841e7613c1d2f8f5af6d628ace696c4c/src/Workspaces/MSBuild/Core/MSBuild/MSBuildProjectLoader.cs). Installed tool help and the actual source/trace artifacts are retained locally; documentation rates can differ by tool/runtime version.
