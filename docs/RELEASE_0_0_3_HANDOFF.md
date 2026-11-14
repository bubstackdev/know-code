# 0.0.3 candidate handoff

Prepared 2026-11-13. Required current-host execution checks are complete within
the scope below. The candidate is signed, notarized and unpublished. Publishing,
tagging and replacing the live development installation have not occurred.

## Included scope

- TypeScript 5.9.3/6.0.2 compiler-API support and exact native 7.0.2 integration;
  repository compiler selection is preserved. Future TS7 versions are not certified.
- SwiftPM and Xcode symbols, references, useful callers/callees/impact and saved
  source. One selected shared Xcode scheme uses its Test configuration, with
  dependencies/tests and distinct device/simulator contexts.
- Cargo-captured library/binary/test contexts and useful Rust direct-call and
  value/type dependency graphs, with exact context/source evidence.
- One common Rust storage/query/graph core through CLI/MCP and .NET/Node libraries.
  Unsupported Swift/Rust/TS7 forms remain explicitly partial or unavailable.

The Xcode bridge is a separate prerequisite/download, required only for Xcode.
It retains its independently pinned source/build provenance. Estate search,
strings/comments and Swift compiler batching are not included.

## Frozen artifact identities

Common producer source: `d106de54d200194ede2cf1beb601ed730404fea5`.
Build ID: `e0883d2733e14f009216f55dfe614fc6`.
Producer SHA-256: `1e4fb8f7fc7c25f21dfd03c10dfaae3cd465ee18053bdf14d084f8b45a8c3bdf`.
Signed manifest SHA-256: `ee6a35ee2670347450ec90f94674f1b0df39a805e577e69f8c544dee23a606de`.

Later integration commits change test readiness waits and documentation, not
compiled/package inputs. Do not stamp those newer revisions onto these binaries.
The bridge recipe is `index-only.7-notice1`, based on BSP
`726ce706cf18ffb7ed402bb44b2e47c0960076eb` and SwiftBuild
`7f96ee0e8aa8a35fe84f3e80932eb9e456356d10`. Its outer index's source `3ba4869`
is explicitly the earlier operator checkout context, not the common producer.

| Final image | SHA-256 | Apple Accepted submission |
| --- | --- | --- |
| Common | `8c90cb3b5f49bfc5356c12b5923cff8318f9cf69cfbe7cea8745b9c47307d646` | `beaf125e-c520-4714-94ac-c709d359fabb` |
| Xcode bridge | `1671c7e5239b1f3a3538caa0736f5c3d0ce1715c82c44eb058df8172358e6241` | `7b7101b4-0b2e-4f1d-a5e9-cdbcca3f3174` |

Both images passed actual ticket stapling/validation, post-staple mounted inventory
and signature checks, and final detached CMS verification. Earlier inner manifests
still truthfully record their pre-notarization stage; outer indexes bind the later
Apple logs and final bytes. The provisional source-3ba common image is superseded.

## Final execution evidence

Receipts are under `.artifacts/implementation-0.0.3/wave-01/`. Counts identify
finite checks, not exhaustive language/compiler parity.

| Gate | Actual evidence |
| --- | --- |
| Required core | All 27 families covered on the same producer across `candidate-release-core-04` (first 15), `candidate-release-mcp-05` (37 MCP cases), `candidate-release-core-suffix-05` (cursors: 17 cases), and `candidate-release-core-remainder-06` (remaining 10 families). Final producer verification passed. |
| Installed CLI/SDK/MCP/offline | `candidate-installed-offline-rust-04`: 36 checks, 68.24 s; TS5.9/6/7, C#, Rust, actual npm/NuGet install, graph/source parity, repair, relocation, network/checkout denial and package safety. |
| Installed Xcode | `candidate-installed-xcode-02`: 17 checks, 87.24 s; six app/dependency/test device/simulator contexts, both SDKs, saved source, errors/repair and cancellation using final common bytes and independent bridge. |
| Installed SwiftPM | `candidate-installed-swiftpm-host-03`: 13.52 s; fresh owned data, CLI/Node/.NET graph/snapshot/source parity, real save, compiler-error partial publication and repair to current generation 5. Default 2 CPU/2 GiB. |
| Real lifecycle | `candidate-installed-lifecycle-03`: 15 checks, 193.35 s; install/update, bounded busy deferral, compatible rollback, failed-start restore, login toggles, uninstall/reinstall; cleanup passed. |
| Published 0.0.2 upgrade | `candidate-published-settings-upgrade-02`: 5 checks, 220.45 s; authenticated public old bytes, settings/trust/index preservation, Rust backfill before any save, opposite SDK version guards, refusal of incompatible old rollback; cleanup passed. |
| Native compatibility | `candidate-native-compatibility-run-02`: 8 actual previous/current Node/.NET native pairings, ABI 1/query 9; typed not-ready result without creating an index. |
| Host security smoke | `candidate-current-host-security-02`: both quarantined final images, Gatekeeper acceptance and normal LaunchServices mount/native metadata execution, with SIP/Gatekeeper enabled; owned mounts detached. |

Core coverage is **segmented**: full run 04 and suffix 05 failed their ordinary
compiler-publication readiness waits. Their passing portions and failed receipts
remain retained. Test-only readiness allowances were adjusted to 90 seconds;
query/cancellation/transport and compiler deadlines were unchanged. No uninterrupted
full core or full Debug pass is claimed. Earlier unchanged provider/corpus evidence
retains its original source/artifact boundaries; it is not recertified as a fresh
full-corpus performance run.

Owned installed coordinators were stopped/cleaned up. Lifecycle/upgrade guards
verified that the live development LaunchAgent and its original PID/plist stayed
unchanged. Original user repositories were not modified.

## Explicit validation limits

- The user removed VM testing from 0.0.3. All VMs are stopped. macOS 15+ Apple
  Silicon remains the distribution floor, but this candidate was not retested on
  macOS 15. Current-host evidence is macOS 27.2/Xcode 27.2/Swift 6.4.2, Node
  24.22.2, .NET SDK 10.0.403 and Rust/rust-analyzer 1.99.1 with matching rust-src.
- SwiftPM's installed check used normal host execution with its own manifest
  sandbox. It does **not** establish network/checkout denial. The earlier strict
  outer Seatbelt attempt failed nested sandbox application; that receipt remains
  failed. No sandbox disabling, preseeded serving index or VM substitute was used.
- The host security smoke uses current-host cached trust and normal connectivity;
  it is not cold/offline/VM first-use certification.
- Real video-encoding responsiveness remains user-deferred and unmeasured.
- Unsupported Rust closure/async/dispatch/cross-unit and some wrapper/generated
  input/macro-origin forms, and native TS7 project-body/provenance limits, remain
  documented partial capabilities. Finite probes do not certify arbitrary inputs.

## Publication handoff

Final public inputs are exactly the 12 files in
`.artifacts/implementation-0.0.3/common-dmg-02/release/` and the 10 files in
`.artifacts/implementation-0.0.3/bridge-dmg-01/release/`. Bridge proof/checksum names
are prefixed `xcode-bridge-`; all 22 names are disjoint. Release notes are retained
at `.artifacts/implementation-0.0.3/release-notes-draft.md`.

Update the runtime and SDKs together and restart embedding/MCP hosts. Forward
upgrade retains data/trust and advances settings capability 1 to 2. Rollback to
0.0.2 is intentionally refused; compatible-runtime rollback is separately tested.

Next is the user's publication decision, followed by repository/tag/release
operations and verification of actual public downloads. No public-download
verification or new-release publication is claimed at this handoff.
