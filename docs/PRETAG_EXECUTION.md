# v0.0.1 pre-tag execution

The user authorized parallel fixes and completion of the existing release gates, then tagging v0.0.1. Feature scope remains frozen; the accepted trust and project-failure decisions in [the triage](CODEBASE_REVIEW_TRIAGE.md) are required fixes.

## Work ownership

- Runtime/cursors: checked wait/unregister, terminal job panic ownership, missing-safe retirement, cleanup-debt isolation, CPU-pause and deadline-safe publication.
- Storage/graph: three cleanup indexes, filtered streamed adjacency, depth-frontier bounds, bounded declaration anchors and terminal recursive-call paths.
- Providers: expected setup failure isolation, explicit oversized-source gaps, recursive/accessor facts and real compiler regressions.
- Native bindings: async/sync panic containment, bounded work outside libuv, environment cleanup and explicit trust methods.
- Setup/transport: executable-selector and finite-file hardening; cancellable worker pipes and safe PATH; MCP coverage schema.
- Integrator: common trust authority, shared contracts/compatibility, CLI, fixture grants, one matching assembled build, serialized heavy checks, packaging/distribution and final source/tag.

Agent edits use durable isolated worktrees under `.artifacts/worktrees/release-*`. Agents run static checks and write regressions; one integrator owns compiler/native/package runs. Do not modify the live installation or original corpora for tests. Retain all failed attempts. Full Debug/optimized checks belong to the settled candidate, not every adjustment.

## Explicit project execution authority

The central per-workspace `trust.json` is separate from `settings.json` and package-script permission. It binds permission to the exact workspace identity. Registration, discovery and opening do not grant it. `know-code trust WORKSPACE` grants; `--revoke` revokes. Granting an unregistered workspace must not enroll/index it. SDKs expose a separate trust operation after opening.

The common Rust `ProjectExecutionGrant` is checked before worker execution and again before publication. Low-level callers constructing `WorkerCommand` must supply that explicit grant; an arbitrary registration or checked-in configuration is insufficient. Coordinator cache/state gates queued work and actively cancels revoked analysis. Preserve existing failure fingerprints, cancelled baselines and dependency preparation authorization. An interrupted allocated rebuild still requires explicit refresh; pre-attempt deferral can resume once permission/space permit it.

Trust is an authorization boundary for compiler JavaScript, MSBuild and generators; it is not a sandbox for code the user trusts. Package scripts remain separately disabled by default. Existing coherent data can remain readable with accurate stale/unavailable evidence when future execution is withheld. Never delete an index to implement revocation.

Lifecycle capability 4 and native query contract 8 cover this new boundary, including SDK refusal of pre-trust natives before workspace control. ABI 1, worker/control framing 7, store 8, settings 1, resource 3, cursor 2 and recovery-receipt format 2 stay unchanged. The installer may quiesce known previous lifecycle sources 1–3 through its narrow bridge; ordinary consumers require current contracts. Exact final artifacts must prove the transition.

## Validation and retained evidence

Development diagnostics are under `.artifacts/pretag-wave/diagnostic/`. The first assembled pre-trust compile passed; its Rust run exposed four fixture expectations that needed correction (cleanup warning policy, actual main/WAL corruption, and DiskPressure type). Those failures stay retained. The first combined C# source chunk builds with zero compiler warnings/errors. None of those prefixes certifies the final trust/native/transport assembly.

The focused checkpoints below cover actual trust, provider failures, managed WAL behavior, the accepted npm 10 policy and the scripts-safe package-manager pin matrix on named development prefixes. Remaining gates include the settled candidate's full Debug/optimized, current/prior compatibility, final signed installed/offline/toolchain, real-encoding/resource, conditional free-VM minimum-OS and notarized normal-security installation checks. The signing pipeline has a diagnostic artifact-bound pass; final assets still require their own evidence.

## Audited focused checkpoint — 2026-11-11

These are development-prefix results, not a final candidate certification. The recorded log SHA-256 values were independently checked; the three Python harness hashes match their recorded values. The current build-04 producer/host and WAL holder source/binary, Node wrapper and real Node hashes also match the WAL receipt.

| Evidence | Bound source/build | Result |
| --- | --- | --- |
| [Rust prefix](../.artifacts/pretag-wave/diagnostic/rust-04/summary.json) | `e21f0e25c61a4301f0a5c7a9c30bd9360ba1ba42` | 245 passed, 2 ignored |
| [Trust](../.artifacts/pt/9e73924a8/summary.json) | build-03 `d3324306ea0e4ecd900ba6e3042b18a6`, source `3cea4a23bdaa7837aa59c2a011a29cde11fa3f7d` | 6 groups passed |
| [Providers](../.artifacts/pf/ab8b21228/summary.json) | Same build-03 prefix | 6 groups passed |
| [Later handoff probes](../.artifacts/pretag-wave/diagnostic/trust-handoff-01/summary.json) | Separate focused Cargo runs after the coordinator handoff fixes | 2 handoff tests, 1 raw-worker revocation test and formatting passed |
| [Managed WAL](../.artifacts/wp/7cdb3fb00/summary.json) | build-04 `276a80615840445cb81776445afb4a5b`, source `90ec4ad110e1d1ab026f35eae2fc1725954de522` | 4 groups passed |

The trust fixture proves inert pregrant/enrollment separation, registration/settings/restart without implicit permission, an actual compiler-module marker executing only after trust, stale reads during revocation, regrant without a new restore retry, cancellation while untrusted, and missing-root revoke/unregister. The provider fixture exercises real unsupported TypeScript 5.8.3, missing declared compiler versus own fallback and outside-store hoisting, near/over-frame TS/C# source gaps, recursive/accessor facts through native queries, scoped malformed project failures, repair and fatal unknown extraction without incomplete publication. The shared compiler selection/restoration resolver is already present in build-03; later scripts-off child-installer propagation is not certified by that provider run.

Build-03 does **not** contain the later coordinator consumed-restore-retry and cancellation-handoff fixes. Those have the separate focused Rust evidence above; the full Rust prefix is not relabeled as a full run of their combined source. The two ignored Rust WAL probes were not counted as passing tests. Build-03's summaries retain its producer hash and output bindings, but its full producer manifest was overwritten by build-04 and was not independently reverified. Build-04's available producer hash is `a24f17c2dfbcf7115a36a8a25be1856af69afb0f4b31feeb21a23532143925f7`.

Successful fixture receipts report their owned coordinator sockets removed; WAL additionally reports zero retained holder processes and removal of its active worker gate. Raw command/error evidence remains beside each summary. The earlier [trust attempt](../.artifacts/pt/d2d892be0/summary.json) remains failed with `KeyError: 'symbols'`; test-only `24c103d` corrected the omitted-empty-array assertion before the passing rerun. No full optimized/platform/package/offline/signing/encoding/performance or release-tag claim follows from this checkpoint.

## Signing preparation — 2026-11-11

The user supplied `developerID_application.cer` and requested a durable copy. The owner-only copy is `.artifacts/signing/developer-id/developerID_application.cer`; its SHA-256 is `f9a80b2743c868aa5579cd1ac2b9cf5a8925a6784cb2a1c0bb367eb0ffbfcf75`. It matches the prepared CSR/key and is installed in the user keychain as Developer ID Application for team `3FMAPDQDGP`. An owned Debug executable copy passed hardened-runtime timestamped signing and strict signature verification. The configured ASC account can see this certificate. Private staging keys/containers were removed after successful keychain signing; the user's original download was preserved.

The public certificate and probe metadata remain ignored local artifacts. They are not final release evidence: no final package has been signed or notarized, and normal-security installation is still required. Preserve the original producer/output hashes and record signing as an explicit transformation when final archives are assembled; do not rewrite unsigned build provenance to claim it produced signed bytes.

### Staged signing and installed diagnostic checkpoint

Source `ff4506d` produced a matching Debug package set in `.artifacts/pretag-wave/packages/diagnostic-02/`. A first packaging attempt correctly rejected .NET generated inputs changed by packaging at a later Git revision; the failure remains in `diagnostic/package-01`. Building and packaging at one unchanged revision passed (`diagnostic/package-02`).

`scripts/sign_packages.py` signed copies of all four expected native payloads, preserved every non-native archive member and mode, retained the unsigned producer, and authenticated the final manifest/transform receipt with a keychain-backed CMS signature. Native verification checks the Apple Developer ID requirement, Team ID, exact certificate, hardened runtime and secure timestamp. Separate probes verified CMS content and rejected tampering. Four archive-boundary tests passed; independent review caught and fixed the optional `codesign` certificate-prefix argument before the actual staged run.

The signed diagnostic manifest SHA-256 is `7cbd139f319b3591f7ec410def8acae60a18f2fca747b1308debc1185b8ead0a`; its producer-record SHA-256 is `5002de85a219e7f4b4470d5d910c0dc66c9fedaac6cdbb2feff00d645d4c7524`. Exact archives, unsigned originals and replay evidence remain in `.artifacts/pretag-wave/packages/signed-diagnostic-01/`. These actual signed archives passed all **27 installed checks**, including both native SDKs, TS/C# analysis, saved-source changes, CLI/MCP parity and relocation with checkout access denied and network denied after restoration. Raw installed evidence is `/Users/zakkeown/Code/.kco/siga4150bf98/result.json`; the invocation/log binding is `diagnostic/signed-install-01/summary.json`.

All three signed archives contain the three newly pinned NuGet license texts with matching upstream commit/hash provenance; the artifact-bound check is `diagnostic/signed-notices-01.json`. The source audit is [PUBLICATION_AUDIT.md](reviews/PUBLICATION_AUDIT.md). Local unsigned copies and replay directories are operator evidence, not release assets.

This is a diagnostic Debug checkpoint, not final release certification. It does not cover the subsequent npm 10 safety-policy changes, the final full/optimized or real-encoding gates, minimum-OS testing, notarization, quarantine/default-security installation or remote publication. No artifact was submitted to Apple notarization and no live runtime was replaced.

## Complete package-manager policy checkpoint

At source `9558e77`, all ten named floor/latest npm/pnpm pins passed the complete six-group gate, including **22 real restoration/policy cases**. Evidence: `.artifacts/preparation-safety/1a1cce11e3c/validation.json` (SHA-256 `59e772ce8ddeb6328f02bf72192adf388339531bab8cf3a2512f60fe4a436fbf`), with a compact binding in `diagnostic/setup-06-full/summary.json`. This includes npm 10 hosted/direct/nested Git refusal; effective packing-policy precedence; registry, tarball, directory and executable-link positives; no ungranted script markers; bounded setup inputs; and current TypeScript compiler-store retarget/dangling-origin checks. The accepted policy and retained failed attempts are described in [NPM_SCRIPTS_OFF.md](reviews/NPM_SCRIPTS_OFF.md).

The user accepted blocking npm 10 Git fetching and rejecting effective `install-links=true` while scripts are disabled, preserving ordinary linked-directory, registry and tarball restoration. The isolated npm 10 preload covers both Git fetchers and its upstream linked-package script bypass; it does not modify cached package files. The shared compiler-origin helper checks the current physical target before Node resolution, avoiding a stale symlink lookup. Both changes feed the existing preparation/update path.

The extended installed harness must still prove the preload works from the relocated signed runtime under checkout/network denial. The complete source pin gate does not certify final packages, every intermediate patch, normal-security installation or the final release. The user will start normal video encoders only after the integrator signals that the final optimized candidate and owned indexes are ready.

## Authorized GitHub replacement

Destination: [bubstackdev/know-code](https://github.com/bubstackdev/know-code). The user explicitly authorized taking over the superseded repository and does not want its old history backed up. No old-history fetch, clone, mirror or backup ref is required. Read-only repository/branch-tip inspection confirmed the destination and administrator access; the local `origin` now points there.

Leave the remote unchanged until the candidate is ready. At cutover, recheck remote tips, use an explicit lease for replacement, and retire superseded code refs/releases as appropriate to the authorized takeover. Do not delete unrelated account/repository resources or create backup branches/tags. Preserve the new implementation's source and bind the final tag/release assets to the validated revision. No remote source replacement, release publication or live activation has happened yet.
