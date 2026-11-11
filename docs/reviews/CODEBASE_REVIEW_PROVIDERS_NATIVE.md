# Provider and native review triage

This is a read-only assessment of the provider/native findings in [CODEBASE-REVIEW.md](../../CODEBASE-REVIEW.md). No panic/execution reproductions, builds or provider source fixes were performed. Source confirmation is not exploit reproduction or passing validation. Main was changing during the review; the revision and inspected-file hashes below delimit this assessment.

## Release disposition

| IDs | Source assessment | Smallest appropriate disposition |
| --- | --- | --- |
| H4 | Confirmed; the suggested attribute alone does not cover asynchronous compute. | Pre-tag crash containment in the Node binding, preserving shared core and reservation ownership. |
| H5 | Confirmed; compiler fidelity includes repository-controlled execution. | Pre-tag personal project-execution grant for **every** repository, including explicit open/register; independent scripts grant. |
| H9 | Confirmed for project planning/configuration/evaluation failures; ordinary compiler diagnostics and some restore failures already produce partial coverage. | Pre-tag per-compilation coverage isolation, without converting protocol/storage failures into successful analysis. |
| M1 | Configuration inheritance confirmed. The particular git-helper execution chain is supported by documented mechanisms but was not reproduced here. | Close executable-configuration bypasses in the existing adapter while preserving registry/auth settings. Trust and `ignore-scripts` are separate controls. |
| M2 | Registry-derived SRI and entry-only cache verification confirmed. Calling all integrity checking circular overstates the result. | Document the actual HTTPS-registry trust anchor; independently pin/sign defaults if that stronger guarantee is required. Complete cache inventory is a separate correctness improvement. |
| M3 | Confirmed: relative/empty PATH entries are retained and tools run in workspace directories. | Pre-tag filter or reject unsafe PATH components and resolve top-level tools to absolute paths through one existing launch seam. |
| M4 | Confirmed missing explicit origin metadata; `../` paths sometimes expose the boundary already. Linked source itself is required product behavior. | Preserve linked analysis, label/report the outside-workspace origin; do not blanket exclude linked files. |
| M7 | Confirmed pipe-liveness failure mechanism; no daemonization reproduction performed. | Pre-tag cancellation-aware bounded pipe handling and completion/drain cleanup in the existing Rust worker transport. |
| M9 | Confirmed host threadpool sharing; admission limits pending queries but does not isolate the host pool or bound workspace waits. | Explicit embedding release decision: retain/document a measured limitation, or move admitted work off the host pool with one binding-local completion path. Do not change host UV settings. |
| M27 | Confirmed; source messages have one bounded frame and can terminate the whole language worker. | Before tag, make the supported size boundary an explicit compilation/artifact coverage gap; chunking is a separate protocol change if required. |
| M28 | Confirmed in both providers, with an additional shared traversal barrier. | Fix stored self-call evidence and terminal callers/callees projection together; provider-only guard change is insufficient. |
| M29 | Confirmed for nested assignment targets and C# coalescing assignment; direct TypeScript `??=` is already recognized. | Fix supported accessor directions, or expose an honest unsupported boundary instead of a false getter call. |
| M31 | Confirmed ancestor search and provenance ambiguity. | Bound repository selection/provenance for required TS; preserve explicit installed/user selections. Swift is experimental and can follow its own backlog. |

These are recommendations for the release triage, not authorization to add a new scheduler, provider engine, configuration language, sandbox or validation framework. The user froze features after the current WAL slice; the work above should close existing correctness, safety and evidence gaps.

## H4: contain panics where the asynchronous call actually runs

[NativeTask::compute](../../bindings/node/native/src/lib.rs) calls the core directly. In the installed dependency, `napi-3.16.1/src/async_work.rs:243-252`, the `unsafe extern "C" execute` callback invokes `.compute()` without an unwind fence. `napi-derive-backend-7.0.0/src/codegen/fn.rs:204-218` shows that `#[napi(catch_unwind)]` surrounds the generated exported function call. For these exports that schedules an `AsyncTask`; it does not wrap the later libuv callback. A core panic can therefore reach a non-unwinding foreign boundary and abort the embedding process. MCP is a separate Rust host, not a Node host; its panic boundaries need their own assessment.

Use an explicit unwind fence inside `Task::compute` around both query and workspace dispatch, returning the existing bounded `internal_error` JSON convention. Protect synchronous exported constructors/methods separately with the supported N-API unwind attribute or the same narrow boundary pattern. Keep the original reservation and task-owned Arc alive through actual completion/resolve; catching a panic must not release a still-running operation or bypass parsing ownership. Do not automatically clear poisoned core locks or claim that catching an unwind repairs state. It also cannot catch aborts, OOM aborts, SIGBUS or invalid C pointers.

Finite evidence: an actual Node child using a test-only faulting Task through the production compute-fence helper survives, receives the bounded error and can make another independent query; a synchronous export panic is also contained; reservations/reader leases are released. Do not add a production panic environment variable or hidden collector.

## H5: the accepted trust boundary

The user's accepted policy is explicit:

- Both auto-discovered and explicitly opened/registered repositories need a separate **personal project-execution grant** before project compiler/toolchain planning, dependency restoration or analysis starts. Open/register is not itself consent.
- The grant is centrally stored and cannot be granted by checked-in `.know-code.json`, project files, dependency metadata or discovery.
- `allow_install_scripts` remains a separate personal permission, default false. Granting compiler/project execution must not silently enable package lifecycle scripts or pnpm hooks.

[TypeScript compilerFor](../../providers/typescript/worker.mjs) resolves and requires project-selected JavaScript before reading `ts.version`. Version validation is compatibility checking, not an execution boundary. [.NET analysis](../../providers/dotnet/Program.cs) selects SDKs, restores projects, loads MSBuild projects and requests source-generated documents. [SdkMetadataServer](../../providers/dotnet/SdkMetadata.cs) explicitly notes that normal InitialTargets/validation hooks can execute. Microsoft's [target ordering documentation](https://learn.microsoft.com/en-us/visualstudio/msbuild/target-build-order) confirms that InitialTargets run before explicitly requested targets; selecting a metadata target does not make evaluation non-executing.

Implement one permission check before the common coordinator analysis/dependency path, including login restore, saves, refresh, discovery, SDK/MCP open and any supported unmanaged analysis entry. Registration and inert status may still work; show a clear trust-required outcome rather than starting workers or granting trust through a retry. Preserve existing graphs honestly stale when analysis is withheld. Revocation must stop active project work and prevent new work; a later explicit permission grant can resume pending work without inventing a package-restore retry. Document what project trust permits, including compiler JavaScript, MSBuild evaluation/tasks and generators. This is user authorization, not a security sandbox for trusted code.

Finite evidence: owned TS module/MSBuild marker projects cannot execute on discover, explicit open, login restore, save or refresh without the grant. Granting project execution starts the intended compiler while npm lifecycle/pnpm hooks remain off. Checked-in grants fail, revocation stops work, and a healthy separately trusted workspace continues. No hostile-source reproduction was run for this assessment.

## H9: isolate project failures without hiding failed facts

The TS worker has one outer catch. Its configuration-planning loop can throw before any valid sibling is analyzed; `compilerFor` and `analyze` can throw later and suppress `Complete`. C# metadata reads, catalog loading and project compilation also escape to the top-level catch. The shared writer correctly preserves the old whole generation when a provider does not complete; it is the provider's failure scope that is too broad.

There are existing narrower paths: TS parse/type diagnostics become a partial report; preparation errors are tracked by install group; C# catches restore errors and emits preparation/report diagnostics. Therefore this finding does not mean every compiler error discards every fact.

Use the existing compilation/report protocol to describe expected project configuration, unsupported compiler and evaluation failures. Emit the failed/unknown compilation context and bounded diagnostics, preserve healthy contexts, and finish the provider only after all planned contexts have an honest outcome. Failed units must replace/remove their prior current facts; do not mix stale old facts into the new partial generation. Keep worker crashes, malformed protocol, failed ingestion, cancellation, resource stops and unknown extraction failures fatal to the staged job.

Planning needs care: a broken explicit TS config must not silently turn its potentially owned files into inferred standalone projects. Failed C# evaluation must not fabricate target frameworks or configuration mappings. Reuse current provider identity/planning helpers and preserve an explicit unknown ownership/target boundary. Do not add independent storage/query logic or a provider-per-language execution engine.

Finite evidence: good plus malformed TS config; good plus unsupported selected compiler; good plus unevaluable C# project; then a save repairing the bad project. Verify healthy current facts, failed-context coverage, removal of old failed-unit facts, no inferred fallback and no fabricated cross-project paths. Verify a broken worker/protocol still preserves the prior coherent generation.

## Restore, tool selection and source boundaries

### M1: scripts-off does not sanitize executable configuration

[preparation.mjs](../../providers/typescript/preparation.mjs) invokes package tools in the install directory with inherited configuration, forcing `--ignore-scripts` and, for pnpm, `--ignore-pnpmfile`. It deliberately preserves other `.npmrc`/workspace settings. npm documents both [project configuration precedence and the configurable git executable](https://docs.npmjs.com/cli/v11/using-npm/config/), and that [git dependencies invoke git](https://docs.npmjs.com/cli/v11/commands/npm/). The specific malicious helper chain remains an inference, not a reproduced exploit.

Keep registry credentials/proxies/workspace fidelity. Force required executable selectors to trusted absolute tools or reject unsupported executable configuration when scripts/hooks are not authorized. Apply the same rule to all supported npm/pnpm versions. Do not replace all repository configuration with a second bespoke package-manager implementation. Test an owned marker helper plus a local git dependency, scripts toggles and ordinary registry/auth settings.

### M2: qualify the integrity claim

[package-tools.mjs](../../providers/typescript/package-tools.mjs) checks exact package identity, official HTTPS archive origin, registry SRI, bounded archive size and optional repository pin hashes. Those checks provide useful integrity relative to the trusted registry/TLS endpoint. They do not independently authenticate a compromised registry/publisher. npm describes a separate [signature verification mechanism](https://docs.npmjs.com/verifying-registry-signatures/); it is not currently implemented here.

Cache hits hash only the entry file; another required JS module can change unnoticed. If stronger cache-corruption detection is a release requirement, record/verify a bounded installed-tree inventory at suitable reuse boundaries. An entry hash plus mutable same-user manifest is not a defense against an attacker already controlling the user's files. Avoid adding a complete new software-supply-chain service during release closure. A malicious registry/cache compromise was not demonstrated by this review.

### M3: remove cwd-dependent executable selection

The installer stores the inherited PATH verbatim after its launcher directory. [Worker launch](../../crates/core/src/update.rs) changes cwd to the workspace while [workspace planning](../../crates/core/src/workspace.rs) requests bare `node`/`dotnet`. Nested C# tools also use bare `dotnet`. Empty, `.` and other relative PATH components can therefore become repository-controlled executable selection.

Use one existing launch/tool-resolution seam: reject or remove empty/relative PATH components, preserve legitimate absolute user tool directories, and pass resolved absolute tool paths to provider launches where practical. The installed login job and manually launched coordinator must agree. Do not globally change the user's shell environment or hard-code a single developer's SDK paths. Test an owned relative-PATH marker beside a legitimate absolute Node/.NET installation.

### M4: label linked/outside source, retain compiler fidelity

TS filters default/external libraries and compiler ownership, but does not require files to be beneath the workspace. It emits `path.relative(root, filename)` and `generated:false`. C# linked documents likewise become relative paths. `SourceArtifact` currently has no explicit linked/outside-workspace origin field. A `../` path signals some cases, while a symlink alias can conceal the canonical boundary.

Linked compiler-selected source is an accepted requirement. Do not blanket reject it. At minimum expose a bounded compilation/source diagnostic describing known outside roots and uncertainty; a structured shared artifact-origin field is preferable when the contract changes. Preserve exact saved text/hash and compilation ownership, and do not recompute old source provenance from a later filesystem target. Project trust must be checked before a compiler reads an untrusted project's selected files.

### M31: distinguish repository pins from inherited environment

TS Node resolution and its fallback manifest search can climb above the registered workspace. Swift `repositoryPins` walks to the filesystem root and later labels selections as repository pins. This is a real provenance ambiguity, not proof every parent configuration is unwanted: monorepo dependencies, installed package-manager stores and personal tool selection can legitimately live outside one registered root.

Reuse bounded workspace selection metadata to distinguish declared repository selection, explicit personal/environment selection, installed cache location and bundled fallback. For an unconfigured TS project, avoid silently treating an unrelated ancestor package as a repository-selected compiler. Do not blanket canonical-path-ban pnpm's installed store. Swift remains an experimental v0.0.3 adapter; correct its pin-origin labeling without expanding v0.0.1 acceptance into Xcode/toolchain redesign.

## Liveness and provider graph correctness

### M7: pipe EOF must not own cancellation

[update.rs](../../crates/core/src/update.rs) reads framed stdout synchronously and drains stderr on a thread. It waits for EOF before completion and joins the stderr thread without a cancellation-aware bound. The resource controller kills owned process groups; an escaped descendant retaining pipe descriptors can prevent EOF after the group dies. Existing process-group protections remain valuable but do not establish a sandbox against daemonization.

Use cancellation-aware finite polling/reads for the same private transport and stop/drain stderr independently of descendant-held EOF. A canceled worker must release the shared slot after owned cleanup; do not kill unrelated escaped processes or accept an incomplete stream as partial success. A fake owned descendant retaining stdout/stderr outside its original process group can prove cancellation/completion liveness without new provider APIs. No such reproduction was run here.

### M9: admission and host-pool isolation solve different problems

The addon still schedules `AsyncTask` on libuv. [Node 24 documentation](https://nodejs.org/download/release/v24.0.0/docs/api/cli.html#uv_threadpool_sizesize) describes the shared default four-thread pool and affected filesystem, crypto, DNS and zlib operations. Four admitted slow queries can occupy it; unadmitted `workspace_json` waits can also queue there. Account-wide admission bounds pending query ownership but does not reserve host-pool capacity.

Do not change `UV_THREADPOOL_SIZE` in an embedding application. If closure is required before tag, use one small binding-owned native execution/completion path with existing admission, cancellation and parse ownership, and no Know Code queue. Status/cancel/wait require their own bounded liveness treatment rather than taking query slots. Avoid introducing Tokio or an additional service just to wrap the same core query. Validate actual fs/crypto progress and environment shutdown while admitted queries/control waits are slow; no timing experiment was performed here.

### M27: the frame boundary is real and smaller than a raw 8 MiB source

[Node framing](../../providers/shared/protocol.mjs) and [.NET Wire](../../providers/dotnet/Program.cs) encode one `SourceArtifact` per envelope and reject encoded messages above 8 MiB. Payload metadata consumes part of that budget. Both providers send the full source in that one message. A large artifact currently fails the worker, including healthy siblings, rather than yielding a scoped gap.

The smallest bounded correction is to measure the encoded artifact before emission, skip its unsupported body/facts explicitly and report path/size/completeness without inventing empty source, hashes or valid ranges. Coordinate with H9 so healthy contexts publish and references to omitted bodies stay explicit external/unknown boundaries. If complete large-file support is required, add bounded chunks through the common worker protocol in a separately reviewed change; do not silently raise all frame caps or create per-language storage engines. Test just-under/over encoded limits and a healthy sibling, including generated-source provenance.

### M28: recursion needs a shared terminal-path correction too

Both provider relation helpers return whenever source ID equals target ID, dropping compiler-proven recursive calls as well as containment. The suggested containment-only condition repairs emitted facts. However, [the shared traversal](../../crates/core/src/graph.rs) then rejects any `next` already contained in the path before constructing a result step; the visit cap also prevents an ordinary revisit of the root.

Preserve the self-call fact, project a bounded terminal self edge for callers/callees, and do not recursively expand it. Keep impact's changed-root behavior explicit; it need not adopt recursive result semantics accidentally. Add TS and C# direct recursion plus mutual recursion checks at default and small work/path limits across the common query operator. Do not merely assert that a row exists in storage and call the public API fixed.

### M29: distinguish the verified forms

C# only recognizes direct `ISimpleAssignmentOperation`, compound assignment and increment/decrement parents. Coalescing assignment and nested tuple/deconstruction targets fall through to the read path. TS recognizes a direct property-access left side across assignment-token kinds, so direct `obj.p ??= value` already produces getter and setter; its tuple/destructuring and element-access forms remain incomplete. The blanket claim that TS `??=` only produces a getter is not supported by this code.

Use compiler operation/AST ownership to identify supported nested write targets, preserving conditional/implicit evidence. Unsupported writes should not become false exact getter calls. Reuse the existing narrow graph adapters; no second lowering engine. Cover direct assignment, compound, increment, `??=`, tuple/destructuring and bracketed accessor targets, distinguishing expected reads/writes and compiler-backed versus unsupported evidence.

## Related low findings

| ID | Assessment and bounded disposition |
| --- | --- |
| L1 | mmap health/live truncation can fault outside Rust unwind handling; source confirms the mapping/length-check pattern. Normal code keeps these fixed-size owned pages, so this is a same-user/external-corruption fault-model limitation, not a demonstrated default-use exploit. Document it; replacing shared-page synchronization is not a cheap panic catch. |
| L2 | `socket` followed by `fcntl(FD_CLOEXEC)` leaves a fork/exec window. Confirmed source pattern; use an available platform atomic creation primitive when supported, without claiming that a later fcntl is atomic. No fork reproduction here. |
| L3 | .NET UTF-8 P/Invoke uses NUL-terminated strings, so embedded NUL in database/scope/raw JSON can change the native meaning; replacement of invalid UTF-16 also changes path identity. Reject unsupported path/raw-request encodings at that binding boundary. This is semantic validation, not evidence of native memory unsafety. |
| L4 | Header include/extern-C guards are absent. Confirmed low-cost packaging/API fix; no ABI change required. |
| L5 | TS setup uses unbounded synchronous reads of selected config/hook paths even when hooks are disabled. A nonregular/unbounded input can consume the worker until resource stop. Add one narrow regular-file, bounded-open/read helper for setup metadata; preserve permitted linked files and report a setup diagnostic. No device-file reproduction was run. |
| L6 | The broad node-reuse claim is qualified: `Control::spawn` already sets `MSBUILDDISABLENODEREUSE=1` and `DOTNET_CLI_USE_MSBUILD_SERVER=0`, inherited by nested tools, and SDK metadata explicitly disables node reuse. Missing redundant flags inside Tools.Restore are not proof the managed default leaks servers. Workload-advertising suppression remains a small separate hardening item, not a demonstrated network failure. |
| L26 | Shared JSON-RPC transport repeatedly concatenates buffered chunks and ignores write backpressure. Confirmed bounded-frame inefficiency/queued-memory issue, currently used by experimental Rust/Swift adapters. Keep a measured backlog item; reuse that one transport rather than adding provider-specific replacements. |
| L27 | Any absolute-path segment named `obj` sets generated=true; legitimate ancestors/linked files can be mislabeled. Prefer evaluated intermediate directories and actual generated-document evidence. |
| L28 | C# diagnostics retain absolute paths and truncation counts include severities not emitted. Confirmed consistency/counting issue; normalize with preserved outside-root provenance and count the filtered diagnostic set. |
| L29 | TS/C# omit unresolved name references without an aggregate diagnostic; Rust reports a count. Compiler errors/partial graph capabilities already expose some incompleteness, so do not equate this with claiming complete resolution. Count meaningful bindable unresolved uses, preserve uncertainty, and do not fabricate symbol edges. |
| L30 | C# operation-root selection does not include primary-constructor base syntax; its own base-constructor and argument calls can be absent. Confirmed narrow adapter gap. Add a compiler-operation fixture and an honest capability boundary or a small supported-root extension. |

## Finite validation and source evidence

Run only the focused cases listed above during implementation, followed by the selected integrated release checkpoint. Nothing in this document reports those tests as executed. H4/H5/H9, restore execution selectors, and pipe cancellation should be closed as explicit release gates; recursion/accessor corrections need public-query evidence. Integrity provenance, host-pool isolation, outside-root structured metadata and experimental-provider improvements need an explicit release disposition rather than silently becoming new engines.

The original report's suggestions should not be applied mechanically: a N-API attribute is not an async compute fence; provider self-edge changes are not a complete recursion API fix; package hashes from a trusted HTTPS registry are useful but are not independent signatures; managed MSBuild reuse is already centrally disabled.

Observed main revision: `50cfb93e8f0388e9e62d8d35721483895dae05ab`. Review date: 2026-11-11T20:28:02.941293+00:00

| Inspected source | SHA-256 |
| --- | --- |
| `CODEBASE-REVIEW.md` | `4481d3b33b7476807f0fa58405b29f1a739c34eb0b8a647f717d03d569da81e2` |
| `bindings/node/native/src/lib.rs` | `6437de5838e637ff02a351ad9b508ef2131c6da59e06887d22ebb774c803de49` |
| `bindings/ffi/src/lib.rs` | `d31f65dba4c717a8c1c23b3aff25c9f35f88f22411c56026f02b1872099cd017` |
| `bindings/ffi/know_code.h` | `c9a7cb48d8a1ec1d504770b0bca4e07e20e342c1514062dc2a898a8e361315c2` |
| `bindings/dotnet/KnowCode/Index.cs` | `41b3cff0b130c6fee882e15fe557f6ed9d04d4003b8c5d715b64ed8573fa5c7a` |
| `providers/typescript/worker.mjs` | `f8448d59b32dbe7d2c320d7d12fcd26154ef820d1e56befd6219e1dcc7315315` |
| `providers/typescript/preparation.mjs` | `211b42783f04331141074728a1c3054b43f5d67de6eba37f43fdc33aac429237` |
| `providers/typescript/package-tools.mjs` | `8cc3999c288b04f507c4503db98c2bace7befd4b3cd2b7e8a6cc2d7a5f470ae7` |
| `providers/typescript/analysis.mjs` | `082a7f56d08ca5901c51c06a7cb6c3da7db67163af067853741d6ba2a9821e95` |
| `providers/typescript/project-links.mjs` | `28490357cbbfc8e16254fa5c7229e3e87d2392e603f097982c6d973c0dc5f1dc` |
| `providers/typescript/graph.mjs` | `2ff91e0edb705fb63945c5e58d199e63266a1d63b00472bddfe3429b19c8f616` |
| `providers/dotnet/Program.cs` | `cf1e6173873ace4595ea1f3de475bb2459cad52d4ef97d6ea9b0e27ae313a44a` |
| `providers/dotnet/SdkMetadata.cs` | `61c9e38a6e922d164ba70d4b4d4e0b6191c5011f43fffe3f158ef988d842e412` |
| `providers/dotnet/Graph.cs` | `c11649b0b13bd5df242c2c90b7aeb3788a55c135d596f967eb27de33993121a8` |
| `providers/shared/protocol.mjs` | `f5e0a70a5edab05cac3fad44949e75c7f28deab7326e8bce3842adea1b761d6e` |
| `providers/shared/jsonrpc.mjs` | `92a728fc4ca48f22c37f9849bfdefdd5e5acbee35618af46333d8c3fb6095f9f` |
| `providers/swift/toolchain.mjs` | `0ac606cfca6396842d72159b479081d7848b09e49019b3a4cc4b414bb31ce624` |
| `crates/core/src/update.rs` | `ae6c97832b9d5d915daf1891b3ccb8254a5af2b037725e8524e2619912a982dc` |
| `crates/core/src/resource.rs` | `34523697b984331b8440ba5523db0ce75a95211c462cc757c748ee20ae7fd2da` |
| `crates/core/src/graph.rs` | `1bc0b345fdd81492efe452a0a0769a5ed656cc9ecc5a64577566682774bf74a8` |
| `crates/core/src/settings.rs` | `0f808f89c1db5dea6a2dc23ca5ac859f76bc772cfea0e03c908e89d1bde6f3f2` |
| `crates/host/src/install.rs` | `211d3654132ed23d4db6c5b5876c2c81882afecc8d96c9b14d4e7fbd85545c0d` |

Dependency-source evidence: cached `napi 3.16.1` (`src/async_work.rs`) and `napi-derive-backend 7.0.0` (`src/codegen/fn.rs`) matching Cargo.lock; inspected read-only. Official references above were checked during this assessment.
