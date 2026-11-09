# TypeScript indexing profile

Historical profiling below preceded the [implemented declaration-root fix and accepted worker collection policy](TYPESCRIPT_PROVIDER.md). Its unchanged-product statements describe that earlier investigation.

This profiles the existing TypeScript worker after the [C# compiler-reuse work](INDEXING_PERFORMANCE.md#c-compiler-reuse-within-one-job). **No product code, compiler option, identity policy, or binary changed.** Private probes and experiments live in `.artifacts/typescript-profile/`. The existing ts-pattern graph was restored to ordinary-worker facts after the experiments.

The main findings are expensive compiler-backed signatures, considerable collectible heap between programs, and three check-order-dependent reference identities. A simple formatting cache has little measured opportunity. Reordering diagnostics is not an output-compatible optimization.

This profiling run completed before the subsequent shared Node codec extraction for Swift/Rust workers. Its [exact original TypeScript worker](../.artifacts/typescript-profile/original/worker.mjs) is archived and verified against the profiling manifest's SHA-256. The private generator now reads that archive; the unchanged-source validation describes the completed profiling run, not later product revisions.

## Workload and instrumentation

The pinned ts-pattern worktree is at `f11be73acb18e1d653d4f252e7d75981d873a2e6`. The repository-selected compiler is **TypeScript 5.9.2**, running on Node **24.22.2**, V8 **13.6.233.17-node.63**, on the same M5 Max test machine. The provider fallback compiler was not substituted. Each `src/match.ts` comment-save/restore probe reanalyzed four contexts and retained six overall: three complete and three explicitly partial. Missing React/example dependencies, example compiler errors, and inferred-module diagnostics remain visible; partials were not removed to improve performance figures.

The refreshed contexts are the compile-time benchmarks, tests, root source config, and inferred benchmarks. Planning still considers configured ownership before inference. The retained [context inventory](../.artifacts/typescript-profile/contexts.json) identifies all six contexts and their diagnostics.

The private worker measures configuration/planning, program creation, checker creation, extraction, diagnostics, and synchronous hot-call wall time. `process.cpuUsage()` covers all worker threads; it can exceed elapsed time because V8 uses background threads. Endpoint heap/RSS snapshots are not peak measurements. Heap deltas are **net changes**, not allocated bytes. Hot timers and phases overlap: signatures, offsets, codec work, and writes occur inside extraction and must not be added to it again. Lazy compiler work is charged to the API that triggers it.

One pair uses phase probes only. Another adds [V8 CPU profiles](https://nodejs.org/api/cli.html#--cpu-prof) at a 1,000-microsecond interval and sampled allocation stacks at a 1 MiB interval. The inspector connects in-process without opening a listening debugger port. Both `includeObjectsCollectedByMajorGC` and `includeObjectsCollectedByMinorGC` are enabled through [HeapProfiler sampling](https://chromedevtools.github.io/devtools-protocol/tot/HeapProfiler/#method-startSampling). A 300,000-record calibration on the installed runtime produced no retained allocation samples after collection with those flags disabled, versus roughly 48 MiB of allocator-attributed estimates with them enabled. This checks that the profile includes collected allocations; it is not an exact byte census.

CPU percentages below are V8 sampled main-thread interval weights, not kernel CPU shares or all-thread process CPU. Heap stack bytes are sampled estimates, not retained heap or resident memory. GC durations can overlap other work. Original profiler files, source manifests, calibration, and counters are retained; no product collector was added.

## Phase-only results

| Stage per saved-file update | Save | Restore |
| --- | --- | --- |
| Entire measured worker | 5.26 s | 5.19 s |
| Planning, including initial compiler loading | 0.067 s | 0.065 s |
| Create four programs | 0.602 s | 0.496 s |
| Obtain their checkers | 0.177 s | 0.177 s |
| Extraction, including lazy semantic work and output | 3.75 s | 3.80 s |
| Final pre-emit diagnostics | 0.629 s | 0.621 s |
| Nested: symbol lookup, 38,178 requests | 0.662 s | 0.739 s |
| Nested: signature type resolution, 13,760 calls | 1.51 s | 1.51 s |
| Nested: signature formatting, 13,760 calls | 1.37 s | 1.38 s |

Signatures account for roughly **55% of this instrumented worker's elapsed time**. A small final diagnostic phase does not mean full type checking is cheap: signature queries already trigger much of the expensive inference. Symbol-ID construction took 59–66 ms, UTF-8 offset conversion 13 ms, encoding 35–36 ms, and pipe writes 17–56 ms. Each update emitted 494 frames. These measurements do not support making those smaller paths the first optimization target.

The test context dominated extraction, at 3.06/3.13 seconds. Its compiler reported 484,300 types and 3,591,721 instantiations. Compile-time benchmarks reported 102,237 types/624,478 instantiations; root source reported 45,653/144,947; inferred benchmarks reported 598/268. Counts are per-program compiler counters, not unique objects across programs or allocation sizes. Tests remain required indexed code.

## CPU and allocation stacks

The private stack pair estimates **6.37/6.42 GiB of allocated heap bytes** per update, including collected objects. Signature type resolution accounts for about 28–29% of sampled byte weights and formatting for 20–21%; binding contributes 15–16%. Prominent allocator stacks involve `checkTypeRelatedTo`, generic/object instantiation, conditional types, maps, inference, and recursive type relationships. This supports a compiler semantic-work explanation rather than a protocol/ID-string bottleneck. These stack categories are inclusive and should not be summed as independent accounting totals.

The unchanged original worker was also run under V8's CPU profiler, without the private phase/formatting wrappers. It likewise placed roughly half of sampled main-thread time beneath `getTypeOfSymbolAtLocation` and `typeToString`; this cross-check supports the hotspot finding. See [stock-worker CPU summaries](../.artifacts/typescript-profile/stock-cpu-summary.json).

Absolute timings varied substantially: the early phase-only workers took about 5.2 seconds, whereas ordinary and stock-profiler runs later took considerably longer. Instrumentation, V8 execution/GC behavior, and host variation were not isolated as causes. **These runs cannot establish an optimization speedup or a stable latency baseline.** Raw ordinary-run and profiler-run measurements are retained separately in the [validation manifest](../.artifacts/typescript-profile/validation.json).

## Experiments that narrow the next step

### Repeated type formatting

Of 13,760 formatting calls, 3,534 distinct type objects were observed within their respective checkers. Despite many repeats, repeated formatting consumed only **22–27 ms**; nearly all formatting time was on first use. A weak identity cache might have a high hit rate while saving less than 1% of worker time in this workload. It is not the recommended first optimization. Formatting still requires correctness checks because compiler state can be lazy.

### Program-boundary collection

In ordinary phase-probed execution, endpoint used heap rose from roughly 26 MiB to 1.3–1.5 GiB across sequential programs. A private event-loop yield between programs did little to reduce this. A private `--expose-gc` experiment collecting after `analyze()` returned reduced heap from approximately 314–316 MiB to 32 MiB after the benchmark program, and from roughly 895–903 MiB to 34 MiB after tests. Its sampled aggregate peak was about 1.12/1.15 GiB. Yield-only samples were about 1.81/1.48 GiB.

All fact fingerprints matched in both experiments. Collection took approximately 3–11 ms at each boundary in these instrumented observations. This demonstrates that much of the completed-program heap is collectible; it does **not** prove that RSS falls to 34 MiB, guarantee a memory ceiling, or justify forcing GC in production. No GC flag, collection call, heap cap, process split, or resource policy was added to the product. A memory-management prototype needs ordinary-build CPU/latency/peak measurements and the shared resource-budget design.

### Diagnostics before extraction: rejected

Moving `getPreEmitDiagnostics()` before extraction made the output different. The observed symbol count changed from 14,011 to 14,014; occurrence count stayed 38,510, but three occurrence identities changed. A repeat reproduced the mismatch. Diagnostics, compiler options, source artifacts, and coverage fingerprints stayed equal.

The exact [snapshot comparison](../.artifacts/typescript-profile/order-difference.json) separates display and identity differences. There are 185 baseline-only versus 188 diagnostics-first symbol rows; many changes are union/signature ordering. Three references in the tests context change a property flag value from `4` to `33554436` (`4 | 33554432`, adding TypeScript's `Transient` bit). The current `symbolId()` includes flags, so those references acquire different IDs despite the same declaration anchor and source range. Observed examples include `input` in an exhaustive-match test and `foo`/`baz` in an instance-of test.

This establishes **check-order-dependent identity output**, not which compiler view should be considered canonical or proof that either resolves to a different source declaration. Transient/instantiated views, declaration identity, kind, signatures, and reference evidence need separate treatment before changing the ID formula. Do not fix this by sorting display strings, discarding all flags, or silently switching analysis order. The rejected experiment and both database snapshots are retained. The original worker restored the previous graph fingerprints afterward.

A subsequent [private identity probe](../.artifacts/typescript-identity/probe.mjs) reproduced those three changes in the tests compilation while preserving the original extraction/signature-call order. TypeScript 5.9.2's `checker.getRootSymbols()` returned the same single non-transient declaration symbol for each affected reference in both checking orders; the retained [after-check](../.artifacts/typescript-identity/after.json) and [before-check](../.artifacts/typescript-identity/before.json) observations show the exact anchors. The compiler API therefore supplies useful canonicalization evidence for these cases. This does not establish a universal identity policy: synthetic union/intersection/spread properties can have multiple roots, and a declaration identity must remain separate from an instantiated signature/view. No product identity or signature behavior was changed by this follow-up.

## Evidence and next decision

The [summary](../.artifacts/typescript-profile/summary.json) contains per-context timings, heap snapshots, compiler counters, GC events, and CPU/allocation rankings. [Exact initial phase sources](../.artifacts/typescript-profile/phases-source/worker.mjs) are retained with their original checksums; current private probe/experiment sources remain alongside them. [Experiment results](../.artifacts/typescript-profile/experiments.json) explicitly mark the diagnostics-first mismatch. Use the retained launcher/generator/summary scripts to reproduce these private investigations; they are not a public profiling feature.

Phase, stack, yield, forced-collection, and restored ordinary-worker fingerprints match the preceding production result: **14,011 symbols, 38,510 occurrences, 124 compilation/artifact memberships, and six coverage rows**. Existing partial coverage remains explicit. The worktree is restored cleanly; source/binary hashes match the previously tested C# compiler-reuse build. Full product suites were not rerun because no product code changed. The [validation record](../.artifacts/typescript-profile/validation.json) includes raw run paths, source manifests, calibration, unchanged artifacts, restored facts, and installed coverage.

Recommended next investigation: define and verify canonical TypeScript symbol identity for transient compiler views, preserving declaration/use relationships and useful signature evidence. That resolves the compatibility issue exposed by analysis-order changes before choosing signature or memory optimizations. Product identity policy and runtime memory-management changes remain unaccepted proposals here.
