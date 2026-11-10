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

Tokens are opaque and portable across clients connected to the same coordinator incarnation. Pages stay on their original generation and replay the same rows, order and next position. A smaller response budget may fail rather than repack a page. Pass a page's `cursor` to `source` or `symbol_context` for bounded reads from that generation; source hashes/ranges must refer to that indexed text. Expiry, coordinator restart or retention pressure can return `cursor_invalidated`; restart the query explicitly. These SDK declarations require native query contract 2; assembled cursor acceptance remains in progress.
