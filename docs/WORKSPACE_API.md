# Native workspace clients

The user accepted finishing the .NET/Node workspace APIs next, automatic startup of an installed coordinator, and typed API errors while compiler errors remain returned status/coverage. This slice removes the need to locate an index database before using an embedded client. Graph queries still execute in the caller's process through the common Rust core.

## Open and use

.NET:

```csharp
using var workspace = await KnowCode.Workspace.OpenAsync("/absolute/repository");
var status = await workspace.WaitForIndexAsync(waitMs: 30_000);
if (status.Generation > 0)
{
    using var response = await workspace.QueryAsync(
        new KnowCode.QueryRequest("symbols", Name: "Greeter"));
    Console.WriteLine(response.RootElement.GetProperty("result"));
}
```

Node:

```js
import {Workspace} from '@know-code/node'; // package name/layout remain development-only
const workspace = await Workspace.open('/absolute/repository');
try {
  const status = await workspace.waitForIndex({waitMs: 30_000});
  if (status.generation > 0) {
    const {result} = await workspace.query({op: 'symbols', name: 'Greeter'});
    console.log(result);
  }
} finally { workspace.close(); }
```

In this checkout import the staged `.artifacts/build/{debug,release}/node/index.mjs`; the distribution package is not published. Opening starts registration/indexing and returns promptly with a canonical root and `InitialStatus` / `initialStatus`. It does **not** wait for dependency restoration or a usable index. Status is a snapshot, not a live mutable object; Node freezes the initial snapshot. Opening a symlink to the same workspace reuses its registration/index.

Both APIs expose `status`, `refresh`, bounded `waitForIndex`, and queries (PascalCase/`Async` on .NET). Status includes phase, availability, generation, observed/published saved-input epochs, analysis error and optional stage/project/message progress. Waiting defaults to 500 ms and accepts up to 120,000 ms; it returns status on completion, analysis failure, unavailability or timeout. A returned status is not a promise that every newer concurrent save has finished or that compiler coverage is complete. Inspect phase/epochs/generation and query freshness/coverage. A first query without a usable publication raises `not_ready`; a coherent older publication may be returned explicitly stale.

Refresh schedules a full refresh/preparation retry and returns its status rather than waiting for completion. Optional `configuration` changes the one persisted C# configuration for the workspace and therefore affects all consumers; omitting it preserves the existing choice. This is separate from Debug/Release builds of Know Code itself. Ordinary status/wait/query operations do not force dependency restoration.

## Startup and installation

The shared Rust control client connects to a running coordinator first. Only a missing/refused local socket permits automatic startup. Permission failures, malformed peers and incompatible protocols are returned; they do not trigger an attempted replacement. Existing sole-writer fencing handles concurrent processes starting together. Startup is serialized within each embedded core instance and bounded by a five-second readiness-poll window, in addition to individual control I/O timeouts.

For libraries the launch executable is selected by `WorkspaceOptions.CoordinatorExecutable` / `coordinatorExecutable`, then `KNOW_CODE_COORDINATOR`, then `know-code` on PATH. A directory-relative explicit executable is resolved once when opening and retained for reconnection. The library never treats the embedding application's executable (`dotnet`/`node`) as the coordinator. CLI/MCP attachment now uses this same Rust path, supplying its known current host executable. There is no shell invocation.

Default state remains centrally per user under `~/Library/Application Support/Know Code/`. Optional `DataDirectory` / `dataDirectory` overrides that default; `KNOW_CODE_DATA_DIR` remains supported for development/test isolation. The returned coordinator database identity determines the effective state root for later calls. Child startup sets its own environment; it never changes the embedding host's global environment. An absent executable raises `coordinator_unavailable` with installation/override instructions. Node.js and .NET SDKs remain separately installed prerequisites; this API does not install them. Executable-relative runtime discovery and native SDK candidate archives are now verified outside the checkout; see [packaging](PACKAGING.md). Stable installation/update/login lifecycle and final distribution approval remain release work.

Control operations reconnect/start after coordinator loss. Ordinary graph queries use existing in-process freshness/stale fallback and do not launch a process on every read. Apps can call status/open to restore coordination. No protocol or storage/query logic is implemented in JS/C#.

## Errors and ownership

.NET `KnowCodeException.Code` and Node `KnowCodeError.code` expose the common core error code. Bootstrap, control and query request failures throw these errors through the new Workspace API. Compiler diagnostics remain coverage, and background analyzer/setup failures remain visible in status with prior facts explicitly stale. A successful control call returning a failed analysis phase is not a transport exception. Host argument/JSON-serialization errors can still be ordinary host exceptions.

Successful queries retain the shared `{result: ...}` envelope. .NET returns an owned `JsonDocument`, which the caller disposes; Node returns the equivalent parsed object. `.NET QueryJsonAsync` permits advanced raw JSON using the same operators. Existing low-level `Index(database)` APIs remain available and retain result/error envelopes; they are not the new workspace lifecycle API.

C ABI v1 remains additive: `kc_workspace_json` joins `kc_query_json` and both return owned UTF-8 JSON freed exactly once with `kc_string_free`. Additive `kc_query_json_elapsed` charges host queue time through the same query path and ownership contract. Null/invalid UTF-8 inputs and native panics return error envelopes. The .NET wrapper centralizes ABI checks, decoding and `finally` release. A missing workspace export reports update-required; Node likewise checks for the new native operation. Protocol v7 and store v6 are unchanged. This does not settle every future DTO/version negotiation issue.

`Dispose()` / `close()` is idempotent, rejects new operations with code `closed`, and releases only that client's admission. Work already accepted can finish using its owned inputs/results. Closing does not unregister/delete a workspace, stop the coordinator or cancel shared indexing. No native workspace handle, persistent query connection or retained reader is created: each call owns/releases its socket/read view. Consequently closing needs no finalizer or persistent background polling thread. If an embedded host actually launches a coordinator child, one 64 KiB-stack reaper thread waits for that child's eventual exit without altering the host's SIGCHLD policy; the normal login-service path creates none. Thread stack size is not a total-memory guarantee.

Query deadline_ms now defaults to 5,000 ms, accepts 0–120,000 ms and covers host preparation/queue time through core execution/JSON encoding. Expiry raises typed deadline_exceeded through Workspace (error envelopes through low-level Index), releases readers and leaves indexing running. These SDKs require the new elapsed-query native export; missing exports return update_required. Enforcement is cooperative; see [exact boundaries and tests](QUERY_LIFECYCLE.md). CancellationToken/AbortSignal support, bounded admission across all native requests, outer transport limits, final DTOs and cursors remain release work. `response_max_bytes` / .NET `ResponseMaxBytes` bounds the complete core JSON envelope (256 KiB default, 1 KiB–2 MiB range), including metadata and source. Workspace raises typed `response_limit_exceeded`; low-level Index retains the error envelope. SDKs require native query contract version 1 and report update_required for missing/incompatible support. This is separate from existing source/detail budgets and does not implement pagination or a peak-memory bound. Closing is not preemptive cancellation and must not be presented as such.

## Verification

`tests/workspace_api.py` drives long-lived clients using the public .NET/Node APIs and isolated durable state, covering missing executables/roots, startup from both hosts, symlink deduplication, matching CLI/native results, compiler context/configuration, typed errors, refreshes, fresh partial compiler-error results, closed/in-flight ownership, indexing without clients, offline graph reads, restart reconciliation, missing-root retention and incompatible-peer nonreplacement. It runs in `scripts/test.sh` and also supports the Release profile. Build/test artifacts remain in `.artifacts/workspace-api/` and `.artifacts/wa/`.

Debug/Release builds pass. The full Debug `scripts/test.sh` passes, including 42 Rust tests and the new 20-case workspace lifecycle suite. Release workspace tests (20 cases), MCP tests (22 cases), all 42 optimized Rust tests, formatting and all-target Clippy with warnings denied pass. A real TypeScript consumer compiles against the public declarations, including readonly/unknown-option type checks.

The installed development LaunchAgent was reloaded with its plist unchanged. Both public SDKs opened this checkout by root, read status, searched for the new .NET Workspace class and read its bounded compiler context with identical current results. No database path was supplied to either SDK. ts-pattern/Polly generations 34/12 and this checkout's generation 105 stayed idle/error-free; these are observations, not fixed current generations. Evidence: `.artifacts/workspace-api/1794243299585353000/validation.json`, `installed.json`, retained review diffs and `.artifacts/workspace-api/verify.sh`.

These checks establish this development API slice, not a published or complete 0.0.1 release. The accepted real-encoding performance gate remains pending.

## Explicit background indexing cancellation

Node `workspace.cancelIndexing()` and .NET `workspace.CancelIndexingAsync()` stop shared workspace analysis and return its current status. They are explicit operations affecting every consumer of that workspace. Saved changes or refresh resume work. Disposing/closing clients and canceling an individual request do not cancel the background job. `outcome_code` distinguishes cancellation/resource exhaustion, with coherent stale fallback or not-ready when no prior index exists. See [resource policy, publication ordering and limits](RESOURCE_CONTROL.md). Older running coordinators need an explicit update before these operations are available. Native query cancellation remains separate unfinished work.
