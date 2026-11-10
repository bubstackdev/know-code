# CLI/MCP workflow

Status: first stdio workflow implemented in development, with focused tools, a bounded structured graph read and source excerpts using the common Rust query operators. This is part of completing 0.0.1; [release readiness](RELEASE_PLAN.md) still includes operational, query and packaging work.

## Start with an explicit workspace

Have the agent launch the built executable with:

```sh
/absolute/path/to/know-code mcp --workspace /absolute/path/to/repository
# Optional: choose the workspace's C# configuration.
/absolute/path/to/know-code mcp --workspace /absolute/path/to/repository --configuration Release
```

For this checkout, build with `bash scripts/build.sh` or `bash scripts/build.sh release` and use `target/debug/know-code` or `target/release/know-code`. The process speaks MCP on stdin/stdout; running it alone in a terminal waits for a client. Configure the executable and arguments in the chosen agent's MCP settings. Client-specific setup remains separate from this command contract.

The user accepted automatic registration/indexing of the explicitly selected workspace. Startup canonicalizes that directory and uses the same attachment helper as CLI index/watch, then the existing Rust coordinator control client. Registration starts analysis in the persistent coordinator without waiting for the initial compiler job to finish. `workspace_status` exposes progress, availability, saved-input epochs and failures. A first query without a usable publication returns `not_ready`; it does not fabricate an empty fresh graph. Existing registrations/indexes are reused without a forced refresh, and an omitted configuration preserves an existing workspace's choice. An explicit configuration changes that workspace-wide choice for all consumers; it is not a separate per-MCP compilation.

An explicit `--workspace` is required. There is no implicit scan of the launch directory or client-provided roots. Explicit symlink paths canonicalize through the existing registry identity path. Directory workspaces without Git also support the existing inferred-project behavior. Parent-folder discovery remains separate unfinished release work. Node/.NET prerequisites, dependency restoration, compiler selection and partial coverage use existing providers; the MCP process does not create another analysis pool.

Closing stdin exits the MCP process and leaves the coordinator/indexing registration active, including when the client closes before protocol initialization. Saved changes continue to index with no MCP consumer connected. Login installation and portable release packaging remain separate from this development workflow.

## Tool surface

Every tool is scoped to the selected workspace and marked read-only. Tools do not accept database paths or SQL. Required compiler/source/target/freshness evidence is preserved in the core result. Text and structured MCP content contain the same JSON envelope, including core errors. Check `isError`, freshness and coverage rather than treating a successful transport response as a complete/current graph.

| Tool | Shared operation | Main input |
| --- | --- | --- |
| `workspace_status` | Coordinator status | No arguments |
| `search_symbols` | `symbols` | Optional exact `name`, lexical `query`, target/result/freshness limits |
| `symbol_context` | `symbol_context` | Symbol ID to signature, compiler declaration extents and aggregate bounded source |
| `coverage_details` | `coverage` | Exact generation, optional compilation selector and bounded diagnostic/option payloads |
| `find_references` | `references` | Opaque `symbol_id`, optional target/result/freshness limits |
| `callers` | `callers` | Symbol and shared graph filters/budgets |
| `callees` | `callees` | Symbol and shared graph filters/budgets |
| `impact` | `impact` | Symbol and shared dependency/path filters/budgets |
| `read_source` | `source` | Indexed `artifact_id`, optional hash-guarded byte range and byte budget |
| `query_graph` | The same callers/callees/impact operators | Structured `op` plus the same symbol, filters and budgets |

`query_graph` currently dispatches one of those three bounded read operators; it introduces no textual language, joins, storage access or second execution engine. Focused and structured results match for equivalent requests. Implicit calls and possible dispatch retain existing defaults/labels; exact-only and dependency-kind filters preserve the core semantics and compilation witnesses. Lexical matches remain distinct from compiler evidence.

## Shared source excerpts

The core `source` operator now returns at most **16 KiB by default**, configurable with `max_bytes` from 1 through **256 KiB**. These are implementation defaults, not accepted hardware/performance guarantees. This behavior is shared by CLI, MCP, .NET and Node; source results add `start_byte`, `end_byte`, `total_bytes` and indexed `start_line`, retaining the full indexed artifact's `content_hash`.

A source location from a graph/symbol/reference result supplies `artifact_id`, `content_hash` and UTF-8 byte coordinates. Pass the content hash whenever requesting `start_byte` or `end_byte`; `end_byte` is exclusive. Omitting end reads forward from start within the byte budget. A range may be empty, must be within indexed text and must end/start on UTF-8 boundaries. Budget truncation stops at a complete character; a budget too small for the next character produces an actionable error. `truncated` describes the requested range, and the returned coordinates identify the actual excerpt.

Hash mismatch rejects reuse of an old range after the indexed source changes. Callers must restart lookup; no current disk text is substituted. Pinned read views retain their own generation's source, as before. SQL selects a bounded window for Rust to materialize rather than returning an unbounded source string to the host; this is not a claim that SQLite internally reads no other pages. Missing artifacts produce an actionable restart-lookup error.

The added JSON request/result fields need matching current query artifacts; a complete published query-capability compatibility policy remains release work. The existing .NET positional request constructor is preserved; new range/hash inputs are optional init properties. Worker Protobuf v7, store v6 and C ABI v1 remain unchanged.

Standalone source excerpts do not complete inline graph-body projection, test filters, cross-target presentation, generation-pinned pagination or total serialized-response budgets. Those remain tracked in the release ledger. Source-body string/comment search is separate unassigned backlog work; symbol/signature search is sufficient for 0.0.1.

## Transport and execution limits

The CLI host pins the [official Rust SDK, rmcp 3.9.0](https://docs.rs/rmcp/3.9.0/rmcp/), with only server/stdio features. HTTP/auth/client transports and macros are disabled. These dependencies do not enter the embedded core's required dependency tree. SDK lifecycle/framing/negotiation are reused rather than implemented again.

Real wire tests cover the 2025-11-25 initialize/initialized lifecycle and the 2026-07-28 discovery/per-request metadata lifecycle. The [official SDK documentation](https://github.com/modelcontextprotocol/rust-sdk) describes both; the [current protocol changes](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/changelog.mdx) define per-request metadata and discovery. SDK advertisement of other compatible versions is not a claim that every client/version has been tested here. Worker length-prefixed Protobuf is a separate boundary from newline-delimited MCP JSON-RPC.

Incoming MCP lines are bounded at 64 KiB. The SDK discards oversized lines and keeps the connection usable; a sender exceeding the limit may wait for a response that is not produced. This transport input limit is separate from result size. Query tools now expose response_max_bytes for the complete shared core JSON result (256 KiB default, 1 KiB–2 MiB); over-budget results return response_limit_exceeded. MCP framing, text/structured duplication and output backpressure remain outside that core cap and need their own release policy, as does process-memory admission.

One current-thread async runtime handles protocol work. At most two shared-core/status operations execute concurrently per MCP process, with immediate `busy` responses instead of an application work queue. Four blocking-thread slots cover stdio and those operations. A canceled request can stop waiting, but already-running core work keeps its permit until completion; cancellation cannot repeatedly detach unbounded query jobs. The shared query deadline (5 seconds default, up to 120 seconds) now charges blocking dispatch, freshness, SQL interruption and core JSON encoding; expiry returns deadline_exceeded. Prompt interruption from MCP cancellation, outer result framing/output backpressure and global per-user query admission remain unfinished. See [query lifecycle](QUERY_LIFECYCLE.md). Runtime shutdown is bounded, and no MCP disconnect sends coordinator shutdown.

The host compiles SDK tracing off in Debug/Release and installs no subscriber, collector, log-level control or uploader. Required protocol metadata, requested results and transient scheduler state remain functional. Ordinary startup errors go to stderr; stdout is exclusively MCP. This adds no Know Code usage recorder or hidden telemetry opt-in.

## Verification

`tests/mcp.py` runs real TS/C# indexing in a durable isolated workspace and compares complete MCP/CLI/Node/.NET results. It checks auto-registration/configuration, explicit selection, EOF before initialization, both tested lifecycles, deterministic read-only schemas, source bounds/hash mismatch, focused/structured parity, visible input errors/no SQL surface, a canceled waiter's retained work slot, oversized input handling, saved updates after disconnect and reconnect without forced reanalysis.

Rust publication tests additionally exercise Unicode boundaries, default/caller source bounds, hash/range errors and coherent old-source reads across publication. All retained test data stays beneath `.artifacts/`. This is development evidence, not a published-package acceptance or real-encoding performance result.

### Development validation closeout (2026-11-09)

Both Debug/Release builds, the full Debug test script (including MCP), optimized Rust workspace/MCP tests, formatting and all-target Clippy with warnings denied pass. The embedded core's package dependency set is unchanged. The optimized CLI is 6,880,800 bytes versus 4,125,216 bytes for the preceding activity-only host; this is artifact size, not a runtime memory measurement.

The existing development coordinator was reloaded with unchanged plist/ProcessType. Eighty current Rust/Node/.NET correctness calls matched before/after reload. Real ts-pattern and Polly MCP searches and hash-guarded declaration excerpts match the CLI at retained generations 33 and 11; both MCP disconnects leave the normal coordinator serving. No index rebuild or MCP-client configuration change was needed for these checks. Actual agent-app dogfooding, portable packaging and the real-encoding gate remain open.

Logs, dependency comparison, installed responses, final source/diff and validation records are retained under `.artifacts/mcp/1794236901404139000/`. Test sessions and their wire evidence are under `.artifacts/mc/`; those are explicit development evidence, not product usage collection.

## Local Codex connection (2026-11-09)

The user selected this Codex conversation as the first agent client. Know Code is now configured as `know_code` in this checkout's machine-local `.codex/config.toml`, with the absolute optimized executable and this repository as the explicit workspace. It is scoped to this trusted project. The file is ignored by Git, and global `~/.codex/config.toml` is unchanged. The launcher supplies user-scoped toolchain PATH entries and .NET CLI telemetry opt-out for coordinator startup if needed.

This uses the [official project-scoped MCP configuration](https://learn.chatgpt.com/docs/extend/mcp?surface=cli). The existing Codex 0.167.0 app-server accepted its documented [`config/mcpServer/reload`](https://learn.chatgpt.com/docs/app-server) API. Initial discovery on the current thread reported all eight tools; the tools then appeared directly in the active model tool list. No app restart, new model task or global tool-approval policy change was used.

Verification went through both the thread's actual MCP connection (`mcpServer/tool/call`) and direct model calls to `workspace_status`, `search_symbols` and `read_source`. The workspace was idle/available, and lookup plus hash-guarded excerpt returned current generation 92, including the exact `ReadView` declaration text from `crates/core/src/query.rs`. Rust coverage remains explicitly partial; a successful navigation check does not promote experimental Rust call-graph support into 0.0.1.

Connection configuration, generated version-specific app-server schema, reload/discovery responses and actual model tool results are retained under `.artifacts/codex/1794239021668706000/`. The one-off verification used the documented Unix WebSocket transport. A raw `codex app-server proxy` is not a JSONL-to-WebSocket adapter; the initial JSONL proxy probe timed out and was closed. The successful verification used HTTP Upgrade/WebSocket framing on the existing local control socket. These are setup artifacts, not a new product protocol layer or retained usage collector.

Initial client connection is verified. Continue dogfooding real code questions through the MCP tools, respecting freshness/coverage and using file tools when indexed capabilities are insufficient. Portable client setup and broader packaged-client acceptance remain release work.

## Compact context and coverage

Ordinary reads now return compact relevant reports and an explicit whole-workspace coverage summary. Full diagnostic/option payloads are available through generation-required `coverage_details`; the shared native request also accepts `coverage_detail:"full"`. `symbol_context` supplies a signature, distinct compiler declarations, indexed line numbers and optional bounded source in one lookup. Unsupported/ambiguous extents remain explicit. See [budgets, fields, compiler evidence and compatibility](SYMBOL_CONTEXT.md).

The context follow-on discovers ten tools in this active conversation. Reloading an unchanged configuration initially reused the old process; briefly disabling/re-enabling this project server and calling the same reload API replaced it. The project configuration was restored byte-for-byte and global settings were untouched. Actual current-generation `symbol_context` and `coverage_details` calls are retained with the [payload and native verification](SYMBOL_CONTEXT.md#verification-and-measurements).
