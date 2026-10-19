//! Links message-bus publishers to consumers across repos, through the message type they share.
//!
//! A message type is identified by its full name (namespace and type), which is also what the bus routes by. A
//! publish or consumer names the type as C# would find it: the enclosing namespaces, nearest first, then the file's
//! `using`s, against the message types (a configured message attribute, `[BusMessage]`) the workspace declares;
//! failing that, the one message type of that name in the repo or the repos it depends on (types ship in the
//! publisher's `*.Models` package). A link is:
//!
//! - `exact`: both sides resolve to the same message type.
//! - `ambiguous`: only the bare name matches (a side resolves to no declared type, or to several).
//!
//! Test code is listed but never linked. Checkouts of one repo count once, as for HTTP routes, and a repo whose
//! checkout is gone counts not at all: its declarations, publishes and consumers are left out, and an answer they
//! would have joined (the same message name) names it as missing.
//!
//! The name is not all a bus routes by: a message reaches a consumer only on the destination it listens on (a
//! virtual host, a queue). Each side's destination is the one its code names (a configured publish call with a
//! `host` or `queue`: `PublishToHost(e, Hosts.Jobs)`, `SendToQueue(c, host, "queue")`), else the one its message type
//! declares (`[BusMessage(Hosts.Orders)]`). When both are known and differ, the link is wrong: dropped when both sides
//! name theirs in code, `ambiguous` when one only inherits its type's. When either isn't known (or one names a queue
//! the other doesn't), the link assumes `same_destination`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kc_store::answer::Assumption;
use kc_store::bus::BusRow;

use crate::workspace::RepoSummary;

/// The message type a publish or consumer names.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    /// The full name, when the name resolves to one declared message type.
    pub full: Option<String>,
    /// The bare name.
    pub name: String,
    /// `exact` (one declared type), `ambiguous` (several declared types of that name), or `unresolved` (none).
    pub confidence: &'static str,
    /// The queue (virtual host) the declared type names.
    pub queue: Option<String>,
}

/// A counterpart across the bus: a consumer of what a publish sends, or a publisher of what a consumer handles.
#[derive(Debug, Clone)]
pub struct BusLink {
    pub repo: PathBuf,
    pub row: BusRow,
    /// `exact` or `ambiguous`.
    pub confidence: &'static str,
    /// What the link did not check: `same_destination` when a side's destination isn't known.
    pub assumptions: Vec<Assumption>,
}

/// Where a publish or consumer sends or listens: the destination its code names (`explicit`), else the one its
/// message type declares; `None` when neither says.
fn destination(row: &BusRow, res: &Resolution) -> Option<(String, bool)> {
    match &row.queue {
        Some(q) => Some((q.to_ascii_lowercase(), true)),
        None => res.queue.as_ref().filter(|_| res.confidence == "exact").map(|q| (q.to_ascii_lowercase(), false)),
    }
}

/// How two sides' destinations (see [`destination`]) bear on a link between them: `None` when they certainly differ
/// (both named in code), else whether they may differ (`ambiguous`) and whether the link assumes them the same.
fn destinations_meet(a: Option<(String, bool)>, b: Option<(String, bool)>) -> Option<(bool, bool)> {
    let (Some((a, a_explicit)), Some((b, b_explicit))) = (a, b) else { return Some((false, true)) };
    if a == b {
        return Some((false, false));
    }
    // A virtual host and a queue on it (`host/queue`): the host matches, the queue is one side's only.
    let host = |d: &str| d.split('/').next().unwrap_or(d).to_string();
    if host(&a) == host(&b) {
        return Some((false, true));
    }
    if a_explicit && b_explicit { None } else { Some((true, true)) }
}

/// The workspace's bus facts, indexed for linking. Build once per answer.
pub struct Bus<'a> {
    summaries: &'a [RepoSummary],
    /// Declared message types by bare name: repo, row, full name.
    defs: HashMap<&'a str, Vec<(&'a Path, &'a BusRow, String)>>,
    /// Publishes and consumers outside tests, by the bare name of their message.
    uses: HashMap<&'a str, Vec<(&'a Path, &'a BusRow)>>,
    /// Repos each repo depends on, computed when needed.
    deps: RefCell<HashMap<PathBuf, Vec<PathBuf>>>,
    /// Repos left out because their checkout is gone (see [`crate::workspace::gone`]).
    gone: Vec<&'a RepoSummary>,
}

impl<'a> Bus<'a> {
    /// The bus facts of `summaries`, one checkout per repo: `home`'s for its own repo, else the most recently indexed.
    pub fn new(summaries: &'a [RepoSummary], home: Option<&Path>) -> Bus<'a> {
        let home: Vec<PathBuf> = home.map(Path::to_path_buf).into_iter().collect();
        let mut repos: Vec<&RepoSummary> = crate::workspace::distinct(summaries, &home);
        repos.sort_by(|a, b| a.root.cmp(&b.root));
        let mut defs: HashMap<&str, Vec<_>> = HashMap::new();
        let mut uses: HashMap<&str, Vec<_>> = HashMap::new();
        for s in &repos {
            for r in &s.bus {
                let Some(name) = r.name() else { continue };
                match r.kind.as_str() {
                    "message" => {
                        let full = r.full_name().unwrap_or_default();
                        defs.entry(name).or_default().push((s.root.as_path(), r, full));
                    }
                    _ if !r.test => uses.entry(name).or_default().push((s.root.as_path(), r)),
                    _ => {}
                }
            }
        }
        Bus { summaries, defs, uses, deps: RefCell::new(HashMap::new()), gone: crate::workspace::gone(summaries) }
    }

    /// The bus facts of the repo at `root`.
    pub fn rows(&self, root: &Path) -> &'a [BusRow] {
        self.summaries.iter().find(|s| s.root == root).map_or(&[], |s| s.bus.as_slice())
    }

    fn dependencies(&self, root: &Path) -> Vec<PathBuf> {
        self.deps
            .borrow_mut()
            .entry(root.to_path_buf())
            .or_insert_with(|| crate::workspace::neighbours(root, self.summaries).0.into_iter().map(|s| s.root.clone()).collect())
            .clone()
    }

    /// The message type `row` (a fact of the repo at `root`) names.
    pub fn resolve(&self, root: &Path, row: &BusRow) -> Option<Resolution> {
        let written = row.message.as_deref()?;
        let name = row.name()?.to_string();
        let found = |full: &str, defs: &[&(&Path, &BusRow, String)]| Resolution {
            full: Some(full.to_string()),
            name: name.clone(),
            confidence: "exact",
            queue: defs.iter().find(|d| d.2 == full).and_then(|d| d.1.queue.clone()),
        };
        if row.kind == "message" {
            return Some(Resolution { full: row.full_name(), name, confidence: "exact", queue: row.queue.clone() });
        }
        let defs: Vec<&(&Path, &BusRow, String)> = self.defs.get(name.as_str()).map(|d| d.iter().collect()).unwrap_or_default();
        if defs.is_empty() {
            return Some(Resolution { full: None, name, confidence: "unresolved", queue: None });
        }
        let declared = |full: &str| defs.iter().any(|d| d.2 == full);
        // As C# looks a name up: the enclosing namespaces, nearest first, then the usings (where two matches would be
        // a compile error), then the name as written.
        let first = row.scopes.first().map_or("", String::as_str);
        let namespaces = row.scopes.iter().take_while(|s| first == s.as_str() || first.starts_with(&format!("{s}.")));
        for ns in namespaces.clone() {
            let full = format!("{ns}.{written}");
            if declared(&full) {
                return Some(found(&full, &defs));
            }
        }
        let n = namespaces.count();
        let mut hits: Vec<String> = row.scopes[n..].iter().map(|u| format!("{u}.{written}")).filter(|f| declared(f)).collect();
        hits.dedup();
        match hits.as_slice() {
            [one] => return Some(found(one, &defs)),
            [] if declared(written) => return Some(found(written, &defs)),
            [] => {}
            _ => return Some(Resolution { full: None, name, confidence: "ambiguous", queue: None }),
        }
        // Usings this index can't see (global ones): the one type of that name in the repo or its dependencies.
        let mut near: Vec<&str> = {
            let deps = self.dependencies(root);
            defs.iter().filter(|d| d.0 == root || deps.iter().any(|p| p == d.0)).map(|d| d.2.as_str()).collect()
        };
        near.sort_unstable();
        near.dedup();
        if let [one] = near.as_slice() {
            return Some(found(one, &defs));
        }
        // Declared elsewhere only, or several times: which one is meant can't be told.
        Some(Resolution { full: None, name, confidence: "ambiguous", queue: None })
    }

    /// The counterparts of `row`, a fact of the repo at `root`: consumers of a publish, publishers of a consumer,
    /// both for a message type. Exact links first.
    pub fn links(&self, root: &Path, row: &BusRow) -> Vec<BusLink> {
        let Some(mine) = self.resolve(root, row) else { return Vec::new() };
        let want = |kind: &str| match row.kind.as_str() {
            "publish" => kind == "consume",
            "consume" => kind == "publish",
            _ => true,
        };
        // A gone repo declaring the message, or publishing or consuming it, would have been resolved against or linked.
        crate::workspace::note_missing_if(self.gone.iter().copied(), |g| {
            g.bus.iter().any(|r| r.name() == Some(mine.name.as_str()) && (r.kind == "message" || (!r.test && want(&r.kind))))
        });
        let mut out = Vec::new();
        for (repo, other) in self.uses.get(mine.name.as_str()).into_iter().flatten() {
            if !want(&other.kind) || (*repo == root && *other == row) {
                continue;
            }
            let Some(theirs) = self.resolve(repo, other) else { continue };
            let confidence = match (&mine.full, &theirs.full) {
                (Some(a), Some(b)) if a == b => "exact",
                (Some(_), Some(_)) => continue,
                _ => "ambiguous",
            };
            // A message type has no destination of its own: its publishers' and consumers' are theirs.
            let meet = if row.kind == "message" { Some((false, false)) } else { destinations_meet(destination(row, &mine), destination(other, &theirs)) };
            let Some((differ, assumed)) = meet else { continue };
            let confidence = if differ { "ambiguous" } else { confidence };
            let assumptions = if assumed { vec![Assumption::SameDestination] } else { Vec::new() };
            out.push(BusLink { repo: repo.to_path_buf(), row: (*other).clone(), confidence, assumptions });
        }
        out.sort_by(|a, b| (a.confidence != "exact", &a.repo, &a.row.file, a.row.line).cmp(&(b.confidence != "exact", &b.repo, &b.row.file, b.row.line)));
        out
    }
}

/// One bus fact about a symbol, with its counterparts, for `context` and `knowcode events`.
#[derive(Debug, Clone)]
pub struct BusEntry {
    /// `message` (the symbol is a message type; links are its publishers and consumers), `publishes` (links are
    /// consumers) or `consumes` (links are publishers).
    pub role: &'static str,
    pub row: BusRow,
    pub resolution: Option<Resolution>,
    pub links: Vec<BusLink>,
}

impl BusEntry {
    fn new(bus: &Bus, root: &Path, row: &BusRow) -> BusEntry {
        let role = match row.kind.as_str() {
            "message" => "message",
            "publish" => "publishes",
            _ => "consumes",
        };
        BusEntry { role, row: row.clone(), resolution: bus.resolve(root, row), links: bus.links(root, row) }
    }

    /// The message's name: full when it resolves, else as written; `?` when unknown.
    pub fn message(&self) -> String {
        self.resolution.as_ref().and_then(|r| r.full.clone()).or_else(|| self.row.message.clone()).unwrap_or_else(|| "?".to_string())
    }

    /// The queue the publish names, or the one its message type declares.
    pub fn queue(&self) -> Option<String> {
        self.row.queue.clone().or_else(|| self.resolution.as_ref().and_then(|r| r.queue.clone()))
    }
}

/// The bus facts about the symbol `qualified` defined in `file` of the repo at `root`, starting at `line` (0: any):
/// the message type it is, the messages it publishes, or those it consumes (as the consumer class or its `Consume`
/// method). Overloads share a qualified name: a publish is the overload's that starts at `line`.
pub fn for_symbol(root: &Path, qualified: &str, file: &str, line: u32, summaries: &[RepoSummary]) -> Vec<BusEntry> {
    let bus = Bus::new(summaries, Some(root));
    bus.rows(root)
        .iter()
        .filter(|r| r.file == file && (r.symbol == qualified || (r.kind == "consume" && r.handler() == qualified)))
        .filter(|r| at_line(r, line))
        .map(|r| BusEntry::new(&bus, root, r))
        .collect()
}

/// The publishes outside tests whose message type isn't known (`message: None`: a variable, a factory's result, a
/// helper's type parameter its callers in other files fill), across the workspace, that may publish the messages of
/// `entries` (a message type, or what a consumer consumes): those in files the message's namespace is in scope of
/// (any file, for a message at top level). A message or consumer answer may be missing them as publishers. Each with
/// its repo's root.
pub fn unknown_publishes(root: &Path, entries: &[BusEntry], summaries: &[RepoSummary]) -> Vec<(PathBuf, BusRow)> {
    let mut spaces: Vec<String> = Vec::new();
    let mut top = false;
    for e in entries.iter().filter(|e| e.role != "publishes") {
        let full = e.message();
        if full == "?" {
            continue;
        }
        // Its namespace, or the one around its outer type for a nested one; never a bare root (`Shop`), which every
        // file of the code base has in scope.
        let parts: Vec<&str> = full.split('.').collect();
        top |= parts.len() == 1;
        for n in [parts.len().saturating_sub(1), parts.len().saturating_sub(2)].into_iter().filter(|n| *n >= 2) {
            let ns = parts[..n].join(".");
            if !spaces.contains(&ns) {
                spaces.push(ns);
            }
        }
    }
    if spaces.is_empty() && !top {
        return Vec::new();
    }
    let home = [root.to_path_buf()];
    let mut repos = crate::workspace::distinct(summaries, &home);
    repos.sort_by(|a, b| a.root.cmp(&b.root));
    let mut out = Vec::new();
    let conventions = crate::config::conventions_now();
    for s in repos {
        // A bus method's own wrapper (`Send<T>(T m) => _bus.Send(m)`) passes on what its callers give it: those calls
        // are publishes of their own, with their types.
        let wrapper = |r: &BusRow| {
            let method = r.symbol.rsplit('.').next().unwrap_or(&r.symbol);
            conventions.bus.publish_call(method.trim_end_matches("Async")).is_some()
        };
        for r in s.bus.iter().filter(|r| r.kind == "publish" && !r.test && r.message.is_none() && !wrapper(r)) {
            if top || r.scopes.iter().any(|sc| spaces.contains(sc)) {
                out.push((s.root.clone(), r.clone()));
            }
        }
    }
    out
}


/// Every publish and consumer in the repo at `root`, outside tests, with its counterparts.
pub fn listing(root: &Path, summaries: &[RepoSummary]) -> Vec<BusEntry> {
    let bus = Bus::new(summaries, Some(root));
    bus.rows(root).iter().filter(|r| r.kind != "message" && !r.test).map(|r| BusEntry::new(&bus, root, r)).collect()
}

/// Whether a bus row belongs to the overload starting at `line` (0: any): only publishes are told apart.
fn at_line(r: &BusRow, line: u32) -> bool {
    r.kind != "publish" || line == 0 || r.start_line == line
}

/// The consumers that a change to the symbol `qualified` (in `file` of the repo at `root`, starting at `line`; 0: any)
/// reaches: those of the message type it is, or of the messages it publishes, each with the message it crosses on.
/// A registration closing a generic consumer runs no code of its own, so it isn't one.
pub fn consumers(root: &Path, qualified: &str, file: &str, line: u32, summaries: &[RepoSummary]) -> Vec<(BusEntry, BusLink)> {
    let bus = Bus::new(summaries, Some(root));
    let mut out = Vec::new();
    let own: Vec<&BusRow> = bus
        .rows(root)
        .iter()
        .filter(|r| r.file == file && r.symbol == qualified && r.kind != "consume" && at_line(r, line))
        .collect();
    for r in &own {
        let e = BusEntry::new(&bus, root, r);
        for l in e.links.iter().filter(|l| l.row.kind == "consume" && !l.row.registration()) {
            out.push((e.clone(), l.clone()));
        }
    }
    // A type the repo publishes by name without declaring it a message: its consumers, by name only.
    if own.is_empty() {
        let name = kc_store::bus::bare_name(qualified);
        let publishes: Vec<&BusRow> = bus.rows(root).iter().filter(|r| r.kind == "publish" && !r.test && r.name() == Some(name)).collect();
        let mut seen = Vec::new();
        for p in publishes {
            let e = BusEntry::new(&bus, root, p);
            for l in e.links.iter().filter(|l| l.row.kind == "consume" && !l.row.registration()) {
                if !seen.contains(&(l.repo.clone(), l.row.symbol.clone())) {
                    seen.push((l.repo.clone(), l.row.symbol.clone()));
                    out.push((e.clone(), BusLink { confidence: "ambiguous", ..l.clone() }));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(kind: &str, symbol: &str, message: &str, scopes: &[&str]) -> serde_json::Value {
        json!({
            "kind": kind, "symbol": symbol, "sym_kind": "class", "file": format!("src/{symbol}.cs"), "line": 3,
            "start_line": 1, "end_line": 9, "message": message, "scopes": scopes, "queue": null, "via": null,
            "test": symbol.ends_with("Tests"),
        })
    }

    fn repo(root: &str, bus: Vec<serde_json::Value>) -> RepoSummary {
        serde_json::from_value(json!({ "root": root, "packages": [], "modules": [], "bus": bus, "stamp": "" })).unwrap()
    }

    fn workspace() -> Vec<RepoSummary> {
        let mut placed = row("message", "OrderPlaced", "OrderPlaced", &["Shop.Orders.Events"]);
        placed["queue"] = json!("Orders");
        vec![
            repo(
                "/w/orders",
                vec![
                    placed,
                    row("message", "Refunded", "Refunded", &["Shop.Orders.Events"]),
                    row("publish", "OrderService.Place", "OrderPlaced", &["Shop.Orders.Service", "Shop.Orders", "Shop", "Shop.Orders.Events"]),
                    row("publish", "OrderService.Note", "Noted", &["Shop.Orders.Service"]),
                ],
            ),
            repo(
                "/w/billing",
                vec![
                    row("message", "Refunded", "Refunded", &["Shop.Billing.Events"]),
                    row("consume", "PlacedConsumer", "OrderPlaced", &["Shop.Billing", "Shop", "Shop.Orders.Events"]),
                    row("consume", "PlacedTests", "OrderPlaced", &["Shop.Billing.Tests", "Shop.Orders.Events"]),
                    // Its own `Refunded`, the nearer namespace's.
                    row("consume", "RefundConsumer", "Refunded", &["Shop.Billing.Events", "Shop.Billing", "Shop", "Shop.Orders.Events"]),
                    // Both `Refunded`s through usings: a compile error, so no telling.
                    row("consume", "Mixed", "Refunded", &["Other", "Shop.Orders.Events", "Shop.Billing.Events"]),
                    row("consume", "NotedConsumer", "Noted", &["Shop.Billing"]),
                ],
            ),
        ]
    }

    #[test]
    fn names_resolve_as_csharp_finds_them() {
        let ws = workspace();
        let bus = Bus::new(&ws, None);
        let res = |root: &str, symbol: &str| {
            let r = bus.rows(Path::new(root)).iter().find(|r| r.symbol == symbol).unwrap();
            let x = bus.resolve(Path::new(root), r).unwrap();
            (x.full, x.confidence)
        };
        let full = |s: &str| Some(s.to_string());
        assert_eq!(res("/w/orders", "OrderService.Place"), (full("Shop.Orders.Events.OrderPlaced"), "exact"));
        assert_eq!(res("/w/billing", "PlacedConsumer"), (full("Shop.Orders.Events.OrderPlaced"), "exact"));
        assert_eq!(res("/w/billing", "RefundConsumer"), (full("Shop.Billing.Events.Refunded"), "exact"));
        assert_eq!(res("/w/billing", "Mixed"), (None, "ambiguous"));
        assert_eq!(res("/w/billing", "NotedConsumer"), (None, "unresolved"));
        let placed = bus.rows(Path::new("/w/orders")).iter().find(|r| r.symbol == "OrderService.Place").unwrap();
        assert_eq!(bus.resolve(Path::new("/w/orders"), placed).unwrap().queue.as_deref(), Some("Orders"));
    }

    #[test]
    fn links_need_the_same_type_or_only_a_name() {
        let ws = workspace();
        let bus = Bus::new(&ws, None);
        let links = |root: &str, symbol: &str| -> Vec<(String, &'static str)> {
            let r = bus.rows(Path::new(root)).iter().find(|r| r.symbol == symbol).unwrap();
            bus.links(Path::new(root), r).into_iter().map(|l| (l.row.symbol, l.confidence)).collect()
        };
        // Tests are never linked.
        assert_eq!(links("/w/orders", "OrderService.Place"), [("PlacedConsumer".to_string(), "exact")]);
        assert_eq!(links("/w/billing", "PlacedConsumer"), [("OrderService.Place".to_string(), "exact")]);
        // A message type links to both sides.
        assert_eq!(links("/w/orders", "OrderPlaced"), [("PlacedConsumer".to_string(), "exact"), ("OrderService.Place".to_string(), "exact")]);
        // Another repo's `Refunded` is another message; a name with no declared type links by name only.
        assert_eq!(links("/w/orders", "Refunded"), [("Mixed".to_string(), "ambiguous")]);
        assert_eq!(links("/w/orders", "OrderService.Note"), [("NotedConsumer".to_string(), "ambiguous")]);
        let c = consumers(Path::new("/w/orders"), "OrderService.Place", "src/OrderService.Place.cs", 1, &ws);
        let got: Vec<(String, String)> = c.iter().map(|(e, l)| (e.message(), l.row.handler())).collect();
        assert_eq!(got, [("Shop.Orders.Events.OrderPlaced".to_string(), "PlacedConsumer.Consume".to_string())]);
    }

    /// A link is right only when both sides use one destination: a declared message type's virtual host, unless the
    /// code names another. Both named and different: no link; one inherited and different: ambiguous; one side's
    /// unknown: `same_destination`.
    #[test]
    fn links_check_the_destination_both_sides_use() {
        let link = |publish_queue: Option<&str>, consume_queue: Option<&str>, declared: Option<&str>| {
            let mut message = row("message", "Shipped", "Shipped", &["Shop.Events"]);
            message["queue"] = json!(declared);
            let mut publish = row("publish", "Shipper.Ship", "Shipped", &["Shop.Events"]);
            publish["queue"] = json!(publish_queue);
            let mut consume = row("consume", "ShippedConsumer", "Shipped", &["Shop.Events"]);
            consume["queue"] = json!(consume_queue);
            let ws = vec![repo("/w/a", vec![message, publish]), repo("/w/b", vec![consume])];
            let bus = Bus::new(&ws, None);
            let r = bus.rows(Path::new("/w/a")).iter().find(|r| r.kind == "publish").unwrap().clone();
            bus.links(Path::new("/w/a"), &r).into_iter().map(|l| (l.confidence, l.assumptions)).collect::<Vec<_>>()
        };
        let same = vec![Assumption::SameDestination];
        // Both inherit the declared host: one destination.
        assert_eq!(link(None, None, Some("Orders")), [("exact", Vec::new())]);
        // Named the same in code on both sides, whatever the type declares.
        assert_eq!(link(Some("Jobs"), Some("Jobs"), Some("Orders")), [("exact", Vec::new())]);
        // Published to another host than the consumer's (named in code on both sides): never delivered.
        assert!(link(Some("Jobs"), Some("Orders"), None).is_empty());
        // Published elsewhere than the host the consumer inherits from the type: may not be delivered.
        assert_eq!(link(Some("Jobs"), None, Some("Orders")), [("ambiguous", same.clone())]);
        // Nobody says: assumed.
        assert_eq!(link(None, None, None), [("exact", same.clone())]);
        // A queue on the consumer's host: the host matches, the queue is assumed.
        assert_eq!(link(Some("orders/refunds"), None, Some("Orders")), [("exact", same.clone())]);
    }

    #[test]
    fn overloads_publish_apart() {
        let mut ws = workspace();
        // A second `Place` overload, starting on line 20, publishing something else.
        let mut other = row("publish", "OrderService.Place", "Noted", &["Shop.Orders.Service"]);
        other["start_line"] = json!(20);
        ws[0] = repo("/w/orders", vec![row("publish", "OrderService.Place", "OrderPlaced", &["Shop.Orders.Events"]), other]);
        let handlers = |line: u32| -> Vec<String> {
            consumers(Path::new("/w/orders"), "OrderService.Place", "src/OrderService.Place.cs", line, &ws)
                .into_iter()
                .map(|(_, l)| l.row.handler())
                .collect()
        };
        assert_eq!(handlers(1), ["PlacedConsumer.Consume"]);
        assert_eq!(handlers(20), ["NotedConsumer.Consume"]);
        assert_eq!(handlers(0).len(), 2);
        let entries = |line: u32| for_symbol(Path::new("/w/orders"), "OrderService.Place", "src/OrderService.Place.cs", line, &ws).len();
        assert_eq!((entries(1), entries(20), entries(0)), (1, 1, 2));
    }
}
