# Pre-tag assessment of CODEBASE-REVIEW.md

Status: source-checked assessment complete; fixes and focused reproduction/validation gates remain open. The review ran while WAL code was changing, so function/source evidence takes precedence over its line numbers and severity labels. The input report is preserved unchanged.

The v0.0.1 feature freeze permits fixes to accepted release behavior. It does not require implementing every recommendation in the report or starting the proposed architecture rewrites.

## Pre-tag priorities

| Area | Findings | Required outcome |
| --- | --- | --- |
| Panic and job completion safety | H2, H3, H4, H7 | A wait/unregister race and a missing cursor during BAD cleanup must return errors rather than poison shared state. Every analysis task must settle scheduler ownership on panic. Node must contain unwinding in the actual native task as well as exported entry points. |
| Workspace-local cursor failures | H8; related M18 | A failed result unlink/pin release remains charged and visible without needlessly stopping unrelated workspace publication. Unknown debt must never be silently treated as zero. Add a finite sibling-progress regression with available headroom. |
| Cleanup complexity | H6, M14, M15 | Add the missing child/cleanup indexes through one compatible setup path and check actual query plans/representative reclamation. The report's 78 s and 4.9 s measurements have not been independently rerun here. |
| Bounded graph semantics and projection | H10, H11, L21; related L19 | Do not expand and charge adjacency beyond the requested depth. Apply relationship-selection predicates before row limits without regressing indexed streaming plans. Bound symbol-search declaration materialization before projection accounting, with explicit limited evidence. Preserve honest work-limit evidence and compilation compatibility. |
| Project-local failure scope | H9; related M27 | The user now explicitly requires healthy projects to publish fresh partial results when another project has an expected configuration/evaluation failure. Failed contexts must be unavailable with diagnostics, with no stale facts merged into the current partial graph. Make oversized-artifact limitations explicit; complete chunking is not automatically required. |
| Compiler graph facts | M28; M29 needs finite form verification | Return direct self-recursive calls with their compiler/source evidence without expanding a traversal cycle. Verify write/read accessor evidence for the named assignment forms; do not emit a getter call that the compiler operation does not imply. |
| Repository execution trust | H5; audit M1/M3 | Enforce the newly accepted personal trust gate before automatic compiler/dependency execution. Separately preserve package-script default-off behavior and check executable-config/PATH bypasses. |
| Deadline and cancellation liveness | M5/M19, M7; gate M9 | Cursor lock waiting and CPU throttling must not defeat the accepted request deadline or block every workspace indefinitely. A worker descendant holding inherited pipes must not make accepted cancellation permanently retain the analysis slot. Gate actual Node filesystem/crypto progress under slow query/control work before accepting the shared-libuv-pool limitation. Use finite owned fault cases to establish the exact required fixes. |
| Advertised managed-query schema | M13 coverage schema | The MCP coverage tool must advertise the snapshot guard that its managed core request requires. Its advertised minimum must execute successfully. This needs a small schema correction, not another validation layer. |

This table is a priority set, not a statement that the fixes or all failure reproductions have run. The linked assessments distinguish confirmed source defects, focused reproduction gates, accepted behavior and backlog work.

## Important corrections to the report

- **H1/M11:** closing a connection at the finite MCP response/output ownership ceiling was explicitly accepted. Do not replace it with an unbounded queue or promise one more error frame when no bounded response ownership is available. Normal global query-slot saturation still returns typed `busy` where transport capacity exists.
- **H4's proposed fix is incomplete:** `#[napi(catch_unwind)]` protects an exported wrapper, but cached napi 3.16.1 calls `Task::compute` later from its own `extern "C"` async-work callback without a catch. The task body itself needs containment; test actual host survival and reservation release.
- **M16:** the report observed an intermediate WAL implementation. Current code settles abandoned staging WAL before recording the active-job resume baseline, with distinct preflight deferral and cancellation handoff handling. Those changes require the prepared managed WAL tests; source inspection alone does not certify them.
- **M22:** separate graph materialization and complete-response/page budgets are accepted. A larger bounded graph can be paged. A nonpaged request may fail with the documented typed response-size error; matching those defaults is not a new release requirement.
- **H9:** the prior documented behavior preserved the whole graph as stale for setup failures, distinct from ordinary compiler diagnostics. During this assessment the user explicitly selected healthy-project publication with failed-project unavailable coverage for v0.0.1. That decision now makes expected project-local failure isolation a pre-tag requirement; do not rewrite history as though this finer setup-failure boundary was already fully implemented or proved.
- **M26:** SQL may scan/sort matching groups, but Rust collects only the bounded grouping keys. Treat the actual plan and latency as evidence rather than claiming all symbols are materialized in Rust.
- **M29:** direct TypeScript `??=` already records getter and setter; the blanket TS claim is overstated. C# coalescing and nested write targets still need correction/explicit unsupported evidence.
- **M2/L6:** registry SRI provides useful integrity relative to the HTTPS registry, although not independent provenance. Managed worker children already inherit disabled MSBuild node reuse/build-server settings. Do not count these as entirely missing protections. Keep the final cache/offline/prerequisite gates.
- **M4/M31:** linked sources and external package-store locations are supported. Preserve them; check honest compiler/source origin and selected-toolchain behavior without imposing a new blanket ban on all paths above the workspace. Swift provenance/performance remains experimental scope.
- **M13 input/M20:** oversized MCP lines and frozen-page metadata growth have documented finite failure behavior and existing tests. Correct the managed coverage schema, document transport limits and maximum-page restart, and retain focused boundary gates. Do not infer wrong-generation data or unbounded allocation from an explicit budget error.

## Accepted trust decision — 2026-11-11

The user selected: **all repositories require a separate personal trust grant before compiler analysis or dependency setup can execute repository-controlled code; explicit opening/registration is not itself a grant**. This applies to auto-discovered and explicitly registered/opened workspaces. The grant is separate from permission to run package lifecycle scripts/hooks; a checked-in file cannot grant personal trust. Implementation, persistence/identity binding, blocked status and revocation details still need a narrow design and tests. Do not claim the grant is implemented.

The source confirms repository-selected TypeScript is loaded before its version is checked, and C# metadata/restoration invokes MSBuild logic. Microsoft explicitly treats restore and project loading as execution of trusted build logic; see [Secure MSBuild usage](https://learn.microsoft.com/en-us/visualstudio/msbuild/msbuild-security-best-practices?view=visualstudio). Default-off install scripts are not a sandbox for compiler/toolchain evaluation.

The shared coordinator must enforce this boundary across all entry points. Include a real prior-runtime compatibility gate so a new SDK cannot silently use a pre-trust coordinator and claim the new policy. Existing stored data/query access and future compiler execution are distinct; do not erase indexes as a shortcut for withholding execution permission.

## Accepted project-failure decision — 2026-11-11

The user selected **publish healthy projects as partial** when one project has a broken configuration. Catch expected configuration, unsupported-selection and project-evaluation failures at their actual project/compilation boundary, emit unavailable coverage with bounded diagnostics, remove/replace that failed context's old current facts, and continue healthy contexts. A failed dependency must remain an explicit proof/body boundary rather than linking a fresh caller to stale definitions.

This does not authorize publishing an interrupted stream or treating worker crashes, malformed protocol, storage faults and shared resource termination as successful partial analysis. Preserve the coherent prior generation for those job-level failures. Do not silently infer around a broken explicit config or substitute an unsupported selected compiler.

## Scope and evidence

Read-only lane assessments retain per-finding source evidence and disposition:

- [Runtime, cursor and MCP assessment](reviews/CODEBASE_REVIEW_RUNTIME.md).
- [Storage and graph assessment](reviews/CODEBASE_REVIEW_STORAGE_GRAPH.md).
- [Provider and native boundary assessment](reviews/CODEBASE_REVIEW_PROVIDERS_NATIVE.md).

[Initial source snapshot](../.artifacts/codebase-review/source-snapshot-01.json) records input/source hashes and the working tree. No exploit, fault workload, reported performance figure or complete release suite has been independently reproduced by this assessment. Existing source-bound test packets remain evidence only for their recorded cases. The current unsigned development artifacts are not tag-ready on this report alone.

Broad coordinator/registry/capability rewrites, replacing every string operation with an enum, changing the test harness framework, dependency deduplication and experimental Swift/Rust optimization belong after the correctness fixes unless a concrete accepted release gate requires a narrow change. Correct stale public contract documentation before tagging; documentation cleanup does not require implementing a new architecture.

## Architecture recommendations

| Report item | Tag disposition |
| --- | --- |
| 1–2: capability consolidation/generated version constants | Verify actual native/SDK/coordinator mismatch behavior on final artifacts. Thin Workspace wrappers delegate control-version checks to the shared Rust client; the absence of a second managed/JS comparison does not prove no handshake. Centralizing checks/constants is useful follow-up if no concrete mismatch case is missed. Do not introduce a capability framework merely to remove repetition before this tag. |
| 3: coordinator decomposition | Fix the named state/ownership races and test their transitions now. Splitting a large module alone does not establish correctness; a registry/scheduler rewrite would expand this release's risk. Extract later around concrete tested seams. |
| 4–5: registry schema/disk accounting modules | Document the actual supported upgrade direction and preserve strong registry durability. A schema ledger is needed before incompatible durable evolution; current additive migration checks are not by themselves data-loss evidence. WAL accounting intentionally reuses incarnation ownership and should not become a second engine. |
| 6: legacy native entry points | Backlog. Removing them is an API choice, not a remedy for the confirmed panic/ownership defects. Current reserved paths and legacy wrappers still need the relevant admission/cancellation guarantees. |
| 7: string operations/constants/schema generation | Backlog unless a specific mismatch is demonstrated. Fix M13's incorrect schema directly; do not rebuild the query language or all schemas. |
| 8–9: instructions and documentation | Reconcile current-looking store/query/lifecycle numbers and pending-feature claims before tag. Keep standing instructions short when consolidating historical status, but preserve accepted decisions and source-bound evidence. This is documentation maintenance, not an architecture gate. |
| 10: harness | Keep one heavy validation owner and focused fault regressions. Confirm the two standalone classification scripts are represented in final candidate evidence; they are not invoked by the current full test script. Add existing commands where coverage is missing. A new harness framework is not required. Replace a timing assertion only when its actual race can be expressed through a stable observed boundary. |
| 11–12: provider/dependency duplication | Backlog. Share genuinely reusable adapters when touched; Rust/Swift optimization remains experimental. Host-only duplicate dependency versions do not violate the small embedded-core surface. |

## Validation order

1. Fix confirmed source-level blockers with targeted deterministic regressions, retaining the current WAL and shared admission contracts. Trust, runtime/cursor safety, storage indexes and graph correctness can be independent edit lanes, with one owner integrating shared files.
2. Resolve focused liveness/accounting/large-input gates against a matching assembled build. Do not invent a new performance SLO from the report's severity or diagnostic timing claims. Known debt stays charged; unsupported forms and failed setup stay explicit.
3. Run the prepared managed WAL fixture and affected compiler/native/cursor/MCP checks. No final-suite run against this pre-fix source would certify the remaining defects away.
4. Freeze source and run the existing final Debug/optimized, installed/offline/toolchain, real-encoding/resource, conditional minimum-OS, license/zero-telemetry and signing/distribution gates. Retain failures and bind the final evidence to the actual artifact.
