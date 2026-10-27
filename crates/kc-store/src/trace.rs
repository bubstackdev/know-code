//! Call and relation trees' parts: a repo's edges, what a symbol reaches (out) and what reaches it (in) through an
//! [`EdgeCache`], the trees and impact rows they make, edge filters and the frontier past a walk's stops. The walks
//! themselves run on the symbol graph (`knowcode::tree`, `knowcode::walk`).
//!
//! Edges come from query-time resolution, so each carries the reference's confidence. Only `exact` and
//! `unique_name` edges are expanded further; `ambiguous` ones are listed as leaves so the tree never invents a path.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::answer::{Assumption, Evidence, EvidencePolicy, PolicyReport, Verdict};
use crate::query::{self, REF_COLS, RefHit, SYMBOL_COLS, SymbolRow, is_type_or_extension, ref_row, resolve_all, symbol_row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
    Both,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceNode {
    pub id: i64,
    pub qualified: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub depth: u8,
    /// Index of the node this one hangs off; `None` for the root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// `in` or `out`, relative to the root.
    pub direction: &'static str,
    /// `calls`, `instantiates`, `uses`, `reads` and `writes` (SQL: a statement reading or writing a table),
    /// `renders` (creates the view whose `body` this is), `conforms_to`,
    /// `conformer`, `witness`, `via_protocol`, `via_di` (a call through a service type, to the implementation the
    /// container binds), `http` (requests the action across an HTTP route), `bus` (consumes a message the node it hangs
    /// off is or publishes), `sql` (code using the SQL table or procedure it hangs off: an Entity Framework entity's
    /// accessors, or a stored procedure call).
    #[serde(skip_serializing_if = "str::is_empty")]
    pub edge: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub confidence: &'static str,
    /// Where the edge happens (the reference site), when it differs from the node's own location: for a `bus` hop
    /// from a publisher, the publish call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// The repo `site` is in, when it isn't the node's: a bus hop's publish, in the publisher's repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_repo: Option<String>,
    /// For a `via_di` edge: where the binding is registered, and its lifetime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    /// The repo the node is in, when it isn't the traced repo's: reached across an HTTP route or the message bus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// For an `http` edge, the route crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpVia>,
    /// For a `bus` edge, the message crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus: Option<BusVia>,
    /// For a `sql` edge, the SQL object crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<SqlVia>,
    /// For a `role` edge, the Ansible role crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleVia>,
    /// For a `deploy` edge, the deploy chain crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy: Option<DeployVia>,
    /// For a `module` edge, the Terraform module call crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<ModuleVia>,
    /// For an edge into a repo using a library, the library crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library: Option<LibraryVia>,
    /// The edge's reference was resolved from a SCIP index (see [`crate::scip`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub precise: bool,
    /// What the edge reaching the node rests on (see [`Evidence`]); none on the root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// The lines the node rests on: the edge's site, else (the root) its declaration (see [`crate::answer::Cite`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cite: Option<crate::answer::Cite>,
}

/// An Ansible role crossed from the repo defining it to a play, task or role using it in another repo.
#[derive(Debug, Clone, Serialize)]
pub struct RoleVia {
    /// The role's name as the user writes it.
    pub role: String,
    /// How sure the link from the name to the role's repo is: `exact` (its requirement's `src` is the repo),
    /// `unique_name` or `ambiguous`.
    pub link: &'static str,
}

/// A Terraform module crossed from the repo holding it to a module call in another repo (see `kc_index::terraform`).
#[derive(Debug, Clone, Serialize)]
pub struct ModuleVia {
    /// The calling block's address (`module.spa`).
    pub module: String,
    /// Its `source`, as written.
    pub source: String,
    /// How sure the link from the source to the repo is: `exact` (a git URL that is the repo's origin), `unique_name`
    /// or `ambiguous`.
    pub link: &'static str,
    /// What the link did not check (see [`Assumption`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
}

/// A library crossed from the repo defining a symbol to a repo using it: a call of the symbol there (resolved into the
/// library), or, for an implementation of a dependency's interface member, a call through that member in the
/// dependency or a repo using it.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryVia {
    /// The repo whose code the call resolves into: the symbol's own, or the dependency declaring the member called
    /// through.
    pub library: String,
    /// For a call through a dependency's interface or base class member, its type (`ILogger`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
    /// The edge as within a repo: `calls`, `instantiates`, `uses`, or `via_protocol` through `through`.
    #[serde(skip)]
    pub edge: &'static str,
    /// How sure the link is: `exact` for a call resolved to the symbol; `ambiguous` through a member another
    /// implementation may answer.
    pub link: &'static str,
}

/// A deploy chain crossed from a shared CI template, an Ansible playbook or a role to an application's CI job
/// deploying with it (see `kc_index::ci`).
#[derive(Debug, Clone, Serialize)]
pub struct DeployVia {
    /// The playbook run, `repo/file`.
    pub playbook: String,
    /// The application's name as the pipeline passes it, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The chain's weakest link: `exact`, `unique_name` or `ambiguous`.
    pub link: &'static str,
    /// What the link did not check (see [`Assumption`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
}

/// An HTTP route crossed from a controller action to the code that requests it: the node creates `contract`,
/// which the client sends as `verb path`, and the action serves it.
#[derive(Debug, Clone, Serialize)]
pub struct HttpVia {
    pub verb: String,
    pub path: Option<String>,
    /// The request contract (request DTO) the node creates, or with `call`, the node itself.
    pub contract: String,
    /// The node requests the route itself, naming the service and path or calling an HTTP client with a URL.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub call: bool,
    /// How sure the link from the contract to the action is: `exact`, `dominant` or `ambiguous`.
    pub link: &'static str,
    /// What the link did not check (see [`Assumption`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
}

impl HttpVia {
    /// `GET /path`, or the verb alone when the path isn't known.
    pub fn route(&self) -> String {
        match &self.path {
            Some(p) => format!("{} /{p}", self.verb),
            None => self.verb.clone(),
        }
    }
}

/// A message-bus hop from a message type, or code publishing it, to a consumer handling it.
#[derive(Debug, Clone, Serialize)]
pub struct BusVia {
    /// The message type: its full name when it resolves, else as written.
    pub message: String,
    /// The virtual host or queue, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// How sure the link is: `exact` (both sides name the same declared type) or `ambiguous` (the bare name only).
    pub link: &'static str,
    /// What the link did not check (see [`Assumption`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
}

/// A hop from a SQL table, view or routine to the code using it: through an Entity Framework entity mapped to the
/// table, or a stored procedure call.
#[derive(Debug, Clone, Serialize)]
pub struct SqlVia {
    /// The SQL object: `schema.name`.
    pub object: String,
    /// `entity` (the code reads or writes an entity mapped to the table) or `call` (it runs the procedure).
    pub how: &'static str,
    /// For `entity`, the entity class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// For `entity`, `read`, `write` or `access`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// How sure the link from the code's name to the SQL definition is: `exact`, `unique_name`, `dominant` or
    /// `ambiguous`.
    pub link: &'static str,
    /// What the link did not check (see [`Assumption`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
}

/// A hop across repos: an HTTP route or a message on the bus; or from SQL to the code using it.
#[derive(Debug, Clone)]
pub enum Via {
    Http(HttpVia),
    Bus(BusVia),
    Sql(SqlVia),
    Role(RoleVia),
    Deploy(DeployVia),
    Module(ModuleVia),
    Library(LibraryVia),
}

impl Via {
    /// The edge kind: `http`, `bus` or `sql`.
    pub fn edge(&self) -> &'static str {
        match self {
            Via::Http(_) => "http",
            Via::Bus(_) => "bus",
            Via::Sql(_) => "sql",
            Via::Role(_) => "role",
            Via::Deploy(_) => "deploy",
            Via::Module(_) => "module",
            Via::Library(l) => l.edge,
        }
    }

    /// The link's confidence.
    pub fn link(&self) -> &'static str {
        match self {
            Via::Http(h) => h.link,
            Via::Bus(b) => b.link,
            Via::Sql(q) => q.link,
            Via::Role(r) => r.link,
            Via::Deploy(d) => d.link,
            Via::Module(m) => m.link,
            Via::Library(l) => l.link,
        }
    }

    /// The HTTP route, for an `http` hop.
    pub fn http(&self) -> Option<HttpVia> {
        if let Via::Http(h) = self { Some(h.clone()) } else { None }
    }

    /// The message, for a `bus` hop.
    pub fn bus(&self) -> Option<BusVia> {
        if let Via::Bus(b) = self { Some(b.clone()) } else { None }
    }

    /// The SQL object, for a `sql` hop.
    pub fn sql(&self) -> Option<SqlVia> {
        if let Via::Sql(q) = self { Some(q.clone()) } else { None }
    }

    /// The Ansible role, for a `role` hop.
    pub fn role(&self) -> Option<RoleVia> {
        if let Via::Role(r) = self { Some(r.clone()) } else { None }
    }

    /// The deploy chain, for a `deploy` hop.
    pub fn deploy(&self) -> Option<DeployVia> {
        if let Via::Deploy(d) = self { Some(d.clone()) } else { None }
    }

    /// The Terraform module call, for a `module` hop.
    pub fn module(&self) -> Option<ModuleVia> {
        if let Via::Module(m) = self { Some(m.clone()) } else { None }
    }

    /// The library crossed, for a `library` hop.
    pub fn library(&self) -> Option<LibraryVia> {
        if let Via::Library(l) = self { Some(l.clone()) } else { None }
    }

    /// What the link did not check.
    pub fn assumptions(&self) -> &[Assumption] {
        match self {
            Via::Http(h) => &h.assumptions,
            Via::Bus(b) => &b.assumptions,
            Via::Sql(q) => &q.assumptions,
            Via::Role(_) => &[],
            Via::Deploy(d) => &d.assumptions,
            Via::Module(m) => &m.assumptions,
            Via::Library(_) => &[],
        }
    }

    /// The evidence of a hop across this link whose own end was found at `confidence` (see [`Evidence::of_edge`]),
    /// with what the link assumed.
    pub fn evidence(&self, confidence: &str) -> Option<Evidence> {
        Evidence::of_edge(self.edge(), confidence, None, Some(self.link()), false).map(|e| e.assuming_all(self.assumptions()))
    }
}

/// A neighbour: the symbol, the edge kind, the edge's confidence, and the reference site.
pub type Edge = (SymbolRow, &'static str, &'static str, Option<String>);

/// Every edge kind a walk can report (`trace`, `impact`, `path`, `flow`).
pub const EDGE_KINDS: &[&str] = &[
    "calls", "instantiates", "uses", "reads", "writes", "renders", "conforms_to", "conformer", "witness", "via_protocol",
    "via_di", "dispatch", "member", "http", "bus", "sql", "ngrx", "includes", "notifies", "templates", "runs", "role",
    "deploy", "depends_on", "provider", "sets", "module", "child", "dialog", "constructs",
];

/// Which edges a walk lists and follows. Empty allows every edge. `calls` also allows the calls that reach their
/// target indirectly (`via_di`, `via_protocol`, `dispatch`); the reference kinds `refs` takes name their edges
/// (`call`, `instantiate`, `type_ref`, `read`, `write`).
///
/// It carries the walk's evidence policy too ([`EvidencePolicy`]), and counts the edges the policy excluded: clones
/// share the count, so one answer's walks (a trace continued across repos, an impact's crossings) add up.
#[derive(Debug, Clone, Default)]
pub struct EdgeFilter {
    edges: Vec<&'static str>,
    policy: EvidencePolicy,
    excluded: Arc<Mutex<Excluded>>,
}

/// The edges a policy excluded: each once, however many times a walk met it.
#[derive(Debug, Default)]
struct Excluded {
    seen: HashSet<u64>,
    by: std::collections::BTreeMap<String, usize>,
}

impl EdgeFilter {
    /// A filter from edge names, comma-separated or not (`["calls,http"]`, `["calls", "http"]`).
    pub fn new(names: &[String]) -> Result<EdgeFilter> {
        let mut edges = Vec::new();
        for name in names.iter().flat_map(|n| n.split(',')).map(str::trim).filter(|n| !n.is_empty()) {
            let one = match name {
                "call" | "calls" => &["calls", "via_di", "via_protocol", "dispatch"][..],
                "instantiate" => &["instantiates", "constructs"][..],
                "type_ref" => &["uses"][..],
                "read" => &["reads"][..],
                "write" => &["writes"][..],
                // Ansible reference kinds.
                "include" => &["includes"][..],
                "notify" => &["notifies"][..],
                "var" => &["uses"][..],
                "template" => &["templates"][..],
                // Terraform reference kinds.
                "tf_ref" | "tf_output" => &["uses"][..],
                "tf_input" => &["sets"][..],
                other => match EDGE_KINDS.iter().find(|k| **k == other) {
                    Some(k) => std::slice::from_ref(k),
                    None => anyhow::bail!("unknown edge kind {other}; one of {}", EDGE_KINDS.join(", ")),
                },
            };
            edges.extend_from_slice(one);
        }
        Ok(EdgeFilter { edges, ..Default::default() })
    }

    /// The filter, walking only the edges `policy` allows.
    pub fn with_policy(mut self, policy: EvidencePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy(&self) -> &EvidencePolicy {
        &self.policy
    }

    /// Whether the walk lists and follows an edge of this kind.
    pub fn allows(&self, edge: &str) -> bool {
        self.edges.is_empty() || self.edges.contains(&edge)
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// What the walk does with an edge of `confidence` resting on `evidence` (see [`EvidencePolicy::judge`]). An
    /// excluded edge is counted, once per `key` (what tells the edge apart: its ends and site).
    pub fn judge(&self, key: impl Hash, confidence: &str, evidence: Option<&Evidence>) -> Verdict {
        let v = self.policy.judge(confidence, evidence);
        if v == Verdict::Exclude {
            let mut h = std::hash::DefaultHasher::new();
            key.hash(&mut h);
            let mut x = self.excluded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if x.seen.insert(h.finish()) {
                *x.by.entry(self.policy.why(confidence, evidence)).or_insert(0) += 1;
            }
        }
        v
    }

    /// Whether the walk goes past an edge of `confidence` resting on `evidence` it already listed.
    pub fn follows(&self, confidence: &str, evidence: Option<&Evidence>) -> bool {
        self.policy.judge(confidence, evidence) == Verdict::Follow
    }

    /// What the policy left out, for the answer's `policy`; `None` under `default`.
    pub fn report(&self) -> Option<PolicyReport> {
        if self.policy.is_default() {
            return None;
        }
        let x = self.excluded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Some(PolicyReport { evidence: self.policy.name().to_string(), excluded: x.by.values().sum(), excluded_by: x.by.clone() })
    }

    /// The edge kinds allowed, by name: [`EdgeFilter::new`] of them is this filter again.
    pub fn names(&self) -> Vec<String> {
        self.edges.iter().map(|e| e.to_string()).collect()
    }
}

/// How a node relates to the node it hangs off, read from the parent: `calls` going out, `called by` coming in.
pub fn relation(edge: &str, direction: &str) -> &'static str {
    let inward = direction == "in";
    match (edge, inward) {
        ("calls", false) => "calls",
        ("calls", true) => "called by",
        ("instantiates", false) => "instantiates",
        ("instantiates", true) => "instantiated by",
        ("uses", false) => "uses",
        ("uses", true) => "used by",
        ("reads", false) => "reads",
        ("reads", true) => "read by",
        ("writes", false) => "writes",
        ("writes", true) => "written by",
        ("renders", _) => "rendered by",
        ("conforms_to", _) => "conforms to",
        ("conformer", _) => "implemented by",
        ("witness", _) => "implemented by",
        ("via_protocol", _) => "called through the interface by",
        ("via_di", false) => "calls through DI",
        ("via_di", true) => "called through DI by",
        ("dispatch", _) => "dispatches to",
        ("member", _) => "member",
        ("http", false) => "requests",
        ("http", true) => "requested by",
        ("bus", false) => "publishes to",
        ("bus", true) => "consumed by",
        ("sql", false) => "maps to",
        ("sql", true) => "used by",
        ("ngrx", _) => "dispatches",
        ("child", false) => "renders",
        ("child", true) => "rendered by",
        ("dialog", false) => "opens",
        ("dialog", true) => "opened by",
        ("constructs", false) => "constructs",
        ("constructs", true) => "constructed by",
        // Ansible: a play or role including a role or task file, a task notifying a handler or rendering a
        // template, a play, role or block running a task; a role used by a play in another repo.
        ("includes", false) => "includes",
        ("includes", true) => "included by",
        ("notifies", false) => "notifies",
        ("notifies", true) => "notified by",
        ("templates", false) => "renders template",
        ("templates", true) => "rendered by",
        ("runs", false) => "runs",
        ("runs", true) => "run by",
        ("role", _) => "used by",
        // A CI template, playbook or role deploying an application: its CI job deploys with it.
        ("deploy", false) => "deploys",
        ("deploy", true) => "deployed by",
        // Terraform: a block depending on another (`depends_on`), using a provider configuration, a module call
        // setting a module's variable; a module used by a module call in another repo.
        ("depends_on", false) => "depends on",
        ("depends_on", true) => "depended on by",
        ("provider", false) => "uses provider",
        ("provider", true) => "provider of",
        ("sets", false) => "sets",
        ("sets", true) => "set by",
        ("module", _) => "used by",
        _ => "",
    }
}

impl Trace {
    /// Keeps the root and the nodes whose kind `keep` accepts; a kept node hangs off its nearest kept ancestor (its
    /// depth one below it). Returns how many nodes were dropped.
    pub fn retain_kinds(&mut self, keep: impl Fn(&str) -> bool) -> usize {
        let n = self.nodes.len();
        let mut new_index: Vec<Option<usize>> = vec![None; n];
        let mut out: Vec<TraceNode> = Vec::new();
        for i in 0..n {
            if i != 0 && !keep(&self.nodes[i].kind) {
                continue;
            }
            let mut p = self.nodes[i].parent;
            while let Some(j) = p {
                if new_index[j].is_some() {
                    break;
                }
                p = self.nodes[j].parent;
            }
            let mut node = self.nodes[i].clone();
            node.parent = p.and_then(|j| new_index[j]);
            node.depth = node.parent.map_or(0, |j| out[j].depth + 1);
            new_index[i] = Some(out.len());
            out.push(node);
        }
        let dropped = n - out.len();
        self.nodes = out;
        dropped
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Trace {
    pub nodes: Vec<TraceNode>,
    pub truncated: bool,
    /// Neighbours seen but left out once `max_nodes` was reached: a lower bound on what the limit cut (their own
    /// neighbours were never read).
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
    /// Nodes listed and not grown (see [`Stop`]), to check for neighbours the trace never read ([`frontier`]).
    #[serde(skip)]
    pub stopped: Vec<Stop>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Reads one repo's edges for [`EdgeCache`]: what a symbol uses and what reaches it.
struct Tracer<'c> {
    conn: &'c Connection,
    refs_by_name: HashMap<(String, Vec<&'static str>), Vec<RefHit>>,
    /// Registration (`file:line, lifetime`) of each implementation member reached `via_di`.
    bindings: HashMap<i64, String>,
    /// Edges resolved from a SCIP index: (the symbol reached, the site).
    precise: HashSet<(i64, String)>,
    /// What each reference edge rests on: (the symbol reached, the site) to the reference's evidence.
    evidence: HashMap<(i64, String), Evidence>,
    /// (caller, target) → the caller's other sites reaching the target, past its edge's (see [`EdgeCache::also`]).
    also: HashMap<(i64, i64), Vec<String>>,
    /// Label calls through a protocol requirement with several implementations `ambiguous`, as the forward
    /// `dispatch` edge is (for walks searching both ways).
    one_of: bool,
}

/// A tree node for `s` at `depth` below `parent`, reached `direction` by an edge of kind `edge`.
pub fn node(
    s: &SymbolRow,
    depth: u8,
    parent: Option<usize>,
    direction: &'static str,
    edge: &'static str,
    confidence: &'static str,
    site: Option<String>,
) -> TraceNode {
    TraceNode {
        id: s.id,
        qualified: s.qualified(),
        kind: s.kind.clone(),
        file: s.file.clone(),
        line: s.line,
        depth,
        parent,
        direction,
        edge,
        confidence,
        site,
        site_repo: None,
        binding: None,
        repo: None,
        http: None,
        bus: None,
        sql: None,
        role: None,
        deploy: None,
        module: None,
        library: None,
        precise: false,
        evidence: None,
        cite: None,
    }
}

impl Tracer<'_> {
    fn symbol(&self, id: i64) -> Result<Option<SymbolRow>> {
        symbol(self.conn, id)
    }

    fn refs_named(&mut self, name: &str, kinds: Vec<&'static str>) -> Result<Vec<RefHit>> {
        let key = (name.to_string(), kinds.clone());
        if !self.refs_by_name.contains_key(&key) {
            let (hits, _) = query::refs(self.conn, name, &kinds, usize::MAX)?;
            self.refs_by_name.insert(key.clone(), hits);
        }
        Ok(self.refs_by_name[&key].clone())
    }

    /// What `sym` uses: calls and instantiations in its body, the types it conforms to, and for a protocol
    /// requirement, nothing more (its implementations are dependents, listed under `in`).
    fn out_edges(&mut self, sym: &SymbolRow) -> Result<Vec<Edge>> {
        let mut out = Vec::new();
        if is_type_or_extension(&sym.kind) {
            let mut stmt = self.conn.prepare_cached(
                "SELECT DISTINCT r.target_name, f.path, r.line FROM relations r JOIN files f ON f.id = r.file_id WHERE r.from_id = ?1",
            )?;
            let targets: Vec<(String, String, u32)> =
                stmt.query_map([sym.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
            for (name, file, line) in targets {
                if let Some(t) = query::definitions(self.conn, &name)?.into_iter().find(|d| !is_type_or_extension(&d.kind) || d.kind != "extension") {
                    out.push((t, "conforms_to", "exact", Some(format!("{file}:{line}"))));
                }
            }
            return Ok(out);
        }
        let sql = format!(
            "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
             WHERE r.from_id = ?1 AND r.kind IN ('call', 'instantiate', 'read', 'write', 'role', 'include', 'notify', 'var', 'template',
                                                'tf_ref', 'depends_on', 'provider', 'tf_input', 'tf_output')
             ORDER BY r.line"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut hits: Vec<RefHit> = stmt.query_map([sym.id], ref_row)?.collect::<rusqlite::Result<_>>()?;
        resolve_all(self.conn, &mut hits)?;
        let mut seen = HashSet::new();
        for h in hits {
            if !matches!(h.confidence, "exact" | "unique_name" | "ambiguous") {
                continue;
            }
            let edge = match h.kind.as_str() {
                "instantiate" => "instantiates",
                "read" => "reads",
                "write" => "writes",
                k => crate::ansible::edge_of(k).or_else(|| crate::terraform::edge_of(k)).unwrap_or("calls"),
            };
            // Through a service type the container binds to one implementation: go where the running code goes.
            if let Some(b) = &h.via_di {
                if seen.insert(b.target.id)
                    && let Some(target) = self.symbol(b.target.id)?
                {
                    self.bindings.insert(b.target.id, format!("{}, {}", b.registered, b.lifetime));
                    let site = format!("{}:{}", h.file, h.line);
                    if h.precise {
                        self.precise.insert((target.id, site.clone()));
                    }
                    if let Some(e) = binding_evidence(b).or_else(|| h.evidence.clone()) {
                        self.evidence.insert((target.id, site.clone()), e);
                    }
                    out.push((target, "via_di", h.confidence, Some(site)));
                }
                continue;
            }
            // Several providers in scope: each implementation, any of which may run (`ambiguous`), with where it is
            // provided.
            if !h.via_di_any.is_empty() {
                let n = h.via_di_any.len();
                for b in &h.via_di_any {
                    if seen.insert(b.target.id)
                        && let Some(target) = self.symbol(b.target.id)?
                    {
                        self.bindings.insert(b.target.id, format!("{}, {}; one of {n} providers in scope", b.registered, b.lifetime));
                        let site = format!("{}:{}", h.file, h.line);
                        if let Some(e) = binding_evidence(b) {
                            self.evidence.insert((target.id, site.clone()), e);
                        }
                        out.push((target, "via_di", "ambiguous", Some(site)));
                    }
                }
                continue;
            }
            for &tid in &h.target_ids {
                if seen.insert(tid)
                    && let Some(target) = self.symbol(tid)?
                {
                    let site = format!("{}:{}", h.file, h.line);
                    if h.precise {
                        self.precise.insert((target.id, site.clone()));
                    }
                    if let Some(e) = &h.evidence {
                        self.evidence.insert((target.id, site.clone()), e.clone());
                    }
                    out.push((target, edge, h.confidence, Some(site)));
                }
            }
        }
        // An Ansible playbook, play, role, task file or block runs the plays and tasks in it.
        if matches!(sym.kind.as_str(), "playbook" | "play" | "role" | "tasks" | "task") {
            let sql = format!(
                "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.parent_id = ?1 AND s.kind IN ('play', 'task') ORDER BY s.start_line"
            );
            let mut stmt = self.conn.prepare_cached(&sql)?;
            let children: Vec<SymbolRow> = stmt.query_map([sym.id], symbol_row)?.collect::<rusqlite::Result<_>>()?;
            for c in children {
                if seen.insert(c.id) {
                    out.push((c, "runs", "exact", None));
                }
            }
        }
        Ok(out)
    }

    /// What reaches `sym`: callers (or, for a type, the code that creates or names it, and its conformers),
    /// implementations of a protocol requirement, and calls made through a protocol requirement it implements.
    fn in_edges(&mut self, sym: &SymbolRow) -> Result<Vec<Edge>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        // SwiftUI never calls `body` from user code: a view's body is reached by creating the view.
        if sym.kind == "property" && sym.name == "body" && let Some(container) = &sym.container {
            let view = query::definitions(self.conn, container)?.into_iter().find(|d| d.tags.contains("swiftui_view"));
            if let Some(view) = view {
                let mut edges = self.in_edges(&view)?;
                edges.retain(|(_, edge, _, _)| *edge == "instantiates");
                return Ok(edges.into_iter().map(|(s, _, c, site)| (s, "renders", c, site)).collect());
            }
        }
        let (name, kinds, target_id) = if sym.kind == "init" {
            let ty = sym.container.as_deref().unwrap_or("");
            let id = query::definitions(self.conn, ty)?.first().map(|d| d.id);
            (ty.rsplit('.').next().unwrap_or(ty).to_string(), vec!["instantiate"], id)
        } else if is_type_or_extension(&sym.kind) {
            (sym.name.clone(), vec!["instantiate", "type_ref"], Some(sym.id))
        } else if let Some(kinds) = crate::sql::relation_ref_kinds(&sym.kind, &sym.file) {
            (sym.name.clone(), kinds.to_vec(), Some(sym.id))
        } else if let Some(kinds) = query::terraform_ref_kinds(sym) {
            (sym.name.clone(), kinds.to_vec(), Some(sym.id))
        } else if let Some(kinds) = query::ansible_ref_kinds(sym) {
            (sym.name.clone(), kinds.to_vec(), Some(sym.id))
        } else {
            (sym.name.clone(), vec!["call", "member"], Some(sym.id))
        };
        // A constructor is reached by the creations its arguments pick (see `RefHit::ctors`); of a type declaring
        // several, a creation that can't be told apart is an ambiguous edge to each, never a confident one.
        let overloads = if sym.kind == "init" { constructor_overloads(self.conn, sym)? } else { 0 };
        let mut push_callers = |this: &mut Self, hits: Vec<RefHit>, want: Option<i64>, edge: &'static str| -> Result<()> {
            for mut h in hits {
                if h.confidence == "external_likely" || !want.is_some_and(|id| h.may_target(id)) {
                    continue;
                }
                if sym.kind == "init" && h.kind == "instantiate" {
                    match &h.ctors {
                        Some(called) if !called.contains(&sym.id) => continue,
                        Some(called) if called.len() > 1 => h.confidence = "ambiguous",
                        Some(_) => {}
                        None if overloads > 1 => h.confidence = "ambiguous",
                        None => {}
                    }
                }
                let Some(from) = &h.from else { continue };
                // A foreign key makes one table name another; it doesn't make the table depend on the other's use.
                if from.id == sym.id || (from.kind == "table" && crate::sql::is_sql_path(&h.file)) {
                    continue;
                }
                if !seen.insert(from.id) {
                    // The caller's edge is listed once, at its first site; its other sites are kept beside it.
                    let at = format!("{}:{}", h.file, h.line);
                    let also = this.also.entry((from.id, sym.id)).or_default();
                    if !also.contains(&at) {
                        also.push(at);
                    }
                    continue;
                }
                if let Some(caller) = this.symbol(from.id)? {
                    let edge = if edge.is_empty() {
                        match h.kind.as_str() {
                            // A call of a factory the container injects, making the class.
                            "instantiate" if h.factory => "constructs",
                            _ if want.is_some_and(|id| h.bound_to(id)) => "via_di",
                            "instantiate" => "instantiates",
                            "type_ref" | "member" => "uses",
                            "read" => "reads",
                            "write" => "writes",
                            k => crate::ansible::edge_of(k).or_else(|| crate::terraform::edge_of(k)).unwrap_or("calls"),
                        }
                    } else {
                        edge
                    };
                    let site = format!("{}:{}", h.file, h.line);
                    if h.precise {
                        this.precise.insert((caller.id, site.clone()));
                    }
                    let bound = if edge == "via_di" { h.via_di.iter().chain(&h.via_di_any).find(|b| want == Some(b.target.id)) } else { None };
                    if let Some(e) = bound.and_then(binding_evidence).or_else(|| h.evidence.clone()) {
                        this.evidence.insert((caller.id, site.clone()), e);
                    }
                    // Bound through one of several providers: it may not be this implementation that runs.
                    let confidence = if edge == "via_di" && !h.via_di_any.is_empty() { "ambiguous" } else { h.confidence };
                    out.push((caller, edge, confidence, Some(site)));
                }
            }
            Ok(())
        };
        let hits = self.refs_named(&name, kinds.clone())?;
        push_callers(self, hits, target_id, "")?;
        // A constructor chaining to it: `: base(…)`, `: this(…)`, Swift's `super.init(…)`.
        if sym.kind == "init" {
            let hits = self.refs_named("init", vec!["call"])?;
            push_callers(self, hits, Some(sym.id), "")?;
        }
        // An Ansible task runs when what holds it runs: its block, play, role or task file; a play, when its playbook
        // runs.
        if matches!(sym.kind.as_str(), "task" | "play") {
            let parent: Option<i64> =
                self.conn.prepare_cached("SELECT parent_id FROM symbols WHERE id = ?1")?.query_row([sym.id], |r| r.get(0))?;
            if let Some(p) = parent
                && seen.insert(p)
                && let Some(p) = self.symbol(p)?
            {
                out.push((p, "runs", "exact", None));
            }
            return Ok(out);
        }

        if is_type_or_extension(&sym.kind) {
            // Conformers and subclasses.
            let sql = format!(
                "SELECT {SYMBOL_COLS} FROM relations r JOIN symbols s ON s.id = r.from_id JOIN files f ON f.id = s.file_id
                 WHERE r.target_name = ?1 COLLATE BINARY ORDER BY f.path, s.start_line"
            );
            let mut stmt = self.conn.prepare_cached(&sql)?;
            let rows: Vec<SymbolRow> = stmt.query_map([&sym.name], symbol_row)?.collect::<rusqlite::Result<_>>()?;
            for r in rows {
                out.push((r, "conformer", "exact", None));
            }
        } else if let Some(container) = &sym.container {
            let owner = query::definitions(self.conn, container)?.into_iter().find(|d| d.kind != "extension");
            // An interface's static member (C#: `private static readonly object[] NoValues`) is its own: implementing
            // types have none to supply, unless it is `abstract` or `virtual`.
            let words = |w: &str| sym.signature.split_whitespace().any(|x| x == w);
            let own_static = words("static") && !words("abstract") && !words("virtual");
            if owner.as_ref().is_some_and(|o| o.kind == "protocol") {
                if own_static {
                    return Ok(out);
                }
                // A protocol requirement: its implementations in conforming types depend on it.
                let sql = format!(
                    "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
                     WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind = ?2 AND s.container IN (
                       SELECT CASE WHEN c.container IS NULL THEN c.name ELSE c.container || '.' || c.name END
                       FROM relations r JOIN symbols c ON c.id = r.from_id WHERE r.target_name = ?3 COLLATE BINARY)
                     AND (?5 = 0 OR (s.min_args IS (SELECT min_args FROM symbols WHERE id = ?4) AND s.max_args IS (SELECT max_args FROM symbols WHERE id = ?4))
                          OR s.max_args = (SELECT max_args FROM symbols WHERE id = ?4))
                     ORDER BY f.path, s.start_byte"
                );
                let mut stmt = self.conn.prepare_cached(&sql)?;
                let short = container.rsplit('.').next().unwrap_or(container);
                // Overloads of the requirement: only the implementations taking the same parameters (see
                // [`query::same_parameters`]).
                let overloaded = query::definitions(self.conn, &sym.qualified())?.iter().filter(|d| d.container == sym.container && d.kind == sym.kind).count() > 1;
                let rows: Vec<SymbolRow> =
                    stmt.query_map(rusqlite::params![sym.name, sym.kind, short, sym.id, overloaded], symbol_row)?.collect::<rusqlite::Result<_>>()?;
                for r in rows {
                    out.push((r, "witness", "exact", None));
                }
            } else {
                // An implementation: calls made through a protocol requirement it implements reach it too.
                let short = container.rsplit('.').next().unwrap_or(container).to_string();
                let mut stmt = self.conn.prepare_cached(
                    "SELECT DISTINCT r.target_name FROM relations r JOIN symbols s ON s.id = r.from_id
                     WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY ORDER BY r.target_name",
                )?;
                let protocols: Vec<String> = stmt.query_map([&short], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                // The members walked through already, so none is walked twice.
                let mut through: HashSet<i64> = HashSet::new();
                let conn = self.conn;
                let arity = |id: i64| -> Result<(Option<u32>, Option<u32>)> {
                    Ok(conn
                        .prepare_cached("SELECT min_args, max_args FROM symbols WHERE id = ?1")?
                        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?)
                };
                let own = arity(sym.id)?;
                for p in protocols {
                    // The requirement taking the arguments it does: a `Find(query, parent)` overload doesn't implement
                    // the interface's `Find(query)`, and calls of that one don't reach it. Of several taking as many
                    // (`Find(query, bool)`, `Find(query, parents)`), the members below tell them apart by type.
                    let mut reqs = Vec::new();
                    for d in query::definitions(self.conn, &format!("{p}.{}", sym.name))? {
                        if d.kind == sym.kind && d.container.as_deref().is_some_and(|c| c == p || c.ends_with(&format!(".{p}"))) {
                            let theirs = arity(d.id)?;
                            if query::same_parameters(theirs, own) {
                                reqs.push(d);
                            }
                        }
                    }
                    let Some(req) = reqs.pop().filter(|_| reqs.is_empty()) else { continue };
                    let is_protocol = query::definitions(self.conn, &p)?.iter().any(|d| d.kind == "protocol");
                    if !is_protocol {
                        continue;
                    }
                    if !through.insert(req.id) {
                        continue;
                    }
                    let hits = self.refs_named(&sym.name, kinds.clone())?;
                    let hits = if self.one_of { through_one_of(self.conn, req.id, hits)? } else { hits };
                    push_callers(self, hits, Some(req.id), "via_protocol")?;
                }
                // Members declared further up: an interface's base interface (`Impl : IDerived`, `IDerived : IBase`,
                // the member on `IBase`), and the abstract or virtual base class member an `override` overrides. A
                // call through any of them may run this implementation.
                // A call through a base member reaches this implementation only through a receiver this type is one
                // of: `IOrders.Get` through `IRepo<Order>.Get` reaches it, `IPlans.Get` (another `IRepo<Plan>`) doesn't.
                let mut is_a = supertype_names(self.conn, &short)?;
                is_a.insert(short.clone());
                for m in query::implemented_members(self.conn, sym)? {
                    if !through.insert(m.id) {
                        continue;
                    }
                    let Some(req) = self.symbol(m.id)? else { continue };
                    let hits: Vec<RefHit> = self
                        .refs_named(&req.name, kinds.clone())?
                        .into_iter()
                        .filter(|h| h.receiver_type.as_deref().is_none_or(|r| is_a.contains(bare_type(r))))
                        .collect();
                    let hits = if self.one_of { through_one_of(self.conn, req.id, hits)? } else { hits };
                    // A base class member runs itself, or another override, for other instances.
                    let hits = if self.one_of && m.shared { ambiguous_all(hits) } else { hits };
                    push_callers(self, hits, Some(req.id), "via_protocol")?;
                }
                // And the interface members a SCIP index says it implements.
                for base in crate::scip::implemented(self.conn, sym.id)? {
                    if !through.insert(base) {
                        continue;
                    }
                    let Some(req) = self.symbol(base)? else { continue };
                    let hits = self.refs_named(&req.name, kinds.clone())?;
                    let hits = if self.one_of { through_one_of(self.conn, req.id, hits)? } else { hits };
                    push_callers(self, hits, Some(req.id), "via_protocol")?;
                }
                // And calls through a library's interface or base class it implements, one no indexed repo defines
                // (`ILogger` from a package): the repo has no member to resolve them to, so they stay unresolved. One
                // on a receiver of that type, passing arguments this member takes, may run it. Another implementation
                // may run instead, the library's own or another type's, so each is `ambiguous`. With the library
                // indexed, they resolve into it and `impact` lists them across repos instead.
                if matches!(sym.kind.as_str(), "method" | "property") {
                    let library: HashSet<String> =
                        query::outside_supertype_names(self.conn, container)?.iter().map(|t| bare_type(t).to_string()).collect();
                    if !library.is_empty() {
                        let mut hits = Vec::new();
                        for mut h in self.refs_named(&sym.name, kinds.clone())? {
                            if h.confidence != "unresolved"
                                || !h.target_ids.is_empty()
                                || h.receiver.is_none()
                                || !h.receiver_type.as_deref().is_some_and(|r| library.contains(bare_type(r)))
                                || !query::may_call_overload(self.conn, &h, sym.id)?
                            {
                                continue;
                            }
                            h.confidence = "ambiguous";
                            h.target_ids = vec![sym.id];
                            h.evidence = Some(Evidence::new(crate::answer::Mechanism::ReceiverType));
                            hits.push(h);
                        }
                        push_callers(self, hits, Some(sym.id), "via_protocol")?;
                    }
                }
            }
        }
        Ok(out)
    }
}

/// How many constructors types named as the constructor `init`'s declare.
fn constructor_overloads(conn: &Connection, init: &SymbolRow) -> Result<usize> {
    let Some(ty) = &init.container else { return Ok(1) };
    let n: i64 = conn.prepare_cached("SELECT COUNT(*) FROM symbols WHERE kind = 'init' AND container = ?1")?.query_row([ty], |r| r.get(0))?;
    Ok(n as usize)
}

/// What a binding rests on when it assumed something (an Angular provider placed by its directory): the registration,
/// with that assumption. `None` for a binding assuming nothing, whose edge takes its usual evidence.
fn binding_evidence(b: &crate::di::Bound) -> Option<Evidence> {
    (!b.assumptions.is_empty()).then(|| Evidence::new(crate::answer::Mechanism::DiBinding).assuming_all(&b.assumptions))
}

/// Calls through the protocol requirement `req`, as calls of one implementation among several: `ambiguous`, as the
/// forward edge from the requirement to each implementation (`dispatch`) is, so a walk filters the edge the same
/// way in either direction.
fn through_one_of(conn: &Connection, req: i64, hits: Vec<RefHit>) -> Result<Vec<RefHit>> {
    if crate::db::running_implementations(conn, req)?.len() <= 1 {
        return Ok(hits);
    }
    Ok(hits
        .into_iter()
        .map(|mut h| {
            if matches!(h.confidence, "exact" | "unique_name") {
                h.confidence = "ambiguous";
            }
            h
        })
        .collect())
}

/// The names of the supertypes of the types named `ty`, transitively (bare names, generic arguments dropped).
fn supertype_names(conn: &Connection, ty: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT r.target_name FROM relations r JOIN symbols s ON s.id = r.from_id
         WHERE s.name = ?1 AND s.name = ?1 COLLATE BINARY AND s.kind IN ('class', 'struct', 'protocol', 'extension')",
    )?;
    let mut out = HashSet::new();
    let mut queue = vec![ty.to_string()];
    while let Some(t) = queue.pop() {
        let parents: Vec<String> = stmt.query_map([&t], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for p in parents {
            let p = bare_type(&p).to_string();
            if out.len() < 64 && out.insert(p.clone()) {
                queue.push(p);
            }
        }
    }
    Ok(out)
}

/// `Ns.IRepo<Order>` → `IRepo`.
fn bare_type(t: &str) -> &str {
    let t = t.split('<').next().unwrap_or(t).trim();
    t.rsplit('.').next().unwrap_or(t)
}

/// `hits`, each a call that may reach another implementation: `ambiguous` where it was confident.
fn ambiguous_all(hits: Vec<RefHit>) -> Vec<RefHit> {
    hits.into_iter()
        .map(|mut h| {
            if matches!(h.confidence, "exact" | "unique_name") {
                h.confidence = "ambiguous";
            }
            h
        })
        .collect()
}

/// Lookups one repo's edges reuse between calls, for walks that drive [`out_edges`] and [`in_edges`] themselves
/// (`trace`, `impact`, `path`, `flow`).
#[derive(Default)]
pub struct EdgeCache {
    refs_by_name: HashMap<(String, Vec<&'static str>), Vec<RefHit>>,
    bindings: HashMap<i64, String>,
    precise: HashSet<(i64, String)>,
    evidence: HashMap<(i64, String), Evidence>,
    also: HashMap<(i64, i64), Vec<String>>,
}

impl EdgeCache {
    /// The other sites in `caller` reaching `target` by the same kind of reference, past the one its edge carries
    /// (`file:line`, in file order): a method calling another on several lines.
    pub fn also(&self, caller: i64, target: i64) -> &[String] {
        self.also.get(&(caller, target)).map_or(&[], Vec::as_slice)
    }

    /// Whether the edge reaching `id` at `site` was resolved from a SCIP index.
    pub fn precise(&self, id: i64, site: Option<&str>) -> bool {
        site.is_some_and(|s| self.precise.contains(&(id, s.to_string())))
    }

    /// What the edge of kind `edge` and `confidence` reaching `id` at `site` rests on (see [`Evidence::of_edge`]).
    pub fn evidence(&self, id: i64, edge: &str, confidence: &str, site: Option<&str>) -> Option<Evidence> {
        let resolved = site.and_then(|s| self.evidence.get(&(id, s.to_string())));
        Evidence::of_edge(edge, confidence, resolved, None, self.precise(id, site))
    }

    /// For an implementation member reached `via_di`: where the binding is registered, and its lifetime.
    pub fn binding(&self, id: i64) -> Option<&str> {
        self.bindings.get(&id).map(String::as_str)
    }

    fn with<T>(&mut self, conn: &Connection, f: impl FnOnce(&mut Tracer) -> Result<T>) -> Result<T> {
        let mut t = Tracer {
            conn,
            refs_by_name: std::mem::take(&mut self.refs_by_name),
            bindings: std::mem::take(&mut self.bindings),
            precise: std::mem::take(&mut self.precise),
            evidence: std::mem::take(&mut self.evidence),
            also: std::mem::take(&mut self.also),
            one_of: false,
        };
        let out = f(&mut t);
        self.refs_by_name = t.refs_by_name;
        self.bindings = t.bindings;
        self.precise = t.precise;
        self.evidence = t.evidence;
        self.also = t.also;
        out
    }
}

/// What `sym` uses, as `trace -d out` expands it: calls and instantiations in its body (a call through a service the
/// container binds going `via_di` to the implementation), and for a type, the types it conforms to.
pub fn out_edges(conn: &Connection, cache: &mut EdgeCache, sym: &SymbolRow) -> Result<Vec<Edge>> {
    cache.with(conn, |t| t.out_edges(sym))
}

/// What reaches `sym`, as `trace -d in` expands it: callers, code creating or naming a type, conformers,
/// implementations of a protocol requirement (`witness`) and calls through a requirement it implements.
pub fn in_edges(conn: &Connection, cache: &mut EdgeCache, sym: &SymbolRow) -> Result<Vec<Edge>> {
    cache.with(conn, |t| {
        t.one_of = true;
        t.in_edges(sym)
    })
}

/// What reaches `sym` as `trace -d in` and `impact` list it ([`in_edges`]), a call through a protocol requirement
/// several types implement keeping the call's own confidence: the trees list the requirement's callers under each
/// implementation, where `path` and `flow` (reading [`in_edges`]) count the call as one of several (`ambiguous`).
pub fn tree_in_edges(conn: &Connection, cache: &mut EdgeCache, sym: &SymbolRow) -> Result<Vec<Edge>> {
    cache.with(conn, |t| t.in_edges(sym))
}

/// For a protocol (interface) member, its implementations in the types that declare the protocol; nothing for
/// other symbols.
pub fn implementations(conn: &Connection, sym: &SymbolRow) -> Result<Vec<SymbolRow>> {
    let mut out = Vec::new();
    for id in crate::db::implementations(conn, sym.id)? {
        if let Some(s) = symbol(conn, id)? {
            out.push(s);
        }
    }
    Ok(out)
}

/// Of a protocol member's [`implementations`], the ids of those that may run outside tests: test doubles are left out
/// when one real implementation remains.
pub fn running_implementations(conn: &Connection, sym: &SymbolRow) -> Result<Vec<i64>> {
    crate::db::running_implementations(conn, sym.id)
}

/// The methods, functions and initializers declared directly in a type, in source order.
pub fn members(conn: &Connection, ty: &SymbolRow) -> Result<Vec<SymbolRow>> {
    let sql = format!(
        "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.parent_id = ?1 AND s.kind IN ('method', 'function', 'init') ORDER BY s.start_line"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt.query_map([ty.id], symbol_row)?.collect::<rusqlite::Result<_>>()?)
}

/// A TypeScript class's fields whose initializers (or accessors) call or create something, in source order:
/// `data = toSignal(this.svc.load())`, `x$ = this.store.select(…)`, `total = computed(() => …)`. They run when an
/// instance is made, as its constructor does.
pub fn initializers(conn: &Connection, ty: &SymbolRow) -> Result<Vec<SymbolRow>> {
    let sql = format!(
        "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.parent_id = ?1 AND s.kind = 'property'
         AND EXISTS(SELECT 1 FROM refs r WHERE r.from_id = s.id AND r.kind IN ('call', 'instantiate')) ORDER BY s.start_line"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt.query_map([ty.id], symbol_row)?.collect::<rusqlite::Result<_>>()?)
}

/// The symbol of `kind` starting at `line` of `file`: a SQL definition the workspace summary names, say.
pub fn symbol_starting(conn: &Connection, file: &str, line: u32, kind: &str) -> Result<Option<SymbolRow>> {
    let sql = format!(
        "SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.path = ?1 AND s.start_line = ?2 AND s.kind = ?3 LIMIT 1"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt.query_map(rusqlite::params![file, line, kind], symbol_row)?.next().transpose()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct Affected {
    pub id: i64,
    pub qualified: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    /// Hops from the changed symbols, along the way the row's evidence (`edge`, `confidence`, `via`) describes.
    pub hop: u8,
    /// When an ambiguous way reaches it nearer than `hop`: that hop. The row keeps the confident way as its evidence,
    /// but the class goes by this nearer hop, so a confident path never makes it look farther than it may be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nearest_ambiguous_hop: Option<u8>,
    /// Proximity (see [`class_of`]): `direct` (hop 1), `near` (2), `far` (3+), `test`, by the nearer of `hop` and
    /// `nearest_ambiguous_hop`.
    pub class: &'static str,
    /// How it depends on the symbol it was reached from.
    pub edge: &'static str,
    pub confidence: &'static str,
    /// The symbol it was reached from.
    pub via: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// The repo it is in, when it isn't the analysed repo: reached across an HTTP route or the message bus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// For an `http` edge, the route crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpVia>,
    /// For a `bus` edge, the message crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus: Option<BusVia>,
    /// For a `sql` edge, the SQL object crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<SqlVia>,
    /// For a `role` edge, the Ansible role crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleVia>,
    /// For a `deploy` edge, the deploy chain crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy: Option<DeployVia>,
    /// For a `module` edge, the Terraform module call crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<ModuleVia>,
    /// For an edge into a repo using a library, the library crossed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library: Option<LibraryVia>,
    /// The other sites in it reaching the symbol it was reached from, past `site` (`file:line`): a method calling
    /// another on several lines.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
    /// For a SQL object dumped in several places, the other copies reached (`file:line`), listed once here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub copies: Vec<String>,
    /// `via` is a definition the change removed (a diff's pre-change side): this symbol still names it, matched by
    /// name, so the change breaks it unless something else of the name now serves.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub via_removed: bool,
    /// What the edge (`edge`, `confidence`) rests on (see [`Evidence`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// The lines the row rests on: the edge's site, else its declaration (see [`crate::answer::Cite`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cite: Option<crate::answer::Cite>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Impact {
    /// The changed symbols the analysis starts from.
    pub scope: Vec<TraceNode>,
    pub affected: Vec<Affected>,
    pub counts: std::collections::BTreeMap<&'static str, usize>,
    /// The nearest proximity class present (`direct`, `near`, `far`), or `none`: how close the change comes, not how
    /// much it matters.
    pub nearest: &'static str,
    /// Affected symbols reached through an edge that isn't `exact`.
    pub low_confidence: usize,
    pub truncated: bool,
    /// Where the walk stopped short of everything depending on the change: listed symbols it didn't walk past (see
    /// [`Stop`]), to check for dependents it never read ([`frontier`]).
    #[serde(skip)]
    pub stopped: Vec<Stop>,
}

/// A symbol a walk listed but didn't walk past: at its depth (`depth`), or reached only by an edge the evidence
/// policy lists without following (`ambiguous`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub id: i64,
    pub hop: u8,
    pub reason: &'static str,
    /// Which way the walk went: `in` (dependents) or `out` (what it uses, for `trace`).
    pub dir: &'static str,
    /// The repo it is in, when it isn't the analysed repo.
    pub repo: Option<String>,
}

/// A symbol a walk stopped at that has dependents (or, for `trace` out, callees) the walk never read: what the
/// answer leaves out, and where to continue from.
#[derive(Debug, Clone, Serialize)]
pub struct Frontier {
    pub qualified: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    /// Its hop (impact) or depth (trace).
    pub hop: u8,
    /// `depth`: the walk ended at the depth asked for. `ambiguous`: the edge reaching it is ambiguous, which the
    /// evidence policy lists without walking past (`default`); `--evidence all` walks past it.
    pub reason: &'static str,
    /// Neighbours one hop past it the answer doesn't list: at least this many.
    pub beyond: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The exact call going on from it (set by the answer: the tool, its symbol, repo, depth and filters).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next: String,
    #[serde(skip)]
    pub id: i64,
    /// Which way the walk went past it: `in` or `out`.
    #[serde(skip)]
    pub dir: &'static str,
}

/// Of the symbols `stops` names (of the repo `conn` indexes), those with neighbours one hop past them in the stop's
/// direction (`in`: dependents, as impact and `trace in` walk; `out`: what it uses) that the edge filter would list and
/// `listed` says the answer doesn't have. At most `budget` are checked; also returns how many weren't.
pub fn frontier(
    conn: &Connection,
    stops: &[Stop],
    listed: &dyn Fn(i64) -> bool,
    filter: &EdgeFilter,
    budget: usize,
) -> Result<(Vec<Frontier>, usize)> {
    let mut t = Tracer { conn, refs_by_name: HashMap::new(), bindings: HashMap::new(), precise: HashSet::new(), evidence: HashMap::new(), also: HashMap::new(), one_of: false };
    let mut out = Vec::new();
    let mut checked = HashSet::new();
    let mut unchecked = 0;
    for s in stops {
        if !checked.insert((s.id, s.dir)) {
            continue;
        }
        if checked.len() > budget {
            unchecked += 1;
            continue;
        }
        let Some(sym) = t.symbol(s.id)? else { continue };
        let mut edges = if s.dir == "out" { t.out_edges(&sym)? } else { t.in_edges(&sym)? };
        if s.dir != "out" {
            edges.extend(ngrx_dispatchers(conn, &sym)?);
        }
        let mut beyond = HashSet::new();
        for (dep, edge, confidence, site) in edges {
            if !filter.allows(edge) || dep.id == sym.id || listed(dep.id) {
                continue;
            }
            let precise = site.as_ref().is_some_and(|x| t.precise.contains(&(dep.id, x.clone())));
            let resolved = site.as_ref().and_then(|x| t.evidence.get(&(dep.id, x.clone()))).cloned();
            let evidence = Evidence::of_edge(edge, confidence, resolved.as_ref(), None, precise);
            // Judged without counting: an edge the policy leaves out is reported as the policy's.
            if filter.policy().judge(confidence, evidence.as_ref()) != Verdict::Exclude {
                beyond.insert(dep.id);
            }
        }
        if !beyond.is_empty() {
            out.push(Frontier {
                qualified: sym.qualified(),
                kind: sym.kind.clone(),
                file: sym.file.clone(),
                line: sym.line,
                hop: s.hop,
                reason: s.reason,
                beyond: beyond.len(),
                repo: s.repo.clone(),
                next: String::new(),
                id: sym.id,
                dir: s.dir,
            });
        }
    }
    Ok((out, unchecked))
}

impl Impact {
    /// Recomputes `counts`, `nearest` and `low_confidence` from `affected`, after dependents were added to it.
    pub fn recount(&mut self) {
        let mut counts = std::collections::BTreeMap::new();
        for a in &self.affected {
            *counts.entry(a.class).or_insert(0) += 1;
        }
        self.nearest = PROXIMITY.into_iter().find(|c| counts.contains_key(c)).unwrap_or("none");
        self.low_confidence = self.affected.iter().filter(|a| a.confidence != "exact").count();
        self.counts = counts;
    }

    /// Lists the copies of one SQL object (a schema dumped in several places of one repo: the same kind and
    /// `schema.name`) once: the nearest copy, outside a secondary copy's directory when one is as near, with the
    /// others in its `copies`. Recounts.
    pub fn collapse_sql_copies(&mut self) {
        let key = |a: &Affected| (a.repo.clone(), a.kind.clone(), a.qualified.to_lowercase());
        let mut at: HashMap<(Option<String>, String, String), usize> = HashMap::new();
        let mut out: Vec<Affected> = Vec::with_capacity(self.affected.len());
        for a in std::mem::take(&mut self.affected) {
            if !crate::sql::is_sql_path(&a.file) {
                out.push(a);
                continue;
            }
            match at.get(&key(&a)) {
                None => {
                    at.insert(key(&a), out.len());
                    out.push(a);
                }
                Some(&i) => {
                    let kept = &mut out[i];
                    let site = |x: &Affected| format!("{}:{}", x.file, x.line);
                    if a.hop == kept.hop && crate::sql::secondary_copy_path(&kept.file) && !crate::sql::secondary_copy_path(&a.file) {
                        let old = std::mem::replace(kept, a);
                        kept.copies = old.copies.clone();
                        kept.copies.push(site(&old));
                    } else {
                        let s = site(&a);
                        if !kept.copies.contains(&s) {
                            kept.copies.push(s);
                        }
                    }
                }
            }
        }
        self.affected = out;
        self.recount();
    }
}

/// The proximity classes of non-test code, nearest first.
pub const PROXIMITY: [&str; 3] = ["direct", "near", "far"];

/// How near a change an affected symbol is, in graph distance: `direct` (hop 1), `near` (2), `far` (3+), or `test`
/// for test code at any hop, which never sets `nearest`. Distance only: it says nothing of how much the symbol
/// matters, how often it runs, or whether the change breaks it.
pub fn class_of(hop: u8, file: &str) -> &'static str {
    if crate::is_test_path(file) {
        return "test";
    }
    match hop {
        1 => "direct",
        2 => "near",
        _ => "far",
    }
}

/// The name a hop had before proximity classes (`critical` hop 1, `high` 2, `medium` 3, `low` 4+), for the
/// deprecated `worst` field kept one release for scripts reading it.
pub fn legacy_class(hop: u8) -> &'static str {
    match hop {
        1 => "critical",
        2 => "high",
        3 => "medium",
        _ => "low",
    }
}

/// The deepest `impact` goes. The walk's budget is its symbol limit (`--limit`): a deeper walk lists no more than
/// that, and says where it stopped (see [`Frontier`]).
pub const MAX_IMPACT_DEPTH: u8 = 16;

/// How much an edge's confidence says: `exact` 3, `unique_name` 2, anything else (`ambiguous`) 0.
fn strength(confidence: &str) -> u8 {
    match confidence {
        "exact" => 3,
        "unique_name" => 2,
        _ => 0,
    }
}

/// The best way an impact walk has reached each symbol so far: the hop and the confidence of the row listing it.
/// Which way is best: a confident (`exact` or `unique_name`) edge beats an `ambiguous` one at any hop, then the
/// nearer hop, then `exact` beats `unique_name`. A better way replaces the row's evidence, and a symbol is grown
/// (its dependents walked) from the first confident way to it and again from any nearer one, so the order edges are
/// read in never hides a dependent or leaves a symbol labelled weaker than an edge found to it.
/// The nearest an ambiguous way reached a symbol is kept too: see [`Affected::nearest_ambiguous_hop`].
#[derive(Debug, Clone, Default)]
pub struct Reached {
    best: HashMap<i64, (u8, u8)>,
    ambiguous: HashMap<i64, u8>,
    /// Symbols listed and not walked past (see [`Stop`]): their hop and why.
    stops: std::collections::BTreeMap<i64, (u8, &'static str)>,
    /// Symbols walked past (grown) at least once.
    grown: HashSet<i64>,
}

impl Reached {
    fn rank(hop: u8, strength: u8) -> (bool, u8, u8) {
        (strength < 2, hop, 3 - strength)
    }

    /// Records `id` as reached at `hop` by an edge of `confidence` unless it was reached as well or better; `None`
    /// when it was. Otherwise says whether to grow it from here: the edge is confident and no confident way at this
    /// hop or nearer was grown before.
    pub fn offer(&mut self, id: i64, hop: u8, confidence: &str) -> Option<bool> {
        self.offer_as(id, hop, confidence, strength(confidence) >= 2)
    }

    /// [`Reached::offer`], growing from here only when `follow` (an evidence policy following `ambiguous` edges
    /// grows them too; one listing them as leaves doesn't).
    pub fn offer_as(&mut self, id: i64, hop: u8, confidence: &str, follow: bool) -> Option<bool> {
        let s = strength(confidence);
        if s < 2 {
            let a = self.ambiguous.entry(id).or_insert(hop);
            *a = (*a).min(hop);
        }
        let before = self.best.get(&id).copied();
        if before.is_some_and(|(h, b)| Self::rank(h, b) <= Self::rank(hop, s)) {
            return None;
        }
        self.best.insert(id, (hop, s));
        Some(follow && before.is_none_or(|(h, b)| b < 2 || h > hop))
    }

    /// Records the changed symbol `id` (hop 0, never bettered). Says whether it was new.
    pub fn root(&mut self, id: i64) -> bool {
        self.best.insert(id, (0, 3)).is_none()
    }

    /// Records that the walk stops at `id`, reached at `hop`, for `reason` (see [`Stop`]): unless it was walked past
    /// already, or recorded as a stop before.
    pub fn stop(&mut self, id: i64, hop: u8, reason: &'static str) {
        if self.grown.contains(&id) {
            return;
        }
        // A deeper walk would go past it: that says more than an ambiguous way to it does.
        let e = self.stops.entry(id).or_insert((hop, reason));
        if reason == "depth" {
            *e = (hop, reason);
        }
    }

    /// Records that the walk goes past `id`: it is no stop, whatever an earlier way to it was.
    pub fn grew(&mut self, id: i64) {
        self.grown.insert(id);
        self.stops.remove(&id);
    }

    /// Whether the walk went past `id`.
    pub fn was_grown(&self, id: i64) -> bool {
        self.grown.contains(&id)
    }

    /// The stops recorded, labelled with `repo`, in id order.
    pub fn stops(&self, repo: Option<&str>) -> Vec<Stop> {
        self.stops.iter().map(|(&id, &(hop, reason))| Stop { id, hop, reason, dir: "in", repo: repo.map(str::to_string) }).collect()
    }

    /// Whether `id` was listed (or is a changed symbol).
    pub fn listed(&self, id: i64) -> bool {
        self.best.contains_key(&id)
    }

    /// The hop `id` was reached at, if it was.
    pub fn hop(&self, id: i64) -> Option<u8> {
        self.best.get(&id).map(|&(h, _)| h)
    }

    /// Sets `a`'s nearest ambiguous hop, when an ambiguous way reached it nearer than its row's way, and its class
    /// by the nearer hop.
    pub fn settle(&self, a: &mut Affected) {
        a.nearest_ambiguous_hop = self.ambiguous.get(&a.id).copied().filter(|&h| h < a.hop);
        a.class = class_of(a.nearest_ambiguous_hop.unwrap_or(a.hop), &a.file);
    }
}

/// When `context` is an Entity Framework context with DbSets of the type `entity`: those DbSet properties. Empty
/// otherwise.
pub fn entity_sets(conn: &Connection, entity: &SymbolRow, context: &SymbolRow) -> Result<Vec<SymbolRow>> {
    if !is_type_or_extension(&entity.kind) || context.kind != "class" {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT {SYMBOL_COLS} FROM db_facts d JOIN symbols s ON s.id = d.symbol_id JOIN files f ON f.id = s.file_id
         WHERE d.kind = 'dbset' AND (d.entity = ?1 OR d.entity LIKE '%.' || ?1) AND s.container = ?2
         ORDER BY f.path, s.start_line"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt.query_map(rusqlite::params![entity.name, context.qualified()], symbol_row)?.collect::<rusqlite::Result<_>>()?)
}

/// NgRx, inward: for an effect (a member calling `createEffect`), the code dispatching an action its `ofType` names
/// (calling the action creator, a constant made by `createAction`), as `path` and `flow` link them forward. Effects
/// and dispatches name actions by name, so the edge is `unique_name` when one action creator in the repo has that
/// name, else `ambiguous`.
pub fn ngrx_dispatchers(conn: &Connection, sym: &SymbolRow) -> Result<Vec<Edge>> {
    let calls = |id: i64, name: &str| -> Result<bool> {
        Ok(conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM refs WHERE from_id = ?1 AND kind = 'call' AND name = ?2)")?
            .query_row(rusqlite::params![id, name], |r| r.get(0))?)
    };
    if !matches!(sym.kind.as_str(), "property" | "method" | "function") || !calls(sym.id, "createEffect")? {
        return Ok(Vec::new());
    }
    let names: Vec<String> = conn
        .prepare_cached("SELECT DISTINCT name FROM refs WHERE from_id = ?1 AND kind IN ('member', 'type_ref') ORDER BY name")?
        .query_map([sym.id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for name in names {
        let actions: Vec<i64> = conn
            .prepare_cached(
                "SELECT s.id FROM symbols s WHERE s.name = ?1
                 AND EXISTS(SELECT 1 FROM refs c WHERE c.from_id = s.id AND c.kind = 'call' AND c.name = 'createAction')
                 ORDER BY (SELECT path FROM files WHERE id = s.file_id), s.start_byte",
            )?
            .query_map([&name], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if actions.is_empty() {
            continue;
        }
        let confidence = if actions.len() == 1 { "unique_name" } else { "ambiguous" };
        let callers: Vec<(i64, String, u32)> = conn
            .prepare_cached(
                "SELECT r.from_id, f.path, MIN(r.line) FROM refs r JOIN files f ON f.id = r.file_id
                 WHERE r.name = ?1 AND r.kind = 'call' AND r.from_id IS NOT NULL GROUP BY r.from_id ORDER BY f.path, MIN(r.line)",
            )?
            .query_map([&name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, file, line) in callers {
            if id == sym.id || actions.contains(&id) || !seen.insert(id) {
                continue;
            }
            if let Some(caller) = symbol(conn, id)? {
                out.push((caller, "ngrx", confidence, Some(format!("{file}:{line}"))));
            }
        }
    }
    // Actions of a group (`ofType(AccountActions.search)`): the code calling that group's creator.
    let groups = crate::ngrx::Groups::load(conn)?;
    for action in groups.named(conn, sym.id)? {
        let confidence = groups.confidence(&action);
        for (id, file, line) in crate::ngrx::dispatchers(conn, &action)? {
            if id == sym.id || !seen.insert(id) {
                continue;
            }
            if let Some(caller) = symbol(conn, id)? {
                out.push((caller, "ngrx", confidence, Some(format!("{file}:{line}"))));
            }
        }
    }
    Ok(out)
}

/// A code site that creates a request contract: the enclosing symbol, the reference's confidence, and `file:line`.
pub type Creator = (SymbolRow, &'static str, Option<String>);

/// The code creating a request contract (a request DTO): what `instantiate`s the type `contract` defined in `file`.
/// Also returns the contract's id. `None` when the contract isn't defined there.
/// A route requested from a call site (a method naming the service and path, or calling an HTTP client with a URL)
/// has that method as its one creator. `line` is the contract's or method's first line (its route's `start_line`), which
/// tells overloads apart; 0 when unknown.
pub fn creators(conn: &Connection, contract: &str, file: &str, line: u32) -> Result<Option<(i64, Vec<Creator>)>> {
    let defs: Vec<SymbolRow> = defined_at(conn, contract, file, line)?;
    let def = defs.iter().find(|d| is_type_or_extension(&d.kind) && d.kind != "extension");
    let Some(def) = def else {
        return Ok(defs.into_iter().next().map(|d| {
            let site = Some(format!("{}:{}", d.file, d.line));
            (d.id, vec![(d, "exact", site)])
        }));
    };
    let (hits, _) = query::refs(conn, &def.name, &["instantiate"], usize::MAX)?;
    let mut out: Vec<Creator> = Vec::new();
    for h in hits {
        if h.confidence == "external_likely" || !h.may_target(def.id) {
            continue;
        }
        let Some(from) = &h.from else { continue };
        if out.iter().any(|(s, ..)| s.id == from.id) || publishes(conn, from.id, &def.name)? {
            continue;
        }
        if let Some(s) = symbol(conn, from.id)? {
            out.push((s, h.confidence, Some(format!("{}:{}", h.file, h.line))));
        }
    }
    Ok(Some((def.id, out)))
}

/// Whether the symbol `id` publishes a bus message named `message`: a DTO that is a bus message and a request contract
/// both, created there, is sent on the bus, not over HTTP.
pub fn publishes(conn: &Connection, id: i64, message: &str) -> Result<bool> {
    let mut stmt = conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM bus_facts WHERE symbol_id = ?1 AND kind = 'publish' AND message_name = ?2)")?;
    Ok(stmt.query_row(rusqlite::params![id, message], |r| r.get(0))?)
}

/// The definitions of `qualified` in `file` starting at `line` (0: any line). Overloads share a qualified name and a
/// file, so a route or a fact naming a symbol is mapped back to it by its first line.
pub fn defined_at(conn: &Connection, qualified: &str, file: &str, line: u32) -> Result<Vec<SymbolRow>> {
    Ok(query::definitions(conn, qualified)?.into_iter().filter(|d| d.file == file && (line == 0 || d.line == line)).collect())
}

/// A symbol by id.
pub fn symbol(conn: &Connection, id: i64) -> Result<Option<SymbolRow>> {
    let sql = format!("SELECT {SYMBOL_COLS} FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1");
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt.query_map([id], symbol_row)?.next().transpose()?)
}

/// The deepest `trace` goes.
pub const MAX_TRACE_DEPTH: u8 = 3;

/// Symbols whose span overlaps a changed line range, innermost first: for each range, the overlapping symbols
/// with no overlapping child.
pub fn symbols_in_ranges(conn: &Connection, file: &str, ranges: &[(u32, u32)]) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.id, s.parent_id, s.start_line, s.end_line FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.path = ?1 AND s.kind != 'extension'",
    )?;
    let syms: Vec<(i64, Option<i64>, u32, u32)> =
        stmt.query_map([file], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for &(a, b) in ranges {
        let hits: Vec<&(i64, Option<i64>, u32, u32)> = syms.iter().filter(|s| s.2 <= b && s.3 >= a).collect();
        for h in &hits {
            let has_inner = hits.iter().any(|o| o.1 == Some(h.0));
            if !has_inner && !out.contains(&h.0) {
                out.push(h.0);
            }
        }
    }
    Ok(out)
}

/// Every symbol defined in a file except extensions (whose members are listed themselves).
pub fn symbols_in_file(conn: &Connection, file: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.id FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.path = ?1 AND s.kind != 'extension'
         ORDER BY s.start_line",
    )?;
    let ids = stmt.query_map([file], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impact_classes_name_distance_not_risk() {
        let got: Vec<&str> = (1..=5).map(|h| class_of(h, "src/a.cs")).collect();
        assert_eq!(got, ["direct", "near", "far", "far", "far"]);
        assert_eq!(class_of(1, "tests/ATests.cs"), "test");
        // The deprecated names, for `worst`, are the old ones by the same hop.
        let old: Vec<&str> = (1..=5).map(legacy_class).collect();
        assert_eq!(old, ["critical", "high", "medium", "low", "low"]);
        // No class name claims business impact.
        for c in PROXIMITY.iter().chain(&["test"]) {
            assert!(!["critical", "high", "medium", "low", "risk", "severe"].contains(c), "{c}");
        }
    }

    #[test]
    fn edge_filters_take_groups_and_reference_kinds() {
        let f = EdgeFilter::new(&["calls,http".to_string()]).unwrap();
        assert!(f.allows("calls") && f.allows("via_di") && f.allows("http"));
        assert!(!f.allows("instantiates") && !f.allows("bus"));
        let f = EdgeFilter::new(&["instantiate".to_string(), "type_ref".to_string()]).unwrap();
        assert!(f.allows("instantiates") && f.allows("uses") && !f.allows("calls"));
        assert!(EdgeFilter::default().allows("anything"));
        assert!(EdgeFilter::new(&["callz".to_string()]).is_err());
    }

    #[test]
    fn relations_read_from_the_parent() {
        assert_eq!(relation("calls", "out"), "calls");
        assert_eq!(relation("calls", "in"), "called by");
        assert_eq!(relation("http", "in"), "requested by");
        assert_eq!(relation("bus", "out"), "publishes to");
    }

    fn n(id: i64, kind: &str, depth: u8, parent: Option<usize>) -> TraceNode {
        TraceNode {
            id,
            qualified: format!("S{id}"),
            kind: kind.into(),
            file: "a.cs".into(),
            line: 1,
            depth,
            parent,
            direction: if parent.is_some() { "in" } else { "" },
            edge: if parent.is_some() { "calls" } else { "" },
            confidence: "exact",
            site: None,
            site_repo: None,
            binding: None,
            repo: None,
            http: None,
            bus: None,
            sql: None,
            role: None,
            deploy: None,
            module: None,
            library: None,
            precise: false,
            evidence: None,
            cite: None,
        }
    }

    #[test]
    fn hidden_kinds_lift_their_children() {
        // root <- property <- method <- class
        let nodes = vec![n(0, "method", 0, None), n(1, "property", 1, Some(0)), n(2, "method", 2, Some(1)), n(3, "class", 3, Some(2))];
        let mut t = Trace { nodes, truncated: false, omitted: 0, stopped: Vec::new() };
        assert_eq!(t.retain_kinds(|k| k == "method"), 2);
        let shape: Vec<(i64, u8, Option<usize>)> = t.nodes.iter().map(|n| (n.id, n.depth, n.parent)).collect();
        assert_eq!(shape, [(0, 0, None), (2, 1, Some(0))]);
    }
}
