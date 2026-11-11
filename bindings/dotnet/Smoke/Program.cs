#nullable enable
using System.Text.Json;
if (args[0] == "--admission-typed")
{
    try {
        using var result = await new KnowCode.Index(args[1], new KnowCode.QueryLimitScope(args[2])).QueryAsync(
            new KnowCode.QueryRequest("symbols") { Tests = (KnowCode.TestFilter)999 });
        Console.WriteLine(result.RootElement.GetRawText());
    } catch (JsonException) { Console.WriteLine("{\"serialization_failed\":true}"); }
    return;
}
if (args[0] == "--scoped-query")
{
    using var result = await new KnowCode.Index(args[1], new KnowCode.QueryLimitScope(args[2])).QueryJsonAsync(args[3]);
    Console.WriteLine(result.RootElement.GetRawText());
    return;
}
if (args[0] == "--admission-queue")
{
    // Hold the host pool so accepted requests retain their reservations while
    // queued; saturation and pre-cancellation must complete without pool work.
    var slots = int.Parse(args[3]);
    ThreadPool.GetMinThreads(out var minimum, out var minimumIo);
    ThreadPool.GetMaxThreads(out var maximum, out var maximumIo);
    if (!ThreadPool.SetMinThreads(1, minimumIo) || !ThreadPool.SetMaxThreads(1, maximumIo))
        throw new Exception("Could not isolate fixture worker pool");
    using var entered = new ManualResetEventSlim();
    using var release = new ManualResetEventSlim();
    using var cancellation = new CancellationTokenSource();
    var blocker = Task.Run(() => { entered.Set(); release.Wait(TimeSpan.FromSeconds(30)); });
    var pending = new List<Task<JsonDocument>>();
    try {
        if (!entered.Wait(TimeSpan.FromSeconds(5))) throw new Exception("Fixture pool did not block");
        var admissionIndex = new KnowCode.Index(args[1], new KnowCode.QueryLimitScope(args[2]));
        for (var i = 0; i < slots; i++)
            pending.Add(admissionIndex.QueryAsync(new KnowCode.QueryRequest("symbols"), i == 0 ? cancellation.Token : default));
        void Busy()
        {
            var rejected = admissionIndex.QueryAsync(new KnowCode.QueryRequest("symbols"));
            if (!rejected.IsCompleted) throw new Exception("Saturation entered the host queue");
            using var rejectedResponse = rejected.GetAwaiter().GetResult();
            if (rejectedResponse.RootElement.GetProperty("error").GetProperty("code").GetString() != "busy")
                throw new Exception(rejectedResponse.RootElement.GetRawText());
        }
        Busy();
        cancellation.Cancel();
        if (pending[0].IsCompleted) throw new Exception("Cancellation detached queued native work");
        Busy();
        try {
            admissionIndex.QueryAsync(new KnowCode.QueryRequest("symbols"), cancellation.Token).GetAwaiter().GetResult().Dispose();
            throw new Exception("Pre-cancellation lost to saturation");
        } catch (OperationCanceledException) { }
        release.Set();
        ThreadPool.SetMaxThreads(maximum, maximumIo);
        ThreadPool.SetMinThreads(minimum, minimumIo);
        for (var i = 0; i < pending.Count; i++) {
            try { using var result = await pending[i].WaitAsync(TimeSpan.FromSeconds(20)); }
            catch (OperationCanceledException) when (i == 0) { }
        }
        using var next = await admissionIndex.QueryAsync(new KnowCode.QueryRequest("symbols"));
        if (next.RootElement.TryGetProperty("error", out var error) && error.GetProperty("code").GetString() == "busy")
            throw new Exception("Reservation leaked after host completion");
        Console.WriteLine("{\"passed\":true,\"check\":\"DotNet_early_shared_admission_and_queued_cancellation\"}");
    } finally {
        release.Set();
        ThreadPool.SetMaxThreads(maximum, maximumIo);
        ThreadPool.SetMinThreads(minimum, minimumIo);
        await blocker.WaitAsync(TimeSpan.FromSeconds(5));
    }
    return;
}
if (args[0] == "--typed-graph-source")
{
    if (args.Length != 6) throw new ArgumentException("Expected DATABASE OP SYMBOL_ID default|true|false SOURCE_MAX_BYTES|-");
    using var result = await new KnowCode.Index(args[1]).QueryAsync(new KnowCode.QueryRequest(args[2], SymbolId:args[3]) {
        IncludeSource = args[4] switch { "default" => null, "true" => true, "false" => false,
            _ => throw new ArgumentException("IncludeSource must be default, true or false") },
        SourceMaxBytes = args[5] == "-" ? null : uint.Parse(args[5])
    });
    Console.WriteLine(result.RootElement.GetRawText());
    return;
}
if (args[0] == "--typed-test-filter")
{
    var filter = Enum.Parse<KnowCode.TestFilter>(args[2], ignoreCase: true);
    using var result = await new KnowCode.Index(args[1]).QueryAsync(new KnowCode.QueryRequest("symbols", Limit:1000) { Tests = filter });
    Console.WriteLine(result.RootElement.GetRawText());
    return;
}
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
    try { workspace = await KnowCode.Workspace.OpenAsync(args[1],new KnowCode.WorkspaceOptions(Field("configuration"),Field("dataDirectory"),Field("coordinatorExecutable"),
        Field("queryLimitScope") is { } scope ? new KnowCode.QueryLimitScope(scope) : null)); }
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
                    case "trust": Print(await workspace.SetProjectTrustAsync(value.GetProperty("grant").GetBoolean()));break;
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
