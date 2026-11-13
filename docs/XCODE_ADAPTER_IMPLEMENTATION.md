# Xcode adapter integration checkpoint

Source checkpoint for the user-approved separate SwiftBuild bridge, 2026-11-13.
This is an unshipped adapter draft, not a validated production/release claim.
The integrator owns native/compiler execution, shared protocol/core changes and
all runtime evidence. The earlier real-project probes do not certify this code.

## Small implementation surface

`providers/swift/xcode.mjs` now holds prerequisite discovery, workspace selection,
ordinary dependency preparation and the Xcode BSP session. The existing worker
routes into it, using policy-derived central state. The shared JSON-RPC client,
compiler normalizer, exact saved-source reader, native IndexStore reader and
relationship ingester remain the implementation path. A small `compileSources`
export in `index.mjs` factors the existing SwiftPM primary/diagnostic loop so
Xcode and SwiftPM use the same full-body action and failure handling. Storage,
queries, target budgets, publication and scheduling stay in the common core.

The root-owned policy contract is Swift-only analysis policy/acknowledgment2,
with existing `scheme_path`, identity-bound `execution_allowed` and central
`cache_directory`. Rejected/missing trust, malformed cache or old managed policy
fails before tooling. Absent-policy SwiftPM/inferred operation retains its old
low-level behavior; absent-policy Xcode remains an explicit gap. Every Swift
healthy/gap/inferred row stamps `swiftExtractionPolicy:1` for common backfill.

Selection uses all configured Xcode containers even for incremental subsets:
one shared scheme for the repository/worktree, automatic only when globally
unique. Internal project workspaces and scheme aliases deduplicate. A sole
outer workspace can receive an explicitly resolved nested scheme; the bridge's
existing Foundation/PIF selector must prove membership and saved Test
configuration. Ambiguous outer associations remain explicit gaps. Unselected
configured containers emit coverage rows and do not expand another scheme.
All emitted units keep the canonical selected project path for atomic scope
replacement. SwiftPM still follows its independent package analysis path.

## Separate installed prerequisite

The adapter accepts recipe `index-only.6-notice1`, CLI identity
`0.1.0-know-code-index-only.6`, pinned sourcekit-xcode-bsp `726ce706…` and
SwiftBuild `7f96ee0e…`. It looks in the versioned user tool location or PATH,
follows the real executable, and requires its adjacent component manifest,
matching CLI/service inventory and exact CLI version. Verification is cached
within one worker rather than scanning all resource bundles for each source.
Missing/incompatible installs return actionable prerequisite diagnostics;
Know Code does not install the tool or add SwiftBuild to its Rust/native SDKs.

The separately versioned `tooling/xcode-bridge` recipe retains upstream source,
reviewable patches/modification notices, all eight dependency revisions,
Package.resolved and upstream license notices. Its flat runtime layout keeps
the bridge/service/resource bundles together. Component packaging/signing and
full inventory checks belong to that recipe, not the embedded graph library.

## Contexts and publication

Device and simulator run sequentially with the selected SDK/toolchain and arm64
triple validated against compiler-returned options. The accepted Test action
configuration comes from the bridge's existing saved-scheme parser, including
its pre-metadata request when dependencies are absent; no second XML parser or
build planner is introduced. Normal package restoration uses this exact context
and the shared preparation checkpoint. Source-only edits reuse valid setup;
module readiness still refreshes through the compiler for each analysis job.
Failed dependency setup waits for changed inputs or explicit retry.

The opaque BSP URI is retained whole and hashed together with project/scheme,
configuration, platform and effective compiler/SDK selection. It is never
parsed for target IDs or joined by names. Pinned SwiftBuild's ConfiguredTarget
computeGUID uses target identity and SDK/run-destination specialization, not
job output paths. A same-project fresh-job/save stability gate is still required.
PIF/source/options fingerprints remain separate evidence. Project body links
remain unavailable; no cross-context path composition is fabricated.

Compiler-selected local/linked source keeps exact bytes and boundary evidence;
third-party checkout source stays outside owned body indexing. Generated files
use the existing `SourceArtifact.generated` flag and compiler target/source
provenance. Work/module/generated outputs stay out of saved watcher inputs to
avoid self-triggering loops; their indexed text stays in SQLite. Targets release
compiler/source residency sequentially. The common governor and update path
retain shared caps, cancellation and one coherent publication across contexts.

Typed preparation success is necessary but does not establish body semantics.
The shared normalizer removes the three qualified indexing shortcuts for body
extraction; actual compiler diagnostics can still publish partial facts. Reader,
compiler-driver or transport failure must not be relabeled successful analysis.
Healthy targets may accompany explicit unavailable context/target rows, while
resource interruption aborts the common publication.

Package-policy inspection now follows exact PIF metadata and precedes public
module preparation. It reuses SwiftPM's existing host manifest evaluator and
conservative plugin/custom-macro classifier; no target module configuration is
substituted. Only the compiler-selected Package.swift projects and their
versioned manifest variants are inspected. Successful/unsupported classifications are cached by saved manifest
hashes and effective toolchain in the existing dependency plan, so body-only
saves do not repeat manifest compilation/restoration. Unsupported external
codegen is an explicit coverage gap with retained repair inputs. Metadata safety
still depends on the separate pinned-tool hostile-plugin gate; PBX script guards
alone are not a package-plugin claim.

## Gates before promoting this checkpoint

- Run the updated focused policy/ambiguity/prerequisite/invalidation fixture and
  Swift/SwiftPM regressions against newly matching artifacts/protocol.
- Qualify the assembled adapter with the installed-component layout, both SDKs,
  exact bytes/calls/source/native parity, source-save repair, generated origin,
  dependency checkpoint reuse and stable opaque identities across fresh jobs.
- Probe hostile SwiftPM build-tool plugins during both metadata and module
  preparation. PBX script guards do not establish package-plugin safety.
  Unsupported plugin/codegen forms must remain explicit gaps; do not add an
  invented package planner or silently claim those forms safe.
- Preserve partial graph/external/unsupported boundaries, deliberate missing
  resources, caps/timeouts/cleanup and failed receipts. The owned six gates and
  real316-source metadata/42 stored-native comparisons remain their original
  research evidence, not this adapter's acceptance results.

No live service, original repository, installed runtime or release is changed by
this source checkpoint. Only Node syntax, Python AST and diff checks were run
by its author; compiler/native checks belong to the integrator.
