# TypeScript declaration identity and worker residency

The adapter uses repository-selected TypeScript 5.9.x and the existing configured/inferred planning path. [Implementation scope](IMPLEMENTATION.md) still applies: a first explicitly partial call/impact slice is now implemented; see [supported forms and limits](CALL_GRAPH.md). The declaration/collection measurements below predate graph activation and do not include its added facts.

## Declaration symbols versus compiler views

TypeScript can return transient instantiated symbols for a use such as `Box<string>.item`. The graph symbol represents the declaration `Box<T>.item`; its signature describes that declaration (`T`), rather than the first visited instantiated use. Occurrences retain exact source ranges and compilation ownership. Per-occurrence instantiated type display is not currently represented by the fact contract.

The narrow [declaration selector](../providers/typescript/symbols.mjs) consults public `checker.getRootSymbols()` for transient views. It substitutes a root only when the compiler supplies **one non-transient symbol with declarations**. ID, kind, external classification and signature all come from that same symbol. It does not remove arbitrary flag bits, select a union/intersection branch, sort signatures, or reorder diagnostics. Multiple-root synthetic views retain the existing encoding; comprehensive identity for those views remains unfinished.

This fixes the three check-order-dependent identities in [earlier profiling](TYPESCRIPT_PROFILING.md) and consolidates instantiated duplicates. Pinned ts-pattern symbol rows decrease from 14,011 to **13,920**; all **38,510 occurrences**, 124 compilation/artifact memberships and six coverage reports remain. Coverage stays three complete/three partial. The 2,777 rebound occurrences are explained by 168 compiler-root mappings, all with identical declaration anchors. Across this corpus, exact occurrence identities and symbol identity/kind/external fields now match with diagnostics before or after extraction. Signature display ordering can still differ; diagnostics order remains unchanged.

The [focused regression](../tests/typescript.mjs) covers generic/constructor parameter properties, union/intersection views, spread, merged declarations, aliases, shorthand property/value occurrences and Unicode offsets with the installed provider compiler. Real-corpus evidence uses repository-selected TypeScript 5.9.2. This is not universal identity stability or refactoring continuity.

## Collect completed compiler programs

The user accepted explicit garbage collection between completed compilations. The Rust launcher passes `--expose-gc` **only to the isolated TypeScript worker**. The worker checks that capability and collects after `analyze()` returns its plain file/reference paths. No compiler program/checker/source graph is intentionally cached across that boundary. Inferred groups use the same boundary. Analysis failure still aborts atomic publication.

Node embedding applications receive neither the flag nor a GC call. Compiler selection is unchanged. Collection is not a memory ceiling or the shared resource-cap implementation; within-compilation peaks remain. Other providers retain their own compiler lifetimes.

## Ordinary-process measurements

Private runners and exact outputs live in [`.artifacts/typescript-declarations`](../.artifacts/typescript-declarations/). They execute six contexts with synchronous fact output, excluding protocol/storage/coordinator costs. No inspector, allocation profiler, heap cap, cache flush or product collector is involved. Environment: M5 Max, Node 24.22.2, repository-selected TypeScript 5.9.2. Other machine activity was not isolated; two observations per variant do not establish performance gates.

| Variant | Elapsed seconds | All-thread CPU seconds | Maximum process RSS, GiB |
| --- | --- | --- | --- |
| Previous view identity, A1/A2 | 8.45 / 7.59 | 14.06 / 12.74 | 1.514 / 1.510 |
| Declaration roots, B1/B2 | 7.59 / 7.37 | 12.93 / 12.50 | 1.494 / 1.480 |
| Roots + collection, C1/C2 | 7.63 / 7.61 | 13.51 / 13.61 | 1.087 / 1.086 |

Collection reduces observed maximum RSS about **27%**, with about **2%** more elapsed time and **7%** more CPU relative to B1/B2 averages. Collection takes 3–26 ms per context. Node `process.resourceUsage().maxRSS` reports KiB; this is not retained heap or aggregate coordinator/child memory. Collected and uncollected emitted facts match exactly. [Comparison](../.artifacts/typescript-declarations/comparison.json) and [root validation](../.artifacts/typescript-declarations/root-validation.json) retain checks. Declaration-only timings overlap enough that no latency gain is claimed for that fix.

Old IDs can change on refresh; store/wire formats do not change. Existing indexes need explicit refresh; provider-code-upgrade invalidation remains unfinished. Restart the development coordinator after rebuilding so worker launch arguments include the new flag.

## Coordinator/store smoke measurement

The subsequent [Debug end-to-end run](../.artifacts/perf/0230ce3558/result.json) includes the real worker protocol, coordinator, SQLite publication and sampled child processes. A comment save and its restoration took **6.63 / 6.26 seconds**, reanalyzing four contexts and retaining six. Sampled aggregate physical footprint peaked at **1.11 / 1.12 GiB**; aggregate RSS was **1.09 / 1.09 GiB**. This uses 250 ms samples, not a guaranteed peak or resource cap. Fresh registration took 24.18 seconds including 15.33 seconds attributed to dependency preparation. Coverage remained three complete/three partial. [Served fact fingerprints](../.artifacts/typescript-declarations/facts-served.json) contain 13,920 symbols, 38,510 occurrences and 124 artifact memberships. The copied worktree was restored cleanly. There is no matched old-product run in this observation, so these numbers establish pipeline operation, not an end-to-end speedup.
