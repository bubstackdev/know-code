# Know Code Node SDK

macOS 15+ / Apple Silicon, Node 24. Install the separate Know Code runtime bundle and put its `bin` directory on PATH. Node.js and the .NET 10 SDK are installed separately; the C# SDK prerequisite applies when indexing C#.

```js
import {Workspace, KnowCodeError} from '@know-code/node';
const workspace = await Workspace.open('/absolute/repository');
try {
  const status = await workspace.waitForIndex({waitMs: 30_000});
  if (status.generation > 0) console.log((await workspace.query({op:'symbols',name:'Greeter'})).result);
} finally { workspace.close(); }
```

Opening registers saved-file indexing and auto-starts an installed coordinator when absent. Optional `coordinatorExecutable` overrides PATH. Status/coverage/freshness remain explicit; waiting can time out before an index is ready. API failures throw `KnowCodeError` with a `code`; compiler errors remain status/coverage. Closing preserves background indexing and permits accepted work to finish. Queries execute in-process. This candidate is an unsigned development artifact; full 0.0.1 acceptance is still in progress. Public code has no usage telemetry.

For paged results, `limit` caps the whole logical result and `page_size` selects 1–1000 items per page:

```js
let response = await workspace.query({op:'symbols', limit:100, page_size:20});
for (;;) {
  console.log(response.result.symbols);
  const cursor = response.result.page?.next_cursor;
  if (cursor == null) break;
  response = await workspace.query({op:'next_page', cursor, deadline_ms:5000, response_max_bytes:262144});
}
```

A `next_page` request accepts only `op`, `cursor`, `deadline_ms` and `response_max_bytes`. Each page exposes a reusable `page.cursor`, nullable `next_cursor`, zero-based position, page count and section totals/start/count. Graph findings and boundaries occupy separate sections; later pages may contain boundaries after findings finish. Original `truncated`, graph limits and coverage still apply on the final page.

Tokens are opaque and portable across clients connected to the same coordinator incarnation. Pages stay on their original generation and replay the same rows, order and next position. A smaller response budget may fail rather than repack a page. Pass a page's `cursor` to `source` or `symbol_context` for bounded reads from that generation; source hashes/ranges must refer to that indexed text. Expiry, coordinator restart or retention pressure can return `cursor_invalidated`; restart the query explicitly. These SDK declarations require native query contract 6; assembled candidate acceptance remains in progress.

Graph queries (`callers`, `callees`, `impact`) omit declaration bodies by default. Set `include_source: true` to attach `context.declarations` to each finding and exact target support. `source_max_bytes` bounds all returned source text together (16 KiB default, 256 KiB maximum), counting repeated representative/support copies and spanning all pages. Bodies come from the pinned indexed source. Exhaustion clips or omits bodies with explicit statuses and a `source_budget` limit while preserving findings under the other graph/response caps. Graph `max_bytes` still bounds graph JSON. Path edges and root pointers retain locations. `symbol_context` continues to include source by default and uses its existing `max_bytes` source budget.

Both `Index.query` and `Workspace.query` accept an optional second argument `{signal}`:

```js
const controller = new AbortController();
const pending = workspace.query({op:'symbols', name:'Greeter'}, {signal:controller.signal});
controller.abort('query no longer needed');
try { await pending; }
catch (error) { if (error.name !== 'AbortError') throw error; }
```

A pre-aborted signal rejects before dispatch. In-flight cancellation waits for the native query to exit before rejecting with an `Error` named `AbortError`, code `ABORT_ERR` and the signal's reason as `cause` when available. A query already completed successfully may still return success after a late abort. Options never enter query JSON, and aborting a query does not cancel shared background indexing or close the workspace. Calls without a signal do not allocate a native cancellation handle.

Filter finding searches with `tests: "only"` or `"exclude"` (default `"all"`); .NET uses `Tests = TestFilter.Only`/`Exclude`. Exclusion retains unknown code; mixed symbols match either view when a declaration matches. References classify use sites; graph traversal crosses nonmatching helpers. Classification/evidence and workspace classification coverage stay explicit. Source/coverage expansion rejects nondefault filters.

Equivalent target findings share one result when compiler evidence and the full projected declaration agree. `supporting_targets` appears for multi-member results and preserves every exact symbol/compilation/target, location and classification; graph members also retain their own complete paths. A returned `id` remains a concrete member. Follow-up references, callers, callees, impact and context queries expand its proven peers by default. Use `target` or `compilation_id` (`Target`/`CompilationId` in .NET) to narrow them. `limit` and `page_size` count whole logical findings. Unknown equivalence stays separate and visible in coverage.

Forward the result's opaque `snapshot` (`Snapshot` in .NET) with a generation-scoped read. The token guards the served index version; portable `cursor` tokens separately retain their pinned generation. Managed snapshot verification is integrated by the common core's recovery path.
