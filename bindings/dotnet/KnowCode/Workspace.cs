using System.Text.Json;

namespace KnowCode;

public sealed record WorkspaceOptions(string? Configuration = null, string? DataDirectory = null, string? CoordinatorExecutable = null);
public sealed record IndexProgress(string Stage, string ProjectPath, string Message);
public sealed record WorkspaceStatus(string WorkspaceRoot, string Database, string Phase, ulong ObservedEpoch,
    ulong PublishedEpoch, ulong Generation, bool Available, string Error, IndexProgress? Progress, string OutcomeCode = "");
internal sealed record WorkspaceDescriptor(string DataDirectory, string? CoordinatorExecutable, WorkspaceStatus Status);

public sealed class KnowCodeException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

public sealed class Workspace : IDisposable
{
    private readonly WorkspaceDescriptor descriptor;
    private readonly string? executable;
    private readonly Index index;
    private int closed;
    private Workspace(WorkspaceDescriptor descriptor)
    {
        this.descriptor = descriptor; this.executable = descriptor.CoordinatorExecutable;
        index = new Index(descriptor.Status.Database);
    }
    public string Root => descriptor.Status.WorkspaceRoot;
    public WorkspaceStatus InitialStatus => descriptor.Status;
    public static async Task<Workspace> OpenAsync(string root, WorkspaceOptions? options = null)
    {
        var result = await ControlAsync(new { Op = "open", WorkspaceRoot = root,
            options?.Configuration, options?.DataDirectory, options?.CoordinatorExecutable }).ConfigureAwait(false);
        return new Workspace(result);
    }
    private void CheckOpen()
    {
        if (Volatile.Read(ref closed) != 0) throw new KnowCodeException("closed","Workspace is closed; open a new client.");
    }
    public Task<WorkspaceStatus> StatusAsync() => OperationAsync("status");
    public Task<WorkspaceStatus> CancelIndexingAsync() => OperationAsync("cancel");
    public Task<WorkspaceStatus> RefreshAsync(string? configuration = null) => OperationAsync("refresh",configuration);
    public Task<WorkspaceStatus> WaitForIndexAsync(uint waitMs = 500) => OperationAsync("wait",waitMs:waitMs);
    private async Task<WorkspaceStatus> OperationAsync(string op, string? configuration = null, uint? waitMs = null)
    {
        CheckOpen();
        var result = await ControlAsync(new { Op = op, descriptor.Status.Database, descriptor.DataDirectory,
            CoordinatorExecutable = executable, Configuration = configuration, WaitMs = waitMs }).ConfigureAwait(false);
        return result.Status;
    }
    public Task<JsonDocument> QueryAsync(QueryRequest request) => QueryJsonAsync(JsonSerializer.Serialize(request,Native.JsonOptions));
    public async Task<JsonDocument> QueryJsonAsync(string request)
    {
        CheckOpen();
        var result = await index.QueryJsonAsync(request).ConfigureAwait(false);
        try { Native.Check(result); return result; }
        catch { result.Dispose(); throw; }
    }
    private static async Task<WorkspaceDescriptor> ControlAsync(object request)
    {
        var json = JsonSerializer.Serialize(request,Native.JsonOptions);
        using var response = await Task.Run(() => Native.Invoke(() => Native.Workspace(json))).ConfigureAwait(false);
        Native.Check(response);
        return response.RootElement.GetProperty("result").Deserialize<WorkspaceDescriptor>(Native.JsonOptions)
            ?? throw new KnowCodeException("protocol_error","Workspace response is missing its result.");
    }
    public void Dispose() { Interlocked.Exchange(ref closed,1); }
}
