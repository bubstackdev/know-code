# Atelier Xcode qualification plan

Read-only preflight, 2026-11-12. This is a plan and bounded source inventory,
not a successful real-corpus graph, dependency-preparation or adoption claim.
The integrator owns every compiler/planner/native execution. The frozen bridge
candidate, snapshot and original repositories have not been changed.

## Inputs and what inspection establishes

Use the preserved snapshot at
`.artifacts/swift-real/1793998371555065000/`, not the live repositories. The
selected project is
`atelier-app/iOS/LibraryPhone/AtelierLibraryPhone.xcodeproj`; its sole retained
shared scheme is `AtelierLibraryPhone`. The accepted policy uses **Test/Debug**
for app, dependencies and tests; Run/Profile are not additional contexts.

The native scheme identifies app `7E2D682768ED1D862A278004` and UI test
`193F239204C53D8B9845014D`. The project declares three local package paths:
`../..`, `../../Phone` and `../../Remote`, relative to `iOS/LibraryPhone`.
Their manifests find sibling `atelier-core` when present, falling back to
`core` only if the named directory is absent. Preserve both the directory
layout and the manifest expressions in any derived copy.

| Inspection | Finding | Qualification implication |
| --- | --- | --- |
| Native project/scheme | Two native targets; no script-phase/build-rule declarations, remote package references, scheme execution actions or test plans | The hostile owned fixture remains the script guard control; this real project is not another hostile-script test. |
| App, Phone, Remote and root Core manifests | Local packages only; Core has no package dependency list; no declared plugins, macro targets or unsafe flags in those four manifests | A local-only selected package closure is expected from source inspection. Require actual exported PIF/service closure evidence before treating it as established. |
| Selected products | App uses AtelierPrivacy, AtelierContent, AtelierPhoneKit and AtelierRemoteWire | Static manifest dependencies also suggest AtelierNaming, AtelierDomain, AtelierWorkflow and AtelierAuthoredScene. These names describe expected build selection, never graph/body identity. |
| Expected source directories | 315 Swift files / 3,415,229 bytes including 24 app files and one UI-test file | The actual compiler-owned target/source inventory is authoritative. Do not enroll all package tests or unrelated workspace targets just to match this count. |
| Macros | 33 files in the expected directories contain `@Observable`; no `#externalMacro` declaration found there | Retain actual compiler plugin paths and verify that these are the selected installed toolchain's Observation macros. Do not strip macro flags to manufacture a successful compile. |
| Other packages/locks | Native/Library/Aether and their remote locks, plus Lars's HOME/env-based library path, exist elsewhere in the snapshot | They are outside the statically expected iOS package closure. Do not restore, evaluate, enroll or use those locks merely because they occur in the repository. Actual compiler selection must confirm the boundary. |
| C/system integration | Core's CSQLite shim includes SDK `sqlite3.h`; its integration targets are outside the expected selected product closure | A compiler-reported additional dependency is evidence to investigate, not permission to guess a body link or substitute a host library. |

There is no `Package.resolved` in the selected Xcode workspace, app root, Phone,
Remote or Core root. Other preserved locks are version-3 remote pin files for
different package roots; they must not be borrowed or rewritten for this job.
No recognized compiler/version-manager pin file is retained in this selective
snapshot. That does not establish that today's originals are unpinned.

The snapshot is intentionally incomplete: `Info.plist`,
`iOS/AtelierPhone.entitlements`, Design/asset catalog inputs and many declared
SwiftPM resource folders were not copied. A failure caused by an omitted
snapshot input is a fixture limitation, not proof that the original project is
broken. Do not synthesize replacement resources or change manifests to erase
that limitation. A later selectively augmented derivative needs its own
provenance receipt; do not mutate this baseline or bulk-copy model resources.

The [source-only receipt](../.artifacts/implementation-0.0.3/xcode-real-preflight-01/preflight.json)
records exact input hashes, expected directories and all preserved file hashes.
All 972 app, 277 Core and 176 Aether retained files still match their original
snapshot inventories; no retained symlinks were found. Receipt SHA-256:
`9b2a08cba67242a787ab6cd0c911812b630ad9a4e315cd32a58026935cfd3f71`.
This check did not read/inventory today's live originals or execute manifests.

## Protect originals before running tools

Create a fresh durable run prefix and a derived paired `atelier-app` /
`atelier-core` copy beneath its `snapshot/` directory. Use that enclosing
directory as qualification workspace root so sibling source ownership remains
inside the owned corpus. Never add symlinks back to live repositories. Retain
before/after source/config inventories, including creation/removal of lock
files; separate owned caches and outputs from saved input changes.

Run the bridge, service, metadata export and compiler descendants under an
inherited macOS sandbox policy denying both reads and writes beneath:

- `/Users/zakkeown/Code/atelier/atelier-app`
- `/Users/zakkeown/Code/atelier/atelier-core`
- `/Users/zakkeown/Code/bubstackdev/aether/aether-kit`
- `/Users/zakkeown/Code/bubstackdev/lars-sdk`
- The frozen baseline `.artifacts/swift-real/1793998371555065000/`

Use exact real paths when constructing `(deny file-read* file-write* (subpath
...))` clauses. This is the existing policy shape in `tests/package_install.py`,
not a new validator. Prove enforcement first with an **owned** denied sentinel
and an allowed control; an ineffective profile must never be tested by trying
to create or change a live-original file. A denied read of an existing original
file is a non-mutating specific-path control. Retain the policy hash and exact
outer command, and verify that it covers descendants rather than only a helper.

Preserve real `HOME`/`CODEX_HOME`. Put `TMPDIR`, job state, modules, products,
IndexStore and any verified package caches under the owned prefix. Keep the
selected coherent Xcode/toolchain environment explicit. An out-of-snapshot
compiler input must be an identified installed toolchain/SDK input or an honest
unavailable dependency; never permit a hidden fallback into a live repository.
Do not change global developer selection, login jobs or installed Know Code.

## Run the finite gates in order

1. Finish the frozen wired bridge's matching build, same-PIF/selected-root
   success fixture and typed failing-module fixture. The failing dependency
   contains a warning saying `Build Complete`; typed failed state must still
   reject preparation. Preserve all failed attempts and dependency identities.
2. In the next source revision, close metadata subprocess stdin explicitly with
   `FileHandle.nullDevice`; it must not inherit BSP protocol input or prompt.
   The installed CLI help confirms package resolution/update/cache flags, but
   those flags alone do not prove offline behavior. First run small owned
   **absent and restored package** probes with network denied and compare exact
   lock bytes. Reuse the existing positive/negative socket policy controls;
   allow only necessary owned Unix IPC paths, without relaxing remote denial.
   An absent package must be an explicit setup gap, never a successful cached
   preparation. No implicit per-save fetch/lock mutation is acceptable.
3. Export metadata from the fresh protected Atelier derivative with network
   denied. Retain the exact PIF bytes/hash, original metadata arguments and
   saved scheme/PBX hashes. Verify the sole selected shared scheme, Test/Debug,
   app/UI-test roots and actual planner dependency closure. Stop on ambiguity,
   an unsupported remote/tool dependency or a missing snapshot input; do not
   retry randomly or add a full build action.
4. Load that **same** retained PIF into the public service. Run device and
   simulator sequentially, retaining actual configured-target/source/options
   evidence and every global/per-target script guard. Confirm arm64 iOS versus
   arm64 iOS-simulator triples from returned arguments without editing triples.
   Prepare required modules through typed public build operations. Only typed
   terminal `ok` + preparation-complete + state `succeeded` may checkpoint.
5. If preparation qualifies, perform direct source indexing with existing
   semantic argument normalization and the matching IndexStore helper. Keep
   saved UTF-8 coordinates, per-target identities, coverage and inputs honest.
   Demonstrate one compiler-proven direct edge through stored callers/callees/
   impact and exact source reads in each platform context. Cross-module bodies,
   unsupported dispatch/initializers and source omissions stay explicitly
   partial; common imported names do not establish compatible body links.
6. Capture aggregate descendant resource samples and complete process cleanup,
   then verify unchanged baseline/derived saved inputs and lock-file absence or
   bytes. Module/interface/header-map/cache preparation is allowed; objects,
   app/test products, binaries, scheme/script/codegen execution are not a
   fallback. A partial snapshot result does not become a full-support receipt.

Bridge version/dependency adoption and production packaging stay undecided until
these concrete results are reviewed. Owned fixture success and sampled resource
observations do not by themselves establish real-corpus, installed/native/
CLI/MCP, cancellation, resource or release acceptance.
