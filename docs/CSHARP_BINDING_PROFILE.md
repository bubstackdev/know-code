# Roslyn binding allocation investigation

This follow-up profiles the production emitter **after** the [per-file symbol-ID optimization](INDEXING_PERFORMANCE.md#c-symbol-id-allocation-pass). At the time of these measurements, product sources, binaries, compiler settings, and telemetry behavior were unchanged. All probes, launchers, traces, and analysis tools live under `.artifacts/binding-profile/`; pinned upstream reference source is under `.research/sources/roslyn/35d9211b841e7613c1d2f8f5af6d628ace696c4c/` with its MIT license. The later [invocation lookup implementation and comparisons](INDEXING_PERFORMANCE.md#c-invocation-reference-lookup) implement the narrow candidate below; this profile remains the preceding baseline.

The important result is that binding still allocates roughly **2.6 GiB per Polly core update**. A small number of expensive lookups dominate. Both initial member-body work and later method-group queries contribute; a larger ID cache would not address those costs.

## Workload and method

The existing pinned Polly worktree, established SQLite index, comment-save/restore probe, Release Know Code build, and Debug C# analysis configuration remain the same. Each update recompiled 53 target contexts and preserved all 59 complete workspace contexts. This remains one public workload on the M5 Max test machine, not a hardware requirement or estate-scale acceptance.

A private copy of the provider wraps the existing `GetSymbolInfo` and `GetDeclaredSymbol` calls. Allocation and timestamp snapshots bracket only the synchronous Roslyn call. Classification, dictionaries, and output generation happen afterward. These counters cover the calling thread, including work Roslyn triggers there; they exclude allocations on other threads. Whole-job allocation includes probe bookkeeping and background work and is not an exclusive Roslyn attribution. No symbols or semantic models are cached by the probe; syntax references used for classification are cleared after each file.

The initial counter-only pair grouped by enclosing method/member/lambda. That proved too coarse: a cheap return-type lookup can precede an expensive body lookup in the same method. The refined probe distinguishes method/accessor blocks, expression bodies, initializers, and signatures. These are **lexical observation buckets**, not Roslyn's internal binding roots. Its first-lookup percentages must not be compared directly with the coarse pair's percentages. Both source manifests are retained.

Two refined runs collected allocation stacks with pinned `dotnet-trace` 10.0.750501, runtime provider `Microsoft-Windows-DotNETRuntime:0x1:5`, and a 64 MiB diagnostic buffer. They reported 33,188/32,309 lost events. Those traces are retained as preliminary evidence. A second pair with a **256 MiB diagnostic buffer reported zero lost events** in both raw EventPipe and converted TraceLog accounting; it supplies the stack table below. Buffer capacity is private profiling overhead, not a proposed product memory budget. Three update pairs were collected in total; none supplies uninstrumented product latency measurements.

The private reader uses TraceEvent 3.1.21, pinned in its own lockfile outside the product build. It groups allocation ticks by allocated type and by the presence of frames within binding/declaration stacks. The [runtime event documentation](https://learn.microsoft.com/en-us/dotnet/fundamentals/diagnostics/runtime-garbage-collection-events#gcallocationtick_v3-event) describes approximately 100 KB allocation ticks: the type/stack belongs to the allocation crossing that threshold. Weighted type/stack bytes are therefore **sampled estimates**, not an exact census of objects or CPU samples. Categories are inclusive and overlap.

## Direct allocation counters

All six updates made **509,001 binding calls** and **124,834 declaration calls**. Binding allocated 2,662–2,682 MiB on the calling thread; declarations allocated 254–258 MiB. The counter-only pair measured 2,680/2,674 MiB for binding, consistent with the traced runs. Allocation here means cumulative churn, not retained memory.

| Observation | Zero-loss traced save | Zero-loss traced restore |
| --- | --- | --- |
| Binding calling-thread allocation | 2,673.5 MiB | 2,661.8 MiB |
| Calls allocating at least 64 KiB | 7,479 | 7,479 |
| Share of binding allocation in those calls | 63.2% | 63.6% |
| First binding lookup in a refined lexical scope: allocation share | 62.0% | 62.2% |
| Later binding lookups: allocation share | 38.0% | 37.8% |
| Binding calls with zero measured allocation | 220,908 | 220,925 |

About **1.5% of calls drive 63% of binding allocation**, while roughly 43% allocate nothing on the calling thread. The largest individual calls allocate about 5–6 MiB. Many occur at a body's first `var` type or member access. This attributes work to the query that triggers it; it does not mean resolving the spelling `var` itself inherently needs megabytes, or that skipping that query removes the underlying work. Later queries may trigger the same lazy body setup.

The retained results also distinguish exact symbols from lookups without a single symbol. A null `SymbolInfo.Symbol` is not itself proof of a compiler error; method groups and other syntax contexts can lack one exact result. No candidate was promoted to an exact binding, and coverage was not inferred from these probe counters.

## Allocation stacks

The zero-loss runs contained 73,900/74,002 allocation ticks across the worker. Only 1/4 ticks lacked a stack. Binding-stack weights totaled about 2,614 MiB per run, close to but distinct from the direct calling-thread counters.

| Frame present in a binding allocation stack | Save weight share | Restore weight share |
| --- | --- | --- |
| `MemberSemanticModel.GetBoundNodes` | 82.7% | 83.0% |
| `EnsureNullabilityAnalysisPerformedIfNecessary` | 56.5% | 57.0% |
| `OverloadResolution` | 20.9% | 19.2% |
| `TypeMap` | 17.1% | 18.3% |
| `NullableWalker` | 14.8% | 15.1% |
| `GetReducedAndFilteredMethodGroupSymbols` | 11.8% | 11.6% |
| `NodeMapBuilder` | 2.5% | 2.5% |

**Do not add these percentages.** For example, nullable walking and overload resolution can occur inside initial body setup; a `TypeMap` can occur inside method-group reduction. These are allocation sample weights, not elapsed-time or CPU percentages.

Prominent sampled allocation types include type-parameter substitution tree nodes, `TypeWithAnnotations[]`, constructed named/method symbols, small symbol dictionaries, substituted parameters, and bound-node map arrays. These support a compiler binding/type-substitution explanation rather than a remaining ID-string-only bottleneck. Raw type estimates remain in the per-run `allocations.json`; their threshold sampling does not establish exact bytes per object type.

The matching [Roslyn member-model source](https://github.com/dotnet/roslyn/blob/35d9211b841e7613c1d2f8f5af6d628ace696c4c/src/Compilers/CSharp/Portable/Compilation/MemberSemanticModel.cs#L1951) explains initial work: nullable-aware semantic information can require binding the complete member, running analysis, recording snapshots, and caching bound nodes. This is consistent with the measured first-body concentration, but does not prove every expensive lookup has the same cause. Disabling nullable analysis would change semantic behavior and is not an accepted optimization.

## Narrow candidate for the next experiment

The pinned [method-group implementation](https://github.com/dotnet/roslyn/blob/35d9211b841e7613c1d2f8f5af6d628ace696c4c/src/Compilers/CSharp/Portable/Compilation/CSharpSemanticModel.cs#L4250) constructs a reduced/filtered method group before returning the selected method from a parent call. Its extension path also performs type inference/substitution. We request only the exact symbol, but querying the method name can still incur this group construction.

**Proposal selected at the time of this profile:** in narrowly recognized invocation positions, test querying the invocation node for the chosen method and reuse that compiler result for the method-name occurrence. Preserve the existing name-based path for delegates, events, dynamic/function-pointer calls, unresolved or ambiguous calls, method groups used as values, and any unsupported syntax. Handle extension normalization and generic/conditional/local-function cases explicitly. No name-only cache, guessed target, operation-walking engine, or skipped declaration/diagnostic pass is proposed.

The observed group-construction share is a reason to experiment, not a promised saving: it overlaps other work, some queries will still need fallback, and another public API can introduce its own costs. A private candidate must match identities, signatures, occurrence ranges, sources, diagnostics, and all targets before any production change. Follow allocation measurements with an adjacent uninstrumented control and peak-memory check. Broader compiler-native incremental reuse remains a separate design issue with lifetime/invalidation/resource tradeoffs; these results do not justify retaining every repository's compiler workspace.

## Evidence and verification

- [Counter-only pair](../.artifacts/perf/58a892e348/result.json), [preliminary 64 MiB traced pair](../.artifacts/perf/8543487a70/result.json), and [zero-loss 256 MiB traced pair](../.artifacts/perf/c195d497b0/result.json). Metadata identifies executed private providers separately from the product assembly recorded by the harness. Timings include probe/trace overhead and are not product performance results.
- [Direct counters and sampled summaries](../.artifacts/binding-profile/summary.json), [refined source manifest](../.artifacts/binding-profile/source-manifest.json), and [coarse source manifest](../.artifacts/binding-profile/coarse/source-manifest.json). Each run retains its original counters, launch details, and trace log; traced runs also retain raw `.nettrace`, converted `.etlx`, and allocation summaries.
- [Counter facts](../.artifacts/binding-profile/facts-counters.json), [preliminary traced facts](../.artifacts/binding-profile/facts-traced.json), and [zero-loss traced facts](../.artifacts/binding-profile/facts-traced-256.json) match the preceding production fingerprints: 132,383 symbols, 627,749 occurrences, 3,763 compilation/artifact memberships, and 59 compilation/coverage rows. The comparison includes public IDs/signatures, ranges, source hashes/origins, compiler options, status, and diagnostics, with the existing fingerprint limitations. It says nothing about unimplemented relationship extraction.
- The corpus source was restored cleanly. Private provider/reader builds completed with zero warnings/errors. The [validation record](../.artifacts/binding-profile/validation.json) checks production binaries against the previous tested artifacts and source hashes against the start-of-investigation manifest. It records all registered workspaces idle/current, including Polly 59/59 complete. No product code changed, so full integration suites were not repeated for this profiling-only task.

To reproduce the private provider, run `python3 .artifacts/binding-profile/prepare.py`, then build its copied csproj in Release with `-p:RestoreLockedMode=true`. Set the launcher's `KNOW_CODE_PROFILE_DOTNET`, `KNOW_CODE_PROFILE_ORIGINAL_PATH`, and durable `KNOW_CODE_PROFILE_RUN`, and prepend `.artifacts/binding-profile/bin` to PATH for the existing Polly update harness. `KNOW_CODE_PROFILE_TRACE=1` enables tracing; `KNOW_CODE_PROFILE_BUFFER_MB=256` selects the measured zero-loss buffer. The counter-only mode uses the current refined probe, unlike the retained initial coarse pair. The [launcher](../.artifacts/binding-profile/bin/dotnet), [probe generator](../.artifacts/binding-profile/prepare.py), [trace reader](../.artifacts/binding-profile/reader/Program.cs), and [summary script](../.artifacts/binding-profile/summarize.py) retain the exact mechanics without adding a product profiling feature.
