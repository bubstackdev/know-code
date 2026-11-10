using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Diagnostics;

namespace KnowCode;

public sealed class Index(string database)
{
    public Task<JsonDocument> QueryAsync(QueryRequest request)
    {
        var started = Stopwatch.GetTimestamp();
        return QueryCoreAsync(JsonSerializer.Serialize(request, Native.JsonOptions), started);
    }

    // Coarse native calls run off the caller's synchronization context.
    public Task<JsonDocument> QueryJsonAsync(string request) => QueryCoreAsync(request, Stopwatch.GetTimestamp());
    private Task<JsonDocument> QueryCoreAsync(string request, long started) => Task.Run(() =>
        Native.Invoke(() => Native.QueryElapsed(database, request,
            (ulong)Stopwatch.GetElapsedTime(started).TotalMilliseconds)));
}

public sealed record QueryRequest(string Op, string? Name = null, string? Query = null,
    string? SymbolId = null, string? ArtifactId = null, string? Target = null, uint Limit = 100, uint FreshnessWaitMs = 500, uint? DeadlineMs = null,
    string[]? Kinds = null, bool IncludeImplicit = true, bool ExactOnly = false, uint? MaxDepth = null,
    uint? MaxEdges = null, uint? MaxNodes = null, uint? PathsPerSymbol = null, uint? MaxBytes = null, uint? TimeBudgetMs = null)
{
    public ulong? StartByte { get; init; }
    public ulong? EndByte { get; init; }
    public string? ContentHash { get; init; }
    public ulong? Generation { get; init; }
    public string? CompilationId { get; init; }
    public bool? IncludeSource { get; init; }
    public string? CoverageDetail { get; init; }
    public uint? DetailMaxBytes { get; init; }
}

internal static partial class Native
{
    internal static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };
    internal static JsonDocument Invoke(Func<nint> call)
    {
        try
        {
            if (AbiVersion() != 1) throw new KnowCodeException("update_required","Update the Know Code managed/native packages together; native ABI is incompatible.");
            var pointer = call();
            try { return JsonDocument.Parse(Marshal.PtrToStringUTF8(pointer)
                ?? throw new KnowCodeException("internal_error","Know Code returned a null result.")); }
            finally { Free(pointer); }
        }
        catch (EntryPointNotFoundException error) { throw new KnowCodeException("update_required", $"Update the Know Code managed/native packages together: {error.Message}"); }
        catch (DllNotFoundException error) { throw new KnowCodeException("native_library_unavailable", $"Install the matching Know Code native library: {error.Message}"); }
    }
    internal static void Check(JsonDocument response)
    {
        if (response.RootElement.TryGetProperty("error",out var error))
            throw new KnowCodeException(error.GetProperty("code").GetString()!,error.GetProperty("message").GetString()!);
    }
    [LibraryImport("know_code_ffi", EntryPoint = "kc_workspace_json", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint Workspace(string request);

    [LibraryImport("know_code_ffi", EntryPoint = "kc_abi_version")]
    internal static partial uint AbiVersion();
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_json", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint Query(string database, string request);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_json_elapsed", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint QueryElapsed(string database, string request, ulong elapsedMs);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_string_free")]
    internal static partial void Free(nint value);
}
