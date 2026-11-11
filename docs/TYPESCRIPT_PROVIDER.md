# TypeScript declaration identity and worker residency

The adapter uses repository-selected TypeScript 5.9.x and the existing configured/inferred planning path. [Implementation scope](IMPLEMENTATION.md) still applies: a first explicitly partial call/impact slice is now implemented; see [supported forms and limits](CALL_GRAPH.md). The declaration/collection measurements below predate graph activation and do not include its added facts.

Test-origin policy 1 adds inferred `.test`/`.spec`/`__tests__` evidence on owned source artifacts, with unknown compilation/default origins and exact worker acknowledgment. Owned module/initializer declaration anchors are additive; identities, calls and reference occurrences stay unchanged. See [adapter contract, snapshot behavior and focused checks](TYPESCRIPT_TEST_CLASSIFICATION.md); shared filters/backfill and final native/package validation remain common-core integration work.

## npm/pnpm preparation policy

The additive Node analysis-policy version 1 enables shared npm/pnpm preparation in the TypeScript worker. The coordinator supplies an absolute user-scoped tool cache and the effective personal workspace script grant; the adapter does not read a private trust environment variable. Absent policy retains installed-npm/scripts-disabled behavior for direct legacy requests; managed consumers now use the shared settings path in [WORKSPACE_SETTINGS.md](WORKSPACE_SETTINGS.md). The worker acknowledges policy 1 before preparation and rejects unsupported policy shapes/versions.

Supported restoration families on Node 24 are stable npm 10–11 and pnpm 10–12. Exact `packageManager` pins, optional Corepack SHA-224/SHA-512 suffixes, and exact `devEngines.packageManager` selections are honored; conflicting selections and unsupported/range/prerelease selections fail actionably without substitution. Unpinned defaults are frozen in `package-tools.mjs`: npm **11.21.2**, pnpm **12.18.0**. A pnpm lock/workspace selects pnpm when no explicit manager exists; conflicting npm/pnpm inputs without a selection fail rather than guess. Yarn restoration remains deferred.

The public npm registry supplied these frozen fixture endpoints on 2026-11-09:

| Family | Lowest stable fixture | Latest stable fixture |
| --- | --- | --- |
| npm 10 | 10.0.0 | 10.9.10 |
| npm 11 | 11.0.0 | 11.21.2 |
| pnpm 10 | 10.0.0 | 10.34.9 |
| pnpm 11 | 11.0.0 | 11.28.11 |
| pnpm 12 | 12.0.0 | 12.18.0 |

Representative endpoint testing is not exhaustive certification of every patch or every upstream package-manager feature. Versioned metadata and exact distribution integrity are retained in `.artifacts/node-preparation/registry-matrix.json` in the implementation worktree. [The focused harness](../tests/node_preparation.mjs) exercises direct framed worker requests to isolate present-policy worker behavior; assembled coordinator/Workspace API checks separately verify the integrated settings/update path.

`package-tools.mjs` shares one download/cache/lock path across managers. It downloads official registry packages, bounds metadata streams to 1 MiB before concatenation, streams archives under a 128 MiB bound, checks registry SHA-512 and any repository pin hash, checks extracted package/executable versions, and atomically publishes a complete cache entry. npm and pnpm 10/11 ship bundled JavaScript; pnpm 12 uses the official `@pnpm/exe.darwin-arm64` native package. Downloads do not execute installation scripts or install Node/.NET. Existing entries check manifest identity/platform and the executable digest before reuse. Acquisition is deduplicated across processes, with a bounded five-minute lock wait; metadata/archive timeouts are 30/180 seconds. Unique owner filenames protect stale-lock reclamation from deleting a newly acquired live lock, and dead-owner staging is reclaimed. Failures do not publish partial tool entries.

Repository lifecycle scripts and pnpmfile hooks are forced off unless the coordinator supplies a personal grant. A grant removes Know Code's forced suppression while retaining stricter repository/manager restrictions; it does not force all dependency builds. npm audit/funding/update checks and debug-log retention are disabled for restoration; pnpm update notification is explicitly disabled through command configuration across families. Checkpoints bind manager/version, runtime, dependency/workspace/lock/config inputs and policy. Source-only saves reuse successful or failed checkpoints; missing successful outputs, changed inputs/policy or explicit retry cause preparation. Optional-only dependencies trigger an initial setup attempt, while legitimately omitted optional/platform packages do not trigger repeated restoration. Previously installed optional outputs remain tracked for removal/recovery.

With scripts disabled, npm 10 Git dependencies are refused (`EKNOWCODE_NPM10_GIT_DISABLED`), and effective `install-links=true` is rejected before installation because upstream preparation can bypass script suppression. Select npm 11, restore affected dependencies separately, or explicitly grant package scripts for the workspace. Registry packages, tarballs and ordinary directory links remain supported; directory links require effective `install-links=false`. The npm 10 guard suppresses linked-package preparation while preserving dependency and executable links. Know Code checks the selected npm's effective configuration and reports the checked policy without rewriting configuration files.

Effective selected tool/source, authorization and install root appear in compilation options; package/config paths join compiler inputs, including absent supported candidates with explicit missing markers. INI `6.0.0` and YAML `2.9.1` parse declarative repository/user hook paths without executing them; missing/existing externally selected hook paths are tracked too. INI 7 was deliberately avoided because it would raise the Node 24 patch-level requirement. Configuration contents are hashed for required setup state, not copied into graph facts or usage telemetry.

Locked restoration uses npm `ci` or pnpm frozen-lockfile and never repairs/upgrades a source lockfile. Unlocked restoration does not create one. Failed setup remains an explicit compiler-coverage diagnostic and preserves valid partial compiler facts. Declared managed-runtime installation requests are rejected with a separately-installed-Node error; native pnpm also disables runtime installation. All preparation stays in the existing worker process tree and shared core checkpoint/publication path.

Actual upstream gotchas found in the fixture runs:

- pnpm 12's native CLI rejects several JavaScript-era boolean flag spellings. Configuration overlays handle lockfile/production settings across families; script and hook suppression use explicit supported flags.
- Invoking the exact native pnpm pin can still trigger an independent package-manager bootstrap from the public registry. After Know Code has verified the exact pin itself, pnpm 11/12 commands use official [`with current`](https://pnpm.io/cli/with) to suppress that second bootstrap; this is essential for warm-cache offline use. The old `managePackageManagerVersions` option was replaced in [pnpm 11](https://pnpm.io/blog/releases/11.0).
- pnpm can skip lifecycle hooks on an unchanged no-op install. Authorization invalidates preparation but does not promise that the manager will repeat an already completed script; missing-output restoration is tested separately.
- pnpm 10's up-to-date path can skip creating a requested author lockfile after lockfile-disabled installation. The harness authors fixture locks without installed modules before testing frozen restoration.
- pnpm 11.0.0's own lockfile-only output for the initial local `.tgz` fixture omitted the tarball resolution and attempted a registry fetch on frozen reinstall. The failure/partial graph was visible and the lock stayed unchanged. The shared frozen-lock matrix uses local-directory dependencies; Know Code does not rewrite upstream lock metadata to hide that issue. That reproducer remains under `.artifacts/node-preparation/1a1c274b519/` in the implementation worktree.

The matrix covers single packages/workspaces, compiler graphs, script/hook suppression despite repository settings, personal authorization, ordinary-save and failed-setup deduplication, explicit retry, missing outputs, unchanged frozen locks, partial graphs and repair. Additional checks cover offline tool reuse/cold failure, unsupported pins, corrupted caches and two real processes sharing one synthetic-registry acquisition. Actual upstream acquisition and synthetic concurrency evidence remain distinct. Coordinator-based invalidation/deduplication and final packaged integration remain integrator-owned gates; this adapter slice does not complete C09 by itself.

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
