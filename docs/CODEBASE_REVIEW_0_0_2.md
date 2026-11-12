# 0.0.2 stabilization and codebase-review closure

The user selected correctness, performance, reliability and simplification only for 0.0.2 on 2026-11-12. Previously planned new features move to later releases with timing undecided. The original [CODEBASE-REVIEW.md](../CODEBASE-REVIEW.md) is preserved as an audit input; its severity labels and proposed fixes are not accepted blindly.

Baseline is published v0.0.1, `e20da5db9f359311b0bc6b2be30ff9db94ac99c3`. The earlier [triage](CODEBASE_REVIEW_TRIAGE.md) records decisions made before tagging; many required fixes subsequently landed. Each finding now needs an explicit current disposition: fixed before this release, fixed in this stabilization work with evidence, accepted behavior/not a defect, or a concrete remaining issue. Source inspection is not test execution.

## Parallel ownership and validation

- Runtime/cursor lane: coordinator state, cancellation/completion, resource locking, cursor ownership and liveness. [Per-finding ledger](reviews/CODEBASE_REVIEW_0_0_2_RUNTIME.md).
- Storage/query lane: indexed cleanup/replacement, bounded graph work/projection, repeated SQL/blob costs and recovery. [Per-finding ledger](reviews/CODEBASE_REVIEW_0_0_2_STORAGE_QUERY.md).
- Providers/native lane: compiler facts/provenance, package/tool selection, shared transport, native boundary safety. [Per-finding ledger](reviews/CODEBASE_REVIEW_0_0_2_PROVIDERS_NATIVE.md).
- Integrator: MCP/CLI, standing instructions/current documentation, cross-lane integration and shared focused verification.

Agents use isolated durable worktrees. One integrator owns heavy compiler/native builds and integration tests; focused regressions run while iterating, and full Debug/optimized suites wait for a settled integration/release checkpoint. Do not change accepted API/trust/graph/budget behavior merely to match a report recommendation. Do not add a second engine or a broad abstraction/validation framework.

## Integration status

Targeted lane changes are integrated, with source-bound focused results below. A static inventory accounts for all 72 original findings exactly once in the root/lane disposition tables; this is review coverage, not a claim that every residual risk or performance question is fixed. Full current Debug/optimized suites and final package/encoding certification remain outstanding. No 0.0.2 package or release has been published.

## Deferred contention benchmark

The user explicitly deferred the real video-encoding gate for 0.0.1. The final e20 candidate's eight current lookup cases, native clients and 59/59 compiler-complete Polly index were prepared; no real-encoding matrix/progress/interference result was measured. Retain that distinction and revisit when normal encoders are available. Local receipts: `.artifacts/pretag-wave/final/lookup-03/` and `.artifacts/pretag-wave/publication/encoding-deferred-2026-11-12.json`.

## MCP/CLI and documentation dispositions

| IDs | Current disposition |
| --- | --- |
| H1, M11 | Accepted bounded transport behavior: four pending responses/two owned frames; exhaustion closes the connection and cancels its queries. Do not promise an extra error frame without output ownership or add an unbounded queue. |
| M10 | Structured/text duplication is the MCP SDK representation; the accepted 8 MiB whole-frame ceiling counts both copies, escaping and framing. No separate query engine or silent evidence stripping. |
| M12 | Changed: tool arguments are checked against that exact tool's advertised property keys before mapping to the common operator. Known options from other tools no longer disappear silently. Names in validation errors are bounded. Real-wire regression covers ignored-option cases and subsequent session health. |
| M13 | Snapshot-required coverage schema was fixed before 0.0.1 and has executable-minimum tests. The 64 KiB input-line codec bound and finite oversized-input behavior remain explicit; no fabricated response ID is promised for an undecodable oversized frame. |
| L22 | Changed: source/context/coverage/graph byte budgets now have distinct descriptions; total finding cap and implicit/possible-target behavior are described. Focused tools plus query_graph remain the accepted hybrid API. Native operators/errors stay shared. |
| L23 | Changed: query/queries retain structured JSON stdout and return exit1 for an error envelope, success0 otherwise. The CLI inspects the envelope without materializing its result payload again. Existing cross-binding fixtures share one exit assertion; native clients retain their host conventions. |
| L24 | Changed: --version/--help/-h work without HOME/account initialization; help includes all commands; numeric parse errors identify flags and repeated singleton options/aliases are rejected before execution. |
| L25 | Changed: bounded filesystem admission runs off the single-threaded stdio runtime. The admission task retains its local permit after cancellation; the shared account reservation still precedes query serialization/dispatch and remains owned through actual work/output. |

The original 12 architecture recommendations are not a mandate to replace working subsystems. Narrow coordinator seams and genuinely shared provider/test helpers are being extracted where exercised. AGENTS.md now contains standing rules/pointers; complete accepted constraints and verbatim historical checkpoints are preserved in PRODUCT_CONSTRAINTS.md and AGENT_HISTORY.md. Leading implementation/release status is reconciled with the actual shipped artifact; source/runtime constants are authoritative. Existing standalone classification checks already run in scripts/test.sh. Capability consolidation, further durable registry evolution, legacy public-ABI removal, broader operation enums and dependency upgrades require a concrete mismatch/reuse/upgrade/measurement case rather than a generic framework. No public native entry points are removed in this wave.

| Architecture item | Explicit disposition / evidence boundary |
| --- | --- |
| 1, capability checks | Retain operation-specific safety checks; no newly unsafe peer acceptance demonstrated. [Boundary audit](reviews/CODEBASE_REVIEW_0_0_2_ARCHITECTURE.md). |
| 2, version constants | Separate contracts remain; actual released8/current9 Node/.NET mismatch/control matrix passed eight cases. Generating repeated constants remains maintenance work. [Audit and packet](reviews/CODEBASE_REVIEW_0_0_2_ARCHITECTURE.md). |
| 3, coordinator structure | Extracted tested wait/job/cancel/registration seams; scheduler/Entry replacement is not implemented or required by this review. [Runtime scope](reviews/CODEBASE_REVIEW_0_0_2_RUNTIME.md). |
| 4, durable registry | Added one host registry boundary and version1 for the existing schema, with additive legacy migration and refusal of unknown formats. Six focused registry cases, including actual hot-journal recovery, passed; later guard changes still need the settled full suite. [Runtime ledger](reviews/CODEBASE_REVIEW_0_0_2_RUNTIME.md). |
| 5, recovery decomposition | Retained current lifecycle/disk-accounting ownership; no concrete reuse or measured coupling justifies a new module/handle rewrite. [Storage boundaries](reviews/CODEBASE_REVIEW_0_0_2_STORAGE_QUERY.md). |
| 6, legacy native APIs | Retained shared wrappers; public API removal requires a product decision. [Architecture audit](reviews/CODEBASE_REVIEW_0_0_2_ARCHITECTURE.md). |
| 7, operations/constants/schemas | Corrected actual MCP/CLI validation boundaries; retained finite shared operators. Broad enum/schema generation remains a maintenance option, not a claimed fix. [Architecture audit](reviews/CODEBASE_REVIEW_0_0_2_ARCHITECTURE.md). |
| 8, standing instructions | Split standing AGENTS rules from preserved PRODUCT_CONSTRAINTS and dated AGENT_HISTORY. No accepted constraints deleted. |
| 9, documentation drift | Current status and contract authority corrected; dated checkpoint numbers remain historical evidence. This is not a claim that every historical paragraph has been rewritten. |
| 10, test harness | Shared CLI exit assertions and focused deterministic barriers added; standalone classification checks were already in the suite. Fixed-sleep negative integration assertions still exist; no blanket flakiness rewrite/closure claimed. |
| 11, provider duplication | Shared source paths/UTF offsets and JSON-RPC transport where actually reused; no general batching/diagnostic framework. [Provider ledger](reviews/CODEBASE_REVIEW_0_0_2_PROVIDERS_NATIVE.md). |
| 12, dependency duplication | Duplicate hash/macro dependency versions remain; no measured correctness/size/latency defect warrants forced upgrades. [Dependency audit](reviews/CODEBASE_REVIEW_0_0_2_ARCHITECTURE.md). |

## Validation so far

The root's initial focused MCP transport run passed all12 existing bounded ownership/output/EOF/duplicate-ID/cancellation cases. First-wave lightweight tests passed C/C++ header compilation, shared JSON-RPC buffering/backpressure, shared source helpers, bounded ordered Rust navigation and Swift toolchain provenance. Receipt: `.artifacts/snow-leopard/wave-01/node-and-header-01/summary.json`. Source changes after those runs still need matching assembled verification; no full 0.0.2 suite/package/encoding certification is claimed.

## Compact query status decision

On 2026-11-12 the user accepted compact query freshness/status metadata, with full workspace configuration available through workspace_status. Compiler coverage and graph evidence remain in query results. The common Rust result/page path implements bounded live metadata and calculated page headroom; the maximum-2-MiB atomic-page growth regression passed. Query9/cursor3 compatibility explicitly rejects incompatible page contracts. Worker framing and stored facts do not change.

## First assembled checkpoint

Source `9b23c25691fccd89edb37a590514754c4e4e3429` passes all266 Rust workspace tests (two explicit manual/coordinator-supplied probes remain ignored) and matching Debug build. Focused compiler/native source parity, primary-constructor call evidence, all-four-API compact response/cursor checks, cursor lifecycle, MCP37, query admission/cancellation, dependency preparation, experimental Rust navigation, coordinator and resources pass. Node/header/source/navigation/Swift-toolchain and one pinned npm cache-tamper/offline cell also pass. Exact command/source receipts are under `.artifacts/snow-leopard/wave-01/`; this is not final Debug/optimized/package certification for later edits.

Preserved failed attempts include the first C# local-pattern name collision (corrected), an operator test run before resolving a cherry-pick conflict, two Rust fixture assertions (corrected without weakening artifact validation), and outdated end-to-end/MCP assertions for intentionally changed CLI/tool errors. Passing focused reruns do not relabel those original attempts as green.

Later focused receipts passed: `registry-02` (six cases, including actual hot journal), `mask-retirement-01`, `fmt-01`, `build-03` and `query-admission-02` under `.artifacts/snow-leopard/wave-01/`; the actual eight-case released/current Node/.NET matrix passed in `previous-native-01` and `.artifacts/snow-leopard/previous-native-contract-01/run-01/result.json`. The raw managed request guard also passed root's focused validation. Receipts retain exact source/diff/producer identities: in particular build03 and the native matrix bind `3afac3b`, while registry02/mask-retirement01 predate later guards. At `995e747`, L7 core11/execution10/coordinator24 focused cases and strict `clippy-03` passed. Subsequent independent review identified actual-HOME and ACL gaps; their narrow L7 follow-up and final matching checks remain pending. These focused results do not certify the later settled source, full Debug/optimized suites, packages or real video encoding.
