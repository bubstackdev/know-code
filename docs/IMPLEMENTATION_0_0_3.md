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
