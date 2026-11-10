#nullable enable
using System.Text.Json;
if (args[0] == "--cancel-query")
{
    using var cancellation = new CancellationTokenSource();
    var pending = new KnowCode.Index(args[1]).QueryJsonAsync(args[2], cancellation.Token);
    cancellation.CancelAfter(int.Parse(args[3]));
    try {
        using var result = await pending;
        Console.WriteLine(result.RootElement.GetRawText());
    } catch (OperationCanceledException error) when (error.CancellationToken == cancellation.Token) {
        Console.WriteLine("{\"cancelled\":true,\"name\":\"OperationCanceledException\"}");
    }
    return;
}
if (args[0] == "--cancel-queue")
{
    ThreadPool.GetMinThreads(out var minimum, out var minimumIo);
    ThreadPool.GetMaxThreads(out var maximum, out var maximumIo);
    if (!ThreadPool.SetMinThreads(1, minimumIo) || !ThreadPool.SetMaxThreads(1, maximumIo))
        throw new Exception("Could not isolate fixture worker pool");
    using var entered = new ManualResetEventSlim();
    using var release = new ManualResetEventSlim();
    using var cancellation = new CancellationTokenSource();
    var blocker = Task.Run(() => { entered.Set(); release.Wait(); });
    try {
        entered.Wait();
        var queuedIndex = new KnowCode.Index(args[1]);
        var pending = queuedIndex.QueryAsync(new KnowCode.QueryRequest("symbols"), cancellation.Token);
        cancellation.Cancel();
        if (pending.IsCompleted) throw new Exception("Cancelled query detached from queued native work");
        release.Set();
        try {
            using var unexpected = await pending;
            throw new Exception("Expected queued query cancellation: " + unexpected.RootElement.GetRawText());
        } catch (OperationCanceledException error) when (error.CancellationToken == cancellation.Token) { }
        using var next = await queuedIndex.QueryAsync(new KnowCode.QueryRequest("symbols"));
        if (next.RootElement.TryGetProperty("error", out var failure) && failure.GetProperty("code").GetString() != "not_ready")
            throw new Exception(next.RootElement.GetRawText());
        Console.WriteLine("{\"passed\":true,\"check\":\"DotNet_queued_query_cancelled_after_native_exit\"}");
    } finally {
        release.Set();
        ThreadPool.SetMaxThreads(maximum, maximumIo);
        ThreadPool.SetMinThreads(minimum, minimumIo);
        await blocker;
    }
    return;
}
if (args[0] == "--next-page")
{
    var pagedIndex = new KnowCode.Index(args[1]);
    using var result = await pagedIndex.NextPageAsync(args[2],
        args.Length > 3 ? uint.Parse(args[3]) : null,
        args.Length > 4 ? uint.Parse(args[4]) : null);
    Console.WriteLine(result.RootElement.GetRawText());
    return;
}
if (args[0] == "--deadline-queue")
{
    ThreadPool.GetMinThreads(out var minimum, out var minimumIo);
    ThreadPool.GetMaxThreads(out var maximum, out var maximumIo);
    if (!ThreadPool.SetMinThreads(1, minimumIo) || !ThreadPool.SetMaxThreads(1, maximumIo))
        throw new Exception("Could not isolate fixture worker pool");
    using var entered = new ManualResetEventSlim();
    using var release = new ManualResetEventSlim();
    var blocker = Task.Run(() => { entered.Set(); release.Wait(); });
    try {
        entered.Wait();
        var queuedIndex = new KnowCode.Index(args[1]);
        var pending = queuedIndex.QueryAsync(new KnowCode.QueryRequest("symbols", DeadlineMs:25));
        Thread.Sleep(100);
        release.Set();
        using var result = await pending;
        if (result.RootElement.GetProperty("error").GetProperty("code").GetString() != "deadline_exceeded")
            throw new Exception(result.RootElement.GetRawText());
        using var next = await queuedIndex.QueryAsync(new KnowCode.QueryRequest("symbols"));
        if (next.RootElement.GetProperty("error").GetProperty("code").GetString() != "not_ready")
            throw new Exception(next.RootElement.GetRawText());
        Console.WriteLine("{\"passed\":true,\"check\":\"DotNet_worker_queue_consumes_overall_deadline\"}");
    } finally {
        release.Set();
        ThreadPool.SetMaxThreads(maximum, maximumIo);
        ThreadPool.SetMinThreads(minimum, minimumIo);
        await blocker;
    }
    return;
}
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
                    case "settings": Print(await workspace.SettingsAsync());break;
                    case "configure": Print(await workspace.ConfigureJsonAsync(value.GetProperty("settings").GetRawText()));break;
                    case "wait": Print(await workspace.WaitForIndexAsync(value.GetProperty("wait_ms").GetUInt32()));break;
                    case "refresh": Print(await workspace.RefreshAsync(value.TryGetProperty("configuration",out var configuration)?configuration.GetString():null));break;
                    case "query":
                        using(var result=await workspace.QueryJsonAsync(value.GetProperty("request").GetRawText())) Console.WriteLine(result.RootElement.GetRawText());
                        break;
                    case "typed_query":
                        using(var result=await workspace.QueryAsync(new KnowCode.QueryRequest("symbols",Name:value.GetProperty("name").GetString()))) Console.WriteLine(result.RootElement.GetRawText());
                        break;
                    case "next_page":
                        using(var result=await workspace.NextPageAsync(value.GetProperty("cursor").GetString()!,
                            value.TryGetProperty("deadline_ms",out var deadline)?deadline.GetUInt32():null,
                            value.TryGetProperty("response_max_bytes",out var maximumBytes)?maximumBytes.GetUInt32():null))
                            Console.WriteLine(result.RootElement.GetRawText());
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
