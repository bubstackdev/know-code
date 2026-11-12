using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Diagnostics;

namespace KnowCode;

/// <summary>One shared embedding/server-host scope. Omit to use the account-wide scope.</summary>
public sealed record QueryLimitScope(string Directory);

/// <summary>Compact query freshness evidence. Full detail remains in Workspace.StatusAsync.
/// Cursor identities are frozen with indexed locations; empty means unavailable at creation.
/// StatusLimited marks phase/outcome tokens projected to unknown.</summary>
public sealed record QueryCoordination(string WorkspaceRoot, string Database, string Phase,
    ulong ObservedEpoch, ulong PublishedEpoch, ulong Generation, bool Available,
    bool HasError, string OutcomeCode, bool StatusLimited);

public sealed class Index
{
    private readonly string database;
    private readonly QueryLimitScope? queryLimitScope;

    public Index(string database, QueryLimitScope? queryLimitScope = null)
    {
        this.database = Native.ValidateInteropText(database, nameof(database));
        if (queryLimitScope is not null) Native.ValidateInteropText(queryLimitScope.Directory, nameof(queryLimitScope));
        this.queryLimitScope = queryLimitScope;
    }

    public Task<JsonDocument> QueryAsync(QueryRequest request, CancellationToken cancellationToken = default) =>
        QueryCoreAsync(() => JsonSerializer.Serialize(request, Native.JsonOptions), Stopwatch.GetTimestamp(), cancellationToken);

    // Coarse native calls run off the caller's synchronization context.
    public Task<JsonDocument> QueryJsonAsync(string request, CancellationToken cancellationToken = default) =>
        QueryCoreAsync(() => request, Stopwatch.GetTimestamp(), cancellationToken, rawRequest: true);
    /// <summary>Reads a frozen cursor page with optional per-request deadline and response-byte budgets.</summary>
    public Task<JsonDocument> NextPageAsync(string cursor, uint? deadlineMs = null, uint? responseMaxBytes = null,
        CancellationToken cancellationToken = default) =>
        QueryCoreAsync(() => JsonSerializer.Serialize(new { Op = "next_page", Cursor = cursor,
            DeadlineMs = deadlineMs, ResponseMaxBytes = responseMaxBytes }, Native.JsonOptions), Stopwatch.GetTimestamp(), cancellationToken);

    private async Task<JsonDocument> QueryCoreAsync(Func<string> serialize, long started, CancellationToken cancellationToken, bool rawRequest = false)
    {
        cancellationToken.ThrowIfCancellationRequested();
        // Raw text already exists: validate its C-string representation before
        // admission can touch native state. Typed/user serialization still runs
        // only after reservation. The original start also charges this work.
        var raw = rawRequest ? Native.ValidateInteropText(serialize(), "request") : null;
        // This synchronous native call runs before user serialization and before
        // cancellation registrations or Task.Run can retain queued requests.
        var acquired = Native.Reserve(queryLimitScope);
        using var reservation = acquired.Reservation;
        if (cancellationToken.IsCancellationRequested)
        {
            acquired.Error?.Dispose();
            cancellationToken.ThrowIfCancellationRequested();
        }
        if (acquired.Error is not null) return acquired.Error;
        var request = rawRequest ? raw! : serialize();
        var maximum = Native.QueryMaxRequestBytes();
        if (System.Text.Encoding.UTF8.GetByteCount(request) > maximum)
            return JsonSerializer.SerializeToDocument(new { Error = new { Code = "invalid_request",
                Message = $"serialized query request exceeds {maximum} UTF-8 bytes" } }, Native.JsonOptions);
        cancellationToken.ThrowIfCancellationRequested();
        using var cancellation = cancellationToken.CanBeCanceled ? Native.CreateQueryCancellation() : null;
        using var registration = cancellationToken.CanBeCanceled
            ? cancellationToken.UnsafeRegister(static state => Native.QueryCancel((QueryCancellationHandle)state!), cancellation)
            : default;
        // Do not cancel Task.Run itself: queued/active native work must finish
        // before its registration, reservation and safe handles can be released.
        var result = await Task.Run(() => Native.Invoke(() => {
            var elapsed = (ulong)Stopwatch.GetElapsedTime(started).TotalMilliseconds;
            return cancellation is null
                ? Native.QueryReserved(database, request, elapsed, nint.Zero, reservation!)
                : Native.QueryReservedCancellable(database, request, elapsed, cancellation, reservation!);
        })).ConfigureAwait(false);
        // Native.Invoke copies, parses and frees the native result while this
        // method still owns the reservation. Delivered documents are caller-owned.
        if (result.RootElement.TryGetProperty("error", out var error)
            && error.GetProperty("code").GetString() == "cancelled")
        {
            result.Dispose();
            throw new OperationCanceledException(cancellationToken);
        }
        return result;
    }
}

internal sealed class QueryReservationHandle : SafeHandle
{
    internal QueryReservationHandle(nint value) : base(nint.Zero, true) { SetHandle(value); }
    public override bool IsInvalid => handle == nint.Zero;
    protected override bool ReleaseHandle() { Native.QueryReservationFree(handle); return true; }
}

internal sealed class QueryCancellationHandle : SafeHandle
{
    internal QueryCancellationHandle(nint value) : base(nint.Zero, true) { SetHandle(value); }
    public override bool IsInvalid => handle == nint.Zero;
    protected override bool ReleaseHandle() { Native.QueryCancelFree(handle); return true; }
}

[System.Text.Json.Serialization.JsonConverter(typeof(TestFilterJsonConverter))]
public enum TestFilter { All, Exclude, Only }
internal sealed class TestFilterJsonConverter() : System.Text.Json.Serialization.JsonStringEnumConverter<TestFilter>(JsonNamingPolicy.SnakeCaseLower, allowIntegerValues: false);

public sealed record QueryRequest(string Op, string? Name = null, string? Query = null,
    string? SymbolId = null, string? ArtifactId = null, string? Target = null, uint Limit = 100, uint FreshnessWaitMs = 500, uint? DeadlineMs = null,
    string[]? Kinds = null, bool IncludeImplicit = true, bool ExactOnly = false, uint? MaxDepth = null,
    uint? MaxEdges = null, uint? MaxNodes = null, uint? PathsPerSymbol = null, uint? MaxBytes = null, uint? TimeBudgetMs = null)
{
    public TestFilter? Tests { get; init; }
    public ulong? StartByte { get; init; }
    public ulong? EndByte { get; init; }
    public string? ContentHash { get; init; }
    public ulong? Generation { get; init; }
    public string? Snapshot { get; init; }
    public string? CompilationId { get; init; }
    /// <summary>Declaration source: symbol_context defaults true; callers/callees/impact default false.</summary>
    public bool? IncludeSource { get; init; }
    /// <summary>Graph-only aggregate UTF-8 source bytes across all findings, targets and returned copies. Default 16384; maximum 262144. Separate from MaxBytes and not renewed per page.</summary>
    public uint? SourceMaxBytes { get; init; }
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
    // Interop strings are NUL-terminated UTF-8. Reject text that would arrive
    // truncated or replacement-encoded; this does not validate JSON syntax.
    private static readonly System.Text.UTF8Encoding StrictUtf8 = new(false, true);
    internal static string ValidateInteropText(string value, string parameter)
    {
        ArgumentNullException.ThrowIfNull(value, parameter);
        if (value.Contains('\0')) throw new ArgumentException("Native text inputs cannot contain NUL.", parameter);
        try { _ = StrictUtf8.GetByteCount(value); }
        catch (System.Text.EncoderFallbackException error)
        { throw new ArgumentException("Native text inputs must contain valid Unicode.", parameter, error); }
        return value;
    }

    internal static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };
    internal static void CheckQueryContract()
    {
        try
        {
            if (AbiVersion() != 1 || QueryContractVersion() != 9)
                throw new KnowCodeException("update_required", "Update the Know Code managed/native packages together; compact query status, frozen-page headroom or shared query admission is missing or incompatible.");
        }
        catch (EntryPointNotFoundException error) { throw new KnowCodeException("update_required", $"Update the Know Code managed/native packages together: {error.Message}"); }
        catch (DllNotFoundException error) { throw new KnowCodeException("native_library_unavailable", $"Install the matching Know Code native library: {error.Message}"); }
    }
    internal static (QueryReservationHandle? Reservation, JsonDocument? Error) Reserve(QueryLimitScope? scope)
    {
        try
        {
            if (AbiVersion() != 1) throw new KnowCodeException("update_required", "Update the Know Code managed/native packages together; native ABI is incompatible.");
            CheckQueryContract();
            var error = QueryReserve(scope?.Directory, out var handle);
            if (error == nint.Zero)
            {
                if (handle == nint.Zero) throw new KnowCodeException("internal_error", "Know Code returned no query reservation.");
                return (new QueryReservationHandle(handle), null);
            }
            try { return (null, JsonDocument.Parse(Marshal.PtrToStringUTF8(error)
                ?? throw new KnowCodeException("internal_error", "Know Code returned a null reservation error."))); }
            finally { Free(error); }
        }
        catch (EntryPointNotFoundException error) { throw new KnowCodeException("update_required", $"Update the Know Code managed/native packages together: {error.Message}"); }
        catch (DllNotFoundException error) { throw new KnowCodeException("native_library_unavailable", $"Install the matching Know Code native library: {error.Message}"); }
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
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_max_request_bytes")]
    internal static partial uint QueryMaxRequestBytes();
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_reserve", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint QueryReserve(string? scope, out nint reservation);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_reservation_free")]
    internal static partial void QueryReservationFree(nint reservation);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_json_reserved", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint QueryReserved(string database, string request, ulong elapsedMs,
        nint cancellation, QueryReservationHandle reservation);
    [LibraryImport("know_code_ffi", EntryPoint = "kc_query_json_reserved", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint QueryReservedCancellable(string database, string request, ulong elapsedMs,
        QueryCancellationHandle cancellation, QueryReservationHandle reservation);
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
