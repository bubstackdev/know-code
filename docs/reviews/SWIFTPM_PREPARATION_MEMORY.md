# SwiftPM module preparation: concurrency source audit

Source-only checkpoint, 2026-11-13. This audit does not run compilers, change resource defaults, or establish the cause of the Atelier memory failure.

## Observed production boundary

The isolated full Atelier run `atelier-production-full-01` stopped during the first Library module preparation with the shared typed `resource_limited` outcome: 2,133 MiB sampled against the configured 2,048 MiB limit, generation zero, after 68.08 seconds. Dependency restoration had completed first. This is aggregate process-tree memory, not a measurement identifying one frontend or one module. Its retained receipt is `.artifacts/implementation-0.0.3/atelier-production-full-01/production-result.json`.

The provider passes `--jobs 1` in the common `packageArguments()` used by restoration and `experimental-build-server`. It awaits each owned target's `buildTarget/prepare` response before analyzing that target or preparing the next target. There is no source evidence here of overlapping provider-owned target preparations.

## Primary source trace

The retained SwiftPM files in `.artifacts/swiftpm-research/` match their Git blob entries in the saved upstream tree `566b994235f46802de60dfe004e0612bdfefabde`:

| File | Upstream blob |
| --- | --- |
| `Sources/Commands/PackageCommands/BuildServer.swift` | `225662ae9ac2f1774786d287eeed9b5237a4d948` |
| `Sources/SwiftPMBuildServer/SwiftPMBuildServer.swift` | `3b36cae1369ee3ab10d3999754f2116858d3b946` |
| `Sources/CoreCommands/Options.swift` | `f407c03a032c6e2c1b46115a7aca9ad6e3334c22` |
| `Sources/SwiftBuildSupport/SwiftBuildSystem.swift` | `fe756d9116c33c1a38c2ce4c5f1393d9900e3af0` |

In this source snapshot:

1. `BuildServer.swift` selects the SwiftBuild backend explicitly, rather than the ordinary native SwiftPM backend.
2. `SwiftPMBuildServer.swift` creates its underlying `SWBBuildServer` using `SwiftBuildSystem.makeBuildRequest()`.
3. `SwiftBuildSystem.swift` assigns `request.schedulerLaneWidthOverride = buildParameters.workers` (line 1179). The options declaration describes `jobs` as the llbuild scheduler-lane count. The retained subset does not include the command-state constructor that maps that option into `BuildParameters.workers`.
4. `SWBBuildServer` copies the supplied build request for preparation. Its serial `preparationQueue` prevents overlapping preparation requests; selecting one target can still prepare its dependency closure.
5. `SWBBuildSystem/BuildOperation.swift` uses `request.schedulerLaneWidthOverride` ahead of its default when constructing llbuild's scheduler lanes.

The last two points also hold in the locally retained, pinned external bridge dependency `swift-build` revision `7f96ee0e8aa8a35fe84f3e80932eb9e456356d10`, under `.artifacts/xcode-bridge/build/checkouts/swift-build/`. They are reference-source findings, not proof that Xcode's installed SwiftPM embeds this exact revision. The selected installed toolchain does not include corresponding SwiftPM source in its `usr/share/pm` directory.

## Separate compiler dimensions

SwiftBuild's compiler spec derives a driver `-jN` from the host's active processor count and can derive `-num-threads N` for parallel whole-module optimization. Neither flag alone proves that several frontend processes ran in this preparation.

In the integrated driver path, `LibSwiftDriver.Executor.execute(workload:..., numParallelJobs:...)` retains a planned build; it does not itself launch a parallel executor for that workload. `SwiftDriverJobSchedulingTaskAction` registers individual driver jobs with the build system, whose scheduler controls their execution. By contrast, frontend-internal threads are a separate dimension from llbuild task lanes. One frontend can also exceed the shared memory limit without concurrency.

## Disposition

No product change is justified by this source audit alone. Keep the two-core/2-GiB defaults and the explicit resource-limited status. The next useful evidence is an owned process-tree sample showing the largest process, exact module and command, concurrent frontend count, and phase at the limit. Do not infer a concurrency bug merely from a host-count `-j` flag, or raise the memory default to make the fixture pass.
