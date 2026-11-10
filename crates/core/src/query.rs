use crate::{Error, Result, protocol::Diagnostic, store::SCHEMA_VERSION};
use rusqlite::{Connection, OpenFlags, OptionalExtension, config::DbConfig, params};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

pub const DEFAULT_SOURCE_BYTES: u32 = 16 * 1024;
pub const MAX_SOURCE_BYTES: u32 = 256 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    pub op: String,
    pub name: Option<String>,
    pub query: Option<String>,
    pub symbol_id: Option<String>,
    pub artifact_id: Option<String>,
    pub target: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default = "default_freshness_wait")]
    pub freshness_wait_ms: u32,
    pub deadline_ms: Option<u32>,
    pub response_max_bytes: Option<u32>,
    pub kinds: Option<Vec<crate::graph::DependencyKind>>,
    #[serde(default = "default_true")]
    pub include_implicit: bool,
    #[serde(default)]
    pub exact_only: bool,
    pub max_depth: Option<u32>,
    pub max_edges: Option<u32>,
    pub max_nodes: Option<u32>,
    pub paths_per_symbol: Option<u32>,
    pub max_bytes: Option<u32>,
    pub time_budget_ms: Option<u32>,
    pub start_byte: Option<u64>,
    pub end_byte: Option<u64>,
    pub content_hash: Option<String>,
    pub generation: Option<u64>,
    pub compilation_id: Option<String>,
    #[serde(default)]
    pub coverage_detail: CoverageDetail,
    pub detail_max_bytes: Option<u32>,
    #[serde(default = "default_true")]
    pub include_source: bool,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CoverageDetail {
    #[default]
    Summary,
    Full,
}
fn default_true() -> bool {
    true
}
fn default_freshness_wait() -> u32 {
    500
}
fn default_limit() -> u32 {
    100
}

#[derive(Clone, Debug, Serialize)]
pub struct Location {
    pub artifact_id: String,
    pub path: String,
    pub content_hash: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub generated: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct SymbolResult {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub external: bool,
    pub compilation_id: String,
    pub target: String,
    pub locations: Vec<Location>,
    #[serde(skip)]
    pub(crate) owner_generation: u64,
    #[serde(skip)]
    pub(crate) locations_limited: bool,
}
#[derive(Debug, Serialize)]
pub struct OccurrenceResult {
    pub symbol_id: String,
    pub role: String,
    pub compilation_id: String,
    pub target: String,
    pub location: Location,
}
#[derive(Debug, Serialize)]
pub struct Coverage {
    pub compilation_id: String,
    pub language: String,
    pub project_path: String,
    pub target: String,
    pub configuration: String,
    pub inferred: bool,
    pub compiler_version: String,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub analysis_options: serde_json::Value,
    pub status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    pub diagnostic_counts: BTreeMap<String, usize>,
    pub details_omitted: bool,
    pub capabilities: BTreeMap<String, String>,
}
#[derive(Debug, Serialize)]
pub struct SourceResult {
    pub artifact_id: String,
    pub path: String,
    pub content_hash: String,
    pub text: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub total_bytes: u64,
    pub start_line: u64,
}
#[derive(Debug, Default, Serialize)]
pub struct WorkspaceCoverage {
    pub compilations: usize,
    pub statuses: BTreeMap<String, usize>,
    pub diagnostic_counts: BTreeMap<String, usize>,
}
#[derive(Debug, Serialize)]
pub struct DeclarationContext {
    pub location: Location,
    pub line: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extent: Option<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceResult>,
    pub source_status: &'static str,
}
#[derive(Debug, Serialize)]
pub struct SymbolContext {
    pub symbol: SymbolResult,
    pub declarations: Vec<DeclarationContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<&'static str>,
}
#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub generation: u64,
    // An explicit refresh prototype cannot claim it observes later saved files.
    pub freshness: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordination: Option<crate::protocol::WorkspaceStatus>,
    pub workspace_coverage: WorkspaceCoverage,
    pub coverage_scope: &'static str,
    pub coverage: Vec<Coverage>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<SymbolResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub occurrences: Vec<OccurrenceResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<crate::graph::GraphResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<SymbolContext>,
}

pub struct ReadView {
    pub(crate) connection: Connection,
    pub(crate) format_version: i32,
    pub generation: u64,
    execution: Arc<Mutex<Option<crate::execution::Execution>>>,
    expired: Cell<bool>,
}
impl ReadView {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let execution = crate::execution::Execution::new(Default::default(), None)?;
        let view = Self::open_controlled(path, execution)?;
        *view.execution.lock().unwrap() = None;
        Ok(view)
    }
    pub(crate) fn open_controlled(
        path: impl AsRef<Path>,
        execution: crate::execution::Execution,
    ) -> Result<Self> {
        Self::open_selected(path.as_ref(), execution, None)
    }
    pub(crate) fn open_pinned(pin: &crate::cursor::GenerationPin) -> Result<Self> {
        let execution = crate::execution::Execution::new(Default::default(), None)?;
        let view = Self::open_selected(&pin.database, execution, Some(pin))?;
        *view.execution.lock().unwrap() = None;
        Ok(view)
    }
    fn open_selected(
        path: &Path,
        execution: crate::execution::Execution,
        pin: Option<&crate::cursor::GenerationPin>,
    ) -> Result<Self> {
        execution.check()?;
        let result = Self::open_inner(path, execution, pin);
        execution.check()?;
        result
    }
    fn open_inner(
        path: &Path,
        execution: crate::execution::Execution,
        pin: Option<&crate::cursor::GenerationPin>,
    ) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::NotReady);
        }
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let execution = Arc::new(Mutex::new(Some(execution)));
        let progress = Arc::clone(&execution);
        connection.progress_handler(
            1_000,
            Some(move || progress.lock().unwrap().is_some_and(|e| e.check().is_err())),
        )?;
        let busy = Arc::clone(&execution);
        connection.busy_handler(Some(move |count| {
            if count >= 2_000 {
                return false;
            }
            let remaining = busy.lock().unwrap().map(|e| e.remaining()).transpose();
            match remaining {
                Err(_) => false,
                Ok(remaining) => {
                    std::thread::sleep(
                        remaining
                            .unwrap_or(Duration::from_millis(1))
                            .min(Duration::from_millis(1)),
                    );
                    true
                }
            }
        }))?;
        // Avoid parameter-triggered reprepare of the nested graph views on each
        // step. Reader plans stay stable; writer planning/durability is unchanged.
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_QPSG, true)?;
        connection.execute_batch("PRAGMA cache_size=-2048; BEGIN DEFERRED;")?;
        let version: i32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if ![2, 3, 4, 5, SCHEMA_VERSION].contains(&version) {
            return Err(Error::UpdateRequired(format!(
                "index format {version} is unsupported; update required (readable: 2 through {SCHEMA_VERSION})"
            )));
        }
        let locations = if version == 2 {
            "CREATE TEMP VIEW occurrence_locations AS SELECT o.generation,o.compilation_id,o.symbol_id,o.role,a.id AS artifact_id,a.path,a.content_hash,a.generated,o.start_byte,o.end_byte FROM occurrences o JOIN artifacts a ON a.generation=o.generation AND a.id=o.artifact_id"
        } else {
            "CREATE TEMP VIEW occurrence_locations AS SELECT s.generation,s.compilation_id,s.id AS symbol_id,o.role,a.id AS artifact_id,a.path,a.content_hash,a.generated,o.start_byte,o.end_byte FROM occurrences o JOIN symbols s ON s.rowid=o.symbol_key JOIN artifacts a ON a.rowid=o.artifact_key"
        };
        connection.execute_batch(locations)?;
        if version >= 4 {
            crate::graph::create_view(&connection, version)?;
        }
        connection.execute_batch("PRAGMA query_only=ON;")?;
        let generation = if let Some(pin) = pin {
            if !crate::cursor::storage_active(&connection)? {
                return Err(Error::CursorInvalidated);
            }
            let generation: Option<u64> = connection.query_row("SELECT p.generation FROM cursor_generation_pins p JOIN cursor_retention_state s ON s.owner=p.owner WHERE s.singleton=1 AND p.id=? AND p.owner=?", params![pin.id.as_slice(), pin.owner.as_slice()], |r| r.get(0)).optional()?;
            match generation {
                Some(generation) if generation == pin.generation() => Some(generation),
                _ => return Err(Error::CursorInvalidated),
            }
        } else {
            connection.query_row(
                "SELECT current_generation FROM state WHERE singleton=1",
                [],
                |r| r.get(0),
            )?
        };
        Ok(Self {
            connection,
            format_version: version,
            generation: generation.ok_or(Error::NotReady)?,
            execution,
            expired: Cell::new(false),
        })
    }

    pub fn query(&self, request: QueryRequest) -> Result<QueryResult> {
        let execution = crate::execution::Execution::new(Default::default(), request.deadline_ms)?;
        let maximum = crate::execution::response_budget(request.response_max_bytes)?;
        let result = self.query_controlled(request, execution).and_then(|value| {
            crate::execution::check_response(&value, execution, maximum)?;
            Ok(value)
        });
        if let Err(error) = execution.check() {
            self.expire()?;
            return Err(error);
        }
        result
    }
    pub(crate) fn query_controlled(
        &self,
        request: QueryRequest,
        execution: crate::execution::Execution,
    ) -> Result<QueryResult> {
        if self.expired.get() {
            return Err(Error::DeadlineExceeded);
        }
        *self.execution.lock().unwrap() = Some(execution);
        let result = self.check().and_then(|_| self.query_inner(request));
        let elapsed = execution.check();
        *self.execution.lock().unwrap() = None;
        if let Err(error) = elapsed {
            self.expire()?;
            return Err(error);
        }
        result
    }
    fn expire(&self) -> Result<()> {
        self.expired.set(true);
        // Statements/rows have unwound; release even a retained caller view.
        if !self.connection.is_autocommit() {
            self.connection.execute_batch("ROLLBACK")?;
        }
        Ok(())
    }
    pub(crate) fn check(&self) -> Result<()> {
        if self.expired.get() {
            return Err(Error::DeadlineExceeded);
        }
        self.execution.lock().unwrap().map_or(Ok(()), |e| e.check())
    }
    fn query_inner(&self, request: QueryRequest) -> Result<QueryResult> {
        if !(1..=1000).contains(&request.limit) {
            return Err(Error::InvalidRequest(
                "limit must be between 1 and 1000".into(),
            ));
        }
        if request.generation.is_some_and(|g| g != self.generation) {
            return Err(Error::InvalidRequest(
                "served generation changed; restart lookup before requesting details".into(),
            ));
        }
        if request.op == "coverage" && request.generation.is_none() {
            return Err(Error::InvalidRequest(
                "coverage requires generation from the result being expanded".into(),
            ));
        }
        let coverage = self.coverage(
            request.op == "coverage" || request.coverage_detail == CoverageDetail::Full,
        )?;
        let mut workspace_coverage = WorkspaceCoverage {
            compilations: coverage.len(),
            ..Default::default()
        };
        for c in &coverage {
            self.check()?;
            *workspace_coverage
                .statuses
                .entry(c.status.clone())
                .or_default() += 1;
            for (severity, count) in &c.diagnostic_counts {
                *workspace_coverage
                    .diagnostic_counts
                    .entry(severity.clone())
                    .or_default() += count;
            }
        }
        let mut result = QueryResult {
            generation: self.generation,
            freshness: "unverified",
            coordination: None,
            workspace_coverage,
            coverage_scope: "workspace",
            coverage,
            truncated: false,
            symbols: vec![],
            occurrences: vec![],
            source: None,
            graph: None,
            contexts: vec![],
        };
        match request.op.as_str() {
            "coverage" => {
                if let Some(id) = &request.compilation_id {
                    result.coverage.retain(|c| &c.compilation_id == id);
                    if result.coverage.is_empty() {
                        return Err(Error::InvalidRequest(
                            "compilation_id is absent from the served generation".into(),
                        ));
                    }
                    result.coverage_scope = "compilation";
                }
                result.truncated = result.coverage.len() > request.limit as usize;
                result.coverage.truncate(request.limit as usize);
            }
            "symbol_context" => {
                let (contexts, truncated) = self.symbol_context(&request)?;
                result.contexts = contexts;
                result.truncated = truncated;
            }
            "callers" | "callees" | "impact" => {
                let graph = crate::graph::query(self, &request, &result.coverage)?;
                result.truncated = !graph.limits.is_empty();
                result.graph = Some(graph);
            }
            "symbols" => {
                let terms = request
                    .query
                    .as_ref()
                    .map(|q| format!("\"{}\"", q.replace('"', "\"\"")));
                if request.query.as_ref().is_some_and(|q| q.trim().is_empty()) {
                    return Err(Error::InvalidRequest("search query is empty".into()));
                }
                let mut statement = self.connection.prepare(&symbols_sql(&request))?;
                let rows = statement.query_map(
                    params![
                        self.generation,
                        request.name,
                        terms,
                        request.target,
                        request.limit + 1
                    ],
                    |r| {
                        Ok(SymbolResult {
                            id: r.get(0)?,
                            name: r.get(1)?,
                            kind: r.get(2)?,
                            signature: r.get(3)?,
                            external: r.get(4)?,
                            compilation_id: r.get(5)?,
                            target: r.get(6)?,
                            locations: vec![],
                            owner_generation: r.get(7)?,
                            locations_limited: false,
                        })
                    },
                )?;
                result.symbols = rows.collect::<std::result::Result<Vec<_>, _>>()?;
                result.truncated = result.symbols.len() > request.limit as usize;
                result.symbols.truncate(request.limit as usize);
                let mut locations = self.connection.prepare_cached("SELECT o.artifact_id,o.path,o.content_hash,o.start_byte,o.end_byte,o.generated FROM occurrence_locations o WHERE o.generation=? AND o.compilation_id=? AND o.symbol_id=? AND o.role='declaration' ORDER BY o.path,o.start_byte")?;
                for symbol in &mut result.symbols {
                    self.check()?;
                    symbol.locations = locations
                        .query_map(
                            params![symbol.owner_generation, symbol.compilation_id, symbol.id],
                            read_location,
                        )?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                }
            }
            "references" => {
                let symbol = request
                    .symbol_id
                    .as_ref()
                    .ok_or_else(|| Error::InvalidRequest("references requires symbol_id".into()))?;
                let mut statement = self.connection.prepare("SELECT o.symbol_id,o.role,o.compilation_id,c.target,o.artifact_id,o.path,o.content_hash,o.start_byte,o.end_byte,o.generated FROM generation_units u JOIN occurrence_locations o ON o.generation=u.owner_generation AND o.compilation_id=u.compilation_id JOIN compilations c ON c.generation=o.generation AND c.id=o.compilation_id WHERE u.generation=? AND o.symbol_id=? AND o.role='reference' AND (? IS NULL OR c.target=?) ORDER BY o.path,o.start_byte,o.compilation_id LIMIT ?")?;
                result.occurrences = statement
                    .query_map(
                        params![
                            self.generation,
                            symbol,
                            request.target,
                            request.target,
                            request.limit + 1
                        ],
                        |r| {
                            Ok(OccurrenceResult {
                                symbol_id: r.get(0)?,
                                role: r.get(1)?,
                                compilation_id: r.get(2)?,
                                target: r.get(3)?,
                                location: Location {
                                    artifact_id: r.get(4)?,
                                    path: r.get(5)?,
                                    content_hash: r.get(6)?,
                                    start_byte: r.get(7)?,
                                    end_byte: r.get(8)?,
                                    generated: r.get(9)?,
                                },
                            })
                        },
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result.truncated = result.occurrences.len() > request.limit as usize;
                result.occurrences.truncate(request.limit as usize);
            }
            "source" => {
                let (source, truncated) = self.source_excerpt(&request, false)?;
                result.source = Some(source);
                result.truncated = truncated;
            }
            _ => {
                return Err(Error::InvalidRequest(
                    "op must be symbols, references, source, symbol_context, coverage, callers, callees, or impact".into(),
                ));
            }
        }
        if request.coverage_detail != CoverageDetail::Full
            && request.op != "coverage"
            && result.graph.is_none()
        {
            let mut relevant = BTreeSet::new();
            for s in &result.symbols {
                relevant.insert(s.compilation_id.clone());
            }
            for c in &result.contexts {
                relevant.insert(c.symbol.compilation_id.clone());
            }
            for o in &result.occurrences {
                relevant.insert(o.compilation_id.clone());
            }
            if request.op == "references"
                && let Some(id) = &request.symbol_id
            {
                let mut q = self.connection.prepare("SELECT s.compilation_id FROM generation_units u JOIN symbols s ON s.generation=u.owner_generation AND s.compilation_id=u.compilation_id JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id WHERE u.generation=? AND s.id=? AND (? IS NULL OR c.target=?)")?;
                for id in q.query_map(
                    params![self.generation, id, request.target, request.target],
                    |r| r.get::<_, String>(0),
                )? {
                    relevant.insert(id?);
                }
            }
            if let Some(source) = &result.source {
                let mut q = self.connection.prepare("SELECT u.compilation_id FROM generation_units u JOIN unit_artifacts a ON a.owner_generation=u.owner_generation AND a.compilation_id=u.compilation_id WHERE u.generation=? AND a.artifact_id=?")?;
                for id in q.query_map(params![self.generation, source.artifact_id], |r| {
                    r.get::<_, String>(0)
                })? {
                    relevant.insert(id?);
                }
            }
            result
                .coverage
                .retain(|c| relevant.contains(&c.compilation_id));
            result.coverage_scope = "relevant";
        }
        let full = request.op == "coverage" || request.coverage_detail == CoverageDetail::Full;
        let mut detail_budget = if full {
            source_budget(if request.op == "coverage" {
                request.max_bytes
            } else {
                request.detail_max_bytes
            })? as usize
        } else {
            0
        };
        for c in &mut result.coverage {
            self.check()?;
            if !full {
                c.analysis_options = serde_json::Value::Null;
                c.diagnostics.clear();
                c.details_omitted = true;
            } else {
                // Bound expandable diagnostic/option payloads together. Required
                // provenance, counts and capabilities remain outside this budget.
                let size = serde_json::to_vec(&c.analysis_options)?.len();
                if size <= detail_budget {
                    detail_budget -= size;
                } else {
                    c.analysis_options = serde_json::Value::Null;
                    c.details_omitted = true;
                }
                let mut diagnostics = vec![];
                for d in c.diagnostics.drain(..) {
                    self.check()?;
                    let size = serde_json::to_vec(&d)?.len();
                    if size <= detail_budget {
                        detail_budget -= size;
                        diagnostics.push(d);
                    } else {
                        c.details_omitted = true;
                    }
                }
                c.diagnostics = diagnostics;
                result.truncated |= c.details_omitted;
            }
        }
        Ok(result)
    }

    fn symbol_context(&self, request: &QueryRequest) -> Result<(Vec<SymbolContext>, bool)> {
        let id = request
            .symbol_id
            .as_ref()
            .ok_or_else(|| Error::InvalidRequest("symbol_context requires symbol_id".into()))?;
        let mut q = self.connection.prepare("SELECT s.rowid FROM generation_units u JOIN symbols s ON s.generation=u.owner_generation AND s.compilation_id=u.compilation_id JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id WHERE u.generation=? AND s.id=? AND (? IS NULL OR c.target=?) AND (? IS NULL OR s.compilation_id=?) ORDER BY s.compilation_id LIMIT ?")?;
        let keys = q
            .query_map(
                params![
                    self.generation,
                    id,
                    request.target,
                    request.target,
                    request.compilation_id,
                    request.compilation_id,
                    request.limit + 1
                ],
                |r| r.get::<_, i64>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if keys.is_empty() {
            return Err(Error::InvalidRequest(
                "symbol_id is not present in the served compilation/target scope".into(),
            ));
        }
        let mut truncated = keys.len() > request.limit as usize;
        let mut budget = source_budget(request.max_bytes)?;
        let mut count = 0;
        let mut contexts = vec![];
        for key in keys.into_iter().take(request.limit as usize) {
            self.check()?;
            if count >= request.limit {
                truncated = true;
                break;
            }
            // Reuse the same bounded symbol/declaration projection as graph reads.
            let mut symbol = crate::graph::symbol(&self.connection, key)?;
            truncated |= symbol.locations_limited;
            let mut declarations = vec![];
            for location in std::mem::take(&mut symbol.locations) {
                self.check()?;
                if count >= request.limit {
                    truncated = true;
                    break;
                }
                count += 1;
                let line = self.source_line(&location.content_hash, location.start_byte)?;
                let mut extents = self.connection.prepare_cached("SELECT artifact_id,path,content_hash,start_byte,end_byte,generated FROM occurrence_locations WHERE generation=? AND compilation_id=? AND symbol_id=? AND role='definition' AND artifact_id=? AND start_byte<=? AND end_byte>=? ORDER BY start_byte,end_byte LIMIT 2")?;
                let ranges = extents
                    .query_map(
                        params![
                            symbol.owner_generation,
                            symbol.compilation_id,
                            symbol.id,
                            location.artifact_id,
                            location.start_byte,
                            location.end_byte
                        ],
                        read_location,
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                // A unique compiler declaration is required. Never select an
                // arbitrary branch of a synthetic/multiple-root symbol view.
                let (extent, mut status) = match ranges.len() {
                    1 => (ranges.into_iter().next(), "omitted"),
                    0 => (None, "extent_unavailable"),
                    _ => (None, "extent_ambiguous"),
                };
                let mut source = None;
                if request.include_source
                    && let Some(extent) = &extent
                {
                    if budget == 0 {
                        status = "budget_exhausted";
                        truncated = true;
                    } else {
                        let mut read = request.clone();
                        read.artifact_id = Some(extent.artifact_id.clone());
                        read.content_hash = Some(extent.content_hash.clone());
                        read.start_byte = Some(extent.start_byte);
                        read.end_byte = Some(extent.end_byte);
                        read.max_bytes = Some(budget);
                        let (body, clipped) = self.source_excerpt(&read, true)?;
                        budget -= (body.end_byte - body.start_byte) as u32;
                        status = if body.text.is_empty() && clipped {
                            "budget_exhausted"
                        } else if clipped {
                            "truncated"
                        } else {
                            "included"
                        };
                        truncated |= clipped;
                        source = Some(body);
                    }
                }
                declarations.push(DeclarationContext {
                    location,
                    line,
                    extent,
                    source,
                    source_status: status,
                });
            }
            let boundary = if declarations.is_empty() {
                Some(if symbol.external {
                    "external_or_other_compilation_body"
                } else {
                    "declaration_unavailable"
                })
            } else {
                None
            };
            contexts.push(SymbolContext {
                symbol,
                declarations,
                boundary,
            });
        }
        Ok((contexts, truncated))
    }

    // Streaming the indexed blob avoids loading an entire file or relying on
    // SQLite TEXT operations (which stop at embedded NULs) for line positions.
    fn source_line(&self, hash: &str, offset: u64) -> Result<u64> {
        let rowid =
            self.connection
                .query_row("SELECT rowid FROM contents WHERE hash=?", [hash], |r| {
                    r.get(0)
                })?;
        let mut blob = self
            .connection
            .blob_open("main", "contents", "utf8", rowid, true)?;
        let mut remaining = offset;
        let mut line = 1;
        let mut previous_cr = false;
        let mut chunk = [0u8; 8192];
        while remaining > 0 {
            self.check()?;
            let count = remaining.min(chunk.len() as u64) as usize;
            blob.read_exact(&mut chunk[..count])?;
            for &byte in &chunk[..count] {
                if byte == b'\r' || (byte == b'\n' && !previous_cr) {
                    line += 1;
                }
                previous_cr = byte == b'\r';
            }
            remaining -= count as u64;
        }
        Ok(line)
    }

    fn source_excerpt(
        &self,
        request: &QueryRequest,
        allow_empty: bool,
    ) -> Result<(SourceResult, bool)> {
        use rusqlite::OptionalExtension;
        let id = request
            .artifact_id
            .as_ref()
            .ok_or_else(|| Error::InvalidRequest("source requires artifact_id".into()))?;
        let budget = source_budget(request.max_bytes)?;
        if (request.start_byte.is_some() || request.end_byte.is_some())
            && request.content_hash.is_none()
        {
            return Err(Error::InvalidRequest(
                "source ranges require content_hash from the indexed location".into(),
            ));
        }
        let start = request.start_byte.unwrap_or(0);
        let sql_start = i64::try_from(start)
            .map_err(|_| Error::InvalidRequest("source start_byte is out of range".into()))?;
        let sql_end = request
            .end_byte
            .map(i64::try_from)
            .transpose()
            .map_err(|_| Error::InvalidRequest("source end_byte is out of range".into()))?;
        let row = self.connection.query_row(
            "SELECT a.path,a.content_hash,length(c.utf8),substr(c.utf8,?3+1,?4),CASE WHEN ?5 IS NULL THEN X'' ELSE substr(c.utf8,?5+1,1) END FROM generation_units u JOIN unit_artifacts ua ON ua.owner_generation=u.owner_generation AND ua.compilation_id=u.compilation_id JOIN artifacts a ON a.generation=ua.owner_generation AND a.id=ua.artifact_id JOIN contents c ON c.hash=a.content_hash WHERE u.generation=?1 AND a.id=?2 LIMIT 1",
            params![self.generation,id,sql_start,budget+3,sql_end],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,u64>(2)?,r.get::<_,Vec<u8>>(3)?,r.get::<_,Vec<u8>>(4)?)),
        ).optional()?.ok_or_else(|| Error::InvalidRequest("source artifact is absent from the served generation; restart lookup".into()))?;
        let (path, hash, total, mut bytes, end_byte) = row;
        if request
            .content_hash
            .as_ref()
            .is_some_and(|expected| expected != &hash)
        {
            return Err(Error::InvalidRequest(
                "indexed source content changed; restart lookup before reusing its byte range"
                    .into(),
            ));
        }
        let end = request.end_byte.unwrap_or(total);
        let continuation = |byte: &u8| byte & 0xc0 == 0x80;
        if start > end
            || end > total
            || bytes.first().is_some_and(continuation)
            || end_byte.first().is_some_and(continuation)
        {
            return Err(Error::InvalidRequest(
                "source range must be ordered, within indexed text, and on UTF-8 boundaries".into(),
            ));
        }
        let mut count = (end - start).min(u64::from(budget)) as usize;
        while count > 0 && bytes.get(count).is_some_and(continuation) {
            count -= 1;
        }
        if count == 0 && start < end && !allow_empty {
            return Err(Error::InvalidRequest(
                "max_bytes cannot fit the next UTF-8 character".into(),
            ));
        }
        bytes.truncate(count);
        let text = String::from_utf8(bytes).map_err(|e| {
            Error::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Blob,
                Box::new(e),
            ))
        })?;
        let actual_end = start + count as u64;
        Ok((
            SourceResult {
                artifact_id: id.clone(),
                path,
                content_hash: hash.clone(),
                text,
                start_byte: start,
                end_byte: actual_end,
                total_bytes: total,
                start_line: self.source_line(&hash, start)?,
            },
            actual_end < end,
        ))
    }

    fn coverage(&self, details: bool) -> Result<Vec<Coverage>> {
        let caps = if self.format_version >= 4 {
            "r.capabilities_json"
        } else {
            "'{}'"
        };
        let options = if details { "c.options_json" } else { "'null'" };
        let mut statement=self.connection.prepare(&format!("SELECT c.id,c.language,c.project_path,c.target,c.configuration,c.inferred,c.compiler_version,r.status,r.diagnostics,{options},{caps} FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id JOIN reports r ON c.generation=r.generation AND c.id=r.compilation_id WHERE u.generation=? ORDER BY c.id"))?;
        let rows = statement.query_map([self.generation], |r| {
            let check = || {
                self.check().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            };
            check()?;
            let diagnostics: Vec<Diagnostic> = serde_json::from_str(&r.get::<_, String>(8)?)
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
            let mut diagnostic_counts = BTreeMap::new();
            for (index, d) in diagnostics.iter().enumerate() {
                if index % 256 == 0 {
                    check()?;
                }
                *diagnostic_counts.entry(d.severity.clone()).or_default() += 1;
            }
            check()?;
            let options: String = r.get(9)?;
            Ok(Coverage {
                compilation_id: r.get(0)?,
                language: r.get(1)?,
                project_path: r.get(2)?,
                target: r.get(3)?,
                configuration: r.get(4)?,
                inferred: r.get(5)?,
                compiler_version: r.get(6)?,
                analysis_options: serde_json::from_str(if options.is_empty() {
                    "{}"
                } else {
                    &options
                })
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        9,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                capabilities: serde_json::from_str(&r.get::<_, String>(10)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        10,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                status: r.get(7)?,
                diagnostic_counts,
                details_omitted: !details,
                diagnostics: if details { diagnostics } else { vec![] },
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
fn source_budget(value: Option<u32>) -> Result<u32> {
    let budget = value.unwrap_or(DEFAULT_SOURCE_BYTES);
    if !(1..=MAX_SOURCE_BYTES).contains(&budget) {
        return Err(Error::InvalidRequest(format!(
            "source/detail max_bytes must be between 1 and {MAX_SOURCE_BYTES}"
        )));
    }
    Ok(budget)
}
// Select only fixed SQL fragments; all request values remain bound parameters.
// Join order keeps exact names on symbol_name and FTS hits on symbol rowids,
// rather than scanning every symbol in each visible compilation.
fn symbols_sql(request: &QueryRequest) -> String {
    let mut sql =
        "SELECT s.id,s.name,s.kind,s.signature,s.external,s.compilation_id,c.target,s.generation "
            .to_owned();
    if request.name.is_some() {
        sql.push_str("FROM generation_units u CROSS JOIN symbols s ON s.generation=u.owner_generation AND s.compilation_id=u.compilation_id CROSS JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id");
        if request.query.is_some() {
            sql.push_str(" CROSS JOIN symbol_search ON symbol_search.rowid=s.rowid");
        }
    } else if request.query.is_some() {
        sql.push_str("FROM symbol_search CROSS JOIN symbols s ON s.rowid=symbol_search.rowid CROSS JOIN generation_units u ON u.owner_generation=s.generation AND u.compilation_id=s.compilation_id CROSS JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id");
    } else {
        sql.push_str("FROM generation_units u JOIN symbols s ON s.generation=u.owner_generation AND s.compilation_id=u.compilation_id JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id");
    }
    sql.push_str(" WHERE u.generation=?1");
    if request.name.is_some() {
        sql.push_str(" AND s.name=?2");
    }
    if request.query.is_some() {
        sql.push_str(" AND symbol_search MATCH ?3");
    }
    if request.target.is_some() {
        sql.push_str(" AND c.target=?4");
    }
    sql.push_str(" ORDER BY s.name,s.id,s.compilation_id LIMIT ?5");
    sql
}

fn read_location(r: &rusqlite::Row<'_>) -> rusqlite::Result<Location> {
    Ok(Location {
        artifact_id: r.get(0)?,
        path: r.get(1)?,
        content_hash: r.get(2)?,
        start_byte: r.get(3)?,
        end_byte: r.get(4)?,
        generated: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::{AnalysisReport, Compilation, envelope::Payload},
        store::Writer,
    };

    #[test]
    fn symbol_filters_use_name_or_fts_lookups() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/search-plans")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("index.db");
        let mut writer = Writer::open(&path).unwrap();
        writer
            .stage(Payload::Compilation(Compilation {
                id: "plans".into(),
                ..Default::default()
            }))
            .unwrap();
        writer
            .stage(Payload::Report(AnalysisReport {
                compilation_id: "plans".into(),
                status: "complete".into(),
                ..Default::default()
            }))
            .unwrap();
        writer.publish().unwrap();
        let view = ReadView::open(&path).unwrap();
        let mut evidence = vec![];
        for name in [None, Some("needle")] {
            for text in [None, Some("needle")] {
                for target in [None, Some("target")] {
                    let request: QueryRequest = serde_json::from_value(serde_json::json!({"op":"symbols", "name":name, "query":text, "target":target})).unwrap();
                    let sql = symbols_sql(&request);
                    let plans: Vec<String> = view
                        .connection
                        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                        .unwrap()
                        .query_map(params![view.generation, name, text, target, 101], |r| {
                            r.get(3)
                        })
                        .unwrap()
                        .map(std::result::Result::unwrap)
                        .collect();
                    if name.is_some() {
                        assert!(
                            plans
                                .iter()
                                .any(|p| p.contains("symbol_name (generation=? AND name=?)")),
                            "{plans:?}"
                        );
                    } else if text.is_some() {
                        assert!(
                            plans[0].contains("symbol_search VIRTUAL TABLE INDEX")
                                && plans[0].contains("M2"),
                            "{plans:?}"
                        );
                        assert!(
                            plans
                                .iter()
                                .any(|p| p.contains("s USING INTEGER PRIMARY KEY (rowid=?)")),
                            "{plans:?}"
                        );
                    }
                    if name.is_some() && text.is_some() {
                        assert!(
                            plans
                                .iter()
                                .any(|p| p.contains("symbol_search VIRTUAL TABLE INDEX")
                                    && p.contains("=M2")),
                            "{plans:?}"
                        );
                    }
                    evidence.push(serde_json::json!({"name":name,"text":text,"target":target,"sql":sql,"plan":plans}));
                }
            }
        }
        std::fs::write(
            path.with_file_name("plans.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
    }
}
