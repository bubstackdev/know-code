# Runtime discovery and local candidate packages

Distribution uses a separate shared coordinator/worker runtime and thin npm/NuGet SDKs for macOS 15+ on Apple Silicon, Node 24 and a separately installed .NET 10 SDK. **Published v0.0.2** passed its artifact-bound signing/notarization, installed/offline and macOS 15 checks. **Working version 0.0.3 is an unreleased candidate**; final packages, installed/platform/upgrade checks and corpus qualification remain pending. See the [candidate audit](RELEASE_0_0_3_AUDIT.md) and [retained failures/current work](IMPLEMENTATION_0_0_3.md). Neither package versions nor historical release receipts certify the candidate.

The platform rationale is the [Node 24 build matrix](https://raw.githubusercontent.com/nodejs/node/v24.22.2/BUILDING.md) and [.NET 10 OS matrix](https://raw.githubusercontent.com/dotnet/core/main/release-notes/10.0/supported-os.md), checked 2026-11-09. The SDK targets .NET 10; repository pins and existing Roslyn compatibility checks still apply. Shipped 0.0.2 supports TS/C# and labels Swift/Rust experimental. The candidate declares all four providers required, with finite compiler/partial-graph boundaries in [COMPILER_ACCEPTANCE.md](COMPILER_ACCEPTANCE.md). Node/.NET remain shared prerequisites. Selected Swift/Xcode SDKs or Cargo/rustc/rust-analyzer/matching rust-src are needed for their respective source analysis. These tools are separately installed, never provisioned by ordinary indexing.

## Layout and discovery

A versioned runtime archive extracts to `know-code-<version>-darwin-arm64/` (working candidate version `0.0.3`):

```text
bin/know-code
lib/know-code/runtime.json
lib/know-code/providers/{typescript,shared,dotnet,rust,swift}/...
lib/know-code/node_modules/...     # pinned worker dependency closure
lib/know-code/proto/analysis.proto
lib/know-code/THIRD_PARTY/...
LICENSE
docs/...
```

Worker resources resolve from the running executable's real path to adjacent `../lib/know-code`, never from its launch directory or a compiled-in checkout path. `KNOW_CODE_RUNTIME_DIR` is an explicit resource-directory override. A damaged/mismatched bundle or explicit override fails actionably without falling back to development files. A `target/debug` or `target/release` executable can still recognize its adjacent development source tree; arbitrary copied executables cannot search for a checkout. Swift's packaged helper is adjacent to its adapter; its development build staging path is used only in development layout.

The manifest versions the resource layout and records worker protocol/profile, store and lifecycle capabilities, the directional read/migrate/rebuild catalog, target, minimum OS, prerequisites and required versus experimental providers. Runtime resource validation checks those declared contracts against its compiled catalog. `know-code --version` needs no resources. `know-code runtime-info` reports selected layout/directory, profile, protocol and actual bundled SQLite/FTS5 identity. Missing worker resources or incompatible manifests require a complete matching runtime installation. The accepted lazy index compatibility/rebuild policies remain separate from executable resource discovery.

SDKs keep in-process graph queries. The npm package contains its native Node-API add-on and declarations; the NuGet package places the native C ABI library under `runtimes/osx-arm64/native`. Neither copies coordinator/workers into each consumer. Put the runtime's `bin` directory on PATH, or use the existing coordinator-executable override. Native dylib identities are relative `@rpath` names; SQLite remains bundled in each native artifact using the same pinned build policy.

## Separate Xcode-only download

The accepted candidate distribution adds an optional separate bridge download
and separate bridge DMG; Xcode indexing requires that pinned tool, while SwiftPM
and queries do not. It is not folded into the runtime/npm/NuGet archive set.
The [tool recipe and layout](../tooling/xcode-bridge/README.md) and
[adoption decision](XCODE_BRIDGE_DECISION.md) describe its executable/service,
resources, licenses and user-scoped discovery. Missing or incompatible tooling
must produce an actionable prerequisite error, without a substitute planner.

Existing `scripts/sign_packages.py` now has separate `bridge-sign` and
`bridge-verify` operations: authenticate the producer and exact two native
signing transformations, then inventory the actual signed bytes. These source
operations are not a signed/notarized bridge release. Final tool build,
separate DMG/notarization, installed device/simulator and platform evidence are
still required; the common runtime's macOS 15 floor does not qualify arbitrary
Xcode versions.

## Build and review

```sh
bash scripts/build.sh
bash scripts/build.sh release
python3 scripts/package.py --profile release
```

Packaging consumes matching built artifacts and creates a runtime `.tar.gz`, npm `.tgz`, NuGet `.nupkg`, SHA-256 checksums, dependency/license evidence and a source/toolchain/artifact manifest under `.artifacts/packages/`. It does not publish, install Node/.NET, activate a login agent or replace the current development installation. Candidate filenames use the working package version; they are not proof of a tagged/released revision. Packaging now requires a completed profile-matched producer record from scripts/build.sh, verifies frozen build inputs/dependency/generated/output hashes before and after assembly, and includes the unchanged record in the runtime and candidate inventory. Package-only inputs have a separate frozen inventory. See BUILD_PROVENANCE.md; hashing current source beside old binaries is rejected. Runtime archive timestamps are normalized, but cross-machine bit-for-bit reproducibility is not claimed.

NuGet worker publication retains its framework-dependent runtime/dependency files and Roslyn build hosts, and omits debug PDBs. Worker JS dependency trees are copied from the pinned installed closure, with their notices, without development workspace symlinks or consumer install scripts. SDK packages include third-party evidence too. A final dependency/license/source provenance review remains a publication gate; this candidate inventory is not a substitute for that review.

Every public release requires exact source/producer/archive provenance, dependency/license closure, signing/notarization and normal-security installed/offline/platform checks. Distribution uses GitHub release assets, including npm/NuGet SDK archives; registry publication is separate. The development LaunchAgent remains separate. See [0.0.1-to-0.0.2 upgrade compatibility](INSTALLATION.md#upgrading-from-001-to-002). Dated checkpoints below retain their original evidence limits.

## Current verification and historical checkpoints

The candidate must run the expanded installed/offline SwiftPM/Cargo and selected
TS6/native-TS7 workflows, including representative Rust MCP queries, on matching
final artifacts. The installed Xcode mode reuses the owned device/simulator
fixture with genuine installed SDKs, checkout denial and authenticated separate
tool inventory. Test source or syntax checks do not certify those runs. Final
normal-security install, actual 0.0.2 upgrade/rollback/native compatibility and
declared-platform checks remain separate. See [validation selection](VALIDATION.md)
and the [release audit](RELEASE_0_0_3_AUDIT.md); the encoding performance gate
remains explicitly deferred.

The following retained packaging/installer/signing checkpoints describe earlier
artifacts and their original limitations. Later shipped 0.0.2 closeout is in
[RELEASE_PLAN.md](RELEASE_PLAN.md); none of these older states is a 0.0.3 pass.

The installed-package harness extracts into a durable directory outside the checkout, installs both SDK archives and restores fixture dependencies before registration, then denies networking and checkout access for fresh TS/C# indexing, saved updates, CLI/MCP and both in-process SDKs. Its controlled socket probes preserve required local coordinator/Roslyn IPC while proving IPv4/IPv6 TCP/UDP denial. See OFFLINE_VALIDATION.md for the exact boundary, evidence and historical-candidate limitations. Relocation and deliberately missing/mismatched resources must be explicit. The initial archive test passed with checkout access denied: actual npm/NuGet installs, TypeScript/Roslyn analysis (including generated source), matching CLI/native/MCP queries, saved updates, runtime relocation and explicit damaged-resource/version failures. The retained experimental Swift/Rust adapters passed the same outside-checkout environment, including the relocated Swift IndexStore helper. These remain experimental and are not new 0.0.1 acceptance requirements.

The test exposed a .NET test-isolation issue: an external consumer build left its shared compiler server under the deny-checkout sandbox; a subsequent normal regression build then failed with access denied. The test now disables shared C# compilation, MSBuild server use and node reuse. Its test-created server was identified by launch time matching the consumer build and stopped; verification resumed. Do not run sandboxed package builds through a reusable compiler/build server.

Packaging also uses a task-local npm cache and platform-filtered offline Cargo metadata. The former avoids unrelated global cache permissions; the latter avoids downloading dependencies for other platforms merely to collect metadata. Six Rust crates omit their repository-level license files; exact commit-pinned upstream evidence is retained in `third_party/licenses/`, checked against crate VCS metadata and hashes during packaging. Dependency metadata and provided notice evidence are not substitutes for final publication review.

Candidate sizes, checksums, build-host/toolchain identities and source hashes are recorded in the adjacent artifact manifest. Final validation is retained under `.artifacts/packaging/1794245705208771000/`.


## Final candidate closeout

Final artifacts are under `.artifacts/packaging/1794245705208771000/candidate-final/`: runtime 17,814,944 bytes, npm SDK 1,644,980 bytes, NuGet SDK 1,711,095 bytes. `SHA256SUMS` and `manifest.json` describe the exact archives, macOS 27.2/ARM64 build host, pinned dependencies/toolchains, native load checks and source hashes. All 135 dependency inventory entries have license metadata and retained notice evidence; final publication/source/license review remains open.

The exact final archives passed 18 installed-package checks in a fresh durable prefix outside the checkout, with checkout reads/writes denied, actual offline npm/NuGet installation and experimental adapter validation. Relocating the runtime preserved SDK reconnection without reinstalling either SDK. Missing resources, bad protocol manifests, invalid overrides and bare copied binaries fail explicitly. A normal checkout compilation also passed immediately afterward, confirming that the fixed test no longer poisons the shared compiler-server environment.

All required Debug components passed across the original prefix and the resumed checks after the test-isolation fix; Release workspace (20 cases), MCP (22 cases), 42 Rust tests, formatting and all-target Clippy passed. See `.artifacts/packaging/1794245705208771000/validation.json`, `.artifacts/packaging/verify-remaining.sh` and the durable external prefix `/Users/zakkeown/Code/.know-code-package-tests/1794245705208771000-final/`. These are local candidates with no external publication or new global login activation. At that earlier closeout, stable installer/update/removal behavior remained open; the installer integration below supersedes that lifecycle status. Older supported OS versions, signing/notarization and the real-encoding gate remain open.

## Installer integration

[User-scoped installation](INSTALLATION.md) adds a stable launcher, real managed login job, staged compatible updates, rollback, service enable/disable and data-preserving uninstall/reinstall. The isolated lifecycle test passed 15 checks, including a deliberately held compiler job: analysis waiting reached its 30-second limit, selection stayed unchanged and a subsequent saved change proved the scheduler resumed. The complete command took 34.03 seconds including pre-pause staging. Failed startup restored both login-managed and on-demand coordinators. The development job's PID was unchanged.

Evidence is retained under `.artifacts/installer/`; final candidate artifacts are in `candidate-retirement/`, with their exact sizes/checksums/provenance in `manifest.json`. Lifecycle test results live in durable external prefixes under `/Users/zakkeown/Code/.know-code-installer-tests/1794248916812241000-*`. The candidate also passes 17 installed-package checks for required TS/C#, actual native SDK installations and shared CLI/MCP queries with checkout access denied. Experimental providers remain covered by the previous 18-check packaging run and the required language regression suite; they are not new release requirements.

The installer deliberately refuses the existing development LaunchAgent rather than adopting it. No candidate has replaced the live development install. Incompatible index rebuild upgrades, automatic crash recovery of an interrupted whole install, old runtime garbage collection, signing/notarization and clean/older-OS verification remain open. Runtime version retention does not retain additional source history.

The final optimized Rust rerun also exposed a duplicated-descriptor freshness-lease retirement race. A retained-descriptor test reproduced it deterministically; the common core now invalidates its input gate and explicitly unlocks the lease. See [the fix and compatibility behavior](LIVE_FRESHNESS.md). The installer candidate was rebuilt with that correction before final verification.

Corrected-candidate closeout: `.artifacts/installer/validation.json` records successful completion of the full Debug suite after the retirement fix, 44 optimized Rust tests, optimized native live/Workspace/MCP checks, 15 isolated real-launchd lifecycle checks and 18 exact-archive checks including experimental providers. Formatting and all-target Clippy passed. The final required Debug run includes all four language adapters; the exact candidate/source hashes were rechecked. No live installation replacement or publication occurred.

## Signing access

The user confirms Apple Developer signing/notarization access is available for Know Code. Availability is not evidence that a matching certificate/private key or notarization credential is installed, or that an artifact has passed notarization. The user accepted Developer ID signing, Apple notarization and successful installation under normal macOS security checks as 0.0.1 release gates. Packaging/signing execution and verification remain unfinished; current retained candidates are unsigned, and their earlier tests do not certify a future signed artifact.
