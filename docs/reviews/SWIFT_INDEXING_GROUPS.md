# Swift indexing by exact compiler settings

Source-only research, 2026-11-13; follow-up to
[SwiftPM preparation reuse](SWIFTPM_PREPARATION_REUSE.md). No compiler, probe,
benchmark or production change accompanies this proposal. The active Atelier
copy and indexing job were left untouched. These observations establish no
speedup or selected-Xcode compiler compatibility.

## Current cost and ownership boundary

[`compileSources`](../../providers/swift/index.mjs) launches the selected driver
once per source, using `-index-file -index-file-path FILE` with a distinct unit
output token. SwiftPM and Xcode already retain exact ordered argument/working
directory groups ([SwiftPM](../../providers/swift/swiftpm.mjs),
[Xcode](../../providers/swift/xcode.mjs)); compilation currently ignores that
deduplication. Repeated parsing/import/semantic setup is therefore a candidate
cost, not an established proportion of elapsed time under encoder contention.

The current reader accepts records only from compiler units whose main file is
in the retained source set; missing per-file units remain partial. Preserve
that ownership gate, local symbols, CALLEDBY relations, full-body checking and
exact saved UTF-8 ranges. Current SwiftPM progress can remain `preparing` during
the later synchronous `compileSources` loop: no analysis progress event is
emitted there. That status alone cannot identify the expensive phase.

## Primary-source evidence

- The official [Swift driver CompileJob implementation](https://github.com/swiftlang/swift-driver/blob/main/Sources/SwiftDriver/Jobs/CompileJob.swift#L130-L180)
  collects repeated `-index-file-path` arguments as primary inputs for one
  indexing compile. Its output-map lookup and per-primary index-output handling
  provide a route to distinct unit tokens without one process per source.
  This is a narrower first candidate than changing to a primary-less module
  invocation; a single shared scalar output token must not collapse units.
- Swift's [frontend index emission](https://github.com/swiftlang/swift/blob/main/lib/FrontendTool/FrontendTool.cpp#L932-L942)
  iterates all primary source files, or uses the module overload when there
  are none. The [module recording overload](https://github.com/swiftlang/swift/blob/main/lib/Index/IndexRecord.cpp#L793-L837)
  itself writes per-source units and requires enough unit-output tokens.
  [`recordSourceFileUnit`](https://github.com/swiftlang/swift/blob/main/lib/Index/IndexRecord.cpp#L689-L734)
  associates the source filename as main file and forwards the local-symbol
  setting. A separate serialized-module unit may lack a main file; it must not
  substitute for per-source ownership evidence.
- [Frontend input/output checks](https://github.com/swiftlang/swift/blob/main/lib/Frontend/FrontendInputsAndOutputs.cpp#L112-L120)
  still constrain some multiple-primary paths. These upstream `main` sources
  guide investigation; they do not prove that the selected installed driver
  supports this exact action/output combination. Do not bypass checks to make
  a candidate pass. Full-body normalization in
  [`semanticArguments`](../../providers/swift/compiler.mjs) must remain intact.

## Proposed finite probe, before implementation

1. On an owned small existing Swift fixture, retain the current per-source
   baseline, exact compiler identity, environment, ordered inputs/arguments,
   working directory and saved-byte hashes. Inspect the selected driver's
   planned jobs for repeated index-file selections and distinct per-primary
   output-map tokens; require the intended single frontend invocation.
2. Compare baseline and grouped IndexStore records through the existing reader
   and graph checks: cross-file calls, local/function-value references,
   initializer/implicit-call labels, Unicode coordinates, an excluded empty
   file, and compiler-error partial results plus saved repair. Require every
   selected file's own unit/record witness and retained exact text; compare
   semantic sets rather than traversal order or incidental unit filenames.
3. Repeat the owned device/simulator case with generated source. Include a
   deliberate second settings group; never union different defines, SDKs,
   targets, module/import options, source lists or working directories. Only
   exact groups can share an invocation, and every original module input must
   remain available as compiler context.

Only after parity passes, measure elapsed/CPU time, process count and sampled
aggregate memory with the existing shared governor. Larger groups may increase
peak memory or enlarge an error's coverage impact. Keep the existing per-source
route unless a bounded grouping policy proves both correctness and resource
behavior. This proposal adds no engine, persistent cache or validation harness.
