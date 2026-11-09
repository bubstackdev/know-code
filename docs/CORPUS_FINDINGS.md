# Indexing corpora

These repositories exercise real TypeScript/C# compiler analysis and expose implementation gaps. They are not a product comparison or performance acceptance suite. Know Code itself is also registered with the development coordinator. The exact public source revisions are retained in [corpora.json](../.research/corpora.json); ignored clones stay under `.research/repos/`.

| Corpus | Pinned revision | License | Useful coverage |
| --- | --- | --- | --- |
| [ts-pattern](https://github.com/gvergnaud/ts-pattern/tree/f11be73acb18e1d653d4f252e7d75981d873a2e6) | `f11be73acb18e1d653d4f252e7d75981d873a2e6` | MIT | Advanced generic types, library/tests, benchmarks, nested example projects, React/TSX |
| [Polly](https://github.com/App-vNext/Polly/tree/289f29808f70f49d3d46de57f42fcd1139bc2bd0) | `289f29808f70f49d3d46de57f42fcd1139bc2bd0` | BSD-3-Clause | Multiple C# libraries, tests, samples/benchmarks, project references, modern and .NET Framework targets |

## Setup and boundaries

ts-pattern's root lockfile selects TypeScript 5.9.2, within the current provider's supported 5.9.x line. Its five `tsconfig.json` projects are discovered, including tests and examples. Root dependencies were restored with `npm ci --ignore-scripts --no-audit --no-fund`; the separate one-file example was restored with `npm install --ignore-scripts --no-audit --no-fund`. The latter creates a local lockfile because upstream does not provide one there. No source/configuration files were changed to make analysis pass.

Normal installation of `examples/gif-fetcher` fails with npm ERESOLVE: its TypeScript `^5.4.4` requirement resolves to 5.9.3, while `react-scripts@5.0.1` declares an optional peer requirement of TypeScript 3/4. No force flag, dependency downgrade, or peer-check bypass was applied. The [restore error](../.artifacts/corpus/ts-pattern-gif-fetcher-restore-error.txt) is retained locally. Missing dependencies consequently remain legitimate partial-coverage evidence for that example.

Polly's `global.json` selects SDK 10.0.401. Normal SDK feature-band resolution selects the installed 10.0.403; no selection file was changed. Its core libraries declare `net10.0`, `net8.0`, `net6.0`, `netstandard2.0`, `net472`, and `net462`. Other projects have their own targets. Registration uses Debug and requests every declared target through the existing provider. This includes tests; runtime installation/execution of every target is not required merely to analyze its compiler inputs.

The coordinator performs NuGet restoration. No upstream full build, benchmark, test suite, or external code-generation script was invoked for either corpus. Standard restore/project evaluation and compiler-integrated generators remain part of indexing. These clones retain their upstream licenses; no implementation code has been copied into Know Code.

## Initial observations

The initial ts-pattern publication used TypeScript 5.9.2 in all five compilations: the root library, tests, and compile-time benchmark were complete; both examples were partial before their separate dependency setup. It contained 13,897 symbol rows across compilation contexts, not 13,897 distinct semantic entities. Coverage is compiler-reported and must remain separate from freshness. Later setup and the fix validation below are separate from this initial observation.

Know Code's own registered workspace published complete reports for five C# project/target compilations and the configured TS fixture. This does not imply Rust source analysis or JavaScript analysis: those are outside the current compiler providers. At initial capture, discovery only created inferred TypeScript when no explicit config existed. The fix below adds mixed configured/inferred coverage; Rust/JavaScript remain outside the current source providers.

Polly subsequently published 59 project/target compilations: 40 complete and 19 partial. Partial reports include project-loading/reference gaps, missing types, argument-resolution errors, and assembly-version mismatches. Its updated generation 3 has published too; do not infer complete coverage from successful publication. Use live coordinator status and query coverage to distinguish setup progress, failure, partial publication, and completion.

CLI smoke queries found `match` in ts-pattern and `QueryRequest` in Know Code; reference queries and retained source retrieval succeeded, and the returned source hashes/ranges matched their symbol locations. ts-pattern correctly returned stale metadata while its post-setup update was queued. Polly returned `not_ready` because it had no published generation yet. Bounded status/query evidence is retained locally in [status.json](../.artifacts/corpus/status.json) and [queries.json](../.artifacts/corpus/queries.json). These are observations from the initial setup, not promises about later live status or query latency.

## Fixes implemented from the corpus

1. **Evaluated MSBuild paths.** C# restoration moved out of Rust's path heuristics and into the existing provider. It evaluates `ProjectAssetsFile`/`MSBuildProjectExtensionsPath` for every declared target/configuration when preparation is needed, then caches output paths. An outer multi-target evaluation can leave `ProjectAssetsFile` empty; per-target evaluation avoids that trap. A regression uses centralized artifacts, confirms there is no project-local assets file, and counts actual restores across source saves.
2. **Required progress and useful errors.** Version 3 of the existing Protobuf contract adds bounded progress and preparation outcomes. The coordinator exposes restoration/analysis stage and recent output through status. NuGet error messages retain stdout and stderr; npm failures retain a bounded output tail. Checkpoints live centrally next to each index, are written by Rust, and survive failed analysis without becoming graph facts or usage telemetry.
3. **Nested npm restoration.** The TS adapter identifies independent package and npm-workspace boundaries and deduplicates preparation per install root. Existing lockfiles use `ci`; unpinned packages install without creating a lockfile. Lifecycle scripts are disabled. Conflicts remain explicit dependency diagnostics alongside compiler partial coverage. Failed attempts are reused on source-only saves, retried after changed preparation inputs or explicit refresh; a successful npm exit with still-missing direct dependencies is also a visible gap. Explicit npm pins are checked; other package managers receive an actionable unsupported-restoration message rather than an npm substitution.
4. **Mixed configured/inferred TypeScript.** Config roots and referenced configurations establish explicit ownership. Remaining saved TS files are inferred by package boundary, using the effective compiler and reporting inferred options. Shared replacement scopes remove/recompute inferred units as ownership changes. Malformed explicit configs still fail without being silently replaced by inference.

The full build, five Rust tests, existing compiler/native and coordinator suites, and the new [preparation regressions](../.artifacts/prep/latest.json) pass. Formatting, Clippy with warnings denied, JS syntax, and native TypeScript declarations were checked. Retained corpus status/query snapshots distinguish original setup from [post-fix coverage](../.artifacts/corpus/after-fixes.json). Generation numbers are observation identifiers, not a stable live-state guarantee.

The real npm restore additionally exposed a **watcher/restore feedback loop**: dependency events could overflow the watcher queue; rescan was incorrectly treated as a forced analysis and forced restore, generating more events. The service was paused for repair and reactivated after a regression passed. Ignored dependency/build-output paths are now filtered before queueing when the state lock is available; filtering never blocks the OS watcher. Overflow/rescan rehashes/reconciles inputs but does not force new facts or retry preparation. Explicit refresh retains its separate retry behavior. A 6,000-file dependency-event flood checks stable publication and restore checkpoints; source/config saves and explicit retries still work through the same scheduler.

A subsequent own-repository query found another **saved generated-input tracking gap**: the provider's `obj/protocol/Analysis.cs` changed during protocol generation, but the broad discovery exclusion hid that change. Physical generated documents and analyzer/additional inputs explicitly reported by the compiler now bypass discovery exclusions and participate in invalidation. General directory traversal still skips output/dependency directories. The regression changes an explicit C# `Compile` input under `obj`, checks old/new symbols, and verifies no restoration. Virtual source-generator documents continue to derive from saved inputs rather than becoming file watchers.

## Latest verification snapshot

After reconciliation, all three registered workspaces were idle/current and kept the same generations across a quiet interval. Symbol, reference, and exact retained-source checks succeeded, and query coverage exposed effective `analysis_options`.

| Workspace | Observed generation | Complete compilations | Partial compilations |
| --- | --- | --- | --- |
| ts-pattern | 28 | 3 | 3 |
| Polly | 5 | 40 | 19 |
| Know Code | 8 | 7 | 0 |

The TS partials cover the unresolved React demo dependency conflict, legacy Node typings in the one-file example, and compiler diagnostics under the explicitly inferred benchmark configuration. Inferred defaults are evidence of the analysis context, not a claim about upstream's intended build options. Polly still reports its project-reference/type/assembly-version gaps. Know Code's generated-protocol mismatch cleared after the explicit-input correction. These are point-in-time observations; the [live evidence snapshot](../.artifacts/corpus/after-fixes.json) records coverage and status.

## Remaining investigation

Polly's project-reference and assembly-version diagnostics were traced and fixed in the provider. The [follow-up investigation](POLLY_RESOLUTION.md) records the move from 40/59 complete contexts to all 59, using SDK-negotiated references and SDK-computed identities. The table above remains the earlier setup snapshot. No upstream source changes, forced peer resolutions, full builds, or fabricated versions were used.

Other Node package managers, full transitive dependency integrity, cancellation, compiler session reuse, unsupported-language/file inventory, and broader toolchain/config change detection remain unfinished. Preparation fingerprints are conservative across workspace configuration; some unrelated configuration changes can cause extra setup. One cold workspace job can delay queued work; progress reporting does not add preemption. The CLI's current explicit-index wait ends after 120 seconds even while the coordinator continues the requested refresh; inspect status rather than treating that timeout as cancellation.

No cold-index duration, memory ceiling, query percentile, or large-estate performance goal is validated by these observations. Restore time, compiler extraction, database publication, warm queries, and save-to-publication latency must be measured separately before changing performance targets. Avoid upstream source mutations merely to get a green corpus report.

## Local Swift and Rust test sources

The user supplied these local repositories for **read-only testing** of the experimental language adapters:

| Language | Original source |
| --- | --- |
| Swift | `/Users/zakkeown/Code/atelier/atelier-app` |
| Swift | `/Users/zakkeown/Code/atelier/atelier-core` |
| Swift | `/Users/zakkeown/Code/bubstackdev/aether/aether-kit` |
| Rust | `/Users/zakkeown/Code/bubstackdev/lars-sdk` |

Inspect originals without running package/compiler tooling there. Run tools against durable snapshots under `.artifacts/swift-real/` and `.artifacts/rust-real/`, keeping indexes, caches and outputs separate. Record snapshot exclusions/provenance and verify original source/configuration hashes and modification metadata afterward. Build artifacts and directory symlinks are excluded from copying. Existing project configuration remains authoritative; unsupported package settings must remain visible rather than being replaced by inferred standalone analysis. Actual findings and retained evidence belong in [Swift provider notes](SWIFT_PROVIDER.md) and [Rust provider notes](RUST_PROVIDER.md).

## Setup references

- [npm ci](https://docs.npmjs.com/cli/v11/commands/npm-ci/): locked installs and lifecycle-script controls.
- [MSBuild project properties](https://learn.microsoft.com/en-us/visualstudio/msbuild/common-msbuild-project-properties): evaluated property values depend on import/order context.
- [Node child processes](https://nodejs.org/api/child_process.html): independent stdout/stderr streams used for bounded restoration output.
