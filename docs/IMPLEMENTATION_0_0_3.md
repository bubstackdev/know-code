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


## Assembled native checkpoint and empty serving state

Debug build01 at `b0f0864` produced `d97f56ecd6db49f09e2ef4116103c985`. Focused Rust/Swift/SwiftPM/Xcode-gap provider checks pass against it. The existing TS/C# compiler→store→CLI/Node/.NET graph fixture also passes with selected native TS7.0.2, including its additional saved/error-repair and missing/unsupported-compiler cases. Workspace API checks pass with Rust/Swift DTO exposure, override/null parity and strict old-peer rejection. Test-only corrections handled omitted empty arrays, workspace-wide coverage and a fixed mock RPC-count assumption; no product/compiler failures were hidden. Evidence: `wave-01/{assembled-debug-build-01,assembled-language-providers-01,native-typescript-stored-graph-02,language-settings-native-02}/`.

The intentional empty serving path now filters Swift loose sources to their exact selected directory groups before launching a worker. A complete empty census over a previously usable serving index supplies its current project paths as explicit removals through the existing update path, including an already-empty manifest. First/no usable publication and recovery/replacement destinations receive no such authorization. Each worker must emit at least one compilation before successful completion; unrelated surviving units can no longer mask an accidentally empty worker stream. All 18 publication regressions and strict workspace Clippy pass. The first new assertion requested compact coverage while expecting full coverage; its failure is retained and the assertion now explicitly requests full coverage. Evidence: `wave-01/{empty-publication-integration-unit-02,empty-publication-clippy-01}/`. Matching build02 passes; the expanded four-language graph/empty/cursor/refresh/restart fixture is still under verification. Empty recovery/replacement authorization remains open.

A finite Rust context probe independently captured five actual Cargo/rustc ordinary/test library/binary/integration units, joined exact emitted artifact paths to Cargo provenance and extern inputs, then proved correct cfg-selected definitions/callees in separate analyzer graphs. The public `!rust_analyzer` cfg override also matched compiler-only branches. Evidence: `.artifacts/implementation-0.0.3/rust-context-feasibility-01/summary.json`. This is a dependency-free owned fixture with existing wrappers refused: production capture, renamed/dev dependencies, cache/failure invalidation, generated sources, duplicate-source contexts and cross-compilation are not certified.


## Combined saved-input/native checkpoint

At matching Debug producer `cea17ee9916c4de5a73ecc48b4608eae`, the expanded
four-language fixture passes 20 assertions including stored Swift/Rust
call/value/constant-impact parity, saved updates and pin repair, deletion of a
Swift group without sibling reanalysis, all-input removal, pinned page/source
retention, explicit empty refresh/restart and new-source rediscovery. Evidence:
`.artifacts/implementation-0.0.3/wave-01/assembled-language-native-05/` (82-second
fixture, not a throughput benchmark). The real sibling invalidation issue was
fixed with component-aware candidate selection and shared group identity;
all 19 publication regressions pass. A cold setup attempt exceeded the existing
120-second fixture wait; the same retained index later reconciled to current in
14 seconds, and the unchanged bound passed on subsequent runs. That timeout is
retained, not a certified startup latency. Full coverage option inspection now
requests an explicit 64 KiB detail budget; omitted optional details are expected
under the compact default.

The frozen Rust context-planning fixture at `d7c6b40` separately passes actual
cfg replay, module/include-data witnesses, tampered-artifact rejection and
shared-source/failed-unit boundaries. It ran from a source-hashed seven-file
snapshot, without worker routing or native artifact claims. Evidence:
`wave-01/rust-context-plan-01/` and its adjacent source manifest.

The wired Xcode candidate now builds at research identity
`0.1.0-know-code-index-only.4`. Both platform success cases pass exact loaded-PIF,
selected-root/Test-configuration, forced guard, typed outcome and real module
checks (`xcode-typed-success-01/`). The initial negative fixture's ordinary body
error was tolerated by indexing-module preparation and returned typed success;
that is retained as a behavior boundary, not proof that source code is valid.
A true unavailable module/input failure and independent source graph/error
checks remain required. The failed Swift concurrency/overload builds and their
focused corrections are retained. No bridge adoption or installed support is
claimed.
