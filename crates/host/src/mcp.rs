use know_code_core::{
    admission, control, execution,
    protocol::{ControlRequest, WorkspaceStatus},
    query,
};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::*,
    service::{RequestContext, RoleServer},
    transport::{async_rw::AsyncRwTransport, stdio_pipes},
};
use serde_json::{Map, Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
#[path = "mcp_transport.rs"]
mod bounded;

pub fn run(data: PathBuf, workspace: WorkspaceStatus) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .build()?;
    let result = runtime.block_on(async {
        let (input, output) = stdio_pipes();
        let connection = Arc::new(bounded::Connection::default());
        let transport = AsyncRwTransport::<RoleServer, _, _>::new(
            input,
            bounded::Output::new(output, connection.clone()),
        )
        .with_max_line_length(64 * 1024);
        let transport = bounded::Bounded::new(transport, connection.clone());
        let server = Mcp {
            data,
            database: workspace.database,
            tools: Arc::new(tools()),
            slots: Arc::new(Semaphore::new(2)),
            connection: connection.clone(),
        };
        let server = bounded::ResponseService::new(server, connection);
        match server.serve(transport).await {
            Ok(server) => {
                server.waiting().await?;
            }
            Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => {}
            Err(error) => return Err(error.into()),
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    });
    // A closed MCP pipe never owns or stops the persistent coordinator. Do not
    // wait indefinitely for Tokio's blocking stdin or an already-running query.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

struct Mcp {
    data: PathBuf,
    database: String,
    tools: Arc<Vec<Tool>>,
    slots: Arc<Semaphore>,
    connection: Arc<bounded::Connection>,
}
fn failure(code: &str, message: impl ToString) -> CallToolResponse {
    CallToolResult::structured_error(json!({"error":{"code":code,"message":message.to_string()}}))
        .into()
}
struct CancelOnDrop(execution::QueryCancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("know-code",env!("CARGO_PKG_VERSION")))
            .with_instructions("Query the explicitly selected workspace's saved, indexed code. Check workspace_status while initial indexing runs. Results expose freshness, partial coverage, targets and uncertainty; possible dispatch is not proof of runtime execution. Use indexed artifact IDs and content hashes for source excerpts. When reusing a generation, forward its snapshot token. Disconnecting leaves background indexing running.")
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|tool| tool.name == name).cloned()
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|r| r.cursor.is_some()) {
            return Err(ErrorData::invalid_params(
                "tool list has no continuation cursor",
                None,
            ));
        }
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let start = know_code_core::execution::QueryStart::default();
        let name = request.name.as_ref();
        if self.get_tool(name).is_none() {
            return Err(ErrorData::invalid_params("unknown Know Code tool", None));
        }
        if context.ct.is_cancelled() {
            return Ok(failure("cancelled", "request cancelled"));
        }
        let cancellation = self.connection.query_cancellation();
        let _cancel_on_exit = CancelOnDrop(cancellation.clone());
        // No application queue: permits live inside the blocking work, including
        // after SDK cancellation drops its waiter. Cancellation cannot multiply work.
        let Ok(permit) = self.slots.clone().try_acquire_owned() else {
            return Ok(failure(
                "busy",
                "two workspace operations are already running; retry later",
            ));
        };
        let mut arguments = request.arguments.unwrap_or_default();
        let status = name == "workspace_status";
        if status {
            if !arguments.is_empty() {
                return Ok(failure(
                    "invalid_request",
                    "workspace_status takes no arguments",
                ));
            }
        } else if name == "query_graph" {
            if !matches!(
                arguments.get("op").and_then(Value::as_str),
                Some("callers" | "callees" | "impact")
            ) {
                return Ok(failure(
                    "invalid_request",
                    "query_graph op must be callers, callees, or impact",
                ));
            }
        } else {
            if arguments.contains_key("op") {
                return Ok(failure(
                    "invalid_request",
                    "focused tools select their own operation",
                ));
            }
            let op = match name {
                "search_symbols" => "symbols",
                "find_references" => "references",
                "read_source" => "source",
                "coverage_details" => "coverage",
                other => other,
            };
            arguments.insert("op".into(), json!(op));
        }
        // One account-wide charge begins before serialization/dispatch and is
        // shared with the blocking task and the eventual transport response.
        let reservation = if status {
            None
        } else {
            match admission::AdmissionScope::local().and_then(|scope| scope.try_reserve()) {
                Ok(reservation) => {
                    let reservation = Arc::new(reservation);
                    let Some(key) = context.extensions.get::<bounded::ResponseKey>() else {
                        return Err(ErrorData::internal_error(
                            "MCP request ownership is missing",
                            None,
                        ));
                    };
                    self.connection
                        .retain_response(key, reservation.clone())
                        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
                    Some(reservation)
                }
                Err(error) => return Ok(failure(error.code(), error.to_string())),
            }
        };
        let encoded = Value::Object(arguments).to_string();
        if !status && let Err(error) = admission::check_request_bytes(encoded.len()) {
            return Ok(failure(error.code(), error.to_string()));
        }
        let database = self.database.clone();
        let data = self.data.clone();
        let worker_cancellation = cancellation.clone();
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if status {
                match control::request_cancellable(
                    &data,
                    ControlRequest {
                        operation: "status".into(),
                        database,
                        ..Default::default()
                    },
                    start,
                    worker_cancellation,
                ) {
                    Ok(response) if !response.workspaces.is_empty() => {
                        CallToolResult::structured(json!({"result":response})).into()
                    }
                    Ok(_) => failure("not_ready", "selected workspace is no longer registered"),
                    Err(error) => failure(error.code(), &error),
                }
            } else {
                let result: Value = serde_json::from_str(&know_code_core::query_json_reserved(
                    &database,
                    &encoded,
                    start,
                    Some(worker_cancellation),
                    reservation.as_ref().expect("query reservation"),
                ))
                .expect("core query emits JSON");
                if result.get("error").is_some() {
                    CallToolResult::structured_error(result).into()
                } else {
                    CallToolResult::structured(result).into()
                }
            }
        });
        tokio::select! {
            _=context.ct.cancelled()=>{
                cancellation.cancel();
                Ok(failure("cancelled","request cancelled; its slot is released when core cleanup finishes"))
            },
            value=task=>value.map_err(|error|ErrorData::internal_error(error.to_string(),None)),
        }
    }
}

fn schema(keys: &[&str], required: &[&str]) -> Map<String, Value> {
    let common = json!({
        "tests":{"type":"string","enum":["all","exclude","only"],"default":"all","description":"Filter findings by declaration/use evidence. Unknown is retained by exclude; traversal remains unfiltered."},
        "name":{"type":"string","description":"Exact, case-sensitive symbol name."},
        "query":{"type":"string","description":"Lexical search text, not semantic graph evidence."},
        "symbol_id":{"type":"string","description":"Exact member ID from a prior lookup. Follow-up operations include equivalent members unless narrowed by target or compilation_id."},
        "artifact_id":{"type":"string","description":"Indexed artifact ID from a source location."},
        "generation":{"type":"integer","minimum":1,"description":"Expected served generation; pass its snapshot token on managed reads."},
        "snapshot":{"type":"string","description":"Opaque snapshot token from the result whose generation is being expanded."},
        "compilation_id":{"type":"string","description":"Narrow findings or a symbol follow-up to one concrete compilation."},
        "include_source":{"type":"boolean","default":false,"description":"Include compiler declaration context on returned graph findings and their exact supporting targets; roots and path edges remain compact."},
        "source_max_bytes":{"type":"integer","minimum":1,"maximum":query::MAX_SOURCE_BYTES,"default":query::DEFAULT_SOURCE_BYTES,"description":"Graph-only aggregate indexed UTF-8 source bytes across the full result, all targets and every returned copy. Separate from max_bytes (graph JSON); pagination does not refill it."},
        "coverage_detail":{"type":"string","enum":["summary","full"],"default":"summary"},
        "detail_max_bytes":{"type":"integer","minimum":1,"maximum":query::MAX_SOURCE_BYTES,"default":query::DEFAULT_SOURCE_BYTES},
        "content_hash":{"type":"string","description":"Indexed location's content hash; required for byte ranges."},
        "start_byte":{"type":"integer","minimum":0},"end_byte":{"type":"integer","minimum":0},
        "target":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":1000,"default":100},
        "page_size":{"type":"integer","minimum":1,"maximum":1000,"description":"Opt into repeatable generation-pinned pages; limit remains the total result cap."},
        "cursor":{"type":"string","description":"Opaque page token returned by a prior query. Expiry, coordinator restart or retention eviction requires restarting the original query."},
        "freshness_wait_ms":{"type":"integer","minimum":0,"maximum":120000,"default":500},
        "deadline_ms":{"type":"integer","minimum":0,"maximum":execution::MAX_DEADLINE_MS,"default":execution::DEFAULT_DEADLINE_MS,"description":"Overall query budget, including freshness, execution and core response encoding; expiry returns deadline_exceeded."},
        "response_max_bytes":{"type":"integer","minimum":execution::MIN_RESPONSE_BYTES,"maximum":execution::MAX_RESPONSE_BYTES,"default":execution::DEFAULT_RESPONSE_BYTES,"description":"Complete core JSON response budget, including metadata/source; excludes outer MCP framing. Exceeding it returns response_limit_exceeded."},
        "include_implicit":{"type":"boolean","default":true},"exact_only":{"type":"boolean","default":false},
        "kinds":{"type":"array","items":{"type":"string","enum":["call","reference","type_reference","inherits","imports","implements","overrides","contains","initializes"]}},
        "max_depth":{"type":"integer","minimum":1},"max_edges":{"type":"integer","minimum":1},
        "max_nodes":{"type":"integer","minimum":1},"paths_per_symbol":{"type":"integer","minimum":1},
        "max_bytes":{"type":"integer","minimum":1},"time_budget_ms":{"type":"integer","minimum":1},
        "op":{"type":"string","enum":["callers","callees","impact"]}
    });
    let properties: Map<_, _> = keys
        .iter()
        .map(|key| ((*key).to_string(), common[*key].clone()))
        .collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}).as_object().unwrap().clone()
}
fn tools() -> Vec<Tool> {
    let waits = [
        "target",
        "compilation_id",
        "limit",
        "page_size",
        "freshness_wait_ms",
        "deadline_ms",
        "response_max_bytes",
        "generation",
        "snapshot",
        "coverage_detail",
        "detail_max_bytes",
    ];
    let mut search = vec!["name", "query", "tests"];
    search.extend(waits);
    let mut references = vec!["symbol_id", "tests"];
    references.extend(waits);
    let mut graph = references.clone();
    graph.extend([
        "kinds",
        "include_implicit",
        "exact_only",
        "max_depth",
        "max_edges",
        "max_nodes",
        "paths_per_symbol",
        "include_source",
        "source_max_bytes",
        "max_bytes",
        "time_budget_ms",
    ]);
    let make = |name, description, input| {
        let mut tool = Tool::new(name, description, input);
        tool.annotations = Some(ToolAnnotations::new().read_only(true).open_world(false));
        tool
    };
    let mut source = schema(
        &[
            "artifact_id",
            "cursor",
            "content_hash",
            "start_byte",
            "end_byte",
            "max_bytes",
            "freshness_wait_ms",
            "deadline_ms",
            "response_max_bytes",
            "generation",
            "snapshot",
            "coverage_detail",
            "detail_max_bytes",
        ],
        &["artifact_id"],
    );
    source["properties"]["max_bytes"] = json!({"type":"integer","minimum":1,"maximum":query::MAX_SOURCE_BYTES,"default":query::DEFAULT_SOURCE_BYTES});
    let mut context = references.clone();
    context.extend(["include_source", "max_bytes", "cursor"]);
    let mut context_schema = schema(&context, &["symbol_id"]);
    context_schema["properties"]["max_bytes"] = source["properties"]["max_bytes"].clone();
    context_schema["properties"]["include_source"] = json!({"type":"boolean","default":true,"description":"Include bounded indexed source for compiler declaration contexts."});
    let mut coverage_schema = schema(
        &[
            "generation",
            "snapshot",
            "compilation_id",
            "limit",
            "page_size",
            "max_bytes",
            "freshness_wait_ms",
            "deadline_ms",
            "response_max_bytes",
        ],
        &[],
    );
    coverage_schema.insert(
        "anyOf".into(),
        json!([{"required":["generation"]},{"required":["snapshot"]}]),
    );
    let mut result = vec![
        make(
            "symbol_context",
            "Inspect a symbol's signature, compiler declaration extents and bounded indexed source. Multiple declarations stay distinct; unavailable extents are explicit. Lines are 1-based; byte ranges are UTF-8, end-exclusive.",
            context_schema,
        ),
        make(
            "coverage_details",
            "Expand coverage options and diagnostics for an exact served snapshot (or generation plus snapshot). max_bytes bounds detail payloads; provenance/counts remain visible when clipped.",
            coverage_schema,
        ),
        make(
            "next_page",
            "Read or replay a saved page using its opaque cursor. The original scope, projection, page size and graph budgets cannot change; freshness may become stale while its generation stays pinned.",
            schema(
                &["cursor", "deadline_ms", "response_max_bytes"],
                &["cursor"],
            ),
        ),
        make(
            "workspace_status",
            "Indexing progress, failures and saved-state epochs for the selected workspace.",
            schema(&[], &[]),
        ),
        make(
            "search_symbols",
            "Find symbols, signatures and declaration locations by exact name or lexical text.",
            schema(&search, &[]),
        ),
        make(
            "find_references",
            "Find compiler-backed references to an indexed symbol, with source and target evidence.",
            schema(&references, &["symbol_id"]),
        ),
        make(
            "callers",
            "Find callers, including labeled implicit and possible calls by default.",
            schema(&graph, &["symbol_id"]),
        ),
        make(
            "callees",
            "Find callees with supporting callsites and explicit external/unresolved boundaries.",
            schema(&graph, &["symbol_id"]),
        ),
        make(
            "impact",
            "Find affected symbols with evidence-bearing, compilation-compatible dependency paths.",
            schema(&graph, &["symbol_id"]),
        ),
        make(
            "read_source",
            "Read bounded indexed UTF-8 source; byte ranges require the location's content_hash. end_byte is exclusive.",
            source,
        ),
    ];
    graph.push("op");
    result.push(make("query_graph","Bounded structured graph read over the shared callers, callees and impact operators. No SQL or textual query language.",schema(&graph,&["op","symbol_id"])));
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}
