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
