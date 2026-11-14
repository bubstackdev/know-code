# Know Code modifications to sourcekit-xcode-bsp

The upstream work remains owned by its contributors and retains Apache-2.0.
SwiftBuild and other dependencies retain their own notices and licenses,
including Swift's Runtime Library Exception where present. Distribution copies
those files under `licenses/`; no upstream copyright is reassigned.

Know Code contributors modified these seven Swift files in 2026:

- `Bootstrap/BuildServerBootstrap.swift`: load the exact compiler-exported PIF,
  select the saved scheme/Test configuration, enforce script-disable settings
  and finalize bound metadata rejections only after successful cleanup.
- `Bootstrap/KnowCodeBuildServer.swift` (new): preserve public BSP behavior while
  requiring typed indexing-preparation completion and operation success.
- `Config/ConfigLoader.swift`: accept the saved scheme, central package paths and per-open metadata
  nonce/input/toolchain binding.
- `Xcode/PIFExport.swift` (new): bounded compiler metadata export with explicit
  package paths, saved locks, no dependency-update fallback and owned outputs;
  launch the selected metadata executable directly and bind natural rejection
  receipts to known inputs/argv/context.
- `Xcode/ProcessRunner.swift`: bound process capture, EOF, deadlines and failures; distinguish
  natural command termination from signals.
- `Xcode/SchemeSelection.swift` (new): resolve the saved shared scheme and its
  Test action to exact native targets in the same compiler-exported PIF.
- `sourcekit-xcode-bsp/SourcekitXcodeBspCommand.swift`: identify the patched CLI as
  `0.1.0-know-code-index-only.7`, with a deliberate exit only for a
  durable metadata-command rejection proof.

Paths are beneath upstream `Sources/SourceKitXcodeBSP/`, except the CLI file
beneath `Sources/`. The distributed patch adds a prominent notice to each
changed file. The seven notice comments preserve attribution. The `.7` source additionally
implements the metadata rejection boundary described above; it differs from
the qualified `.6` source and needs its own build/provenance and relevant checks.
No earlier binary receipt is reused as validation of the new behavior.

This is an Xcode analysis tool, not an additional graph/query/store engine or
permanent service. Mandatory script guards are not permission to run an
untrusted repository. Know Code's personal trust gate and resource governor
remain authoritative. No Know Code usage collector or upload path is added.
