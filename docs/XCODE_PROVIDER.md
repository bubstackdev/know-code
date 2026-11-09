# Xcode provider investigation and guarded prototype

Status: experimental v0.0.3 work. SwiftPM remains supported by the earlier
adapter. **Xcode's required device-plus-simulator project support is not
complete.** The installed command-line indexing-settings interface is not a
reliable context selector; this is a measured blocker, not a missing compiler.

## Smallest integration and its limit

The rejected experiment used the installed, documented
`xcodebuild -showBuildSettingsForIndex -json` operation. It returns actual
per-file `swiftASTCommandArguments`, module names, toolchain identifiers and
source membership. Those arguments can feed the existing Swift compiler and
IndexStore reader; no second fact store, graph/query engine, transport, or
build-system dependency is needed for that ingestion path.

The archived narrow prototype normalizes a project's internal workspace to its
`project.pbxproj`, reads its sole saved shared scheme, and selects native app
and test targets by saved BlueprintIdentifier. It does not establish a general
one-scheme/all-schemes policy. Multiple shared schemes and outer/cross-project
workspaces produce explicit gaps. Project and scheme files are inputs to the
shared saved-file invalidation path. Project group paths supply additional
xcconfig/entitlement inputs, and direct local package manifests are recorded.
Complete transitive Xcode dependency-input discovery is not implemented.

Target-specific queries include UI-test targets that the supplied scheme's
ordinary build-target query omits. Device and simulator contexts have separate
identities/provenance; source payload IDs retain the shared filename/content
identity. The selected compiler, IndexStore library, SDK, architecture and
actual returned target triple are checked before compiler facts are accepted.
Returned simulator arguments must never be relabeled as device arguments.

An initial owned-fixture run produced four complete contexts (app and UI tests,
each for device and simulator), using actual compiler arguments. A saved-source
repeat exposed the blocker below. The successful first run is not acceptance
of reliable Xcode support.

The current production module is a small explicit-gap reporter. It deduplicates
the internal project workspace, records saved scheme inputs, emits one partial
compilation with `xcode_reliable_planner_required`, and runs no planner/compiler,
project build, script, restoration, or source inference. The rejected adapter
and its original full-support test are retained with source hashes in
[the evidence manifest](../.artifacts/xcode-owned/rejected-adapter/manifest.json).
[The current passing worker regression](../.artifacts/xcode-provider/latest.json)
checks that boundary and stable compilation identity after a saved scheme change;
it is not a claim of Swift semantic support for Xcode projects.

The same regression now exercises the real coordinator: a saved scheme and a newly created, changed and removed `.xcconfig` each publish another coherent partial generation. No source facts or build/script execution are invented to make these updates look complete.

## Reproducible wrong-context behavior

On installed Xcode 27.2 / Apple Swift 6.4.2, exactly the same settings-only
command can return different contexts for the same saved file. Six identical
queries requested all of:

- `-sdk iphoneos` and `-destination generic/platform=iOS`;
- `-arch arm64`, `SDKROOT=iphoneos`, `SUPPORTED_PLATFORMS=iphoneos`,
  `ONLY_ACTIVE_ARCH=YES`;
- target/platform-specific products, intermediates, module and index paths.

They returned one arm64 device context, three x86_64 simulator contexts, and
two arm64 simulator contexts. There was no source/configuration change between
the requests. `-showBuildSettingsForIndex` produces only one argument array per
source in its JSON; the missing contexts are not duplicate JSON keys that our
parser accidentally discarded. Some returned output paths also ignore the
CLI output-directory overrides and refer to Xcode's own DerivedData locations.

[Exact command and six outcomes](../.artifacts/xcode-owned/repeat-device-summary.json),
[first device response](../.artifacts/xcode-owned/repeat-device-0.json),
[subsequent simulator response](../.artifacts/xcode-owned/repeat-device-1.json).
Fresh output paths and explicit generic destinations do not establish a stable
selection mechanism. Repeated random retries are not an accepted solution.

The official Swift Build source helps explain why the distinction matters:
`SWBCore/BuildRequestContext.swift` selects a preferred indexing configured
target, and its nil-run-destination path prefers simulator SDKs. The public
configured-target/indexing APIs retain this context explicitly. This source
does not prove the closed Xcode CLI's exact implementation, but the observed
CLI output cannot safely satisfy the requested context contract.

## Execution boundary

The owned fixture has both a shell build phase that would create
`PHASE_EXECUTED` and a scheme pre-action that would create `SCHEME_EXECUTED`.
Settings-only queries and direct per-file compiler indexing left both markers
absent. No object files, app products, tests, script phases or external
generators were executed. The prototype strips compiler output/cache options
and the unavailable SDK-stat-cache optimization from returned indexing
arguments, adds private IndexStore/module-cache destinations, and processes
one compiler invocation at a time. It preserves semantic flags and validates
the actual compiler context.

Missing generated Swift documents can be omitted from a direct type-check only
with an explicit partial-coverage diagnostic. Missing package modules or
compiler errors also remain partial. This is not automatic dependency/module
preparation and does not satisfy full Xcode project coverage. There is no
application build fallback, handwritten flag inference or target-triple edit.

An owned-fixture probe combining `-showBuildSettingsForIndex` with
`build-for-testing` also left its markers absent. Automatic approval review
rejected applying that invocation to the snapshot because the appended action
could perform a full project build. It was not retried. The safer target-only
settings query subsequently supplied UI-test metadata without a build action.
That permitted path still has the independent context-selection blocker.

## Supplied repository evaluation

All project tooling used a new durable copy under
`.artifacts/xcode-real/snapshot/`, derived from the previously preserved Swift
corpus snapshot. No Xcode tool ran against the user's original Atelier or
Aether repositories. Live originals can change concurrently; no broad claim
that they stayed unchanged is made.

The copied AtelierLibraryPhone project supplies 24 app Swift files and one
UI-test Swift file. Settings-only queries successfully recovered both target
descriptions. The first full guarded prototype attempt rejected incompatible
app device metadata before emitting facts. Its source/configuration inventory
remained byte- and metadata-identical before/after:
[snapshot run evidence](../.artifacts/xcode-real/1794010339823707000/summary.json).
This is not a successful dual-context graph or dependency-preparation claim.

## Next viable planner boundary

The public Swift Build session API offers configured targets and
`generateIndexingFileSettings`; its BSP server exposes those configured
contexts through build-target identifiers. A bridge must select the actual
platform/architecture context rather than collapse by source path. This fits
the existing shared JSON-RPC client and Swift IndexStore path.

The installed SwiftBuild framework has no public Swift module/interface; its
exported Swift module is named `XCBuild`. Treating its private binary as an
importable public SDK is not a supported tiny-helper strategy. `swbuild`
provides description creation and index preparation commands but no public
per-file settings command. Its serialized task-store is not an adopted API.

The inspected Apache-2.0 `sourcekit-xcode-bsp` bridge at
`726ce706cf18ffb7ed402bb44b2e47c0960076eb` uses the official SwiftBuild BSP. It
requires a separately built `SWBBuildServiceBundle` alongside the executable.
It currently enrolls all workspace targets, has no saved-scheme selection
option, pins SwiftBuild to `7f96ee0e8aa8a35fe84f3e80932eb9e456356d10`, and
tracks the swift-tools-protocols main branch. Its normal prepare endpoint can
execute indexing preparation tasks; script/codegen exclusions must be proven
before invoking it on repository projects.

A research-only build was started, then stopped when the installed metadata
path appeared sufficient. Its partial build/checkouts occupied about 500 MiB;
this is development footprint, not a measured runtime/distribution size. The
bridge was not installed, bundled, or added to Know Code dependencies. No
planner or preparation request was sent through it to a user project.

Recommendation for the next experiment: make a pinned BSP bridge an optional
separate prerequisite, detect its executable/service pair and compatibility,
and prove explicit configured-target selection before considering distribution.
Keep the existing SwiftPM/standalone path independent. Do not adopt this bridge
as a dependency merely because its architecture fits: the checked revision has
not passed the fixture, module-only preparation or real-corpus gates. Bundled
binary size, idle/running memory, startup time and supported Xcode range are
unmeasured; 500 MiB of partial build state must not be presented as release size.

The next architecture decision is a pinned optional external BSP prerequisite
versus packaging a compatible bridge/service with Know Code. The former keeps
the base distribution light but adds setup/version-management burden; the
latter avoids separate user installation but adds a substantial tool/service
and release compatibility surface. Either choice still needs scheme filtering,
actual device/simulator target selection and bounded preparation tests. Neither
requires another graph/storage engine.

Primary sources: [Xcode BSP bridge](https://github.com/slime-studio/sourcekit-xcode-bsp),
[official Swift Build session API](https://github.com/swiftlang/swift-build/blob/main/Sources/SwiftBuild/SWBBuildServiceSession.swift),
[official BSP implementation](https://github.com/swiftlang/swift-build/blob/main/Sources/SwiftBuild/SWBBuildServer.swift),
[preferred indexing context selection](https://github.com/swiftlang/swift-build/blob/main/Sources/SWBCore/BuildRequestContext.swift).

## Approved optional BSP experiment

The user subsequently approved testing an optional bridge. This section
supersedes the earlier statement that the research build was stopped and no
BSP experiment had run. Production remains the explicit-gap reporter; no bridge
has been installed into a user directory, added as a product dependency, or
wired into the worker.

The existing exact source graph was frozen in
[source-graph.json](../.artifacts/xcode-bsp/source-graph.json) and
[Package.resolved](../.artifacts/xcode-bsp/Package.resolved). Checkout HEADs and
manifest SHA-256 values were verified. The build used
`--force-resolved-versions --skip-update --product sourcekit-xcode-bsp --jobs 2`;
it did not follow the dependencies' declared `main` branches or update the
lockfile. The bridge's eight resolved dependencies all remained on their
recorded commits. Top-level licenses are Apache-2.0, with Swift's runtime
library exception on the Swift dependencies. Original license texts are
included in the staged research distribution. This is not a complete release
license audit of every vendored source component.

The build succeeded with the installed Xcode toolchain. It resumed previous
partial compilation and took 163.44 seconds; that is **not a clean build
benchmark**. Nothing installed or switched a compiler. The unmodified staged
research distribution contains the bridge executable, matching
`SWBBuildServiceBundle`, eight resource bundles and license texts. It excludes
objects, Swift modules, dSYMs, source checkouts and build caches.

| Measured item | Result | Scope |
| --- | --- | --- |
| Staged runtime logical bytes | 94,533,215 bytes, about 90.15 MiB | Two release executables, resources and licenses; no stripping experiment |
| Staged runtime disk allocation | 92,724 KiB | `du -sk`; filesystem dependent |
| Build/checkouts disk allocation | 1,176,364 KiB | Separate development footprint after the resumed build |
| First measured graph initialization | 2.83 seconds | Initial owned device fixture; not cold OS/filesystem caches |
| Later graph-ready startup | About 0.59–0.84 seconds | New bridge/service sessions with reused planner state |
| Ordinary bridge plus service RSS | About 64.6–69.0 MiB | Sum of macOS RSS snapshots, not unique physical memory or a peak |
| RSS after owned indexing preparation | About 82.9–86.8 MiB | Bridge plus service; compiler process peaks not measured |

[Runtime hashes and inventory](../.artifacts/xcode-bsp/runtime-manifest.json)
record the actual files. Both executables link system libraries/frameworks;
the bridge's build-cache SQLite is separate from Know Code's serving index and
does not change the Rust core's pinned SQLite policy. Compiler/preparation peak
memory, compression/distribution size, large-project latency, minimum supported
Xcode versions and long-lived idle behavior remain unmeasured.

### Configured contexts and saved-source proof

The staged bridge/service were launched from separate centrally located
configuration directories with explicit `platform: iphoneos` and
`platform: iphonesimulator`. Every request used the existing shared JSON-RPC
client. Three repeated reads of targets, sources and per-file compiler options
per session remained identical, with arm64 device or simulator triples matching
the requested context. Both native fixture targets were present. New sessions
after a saved source rename preserved the compilation selection.

One important identity limitation remains: **raw BSP target URIs are identical
across the separate device and simulator sessions**. They distinguish app from
UI tests within a session, but are not globally unique compilation identities.
Know Code must namespace them by explicit planner context (platform, SDK,
toolchain, configuration and project/scheme ownership); it must never merge
device/simulator facts merely because the BSP URI matches.

[Metadata-only evidence](../.artifacts/xcode-bsp/probe-1794011016657/summary.json)
contains four session results and ordinary process measurements. This probe
sent no preparation requests. The first direct compiler attempt correctly
failed because the official `index-overlay.yaml` had not yet been generated;
the overlay was neither fabricated nor removed from semantic options.

### Owned-fixture indexing preparation

After the metadata proof, the parent authorized official preparation on the
owned fixture only. Its project settings explicitly set
`INDEX_DISABLE_SCRIPT_EXECUTION=YES` and `INDEX_FORCE_SCRIPT_EXECUTION=NO`. The
negative shell phase declared an output under `DERIVED_FILE_DIR` and would
write both a marker and `NeverGenerated.swift` if executed. This avoids relying
only on the build engine's heuristic that scripts without outputs are skipped.
The official SwiftBuild source's `ShellScriptTaskProducer` checks these index
settings. The scheme pre-action retained its independent execution marker.

Four official BSP preparation requests (device/simulator, before/after save)
created the real overlay, header maps and compiler file lists. They created
empty `.app`/`.xctest` directories but no product files, object files or external
generated source. Both execution markers remained absent. No tests ran. The
fixture has no intertarget module imports, so this does **not** prove safe
preparation of a dependency closure or a custom macro-tool target.

Direct compiler IndexStore extraction then succeeded in all eight target
analyses: app and UI tests, device and simulator, before and after the save.
The app's actual compiler declarations contained `deviceOnly` only for device
and `simulatorOnly` only for simulator. The saved rename changed the compiler
declaration from `message(_:)` to `savedMessage(_:)`. No target triple, SDK,
overlay or source binding was guessed.

[Preparation and compiler proof](../.artifacts/xcode-bsp/probe-1794011081661/summary.json)
and [validation manifest](../.artifacts/xcode-bsp/validation.json) retain the
commands, full BSP responses, compiler diagnostics and IndexStore records. All
bridge/service processes exited after the probes. Original user repositories
were not accessed by these tools.

The optional bridge is now a promising demonstrated mechanism, not production
Xcode support. Before arbitrary-project preparation, the bridge must enforce
script-disable overrides itself rather than depend on cooperative project
settings. Scheme filtering, selected build configuration, project-package
restoration/ownership, generated-input gaps, shared resource caps and real
snapshot dependency preparation remain open. Keep it optional while these
boundaries are implemented and measured.

Minimum next gates, in order: enforce mandatory script-disable overrides in
the bridge request (including target-specific parameters); prove an owned
app/test fixture with an imported dependency module can prepare that module
without object/product/codegen execution; then evaluate the preserved real
project snapshots with dependency and generated-input coverage reported. The
current turn stops before those gates and does not adopt the bridge.
