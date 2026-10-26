//! Links HTTP routes across repos: request contracts a frontend sends, gateway DTOs, and requests made in code (gateway
//! calls by service and path, HTTP client calls with a URL) to the controller actions that serve them.
//!
//! A contract and an action match on verb and path, compared case-insensitively with route parameters (`{id}`,
//! `{orderId:int}`) as wildcards. The answer says how sure it is:
//!
//! - `exact`: one repo serves the route.
//! - `dominant`: several repos serve it, and this one serves more of the frontend's routes than any other: the
//!   frontend's backend, as its configured base URL would say.
//! - `ambiguous`: several repos serve it and none stands out, or another repo is the frontend's backend.
//!
//! Only repos whose checkout is there count (see [`crate::workspace::distinct`]): a repo indexed once and since
//! deleted neither serves nor requests anything, and where it would have (served the route linked, requested the
//! action), the answer names it as missing.
//!
//! A link matches verb and path only: that the client's requests reach the repo serving it (its base URL, host or
//! service) is checked only where the code names the service (a gateway contract or call naming a service exactly one
//! indexed repo hosts) or a host the repos' configuration maps (see below). Every other link assumes `same_deployment`.
//!
//! An endpoint answering every verb (minimal APIs' `Map`, verb `*`) serves a request of any verb on its path.
//!
//! Hosts: a client route's URL (or its typed client's base address) may name a host, and the repos' configuration
//! says which repo serves which host (see `kc_extract::HostFact`): launch profiles and Kestrel URLs, compose services
//! built from the repo, Aspire projects, ingress rules, and proxy host rules (HAProxy, nginx) routing a host to a
//! backend service, which is the repo hosting that service (its `service` registration), serving it as a host, or
//! named like it. A host exactly one repo serves decides:
//!
//! - a route naming that host links to that repo only, `exact`, when it serves the route;
//! - among several repos serving a route, the one serving a host the client repo's configuration names (a URL in its
//!   settings) is `dominant`, the others `ambiguous`.
//!
//! Such a link carries the host evidence (`host`: the host, where the client names it, where it is mapped to the repo)
//! and assumes `configured_host` instead of `same_deployment`: which environment's configuration applies, and (for the
//! repo's settings) which key the request reads, were not checked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kc_store::answer::Assumption;
use kc_store::query::RouteRow;
use serde::Serialize;

use crate::workspace::RepoSummary;

#[derive(Debug, Clone)]
pub struct RouteLink {
    pub repo: PathBuf,
    pub route: RouteRow,
    pub confidence: &'static str,
    /// What the link did not check: `same_deployment` unless the client names the service the repo hosts, or a host
    /// configuration maps to it (`configured_host`).
    pub assumptions: Vec<Assumption>,
    /// The host evidence the link rests on, when there is any.
    pub host: Option<HostLink>,
}

/// Why a link's requests are taken to reach the repo it goes to: the client names `host`, and configuration maps it
/// to that repo.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostLink {
    pub host: String,
    /// Where the client names the host: `the request's URL`, a typed client's base address, or a key of its repo's
    /// settings (`file:line`).
    pub client: String,
    /// Where the host is mapped to the serving repo: `deploy/haproxy.cfg:139 (proxy, to the service orders, which
    /// orders-api hosts)`.
    pub server: String,
}

/// What a link to a route assumes: nothing when the service the client names is the one the serving repo hosts
/// (`hosted`), else that both run in one deployment.
fn assumed(hosted: bool) -> Vec<Assumption> {
    if hosted { Vec::new() } else { vec![Assumption::SameDeployment] }
}

/// Whether a server route's key serves a client route's: the same path, and the same verb or every verb.
fn serves_key(server: &(String, String), client: &(String, String)) -> bool {
    server.1 == client.1 && (server.0 == client.0 || server.0 == "*")
}

/// The server routes serving key `k`: those of its verb, and those answering every verb in repos with none of its
/// verb. ASP.NET Core prefers an endpoint constrained to the request's verb over one answering every verb (a
/// conventionally routed action with no `[Http*]`, an admin app's `Orders/Index` at `Orders`), so the latter is no
/// candidate where the former serves the path.
fn candidates<'a>(served: &HashMap<(String, String), Vec<(&'a Path, &'a RouteRow)>>, k: &(String, String)) -> Vec<(&'a Path, &'a RouteRow)> {
    let mut out = served.get(k).cloned().unwrap_or_default();
    if k.0 != "*"
        && let Some(any) = served.get(&("*".to_string(), k.1.clone()))
    {
        let explicit: Vec<&Path> = out.iter().map(|(p, _)| *p).collect();
        out.extend(any.iter().copied().filter(|(p, _)| !explicit.contains(p)));
    }
    out
}

/// Lower-case letters and digits only: how a backend's name is compared with a service's or a repo's.
fn squash(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

type ServedHosts = HashMap<String, Vec<(PathBuf, String)>>;

thread_local! {
    /// The last [`served_hosts`] answer, by a fingerprint of the summaries it was read from: a `routes` answer asks
    /// once per client route.
    static SERVED_HOSTS: std::cell::RefCell<Option<(u64, std::rc::Rc<ServedHosts>)>> = const { std::cell::RefCell::new(None) };
}

/// A fingerprint of what the links are read from, for answers cached per thread.
fn fingerprint(summaries: &[RepoSummary]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in summaries {
        (&s.root, s.indexed_at, s.gone, &s.hosts, s.routes.len()).hash(&mut h);
    }
    h.finish()
}

/// The repos each host is served by (see the module docs), with where that is said, one entry per repo.
fn served_hosts(summaries: &[RepoSummary]) -> std::rc::Rc<ServedHosts> {
    let key = fingerprint(summaries);
    if let Some(hit) = SERVED_HOSTS.with(|c| c.borrow().as_ref().filter(|(k, _)| *k == key).map(|(_, v)| v.clone())) {
        return hit;
    }
    let v = std::rc::Rc::new(served_hosts_uncached(summaries));
    SERVED_HOSTS.with(|c| *c.borrow_mut() = Some((key, v.clone())));
    v
}

fn served_hosts_uncached(summaries: &[RepoSummary]) -> ServedHosts {
    let repos = distinct(summaries);
    let mut out: HashMap<String, Vec<(PathBuf, String)>> = HashMap::new();
    let mut add = |host: &str, repo: &Path, why: String| {
        let v = out.entry(host.to_string()).or_default();
        if !v.iter().any(|(r, _)| r == repo) {
            v.push((repo.to_path_buf(), why));
        }
    };
    // Hosts a repo serves itself, for backends named by a host.
    let own: Vec<(&str, &Path, String)> = repos
        .iter()
        .flat_map(|s| {
            s.hosts.iter().filter(|h| h.role == "serves" && h.target.is_none()).map(|h| (h.host.as_str(), s.root.as_path(), format!("{}:{}", h.file, h.line)))
        })
        .collect();
    for s in &repos {
        for h in s.hosts.iter().filter(|h| h.role == "serves") {
            let at = format!("{}:{} ({})", h.file, h.line, h.kind);
            let Some(target) = h.target.as_deref() else {
                add(&h.host, &s.root, at);
                continue;
            };
            let t = squash(target);
            let mut found = false;
            for r in &repos {
                let name = r.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let why = if r.routes.iter().any(|x| x.side == "service" && x.service.as_deref().is_some_and(|n| squash(n) == t)) {
                    Some(format!("{at}, to {target}, the service {name} hosts"))
                } else if let Some((_, _, w)) = own.iter().find(|(o, root, _)| *root == r.root.as_path() && o.eq_ignore_ascii_case(target)) {
                    Some(format!("{at}, to {target}, which {name} serves ({w})"))
                } else if squash(name.rsplit('.').next().unwrap_or(&name)) == t {
                    Some(format!("{at}, to {target}, named like {name}"))
                } else {
                    None
                };
                if let Some(why) = why {
                    add(&h.host, &r.root, why);
                    found = true;
                }
            }
            // An ingress rule in an app's own repo sends to that app.
            if !found && h.kind == "ingress" {
                add(&h.host, &s.root, at);
            }
        }
    }
    out
}

/// The one repo serving `host`, when exactly one does, and where that is said.
fn host_repo<'a>(host: &str, served: &'a HashMap<String, Vec<(PathBuf, String)>>) -> Option<&'a (PathBuf, String)> {
    match served.get(host).map(Vec::as_slice) {
        Some([one]) => Some(one),
        _ => None,
    }
}

/// Verb and normalized path, or `None` for a route whose path isn't known.
fn key(r: &RouteRow) -> Option<(String, String)> {
    let path = r.path.as_ref()?;
    let norm: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| if s.starts_with('{') { "{}".to_string() } else { s.to_ascii_lowercase() })
        .collect();
    Some((r.verb.to_ascii_uppercase(), norm.join("/")))
}

/// One summary per repo, gone checkouts left out (see [`crate::workspace::distinct`]).
pub(crate) fn distinct(summaries: &[RepoSummary]) -> Vec<&RepoSummary> {
    crate::workspace::distinct(summaries, &[])
}

fn identity_of(root: &Path, summaries: &[RepoSummary]) -> String {
    crate::workspace::identity_of(root, summaries)
}

/// Server routes by key, with the repo serving each, one checkout per repo.
fn served(summaries: &[RepoSummary]) -> HashMap<(String, String), Vec<(&Path, &RouteRow)>> {
    let mut map: HashMap<_, Vec<_>> = HashMap::new();
    for s in distinct(summaries) {
        for r in s.routes.iter().filter(|r| r.side == "server") {
            if let Some(k) = key(r) {
                map.entry(k).or_default().push((s.root.as_path(), r));
            }
        }
    }
    map
}

/// The repo serving strictly more of `client_repo`'s routes than any other.
fn dominant<'a>(client_repo: &Path, summaries: &'a [RepoSummary], served: &HashMap<(String, String), Vec<(&'a Path, &'a RouteRow)>>) -> Option<&'a Path> {
    let mine = summaries.iter().find(|s| s.root == client_repo)?;
    let mut counts: HashMap<&Path, usize> = HashMap::new();
    for r in mine.routes.iter().filter(|r| r.side == "client") {
        let Some(k) = key(r) else { continue };
        let mut repos: Vec<&Path> = candidates(served, &k).iter().map(|(p, _)| *p).collect();
        repos.sort();
        repos.dedup();
        for p in repos {
            *counts.entry(p).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&Path, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    match ranked.as_slice() {
        [(p, _)] => Some(*p),
        [(p, n), (_, m), ..] if n > m => Some(*p),
        _ => None,
    }
}

/// Whether the repo has a route of `side` matching key `k`: a client route `k` serves, or a server (or commented)
/// route serving `k`.
fn has_route(s: &RepoSummary, side: &str, k: &(String, String)) -> bool {
    s.routes.iter().any(|r| {
        r.side == side && key(r).is_some_and(|rk| if side == "client" { serves_key(k, &rk) } else { serves_key(&rk, k) })
    })
}

/// The repo hosting a service name, when exactly one does.
fn host(service: &str, summaries: &[RepoSummary]) -> Option<PathBuf> {
    let hosts: Vec<&RepoSummary> = distinct(summaries)
        .into_iter()
        .filter(|s| s.routes.iter().any(|r| r.side == "service" && r.service.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(service))))
        .collect();
    match hosts.as_slice() {
        [one] => Some(one.root.clone()),
        _ => None,
    }
}

/// Whether a repo may serve requests to `service`: it hosts that service, or no named service at all.
fn may_host(repo: &Path, service: &str, summaries: &[RepoSummary]) -> bool {
    let Some(s) = summaries.iter().find(|s| s.root == repo) else { return true };
    let mut names = s.routes.iter().filter(|r| r.side == "service").filter_map(|r| r.service.as_deref()).peekable();
    names.peek().is_none() || names.any(|n| n.eq_ignore_ascii_case(service))
}

/// The actions that serve a client route, most likely first. A gateway request naming a service that one repo
/// hosts is looked up only there; one naming a service no indexed repo hosts is never linked to a repo hosting
/// other services.
pub fn handlers(client: &RouteRow, client_repo: &Path, summaries: &[RepoSummary]) -> Vec<RouteLink> {
    let mut links = route_handlers(client, client_repo, summaries);
    // A URL read from a local: as if written in the call, and saying so.
    if client.local.is_some() {
        for l in &mut links {
            if !l.assumptions.contains(&Assumption::UrlLocal) {
                l.assumptions.push(Assumption::UrlLocal);
                l.assumptions.sort();
            }
        }
    }
    links
}

fn route_handlers(client: &RouteRow, client_repo: &Path, summaries: &[RepoSummary]) -> Vec<RouteLink> {
    if client.side == "operation" {
        return operation_handlers(client, client_repo, summaries);
    }
    let Some(k) = key(client) else { return Vec::new() };
    // A gone repo serving the route would have been a candidate.
    crate::workspace::note_missing_if(crate::workspace::gone(summaries), |g| {
        has_route(g, "server", &k) && client.service.as_deref().is_none_or(|svc| may_host(&g.root, svc, summaries))
    });
    let served = served(summaries);
    let cands = candidates(&served, &k);
    if cands.is_empty() {
        return Vec::new();
    }
    if let Some(service) = client.service.as_deref() {
        if let Some(host) = host(service, summaries) {
            return cands
                .iter()
                .filter(|(repo, _)| *repo == host)
                .map(|(repo, route)| RouteLink {
                    repo: repo.to_path_buf(),
                    route: (*route).clone(),
                    confidence: "exact",
                    assumptions: assumed(true),
                    host: None,
                })
                .collect();
        }
        let cands: Vec<_> = cands.iter().filter(|(repo, _)| may_host(repo, service, summaries)).cloned().collect();
        return by_hosts(rank(cands, client_repo, summaries, &served), client, client_repo, summaries);
    }
    // The client names a host one repo serves: the link is to that repo, when it serves the route.
    if let Some(h) = client.host.as_deref()
        && cands.iter().any(|(r, _)| *r != client_repo)
    {
        let hosts = served_hosts(summaries);
        // Within one repo, a host says nothing a link by path doesn't.
        if let Some((repo, server)) = host_repo(h, &hosts).filter(|(r, _)| r.as_path() != client_repo) {
            let mine: Vec<RouteLink> = cands
                .iter()
                .filter(|(r, _)| *r == repo.as_path())
                .map(|(r, route)| RouteLink {
                    repo: r.to_path_buf(),
                    route: (*route).clone(),
                    confidence: "exact",
                    assumptions: vec![Assumption::ConfiguredHost],
                    host: Some(HostLink {
                        host: h.to_string(),
                        client: client.host_at.as_ref().map_or_else(|| "the request's URL".to_string(), |at| format!("the client's base address ({at})")),
                        server: server.clone(),
                    }),
                })
                .collect();
            if !mine.is_empty() {
                return mine;
            }
        }
    }
    by_hosts(rank(cands, client_repo, summaries, &served), client, client_repo, summaries)
}

/// Links ranked by verb and path, with the host the client repo's settings name deciding among them (see the module
/// docs): the one repo among them serving such a host is `dominant` (or stays `exact`), with the evidence, and assumes
/// `configured_host`; the others are `ambiguous`.
fn by_hosts(mut links: Vec<RouteLink>, client: &RouteRow, client_repo: &Path, summaries: &[RepoSummary]) -> Vec<RouteLink> {
    let Some(mine) = summaries.iter().find(|s| s.root == client_repo) else { return links };
    let configured: Vec<&kc_store::query::HostRow> = mine.hosts.iter().filter(|h| h.role == "configures" && h.kind == "config").collect();
    // Within one repo, a host says nothing a link by path doesn't.
    if configured.is_empty() || client.host.is_some() || links.iter().all(|l| l.repo == client_repo) {
        return links;
    }
    let hosts = served_hosts(summaries);
    let mut found: Vec<(PathBuf, HostLink)> = Vec::new();
    for h in configured {
        if let Some((repo, server)) = host_repo(&h.host, &hosts)
            && repo.as_path() != client_repo
            && links.iter().any(|l| l.repo == *repo)
            && !found.iter().any(|(r, _)| r == repo)
        {
            let key = h.target.as_deref().map(|k| format!(", {k}")).unwrap_or_default();
            found.push((repo.clone(), HostLink { host: h.host.clone(), client: format!("its repo's settings ({}:{}{key})", h.file, h.line), server: server.clone() }));
        }
    }
    let [(repo, evidence)] = found.as_slice() else { return links };
    let several = links.iter().any(|l| l.repo != *repo);
    for l in &mut links {
        if l.repo == *repo {
            l.confidence = if several { "dominant" } else { l.confidence };
            l.assumptions = vec![Assumption::ConfiguredHost];
            l.host = Some(evidence.clone());
        } else {
            l.confidence = "ambiguous";
        }
    }
    links.sort_by_key(|l| (l.confidence == "ambiguous", l.repo.clone()));
    links
}

/// A server route's action and controller: `ItemsController.GetItems` is (`GetItems`, `Items`).
fn action_of(r: &RouteRow) -> (&str, &str) {
    let mut parts = r.symbol.rsplit('.');
    let action = parts.next().unwrap_or_default();
    let class = parts.next().unwrap_or_default();
    (action, class.strip_suffix("Controller").unwrap_or(class))
}

/// The actions an operation of a generated client may be (see `kc_extract::typescript`): those named like the
/// operation, under the controller named like the client's tag when one is, else under any controller. When the
/// client repo is known to call some server repos (see [`called`]), only theirs: an action of that name in a repo it
/// isn't known to call is another API's. The link rests on names (`operation_name`): `exact` when one action fits,
/// `ambiguous` when several do, or when the client has no tag and the operation is one word (`update`): nothing then
/// says the receiver is the API's client rather than another of the name. When no server is known, an untagged operation links to every action of that name,
/// `ambiguous`, assuming `server_unverified`: nothing says which server the client sends to.
///
/// An untagged method of an external client the repo only declares (a `.d.ts`, see [`RouteRow::detail`]) makes its
/// request inside code that wasn't read, so its URL isn't known: the server's `…Legacy` twin of the action (the same
/// operation under its older route) is as likely, and the links are `ambiguous` at most, assuming `client_unread`.
fn operation_handlers(client: &RouteRow, client_repo: &Path, summaries: &[RepoSummary]) -> Vec<RouteLink> {
    let Some(op) = client.path.as_deref() else { return Vec::new() };
    let unread = declared_untagged(client);
    let mut named = named_actions(op, summaries);
    if unread {
        named.extend(named_actions(&format!("{op}Legacy"), summaries));
    }
    let tagged: Vec<(&Path, &RouteRow)> = match client.service.as_deref() {
        Some(tag) => named.iter().filter(|(_, r)| squash(action_of(r).1) == squash(tag)).copied().collect(),
        None => Vec::new(),
    };
    let by_tag = !tagged.is_empty();
    let cands = if by_tag { tagged } else { named };
    let known = called(client_repo, summaries);
    // With no server known, an untagged operation's links say so; a tagged one's rest on its tag.
    let unverified = known.is_empty() && !by_tag;
    let cands: Vec<(&Path, &RouteRow)> =
        if known.is_empty() { cands } else { cands.into_iter().filter(|(repo, _)| known.iter().any(|k| k == repo)).collect() };
    // A one-word operation (`update`, `login`) not under the controller its tag names is a name many APIs' methods
    // share: never `exact`.
    let specific = by_tag || op.chars().skip(1).any(|c| c.is_ascii_uppercase() || c == '_');
    let confidence = if cands.len() == 1 && !unverified && specific && !unread { "exact" } else { "ambiguous" };
    let mut assumptions = vec![Assumption::SameDeployment, Assumption::OperationName];
    if unverified {
        assumptions.push(Assumption::ServerUnverified);
    }
    if unread {
        assumptions.push(Assumption::ClientUnread);
    }
    assumptions.sort();
    let mut links: Vec<RouteLink> = cands
        .into_iter()
        .map(|(repo, route)| RouteLink { repo: repo.to_path_buf(), route: route.clone(), confidence, assumptions: assumptions.clone(), host: None })
        .collect();
    links.sort_by(|a, b| (&a.repo, &a.route.symbol).cmp(&(&b.repo, &b.route.symbol)));
    links
}

/// Whether an operation is an untagged call to an external client the repo only declares (`detail` says where): its
/// request is made in code that wasn't read.
fn declared_untagged(op: &RouteRow) -> bool {
    op.side == "operation" && op.service.is_none() && op.detail.is_some()
}

/// Whether the operation `op` may be the action named `action`: of its name, or, for an external client the repo only
/// declares, its `…Legacy` twin (see [`operation_handlers`]).
fn may_be_action(op: &RouteRow, action: &str) -> bool {
    let Some(name) = op.path.as_deref() else { return false };
    action.eq_ignore_ascii_case(name)
        || declared_untagged(op)
            && action.get(..name.len()).is_some_and(|a| a.eq_ignore_ascii_case(name))
            && action.get(name.len()..).is_some_and(|rest| rest.eq_ignore_ascii_case("Legacy"))
}

/// The actions named `op` in any repo, one per action (an action routed under several paths is one action).
fn named_actions<'a>(op: &str, summaries: &'a [RepoSummary]) -> Vec<(&'a Path, &'a RouteRow)> {
    let mut named: Vec<(&Path, &RouteRow)> = Vec::new();
    for s in distinct(summaries) {
        for r in s.routes.iter().filter(|r| r.side == "server" && action_of(r).0.eq_ignore_ascii_case(op)) {
            if !named.iter().any(|(p, x)| *p == s.root.as_path() && x.symbol == r.symbol && x.file == r.file && x.start_line == r.start_line) {
                named.push((s.root.as_path(), r));
            }
        }
    }
    named
}

type Called = (u64, HashMap<PathBuf, std::rc::Rc<Vec<PathBuf>>>);

thread_local! {
    /// The last [`called`] answers, by a fingerprint of the summaries they were read from and the client repo: a
    /// `routes` answer asks once per operation.
    static CALLED: std::cell::RefCell<Called> = std::cell::RefCell::new((0, HashMap::new()));
}

/// The server repos the repo at `client_repo` is known to send requests to: itself when it serves routes, a repo it
/// depends on through its manifests (a client package the server publishes), the one repo serving one of its requests
/// by path (a route with a named segment), the one repo serving a host its requests or settings name, the repo hosting
/// a service its gateway requests name, and the one repo with the action a tagged operation of its names.
fn called(client_repo: &Path, summaries: &[RepoSummary]) -> std::rc::Rc<Vec<PathBuf>> {
    let key = fingerprint(summaries);
    if let Some(hit) = CALLED.with(|c| {
        let c = c.borrow();
        if c.0 == key { c.1.get(client_repo).cloned() } else { None }
    }) {
        return hit;
    }
    let v = std::rc::Rc::new(called_uncached(client_repo, summaries));
    CALLED.with(|c| {
        let mut c = c.borrow_mut();
        if c.0 != key {
            *c = (key, HashMap::new());
        }
        c.1.insert(client_repo.to_path_buf(), v.clone());
    });
    v
}

fn called_uncached(client_repo: &Path, summaries: &[RepoSummary]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: &Path| {
        if !out.iter().any(|o| o == p) {
            out.push(p.to_path_buf());
        }
    };
    let Some(mine) = summaries.iter().find(|s| s.root == client_repo) else { return out };
    if mine.routes.iter().any(|r| r.side == "server") {
        add(client_repo);
    }
    for d in crate::workspace::neighbours(client_repo, summaries).0 {
        if !d.gone && d.routes.iter().any(|r| r.side == "server") {
            add(&d.root);
        }
    }
    let served = served(summaries);
    let hosts = served_hosts(summaries);
    for r in &mine.routes {
        match r.side.as_str() {
            "client" => {
                if let Some(svc) = r.service.as_deref()
                    && let Some(h) = host(svc, summaries)
                {
                    add(&h);
                }
                if let Some((repo, _)) = r.host.as_deref().and_then(|h| host_repo(h, &hosts)) {
                    add(repo);
                }
                let Some(k) = key(r).filter(|k| k.1.split('/').any(|seg| !seg.is_empty() && seg != "{}")) else { continue };
                let mut repos: Vec<&Path> = candidates(&served, &k).iter().map(|(p, _)| *p).collect();
                repos.sort();
                repos.dedup();
                if let [one] = repos.as_slice() {
                    add(one);
                }
            }
            "operation" => {
                let (Some(op), Some(tag)) = (r.path.as_deref(), r.service.as_deref()) else { continue };
                let mut repos: Vec<&Path> =
                    named_actions(op, summaries).into_iter().filter(|(_, x)| squash(action_of(x).1) == squash(tag)).map(|(p, _)| p).collect();
                repos.sort();
                repos.dedup();
                if let [one] = repos.as_slice() {
                    add(one);
                }
            }
            _ => {}
        }
    }
    for h in mine.hosts.iter().filter(|h| h.role == "configures" && h.kind == "config") {
        if let Some((repo, _)) = host_repo(&h.host, &hosts) {
            add(repo);
        }
    }
    out
}

/// Candidate actions with how sure each link is, most likely first.
fn rank<'a>(
    cands: Vec<(&'a Path, &'a RouteRow)>,
    client_repo: &Path,
    summaries: &'a [RepoSummary],
    served: &HashMap<(String, String), Vec<(&'a Path, &'a RouteRow)>>,
) -> Vec<RouteLink> {
    let mut repos: Vec<&Path> = cands.iter().map(|(p, _)| *p).collect();
    repos.sort();
    repos.dedup();
    let top = if repos.len() > 1 { dominant(client_repo, summaries, served) } else { None };
    let mut links: Vec<RouteLink> = cands
        .iter()
        .map(|(repo, route)| RouteLink {
            repo: repo.to_path_buf(),
            route: (*route).clone(),
            confidence: match (repos.len(), top) {
                (1, _) => "exact",
                (_, Some(t)) if t == *repo => "dominant",
                _ => "ambiguous",
            },
            assumptions: assumed(false),
            host: None,
        })
        .collect();
    links.sort_by_key(|l| (l.confidence == "ambiguous", l.repo.clone()));
    links
}

/// The client routes (request contracts, in any repo) that a server route serves, with how sure each link is.
pub fn callers(server: &RouteRow, server_repo: &Path, summaries: &[RepoSummary]) -> Vec<RouteLink> {
    let Some(k) = key(server) else { return Vec::new() };
    let me = identity_of(server_repo, summaries);
    crate::workspace::note_missing_if(crate::workspace::gone(summaries), |g| has_route(g, "client", &k));
    let mut out = Vec::new();
    let (action, _) = action_of(server);
    for s in distinct(summaries) {
        let requests = |r: &&RouteRow| match r.side.as_str() {
            "client" => key(r).is_some_and(|ck| serves_key(&k, &ck)),
            "operation" => may_be_action(r, action),
            _ => false,
        };
        for c in s.routes.iter().filter(requests) {
            // The serving checkout may be another checkout of this repo; the route is matched by symbol, file and first
            // line (overloads share a name), and verb and path (an endpoint registered under several groups).
            if let Some(l) = handlers(c, &s.root, summaries).into_iter().find(|l| {
                identity_of(&l.repo, summaries) == me
                    && l.route.symbol == server.symbol
                    && l.route.file == server.file
                    && l.route.start_line == server.start_line
                    && (c.side == "operation" || key(&l.route) == key(server))
            }) {
                out.push(RouteLink { repo: s.root.clone(), route: c.clone(), confidence: l.confidence, assumptions: l.assumptions, host: l.host });
            }
        }
    }
    out
}

/// The commented-out `[Http*]` attributes matching a client route that nothing serves: the action it was sent to,
/// commented out, so the contract is likely left over. A gateway request is matched only in repos that may host its
/// service.
pub fn commented(client: &RouteRow, summaries: &[RepoSummary]) -> Vec<RouteLink> {
    let Some(k) = key(client) else { return Vec::new() };
    let host = client.service.as_deref().and_then(|s| host(s, summaries));
    if host.is_none() {
        crate::workspace::note_missing_if(crate::workspace::gone(summaries), |g| {
            has_route(g, "commented", &k) && client.service.as_deref().is_none_or(|svc| may_host(&g.root, svc, summaries))
        });
    }
    let mut out = Vec::new();
    for s in distinct(summaries) {
        let fits = match (&host, client.service.as_deref()) {
            (Some(h), _) => *h == s.root,
            (None, Some(service)) => may_host(&s.root, service, summaries),
            (None, None) => true,
        };
        if !fits {
            continue;
        }
        for r in s.routes.iter().filter(|r| r.side == "commented" && key(r).as_ref() == Some(&k)) {
            out.push(RouteLink { repo: s.root.clone(), route: r.clone(), confidence: "exact", assumptions: assumed(host.is_some()), host: None });
        }
    }
    out
}

/// Whether another client route in any indexed repo requests the same verb and path as `client` (of the repo at
/// `repo`): a frontend's own contract for a backend's DTO, say, so the DTO not being created doesn't leave the route
/// unused.
pub fn requested_elsewhere(client: &RouteRow, repo: &Path, summaries: &[RepoSummary]) -> bool {
    let Some(k) = key(client) else { return false };
    let me = identity_of(repo, summaries);
    let elsewhere = distinct(summaries).into_iter().any(|s| {
        let same_repo = s.identity() == me;
        s.routes.iter().any(|r| {
            r.side == "client" && key(r).as_ref() == Some(&k) && !(same_repo && r.symbol == client.symbol && r.file == client.file)
        })
    });
    if !elsewhere {
        crate::workspace::note_missing_if(crate::workspace::gone(summaries), |g| has_route(g, "client", &k));
    }
    elsewhere
}

/// Normalized path segments, route parameters as `{}`.
fn segments(r: &RouteRow) -> Option<Vec<String>> {
    key(r).map(|(_, p)| p.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect())
}

/// Whether `short` is a proper tail of `long`, segment by segment (`{}` matching anything), sharing a named segment:
/// a route whose prefix one side doesn't know (a base address, a base controller's `[Route]`, a path base).
fn tail_of(short: &[String], long: &[String]) -> bool {
    let tail = || short.iter().zip(&long[long.len() - short.len()..]);
    short.len() < long.len() && tail().all(|(a, b)| a == b || a == "{}" || b == "{}") && tail().any(|(a, b)| a == b && a != "{}")
}

/// Routes of `side` whose path is the other's with a prefix more or less (see [`tail_of`]), verbs compatible: the
/// nearest candidates when nothing matches exactly. At most three.
fn nearest<'a>(r: &RouteRow, side: &str, summaries: &'a [RepoSummary]) -> Vec<(&'a Path, &'a RouteRow)> {
    let (Some(k), Some(mine)) = (key(r), segments(r)) else { return Vec::new() };
    let mut out: Vec<(usize, &Path, &RouteRow)> = Vec::new();
    for s in distinct(summaries) {
        for x in s.routes.iter().filter(|x| x.side == side) {
            let (Some(xk), Some(theirs)) = (key(x), segments(x)) else { continue };
            let verbs = xk.0 == k.0 || xk.0 == "*" || k.0 == "*";
            if verbs && (tail_of(&mine, &theirs) || tail_of(&theirs, &mine)) {
                // Most named segments in common first.
                let named = mine.iter().rev().zip(theirs.iter().rev()).filter(|(a, b)| a == b && *a != "{}").count();
                out.push((named, s.root.as_path(), x));
            }
        }
    }
    out.sort_by_key(|(n, ..)| std::cmp::Reverse(*n));
    out.into_iter().take(3).map(|(_, p, x)| (p, x)).collect()
}

/// What an HTTP entry with no links leaves to check, in one line: the repos searched and how, the requests whose
/// URL is known only at run time, and the nearest routes by path (another prefix). `route` is the entry's own route
/// (`server` for `serves`, else a client or operation route). `brief` (one of many in a list): the nearest routes
/// only.
pub fn unlinked_note(route: &RouteRow, summaries: &[RepoSummary], brief: bool) -> Option<String> {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let listed = |names: Vec<String>| -> String {
        let n = names.len();
        let mut shown: Vec<String> = names.into_iter().take(4).collect();
        if n > 4 {
            shown.push(format!("+{}", n - 4));
        }
        shown.join(", ")
    };
    let site = |p: &Path, r: &RouteRow| format!("{} {}:{}", name(p), r.file, r.line);
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut parts = Vec::new();
    let server = route.side == "server";
    if server && !brief {
        let clients: Vec<&RepoSummary> =
            distinct(summaries).into_iter().filter(|s| s.routes.iter().any(|r| matches!(r.side.as_str(), "client" | "operation" | "unresolved"))).collect();
        if clients.is_empty() {
            parts.push("no indexed repo makes an HTTP request KnowCode reads (fetch, axios, $.ajax, HttpClient, generated clients)".to_string());
        } else {
            parts.push(format!(
                "searched {} repo{} with requests ({}) for URL literals and templates, base + path (fetch, axios, $.ajax, HttpClient, API wrappers)",
                clients.len(),
                plural(clients.len()),
                listed(clients.iter().map(|s| name(&s.root)).collect()),
            ));
            // Generated clients: their calls name an operation, matched to the action by name.
            let mut tags: Vec<String> = Vec::new();
            for r in clients.iter().flat_map(|s| s.routes.iter().filter(|r| r.side == "operation")) {
                let t = r.service.clone().unwrap_or_else(|| "untagged".to_string());
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
            parts.push(if tags.is_empty() {
                "no generated-client calls".to_string()
            } else {
                format!("generated clients ({}) by operation `{}`", listed(tags), action_of(route).0)
            });
            let unresolved: Vec<(&Path, &RouteRow)> =
                clients.iter().flat_map(|s| s.routes.iter().filter(|r| r.side == "unresolved").map(move |r| (s.root.as_path(), r))).collect();
            if !unresolved.is_empty() {
                let first: Vec<String> = unresolved.iter().take(2).map(|(p, r)| site(p, r)).collect();
                parts.push(format!(
                    "{} run-time URL{} unread ({}{})",
                    unresolved.len(),
                    plural(unresolved.len()),
                    first.join(", "),
                    if unresolved.len() > 2 { ", …" } else { "" }
                ));
            }
        }
    } else if !brief {
        let servers: Vec<&RepoSummary> = distinct(summaries).into_iter().filter(|s| s.routes.iter().any(|r| r.side == "server")).collect();
        let names = listed(servers.iter().map(|s| name(&s.root)).collect());
        parts.push(match (servers.len(), route.side.as_str()) {
            (0, _) => "no indexed repo serves HTTP routes KnowCode reads".to_string(),
            (n, "operation") => {
                let op = route.path.as_deref().unwrap_or_default();
                // Actions of that name in repos the client isn't known to call: another API's.
                let mut elsewhere: Vec<String> = named_actions(op, summaries).iter().map(|(p, _)| name(p)).collect();
                elsewhere.sort();
                elsewhere.dedup();
                if elsewhere.is_empty() {
                    format!("no action named `{op}` in {n} repo{} serving routes ({names})", plural(n))
                } else {
                    format!("actions named `{op}` only in repos it isn't known to call ({})", listed(elsewhere))
                }
            }
            (n, _) => format!("searched {n} repo{} serving routes ({names})", plural(n)),
        });
        let unplaced = distinct(summaries).into_iter().filter(|s| s.routes.iter().any(|r| r.side == "unsupported")).count();
        if unplaced > 0 {
            parts.push(format!("{unplaced} repo{} routes KnowCode can't place", if unplaced == 1 { " registers" } else { "s register" }));
        }
    }
    // A method the repo only declares (a `.d.ts`): an external client, whose code the workspace doesn't hold. Said even
    // in a list, so an answer doesn't silently drop the call.
    if route.side == "operation"
        && let Some(d) = route.detail.as_deref()
    {
        parts.push(format!("an external client: {d}; its code isn't in the workspace, so which route it requests, if any, isn't known"));
    }
    let near = nearest(route, if server { "client" } else { "server" }, summaries);
    if !near.is_empty() {
        let shown: Vec<String> = near
            .iter()
            .map(|(p, r)| {
                let at = if server { site(p, r) } else { format!("{} {}", name(p), r.symbol) };
                format!("{} /{} ({at})", r.verb, r.path.as_deref().unwrap_or_default())
            })
            .collect();
        parts.push(format!("nearest by path: {}", shown.join("; ")));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn is_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "struct" | "enum" | "protocol")
}

/// One HTTP fact about a symbol, for `context`.
#[derive(Debug, Clone)]
pub struct HttpEntry {
    /// `requests`: the symbol is a request contract, code that creates one, or a method making a request (a gateway or
    /// HTTP client call), and the links are the actions serving it.
    /// `serves`: the symbol is a controller action, and the links are the contracts that request it.
    pub role: &'static str,
    /// The contract or action the entry is about.
    pub symbol: String,
    pub verb: String,
    pub path: Option<String>,
    /// The service a gateway contract is sent to.
    pub service: Option<String>,
    pub links: Vec<RouteLink>,
    /// For a contract no action serves: the commented-out `[Http*]` attributes that served its route (see
    /// [`commented`]).
    pub commented_out: Vec<RouteLink>,
    /// The route itself, for [`unlinked_note`] when there are no links.
    pub route: RouteRow,
}

/// The HTTP facts about a symbol of the repo at `root`: the contract it is or creates (`requests`), or the action
/// it is (`serves`).
pub fn for_symbol(
    conn: &rusqlite::Connection,
    root: &Path,
    qualified: &str,
    symbol_id: i64,
    summaries: &[RepoSummary],
) -> anyhow::Result<Vec<HttpEntry>> {
    let Some(mine) = summaries.iter().find(|s| s.root == root) else { return Ok(Vec::new()) };
    // Contracts this code creates: `send(new ApiGetFilters(…))`, `SendAsync(new EP.DeleteSubscriber())`. A DTO that
    // is a bus message too, which the code publishes (`_bus.Publish(new X {…})`), goes on the bus, not over HTTP.
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT name FROM refs WHERE from_id = ?1 AND kind = 'instantiate'
         AND name NOT IN (SELECT message_name FROM bus_facts WHERE symbol_id = ?1 AND kind = 'publish' AND message_name IS NOT NULL)",
    )?;
    let created: Vec<String> = stmt.query_map([symbol_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    // A contract is a class; a method's own requests (a gateway or HTTP client call in it) are found by symbol.
    let created_contract =
        |r: &RouteRow| is_type_kind(&r.kind) && created.iter().any(|c| c == r.symbol.rsplit('.').next().unwrap_or(&r.symbol));
    // Overloads share a qualified name: a route is the symbol's own when it also starts on the symbol's first line.
    let start: Option<u32> = conn
        .prepare_cached("SELECT start_line FROM symbols WHERE id = ?1")?
        .query_map([symbol_id], |r| r.get(0))?
        .next()
        .transpose()?;
    let own = |r: &RouteRow| r.symbol == qualified && start.is_none_or(|l| l == r.start_line);
    let mut out = Vec::new();
    // Gateway DTOs ship in the serving repo's packages: look in the repos this one depends on, too.
    for s in crate::workspace::neighbours(root, summaries).0 {
        if s.gone {
            // A dependency whose checkout is gone: its contracts are not there to link.
            if s.routes.iter().any(|r| r.side == "client" && created_contract(r)) {
                crate::workspace::note_missing(&s.root);
            }
            continue;
        }
        for r in s.routes.iter().filter(|r| r.side == "client" && created_contract(r)) {
            out.push(requests(r, &s.root, summaries));
        }
    }
    for r in &mine.routes {
        match r.side.as_str() {
            "client" if own(r) || created_contract(r) => out.push(requests(r, root, summaries)),
            "operation" if own(r) => out.push(requests(r, root, summaries)),
            "server" if own(r) => {
                let links = callers(r, root, summaries);
                out.push(HttpEntry {
                    role: "serves",
                    symbol: r.symbol.clone(),
                    verb: r.verb.clone(),
                    path: r.path.clone(),
                    service: r.service.clone(),
                    route: r.clone(),
                    links,
                    commented_out: Vec::new(),
                })
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The `requests` entry of a client route of the repo at `repo`.
fn requests(r: &RouteRow, repo: &Path, summaries: &[RepoSummary]) -> HttpEntry {
    let links = handlers(r, repo, summaries);
    let commented_out = if links.is_empty() && is_type_kind(&r.kind) { commented(r, summaries) } else { Vec::new() };
    HttpEntry {
        role: "requests",
        symbol: r.symbol.clone(),
        verb: r.verb.clone(),
        path: r.path.clone(),
        service: r.service.clone(),
        route: r.clone(),
        links,
        commented_out,
    }
}

/// Whether the symbol `symbol_id` of the repo `conn` indexes is a controller action: a cheap check before reading
/// the workspace's routes.
pub fn serves(conn: &rusqlite::Connection, symbol_id: i64) -> anyhow::Result<bool> {
    let mut stmt = conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM http_routes WHERE symbol_id = ?1 AND side = 'server')")?;
    Ok(stmt.query_row([symbol_id], |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn route(side: &str, verb: &str, path: &str, service: Option<&str>, symbol: &str) -> serde_json::Value {
        json!({ "side": side, "verb": verb, "path": path, "service": service, "symbol": symbol, "kind": "method",
                "file": "src/a.cs", "start_line": 1, "end_line": 9, "line": 2 })
    }

    fn repo(root: &str, routes: Vec<serde_json::Value>) -> RepoSummary {
        serde_json::from_value(json!({ "root": root, "packages": [], "modules": [], "routes": routes, "stamp": "" })).unwrap()
    }

    /// A repo whose checkout is gone serves nothing: it neither makes a link ambiguous nor shows as one, and the
    /// answer linking the route it served names it missing; a gone repo serving other routes isn't named.
    #[test]
    fn a_gone_checkout_serves_nothing_and_is_noted_where_it_would_have() {
        let mut ws = vec![
            repo("/w/web", vec![route("client", "POST", "orders", None, "Api.placeOrder"), route("client", "GET", "items", None, "Api.items")]),
            repo("/w/api", vec![route("server", "POST", "orders", None, "OrdersController.Place"), route("server", "GET", "items", None, "ItemsController.List")]),
            repo("/w/extra", vec![route("server", "POST", "orders", None, "StaleController.Place"), route("server", "GET", "items", None, "StaleController.List")]),
            repo("/w/lonely", vec![route("server", "GET", "elsewhere", None, "ElsewhereController.Get")]),
        ];
        let link = |ws: &[RepoSummary]| -> Vec<(String, &'static str)> {
            let c = ws[0].routes.iter().find(|r| r.symbol == "Api.placeOrder").unwrap();
            handlers(c, Path::new("/w/web"), ws).into_iter().map(|l| (l.repo.to_string_lossy().into_owned(), l.confidence)).collect()
        };
        drop(crate::workspace::take_missing());
        assert_eq!(link(&ws), [("/w/api".to_string(), "ambiguous"), ("/w/extra".to_string(), "ambiguous")]);
        assert!(crate::workspace::take_missing().is_empty());
        ws[2].gone = true;
        ws[3].gone = true;
        assert_eq!(link(&ws), [("/w/api".to_string(), "exact")]);
        assert_eq!(crate::workspace::take_missing(), [PathBuf::from("/w/extra")]);
        // From the action's side: the same link, and the gone repo is no caller.
        let server = ws[1].routes.iter().find(|r| r.symbol == "OrdersController.Place").unwrap();
        let back: Vec<_> = callers(server, Path::new("/w/api"), &ws).into_iter().map(|l| (l.route.symbol, l.confidence)).collect();
        assert_eq!(back, [("Api.placeOrder".to_string(), "exact")]);
        drop(crate::workspace::take_missing());
    }

    /// A route matched by verb and path assumes both ends are one deployment; a gateway call naming the service the
    /// serving repo hosts has checked it.
    #[test]
    fn links_assume_one_deployment_unless_the_service_is_named() {
        let ws = vec![
            repo("/w/web", vec![route("client", "GET", "orders/{id}", None, "Api.getOrder"), route("client", "GET", "orders/{id}", Some("Orders"), "Gw.getOrder")]),
            repo("/w/orders", vec![route("server", "GET", "orders/{id}", None, "OrdersController.Get"), route("service", "", "", Some("Orders"), "Startup")]),
        ];
        let links = |symbol: &str| -> Vec<(&'static str, Vec<Assumption>)> {
            let c = ws[0].routes.iter().find(|r| r.symbol == symbol).unwrap();
            handlers(c, Path::new("/w/web"), &ws).into_iter().map(|l| (l.confidence, l.assumptions)).collect()
        };
        assert_eq!(links("Api.getOrder"), [("exact", vec![Assumption::SameDeployment])]);
        assert_eq!(links("Gw.getOrder"), [("exact", Vec::new())]);
        // From the action's side, the same links.
        let server = ws[1].routes.iter().find(|r| r.side == "server").unwrap();
        let mut back: Vec<(String, Vec<Assumption>)> = callers(server, Path::new("/w/orders"), &ws).into_iter().map(|l| (l.route.symbol, l.assumptions)).collect();
        back.sort();
        assert_eq!(back, [("Api.getOrder".to_string(), vec![Assumption::SameDeployment]), ("Gw.getOrder".to_string(), Vec::new())]);
    }

    /// An endpoint answering every verb (`Map`) serves requests of any verb on its path, and is found from both sides.
    #[test]
    fn an_any_verb_endpoint_serves_every_verb() {
        let ws = vec![
            repo("/w/web", vec![route("client", "DELETE", "hooks/{id}", None, "Api.drop"), route("client", "GET", "other", None, "Api.other")]),
            repo("/w/hooks", vec![route("server", "*", "hooks/{id:int}", None, "HookApi.Any")]),
        ];
        let drop = ws[0].routes.iter().find(|r| r.symbol == "Api.drop").unwrap();
        let links: Vec<_> = handlers(drop, Path::new("/w/web"), &ws).into_iter().map(|l| (l.route.symbol, l.confidence)).collect();
        assert_eq!(links, [("HookApi.Any".to_string(), "exact")]);
        let any = &ws[1].routes[0];
        let back: Vec<_> = callers(any, Path::new("/w/hooks"), &ws).into_iter().map(|l| l.route.symbol).collect();
        assert_eq!(back, ["Api.drop"]);
    }
}
