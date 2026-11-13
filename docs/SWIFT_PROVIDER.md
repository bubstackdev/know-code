# Swift compiler provider

Status: a development slice for standalone Swift and SwiftPM symbol/reference indexing through the existing core. SwiftPM uses its official build server for project settings and indexing preparation. Xcode project planning remains an explicit gap. This is not a complete v0.0.3 release: full typed signatures, call/impact extraction, custom macro preparation, and external build-tool generation remain unfinished.

## Shared boundaries and supported scope

The Node worker reuses [the shared versioned Protobuf codec](../providers/shared/protocol.mjs), the 8 MiB frame bound, and 128-record fact batches. Its SwiftPM client reuses [the shared compiler-tool JSON-RPC transport](../providers/shared/jsonrpc.mjs) used by the Rust provider. The native helper reads the installed toolchain's public `libIndexStore` C API. It adds no IndexStoreDB database, storage engine, query engine, hosted service, or usage collector. Rust still owns publication, queries, native .NET/Node parity, and saved-file coordination.

Standalone saved `.swift` files are grouped by immediate directory under `$swift-inferred:<relative directory>` (`.` at the workspace root). The adapter runs compiler type checking with inferred module settings; those assumptions are visible. Explicit SwiftPM/Xcode ownership excludes files from standalone fallback, including scoped refreshes. Broken explicit settings are never replaced with inference.

SwiftPM creates a separate compilation context for each source-bearing target, including test targets and SwiftPM-generated test entry documents. This slice analyzes the selected host macOS SDK/configuration; it does not enumerate additional Apple platform variants. Compiler USRs identify symbols within the compilation context. Compilation IDs contain the package path and SHA-256 of the exact BSP target URI; the full target URI remains in analysis options. This avoids repeating a long URI in every occurrence without discarding target provenance. Physical source payloads still deduplicate through the shared Rust store.

Restored dependencies provide compiler inputs and external identities. Their source is not published as project source. Nested/local packages have their own selected package ownership; one package request does not duplicate another package's target facts. Local dependency inputs can still be watched when consumed for accurate binding. Aggregate product targets without source are not separate source compilations.

IndexStore provides compiler USRs, display names, kinds, declarations/references, and containment evidence. Locals and parameters are included. Overloads retain distinct USRs. The initial signature field is the compiler's IndexStore display name, such as `choose(_:)`, rather than a full typed signature; `analysis_options.signatures` states `compiler_index_names_only`. Language-specific display names need not equal the source token at a compiler anchor.

## 0.0.3 relationship checkpoint

The reader now preserves compiler relation roles and related symbol identities,
kinds and subkinds. The adapter emits calls only for IndexStore `CALL` records
with an actual callable `CALLEDBY` owner. A function reference without `CALL`
stays a dependency, not an invocation. Accessor/operator/conversion calls retain
an implicit label; dynamic compiler bindings remain virtual declarations, not
proven concrete runtime targets. Source-backed containment, value/type
dependencies and compiler ancestry feed the same bounded Rust operators as
the other languages. Per-record deduplication and 128-record batches avoid a
second in-memory edge graph.

Call, dependency and dispatch capabilities are explicitly partial, separately
from a healthy compiler report. Unsupported Xcode/SwiftPM planning reports make
those capabilities unavailable. Cross-project bodies and receiver-specialized
dispatch proofs are not synthesized from names or USRs.

The [owned raw compiler probe](../.artifacts/implementation-0.0.3/swift-relations-01/result.json)
exposes an important limit: stored-property initialization, immediate global
initialization and a deferred stored closure all supply `CALL` with variable/
property `CONTAINEDBY`, without `CALLEDBY`. Containment alone cannot establish
when those calls execute. This slice retains their compiler-backed static
dependencies and reports unavailable execution ownership; it creates no fake
file initializer or call attributed to later variable use. Filling those
forms needs reliable additional scope evidence. Implicit calls without usable
source anchors, complete import coverage and broader dispatch also remain gaps.

That probe built the reader and indexed one frozen owned Swift fixture; it did
not certify the new graph adapter, common-core integration or installed SDKs.
The focused Swift tests now request those relationship distinctions; their
matching build/run evidence must be supplied by the integration owner.

## SwiftPM planning and preparation

The adapter uses the selected toolchain's `swift package` commands to evaluate manifests and restore dependencies, then starts `experimental-build-server`. It requests the target/source inventory, indexing-only `buildTarget/prepare`, and per-file `textDocument/sourceKitOptions`. SwiftPM owns target membership, compiler flags, platform deployment settings, feature switches, dependency-module planning, and working directories. Know Code does not reconstruct those from a hand-written build engine.

The accepted module-preparation request budget is ten minutes for SwiftPM and
Xcode `buildTarget/prepare` only. Metadata, initialization, source/options and
shutdown requests retain their 120-second default. Query deadlines and shared
CPU/memory limits are unchanged. A longer preparation budget does not establish
successful preparation or relax compiler/transport failure handling.

Installed Swift 6.4.2 uses unprefixed BSP extension methods; newer upstream definitions use `sourcekit/`. The client attempts the newer method and falls back only for structured JSON-RPC error `-32601` (method not found). Actual setup/protocol failures are not masked by fallback. The requested target's canonical compiler path must match the selected compiler; there is no silent toolchain substitution.

Dependency restoration uses the existing `PreparationResult`/progress contract. Checkpoints cover the request's configuration fingerprint, compiler/SDK identity, workspace state, resolved package paths, and manifest/lock hashes. Source-only saves reuse valid successful setup and retain failed setup until changed inputs or explicit retry. Compiler module preparation still runs through SwiftPM's incremental machinery so a saved API change cannot leave dependents using an old module. The focused regression renames an exported API while introducing a compiler error: the consumer becomes partial and does not resolve the removed API from cached output; repair restores complete coverage.

SwiftPM derived-state directories and preparation fingerprints share an explicit
compatibility tag and canonical central work root. The canonical-work layout
does not reuse older trees whose serialized module paths used an alias. Each
affected package needs one cold setup; prior trees remain on disk, with their
retention/disk cost unchanged by this fence. No cache is deleted or retried in
response to a compiler crash. Compiler/SDK selection, semantic flags and saved
input validation remain intact. Compiler-provided context identities can change
once with the new derived layout; warm and saved updates must remain stable
within it. Matching real-corpus qualification is still required.

Root and resolved dependency manifests are preflighted before creating the BSP server, because SwiftPM can run build plugins while constructing its build graph. This slice reports external build-tool plugins as a coverage gap rather than building or executing them. This preflight is conservative, including plugin declarations/usages in the resolved graph. Custom `.macro` tool targets are reported separately as compiler-integrated macro preparation gaps; no automatic tool/product build is attempted. Preinstalled toolchain macros, including Swift Testing in the verified Xcode context below, can be consumed through the actual compiler options.

All SwiftPM work uses central scratch/cache directories and `--jobs 1`. Verified preparation produces Swift modules and generated project source without object/product builds. Resources or separately generated outputs missing from a supplied source snapshot remain explicit gaps, not evidence of a defect in the original project.

After preparation, the selected Swift driver runs `-index-file -index-file-path` for each requested source using its actual semantic arguments, an isolated job IndexStore, and unique index-unit output paths. Action/output/index options are replaced explicitly. The full semantic settings are retained once per distinct exact argument-list/working-directory pair; source entries refer to that settings identity. A 48-source real target needed about 24 KiB of options rather than duplicating the module file list 48 times. The current shared queries still return full `analysis_options` in coverage, so this metadata can make AI responses verbose. Deduplication is not compact projection/budget support; that remains shared query work.

Saved dependency inputs come from BSP target/dependency-closure source membership, exact modulemap arguments, and public IndexStore unit/file dependencies. Mutable compiler outputs and toolchain/SDK state are excluded from saved-source watching. The system-library fixture verifies both its header and modulemap become inputs. Direct frontend dependency-output flags were rejected after a real multi-file driver conflict; no custom header/modulemap parser was added.

## Exact source evidence and bounded ingestion

The shared [compiler argument helper](../providers/swift/compiler.mjs) redirects
output/action slots for per-file IndexStore work and removes exactly three
paired `-Xfrontend` module-preparation shortcuts: lazy typechecking,
skip-non-exportable declarations, and allow-module-with-compiler-errors. Selected
SDK/target, language version, defines, library evolution, imports and plugins
remain intact. Fast module preparation does not use this helper and stays
unchanged; readiness is separate from semantic completeness.

The owned Xcode library-evolution fixture established this boundary: preserving
the active shortcuts retained private/local declarations but lost required body
calls; explicitly removing the three pairs recovered those calls/value-reference
evidence in both device and simulator contexts. Independent body-error checks
then reported partial coverage and the actual unresolved-value diagnostic,
without inventing its symbol. Earlier non-evolution successes had explicitly
ignored lazy/skip flags and did not cover that active case. These are finite
compiler/IndexStore results, not complete Swift coverage or production Xcode
bridge adoption. Retained receipts preserve the [failed active baseline](../.artifacts/implementation-0.0.3/xcode-evolution-graph-preserve-01/result.json),
[full-body facts](../.artifacts/implementation-0.0.3/xcode-evolution-graph-full-body-01/result.json)
and [body-error result](../.artifacts/implementation-0.0.3/xcode-evolution-body-error-full-01/result.json).
The helper still requires the matching SwiftPM/native validation batch after
integration.

Source bytes retain SHA-256 identity and half-open UTF-8 ranges. IndexStore gives one-based UTF-8 line/column starts. The adapter scans only the token beginning at that compiler position; it never searches by symbol name or infers a binding. LF, CRLF, CR, UTF-8 BOMs, ordinary Unicode identifiers, escaped identifiers, and operators are covered by fixtures. Source bytes are rechecked after compiler analysis, preventing ranges from attaching to different saved text.

Compiler anchors include punctuation: a subscript or array-type initializer can point at `[`, and an implicit computed getter's definition can point at `{` even without IndexStore's Implicit bit. Range operators begin with `.`. These are exact source tokens attached to compiler-provided identities/roles, not guessed identifier locations. Dot characters are accepted inside an operator only when its first character is a dot; a valid adjacent postfix `+` and `..<` remain separate tokens. Genuinely unsupported positions still mark coverage partial. Compiler-marked implicit occurrences without an independent token are omitted.

The native reader skips system units, reads occurrence records only for requested Swift sources, and deduplicates records shared by units. Other relevant non-system unit metadata supplies dependency inputs without collecting SDK-wide occurrence inventories. Its JSONL output spools to a durable per-job file and is read twice: first for symbol/internal-external classification, then for bounded occurrence batches. Occurrences are not retained for the entire module. Deduplication is scoped to a record; compiler symbol identities remain target-scoped. The small native record/source lists use simple linear lookups; estate-scale performance is not established.

Ordinary compiler errors publish valid partial facts with diagnostics. Missing primary units or unsupported ranges remain partial. Toolchain/helper incompatibility, process timeout/signal, compiler crash, malformed records, or protocol failure aborts publication through the common core. Typed BSP task completion distinguishes recovered initialization logs from current setup errors: a known initial missing build description is retained as recovered information only after SwiftPM reports successful graph reload. Real failed reload/preparation and compiler errors remain visible.

## Toolchains, state, and limits

The v0.0.3 worker consumes the coordinator's typed Swift analysis-policy branch
at version 1. An optional `scheme_path` must identify an existing, absolute,
normalized `.xcscheme` file; wrong branches/versions and invalid paths fail before
provider acknowledgment or tooling. Legacy requests without policy retain their
low-level behavior. The explicit saved path participates in Xcode input evidence
and reported selection. The accepted Xcode policy selects the shared scheme Test action configuration
for the app, dependencies and tests; it does not expand the Run configuration.
Missing Test action/configuration must be actionable. Actual shared-scheme
membership, automatic sole-scheme selection and planner target/configuration
mapping remain required Xcode work;
accepting a policy path does not establish a supported Xcode graph. The shared
schema/settings integration is owned by the integrator.

The worker requires separately installed Swift 6.x, its matching `libIndexStore.dylib`, an Apple SDK, and the existing Node runtime. The tested compiler is Apple Swift 6.4.2; other Swift 6 versions are not validated and Swift 5/7 are rejected. No compiler or version manager is installed or invoked to download a compiler.

[Toolchain selection](../providers/swift/toolchain.mjs) is scoped to each package or standalone directory. Explicit `DEVELOPER_DIR`/`TOOLCHAINS` overrides win over automatic discovery and must satisfy any repository pin; conflicts and missing overrides fail with an actionable error. An explicit `SDKROOT` also retains the active developer context unless another override selects it; automatic Xcode preference does not replace an existing SDK choice. `SWIFT_EXEC`/`SWIFT_EXEC_MANIFEST` may confirm the selected compiler; a different executable requires selecting its matching installed toolchain with those standard overrides, rather than silently mixing compiler sets. Environment overrides apply to the worker/coordinator process; setting one only in a query consumer does not reconfigure an already running coordinator.

Without an explicit override, selection honors the nearest recognized Swift pin (`.swift-version`, or the `swift` entry in `.tool-versions`) and the nearest Xcode pin (`.xcode-version`, or the `xcode` entry in `.tool-versions`). Conflicting entries in the same directory are errors. Numeric Swift versions match exactly, or by minor series for a two-component pin; installed toolchain bundle names/identifiers are also accepted. `system` explicitly uses the active developer selection and `xcode` requires Xcode. Moving version-manager aliases and uninstalled versions are not resolved or downloaded. `Package.swift`'s tools-version remains SwiftPM's minimum-version requirement, not an exact toolchain pin. Other manager-specific configuration formats are not interpreted by this slice.

For an unpinned project, prefer a compatible installed Xcode: active full Xcode first, then conventional `Xcode.app`, then other `Xcode*.app` installations in `/Applications` and `~/Applications`. Standard system/user and embedded Xcode `.xctoolchain` directories supply installed candidates for pins. A custom app location/name can be selected explicitly through `DEVELOPER_DIR`. Probe the actual compiler, matching Swift/IndexStore, and SDK; later BSP capability checks still apply. If no Xcode is installed, use the active CLT selection. An installed but incompatible/broken Xcode is reported rather than hidden by an automatic CLT substitution; an explicit CLT override remains available.

The selected environment is passed to manifest evaluation, restoration, BSP and compiler subprocesses without changing `process.env` or global `xcode-select`. An inherited `SDKROOT` is preserved and exposed as `sdkOverride`; SDK choice is passed explicitly through SwiftPM's `--sdk`, and returned compiler settings must match it. An unsupported explicit compiler/SDK combination fails rather than silently substituting a different SDK. This matters for launchers such as Apple's Python that inject an SDK path into child environments; default-policy fixtures clear that injected override deliberately. Preparation/module caches include effective compiler/SDK identity. Coverage records `toolchainSelection`, compiler path/version and SDK identity, including pin origin. Saved pin creation/change/removal uses the shared invalidation path; existing ancestor pins are also tracked. Newly created pins outside the registered workspace and tool installation/global-selection changes may still require explicit refresh. Refresh existing Swift indexes to adopt this new default; provider code upgrades alone do not yet trigger automatic reanalysis.

A controlled CLT 6.4.2 probe found inconsistent SwiftPM compiler options for Swift Testing: a later response could omit its required plugin path. That yields honest partial compiler diagnostics rather than guessed search paths. The verified Xcode context passed actual `@Test`/`#expect` indexing cold, warm, and after a source save. [Original semantic evidence](../.artifacts/swiftpm-testing/latest.json) predates the default-policy change; the current SwiftPM regression also checks macro indexing and saved updates when Xcode is selected. Global developer selection remains unchanged. This is a tested context, not a promise that all CLT/Xcode versions behave alike.

Central state lives under `KNOW_CODE_DATA_DIR/workers/swift/`, or `~/Library/Application Support/Know Code/workers/swift/`. Per-job IndexStores/spools are removed after ordinary completion/failure; module and package caches are reused. Cache rotation and reclaiming job scratch abandoned by process termination remain unfinished. Commands have a 120-second timeout; worker frames and individual helper records are bounded at 8 MiB. The source payload still occupies one frame. No numeric total-memory cap or estate-scale throughput claim is made.

Xcode `.pbxproj`/workspace planning, full typed signatures/docs, complete macro-expanded document mapping, external generated outputs and custom macro-tool preparation remain gaps. Call/impact extraction has the partial source checkpoint above, not complete language coverage. Removing the last compilation in a Swift-only workspace also hits the shared writer's existing empty-publication limitation: prior data stays stale instead of becoming a current empty graph. Mixed-workspace group deletion is covered.

## Verification and real repositories

```sh
bash providers/swift/build.sh debug
python3 tests/swift_provider.py
python3 tests/swiftpm_provider.py
# Main build/test scripts also exercise the shared core/native paths.
```

The helper builds under `.artifacts/build/{debug,release}/swift/`; the host passes its compiled profile. [Standalone results](../.artifacts/swift-provider/latest.json) cover compiler bindings, overloads, locals, exact Unicode/BOM/CRLF/punctuation ranges, scoped updates, partial repair, and source preservation. [SwiftPM results](../.artifacts/swiftpm-provider/latest.json) cover three targets, local/system dependencies, generated test source, versioned-manifest ownership, saved exported-API invalidation, dependency checkpoint reuse, partial repair, and an external-plugin negative marker. No tests or model workloads in the indexed packages are executed.

The user supplied atelier-app, atelier-core, and aether-kit for read-only evaluation. [Snapshot provenance](../.artifacts/swift-real/1793998371555065000/summary.json) preserves selected source/configuration files and unchanged relative dependency expressions. Source/config SHA-256, size, mode, ownership, inode, mtime, and ctime were recorded before/after; no tooling was run against originals. Access times are excluded because reading can update them. Snapshots omit `.git`, `.build*`, DerivedData, `.worktrees`, dependencies/build caches, and symlinks. Missing runtime resources/local packages in these selective snapshots must not be attributed to original repositories.

The initial discovery-only assessment, before SwiftPM implementation, reported all eleven explicit contexts as partial with zero graph facts: atelier-app 961 Swift files/7 contexts, atelier-core 267/3, aether-kit 173/1. That is historical evidence of the former planning gap, not the current provider's SwiftPM capability.

Subsequent actual SwiftPM indexing of atelier-core's root package produced twelve target contexts and 239 artifacts (including generated test entries), with about 2.50 MB of source, 53,693 symbols and 137,587 occurrences. It initially flagged 1,767 omitted punctuation anchors as partial. Six retained compiler stores isolated 942 such cases; exact punctuation-token support reduced that replay to zero omissions without dropping or rebinding compiler records. [The initial real run](../.artifacts/swiftpm-real/1794000497348348000/response.json) streamed 85.5 MB of worker frames through a bounded test sink. After the fix, the same package produced **12 complete / 0 partial** reports and 139,354 occurrences. [Exact closeout evidence](../.artifacts/swiftpm-real/1794001089425651000/validation.json) verifies identical symbol identities/signatures/source payloads, preservation of every earlier occurrence, and exactly 1,767 additional compiler-anchored punctuation/operator spans. All 235 saved source artifacts actually analyzed from the root package still match their originals. The final broader audit observed later changes in the three live repositories (11 selected aether-kit files, two optional atelier-core integration files, and nine atelier-app files). Read-only Git HEAD/status/diff evidence and field-level hash/metadata differences are retained; no writer is attributed and no original was reverted or rewritten. The initially verified snapshots remained frozen, so this does not change the root-package graph comparison or imply that the broader originals stayed unchanged throughout the work. These are root-package results; optional nested integration packages and the other supplied repositories are not being claimed as fully indexed.

## Primary sources

- [Swift IndexStore units/records/USRs](https://github.com/swiftlang/indexstore-db/blob/main/Sources/IndexStore/Index%20Store.md).
- [Public IndexStore C API and version policy](https://github.com/swiftlang/llvm-project/blob/next/clang/include/indexstore/indexstore.h); verified header retained in `.artifacts/swift-probe/indexstore.h`.
- [Official SwiftPM build server](https://github.com/swiftlang/swift-package-manager/blob/main/Sources/SwiftPMBuildServer/SwiftPMBuildServer.swift) and [its compiler-option/preparation tests](https://github.com/swiftlang/swift-package-manager/blob/main/Tests/SwiftPMBuildServerTests/BuildServerTests.swift).
- [Swiftly toolchain/version-file documentation](https://docs.swift.org/latest/documentation/swiftlydocs/use-toolchains/); Know Code reads recognized pins without asking a version manager to install or switch tools.
- [BSP prepare contract](https://github.com/swiftlang/swift-tools-protocols/blob/main/Sources/BuildServerProtocol/Messages/BuildTargetPrepareRequest.swift) and [per-file SourceKit options](https://github.com/swiftlang/swift-tools-protocols/blob/main/Sources/BuildServerProtocol/Messages/TextDocumentSourceKitOptionsRequest.swift).

The compiler behaviors and counts above are measured development evidence, not release performance/compatibility guarantees.
