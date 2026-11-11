# Know Code .NET SDK

macOS 15+ / Apple Silicon, .NET 10. Install the separate Know Code runtime bundle and put its `bin` directory on PATH. Node 24 and the .NET 10 SDK are installed separately for compiler analysis.

```csharp
using var workspace = await KnowCode.Workspace.OpenAsync("/absolute/repository");
var status = await workspace.WaitForIndexAsync(30_000);
if (status.Generation > 0) {
    using var response = await workspace.QueryAsync(new KnowCode.QueryRequest("symbols", Name: "Greeter"));
    Console.WriteLine(response.RootElement.GetProperty("result"));
}
```

Opening registers saved-file indexing and auto-starts an installed coordinator when absent. `WorkspaceOptions.CoordinatorExecutable` overrides PATH. Status/coverage/freshness remain explicit; waiting can time out. API failures throw `KnowCodeException` with `Code`; compiler errors remain status/coverage. Disposal preserves background indexing and permits accepted work to finish. Queries execute in-process. The package includes its macOS ARM64 native library. This candidate is unsigned; full 0.0.1 acceptance remains in progress. Public code has no usage telemetry.

Set `PageSize` to 1–1000 to page a supported query. `Limit` still caps the whole logical result; the positional `QueryRequest` constructor is unchanged:

```csharp
using var first = await workspace.QueryAsync(new KnowCode.QueryRequest("symbols", Limit: 100) { PageSize = 20 });
var page = first.RootElement.GetProperty("result").GetProperty("page");
if (page.GetProperty("next_cursor").GetString() is { } cursor) {
    using var next = await workspace.NextPageAsync(cursor, deadlineMs: 5_000, responseMaxBytes: 262_144);
    Console.WriteLine(next.RootElement.GetProperty("result"));
}
```

`Index.NextPageAsync` and `Workspace.NextPageAsync` serialize only `op`, `cursor` and supplied per-request budgets. Use these helpers rather than `new QueryRequest("next_page")`, whose ordinary query defaults are invalid for strict continuations. Like existing queries, `Index` returns error envelopes while `Workspace` throws typed errors.

Every page provides its reusable `page.cursor`, nullable `next_cursor`, zero-based position, page count and section totals/start/count. Graph findings and boundaries occupy separate sections; later pages can contain boundaries after findings finish. Original `truncated`, graph limits and coverage persist through the final page. Replay preserves rows, order and next position; a smaller response budget may fail rather than repartition the result.

Tokens are opaque and portable across clients using the same coordinator incarnation. Set `QueryRequest.Cursor` for separate `source` or `symbol_context` reads through the pinned generation; source hashes/ranges must match that indexed text. Expiry, coordinator restart or retention pressure returns `cursor_invalidated`, requiring an explicit new query. The SDK requires native query contract 5; assembled candidate acceptance remains in progress.

`QueryAsync`, `QueryJsonAsync` and `NextPageAsync` on both `Index` and `Workspace` accept an optional final `CancellationToken` parameter:

```csharp
using var cancellation = new CancellationTokenSource();
var pending = workspace.QueryAsync(new KnowCode.QueryRequest("symbols", Name: "Greeter"), cancellation.Token);
cancellation.Cancel();
try { using var result = await pending; }
catch (OperationCanceledException) { /* Only this query was cancelled. */ }
```

A pre-cancelled token stops before dispatch. In-flight cancellation returns `OperationCanceledException` from the awaited task after native work exits; its token matches the supplied token. Cancellation registration and native handle remain alive until then. Core `cancelled` is translated to this host exception, while a completed core success may still succeed after a late cancellation. Tokens are not serialized into query JSON, and query cancellation does not cancel indexing or dispose the workspace. Calls without a cancellable token do not allocate a native cancellation handle.

Filter finding searches with `tests: "only"` or `"exclude"` (default `"all"`); .NET uses `Tests = TestFilter.Only`/`Exclude`. Exclusion retains unknown code; mixed symbols match either view when a declaration matches. References classify use sites; graph traversal crosses nonmatching helpers. Classification/evidence and workspace classification coverage stay explicit. Source/coverage expansion rejects nondefault filters.

Equivalent target findings share one result when compiler evidence and the full projected declaration agree. `supporting_targets` appears for multi-member results and preserves every exact symbol/compilation/target, location and classification; graph members also retain their own complete paths. A returned `id` remains a concrete member. Follow-up references, callers, callees, impact and context queries expand its proven peers by default. Use `target` or `compilation_id` (`Target`/`CompilationId` in .NET) to narrow them. `limit` and `page_size` count whole logical findings. Unknown equivalence stays separate and visible in coverage.

Forward the result's opaque `snapshot` (`Snapshot` in .NET) with a generation-scoped read. The token guards the served index version; portable `cursor` tokens separately retain their pinned generation. Managed snapshot verification is integrated by the common core's recovery path.
