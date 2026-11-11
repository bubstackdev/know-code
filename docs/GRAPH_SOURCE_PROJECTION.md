# Graph declaration source projection

Status: accepted contract; implementation and validation pending. This is the Q06 slice, not whole-release acceptance.

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

Implementation and final evidence will be recorded here when complete.
