# Build-to-package evidence

`bash scripts/build.sh [debug|release]` runs the existing npm/Cargo/Swift-helper/.NET build through one Python standard-library helper, `scripts/build_record.py`. It writes `.artifacts/build/<profile>/build-record.json` only after the whole build succeeds. An unfinished attempt remains explicit in `in-progress.json`; packaging refuses to reuse an older successful record while that marker exists. A per-checkout exclusive file lock excludes overlapping build/package writers. Known compiler/publish/pack commands inherit the descriptor, retaining the fence if their driver exits while they still run. Reusable build/compiler servers are disabled. This is not a boundary against arbitrary daemonizing tools. Separate worktrees retain separate targets, generated files and staging.

## Recorded producer inputs

The record contains a unique build ID, source revision when available, exact build-relevant file hashes, effective profile/options, compiler/tool/SDK identities and a finite allowlist of build flags. It does not dump the environment or inspect signing credentials. Build-relevant source includes the Rust/native/managed/provider/protocol inputs, manifests/locks, build/generation scripts and configuration. Documentation and package-only README/package-script changes have a separate packaging inventory and do not require compiling unchanged product code.

Sources and finite applicable Cargo/MSBuild configuration candidates are captured before restoration and checked again after restoration and compilation. Newly created configuration files also invalidate the attempt. Restoration prepares pinned npm, NuGet and Cargo dependencies before compiler work. The snapshot includes the actual installed runtime JS closure, including nested non-JS files; the reachable normal/build Cargo package source closure, including non-workspace local/path/patch packages with no registry source; and restored NuGet package bytes and assets/generated restore imports. Only workspace package source already covered by the source inventory is omitted from duplicate dependency hashing. Locks alone do not stand in for the consumed files.

Cargo's JSON build messages identify the actual native outputs and active core generator OUT_DIR. They must report a successful build of every required native target. The generated Rust protocol, C# protocol and managed generated sources are hashed separately. The record covers native binaries, Swift helper, Node staging, complete provider/SDK/smoke output trees and compiled managed intermediates. The staged Node add-on and .NET smoke native library must match the corresponding producer dylib hashes. An unrelated/stale native file left on disk is insufficient evidence.

Fixed build options remain macOS 15 deployment, profile-matched Cargo/Swift/.NET compilation, no reusable C# compiler/MSBuild servers, and locked dependency restoration. User-scoped toolchains stay external. The build fixes its Cargo target directory to the current checkout and rejects `CARGO_BUILD_TARGET` overrides that would silently move required native outputs into a different layout. This is local build evidence, not a cross-machine bit-for-bit reproducibility claim or a signed builder attestation.

## Packaging gate

`python3 scripts/package.py --profile debug|release` takes the exclusive producer lock and verifies the completed record before creating a staging tree. Publication/packing can mutate managed intermediates, so two packaging operations in one checkout may not overlap either. The standalone read-only `scripts/build_record.py verify <profile>` command uses a shared lock. Missing/incomplete/wrong-profile records, changed source/dependency/generated inputs, changed tools or changed/mixed outputs fail with a rebuild instruction. Verification repeats after assembly. Package-only inputs are also frozen and rechecked, with a packaging retry instruction rather than a needless product rebuild when they change.

The published .NET worker's actual files must match the frozen provider bin tree. `dotnet publish --no-build` can create an intermediate deps JSON; that packaging-generated intermediate is not confused with a compiled product output. Its published bytes still must match the producer's frozen runtime payload. npm and NuGet archives are inspected directly: the Node native add-on, NuGet native library and managed SDK assembly must match the corresponding producer hashes.

The output includes:

- `build-record.json`, copied unchanged from the completed build;
- a `build_provenance` reference/build ID/hash and `packaging_inputs` in `manifest.json`;
- checksums for all three archives, the build record and manifest;
- the same build record inside the runtime resources, keeping the separate runtime self-describing.

This metadata describes static build inputs/outputs. It does not add runtime usage collection. Final signing/notarization must preserve the chain from these unsigned producer bytes through any signing transformation to the exact distributed archive bytes; this slice does not sign, publish or certify a release candidate.

## Focused verification

Run `python3 tests/build_record.py`. Fixtures and results remain under `.artifacts/build-record-tests/`. The 18 cases include missing/partial/wrong-profile records; source/configuration changes during restoration/compilation or without a new commit; external Cargo path-source changes; nested dependency/configuration/generated-protocol changes; altered NuGet/native/worker bytes; interrupted producer attempts; mixed staging; missing Cargo producer evidence; output-to-package digest binding; build/package exclusion; and a real child retaining the fence after its driver exits. A docs-only change preserves build validity but changes packaging input evidence.

The real isolated build/package demonstration, exact source/build/archive identities and retained failures belong under `.artifacts/provenance/` in this worktree. These checks do not replace current-candidate installed/offline, optimized, performance, signing/platform or zero-telemetry acceptance. Root integration must rebuild after relevant product changes and rerun affected assembled-candidate gates.
