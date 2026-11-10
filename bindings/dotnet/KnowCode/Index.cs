using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Diagnostics;

namespace KnowCode;

public sealed class Index(string database)
{
    public Task<JsonDocument> QueryAsync(QueryRequest request, CancellationToken cancellationToken = default)
    {
        if (cancellationToken.IsCancellationRequested) return Task.FromCanceled<JsonDocument>(cancellationToken);
        var started = Stopwatch.GetTimestamp();
        return QueryCoreAsync(JsonSerializer.Serialize(request, Native.JsonOptions), started, cancellationToken);
    }

    // Coarse native calls run off the caller's synchronization context.
    public Task<JsonDocument> QueryJsonAsync(string request, CancellationToken cancellationToken = default) =>
        QueryCoreAsync(request, Stopwatch.GetTimestamp(), cancellationToken);
    /// <summary>Reads a frozen cursor page with optional per-request deadline and response-byte budgets.</summary>
    public Task<JsonDocument> NextPageAsync(string cursor, uint? deadlineMs = null, uint? responseMaxBytes = null,
        CancellationToken cancellationToken = default)
    {
        if (cancellationToken.IsCancellationRequested) return Task.FromCanceled<JsonDocument>(cancellationToken);
        return QueryJsonAsync(JsonSerializer.Serialize(new { Op = "next_page", Cursor = cursor,
            DeadlineMs = deadlineMs, ResponseMaxBytes = responseMaxBytes }, Native.JsonOptions), cancellationToken);
    }
    private async Task<JsonDocument> QueryCoreAsync(string request, long started, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        using var cancellation = cancellationToken.CanBeCanceled ? Native.CreateQueryCancellation() : null;
        using var registration = cancellationToken.CanBeCanceled
            ? cancellationToken.UnsafeRegister(static state => Native.QueryCancel((QueryCancellationHandle)state!), cancellation)
            : default;
        // Do not cancel Task.Run itself: queued/active native work must finish
        // before its registration and safe handle can be released.
        var result = await Task.Run(() => Native.Invoke(() => {
            Native.CheckQueryContract();
            var elapsed = (ulong)Stopwatch.GetElapsedTime(started).TotalMilliseconds;
            return cancellation is null ? Native.QueryElapsed(database, request, elapsed)
                : Native.QueryCancellable(database, request, elapsed, cancellation);
        })).ConfigureAwait(false);
        if (result.RootElement.TryGetProperty("error", out var error)
            && error.GetProperty("code").GetString() == "cancelled")
        {
            result.Dispose();
            throw new OperationCanceledException(cancellationToken);
        }
        return result;
    }
}

internal sealed class QueryCancellationHandle : SafeHandle
{
    internal QueryCancellationHandle(nint value) : base(nint.Zero, true) { SetHandle(value); }
    public override bool IsInvalid => handle == nint.Zero;
    protected override bool ReleaseHandle() { Native.QueryCancelFree(handle); return true; }
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
    public uint? ResponseMaxBytes { get; init; }
    /// <summary>Opts supported queries into pagination (1–1000 items per page). Limit remains the total cap.</summary>
    public uint? PageSize { get; init; }
    /// <summary>Pinned source/symbol_context token. Use NextPageAsync for strict continuation requests.</summary>
    public string? Cursor { get; init; }
}

internal static partial class Native
{
    internal static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };
    internal static void CheckQueryContract()
    {
        if (QueryContractVersion() != 3)
            throw new KnowCodeException("update_required", "Update the Know Code managed/native packages together; cancellable query support is incompatible.");
    }
    internal static QueryCancellationHandle CreateQueryCancellation()
    {
        try
        {
            if (AbiVersion() != 1) throw new KnowCodeException("update_required", "Update the Know Code managed/native packages together; native ABI is incompatible.");
            CheckQueryContract();
            var value = QueryCancelNew();
            if (value == nint.Zero) throw new KnowCodeException("internal_error", "Know Code could not allocate a query cancellation handle.");
            return new QueryCancellationHandle(value);
        }
        catch (EntryPointNotFoundException error) { throw new KnowCodeException("update_required", $"Update the Know Code managed/native packages together: {error.Message}"); }
        catch (DllNotFoundException error) { throw new KnowCodeException("native_library_unavailable", $"Install the matching Know Code native library: {error.Message}"); }
    }
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
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_contract_version")]
    internal static partial uint QueryContractVersion();
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_json", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint Query(string database, string request);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_json_elapsed", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint QueryElapsed(string database, string request, ulong elapsedMs);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_cancel_new")]
    internal static partial nint QueryCancelNew();
    // SafeHandle parameters retain ownership across the native call and a
    // concurrent cancellation callback through generated interop addrefs.
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_cancel")]
    internal static partial void QueryCancel(QueryCancellationHandle cancellation);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_cancel_free")]
    internal static partial void QueryCancelFree(nint cancellation);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_json_cancellable", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint QueryCancellable(string database, string request, ulong elapsedMs, QueryCancellationHandle cancellation);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_string_free")]
    internal static partial void Free(nint value);
}
