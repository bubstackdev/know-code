//! A symbol graph across indexed repos, walked in either direction: `knowcode path` (how one symbol reaches another)
//! and `knowcode flow` (what happens end to end from an entry point).
//!
//! A node is a symbol in a repo. Forward, its edges are what `trace -d out` follows (calls and instantiations, a call
//! through a DI-bound service going `via_di` to the implementation) and what runs next across a boundary: a protocol
//! member to its implementations (`dispatch`), an Entity Framework accessor to the DbSet it reads or writes, a DbSet
//! to the SQL table it maps to and a procedure call to the SQL routine (`sql`), a request to the controller action
//! serving it (`http`), and a publish to the consumers of the message (`bus`). Backward, the same edges seen from the
//! other end: callers as `trace -d in` finds them, an action's requesters, a SQL object's users, a consumer's
//! publishers. Each edge carries its confidence, and a crossing's link confidence (an `ambiguous` link makes the edge
//! ambiguous).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rusqlite::OptionalExtension;

use anyhow::{Result, bail};
use kc_index::workspace::RepoSummary;
use kc_store::answer::{Evidence, Incomplete, Mechanism, Reason, Verdict};
use kc_store::db::DbModel;
use kc_store::query::{self, SymbolRow};
use kc_store::trace::{self, BusVia, EdgeCache, HttpVia, SqlVia, Via};
use serde::Serialize;

use crate::graph::Gate;
use crate::ops::{self, HttpCrossing, Opened, Out, Scope, repo_name};

/// A node: the repo (an index into [`Graph`]'s roots) and the symbol's id there.
pub type Key = (usize, i64);

/// What a type runs as a walk's sources (its members, or what building it runs), and the types owning them.
pub type Runs = (Vec<(Key, SymbolRow)>, Vec<String>);

/// An edge as a walk sees it: the symbol at the other end, how the two connect, and where.
#[derive(Clone)]
pub struct Hop {
    pub to: Key,
    pub sym: SymbolRow,
    /// `calls`, `via_di`, `dispatch` (a protocol member to an implementation), `instantiates`, `uses`, `reads`,
    /// `writes` (Entity Framework: an accessor to its DbSet; SQL: a routine to a table), `sql` (a DbSet to the SQL
    /// table it maps to, a procedure call to the SQL routine), `http`, `bus`, `ngrx` (code dispatching an NgRx
    /// action to an effect handling it), `member` (between a type and its members, at the ends of a path).
    pub edge: &'static str,
    pub confidence: &'static str,
    /// `file:line` of the call, request or publish, in the edge's source.
    pub site: Option<String>,
    /// For `via_di`: where the binding is registered, and its lifetime.
    pub binding: Option<String>,
    pub via: Option<Via>,
    /// For `ngrx`: the action dispatched.
    pub action: Option<String>,
    /// What the edge rests on, when the walk decided it itself; else [`Graph::evidence`] finds it.
    pub evidence: Option<Evidence>,
}

/// The cross-repo symbol graph: repos open as walks reach them (at most `max_repos`), with each repo's edge caches.
pub struct Graph<'a> {
    crossing: HttpCrossing<'a>,
    summaries: &'a [RepoSummary],
    sql: Option<kc_index::sql::Sql<'a>>,
    roots: Vec<PathBuf>,
    caches: Vec<EdgeCache>,
    models: HashMap<usize, Rc<DbModel>>,
    /// Whether each repo, or a repo it depends on, declares any client route: only then can its code request one.
    requests: HashMap<usize, bool>,
    /// Whether each repo has NgRx effects.
    effects: HashMap<usize, bool>,
    /// Each repo's NgRx action groups.
    groups: HashMap<usize, Rc<kc_store::ngrx::Groups>>,
    /// Whether each container, by repo, is a protocol.
    protocols: HashMap<(usize, String), bool>,
    /// Each repo's components and the components their templates use.
    children: HashMap<usize, Rc<HashMap<i64, Vec<query::NgChild>>>>,
    /// Nodes whose edges were read, either way.
    pub expanded: usize,
    /// The edges walks follow (`member`, between a type and its members at a path's ends, always), with their
    /// evidence policy, for the query the walk runs for: every edge [`Graph::out`] and [`Graph::into`] read passes it.
    gate: Gate,
}

/// Link confidence as an edge confidence label (see [`crate::graph::across`]): an ambiguous link makes the edge
/// ambiguous; a `dominant` link (one of several handlers or schemas, preferred by a heuristic) is labelled
/// `unique_name`, the nearest of the confidence values. Whether a walk follows it is not decided here: its evidence is
/// `name_dominant` (see [`Graph::evidence`]), and the walk's evidence policy decides (`default` follows it,
/// `confirmed` doesn't).
fn crossed(confidence: &'static str, link: &'static str) -> &'static str {
    crate::graph::across(confidence, link)
}

/// An Entity Framework accessor's edge to its DbSet: a use of the DbSet property, found through the context's type.
fn ef_evidence(confidence: &str) -> Option<Evidence> {
    Evidence::by_confidence(confidence, Mechanism::ReceiverType)
}

fn site(file: &str, line: u32) -> Option<String> {
    Some(format!("{file}:{line}"))
}

impl<'a> Graph<'a> {
    pub fn new(home: &'a Opened, summaries: &'a [RepoSummary], no_sync: bool, max_repos: usize) -> Self {
        Graph {
            crossing: HttpCrossing::new(home, no_sync).limit(max_repos),
            summaries,
            sql: None,
            roots: vec![home.root.clone()],
            caches: vec![EdgeCache::default()],
            models: HashMap::new(),
            requests: HashMap::new(),
            effects: HashMap::new(),
            groups: HashMap::new(),
            protocols: HashMap::new(),
            children: HashMap::new(),
            expanded: 0,
            gate: Gate::default(),
        }
    }

    /// Follows only the edges `filter` allows, crossings included.
    pub fn with_edges(mut self, filter: trace::EdgeFilter) -> Self {
        self.crossing.set_filter(&filter);
        self.gate = Gate::new(filter);
        self
    }

    /// Starts another walk on this graph, following `filter`: the edges read so far stay cached as they were read, before
    /// any gate, and every edge the new walk reads passes its own gate; what a walk reports of itself starts again (see
    /// [`HttpCrossing::begin_walk`]). `task` and `review` walk one change's dependents and its tests this way.
    pub fn begin_walk(&mut self, filter: trace::EdgeFilter) {
        self.crossing.begin_walk(&filter);
        self.gate = Gate::new(filter);
        self.expanded = 0;
    }

    /// The walk's gate: its edges, evidence policy and query.
    pub fn gate(&self) -> &Gate {
        &self.gate
    }

    /// The crossings across repos the walk reads (HTTP, the bus, SQL, deploys, Terraform modules), and the repos open.
    pub(crate) fn crossing(&mut self) -> &mut HttpCrossing<'a> {
        &mut self.crossing
    }

    /// [`HttpCrossing::frontier`] of a walk's stops, with the home repo's edge cache: what the walk resolved is read
    /// once.
    pub(crate) fn frontier(
        &mut self,
        stops: &[kc_store::trace::Stop],
        listed: &dyn Fn(Option<&str>, i64) -> bool,
        budget: usize,
    ) -> Result<(Vec<kc_store::trace::Frontier>, usize)> {
        let Graph { crossing, caches, .. } = self;
        match caches.first_mut() {
            Some(home) => crossing.frontier(home, stops, listed, budget),
            None => crossing.frontier(&mut EdgeCache::default(), stops, listed, budget),
        }
    }

    /// The root of repo `repo`.
    pub fn root(&self, repo: usize) -> &Path {
        &self.roots[repo]
    }

    /// The index of repo `repo`.
    pub fn conn(&self, repo: usize) -> &rusqlite::Connection {
        self.store(repo).conn()
    }

    /// The repo at `root` as the walk numbers it, once the crossing has opened it.
    pub(crate) fn index_of(&mut self, root: &Path) -> usize {
        self.index(root)
    }

    /// The edges `trace` and `impact` read at `sym` (node `k`), in its repo: what it uses (`out`: calls,
    /// instantiations, a call through a DI-bound service to the implementation, a type's conformances), or what
    /// reaches it (`in`: callers, uses, conformers, implementations of a protocol requirement and calls through one,
    /// and with `dispatchers`, code dispatching an NgRx action an effect handles). Each with whether a SCIP index
    /// answered it. Of the kinds the walk allows; its evidence policy is the walk's to apply (see [`Gate::judge`]).
    pub fn tree_edges(&mut self, k: Key, sym: &SymbolRow, dir: &str, dispatchers: bool) -> Result<Vec<(Hop, bool)>> {
        self.gate.expand()?;
        self.expanded += 1;
        let repo = k.0;
        let conn = self.crossing.store(&self.roots[repo]).conn();
        let cache = &mut self.caches[repo];
        let mut edges = if dir == "out" { trace::out_edges(conn, cache, sym)? } else { trace::tree_in_edges(conn, cache, sym)? };
        if dispatchers {
            edges.extend(trace::ngrx_dispatchers(conn, sym)?);
        }
        let mut out = Vec::with_capacity(edges.len());
        for (t, edge, confidence, site) in edges {
            if !self.gate.allows(edge) {
                continue;
            }
            let precise = cache.precise(t.id, site.as_deref());
            let evidence = cache.evidence(t.id, edge, confidence, site.as_deref());
            let binding = if edge == "via_di" { cache.binding(t.id).map(str::to_string) } else { None };
            out.push((Hop { to: (repo, t.id), sym: t, edge, confidence, site, binding, via: None, action: None, evidence }, precise));
        }
        Ok(out)
    }

    /// The home repo's Angular routes.
    pub fn routes(&self) -> Result<Vec<query::NgRoute>> {
        query::ng_route_table(self.store(0).conn())
    }

    pub fn repo_name(&self, repo: usize) -> String {
        repo_name(&self.roots[repo])
    }

    /// The indexes the walk opened, the home repo's first: what it read, for checking its cites.
    pub fn stores(&self) -> Vec<(PathBuf, &kc_store::Store)> {
        self.roots.iter().filter(|r| self.crossing.opened_root(r)).map(|r| (r.clone(), self.crossing.store(r))).collect()
    }

    /// What hop `h` rests on: its crossing's link, else the evidence of the reference it came from.
    pub fn evidence(&self, h: &Hop) -> Option<Evidence> {
        if let Some(via) = &h.via {
            return via.evidence(h.confidence);
        }
        self.caches.get(h.to.0).and_then(|c| c.evidence(h.sym.id, h.edge, h.confidence, h.site.as_deref()))
    }

    /// The other sites in `caller` (of `repo`) reaching `target`, past the one its edge carries (`file:line`).
    pub fn also(&self, repo: usize, caller: i64, target: i64) -> Vec<String> {
        self.caches.get(repo).map(|c| c.also(caller, target).to_vec()).unwrap_or_default()
    }

    /// The walk's edge filter, with its evidence policy.
    pub fn filter(&self) -> &trace::EdgeFilter {
        self.gate.filter()
    }

    /// Keeps the hops the walk's filter allows: of the edge kinds asked for, and not excluded by its evidence policy
    /// (counted there). `k` is the node they were read from, forward (`out`) or backward (`into`).
    fn keep(&self, k: Key, forward: bool, hops: &mut Vec<Hop>) {
        hops.retain(|h| {
            if !self.gate.allows(h.edge) {
                return false;
            }
            if self.gate.filter().policy().is_default() {
                return true;
            }
            let evidence = h.evidence.clone().or_else(|| self.evidence(h));
            let (from, to) = if forward { (k, h.to) } else { (h.to, k) };
            self.gate.judge((from, to, h.edge, &h.site), h.confidence, evidence.as_ref()) != Verdict::Exclude
        });
    }

    /// Repos open, the home repo included.
    pub fn repos_opened(&self) -> usize {
        self.crossing.opened()
    }

    fn store(&self, repo: usize) -> &kc_store::Store {
        self.crossing.store(&self.roots[repo])
    }

    /// The index of a repo the crossing already opened.
    fn index(&mut self, root: &Path) -> usize {
        match self.roots.iter().position(|r| r == root) {
            Some(i) => i,
            None => {
                self.roots.push(root.to_path_buf());
                self.caches.push(EdgeCache::default());
                self.roots.len() - 1
            }
        }
    }

    /// Opens a repo, unless the repo limit is reached (then `None`, and [`Graph::repo_limit`] says so).
    pub fn open(&mut self, root: &Path) -> Result<Option<usize>> {
        if let Some(i) = self.roots.iter().position(|r| r == root) {
            return Ok(Some(i));
        }
        if !root.join(".git").exists() || !self.crossing.open(root)? {
            return Ok(None);
        }
        Ok(Some(self.index(root)))
    }

    /// Whether a crossing was left out because the repo limit was reached.
    pub fn repo_limit(&self) -> bool {
        self.crossing.limited
    }

    /// The definition of `query` `trace` would pick, and its overloads: the definitions of the same name in the same
    /// container and file.
    pub fn overloads(&self, repo: usize, query: &str) -> Result<Vec<SymbolRow>> {
        let all = query::definitions(self.store(repo).conn(), query)?;
        let Some(first) = all.first().cloned() else { return Ok(Vec::new()) };
        Ok(all.into_iter().filter(|d| d.kind == first.kind && d.container == first.container && d.file == first.file).collect())
    }

    /// Every definition `query` names in a repo, of the kind ranking first (see [`query::definitions`]): a path may
    /// start or end at any of them, rather than at one picked silently ([`MAX_ENDS`] at a time).
    pub fn all_named(&self, repo: usize, query: &str) -> Result<Vec<SymbolRow>> {
        let all = query::definitions(self.store(repo).conn(), query)?;
        let Some(first) = all.first().cloned() else { return Ok(Vec::new()) };
        Ok(all.into_iter().filter(|d| d.kind == first.kind).collect())
    }

    /// The type an entry point names other than by its name, in the home repo: a component's or directive's selector
    /// (`app-order-list`, `[appTooltip]`), or a file (a component's template, or a source file's first component or
    /// type).
    pub fn entry_component(&self, entry: &str) -> Result<Option<SymbolRow>> {
        let entry = entry.trim();
        let conn = self.store(0).conn();
        let selector = entry.contains('-') && entry.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        let attribute = entry.starts_with('[') && entry.ends_with(']');
        if selector || attribute {
            return query::selector_entry(conn, entry);
        }
        let file = (entry.contains('/') || entry.contains('.')) && !entry.contains([':', '#', ' ']) && entry.rsplit_once('.').is_some_and(|(_, ext)| {
            !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.chars().any(|c| c.is_ascii_lowercase())
        });
        if file {
            return query::file_entry(conn, entry);
        }
        Ok(None)
    }

    /// The first definition of `query` in a repo, as `trace` picks it.
    pub fn resolve(&self, repo: usize, query: &str) -> Result<Option<SymbolRow>> {
        Ok(query::definitions(self.store(repo).conn(), query)?.into_iter().next())
    }

    pub fn symbol(&self, repo: usize, id: i64) -> Result<Option<SymbolRow>> {
        trace::symbol(self.store(repo).conn(), id)
    }

    /// A type's methods and initializers, as hops from the type (`member`).
    pub fn members(&self, k: Key, ty: &SymbolRow) -> Result<Vec<Hop>> {
        Ok(trace::members(self.store(k.0).conn(), ty)?
            .into_iter()
            .map(|m| Hop { evidence: None, to: (k.0, m.id), sym: m, edge: "member", confidence: "exact", site: None, binding: None, via: None, action: None })
            .collect())
    }

    /// A TypeScript class's fields whose initializers call something, as hops from the class (`member`): they run
    /// when it is made, as its constructor does.
    pub fn initializers(&self, k: Key, ty: &SymbolRow) -> Result<Vec<Hop>> {
        if !kc_store::is_ts_path(&ty.file) {
            return Ok(Vec::new());
        }
        Ok(trace::initializers(self.store(k.0).conn(), ty)?
            .into_iter()
            .map(|m| Hop { evidence: None, to: (k.0, m.id), sym: m, edge: "member", confidence: "exact", site: None, binding: None, via: None, action: None })
            .collect())
    }

    /// A class and the classes it extends that the repo defines, nearest first (at most 8).
    pub fn lineage(&self, k: Key, ty: &SymbolRow) -> Result<Vec<SymbolRow>> {
        let conn = self.store(k.0).conn();
        let mut out = vec![ty.clone()];
        let mut i = 0;
        while i < out.len() && out.len() < 8 {
            let bases: Vec<String> = conn
                .prepare_cached("SELECT target_name FROM relations WHERE from_id = ?1 ORDER BY line")?
                .query_map([out[i].id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for b in bases {
                let base = query::definitions(conn, &b)?.into_iter().find(|d| d.kind == "class" && !kc_store::is_test_path(&d.file));
                if let Some(base) = base.filter(|b| !out.iter().any(|o| o.id == b.id)) {
                    out.push(base);
                }
            }
            i += 1;
        }
        Ok(out)
    }

    /// The type a TypeScript class declares a field `name` with (its annotation, or what initializes it, as
    /// `inject(Service)`), when that is a plain name.
    fn field_type(&self, repo: usize, class: &SymbolRow, name: &str) -> Result<Option<String>> {
        let t: Option<Option<String>> = self
            .store(repo)
            .conn()
            .prepare_cached("SELECT type_name FROM symbols WHERE parent_id = ?1 AND name = ?2 AND name = ?2 COLLATE BINARY AND kind = 'property' LIMIT 1")?
            .query_row(rusqlite::params![class.id, name], |r| r.get(0))
            .optional()?;
        Ok(t.flatten().filter(|t| !t.is_empty() && !t.starts_with(['{', '"']) && t.chars().next().is_some_and(char::is_uppercase)))
    }

    /// A class of the repo named `name` (not a test's), the first as `trace` picks it.
    fn class_named(&self, repo: usize, name: &str) -> Result<Option<SymbolRow>> {
        Ok(query::definitions(self.store(repo).conn(), name)?.into_iter().find(|d| matches!(d.kind.as_str(), "class" | "protocol") && !kc_store::is_test_path(&d.file)))
    }

    /// Calls in `m`, a member that `lineage[0]` runs (its own or inherited), through `this.field` where a class
    /// nearer in the lineage than `m`'s declares the field with another type (`service = inject(EmailCodes)` over a base's
    /// `abstract service: Codes`): the member of that type each reaches instead, by site.
    pub fn overridden(&self, k: Key, m: &SymbolRow, lineage: &[SymbolRow]) -> Result<Vec<Hop>> {
        let Some(at) = lineage.iter().position(|c| Some(c.qualified()) == m.container) else { return Ok(Vec::new()) };
        if at == 0 {
            return Ok(Vec::new());
        }
        let calls: Vec<(String, String, u32, String)> = self
            .store(k.0)
            .conn()
            .prepare_cached(
                "SELECT r.name, r.receiver, r.line, f.path FROM refs r JOIN files f ON f.id = r.file_id
                 WHERE r.from_id = ?1 AND r.kind = 'call' AND r.receiver LIKE 'this.%' ORDER BY r.line",
            )?
            .query_map([m.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let calls_on_this: Vec<(String, u32, String)> = self
            .store(k.0)
            .conn()
            .prepare_cached(
                "SELECT r.name, r.line, f.path FROM refs r JOIN files f ON f.id = r.file_id
                 WHERE r.from_id = ?1 AND r.kind = 'call' AND r.receiver = 'this' ORDER BY r.line",
            )?
            .query_map([m.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut hops = Vec::new();
        // `this.m()` in an inherited member runs the nearest class's `m`: an override in the subclass, say of an
        // abstract method.
        for (name, line, file) in calls_on_this {
            for c in &lineage[..at] {
                let found = query::definitions(self.store(k.0).conn(), &format!("{}.{name}", c.qualified()))?
                    .into_iter()
                    .find(|d| matches!(d.kind.as_str(), "method" | "function") && d.container.as_deref() == Some(c.qualified().as_str()));
                if let Some(s) = found {
                    hops.push(Hop { evidence: Some(Evidence::new(Mechanism::ReceiverType)), to: (k.0, s.id), sym: s, edge: "calls", confidence: "exact", site: site(&file, line), binding: None, via: None, action: None });
                    break;
                }
            }
        }
        for (name, receiver, line, file) in calls {
            let field = receiver.trim_start_matches("this.").trim_start_matches('#');
            if field.is_empty() || !field.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') {
                continue;
            }
            let mut ty = None;
            for c in &lineage[..at] {
                if let Some(t) = self.field_type(k.0, c, field)? {
                    ty = Some(t);
                    break;
                }
            }
            let Some(ty) = ty else { continue };
            let Some(class) = self.class_named(k.0, &ty)? else { continue };
            let mut target = None;
            for t in self.lineage(k, &class)? {
                let found = query::definitions(self.store(k.0).conn(), &format!("{}.{name}", t.qualified()))?
                    .into_iter()
                    .find(|d| matches!(d.kind.as_str(), "method" | "function") && d.container.as_deref() == Some(t.qualified().as_str()));
                if found.is_some() {
                    target = found;
                    break;
                }
            }
            if let Some(s) = target {
                hops.push(Hop { evidence: Some(Evidence::new(Mechanism::ReceiverType)), to: (k.0, s.id), sym: s, edge: "calls", confidence: "exact", site: site(&file, line), binding: None, via: None, action: None });
            }
        }
        Ok(hops)
    }

    /// The services a TypeScript class injects (fields typed or initialized with one: `inject(Service)`, constructor
    /// parameter properties) whose construction runs code, its constructor or field initializers calling something,
    /// as `constructs` hops; an abstract service or token through the one implementation its providers bind. A
    /// `providedIn: 'root'` service's hop carries its registration, `singleton`: the first injector anywhere builds it.
    /// A service whose own construction runs nothing counts when one it injects does, `depth` levels down.
    pub fn injected(&self, k: Key, lineage: &[SymbolRow], depth: u8) -> Result<Vec<Hop>> {
        let conn = self.store(k.0).conn();
        let mut hops: Vec<Hop> = Vec::new();
        for c in lineage {
            let fields: Vec<(String, String, u32, String)> = conn
                .prepare_cached(
                    "SELECT s.name, s.type_name, s.start_line, f.path FROM symbols s JOIN files f ON f.id = s.file_id
                     WHERE s.parent_id = ?1 AND s.kind = 'property' AND s.type_name IS NOT NULL ORDER BY s.start_line",
                )?
                .query_map([c.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (_, ty, line, file) in fields {
                if ty.starts_with(['{', '"']) || !ty.chars().next().is_some_and(char::is_uppercase) {
                    continue;
                }
                let Some(mut svc) = self.class_named(k.0, &ty)? else { continue };
                let abstract_ = svc.kind == "protocol" || format!(" {}", svc.signature).contains(" abstract class ");
                if abstract_ {
                    let bound = kc_store::di::ts_bound(conn, &svc.name, &c.file, Some(&c.name))?;
                    let [one] = bound.as_slice() else { continue };
                    let Some(imp) = one.implementation.as_deref().and_then(|i| self.class_named(k.0, i).ok().flatten()) else { continue };
                    svc = imp;
                }
                if !svc.attrs.contains("Injectable") || hops.iter().any(|h| h.to.1 == svc.id) || lineage.iter().any(|l| l.id == svc.id) {
                    continue;
                }
                let runs = self.construction(k, &svc)?;
                if runs.0.is_empty() && (depth == 0 || self.injected(k, &self.lineage(k, &svc)?, depth - 1)?.is_empty()) {
                    continue;
                }
                let root: Option<(String, u32)> = conn
                    .prepare_cached(
                        "SELECT f.path, d.line FROM di_bindings d JOIN files f ON f.id = d.file_id
                         WHERE d.service_name = ?1 AND d.implementation = ?1 AND d.lifetime = 'singleton' ORDER BY f.path, d.line LIMIT 1",
                    )?
                    .query_row([&svc.name], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                let binding = root.map(|(f, l)| format!("{f}:{l}, singleton"));
                hops.push(Hop { evidence: None, to: (k.0, svc.id), sym: svc, edge: "constructs", confidence: "exact", site: site(&file, line), binding, via: None, action: None });
            }
        }
        hops.retain(|h| self.gate.allows(h.edge));
        Ok(hops)
    }

    /// What building an instance of a TypeScript class runs: its constructor and the field initializers that call
    /// something, its base classes' included, with the classes as owners.
    pub fn construction(&self, k: Key, ty: &SymbolRow) -> Result<Runs> {
        let mut sources = Vec::new();
        let mut owners = Vec::new();
        for t in self.lineage(k, ty)? {
            // The constructor and field initializers (the callbacks they store left out as the walk meets them; see
            // [`Graph::deferred`]).
            let mut runs: Vec<SymbolRow> = self.members(k, &t)?.into_iter().map(|h| h.sym).filter(|m| m.kind == "init").collect();
            runs.extend(self.initializers(k, &t)?.into_iter().map(|h| h.sym));
            runs.sort_by_key(|m| m.line);
            sources.extend(runs.into_iter().map(|m| ((k.0, m.id), m)));
            owners.extend(t.self_type());
        }
        // A constructor only assigning fields calls nothing: it isn't worth a step.
        let conn = self.store(k.0).conn();
        let mut calls = conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM refs WHERE from_id = ?1 AND kind = 'call')")?;
        let mut any = false;
        for (_, s) in &sources {
            any |= calls.query_row([s.id], |r| r.get::<_, bool>(0))?;
        }
        if !any {
            sources.clear();
        }
        Ok((sources, owners))
    }

    /// The values an NgRx dispatch of `action` in `sym` passes (see the TypeScript extractor's `payload` facts):
    /// literals and enum members as written, and a `this.field` as the literal the nearest class of `lineage` (or the
    /// method's own class) initializes it with; and whether some `this.field` had no literal to look up. `None` when
    /// the dispatch passes none.
    pub fn payload_values(&self, k: Key, sym: &SymbolRow, action: &str, lineage: Option<&[SymbolRow]>) -> Result<Option<(Vec<String>, bool)>> {
        let conn = self.store(k.0).conn();
        let rows: Vec<String> = conn
            .prepare_cached("SELECT extra FROM ng_facts WHERE symbol_id = ?1 AND kind = 'payload' AND value = ?2")?
            // A group's action is recorded by its creator's name (`Group.search` → `search`).
            .query_map(rusqlite::params![sym.id, action.rsplit('.').next().unwrap_or(action)], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if rows.is_empty() {
            return Ok(None);
        }
        let own: Vec<SymbolRow> = match lineage {
            Some(l) => l.to_vec(),
            None => match &sym.container {
                Some(c) => self.class_named(k.0, c)?.into_iter().collect(),
                None => Vec::new(),
            },
        };
        let mut out = Vec::new();
        let mut unknown = false;
        for token in rows.iter().flat_map(|r| r.split(' ')) {
            match token.strip_prefix("this.") {
                Some(field) => {
                    let mut found = None;
                    for c in &own {
                        if let Some(v) = self.field_literal(k.0, c, field)? {
                            found = Some(v);
                            break;
                        }
                    }
                    match found {
                        Some(v) => out.push(v),
                        None => unknown = true,
                    }
                }
                None => out.push(token.to_string()),
            }
        }
        Ok(Some((out, unknown)))
    }

    /// The callbacks a TypeScript field's initializer or a method holds rather than runs (see the extractor's
    /// `deferred` facts): `lazy` (an RxJS operator's, run when the observable is subscribed) or `stored` (kept, run when
    /// called), with their lines.
    pub fn deferred(&self, k: Key) -> Result<Vec<(String, u32, u32)>> {
        let rows: Vec<(String, String)> = self
            .store(k.0)
            .conn()
            .prepare_cached("SELECT value, extra FROM ng_facts WHERE symbol_id = ?1 AND kind = 'deferred'")?
            .query_map([k.1], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(v, range)| {
                let (a, b) = range.split_once('-')?;
                Some((v, a.parse().ok()?, b.parse().ok()?))
            })
            .collect())
    }

    /// The literal a class's field is initialized with (`readonly kind: Kind = 'privacy'`, `= Kind.Privacy`), read from
    /// its declaration's line.
    fn field_literal(&self, repo: usize, class: &SymbolRow, name: &str) -> Result<Option<String>> {
        let at: Option<(String, u32)> = self
            .store(repo)
            .conn()
            .prepare_cached(
                "SELECT f.path, s.start_line FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.parent_id = ?1 AND s.name = ?2 AND s.name = ?2 COLLATE BINARY AND s.kind = 'property' LIMIT 1",
            )?
            .query_row(rusqlite::params![class.id, name], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((file, line)) = at else { return Ok(None) };
        let Ok(text) = std::fs::read_to_string(self.roots[repo].join(&file)) else { return Ok(None) };
        let Some(decl) = text.lines().nth(line as usize - 1) else { return Ok(None) };
        let Some((_, value)) = decl.split_once('=') else { return Ok(None) };
        let value = value.trim().trim_end_matches([';', ',']).trim();
        if let Some(s) = value.strip_prefix(['\'', '"', '`']).and_then(|v| v.strip_suffix(['\'', '"', '`'])) {
            return Ok(Some(s.to_string()));
        }
        // `Kind.Privacy`: the member's name, as branches record it.
        Ok(value
            .rsplit_once('.')
            .filter(|(o, m)| o.chars().all(|c| c.is_alphanumeric() || c == '_') && m.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .map(|(_, m)| m.to_string()))
    }

    /// An NgRx effect's branches on its action's values: (value, first line, last line); `*` is a `default`.
    pub fn effect_branches(&self, k: Key) -> Result<Vec<(String, u32, u32)>> {
        let rows: Vec<(String, String)> = self
            .store(k.0)
            .conn()
            .prepare_cached("SELECT value, extra FROM ng_facts WHERE symbol_id = ?1 AND kind = 'branch' ORDER BY line")?
            .query_map([k.1], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(v, range)| {
                let (a, b) = range.split_once('-')?;
                Some((v, a.parse().ok()?, b.parse().ok()?))
            })
            .collect())
    }

    /// The components and directives a component's template uses (`child`), in template order; nothing for other
    /// symbols. Uses resolve by selector among the repo's own components and directives, `exact`; a use none of
    /// them matches, among those of the repos it depends on, else (a custom element) of any indexed repo:
    /// `unique_name` when one repo declares a match, `ambiguous` when several do.
    pub fn ng_children(&mut self, k: Key, ty: &SymbolRow) -> Result<Vec<Hop>> {
        if !query::is_type_or_extension(&ty.kind) || !kc_store::is_ts_path(&ty.file) {
            return Ok(Vec::new());
        }
        let all = match self.children.get(&k.0) {
            Some(c) => c.clone(),
            None => {
                let c = Rc::new(query::ng_children(self.store(k.0).conn())?);
                self.children.insert(k.0, c.clone());
                c
            }
        };
        let mut hops = Vec::new();
        let root = self.roots[k.0].clone();
        let summaries = self.summaries;
        let dependencies = kc_index::workspace::neighbours(&root, summaries).0;
        for c in all.get(&ty.id).into_iter().flatten() {
            let at = site(&c.file, c.line);
            if let Some(id) = c.id {
                let Some(s) = self.symbol(k.0, id)? else { continue };
                hops.push(Hop { evidence: None, to: (k.0, id), sym: s, edge: "child", confidence: "exact", site: at, binding: None, via: None, action: None });
                continue;
            }
            // Another repo's: the libraries it depends on first.
            // A repo whose checkout is gone declares nothing: left out of the count, and missing where it would match.
            let matches = |repos: &mut dyn Iterator<Item = &RepoSummary>| -> Vec<(PathBuf, query::NgSelector)> {
                let mut found = Vec::new();
                for r in repos.filter(|r| r.root != root) {
                    let mut hits = r.selectors.iter().filter(|s| c.used.matched_by(&s.part)).peekable();
                    if r.gone {
                        if hits.peek().is_some() {
                            kc_index::workspace::note_missing(&r.root);
                        }
                        continue;
                    }
                    found.extend(hits.map(|s| (r.root.clone(), s.clone())));
                }
                found
            };
            let mut found = matches(&mut dependencies.iter().copied());
            // Any indexed repo's only for a custom element: an attribute no dependency selects is a plain input.
            if found.is_empty() && c.used.key().0 == "renders" {
                found = matches(&mut summaries.iter());
            }
            let mut repos: Vec<&PathBuf> = found.iter().map(|(r, _)| r).collect();
            repos.dedup();
            let confidence = if repos.len() == 1 { "unique_name" } else { "ambiguous" };
            for (repo, sel) in &found {
                let Some(to) = self.open(repo)? else { continue };
                let Some(s) = self.in_file(to, &sel.component, &sel.file)? else { continue };
                if !hops.iter().any(|h| h.to == (to, s.id)) {
                    hops.push(Hop { evidence: None, to: (to, s.id), sym: s, edge: "child", confidence, site: at.clone(), binding: None, via: None, action: None });
                }
            }
        }
        self.keep(k, true, &mut hops);
        Ok(hops)
    }

    /// Whether the symbol a hop reaches is a protocol (interface) member.
    pub fn is_protocol_member(&mut self, h: &Hop) -> Result<bool> {
        let Some(container) = h.sym.container.clone() else { return Ok(false) };
        let key = (h.to.0, container);
        if let Some(&b) = self.protocols.get(&key) {
            return Ok(b);
        }
        let b = query::definitions(self.store(h.to.0).conn(), &key.1)?.iter().any(|d| d.kind == "protocol");
        self.protocols.insert(key, b);
        Ok(b)
    }

    fn model(&mut self, repo: usize) -> Result<Rc<DbModel>> {
        if let Some(m) = self.models.get(&repo) {
            return Ok(m.clone());
        }
        let m = Rc::new(self.crossing.store(&self.roots[repo]).db_model()?);
        self.models.insert(repo, m.clone());
        Ok(m)
    }

    /// What `sym` (node `k`) leads to.
    pub fn out(&mut self, k: Key, sym: &SymbolRow) -> Result<Vec<Hop>> {
        self.gate.expand()?;
        self.expanded += 1;
        let repo = k.0;
        let mut hops = Vec::new();
        {
            let conn = self.crossing.store(&self.roots[repo]).conn();
            let cache = &mut self.caches[repo];
            for (t, edge, confidence, site) in trace::out_edges(conn, cache, sym)? {
                // A type's conformances aren't something it runs.
                if edge == "conforms_to" {
                    continue;
                }
                let binding = if edge == "via_di" { cache.binding(t.id).map(str::to_string) } else { None };
                hops.push(Hop { evidence: None, to: (repo, t.id), sym: t, edge, confidence, site, binding, via: None, action: None });
            }
            // A protocol member runs as one of its implementations.
            // With test doubles beside one real implementation, the real one.
            let impls = trace::implementations(conn, sym)?;
            let running = if impls.len() > 1 { trace::running_implementations(conn, sym)? } else { impls.iter().map(|s| s.id).collect() };
            for s in impls {
                let confidence = if running == [s.id] { "exact" } else { "ambiguous" };
                hops.push(Hop { evidence: None, to: (repo, s.id), sym: s, edge: "dispatch", confidence, site: None, binding: None, via: None, action: None });
            }
        }
        self.ngrx_out(repo, sym, &mut hops)?;
        self.dialog_out(repo, sym, &mut hops)?;
        self.data_out(repo, sym, &mut hops)?;
        if !query::is_type_or_extension(&sym.kind) {
            self.http_out(repo, sym, &mut hops)?;
            self.bus_out(repo, sym, &mut hops)?;
        }
        self.keep(k, true, &mut hops);
        Ok(hops)
    }

    /// What leads to `sym` (node `k`): each hop's `to` is a predecessor, and its edge runs from there to `sym`.
    pub fn into(&mut self, k: Key, sym: &SymbolRow) -> Result<Vec<Hop>> {
        self.gate.expand()?;
        self.expanded += 1;
        let repo = k.0;
        let mut hops = Vec::new();
        {
            let conn = self.crossing.store(&self.roots[repo]).conn();
            let cache = &mut self.caches[repo];
            for (s, edge, confidence, site) in trace::in_edges(conn, cache, sym)? {
                // Conformers and implementations depend on `sym` without running before it.
                if matches!(edge, "conformer" | "witness") {
                    continue;
                }
                hops.push(Hop { evidence: None, to: (repo, s.id), sym: s, edge, confidence, site, binding: None, via: None, action: None });
            }
        }
        self.ngrx_in(repo, sym, &mut hops)?;
        self.data_in(repo, sym, &mut hops)?;
        self.http_in(repo, sym, &mut hops)?;
        self.bus_in(repo, sym, &mut hops)?;
        self.keep(k, false, &mut hops);
        Ok(hops)
    }

    /// Entity Framework and SQL, forward: an accessor to the DbSet it reads or writes, a DbSet (or an entity mapped
    /// without one) to its SQL table, a procedure call to its SQL routine.
    fn data_out(&mut self, repo: usize, sym: &SymbolRow, hops: &mut Vec<Hop>) -> Result<()> {
        let model = self.model(repo)?;
        if model.is_empty() && model.wrapped.is_empty() {
            return Ok(());
        }
        for t in &model.tables {
            let set = t.dbset_id.or(t.entity_id);
            // A caller listed only for a write through its callee reaches the table through that call.
            for a in t.accessors.iter().filter(|a| a.id == sym.id && !a.only_via) {
                let Some(id) = set else { continue };
                let Some(s) = self.symbol(repo, id)? else { continue };
                let edge = match a.mode.as_str() {
                    "read" => "reads",
                    "write" | "read_write" => "writes",
                    _ => "uses",
                };
                let confidence = ef_confidence(&a.confidence);
                hops.push(Hop { evidence: ef_evidence(confidence), to: (repo, id), sym: s, edge, confidence, site: site(&a.file, a.line), binding: None, via: None, action: None });
            }
            if set == Some(sym.id) {
                let via = SqlVia { object: String::new(), how: "entity", entity: Some(t.entity.clone()), mode: None, link: "", assumptions: Vec::new() };
                self.sql_defs(repo, &t.table, kc_index::sql::TABLE_KINDS, via, site(&t.file, t.line), hops)?;
            }
        }
        // A call through a wrapper naming a procedure (`CreateCommand("schema.proc")`) links only to a routine of that
        // name, as calls do.
        for p in model.procedures.iter().chain(&model.wrapped).filter(|p| p.id == sym.id) {
            let via = SqlVia { object: String::new(), how: "call", entity: None, mode: None, link: "", assumptions: Vec::new() };
            self.sql_defs(repo, &p.name, kc_index::sql::ROUTINE_KINDS, via, site(&p.file, p.line), hops)?;
        }
        Ok(())
    }

    /// The SQL definitions the name `written` (in code of `repo`) links to, as `sql` hops.
    fn sql_defs(&mut self, repo: usize, written: &str, kinds: &[&str], via: SqlVia, at: Option<String>, hops: &mut Vec<Hop>) -> Result<()> {
        let root = self.roots[repo].clone();
        let summaries = self.summaries;
        let links = self.sql.get_or_insert_with(|| kc_index::sql::Sql::new(summaries, Some(&root))).definitions(&root, written, kinds);
        for l in links {
            let Some(to) = self.open(&l.repo)? else { continue };
            let Some(s) = trace::symbol_starting(self.store(to).conn(), &l.def.file, l.def.line, &l.def.kind)? else { continue };
            let via = SqlVia { object: l.def.qualified(), link: l.confidence, assumptions: l.assumptions(), ..via.clone() };
            hops.push(Hop { evidence: None,
                to: (to, s.id),
                sym: s,
                edge: "sql",
                confidence: crossed("exact", l.confidence),
                site: at.clone(),
                binding: None,
                via: Some(Via::Sql(via)),
                action: None,
            });
        }
        Ok(())
    }

    /// Entity Framework and SQL, backward: a DbSet's (or entity's) accessors; a SQL table's DbSets and a SQL
    /// routine's callers, in any indexed repo.
    fn data_in(&mut self, repo: usize, sym: &SymbolRow, hops: &mut Vec<Hop>) -> Result<()> {
        let model = self.model(repo)?;
        for t in model.tables.iter().filter(|t| t.dbset_id.or(t.entity_id) == Some(sym.id)) {
            for a in t.accessors.iter().filter(|a| !a.only_via) {
                let Some(s) = self.symbol(repo, a.id)? else { continue };
                let edge = match a.mode.as_str() {
                    "read" => "reads",
                    "write" | "read_write" => "writes",
                    _ => "uses",
                };
                let confidence = ef_confidence(&a.confidence);
                hops.push(Hop { evidence: ef_evidence(confidence), to: (repo, a.id), sym: s, edge, confidence, site: site(&a.file, a.line), binding: None, via: None, action: None });
            }
        }
        if !(kc_store::sql::is_sql_path(&sym.file) && kc_store::sql::is_object(sym)) {
            return Ok(());
        }
        let root = self.roots[repo].clone();
        for u in self.crossing.sql_uses(&root, sym, self.summaries)? {
            let from = self.index(&u.root);
            let (id, via, at) = match (&u.table, &u.procedure) {
                (Some(t), _) => {
                    let Some(id) = t.dbset_id.or(t.entity_id) else { continue };
                    (id, SqlVia { object: sym.qualified(), how: "entity", entity: Some(t.entity.clone()), mode: None, link: u.confidence, assumptions: u.assumptions.clone() }, site(&t.file, t.line))
                }
                (None, Some(p)) => (p.id, SqlVia { object: sym.qualified(), how: "call", entity: None, mode: None, link: u.confidence, assumptions: u.assumptions.clone() }, site(&p.file, p.line)),
                (None, None) => continue,
            };
            let Some(s) = self.symbol(from, id)? else { continue };
            hops.push(Hop { evidence: None, to: (from, id), sym: s, edge: "sql", confidence: crossed("exact", u.confidence), site: at, binding: None, via: Some(Via::Sql(via)), action: None });
        }
        Ok(())
    }

    /// Whether code in `repo` may request an HTTP route: it, or a repo it depends on, declares a client route.
    fn may_request(&mut self, repo: usize) -> bool {
        if let Some(&b) = self.requests.get(&repo) {
            return b;
        }
        let root = &self.roots[repo];
        let client = |s: &RepoSummary| s.routes.iter().any(|r| matches!(r.side.as_str(), "client" | "operation"));
        let mine = self.summaries.iter().find(|s| &s.root == root).is_some_and(client);
        let b = mine || kc_index::workspace::neighbours(root, self.summaries).0.into_iter().any(client);
        self.requests.insert(repo, b);
        b
    }

    /// HTTP, forward: a method requesting a route (sending a contract it creates, or calling a gateway or an HTTP
    /// client with a path) to the actions serving it.
    fn http_out(&mut self, repo: usize, sym: &SymbolRow, hops: &mut Vec<Hop>) -> Result<()> {
        if !self.may_request(repo) {
            return Ok(());
        }
        let root = self.roots[repo].clone();
        let qualified = sym.qualified();
        let entries = kc_index::http::for_symbol(self.store(repo).conn(), &root, &qualified, sym.id, self.summaries)?;
        for e in entries.into_iter().filter(|e| e.role == "requests") {
            let call = e.symbol == qualified;
            let at = if call { self.request_line(&root, &e.symbol, &sym.file) } else { self.creation_line(repo, sym, &e.symbol)? };
            for l in e.links {
                let Some(to) = self.open(&l.repo)? else { continue };
                // C# creating a contract its own repo serves uses it as a request object in process, not to call
                // itself over HTTP.
                if !call && to == repo && sym.file.ends_with(".cs") {
                    continue;
                }
                // Overloads share a name: the action is the one starting where the route's symbol does.
                let at_route = trace::defined_at(self.store(to).conn(), &l.route.symbol, &l.route.file, l.route.start_line)?;
                let Some(action) = at_route.into_iter().next() else { continue };
                let via = HttpVia { verb: e.verb.clone(), path: e.path.clone(), contract: e.symbol.clone(), call, link: l.confidence, assumptions: l.assumptions.clone() };
                hops.push(Hop { evidence: None,
                    to: (to, action.id),
                    sym: action,
                    edge: "http",
                    confidence: crossed("exact", l.confidence),
                    site: at.clone().or_else(|| site(&sym.file, sym.line)),
                    binding: None,
                    via: Some(Via::Http(via)),
                    action: None,
                });
            }
        }
        Ok(())
    }

    /// Where a method requests a route itself: the client route's line.
    fn request_line(&self, root: &Path, symbol: &str, file: &str) -> Option<String> {
        let s = self.summaries.iter().find(|s| s.root == root)?;
        let r = s.routes.iter().find(|r| matches!(r.side.as_str(), "client" | "operation") && r.symbol == symbol && r.file == file && r.line > 0)?;
        site(&r.file, r.line)
    }

    /// Where `sym` creates the contract `contract`.
    fn creation_line(&self, repo: usize, sym: &SymbolRow, contract: &str) -> Result<Option<String>> {
        let short = contract.rsplit('.').next().unwrap_or(contract);
        let line: Option<u32> = self
            .store(repo)
            .conn()
            .prepare_cached("SELECT MIN(line) FROM refs WHERE from_id = ?1 AND kind = 'instantiate' AND name = ?2")?
            .query_row((sym.id, short), |r| r.get(0))?;
        Ok(line.and_then(|l| site(&sym.file, l)))
    }

    /// HTTP, backward: from a controller action to the code requesting it, in any indexed repo.
    fn http_in(&mut self, repo: usize, sym: &SymbolRow, hops: &mut Vec<Hop>) -> Result<()> {
        if !kc_index::http::serves(self.store(repo).conn(), sym.id)? {
            return Ok(());
        }
        let root = self.roots[repo].clone();
        for l in self.crossing.landings_with(&root, &sym.qualified(), &sym.file, sym.line, self.summaries)? {
            let from = self.index(&l.root);
            for (s, confidence, at) in l.creators {
                hops.push(Hop { evidence: None,
                    to: (from, s.id),
                    sym: s,
                    edge: "http",
                    confidence: crossed(confidence, l.via.link()),
                    site: at,
                    binding: None,
                    via: Some(l.via.clone()),
                    action: None,
                });
            }
        }
        Ok(())
    }

    /// The bus, forward: a method publishing a message to each consumer's `Consume` method (or the consumer class,
    /// without one), in any indexed repo.
    fn bus_out(&mut self, repo: usize, sym: &SymbolRow, hops: &mut Vec<Hop>) -> Result<()> {
        if !matches!(sym.kind.as_str(), "method" | "function" | "init") || !kc_store::bus::involved(self.store(repo).conn(), sym.id)? {
            return Ok(());
        }
        let root = self.roots[repo].clone();
        for (entry, link) in kc_index::bus::consumers(&root, &sym.qualified(), &sym.file, sym.line, self.summaries) {
            let Some(to) = self.open(&link.repo)? else { continue };
            let Some(handler) = self.in_file(to, &link.row.handler(), &link.row.file)?.or(self.in_file(to, &link.row.symbol, &link.row.file)?) else {
                continue;
            };
            let via = BusVia { message: entry.message(), queue: entry.queue(), link: link.confidence, assumptions: link.assumptions.clone() };
            hops.push(Hop { evidence: None,
                to: (to, handler.id),
                sym: handler,
                edge: "bus",
                confidence: crossed("exact", link.confidence),
                site: site(&entry.row.file, entry.row.line),
                binding: None,
                via: Some(Via::Bus(via)),
                action: None,
            });
        }
        Ok(())
    }

    /// The bus, backward: from a consumer (its class or `Consume` method) to the methods publishing what it handles.
    fn bus_in(&mut self, repo: usize, sym: &SymbolRow, hops: &mut Vec<Hop>) -> Result<()> {
        let root = self.roots[repo].clone();
        let qualified = sym.qualified();
        let consumes = self.summaries.iter().find(|s| s.root == root).is_some_and(|s| {
            s.bus.iter().any(|r| r.kind == "consume" && !r.registration() && r.file == sym.file && (r.symbol == qualified || r.handler() == qualified))
        });
        if !consumes {
            return Ok(());
        }
        for e in kc_index::bus::for_symbol(&root, &qualified, &sym.file, sym.line, self.summaries) {
            for l in e.links.iter().filter(|l| l.row.kind == "publish") {
                let Some(from) = self.open(&l.repo)? else { continue };
                let Some(publisher) = self.in_file(from, &l.row.symbol, &l.row.file)? else { continue };
                let via = BusVia { message: e.message(), queue: e.queue(), link: l.confidence, assumptions: l.assumptions.clone() };
                hops.push(Hop { evidence: None,
                    to: (from, publisher.id),
                    sym: publisher,
                    edge: "bus",
                    confidence: crossed("exact", l.confidence),
                    site: site(&l.row.file, l.row.line),
                    binding: None,
                    via: Some(Via::Bus(via)),
                    action: None,
                });
            }
        }
        Ok(())
    }

    /// NgRx, forward: a call to an action creator (a constant made by `createAction`) dispatches the action, and the
    /// effects (`createEffect`) whose `ofType` names it run next. Calls and effects name actions by name, often
    /// through a namespace import (`CartActions.LoadCart()`), so the link is `unique_name` when one action creator in
    /// the repo has that name, else `ambiguous`.
    fn ngrx_out(&mut self, repo: usize, sym: &SymbolRow, hops: &mut Vec<Hop>) -> Result<()> {
        if !self.ngrx(repo)? {
            return Ok(());
        }
        let conn = self.store(repo).conn();
        let dispatched: Vec<(String, u32)> = conn
            .prepare_cached(
                "SELECT r.name, MIN(r.line) FROM refs r WHERE r.from_id = ?1 AND r.kind = 'call'
                 AND EXISTS(SELECT 1 FROM symbols s JOIN refs c ON c.from_id = s.id
                            WHERE s.name = r.name AND c.kind = 'call' AND c.name = 'createAction')
                 GROUP BY r.name ORDER BY MIN(r.line)",
            )?
            .query_map([sym.id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (name, line) in dispatched {
            let confidence = if ngrx_actions(conn, &name)?.len() == 1 { "unique_name" } else { "ambiguous" };
            for e in ngrx_effects(conn, &name)? {
                let Some(s) = trace::symbol(conn, e)? else { continue };
                let action = Some(name.clone());
                hops.push(Hop { evidence: None, to: (repo, e), sym: s, edge: "ngrx", confidence, site: site(&sym.file, line), binding: None, via: None, action });
            }
        }
        // Actions of a group (`AccountActions.search(…)`), handled by the effects whose `ofType` names them.
        let groups = self.groups(repo)?;
        if groups.is_empty() {
            return Ok(());
        }
        let conn = self.store(repo).conn();
        for (action, line) in groups.dispatched(conn, sym.id)? {
            let confidence = groups.confidence(&action);
            for e in kc_store::ngrx::effects(conn, &action)? {
                if e == sym.id {
                    continue;
                }
                let Some(s) = trace::symbol(conn, e)? else { continue };
                let a = Some(action.clone());
                hops.push(Hop { evidence: None, to: (repo, e), sym: s, edge: "ngrx", confidence, site: site(&sym.file, line), binding: None, via: None, action: a });
            }
        }
        Ok(())
    }

    /// The repo's NgRx action groups, loaded once.
    fn groups(&mut self, repo: usize) -> Result<Rc<kc_store::ngrx::Groups>> {
        if let Some(g) = self.groups.get(&repo) {
            return Ok(g.clone());
        }
        let g = Rc::new(kc_store::ngrx::Groups::load(self.store(repo).conn())?);
        self.groups.insert(repo, g.clone());
        Ok(g)
    }

    /// NgRx, backward: from an effect to the code dispatching an action its `ofType` names.
    fn ngrx_in(&mut self, repo: usize, sym: &SymbolRow, hops: &mut Vec<Hop>) -> Result<()> {
        if !self.ngrx(repo)? {
            return Ok(());
        }
        let conn = self.store(repo).conn();
        if !calls(conn, sym.id, "createEffect")? {
            return Ok(());
        }
        let names: Vec<String> = conn
            .prepare_cached("SELECT DISTINCT name FROM refs WHERE from_id = ?1 AND kind IN ('member', 'type_ref') ORDER BY name")?
            .query_map([sym.id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for name in names {
            let actions = ngrx_actions(conn, &name)?;
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
                .collect::<std::result::Result<_, _>>()?;
            for (id, file, line) in callers {
                let Some(caller) = trace::symbol(conn, id)? else { continue };
                let action = Some(name.clone());
                hops.push(Hop { evidence: None, to: (repo, id), sym: caller, edge: "ngrx", confidence, site: site(&file, line), binding: None, via: None, action });
            }
        }
        let groups = self.groups(repo)?;
        let conn = self.store(repo).conn();
        for action in groups.named(conn, sym.id)? {
            let confidence = groups.confidence(&action);
            for (id, file, line) in kc_store::ngrx::dispatchers(conn, &action)? {
                if id == sym.id {
                    continue;
                }
                let Some(caller) = trace::symbol(conn, id)? else { continue };
                let a = Some(action.clone());
                hops.push(Hop { evidence: None, to: (repo, id), sym: caller, edge: "ngrx", confidence, site: site(&file, line), binding: None, via: None, action: a });
            }
        }
        Ok(())
    }

    /// Dialogs, forward: a method opening a component (`dialog.open(EditComponent)`) to that component.
    fn dialog_out(&mut self, repo: usize, sym: &SymbolRow, hops: &mut Vec<Hop>) -> Result<()> {
        if query::is_type_or_extension(&sym.kind) || !kc_store::is_ts_path(&sym.file) {
            return Ok(());
        }
        let conn = self.store(repo).conn();
        for h in query::ng_opened(conn, sym.id)? {
            let confidence = if h.target_ids.len() == 1 { h.confidence } else { "ambiguous" };
            for &id in &h.target_ids {
                let Some(s) = trace::symbol(conn, id)? else { continue };
                hops.push(Hop { evidence: None, to: (repo, id), sym: s, edge: "dialog", confidence, site: site(&h.file, h.line), binding: None, via: None, action: None });
            }
        }
        Ok(())
    }

    /// Whether the repo has NgRx effects at all: the check before looking for actions.
    fn ngrx(&mut self, repo: usize) -> Result<bool> {
        if let Some(&b) = self.effects.get(&repo) {
            return Ok(b);
        }
        let b = self.store(repo).conn().query_row("SELECT EXISTS(SELECT 1 FROM refs WHERE name = 'createEffect' AND kind = 'call')", [], |r| r.get(0))?;
        self.effects.insert(repo, b);
        Ok(b)
    }
    fn in_file(&self, repo: usize, qualified: &str, file: &str) -> Result<Option<SymbolRow>> {
        Ok(query::definitions(self.store(repo).conn(), qualified)?.into_iter().find(|d| d.file == file))
    }
}

/// Whether the symbol `id` calls `name` itself: `createAction` makes an NgRx action creator, `createEffect` an effect.
fn calls(conn: &rusqlite::Connection, id: i64, name: &str) -> Result<bool> {
    let mut stmt = conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM refs WHERE from_id = ?1 AND kind = 'call' AND name = ?2)")?;
    Ok(stmt.query_row((id, name), |r| r.get(0))?)
}

/// The NgRx action creators named `name`.
fn ngrx_actions(conn: &rusqlite::Connection, name: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.id FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.name = ?1
         AND EXISTS(SELECT 1 FROM refs c WHERE c.from_id = s.id AND c.kind = 'call' AND c.name = 'createAction')
         ORDER BY f.path, s.start_byte",
    )?;
    Ok(stmt.query_map([name], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?)
}

/// The NgRx effects naming the action `name` other than by calling it (in `ofType`): those handling it.
fn ngrx_effects(conn: &rusqlite::Connection, name: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.from_id FROM refs r JOIN symbols s ON s.id = r.from_id JOIN files f ON f.id = s.file_id
         WHERE r.name = ?1 AND r.kind IN ('member', 'type_ref')
         AND EXISTS(SELECT 1 FROM refs e WHERE e.from_id = r.from_id AND e.kind = 'call' AND e.name = 'createEffect')
         GROUP BY r.from_id ORDER BY MIN(f.path), MIN(s.start_byte)",
    )?;
    Ok(stmt.query_map([name], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?)
}

/// An Entity Framework accessor's confidence as an edge confidence (`nearest`: a `Set<T>()` several contexts declare).
fn ef_confidence(c: &str) -> &'static str {
    match c {
        "exact" => "exact",
        "unique_name" | "nearest" => "unique_name",
        _ => "ambiguous",
    }
}

/// One symbol of a path or flow, with the edge reaching it from the one before.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub repo: String,
    pub qualified: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    /// How the step before reaches this one (see [`Hop::edge`]); empty for the first.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub edge: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub confidence: &'static str,
    /// `file:line` of the call, request or publish, in the step before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpVia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus: Option<BusVia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<SqlVia>,
    /// For an `ngrx` edge, the action dispatched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ngrx: Option<String>,
    /// What the edge reaching this step rests on (see [`Evidence`]); none on the first step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// In a flow, the calls the step before reaches this one through that the flow hides (logging, mapping or
    /// utility code), outermost first: `OrdersHelper.Save (utility)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub through: Vec<String>,
    /// The lines the step rests on: the edge's site, else its declaration (see [`kc_store::answer::Cite`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cite: Option<kc_store::answer::Cite>,
}

impl Step {
    pub fn at(g: &Graph, repo: usize, s: &SymbolRow) -> Step {
        Step {
            repo: g.repo_name(repo),
            qualified: s.qualified(),
            kind: s.kind.clone(),
            file: s.file.clone(),
            line: s.line,
            edge: "",
            confidence: "",
            site: None,
            binding: None,
            http: None,
            bus: None,
            sql: None,
            ngrx: None,
            evidence: None,
            through: Vec::new(),
            cite: None,
        }
    }

    /// The step `h` reaches, by `h`'s edge.
    pub fn by(g: &Graph, h: &Hop) -> Step {
        let via = h.via.as_ref();
        Step {
            edge: h.edge,
            confidence: h.confidence,
            site: h.site.clone(),
            binding: h.binding.clone(),
            http: via.and_then(Via::http),
            bus: via.and_then(Via::bus),
            sql: via.and_then(Via::sql),
            ngrx: h.action.clone(),
            evidence: {
                let e = h.evidence.clone().or_else(|| g.evidence(h));
                kc_store::answer::debug_check(|| format!("{} to {}", h.edge, h.sym.qualified()), h.confidence, e.as_ref());
                e
            },
            ..Step::at(g, h.to.0, &h.sym)
        }
    }
}

/// Bounds and shape of a path search.
pub struct PathOptions {
    /// Hops a path may have.
    pub max_hops: u8,
    /// Nodes whose edges are read, both directions together.
    pub max_nodes: usize,
    pub max_repos: usize,
    /// Paths to return.
    pub paths: usize,
    /// Search forward only (for comparison with the bidirectional search).
    pub one_way: bool,
    /// Follow `ambiguous` edges too. Off, a path uses only edges that are `exact` or `unique_name` (a `dominant` link
    /// counts as `unique_name`), and with none the answer is "no confident path".
    pub allow_ambiguous: bool,
    /// The edges a path may use.
    pub edges: trace::EdgeFilter,
    /// Definitions of `from` (`to`) passed over before the [`MAX_ENDS`] the search starts (ends) at: a cursor
    /// continuing the candidates searches from the next ones.
    pub from_skip: usize,
    pub to_skip: usize,
}

#[derive(Debug, Serialize)]
pub struct FoundPath {
    pub hops: usize,
    /// The sum of the edges' weights: 2 for an exact edge, 3 for a unique_name one, 10 for an ambiguous one (plus 1 for
    /// a type use or instantiation, or a dominant link); 1 between a type and its member.
    pub weight: u32,
    /// From `from` to `to`, each with the edge reaching it.
    pub steps: Vec<Step>,
}

#[derive(Debug, Serialize)]
pub struct PathAnswer {
    /// Where the best path starts and ends; with none, the first definition of each name.
    pub from: Step,
    pub to: Step,
    /// When a name means several definitions (not overloads of one), each of them: the search started (or ended)
    /// at any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub from_candidates: Vec<Step>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub to_candidates: Vec<Step>,
    /// More definitions have the name than the search starts (ends) at ([`MAX_ENDS`]): a path from (to) the others
    /// isn't looked for.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub from_candidates_truncated: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub to_candidates_truncated: bool,
    pub paths: Vec<FoundPath>,
    /// How many paths were found, when more than the `paths` asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths_total: Option<usize>,
    pub max_hops: u8,
    /// Nodes whose edges were read, forward and backward.
    pub explored: usize,
    pub explored_forward: usize,
    pub explored_backward: usize,
    pub repos_opened: usize,
    /// Whether `ambiguous` edges were followed.
    pub allow_ambiguous: bool,
    /// The node or repo budget stopped the search: a path may exist that it didn't reach.
    pub truncated: bool,
    /// With no path found within the bounds asked: the call looking further (more hops, or ambiguous edges too).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// How long an edge is: confident edges short, ambiguous ones long.
fn weight(h: &Hop) -> u32 {
    if h.edge == "member" {
        return 1;
    }
    let base = match h.confidence {
        "exact" | "" => 2,
        "unique_name" => 3,
        _ => 10,
    };
    let looser = matches!(h.edge, "uses" | "instantiates") || h.via.as_ref().is_some_and(|v| matches!(v.link(), "dominant" | "unique_name"));
    base + u32::from(looser)
}

/// One side's way to a node: its cost and hops from that side's end, and the hop it came by (forward: from the
/// node before, by that node's label of the hops given; backward: to the node after, with the edge running from this
/// node).
struct Label {
    cost: u32,
    hops: u8,
    link: Option<(Key, u8, Rc<Hop>)>,
}

struct Side {
    /// Each node's ways that no other is both cheaper and shorter than (when found): the cheapest way to a node may
    /// take more hops than the path may, while a costlier, shorter one fits.
    labels: HashMap<Key, Vec<Label>>,
    heap: BinaryHeap<Reverse<Queued>>,
    /// Labels expanded, by node and hops.
    done: HashSet<(Key, u8)>,
    /// Nodes expanded at least once.
    nodes: HashSet<Key>,
    expanded: usize,
}

impl Side {
    fn new(ends: &[(Key, Pos)]) -> Side {
        let mut s = Side { labels: HashMap::new(), heap: BinaryHeap::new(), done: HashSet::new(), nodes: HashSet::new(), expanded: 0 };
        for (k, pos) in ends {
            s.labels.insert(*k, vec![Label { cost: 0, hops: 0, link: None }]);
            s.heap.push(Reverse((0, 0, pos.clone(), k.0, k.1)));
        }
        s
    }

    fn top(&self) -> Option<u32> {
        self.heap.peek().map(|Reverse((c, ..))| *c)
    }

    /// `k`'s way of `hops` hops.
    fn label(&self, k: Key, hops: u8) -> Option<&Label> {
        self.labels.get(&k)?.iter().find(|l| l.hops == hops)
    }

    /// Whether another way to `k` is as cheap and as short as `cost` in `hops`, and better in one.
    fn dominated(&self, k: Key, cost: u32, hops: u8) -> bool {
        self.labels.get(&k).is_some_and(|ls| ls.iter().any(|l| l.cost <= cost && l.hops <= hops && (l.cost, l.hops) != (cost, hops)))
    }

    /// Records a way to `k` unless one as cheap and as short is known; says whether it was recorded.
    fn relax(&mut self, k: Key, pos: Pos, cost: u32, hops: u8, link: (Key, u8, Rc<Hop>)) -> bool {
        if self.done.contains(&(k, hops)) || self.labels.get(&k).is_some_and(|ls| ls.iter().any(|l| l.cost <= cost && l.hops <= hops)) {
            return false;
        }
        let ls = self.labels.entry(k).or_default();
        let label = Label { cost, hops, link: Some(link) };
        match ls.iter_mut().find(|l| l.hops == hops) {
            Some(l) => *l = label,
            None => ls.push(label),
        }
        self.heap.push(Reverse((cost, hops, pos, k.0, k.1)));
        true
    }
}

/// Where a symbol is (repo name, file, line, qualified name): what breaks ties between equally cheap ways, so the
/// search explores the same way whatever order the index assigned row ids in.
type Pos = (String, String, u32, String);

/// A way waiting in a search's heap: cost, hops, where the node is (for ties), and the node.
type Queued = (u32, u8, Pos, usize, i64);

fn pos(g: &Graph, repo: usize, s: &SymbolRow) -> Pos {
    (g.repo_name(repo), s.file.clone(), s.line, s.qualified())
}

/// A candidate path: weight, hops, the nodes in order, and the steps.
type Candidate = (u32, usize, Vec<Key>, Vec<Step>);

struct Search<'g, 'a> {
    g: &'g mut Graph<'a>,
    /// The symbols the path may start at (overloads of the name asked for), and end at.
    from: Vec<Key>,
    to: Vec<Key>,
    /// The symbol of every node either side has labelled.
    syms: HashMap<Key, SymbolRow>,
    fwd: Side,
    bwd: Side,
    /// Each node's hops either way, read once however many of its labels are expanded.
    outs: HashMap<Key, Rc<Vec<Hop>>>,
    ins: HashMap<Key, Rc<Vec<Hop>>>,
    found: Vec<Candidate>,
    max_hops: u8,
    allow_ambiguous: bool,
}

impl Search<'_, '_> {
    /// The nodes and steps from `from` to `k`, along forward labels from `k`'s of `hops` hops.
    fn forward_chain(&self, k: Key, hops: u8) -> (Vec<Key>, Vec<Step>) {
        let mut keys = vec![k];
        let mut steps = Vec::new();
        let (mut at, mut h) = (k, hops);
        while let Some((prev, ph, hop)) = self.fwd.label(at, h).and_then(|l| l.link.as_ref()) {
            steps.push(Step::by(self.g, hop));
            keys.push(*prev);
            (at, h) = (*prev, *ph);
        }
        steps.push(Step::at(self.g, at.0, &self.syms[&at]));
        keys.reverse();
        steps.reverse();
        (keys, steps)
    }

    /// Records the path through the edge `hop` from `u` (labelled forward) to `hop.to` (labelled backward): along
    /// `u`'s forward label of `fwd` hops and `hop.to`'s backward one of `bwd`, or when not given, the cheapest pair
    /// within the hop limit.
    fn meet(&mut self, u: Key, fwd: Option<u8>, hop: &Hop, bwd: Option<u8>) {
        let v = hop.to;
        let (Some(fus), Some(bvs)) = (self.fwd.labels.get(&u), self.bwd.labels.get(&v)) else { return };
        let mut best: Option<(u32, u8, u8)> = None;
        for fu in fus.iter().filter(|l| fwd.is_none_or(|h| l.hops == h)) {
            for bv in bvs.iter().filter(|l| bwd.is_none_or(|h| l.hops == h)) {
                if fu.hops as usize + 1 + bv.hops as usize > self.max_hops as usize {
                    continue;
                }
                let cost = fu.cost + weight(hop) + bv.cost;
                if best.is_none_or(|b| cost < b.0) {
                    best = Some((cost, fu.hops, bv.hops));
                }
            }
        }
        let Some((cost, fh, bh)) = best else { return };
        let (mut keys, mut steps) = self.forward_chain(u, fh);
        keys.push(v);
        steps.push(Step::by(self.g, hop));
        let (mut at, mut h) = (v, bh);
        while let Some((next, nh, e)) = self.bwd.label(at, h).and_then(|l| l.link.as_ref()) {
            steps.push(Step::by(self.g, e));
            keys.push(*next);
            (at, h) = (*next, *nh);
        }
        let mut seen = HashSet::new();
        if !keys.iter().all(|k| seen.insert(*k)) || self.found.iter().any(|c| c.2 == keys) {
            return;
        }
        self.found.push((cost, keys.len() - 1, keys, steps));
        self.found.sort_by_key(|c| (c.0, c.1));
    }

    /// Fills in where a `via_di` step's binding is registered when the backward search found it (in-edges don't
    /// carry it): from the step before's own out-edges.
    fn bind(&mut self, keys: &[Key], steps: &mut [Step]) -> Result<()> {
        for i in 1..steps.len() {
            if steps[i].edge != "via_di" || steps[i].binding.is_some() {
                continue;
            }
            let prev = self.syms[&keys[i - 1]].clone();
            if let Some(h) = self.g.out(keys[i - 1], &prev)?.into_iter().find(|h| h.to == keys[i] && h.binding.is_some()) {
                steps[i].binding = h.binding;
            }
        }
        Ok(())
    }

    fn expand_forward(&mut self, k: Key, hops: u8) -> Result<()> {
        let Some(&Label { cost, .. }) = self.fwd.label(k, hops) else { return Ok(()) };
        let sym = self.syms[&k].clone();
        if self.fwd.nodes.insert(k) {
            self.fwd.expanded += 1;
        }
        let out = match self.outs.get(&k) {
            Some(o) => o.clone(),
            None => {
                let mut out = self.g.out(k, &sym)?;
                if self.from.contains(&k) && query::is_type_or_extension(&sym.kind) {
                    out.extend(self.g.members(k, &sym)?);
                }
                let out = Rc::new(out);
                self.outs.insert(k, out.clone());
                out
            }
        };
        for h in out.iter() {
            if !self.allow_ambiguous && h.confidence == "ambiguous" {
                continue;
            }
            self.syms.entry(h.to).or_insert_with(|| h.sym.clone());
            let h = Rc::new(h.clone());
            let pos = pos(self.g, h.to.0, &h.sym);
            self.fwd.relax(h.to, pos, cost + weight(&h), hops + 1, (k, hops, h.clone()));
            if self.bwd.labels.contains_key(&h.to) {
                self.meet(k, Some(hops), &h, None);
            }
        }
        Ok(())
    }

    fn expand_backward(&mut self, k: Key, hops: u8) -> Result<()> {
        let Some(&Label { cost, .. }) = self.bwd.label(k, hops) else { return Ok(()) };
        let sym = self.syms[&k].clone();
        if self.bwd.nodes.insert(k) {
            self.bwd.expanded += 1;
        }
        let into = match self.ins.get(&k) {
            Some(i) => i.clone(),
            None => {
                let mut into = self.g.into(k, &sym)?;
                if self.to.contains(&k) && query::is_type_or_extension(&sym.kind) {
                    // A path to a type may end at one of its members.
                    into.extend(self.g.members(k, &sym)?);
                }
                let into = Rc::new(into);
                self.ins.insert(k, into.clone());
                into
            }
        };
        for h in into.iter() {
            if !self.allow_ambiguous && h.confidence == "ambiguous" {
                continue;
            }
            let p = h.to;
            self.syms.entry(p).or_insert_with(|| h.sym.clone());
            // The edge from `p` to `k`, as a hop reaching `k`.
            // The evidence of the edge as found (keyed by its caller), before it is turned to point forward.
            let edge = Rc::new(Hop { evidence: h.evidence.clone().or_else(|| self.g.evidence(h)), to: k, sym: sym.clone(), ..h.clone() });
            self.bwd.relax(p, pos(self.g, p.0, &h.sym), cost + weight(&edge), hops + 1, (k, hops, edge.clone()));
            if self.fwd.labels.contains_key(&p) {
                self.meet(p, None, &edge, Some(hops));
            }
        }
        Ok(())
    }
}

/// The best few ways `from` reaches `to`: bidirectional best-first search, from `from` over what each symbol leads
/// to and from `to` over what leads to it, expanding whichever side is cheaper (backward edges are name lookups,
/// forward ones a symbol's own references, so the backward frontier counts double), until the cheapest unexplored
/// node on each side together cost at least the paths found.
/// `from` and `to` are the definitions of the names asked for: a path may start at any of the first and end at any of
/// the second.
pub fn search(g: &mut Graph, from: Vec<(Key, SymbolRow)>, to: Vec<(Key, SymbolRow)>, opts: &PathOptions) -> Result<PathAnswer> {
    let max_hops = opts.max_hops.clamp(1, MAX_PATH_HOPS);
    // At most `MAX_PATHS` an answer, unless all are asked for (a cursor continuing them).
    let want = if opts.paths == usize::MAX { usize::MAX } else { opts.paths.clamp(1, MAX_PATHS) };
    let syms: HashMap<Key, SymbolRow> = from.iter().chain(&to).cloned().collect();
    let (starts, ends): (Vec<Key>, Vec<Key>) = (from.iter().map(|f| f.0).collect(), to.iter().map(|t| t.0).collect());
    let placed = |all: &[(Key, SymbolRow)]| -> Vec<(Key, Pos)> { all.iter().map(|(k, s)| (*k, pos(g, k.0, s))).collect() };
    let (fwd, bwd) = (Side::new(&placed(&from)), Side::new(&placed(&to)));
    let mut s = Search {
        fwd,
        bwd,
        from: starts,
        to: ends,
        syms,
        g,
        outs: HashMap::new(),
        ins: HashMap::new(),
        found: Vec::new(),
        max_hops,
        allow_ambiguous: opts.allow_ambiguous || opts.edges.policy().follows_ambiguous(),
    };
    if opts.one_way {
        s.bwd.heap.clear();
    }
    let mut truncated = false;
    loop {
        let (tf, tb) = (s.fwd.top(), s.bwd.top());
        if tf.is_none() && tb.is_none() {
            break;
        }
        // Done when nothing left can beat the paths found, or come within `ALTERNATIVE_SLACK` of the best.
        let bound = tf.unwrap_or(0) + tb.unwrap_or(0);
        if s.found.first().is_some_and(|best| bound >= best.0 + ALTERNATIVE_SLACK || (s.found.len() >= want && bound >= s.found[want - 1].0)) {
            break;
        }
        if s.g.expanded >= opts.max_nodes {
            truncated = true;
            break;
        }
        let forward = match (tf, tb) {
            (Some(_), None) => true,
            (None, _) => false,
            // The smaller frontier (backward counting double), unless one side has done four times the other's work:
            // a frontier can stay small while each of its nodes has hundreds of callers.
            _ if s.bwd.expanded > 4 * s.fwd.expanded + 4 => true,
            _ if s.fwd.expanded > 4 * s.bwd.expanded + 4 => false,
            _ => s.fwd.heap.len() <= 2 * s.bwd.heap.len(),
        };
        let side = if forward { &mut s.fwd } else { &mut s.bwd };
        let Some(Reverse((cost, hops, _, repo, id))) = side.heap.pop() else { continue };
        let k = (repo, id);
        // A way since bettered, or as cheap and as short as another, isn't expanded.
        if side.label(k, hops).is_none_or(|l| l.cost < cost) || side.dominated(k, cost, hops) || !side.done.insert((k, hops)) || hops >= max_hops {
            continue;
        }
        if forward {
            s.expand_forward(k, hops)?;
        } else {
            s.expand_backward(k, hops)?;
        }
    }
    let mut found = std::mem::take(&mut s.found);
    // Confident paths first: one ambiguous edge outranks any number of confident ones.
    found.sort_by_key(|c| (c.3.iter().filter(|s| s.confidence == "ambiguous").count(), c.0, c.1));
    let mut paths = Vec::new();
    let paths_total = (found.len() > want).then_some(found.len());
    for (weight, hops, keys, mut steps) in found.into_iter().take(want) {
        s.bind(&keys, &mut steps)?;
        paths.push(FoundPath { hops, weight, steps });
    }
    let (from_at, to_at) = if paths.is_empty() {
        (0, 0)
    } else {
        let (f, t) = found_keys(&paths, &from, &to);
        let pos = |k: Key, all: &[(Key, SymbolRow)]| all.iter().position(|(x, _)| *x == k).unwrap_or(0);
        (pos(f, &from), pos(t, &to))
    };
    Ok(PathAnswer {
        from: Step::at(s.g, from[from_at].0.0, &from[from_at].1),
        to: Step::at(s.g, to[to_at].0.0, &to[to_at].1),
        from_candidates: candidates(s.g, &from, opts.from_skip > 0),
        to_candidates: candidates(s.g, &to, opts.to_skip > 0),
        from_candidates_truncated: false,
        to_candidates_truncated: false,
        paths,
        paths_total,
        max_hops,
        explored: s.fwd.expanded + s.bwd.expanded,
        explored_forward: s.fwd.expanded,
        explored_backward: s.bwd.expanded,
        repos_opened: s.g.repos_opened(),
        allow_ambiguous: opts.allow_ambiguous || opts.edges.policy().follows_ambiguous(),
        truncated: truncated || s.g.repo_limit(),
        next: None,
    })
}

/// The start and end nodes of the best path, matched back to the definitions asked for by place.
fn found_keys(paths: &[FoundPath], from: &[(Key, SymbolRow)], to: &[(Key, SymbolRow)]) -> (Key, Key) {
    let p = &paths[0];
    let at = |s: &Step, all: &[(Key, SymbolRow)]| {
        all.iter().find(|(_, d)| d.file == s.file && d.line == s.line && d.qualified() == s.qualified).map_or(all[0].0, |(k, _)| *k)
    };
    (at(&p.steps[0], from), at(&p.steps[p.steps.len() - 1], to))
}

/// One step per distinct definition among `ends`, when they are more than one name's overloads (several containers
/// or files); none otherwise.
fn candidates(g: &Graph, ends: &[(Key, SymbolRow)], always: bool) -> Vec<Step> {
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut out = Vec::new();
    for ((repo, _), d) in ends {
        let key = (d.qualified(), d.file.clone());
        if !seen.contains(&key) {
            seen.push(key);
            out.push(Step::at(g, *repo, d));
        }
    }
    if out.len() > 1 || always { out } else { Vec::new() }
}

/// The most definitions of one name a path search starts or ends at.
pub const MAX_ENDS: usize = 16;

/// How much heavier than the best path an alternative may be.
const ALTERNATIVE_SLACK: u32 = 8;

/// The most hops a path may have.
pub const MAX_PATH_HOPS: u8 = 12;

/// Paths a `path` answer lists at most.
pub const MAX_PATHS: usize = 5;

/// The most other repos a path without `--to-repo` ends in when the queried repo doesn't define its end.
pub const MAX_END_REPOS: usize = 8;

/// The definitions of `to` in other indexed repos, for a path whose end the queried repo (`g`'s home) doesn't define:
/// in each repo the workspace summary says may define it and whose index does, as `--to-repo` would pick them. Fails
/// naming the repos when there are none, or more than [`MAX_END_REPOS`].
fn ends_elsewhere(g: &mut Graph, summaries: &[RepoSummary], to: &str, skip: usize) -> Result<(Vec<(Key, SymbolRow)>, bool)> {
    let home = g.roots[0].clone();
    let mut repos = Vec::new();
    for s in summaries.iter().filter(|s| s.root != home && s.may_define(to)) {
        if s.gone {
            // Its index is left, but the checkout it describes is not there to end in.
            kc_index::workspace::note_missing(&s.root);
            continue;
        }
        let Ok(store) = kc_index::open(&s.root) else { continue };
        if !query::definitions(store.conn(), to)?.is_empty() {
            repos.push(s.root.clone());
        }
        if repos.len() > MAX_END_REPOS {
            break;
        }
    }
    let names = |rs: &[PathBuf]| rs.iter().map(|r| repo_name(r)).collect::<Vec<_>>().join(", ");
    if repos.is_empty() {
        bail!("no definition named {to} in {} or any other indexed repo", repo_name(&home));
    }
    if repos.len() > MAX_END_REPOS {
        bail!(
            "no definition named {to} in {}; more than {MAX_END_REPOS} other repos define it ({}, …): pick one with --to-repo",
            repo_name(&home),
            names(&repos[..MAX_END_REPOS])
        );
    }
    let mut ends = Vec::new();
    for r in &repos {
        let Some(i) = g.open(r)? else { continue };
        ends.extend(g.all_named(i, to)?.into_iter().map(|s| ((i, s.id), s)));
    }
    if ends.is_empty() {
        bail!("no definition named {to} in {}; defined in {}, beyond the repo limit: pick one with --to-repo", repo_name(&home), names(&repos));
    }
    ends.drain(..skip.min(ends.len()));
    let cut = ends.len() > MAX_ENDS;
    ends.truncate(MAX_ENDS);
    Ok((ends, cut))
}

/// `knowcode path`: how `from` (in the queried repo) reaches `to` (in `to_repo`; by default the queried repo, else the
/// other indexed repos defining it, at most [`MAX_END_REPOS`]).
pub fn path(from: &str, to: &str, to_repo: Option<&Path>, opts: &PathOptions, args: &Scope) -> Result<Out<PathAnswer>> {
    let _rivals = opts.edges.explain_rivals();
    let roots = ops::target_repos(&args.repos)?;
    let Some(root) = roots.first() else { bail!("no repo to query") };
    // The repo `to` is in is resolved like the queried one, and both are in the workspace the walk crosses.
    let to_root = to_repo.map(|r| ops::target_repos(&[r.to_path_buf()])).transpose()?.map(|mut r| r.remove(0));
    let focus: Vec<PathBuf> = roots.iter().cloned().chain(to_root.clone()).collect();
    let o = ops::open_fresh(root, args.no_sync)?;
    let summaries = ops::full_summaries_for(&o, &focus)?;
    let (mut answer, cite_notes) = {
        let mut g = Graph::new(&o, &summaries, args.no_sync, opts.max_repos).with_edges(opts.edges.clone());
        // The ends past those skipped, at most MAX_ENDS of them, and whether more follow.
        let window = |all: Vec<SymbolRow>, skip: usize| -> (Vec<SymbolRow>, bool) {
            let rest: Vec<SymbolRow> = all.into_iter().skip(skip).collect();
            let cut = rest.len() > MAX_ENDS;
            (rest.into_iter().take(MAX_ENDS).collect(), cut)
        };
        let (a, from_cut) = window(g.all_named(0, from)?, opts.from_skip);
        if a.is_empty() {
            bail!("no definition named {from} in {}", repo_name(root));
        }
        let (b, to_cut): (Vec<(Key, SymbolRow)>, bool) = match &to_root {
            Some(r) => {
                let Some(i) = g.open(r)? else { bail!("can't open {}", r.display()) };
                let (b, cut) = window(g.all_named(i, to)?, opts.to_skip);
                if b.is_empty() {
                    bail!("no definition named {to} in {}", g.repo_name(i));
                }
                (b.into_iter().map(|s| ((i, s.id), s)).collect(), cut)
            }
            None => {
                if g.all_named(0, to)?.is_empty() {
                    ends_elsewhere(&mut g, &summaries, to, opts.to_skip)?
                } else {
                    let (b, cut) = window(g.all_named(0, to)?, opts.to_skip);
                    if b.is_empty() {
                        bail!("no definition named {to} in {} past the first {}", repo_name(root), opts.to_skip);
                    }
                    (b.into_iter().map(|s| ((0, s.id), s)).collect(), cut)
                }
            }
        };
        if a.iter().any(|x| b.iter().any(|(k, _)| (0, x.id) == *k)) {
            bail!("{from} and {to} are the same symbol");
        }
        let a = a.into_iter().map(|s| ((0, s.id), s)).collect();
        let mut answer = search(&mut g, a, b, opts)?;
        answer.from_candidates_truncated = from_cut;
        answer.to_candidates_truncated = to_cut;
        let mut citer = crate::cite::Citer::new([]).with_stores(g.stores());
        citer.path(&mut answer);
        (answer, citer.incomplete())
    };
    // Where the search stopped: its budget (with the call raising it), or its hops with no path found (with the call
    // going further).
    let call = |set: &[(&str, serde_json::Value)]| {
        let mut c = serde_json::json!({ "from": from, "to": to, "repo": repo_name(root), "max_hops": opts.max_hops.min(MAX_PATH_HOPS) });
        if let Some(r) = to_repo {
            c["to_repo"] = repo_name(r).into();
        }
        let policy = ops::policy_word(&opts.edges);
        if !policy.is_empty() {
            c["evidence"] = policy.into();
        }
        for (k, v) in set {
            c[*k] = v.clone();
        }
        format!("kc_path {c}")
    };
    let mut stop = None;
    if answer.truncated {
        let next = call(&[("max_nodes", (opts.max_nodes.saturating_mul(2)).into()), ("max_repos", (opts.max_repos.saturating_mul(2)).into())]);
        stop = Some(
            Incomplete::new(
                Reason::Capped,
                format!("path search: the node budget ({}) or repo limit ({}) stopped it: a path may exist that it didn't reach", opts.max_nodes, opts.max_repos),
            )
            .with_hint(format!("continue: {next}")),
        );
    } else if answer.paths.is_empty() {
        // A complete answer within the bounds asked: the call that looks further.
        let next = if opts.max_hops < MAX_PATH_HOPS {
            Some(call(&[("max_hops", MAX_PATH_HOPS.into())]))
        } else if !opts.allow_ambiguous {
            Some(call(&[("allow_ambiguous", true.into())]))
        } else {
            None
        };
        answer.next = next;
    }
    let items = serde_json::json!([answer]);
    let mut env = ops::envelope(std::slice::from_ref(&o), items, 1, 1);
    ops::report_policy(&mut env, &opts.edges);
    for i in stop.into_iter().chain(cite_notes) {
        ops::push_incomplete(&mut env, i);
    }
    // Asked for longer paths than the search goes: said, so a shorter search never passes for the one asked for.
    if opts.max_hops > MAX_PATH_HOPS {
        ops::push_incomplete(
            &mut env,
            Incomplete::new(Reason::Depth, format!("max_hops: {} asked, {MAX_PATH_HOPS} searched (the longest path searched)", opts.max_hops))
                .with_hint("path from a symbol on the way to search the rest"),
        );
    }
    let req = crate::cursor::Request::Path {
        from: from.to_string(),
        to: to.to_string(),
        to_repo: to_repo.map(Path::to_path_buf),
        max_hops: opts.max_hops,
        max_nodes: opts.max_nodes,
        max_repos: opts.max_repos,
        paths: opts.paths,
        one_way: opts.one_way,
        allow_ambiguous: opts.allow_ambiguous,
        edges: opts.edges.names(),
        from_skip: opts.from_skip,
        to_skip: opts.to_skip,
        evidence: ops::policy_word(&opts.edges),
    };
    use crate::cursor::List;
    let lists = vec![
        List::new("/items/0/paths", opts.paths, answer.paths_total.is_some()),
        List { start: opts.from_skip, ..List::new("/items/0/from_candidates", MAX_ENDS, answer.from_candidates_truncated) },
        List { start: opts.to_skip, ..List::new("/items/0/to_candidates", MAX_ENDS, answer.to_candidates_truncated) },
    ];
    crate::cursor::offer(&mut env, req, args, lists, &[]);
    ops::finish(&mut env);
    Ok(Out { env, data: answer })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ops::tests::{repo, test_cache};

    /// A frontend whose route shows a component dispatching an NgRx action, whose effect calls a service sending an
    /// request contract; an API whose action calls through a DI-bound interface into a service writing an Entity
    /// Framework table and publishing a message; a SQL repo defining the table. `tag` goes into the route, the
    /// namespace and the table, so that tests running at once each link only to their own. Returns (web, api, schema).
    pub(crate) fn shop(base: &Path, tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let t = |s: &str| s.replace('~', tag);
        let web = repo(
            base,
            "web",
            &[
                ("src/Api.ts", &t("export class Api {\n  static readonly orders = 'Orders~';\n}\n")),
                ("src/contracts.ts", "@RequestContract({ endpoint: Api.orders, httpMethod: HttpMethods.post })\nexport class ApiPlaceOrder {}\n"),
                ("src/order.actions.ts", "export const PlaceOrder = createAction('[Orders] Place', props<{ id: number }>());\n"),
                (
                    "src/order.effects.ts",
                    "export class OrderEffects {\n  constructor(private actions$: Actions, private orders: OrderService) {}\n  place$ = createEffect(() => this.actions$.pipe(ofType(PlaceOrder), mergeMap(() => this.orders.place())));\n}\n",
                ),
                ("src/logger.service.ts", "export class LoggerService {\n  info(m: string): void {}\n}\n"),
                (
                    "src/order.service.ts",
                    "export class OrderService {\n  constructor(private http: HttpService, private log: LoggerService) {}\n  place(): void {\n    this.log.info('placing');\n    this.http.send(new ApiPlaceOrder());\n  }\n}\n",
                ),
                (
                    "src/order.page.ts",
                    "@Component({ selector: 'app-order', templateUrl: './order.page.html' })\nexport class OrderPage {\n  constructor(private store: Store) {}\n  submit(): void {\n    this.check();\n  }\n  private check(): void {\n    this.store.dispatch(PlaceOrder({ id: 1 }));\n  }\n}\n",
                ),
                // A lazy routes constant, named like the app's own.
                (
                    "src/app.routes.ts",
                    "export const routes: Routes = [\n  { path: 'orders', loadChildren: () => import('./orders/order.routes').then(m => m.routes) },\n];\n",
                ),
                ("src/orders/order.routes.ts", "export const routes: Routes = [\n  { path: 'new', component: OrderPage },\n];\n"),
            ],
        );
        let api = repo(
            base,
            "api",
            &[
                (
                    "src/OrdersController.cs",
                    &t("public class OrdersController : Controller\n{\n    private readonly IOrderService _svc;\n    [HttpPost(\"Orders~\")]\n    public void Orders() => _svc.Place();\n}\n"),
                ),
                (
                    "src/CheckoutService.cs",
                    &t("namespace Shop.Api~;\npublic interface IOrderService { void Place(); }\npublic class CheckoutService : IOrderService\n{\n    private readonly ShopContext _db;\n    private readonly IShopBus _bus;\n    public void Place()\n    {\n        _db.Orders.Add(new Order());\n        _db.SaveChanges();\n        _bus.Publish(new OrderPlaced());\n    }\n}\n"),
                ),
                ("src/Shop.cs", &t("namespace Shop.Api~;\n[Table(\"orders_~\")]\npublic class Order { }\npublic class ShopContext : DbContext\n{\n    public DbSet<Order> Orders { get; set; }\n}\n")),
                ("src/OrderPlaced.cs", &t("namespace Shop.Api~;\n[ShopMessage(ShopHosts.Orders)]\npublic class OrderPlaced { }\n")),
                (
                    "src/OrderPlacedConsumer.cs",
                    &t("namespace Shop.Api~;\npublic class OrderPlacedConsumer : ShopConsumer<OrderPlaced, OrderPlacedConsumer>\n{\n    public override Task Consume(OrderPlaced message, ConsumeContext<OrderPlaced> context) => null;\n}\n"),
                ),
                (
                    "src/Startup.cs",
                    "public class Startup\n{\n    public void ConfigureServices(IServiceCollection services)\n    {\n        services.AddScoped<IOrderService, CheckoutService>();\n    }\n}\n",
                ),
            ],
        );
        let schema = repo(base, "schema", &[("src/shop.sql", &t("-- Host: db1    Database: shop\nCREATE TABLE `orders_~` (\n  `id` int NOT NULL\n);\n"))]);
        (web, api, schema)
    }

    fn opts(one_way: bool) -> PathOptions {
        PathOptions { max_hops: 8, max_nodes: 2000, max_repos: 16, paths: 3, one_way, allow_ambiguous: false, edges: Default::default(), from_skip: 0, to_skip: 0 }
    }

    /// `(repo, qualified, edge)` of each step of a path.
    fn hops(p: &FoundPath) -> Vec<(String, String, &'static str)> {
        p.steps.iter().map(|s| (s.repo.clone(), s.qualified.clone(), s.edge)).collect()
    }

    #[test]
    fn paths_cross_ngrx_http_di_ef_and_sql_from_either_end() {
        let cache = test_cache();
        let base = cache.dir("path");
        let (web, api, schema) = shop(&base, "p");
        let scope = Scope { repos: vec![web.clone()], no_sync: false };
        let s = |r: &str, q: &str, e: &'static str| (r.to_string(), q.to_string(), e);
        let want = [
            s("web", "OrderPage.submit", ""),
            s("web", "OrderPage.check", "calls"),
            s("web", "OrderEffects.place$", "ngrx"),
            s("web", "OrderService.place", "calls"),
            s("api", "OrdersController.Orders", "http"),
            s("api", "CheckoutService.Place", "via_di"),
            s("api", "ShopContext.Orders", "writes"),
            s("schema", "shop.orders_p", "sql"),
        ];
        let both = path("OrderPage.submit", "orders_p", Some(&schema), &opts(false), &scope).unwrap();
        let a = &both.data;
        assert_eq!(hops(&a.paths[0]), want);
        assert_eq!(a.paths[0].hops, 7);
        let http = a.paths[0].steps[4].http.as_ref().unwrap();
        assert_eq!((http.verb.as_str(), http.contract.as_str(), http.link), ("POST", "ApiPlaceOrder", "exact"));
        assert_eq!(a.paths[0].steps[2].ngrx.as_deref(), Some("PlaceOrder"));
        assert_eq!(a.paths[0].steps[2].site.as_deref(), Some("src/order.page.ts:8"));
        assert_eq!(a.paths[0].steps[5].site.as_deref(), Some("src/OrdersController.cs:5"));
        assert_eq!(both.env["items"][0]["paths"][0]["steps"][7]["sql"]["object"], "shop.orders_p");

        // Forward only finds the same path, reading more nodes to get there.
        let one = path("OrderPage.submit", "orders_p", Some(&schema), &opts(true), &scope).unwrap().data;
        assert_eq!(hops(&one.paths[0]), want);
        assert_eq!(one.explored_backward, 0);

        // A type end: from the component to the consumer the service's publish reaches.
        let a = path("OrderPage", "OrderPlacedConsumer.Consume", Some(&api), &opts(false), &scope).unwrap().data;
        let last: Vec<_> = hops(&a.paths[0]).into_iter().rev().take(2).collect();
        assert_eq!(last, [s("api", "OrderPlacedConsumer.Consume", "bus"), s("api", "CheckoutService.Place", "via_di")]);
        assert_eq!(a.paths[0].steps[1].edge, "member");

        // Nothing runs backward from the API into the frontend.
        let none = path("CheckoutService.Place", "OrderPage.submit", Some(&web), &opts(false), &Scope { repos: vec![api.clone()], no_sync: false })
            .unwrap()
            .data;
        assert!(none.paths.is_empty());
        assert!(none.explored > 0 && !none.truncated);

        // Edge filters: the path needs ngrx, http, via_di, writes and sql besides calls; without http there is none.
        let only = |e: &[&str]| PathOptions { edges: trace::EdgeFilter::new(&e.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(), ..opts(false) };
        let a = path("OrderPage.submit", "orders_p", Some(&schema), &only(&["calls", "ngrx", "http", "writes", "sql"]), &scope).unwrap().data;
        assert_eq!(hops(&a.paths[0]), want);
        let a = path("OrderPage.submit", "orders_p", Some(&schema), &only(&["calls", "ngrx", "writes", "sql"]), &scope).unwrap().data;
        assert!(a.paths.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
    #[test]
    fn a_name_meaning_several_definitions_tries_them_all_and_impact_goes_back_through_ngrx() {
        let cache = test_cache();
        let base = cache.dir("path-many");
        let (web, _api, schema) = shop(&base, "q");
        // Another schema's table of the same name, sorting first by path: never picked silently.
        std::fs::create_dir_all(schema.join("a_archive")).unwrap();
        std::fs::write(schema.join("a_archive/archive.sql"), "-- Host: db1    Database: archive\nCREATE TABLE `orders_q` (\n  `id` int NOT NULL\n);\n").unwrap();
        let scope = Scope { repos: vec![web.clone()], no_sync: false };
        let a = path("OrderPage.submit", "orders_q", Some(&schema), &opts(false), &scope).unwrap().data;
        let ends: Vec<String> = a.to_candidates.iter().map(|s| format!("{} {}", s.qualified, s.file)).collect();
        assert_eq!(ends, ["archive.orders_q a_archive/archive.sql", "shop.orders_q src/shop.sql"]);
        assert_eq!((a.to.qualified.as_str(), a.paths[0].steps.last().unwrap().qualified.as_str()), ("shop.orders_q", "shop.orders_q"));
        assert!(crate::render::path_text(&a).starts_with("to: 2 definitions share the name, all tried: archive.orders_q"));
        // One end picked by place.
        let a = path("OrderPage.submit", "a_archive/archive.sql#orders_q", Some(&schema), &opts(false), &scope).unwrap().data;
        assert!(a.paths.is_empty() && a.to_candidates.is_empty() && a.to.file == "a_archive/archive.sql");

        // Impact on the service the effect calls reaches the component dispatching the effect's action.
        let p = crate::ops::ImpactParams { symbol: Some("OrderService.place".into()), depth: 3, limit: 100, ..Default::default() };
        let out = crate::ops::impact(&p, &scope).unwrap();
        let got: Vec<(u8, String, &str)> = out.data[0].impact.affected.iter().map(|a| (a.hop, a.qualified.clone(), a.edge)).collect();
        assert!(got.contains(&(1, "OrderEffects.place$".to_string(), "calls")), "{got:?}");
        assert!(got.contains(&(2, "OrderPage.check".to_string(), "ngrx")), "{got:?}");
        assert!(got.contains(&(3, "OrderPage.submit".to_string(), "calls")), "{got:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_route_lands_on_the_overload_serving_it() {
        let cache = test_cache();
        let base = cache.dir("overload");
        let web = repo(
            &base,
            "web",
            &[
                (
                    "src/statements.ts",
                    "@RequestContract({ endpoint: 'statements/find', httpMethod: HttpMethods.get })\nexport class ApiFindStatements {}\n",
                ),
                (
                    "src/statement.service.ts",
                    "export class StatementService {\n  constructor(private http: HttpService) {}\n  find(): void {\n    this.http.send(new ApiFindStatements());\n  }\n}\n",
                ),
            ],
        );
        let api = repo(
            &base,
            "api",
            &[(
                "src/InvoicingController.cs",
                "[Route(\"statements\")]\npublic class InvoicingController : Controller\n{\n    private readonly StatementRepository _repo;\n    [HttpGet(\"id/{id}\")]\n    public Task GetById(int id) => _repo.Get(id);\n\n    [HttpGet(\"find\")]\n    public Task GetById(string q, int page) => _repo.Find(q);\n}\n\
public class StatementRepository\n{\n    public Task Get(int id) => null;\n    public Task Find(string q) => null;\n}\n",
            )],
        );
        let scope = Scope { repos: vec![web.clone()], no_sync: false };
        let opts = crate::flow::FlowOptions { depth: 10, max_steps: 80, max_repos: 16, edges: Default::default(), children: crate::flow::DEFAULT_CHILDREN };
        let f = crate::flow::flow("StatementService.find", &opts, &scope).unwrap().data.unwrap();
        let steps: Vec<_> = f.steps.iter().map(|s| (s.step.qualified.as_str(), s.step.edge, s.step.line)).collect();
        assert_eq!(steps.len(), 2, "{steps:?}");
        assert_eq!((steps[0].0, steps[0].1), ("InvoicingController.GetById", "http"));
        assert!(steps[0].2 >= 8, "the `find` overload, not the `id` one: {steps:?}");
        assert_eq!((steps[1].0, steps[1].1), ("StatementRepository.Find", "calls"));
        // Inward from the `id` overload, nothing requests it.
        let a = path("StatementService.find", "StatementRepository.Get", Some(&api), &opts_p(), &scope).unwrap().data;
        assert!(a.paths.is_empty(), "{:?}", a.paths);
        let a = path("StatementService.find", "StatementRepository.Find", Some(&api), &opts_p(), &scope).unwrap().data;
        assert_eq!(a.paths[0].hops, 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    fn opts_p() -> PathOptions {
        opts(false)
    }

    #[test]
    fn a_path_without_to_repo_ends_in_the_other_repos_defining_its_end() {
        let cache = test_cache();
        let base = cache.dir("path-elsewhere");
        let (web, _api, _schema) = shop(&base, "e");
        let scope = Scope { repos: vec![web.clone()], no_sync: false };
        // The table isn't defined in the queried repo: the repo defining it is searched, as `--to-repo` would.
        let a = path("OrderPage.submit", "orders_e", None, &opts(false), &scope).unwrap().data;
        let last = a.paths[0].steps.last().unwrap();
        assert_eq!((last.repo.as_str(), last.qualified.as_str(), last.edge), ("schema", "shop.orders_e", "sql"));
        assert_eq!((a.to.repo.as_str(), a.paths[0].hops), ("schema", 7));
        // Defined nowhere.
        let err = path("OrderPage.submit", "orders_none_e", None, &opts(false), &scope).err().unwrap().to_string();
        assert_eq!(err, "no definition named orders_none_e in web or any other indexed repo");
        // Defined in too many repos to search: they are named instead.
        for i in 0..=MAX_END_REPOS {
            repo(&base, &format!("copy{i}"), &[("src/Many.cs", "public class ManyEndsE { }\n")]);
        }
        let err = path("OrderPage.submit", "ManyEndsE", None, &opts(false), &scope).err().unwrap().to_string();
        assert!(err.starts_with("no definition named ManyEndsE in web; more than 8 other repos define it (copy"), "{err}");
        assert!(err.ends_with("pick one with --to-repo"), "{err}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Every `(repo, qualified)` an answer lists: trace nodes, impact rows, path and flow steps.
    fn listed(v: &serde_json::Value, repo: &str, out: &mut std::collections::BTreeSet<(String, String)>) {
        match v {
            serde_json::Value::Object(o) => {
                let repo = o.get("repo").and_then(|r| r.as_str()).unwrap_or(repo);
                if let Some(q) = o.get("qualified").and_then(|q| q.as_str()) {
                    out.insert((repo.to_string(), q.to_string()));
                }
                for (k, x) in o {
                    if !matches!(k.as_str(), "provenance" | "policy" | "incomplete") {
                        listed(x, repo, out);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| listed(x, repo, out)),
            _ => {}
        }
    }

    /// The default policy is the walks as they were; a stricter policy never lists a symbol a looser one doesn't:
    /// `confirmed` ⊆ `default` ⊆ `all`, for trace, impact, path and flow, across HTTP, DI, NgRx, Entity Framework,
    /// SQL and the bus.
    #[test]
    fn stricter_evidence_policies_never_add_to_a_walk() {
        let cache = test_cache();
        let base = cache.dir("policy-invariant");
        let (web, api, schema) = shop(&base, "inv");
        let scope = |r: &PathBuf| Scope { repos: vec![r.clone()], no_sync: false };
        let filter = |p: &str| crate::ops::WalkFilter { evidence: p.to_string(), ..Default::default() };
        // Each walk's answer under a policy (`None`: none given).
        let answers = |p: Option<&str>| -> Vec<serde_json::Value> {
            let f = filter(p.unwrap_or(""));
            let mut out = Vec::new();
            for (root, symbol) in [(&web, "OrderPage.submit"), (&web, "OrderService.place"), (&api, "CheckoutService.Place"), (&api, "OrdersController.Orders"), (&api, "Order")] {
                out.push(crate::ops::trace(symbol, "both", 3, 500, &f, &scope(root)).unwrap().env);
                let ip = crate::ops::ImpactParams { symbol: Some(symbol.into()), depth: 5, limit: 500, filter: f.clone(), ..Default::default() };
                out.push(crate::ops::impact(&ip, &scope(root)).unwrap().env);
            }
            let edges = crate::ops::edge_filter(&[], p.unwrap_or("")).unwrap();
            let fo = crate::flow::FlowOptions { depth: 10, max_steps: 500, max_repos: 16, edges: edges.clone(), children: crate::flow::DEFAULT_CHILDREN };
            out.push(crate::flow::flow("OrderPage", &fo, &scope(&web)).unwrap().env);
            let po = PathOptions { edges, max_nodes: 5000, ..opts(false) };
            out.push(path("OrderPage.submit", "orders_inv", Some(&schema), &po, &scope(&web)).unwrap().env);
            out
        };
        let strip = |mut v: serde_json::Value| {
            // What differs between two runs of the same walk: when it was answered, and how long the check took.
            if let Some(o) = v.as_object_mut() {
                o.remove("provenance");
                o.remove("freshness");
            }
            v
        };
        let none = answers(None);
        let default = answers(Some("default"));
        assert_eq!(none.into_iter().map(strip).collect::<Vec<_>>(), default.clone().into_iter().map(strip).collect::<Vec<_>>());
        assert!(default.iter().all(|e| e.get("policy").is_none()));
        let confirmed = answers(Some("confirmed"));
        let all = answers(Some("all"));
        let mut grew = false;
        for ((c, d), a) in confirmed.iter().zip(&default).zip(&all) {
            let set = |v: &serde_json::Value| {
                let mut s = std::collections::BTreeSet::new();
                listed(&v["items"], "", &mut s);
                s
            };
            let (c, d, a) = (set(c), set(d), set(a));
            assert!(c.is_subset(&d), "confirmed lists more than default: {:?}", c.difference(&d).collect::<Vec<_>>());
            assert!(d.is_subset(&a), "default lists more than all: {:?}", d.difference(&a).collect::<Vec<_>>());
            grew |= c.len() < d.len();
        }
        // The policy cut something somewhere, and said so.
        assert!(grew);
        assert!(confirmed.iter().any(|e| e["policy"]["excluded"].as_u64() > Some(0) && e["complete"] == false));
        assert!(confirmed.iter().all(|e| e["policy"]["evidence"] == "confirmed"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A path through a convention (NgRx actions by name) or an HTTP route nothing ties to one deployment is there by
    /// default, and gone under a policy refusing it, which says what it left out.
    #[test]
    fn a_policy_refusing_an_edge_says_so() {
        let cache = test_cache();
        let base = cache.dir("policy-path");
        let (web, _api, schema) = shop(&base, "pol");
        let scope = Scope { repos: vec![web.clone()], no_sync: false };
        let with = |p: &str| PathOptions { edges: crate::ops::edge_filter(&[], p).unwrap(), ..opts(false) };
        let d = path("OrderPage.submit", "orders_pol", Some(&schema), &with("default"), &scope).unwrap();
        let http = d.data.paths[0].steps.iter().find(|s| s.edge == "http").unwrap();
        assert_eq!(http.evidence.as_ref().map(|e| e.assumptions.clone()), Some(vec![kc_store::answer::Assumption::SameDeployment]));
        // NgRx actions are linked by name (`name_unique`).
        for (policy, why) in [("confirmed", "name_unique"), ("default,-same_deployment", "same_deployment")] {
            let out = path("OrderPage.submit", "orders_pol", Some(&schema), &with(policy), &scope).unwrap();
            assert!(out.data.paths.is_empty(), "{policy}");
            assert!(out.env["policy"]["excluded_by"][why].as_u64() > Some(0), "{policy}: {}", out.env["policy"]);
            assert_eq!(out.env["complete"], false);
            assert_eq!(out.env["incomplete"][0]["reason"], "policy");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A route two repos serve, linked to the one serving more of the frontend's routes (`dominant`): a name
    /// preference, not a checked route. The default policy follows it as before (`unique_name`, evidence
    /// `name_dominant`); `confirmed` leaves it out and counts it.
    #[test]
    fn a_dominant_route_is_followed_by_default_and_left_out_when_confirmed() {
        let cache = test_cache();
        let base = cache.dir("policy-dominant");
        let web = repo(
            &base,
            "web",
            &[
                (
                    "src/contracts.ts",
                    "@RequestContract({ endpoint: 'domorders', httpMethod: HttpMethods.post })\nexport class ApiDomOrder {}\n@RequestContract({ endpoint: 'domcarts', httpMethod: HttpMethods.get })\nexport class ApiDomCart {}\n",
                ),
                (
                    "src/dom.service.ts",
                    "export class DomService {\n  constructor(private http: HttpService) {}\n  place(): void {\n    this.http.send(new ApiDomOrder());\n  }\n  cart(): void {\n    this.http.send(new ApiDomCart());\n  }\n}\n",
                ),
            ],
        );
        let main = repo(
            &base,
            "main-api",
            &[(
                "src/DomController.cs",
                "public class DomController : Controller\n{\n    [HttpPost(\"domorders\")]\n    public void Orders() { }\n    [HttpGet(\"domcarts\")]\n    public void Carts() { }\n}\n",
            )],
        );
        let _other = repo(&base, "other-api", &[("src/OtherController.cs", "public class OtherController : Controller\n{\n    [HttpPost(\"domorders\")]\n    public void Orders() { }\n}\n")]);
        let scope = Scope { repos: vec![main.clone()], no_sync: false };
        let trace_with = |p: &str| {
            let f = crate::ops::WalkFilter { evidence: p.into(), edges: vec!["http".into()], ..Default::default() };
            crate::ops::trace("DomController.Orders", "in", 1, 50, &f, &scope).unwrap()
        };
        let d = trace_with("default");
        let n = &d.data.found[0].1.nodes;
        let got: Vec<_> = n.iter().skip(1).map(|n| (n.qualified.as_str(), n.confidence, n.http.as_ref().map(|h| h.link), n.evidence.as_ref().map(|e| e.mechanism))).collect();
        assert_eq!(got, [("DomService.place", "unique_name", Some("dominant"), Some(kc_store::answer::Mechanism::NameDominant))]);
        let c = trace_with("confirmed");
        assert_eq!(c.data.found[0].1.nodes.len(), 1);
        assert_eq!(c.env["policy"]["excluded_by"]["name_dominant"], 1, "{}", c.env);
        // Forward, from the service: the path to the main API's action exists by default only.
        let to = |p: &str| {
            let po = PathOptions { edges: crate::ops::edge_filter(&[], p).unwrap(), ..opts(false) };
            path("DomService.place", "DomController.Orders", Some(&main), &po, &Scope { repos: vec![web.clone()], no_sync: false }).unwrap().data.paths.len()
        };
        assert_eq!((to("default"), to("confirmed")), (1, 0));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A walk reads no more edges once its query is cancelled: the next expansion fails as cancelled.
    #[test]
    fn a_cancelled_query_stops_its_walk_at_the_next_expansion() {
        let cache = test_cache();
        let base = cache.dir("cancelled-walk");
        let (web, _api, _schema) = shop(&base, "c");
        let query = kc_store::limits::Operation::new();
        let _q = kc_store::limits::enter(query.clone());
        let o = ops::open_fresh(&web, false).unwrap();
        let summaries = ops::full_summaries_for(&o, std::slice::from_ref(&web)).unwrap();
        let mut g = Graph::new(&o, &summaries, false, 16);
        let sym = g.resolve(0, "OrderService.place").unwrap().unwrap();
        assert!(!g.out((0, sym.id), &sym).unwrap().is_empty());
        query.cancel();
        let err = g.out((0, sym.id), &sym).err().expect("cancelled");
        assert_eq!(err.downcast_ref::<kc_store::limits::Cut>().map(|c| c.reason), Some("cancelled"));
        assert!(Graph::into(&mut g, (0, sym.id), &sym).is_err());
        drop(g);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Every walk's whole answer, JSON and text, under each evidence policy, as a snapshot
    /// (`tests/snapshots/walks.txt`): a refactor of the walks keeps them byte for byte, order included. Set
    /// `KNOWCODE_TEST_BLESS=1` to write the snapshot again after a deliberate change, and review its diff.
    #[test]
    fn walk_answers_match_their_snapshot() {
        let cache = test_cache();
        let base = cache.dir("walk-snapshot");
        let (web, api, schema) = shop(&base, "snap");
        let scope = |r: &PathBuf| Scope { repos: vec![r.clone()], no_sync: false };
        // What differs between two runs of the same walk: when it was answered, how long the check took, the commit
        // and index generation read, and the cursors made from them.
        fn strip(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Object(o) => {
                    for k in ["provenance", "freshness", "knowcode", "commit", "generation", "cursor", "cursors"] {
                        o.remove(k);
                    }
                    // The generations a cursor resumes from.
                    if let Some(r) = o.get_mut("_resume").and_then(|r| r.as_object_mut()) {
                        r.remove("g");
                    }
                    o.values_mut().for_each(strip);
                }
                serde_json::Value::Array(a) => a.iter_mut().for_each(strip),
                _ => {}
            }
        }
        let mut out = String::new();
        let mut put = |title: String, mut env: serde_json::Value, text: String| {
            strip(&mut env);
            out.push_str(&format!("=== {title}\n{}\n--- text\n{text}\n", serde_json::to_string_pretty(&env).unwrap()));
        };
        for policy in ["", "confirmed", "all"] {
            let f = crate::ops::WalkFilter { evidence: policy.to_string(), ..Default::default() };
            for (root, symbol) in [(&web, "OrderPage.submit"), (&web, "OrderService.place"), (&api, "CheckoutService.Place"), (&api, "OrdersController.Orders"), (&api, "Order")] {
                for direction in ["in", "out", "both"] {
                    let t = crate::ops::trace(symbol, direction, 3, 500, &f, &scope(root)).unwrap();
                    let text: String = t.data.found.iter().map(|(r, tr)| crate::render::trace_text(r, tr, t.data.multi)).collect();
                    put(format!("trace {direction} {symbol} [{policy}]"), t.env, text);
                }
                let ip = crate::ops::ImpactParams { symbol: Some(symbol.into()), depth: 5, limit: 500, filter: f.clone(), ..Default::default() };
                let i = crate::ops::impact(&ip, &scope(root)).unwrap();
                let text = crate::render::impact_text(&i.data, i.data.len() > 1);
                put(format!("impact {symbol} [{policy}]"), i.env, text);
            }
            let edges = crate::ops::edge_filter(&[], policy).unwrap();
            for entry in ["OrderPage", "/orders/new/", "OrderService.place"] {
                let fo = crate::flow::FlowOptions { depth: 10, max_steps: 500, max_repos: 16, edges: edges.clone(), children: crate::flow::DEFAULT_CHILDREN };
                let fl = crate::flow::flow(entry, &fo, &scope(&web)).unwrap();
                let text = crate::render::flow_text(entry, fl.data.as_ref());
                put(format!("flow {entry} [{policy}]"), fl.env, text);
            }
            for (from, to, at, one_way) in
                [("OrderPage.submit", "orders_snap", &schema, false), ("OrderPage.submit", "orders_snap", &schema, true), ("OrderPage", "OrderPlacedConsumer.Consume", &api, false)]
            {
                let po = PathOptions { edges: edges.clone(), max_nodes: 5000, ..opts(one_way) };
                let p = path(from, to, Some(at), &po, &scope(&web)).unwrap();
                let text = crate::render::path_text(&p.data);
                put(format!("path {from} -> {to} one_way={one_way} [{policy}]"), p.env, text);
            }
        }
        // A route two repos serve, one preferred by a heuristic (`dominant`), requested by code importing the contract
        // (an exact reference): the confidence and evidence each walk gives the hop across the link.
        let dom_web = repo(
            &base,
            "dom-web",
            &[
                (
                    "src/contracts.ts",
                    "@RequestContract({ endpoint: 'snapdom', httpMethod: HttpMethods.post })\nexport class ApiSnapDom {}\n@RequestContract({ endpoint: 'snapcart', httpMethod: HttpMethods.get })\nexport class ApiSnapCart {}\n",
                ),
                (
                    "src/dom.service.ts",
                    "import { ApiSnapDom, ApiSnapCart } from './contracts';\nexport class DomService {\n  constructor(private http: HttpService) {}\n  place(): void {\n    this.http.send(new ApiSnapDom());\n  }\n  cart(): void {\n    this.http.send(new ApiSnapCart());\n  }\n}\n",
                ),
            ],
        );
        let dom_api = repo(
            &base,
            "dom-api",
            &[("src/DomController.cs", "public class DomController : Controller\n{\n    [HttpPost(\"snapdom\")]\n    public void Orders() { }\n    [HttpGet(\"snapcart\")]\n    public void Carts() { }\n}\n")],
        );
        let _dom_other = repo(&base, "dom-other", &[("src/OtherController.cs", "public class OtherController : Controller\n{\n    [HttpPost(\"snapdom\")]\n    public void Orders() { }\n}\n")]);
        for policy in ["", "confirmed"] {
            let f = crate::ops::WalkFilter { evidence: policy.to_string(), ..Default::default() };
            let t = crate::ops::trace("DomController.Orders", "in", 2, 500, &f, &scope(&dom_api)).unwrap();
            let text: String = t.data.found.iter().map(|(r, tr)| crate::render::trace_text(r, tr, t.data.multi)).collect();
            put(format!("trace in DomController.Orders (dominant route) [{policy}]"), t.env, text);
            let ip = crate::ops::ImpactParams { symbol: Some("DomController.Orders".into()), depth: 3, limit: 500, filter: f.clone(), ..Default::default() };
            let i = crate::ops::impact(&ip, &scope(&dom_api)).unwrap();
            let text = crate::render::impact_text(&i.data, i.data.len() > 1);
            put(format!("impact DomController.Orders (dominant route) [{policy}]"), i.env, text);
            let edges = crate::ops::edge_filter(&[], policy).unwrap();
            let po = PathOptions { edges, max_nodes: 5000, ..opts(false) };
            let p = path("DomService.place", "DomController.Orders", Some(&dom_api), &po, &scope(&dom_web)).unwrap();
            let text = crate::render::path_text(&p.data);
            put(format!("path DomService.place -> DomController.Orders (dominant route) [{policy}]"), p.env, text);
        }
        // Repos are named by their canonical path, which may differ from the base's (`/var` is `/private/var`).
        let canonical = std::fs::canonicalize(&*base).unwrap_or_else(|_| base.to_path_buf());
        let got = out.replace(canonical.to_string_lossy().as_ref(), "<base>").replace(base.to_string_lossy().as_ref(), "<base>");
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/walks.txt");
        if std::env::var_os("KNOWCODE_TEST_BLESS").is_some() {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, &got).unwrap();
        }
        let want = std::fs::read_to_string(&file).unwrap_or_default();
        if got != want {
            let at = got.lines().zip(want.lines()).position(|(a, b)| a != b).unwrap_or(got.lines().count().min(want.lines().count()));
            let around = |s: &str| s.lines().skip(at.saturating_sub(5)).take(12).collect::<Vec<_>>().join("\n");
            panic!("walk answers differ from tests/snapshots/walks.txt at line {}:\n--- got\n{}\n--- want\n{}", at + 1, around(&got), around(&want));
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
