# Know Code modifications to sourcekit-xcode-bsp

The upstream work remains owned by its contributors and retains Apache-2.0.
SwiftBuild and other dependencies retain their own notices and licenses,
including Swift's Runtime Library Exception where present. Distribution copies
those files under `licenses/`; no upstream copyright is reassigned.

Know Code contributors modified these seven Swift files in 2026:

- `Bootstrap/BuildServerBootstrap.swift`: load the exact compiler-exported PIF,
  select the saved scheme/Test configuration and enforce script-disable settings.
- `Bootstrap/KnowCodeBuildServer.swift` (new): preserve public BSP behavior while
  requiring typed indexing-preparation completion and operation success.
- `Config/ConfigLoader.swift`: accept the saved scheme and central package paths.
- `Xcode/PIFExport.swift` (new): bounded compiler metadata export with explicit
  package paths, saved locks, no dependency-update fallback and owned outputs.
- `Xcode/ProcessRunner.swift`: bound process capture, EOF, deadlines and failures.
- `Xcode/SchemeSelection.swift` (new): resolve the saved shared scheme and its
  Test action to exact native targets in the same compiler-exported PIF.
- `sourcekit-xcode-bsp/SourcekitXcodeBspCommand.swift`: identify the patched CLI as
  `0.1.0-know-code-index-only.6`.

Paths are beneath upstream `Sources/SourceKitXcodeBSP/`, except the CLI file
beneath `Sources/`. The distributed patch adds a prominent notice to each
changed file. These seven notice comments are the only difference from the
qualified wired08 source packet; no behavior change is implied by the notices.
The newly packaged source requires its own build/provenance and relevant checks.

This is an Xcode analysis tool, not an additional graph/query/store engine or
permanent service. Mandatory script guards are not permission to run an
untrusted repository. Know Code's personal trust gate and resource governor
remain authoritative. No Know Code usage collector or upload path is added.
