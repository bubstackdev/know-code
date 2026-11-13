# Xcode bridge adoption decision

Approved by the user on 2026-11-13: adopt a pinned,
separately installed, Xcode-only bridge; SwiftPM and the Rust common core do not
gain a required runtime dependency. This is a dependency decision, not release
certification or permission to publish/install anything.

## Concrete candidate and cost

The research candidate is `sourcekit-xcode-bsp` at
`726ce706cf18ffb7ed402bb44b2e47c0960076eb`, with the reviewed Know Code guard and
context patches, CLI identity `0.1.0-know-code-index-only.6`. Its SwiftBuild pin
is `7f96ee0e8aa8a35fe84f3e80932eb9e456356d10`; all eight dependency revisions
were verified unchanged before/after the build. Exact source, dependency and
binary evidence is under `.artifacts/implementation-0.0.3/` in
`xcode-wired-build-05/result.json` and `xcode-runtime-wired-08.json`.

The staged runtime contains 185 files and 94,834,623 logical bytes (90.44 MiB),
excluding user Xcode, caches and indexes. Seven sourcekit bridge source files
differ from the pinned reference, approximately 625 added and 135 removed
lines. Adoption therefore means maintaining a small pinned patch set, not
assuming the unmodified upstream executable provides these guarantees.
No second graph, storage or query engine is introduced.

The bridge runs during Xcode analysis, not as an additional permanent service.
The owned package fixtures used under 0.5 GiB sampled aggregate RSS; protected
Atelier preparation used about 0.70 GiB sampled RSS. These are finite samples,
not true peaks, minimum hardware requirements or throughput guarantees. The
real checks reused the existing core governor with two CPU cores and 2 GiB
sampled memory enforcement.

## Evidence that now passes

- Exact saved scheme/Test configuration, same-PIF service loading, distinct
  device/simulator destinations and mandatory global/per-target script guards.
- Real package missing/restored-cache boundaries and typed module readiness on
  both platforms, with analysis network/source protections unchanged.
- Protected Atelier preparation: 10 targets, 316 compiler-returned source
  entries, installed-Xcode explicit plugin origins and unchanged copied/baseline
  bytes, modes, mtimes and locks. The extra source beyond the preflight is
  compiler-generated asset-symbol source.
- One existing Atelier primary, `Canonical.swift`, compiled with each SDK's
  actual options and ingested through the existing Swift provider and common
  core publication path. Stored `CanonicalJSON.encode(_:) → encoder()` calls,
  callers/callees/impact, references and exact source match CLI/Node/.NET across
  42 comparisons. Public IDs were returned by the query operators, not inferred
  from their contents. Capabilities remain partial and unmanaged research
  freshness remains explicitly unverified.

Receipts: [protected project checks](XCODE_ATELIER_QUALIFICATION.md),
[package-cache checks](XCODE_PACKAGE_CACHE_QUALIFICATION.md), and
`.artifacts/implementation-0.0.3/xcode-real-stored-query-01/result.json`.
The user separately approved trust for the two isolated research copies; no
live repository or global execution grant was changed.

## Remaining integration work

Integrate a narrow adapter through the current Swift worker, shared preparation,
settings, source publication and scheduler. Detect a missing/incompatible
external bridge with an actionable error. Preserve SDK/context identities,
generated-source provenance, partial diagnostics and full-body normalization;
do not copy research orchestration into a parallel production engine.

Then validate whole selected-target indexing, saved changes, failures,
cancellation, bounded resource cleanup, native/CLI/MCP workflows and installed
packaging. Only macOS 27.2/Xcode 27.2 was exercised for this bridge; older
supported platform/toolchain combinations still need qualification or an
explicit capability boundary. The selective snapshot's omitted resources are
not certified by module readiness. Broader Swift dispatch, project-body links,
macro-origin coverage and performance remain explicitly incomplete.

The alternative is further planner research while retaining the Xcode release
gate. The installed indexing-settings CLI already failed reliable SDK-context
selection; owned-fixture success alone must not silently replace the accepted
device/simulator requirement.

## Packaging audit proposal — 2026-11-13

This source audit proposes the remaining packaging work; it does **not**
implement, qualify or select an archive/DMG layout or signing identifiers.
The current pinned recipe is [index-only.7-notice1](../tooling/xcode-bridge/recipe.json);
the earlier `.6` adoption evidence above retains its historical boundary.

- Extend [the existing signing tool](../scripts/sign_packages.py) with a
  separate bridge operation, preserving its exact three-archive common-runtime,
  npm and NuGet contract. The [bridge builder](../tooling/xcode-bridge/build.py)
  stages two native executables, `sourcekit-xcode-bsp` and
  `SWBBuildServiceBundle`; both would require Developer ID hardened-runtime
  signatures and timestamps.
- Validate an exact two-entry native-payload signing transform. Regenerate
  `know-code-xcode-bridge.json` inventory hashes/sizes from the **signed bytes**
  while retaining the unsigned input inventory, recipe/source/dependency pins
  and signing provenance. Preserve every other payload/resource, mode and
  license. [Provider discovery](../providers/swift/xcode.mjs) verifies those
  exact executable hashes; signing must not weaken that contract. The builder's
  `staged_build_only` record is tool-build evidence, not the common runtime's
  producer schema or release certification.
- Extend the existing DMG/index/notarization and
  [installed-package workflow](../tests/package_install.py) for optional tool
  assets and actual installed device/simulator cases, following
  [distribution provenance](PACKAGING.md) and the
  [notarization operator workflow](reviews/NOTARIZATION_OPERATOR.md).
  Whole-tool resources must work outside the checkout under normal security;
  installing Xcode is not part of this proposal or current tooling.

Archive/DMG layout, signing identifiers, final signed-tool installation and
platform qualification remain separate decisions/checkpoints. No new product
code, build, signing, installation or runtime check accompanies this section.
