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

The unavailable-input check subsequently passed for both device and simulator
using the unchanged wired06 runtime. A declared but missing dependency source
produced a typed failed terminal result, failed operation state and RPC error,
despite a misleading success-like source warning. The successful preparation
notification alone was insufficient. Evidence:
`.artifacts/implementation-0.0.3/xcode-typed-unavailable-01/` (approximately
396 MiB sampled aggregate RSS, not a hard or true peak). The earlier body-error
fixture remains a separate demonstration of tolerated indexing preparation;
neither result certifies semantic graph completeness.

Rust context collection is integrated but not yet routed into the worker.
Its first focused fixture failed the warm-cache assertion: two compiler
captures occurred where zero were expected. Evidence:
`wave-01/rust-context-collection-01/`. Cache reuse remains unverified until the
cause is fixed and the focused fixture passes. Swift compiler argument
normalization was extracted unchanged into one helper for both planners.

The collection rerun at `e720a63` passes all seven focused scenarios, including
warm reuse without compiler capture, known include edits, artifact replacement,
healthy contexts after a test compilation failure, wrapper refusal and scratch
cleanup (`wave-01/rust-context-collection-02/`). The fixture now declares its
build-script marker as an output through precise rerun inputs; Cargo's default
whole-package watch had legitimately invalidated the earlier run. Capture also
preserves actual Cargo jobserver descriptors rather than dropping them in the
wrapper subprocess. Worker routing remains pending, and independent review
found a blocking-open-before-regular-file-check issue being addressed separately.

The package-cache experiment has not qualified. The first attempt selected
Command Line Tools and stopped before compiler execution. With explicit Xcode
selection, the restore still tried the global SwiftPM manifest cache despite
an owned `-packageCachePath`; its access-denial guard correctly stopped that
attempt. Evidence: `wave-01/xcode-package-cache-02/` and `-03/`, with detailed
commands in the adjacent run directories. Keep the global cache blocked while
finding supported manifest-cache isolation; do not treat package repository
cache flags as complete offline/cache control. Neither attempt reached the
missing/restored-cache analysis comparison.

Actual compiler/IndexStore extraction now passes the owned Xcode body gates on
both platforms, preserving all captured indexing flags. Each app context
contains 18 symbols and 23 relationships, including a private Unicode-named
function, nested local declarations/calls and a function-value reference that
is not a call. BOM/CRLF source locations decode at exact UTF-8 byte ranges.
Independent body-error extraction reports the expected missing-value diagnostic,
partial coverage and no invented symbol even though module preparation
succeeded. Evidence: `xcode-graph-plan-01/`,
`xcode-compiler-graph-preserve-01/`, `xcode-graph-body-error-plan-01/` and
`xcode-compiler-body-error-preserve-01/` under the implementation artifact root.
The shared ingester still reports honest partial graph capabilities; this is
finite compiler/protocol evidence, not storage/native parity, real-corpus or
installed production certification. No indexing shortcut removal was justified
or attempted. Independent review confirmed the finite token/edge claims but
found that lazy/skip flags were explicitly ignored without library evolution.
These passes therefore do not qualify their active behavior for
library-evolution targets; a separate fixture is required. The compiler's
`<unknown>:0: warning:` form was also incorrectly falling through to the
unrecognized-error fallback. Shared diagnostic parsing now preserves its
warning severity and empty source path, with a focused regression in the
existing Swift provider check; unknown failure text still remains an error.
The original receipts retain the conservative false-error reports.
Sampled aggregate compiler/probe RSS stayed around 638–645 MiB; these samples
are neither hard limits nor true peaks.

The combined `rust-contexts` catalog passes at `ab992a6`
(`wave-01/rust-contexts-catalog-03/`, approximately 11 seconds). It includes
nonblocking FIFO rejection, atomic derived-cache replacement, partial-write
cleanup, unsupported jobserver refusal, compiler cfg/input/artifact planning
and collection reuse/failure cases. Independent review also bound reuse to PATH
and this Cargo invocation's selected compiler. Cargo's public compiler-info
cache control supplies a current tool query while retaining compilation reuse.
`--keep-going` lets healthy units finish after a failing sibling context. The
two intermediate failed catalog receipts remain: the first exposed cached tool
queries causing recapture; the second exposed Cargo's early-stop ordering.
Cargo-injected nonstandard environment inputs still conservatively prevent
reuse when their effective origin cannot be proven. Selected-root worker
routing and procedural-macro graph replay remain subsequent gates.

The private Xcode manifest-cache bypass experiment failed option parsing
(`xcode-package-cache-04/`); it is not a supported mechanism. The next experiment
allows the normal compiler manifest-cache subtree while continuing to deny
global dependency repositories/checkouts/artifacts, original source, network
and seed fallback in the missing-cache phase. Missing owned clones must fail;
restored owned clones must succeed. This revised scope checks dependency-source
independence explicitly and does not claim complete compiler-cache isolation.

The separate library-evolution fixture verified active compiler shortcuts and
then exposed missing local-to-private body calls in the preserved-options run
(`xcode-evolution-graph-preserve-01/`). Removing only the three observed paired
indexing shortcuts restored the required calls/value references on both
platforms; independent body-error extraction also retained the real diagnostic,
partial coverage and no invented symbol. Evidence:
`xcode-evolution-plan-01/`, `xcode-evolution-graph-full-body-01/`,
`xcode-evolution-body-error-plan-01/` and
`xcode-evolution-body-error-full-01/`. Body extraction will use the shared
argument helper to remove these action shortcuts while retaining selected SDK,
target, language, conditions and imports. Fast module preparation is unchanged.
Different cache warmth makes these runs unsuitable as a performance comparison.
The bridge remains an unadopted research runtime.

The selected-root Rust worker gate passes at `4ea97fa`
(`wave-01/rust-context-worker-01/`, approximately 25 seconds): five actual
normal/test library, binary and integration contexts, exact cfg-selected call
and declaration bytes, root-only facts, lookup-only dependencies, safe
builtin-derive/macro omissions, stable unit identities on save and reused
dependency setup. Proc-macro replay is still a separate gate.

Independent review found that unchanged old combined Rust views could bypass
analysis after startup. The existing common readiness/backfill path now checks
`optionsJson.rustContextPolicy=1`; selected units and unavailable/summary rows
stamp that marker. Old evidence triggers ordinary bounded reanalysis, while a
current unavailable row does not trigger an expensive retry loop. All 20
publication tests and strict workspace Clippy pass with the core change
(`wave-01/{publication-policy-backfill-02,provider-policy-clippy-01}/`). Cargo
workspace diagnostics are preserved on explicitly scoped coverage rows rather
than attributed as compiler failures to every healthy unit. The native language
fixture now selects a normal Rust context for graph assertions and separately
checks both contexts, shared source storage and saved invalidation.

The integrated shared Swift normalizer itself passes healthy and body-error
library-evolution checks on both platforms, without a second research-layer
flag removal (`xcode-evolution-normalizer-01/` and
`xcode-evolution-normalizer-body-error-01/`). Matching assembled artifacts and
SwiftPM/native regression checks remain the next integration checkpoint.

Matching Debug producer `9aa4dcffb1aa441dabd4545b65c67032`, built at `8155a11`,
passes the expanded four-language native integration check (21 assertions,
approximately 109 seconds) and the focused Swift/SwiftPM/Xcode-selection batch
(approximately 42 seconds). Evidence:
`wave-01/{assembled-debug-build-04,assembled-language-contexts-06,assembled-swift-provider-batch-04}/`.
This is an assembled Debug checkpoint, not optimized/installed/release or
encoding-performance certification.

The generic Xcode package-cache gate now passes after correcting a canonical
cache-alias denial and separating normal preparation from strict offline
analysis. See [the durable cache qualification findings](XCODE_PACKAGE_CACHE_QUALIFICATION.md)
for exact evidence and failed attempts. Wired07 adds central paths but is still
under qualification. Proc-macro replay remains unintegrated: independent review
identified asynchronous loader-diagnostic proof and same-kind attribute-origin
cases requiring stronger evidence before its bindings can be published.

## Integrated macro and protected Xcode checkpoint — 2026-11-13

The Rust macro bundle is integrated at `7e10a53`. Immutable snapshot07 passed
14 assertions, including actual Cargo-injected binding selection, canonical
callables, same-kind type/field omissions, attributed-module ancestry and real
loader-failure refusal. Snapshot06 exposed a `MOD` versus `MODULE` grammar
mismatch; the narrow fix preserves plain nested-module symbols. Those failures
remain retained. [Rust provider documentation](RUST_PROVIDER.md) now replaces
the stale combined-view description with exact implementation boundaries.

The existing readiness path now backfills pre-macro policy-1 Rust rows with
policy 2, including old unavailable rows, without looping on current gaps.
All 20 publication tests, formatting and strict workspace Clippy pass.
Matching Debug producer `a76e4dfb22004262906f6620102a48de` is built at `2009f5f`;
the updated managed/context catalog passes at `9ead59b`
(`wave-01/assembled-rust-providers-07/`). Unsupported wrappers remain explicit
gaps. The older fixture's expectations were corrected rather than adding
unproven wrapper support. Matching native integration subsequently passes all
21 assertions across CLI/Node/.NET
(`wave-01/assembled-language-contexts-07/`, approximately 135 seconds). The
producer verifies before and after the run; optimized/installed/release
validation remains separate.

Actual wired08 package metadata, missing-cache boundaries and typed package
module preparation now pass on both device and simulator with the existing
analysis profiles and saved inputs unchanged. Each normal preparation and
protected positive analysis pair shares one current environment; historical
launcher fingerprints cannot safely be assumed across tool calls. See
[the package-cache receipts and boundaries](XCODE_PACKAGE_CACHE_QUALIFICATION.md).
No bridge rebuild, production adoption, installation or live-service change
was needed. A separately protected Atelier snapshot is prepared for the next
metadata/module check; its known omitted resources remain explicit.

## Approved production Xcode integration

On 2026-11-13 the user approved [the concrete bridge proposal](XCODE_BRIDGE_DECISION.md).
The adapter and separately installed tool recipe are being implemented in
isolated worktrees, with root owning shared contracts and all heavy validation.
Swift policy 2 adds derived personal trust and central cache fields, while
TS/C#/Rust keep policy 1. Existing readiness backfills older Swift extraction
rows; no store/framing change or second execution/storage engine is introduced.
Protocol, settings and publication checks pass individually; assembled Swift
workers and native/installed gates remain pending until the adapter lands.
Earlier research checkpoints above retain their historical limits.


## Production Xcode native checkpoint — 2026-11-13

Matching Debug producer `04faec9ab4194780ae7b4c458cf98db1`, built at
`3a82a58`, now includes the adopted optional bridge adapter, Swift-only policy 2,
central personal-trust/cache derivation and Swift extraction-policy backfill.
The pinned separate tool recipe is `index-only.6-notice1`; no live installation
or release was changed. Source-only saves keep dependency preparation separate
from compiler analysis. Exact compiler-selected missing/external configuration
and versioned package-manifest repair inputs stay in the common watcher path.

The owned production Xcode fixture passes six app/dependency/UI-test contexts
across device and simulator, selected Test configuration, compiler-bound
callers/callees/impact and references, retained exact UTF-8 source, generated
asset-source labels, and CLI/Node/.NET equality. Saved body edits retain context
and symbol identities without repeating dependency restoration. Body errors
publish fresh partial results and repair on save. Evidence:
`wave-01/assembled-xcode-integration-02/` and
`.artifacts/xcode-integration/1794586873180331000/summary.json` (34-second
fixture, not a performance benchmark). The initial test failed because its
coverage expansion omitted the required generation/snapshot; that failed
attempt remains retained. A read-only inspection of that publication had six
healthy contexts; it was not relabeled a passing integration run.

Quick source/toolchain and standalone Swift graph checks pass. The sandboxed
SwiftPM attempt failed on compiler-cache writes; the supported escalated
SwiftPM/Xcode-policy/Workspace-API batch passes, retaining both attempts:
`wave-01/assembled-swift-providers-{09,10}/`. These receipts precede the final
small manifest-repair watch fix; the production Xcode checkpoint above uses the
new matching producer. The four-language native regression, missing-input
repair and current strict Clippy checkpoint are next. See
[Xcode integration details](XCODE_ADAPTER_IMPLEMENTATION.md) and the
[unresolved metadata failure boundary](XCODE_METADATA_FAILURE_BOUNDARY.md).
Optimized, installed, cleanup/cancellation and broader toolchain qualification
remain separate gates; no full Debug, release or encoding-performance pass is
claimed.


The matching four-language native regression subsequently passes all 21 checks
(`wave-01/assembled-language-contexts-08/`, 105 seconds), including stored
Swift/Rust graph/source parity, saved changes, empty generations and retained
cursor/source behavior. The extended Xcode fixture passes two additional gates:
a declared source deletion produces typed unavailable device/simulator contexts
without current prior bodies; restoring those exact saved bytes automatically
repairs all six contexts and graph queries. Evidence:
`wave-01/assembled-xcode-integration-03/` and
`.artifacts/xcode-integration/1794587115036710000/summary.json` (53 seconds).
Formatting and strict workspace/all-targets Clippy also pass at `e1d1f12`
(`wave-01/assembled-{format,clippy}-08/`). A narrow separately versioned bridge
follow-up is now being implemented for the remaining metadata command-rejection
boundary; it has no runtime evidence yet and does not replace these `.6`
receipts.


## Integrated .7 metadata rejection and cache repair

The narrow bridge follow-up is integrated at `e9306e4`, with adapter `abe1581`
and typed exit evidence in the shared JSON-RPC client. No worker/core/storage
protocol or graph engine was added. The separately built `index-only.7-notice1`
tool passes source/patch and component-inventory checks, both-SDK typed success
and true unavailable-input failure probes. Its 200 inventoried files occupy
95,015,745 logical bytes; it remains an optional separately staged prerequisite,
not a new common-core dependency or installed/public artifact.

A protected plugin probe first failed before initialization because its reused
driver still expected `.6`. New retained packet04 reached metadata but failed
on nested manifest sandboxing for both SDKs. Normal preparation used `xcrun`
while `.7` metadata used the selected direct executable. Changing preparation
to that same direct executable restores both protected SDK checks without
changing the inherited environment, analysis policy or plugin/scanner rules.
The production fix is `9806ec8`, with preparation fingerprint2 so old setup is
not reused silently. Exact variable-level causality remains unproven; the tested
fix matches the launch routes instead of emulating environment magic. Evidence:
`wave-01/xcode-plugin-probe-{device,simulator}-05/` and the
[cache qualification ledger](XCODE_PACKAGE_CACHE_QUALIFICATION.md).

Matching Debug producer `1d2fcd1df0d34b37bab70964c94f8b27`, built at `bea7b4f`,
passes all 14 production Xcode checks at the final cache fix, including fresh
healthy TypeScript results beside a malformed Xcode project and automatic saved
repair of both SDK graphs. The existing Xcode policy fixture additionally
passes 23 rejection-proof faults plus its valid receipt; all 15 shared JSON-RPC
checks pass. Evidence:
`wave-01/{assembled-swift-build-11,assembled-xcode-integration-05,xcode-rejection-contract-01}/`.
The [metadata boundary document](XCODE_METADATA_FAILURE_BOUNDARY.md) distinguishes
proof-reader negatives from untested actual OS/backend failure paths.

The existing `package_install.py --experimental` section now has source for
meaningful dependency-free SwiftPM/Cargo call/value-impact, retained-source,
native parity and saved compiler-error repair checks. This extension is **not
run or certified** yet; normal SwiftPM manifest sandboxing inside its outer
installed/offline policy remains an explicit runtime question. No packaging
manifest promotion or new installed test harness was introduced. Optimized,
installed, broader interruption/cleanup and platform gates remain ahead; the
real video-encoding performance gate stays deferred. Required languages retain
priority over optional estate work.


The actual Xcode cancellation extension also passes at `e16caf6`: observation
of active preparation precedes the CLI cancellation acknowledgement, generation
and exact source/graph remain coherent and explicitly stale, then a new save
after durable cancellation resumes both SDK graphs beside the TypeScript
sibling. All 16 fixture checks pass with the unchanged matching producer
(`wave-01/assembled-xcode-integration-06/`, 85 seconds;
`.artifacts/xcode-integration/1794589193943965000/summary.json`). A separate
post-fixture census found no process with the exact owned `.7` runtime prefix
and no isolated coordinator socket. That single census does not certify every
cancelled compiler descendant or continuous process lifetime.

Read-only real-corpus preflight found that the live Atelier repositories have
changed since the earlier frozen copies: a new Aether2 package increases the
current roots to nine, and 12 of the 13 resources omitted by the old selective
copies now exist. The RefineEncoder resource remains absent. Sibling app/core
paths preserve local references; the Lars HOME/environment-selected library
remains an explicit external boundary. A new complete owned source copy is
being prepared separately; no old grant, snapshot, index or receipt is silently
reused or changed. It will require its own personal trust grant before compiler
or dependency execution. The exact preflight/copy manifest, rather than these
historical package counts, governs the next production run.


## Full current Atelier run: explicit resource limit

The user granted compiler/dependency execution to the new complete source copy
at `.artifacts/implementation-0.0.3/atelier-production-full-01/snapshot`, with
personal state confined to its sibling `personal-data` directory. All 1,782
files (182,755,166 bytes) independently match the copied source hashes, modes
and mtimes; the original before/after census also matched. The approved manifest
SHA-256 is `9b29cfc61320a8b69a47e76ece11ad619043fb6ae6aca99cc12e6497037f918b`.
The first long data path exceeded macOS's Unix-socket pathname limit; a durable
`.artifacts/af01` symlink points to the same approved physical data directory.
A sandbox-denied coordinator start and both failed grant attempts remain
retained before the supported escalated grant succeeded. No global/original
trust or live installation was changed.

Normal production indexing stopped correctly at the unchanged shared default:
2,133 MiB observed against 2,048 MiB allowed, during AtelierLibrary's SwiftPM
module preparation after 68 seconds. Generation stayed zero; this is **not a
full-corpus pass**. An explicit warm diagnostic retry also stopped at the limit
(2,155 MiB observed). A bounded sampler using the same macOS physical-footprint
API captured a 1,921 MiB aggregate sample before termination: AetherCore's Swift
frontend used about 1,638 MiB, SwiftPM about 242 MiB, Node about 34 MiB and the
Know Code host about 7 MiB. These are sampled observations, not a true compiler
peak or proof that the full workload never overlaps frontends.

Evidence: `wave-01/atelier-full-production-01/`,
`wave-01/atelier-full-memory-probe-01/`, and the full-copy
`memory-probe-01/{result,analysis}.json`. The diagnostic driver's passing exit
means the measurement completed; it does not turn the failed cold index or its
resource-limited retry into a successful corpus run. Both owned coordinators
were stopped by their drivers. [The source audit](reviews/SWIFTPM_PREPARATION_MEMORY.md)
finds no demonstrated lost `--jobs 1` forwarding or preparation overlap; it is
not an exact installed-SwiftPM binary proof. No compiler flag, resource default
or limit has been changed to manufacture a pass.


## Atelier retry: failed completion and empty compiler records

The user approved a temporary 4 GiB limit for this isolated full-copy test;
the shared default and live installation remain 2 GiB. Every attempt's driver
restores its isolated setting and stops its owned coordinator in `finally`.
The first lifted-limit attempt failed after 223 seconds with a native reader
error masked by `unexpected message during staging`. An intermediate retry
only replayed the saved failed-input guard and is retained as a harness
failure, not another compiler run. The corrected driver acknowledges startup
cancellation before issuing one explicit public SDK refresh.

The next actual attempt (`production-4g-03`, 92.86 seconds, generation zero)
identified an IndexStore traversal failure for `ClipLiveTests.swift`, whose
contents are entirely excluded by the selected conditional-compilation
context. A separate owned two-file reproduction failed the same way. The
reader now uses the public symbol enumeration API to establish a valid empty
record before skipping its absent occurrence cursor. Nonempty record traversal
and corrupt-record errors remain required; see
[the narrow fix and focused qualification](reviews/SWIFT_EMPTY_INDEX_RECORDS.md).

The common update path now explicitly rejects `Complete { success: false }`
without staging it as facts, preserving the previous generation and useful
worker diagnostics. The TypeScript fault path now emits this existing failed
completion consistently with Swift. Existing publication regressions pass all
20 assertions (`reader-completion-publication-01`); the `provider-failures`
check passes inside the retained selected run `reader-completion-providers-02`,
whose later Swift fixture failure means that overall run did **not** pass.
The corrected focused Swift/SwiftPM checks pass against matching Debug
producer `190ffd3a92a3473fa1b28a9dac6761ae` at test source `747d0e7`
(`empty-record-swift-providers-03`). Failed build/provenance and fixture attempts
remain retained. A full-corpus retry is a separate qualification, not implied
by these passes.


The next approved lifted-limit attempt, `production-4g-04`, ran for 973.28
seconds and passed the prior empty-record boundary, then failed before
publication at Xcode context-proof validation. Generation remained zero;
1,082,488,522 worker stdout bytes are an observed stream size, not published
index size or a throughput benchmark. Its runner restored 2 GiB and stopped
the owned coordinator. Retained PIF sidecars prove a path-alias mismatch:
the bridge canonicalized the `.artifacts/af01` data alias to `personal-data`,
while the provider's containment check used the lexical alias. Both sidecar
PIF hashes, saved scheme paths and Debug configurations match. The failed
attempt remains a failure; a narrow canonical job-root correction and matching
qualification are still pending. Evidence: `wave-01/atelier-full-production-4g-04/`
and `atelier-production-full-01/production-4g-04/`.

The user separately approved compiler/build-script/procedural-macro execution
for the fresh complete Lars copy, with the grant confined to its sibling
`personal-data`. The exact-copy and source census are recorded in
[the Lars preflight](reviews/RUST_LARS_0_0_3_PREFLIGHT.md). The public CLI grant
succeeded through the short `.artifacts/lf01` alias with lifecycle scripts still
disabled; its unregistered coordinator was stopped. Actual normal indexing
is a separate run at the unchanged two-core/2-GiB defaults, not covered by the
Atelier-only 4-GiB allowance. Evidence: `wave-01/lars-trust-01/` and
`lars-production-full-01/trust-grant-01.json`.


The Xcode path correction (`e9d479c`) canonicalizes the bridge job directory
and checks actual PIF containment while retaining hash/closure/scheme/config
binding. Aliased roots pass and a PIF symlink escaping its job directory fails.
Matching producer `b51438c2d1c8433491478663e8b562b2` passes `xcode-provider`
and all 16 device/simulator integration checks
(`wave-01/xcode-canonical-job-context-01/`). These are focused owned-fixture
passes, not a successful real-corpus retry.

`production-4g-05` and `06` failed before indexing began: cold-start `resources`
returned macOS `WouldBlock` after about 0.56–0.57 seconds. Both restored 2 GiB.
A separate owned foreground measurement showed connection refusal at 0.006 s,
a timed-out status read at 0.538 s, and readiness at 0.567 s. The listener binds
before saved workspace restoration finishes; the old post-spawn probe loop
returned its first 500-ms read timeout despite its existing five-second
startup window. Only the owned-child wait now retries `WouldBlock`/`TimedOut`
within that window, with each probe capped by the remaining budget. Initial
probes, security/protocol/version faults and the single-child policy remain
strict. All nine focused control tests, formatting and strict workspace Clippy
pass at `93c6161`; matching Debug producer is
`cce7be50d0054df3ae36146783ddb3a9`. The delayed-readiness test also verifies
permanent bad-version/payload replies are not retried. Evidence:
`wave-01/{atelier-coordinator-startup-01,coordinator-owned-startup-unit-02,assembled-batch-fmt-02,assembled-batch-clippy-02,assembled-corpus-fixes-build-01}/`.

The first real Lars run reached idle generation 2 with 12 coverage rows in
109.44 seconds under the unchanged 2-GiB limit. It fails graph qualification:
all Rust contexts are unavailable because required ordinary host libraries in
tooling dependency chains were rejected before compilation. Healthy C#/TS
results and explicit missing NuGet/Swift/vendored-UI gaps are retained; neither
partial publication nor the passing source census is a Rust graph pass. The
narrow correction admits only typed host-tool library preparation and leaves
actual compiler/input/artifact joins authoritative. Its source-only I/O checks
pass; matching actual compiler/managed and real-Lars retries remain pending.
See [the exact diagnosis](reviews/RUST_LARS_0_0_3_PREFLIGHT.md).

The approved Atelier source copy remains bound to its original manifest.
A later read-only census found six original source files changed since copying,
with timestamps preceding `production-4g-04`; it does not certify a now-identical
live checkout or attribute those changes to a writer. No originals were
restored, edited or refreshed into the approved copy. Evidence:
`atelier-production-full-01/original-census-after-4g-06.json`.


## Real retained graphs and cache-path qualification

`production-4g-07` published Atelier generation 7 after 1,111.83 seconds with
105 compilation contexts (89 complete, 16 partial). A follow-up analysis began
while the workspace remained stale. The owner cancelled at 1,303.38 seconds
to address a proven generated-input enrollment defect; the driver restored
2 GiB and stopped its coordinator. That run remains a **failed current-index
qualification**. Two central generated asset files were present in the saved
Xcode input list because physical cache paths failed lexical alias checks.
One follow-up was observed; an endless loop or attribution of every freshness
event to those files was not measured. See
`atelier-production-full-01/generation-7-observation.json`.

A separate explicitly stale generation-7 read probe passes CLI/Node/.NET
parity for real `CanonicalJSON.encode(_:)` → `encoder()` callees, reverse
callers, impact and exact complete saved source in SwiftPM, iPhone device and
simulator contexts (`wave-01/atelier-generation-7-native-02/`). All 105 coverage
rows were present, while the bounded expanded-detail budget omitted details
for 84 rows and marked truncation. Its first attempt exceeded the API's
256-KiB detail cap and remains failed. These are useful retained-graph results,
not current freshness, cold-index performance or distribution certification.

The Swift cache boundary correction at `c28865d` canonicalizes the work root
once and filters old physical/aliased central outputs from retained input
plans, preserving missing authored external inputs. Matching producer
`3e5bdf885a014c36a32e0c3d6d409bf2` passes Swift, SwiftPM, Xcode provider and
all 17 aliased-storage Xcode integration checks. Both SDKs retain generated
source facts while published input lists exclude central output paths.
Evidence: `wave-01/swift-canonical-cache-contexts-01/`.

The real saved-source check then stopped after 26.27 seconds with a Swift
frontend SIGSEGV during Library SwiftPM analysis. Generation 7, exact edited
source bytes/mode/mtime and the 2-GiB setting were preserved/restored; no
current result is asserted. Eighteen matching-time crash reports were retained
alongside read-only cache metadata. Their Clang ASTReader stacks narrow the
failure to module loading, but do not establish corrupt cache data, exact
failed arguments or upstream-only fault ownership. The older cache and mixed
path-spelling build plans remain intact. Evidence:
`wave-01/atelier-saved-xcode-convergence-01/` and its corresponding corpus
`compiler-crashes/` directory. Bounded operational compiler diagnostics and a
controlled follow-up remain pending.

Rust's host-library and bounded input-discovery fixes pass the existing
managed/context/macro checks and the expanded actual-Cargo collection case:
cold discovery 2 checks, invalidated warm proof 3, then steady warm reuse 1
check with zero captures. A per-package normal/test scheduling bug exposed by
the first new test was corrected; its failed attempt remains retained.
Evidence: `wave-01/{rust-tooling-host-contexts-01,rust-stale-include-discovery-02}/`.

The third real Lars run reached current generation 6 with 19 coverage rows at
2 GiB after 192.87 seconds. Normal/test core contexts and their exact dependency
proofs are present; the old driver's `StopIteration` occurs on a call inside a
closure, a documented unsupported deferred-body form. It is not a missing core
context or remaining input-snapshot failure. That run remains failed under its
original expectation. A new read-only probe passes CLI/Node/.NET equality for
an actual direct `softmax` → `pairwise_sum` call, callers, impact and full saved
UTF-8 source against the retained generation, honestly flagged stale after the
coordinator stopped (`wave-01/lars-published-direct-01/`). Native-link flags and
metadata-only macro forms remain explicit gaps; no broader support or closure
fallback was invented. Matching producer for this probe is
`22ac387bedec4b4a9680dd35170ae41d` at `6aa7ae2`.


## Compiler failure diagnostics and pending preparation budget

Signal diagnostics now retain bounded UTF-8 stderr and append exact source,
compiler and module-cache context at the end of the message, so the existing
8-KiB shared diagnostic tail preserves them. The lightweight existing
SIGTERM/ENOTDIR checks pass. Controlled saved-source attempts 02 and 03 remain
failed, with original source and 2 GiB restored. Attempt 03 identifies
`atelier-app/Library/Sources/AtelierLibraryApp/main.swift`, the owned Library
`build/out/ModuleCache.noindex`, and `_DarwinFoundation1` duplicate-module
text preceding the SIGSEGV. This establishes the actual compiler context;
it does not establish which serialized artifact is responsible.

The 465 owned PCM files (502,063,368 logical bytes) were preserved by a
copy-on-write copy with matching SHA-256 hashes before the later diagnostic
replay. The entire Library `build/out` was then moved intact to
`quarantine-library-out-01`; checkouts, source and serving index were not
removed. The clean-output attempt 04 remains failed after 209.06 seconds with
a BSP child SIGTERM. It does **not** certify a cache repair or prove that the
original crash disappeared: a request/cleanup timeout can mask the primary
error. Old output and cache evidence remain durable.

The shared JSON-RPC timer now records its original timeout as failure before
terminating, rejecting all pending requests with that same cause. All 15
transport checks pass, including cause identity after child exit. SwiftPM now
performs graceful shutdown only after successful analysis and always
terminates its child, preventing a secondary cleanup RPC from replacing a
primary failure. The 120-second duration remains unchanged. The user has been
asked whether heavy Swift module preparation may use up to ten minutes while
metadata stays at two minutes and query/CPU/RAM budgets remain unchanged;
that preference is pending, not silently granted.

Evidence: `wave-01/jsonrpc-timeout-cause-01/`,
`swift-operational-diagnostics-{01,02}/`,
`wave-01/atelier-saved-xcode-diagnostic-{02,03}/`,
`wave-01/atelier-fresh-derived-cache-04/`, and the corpus cache/crash manifests.
Matching causal-failure Debug producer is
`c0a52d35308044578c46e4e2363cfce5` at `0c4db23`.
The relevant compiler-transport consumer checks are a separate recorded run;
no release, installed or encoding qualification is implied.


The user subsequently approved ten minutes **only** for heavy Swift module
preparation. `f20f90b` adds a per-request timer override and passes it solely
at SwiftPM/Xcode `buildTarget/prepare`; metadata remains 120 seconds and
query/CPU/RAM budgets are unchanged. All 16 transport checks pass, including
a short delayed-response override followed by the unchanged default timeout.

The transport-consumer run at `0c4db23` remains failed overall: its individual
transport, Rust navigation/provider, Swift toolchain/provider/SwiftPM and
Xcode-provider checks pass, but the four-language fixture expires its
120-second initial wait while C# is still analyzing, without a reported
provider failure. The initial correctness-only setup guard is now 300 seconds;
saved-update waits and product deadlines remain unchanged. This is not a
latency pass or a reinterpretation of the failed run.

An executable-name/CPU-only observation records load average about 103 on
18 logical CPUs with active AV1 encoders. Their jobs were not modified.
These functional checks under load do not certify the deferred real-encoding
performance gate. Evidence: `validation-load-01.json`,
`wave-01/{compiler-transport-consumers-01,jsonrpc-prepare-budget-01}/`.

## SwiftPM cache compatibility qualification

The fifth saved-Xcode attempt remains failed after 680.30 seconds. After the
Library output quarantine, analysis advanced to Native, where its untouched
legacy module cache produced a CoreAudio duplicate-module diagnostic and a
compiler SIGSEGV while indexing `AtelierLeaseSignalProbe/main.swift`. Generation
7 stayed available; the owned edited file's bytes/mode/mtime, two-core/2-GiB
settings, and stopped coordinator were restored. This comparison supports a
derived-cache compatibility boundary; it does not identify a corrupt PCM or
certify an upstream-only fault.

`874f110` adds one compatibility tag and the canonical work root to both the
existing SwiftPM state key and preparation fingerprint. Incompatible old trees
are retained, not deleted or retried. First use creates fresh derived state
and may refresh compiler context identities once; subsequent warm/saved
analysis must preserve identities within the new layout. Old-tree disk cost
remains, and no new cleanup mechanism is claimed.

Matching Debug producer `1cf420a3ade042bcb3920701c4bf6e69` built successfully.
The targeted SwiftPM check passes legacy failed-checkpoint bypass, retained
old-cache bytes, successful new preparation, warm reuse, saved repair and
stable new-layout identities. The combined run also passes all 16 transport checks, all 17 Xcode
device/simulator integration checks, and the 21-check four-language/native
workflow, including scoped saves and empty publication. These are focused
development checks, not release, installed or encoding acceptance.

Evidence: `atelier-production-full-01/saved-xcode-qualification-05/`,
`wave-01/assembled-swift-cache-fence-build-01/`, and
`wave-01/swift-cache-fence-consumers-01/`. The next owned saved-file replay
allows the one-time layout transition, then requires exact context/caller/
callee identity stability between its changed and restored generations.

`f49e57f` separately reports body compilation and IndexStore reading as
`analyzing` through the existing progress protocol. Its matching producer is
`1faad96f329c42398a496c8340a0edd3`; the existing SwiftPM fixture passes actual
body/reader/publication event ordering and cache-reuse assertions
(`wave-01/swift-analysis-progress-01/`). No new progress tracker or query
behavior was added. The owned saved-file attempt 06 is running on this source;
no real-corpus pass is claimed yet.

The [exact-settings grouping research](reviews/SWIFT_INDEXING_GROUPS.md) now
has a finite three-file parity pass through the unchanged reader and ingestion
path. This remains research, with no production grouping or performance claim.

### Dependency restoration budget follow-up

Saved-file attempt 06 hit the separate two-minute SwiftPM dependency-download
timeout while filling the new Library cache. Several packages were fetched,
but restoration failed before all downloads completed. This is neither a cache
compatibility pass nor a repeated compiler crash. The owner interrupted that
attempt before retrying; the stop signal selected the receipt wrapper rather
than its child, so automatic cleanup did not run. Explicit owner cleanup then
restored exact source bytes/mode/mtime and two-core/2-GiB settings. A refused
isolated socket connection plus an unheld coordinator writer lock confirmed
shutdown; a remaining socket file alone was not treated as a running service.
The original running receipts, interruption and cleanup evidence are retained,
and attempt 06 is recorded as interrupted, never passed.

The user then explicitly approved ten minutes for **dependency restoration**
as well. `163bd6a` uses the existing shared Swift process helper's optional
timeout only for SwiftPM `resolve` and Xcode package resolution. Metadata,
per-file compiler and reader commands remain at two minutes; query/CPU/RAM
budgets are unchanged. Existing failed preparation checkpoints still require
explicit refresh. The next owned replay therefore explicitly retries setup
before checking ordinary saved-source restoration, rather than silently
bypassing the failed-checkpoint policy.

Evidence: `atelier-production-full-01/saved-xcode-qualification-06/`
(`preparation-before-stop.json`, `operator-stop.json`, `manual-cleanup.json`,
`cleanup-confirmation-02.json`) and
`wave-01/assembled-swift-restore-budget-build-01/`. The matching build passes;
all three affected provider checks also pass in 49 seconds
(`wave-01/swift-restore-budget-consumers-01/`), including real-child timeout
forwarding, retained default behavior and actual SwiftPM progress/cache reuse.
Matching producer is `0be3d8a559f04daa81f8da32c5c48e61`.

Attempt 07 failed the test driver's 30-second subprocess guard around blocking
CLI `index`, before corpus qualification. Its normal finally block restored
source/settings and stopped the isolated coordinator. Attempt 08 instead uses
the existing nonblocking Node `Workspace.refresh()` to submit the explicit
retry, then polls bounded status. The CLI's own two-minute initial wait is
unchanged; no product timeout was increased to repair the harness. Attempt 08
is running at this checkpoint. Neither failed attempt is relabeled as passing.

## Corpus reconciliation and packaging preparation

Attempt 08 remains failed at its 1,800-second first-current guard (1,804.26
seconds including cleanup). It published generation 16 for captured epoch 5,
then reconciled observed epoch 12 after setup/compiler-input discovery. The
observed epoch stayed stable during the second pass, but that pass had not
finished before the guard. This is no current-data, latency or encoding pass;
neither a compiler crash nor an endless-loop cause was established. Cleanup
restored exact owned source and two-core/2-GiB settings.

The warm follow-up 09 failed **before indexing** in 3.51 seconds: initial
resource reads/set succeeded, then `watch`, cancellation and the resource reset
returned OS error 35. Shutdown succeeded. The automatic memory reset therefore
did not complete; owner cleanup verified the coordinator writer lock was free
and its socket refused connections, then atomically restored the isolated
setting from 4 GiB to 2 GiB. Exact source bytes/mtime were also verified. The
original cleanup failures and subsequent `manual-cleanup.json` remain retained.
This setup error is not the configured memory-cap outcome.

The retained generation-16 preflight contains 105 Swift compilation rows with
**zero** saved input paths under either spelling of the central Swift cache.
Four saved lock hashes were recorded before the warm follow-up. A short
isolated coordinator trace subsequently completed all resource/watch/cancel/
status calls in 6–18 ms and shut down with 2 GiB restored; it did not reproduce
the stall. Source review identified an enrollment/retention lock chain worth
a deterministic regression, not a confirmed attribution for attempt 09.
Further corpus runs wait for that control-path investigation.

Evidence: `wave-01/atelier-restore-budget-saved-08/`,
`wave-01/atelier-warm-convergence-saved-09/`, their owned driver directories,
and `coordinator-admission-trace-01/`. No required corpus gate is waived.

The user selected **packaging gates before Swift batching**, and a **separate
bridge download and DMG**. Candidate source now has coordinated 0.0.3 versions,
four required provider entries and omission checks, installed exact TS6.0.2 /
native TS7.0.2 cells, a representative installed Rust MCP graph, separate
bridge signing/verification, and an installed mode of the existing Xcode
fixture. Published support remains 0.0.2. These source changes require fresh
matching artifacts; no old producer is certified against them.

At `fb0b964`, the four quick source checks and all six archive-boundary tests
pass (`wave-01/{candidate-source-quick-01,bridge-signing-boundaries-01}/`). The
new package/compiler/Xcode installation modes have not run. The next bridge
build must record its builder digest before compilation; the old build-02
record is deliberately not retrofitted. Signing, notarization, installed,
upgrade/rollback and final platform evidence remain pending.

## Registration liveness fix and initial Release preflight

`649d039` separates the tiny cursor enrollment fence from retention/cache state,
so registration can clear that fence while maintenance holds the latter lock.
Coordinator enrollment ordering remains under the existing workspace lock;
unregister's pre-existing maintenance wait is unchanged. Peer review found no
inverse lock ordering. Ordinary cursor queries already use deadline-aware state
acquisition; this change does not raise deadlines or refactor retention.
The short trace did not reproduce attempt 09, so its exact cause remains
unconfirmed. The deterministic real-control regression holds maintenance and
checks registration plus sibling status/cancel/resource requests at the existing
500-ms budget.

The regression's first run failed its final cancellation assertion because the
fixture used an unmanaged `Control::default()` handle. `c7a47e6` supplies a real
managed job instead; the complete regression then passes. All 21 existing
cursor service tests, formatting and strict Clippy also pass. The original
failed fixture receipt is retained, not promoted.

The first required Release core run stopped at a stale cursor-peer positive
fixture lacking current settings/resource versions. `1e96cce` corrects that
fixture and isolates cursor-version rejection from unrelated incompatibilities;
production compatibility guards are unchanged. The second core attempt found
a raw test-lock/fork interaction: closing the fixture's descriptor could leave
its lock held by a parallel pre-exec fork. `3ba4869` explicitly unlocks the
fixture before malformed-policy assertions. Production `HeldLock` already
does this. All nine admission regressions and then the complete optimized Rust
workspace pass: **290 passed, two existing manual helpers ignored**. Formatting
and strict Clippy pass on that source. Both stopped core runs remain failed;
fresh matching artifacts and the complete core gate still follow.

The separate bridge build-03 passes with 200 staged files/95,015,745 logical
bytes and a pre-compilation builder digest. Signing-03 also passes: exactly two
native transformations, preserved resources/modes, signed inventory and the
pinned Developer ID certificate. The compressed archive SHA-256 is
`636806ff6a9604c8bcd2a97dd4e030ed1b20da33f26f3680301069d6b9d5c4bd`.
It remains **not notarized or installed-qualified**; its separate DMG and actual
installed Xcode gate have not run.

Evidence: `wave-01/{cursor-enrollment-control-release-01,cursor-enrollment-control-release-02,cursor-service-release-01,candidate-release-core-01,candidate-release-core-02,cursor-peer-contract-release-01,admission-release-01,candidate-rust-release-01,candidate-format-03,candidate-clippy-03,bridge-producer-build-03,bridge-signing-03}/`,
plus `.artifacts/xcode-tooling/{build-03,signed-03}/`.

The source-matched optimized producer is now
`99cad771bd0f4385b5c9ee0b40665a64` at `3ba4869`. The complete required Release
core gate passes all 27 selected checks in 443 seconds. The six relevant
language checks also pass in 262 seconds using the exact signed bridge-03,
including all 17 device/simulator integration checks and the four-language
native workflow. Exact TS6.0.2 semantics and native TS7.0.2 stored graph/source/
save/error/no-fallback checks pass separately. Receipts are
`wave-01/{candidate-release-core-03,candidate-release-language-consumers-01,candidate-typescript-60-semantic-01,candidate-native-typescript-release-01}/`.

Owned Atelier attempt 10 is running on this Release producer with a private
PATH selecting the actual signed bridge-03 runtime. Registration has succeeded
and source analysis is progressing. Its first current publication and saved
restoration remain pending; earlier corpus failures are unchanged. Common
archive assembly, final installation and distribution gates remain separate.

The common three archives subsequently assemble with no unresolved license
evidence (`wave-01/candidate-packages-01/`) and pass signing verification
(`wave-01/candidate-common-signing-01/`, signed manifest SHA-256
`ac7fc4402b6611433af5f2764e7a3fc65c1035a6744635c3678718951b2dc5b7`).
They remain not notarized or installed-qualified.

Attempt 10 publishes generation 18 for epoch 6, then reconciles epoch 7.
Read-only investigation found 80 newly enrolled, existing SDK interface files
(36 macOS, 22 device, 22 simulator) absent from the acknowledged 1,544-path
before-manifest. None is under the central cache, and the recorded before paths
had no observed metadata changes/missing files. `execute()` deliberately omits
serving-index compiler extras for a cancellation baseline or prior cancelled
outcome; that explains why a post-cancellation run can need one broad
membership-reconciliation pass even if serving facts already knew those SDK
files. The packet does not prove which extras generation 16 contained, nor
guarantee future convergence. Actual source: `coordinator.rs`'s cancellation
extra-input selection and `JobOutput` before/after enrollment rule.
Evidence: `saved-xcode-qualification-10/sdk-input-enrollment-01.json`, SHA-256
`50dcc37bef3a6ac1a8dd74e7d7fd0e22cc4c277d387f43f3597a1a8806e2d28b`.
No guard was removed or release telemetry added. Attempt 10 subsequently
converged; its final result and cleanup are recorded below.


### Frozen candidate: corpus pass and installed sandbox boundary

The optimized producer remains source `3ba4869d2b5f94b414e666cdbefbe97acae011bb`,
build ID `99cad771bd0f4385b5c9ee0b40665a64`. The full required Release core
run (`candidate-release-core-03`) passed all 27 checks; the six relevant
language/consumer checks, exact TypeScript 6.0.2 semantics, native TypeScript
7.0.2 graph checks, 290 Rust tests (two existing manual helpers ignored),
formatting and strict workspace Clippy also passed. Earlier failed core
attempts remain failed receipts, with scoped fixture corrections documented
above.

The owned Atelier attempt 10 passed in 1,952.71 seconds. It reached current
generation 19, then current generation 20 after automatic saved-source
restoration. Exact caller/callee/impact and UTF-8 source evidence remained
stable across SwiftPM, iPhone device and simulator contexts. Its 105 coverage
rows were 89 complete and 16 partial; graph capabilities remain explicitly
partial. This is warm retained-index qualification, not a cold full-index,
performance or video-encoding certification. The separate
`saved-xcode-qualification-10/cleanup-verification.json` confirms original
source bytes/mode/mtime restored, two CPU cores/2 GiB restored, writer lock
free and the owned coordinator stopped.

The first signed installed/offline attempt (`candidate-installed-offline-01`,
outside-checkout prefix `p003-01`) **failed** during SwiftPM manifest loading:
`sandbox-exec: sandbox_apply: Operation not permitted`. Earlier checks in that
process do not constitute a completed installed gate. No SwiftPM sandbox
bypass, manifest priming or serving-index preseed was applied.

Two benign `/usr/bin/true` controls isolate the boundary. Nested allow-default
policies succeed; adding an outer denial for an unrelated nonexistent file,
network access, or the checkout makes the inner sandbox application fail with
exit 71. The first control's assertion assumed all nesting failed and was
itself wrong; its retained failure is not a product result. Actual outcomes
are in `nested-sandbox-control-01/result.json` and
`nested-sandbox-control-02/result.json`. This establishes a composition problem
with the host validation policy, rather than a general inability to nest
sandbox profiles. SwiftPM must retain its own manifest sandbox.

Continue the independently selectable installed Rust/TS/C# gate under the
existing host protection. SwiftPM requires a separately identified isolation
gate with genuine checkout/network exclusion, not a relaxed run reported as
the original host gate. Signed common archives and separate signed bridge
archives are prepared; DMG, notarization, installed Xcode, actual previous
release upgrade and macOS 15 final-artifact gates remain pending. The user
selected separate bridge download and DMG; Swift compiler batching remains
deferred until packaging gates.


The user confirmed one GitHub 0.0.3 release containing both downloads. Public
bridge proof/checksum assets will use `xcode-bridge-` prefixes to avoid
collisions with the common runtime's files. Image-internal proof names and
their authenticated bytes stay unchanged; the bridge TAR/DMG remain separate.

Installed Rust-only attempt 02 also remains failed: its 90-second test wait
expired while the existing analyzer was still loading (generation 5,
observed epoch 19, published epoch 15). The provider's own readiness/request
deadline is 120 seconds per context, with normal/test contexts distinct.
This result does not establish an analyzer hang or a successful installed
Rust gate. A scoped harness wait correction must allow the provider to return
its actual outcome; compiler/product deadlines and resource limits stay fixed.

The no-share macOS 15 prerequisite-only VM boot reported 15.7.7/ARM64 and the
CLT Swift executable path, then stopped responding during the version probe.
A later bounded SSH observation timed out during banner exchange. The owner
stopped that attempt after 162 seconds; no candidate native code or indexing
ran. Exact Swift version and guest free space remain unknown. Sparse disk
allocation is not guest free-space evidence, and no resource-pressure cause
has been established. A source-reviewed Tart `--net-fd` discard-only socket
supervisor is prepared for a separately named isolation gate; it has not run.


### Signed installed gate and separate image preparation

Fresh `candidate-installed-offline-rust-03` passed all 36 checks in 487.18
seconds, using signed common stage `signed-01` and new outside-checkout prefix
`/Users/zakkeown/Code/.kco/.artifacts/p003-03`. Its result explicitly certifies
TypeScript, C# and Rust; Swift was not selected. This includes exact TS 6.0.2
and native 7.0.2, CLI/Node/.NET graph/source parity, Rust MCP, saved compiler
error/repair, offline/checkout exclusion controls, runtime relocation, npm 10
policy and damaged/missing resource checks. Final generation is 10. Neither
failed attempt 01 nor 02 is reclassified as passing. Read-only inspection of
both retained failed indexes later found published Rust normal/test contexts;
that supports available Rust facts, not a completed installed gate or a timing
claim (`installed-rust-timing-01/findings.json`).

Common and separate bridge images now each pass preparation, Developer ID
signing and read-only mounted verification against their authenticated
archives/native inventories (`candidate-{common,bridge}-image-*-01`). The
shared operator preserves four common native items; the bridge adapter
preserves exactly two and its full pinned runtime inventory. Public bridge
proof copies have the accepted prefixes; image-internal proof bytes stay
unchanged. These are local candidate images, not published or notarized
artifacts, and installed Xcode qualification is a separate run.

The first common Apple notarization submission was rejected before returning
any submission ID: the developer team has an unaccepted or expired agreement.
The Account Holder must resolve that in App Store Connect. The raw CLI receipt
is retained locally under `common-dmg-01/evidence/notary-submit.*`; temporary
URLs/credentials are not public assets. No automatic resubmission is allowed
after the marker, and no bridge submission occurred. The user was asked to
signal acceptance; other independent gates continue. This is Apple's account
prerequisite, not a sandbox/automatic-approval rejection.


### Installed Xcode and finalized notarized images

`candidate-installed-xcode-01` passed all 17 existing integration checks in
378.53 seconds, using the signed outside-checkout common install and separate
bridge under checkout/network denial. Its fresh owned device/simulator graphs,
CLI/Node/.NET parity, saved edits, compiler/source/PBX error recovery and
cancellation checks pass. The final result is
`/Users/zakkeown/Code/.kco/.artifacts/x003-01/summary.json`. This certifies the
finite current-host installed integration, not whole-language completeness,
encoding performance or older-Xcode compatibility.

After the user reported agreement acceptance, the immediate Apple retry still
failed with the same prerequisite. A later read-only submission listing worked
and contained no 0.0.3 jobs; attempt 03 submitted each image once. Both are
Accepted, with raw logs matched to the actual pre-staple image SHA-256 and all
required native CDHashes/ARM64 architectures:

| Candidate | Apple submission | Final stapled DMG SHA-256 | Outer index SHA-256 |
| --- | --- | --- | --- |
| Common | `70fbffbe-7efd-4cb3-b6a8-52e71fd3c938` | `37e572bb0b9caad2bf1c26314ef8ce8cce2a0ee218727f98b0488048b90dc0c5` | `e1a70ad15b442f07ebcdb959998292dbe0c7f155aaed81e4c1802fef9204b4af` |
| Bridge | `7b7101b4-0b2e-4f1d-a5e9-cdbcca3f3174` | `1671c7e5239b1f3a3538caa0736f5c3d0ce1715c82c44eb058df8172358e6241` | `a0c048d243f028334aa7be8f39f23a011db224cc3ca2264dd5e85793b2dde677` |

The common ASC staple wrapper returned failure after Apple's stapler reported
success, saying the target changed. Preserve that failed wrapper receipt.
Read-only ticket validation and the existing post-staple mounted inventory,
image signature and native-CDHash checks subsequently passed. The bridge used
Apple's `xcrun stapler` directly, followed by those same strict post-staple
checks. Both images then passed the existing shared finalizer and strict outer
CMS verification. The inner signing manifests remain unchanged and truthfully
say `not_performed` for their earlier stage; the outer indexes certify the
later actual notarization. The 12 common and 10 prefixed bridge public asset
names are disjoint (`final-candidate-assets.json`). Nothing is published.

The first actual published-0.0.2 transition probe failed on a fixture API error:
it sent response-only `coverage_scope` as a request field. Its cleanup passed;
no product upgrade failure was established. The original driver and failed
receipt remain retained. A two-line correction removes that request field and
asserts workspace coverage on the response instead. Both old/current request
schemas were checked before scheduling a new attempt.

The cloned macOS 15 prerequisite boot also failed its bounded SSH banner check,
before any candidate native code ran. Tart's supported graceful stop returned
zero. This supplies no macOS 15 candidate first-use, Swift compiler or SwiftPM
qualification; those gates remain open. No live development LaunchAgent or
original supplied repository was changed.


### Lifecycle registration deadline correction — new common candidate required

`candidate-installed-lifecycle-01` failed its first `watch` after two lifecycle
checks. The command took 0.637028 seconds and returned raw Darwin `EAGAIN`,
yet the retained registry, initialized lifecycle selector and Release personal
settings show registration side effects completed. The register request had
only a 500 ms socket reply timeout; durable index-family creation and registry
sync occur before the server acknowledgement. No stack trace identifies the
exact syscall or proves host disk contention. The source/evidence supports
correcting the acknowledgement budget without retrying an uncertain mutation.

The fixture did uninstall its unique owned runtime/login job before its cleanup
callback hit a late `__file__` NameError in the final inactivity probe. Preserve
that failed cleanup receipt. Root separately verified both owner locks free,
unique job/installation absent, socket refused with stale socket retained,
and original development PID/plist hashes/inodes unchanged
(`root-cleanup-verification.json` in the retained lifecycle prefix). A narrow
artifact-only cleanup helper correction is prepared for the next run.

Reviewed change `7ed3b3d` reuses the existing Execution/TimedStream for one
absolute five-second **register/refresh acknowledgement** deadline after strict
startup/version probes. It does not retry a timed-out acknowledgement, alter
ordinary query/settings/cancel budgets, or add telemetry. Three focused tests
cover delayed acknowledgement through public attach for both operations,
incomplete-frame typed expiry, and strict malformed/version failure. Source
peer review and targeted formatting passed; runtime tests are still pending.
This is not a whole-startup five-second guarantee.

The previously finalized source-3ba common image remains authentic evidence for
that provisional artifact, but this production fix requires a new matching
common producer/packages and relevant final gates. The independently frozen
bridge bytes remain unchanged. No old receipt certifies the changed core.
The user observed the diagnostic VM progress from black screen to Apple logo
and desktop; SSH still timed out, so no guest failure cause or candidate
qualification is inferred. The bounded console session stopped gracefully.


### Accepted validation scope change: no VM gate for 0.0.3

The user explicitly removed VM testing from this release: “Take the VM out of
scope, too big a PITA.” All VM runs are stopped; do not resume this work as a
release requirement. Keep the diagnostic artifacts and unexecuted operator
sources as research, with no candidate first-use/SwiftPM qualification claim.
The distribution floor remains macOS 15+ Apple Silicon, but 0.0.3 is qualified
on the current host and must disclose that macOS 15 was not retested. This
supersedes the earlier pending VM gate in this ledger/audit. It does not waive
remaining current-host installation, language, native or upgrade checks, and
it does not convert prior failed host SwiftPM sandbox checks into passes.

During the last diagnostic session the user saw the desktop, confirmed guest
IP 192.168.128.4 and Remote Login enabled. A direct probe received an OpenSSH
9.9 banner, then the existing SSH helper collected macOS 15.7.7/ARM64, CLT
16.4.0.0.1.1747106510, four GiB RAM and 15,713,836 KiB free before the console
session's bounded shutdown. The remaining metadata probe timed out; no Swift
compiler version or candidate code was qualified. The helper uses its first
IP argument correctly and has no proxy or hostname override. Boot/network
root cause was not established. One diagnostic helper reused its output name;
the first failed observation's reported fields were recovered from the retained
tool response with that limitation explicit in `health-receipt-recovery.json`.
Future observations use timestamped paths. No failed release gate was relabeled.
