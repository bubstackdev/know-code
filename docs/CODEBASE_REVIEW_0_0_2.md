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

Initial source rechecks confirm numerous high findings were repaired before 0.0.1. Remaining work is being fixed in small lane-owned commits; implementation claims and passing results will be recorded after integration. No 0.0.2 package or release has been published.

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

The original 12 architecture recommendations are not a mandate to replace working subsystems. Narrow coordinator seams and genuinely shared provider/test helpers are being extracted where exercised. AGENTS.md now contains standing rules/pointers; complete accepted constraints and verbatim historical checkpoints are preserved in PRODUCT_CONSTRAINTS.md and AGENT_HISTORY.md. Leading implementation/release status is reconciled with the actual shipped artifact; source/runtime constants are authoritative. Existing standalone classification checks already run in scripts/test.sh. Capability consolidation, durable registry evolution, legacy public-ABI removal, broader operation enums and dependency upgrades require a concrete mismatch/reuse/upgrade/measurement case rather than a generic framework. No public native entry points are removed in this wave.

## Validation so far

The root's initial focused MCP transport run passed all12 existing bounded ownership/output/EOF/duplicate-ID/cancellation cases. First-wave lightweight tests passed C/C++ header compilation, shared JSON-RPC buffering/backpressure, shared source helpers, bounded ordered Rust navigation and Swift toolchain provenance. Receipt: `.artifacts/snow-leopard/wave-01/node-and-header-01/summary.json`. Source changes after those runs still need matching assembled verification; no full 0.0.2 suite/package/encoding certification is claimed.

## Compact query status decision

On 2026-11-12 the user accepted compact query freshness/status metadata, with full workspace configuration available through workspace_status. Compiler coverage and graph evidence remain in query results. This is being implemented through the common Rust result/page path, including bounded live metadata and page headroom, so a valid maximum-size page cannot become unreadable merely because live status grows. Native/cursor compatibility must explicitly reject incompatible page contracts; this does not change worker framing or stored facts.
