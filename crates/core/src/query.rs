use crate::{Error, Result, protocol::Diagnostic, store::SCHEMA_VERSION};
use rusqlite::{Connection, OpenFlags, OptionalExtension, config::DbConfig, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

pub const DEFAULT_SOURCE_BYTES: u32 = 16 * 1024;
pub const MAX_SOURCE_BYTES: u32 = 256 * 1024;
const MAX_LINE_CHECKPOINTS: usize = 128;

#[derive(Clone, Copy)]
struct LineCheckpoint {
    line: u64,
    previous_cr: bool,
}

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
    pub include_source: Option<bool>,
    pub source_max_bytes: Option<u32>,
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub locations_limited: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct SymbolSupport {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub locations_limited: bool,
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
            locations_limited: s.locations_limited,
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
#[derive(Clone, Debug, Serialize)]
pub struct DeclarationProjection {
    pub declarations: Vec<DeclarationContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<&'static str>,
}
pub(crate) struct DeclarationOptions {
    pub allowance: usize,
    pub include_source: bool,
    pub duplicate: bool,
}
pub(crate) struct DeclarationLimits {
    pub declarations: bool,
    pub source: bool,
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
    pub coordination: Option<QueryCoordination>,
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

/// Freshness evidence for ordinary queries. Full configuration, progress and
/// error detail remain available through the workspace-status operation.
#[derive(Clone, Debug, Serialize)]
pub struct QueryCoordination {
    pub workspace_root: String,
    pub database: String,
    pub phase: String,
    pub observed_epoch: u64,
    pub published_epoch: u64,
    pub generation: u64,
    pub available: bool,
    pub has_error: bool,
    pub outcome_code: String,
    pub status_limited: bool,
}
impl From<&crate::protocol::WorkspaceStatus> for QueryCoordination {
    fn from(status: &crate::protocol::WorkspaceStatus) -> Self {
        let (phase, phase_limited) = Self::token(&status.phase);
        let (outcome_code, outcome_limited) = Self::token(&status.outcome_code);
        Self {
            workspace_root: status.workspace_root.clone(),
            database: status.database.clone(),
            phase,
            observed_epoch: status.observed_epoch,
            published_epoch: status.published_epoch,
            generation: status.generation,
            available: status.available,
            has_error: !status.error.is_empty(),
            outcome_code,
            status_limited: phase_limited || outcome_limited,
        }
    }
}
impl QueryCoordination {
    const MAX_TOKEN_BYTES: usize = 32;
    fn token(value: &str) -> (String, bool) {
        if value.len() <= Self::MAX_TOKEN_BYTES
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            (value.into(), false)
        } else {
            ("unknown".into(), true)
        }
    }
    pub(crate) fn bounded(mut self) -> Self {
        for token in [&mut self.phase, &mut self.outcome_code] {
            let (projected, limited) = Self::token(token);
            *token = projected;
            self.status_limited |= limited;
        }
        self
    }
    // The identities stay frozen with each saved page; only these finite
    // status fields can grow. False is the longest JSON boolean spelling.
    pub(crate) fn maximum_live(workspace_root: String, database: String) -> Self {
        Self {
            workspace_root,
            database,
            phase: "x".repeat(Self::MAX_TOKEN_BYTES),
            observed_epoch: u64::MAX,
            published_epoch: u64::MAX,
            generation: u64::MAX,
            available: false,
            has_error: false,
            outcome_code: "x".repeat(Self::MAX_TOKEN_BYTES),
            status_limited: false,
        }
    }
}

struct SourceRead<'a> {
    artifact_id: &'a str,
    content_hash: Option<&'a str>,
    start_byte: Option<u64>,
    end_byte: Option<u64>,
    budget: u32,
    owner: Option<(u64, &'a str)>,
}
pub(crate) struct ProjectionBudget {
    remaining: usize,
    execution: Option<crate::execution::Execution>,
}
impl ProjectionBudget {
    pub(crate) fn new(view: &ReadView, request: &QueryRequest) -> Result<Self> {
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
    pub(crate) fn fork(&self) -> Self {
        Self {
            remaining: self.remaining,
            execution: self.execution.clone(),
        }
    }
    pub(crate) fn charge(&mut self, value: &impl Serialize) -> Result<()> {
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
    // Request-local, bounded hints into immutable indexed content. The hash
    // keeps compilation owners sharing bytes safe without conflating sources.
    line_checkpoints: RefCell<BTreeMap<(String, u64), LineCheckpoint>>,
    pub(crate) health: crate::recovery::IndexReadGuard,
    logical_identity: Option<Vec<u8>>,
    // Public views retain their slot even while idle; controlled nested views
    // extend their existing outer request's charge rather than acquire again.
    _permit: Option<crate::admission::Permit>,
}
impl ReadView {
    /// Bounded coordinator metadata inspection remains available when all
    /// consumer slots are occupied. It never returns an unadmitted read view.
    pub fn readiness(path: impl AsRef<Path>) -> Result<(u64, bool)> {
        let execution = crate::execution::Execution::new(Default::default(), None)?;
        let view = Self::open_controlled(path, execution)?;
        let result = (|| {
            view.check()?;
            let readiness = (
                view.generation,
                view.needs_test_classification()?
                    || view.needs_target_equivalence()?
                    || view.needs_provider_backfill()?,
            );
            view.check()?;
            Ok(readiness)
        })();
        crate::index_result(&view.health, result)
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_reserved(path.as_ref(), None, None)
    }
    pub fn open_in_scope(
        path: impl AsRef<Path>,
        scope: &crate::admission::AdmissionScope,
    ) -> Result<Self> {
        Self::open_reserved(path.as_ref(), Some(scope), None)
    }
    fn open_reserved(
        path: &Path,
        scope: Option<&crate::admission::AdmissionScope>,
        pin: Option<&crate::cursor::GenerationPin>,
    ) -> Result<Self> {
        let execution = crate::execution::Execution::new(Default::default(), None)?;
        let local;
        let scope = match scope {
            Some(scope) => scope,
            None => {
                local = crate::admission::AdmissionScope::local()?;
                &local
            }
        };
        let reservation = scope.try_reserve()?;
        let view = Self::open_selected(path, execution.with_permit(reservation.submit()?), pin)?;
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
        Self::open_reserved(&pin.database, None, Some(pin))
    }
    pub(crate) fn open_pinned_in_scope(
        pin: &crate::cursor::GenerationPin,
        scope: &crate::admission::AdmissionScope,
    ) -> Result<Self> {
        Self::open_reserved(&pin.database, Some(scope), Some(pin))
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
        let permit = execution.permit();
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
        if version == 1 && !health.incarnation().is_empty() {
            return Err(Error::NotReady);
        }
        if !crate::store::READABLE_FORMATS.contains(&version) {
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
            line_checkpoints: RefCell::new(BTreeMap::new()),
            health: health.clone(),
            logical_identity,
            _permit: permit,
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
        let mut execution = execution.with_cancellation(cancellation);
        if let Some(permit) = &self._permit {
            execution = execution.with_permit(permit.clone());
        }
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
        self.line_checkpoints.borrow_mut().clear();
        let result = crate::index_result(
            &self.health,
            self.check().and_then(|_| self.query_inner(request)),
        );
        let elapsed = execution.check();
        self.line_checkpoints.borrow_mut().clear();
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
        let graph = matches!(request.op.as_str(), "callers" | "callees" | "impact");
        if request.source_max_bytes.is_some() && !graph {
            return Err(Error::InvalidRequest(
                "source_max_bytes applies only to graph queries".into(),
            ));
        }
        if graph {
            source_budget_named(request.source_max_bytes, "source_max_bytes")?;
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
                        let symbol = crate::graph::symbol(&self.connection, key)?;
                        result.truncated |= symbol.locations_limited;
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
        if request.coverage_detail != CoverageDetail::Full && request.op != "coverage" {
            let mut relevant = relevant_roots;
            if let Some(graph) = &result.graph {
                relevant.extend(graph.relevant_compilations.iter().cloned());
            }
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
            owners.insert(
                self.connection
                    .prepare_cached("SELECT compilation_id FROM symbols WHERE rowid=?")?
                    .query_row([key], |r| r.get::<_, String>(0))?,
            );
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
                o.start_byte,o.end_byte,a.generated,t.mask,t.classification_json
            FROM grouped_symbols s {joins}
            JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id
            JOIN test_origin_masks t ON t.owner_generation=s.generation AND t.compilation_id=s.compilation_id AND t.artifact_id=a.id
            WHERE s.symbol_key IN(SELECT value FROM json_each(?1)) AND o.role='reference' AND {predicate}
        ), selected AS MATERIALIZED (SELECT {key} FROM uses GROUP BY {key} ORDER BY {key} LIMIT ?2)
        SELECT dense_rank() OVER (ORDER BY {key}),symbol_id,compilation_id,target,artifact_id,path,content_hash,start_byte,end_byte,generated,classification_json
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
                &support.location.artifact_id,
                &support.location.path,
                &r.get::<_, String>(10)?,
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
                let mut metadata = projection.fork();
                let (context, limits) = self.project_declarations(
                    &symbol,
                    DeclarationOptions {
                        allowance: declaration_slots,
                        include_source: request.include_source.unwrap_or(true),
                        duplicate: multiple && representative.is_none(),
                    },
                    &mut budget,
                    &mut metadata,
                )?;
                truncated |= limits.declarations || limits.source;
                let DeclarationProjection {
                    declarations,
                    boundary,
                } = context;
                // Context stores anchors once in declarations; graph callers
                // retain their compact symbol locations when using the helper.
                symbol.locations.clear();
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

    pub(crate) fn project_declarations(
        &self,
        symbol: &SymbolResult,
        options: DeclarationOptions,
        budget: &mut u32,
        metadata: &mut ProjectionBudget,
    ) -> Result<(DeclarationProjection, DeclarationLimits)> {
        self.check()?;
        let DeclarationOptions {
            allowance,
            include_source,
            duplicate,
        } = options;
        let mut limits = DeclarationLimits {
            declarations: symbol.locations_limited || symbol.locations.len() > allowance,
            source: false,
        };
        let copies = if duplicate { 2 } else { 1 };
        let mut declarations = vec![];
        for location in symbol.locations.iter().take(allowance) {
            self.check()?;
            let line = self.source_line(&location.content_hash, location.start_byte)?;
            let mut extents = self.connection.prepare_cached("SELECT artifact_id,path,content_hash,start_byte,end_byte,generated FROM occurrence_locations WHERE generation=? AND compilation_id=? AND symbol_id=? AND role='definition' AND artifact_id=? AND content_hash=? AND start_byte<=? AND end_byte>=? ORDER BY start_byte,end_byte LIMIT 2")?;
            let ranges = extents
                .query_map(
                    params![
                        symbol.owner_generation,
                        symbol.compilation_id,
                        symbol.id,
                        location.artifact_id,
                        location.content_hash,
                        location.start_byte,
                        location.end_byte
                    ],
                    read_location,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let (extent, mut status) = match ranges.len() {
                1 => (ranges.into_iter().next(), "omitted"),
                0 => (None, "extent_unavailable"),
                _ => (None, "extent_ambiguous"),
            };
            let mut source = None;
            if include_source && let Some(extent) = &extent {
                let available = *budget / copies;
                if available == 0 {
                    status = "budget_exhausted";
                    limits.source = true;
                } else {
                    let (body, clipped) = self.source_excerpt_scoped(
                        SourceRead {
                            artifact_id: &extent.artifact_id,
                            content_hash: Some(&extent.content_hash),
                            start_byte: Some(extent.start_byte),
                            end_byte: Some(extent.end_byte),
                            budget: available,
                            owner: Some((symbol.owner_generation, &symbol.compilation_id)),
                        },
                        true,
                    )?;
                    *budget -= (body.end_byte - body.start_byte) as u32 * copies;
                    status = if body.text.is_empty() && clipped {
                        "budget_exhausted"
                    } else if clipped {
                        "truncated"
                    } else {
                        "included"
                    };
                    limits.source |= clipped;
                    source = Some(body);
                }
            }
            let declaration = DeclarationContext {
                location: location.clone(),
                line,
                extent,
                source,
                source_status: status,
            };
            metadata.charge(&declaration)?;
            if duplicate {
                metadata.charge(&declaration)?;
            }
            declarations.push(declaration);
        }
        let boundary = symbol.locations.is_empty().then_some(if symbol.external {
            "external_or_other_compilation_body"
        } else {
            "declaration_unavailable"
        });
        Ok((
            DeclarationProjection {
                declarations,
                boundary,
            },
            limits,
        ))
    }

    // Streaming the indexed blob avoids loading an entire file or relying on
    // SQLite TEXT operations (which stop at embedded NULs) for line positions.
    fn source_line(&self, hash: &str, offset: u64) -> Result<u64> {
        self.check()?;
        let checkpoint = self
            .line_checkpoints
            .borrow()
            .range((hash.to_owned(), 0)..=(hash.to_owned(), offset))
            .next_back()
            .map(|((_, start), checkpoint)| (*start, *checkpoint));
        if let Some((start, checkpoint)) = checkpoint
            && start == offset
        {
            return Ok(checkpoint.line);
        }
        let (start, checkpoint) = checkpoint.unwrap_or((
            0,
            LineCheckpoint {
                line: 1,
                previous_cr: false,
            },
        ));
        let rowid = self
            .connection
            .prepare_cached("SELECT rowid FROM contents WHERE hash=?")?
            .query_row([hash], |r| r.get(0))?;
        let mut blob = self
            .connection
            .blob_open("main", "contents", "utf8", rowid, true)?;
        blob.seek(SeekFrom::Start(start))?;
        let mut remaining = offset - start;
        let mut line = checkpoint.line;
        let mut previous_cr = checkpoint.previous_cr;
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
        let mut checkpoints = self.line_checkpoints.borrow_mut();
        if checkpoints.len() == MAX_LINE_CHECKPOINTS {
            checkpoints.clear();
        }
        checkpoints.insert(
            (hash.to_owned(), offset),
            LineCheckpoint { line, previous_cr },
        );
        Ok(line)
    }

    fn source_excerpt(
        &self,
        request: &QueryRequest,
        allow_empty: bool,
    ) -> Result<(SourceResult, bool)> {
        let id = request
            .artifact_id
            .as_deref()
            .ok_or_else(|| Error::InvalidRequest("source requires artifact_id".into()))?;
        self.source_excerpt_scoped(
            SourceRead {
                artifact_id: id,
                content_hash: request.content_hash.as_deref(),
                start_byte: request.start_byte,
                end_byte: request.end_byte,
                budget: source_budget(request.max_bytes)?,
                owner: None,
            },
            allow_empty,
        )
    }
    fn source_excerpt_scoped(
        &self,
        read: SourceRead<'_>,
        allow_empty: bool,
    ) -> Result<(SourceResult, bool)> {
        self.check()?;
        let id = read.artifact_id;
        let budget = read.budget;
        if (read.start_byte.is_some() || read.end_byte.is_some()) && read.content_hash.is_none() {
            return Err(Error::InvalidRequest(
                "source ranges require content_hash from the indexed location".into(),
            ));
        }
        let start = read.start_byte.unwrap_or(0);
        let sql_start = i64::try_from(start)
            .map_err(|_| Error::InvalidRequest("source start_byte is out of range".into()))?;
        let sql_end = read
            .end_byte
            .map(i64::try_from)
            .transpose()
            .map_err(|_| Error::InvalidRequest("source end_byte is out of range".into()))?;
        let row = self.connection.query_row(
            "SELECT a.path,a.content_hash,length(c.utf8),substr(c.utf8,?3+1,?4),CASE WHEN ?5 IS NULL THEN X'' ELSE substr(c.utf8,?5+1,1) END FROM generation_units u JOIN unit_artifacts ua ON ua.owner_generation=u.owner_generation AND ua.compilation_id=u.compilation_id JOIN artifacts a ON a.generation=ua.owner_generation AND a.id=ua.artifact_id JOIN contents c ON c.hash=a.content_hash WHERE u.generation=?1 AND a.id=?2 AND (?6 IS NULL OR (u.owner_generation=?6 AND u.compilation_id=?7 AND a.content_hash=?8)) LIMIT 1",
            params![self.generation,id,sql_start,budget+3,sql_end,read.owner.map(|o|o.0),read.owner.map(|o|o.1),read.owner.and(read.content_hash)],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,u64>(2)?,r.get::<_,Vec<u8>>(3)?,r.get::<_,Vec<u8>>(4)?)),
        ).optional()?.ok_or_else(|| Error::InvalidRequest("source artifact is absent from the served generation; restart lookup".into()))?;
        let (path, hash, total, mut bytes, end_byte) = row;
        if read
            .content_hash
            .is_some_and(|expected| expected != hash.as_str())
        {
            return Err(Error::InvalidRequest(
                "indexed source content changed; restart lookup before reusing its byte range"
                    .into(),
            ));
        }
        let end = read.end_byte.unwrap_or(total);
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
                artifact_id: id.into(),
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

    /// Older Rust extraction policies need ordinary bounded reanalysis.
    fn needs_provider_backfill(&self) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id WHERE u.generation=? AND c.language='rust' AND coalesce(json_extract(nullif(c.options_json,''),'$.rustContextPolicy'),0)<>2)",
            [self.generation], |r| r.get(0),
        )?)
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
    source_budget_named(value, "source/detail max_bytes")
}
fn source_budget_named(value: Option<u32>, field: &str) -> Result<u32> {
    let budget = value.unwrap_or(DEFAULT_SOURCE_BYTES);
    if !(1..=MAX_SOURCE_BYTES).contains(&budget) {
        return Err(Error::InvalidRequest(format!(
            "{field} must be between 1 and {MAX_SOURCE_BYTES}"
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
pub(crate) fn fixture_scope(path: &Path) -> Result<crate::admission::AdmissionScope> {
    crate::admission::AdmissionScope::at(
        path.parent()
            .ok_or_else(|| Error::InvalidRequest("fixture database needs a parent".into()))?
            .join("query-admission"),
    )
}

#[cfg(test)]
pub(crate) fn fixture_view(path: impl AsRef<Path>) -> Result<ReadView> {
    let path = path.as_ref();
    ReadView::open_in_scope(path, &fixture_scope(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::{AnalysisReport, Compilation, envelope::Payload},
        store::Writer,
    };

    #[test]
    fn query_coordination_is_bounded_without_configuration_or_error_payloads() {
        let mut status = crate::protocol::WorkspaceStatus {
            workspace_root: "workspace/π".into(),
            database: "state/index.db".into(),
            phase: "resource_limited".into(),
            outcome_code: "disk_pressure".into(),
            generation: u64::MAX,
            observed_epoch: u64::MAX,
            published_epoch: u64::MAX,
            error: "compiler detail".repeat(100_000),
            progress: Some(crate::protocol::Progress {
                message: "progress".repeat(100_000),
                ..Default::default()
            }),
            requested_settings: Some(crate::protocol::WorkspaceAnalysisSettings {
                configuration_origin: "configuration".repeat(100_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let projected = QueryCoordination::from(&status);
        let bytes = serde_json::to_vec(&projected).unwrap();
        let maximum =
            QueryCoordination::maximum_live(status.workspace_root.clone(), status.database.clone());
        assert!(bytes.len() <= serde_json::to_vec(&maximum).unwrap().len());
        let value = serde_json::to_value(projected).unwrap();
        assert_eq!(value["workspace_root"], status.workspace_root);
        assert_eq!(value["database"], status.database);
        assert_eq!(value["has_error"], true);
        assert_eq!(value["outcome_code"], "disk_pressure");
        assert_eq!(value["status_limited"], false);
        for omitted in ["error", "progress", "requested_settings"] {
            assert!(value.get(omitted).is_none());
        }
        status.phase = "\0".repeat(100_000);
        status.outcome_code = "e".repeat(100_000);
        let limited = QueryCoordination::from(&status);
        assert!(limited.status_limited && limited.has_error);
        assert_eq!(limited.phase, "unknown");
        assert_eq!(limited.outcome_code, "unknown");
        assert!(
            serde_json::to_vec(&limited).unwrap().len()
                <= serde_json::to_vec(&maximum).unwrap().len()
        );
    }

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
        let view = crate::query::fixture_view(&path).unwrap();
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
        let view = crate::query::fixture_view(&path).unwrap();
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
    #[test]
    fn test_mask_index_plan_comparison_preserves_symbol_results() {
        use crate::protocol::{
            FactBatch, Occurrence, SourceArtifact, Symbol, TestClassification, TestEvidence,
        };
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/test-mask-plans")
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
        for unit in 0..4 {
            let id = format!("unit{unit}");
            writer
                .stage(Payload::Compilation(Compilation {
                    id: id.clone(),
                    target: format!("target{}", unit % 2),
                    ..Default::default()
                }))
                .unwrap();
            for kind in ["unknown", "test", "non_test"] {
                writer
                    .stage(Payload::Source(SourceArtifact {
                        id: format!("{id}/{kind}"),
                        path: format!("{id}/{kind}.ts"),
                        utf8: b"x".to_vec(),
                        sha256: format!("{:x}", Sha256::digest(b"x")),
                        compilation_id: id.clone(),
                        test_classification: Some(TestClassification {
                            policy_version: crate::testing::POLICY_VERSION,
                            kind: kind.into(),
                            evidence: vec![TestEvidence {
                                basis: "fixture".into(),
                                value: kind.into(),
                                path: id.clone(),
                                inferred: false,
                            }],
                        }),
                        ..Default::default()
                    }))
                    .unwrap();
            }
            let mut facts = FactBatch {
                compilation_id: id.clone(),
                ..Default::default()
            };
            for number in 0..256 {
                let symbol_id = format!("symbol{number:04}");
                facts.symbols.push(Symbol {
                    id: symbol_id.clone(),
                    name: if number % 8 == 0 {
                        "needle".into()
                    } else {
                        format!("member{number:04}")
                    },
                    kind: "function".into(),
                    ..Default::default()
                });
                let mask = number % 7 + 1;
                for (bit, kind) in [(1, "unknown"), (2, "test"), (4, "non_test")] {
                    if mask & bit != 0 {
                        facts.occurrences.push(Occurrence {
                            symbol_id: symbol_id.clone(),
                            artifact_id: format!("{id}/{kind}"),
                            role: "declaration".into(),
                            end_byte: 1,
                            ..Default::default()
                        });
                    }
                }
            }
            writer.stage(Payload::Facts(facts)).unwrap();
            writer
                .stage(Payload::Report(AnalysisReport {
                    compilation_id: id,
                    status: "complete".into(),
                    ..Default::default()
                }))
                .unwrap();
        }
        writer.publish().unwrap();

        // Exact production SQL, including target/compilation scopes and all
        // masks. Compare both unanalysed and analysed planner statistics.
        let mut evidence = vec![];
        for analyzed in [false, true] {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch("CREATE INDEX IF NOT EXISTS symbol_test_mask ON symbols(test_mask)")
                .unwrap();
            if analyzed {
                connection.execute_batch("ANALYZE").unwrap();
            }
            drop(connection);
            let capture = |with_index: bool| {
                let view = crate::query::fixture_view(&path).unwrap();
                let masks: Vec<(u8, u32)> = view.connection.prepare("SELECT test_mask,count(*) FROM symbols GROUP BY test_mask ORDER BY test_mask").unwrap()
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(std::result::Result::unwrap).collect();
                assert_eq!(
                    masks.iter().map(|(mask, _)| *mask).collect::<Vec<_>>(),
                    vec![1, 2, 3, 4, 5, 6, 7]
                );
                let sqlite_source: String = view
                    .connection
                    .query_row("SELECT sqlite_source_id()", [], |r| r.get(0))
                    .unwrap();
                let mut captures = vec![];
                for (name, text) in [
                    (None, None),
                    (Some("needle"), None),
                    (None, Some("needle")),
                    (Some("needle"), Some("needle")),
                ] {
                    for tests in ["all", "only", "exclude"] {
                        for target in [None, Some("target1")] {
                            for compilation in [None, Some("unit0")] {
                                for limit in [12, 1000] {
                                    let input = serde_json::json!({"op":"symbols", "name":name, "query":text, "tests":tests, "target":target, "compilation_id":compilation, "limit":limit});
                                    let request: QueryRequest =
                                        serde_json::from_value(input.clone()).unwrap();
                                    let sql = symbols_sql(&request);
                                    let terms = text.map(|text| format!("\"{text}\""));
                                    let parameters = params![
                                        view.generation,
                                        name,
                                        terms,
                                        target,
                                        limit + 1,
                                        compilation
                                    ];
                                    let plan: Vec<String> = view
                                        .connection
                                        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                                        .unwrap()
                                        .query_map(parameters, |r| r.get(3))
                                        .unwrap()
                                        .map(std::result::Result::unwrap)
                                        .collect();
                                    let keys: Vec<String> = view
                                        .connection
                                        .prepare(&sql)
                                        .unwrap()
                                        .query_map(parameters, |r| r.get(0))
                                        .unwrap()
                                        .map(std::result::Result::unwrap)
                                        .collect();
                                    // Also compare complete projected result/classification
                                    // bytes for every search/mask shape, at a small limit.
                                    let projected =
                                        if target.is_none() && compilation.is_none() && limit == 12
                                        {
                                            Some(
                                                serde_json::to_vec(&view.query(request).unwrap())
                                                    .unwrap(),
                                            )
                                        } else {
                                            None
                                        };
                                    captures.push((serde_json::json!({"request":input,"sql":sql,"plan":plan,"mask_index_used":plan.iter().any(|p| p.contains("symbol_test_mask")),"rows":keys.len(),"keys_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&keys).unwrap())),"projected_sha256":projected.as_ref().map(|bytes| format!("{:x}",Sha256::digest(bytes)))}), keys, projected));
                                }
                            }
                        }
                    }
                }
                (
                    serde_json::json!({"with_index":with_index,"sqlite_version":rusqlite::version(),"sqlite_source_id":sqlite_source,"mask_counts":masks,"mask_index_used_any":captures.iter().any(|(record, _, _)|record["mask_index_used"] == true),"queries":captures.iter().map(|(record, _, _)|record).collect::<Vec<_>>()}),
                    captures,
                )
            };
            let (before, before_rows) = capture(true);
            Connection::open(&path)
                .unwrap()
                .execute_batch("DROP INDEX symbol_test_mask")
                .unwrap();
            let (after, after_rows) = capture(false);
            let unchanged = before_rows.iter().zip(&after_rows).all(
                |((_, keys, projected), (_, other_keys, other_projected))| {
                    keys == other_keys && projected == other_projected
                },
            );
            evidence.push(serde_json::json!({"analyzed":analyzed,"results_unchanged":unchanged,"before":before,"after":after}));
            std::fs::write(
                path.with_file_name("comparison.json"),
                serde_json::to_vec_pretty(&evidence).unwrap(),
            )
            .unwrap();
            assert!(
                unchanged,
                "mask-index removal changed a bounded selection/projection; see {}",
                path.with_file_name("comparison.json").display()
            );
        }
        eprintln!(
            "test-mask plan evidence: {}",
            path.with_file_name("comparison.json").display()
        );
    }
    fn declaration_fixture() -> (std::path::PathBuf, [u64; 3]) {
        use crate::protocol::{FactBatch, Occurrence, SourceArtifact, Symbol};
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/declaration-projection")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("index.db");
        let mut generations = [0; 3];
        for (index, (comp, text)) in [
            ("left", "l left"),
            ("right", "r π\0😀"),
            ("third", "r π\0😀"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut writer = if index == 0 {
                Writer::open(&path).unwrap()
            } else {
                Writer::open_incremental(&path, &[comp.into()]).unwrap()
            };
            writer
                .stage(Payload::Compilation(Compilation {
                    id: comp.into(),
                    language: "typescript".into(),
                    ..Default::default()
                }))
                .unwrap();
            writer
                .stage(Payload::Source(SourceArtifact {
                    id: "collision".into(),
                    path: format!("{comp}.ts"),
                    compilation_id: comp.into(),
                    utf8: text.as_bytes().into(),
                    sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
                    test_classification: Some(crate::protocol::TestClassification {
                        policy_version: crate::testing::POLICY_VERSION,
                        kind: match comp {
                            "left" => "non_test",
                            "right" => "test",
                            _ => "unknown",
                        }
                        .into(),
                        evidence: vec![crate::protocol::TestEvidence {
                            basis: "fixture".into(),
                            value: comp.into(),
                            path: format!("{comp}.ts"),
                            ..Default::default()
                        }],
                    }),
                    ..Default::default()
                }))
                .unwrap();
            writer
                .stage(Payload::Facts(FactBatch {
                    compilation_id: comp.into(),
                    symbols: vec![Symbol {
                        id: "opaque".into(),
                        name: "r".into(),
                        ..Default::default()
                    }],
                    occurrences: vec![
                        Occurrence {
                            symbol_id: "opaque".into(),
                            artifact_id: "collision".into(),
                            role: "declaration".into(),
                            end_byte: 1,
                            ..Default::default()
                        },
                        Occurrence {
                            symbol_id: "opaque".into(),
                            artifact_id: "collision".into(),
                            role: "definition".into(),
                            end_byte: text.len() as u64,
                            ..Default::default()
                        },
                        Occurrence {
                            symbol_id: "opaque".into(),
                            artifact_id: "collision".into(),
                            role: "reference".into(),
                            end_byte: 1,
                            ..Default::default()
                        },
                    ],
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
            generations[index] = writer.publish().unwrap();
        }
        (path, generations)
    }
    #[test]
    fn references_project_joined_origin_evidence_without_point_queries() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let (path, _) = declaration_fixture();
        let mut view = crate::query::fixture_view(path).unwrap();
        view.connection
            .authorizer(Some(|context: AuthContext<'_>| {
                // The bounded reference stream already reads test_origins
                // through test_origin_masks. A second top-level lookup for
                // every projected reference would duplicate that SQL work.
                if matches!(
                    context.action,
                    AuthAction::Read {
                        table_name: "test_origins",
                        ..
                    }
                ) && context.accessor.is_none()
                {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }))
            .unwrap();
        let result = view
            .query(
                serde_json::from_value(serde_json::json!({"op":"references","symbol_id":"opaque"}))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(result.occurrences.len(), 3);
        for occurrence in result.occurrences {
            let expected = match occurrence.compilation_id.as_str() {
                "left" => "non_test",
                "right" => "test",
                _ => "unknown",
            };
            assert_eq!(occurrence.test_classification.kind, expected);
            assert_eq!(occurrence.test_classification.evidence.len(), 1);
            let evidence = &occurrence.test_classification.evidence[0];
            assert_eq!(evidence.artifact_id, occurrence.location.artifact_id);
            assert_eq!(evidence.source_path, occurrence.location.path);
            assert_eq!(evidence.evidence[0].value, occurrence.compilation_id);
        }
    }
    #[test]
    fn indexed_line_checkpoints_preserve_crlf_nul_utf8_and_request_bounds() {
        let (path, _) = declaration_fixture();
        let mut text = vec![b'x'; 8191];
        text.extend_from_slice("\r\n\0π😀\r\nz\n".as_bytes());
        text.extend_from_slice(&b"a\r\nb\nc\rd".repeat(160));
        let hash = format!("{:x}", Sha256::digest(&text));
        let mut writer = Writer::open_incremental(&path, &["lines".into()]).unwrap();
        writer
            .stage(Payload::Compilation(Compilation {
                id: "lines".into(),
                ..Default::default()
            }))
            .unwrap();
        writer
            .stage(Payload::Source(crate::protocol::SourceArtifact {
                id: "lines".into(),
                compilation_id: "lines".into(),
                path: "lines.ts".into(),
                utf8: text.clone(),
                sha256: hash.clone(),
                ..Default::default()
            }))
            .unwrap();
        writer
            .stage(Payload::Report(AnalysisReport {
                compilation_id: "lines".into(),
                status: "complete".into(),
                ..Default::default()
            }))
            .unwrap();
        writer.publish().unwrap();
        let view = crate::query::fixture_view(path).unwrap();
        assert!(view.source_line("not-indexed", 0).is_err());
        assert_eq!(view.source_line(&hash, 8193).unwrap(), 2);
        // Include a CRLF crossing the blob chunk boundary, offsets inside
        // multibyte text and backward seeks after later checkpoints exist.
        let offsets = (0..text.len())
            .step_by(7)
            .chain(8189..8210)
            .chain((8189..8210).rev());
        for offset in offsets {
            let prefix = &text[..offset];
            let breaks = prefix.iter().filter(|&&b| b == b'\r' || b == b'\n').count()
                - prefix.windows(2).filter(|pair| *pair == b"\r\n").count();
            assert_eq!(
                view.source_line(&hash, offset as u64).unwrap(),
                breaks as u64 + 1,
                "offset {offset}"
            );
            assert!(view.line_checkpoints.borrow().len() <= MAX_LINE_CHECKPOINTS);
        }
        let cancellation = crate::execution::QueryCancellation::default();
        let execution = crate::execution::Execution::new(Default::default(), None)
            .unwrap()
            .with_cancellation(Some(cancellation.clone()));
        *view.execution.lock().unwrap() = Some(execution);
        cancellation.cancel();
        assert!(matches!(
            view.source_line(&hash, 8193),
            Err(Error::QueryCancelled)
        ));
        *view.execution.lock().unwrap() = None;
        view.query(
            serde_json::from_value(serde_json::json!({"op":"symbol_context","symbol_id":"opaque"}))
                .unwrap(),
        )
        .unwrap();
        assert!(view.line_checkpoints.borrow().is_empty());
    }
    #[test]
    fn declaration_source_selects_exact_owner_origin_and_hash_before_limit() {
        let (path, generations) = declaration_fixture();
        let view = crate::query::fixture_view(&path).unwrap();
        let text = "r π\0😀";
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        for (comp, generation) in [("right", generations[1]), ("third", generations[2])] {
            let result = view
                .source_excerpt_scoped(
                    SourceRead {
                        artifact_id: "collision",
                        content_hash: Some(&hash),
                        start_byte: Some(0),
                        end_byte: Some(text.len() as u64),
                        budget: 100,
                        owner: Some((generation, comp)),
                    },
                    true,
                )
                .unwrap()
                .0;
            assert_eq!(result.path, format!("{comp}.ts"));
            assert_eq!(result.text, text);
            let context=view.query(serde_json::from_value(serde_json::json!({"op":"symbol_context","symbol_id":"opaque","compilation_id":comp})).unwrap()).unwrap();
            assert_eq!(
                context.contexts[0].declarations[0]
                    .source
                    .as_ref()
                    .unwrap()
                    .path,
                format!("{comp}.ts")
            );
            assert_eq!(
                context.contexts[0].declarations[0]
                    .source
                    .as_ref()
                    .unwrap()
                    .text,
                text
            );
        }
        // Neither another owner with equal bytes nor another hash at the same
        // artifact id may satisfy this concrete compiler origin.
        for (generation, comp) in [
            (generations[0], "left"),
            (generations[0], "right"),
            (generations[2], "right"),
        ] {
            assert!(matches!(
                view.source_excerpt_scoped(
                    SourceRead {
                        artifact_id: "collision",
                        content_hash: Some(&hash),
                        start_byte: Some(0),
                        end_byte: Some(1),
                        budget: 10,
                        owner: Some((generation, comp))
                    },
                    true
                ),
                Err(Error::InvalidRequest(_))
            ));
        }
    }
    #[test]
    fn source_budget_is_graph_only_and_declaration_projection_uses_overall_guards() {
        let (path, _) = declaration_fixture();
        let view = crate::query::fixture_view(&path).unwrap();
        for op in [
            "symbols",
            "source",
            "symbol_context",
            "references",
            "coverage",
        ] {
            let result = view.query(
                serde_json::from_value(
                    serde_json::json!({"op":op,"generation":view.generation,"source_max_bytes":1}),
                )
                .unwrap(),
            );
            assert!(
                matches!(result,Err(Error::InvalidRequest(message)) if message.contains("source_max_bytes"))
            );
        }
        for cap in [0, MAX_SOURCE_BYTES + 1] {
            assert!(matches!(view.query(serde_json::from_value(serde_json::json!({"op":"callees","symbol_id":"opaque","source_max_bytes":cap,"include_source":false})).unwrap()),Err(Error::InvalidRequest(_))));
        }
        let key = view
            .connection
            .query_row(
                "SELECT rowid FROM symbols WHERE compilation_id='third'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let symbol = crate::graph::symbol(&view.connection, key).unwrap();
        let request: QueryRequest = serde_json::from_str(r#"{"op":"symbol_context"}"#).unwrap();
        for cancelled in [false, true] {
            let cancellation = crate::execution::QueryCancellation::default();
            let execution = crate::execution::Execution::new(
                Default::default(),
                Some(if cancelled { 5000 } else { 0 }),
            )
            .unwrap()
            .with_cancellation(Some(cancellation.clone()));
            if cancelled {
                cancellation.cancel();
            }
            *view.execution.lock().unwrap() = Some(execution);
            let mut metadata = ProjectionBudget::new(&view, &request).unwrap();
            let mut source = 100;
            let result = view.project_declarations(
                &symbol,
                DeclarationOptions {
                    allowance: 16,
                    include_source: true,
                    duplicate: false,
                },
                &mut source,
                &mut metadata,
            );
            assert!(if cancelled {
                matches!(result, Err(Error::QueryCancelled))
            } else {
                matches!(result, Err(Error::DeadlineExceeded))
            });
            assert_eq!(source, 100);
        }
        *view.execution.lock().unwrap() = None;
        let mut metadata = ProjectionBudget {
            remaining: 1,
            execution: None,
        };
        let mut source = 100;
        assert!(matches!(
            view.project_declarations(
                &symbol,
                DeclarationOptions {
                    allowance: 16,
                    include_source: true,
                    duplicate: false
                },
                &mut source,
                &mut metadata
            ),
            Err(Error::ResponseLimitExceeded)
        ));
    }
}
