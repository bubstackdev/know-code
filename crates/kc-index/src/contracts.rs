//! Contract changes a diff makes, with the consumers in other repos they break: for `review`.
//!
//! An HTTP route the diff removes or changes is found by comparing the server routes each changed file declares before
//! the change (its version at the diff's base, extracted as indexing does: [`crate::diff::old_facts`]) with those it
//! declares now. The consumers are the client requests in other repos that the old route served: found by the links
//! `review` would have read before the change (the workspace's summaries, with the old route put back in the server
//! repo's, through [`crate::http::callers`]), and kept only when no route the repo serves now still answers them
//! ([`crate::http::handlers`] on the summaries as they are). Nothing here edits how links are made: both are the
//! link functions every answer uses.
//!
//! A consumer is `broken` when its request matched the old route exactly; `possible` when the link was by name or one
//! of several (`unique_name`, `ambiguous`). A route whose old path can't be placed (a prefix taken from a base class
//! that the change touched, a constant) is said in `unsettled`, never guessed.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use kc_store::Store;
use kc_store::query::RouteRow;
use serde::Serialize;

use crate::diff::Diff;
use crate::workspace::RepoSummary;

/// A route, message or table the diff removes or changes.
#[derive(Debug, Clone, Serialize)]
pub struct ContractChange {
    /// `http` (and later `bus`, `sql`).
    pub kind: &'static str,
    /// `GET api/items/{id}`.
    pub contract: String,
    /// `removed` or `changed`.
    pub change: &'static str,
    /// For a changed route, what it is now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Where it was declared: its file now (its file before the change when the change deletes it) and line.
    pub file: String,
    pub line: u32,
    pub consumers: Vec<Consumer>,
}

/// A request in another repo that the old contract served and nothing serves now.
#[derive(Debug, Clone, Serialize)]
pub struct Consumer {
    /// The repo's root.
    pub repo: String,
    pub file: String,
    pub line: u32,
    pub symbol: String,
    /// `broken` (the request matched the old route exactly) or `possible`.
    pub verdict: &'static str,
    pub confidence: &'static str,
    /// What the link rests on: `http_route`, with the assumptions it made (`same_deployment`).
    pub link: String,
}

#[derive(Debug, Default)]
pub struct Contracts {
    pub changes: Vec<ContractChange>,
    /// What couldn't be settled: a route whose old path can't be placed.
    pub unsettled: Vec<String>,
}

/// A server route as written in a file (its symbol, verb and template, before the query resolves prefixes and
/// constants), with its line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Written {
    symbol: String,
    verb: String,
    path: String,
    path_ref: Option<String>,
}

fn written(facts: &kc_extract::FileFacts) -> Vec<(Written, u32, (u32, u32))> {
    facts
        .http_routes
        .iter()
        .filter(|r| r.side == "server")
        .filter_map(|r| {
            let s = facts.symbols.get(r.sym?)?;
            let symbol = s.container.as_ref().map_or(s.name.clone(), |c| format!("{c}.{}", s.name));
            Some((Written { symbol, verb: r.verb.clone(), path: r.path.clone(), path_ref: r.path_ref.clone() }, r.line, (s.start_line, s.end_line)))
        })
        .collect()
}

/// The full path an old template had: itself when it is written out whole; else under the prefix the query gives a
/// route the file still declares in the same class (its resolved path less what its template writes after the
/// placeholder). `None` when nothing places it.
fn old_path(old: &Written, now: &[(Written, u32, (u32, u32))], rows: &[RouteRow]) -> Option<String> {
    if old.path_ref.is_some() {
        return None;
    }
    if !old.path.starts_with('{') || !(old.path.starts_with("{^") || old.path.starts_with("{=") || old.path.starts_with("{@")) {
        return Some(old.path.trim_matches('/').to_string());
    }
    if !old.path.starts_with("{^") {
        return None;
    }
    let rest = |p: &str| p.split_once('}').map(|(_, r)| r.trim_start_matches('/').to_string());
    let class = |s: &str| s.rsplit_once('.').map(|(c, _)| c.to_string());
    let old_rest = rest(&old.path)?;
    for (w, _, _) in now.iter().filter(|(w, ..)| w.path.starts_with("{^") && class(&w.symbol) == class(&old.symbol)) {
        let Some(their_rest) = rest(&w.path) else { continue };
        for r in rows.iter().filter(|r| r.side == "server" && r.symbol == w.symbol && r.verb == w.verb) {
            let Some(full) = r.path.as_deref() else { continue };
            let prefix = if their_rest.is_empty() { Some(full) } else { full.strip_suffix(&their_rest).map(|p| p.trim_end_matches('/')) };
            if let Some(prefix) = prefix {
                return Some(if old_rest.is_empty() { prefix.to_string() } else { format!("{prefix}/{old_rest}") });
            }
        }
    }
    None
}

/// The HTTP routes the diff of the repo at `root` removes or changes, with their consumers in the repos `summaries`
/// covers.
pub fn http(root: &Path, store: &Store, diff: &Diff, summaries: &[RepoSummary]) -> Result<Contracts> {
    let mut out = Contracts::default();
    let olds = crate::diff::old_facts(root, diff, |c| c.removes_lines || c.renamed_from.is_some() || c.deleted)?;
    if olds.iter().all(|o| o.facts.http_routes.iter().all(|r| r.side != "server")) {
        return Ok(out);
    }
    let rows = kc_store::query::http_routes(store.conn())?;
    let privacy = crate::privacy::Privacy::load(root);
    for old in &olds {
        let before = written(&old.facts);
        if before.is_empty() {
            continue;
        }
        // What the file declares now, read as indexing reads it; a deleted file declares nothing.
        let now: Vec<(Written, u32, (u32, u32))> = match &old.now {
            Some(file) if !privacy.blocks(file) => match std::fs::read(root.join(file)) {
                Ok(bytes) => written(&kc_extract::extract_file(old.lang, file, &kc_extract::decode(&bytes))),
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        };
        let kept: HashSet<&Written> = now.iter().map(|(w, ..)| w).collect();
        let here = old.now.clone().unwrap_or_else(|| old.file.clone());
        let file_rows: Vec<RouteRow> = rows.iter().filter(|r| r.file == here).cloned().collect();
        for (w, line, (start, end)) in before.iter().filter(|(w, ..)| !kept.contains(w)) {
            let Some(path) = old_path(w, &now, &file_rows) else {
                out.unsettled.push(format!("{} {} ({}:{line}): its path before the change can't be placed, so its consumers aren't read", w.verb, w.path, old.file));
                continue;
            };
            // Changed when the action still serves a route of its own, removed when it serves none.
            let still: Vec<&RouteRow> = file_rows.iter().filter(|r| r.side == "server" && r.symbol == w.symbol).collect();
            let (change, detail) = match still.first() {
                Some(r) => ("changed", Some(format!("now {} {}", r.verb, r.path.as_deref().unwrap_or("")))),
                None => ("removed", None),
            };
            let old_row = RouteRow {
                side: "server".into(),
                verb: w.verb.clone(),
                path: Some(path.clone()),
                service: None,
                symbol: w.symbol.clone(),
                kind: "method".into(),
                file: old.file.clone(),
                start_line: *start,
                end_line: *end,
                line: *line,
                host: None,
                host_at: None,
                detail: None,
                local: None,
            };
            // The links as they were: the old route put back among the server repo's routes.
            let mut before_summaries: Vec<RepoSummary> = summaries.to_vec();
            if let Some(s) = before_summaries.iter_mut().find(|s| s.root == root) {
                s.routes.push(old_row.clone());
            }
            let mut consumers = Vec::new();
            for l in crate::http::callers(&old_row, root, &before_summaries) {
                // Served still, by a route the repo serves now: not broken.
                if crate::http::handlers(&l.route, &l.repo, summaries).iter().any(|h| h.repo == root) {
                    continue;
                }
                let mut link = "http_route".to_string();
                for a in &l.assumptions {
                    if let Ok(serde_json::Value::String(a)) = serde_json::to_value(a) {
                        link.push_str(&format!(", {a}"));
                    }
                }
                consumers.push(Consumer {
                    repo: l.repo.to_string_lossy().into_owned(),
                    file: l.route.file.clone(),
                    line: l.route.line,
                    symbol: l.route.symbol.clone(),
                    verdict: if l.confidence == "exact" { "broken" } else { "possible" },
                    confidence: l.confidence,
                    link,
                });
            }
            out.changes.push(ContractChange { kind: "http", contract: format!("{} {path}", w.verb), change, detail, file: here.clone(), line: *line, consumers });
        }
    }
    Ok(out)
}
