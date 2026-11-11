use crate::{Error, Result, protocol::Diagnostic, store::SCHEMA_VERSION};
use rusqlite::{Connection, OpenFlags, OptionalExtension, config::DbConfig, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
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
    #[serde(default)]
    pub tests: crate::testing::TestFilter,
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
    pub page_size: Option<u32>,
    pub cursor: Option<String>,
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
    pub snapshot: Option<String>,
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
    pub test_classification: crate::testing::Classification,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub supporting_targets: Vec<SymbolSupport>,
    #[serde(skip)]
    pub(crate) owner_generation: u64,
    #[serde(skip)]
    pub(crate) locations_limited: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct SymbolSupport {
    pub symbol_id: String,
    pub compilation_id: String,
    pub target: String,
    pub signature: String,
    pub locations: Vec<Location>,
    pub test_classification: crate::testing::Classification,
}
impl From<&SymbolResult> for SymbolSupport {
    fn from(s: &SymbolResult) -> Self {
        Self {
            symbol_id: s.id.clone(),
            compilation_id: s.compilation_id.clone(),
            target: s.target.clone(),
            signature: s.signature.clone(),
            locations: s.locations.clone(),
            test_classification: s.test_classification.clone(),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct OccurrenceSupport {
    pub symbol_id: String,
    pub compilation_id: String,
    pub target: String,
    pub location: Location,
    pub test_classification: crate::testing::Classification,
}
#[derive(Debug, Serialize)]
pub struct OccurrenceResult {
    pub symbol_id: String,
    pub role: String,
    pub compilation_id: String,
    pub target: String,
    pub location: Location,
    pub test_classification: crate::testing::Classification,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub supporting_targets: Vec<OccurrenceSupport>,
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
#[derive(Clone, Debug, Serialize)]
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
    pub test_classification: BTreeMap<String, usize>,
    pub target_equivalence: BTreeMap<String, usize>,
    pub diagnostic_counts: BTreeMap<String, usize>,
}
#[derive(Clone, Debug, Serialize)]
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub supporting_targets: Vec<ContextSupport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<&'static str>,
}
#[derive(Debug, Serialize)]
pub struct ContextSupport {
    #[serde(flatten)]
    pub symbol: SymbolSupport,
    pub declarations: Vec<DeclarationContext>,
    pub boundary: Option<&'static str>,
}
#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
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

struct ProjectionBudget {
    remaining: usize,
    execution: Option<crate::execution::Execution>,
}
impl ProjectionBudget {
    fn new(view: &ReadView, request: &QueryRequest) -> Result<Self> {
        let remaining = view
            .projection_maximum
            .get()
            .unwrap_or(crate::execution::response_budget(
                request.response_max_bytes,
            )?);
        Ok(Self {
            remaining,
            execution: view.execution.lock().unwrap().clone(),
        })
    }
    fn charge(&mut self, value: &impl Serialize) -> Result<()> {
        struct Count(usize);
        impl Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 += bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let Count(size) =
            crate::execution::write_json(value, self.execution.clone(), self.remaining, Count(0))?;
        self.remaining -= size;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Stopped {
    Deadline,
    Cancelled,
}
impl Stopped {
    fn error(self) -> Error {
        match self {
            Self::Deadline => Error::DeadlineExceeded,
            Self::Cancelled => Error::QueryCancelled,
        }
    }
}
pub struct ReadView {
    pub(crate) connection: Connection,
    pub(crate) format_version: i32,
    pub generation: u64,
    execution: Arc<Mutex<Option<crate::execution::Execution>>>,
    stopped: Cell<Option<Stopped>>,
    projection_maximum: Cell<Option<usize>>,
    pub(crate) health: crate::recovery::IndexReadGuard,
    logical_identity: Option<Vec<u8>>,
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
    pub(crate) fn open_pinned_controlled(
        pin: &crate::cursor::GenerationPin,
        execution: crate::execution::Execution,
    ) -> Result<Self> {
        Self::open_selected(&pin.database, execution, Some(pin))
    }
    fn open_selected(
        path: &Path,
        execution: crate::execution::Execution,
        pin: Option<&crate::cursor::GenerationPin>,
    ) -> Result<Self> {
        execution.check()?;
        let health = if let Some(pin) = pin {
            pin.health.clone()
        } else {
            crate::recovery::IndexReadGuard::open(path, Some(execution.clone()))?
        };
        health.check()?;
        let result =
            crate::index_result(&health, Self::open_inner(&health, execution.clone(), pin));
        execution.check()?;
        result
    }
    fn open_inner(
        health: &crate::recovery::IndexReadGuard,
        execution: crate::execution::Execution,
        pin: Option<&crate::cursor::GenerationPin>,
    ) -> Result<Self> {
        let path = health.physical_path();
        if !path.is_file() {
            return Err(Error::NotReady);
        }
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let execution = Arc::new(Mutex::new(Some(execution)));
        let progress = Arc::clone(&execution);
        let progress_health = health.clone();
        connection.progress_handler(
            1_000,
            Some(move || {
                progress_health.check().is_err()
                    || progress
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|e| e.check().is_err())
            }),
        )?;
        let busy = Arc::clone(&execution);
        let busy_health = health.clone();
        connection.busy_handler(Some(move |count| {
            if count >= 2_000 || busy_health.check().is_err() {
                return false;
            }
            let remaining = busy
                .lock()
                .unwrap()
                .as_ref()
                .map(|e| e.remaining())
                .transpose();
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
        if ![2, 3, 4, 5, 6, 7, SCHEMA_VERSION].contains(&version) {
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
        crate::testing::create_views(&connection, version)?;
        crate::grouping::create_view(&connection, version)?;
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
        let logical_identity = if health.incarnation().is_empty() {
            None
        } else {
            let logical = health.logical_path();
            let parent = logical.parent().ok_or_else(|| {
                Error::InvalidRequest("index logical handle has no parent".into())
            })?;
            let name = logical.file_name().ok_or_else(|| {
                Error::InvalidRequest("index logical handle has no filename".into())
            })?;
            Some(
                std::fs::canonicalize(parent)?
                    .join(name)
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
            )
        };
        Ok(Self {
            connection,
            format_version: version,
            generation: generation.ok_or(Error::NotReady)?,
            execution,
            stopped: Cell::new(None),
            projection_maximum: Cell::new(None),
            health: health.clone(),
            logical_identity,
        })
    }

    pub fn query(&self, request: QueryRequest) -> Result<QueryResult> {
        self.query_with_cancellation(request, None)
    }
    pub fn query_cancellable(
        &self,
        request: QueryRequest,
        cancellation: crate::execution::QueryCancellation,
    ) -> Result<QueryResult> {
        self.query_with_cancellation(request, Some(cancellation))
    }
    fn query_with_cancellation(
        &self,
        request: QueryRequest,
        cancellation: Option<crate::execution::QueryCancellation>,
    ) -> Result<QueryResult> {
        let execution = crate::execution::Execution::new(Default::default(), request.deadline_ms)?;
        let execution = execution.with_cancellation(cancellation);
        let maximum = crate::execution::response_budget(request.response_max_bytes)?;
        let result = self
            .query_controlled(request, execution.clone())
            .and_then(|value| {
                crate::execution::check_response(&value, execution.clone(), maximum)?;
                Ok(value)
            });
        if let Err(error) = execution.check() {
            self.expire(&error)?;
            return Err(error);
        }
        crate::index_result(&self.health, result)
    }
    /// The cursor client materializes one bounded logical result before packing
    /// pages. Its aggregate allowance differs from each page's response cap.
    pub(crate) fn query_materialized_controlled(
        &self,
        request: QueryRequest,
        execution: crate::execution::Execution,
    ) -> Result<QueryResult> {
        self.projection_maximum
            .set(Some(crate::cursor::MAX_RESULT_BYTES as usize));
        let result = self.query_controlled(request, execution);
        self.projection_maximum.set(None);
        result
    }
    pub(crate) fn query_controlled(
        &self,
        request: QueryRequest,
        execution: crate::execution::Execution,
    ) -> Result<QueryResult> {
        if let Some(stopped) = self.stopped.get() {
            return Err(stopped.error());
        }
        *self.execution.lock().unwrap() = Some(execution.clone());
        let result = crate::index_result(
            &self.health,
            self.check().and_then(|_| self.query_inner(request)),
        );
        let elapsed = execution.check();
        *self.execution.lock().unwrap() = None;
        if let Err(error) = elapsed {
            self.expire(&error)?;
            return Err(error);
        }
        result
    }
    fn expire(&self, error: &Error) -> Result<()> {
        self.stopped
            .set(Some(if matches!(error, Error::QueryCancelled) {
                Stopped::Cancelled
            } else {
                Stopped::Deadline
            }));
        // Statements/rows have unwound; release even a retained caller view.
        if !self.connection.is_autocommit() {
            self.connection.execute_batch("ROLLBACK")?;
        }
        Ok(())
    }
    pub(crate) fn check(&self) -> Result<()> {
        if let Some(stopped) = self.stopped.get() {
            return Err(stopped.error());
        }
        self.health.check()?;
        self.execution
            .lock()
            .unwrap()
            .as_ref()
            .map_or(Ok(()), |e| e.check())
    }
    pub(crate) fn snapshot_token(&self) -> Option<String> {
        if self.health.incarnation().is_empty() {
            return None;
        }
        let mut digest = Sha256::new();
        digest.update(b"know-code-snapshot-v1\0");
        for value in [
            self.logical_identity
                .as_deref()
                .expect("managed logical identity"),
            self.health.incarnation().as_bytes(),
            &self.generation.to_le_bytes(),
        ] {
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value);
        }
        Some(format!("kcs1{:x}", digest.finalize()))
    }
    fn query_inner(&self, request: QueryRequest) -> Result<QueryResult> {
        if request.page_size.is_some() || request.cursor.is_some() {
            return Err(Error::InvalidRequest("portable pagination requires the managed query entry point, not a retained low-level ReadView".into()));
        }
        if !(1..=1000).contains(&request.limit) {
            return Err(Error::InvalidRequest(
                "limit must be between 1 and 1000".into(),
            ));
        }
        let snapshot = self.snapshot_token();
        if request.snapshot.is_some() && request.snapshot != snapshot {
            return Err(Error::SnapshotInvalidated);
        }
        if snapshot.is_some() && request.generation.is_some() && request.snapshot.is_none() {
            return Err(Error::InvalidRequest(
                "managed generation guards require the snapshot token from a fresh lookup".into(),
            ));
        }
        if request.generation.is_some_and(|g| g != self.generation) {
            return Err(Error::InvalidRequest(
                "served generation changed; restart lookup before requesting details".into(),
            ));
        }
        if request.op == "coverage" && request.generation.is_none() && request.snapshot.is_none() {
            return Err(Error::InvalidRequest(
                "coverage requires generation from the result being expanded".into(),
            ));
        }
        let coverage = self.coverage(
            request.op == "coverage" || request.coverage_detail == CoverageDetail::Full,
        )?;
        if request.tests != crate::testing::TestFilter::All
            && ["source", "coverage"].contains(&request.op.as_str())
        {
            return Err(Error::InvalidRequest(
                "tests applies to findings, not source/coverage expansion".into(),
            ));
        }
        let mut workspace_coverage = WorkspaceCoverage {
            compilations: coverage.len(),
            ..Default::default()
        };
        for c in &coverage {
            self.check()?;
            *workspace_coverage
                .target_equivalence
                .entry(
                    c.capabilities
                        .get("target_equivalence")
                        .cloned()
                        .unwrap_or_else(|| "unknown".into()),
                )
                .or_default() += 1;
            *workspace_coverage
                .test_classification
                .entry(
                    c.capabilities
                        .get("test_classification")
                        .cloned()
                        .unwrap_or_else(|| "unknown".into()),
                )
                .or_default() += 1;
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
            snapshot,
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
        let mut relevant_roots = BTreeSet::new();
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
                let keys = statement
                    .query_map(
                        params![
                            self.generation,
                            request.name,
                            terms,
                            request.target,
                            request.limit + 1,
                            request.compilation_id
                        ],
                        |r| r.get::<_, String>(0),
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result.truncated = keys.len() > request.limit as usize;
                let mut budget = ProjectionBudget::new(self, &request)?;
                for group in keys.into_iter().take(request.limit as usize) {
                    self.check()?;
                    let mut representative = None;
                    let mut supports = vec![];
                    crate::grouping::members(self, &request, &group, |key| {
                        let symbol = crate::graph::symbol_full(&self.connection, key)?;
                        if let Some(first) = &representative {
                            if supports.is_empty() {
                                let support = SymbolSupport::from(first);
                                budget.charge(&support)?;
                                supports.push(support);
                            }
                            let support = SymbolSupport::from(&symbol);
                            budget.charge(&support)?;
                            supports.push(support);
                        } else {
                            budget.charge(&symbol)?;
                            representative = Some(symbol);
                        }
                        Ok(())
                    })?;
                    if let Some(mut symbol) = representative {
                        symbol.supporting_targets = supports;
                        result.symbols.push(symbol);
                    }
                }
            }
            "references" => {
                let (occurrences, truncated, owners) = self.references(&request)?;
                result.occurrences = occurrences;
                result.truncated = truncated;
                relevant_roots = owners;
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
            let mut relevant = relevant_roots;
            for s in &result.symbols {
                relevant.insert(s.compilation_id.clone());
                relevant.extend(
                    s.supporting_targets
                        .iter()
                        .map(|s| s.compilation_id.clone()),
                );
            }
            for c in &result.contexts {
                relevant.insert(c.symbol.compilation_id.clone());
                relevant.extend(
                    c.supporting_targets
                        .iter()
                        .map(|s| s.symbol.compilation_id.clone()),
                );
            }
            for o in &result.occurrences {
                relevant.insert(o.compilation_id.clone());
                relevant.extend(
                    o.supporting_targets
                        .iter()
                        .map(|s| s.compilation_id.clone()),
                );
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

    fn references(
        &self,
        request: &QueryRequest,
    ) -> Result<(Vec<OccurrenceResult>, bool, BTreeSet<String>)> {
        let roots = crate::grouping::roots(self, request)?;
        let mut owners = BTreeSet::new();
        for key in &roots {
            self.check()?;
            owners.insert(self.connection.query_row(
                "SELECT compilation_id FROM symbols WHERE rowid=?",
                [key],
                |r| r.get::<_, String>(0),
            )?);
        }
        let roots_json = serde_json::to_string(&roots)?;
        let predicate = request.tests.predicate("t.mask");
        // Equivalent use locations need the same referent group, evaluated
        // origin family, exact source bytes/range, and classification mask.
        let family = if self.format_version >= 8 {
            "CASE WHEN c.target_family<>'' AND a.generated=0 THEN 'f:'||c.target_family ELSE 'c:'||c.id END"
        } else {
            "'c:'||c.id"
        };
        let joins = if self.format_version >= 3 {
            "JOIN occurrences o ON o.symbol_key=s.symbol_key JOIN artifacts a ON a.rowid=o.artifact_key"
        } else {
            "JOIN occurrences o ON o.generation=s.generation AND o.compilation_id=s.compilation_id AND o.symbol_id=s.id JOIN artifacts a ON a.generation=o.generation AND a.id=o.artifact_id"
        };
        // Select logical groups before materializing support. The second pass
        // joins the selected keys once, not once per result or per framework.
        let key =
            "path,start_byte,end_byte,family,finding_key,artifact_id,content_hash,generated,mask";
        let sql=format!("WITH uses AS NOT MATERIALIZED (
            SELECT s.id AS symbol_id,s.compilation_id,c.target,s.generation AS owner_generation,
                {family} AS family,s.finding_key,a.id AS artifact_id,a.path,a.content_hash,
                o.start_byte,o.end_byte,a.generated,t.mask
            FROM grouped_symbols s {joins}
            JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id
            JOIN test_origin_masks t ON t.owner_generation=s.generation AND t.compilation_id=s.compilation_id AND t.artifact_id=a.id
            WHERE s.symbol_key IN(SELECT value FROM json_each(?1)) AND o.role='reference' AND {predicate}
        ), selected AS MATERIALIZED (SELECT {key} FROM uses GROUP BY {key} ORDER BY {key} LIMIT ?2)
        SELECT dense_rank() OVER (ORDER BY {key}),symbol_id,compilation_id,target,artifact_id,path,content_hash,start_byte,end_byte,generated,owner_generation
        FROM uses JOIN selected USING({key}) ORDER BY {key},target,compilation_id,symbol_id");
        let mut q = self.connection.prepare(&sql)?;
        let mut rows = q.query(params![roots_json, request.limit + 1])?;
        let mut output = Vec::<OccurrenceResult>::new();
        let mut budget = ProjectionBudget::new(self, request)?;
        while let Some(r) = rows.next()? {
            self.check()?;
            let rank = r.get::<_, usize>(0)?;
            if rank > request.limit as usize {
                return Ok((output, true, owners));
            }
            let support = OccurrenceSupport {
                symbol_id: r.get(1)?,
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
                test_classification: Default::default(),
            };
            let mut support = support;
            support.test_classification = crate::testing::origin(
                &self.connection,
                r.get(10)?,
                &support.compilation_id,
                &support.location.artifact_id,
                &support.location.path,
            )?;
            if rank > output.len() {
                let occurrence = OccurrenceResult {
                    symbol_id: support.symbol_id,
                    role: "reference".into(),
                    compilation_id: support.compilation_id,
                    target: support.target,
                    location: support.location,
                    test_classification: support.test_classification,
                    supporting_targets: vec![],
                };
                budget.charge(&occurrence)?;
                output.push(occurrence);
            } else {
                let occurrence = &mut output[rank - 1];
                if occurrence.supporting_targets.is_empty() {
                    let first = OccurrenceSupport {
                        symbol_id: occurrence.symbol_id.clone(),
                        compilation_id: occurrence.compilation_id.clone(),
                        target: occurrence.target.clone(),
                        location: occurrence.location.clone(),
                        test_classification: occurrence.test_classification.clone(),
                    };
                    budget.charge(&first)?;
                    occurrence.supporting_targets.push(first);
                }
                budget.charge(&support)?;
                occurrence.supporting_targets.push(support);
            }
        }
        Ok((output, false, owners))
    }

    fn symbol_context(&self, request: &QueryRequest) -> Result<(Vec<SymbolContext>, bool)> {
        let roots = crate::grouping::roots(self, request)?;
        let mut groups = BTreeMap::<String, Vec<i64>>::new();
        for key in roots {
            self.check()?;
            if request
                .tests
                .matches(crate::testing::symbol_mask(&self.connection, key)?)
            {
                groups
                    .entry(crate::grouping::key(&self.connection, key)?)
                    .or_default()
                    .push(key);
            }
        }
        let mut truncated = groups.len() > request.limit as usize;
        let mut budget = source_budget(request.max_bytes)?;
        let mut projection = ProjectionBudget::new(self, request)?;
        let mut contexts = vec![];
        let mut declaration_slots = request.limit as usize;
        for keys in groups.into_values().take(request.limit as usize) {
            if declaration_slots == 0 {
                truncated = true;
                break;
            }
            let mut used_slots = 0;
            let multiple = keys.len() > 1;
            let mut representative = None;
            let mut supporting_targets = vec![];
            for key in keys {
                self.check()?;
                // Reuse the same bounded symbol/declaration projection as graph reads.
                let mut symbol = crate::graph::symbol(&self.connection, key)?;
                truncated |= symbol.locations_limited || symbol.locations.len() > declaration_slots;
                // Equivalent targets share declaration slots, but each retains
                // its own exact evidence and consumes the shared source budget.
                used_slots = used_slots.max(symbol.locations.len().min(declaration_slots));
                let mut declarations = vec![];
                for location in std::mem::take(&mut symbol.locations)
                    .into_iter()
                    .take(declaration_slots)
                {
                    self.check()?;
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
                if multiple {
                    let mut metadata = SymbolSupport::from(&symbol);
                    metadata.locations = declarations.iter().map(|d| d.location.clone()).collect();
                    let support = ContextSupport {
                        symbol: metadata,
                        declarations: declarations.clone(),
                        boundary,
                    };
                    projection.charge(&support)?;
                    supporting_targets.push(support);
                }
                if representative.is_none() {
                    let context = SymbolContext {
                        symbol,
                        declarations,
                        supporting_targets: vec![],
                        boundary,
                    };
                    projection.charge(&context)?;
                    representative = Some(context);
                }
            }
            declaration_slots -= used_slots;
            if let Some(mut context) = representative {
                context.supporting_targets = supporting_targets;
                contexts.push(context);
            }
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

    /// Missing policy evidence requires ordinary bounded reanalysis, not restoration.
    pub fn needs_test_classification(&self) -> Result<bool> {
        let missing = if self.format_version >= 7 {
            "coalesce(json_extract(nullif(c.test_classification_json,''),'$.policy_version'),0)<>1"
        } else {
            "1"
        };
        Ok(self.connection.query_row(&format!("SELECT EXISTS(SELECT 1 FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id WHERE u.generation=? AND c.language IN ('typescript','csharp') AND {missing})"),[self.generation],|r|r.get(0))?)
    }

    /// Missing C# policy evidence schedules ordinary bounded backfill.
    pub fn needs_target_equivalence(&self) -> Result<bool> {
        let missing = if self.format_version >= 8 {
            "c.target_equivalence_policy<>1"
        } else {
            "1"
        };
        Ok(self.connection.query_row(&format!("SELECT EXISTS(SELECT 1 FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id WHERE u.generation=? AND c.language='csharp' AND {missing})"),[self.generation],|r|r.get(0))?)
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
                capabilities: {
                    let mut capabilities: BTreeMap<String, String> =
                        serde_json::from_str(&r.get::<_, String>(10)?).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                10,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;
                    capabilities
                        .entry("test_classification".into())
                        .or_insert_with(|| "unknown".into());
                    capabilities
                        .entry("target_equivalence".into())
                        .or_insert_with(|| "unknown".into());
                    capabilities
                },
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
    let mut sql = "SELECT s.finding_key ".to_owned();
    if request.name.is_some() {
        sql.push_str("FROM generation_units u CROSS JOIN grouped_symbols s ON s.generation=u.owner_generation AND s.compilation_id=u.compilation_id CROSS JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id");
        if request.query.is_some() {
            sql.push_str(" CROSS JOIN symbol_search ON symbol_search.rowid=s.symbol_key");
        }
    } else if request.query.is_some() {
        sql.push_str("FROM symbol_search CROSS JOIN grouped_symbols s ON s.symbol_key=symbol_search.rowid CROSS JOIN generation_units u ON u.owner_generation=s.generation AND u.compilation_id=s.compilation_id CROSS JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id");
    } else {
        sql.push_str("FROM generation_units u JOIN grouped_symbols s ON s.generation=u.owner_generation AND s.compilation_id=u.compilation_id JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id");
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
    if request.tests != crate::testing::TestFilter::All {
        sql.push_str(" AND ");
        sql.push_str(&request.tests.predicate("s.test_mask"));
    }
    sql.push_str(" AND (?6 IS NULL OR s.compilation_id=?6) GROUP BY s.finding_key ORDER BY MIN(s.name),MIN(s.id),MIN(s.compilation_id) LIMIT ?5");
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
    fn cursor_materialization_has_one_bounded_aggregate_budget_before_page_packing() {
        use crate::protocol::{FactBatch, Occurrence, SourceArtifact, Symbol};
        use sha2::{Digest, Sha256};
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/grouping-materialization")
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
        for comp in ["a", "b"] {
            writer
                .stage(Payload::Compilation(Compilation {
                    id: comp.into(),
                    target: comp.into(),
                    target_family: "family".into(),
                    target_equivalence_policy: 1,
                    ..Default::default()
                }))
                .unwrap();
            writer
                .stage(Payload::Source(SourceArtifact {
                    id: "body".into(),
                    path: "body.cs".into(),
                    compilation_id: comp.into(),
                    utf8: b"x".to_vec(),
                    sha256: format!("{:x}", Sha256::digest(b"x")),
                    ..Default::default()
                }))
                .unwrap();
            writer
                .stage(Payload::Facts(FactBatch {
                    compilation_id: comp.into(),
                    symbols: (0..20)
                        .map(|i| Symbol {
                            id: format!("{comp}:{i}"),
                            name: format!("Member{i:02}"),
                            kind: "method".into(),
                            signature: "signature".repeat(64),
                            declaration_key: format!("decl:{i}"),
                            ..Default::default()
                        })
                        .collect(),
                    occurrences: (0..20)
                        .map(|i| Occurrence {
                            symbol_id: format!("{comp}:{i}"),
                            artifact_id: "body".into(),
                            role: "declaration".into(),
                            end_byte: 1,
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                }))
                .unwrap();
            writer
                .stage(Payload::Report(AnalysisReport {
                    compilation_id: comp.into(),
                    status: "complete".into(),
                    ..Default::default()
                }))
                .unwrap();
        }
        writer.publish().unwrap();
        let view = ReadView::open(&path).unwrap();
        let request: QueryRequest =
            serde_json::from_value(serde_json::json!({"op":"symbols","response_max_bytes":8192}))
                .unwrap();
        assert!(matches!(
            view.query(request.clone()),
            Err(Error::ResponseLimitExceeded)
        ));
        let result = view
            .query_materialized_controlled(
                request,
                crate::execution::Execution::new(Default::default(), None).unwrap(),
            )
            .unwrap();
        assert_eq!(result.symbols.len(), 20);
        assert!(
            result
                .symbols
                .iter()
                .all(|symbol| symbol.supporting_targets.len() == 2)
        );
        assert!(serde_json::to_vec(&result).unwrap().len() > 8192);
    }
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
                        .query_map(
                            params![
                                view.generation,
                                name,
                                text,
                                target,
                                101,
                                Option::<String>::None
                            ],
                            |r| r.get(3),
                        )
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
