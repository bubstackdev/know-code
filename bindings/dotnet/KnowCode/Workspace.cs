using System.Text.Json;

namespace KnowCode;

public sealed record WorkspaceOptions(string? Configuration = null, string? DataDirectory = null, string? CoordinatorExecutable = null, QueryLimitScope? QueryLimitScope = null);
public sealed record IndexProgress(string Stage, string ProjectPath, string Message);
public sealed record CSharpBuildContext(string? Configuration, string? SolutionPath);
public sealed record NodePreparationPolicy(string CacheDirectory, bool AllowInstallScripts);
public sealed record AnalysisSettings(CSharpBuildContext Csharp, NodePreparationPolicy Node,
    string ConfigurationOrigin, string SolutionOrigin, string ProjectFile, string PersonalFile,
    bool ProjectExecutionAllowed = false);
public sealed record WorkspaceStatus(string WorkspaceRoot, string Database, string Phase, ulong ObservedEpoch,
    ulong PublishedEpoch, ulong Generation, bool Available, string Error, IndexProgress? Progress, string OutcomeCode = "", AnalysisSettings? RequestedSettings = null);
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
    private Workspace(WorkspaceDescriptor descriptor, QueryLimitScope? queryLimitScope)
    {
        this.descriptor = descriptor; this.executable = descriptor.CoordinatorExecutable;
        index = new Index(descriptor.Status.Database, queryLimitScope);
    }
    public string Root => descriptor.Status.WorkspaceRoot;
    public WorkspaceStatus InitialStatus => descriptor.Status;
    public static async Task<Workspace> OpenAsync(string root, WorkspaceOptions? options = null)
    {
        var result = await ControlAsync(new { Op = "open", WorkspaceRoot = root,
            options?.Configuration, options?.DataDirectory, options?.CoordinatorExecutable }).ConfigureAwait(false);
        return new Workspace(result, options?.QueryLimitScope);
    }
    private void CheckOpen()
    {
        if (Volatile.Read(ref closed) != 0) throw new KnowCodeException("closed","Workspace is closed; open a new client.");
    }
    public Task<WorkspaceStatus> StatusAsync() => OperationAsync("status");
    public Task<WorkspaceStatus> CancelIndexingAsync() => OperationAsync("cancel");
    public Task<WorkspaceStatus> SettingsAsync() => OperationAsync("settings");
    /// <summary>Grants/revokes project execution for this workspace identity. Opening never grants trust.</summary>
    public Task<WorkspaceStatus> SetProjectTrustAsync(bool grant) => OperationAsync("trust", grant: grant);
    /// <summary>Replaces personal settings. Omitted fields inherit/default; explicit null selects compiler defaults.</summary>
    public Task<WorkspaceStatus> ConfigureJsonAsync(string settings)
    {
        CheckOpen();
        try {
            using var document = JsonDocument.Parse(settings);
            return OperationAsync("configure",personalSettings:document.RootElement.Clone());
        } catch(JsonException error) { throw new KnowCodeException("invalid_request",error.Message); }
    }
    public Task<WorkspaceStatus> RefreshAsync(string? configuration = null) => OperationAsync("refresh",configuration);
    public Task<WorkspaceStatus> WaitForIndexAsync(uint waitMs = 500) => OperationAsync("wait",waitMs:waitMs);
    private async Task<WorkspaceStatus> OperationAsync(string op, string? configuration = null, uint? waitMs = null, JsonElement? personalSettings = null, bool? grant = null)
    {
        CheckOpen();
        var result = await ControlAsync(new { Op = op, descriptor.Status.Database, descriptor.DataDirectory,
            CoordinatorExecutable = executable, Configuration = configuration, WaitMs = waitMs, PersonalSettings = personalSettings, Grant = grant }).ConfigureAwait(false);
        return result.Status;
    }
    public async Task<JsonDocument> QueryAsync(QueryRequest request, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        CheckOpen();
        return Checked(await index.QueryAsync(request, cancellationToken).ConfigureAwait(false));
    }
    public async Task<JsonDocument> QueryJsonAsync(string request, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        CheckOpen();
        return Checked(await index.QueryJsonAsync(request, cancellationToken).ConfigureAwait(false));
    }
    /// <summary>Reads a frozen cursor page; failures use the same typed errors as other workspace queries.</summary>
    public async Task<JsonDocument> NextPageAsync(string cursor, uint? deadlineMs = null, uint? responseMaxBytes = null,
        CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        CheckOpen();
        return Checked(await index.NextPageAsync(cursor, deadlineMs, responseMaxBytes, cancellationToken).ConfigureAwait(false));
    }
    private static JsonDocument Checked(JsonDocument result)
    {
        try { Native.Check(result); return result; }
        catch { result.Dispose(); throw; }
    }
    private static async Task<WorkspaceDescriptor> ControlAsync(object request)
    {
        Native.CheckQueryContract();
        var json = JsonSerializer.Serialize(request,Native.JsonOptions);
        using var response = await Task.Run(() => Native.Invoke(() => Native.Workspace(json))).ConfigureAwait(false);
        Native.Check(response);
        return response.RootElement.GetProperty("result").Deserialize<WorkspaceDescriptor>(Native.JsonOptions)
            ?? throw new KnowCodeException("protocol_error","Workspace response is missing its result.");
    }
    public void Dispose() { Interlocked.Exchange(ref closed,1); }
}
