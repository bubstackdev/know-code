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
