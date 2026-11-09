#nullable enable
using System.Text.Json;
if (args[0] == "--workspace")
{
    using var optionsJson = JsonDocument.Parse(args[2]);
    var options = optionsJson.RootElement;
    string? Field(string name) => options.TryGetProperty(name,out var value) ? value.GetString() : null;
    KnowCode.Workspace workspace;
    void Print(object value) => Console.WriteLine(JsonSerializer.Serialize(value,new JsonSerializerOptions {PropertyNamingPolicy=JsonNamingPolicy.SnakeCaseLower}));
    void Failure(KnowCode.KnowCodeException error) => Print(new { Error = new { error.Code, error.Message } });
    try { workspace = await KnowCode.Workspace.OpenAsync(args[1],new KnowCode.WorkspaceOptions(Field("configuration"),Field("dataDirectory"),Field("coordinatorExecutable"))); }
    catch(KnowCode.KnowCodeException error) { Failure(error);return; }
    using(workspace)
    {
        Print(new { Opened=workspace.InitialStatus,workspace.Root });
        while(Console.ReadLine() is { } line)
        {
            using var action=JsonDocument.Parse(line);var value=action.RootElement;
            try {
                switch(value.GetProperty("op").GetString()) {
                    case "cancel": Print(await workspace.CancelIndexingAsync());break;
                    case "status": Print(await workspace.StatusAsync());break;
                    case "wait": Print(await workspace.WaitForIndexAsync(value.GetProperty("wait_ms").GetUInt32()));break;
                    case "refresh": Print(await workspace.RefreshAsync(value.TryGetProperty("configuration",out var configuration)?configuration.GetString():null));break;
                    case "query":
                        using(var result=await workspace.QueryJsonAsync(value.GetProperty("request").GetRawText())) Console.WriteLine(result.RootElement.GetRawText());
                        break;
                    case "typed_query":
                        using(var result=await workspace.QueryAsync(new KnowCode.QueryRequest("symbols",Name:value.GetProperty("name").GetString()))) Console.WriteLine(result.RootElement.GetRawText());
                        break;
                    case "close": workspace.Dispose();workspace.Dispose();Print(new {Closed=true});break;
                    case "close_during_query":
                        var pending=workspace.QueryJsonAsync(value.GetProperty("request").GetRawText());workspace.Dispose();
                        using(var result=await pending) Console.WriteLine(result.RootElement.GetRawText());
                        break;
                }
            } catch(KnowCode.KnowCodeException error) { Failure(error); }
        }
    }
    return;
}
var index = new KnowCode.Index(args[0]);
using var response = await index.QueryJsonAsync(args[1]);
Console.WriteLine(response.RootElement.GetRawText());
if (args.Length == 3 && args[2] == "--repeat")
{
    // Test consumers can change coordinator state between calls without
    // rebuilding or adding diagnostic exports to the native library.
    if (Console.ReadLine() is null) return;
    using var next = await index.QueryJsonAsync(args[1]);
    Console.WriteLine(next.RootElement.GetRawText());
}
