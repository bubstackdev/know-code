# Graph declaration source projection

Status: implemented development slice with focused combined Debug validation. This is the Q06 slice, not whole-release acceptance. Core implementation and thin SDK/MCP support share native query contract 6.

## Behavior

Callers, callees and impact (including structured graph queries) remain compact by default. `include_source: true` requests declaration context on returned findings and their exact supporting targets. Root pointers and path edges retain their existing locations; they do not receive source bodies. Symbol context retains its existing default of including source. Explicit false omits source in either operation.

Graph `source_max_bytes` defaults to 16 KiB and has a 256 KiB maximum. It bounds the aggregate UTF-8 source text across the complete result, all targets and every serialized copy, including a representative repeated as its first support. It is separate from graph `max_bytes`, which continues to bound graph JSON. Source exhaustion clips or omits bodies and marks that source limitation; it does not stop traversal or remove otherwise-returned findings. Existing graph, response, deadline and cancellation limits still apply and remain explicit.

Each finding and concrete support may carry optional `context: { declarations, boundary? }`, reusing `DeclarationContext` and its existing source statuses. Omitted/false graph source requests omit `context` entirely. A concrete member projects at most 16 declaration anchors; omitted anchors are a declaration limit, distinct from source exhaustion. Graph `limit` remains the finding cap. Symbol context retains its existing logical declaration-slot limit across equivalent groups.

## Shared implementation boundaries

Extract one concrete-symbol declaration projector from symbol context. It accepts an explicit declaration allowance and shared source budget; graph projection must not call the whole symbol-context operation or expand additional stored peers. Sort concrete graph members before projection and account for representative duplication before reading source. Preserve graph symbol locations and exact support paths.

Use only compiler-produced definition extents belonging to the concrete member in the pinned read view. A source read must select served-generation membership plus exact owner generation, compilation, artifact and content hash before selecting a row. Hash equality alone does not establish origin. Missing or ambiguous extents retain their existing labels; no current-disk, name/signature or opaque-ID parsing fallback is allowed. Storage/integrity failures remain query failures rather than body-unavailable labels.

Source projection uses the same overall deadline, cancellation and health guards as the query. Graph `time_budget_ms` remains the traversal budget; it is not a separate promise about enrichment duration. The full response ceiling also bounds projection metadata. Requested source counts toward graph JSON and complete-response limits.

## Pagination and compatibility

Materialize once using one pinned generation and one aggregate source budget. Nest context in each whole finding/support row so pagination cannot separate bodies from their evidence. Continuations and retries neither refill source budgets nor switch source versions. Oversized atomic rows fail explicitly under the existing page limit.

Advance the native query contract from 5 to 6: contract-5 kernels silently ignore graph source requests. ABI 1, store 8, worker/control framing 7, lifecycle 2, resource 3 and cursor 2 remain unchanged. Thin SDKs forward optional values; MCP graph schemas default source off while symbol context defaults it on. Validate external source-budget fields once in the shared request boundary; unsupported operations must not silently accept the new graph-only field.

## Finite validation plan

- Shared Rust tests: default/explicit projection, exact owner/hash selection, unique/missing/ambiguous extents, declaration versus source limits, UTF-8 and duplicate-copy budgeting, graph limits and preserved traversal/findings.
- Native/CLI/MCP parity on saved TypeScript and C# facts, including supporting targets and compiler-selected project bodies; no source on roots/paths or untraversed peers.
- Cursor replay and pinned source across saved publication, aggregate budget across pages, atomic oversized rows, response bounds, cancellation/deadlines and contract-5 rejection.
- Preserve symbol-context defaults, source statuses, logical slot behavior and existing graph evidence. Run focused checks during development, then one frozen combined checkpoint after integration. Earlier upgrade evidence remains historical for changed inputs.

## Combined checkpoint — 2026-11-11

The isolated checkout `.artifacts/gp` builds source `bb61cf4d2423c63be486adcaffe115152eb590fc`, producer `5fb84589b15c4b2caabb1a109f3bf6b5`. The [combined packet](../.artifacts/graph-source/1794418392450/validation.json) binds the exact product, generated files, outputs, test sources and raw logs. The 12 generated inputs were captured immediately after building, before subsequent Cargo checks. The 93 product inputs, 8,450 dependency inputs and 147 outputs remain bound to that producer; the post-check producer verification passes.

191 Rust tests, formatting, strict workspace Clippy and 18 provenance fault cases pass. Ten affected integration runs pass: call graph (23 check groups), compiler-selected project links (18), test classification/grouping (8), cursors (16), MCP (36), Workspace API (31), cancellation (7), deadlines (2), response bounds (5), and upgrades (5). These counts describe finite cases, not exhaustive compiler/runtime behavior.

The new integration assertions cover TS/C# opt-in/default/false behavior through CLI and both native SDKs; typed .NET nullable requests; exact selected project file/hash/extent, including a dependency framework different from the root; duplicate source copies on target supports; clipping without lost findings; grouped atomic pages and replay after a response-cap error; and unchanged old source/omission state after a save. Existing symbol-context defaults and declaration-slot behavior remain tested. A [separate actual-native-5 fixture](../.artifacts/graph-source/1794418392450/native5-rejection/validation.json) pairs current wrappers with the prior certified binaries: Node and .NET both return typed `update_required` before opening an index. Source-level review by an independent agent found no blocker in ownership, budgets, defaults, evidence or guard propagation.

The matching runtime/npm/NuGet archives pass all 25 installed-offline checks, including graph source through both native SDKs/CLI and installed MCP, after explicit dependency preparation. Checkout and network denial are independently exercised; source bodies remain exact retained UTF-8. The [archive handoff](../.artifacts/graph-source/1794418392450/package-validation/attempt-01/validation.json) records exact archive identities and unchanged before/after producer evidence; its SHA-256 is `affd3a4f278f41f11625ebb80a46a23bc28fdb37e3a7d419797219a9c2dea0c8`. The finalized combined packet SHA-256 is `040a36fd0fca6eabe642a971675eb0057fc98e8fb733c07f14cb9663e07b845f`; the [independent audit](../.artifacts/graph-source/1794418392450/independent-audit.json) sits beside it. Owned test coordinators stopped; the installed test socket is absent and its writer fence is free.

The final independent audit passes with no discrepancy (audit SHA-256 `8f8c9d5f69a5d1d04a4576131dabc8c2ab7e6af90fb6e764809b4ccafb030535`). It verifies the final packet, producer/source/generated/output/test identities, actual native-5 rejection, exact archives and payloads, recorded installed commands/results, dependency-before-registration ordering, network/checkout denial and socket absence. The package owner records the free writer fence; the reviewer did not independently acquire it. Its additional raw fixture inspection confirms 80 source bytes shared across three cursor pages and across grouped representative/support copies.

No full Debug script, optimized profile, macOS 15 machine/VM, real-encoding performance, physical WAL pressure, installer transition, signing/notarization or publication was certified by these focused runs. Earlier upgrade/installer packets retain their original scope. No live development installation or original external corpus was changed.
