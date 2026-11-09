# 0.0.1 acceptance draft audit

Three independent read-only subagents reviewed the acceptance draft: requirement scope, validation strength, and parallel execution. None changed source or ran builds/tests. The parent verified that the audited document inputs remained unchanged while the reviews ran; their hashes and the source inventory are retained under `.artifacts/release-audit/1794260554365978000/`.

The revised [acceptance contract](RELEASE_ACCEPTANCE.md) incorporates the traceable corrections below. These are improvements to the draft and execution plan, not passing release evidence. A goal has not been started.

## Findings and disposition

| Finding | Disposition in the draft | Remaining work |
| --- | --- | --- |
| C07 allowed a full build/codegen if it was merely announced, weakening the accepted prohibition on automatically invoking it to fill missing outputs. | Restored the stronger requirement; distinguished compiler-integrated generation and expressly opted-in package-install scripts. | Verify behavior against the finite compiler/generation catalog. |
| Whole-product offline operation after restoration was missing, despite the accepted local-first constraint. | Added supported initial/incremental analysis and native/CLI/MCP reads with networking denied to D02. | Extend the existing installed-package harness; its current policy denies checkout access but still permits networking. Network denial alone does not prove zero local collection. |
| Shared bounded Protobuf worker stdio and local per-user Unix control were not explicit AC outcomes. | Added the existing transport constraints to C01 and malformed/truncated/oversized/incompatible-peer evidence. | Preserve existing shared codecs/protocol tests; no new transport framework is required. |
| S02 did not explicitly preserve the initial SQLite source-store boundary or exclude an unbounded nonsearchable source archive. | Specified workspace SQLite, deferred external blobs pending measured need, and bounded current/pinned/recovery retention. | Verify cursor/rebuild reclamation as those paths are implemented. |
| Packaging can associate current source hashes with older prebuilt binaries. | D04 now requires a build record linking frozen inputs/toolchain/flags/output hashes to the packaged/signed artifacts. | Correct the build/packaging evidence path. Reuse the artifact manifest; do not create a separate provenance framework. |
| R04 could be misread as one pooled p95. | Referenced the existing measurement protocol and required each declared fixture/query/runtime/target cell to pass separately. | Freeze measurement inputs, retain tails and perform the real-encoding run; synthetic results remain insufficient. |
| “Supported” and “bounded” still leave verification surfaces open. | Added an explicit requirement to freeze finite support, graph-case, query/cursor and compatibility matrices before goal freeze. | Prepare those matrices. Mechanisms/file names can be implementation choices; agents cannot narrow promised cases to obtain a pass. |
| R02's cancellation-before-first-baseline/offline-save limitation is documented but not accepted as an exception. | Marked it as an unresolved gap requiring a fix/test or a specific user-approved exception. | Reproduce/resolve that case; documentation alone cannot satisfy resume-on-save acceptance. |
| A failing product could be mislabeled as an unviable VM and waive D02. | Defined free VM viability independently: boot macOS 15 ARM64 and run the declared prerequisites. | Establish the guest. Once viable, failing Know Code checks are release failures. |
| The original Q06-Q08 bundle is coupled across query/provider contracts. | Replaced it as the first-wave recommendation with C09 package preparation plus C06 C# build-context planning. | Agree their small shared contract before forking implementation worktrees. |
| Both proposed tasks need core input tracking, and mapped C# identities need more than metadata changes. | Assigned shared protocol/settings/control/invalidation to the integrator. C06 explicitly owns `ProjectLinks.cs`; C09 owns its narrow adapter/fixtures. | Track `pnpm-workspace.yaml` and solution files centrally; derive C# reference witnesses/IDs from effective mapped contexts. |
| A source worktree alone does not isolate dependency replacement, generated protocol files or native smoke outputs. | Made generated/build/test-output isolation explicit alongside test cadence/reviewer ownership. | Establish a reviewed source baseline first; use per-worktree outputs and isolated runtime/state. Exclude generated Python bytecode rather than blanket staging. |

## Evidence anchors

- Accepted constraints: [DESIGN.md](DESIGN.md), especially local operation, transports, source storage, generation behavior and correctness.
- Stale-build evidence risk: [scripts/package.py](../scripts/package.py) consumes existing outputs and separately inventories current source.
- Existing sandbox scope: [tests/package_install.py](../tests/package_install.py) denies checkout access while allowing networking.
- Measurement contract: [PERFORMANCE.md](PERFORMANCE.md#accepted-first-lookup-gate).
- Cancellation limitation/current checks: [RESOURCE_CONTROL.md](RESOURCE_CONTROL.md), [tests/resources.py](../tests/resources.py).
- Shared input discovery: [crates/core/src/workspace.rs](../crates/core/src/workspace.rs).
- C# context identity coupling: [providers/dotnet/ProjectLinks.cs](../providers/dotnet/ProjectLinks.cs), [ProjectCatalog.cs](../providers/dotnet/ProjectCatalog.cs), [SdkMetadata.cs](../providers/dotnet/SdkMetadata.cs).
- Generated/staged output coupling: [scripts/build.sh](../scripts/build.sh), [scripts/generate-protocol.sh](../scripts/generate-protocol.sh).

## Next execution boundary

The source baseline and shared interfaces are prerequisites to parallel writes. The first pair can then work on npm/pnpm preparation and repository-derived C# contexts in separate owned paths, with a read-only reviewer and parent-controlled integration. Unrelated cursor/rebuild implementation mechanisms need not hold up those tasks.

Before activating a full-release goal, finish the finite acceptance/support matrices and resolve material scope choices through Q&A. Keep real-encoding and conditional VM evidence explicit. Signing/notarization access is available, but final signed-artifact verification is still required. Publication remains a separate user decision.
