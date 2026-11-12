# 0.0.3 implementation work

Started 2026-11-12 after the user approved implementation with TypeScript compatibility leading, Swift/Xcode and Rust in parallel, estate search conditional, and one validation owner. [Scope and accepted decisions](RELEASE_0_0_3_AUDIT.md) remain the requirement reference. This is a work ledger, not a release-readiness claim or publication authorization.

## Ownership and first checkpoints

| Lane | Owned implementation | First reviewable checkpoint |
| --- | --- | --- |
| TypeScript | `providers/typescript/`, focused TypeScript compiler tests | Integrate TS6 selection without regressing 5.9; establish a native 7.0.2 worker path using the selected compiler and honest source/context evidence. Preserve missing-proof boundaries. |
| Swift/Xcode | `providers/swift/`, focused Swift tests/fixtures | Emit compiler-backed relationship evidence through the existing fact contract; qualify production Xcode planning/preparation for one selected scheme and distinct device/simulator contexts. Bridge adoption still needs evidence. |
| Rust | `providers/rust/`, focused Rust tests/fixtures | Add trustworthy direct-call/dependency extraction and compiler-context/source ownership, then ordinary restoration and trusted build-script/proc-macro preparation. |
| Integrator | Shared core/protocol/settings/control/bindings and build/validation scripts | Resolve shared contracts before adapter dependencies land; repair intentional empty publication safely; review/integrate provider commits and execute focused checks sequentially. |

Agents edit isolated durable worktrees. They must not change shared contracts, perform heavy compiler/native/build runs, modify original user corpora or update the live development installation. Static/source-only checks are allowed when they do not launch compilers. Hand off concrete commits with limitations and requested checks; the integrator is the sole heavy validation owner. Independent work can continue while that lane runs.

The current protocol already carries source-backed relationships, capability reports and declaration extents. Prefer those fields over schema changes. If new language settings are needed, the integrator owns typed checked-in/personal resolution, invalidation and acknowledged worker-policy changes. Adapters must not invent environment-only configuration or personal trust grants.

## Validation policy

- During development run changed-area checks and focused regressions; retain failed attempts. Do not repeatedly run the historical full suite.
- Batch native builds after compatible source changes assemble. Preserve producer/profile/source bindings; no stale artifact certification.
- Public acceptance must eventually include required language workflows through installed/native/CLI/MCP paths, relevant graph/context tests and one matching release distribution checkpoint. Existing TS/C# evidence is not new-language certification.
- Real video-encoding performance remains user-deferred and unmeasured. No new signing, release publication or live installation change is authorized merely by implementation work.

## Starting evidence

TS6 worker-only and native TS7.0.2 API feasibility are retained in `.artifacts/ts7-feasibility/CHECKPOINT.json` and [TS7 readiness](TYPESCRIPT_7_READINESS.md). The shipped compiler guard still accepts only 5.9.x. Native API observations do not establish fact publication, generic identity parity, independent project-body links or production transport bounds.

Estate work remains behind required TypeScript/Swift/Rust/Xcode work. Its accepted first slice is bounded symbol search over registered repositories, with explicit truncation and per-workspace drill-down; no estate cursors or semantic cross-repository graph composition.

## First integration checkpoint

TypeScript source now accepts selected 5.9.x and 6.0.x compilers. The existing identity/generic/Unicode semantic fixture passed against both 5.9.3 and exact restored 6.0.2; native TS7 remains a separate lane, not a version-guard relaxation. Evidence: `.artifacts/implementation-0.0.3/wave-01/typescript-{59,60}-semantic-01/`.

The shared writer permits an intentional worker-free scoped removal to publish the last unit's deletion after a prior usable publication. Empty initial/unscoped analysis and ordinary empty worker output remain rejected. Current and cursor-pinned empty generations survive reclamation; old readers retain their exact source, and released empty generations are reclaimed. All 17 publication regressions and strict core Clippy passed. The first focused test caught cursor tables absent before retention activation; the failure is retained and the cleanup query now guards that case. Evidence: `.artifacts/implementation-0.0.3/wave-01/{empty-publication-focused-01,empty-publication-focused-02,publication-regressions-01,core-clippy-01}/`.

This is specifically the scoped-removal path. Full refresh/rebuild of an empty workspace and initial unsupported/no-source behavior still need their own end-to-end acceptance; this checkpoint does not waive them. No storage schema or query contract changed.

The updated Swift reader compiled with strict warnings and captured an owned fixture's actual relationship roles (45 records, seven call occurrences). Immediate global/property initializers and a deferred stored closure all lacked `CALLEDBY`, so containment alone cannot establish their execution owners. That evidence guides conservative partial extraction; it does not establish Swift graph release acceptance. Raw evidence: `.artifacts/implementation-0.0.3/swift-relations-01/`.


## Compiler and policy checkpoint

Native TypeScript 7.0.2 now emits existing facts with honest partial capability reports. The strengthened compiler fixture passes at `786b6e4`, including separate constructor parameter/property identities and generic declaration-root references; 5.9.3 and 6.0.2 semantic regressions also pass. [TypeScript evidence and unresolved boundaries](TYPESCRIPT_7_READINESS.md#worker-implementation-checkpoint--2026-11-12) distinguish worker success from stored/native acceptance.

Rust's focused provider fixture passes at `ea4073a`, covering outgoing direct calls, value/type dependencies, deferred/nested ownership, constants/static reads, compiler-selected module files, Unicode ranges and source exclusions. Build scripts/macros are still disabled in that checkpoint. The first attempt exposed omitted constant/static token kinds; a later attempt reached the sandbox-denied local no-download trap. Both failures remain retained; the unchanged-source escalated run passed. Evidence: `wave-01/rust-provider-graph-03/`.

Shared typed settings now resolve Rust target/features/default-feature choices and Swift scheme paths through the existing checked-in/personal/null precedence. Policy 1 has explicit per-language branches and worker acknowledgment; control/settings capability 2 prevents old coordinators ignoring new choices. Nine settings and three policy unit tests pass. SDK DTOs expose the fields, and trust-status presentation derives both top-level and nested Rust permission from current personal trust. The known settings-1 installer bridge is strictly directional for forward updates; normal clients require settings 2. Fourteen affected core-control tests, three installer-contract tests, the trust-status unit test and strict workspace Clippy pass. Matching SDK/native and actual prior-release installed upgrade checks remain pending. See [workspace settings](WORKSPACE_SETTINGS.md).

## Xcode experiment checkpoint

The separate guarded bridge compiled with two jobs using all eight frozen dependency revisions; before/after checks found no tracked dependency changes. The first build was blocked by the sandbox's compiler-cache denial; the supported escalation run passed. This was a 230.49-second resumed build, not a clean-build benchmark. Neither the original research runtime nor the live installation changed.

Actual hostile-script and imported-module fixtures passed on device and simulator: forced global/per-target script-disable overrides survived into retained SwiftBuild requests; no scheme/build-script marker, generated script output, object or app/framework binary appeared. The dependency fixture produced actual `.swiftmodule` preparation and planner-reported app-to-dependency identity. Observed aggregate descendant RSS maxima were about 371/393 MiB with approximately 100 ms sampling; these are sampled RSS sums including the driver/planner/compiler, not unique memory, true peaks or product cap certification. Exact receipts: `.artifacts/implementation-0.0.3/xcode-{guard,dependency}-qualification-01/` and adjacent process samples.

The accepted selected scheme uses its Test action configuration. A separately compiled Foundation selector passed nine structural cases and a fresh actual `xcodebuild -dumpPIF` export, resolving exact saved scheme/native PBX project identities to unique targets in that exported project. No opaque GUID reconstruction or graph identity inference is used. Evidence: `xcode-selection-structural-01/` and `xcode-selection-export-01/`. Loading that same PIF into the bridge and proving selected roots/dependencies, reliable terminal preparation failures, real preserved snapshots and production cancellation/resources remain pending. The bridge is not adopted, installed or production-supported by these experiments.
