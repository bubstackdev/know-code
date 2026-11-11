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

Required next checks include actual native trust/revocation/restart/identity cases; typed project failures and fresh healthy siblings; graph/cleanup plans; retained raw readers/WAL pause/resume and active rebuild retry; real Node panic and fs/crypto progress with test-only hooks absent from default artifacts; current/prior compatibility; then the frozen full Debug/optimized, installed offline/toolchain, real-encoding/resource, conditional free-VM minimum-OS, license/zero-telemetry and signed/notarized installation gates.

## Authorized GitHub replacement

Destination: [bubstackdev/know-code](https://github.com/bubstackdev/know-code). The user explicitly authorized taking over the superseded repository and does not want its old history backed up. No old-history fetch, clone, mirror or backup ref is required. Read-only repository/branch-tip inspection confirmed the destination and administrator access; the local `origin` now points there.

Leave the remote unchanged until the candidate is ready. At cutover, recheck remote tips, use an explicit lease for replacement, and retire superseded code refs/releases as appropriate to the authorized takeover. Do not delete unrelated account/repository resources or create backup branches/tags. Preserve the new implementation's source and bind the final tag/release assets to the validated revision. No remote source replacement, release publication or live activation has happened yet.
