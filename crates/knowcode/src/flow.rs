//! End-to-end flows: what happens when an entry point runs, stitched across repos. An Angular route shows a
//! component, whose methods call services, which request API routes, served by ASP.NET actions, which call services
//! through the DI container, which read and write tables; a publish reaches the consumers of its message.
//!
//! A flow is the forward closure of the entry over the walk's edges ([`crate::walk`]), kept to the spine:
//!
//! - **Followed:** calls to a method of another type; calls through a DI-bound service to the implementation
//!   (`via_di`) and from a protocol member to its one implementation (`dispatch`); HTTP requests to the actions
//!   serving them; publishes to the consumers of the message; NgRx actions dispatched to the effects handling them
//!   (`ngrx`); Entity Framework reads and writes of a DbSet (once per mode), the SQL table it maps to, a stored
//!   procedure call to the SQL routine, and the tables a SQL routine reads and writes; a component a method opens
//!   (`dialog`: `dialog.open(EditComponent)`), and the components and directives a component's template uses
//!   (`child`, as many levels down as [`FlowOptions::children`] says; one leading to no step is left out, counted
//!   under `quiet_child`). A component reached either way runs as the entry does: its members are its flow. The
//!   services a TypeScript component injects whose construction runs code (`constructs`, marked `singleton` for a
//!   `providedIn: 'root'` one); and, through a service several Angular providers in scope bind, each implementation
//!   (an `ambiguous` `via_di`, bound at its provider). An NgRx effect branching on its action's value runs only the
//!   branch the dispatch names; when the dispatch doesn't fix the value, every branch's calls are kept, each marked
//!   as one of the branches (`ambiguous`). Calls an observable's operators make are marked `lazy`.
//! - **Step limit:** shared among the entry's branches (its own calls, each dialog, child or service built): while one
//!   grows, each later one keeps [`BRANCH_RESERVE`] steps for itself.
//! - **Followed through, not listed:** what these lead to counts as the caller's: calls to methods of the caller's
//!   own type (private helpers); calls to free functions (no type); calls to a protocol (interface) member, which
//!   run as the implementation it dispatches to. A component's own methods are its flow's start, the same way, and
//!   in TypeScript its constructor and the fields whose initializers call something (`data = toSignal(…)`).
//! - **Hidden, followed through, and counted:** logging (`logging`: a type, file or method with `Log` as a word of
//!   its name, or `Logger` or `Logging` in it, or a file under a `Logging` directory); mapping (`mapping`: a type or
//!   file ending in `Mapper`, `Mappers`, `Mapping`, `Mappings`, `Profile`, `Converter` or `Formatter`, methods named
//!   `Map`, `MapTo`, `MapFrom`, `ProjectTo` or `Adapt`); utilities (`utility`: a type or file ending in
//!   `Extensions`, `Helper`, `Helpers`, `Util`, `Utils`, `Utility`, `Utilities` or `Constants`). Such a call isn't a
//!   step, but what it leads to is, under the caller, naming the hidden calls it went through (`through`): a name
//!   says what code is for, never that it does nothing worth showing.
//! - **Left out, and counted:** edges that aren't `exact` or `unique_name` (`ambiguous`); test code (`test`); the
//!   call making a request or a publish, at the site of the `http` or `bus` edge (a gateway's `SendAsync`,
//!   `Publish`: `transport`).
//! - **Left out silently:** instantiations, type uses, constructors and properties. Framework and library calls
//!   never resolve to indexed code, so they never show.
//!
//! Each symbol is a step once, where it is first reached (depth first, in source order), unless that step was cut
//! at the depth limit and the symbol is reached again nearer the entry: it is a step there too, and grown from there,
//! so the order calls are read in never hides a step within the depth asked for.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::rc::Rc;

use anyhow::{Result, bail};
use kc_store::query::{self, NgRoute, SymbolRow};
use serde::Serialize;

use crate::ops::{self, Out, Scope};
use crate::walk::{Graph, Hop, Key, Step};

/// How many levels of injected services a component's construction looks through for one whose construction runs
/// code (a component injecting a facade injecting the service that loads at construction).
const INJECTION_DEPTH: u8 = 2;

/// Levels of child components a flow follows unless told otherwise.
pub const DEFAULT_CHILDREN: u8 = 2;

#[derive(Clone)]
pub struct FlowOptions {
    /// Steps deep a branch goes.
    pub depth: u8,
    /// Steps to list at most.
    pub max_steps: usize,
    pub max_repos: usize,
    /// The edges the flow follows.
    pub edges: kc_store::trace::EdgeFilter,
    /// Levels of child components (used in a template) followed below a component; 0 follows none. Dialogs a
    /// component opens are followed regardless.
    pub children: u8,
}

#[derive(Debug, Serialize)]
pub struct FlowStep {
    /// 1-based, in the order the tree is printed.
    pub index: usize,
    /// The step this one is reached from; `None` for those reached from the start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    pub depth: u8,
    #[serde(flatten)]
    pub step: Step,
}

#[derive(Debug, Serialize)]
pub struct Flow {
    pub entry: String,
    /// The route the entry names, when it is an Angular route path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<NgRoute>,
    /// What the flow starts from: a component, an action, a method.
    pub start: Step,
    pub steps: Vec<FlowStep>,
    /// Repos the flow runs through, in order of first step.
    pub repos: Vec<String>,
    /// Edges not followed, by reason (see the module docs).
    pub left_out: BTreeMap<&'static str, usize>,
    /// Calls followed through without being listed as steps, by reason (see the module docs).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hidden: BTreeMap<&'static str, usize>,
    /// `max_steps`, or the repo limit, cut the flow short.
    pub truncated: bool,
    /// Steps at the depth asked for, listed but not followed further: within the bounds asked, so not a cut, but
    /// what a deeper flow would grow.
    #[serde(skip_serializing_if = "is_zero")]
    pub at_depth_limit: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Why a flow leaves out a call to `target`: logging, mapping or utility code, by the name of its type or of its
/// file (a C# extension method's container is the type it extends).
pub fn noise(target: &SymbolRow) -> Option<&'static str> {
    let ty = target.container.as_deref().map_or("", |c| c.rsplit('.').next().unwrap_or(c));
    let stem = target.file.rsplit('/').next().unwrap_or(&target.file).split('.').next().unwrap_or("");
    let named = |suffixes: &[&str]| suffixes.iter().any(|s| ty.ends_with(s) || stem.ends_with(s));
    let name = target.name.as_str();
    let in_logging = target.file.split('/').any(|d| d == "Logging");
    if [ty, stem, name].iter().any(|n| log_word(n) || n.contains("Logger") || n.contains("Logging")) || in_logging {
        return Some("logging");
    }
    const MAPPING: &[&str] = &["Mapper", "Mappers", "Mapping", "Mappings", "Profile", "Converter", "Formatter"];
    if named(MAPPING) || matches!(name, "Map" | "MapTo" | "MapFrom" | "ProjectTo" | "Adapt") {
        return Some("mapping");
    }
    const UTILITY: &[&str] = &["Extensions", "Helper", "Helpers", "Util", "Utils", "Utility", "Utilities", "Constants"];
    if named(UTILITY) {
        return Some("utility");
    }
    None
}

/// Whether `s` has `Log` as a camel-case word: `AuditLog`, `LogProjections`, `LogError`, not `Login` or `Catalog`.
fn log_word(s: &str) -> bool {
    s.match_indices("Log").any(|(i, _)| s[i + 3..].chars().next().is_none_or(|c| !c.is_ascii_lowercase()))
}

enum Class {
    /// A spine step.
    Step,
    /// A same-type helper: followed, not listed.
    Through,
    /// Logging, mapping or utility code: followed, not listed, counted under the reason and named on what it leads to.
    Hide(&'static str),
    /// Not followed; counted under the reason unless it's empty.
    Leave(&'static str),
}

/// How a flow treats `h`, out of code whose own types (its type, and for a component its base classes) are `owners`;
/// `all` when the evidence policy follows `ambiguous` edges.
fn classify(h: &Hop, owners: &[String], protocol: bool, all: bool) -> Class {
    // A service several providers in scope bind (one per route): each implementation is a step, marked ambiguous.
    // So is a call in one of an effect's branches on its payload, when the dispatch doesn't say which runs.
    let one_of_providers = h.binding.as_deref().is_some_and(|b| b.contains(ONE_OF));
    if !matches!(h.confidence, "exact" | "unique_name") && !one_of_providers && !all {
        return Class::Leave("ambiguous");
    }
    if kc_store::is_test_path(&h.sym.file) {
        return Class::Leave("test");
    }
    match h.edge {
        "http" | "bus" | "ngrx" | "sql" | "reads" | "writes" | "uses" | "child" | "dialog" | "constructs" => Class::Step,
        "calls" | "via_di" | "dispatch" => {
            if !matches!(h.sym.kind.as_str(), "method" | "function") {
                return Class::Leave("");
            }
            if let Some(reason) = noise(&h.sym) {
                return Class::Hide(reason);
            }
            // The caller's own helpers, free functions, and protocol members (which run as the implementation
            // they dispatch to) are followed through.
            let own = h.sym.container.as_ref().is_some_and(|c| owners.contains(c));
            if h.edge == "calls" && (own || h.sym.container.is_none() || protocol) {
                return Class::Through;
            }
            Class::Step
        }
        _ => Class::Leave(""),
    }
}

struct Walker<'g, 'a> {
    g: &'g mut Graph<'a>,
    opts: &'g FlowOptions,
    steps: Vec<FlowStep>,
    seen: HashSet<Key>,
    /// The depth each symbol was last listed at as a step.
    listed_at: HashMap<Key, u8>,
    /// Symbols whose step (or one below it) the depth limit cut: reached nearer the entry, they are listed again.
    cut: HashSet<Key>,
    /// Steps the depth limit cut so far.
    cuts: usize,
    /// Data steps listed, by DbSet and mode.
    data: HashSet<(Key, &'static str)>,
    left_out: BTreeMap<&'static str, usize>,
    hidden: BTreeMap<&'static str, usize>,
    truncated: bool,
    /// For the members a class runs (its own and inherited), the class and its bases: calls through `this.field`
    /// take the field's type from the nearest class declaring it.
    lineages: HashMap<Key, Rc<Vec<SymbolRow>>>,
    /// The step limit for the branch being grown (see [`BRANCH_RESERVE`]).
    cap: usize,
    /// For an NgRx effect reached by a dispatch: the values the dispatch passes (see [`Graph::payload_values`]).
    payloads: HashMap<Key, Option<(Vec<String>, bool)>>,
    /// How many `constructs` steps the walk is under: callbacks stored for later don't run then.
    constructing: usize,
}

/// Marks a step one of several that may run (a provider, a branch): followed although ambiguous.
pub const ONE_OF: &str = "one of ";

/// Marks a call an observable's operator makes: it runs when the observable is subscribed.
pub const LAZY_NOTE: &str = "lazy: runs when the observable is subscribed";

/// An effect's out-hops for a dispatch passing `payload`, given the effect's `branches` on its action (value, lines):
/// when a branch's value is among those passed, the calls in other branches (and `default`) don't run; when none is,
/// or what is passed isn't known, every branch's calls are kept, each marked as one of the branches.
fn narrow(out: Vec<Hop>, branches: &[(String, u32, u32)], payload: Option<&(Vec<String>, bool)>) -> Vec<Hop> {
    if branches.is_empty() {
        return out;
    }
    let line = |h: &Hop| h.site.as_deref().and_then(|s| s.rsplit_once(':')).and_then(|(_, l)| l.parse::<u32>().ok());
    let within = |h: &Hop| -> Vec<&(String, u32, u32)> {
        line(h).map_or_else(Vec::new, |l| branches.iter().filter(|(_, a, b)| *a <= l && l <= *b).collect())
    };
    let values: &[String] = payload.map_or(&[], |p| p.0.as_slice());
    let matched = branches.iter().any(|(v, ..)| values.contains(v));
    if matched {
        return out.into_iter().filter(|h| within(h).iter().all(|(v, ..)| values.contains(v))).collect();
    }
    let n = branches.iter().filter(|(v, ..)| v != "*").count().max(1);
    out.into_iter()
        .map(|mut h| {
            if !within(&h).is_empty() {
                h.confidence = "ambiguous";
                h.binding = Some(format!("{ONE_OF}{n} branches on the action's value, which the dispatch doesn't fix"));
            }
            h
        })
        .collect()
}

/// Steps each of an entry's later branches keeps in reserve while an earlier one grows, so that the step limit cuts
/// every branch short rather than leaving the last ones out.
const BRANCH_RESERVE: usize = 3;

impl Walker<'_, '_> {
    /// Adds the steps `sources` lead to (they and the same-type helpers they call), each followed by its own, below
    /// `parent` at `depth`.
    /// `extra` are hops from the sources' owner itself (a component's children); `level` is how many `child` edges
    /// lead here. Says whether the sources lead to any step, listed here or already listed elsewhere.
    fn grow(&mut self, parent: Option<usize>, depth: u8, sources: Vec<(Key, SymbolRow)>, owners: Vec<String>, extra: Vec<Hop>, level: u8) -> Result<bool> {
        // Each hop, with the hidden calls it is reached through.
        let mut hops: Vec<(Hop, Rc<Vec<String>>)> = Vec::new();
        let mut through: HashSet<Key> = sources.iter().map(|(k, _)| *k).collect();
        let none = Rc::new(Vec::new());
        let mut queue: VecDeque<(Key, SymbolRow, Rc<Vec<String>>)> = sources.into_iter().map(|(k, s)| (k, s, none.clone())).collect();
        while let Some((k, sym, via)) = queue.pop_front() {
            let lineage = self.lineages.get(&k).cloned();
            let mut out = self.g.out(k, &sym)?;
            if let Some(l) = &lineage {
                let over = self.g.overridden(k, &sym, l)?;
                if !over.is_empty() {
                    out.retain(|h| !(matches!(h.edge, "calls" | "via_di" | "dispatch") && over.iter().any(|o| o.site == h.site)));
                    out.extend(over);
                }
            }
            // What a dispatch passes, for the effect it reaches; an effect branching on it runs the branch it names.
            for h in out.iter().filter(|h| h.edge == "ngrx") {
                if !self.payloads.contains_key(&h.to)
                    && let Some(a) = &h.action
                {
                    let v = self.g.payload_values(k, &sym, a, lineage.as_deref().map(Vec::as_slice))?;
                    self.payloads.insert(h.to, v);
                }
            }
            if let Some(p) = self.payloads.get(&k).cloned() {
                out = narrow(out, &self.g.effect_branches(k)?, p.as_ref());
            }
            // An observable's operators run when it is subscribed: what they call says so. A callback stored for
            // later doesn't run while the object is built.
            let deferred = self.g.deferred(k)?;
            if !deferred.is_empty() {
                let line = |h: &Hop| h.site.as_deref().and_then(|s| s.rsplit_once(':')).and_then(|(_, l)| l.parse::<u32>().ok());
                let how = |h: &Hop| line(h).and_then(|l| deferred.iter().find(|(_, a, b)| *a <= l && l <= *b)).map(|(v, ..)| v.as_str());
                let constructing = self.constructing > 0;
                out.retain(|h| !(constructing && how(h) == Some("stored")));
                for h in out.iter_mut().filter(|h| h.binding.is_none() && how(h) == Some("lazy")) {
                    h.binding = Some(LAZY_NOTE.to_string());
                }
            }
            for h in out {
                let protocol = h.edge == "calls" && self.g.is_protocol_member(&h)?;
                match classify(&h, &owners, protocol, self.g.filter().policy().follows_ambiguous()) {
                    // Overloads one call may reach show once.
                    Class::Step => {
                        if !hops.iter().any(|(o, _)| o.to.0 == h.to.0 && o.site == h.site && o.sym.qualified() == h.sym.qualified()) {
                            hops.push((h, via.clone()));
                        }
                    }
                    Class::Through => {
                        if through.insert(h.to) {
                            if let Some(l) = &lineage {
                                self.lineages.entry(h.to).or_insert_with(|| l.clone());
                            }
                            queue.push_back((h.to, h.sym, via.clone()));
                        }
                    }
                    Class::Hide(reason) => {
                        *self.hidden.entry(reason).or_default() += 1;
                        if through.insert(h.to) {
                            let mut named = (*via).clone();
                            named.push(format!("{} ({reason})", h.sym.qualified()));
                            queue.push_back((h.to, h.sym, Rc::new(named)));
                        }
                    }
                    Class::Leave("") => {}
                    Class::Leave(reason) => *self.left_out.entry(reason).or_default() += 1,
                }
            }
        }
        for h in extra {
            match classify(&h, &owners, false, self.g.filter().policy().follows_ambiguous()) {
                Class::Step => hops.push((h, none.clone())),
                Class::Leave(reason) if !reason.is_empty() => *self.left_out.entry(reason).or_default() += 1,
                _ => {}
            }
        }
        // The transport call making a request or publishing (a gateway's `SendAsync`, `Publish`) isn't a step: the
        // `http` or `bus` edge from the same site is.
        let wire: HashSet<Option<String>> = hops.iter().filter(|(h, _)| matches!(h.edge, "http" | "bus")).map(|(h, _)| h.site.clone()).collect();
        let before = hops.len();
        hops.retain(|(h, _)| h.edge != "calls" || !wire.contains(&h.site));
        if hops.len() < before {
            *self.left_out.entry("transport").or_default() += before - hops.len();
        }
        // Whether these sources lead to a step: one listed here and kept, or one listed already elsewhere.
        let mut led = false;
        let n = hops.len();
        for (i, (h, via)) in hops.into_iter().enumerate() {
            // A DbSet shows once per mode: read here, written there.
            let data = matches!(h.edge, "reads" | "writes" | "uses");
            // A step listed already shows again only when the depth limit cut it there and it is nearer the entry here.
            let again = self.cut.contains(&h.to) && self.listed_at.get(&h.to).is_some_and(|&d| depth < d);
            if (!data && self.seen.contains(&h.to) && !again) || (data && !self.data.insert((h.to, h.edge))) {
                led |= !matches!(h.edge, "child" | "constructs" | "dialog");
                continue;
            }
            // The entry's branches (own calls, each dialog, child, service built) share the step limit: each keeps
            // room for its first steps before an earlier one goes deep.
            if parent.is_none() {
                self.cap = self.opts.max_steps.saturating_sub(BRANCH_RESERVE * (n - i - 1)).max(self.steps.len() + 1).min(self.opts.max_steps);
            }
            if self.steps.len() >= self.cap {
                self.truncated = true;
                led = true;
                continue;
            }
            self.seen.insert(h.to);
            self.listed_at.insert(h.to, depth);
            self.cut.remove(&h.to);
            let at = self.steps.len();
            let mut step = Step::by(self.g, &h);
            step.through = (*via).clone();
            self.steps.push(FlowStep { index: at + 1, parent: parent.map(|p| p + 1), depth, step });
            if depth + 1 >= self.opts.depth {
                self.cut.insert(h.to);
                self.cuts += 1;
                led = true;
                continue;
            }
            let cuts = self.cuts;
            if h.edge == "constructs" {
                let (sources, owners) = self.g.construction(h.to, &h.sym)?;
                self.seen.extend(sources.iter().map(|(k, _)| *k));
                // And the services it injects in turn are built with it.
                let lineage = self.g.lineage(h.to, &h.sym)?;
                let deps = self.g.injected(h.to, &lineage, INJECTION_DEPTH)?;
                // Building a service that runs nothing of the code base's isn't a step.
                self.constructing += 1;
                let led = self.grow(Some(at), depth + 1, sources, owners, deps, level)?;
                self.constructing -= 1;
                if !led && self.steps.len() == at + 1 {
                    self.steps.pop();
                    *self.left_out.entry("quiet_construct").or_default() += 1;
                }
            } else if matches!(h.edge, "child" | "dialog") {
                let level = level + u8::from(h.edge == "child");
                let (sources, owners, kids) = self.component(h.to, &h.sym, level)?;
                self.seen.extend(sources.iter().map(|(k, _)| *k));
                let led = self.grow(Some(at), depth + 1, sources, owners, kids, level)?;
                // A child leading nowhere is part of the page, not of what it does.
                if h.edge == "child" && !led && self.steps.len() == at + 1 {
                    self.steps.pop();
                    *self.left_out.entry("quiet_child").or_default() += 1;
                }
            } else {
                let owners = h.sym.container.clone().into_iter().collect();
                self.grow(Some(at), depth + 1, vec![(h.to, h.sym)], owners, Vec::new(), level)?;
            }
            if self.cuts > cuts {
                self.cut.insert(h.to);
            }
            // Kept, unless it was a quiet child or construction taken back.
            led |= self.steps.len() > at;
        }
        Ok(led)
    }

    /// What a type runs as a flow's start, or as a component reached from one: its methods, and in TypeScript its
    /// constructor, the fields whose initializers call something and what it inherits from its base classes in the
    /// repo, in source order, with the type (and those bases) as their owners; and, for a component `level` child
    /// edges down, its children while the levels allow.
    #[allow(clippy::type_complexity)]
    fn component(&mut self, k: Key, ty: &SymbolRow, level: u8) -> Result<(Vec<(Key, SymbolRow)>, Vec<String>, Vec<Hop>)> {
        let ts = kc_store::is_ts_path(&ty.file);
        let types = if ts { self.g.lineage(k, ty)? } else { vec![ty.clone()] };
        let mut sources: Vec<(Key, SymbolRow)> = Vec::new();
        let mut owners = Vec::new();
        let lineage = Rc::new(types.clone());
        for t in &types {
            let mut members: Vec<SymbolRow> = self.g.members(k, t)?.into_iter().filter(|h| ts || h.sym.kind != "init").map(|h| h.sym).collect();
            members.extend(self.g.initializers(k, t)?.into_iter().map(|h| h.sym));
            members.sort_by_key(|m| m.line);
            // An override hides what it overrides.
            members.retain(|m| m.kind == "init" || !sources.iter().any(|(_, o)| o.name == m.name && o.kind == m.kind));
            sources.extend(members.into_iter().map(|m| ((k.0, m.id), m)));
            owners.extend(t.self_type());
        }
        if types.len() > 1 {
            for (k, _) in &sources {
                self.lineages.insert(*k, lineage.clone());
            }
        }
        // The services it injects are built with it (listed after its own steps, before its children).
        let mut kids = if ts { self.g.injected(k, &types, INJECTION_DEPTH)? } else { Vec::new() };
        if level < self.opts.children {
            kids.extend(self.g.ng_children(k, ty)?);
        }
        Ok((sources, owners, kids))
    }
}

/// The route `entry` names among the repo's Angular routes: its full path, without leading or trailing `/` or a
/// trailing wildcard (`login/**` is `login`), ignoring case.
fn route_named(g: &Graph, entry: &str) -> Result<Option<NgRoute>> {
    let want = entry.trim().trim_matches('/');
    if want.contains(char::is_whitespace) {
        return Ok(None);
    }
    let bare = |p: &str| p.trim_end_matches("**").trim_matches('/').to_ascii_lowercase();
    Ok(g.routes()?.into_iter().find(|r| bare(&r.path) == bare(want)))
}

/// The flow from `entry` (an Angular route path, a component, an action or another symbol) in the graph's home repo.
pub fn flow_in(g: &mut Graph, entry: &str, opts: &FlowOptions) -> Result<Option<Flow>> {
    let route = if entry.contains('/') || !entry.contains('.') { route_named(g, entry)? } else { None };
    let start = match &route {
        Some(r) => g.resolve(0, &r.component)?,
        None => match g.entry_component(entry)? {
            Some(c) => Some(c),
            None => g.resolve(0, entry)?,
        },
    };
    let Some(start) = start else { return Ok(None) };
    let key = (0, start.id);
    let mut w = Walker {
        g,
        opts,
        steps: Vec::new(),
        seen: HashSet::from([key]),
        listed_at: HashMap::new(),
        cut: HashSet::new(),
        cuts: 0,
        data: HashSet::new(),
        left_out: BTreeMap::new(),
        hidden: BTreeMap::new(),
        truncated: false,
        lineages: HashMap::new(),
        cap: opts.max_steps,
        payloads: HashMap::new(),
        constructing: 0,
    };
    let (sources, owners, kids) = if query::is_type_or_extension(&start.kind) {
        w.component(key, &start, 0)?
    } else {
        // Every overload of the method named (a place names one symbol).
        let overloads = if entry.contains(':') { vec![start.clone()] } else { w.g.overloads(0, entry)? };
        let overloads = if overloads.iter().any(|s| s.id == start.id) { overloads } else { vec![start.clone()] };
        (overloads.into_iter().map(|s| ((0, s.id), s)).collect(), start.container.clone().into_iter().collect(), Vec::new())
    };
    w.seen.extend(sources.iter().map(|(k, _)| *k));
    w.grow(None, 0, sources, owners, kids, 0)?;
    let at_depth_limit = w.cuts;
    let (steps, left_out, hidden, truncated) = (w.steps, w.left_out, w.hidden, w.truncated || g.repo_limit());
    let start = Step::at(g, 0, &start);
    let mut repos = vec![start.repo.clone()];
    for s in &steps {
        if !repos.contains(&s.step.repo) {
            repos.push(s.step.repo.clone());
        }
    }
    Ok(Some(Flow { entry: entry.to_string(), route, start, steps, repos, left_out, hidden, truncated, at_depth_limit }))
}

/// `knowcode flow`: what happens end to end from `entry` in the queried repo.
pub fn flow(entry: &str, opts: &FlowOptions, args: &Scope) -> Result<Out<Option<Flow>>> {
    let roots = ops::target_repos(&args.repos)?;
    let Some(root) = roots.first() else { bail!("no repo to query") };
    let o = ops::open_fresh(root, args.no_sync)?;
    let summaries = ops::full_summaries_for(&o, &roots)?;
    let found = {
        let mut g = Graph::new(&o, &summaries, args.no_sync, opts.max_repos).with_edges(opts.edges.clone());
        flow_in(&mut g, entry, opts)?
    };
    let items = match &found {
        Some(f) => serde_json::json!([f]),
        None => serde_json::json!([]),
    };
    let n = usize::from(found.is_some());
    let mut env = ops::envelope(std::slice::from_ref(&o), items, n, n);
    if found.as_ref().is_some_and(|f| f.truncated) {
        env["truncated"] = serde_json::json!(true);
    }
    ops::report_policy(&mut env, &opts.edges);
    ops::finish(&mut env);
    Ok(Out { env, data: found })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::test_cache;

    fn sym(container: Option<&str>, name: &str, file: &str) -> SymbolRow {
        SymbolRow {
            id: 1,
            name: name.into(),
            kind: "method".into(),
            container: container.map(String::from),
            signature: String::new(),
            doc: None,
            attrs: String::new(),
            tags: String::new(),
            file: file.into(),
            line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 0,
            complexity: 0,
            namespace: None,
        }
    }

    #[test]
    fn noise_is_logging_mapping_and_utilities_by_type_file_or_method_name() {
        let n = |c: Option<&str>, name: &str, file: &str| noise(&sym(c, name, file));
        assert_eq!(n(Some("ILogger"), "Info", "a.cs"), Some("logging"));
        assert_eq!(n(Some("AuditService"), "LogChange", "a.cs"), Some("logging"));
        assert_eq!(n(Some("AuthService"), "Login", "a.cs"), None);
        assert_eq!(n(Some("CatalogService"), "GetLogs", "a.cs"), None);
        assert_eq!(n(Some("AuditLogGuard"), "Guard", "a.cs"), Some("logging"));
        assert_eq!(n(Some("Checkout"), "ToProjection", "src/Logging/Projections.cs"), Some("logging"));
        assert_eq!(n(Some("OrderMapper"), "ToDto", "a.cs"), Some("mapping"));
        assert_eq!(n(Some("ClientMappers"), "ToModel", "a.cs"), Some("mapping"));
        assert_eq!(n(Some("Svc"), "Map", "a.cs"), Some("mapping"));
        assert_eq!(n(Some("StringExtensions"), "Trim2", "a.cs"), Some("utility"));
        // A C# extension method's container is the type it extends; its file says what it is.
        assert_eq!(n(Some("ApiResult"), "ThrowIfError", "src/Extensions/ResultExtensions.cs"), Some("utility"));
        assert_eq!(n(Some("Ns.OrderService"), "Place", "src/OrderService.cs"), None);
    }

    #[test]
    fn a_route_flows_through_ngrx_http_di_ef_sql_and_the_bus() {
        let cache = test_cache();
        let base = cache.dir("flow");
        let (web, _api, _schema) = crate::walk::tests::shop(&base, "f");
        let opts = FlowOptions { depth: 10, max_steps: 80, max_repos: 16, edges: Default::default(), children: DEFAULT_CHILDREN };
        let out = flow("/orders/new/", &opts, &Scope { repos: vec![web.clone()], no_sync: false }).unwrap();
        let f = out.data.unwrap();
        assert_eq!(f.route.as_ref().map(|r| r.component.as_str()), Some("OrderPage"));
        assert_eq!(f.start.qualified, "OrderPage");
        let steps: Vec<_> = f.steps.iter().map(|s| (s.index, s.parent, s.step.repo.as_str(), s.step.qualified.as_str(), s.step.edge)).collect();
        assert_eq!(
            steps,
            [
                // `check` is the component's own: followed through, not listed.
                (1, None, "web", "OrderEffects.place$", "ngrx"),
                (2, Some(1), "web", "OrderService.place", "calls"),
                (3, Some(2), "api", "OrdersController.Orders", "http"),
                (4, Some(3), "api", "CheckoutService.Place", "via_di"),
                (5, Some(4), "api", "ShopContext.Orders", "writes"),
                (6, Some(5), "schema", "shop.orders_f", "sql"),
                (7, Some(4), "api", "OrderPlacedConsumer.Consume", "bus"),
            ]
        );
        assert_eq!(f.repos, ["web", "api", "schema"]);
        // The logger call is followed through, hidden: it leads to no step.
        assert_eq!((f.hidden.get("logging"), f.left_out.get("logging")), (Some(&1), None));
        assert!(!f.truncated);
        // The same flow from the component, and from a method.
        let by_component = flow("OrderPage", &opts, &Scope { repos: vec![web.clone()], no_sync: false }).unwrap().data.unwrap();
        assert_eq!(by_component.steps.len(), 7);
        let from_service = flow("OrderService.place", &opts, &Scope { repos: vec![web.clone()], no_sync: false }).unwrap().data.unwrap();
        assert_eq!(from_service.steps.first().map(|s| s.step.qualified.as_str()), Some("OrdersController.Orders"));
        // Depth bounds each branch.
        let shallow = flow("orders/new", &FlowOptions { depth: 2, ..opts.clone() }, &Scope { repos: vec![web], no_sync: false }).unwrap().data.unwrap();
        assert_eq!(shallow.steps.len(), 2);
        assert!(flow("nowhere/at/all", &opts, &Scope { repos: vec![base.join("web")], no_sync: false }).unwrap().data.is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A page whose template uses a child list, which loads orders through an abstract facade a route provider binds;
    /// the page opens an edit dialog, saving through an `InjectionToken` provided with a factory; a field initializer
    /// and the constructor call services too.
    fn shop_ui(base: &std::path::Path) -> std::path::PathBuf {
        crate::ops::tests::repo(
            base,
            "ui",
            &[
                (
                    "src/app/orders/orders.facade.ts",
                    "export abstract class OrdersFacade {\n  abstract load(): void;\n}\n",
                ),
                (
                    "src/app/orders/http-orders.facade.ts",
                    "@Injectable()\nexport class HttpOrdersFacade extends OrdersFacade {\n  constructor(private api: OrdersApi) { super(); }\n  load(): void {\n    this.api.list();\n  }\n}\n",
                ),
                ("src/app/orders/orders.api.ts", "@Injectable({ providedIn: 'root' })\nexport class OrdersApi {\n  list(): void {}\n  save(): void {}\n  count(): number { return 0; }\n  watch(): void {}\n}\n"),
                (
                    "src/app/orders/orders.routes.ts",
                    "export const routes: Routes = [\n  { path: 'orders', component: OrdersPage, providers: [{ provide: OrdersFacade, useClass: HttpOrdersFacade }] },\n];\n",
                ),
                ("src/app/orders/save.token.ts", "export interface Saver { save(): void; }\nexport const SAVER = new InjectionToken<Saver>('saver');\n"),
                (
                    "src/app/orders/api-saver.ts",
                    "export class ApiSaver implements Saver {\n  private api = inject(OrdersApi);\n  save(): void {\n    this.api.save();\n  }\n}\nexport class OtherSaver implements Saver {\n  save(): void {}\n}\n",
                ),
                (
                    "src/app/app.config.ts",
                    "export const appConfig = { providers: [{ provide: SAVER, useFactory: () => new ApiSaver() }] };\n",
                ),
                (
                    "src/app/orders/order-list.component.ts",
                    "@Component({ selector: 'app-order-list', template: '<li>x</li>' })\nexport class OrderListComponent {\n  private facade = inject(OrdersFacade);\n  ngOnInit(): void {\n    this.facade.load();\n  }\n}\n",
                ),
                ("src/app/orders/badge.component.ts", "@Component({ selector: 'app-badge', template: '<b>x</b>' })\nexport class BadgeComponent {}\n"),
                (
                    "src/app/orders/edit-order.component.ts",
                    "@Component({ selector: 'app-edit-order', template: '<form></form>' })\nexport class EditOrderComponent {\n  private saver = inject(SAVER);\n  submit(): void {\n    this.saver.save();\n  }\n}\n",
                ),
                (
                    "src/app/orders/orders.page.ts",
                    "@Component({ selector: 'app-orders-page', templateUrl: './orders.page.html' })\nexport class OrdersPage {\n  private dialog = inject(MatDialog);\n  private api = inject(OrdersApi);\n  total = computed(() => this.api.count());\n  constructor() {\n    effect(() => this.api.watch());\n  }\n  edit(): void {\n    this.dialog.open(EditOrderComponent, { width: '400px' });\n  }\n}\n",
                ),
                ("src/app/orders/orders.page.html", "<app-order-list></app-order-list>\n<app-badge></app-badge>\n"),
            ],
        )
    }

    #[test]
    fn a_component_flows_into_children_dialogs_providers_and_initializers() {
        let cache = test_cache();
        let base = cache.dir("flow-ng");
        let ui = shop_ui(&base);
        let scope = Scope { repos: vec![ui.clone()], no_sync: false };
        let opts = FlowOptions { depth: 10, max_steps: 80, max_repos: 4, edges: Default::default(), children: DEFAULT_CHILDREN };
        let f = flow("OrdersPage", &opts, &scope).unwrap().data.unwrap();
        let steps: Vec<_> = f.steps.iter().map(|s| (s.parent, s.step.qualified.as_str(), s.step.edge)).collect();
        assert_eq!(
            steps,
            [
                // The field initializer's `computed` and the constructor's `effect`, then the dialog.
                (None, "OrdersApi.count", "calls"),
                (None, "OrdersApi.watch", "calls"),
                (None, "EditOrderComponent", "dialog"),
                // Through the token the factory provides.
                (Some(3), "ApiSaver.save", "via_di"),
                (Some(4), "OrdersApi.save", "calls"),
                // The child the template uses, through the abstract facade the route provides.
                (None, "OrderListComponent", "child"),
                (Some(6), "HttpOrdersFacade.load", "via_di"),
                (Some(7), "OrdersApi.list", "calls"),
            ],
            "{steps:?}"
        );
        assert_eq!(f.steps[2].step.site.as_deref(), Some("src/app/orders/orders.page.ts:10"));
        assert_eq!(f.steps[6].step.binding.as_deref(), Some("src/app/orders/orders.routes.ts:2, default"));
        // The badge leads nowhere: left out, and counted.
        assert_eq!(f.left_out.get("quiet_child"), Some(&1));
        // No children asked for; dialogs still followed.
        let flat = flow("OrdersPage", &FlowOptions { children: 0, ..opts.clone() }, &scope).unwrap().data.unwrap();
        assert!(flat.steps.iter().all(|s| s.step.edge != "child") && flat.steps.iter().any(|s| s.step.edge == "dialog"));
        // The same start by selector, by file, by template and by place.
        for entry in ["app-orders-page", "src/app/orders/orders.page.ts", "orders/orders.page.html", "src/app/orders/orders.page.ts:2"] {
            let g = flow(entry, &opts, &scope).unwrap().data.unwrap_or_else(|| panic!("{entry}"));
            assert_eq!((g.start.qualified.as_str(), g.steps.len()), ("OrdersPage", 8), "{entry}");
        }
        let at = flow("src/app/orders/orders.page.ts:10", &opts, &scope).unwrap().data.unwrap();
        assert_eq!(at.start.qualified, "OrdersPage.edit");
        assert_eq!(at.steps[0].step.qualified, "EditOrderComponent");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn angular_providers_bind_by_scope_and_an_abstract_class_by_its_one_injectable_subclass() {
        let cache = test_cache();
        let base = cache.dir("flow-di");
        let ui = crate::ops::tests::repo(
            &base,
            "ui",
            &[
                ("src/app/prices.ts", "export abstract class Prices {\n  abstract quote(): void;\n}\n@Injectable({ providedIn: 'root' })\nexport class RealPrices extends Prices {\n  quote(): void {}\n}\n"),
                ("src/app/prices.stories.ts", "export default { providers: [{ provide: Prices, useClass: MockPrices }] };\n"),
                ("src/app/contacts.ts", "export interface Contacts { save(): void; }\nexport const CONTACTS = new InjectionToken<Contacts>('c');\nexport class DomainContacts implements Contacts {\n  save(): void {}\n}\nexport class AccountContacts implements Contacts {\n  save(): void {}\n}\n"),
                ("src/app/domains/domains.routes.ts", "export const routes: Routes = [{ path: 'd', component: DomainPage, providers: [{ provide: CONTACTS, useClass: DomainContacts }] }];\n"),
                ("src/app/account/account.routes.ts", "export const routes: Routes = [{ path: 'a', component: AccountPage, providers: [{ provide: CONTACTS, useClass: AccountContacts }] }];\n"),
                (
                    "src/app/domains/domain.page.ts",
                    "@Component({ selector: 'app-domain', template: '' })\nexport class DomainPage {\n  private contacts = inject(CONTACTS);\n  private prices = inject(Prices);\n  go(): void {\n    this.contacts.save();\n    this.prices.quote();\n  }\n}\n",
                ),
                // Outside every provider's directory, with two that disagree: either may run.
                ("src/app/shared/other.page.ts", "export class OtherPage {\n  private contacts = inject(CONTACTS);\n  go(): void {\n    this.contacts.save();\n  }\n}\n"),
            ],
        );
        let scope = Scope { repos: vec![ui.clone()], no_sync: false };
        let opts = FlowOptions { depth: 10, max_steps: 80, max_repos: 4, edges: Default::default(), children: DEFAULT_CHILDREN };
        let f = flow("DomainPage", &opts, &scope).unwrap().data.unwrap();
        let steps: Vec<_> = f.steps.iter().map(|s| (s.step.qualified.as_str(), s.step.edge)).collect();
        assert_eq!(steps, [("DomainContacts.save", "via_di"), ("RealPrices.quote", "via_di")]);
        let other = flow("OtherPage", &opts, &scope).unwrap().data.unwrap();
        let steps: Vec<_> = other.steps.iter().map(|s| (s.step.qualified.as_str(), s.step.edge, s.step.confidence, s.step.binding.as_deref().unwrap_or(""))).collect();
        assert_eq!(
            steps,
            [
                ("AccountContacts.save", "via_di", "ambiguous", "src/app/account/account.routes.ts:1, default; one of 2 providers in scope"),
                ("DomainContacts.save", "via_di", "ambiguous", "src/app/domains/domains.routes.ts:1, default; one of 2 providers in scope"),
            ]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A provider placed at the call only by its directory (a route file beside the page) is a convention: the
    /// binding says `directory_convention`. A component's own `providers` are explicit and take precedence over a
    /// provider beside it in the directory; the root injector's (the config the application bootstraps with) apply
    /// everywhere.
    #[test]
    fn providers_by_directory_are_a_convention_and_a_components_own_win() {
        let cache = test_cache();
        let base = cache.dir("flow-di-dir");
        let ui = crate::ops::tests::repo(
            &base,
            "ui",
            &[
                ("src/app/contacts.ts", "export interface Contacts { save(): void; }\nexport const CONTACTS = new InjectionToken<Contacts>('c');\nexport class DomainContacts implements Contacts {\n  save(): void {}\n}\nexport class AccountContacts implements Contacts {\n  save(): void {}\n}\n"),
                ("src/app/domains/domains.routes.ts", "export const routes: Routes = [{ path: 'd', component: DomainPage, providers: [{ provide: CONTACTS, useClass: DomainContacts }] }];\n"),
                ("src/app/domains/domain.page.ts", "@Component({ selector: 'app-domain', template: '' })\nexport class DomainPage {\n  private contacts = inject(CONTACTS);\n  private clock = inject(Clock);\n  go(): void {\n    this.contacts.save();\n    this.clock.now();\n  }\n}\n"),
                ("src/app/clock.ts", "export abstract class Clock {\n  abstract now(): void;\n}\nexport class SystemClock extends Clock {\n  now(): void {}\n}\n"),
                ("src/app/app.config.ts", "export const appConfig: ApplicationConfig = { providers: [{ provide: Clock, useClass: SystemClock }] };\n"),
                ("src/main.ts", "import { appConfig } from './app/app.config';\nbootstrapApplication(App, appConfig);\n"),
                ("src/app/billing/billing.module.ts", "@NgModule({ providers: [{ provide: CONTACTS, useClass: AccountContacts }] })\nexport class BillingModule {}\n"),
                (
                    "src/app/billing/billing.page.ts",
                    "@Component({ selector: 'app-billing', template: '', providers: [{ provide: CONTACTS, useClass: DomainContacts }] })\nexport class BillingPage {\n  private contacts = inject(CONTACTS);\n  go(): void {\n    this.contacts.save();\n  }\n}\n",
                ),
            ],
        );
        let scope = Scope { repos: vec![ui.clone()], no_sync: false };
        let opts = FlowOptions { depth: 10, max_steps: 80, max_repos: 4, edges: Default::default(), children: DEFAULT_CHILDREN };
        let step = |entry: &str| {
            let f = flow(entry, &opts, &scope).unwrap().data.unwrap();
            f.steps.iter().map(|s| (s.step.qualified.clone(), s.step.confidence, s.step.evidence.as_ref().map(|e| e.assumptions.clone()).unwrap_or_default())).collect::<Vec<_>>()
        };
        let convention = vec![kc_store::answer::Assumption::DirectoryConvention];
        assert_eq!(step("DomainPage"), [("DomainContacts.save".to_string(), "exact", convention), ("SystemClock.now".to_string(), "exact", Vec::new())]);
        assert_eq!(step("BillingPage"), [("DomainContacts.save".to_string(), "exact", Vec::new())]);
        // A policy refusing the convention leaves the first out.
        let strict = FlowOptions { edges: crate::ops::edge_filter(&[], "default,-directory_convention").unwrap(), ..opts.clone() };
        let out = flow("DomainPage", &strict, &scope).unwrap();
        assert_eq!(out.data.unwrap().steps.iter().map(|s| s.step.qualified.as_str()).collect::<Vec<_>>(), ["SystemClock.now"]);
        assert_eq!(out.env["policy"]["excluded_by"]["directory_convention"], 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A page under a module whose factory defers to the parent route's provider (`inject(T, { skipSelf }) ?? …`),
    /// reached under two routes; a dialog base class calling through an abstract field its subclass overrides; and a
    /// page injecting a service whose field initializer loads data.
    #[test]
    fn parent_providers_fan_out_overridden_fields_rebind_and_injected_services_construct() {
        let cache = test_cache();
        let base = cache.dir("flow-round2");
        let ui = crate::ops::tests::repo(
            &base,
            "ui",
            &[
                ("src/app/editor.ts", "export interface Editor { save(): void; }\nexport const EDITOR = new InjectionToken<Editor>('e');\nexport class LocalEditor implements Editor {\n  save(): void {}\n}\n"),
                ("src/app/domains/domains.routes.ts", "export const routes: Routes = [{ path: 'd', loadChildren: () => ContactsModule, providers: [{ provide: EDITOR, useClass: DomainEditor }] }];\nexport class DomainEditor implements Editor {\n  save(): void {}\n}\n"),
                ("src/app/bulk/bulk.routes.ts", "export const routes: Routes = [{ path: 'b', loadChildren: () => ContactsModule, providers: [{ provide: EDITOR, useClass: BulkEditor }] }];\nexport class BulkEditor implements Editor {\n  save(): void {}\n}\n"),
                ("src/app/contacts/contacts.module.ts", "@NgModule({ providers: [{ provide: EDITOR, useFactory: () => inject(EDITOR, { optional: true, skipSelf: true }) ?? inject(LocalEditor) }] })\nexport class ContactsModule {}\n"),
                (
                    "src/app/contacts/view.page.ts",
                    "@Component({ selector: 'app-view', template: '' })\nexport class ViewPage {\n  private editor = inject(EDITOR);\n  save(): void {\n    this.editor.save();\n  }\n}\n",
                ),
                ("src/app/codes.ts", "export interface Codes { resend(): void; }\n@Injectable()\nexport class EmailCodes implements Codes {\n  resend(): void {}\n}\n@Injectable()\nexport class SmsCodes implements Codes {\n  resend(): void {}\n}\n"),
                ("src/app/code-dialog.base.ts", "export abstract class CodeDialogBase {\n  protected abstract readonly service: Codes;\n  resend(): void {\n    this.service.resend();\n  }\n}\n"),
                (
                    "src/app/email-code.component.ts",
                    "@Component({ selector: 'app-email-code', template: '' })\nexport class EmailCodeComponent extends CodeDialogBase {\n  protected readonly service = inject(EmailCodes);\n}\n",
                ),
                ("src/app/api.ts", "@Injectable({ providedIn: 'root' })\nexport class PricesApi {\n  all(): void {}\n}\n"),
                (
                    "src/app/pricing.service.ts",
                    "@Injectable({ providedIn: 'root' })\nexport class PricingService {\n  private api = inject(PricesApi);\n  prices = toSignal(this.api.all());\n  min(): number { return 1; }\n}\n",
                ),
                // `this.load()` in the base runs the subclass's override, never another subclass's that DI would bind.
                (
                    "src/app/panel.base.ts",
                    "export abstract class PanelBase {\n  abstract load(): void;\n  ngOnInit(): void {\n    this.load();\n  }\n}\n@Injectable({ providedIn: 'root' })\nexport class OtherPanel extends PanelBase {\n  load(): void {}\n}\n",
                ),
                (
                    "src/app/orders.panel.ts",
                    "@Component({ selector: 'app-orders-panel', template: '' })\nexport class OrdersPanel extends PanelBase {\n  private api = inject(PricesApi);\n  load(): void {\n    this.api.all();\n  }\n}\n",
                ),
                (
                    "src/app/cart.component.ts",
                    "@Component({ selector: 'app-cart', template: '' })\nexport class CartComponent {\n  private pricing = inject(PricingService);\n  years(): number {\n    return this.pricing.min();\n  }\n}\n",
                ),
            ],
        );
        let scope = Scope { repos: vec![ui.clone()], no_sync: false };
        let opts = FlowOptions { depth: 10, max_steps: 80, max_repos: 4, edges: Default::default(), children: DEFAULT_CHILDREN };
        let steps = |entry: &str| -> Vec<(String, &'static str, &'static str, String)> {
            let f = flow(entry, &opts, &scope).unwrap().data.unwrap();
            f.steps.iter().map(|s| (s.step.qualified.clone(), s.step.edge, s.step.confidence, s.step.binding.clone().unwrap_or_default())).collect()
        };
        let s = |q: &str, e: &'static str, c: &'static str, b: &str| (q.to_string(), e, c, b.to_string());
        // Any parent route's provider, or the module's own fallback: all listed, none picked.
        assert_eq!(
            steps("ViewPage"),
            [
                s("BulkEditor.save", "via_di", "ambiguous", "src/app/bulk/bulk.routes.ts:1, default; one of 3 providers in scope"),
                s("LocalEditor.save", "via_di", "ambiguous", "src/app/contacts/contacts.module.ts:1, default; one of 3 providers in scope"),
                s("DomainEditor.save", "via_di", "ambiguous", "src/app/domains/domains.routes.ts:1, default; one of 3 providers in scope"),
            ]
        );
        // The base's call goes to the subclass's field type, not to either implementation of the interface.
        assert_eq!(steps("EmailCodeComponent"), [s("EmailCodes.resend", "calls", "exact", "")]);
        assert_eq!(steps("OrdersPanel"), [s("PricesApi.all", "calls", "exact", "")]);
        // Injecting the pricing service builds it: its initializer's call runs, marked as a root singleton's.
        assert_eq!(
            steps("CartComponent"),
            [
                s("PricingService.min", "calls", "exact", ""),
                s("PricingService", "constructs", "exact", "src/app/pricing.service.ts:1, singleton"),
                s("PricesApi.all", "calls", "exact", ""),
            ]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A component providing a facade and the coordinator injecting it, while a route elsewhere aliases the facade
    /// to a token; settings components dispatching one action whose effect switches on the setting; a service
    /// whose construction only assigns, and one storing handlers; a library's element in another repo; a library
    /// type the app provides; and a step limit shared by branches.
    #[test]
    fn self_providers_payload_branches_quiet_construction_library_children_and_framework_types() {
        let cache = test_cache();
        let base = cache.dir("flow-round3");
        let s = |q: &str, e: &'static str, c: &'static str| (q.to_string(), e, c);
        crate::ops::tests::repo(
            &base,
            "lib",
            &[("src/badge.component.ts", "@Component({ selector: 'lib-badge', template: '' })\nexport class LibBadgeComponent {\n  private api = inject(LibApi);\n  ngOnInit(): void {\n    this.api.ping();\n  }\n}\nexport class LibApi {\n  ping(): void {}\n}\n")],
        );
        let ui = crate::ops::tests::repo(
            &base,
            "ui",
            &[
                ("src/app/editor.ts", "export interface Editor { save(): void; }\nexport const EDITOR = new InjectionToken<Editor>('e');\n"),
                ("src/app/contacts/bulk.facade.ts", "@Injectable()\nexport class BulkFacade implements Editor {\n  save(): void {\n    this.api.bulk();\n  }\n}\nexport class OtherFacade implements Editor {\n  save(): void {}\n}\n"),
                ("src/app/bulk/bulk.routes.ts", "export const routes: Routes = [{ path: 'b', component: X, providers: [{ provide: EDITOR, useClass: OtherFacade }, { provide: BulkFacade, useExisting: EDITOR }] }];\n"),
                (
                    "src/app/dialog/container/container.component.ts",
                    "@Component({ selector: 'app-container', template: '<lib-badge></lib-badge>', providers: [BulkFacade, SaveCoordinator, { provide: EDITOR, useExisting: BulkFacade }] })\nexport class ContainerComponent {\n  private coordinator = inject(SaveCoordinator);\n  go(): void {\n    this.coordinator.save();\n  }\n}\n",
                ),
                ("src/app/dialog/services/save.coordinator.ts", "@Injectable()\nexport class SaveCoordinator {\n  private facade = inject(BulkFacade);\n  save(): void {\n    this.facade.save();\n  }\n}\n"),
                ("src/app/api.ts", "@Injectable({ providedIn: 'root' })\nexport class Api {\n  bulk(): void {}\n  removeA(): void {}\n  removeB(): void {}\n  run(): void {}\n}\n"),
                ("src/app/disable.actions.ts", "export const Disable = createAction('[S] Disable', props<{ setting: string }>());\n"),
                (
                    "src/app/settings.effects.ts",
                    "export class SettingsEffects {\n  private api = inject(Api);\n  disable$ = createEffect(() => this.actions$.pipe(ofType(Disable), mergeMap((action) => {\n    switch (action.setting) {\n      case 'a':\n        return this.api.removeA();\n      case 'b':\n        return this.api.removeB();\n    }\n  })));\n}\n",
                ),
                ("src/app/setting.base.ts", "export abstract class SettingBase {\n  abstract readonly kind: string;\n  off(): void {\n    this.store.dispatch(Disable({ setting: this.kind }));\n  }\n}\n"),
                ("src/app/setting-b.component.ts", "@Component({ selector: 'app-setting-b', template: '' })\nexport class SettingBComponent extends SettingBase {\n  readonly kind = 'b';\n}\n"),
                ("src/app/any-setting.component.ts", "@Component({ selector: 'app-any', template: '' })\nexport class AnySettingComponent {\n  off(s: string): void {\n    this.store.dispatch(Disable({ setting: s }));\n  }\n}\n"),
                ("src/app/holder.service.ts", "@Injectable({ providedIn: 'root' })\nexport class Holder {\n  private api = inject(Api);\n  private handlers = new Map([['x', () => this.api.run()]]);\n}\n"),
                ("src/app/plain.service.ts", "@Injectable({ providedIn: 'root' })\nexport class Plain {\n  private holder = inject(Holder);\n}\n"),
                ("src/app/page.component.ts", "@Component({ selector: 'app-page', template: '' })\nexport class PageComponent {\n  private plain = inject(Plain);\n}\n"),
                ("src/app/prices.service.ts", "@Injectable({ providedIn: 'root' })\nexport class Prices {\n  private api = inject(Api);\n  all$ = timer(0, 1000).pipe(switchMap(() => this.api.run()));\n}\n"),
                ("src/app/prices.component.ts", "@Component({ selector: 'app-prices', template: '' })\nexport class PricesComponent {\n  private prices = inject(Prices);\n}\n"),
                ("src/app/sentry.handler.ts", "@Injectable()\nexport class SentryHandler implements ErrorHandler {\n  handleError(e: unknown): void {\n    this.api.run();\n  }\n}\n"),
                ("src/app/app.module.ts", "@NgModule({ providers: [{ provide: ErrorHandler, useClass: SentryHandler }] })\nexport class AppModule {}\n"),
                ("src/app/boundary.component.ts", "@Component({ selector: 'app-boundary', template: '' })\nexport class BoundaryComponent {\n  private original = inject(ErrorHandler);\n  fail(e: unknown): void {\n    this.original.handleError(e);\n  }\n}\n"),
            ],
        );
        let scope = Scope { repos: vec![ui.clone()], no_sync: false };
        let opts = FlowOptions { depth: 10, max_steps: 80, max_repos: 4, edges: Default::default(), children: DEFAULT_CHILDREN };
        let steps = |entry: &str, o: &FlowOptions| -> Vec<(String, &'static str, &'static str)> {
            let f = flow(entry, o, &scope).unwrap().data.unwrap();
            f.steps.iter().map(|x| (x.step.qualified.clone(), x.step.edge, x.step.confidence)).collect()
        };
        // The coordinator the container provides gets the facade the container provides as itself, never the
        // route's alias elsewhere; the library's element in the other repo is a child.
        assert_eq!(
            steps("ContainerComponent", &opts),
            [
                s("SaveCoordinator.save", "calls", "exact"),
                s("BulkFacade.save", "calls", "exact"),
                s("Api.bulk", "calls", "unique_name"),
                s("LibBadgeComponent", "child", "unique_name"),
                s("LibApi.ping", "calls", "exact"),
            ]
        );
        // The dispatch passes 'b': only that branch runs; passing an unknown value, both, each one of the branches.
        assert_eq!(steps("SettingBComponent", &opts), [s("SettingsEffects.disable$", "ngrx", "unique_name"), s("Api.removeB", "calls", "exact")]);
        assert_eq!(
            steps("AnySettingComponent", &opts),
            [s("SettingsEffects.disable$", "ngrx", "unique_name"), s("Api.removeA", "calls", "ambiguous"), s("Api.removeB", "calls", "ambiguous")]
        );
        // Building the plain service builds the holder, whose stored handler doesn't run: nothing to list.
        let page = flow("PageComponent", &opts, &scope).unwrap().data.unwrap();
        assert!(page.steps.is_empty() && page.left_out.contains_key("quiet_construct"), "{:?}", page.steps);
        // An observable's operator runs when it is subscribed: marked so, and never a symbol of its own anywhere.
        let f = flow("PricesComponent", &opts, &scope).unwrap().data.unwrap();
        let lazy: Vec<_> = f.steps.iter().map(|x| (x.step.qualified.as_str(), x.step.binding.as_deref())).collect();
        assert_eq!(lazy, [("Prices", Some("src/app/prices.service.ts:1, singleton")), ("Api.run", Some(LAZY_NOTE))]);
        let o = crate::ops::open_fresh(&ui, false).unwrap();
        assert!(kc_store::query::definitions(o.store.conn(), "Prices").unwrap().iter().all(|d| !d.name.contains("$")));
        let (found, _) = kc_store::query::find(o.store.conn(), "all", None, 50).unwrap();
        assert!(found.iter().all(|h| !format!("{h:?}").contains("$lazy")), "{found:?}");
        // A library type the app provides an implementation of.
        assert_eq!(steps("BoundaryComponent", &opts), [s("SentryHandler.handleError", "via_di", "unique_name"), s("Api.run", "calls", "unique_name")]);
        // With room for 2 steps, each branch of the container (its own calls, its child) still gets its first.
        let tight = steps("ContainerComponent", &FlowOptions { max_steps: 4, ..opts.clone() });
        assert!(tight.iter().any(|x| x.1 == "child"), "{tight:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Actions made by `createActionGroup` (event names, and keys computed from a constant), dispatched as
    /// `Group.creator(…)` and handled by `ofType(Group.creator)`; a payload switch narrowed; `impact` back from the effect.
    #[test]
    fn action_groups_link_dispatches_to_effects() {
        let cache = test_cache();
        let base = cache.dir("flow-groups");
        let ui = crate::ops::tests::repo(
            &base,
            "ui",
            &[
                (
                    "src/app/account.actions.ts",
                    "export const AccountTypes = {\n  load: 'Load Account',\n} as const;\nexport const AccountActions = createActionGroup({\n  source: 'Account',\n  events: {\n    'Search Accounts': props<{ q: string }>(),\n    [AccountTypes.load]: emptyProps(),\n    Remove: props<{ kind: string }>(),\n  },\n});\n",
                ),
                ("src/app/api.ts", "@Injectable({ providedIn: 'root' })\nexport class AccountApi {\n  search(): void {}\n  load(): void {}\n  removeHard(): void {}\n  removeSoft(): void {}\n}\n"),
                (
                    "src/app/account.effects.ts",
                    "export class AccountEffects {\n  private api = inject(AccountApi);\n  search$ = createEffect(() => this.actions$.pipe(ofType(AccountActions.searchAccounts), mergeMap(() => this.api.search())));\n  load$ = createEffect(() => this.actions$.pipe(ofType(AccountActions.loadAccount), mergeMap(() => this.api.load())));\n  remove$ = createEffect(() => this.actions$.pipe(ofType(AccountActions.remove), mergeMap((a) => {\n    switch (a.kind) {\n      case 'hard':\n        return this.api.removeHard();\n      case 'soft':\n        return this.api.removeSoft();\n    }\n  })));\n}\n",
                ),
                // A reducer spec creating an action without dispatching it, and an unrelated `action()` call.
                (
                    "src/app/account.reducer.spec.ts",
                    "describe('reducer', () => {\n  it('loads', () => {\n    const action = AccountActions.loadAccount();\n    expect(reducer(undefined, action)).toBeTruthy();\n  });\n});\n",
                ),
                ("src/app/other.component.ts", "export class OtherComponent {\n  run(): void {\n    action('x');\n  }\n}\n"),
                (
                    "src/app/accounts.component.ts",
                    "@Component({ selector: 'app-accounts', template: '' })\nexport class AccountsComponent {\n  find(): void {\n    this.store.dispatch(AccountActions.searchAccounts({ q: 'x' }));\n  }\n  open(): void {\n    const action = AccountActions.loadAccount();\n    this.store.dispatch(action);\n  }\n  drop(): void {\n    this.store.dispatch(AccountActions.remove({ kind: 'soft' }));\n  }\n}\n",
                ),
            ],
        );
        let scope = Scope { repos: vec![ui.clone()], no_sync: false };
        let opts = FlowOptions { depth: 10, max_steps: 80, max_repos: 4, edges: Default::default(), children: DEFAULT_CHILDREN };
        let f = flow("AccountsComponent", &opts, &scope).unwrap().data.unwrap();
        let steps: Vec<_> = f.steps.iter().map(|x| (x.step.qualified.as_str(), x.step.edge, x.step.ngrx.as_deref())).collect();
        assert_eq!(
            steps,
            [
                ("AccountEffects.search$", "ngrx", Some("AccountActions.searchAccounts")),
                ("AccountApi.search", "calls", None),
                ("AccountEffects.load$", "ngrx", Some("AccountActions.loadAccount")),
                ("AccountApi.load", "calls", None),
                // The dispatch passes 'soft': only that branch.
                ("AccountEffects.remove$", "ngrx", Some("AccountActions.remove")),
                ("AccountApi.removeSoft", "calls", None),
            ]
        );
        // Back from the effect to the component dispatching its action.
        let p = crate::ops::ImpactParams { symbol: Some("AccountEffects.load$".into()), depth: 2, limit: 50, ..Default::default() };
        let out = crate::ops::impact(&p, &scope).unwrap();
        let got: Vec<(String, &str)> = out.data[0].impact.affected.iter().map(|a| (a.qualified.clone(), a.edge)).collect();
        assert!(got.contains(&("AccountsComponent.open".to_string(), "ngrx")), "{got:?}");
        // Only real dispatches: not the spec's `const action = …`, nor what calls something named `action`.
        assert!(got.iter().all(|(q, _)| !q.contains("action") && !q.starts_with("OtherComponent")), "{got:?}");
        assert_eq!(got.iter().filter(|(_, e)| *e == "ngrx").count(), 1, "{got:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
