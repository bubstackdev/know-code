//! Shared bounded graph operators. Compiler workers supply evidence, never walks.
mod dispatch;
use crate::{
    Error, Result,
    query::{
        Coverage, DeclarationOptions, DeclarationProjection, Location, ProjectionBudget,
        QueryRequest, ReadView, SymbolResult, SymbolSupport,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Call,
    Reference,
    TypeReference,
    Inherits,
    Imports,
    Implements,
    Overrides,
    Contains,
    Initializes,
}
impl DependencyKind {
    pub fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "call" => Self::Call,
            "reference" => Self::Reference,
            "type_reference" => Self::TypeReference,
            "inherits" => Self::Inherits,
            "imports" => Self::Imports,
            "implements" => Self::Implements,
            "overrides" => Self::Overrides,
            "contains" => Self::Contains,
            "initializes" => Self::Initializes,
            _ => return Err(Error::Protocol(format!("unknown dependency kind {value}"))),
        })
    }
}

// Keep sparse provenance out of adjacency buffers. Inline Option<T> reserves
// the whole payload even when absent; JSON still contains the same evidence.
#[derive(Clone, Debug, Serialize)]
pub struct Edge {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_context_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_proof: Option<Box<DispatchProof>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_definition_link: Option<Box<DefinitionLink>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_compilation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_link: Option<Box<DefinitionLink>>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub dispatch_key: String,
    pub source_id: String,
    pub target_id: Option<String>,
    pub kind: DependencyKind,
    pub resolution: String,
    pub implicit: bool,
    pub dispatch: String,
    pub compilation_id: String,
    pub target: String,
    pub location: Location,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_evidence: Option<Box<Location>>,
}
#[derive(Clone, Debug, Serialize)]
pub struct DispatchProof {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub dispatch_key: String,
    pub source_id: String,
    pub target_id: String,
    pub kind: DependencyKind,
    pub compilation_id: String,
    pub location: Location,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_definition_link: Option<DefinitionLink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_definition_link: Option<DefinitionLink>,
}
#[derive(Clone, Debug, Serialize)]
pub struct DefinitionLink {
    pub reference_symbol_id: String,
    pub source_compilation_id: String,
    pub definition_symbol_id: String,
    pub definition_compilation_id: String,
    pub input_fingerprint: String,
}
#[derive(Debug, Serialize)]
pub struct ContextConflict {
    pub project_path: String,
    pub selected_compilation_id: String,
    pub required_compilation_id: String,
}
#[derive(Debug, Serialize)]
pub struct Finding {
    pub symbol: SymbolResult,
    pub paths: Vec<Vec<Edge>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<DeclarationProjection>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub supporting_targets: Vec<GraphSupport>,
}
#[derive(Debug, Serialize)]
pub struct GraphSupport {
    #[serde(flatten)]
    pub symbol: SymbolSupport,
    pub paths: Vec<Vec<Edge>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<DeclarationProjection>,
}
#[derive(Debug, Serialize)]
pub struct Boundary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_conflict: Option<ContextConflict>,
    pub reason: String,
    pub symbol_id: String,
    pub compilation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Edge>,
}
#[derive(Debug, Serialize)]
pub struct GraphResult {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub root_definitions: Vec<DefinitionLink>,
    pub findings: Vec<Finding>,
    pub boundaries: Vec<Boundary>,
    pub capability_coverage: BTreeMap<String, String>,
    pub limits: Vec<String>,
    pub examined_edges: u32,
    pub visited_nodes: u32,
}

// Serving adjacency canonicalizes exact definition pointers. Dispatch expansion
// requires the specialization/proof contract, including for legacy read views.
pub(crate) fn create_view(connection: &Connection, version: i32) -> Result<()> {
    connection.execute_batch("CREATE TEMP VIEW raw_graph_edges AS
      SELECT r.source_key,r.target_key,r.kind,r.resolution,r.artifact_key,r.start_byte,r.end_byte,r.implicit,r.dispatch,
        NULL AS binding_key,NULL AS proof_artifact,NULL AS proof_start,NULL AS proof_end,r.rowid AS ordinal,0 AS candidate
      FROM relationships r WHERE r.resolution<>'legacy_unknown'
;")?;
    if version >= 5 {
        connection.execute_batch("CREATE TEMP VIEW definition_links AS
          SELECT s.rowid AS source_key,t.rowid AS target_key,s.definition_fingerprint AS fingerprint
          FROM generation_units u JOIN symbols s ON s.generation=u.owner_generation AND s.compilation_id=u.compilation_id
          JOIN compilations sc ON sc.generation=s.generation AND sc.id=s.compilation_id
          JOIN reports sr ON sr.generation=sc.generation AND sr.compilation_id=sc.id
          JOIN generation_units d ON d.generation=u.generation AND d.compilation_id=s.definition_compilation_id
          JOIN compilations tc ON tc.generation=d.owner_generation AND tc.id=d.compilation_id
          JOIN reports tr ON tr.generation=tc.generation AND tr.compilation_id=tc.id
          JOIN symbols t ON t.generation=tc.generation AND t.compilation_id=tc.id AND t.id=s.definition_symbol_id
          WHERE u.generation=(SELECT current_generation FROM state) AND s.definition_compilation_id<>'' AND t.external=0
            AND s.definition_fingerprint=tc.input_fingerprint AND tc.input_fingerprint<>''
            AND json_extract(sr.capabilities_json,'$.project_links')='complete'
            AND json_extract(tr.capabilities_json,'$.project_links')='complete'
            AND EXISTS(SELECT 1 FROM json_each(sc.references_json) r WHERE json_extract(r.value,'$.compilation_id')=tc.id AND json_extract(r.value,'$.project_path')=tc.project_path);
          CREATE TEMP VIEW graph_edges AS
          SELECT r.source_key,r.target_key,r.kind,r.resolution,r.artifact_key,r.start_byte,r.end_byte,r.implicit,r.dispatch,
            NULL AS binding_key,NULL AS proof_artifact,NULL AS proof_start,NULL AS proof_end,r.rowid AS ordinal,0 AS candidate,
            NULL AS definition_reference_key,NULL AS definition_fingerprint FROM relationships r
          WHERE r.resolution<>'legacy_unknown' AND NOT EXISTS(SELECT 1 FROM definition_links d WHERE d.source_key=r.target_key)
          UNION ALL
          SELECT r.source_key,d.target_key,r.kind,r.resolution,r.artifact_key,r.start_byte,r.end_byte,r.implicit,r.dispatch,
            r.target_key,NULL,NULL,NULL,r.rowid,0,r.target_key,d.fingerprint
          FROM definition_links d JOIN relationships r ON r.target_key=d.source_key WHERE r.resolution<>'legacy_unknown'
;")?;
        if version >= 6 {
            dispatch::create_view(connection)?;
        }
    } else {
        connection.execute_batch("CREATE TEMP VIEW graph_edges AS SELECT e.*,NULL AS definition_reference_key,NULL AS definition_fingerprint FROM raw_graph_edges e")?;
    }
    Ok(())
}

struct Step {
    node: i64,
    parent: Option<usize>,
    edge: Option<Edge>,
    possible: bool,
    context: usize,
}
#[derive(Deserialize)]
struct SelectedContext {
    compilation_id: String,
    project_path: String,
    #[serde(default)]
    input_fingerprint: String,
}
struct Context {
    project: String,
    references: Vec<SelectedContext>,
    complete: bool,
    fingerprint: String,
}
type Bindings = Arc<BTreeMap<String, String>>;
struct Contexts {
    units: BTreeMap<String, Context>,
    sets: Vec<Bindings>,
    interned: BTreeMap<Bindings, usize>,
    consumers: BTreeMap<String, BTreeSet<String>>,
}
impl Contexts {
    fn load(view: &ReadView) -> Result<Self> {
        let mut units = BTreeMap::new();
        if view.format_version >= 5 {
            let mut stmt=view.connection.prepare("SELECT c.id,c.project_path,c.references_json,coalesce(json_extract(r.capabilities_json,'$.project_links'),'unknown'),c.input_fingerprint FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id LEFT JOIN reports r ON r.generation=c.generation AND r.compilation_id=c.id WHERE u.generation=?")?;
            let rows = stmt.query_map([view.generation], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            for row in rows {
                view.check()?;
                let (id, project, refs, status, fingerprint) = row?;
                units.insert(
                    id,
                    Context {
                        project,
                        references: serde_json::from_str(&refs)?,
                        complete: status == "complete",
                        fingerprint,
                    },
                );
            }
        }
        let mut consumers = BTreeMap::<String, BTreeSet<String>>::new();
        for (id, unit) in &units {
            view.check()?;
            if !unit.complete
                || unit.references.iter().any(|r| {
                    r.input_fingerprint.is_empty()
                        || units
                            .get(&r.compilation_id)
                            .is_none_or(|target| target.fingerprint != r.input_fingerprint)
                })
            {
                continue;
            }
            for selected in std::iter::once(id.as_str())
                .chain(unit.references.iter().map(|r| r.compilation_id.as_str()))
            {
                consumers
                    .entry(selected.into())
                    .or_default()
                    .insert(id.clone());
            }
        }
        let empty = Arc::new(BTreeMap::new());
        Ok(Self {
            units,
            sets: vec![empty.clone()],
            interned: BTreeMap::from([(empty, 0)]),
            consumers,
        })
    }
    fn witnesses(&self, caller: &str, implementation: &str) -> Vec<Option<String>> {
        if caller == implementation {
            return vec![None];
        }
        let Some(a) = self.consumers.get(caller) else {
            return vec![];
        };
        let Some(b) = self.consumers.get(implementation) else {
            return vec![];
        };
        a.intersection(b).map(|id| Some(id.clone())).collect()
    }
    fn merge(
        &mut self,
        index: usize,
        id: &str,
    ) -> std::result::Result<usize, Option<ContextConflict>> {
        let Some(unit) = self.units.get(id) else {
            return Err(None);
        };
        if !unit.complete {
            return Err(None);
        }
        let required = std::iter::once((&unit.project, id)).chain(
            unit.references
                .iter()
                .map(|r| (&r.project_path, r.compilation_id.as_str())),
        );
        if required
            .clone()
            .all(|(project, context)| self.sets[index].get(project).is_some_and(|v| v == context))
        {
            return Ok(index);
        }
        let mut selected = (*self.sets[index]).clone();
        for (project, required) in required {
            if let Some(previous) = selected.get(project) {
                if previous != required {
                    return Err(Some(ContextConflict {
                        project_path: project.clone(),
                        selected_compilation_id: previous.clone(),
                        required_compilation_id: required.into(),
                    }));
                }
            } else {
                selected.insert(project.clone(), required.into());
            }
        }
        let selected = Arc::new(selected);
        if let Some(index) = self.interned.get(&selected) {
            return Ok(*index);
        }
        let index = self.sets.len();
        self.sets.push(selected.clone());
        self.interned.insert(selected, index);
        Ok(index)
    }
}
fn path(steps: &[Step], mut index: usize) -> Vec<Edge> {
    let mut edges = vec![];
    loop {
        if let Some(edge) = &steps[index].edge {
            edges.push(edge.clone());
        }
        match steps[index].parent {
            Some(parent) => index = parent,
            None => break,
        }
    }
    edges.reverse();
    edges
}
fn contains(steps: &[Step], mut index: usize, node: i64) -> bool {
    loop {
        if steps[index].node == node {
            return true;
        }
        match steps[index].parent {
            Some(parent) => index = parent,
            None => return false,
        }
    }
}
pub(crate) fn symbol_full(connection: &Connection, key: i64) -> Result<SymbolResult> {
    symbol_projection(connection, key, false)
}
pub(crate) fn symbol(connection: &Connection, key: i64) -> Result<SymbolResult> {
    symbol_projection(connection, key, true)
}
fn symbol_projection(connection: &Connection, key: i64, bounded: bool) -> Result<SymbolResult> {
    let mut result=connection.query_row("SELECT s.id,s.name,s.kind,s.signature,s.external,s.compilation_id,c.target,s.generation FROM symbols s JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id WHERE s.rowid=?",[key],|r|Ok(SymbolResult{
        id:r.get(0)?,name:r.get(1)?,kind:r.get(2)?,signature:r.get(3)?,external:r.get(4)?,compilation_id:r.get(5)?,target:r.get(6)?,owner_generation:r.get(7)?,locations:vec![],test_classification:Default::default(),locations_limited:false,supporting_targets:vec![]
    }))?;
    let mut locations=connection.prepare_cached(if bounded {"SELECT artifact_id,path,content_hash,start_byte,end_byte,generated FROM occurrence_locations WHERE generation=? AND compilation_id=? AND symbol_id=? AND role='declaration' ORDER BY path,start_byte LIMIT 17"} else {"SELECT artifact_id,path,content_hash,start_byte,end_byte,generated FROM occurrence_locations WHERE generation=? AND compilation_id=? AND symbol_id=? AND role='declaration' ORDER BY path,start_byte"})?;
    result.locations = locations
        .query_map(
            params![result.owner_generation, result.compilation_id, result.id],
            read_location,
        )?
        .collect::<std::result::Result<_, _>>()?;
    result.test_classification = crate::testing::symbol(connection, key)?;
    result.locations_limited = bounded && result.locations.len() > 16;
    if bounded {
        result.locations.truncate(16);
    }
    Ok(result)
}
fn external_boundary(view: &ReadView, key: i64) -> Result<&'static str> {
    if view.format_version < 5 {
        return Ok("external_or_other_compilation_body");
    }
    let pointer: (String,String,String)=view.connection.query_row("SELECT definition_compilation_id,definition_symbol_id,definition_fingerprint FROM symbols WHERE rowid=?",[key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if pointer.0.is_empty() {
        return Ok("external_or_other_compilation_body");
    }
    let fingerprint=view.connection.query_row("SELECT c.input_fingerprint FROM generation_units u JOIN compilations c ON c.generation=u.owner_generation AND c.id=u.compilation_id WHERE u.generation=? AND c.id=?",params![view.generation,pointer.0],|r|r.get::<_,String>(0)).optional()?;
    Ok(match fingerprint {
        None => "definition_compilation_unavailable",
        Some(current) if current != pointer.2 => "definition_snapshot_mismatch",
        Some(_) => "definition_body_unavailable",
    })
}
fn read_location(row: &rusqlite::Row<'_>) -> rusqlite::Result<Location> {
    location(row, 0)
}
fn location(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Location> {
    Ok(Location {
        artifact_id: row.get(offset)?,
        path: row.get(offset + 1)?,
        content_hash: row.get(offset + 2)?,
        start_byte: row.get(offset + 3)?,
        end_byte: row.get(offset + 4)?,
        generated: row.get(offset + 5)?,
    })
}
fn bounded(value: Option<u32>, default: u32, maximum: u32, name: &str) -> Result<u32> {
    let value = value.unwrap_or(default);
    if value == 0 || value > maximum {
        return Err(Error::InvalidRequest(format!(
            "{name} must be between 1 and {maximum}"
        )));
    }
    Ok(value)
}
// Keep the bounded edge stream on the left of lookup joins. SQLite otherwise
// may materialize all neighbors into an automatic index before honoring LIMIT.
fn adjacency_sql(incoming: bool, virtual_calls: bool, keys: bool) -> String {
    let key = if keys { "rk.dispatch_key" } else { "''" };
    let key_join = if keys {
        " CROSS JOIN relationships rk ON rk.rowid=e.ordinal"
    } else {
        ""
    };
    let filter = if virtual_calls {
        " AND e.kind='call' AND e.resolution='compiler_bound' AND e.dispatch IN ('virtual','interface')"
    } else {
        ""
    };
    let column = if incoming {
        "e.target_key"
    } else {
        "e.source_key"
    };
    format!("SELECT e.source_key,e.target_key,s.id,t.id,e.kind,e.resolution,e.implicit,e.dispatch,s.compilation_id,c.target,
      a.id,a.path,a.content_hash,e.start_byte,e.end_byte,a.generated,b.id,
      p.id,p.path,p.content_hash,e.proof_start,e.proof_end,p.generated,t.compilation_id,dr.id,dr.compilation_id,e.definition_fingerprint,{key}
      FROM graph_edges e CROSS JOIN symbols s ON s.rowid=e.source_key LEFT JOIN symbols t ON t.rowid=e.target_key
      CROSS JOIN compilations c ON c.generation=s.generation AND c.id=s.compilation_id
      CROSS JOIN generation_units u ON u.owner_generation=s.generation AND u.compilation_id=s.compilation_id
      CROSS JOIN artifacts a ON a.rowid=e.artifact_key LEFT JOIN symbols b ON b.rowid=e.binding_key LEFT JOIN artifacts p ON p.rowid=e.proof_artifact LEFT JOIN symbols dr ON dr.rowid=e.definition_reference_key
      {key_join} WHERE u.generation=? AND {column}=?{filter} LIMIT ?")
}
struct Neighbor {
    source: i64,
    target: Option<i64>,
    edge: Edge,
}
fn read_neighbor(r: &rusqlite::Row<'_>) -> rusqlite::Result<Neighbor> {
    let kind = DependencyKind::parse(&r.get::<_, String>(4)?)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let resolution = r.get::<_, String>(5)?;
    let implicit = r.get(6)?;
    Ok(Neighbor {
        source: r.get(0)?,
        target: r.get(1)?,
        edge: Edge {
            dispatch_key: r.get(27)?,
            dispatch_context_id: None,
            dispatch_context_fingerprint: None,
            dispatch_proof: None,
            binding_definition_link: None,
            target_compilation_id: r.get(23)?,
            definition_link: if let Some(reference_symbol_id) = r.get::<_, Option<String>>(24)? {
                Some(Box::new(DefinitionLink {
                    reference_symbol_id,
                    source_compilation_id: r.get(25)?,
                    definition_symbol_id: r.get::<_, String>(3)?,
                    definition_compilation_id: r.get(23)?,
                    input_fingerprint: r.get(26)?,
                }))
            } else {
                None
            },
            source_id: r.get(2)?,
            target_id: r.get(3)?,
            kind,
            resolution: resolution.clone(),
            implicit,
            dispatch: r.get(7)?,
            compilation_id: r.get(8)?,
            target: r.get(9)?,
            location: location(r, 10)?,
            binding_target_id: r.get(16)?,
            dispatch_evidence: if r.get::<_, Option<String>>(17)?.is_some() {
                Some(Box::new(location(r, 17)?))
            } else {
                None
            },
        },
    })
}
pub(crate) fn query(
    view: &ReadView,
    request: &QueryRequest,
    coverage: &[Coverage],
) -> Result<GraphResult> {
    let id = request
        .symbol_id
        .as_ref()
        .ok_or_else(|| Error::InvalidRequest("graph queries require symbol_id".into()))?;
    let depth = bounded(
        request.max_depth,
        if request.op == "impact" { 5 } else { 1 },
        32,
        "max_depth",
    )?;
    let edge_budget = bounded(request.max_edges, 10000, 100000, "max_edges")?;
    let node_budget = bounded(request.max_nodes, 2000, 20000, "max_nodes")?;
    let paths = bounded(request.paths_per_symbol, 1, 8, "paths_per_symbol")?;
    let bytes = bounded(request.max_bytes, 1024 * 1024, 8 * 1024 * 1024, "max_bytes")? as usize;
    let duration =
        Duration::from_millis(
            bounded(request.time_budget_ms, 1000, 10000, "time_budget_ms")? as u64,
        );
    if request.kinds.as_ref().is_some_and(Vec::is_empty) {
        return Err(Error::InvalidRequest("kinds must not be empty".into()));
    }
    if request.op != "impact"
        && request
            .kinds
            .as_ref()
            .is_some_and(|kinds| kinds.iter().any(|k| *k != DependencyKind::Call))
    {
        return Err(Error::InvalidRequest(
            "callers/callees accept only the call dependency kind".into(),
        ));
    }
    let incoming = request.op != "callees";
    let capability = if request.op == "impact" {
        "dependencies"
    } else {
        "calls"
    };
    let mut result = GraphResult {
        root_definitions: vec![],
        findings: vec![],
        boundaries: vec![],
        capability_coverage: BTreeMap::new(),
        limits: vec![],
        examined_edges: 0,
        visited_nodes: 0,
    };
    for c in coverage {
        view.check()?;
        result.capability_coverage.insert(
            c.compilation_id.clone(),
            c.capabilities
                .get(capability)
                .cloned()
                .unwrap_or_else(|| "unknown".into()),
        );
    }
    let (roots, roots_limited) =
        crate::grouping::roots_bounded(view, request, Some(node_budget as usize))?;
    if roots_limited {
        result.limits.push("node_budget".into());
    }
    if view.format_version < 4 {
        result.boundaries.push(Boundary {
            context_conflict: None,
            reason: "capability_unknown_in_legacy_index".into(),
            symbol_id: id.clone(),
            compilation_id: String::new(),
            evidence: None,
        });
        return Ok(result);
    }
    let mut contexts = Contexts::load(view)?;
    let mut steps = vec![];
    let mut pending = BinaryHeap::new();
    for mut root in roots {
        view.check()?;
        let original_root = root;
        let s = symbol(&view.connection, root)?;
        let mut context = match contexts.merge(0, &s.compilation_id) {
            Ok(context) => context,
            Err(Some(conflict)) => {
                result.boundaries.push(Boundary {
                    reason: "incompatible_compilation_context".into(),
                    symbol_id: s.id.clone(),
                    compilation_id: s.compilation_id.clone(),
                    evidence: None,
                    context_conflict: Some(conflict),
                });
                continue;
            }
            Err(None) => 0,
        };
        if view.format_version >= 5 {
            let linked=view.connection.query_row("SELECT d.target_key,s.id,s.compilation_id,t.id,t.compilation_id,d.fingerprint FROM definition_links d JOIN symbols s ON s.rowid=d.source_key JOIN symbols t ON t.rowid=d.target_key WHERE d.source_key=?",[root],|r|Ok((r.get::<_,i64>(0)?,DefinitionLink{reference_symbol_id:r.get(1)?,source_compilation_id:r.get(2)?,definition_symbol_id:r.get(3)?,definition_compilation_id:r.get(4)?,input_fingerprint:r.get(5)?}))).optional()?;
            if let Some((key, link)) = linked {
                match contexts.merge(context, &link.definition_compilation_id) {
                    Ok(merged) => {
                        context = merged;
                        root = key;
                        result.root_definitions.push(link);
                    }
                    Err(conflict) => {
                        result.boundaries.push(Boundary {
                            context_conflict: conflict,
                            reason: "incompatible_compilation_context".into(),
                            symbol_id: s.id.clone(),
                            compilation_id: s.compilation_id.clone(),
                            evidence: None,
                        });
                        continue;
                    }
                }
            }
        }
        if s.external && root == original_root {
            result.boundaries.push(Boundary {
                reason: external_boundary(view, root)?.into(),
                symbol_id: s.id.clone(),
                compilation_id: s.compilation_id.clone(),
                evidence: None,
                context_conflict: None,
            });
        }
        if result
            .capability_coverage
            .get(&s.compilation_id)
            .is_none_or(|c| c == "unknown" || c == "unavailable")
        {
            result.boundaries.push(Boundary {
                context_conflict: None,
                reason: "capability_unavailable".into(),
                symbol_id: s.id.clone(),
                compilation_id: s.compilation_id.clone(),
                evidence: None,
            });
        }
        let index = steps.len();
        steps.push(Step {
            node: root,
            parent: None,
            edge: None,
            possible: false,
            context,
        });
        pending.push(Reverse((false, 0, index)));
    }
    let mut visits = BTreeMap::<(i64, bool, usize), u32>::new();
    let mut visited_nodes = BTreeSet::new();
    let mut findings = BTreeMap::<i64, usize>::new();
    let mut groups = BTreeMap::<String, usize>::new();
    let mut group_positions = vec![];
    let mut boundary_keys = BTreeSet::new();
    let mut size = 256usize;
    let started = Instant::now();
    let mut expand = true;
    while let Some(Reverse((possible, distance, index))) = pending.pop() {
        view.check()?;
        if expand && started.elapsed() >= duration {
            result.limits.push("time_budget".into());
            expand = false;
        }
        let node = steps[index].node;
        // Keep bounded and possible states separate: a shorter possible
        // route can reach more nodes within the depth budget even when the
        // projected path to this node prefers a longer bound route.
        let seen = visits
            .entry((node, possible, steps[index].context))
            .or_default();
        if *seen >= paths {
            continue;
        }
        if !visited_nodes.contains(&node) && result.visited_nodes == node_budget {
            result.limits.push("node_budget".into());
            break;
        }
        *seen += 1;
        if visited_nodes.insert(node) {
            result.visited_nodes += 1;
        }
        if steps[index].parent.is_some()
            && (request.tests == crate::testing::TestFilter::All
                || request
                    .tests
                    .matches(crate::testing::symbol_mask(&view.connection, node)?))
            && findings
                .get(&node)
                .is_none_or(|p| result.findings[*p].paths.len() < paths as usize)
        {
            let p = path(&steps, index);
            if let Some(position) = findings.get(&node) {
                let added = serde_json::to_vec(&p)?.len();
                if size + added > bytes {
                    result.limits.push("response_bytes".into());
                    break;
                }
                size += added;
                result.findings[*position].paths.push(p);
            } else {
                let key = crate::grouping::key(&view.connection, node)?;
                if !groups.contains_key(&key) && groups.len() >= request.limit as usize {
                    // The omitted finding does not stop other concrete peers
                    // from contributing to admitted groups or being traversed.
                    if !result.limits.iter().any(|s| s == "result_limit") {
                        result.limits.push("result_limit".into());
                    }
                } else {
                    let position = groups.len();
                    let group = *groups.entry(key).or_insert(position);
                    let finding = Finding {
                        symbol: symbol(&view.connection, node)?,
                        paths: vec![p],
                        context: None,
                        supporting_targets: vec![],
                    };
                    let boundaries = [
                        (
                            finding.symbol.external,
                            if finding.symbol.external {
                                external_boundary(view, node)?
                            } else {
                                ""
                            },
                        ),
                        (
                            finding.symbol.locations_limited,
                            "declaration_locations_limited",
                        ),
                    ]
                    .into_iter()
                    .filter(|(include, _)| *include)
                    .map(|(_, reason)| Boundary {
                        context_conflict: None,
                        reason: reason.into(),
                        symbol_id: finding.symbol.id.clone(),
                        compilation_id: finding.symbol.compilation_id.clone(),
                        evidence: None,
                    })
                    .collect::<Vec<_>>();
                    let added = serde_json::to_vec(&finding)?.len()
                        + boundaries
                            .iter()
                            .map(serde_json::to_vec)
                            .collect::<std::result::Result<Vec<_>, _>>()?
                            .iter()
                            .map(Vec::len)
                            .sum::<usize>();
                    if size + added > bytes {
                        result.limits.push("response_bytes".into());
                        break;
                    }
                    size += added;
                    result.boundaries.extend(boundaries);
                    findings.insert(node, result.findings.len());
                    result.findings.push(finding);
                    group_positions.push(group);
                }
            }
        }
        // Once search is limited, project already discovered paths without
        // fetching more adjacency. Do not discard valid frontier evidence.
        if !expand {
            continue;
        }
        let work = dispatch::Budget {
            edges: edge_budget,
            started,
            duration,
        };
        let neighbors = dispatch::neighbors(view, node, incoming, request, &work, &mut result)?;
        for Neighbor {
            source,
            target,
            edge,
        } in neighbors
        {
            if contexts.sets.len() > node_budget as usize {
                result.limits.push("context_budget".into());
                break;
            }
            let kind = edge.kind;
            let resolution = edge.resolution.clone();
            if request.op == "callees"
                && !request.exact_only
                && edge.kind == DependencyKind::Call
                && matches!(edge.dispatch.as_str(), "virtual" | "interface")
                && edge.dispatch_key.is_empty()
            {
                let boundary = Boundary {
                    reason: "dispatch_specialization_unproven".into(),
                    symbol_id: edge.target_id.clone().unwrap_or_default(),
                    compilation_id: edge.compilation_id.clone(),
                    evidence: Some(edge.clone()),
                    context_conflict: None,
                };
                let added = serde_json::to_vec(&boundary)?.len();
                if size + added > bytes {
                    result.limits.push("response_bytes".into());
                    break;
                }
                size += added;
                result.boundaries.push(boundary);
            }
            if target.is_none() {
                let key = (
                    edge.compilation_id.clone(),
                    edge.location.artifact_id.clone(),
                    edge.location.start_byte,
                    kind as u8,
                );
                if boundary_keys.insert(key) {
                    let boundary = Boundary {
                        context_conflict: None,
                        reason: "unresolved_target".into(),
                        symbol_id: edge.source_id.clone(),
                        compilation_id: edge.compilation_id.clone(),
                        evidence: Some(edge),
                    };
                    let added = serde_json::to_vec(&boundary)?.len();
                    if size + added > bytes {
                        result.limits.push("response_bytes".into());
                        break;
                    }
                    size += added;
                    result.boundaries.push(boundary);
                }
                continue;
            }
            let next = if incoming {
                source
            } else {
                target.expect("checked target")
            };
            if contains(&steps, index, next) {
                continue;
            }
            if distance >= depth {
                result.limits.push("depth_budget".into());
                continue;
            }
            if steps.len() >= edge_budget as usize + node_budget as usize {
                result.limits.push("state_budget".into());
                break;
            }
            let next_compilation = if incoming {
                &edge.compilation_id
            } else {
                edge.target_compilation_id
                    .as_ref()
                    .expect("target compilation")
            };
            let current_compilation = if incoming {
                edge.target_compilation_id
                    .as_ref()
                    .expect("target compilation")
            } else {
                &edge.compilation_id
            };
            let witnesses = if let Some(proof) = &edge.dispatch_proof {
                contexts.witnesses(&edge.compilation_id, &proof.compilation_id)
            } else {
                vec![None]
            };
            if witnesses.is_empty() {
                let boundary = Boundary {
                    reason: "dispatch_context_unproven".into(),
                    symbol_id: edge.target_id.clone().unwrap_or_default(),
                    compilation_id: edge.compilation_id.clone(),
                    evidence: Some(edge),
                    context_conflict: None,
                };
                let added = serde_json::to_vec(&boundary)?.len();
                if size + added > bytes {
                    result.limits.push("response_bytes".into());
                    break;
                }
                size += added;
                result.boundaries.push(boundary);
                continue;
            }
            for witness in witnesses {
                let mut context = steps[index].context;
                let mut required = vec![];
                if next_compilation != current_compilation {
                    required.push(next_compilation.clone());
                }
                if let Some(proof) = &edge.dispatch_proof {
                    required.push(proof.compilation_id.clone());
                    for link in [
                        edge.binding_definition_link.as_deref(),
                        proof.source_definition_link.as_ref(),
                        proof.target_definition_link.as_ref(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        required.push(link.definition_compilation_id.clone());
                    }
                    if let Some(id) = &witness {
                        required.push(id.clone());
                    }
                }
                let mut conflict = None;
                for id in required {
                    if &id == current_compilation {
                        continue;
                    }
                    match contexts.merge(context, &id) {
                        Ok(merged) => context = merged,
                        Err(reason) => {
                            conflict = Some(reason);
                            break;
                        }
                    }
                }
                if let Some(conflict) = conflict {
                    let boundary = Boundary {
                        reason: if conflict.is_some() {
                            "incompatible_compilation_context"
                        } else {
                            "project_context_unavailable"
                        }
                        .into(),
                        symbol_id: if incoming {
                            edge.source_id.clone()
                        } else {
                            edge.target_id.clone().unwrap()
                        },
                        compilation_id: next_compilation.clone(),
                        evidence: Some(edge.clone()),
                        context_conflict: conflict,
                    };
                    let added = serde_json::to_vec(&boundary)?.len();
                    if size + added > bytes {
                        result.limits.push("response_bytes".into());
                        break;
                    }
                    size += added;
                    result.boundaries.push(boundary);
                    continue;
                }
                if contexts.sets.len() > node_budget as usize {
                    result.limits.push("context_budget".into());
                    break;
                }
                if steps.len() >= edge_budget as usize + node_budget as usize {
                    result.limits.push("state_budget".into());
                    break;
                }
                let mut edge = edge.clone();
                edge.dispatch_context_fingerprint = witness
                    .as_ref()
                    .map(|id| contexts.units[id].fingerprint.clone());
                edge.dispatch_context_id = witness;
                let candidate = steps.len();
                let possible = possible || resolution == "possible_dispatch";
                steps.push(Step {
                    node: next,
                    parent: Some(index),
                    edge: Some(edge),
                    possible,
                    context,
                });
                pending.push(Reverse((
                    steps[candidate].possible,
                    distance + 1,
                    candidate,
                )));
            }
        }
        if result
            .limits
            .iter()
            .any(|s| !matches!(s.as_str(), "depth_budget" | "result_limit"))
        {
            expand = false;
        }
    }
    let mut projected = (0..groups.len()).map(|_| Vec::new()).collect::<Vec<_>>();
    for (finding, position) in std::mem::take(&mut result.findings)
        .into_iter()
        .zip(group_positions)
    {
        projected[position].push(finding);
    }
    let include_source = request.include_source == Some(true);
    let mut source_budget = request
        .source_max_bytes
        .unwrap_or(crate::query::DEFAULT_SOURCE_BYTES);
    let mut projection = include_source
        .then(|| ProjectionBudget::new(view, request))
        .transpose()?;
    let mut projection_bytes = 256usize;
    for boundary in &result.boundaries {
        projection_bytes = projection_bytes.saturating_add(serde_json::to_vec(boundary)?.len());
    }
    for mut members in projected {
        view.check()?;
        if members.is_empty() {
            continue;
        }
        members.sort_by(|a, b| {
            (&a.symbol.target, &a.symbol.compilation_id, &a.symbol.id).cmp(&(
                &b.symbol.target,
                &b.symbol.compilation_id,
                &b.symbol.id,
            ))
        });
        if let Some(projection) = &projection {
            let multiple = members.len() > 1;
            let mut metadata = projection.fork();
            for (index, member) in members.iter_mut().enumerate() {
                let (context, limits) = view.project_declarations(
                    &member.symbol,
                    DeclarationOptions {
                        allowance: 16,
                        include_source: true,
                        duplicate: multiple && index == 0,
                    },
                    &mut source_budget,
                    &mut metadata,
                )?;
                if limits.declarations {
                    result.limits.push("declaration_limit".into());
                }
                if limits.source {
                    result.limits.push("source_budget".into());
                }
                member.context = Some(context);
            }
        }
        if members.len() == 1 {
            let finding = members.pop().expect("one member");
            projection_bytes = projection_bytes.saturating_add(serde_json::to_vec(&finding)?.len());
            if projection_bytes > bytes {
                result.limits.push("response_bytes".into());
                break;
            }
            if let Some(projection) = &mut projection {
                projection.charge(&finding)?;
            }
            result.findings.push(finding);
            continue;
        }
        let first = &members[0];
        let finding = Finding {
            symbol: first.symbol.clone(),
            paths: first.paths.clone(),
            context: first.context.clone(),
            supporting_targets: members
                .into_iter()
                .map(|m| GraphSupport {
                    symbol: SymbolSupport::from(&m.symbol),
                    paths: m.paths,
                    context: m.context,
                })
                .collect(),
        };
        projection_bytes = projection_bytes.saturating_add(serde_json::to_vec(&finding)?.len());
        if projection_bytes > bytes {
            result.limits.push("response_bytes".into());
            break;
        }
        if let Some(projection) = &mut projection {
            projection.charge(&finding)?;
        }
        result.findings.push(finding);
    }
    result.limits.sort();
    result.limits.dedup();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adjacency_never_scans_or_materializes_the_whole_relationship_store() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/graph-plans")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("index.db");
        use crate::protocol::{AnalysisReport, Compilation, envelope::Payload};
        let mut writer = crate::store::Writer::open(&path).unwrap();
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
        let view = crate::query::fixture_view(path).unwrap();
        let c = &view.connection;
        for incoming in [true, false] {
            for query in [
                adjacency_sql(incoming, false, true),
                dispatch::proof_sql(incoming),
            ] {
                // Parameter changes must not repeatedly replan these nested views.
                let mut adjacency = c.prepare(&query).unwrap();
                for parameters in [
                    [view.generation as i64, 0, 1],
                    [0, 1, 10001],
                    [1, i64::MAX, 100001],
                ] {
                    let mut rows = adjacency.query(parameters).unwrap();
                    while rows.next().unwrap().is_some() {}
                }
                assert_eq!(
                    adjacency.get_status(rusqlite::StatementStatus::RePrepare),
                    0
                );
                let sql = format!("EXPLAIN QUERY PLAN {query}");
                let mut stmt = c.prepare(&sql).unwrap();
                for detail in stmt
                    .query_map([0, 1, 10001], |r| r.get::<_, String>(3))
                    .unwrap()
                {
                    let detail = detail.unwrap();
                    assert!(
                        !matches!(detail.as_str(), "SCAN r" | "SCAN i" | "SCAN c")
                            && !detail.contains("AUTOMATIC")
                            && !detail.contains("TEMP B-TREE"),
                        "{detail}"
                    );
                }
            }
        }
    }
    fn declaration_graph(count: usize, extents: u8) -> std::path::PathBuf {
        use crate::protocol::{
            AnalysisReport, Compilation, FactBatch, Occurrence, Relationship, SourceArtifact,
            Symbol, envelope::Payload,
        };
        use sha2::{Digest, Sha256};
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/graph-source-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("index.db");
        let mut text = String::from("// π\0\r\nroot\n");
        let root = text.find("root").unwrap() as u64;
        let mut anchors = vec![];
        for _ in 0..count {
            let start = text.len() as u64;
            text.push_str("😀fπ\n");
            anchors.push((start + 4, start, start + 7));
        }
        let g = text.len() as u64;
        text.push_str("g\n");
        let mut writer = crate::store::Writer::open(&path).unwrap();
        for (comp, target) in [("a", "target2"), ("b", "target1"), ("c", "target0")] {
            writer
                .stage(Payload::Compilation(Compilation {
                    id: comp.into(),
                    language: "csharp".into(),
                    target: target.into(),
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
                    utf8: text.as_bytes().into(),
                    sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
                    ..Default::default()
                }))
                .unwrap();
            let symbols = ["root", "f", "g"]
                .into_iter()
                .map(|name| Symbol {
                    id: format!("{comp}:{name}"),
                    name: name.into(),
                    kind: "method".into(),
                    signature: format!("void {name}()"),
                    declaration_key: if comp == "c" && name == "root" {
                        "untraversed-root".into()
                    } else {
                        format!("decl:{name}")
                    },
                    ..Default::default()
                })
                .collect();
            let mut occurrences = vec![];
            for (name, start, end) in [("root", root, root + 4), ("g", g, g + 1)] {
                for role in ["declaration", "definition"] {
                    occurrences.push(Occurrence {
                        symbol_id: format!("{comp}:{name}"),
                        artifact_id: "body".into(),
                        start_byte: start,
                        end_byte: end,
                        role: role.into(),
                        ..Default::default()
                    });
                }
            }
            for (anchor, start, end) in &anchors {
                occurrences.push(Occurrence {
                    symbol_id: format!("{comp}:f"),
                    artifact_id: "body".into(),
                    start_byte: *anchor,
                    end_byte: anchor + 1,
                    role: "declaration".into(),
                    ..Default::default()
                });
                for branch in 0..extents {
                    occurrences.push(Occurrence {
                        symbol_id: format!("{comp}:f"),
                        artifact_id: "body".into(),
                        start_byte: *start,
                        end_byte: end + u64::from(branch),
                        role: "definition".into(),
                        ..Default::default()
                    });
                }
            }
            let relationships = ["f", "g"]
                .into_iter()
                .map(|to| Relationship {
                    source_id: format!("{comp}:root"),
                    target_id: format!("{comp}:{to}"),
                    kind: "call".into(),
                    resolution: "compiler_bound".into(),
                    artifact_id: "body".into(),
                    start_byte: root,
                    end_byte: root + 1,
                    dispatch: "direct".into(),
                    ..Default::default()
                })
                .collect();
            writer
                .stage(Payload::Facts(FactBatch {
                    compilation_id: comp.into(),
                    symbols,
                    occurrences,
                    relationships,
                }))
                .unwrap();
            writer
                .stage(Payload::Report(AnalysisReport {
                    compilation_id: comp.into(),
                    status: "complete".into(),
                    capabilities: [
                        ("calls".into(), "complete".into()),
                        ("dependencies".into(), "complete".into()),
                        ("target_equivalence".into(), "complete".into()),
                    ]
                    .into(),
                    ..Default::default()
                }))
                .unwrap();
        }
        writer.publish().unwrap();
        path
    }
    fn projected(view: &ReadView, extra: serde_json::Value) -> crate::query::QueryResult {
        let mut request = serde_json::json!({"op":"callees","symbol_id":"a:root"});
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        view.query(serde_json::from_value(request).unwrap())
            .unwrap()
    }
    fn compact(mut graph: serde_json::Value) -> serde_json::Value {
        graph.as_object_mut().unwrap().remove("limits");
        for finding in graph["findings"].as_array_mut().unwrap() {
            finding.as_object_mut().unwrap().remove("context");
            if let Some(supports) = finding
                .get_mut("supporting_targets")
                .and_then(|v| v.as_array_mut())
            {
                for support in supports {
                    support.as_object_mut().unwrap().remove("context");
                }
            }
        }
        graph
    }
    #[test]
    fn graph_source_is_opt_in_and_default_executes_no_source_sql() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let path = declaration_graph(1, 1);
        let mut view = crate::query::fixture_view(path).unwrap();
        view.connection
            .authorizer(Some(|context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Read {
                        table_name: "contents",
                        ..
                    }
                ) {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }))
            .unwrap();
        let omitted = projected(&view, serde_json::json!({}));
        let explicit = projected(&view, serde_json::json!({"include_source":false}));
        let omitted = serde_json::to_value(omitted.graph.unwrap()).unwrap();
        assert_eq!(
            omitted,
            serde_json::to_value(explicit.graph.unwrap()).unwrap()
        );
        assert!(
            omitted["findings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|f| f.get("context").is_none())
        );
        let request = serde_json::from_value(
            serde_json::json!({"op":"callees","symbol_id":"a:root","include_source":true}),
        )
        .unwrap();
        assert!(
            matches!(view.query(request), Err(Error::Sqlite(_))),
            "storage failures must not become body-unavailable labels"
        );
    }
    #[test]
    fn graph_source_budget_counts_all_wire_copies_and_preserves_findings_paths_and_targets() {
        let path = declaration_graph(1, 1);
        let view = crate::query::fixture_view(path).unwrap();
        let baseline =
            serde_json::to_value(projected(&view, serde_json::json!({})).graph.unwrap()).unwrap();
        for budget in 1..=24 {
            let result = projected(
                &view,
                serde_json::json!({"include_source":true,"source_max_bytes":budget}),
            );
            let graph = serde_json::to_value(result.graph.as_ref().unwrap()).unwrap();
            assert_eq!(compact(graph.clone()), compact(baseline.clone()));
            assert_eq!(result.truncated, budget < 24);
            let limited = graph["limits"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "source_budget");
            assert_eq!(limited, budget < 24);
            let mut bytes = 0;
            for finding in graph["findings"].as_array().unwrap() {
                let supports = finding["supporting_targets"].as_array().unwrap();
                assert_eq!(supports.len(), 2);
                assert_eq!(
                    supports
                        .iter()
                        .map(|s| s["compilation_id"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    ["b", "a"]
                );
                assert!(
                    !finding["symbol"]["locations"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                for member in std::iter::once(finding).chain(supports) {
                    let context = &member["context"];
                    assert_eq!(context["declarations"].as_array().unwrap().len(), 1);
                    for declaration in context["declarations"].as_array().unwrap() {
                        if let Some(source) = declaration.get("source") {
                            let text = source["text"].as_str().unwrap();
                            bytes += text.len();
                            let body = if finding["symbol"]["name"] == "f" {
                                "😀fπ"
                            } else {
                                "g"
                            };
                            assert!(body.starts_with(text));
                            assert!(body.is_char_boundary(text.len()));
                            assert_eq!(declaration["line"], if body == "g" { 4 } else { 3 });
                        }
                    }
                }
            }
            assert!(bytes <= budget, "budget={budget},wire={bytes}");
        }
        let full = projected(&view, serde_json::json!({"include_source":true}))
            .graph
            .unwrap();
        assert!(full.root_definitions.is_empty());
        assert!(
            full.findings
                .iter()
                .flat_map(|f| &f.paths)
                .flatten()
                .all(|edge| serde_json::to_value(edge).unwrap().get("context").is_none())
        );
    }
    #[test]
    fn graph_declaration_limit_is_independent_of_finding_and_source_budgets() {
        let path = declaration_graph(17, 1);
        let view = crate::query::fixture_view(path).unwrap();
        let result = projected(&view, serde_json::json!({"include_source":true,"limit":1}));
        let graph = result.graph.unwrap();
        assert_eq!(graph.findings.len(), 1);
        assert_eq!(graph.findings[0].symbol.name, "f");
        assert!(graph.limits.iter().any(|s| s == "declaration_limit"));
        assert!(!graph.limits.iter().any(|s| s == "source_budget"));
        assert_eq!(
            graph.findings[0]
                .context
                .as_ref()
                .unwrap()
                .declarations
                .len(),
            16
        );
        assert!(
            graph.findings[0].supporting_targets.iter().all(|s| s
                .context
                .as_ref()
                .unwrap()
                .declarations
                .len()
                == 16)
        );
    }
    #[test]
    fn graph_extent_labels_and_graph_and_complete_response_caps_remain_explicit() {
        for (extents, label) in [(0, "extent_unavailable"), (2, "extent_ambiguous")] {
            let path = declaration_graph(1, extents);
            let view = crate::query::fixture_view(path).unwrap();
            let result = projected(&view, serde_json::json!({"include_source":true}));
            let graph = result.graph.unwrap();
            let f = graph
                .findings
                .iter()
                .find(|f| f.symbol.name == "f")
                .unwrap();
            assert_eq!(
                f.context.as_ref().unwrap().declarations[0].source_status,
                label
            );
            assert!(f.context.as_ref().unwrap().declarations[0].source.is_none());
            assert!(!graph.limits.iter().any(|s| s == "source_budget"));
        }
        let path = declaration_graph(1, 1);
        let view = crate::query::fixture_view(path).unwrap();
        let baseline = projected(&view, serde_json::json!({}));
        let total = serde_json::to_vec(&baseline).unwrap().len();
        let graph_cap = serde_json::to_vec(baseline.graph.as_ref().unwrap())
            .unwrap()
            .len()
            + 512;
        let compact = projected(&view, serde_json::json!({"max_bytes":graph_cap}));
        assert!(
            !compact
                .graph
                .unwrap()
                .limits
                .iter()
                .any(|s| s == "response_bytes")
        );
        let enriched = projected(
            &view,
            serde_json::json!({"max_bytes":graph_cap,"include_source":true}),
        );
        assert!(
            enriched
                .graph
                .unwrap()
                .limits
                .iter()
                .any(|s| s == "response_bytes")
        );
        let request=serde_json::from_value(serde_json::json!({"op":"callees","symbol_id":"a:root","include_source":true,"response_max_bytes":total+64})).unwrap();
        assert!(matches!(
            view.query(request),
            Err(Error::ResponseLimitExceeded)
        ));
    }
}
