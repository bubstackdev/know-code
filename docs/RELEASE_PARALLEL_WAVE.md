# Parallel release work — 2026-11-10

The user requested agent parallelism for the remaining 0.0.1 work. This wave completes independent release gates while the integrator owns cursor/control/query integration. It does not create a goal, reduce accepted scope, publish artifacts, inspect signing credentials or replace the live development installation.

## Owners and boundaries

| Owner | Deliverable | Owned paths | Exclusions |
| --- | --- | --- | --- |
| Integrator | Cursor/control/result-stream integration, shared protocol/API contracts, assembled candidate and validation | Core/host/protocol/bindings; shared release/acceptance documents | Do not edit either agent's owned build/package/test files until handoff. |
| Offline implementer | D02 post-restoration initial/incremental TS/C# analysis and CLI/MCP/both SDK queries with networking and checkout access denied | `tests/package_install.py`; narrowly justified owned fixtures/helper; `docs/OFFLINE_VALIDATION.md` | No product/core/schema/build/package script changes, no weakening existing checks, no optional-language expansion. |
| Provenance implementer | D04 a build record linking frozen build inputs/toolchain/flags to outputs; packaging rejects stale, missing or mixed evidence | `scripts/build.sh`, `scripts/package.py`; one shared helper if needed; focused provenance tests; `docs/BUILD_PROVENANCE.md` | No package-install harness/core/schema/provider/SDK changes; no full provenance framework/dependencies or unverified bypass flag. |
| Independent reviewer | Read-only contract/diff/evidence review of both agents and substantive integrator changes | No implementation writes | Findings go to task owners/integrator; passing branch checks do not certify the assembled candidate. |

Worktrees: `.artifacts/worktrees/release-offline/` and `.artifacts/worktrees/release-provenance/`. Each has separate Cargo targets, node_modules, generated protocol/provider outputs, SDK staging and test state. Shared immutable dependency/tool caches are allowed. Retained work never goes into reboot-cleaned directories. Installed-package test prefixes stay durable outside the tested checkout; use supported escalation when required.

## Offline gate

Reuse the actual archive/consumer harness. Separate dependency setup from a proven network-denied runtime phase, and prove denial rather than relying on package-tool `--offline`. Local coordinator Unix sockets must still function. No successful result may depend on cached indexes: test fresh initial indexing after dependency restoration, saved updates and the required public query surfaces. Preserve exact source/freshness/coverage assertions and existing resource/version/lifecycle checks. Missing restorations must fail actionably rather than silently escaping the sandbox.

Use the frozen archives in `.artifacts/cancellation-boundary/1794273886245465000/packages/` for a focused harness-development run if useful; record their hashes and old source identity explicitly. That is historical-candidate evidence, not current-candidate certification. After integration, rerun against newly built and provenance-verified final archives. No live prefix, source corpus or login job may be modified.

## Provenance gate

Extend existing build/package manifests with finite build-relevant input hashes, effective profile/toolchain/allowlisted flags and actual output hashes. Capture inputs before building and verify them again at completion. Packaging must verify the corresponding current inputs and consumed outputs before producing a candidate. Documentation/package-only inputs may have their own packaging inventory; do not force a product rebuild for an unrelated prose correction. Generated inputs/dependency closures must remain pinned/accounted for, not omitted merely because git ignores build outputs.

Reject missing records, wrong profiles, changed build inputs, changed outputs and mixed native/worker artifacts with actionable rebuild instructions. Use focused fault cases, plus one real isolated build/package demonstration when scheduled. Record build metadata only, never an environment or credential dump. Keep the implementation to the existing scripts/manifests and a small shared helper if justified.

## Validation and integration

Owners run focused checks in their own trees and preserve failures. No concurrent full suites or performance runs. The integrator schedules expensive compiler/package jobs; agents report when they need that slot. No private/global toolchain installation or network bypass: use supported escalation for blocked authorized work.

Handoffs include the exact commit, owned-path diff, commands/profile, input/archive identities, durable evidence, failures and remaining limits. Reviewer findings are fixed by the owner. The integrator merges sequentially, checks affected boundaries, then freezes one assembled candidate for broad Debug/optimized/native/package/platform/real-encoding gates. Required unavailable measurements remain open. Avoid introducing a validator or framework per criterion.

Recovery/replacement-index work and pagination share generation, pin and reader ownership. Their contracts require integrator coordination before parallel writes. Query body projection, test classification and cross-target grouping also share query/provider contracts; this wave does not assign overlapping engines to separate agents. Further bounded packets can start after these interfaces are established and slots free up.
