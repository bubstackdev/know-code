# Language worker contract

Status: versioned Protobuf batches over local stdin/stdout are accepted and implemented in the [first working slice](IMPLEMENTATION.md). Protocol v7 uses four-byte little-endian lengths, an 8 MiB frame bound, and 128-occurrence fact batches. The current contract includes compilation source ownership, project dependency/input metadata, coordinator control envelopes, required preparation checkpoints, and bounded progress messages. V3 added configured/loose TS inputs and preparation retry/fingerprint context; V4 accompanies compact-key storage and deliberate library/coordinator compatibility. V5 adds source-backed relationship evidence, implicit/dispatch flags and capability reports; V6 adds selected project contexts, compilation snapshot witnesses and exact definition pointers; V7 adds receiver-specialization keys and snapshot fingerprints on selected references; incompatible older peers return update-required errors. The reader supports store formats 2/3/4/5/6 and the writer migrates formats 1/2/3/4/5 to 6; legacy triples remain legacy_unknown rather than being promoted to compiler-backed calls; wire and store versions are distinct contracts. Cancellation, chunking, worker reuse, and broader deadline/lifecycle work remain unfinished.

## Purpose and boundary

The coordinator uses one analysis contract for TS/C# and future Swift/Rust providers. Workers own compiler/project semantics and emit normalized evidence. Rust owns persistence, generation publication, graph queries, and impact traversal. Workers do not write live SQLite indexes or provide a second graph query engine.

See [fact model](FACT_MODEL.md) and [update pipeline](UPDATE_PIPELINE.md). Those describe logical facts and publication; a transport encoding must not redefine their semantics or leak into the in-process query API.

## Accepted transport

Use versioned Protobuf messages in bounded, length-prefixed batches over local child-process stdin/stdout. Both worker-control and fact messages use the same envelope/codec. Keep ordinary process error output separate from framed stdout. A server listener or network RPC framework is not part of the worker transport.

Design rationale:

- A shared schema can generate message types/codecs across worker languages rather than hand-maintaining several wire formats.
- Binary fields can carry source chunks and fact batches without forcing textual/base64 representation on the main transfer path.
- Explicit schemas and field-evolution rules support separately built workers and future providers.
- The TS/C# reference indexers already use Protobuf for SCIP artifacts, though SCIP alone does not supply Know Code's invocation, update, or control semantics.

Tradeoffs: schema code generation and runtime libraries add build dependencies; binary streams need decoded diagnostic views. Throughput/allocation benefits must be measured on real facts rather than assumed from the format name. End users of packaged artifacts should not need a schema compiler merely to run Know Code.

The initial [schema](../proto/analysis.proto) is shared by Rust prost 0.14.4, JS protobufjs 7.5.4, and C# Google.Protobuf 3.36.2, pinned through lockfiles. Rust/C# types are generated using a pinned native ARM64 protoc; JS loads the same schema. These are initial implementation selections; Swift codecs and release protocol evolution remain open. End users do not need protoc to run built artifacts.

## Project-scoped SDK tooling

The C# provider also supports an internal `--sdk-metadata` mode of the same binary. The analysis worker sends a typed `SdkMetadataRequest` for one project/configuration and the selected installed SDK, then receives progress, `SdkMetadataResult`, and completion. It reuses the same version/job checks, four-byte framing, 8 MiB bound, and C# codec. These optional envelope fields are internal tooling additions to v4; the regular analysis/control contract, store format, and native ABI are unchanged. The helper has no index/query responsibilities and cannot become a second graph engine.

A planning request discovers frameworks/assets without running identity targets. Restoration finishes before complete metadata or Roslyn inputs are cached. Complete metadata is evaluated once per project in the current job, with distinct properties/results for every target. The helper runs in that project's directory and exits afterward, preserving the SDK's process-wide startup-directory semantics without a resident per-repository process.

Identity evaluation invokes the standard `GetAssemblyVersion` target, including repository hooks. Project/import caches are unloaded between targets because those hooks can modify imported files. SDK assemblies are compile-only references, resolved from the selected installed SDK through Locator before their types are JITted. Error details are bounded; SDK/protocol failure aborts publication and retains prior coherent facts. See [implementation](IMPLEMENTATION.md) and [profiling evidence](CSHARP_PROFILING.md).

## Minimal message responsibilities

| Responsibility | Contents / behavior |
| --- | --- |
| Handshake | Protocol/fact schema compatibility, worker/provider/compiler identity, capabilities, build profile. |
| Analyze/update request | Job/workspace/compilation identity, observed input manifest, requested scope, deadline/cancellation context. |
| Fact/source batch | Ownership/input identity, bounded symbols/occurrences/relations/reports or source chunks, ordering/reference rules. |
| Progress/setup status | Project readiness, prerequisites/restore, generation progress, actionable structured failures. |
| Completion | Finished analysis units and input versions, completeness/coverage summary; source errors can complete valid partial analysis. |
| Cancel/shutdown | Acknowledge lifecycle state, stop work cooperatively, and release resources. |

These are protocol responsibilities, not a mandatory message-per-row design. Prefer a small envelope and typed payload families, not separate RPC services or a workflow DSL.

## Completion and compatibility

- Every batch belongs to a job and scoped analysis/input version; late batches cannot publish over newer work.
- A successful decode is not permission to publish. The coordinator requires the expected job/unit completion and common fact invariants before atomic publication.
- Compiler errors are data/coverage, not necessarily a failed stream. A crashed worker or truncated frame is not a completed partial graph.
- Check compatibility once at worker startup, and decode/validate each external batch at the shared ingestion boundary. Do not repeat the same validation in transports, store wrappers, and query operations.
- Evolve schemas with deliberate version/capability rules. Generated codecs do not prove semantic compatibility, and unknown fields must not hide a required unsupported capability.
- Do not hash arbitrary Protobuf serialization as a canonical semantic identity/configuration fingerprint; define canonical input identity separately.

## Resource and lifecycle requirements

- Bound frames, aggregate in-flight bytes, and queued jobs. Stream many bounded messages rather than one project-sized object.
- Chunk or reference large source artifacts with explicit lifetime; do not make the maximum source-file size an unlimited frame/memory allocation.
- Apply backpressure while Rust stages batches, keeping compiler/process pipes from creating unbounded retained output.
- Use cancellation/deadlines in the common job lifecycle. Compiler responsiveness and forceful worker retirement need provider-specific handling without corrupting staged/serving state.
- Keep project caches scoped to explicit workspace/compilation/toolchain ownership. Worker reuse must not leak ambient project configuration.
- Honor repository-selected TypeScript/.NET SDK versions. Handshake/setup and analysis ownership must distinguish the effective compiler/SDK from the worker host runtime; unsupported/missing selections produce actionable failures. Do not reuse incompatible compiler state across toolchains or silently substitute another version. Exact version support, load/isolation strategy, and handshake fields remain open.
- Public zero-telemetry builds still report required progress/errors, but do not capture usage traces. Debug/personal builds can record decoded requests/facts/state under the accepted local rotation policy.

Detailed pool affinity, SDK-version isolation, startup/idle retirement, framing recovery, and performance budgets remain design work.

## Primary references

- [Protobuf overview](https://protobuf.dev/overview/): shared schemas and generated message codecs.
- [Streaming and large-data techniques](https://protobuf.dev/programming-guides/techniques/): messages are not self-delimiting; streams need framing and large datasets should be decomposed into smaller messages.
- [Rust prost](https://github.com/tokio-rs/prost), [TypeScript Protobuf-ES](https://github.com/bufbuild/protobuf-es), [C# generated code](https://protobuf.dev/reference/csharp/csharp-generated/), [SwiftProtobuf](https://github.com/apple/swift-protobuf): candidate codec ecosystems.
- [Non-canonical serialization](https://protobuf.dev/programming-guides/serialization-not-canonical/): serialization is not a universal canonical identity representation.

The implementations/codecs above are built and exercised by integration tests. This does not establish production throughput, frame-size suitability for every source file, or large-estate performance guarantees.
