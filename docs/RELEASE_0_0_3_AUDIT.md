# 0.0.3 scope audit

Audited 2026-11-12 against shipped source `c77f972dd9c8604cdc7a0723eae8c3ee5e2692b0` ([0.0.2 — Snow Leopard](https://github.com/bubstackdev/know-code/releases/tag/v0.0.2)). Three read-only agent audits covered Swift, Rust and estate work; the integrator checked shared publication, packaging and validation boundaries. No new builds, benchmarks, compiler runs or corpus mutations were performed. Retained results below are historical evidence, not fresh certification of a 0.0.3 candidate.

Implementation-state observations in the initial audit below describe the
shipped 0.0.2 baseline; accepted requirements remain operative. See the
[dated Rust release addendum](#rust-release-preparation-addendum--2026-11-13)
for current unshipped Rust evidence, superseded corpus observations and the
remaining promotion boundary.

## Decisions and scope status

- Swift and Rust source support remain the accepted 0.0.3 language goal. Both adapters already exist, are packaged, and have real semantic navigation evidence. This is completion/promotion work, not two new providers.
- The user requested an audit before fixing the release scope, with estate work if capacity permits. Their selected estate priority is **symbol search across already registered repositories**.
- The user accepted a first estate-search slice with **one bounded result set, explicit truncation and workspace-specific drill-down**. Multi-repository pagination follows later. This bounds the feature if included; estate search remains conditional on capacity alongside the required language work.
- The user subsequently confirmed that **working Xcode device-and-simulator indexing is required for the 0.0.3 tag**. It is a release gate, not an optional stretch feature or a claim that the experiment is production-ready.
- The user also confirmed that **useful callers, callees and impact queries for both Swift and Rust are required for 0.0.3**, with unsupported language forms explicitly marked partial. Navigation alone does not satisfy that release bar. Semantic relationships must use the common fact/query path; missing capability must not appear as a complete empty graph.
- The accepted Xcode policy is **one selected shared scheme per workspace**, including its dependencies and tests for device and simulator. Select it automatically when exactly one shared scheme exists; require a workspace choice when several exist. This is the required behavior, not a claim that production selection is implemented.
- The accepted Rust execution policy permits Cargo build scripts and procedural macros under the **existing personal repository trust grant**, within shared resource limits. They execute project/dependency code. Reuse the existing execution-grant path rather than creating another permission layer; untrusted workspaces must not run them. The shipped adapter still disables both, so enabling them and validating generated/configuration provenance remain implementation work.
- The accepted Rust selection policy uses repository-configured target platforms (the host when unset) and default features, with workspace overrides for targets/features. Include libraries, binaries and tests with distinct compilation contexts. Read configured build targets rather than treating every platform mentioned in conditional dependency declarations as selected; do not silently enumerate every possible feature combination. The shipped adapter's single workspace view does not implement this policy yet.
- The user accepted a separately installed Xcode-only prerequisite with an actionable missing-tool error **if** the optional SwiftBuild bridge passes its remaining gates. This settles conditional packaging, not bridge adoption or technical qualification.
- **Required scope correction:** the user identified omitted TypeScript 7 support and confirmed that it is a **required 0.0.3 release gate, ahead of optional estate work**. Shipped TypeScript support remains 5.9.x only; the worker rejects later families. See [TS7 readiness](TYPESCRIPT_7_READINESS.md) for the native compiler/API transition. Exact supported versions, integration feasibility and evidence still need work; accepting the gate does not prove support or authorize compiler substitution.
- The accepted TypeScript support matrix is **5.9.x, 6.0.x and explicitly tested TS7 releases**. Preserve existing 5.9 behavior, prove TS6 through the legacy API path and evaluate exact native TS7 versions separately. This does not promise every future 7.x release or automatic prerelease support.
- Other acceptance suggestions and the work order below remain proposals. The finite supported graph forms, remaining Xcode integration boundaries and the optional bridge's adoption still need scope decisions; the confirmations above do not approve a particular dependency.
- Company onboarding/sync, semantic cross-repository graph traversal, Angular templates and refactoring continuity remain backlog requirements with release timing undecided after 0.0.2 became stabilization-only. Prior 0.0.2 assignments in older design tables are historical.

## What already works

| Area | Implemented behavior | Evidence and important limit |
| --- | --- | --- |
| Swift standalone | Selected compiler IndexStore symbols/references, overload/local identities, retained UTF-8 source/ranges and compiler-error partial publication/repair | [Extraction](../providers/swift/index.mjs), [fixture](../tests/swift_provider.py); retained release fixture has 57 symbols/79 occurrences. Navigation, not call/dependency completeness. |
| SwiftPM | Official build-server target/source/compiler-option planning, dependency preparation/checkpoints, separate target contexts, generated test entries and saved changes | [Adapter](../providers/swift/swiftpm.mjs), [tests](../tests/swiftpm_provider.py). Retained atelier-core root-package result: 12 complete navigation contexts, 53,693 symbols, 139,354 occurrences, source preservation; `.artifacts/swiftpm-real/1794001089425651000/validation.json`. Does not certify every nested package, Apple SDK variant or macro/plugin. |
| Rust Cargo navigation | Workspace/member deduplication, repository-selected rust-analyzer definitions, signatures, locals, cross-file references and retained exact source | [Worker](../providers/rust/worker.mjs), [navigation](../providers/rust/navigation.mjs), [tests](../tests/rust_provider.py). Retained fixture: 31 symbols/60 occurrences; lars-sdk worker run: 6,789 symbols/28,252 occurrences across 44 files after manual dependency preparation. Rust coverage remains explicitly partial. |
| Shared integration | All four providers use the same worker contract, coordinator, SQLite publication and native queries; scoped saves and toolchain/config changes are tested | [Mixed-language regression](../tests/languages.py), `.artifacts/languages/latest.json`. Rust core/native parity is not rustc-based source extraction; rust-analyzer is the Rust source semantic engine. |
| Distribution | Runtime already contains the Swift/Rust adapters and the signed Swift IndexStore reader | [Packaging](../scripts/package.py#L72). Both languages are labeled `experimental-v0.0.3`; shipping bytes does not establish supported-language acceptance. |

## Language gaps that matter

| Gap | Actual boundary | Proposed disposition |
| --- | --- | --- |
| Swift graph evidence | [Fact emission](../providers/swift/index.mjs#L130) emits symbols/occurrences only. [Native reader](../providers/swift/indexstore-reader.c#L65) preserves containment, not the richer relationships; implicit occurrences are skipped. | Extract a stated compiler-backed call/dependency slice and explicit capability coverage into existing facts/operators. Do not reconstruct calls from identifier references or promise every lowered operation. |
| Rust graph evidence | [Worker](../providers/rust/worker.mjs#L89) initializes relationship arrays but never populates them; occurrences have an empty enclosing symbol. | Establish actual semantic call/dependency evidence and ownership before offering Rust callers/callees/impact as supported. Names, hovers and navigation tokens alone cannot prove calls. Broader dispatch can remain explicitly partial. |
| Useful Swift context | Swift currently uses `signature: item.name`; [mixed-language tests](../tests/languages.py#L122) explicitly expect `extent_unavailable`. | Add useful typed signatures and compiler-associated declaration/body context; retain exact indexed-source ranges and honest unavailable cases. |
| Rust Cargo scope and ownership | One analyzer view per Cargo workspace; no exhaustive per-target/feature context model. [Source discovery](../providers/rust/worker.mjs#L20) recursively scans member directories, including potentially unowned sources; every source is labeled `generated: false`. | Implement the accepted repository-target/default-feature policy with workspace overrides and distinct library/binary/test contexts. Preserve evaluated ownership, generated/test provenance and invalidation; avoid treating vendor/orphan/worktree source as compiler-owned. Do not expand every feature combination implicitly. |
| Rust dependency setup and generation | Cargo restoration is external today. Build scripts/proc macros are disabled and generated-source coverage is incomplete. | Integrate ordinary dependency restoration with the existing preparation path. The user accepted build-script/proc-macro execution under the existing personal repository trust grant and shared caps. Preserve input/generated-source provenance, cancellation and trust checks; report unsupported generation explicitly. |
| Last-unit removal | [Shared publication](../crates/core/src/store.rs#L854) rejects zero compilation units. Existing Swift group-deletion testing retains other languages. | Fix the common empty-publication path with narrow regressions for a language-only workspace. Do not create provider-specific fake compilations to represent emptiness. |
| Operational coverage | Shared scheduling/resource enforcement already exists; adapter-specific compiler/planner peaks and complete installed workflows are not established by navigation fixtures. Swift cache rotation/abandoned scratch cleanup also remain unfinished. | Reuse existing control/cleanup infrastructure and measure representative cold indexing, saved edits, memory/disk use and query responsiveness. Honor the accepted shared caps; resource-limited outcomes must stay explicit. |

### Xcode is the largest planning risk

The [production adapter](../providers/swift/xcode.mjs#L14) currently emits an honest partial planning report with **zero semantic facts**. Scheme/xcconfig invalidation works; it does not establish Xcode source support. The installed indexing-settings CLI was rejected for inconsistent device/simulator selection; do not repeat that route as a shortcut.

The optional SwiftBuild/BSP experiment passed an owned app/UI-test fixture for device and simulator plus saved-source changes (`.artifacts/xcode-bsp/validation.json`). It is separate from production. Required work includes request-enforced script disabling, dependency-module preparation, context/scheme selection, restoration/input ownership, real preserved snapshot validation, cancellation and bounded resource use. Raw BSP URIs must remain namespaced by context. See [Xcode boundaries](XCODE_PROVIDER.md).

SwiftPM-first and distinct device/simulator support remain accepted requirements, and working Xcode indexing now explicitly gates 0.0.3. Use one selected shared scheme per workspace, including its dependencies and tests; choose the sole shared scheme automatically and require a choice when several exist. Bridge adoption remains undecided: the earlier authorization covered experimentation. If it qualifies, the user accepted a separately installed Xcode-only prerequisite with an actionable missing-tool error. Its retained approximate 90 MiB staged size and ordinary 65–87 MiB service RSS do not establish compiler peak memory or production overhead. Xcode is substantive implementation work; conditional packaging does not select or qualify a runtime dependency.

## Estate candidate: bounded symbol search

This is the user's preferred optional addition. Existing registration, parent discovery and background indexing can supply the workspace set. Current [read views](../crates/core/src/query.rs#L390), [Workspace API requests](../crates/core/src/control.rs#L533) and [MCP attachment](../crates/host/src/mcp.rs#L55) each select one database, so estate search still needs implementation.

Recommended smallest useful behavior:

- Search symbol names/signatures over registered workspaces through the common Rust search operators, returning workspace-qualified findings and exact drill-down locations.
- Preserve each workspace's snapshot, target evidence, coverage and freshness. A collection of repository snapshots is not one globally atomic estate snapshot. Do not merge same-named symbols from different repositories.
- Report unavailable, untrusted, not-ready and stale workspaces explicitly. An incomplete estate search must not look like an exhaustive empty answer. Searching retained facts must not create an execution trust grant or register/clone arbitrary repositories.
- Use one overall request deadline, aggregate work/result/response bounds and the existing shared admission scope. The proposed freshness allowance is the existing 500 ms default for the whole estate request, capped by its overall deadline, rather than another 500 ms per repository. Do not loop the public per-workspace API with a fresh deadline, freshness wait and resource allocation for every repository. Bound simultaneously retained readers and release them on cancellation/deadline.
- The accepted first slice returns one bounded result set with explicit truncation and workspace-specific drill-down. Multi-workspace pagination follows later; it would require coherent snapshot-vector/lease semantics. Do not reuse a single-workspace generation token misleadingly. Existing workspace-specific pagination remains available for drill-down.
- Reuse the existing FTS/symbol search and in-process bindings. No central duplicated symbol index, new graph engine or whole-estate RAM cache is justified for this slice. Exact API, ranking and workspace scope selection remain design decisions.

This does **not** implement semantic cross-repository callers/impact. Those paths require exact version/context/snapshot selection beyond the current single-database graph joins. Company cloning/sync is also a separate feature: authenticated `gh`/`glab` scope is not permission to execute repository code, and managed updates need durable state, bandwidth/disk controls and a policy for local changes. See [company onboarding](COMPANY_ONBOARDING.md) and [scaling](SCALING.md).

## Proposed parallel work and release gates

| Lane | First concrete checkpoint | Shared-change ownership |
| --- | --- | --- |
| TypeScript compatibility, required | Prove the accepted 5.9.x/6.0.x/tested-TS7 matrix: preserve 5.9, exercise the TS6 legacy API and establish native TS7 extraction with correct identity/context evidence. Evaluate stable 7.0 separately from the new 7.1 beta API. | Own narrow TypeScript compiler adaptation. Reuse worker preparation/framing and the existing core; no silent legacy or different-native-version substitution. |
| Swift/Xcode | Demonstrate usable compiler-backed relationships through the existing IndexStore reader, and qualify a reliable device/simulator planning/preparation path with the selected scheme, scripts disabled and real saved-snapshot evidence. | Own Swift adapter/helper code. Bring dependency-adoption evidence before committing to the optional BSP bridge. |
| Rust | Demonstrate trustworthy call/dependency evidence and selected-context/source ownership; then integrate restoration and trusted build-script/proc-macro preparation. | Own the Cargo/rust-analyzer adapter. Preserve the shared trust/preparation path and distinguish supported forms from partial boundaries. |
| Integrator | Settle common fact/settings contracts required by both lanes, repair empty-publication behavior, and review cross-language queries, resource use and packaged acceptance. | One owner for shared protocol, settings, core/publication, SDK/API and build/validation scripts. |
| Estate, conditional | Add bounded symbol search over registered indexes after its shared request/admission contract is settled. | Reuse Rust operators and aggregate budgets; do not add semantic graph composition or estate cursors. Language completion has priority. |

1. Establish TS7 integration feasibility and the supported capability matrix before estimating estate capacity. TS7 and Xcode device/simulator indexing are required release gates. Avoid an exhaustive Swift/Rust compiler-parity promise; explicitly partial advanced dispatch/generation is compatible with an honest supported slice.
2. Run separate Swift and Rust adapter lanes. Give one integrator ownership of shared fact/protocol/settings/publication/query changes; agree any shared contract change before both lanes depend on it.
3. Keep estate symbol search as a separable lane after its shared execution/admission contract is settled. It should not wait for new cross-repository semantic identities, but must preserve repository-qualified identities. Limit active heavy build/compiler runs to one owner.
4. During development run affected existing provider/toolchain/languages checks and focused graph/publication regressions. Do not repeat all historical suites for each adapter edit. Estate checks should cover one failed/stale workspace, bounded many-workspace traversal, global caps, admission/cancellation, provenance and drill-down coherency.
5. At release, promote meaningful SwiftPM/Cargo workflows into required packaged/native/CLI/MCP acceptance. [Current installed coverage](../tests/package_install.py#L323) is an opt-in tiny symbol smoke; the 0.0.2 default 30-check TS/C# archive gate does not certify SwiftPM, Xcode, Cargo graphs or their prerequisites. Xcode device/simulator indexing must pass actual production-path acceptance before tagging 0.0.3; the separate experiment cannot substitute for it.
6. Run the required core plus relevant language/estate matrices, then one matching distribution/signing/platform checkpoint. Extend the existing harnesses instead of adding a parallel validation framework. The user-deferred real video-encoding benchmark remains unmeasured; this audit does not turn it into a pass or silently reimpose it as a release blocker.

**Current direction:** deliver TS7 compatibility and useful, explicitly scoped Swift/Rust graph support with production Xcode device/simulator indexing. Bounded estate symbol search remains the sole optional feature candidate after required work. There is no evidence yet for a schedule or a claim that all of it fits; native TypeScript integration, production Xcode and Rust semantic/context work determine capacity.

## Remaining release integration audit — 2026-11-13

Read-only review of candidate `a3cbae8`; no new build, installed, signing or
platform pass is asserted here. Earlier sections describe the initial audit,
not current implementation: bridge adoption is now approved, and matching
Debug production Xcode coverage includes 16 checks for both SDKs, native graph
parity, partial publication/repair and actual preparation cancellation. See
[the current ledger](IMPLEMENTATION_0_0_3.md#integrated-7-metadata-rejection-and-cache-repair)
and [its cancellation receipt](../.artifacts/implementation-0.0.3/wave-01/assembled-xcode-integration-06/).

### Accepted gates still requiring a release checkpoint

- Promote only the finite validated language/compiler contract, including exact
  tested TS7 releases, useful Swift/Rust graphs and Xcode device/simulator
  support. [The packager](../scripts/package.py) still labels Swift/Rust
  `experimental-v0.0.3`; [README](../README.md) and package versions still
  describe 0.0.2. Update prerequisites and honest unsupported/partial forms
  alongside that promotion, rather than treating shipped adapter bytes as
  supported-language evidence.
- Run one matching optimized core/relevant-language checkpoint and the existing
  installed/native/CLI/MCP workflow on final artifacts. The expanded
  [installed SwiftPM/Cargo checks](../tests/package_install.py) remain **unrun**;
  nested SwiftPM manifest sandboxing under the outer offline/checkout-denial
  policy is an explicit integration risk. Installed selected-native TS7 and
  new-language MCP graphs also lack current artifact-bound evidence; the
  existing MCP section precedes the opt-in language section and covers TS/C#.
- Complete final distribution signing/notarization, normal-security install,
  actual 0.0.2 upgrade/rollback/native-contract and declared-platform checks
  using the existing [packaging](PACKAGING.md), [local-install](LOCAL_INSTALL.md)
  and [validation](VALIDATION.md) mechanisms. Preserve exact producer/artifact
  identity, extraction-policy backfill, retained data and compatibility errors.

**Concrete separate-tool packaging gap:** the
[bridge builder](../tooling/xcode-bridge/build.py) inventories unsigned staged
CLI/service bytes in `know-code-xcode-bridge.json`, and
[the adapter](../providers/swift/xcode.mjs) checks those exact hashes. Developer
ID signing changes the bytes. The final tool inventory must describe the
signed payload, retain the unsigned-to-signed provenance and pass installed
verification without weakening those checks.
[Common signing](../scripts/sign_packages.py) currently handles exactly the
runtime/npm/NuGet archives; it does not package/sign this separate bridge.
The bridge's deployment target alone also does not establish older Xcode/macOS
compatibility; qualify and state its toolchain boundary separately.

### Auditor recommendations, not additional accepted feature scope

- Use one representative installed new-language MCP graph and the existing
  real-HOME lifecycle/native-contract drivers, rather than multiplying every
  language by every transport or rerunning all historical matrices. Final
  artifact evidence remains necessary; this is a proposed economical route.
- Finish bounded real-corpus qualification with current implementations.
  Candidate `a3cbae8`'s earlier observations recorded the failed 2 GiB Atelier
  attempts and an isolated 4 GiB diagnostic request then awaiting a decision.
  That waiting state and the claim that Lars evidence is only old navigation
  are superseded by the dated addendum below and the
  [current implementation ledger](IMPLEMENTATION_0_0_3.md#real-retained-graphs-and-cache-path-qualification).
  The later user-approved Atelier-only allowance did not change the 2 GiB
  default or certify the failed earlier attempts. Preserve external-input and
  resource gaps; this recommendation does not require every unsupported nested
  package to become complete.

The real encoding-performance gate remains explicitly deferred. Optional estate
search and unaccepted initial audit proposals do not become release blockers
through this checklist.


## Rust release-preparation addendum — 2026-11-13

Read-only review of candidate `f49e57f`; no new compiler, test, build or runtime
execution. **Shipped 0.0.2 remains experimental.** The unshipped 0.0.3 source
implements compiler-captured Cargo contexts, trusted build-script/proc-macro
preparation and useful partial graphs. This addendum records existing proof and
remaining release work; it does not promote support or waive artifact gates.

The finite slice is Cargo workspaces with repository-selected targets/default
features and explicit overrides; distinct library/binary/test contexts; exact
saved-source navigation; authored direct calls and value/type dependencies;
and bounded callers/callees/impact through the common core. Dependencies remain
lookup-only within a selected root graph. Compiler identity, cfg, pre-compilation
input hashes and emitted/consumed artifact joins remain mandatory.

| Existing proof | Actual boundary |
| --- | --- |
| [Host-tool/context fixtures](../.artifacts/implementation-0.0.3/wave-01/rust-tooling-host-contexts-01/) and [expanded discovery regression](../.artifacts/implementation-0.0.3/wave-01/rust-stale-include-discovery-02/) | Intermediate host-library preparation, exact macro artifacts, selected contexts and bounded stale-output/include discovery pass. The preceding failed cold discovery attempt remains retained. |
| [Lars production-default-03](../.artifacts/implementation-0.0.3/lars-production-full-01/production-default-03/) | At unchanged 2 CPU/2 GiB defaults, published current generation 6 with 19 coverage rows and seven selected Rust contexts, including normal/test lars_core. This run still **failed** its original closure-call expectation; it is not a full-corpus pass. |
| [Lars direct-call/native probe](../.artifacts/implementation-0.0.3/wave-01/lars-published-direct-01/) and [result](../.artifacts/implementation-0.0.3/lars-production-full-01/published-direct-01/result.json) | Passed in 1.50 seconds against retained generation 6: actual softmax→pairwise_sum direct callers/callees/impact, complete saved UTF-8/hash/ranges and CLI/Node/.NET equality. Reads were explicitly **stale after coordinator stop**. This is neither current indexing nor installed/optimized/performance certification. |

No further Rust feature blocker is established by these finite results.
Native `-l` (ring), selected wrappers/configuration includes/escaped dep-info,
metadata-only macros without emitted dylibs, conservative expansion-origin
omissions, arbitrary macro file/environment/nondeterministic behavior,
closures/async deferred bodies, implicit operations, trait/dynamic dispatch
and cross-unit body links remain explicit gaps. Do not remove them to make Lars
complete or relabel partial reports as exhaustive. In particular, logaddexp0's
call inside logistic_loss's closure is deliberately outside current execution
ownership; the direct-call probe preserves that failed expectation.

Before a 0.0.3 promotion, the remaining Rust release gate is **matching final
artifact evidence**: optimized core/relevant language checks, packaged/offline
Cargo graph and saved-error/repair behavior, native/CLI and representative
new-language MCP queries, followed by the shared signing/normal-security
install/upgrade/platform checkpoint. Reuse the expanded
[installed-package harness](../tests/package_install.py): its Cargo/SwiftPM
section is still behind `--experimental` and must be required for this release.
Its existing MCP section precedes that section and currently proves TS/C#;
`c05caf9` adds one representative installed Rust MCP graph check to the later
experimental section using the existing helpers and Cargo fixture. It checks
compiler-bound paths, snapshot/generation and source provenance against native
queries, but has only passed syntax checks so far. Running that packaged gate
must close the evidence gap. This addendum asserts no final installed or
release pass.

The smallest promotion changes, **after those gates**, are:

- In [scripts/package.py](../scripts/package.py), change Rust's
  `experimental-v0.0.3` label to the required-provider contract and describe
  separately installed Cargo/rustc, rust-analyzer and matching rust-src when
  indexing Rust. Existing evidence uses the 1.99.1 toolset; repository MSRV 1.85
  is not a validated compiler matrix. Preserve selection/errors without silent
  tool substitution or installation.
- In [crates/host/src/runtime.rs](../crates/host/src/runtime.rs), extend the
  existing required-provider presence check beyond TS/C# when Rust is promoted;
  otherwise omitting Rust's manifest entry bypasses resource validation despite
  a required-language claim. Reuse the existing damaged-package checks.
- Reconcile the current contract in [README](../README.md),
  [Rust provider](RUST_PROVIDER.md), [compiler acceptance](COMPILER_ACCEPTANCE.md),
  [packaging](PACKAGING.md) and [validation](VALIDATION.md), plus the normal
  coordinated Cargo/npm/NuGet version bump. AGENTS.md and
  PRODUCT_CONSTRAINTS.md must distinguish their old disabled/single-view
  **shipped baseline** statements from current implementation. Keep worker
  graph capabilities `partial` and `rustContextPolicy=2`; promotion requires no
  fabricated completeness or new fact/protocol contract.

The current Swift/Xcode and TS7 gates remain independently required. Optional
estate work and the explicitly deferred encoding benchmark acquire no new
release requirement from this Rust review.
