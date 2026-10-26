//! Plain-text output. Compact on purpose: agents read this too.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use kc_store::manifests::{ManifestHit, RepoEdge};
use kc_store::query::{Context, FindHit, RefHit, SymbolRow};

use crate::repo_name;

/// `writeln!` into a `String`, which can't fail.
macro_rules! w {
    ($out:expr, $($arg:tt)*) => {{
        use std::fmt::Write as _;
        let _ = writeln!($out, $($arg)*);
    }};
}

/// A row's cited lines under it, each numbered (`    4 | this.log.info('placing');`), then what the cap left out. A site
/// quoted with the rest of its statement marks the statement's other lines with `-` (`    3 - await this.http`).
///
/// Lines quoted already in this answer (`seen`) aren't printed again: the row's own location says where they are.
fn cite_text(out: &mut String, indent: &str, cite: Option<&kc_store::answer::Cite>, seen: &mut std::collections::HashSet<String>) {
    let Some(c) = cite else { return };
    if c.text.is_some() && !seen.insert(format!("{}|{}", c.repo.as_deref().unwrap_or(""), c.at)) {
        return;
    }
    let first: u32 = c.at.rsplit_once(':').and_then(|(_, l)| l.split('-').next()).and_then(|l| l.parse().ok()).unwrap_or(0);
    for (i, line) in c.text.as_deref().unwrap_or("").lines().enumerate() {
        let n = first + i as u32;
        let mark = if c.line.is_some_and(|l| l != n) { '-' } else { '|' };
        w!(out, "{indent}  {n:>4} {mark} {line}");
    }
    if let Some(cut) = &c.cut {
        w!(out, "{indent}       … {cut}");
    }
    // Checked against the index: said when the lines drifted.
    if let Some(d) = &c.drift {
        w!(out, "{indent}       ! {}: {d}", c.check.as_str());
    }
}

/// [`cite_text`], printed.
fn print_cite(indent: &str, cite: Option<&kc_store::answer::Cite>) {
    let mut out = String::new();
    cite_text(&mut out, indent, cite, &mut std::collections::HashSet::new());
    print!("{out}");
}

fn loc(root: Option<&Path>, file: &str, line: u32) -> String {
    match root {
        Some(r) => format!("{}/{file}:{line}", repo_name(r)),
        None => format!("{file}:{line}"),
    }
}

/// A location in `repo`, when it's another repo than the one queried (reached across an HTTP route).
fn loc_in(root: Option<&Path>, repo: Option<&str>, file: &str, line: u32) -> String {
    match repo {
        Some(r) => format!("{r}/{file}:{line}"),
        None => loc(root, file, line),
    }
}

/// `http GET /path exact: new Contract` (or `: call` for code requesting the route itself), for an edge crossing an
/// HTTP route.
fn http_edge(h: &kc_store::trace::HttpVia) -> String {
    if h.call {
        return format!("http {} {}: call by {}", h.route(), h.link, h.contract);
    }
    format!("http {} {}: new {}", h.route(), h.link, h.contract)
}

/// `bus exact: Ns.Message (Queue)`, for an edge crossing the message bus.
fn bus_edge(b: &kc_store::trace::BusVia) -> String {
    let queue = b.queue.as_deref().map(|q| format!(" ({q})")).unwrap_or_default();
    format!("bus {}: {}{queue}", b.link, b.message)
}

/// `sql exact: write Order (shop.orders)` or `sql unique_name: call shop.place_order`, for an edge from a SQL object to
/// the code using it.
fn sql_edge(q: &kc_store::trace::SqlVia) -> String {
    match (&q.entity, &q.mode) {
        (Some(e), Some(m)) => format!("sql {}: {m} {e} ({})", q.link, q.object),
        (Some(e), None) => format!("sql {}: entity {e} ({})", q.link, q.object),
        _ => format!("sql {}: {} {}", q.link, q.how, q.object),
    }
}

/// The edge label of a trace node or affected symbol: the route, message or SQL object crossed, else the edge kind.
fn edge_label(
    edge: &str,
    http: Option<&kc_store::trace::HttpVia>,
    bus: Option<&kc_store::trace::BusVia>,
    sql: Option<&kc_store::trace::SqlVia>,
) -> String {
    match (http, bus, sql) {
        (Some(h), _, _) => http_edge(h),
        (_, Some(b), _) => bus_edge(b),
        (_, _, Some(q)) => sql_edge(q),
        _ => edge.to_string(),
    }
}

/// The qualified name, after its namespace or module where an answer sets one to tell same-named definitions apart.
fn qualified(s: &SymbolRow) -> String {
    match &s.namespace {
        Some(ns) => format!("{ns}.{}", s.qualified()),
        None => s.qualified(),
    }
}

pub fn at(chain: &[kc_store::query::SymbolAt], indexed: bool) {
    if chain.is_empty() {
        println!("{}", if indexed { "no symbol encloses that line" } else { "file not indexed" });
    }
    for s in chain {
        println!("{:<9} {}  {}:{}-{}", kw(&s.kind, &s.file), s.qualified, s.file, s.start_line, s.end_line);
        println!("          {}", s.signature);
    }
}

/// `capped`: more definitions matched than were read, so `total` is a lower bound and a match may be missing.
pub fn find(items: &[(PathBuf, FindHit)], total: usize, multi: bool, capped: bool, declared: &[String]) {
    let cap = || {
        if capped {
            println!(
                "… incomplete: more than {} definitions match; narrow it (Outer.name, a longer name, --kind)",
                kc_store::query::FIND_CANDIDATES
            );
        }
    };
    if items.is_empty() {
        println!("no matches");
        cap();
        return;
    }
    for (root, h) in items {
        let s = &h.symbol;
        let tags = if s.tags.is_empty() { String::new() } else { format!("  [{}]", s.tags) };
        println!("{:<9} {}  {}{tags}", kw(&s.kind, &s.file), qualified(s), loc(multi.then_some(root.as_path()), &s.file, s.line));
        println!("          {}", s.signature);
    }
    if total > items.len() {
        let more = if capped { "at least " } else { "" };
        println!("… {more}{} more (raise --limit or narrow with --kind)", total - items.len());
    }
    cap();
    for d in declared {
        println!("{d}");
    }
}

pub fn locate(d: &crate::ops::LocateData) {
    if d.terms.is_empty() {
        println!("no searchable words in the query");
        return;
    }
    if d.items.is_empty() {
        println!("no files match {}", d.terms.join(" "));
        return;
    }
    for (root, h) in &d.items {
        let file = match d.multi {
            true => format!("{}/{}", repo_name(root), h.file),
            false => h.file.clone(),
        };
        println!("{:>7.2}  {file}  [{}]", h.score, h.terms.join(" "));
        for s in &h.symbols {
            let name = match &s.container {
                Some(c) => format!("{c}.{}", s.name),
                None => s.name.clone(),
            };
            println!("           {:<9} {name}  :{}", kw(&s.kind, &h.file), s.line);
        }
    }
    if d.total > d.items.len() {
        println!("… {} more files match (raise --limit)", d.total - d.items.len());
    }
}

fn confidence_line(by_conf: &BTreeMap<&'static str, usize>) -> String {
    by_conf.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
}

/// [`ref_line`] for a hit in the queried repo, for tests.
#[cfg(test)]
pub(crate) fn ref_line_for_test(h: &RefHit) -> String {
    ref_line(None, h)
}

fn ref_line(root: Option<&Path>, h: &RefHit) -> String {
    let from = match &h.from {
        Some(f) => format!("in {}", match &f.container {
            Some(c) => format!("{c}.{}", f.name),
            None => f.name.clone(),
        }),
        None => "at top level".to_string(),
    };
    // An Ansible include or template is shown by the path as written.
    if matches!(h.kind.as_str(), "include" | "template") && h.receiver.is_some() {
        let mut h = h.clone();
        h.name = h.receiver.take().unwrap_or_default();
        return ref_line(root, &h);
    }
    // A role used through one of its task files: the role, then the file (`Configure.Orders (tasks_from x.yml)`).
    if h.kind == "role" && h.receiver.is_some() {
        let mut h = h.clone();
        h.name = format!("{} (tasks_from {})", h.name, h.receiver.take().unwrap_or_default());
        return ref_line(root, &h);
    }
    let recv = match h.receiver.as_deref() {
        Some(".") => ".".to_string(),
        Some(r) => format!("{r}."),
        None => String::new(),
    };
    let target = match h.targets.as_slice() {
        [] => String::new(),
        [t] => match &t.repo {
            Some(r) => format!(" → {} ({r}/{}:{})", t.qualified, t.file, t.line),
            None => format!(" → {} ({}:{})", t.qualified, t.file, t.line),
        },
        ts => {
            // Same-named candidates (copies of one role, template variants) are told apart by place.
            let same = ts.iter().any(|t| ts.iter().filter(|u| u.qualified == t.qualified).count() > 1);
            let each = |t: &kc_store::query::Target| {
                let file = match &t.repo {
                    Some(r) => format!("{r}/{}", t.file),
                    None => t.file.clone(),
                };
                if same { format!("{} ({file}:{})", t.qualified, t.line) } else { t.qualified.clone() }
            };
            format!(" → {} candidates: {}", ts.len(), ts.iter().map(each).collect::<Vec<_>>().join(", "))
        }
    };
    let at = match &h.repo {
        Some(r) => format!("{r}/{}:{}", h.file, h.line),
        None => loc(root, &h.file, h.line),
    };
    // The column tells two references on one line apart.
    let at = if h.col > 0 { format!("{at}:{}", h.col) } else { at };
    let bound = match &h.via_di {
        Some(b) => format!(" ⇒ {} via DI ({}:{})", b.target.qualified, b.target.file, b.target.line),
        // Several providers in scope: each, with where it is provided.
        None if !h.via_di_any.is_empty() => {
            let each: Vec<String> = h.via_di_any.iter().map(|b| format!("{} (provided at {})", b.target.qualified, b.registered)).collect();
            format!(" ⇒ one of {} via DI: {}", each.len(), each.join(", "))
        }
        None => String::new(),
    };
    // A SCIP index's answer; `corrected` when the heuristics had said otherwise.
    let precise = match (h.precise, h.corrected_from) {
        (true, Some(c)) => format!(" precise, corrected from {c}"),
        (true, None) => " precise".to_string(),
        _ => String::new(),
    };
    let through = h.through.as_ref().map(|t| format!("  through {t}")).unwrap_or_default();
    // Why the confidence is capped, when evidence that would decide it was missing.
    let basis = h.basis.map(|b| format!("  ({b})")).unwrap_or_default();
    // How a type reference uses the type: `type_ref (type_argument)` for `Get<IOrders>()`.
    let kind = match &h.usage {
        Some(u) => format!("{} ({u})", h.kind),
        None => h.kind.clone(),
    };
    format!("{at}  {kind} {recv}{}  {from}{through}  [{}{precise}{target}{bound}]{basis}", h.name, h.confidence)
}

pub fn refs(name: &str, d: &crate::ops::RefsData) {
    let (items, total, multi) = (&d.items, d.total, d.multi);
    println!("{total} references to {name} ({})", confidence_line(&d.by_confidence));
    if let Some(c) = &d.callers {
        println!("{c}");
    }
    if !d.overloads.is_empty() {
        println!("overloads:");
        for o in &d.overloads {
            println!("  {o}");
        }
    }
    for (root, h) in items {
        let test = if kc_store::is_test_path(&h.file) { "  (test)" } else { "" };
        println!("{}{test}", ref_line(multi.then_some(root.as_path()), h));
        print_cite("", h.cite.as_ref());
    }
    if total > items.len() {
        println!("… {} more (raise --limit)", total - items.len());
    }
    if let Some(serde_json::Value::Object(h)) = &d.hierarchy {
        for (k, rows) in h {
            println!("{}:", k.replace('_', " "));
            for r in rows.as_array().into_iter().flatten() {
                println!("  {}", r.as_str().unwrap_or(""));
            }
        }
    }
}

pub fn context(root: &Path, c: &Context, multi: bool) {
    let s = &c.symbol;
    let r = multi.then_some(root);
    println!("{} {}  {}", kw(&s.kind, &s.file), qualified(s), loc(r, &s.file, s.line));
    if let Some(doc) = &s.doc {
        println!("  /// {doc}");
    }
    if !s.attrs.is_empty() {
        println!("  attributes: {}", s.attrs.split(',').map(|a| format!("@{a}")).collect::<Vec<_>>().join(" "));
    }
    if !s.tags.is_empty() {
        println!("  tags: {}", s.tags);
    }
    if !c.relations.is_empty() {
        let rels: Vec<String> = c
            .relations
            .iter()
            .map(|rel| format!("{} {}{}", rel.kind, rel.target, if rel.external { " (external)" } else { "" }))
            .collect();
        println!("  {}", rels.join(", "));
    }
    let cut = if c.source_truncated { ", cut at the budget (raise --budget)" } else { "" };
    match c.source_mode {
        "none" => {}
        "skeleton" => {
            let lines = c.source_lines.as_deref().unwrap_or("");
            println!("--- skeleton ({}-{}; lines {lines} kept, bodies elided; --source full for all){cut}", s.line, s.end_line);
            println!("{}", c.source.trim_end());
            println!("---");
        }
        _ => {
            println!("--- source ({}-{}){cut}", s.line, s.end_line);
            println!("{}", c.source.trim_end());
            println!("---");
        }
    }
    if !c.parts.is_empty() {
        println!("partial, in {} parts: {}", c.parts.len(), c.parts.join(", "));
    }
    if !c.extensions.is_empty() {
        println!("extensions ({}):", c.extensions.len());
        for e in &c.extensions {
            println!("  {}  {}", e.signature, loc(r, &e.file, e.line));
        }
    }
    if c.members_total > 0 {
        println!("members ({}):", c.members_total);
        for m in &c.members {
            println!("  {:<9} {}  {}", kw(&m.kind, &m.file), m.signature, loc(r, &m.file, m.line));
        }
        if c.members_total > c.members.len() {
            println!("  … {} more", c.members_total - c.members.len());
        }
    }
    if !c.conformers.is_empty() {
        println!("conformers and subclasses ({}):", c.conformers.len());
        for m in &c.conformers {
            println!("  {:<9} {}  {}", kw(&m.kind, &m.file), qualified(m), loc(r, &m.file, m.line));
        }
    }
    if !c.conformers_indirect.is_empty() {
        println!("through other types ({}):", c.conformers_indirect_total.unwrap_or(c.conformers_indirect.len()));
        for m in &c.conformers_indirect {
            println!("  {:<9} {}  {}  via {}", kw(&m.kind, &m.file), m.name, loc(r, &m.file, m.line), m.via);
        }
    }
    if !c.conformers_elsewhere.is_empty() {
        println!("conformers and subclasses in other repos ({}):", c.conformers_elsewhere.len());
        for e in &c.conformers_elsewhere {
            let m = &e.symbol;
            println!("  {:<9} {}  {}:{}:{}", kw(&m.kind, &m.file), qualified(m), e.repo, m.file, m.line);
        }
    }
    let summary = &c.callers_summary;
    println!("callers ({}{}):", summary.total, if summary.total > 0 {
        format!("; {}", confidence_line(&summary.by_confidence))
    } else {
        String::new()
    });
    for h in &c.callers {
        println!("  {}", ref_line(r, h));
        if let Some(code) = &h.code {
            println!("      {code}");
        }
        print_cite("  ", h.cite.as_ref());
    }
    if summary.total > c.callers.len() {
        println!("  … {} more (knowcode refs {})", summary.total - c.callers.len(), s.name);
    }
    if c.callees_total > 0 {
        println!("callees ({}):", c.callees_total);
        for h in &c.callees {
            println!("  {}", ref_line(r, h));
            print_cite("  ", h.cite.as_ref());
        }
        if c.callees_total > c.callees.len() {
            println!("  … {} more", c.callees_total - c.callees.len());
        }
    }
    if let Some(di) = &c.di {
        println!("di ({} injection site{}):", di.injection_sites, if di.injection_sites == 1 { "" } else { "s" });
        if !di.bound.is_empty() {
            println!("  bound to: {}", di.bound.join(", "));
        }
        for (label, regs) in [("registered", &di.registrations), ("registered as", &di.registered_as)] {
            for g in regs {
                let what = match label {
                    "registered" => g.implementation.as_deref().unwrap_or("(factory)").to_string(),
                    _ => g.service.as_deref().unwrap_or("(scanned)").to_string(),
                };
                let mut notes = vec![g.lifetime.clone(), g.kind.clone()];
                if g.implemented_interfaces {
                    notes.push("implemented interfaces".into());
                }
                if g.test {
                    notes.push("test".into());
                }
                println!("  {label} {what} ({})  {}", notes.join(", "), loc(r, &g.file, g.line));
            }
        }
        for f in &di.flags {
            println!("  ! {f}");
        }
    }
    if !c.di_implementers.is_empty() {
        println!("registered through its hierarchy:");
        for d in &c.di_implementers {
            println!("  {d}");
        }
    }
    for n in &c.none_found {
        println!("{n}");
    }
    if !c.alternatives.is_empty() {
        println!("other definitions named {} (pick one with <file>#{}, <file>:<line> or <namespace>.{}):", s.name, s.name, s.qualified());
        for a in &c.alternatives {
            println!("  {:<9} {}  {}", kw(&a.kind, &a.file), qualified(a), loc(r, &a.file, a.line));
        }
    }
}

/// When no reference resolves to a callable: said, with how many references share its name.
pub fn named_like(c: &Context, n: Option<usize>) {
    let Some(n) = n else { return };
    if n > 0 {
        println!("no reference resolves to {}; {n} references named {} resolve elsewhere or not at all (knowcode refs {})", c.symbol.qualified(), c.symbol.name, c.symbol.name);
    } else {
        println!("no reference resolves to {}", c.symbol.qualified());
    }
}

/// The config file, cache directory, notes database and workspace roots `status` reports, printed once before the repos.
pub fn config_status(c: &serde_json::Value) {
    let s = |p: &str| c.pointer(p).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let exists = c["exists"].as_bool() == Some(true);
    println!("config   {}{}", s("/file"), if exists { "" } else { " (not created)" });
    let source = match s("/cache_dir/source").as_str() {
        "env" => "from KNOWCODE_CACHE_DIR".to_string(),
        "config" => "from config.toml".to_string(),
        "legacy" => "from the legacy cache_dir file".to_string(),
        other => other.to_string(),
    };
    println!("cache    {} ({source})", s("/cache_dir/path"));
    if let Some(bytes) = c.pointer("/cache_max_size/bytes").and_then(|n| n.as_u64()) {
        if bytes == 0 { println!("budget   unlimited (size eviction disabled)"); }
        else { println!("budget   {bytes} bytes (idle indexes evicted least recently used)"); }
    }
    let n = c.pointer("/notes/count").and_then(|v| v.as_u64()).unwrap_or(0);
    let from = match s("/notes/source").as_str() {
        "env" => "from KNOWCODE_DATA_DIR".to_string(),
        "config" => "from config.toml".to_string(),
        other => other.to_string(),
    };
    println!("notes    {} ({n} note{}; {from})", s("/notes/path"), if n == 1 { "" } else { "s" });
    let roots: Vec<&str> = c["workspace_roots"].as_array().map(|a| a.iter().filter_map(|r| r.as_str()).collect()).unwrap_or_default();
    if roots.is_empty() {
        println!("roots    none: the workspace is every indexed repo");
    } else {
        println!("roots    {}", roots.join(", "));
    }
    println!();
}

/// `status`'s privacy policy in one line: the file looked for and its state, the level, the threshold, what is blocked.
fn privacy_line(p: &serde_json::Value) -> String {
    let name = p["name"].as_str().unwrap_or("");
    let state = match p["file"].as_str().unwrap_or("") {
        "malformed" => format!("malformed, taken as {}", p["malformed_as"].as_str().unwrap_or("restricted")),
        other => other.to_string(),
    };
    let blocked = if p["blocked"] == "repo" {
        "the whole repo blocked".to_string()
    } else {
        match p["blocked_paths"].as_u64().unwrap_or(0) {
            0 => "nothing blocked".to_string(),
            1 => "1 path blocked".to_string(),
            n => format!("{n} paths blocked"),
        }
    };
    format!("{name} {state}; level {}, blocks at {} and below; {blocked}", p["global_level"], p["block_at"])
}

pub fn status(v: &serde_json::Value) {
    let s = |p: &str| v.pointer(p).cloned().unwrap_or_default();
    println!("repo     {}", s("/repo").as_str().unwrap_or(""));
    if let Some(p) = v.get("privacy") {
        println!("privacy  {}", privacy_line(p));
    }
    if v.pointer("/index/exists").and_then(|v| v.as_bool()) == Some(false) {
        let background = match s("/background").as_str() {
            Some("queued") => "; queued for background indexing",
            Some(_) => "; not indexed in the background: the cache is near cache.max_size",
            None => "",
        };
        println!("index    not built ({}{background})", s("/discovery").as_str().unwrap_or("unindexed"));
        for note in crate::ops::freshness_notes(v) { println!("{note}"); }
        return;
    }
    println!(
        "commit   {}{}",
        s("/commit").as_str().unwrap_or("(none)"),
        if s("/dirty").as_bool() == Some(true) { " (dirty)" } else { "" }
    );
    println!(
        "index    {} ({:.1} MB)",
        s("/index/path").as_str().unwrap_or(""),
        s("/index/bytes").as_f64().unwrap_or(0.0) / 1e6
    );
    println!(
        "fresh    {} stale, {} removed (checked in {} ms{})",
        s("/freshness/stale_files"),
        s("/freshness/removed_files"),
        s("/freshness/check_ms"),
        if s("/verified").as_bool() == Some(true) { ", every file hashed" } else { "" }
    );
    for f in v.pointer("/freshness/failed").and_then(|w| w.as_array()).map(Vec::as_slice).unwrap_or_default() {
        println!("           {} could not be read: its facts are as last indexed", f.as_str().unwrap_or(""));
    }
    println!(
        "coverage {} files, {} lines, {} skipped, {} with parse errors",
        s("/coverage/files"),
        s("/coverage/lines"),
        s("/coverage/skipped"),
        s("/coverage/with_parse_errors")
    );
    if let Some(worst) = v.pointer("/coverage/worst_parse_errors").and_then(|w| w.as_array()) {
        for w in worst {
            println!("           {} ({} errors)", w["file"].as_str().unwrap_or(""), w["errors"]);
        }
    }
    for f in v.pointer("/coverage/skipped_large").and_then(|w| w.as_array()).map(Vec::as_slice).unwrap_or_default() {
        println!("           {} not indexed: too large ({} bytes)", f["file"].as_str().unwrap_or(""), f["bytes"]);
    }
    for f in v.pointer("/coverage/skipped_symlinks").and_then(|w| w.as_array()).map(Vec::as_slice).unwrap_or_default() {
        println!("           {} not indexed: a symbolic link (never followed)", f.as_str().unwrap_or(""));
    }
    let lang = v.pointer("/coverage/by_lang").and_then(|m| m.as_object()).map(crate::ops::repo_lang).unwrap_or_default();
    let map = |p: &str| -> String {
        let mut parts: Vec<String> = v
            .pointer(p)
            .and_then(|m| m.as_object())
            .map(|m| m.iter().map(|(k, n)| format!("{} {n}", if p == "/symbols" { crate::ops::kind_word(k, &lang) } else { k })).collect())
            .unwrap_or_default();
        parts.sort();
        parts.join(", ")
    };
    // Empty counts are left out rather than printed as a bare label.
    for (label, p) in [("symbols", "/symbols"), ("refs", "/refs"), ("tags", "/tags")] {
        let m = map(p);
        if !m.is_empty() {
            println!("{label:<8} {m}");
        }
    }
    println!("other    {} relations, {} imports; extractors {}", s("/relations"), s("/imports"), map("/extractor_versions"));
    if v.get("scip").is_some() {
        match v.pointer("/scip/error").and_then(|e| e.as_str()) {
            Some(e) => println!("scip     {} unreadable: {e}", s("/scip/source").as_str().unwrap_or("")),
            None => println!(
                "scip     {} ({}): {} of {} covered files unchanged since; {} reference sites{}",
                s("/scip/source").as_str().unwrap_or(""),
                s("/scip/tool").as_str().unwrap_or(""),
                s("/scip/coverage/current"),
                s("/scip/coverage/files"),
                s("/scip/coverage/refs"),
                match v.pointer("/scip/agreement") {
                    Some(a) => format!("; {} upgraded to exact, {} corrected", a["upgraded"], a["corrected"]),
                    None => String::new(),
                },
            ),
        }
        if let Some(stale) = v.pointer("/scip/coverage/stale_projects").and_then(|s| s.as_array()).filter(|s| !s.is_empty()) {
            let names: Vec<&str> = stale.iter().filter_map(|s| s.as_str()).collect();
            println!("         stale: inputs changed since the SCIP index was made, so it isn't applied to {}; run the indexer again", names.join(", "));
        }
    }
    println!();
}

pub fn deps(packages: &serde_json::Map<String, serde_json::Value>, edges: &[(PathBuf, RepoEdge)]) {
    // Built whole and printed once: over every repo it is tens of thousands of lines, and stdout flushes each line.
    let mut out = String::new();
    for (repo, pkgs) in packages {
        let pkgs = pkgs.as_array().map(Vec::as_slice).unwrap_or_default();
        if pkgs.is_empty() {
            w!(out, "{repo}: no manifests");
            continue;
        }
        w!(out, "{repo}:");
        for p in pkgs {
            let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
            let targets = p["targets"].as_array().map(Vec::as_slice).unwrap_or_default();
            let names: Vec<String> = targets.iter().map(|t| format!("{} ({})", t["name"].as_str().unwrap_or(""), t["kind"].as_str().unwrap_or(""))).collect();
            w!(out, "  {} {}  {}:{}", s("kind"), s("name"), s("file"), p["line"]);
            if !names.is_empty() {
                w!(out, "    targets: {}", names.join(", "));
            }
            for d in p["deps"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let loc = d["location"].as_str().unwrap_or("");
                let req = d["requirement"].as_str().map(|r| format!(" {r}")).unwrap_or_default();
                let to = d["resolved_repo"].as_str().map(|r| format!(" -> {}", Path::new(r).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())).unwrap_or_default();
                let blocked = d["blocked"].as_str().map(|b| format!(" ({b})")).unwrap_or_default();
                w!(out, "    dep {} [{}{}{}]{}{blocked}  line {}", d["name"].as_str().unwrap_or(""), d["source"].as_str().unwrap_or(""), if loc.is_empty() { String::new() } else { format!(" {loc}") }, req, to, d["line"]);
            }
        }
    }
    w!(out, "repo edges ({}):", edges.len());
    for (from, e) in edges {
        let to = Path::new(&e.to_repo).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let products = if e.products.is_empty() { String::new() } else { format!("; products {}", e.products.join(", ")) };
        let blocked = if kc_index::privacy::Privacy::load(Path::new(&e.to_repo)).whole() { format!(" ({})", crate::ops::BLOCKED_NOTE) } else { String::new() };
        w!(out, "  {} -> {}{blocked}  {}:{}  ({} depends on {}{products})", repo_name(from), to, e.file, e.line, e.package, e.dependency);
        // A few sites say where the dependency is used; the full list is in the JSON.
        const SHOWN: usize = 8;
        let n = e.import_sites.len();
        if n > SHOWN {
            let files: std::collections::HashSet<&str> = e.import_sites.iter().filter_map(|s| s.split(':').next()).collect();
            w!(out, "    {n} import sites in {} files: {}, … ({} more with --json)", files.len(), e.import_sites[..SHOWN].join(", "), n - SHOWN);
        } else if n > 0 {
            w!(out, "    {n} import sites: {}", e.import_sites.join(", "));
        }
    }
    print!("{out}");
}

pub fn manifest_hits(hits: &[ManifestHit]) {
    if hits.is_empty() {
        return;
    }
    println!("manifests:");
    for h in hits {
        let pkg = if h.kind == "package" { String::new() } else { format!(" in {}", h.package) };
        println!("  {:<9} {}{pkg}  {}:{}", h.kind, h.name, h.file, h.line);
    }
}

/// A trace as an indented tree: each node under the node it hangs off, with how it relates to it read from the
/// parent (`calls`, `called by`, `requested by`, …), its confidence and the site.
pub fn trace_text(root: &Path, t: &kc_store::trace::Trace, multi: bool) -> String {
    let r = multi.then_some(root);
    let mut out = String::new();
    let mut seen = std::collections::HashSet::new();
    for (dir, label) in [("", ""), ("out", "reaches (out):"), ("in", "reached by (in):")] {
        let nodes: Vec<&kc_store::trace::TraceNode> = t.nodes.iter().filter(|n| n.direction == dir).collect();
        if nodes.is_empty() {
            continue;
        }
        if !label.is_empty() {
            w!(out, "{label}");
        }
        for n in nodes {
            let indent = "  ".repeat(n.depth as usize);
            let tag = if n.edge.is_empty() {
                String::new()
            } else {
                format!(
                    "{} ",
                    module_label(n.edge, n.direction, n.module.as_ref()).unwrap_or_else(|| relation_label(
                        n.edge,
                        n.direction,
                        n.http.as_ref(),
                        n.bus.as_ref(),
                        n.sql.as_ref(),
                        n.role.as_ref(),
                        n.deploy.as_ref()
                    ))
                )
            };
            let conf = if n.confidence.is_empty() { String::new() } else { format!("  [{}]", n.confidence) };
            let site_repo = n.site_repo.as_deref().or(n.repo.as_deref());
            let site = n.site.as_deref().map(|s| format!(" at {}", in_repo(site_repo, s))).unwrap_or_default();
            let binding = n.binding.as_deref().map(|b| format!(" (bound at {b})")).unwrap_or_default();
            w!(out, "{indent}{tag}{} {}  {}{conf}{site}{binding}", kw(&n.kind, &n.file), n.qualified, loc_in(r, n.repo.as_deref(), &n.file, n.line));
            cite_text(&mut out, &indent, n.cite.as_ref(), &mut seen);
        }
    }
    if t.nodes.len() == 1 {
        w!(out, "nothing reaches it or is reached from it within the depth, direction and filters asked for");
    }
    if t.truncated {
        w!(out, "… truncated at the node limit: at least {} more neighbours left out (raise --limit, lower --depth, or filter with --edges/--kinds)", t.omitted);
    }
    out
}

pub fn trace(root: &Path, t: &kc_store::trace::Trace, multi: bool) {
    print!("{}", trace_text(root, t, multi));
}

/// For a Terraform module crossing, how a node relates to its parent: `used by (module.cdn source … unique_name)`.
fn module_label(edge: &str, direction: &str, module: Option<&kc_store::trace::ModuleVia>) -> Option<String> {
    let m = module?;
    Some(format!("{} ({} source {} {})", kc_store::trace::relation(edge, direction), m.module, m.source, m.link))
}

/// How a node relates to its parent, read from the parent: `called by`, or for a crossing, the relation and what
/// it crosses (`requested by (http POST /orders exact: new PlaceOrder)`).
fn relation_label(
    edge: &str,
    direction: &str,
    http: Option<&kc_store::trace::HttpVia>,
    bus: Option<&kc_store::trace::BusVia>,
    sql: Option<&kc_store::trace::SqlVia>,
    role: Option<&kc_store::trace::RoleVia>,
    deploy: Option<&kc_store::trace::DeployVia>,
) -> String {
    let rel = kc_store::trace::relation(edge, direction);
    let rel = if rel.is_empty() { edge } else { rel };
    if let Some(r) = role {
        return format!("{rel} (role {} {})", r.role, r.link);
    }
    if let Some(d) = deploy {
        let app = d.app.as_deref().map(|a| format!(" as {a}")).unwrap_or_default();
        return format!("{rel} (runs {}{app} {})", d.playbook, d.link);
    }
    if http.is_some() || bus.is_some() || sql.is_some() {
        format!("{rel} ({})", edge_label(edge, http, bus, sql))
    } else {
        rel.to_string()
    }
}

/// One affected symbol's line in the impact tree.
fn affected_line(root: Option<&Path>, a: &kc_store::trace::Affected) -> String {
    let mut site = a.site.as_deref().map(|s| format!(" at {}", in_repo(a.repo.as_deref(), s))).unwrap_or_default();
    if let Some(first) = a.site.as_deref()
        && !a.also.is_empty()
    {
        // Its other sites: lines of the same file by number, any other file in full.
        let file = first.rsplit_once(':').map_or(first, |(f, _)| f);
        let lines: Vec<&str> = a.also.iter().map(|s| s.strip_prefix(file).and_then(|r| r.strip_prefix(':')).unwrap_or(s)).collect();
        site.push_str(&format!(" (also {})", lines.join(", ")));
    }
    if !a.copies.is_empty() {
        site.push_str(&format!(" (also copies at {})", a.copies.join(", ")));
    }
    if let Some(h) = a.nearest_ambiguous_hop {
        site.push_str(&format!(" (ambiguous at hop {h})"));
    }
    if a.via_removed {
        site.push_str(&format!(" (names {}, which the change removes)", a.via));
    }
    format!(
        "{} {} {}  {}  [{}] {}{site}",
        module_label(a.edge, "in", a.module.as_ref()).unwrap_or_else(|| relation_label(
            a.edge,
            "in",
            a.http.as_ref(),
            a.bus.as_ref(),
            a.sql.as_ref(),
            a.role.as_ref(),
            a.deploy.as_ref()
        )),
        kw(&a.kind, &a.file),
        a.qualified,
        loc_in(root, a.repo.as_deref(), &a.file, a.line),
        a.confidence,
        a.class,
    )
}

/// An impact as text: the counts, then the affected symbols as a tree under the changed symbols (each under the
/// symbol it was reached from, in the answer's order), then cross-repo references, coupled files and warnings.
pub fn impact_text(repos: &[crate::ops::ImpactRepo], multi: bool) -> String {
    let mut out = String::new();
    let mut seen = std::collections::HashSet::new();
    if repos.is_empty() {
        w!(out, "nothing to analyse: no symbol, file or change matched in the repos searched");
    }
    for r in repos {
        let root = multi.then_some(r.root.as_path());
        let i = &r.impact;
        if !r.others.is_empty() {
            w!(out, "also defined (analyse one with path#Name): {}", r.others.join(", "));
        }
        if !r.changes.is_empty() {
            let files: Vec<String> = r
                .changes
                .iter()
                .map(|c| match &c.renamed_from {
                    _ if c.deleted => format!("{} (deleted)", c.file),
                    Some(from) => format!("{} (renamed from {from})", c.file),
                    None => c.file.clone(),
                })
                .collect();
            w!(out, "changed: {}", files.join(", "));
        }
        let counts: Vec<String> = ["direct", "near", "far", "test"]
            .iter()
            .filter_map(|c| i.counts.get(c).map(|n| format!("{c} {n}")))
            .collect();
        // Proximity is graph distance (direct: hop 1, near: 2, far: 3+), never a judgement of risk.
        w!(
            out,
            "by proximity: {}{}{}; low-confidence {}{}{}",
            if counts.is_empty() { "nothing affected".to_string() } else { counts.join(", ") },
            if r.cross_repo.is_empty() { String::new() } else { format!(", cross-repo {}", r.cross_repo.len()) },
            if r.coupled.is_empty() { String::new() } else { format!(", coupled {}", r.coupled.len()) },
            i.low_confidence,
            if i.truncated { format!(" (counts of the {} shown: the walk stopped at the limit; raise -n for all)", i.affected.len()) } else { String::new() },
            if r.hidden > 0 { format!("; {} hidden by --kinds", r.hidden) } else { String::new() },
        );
        // Children of each symbol, in the answer's order: the affected symbols reached from it one hop further.
        let mut children: HashMap<(&str, u8), Vec<usize>> = HashMap::new();
        for (k, a) in i.affected.iter().enumerate() {
            children.entry((a.via.as_str(), a.hop.saturating_sub(1))).or_default().push(k);
        }
        let mut shown = vec![false; i.affected.len()];
        let tree = |out: &mut String, start: &str, shown: &mut Vec<bool>, seen: &mut std::collections::HashSet<String>| {
            let mut stack: Vec<(usize, usize)> = Vec::new();
            let push = |stack: &mut Vec<(usize, usize)>, key: (&str, u8), depth: usize, shown: &[bool]| {
                if let Some(kids) = children.get(&key) {
                    stack.extend(kids.iter().rev().filter(|k| !shown[**k]).map(|k| (*k, depth)));
                }
            };
            push(&mut stack, (start, 0), 1, shown);
            while let Some((k, d)) = stack.pop() {
                if shown[k] {
                    continue;
                }
                shown[k] = true;
                let a = &i.affected[k];
                w!(out, "{}{}", "  ".repeat(d), affected_line(root, a));
                cite_text(out, &"  ".repeat(d), a.cite.as_ref(), seen);
                push(&mut stack, (a.qualified.as_str(), a.hop), d + 1, shown);
            }
        };
        for (n, s) in i.scope.iter().enumerate() {
            // Past the first eight, a changed symbol is listed only when something depends on it.
            if n >= 8 && !children.contains_key(&(s.qualified.as_str(), 0)) {
                continue;
            }
            w!(out, "{} {}  {}", kw(&s.kind, &s.file), s.qualified, loc(root, &s.file, s.line));
            cite_text(&mut out, "", s.cite.as_ref(), &mut seen);
            tree(&mut out, &s.qualified, &mut shown, &mut seen);
        }
        // Definitions the change removed, with the code still naming them under each.
        let at = r.base.as_deref().map(|b| &b[..b.len().min(10)]).unwrap_or("base");
        for g in &r.removed {
            w!(out, "removed {} {}  {} at {at}  [{} still name it]", g.kind, g.qualified, loc(root, &g.file, g.line), g.named_by.len());
            tree(&mut out, &g.qualified, &mut shown, &mut seen);
            // Reached from a changed symbol first, so listed under it; named here too.
            for n in g.named_by.iter().filter(|n| !i.affected.iter().any(|a| a.via_removed && a.via == g.qualified && a.qualified == n.qualified)) {
                w!(out, "  names it {}  {}  [{}] at {} (also listed above)", n.qualified, loc(root, &n.file, n.line), n.confidence, n.site);
            }
        }
        if !r.removed.is_empty() {
            w!(out, "  (pre-change side {})", crate::removed::PRE_CHANGE);
        }
        let quiet = i.scope.iter().skip(8).filter(|s| !children.contains_key(&(s.qualified.as_str(), 0))).count();
        if quiet > 0 {
            w!(out, "… {quiet} more changed symbols with nothing depending on them (the JSON answer lists them)");
        }
        // Reached from a symbol not in the tree (a crossing's landing, say): listed flat.
        for (k, a) in i.affected.iter().enumerate() {
            if !shown[k] {
                w!(out, "  {}  (via {})", affected_line(root, a), a.via);
                cite_text(&mut out, "  ", a.cite.as_ref(), &mut seen);
            }
        }
        for h in &r.cross_repo {
            let class = if kc_store::is_test_path(&h.file) { "test" } else { "direct" };
            w!(out, "  {class:<8} cross-repo {}", ref_line(root, h));
            cite_text(&mut out, "  ", h.cite.as_ref(), &mut seen);
        }
        for c in &r.coupled {
            w!(
                out,
                "  {:<8} {}{}  [changed with {} in {} of {} commits]",
                c.class,
                loc_file(root, &c.file),
                if matches!(c.kind.as_str(), "template" | "style" | "javascript") { format!(" ({})", c.kind) } else { String::new() },
                c.with,
                c.support,
                c.commits
            );
        }
        for m in &r.missed_partners {
            w!(out, "warning: {}", m.message);
        }
        for n in &r.notes {
            w!(out, "warning: {}", n.line());
        }
        for t in &r.tables {
            w!(out, "table {} ({}) is read and written by:", t.table, t.entity);
            for a in &t.accessors {
                w!(out, "  {:<6} {}  {}  [{}]{}", a.mode, a.symbol, loc(root, &a.file, a.line), a.confidence, a.note());
            }
        }
        // Where the container makes (or hands out) the types on the path: no reference the walk follows says so.
        for g in &r.registrations {
            let what = match (g.role, &g.service, &g.implementation) {
                ("implementation", Some(s), _) => format!("{} as {s}", g.ty),
                ("service", _, Some(i)) => format!("{i} as {}", g.ty),
                _ => g.ty.clone(),
            };
            let test = if g.test { ", test" } else { "" };
            w!(out, "  registered {what} ({}{test})  {}  [type on the path at hop {}]", g.lifetime, loc(root, &g.file, g.line), g.hop);
        }
        // Where the walk stopped with dependents it didn't list (the `incomplete` lines say how to go on).
        for f in &r.frontier {
            let why = if f.reason == "depth" { "at the depth asked" } else { "reached only by ambiguous edges" };
            let repo = f.repo.as_deref().map(|r| format!("{r}: ")).unwrap_or_default();
            w!(out, "  stopped at {} {repo}{}  {}  [{why}; {}+ dependents not listed]", kw(&f.kind, &f.file), f.qualified, loc(root, &f.file, f.line), f.beyond);
        }
        if i.truncated {
            w!(out, "  … truncated at the symbol limit (raise --limit, lower --depth, or filter with --edges/--kinds)");
        }
    }
    out
}

pub fn impact(repos: &[crate::ops::ImpactRepo], multi: bool) {
    print!("{}", impact_text(repos, multi));
}

fn loc_file(root: Option<&Path>, file: &str) -> String {
    match root {
        Some(r) => format!("{}/{file}", repo_name(r)),
        None => file.to_string(),
    }
}

pub fn coupled(repos: &[crate::ops::CoupledRepo], file: &str) {
    if repos.is_empty() {
        println!("no file {file} in the repo");
    }
    let multi = repos.len() > 1;
    for r in repos {
        let root = multi.then_some(r.root.as_path());
        coupled_partners(r, root);
        // Structural guesses, apart from history and said to be guesses.
        if let Some((note, g)) = &r.fallback {
            println!("{note}");
            if !g.list.is_empty() {
                println!("  guesses by structure (low confidence, not history):");
            }
            for x in &g.list {
                let rels: Vec<String> = x
                    .relations
                    .iter()
                    .map(|rel| if rel.names.is_empty() { rel.how.to_string() } else { format!("{} {}", rel.how, rel.names.join(", ")) })
                    .collect();
                println!("    {}  ({})", loc_file(root, &x.file), rels.join("; "));
            }
            if g.total > g.list.len() {
                println!("    … {} more", g.total - g.list.len());
            }
        }
    }
}

/// The history partners of one repo's answer: co-change evidence only.
fn coupled_partners(r: &crate::ops::CoupledRepo, root: Option<&Path>) {
    let Some(first) = r.partners.first() else {
        println!("{}: no co-change history (commits of at most 50 files; {})", loc_file(root, &r.file), r.history);
        return;
    };
    println!(
        "{}: {} commits ({}); changed with it, most shared commits first (recency: weighted by age, half per 90 days):",
        loc_file(root, &r.file),
        first.coupled.commits,
        r.history
    );
    for p in &r.partners {
        let mut tags = Vec::new();
        if matches!(p.coupled.kind.as_str(), "template" | "style" | "javascript" | "config" | "docs" | "script" | "infra" | "other") {
            tags.push(p.coupled.kind.as_str());
        }
        if p.test {
            tags.push("test");
        }
        if !p.static_link {
            tags.push("no static link");
        }
        if !p.coupled.exists_now {
            tags.push("gone now");
        }
        let tags = if tags.is_empty() { String::new() } else { format!("  ({})", tags.join(", ")) };
        println!(
            "  {:>3} of {:<3} {:>4.0}%  recency {:<5.2} {}{tags}",
            p.coupled.support,
            p.coupled.commits,
            p.coupled.confidence * 100.0,
            p.coupled.weight,
            loc_file(root, &p.coupled.file)
        );
    }
}

pub fn tests(repos: &[crate::ops::TestsRepo], multi: bool) {
    if repos.is_empty() {
        println!("nothing to select for (no matching file, symbol or change)");
    }
    for r in repos {
        let root = multi.then_some(r.root.as_path());
        if !r.unknown.is_empty() {
            println!("not found: {}", r.unknown.join(", "));
        }
        let s = &r.selection;
        let total = s.selected.len() + s.left_out.len();
        if s.selected.is_empty() {
            println!("no test is likely enough to list as most likely relevant ({total} candidates below)");
        } else {
            println!("most likely relevant ({} of {total} candidates; not every test the change reaches):", s.selected.len());
        }
        for t in &s.selected {
            println!("{:.2}  {}", t.confidence, loc_file(root, &t.file));
            for reason in &t.reasons {
                println!("        {reason}");
            }
            if t.methods.len() > 1 {
                let more = if t.methods.len() > 5 { format!(" … +{}", t.methods.len() - 5) } else { String::new() };
                println!("        methods: {}{more}", t.methods.iter().take(5).cloned().collect::<Vec<_>>().join(", "));
            }
        }
        if !s.left_out.is_empty() {
            let shown: Vec<String> =
                s.left_out.iter().take(5).map(|t| format!("{} {:.2}", loc_file(root, &t.file), t.confidence)).collect();
            let more = if s.left_out.len() > 5 { format!(" … +{}", s.left_out.len() - 5) } else { String::new() };
            let why = if r.over_limit > 0 { format!(" ({} over --limit)", r.over_limit) } else { String::new() };
            println!("also candidates, not shown safe to skip {}{why}: {}{more}", s.left_out.len(), shown.join(", "));
        }
        if s.truncated {
            println!("… the reverse-edge walk stopped at its cap");
        }
    }
}

pub fn hotspots(d: &crate::ops::HotspotsData, env: &serde_json::Value) {
    if let [(root, files)] = d.repos.as_slice() {
        if files.is_empty() {
            println!("no indexed source files{}", d.path.as_deref().map(|p| format!(" matching {p}")).unwrap_or_default());
            return;
        }
        println!("{} ({} files; risk is the concern percentile, health 10 = healthiest)", repo_name(root), files.len());
        if let Some(h) = d.history.first() {
            history_line(h, d.boundary_only.first().copied().unwrap_or(false));
        }
        println!(
            "  {:>4} {:>3} {:>6}  {:>7} {:>4} {:>4} {:>4} {:>6} {:>4} {:>5} {:>4} {:>3}  file",
            "risk", "hlt", "concrn", "churn", "fix%", "cplx", "nest", "longfn", "auth", "fanin", "cpld", "rec"
        );
        for f in files.iter().take(d.top) {
            println!(
                "  {:>4} {:>3} {:>6.2}  {:>7} {:>3.0}% {:>4} {:>4} {:>6} {:>4} {:>5} {:>4} {:>3}  {}",
                f.risk,
                f.health,
                f.concern,
                f.weighted_churn,
                f.fix_ratio * 100.0,
                f.complexity,
                f.max_nesting,
                f.longest_function,
                f.authors_365d,
                f.fan_in,
                f.coupled_files,
                if f.recurring { "yes" } else { "" },
                f.file
            );
        }
        // One file asked for: the details that don't fit a column.
        if let (Some(_), [f]) = (&d.path, files.as_slice()) {
            println!(
                "  commits {} (fixes {}), {} functions{}",
                f.commits_365d,
                f.fix_commits_365d,
                f.functions,
                f.longest_function_name.as_deref().map(|n| format!(", longest {n} ({} lines)", f.longest_function)).unwrap_or_default()
            );
            if let Some(c) = &f.co_change {
                println!("  changes most with {} ({} shared commits, {:.0}% of its commits of 50 files or fewer)", c.file, c.shared_commits, c.ratio * 100.0);
            }
            if f.fix_commits_365d > 0 {
                println!("  time-weighted fix score {:.3} (recent fixes count most)", f.fix_score);
            }
            findings(&d.findings);
        }
        return;
    }
    for ((r, h), &none) in d.repos.iter().zip(&d.history).zip(&d.boundary_only) {
        if none {
            println!("{}: no countable history (shallow clone with only its boundary commit)", repo_name(&r.0));
        }
        if let Some(m) = h.dormant_months {
            println!("{}: dormant {m} months (last commit {})", repo_name(&r.0), h.last_commit.as_deref().unwrap_or("-"));
        }
    }
    println!("repos by mean file health (lowest first):");
    for r in env["items"].as_array().map(Vec::as_slice).unwrap_or_default() {
        println!(
            "  {:<16} health {:>4}  {:>4} files  worst {} ({})",
            r["repo"].as_str().unwrap_or(""),
            r["mean_health"],
            r["files"],
            r["worst_file"].as_str().unwrap_or("-"),
            r["worst_concern"]
        );
    }
}

pub fn overview(env: &serde_json::Value) {
    let items = env["items"].as_array().map(Vec::as_slice).unwrap_or_default();
    let list = |v: &serde_json::Value| -> String {
        v.as_array()
            .map(|a| a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    };
    let rows = |v: &serde_json::Value| v.as_array().map(Vec::as_slice).unwrap_or_default().to_vec();
    if items.len() > 1 {
        // One line per repo: the repos on each side, with how many dependencies point at each when more than one.
        let names = |v: &serde_json::Value| -> String {
            rows(v)
                .iter()
                .map(|e| {
                    let n = e["dependencies"].as_u64().unwrap_or(1);
                    if n > 1 { format!("{} ×{n}", e["repo"].as_str().unwrap_or("")) } else { e["repo"].as_str().unwrap_or("").to_string() }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        for r in items {
            println!(
                "{:<16} {:>5} files {:>7} lines {:>6} symbols{}{}",
                r["repo"].as_str().unwrap_or(""),
                r["files"],
                r["lines"],
                r["symbols"],
                if rows(&r["depends_on"]).is_empty() { String::new() } else { format!("  depends on {}", names(&r["depends_on"])) },
                if rows(&r["dependents"]).is_empty() { String::new() } else { format!("  used by {}", names(&r["dependents"])) },
            );
        }
        return;
    }
    let Some(r) = items.first() else { return };
    let lang = r["languages"].as_object().map(crate::ops::repo_lang).unwrap_or_default();
    let views = r["swiftui_views"].as_u64().filter(|n| *n > 0).map(|n| format!(", {n} SwiftUI views")).unwrap_or_default();
    println!("{}: {} files, {} lines{views}", r["repo"].as_str().unwrap_or(""), r["files"], r["lines"]);
    let map = |v: &serde_json::Value, word: &dyn Fn(&str) -> String| -> String {
        let mut parts: Vec<String> = v.as_object().map(|m| m.iter().map(|(k, n)| format!("{} {n}", word(k))).collect()).unwrap_or_default();
        parts.sort();
        parts.join(", ")
    };
    println!("languages: {}", map(&r["languages"], &|k| k.to_string()));
    if let Some(large) = r["skipped_large"].as_array() {
        let files: Vec<&str> = large.iter().filter_map(|f| f["file"].as_str()).collect();
        println!("not indexed, too large: {}", files.join(", "));
    }
    println!("symbols: {}", map(&r["symbols"], &|k| crate::ops::kind_word(k, &lang).to_string()));
    for p in rows(&r["packages"]) {
        println!("package {} {} ({}): {}", p["kind"].as_str().unwrap_or(""), p["name"].as_str().unwrap_or(""), p["manifest"].as_str().unwrap_or(""), list(&p["targets"]));
    }
    // One line per repo on the other side, with the manifest lines declaring the dependencies.
    for (label, key) in [("depends on", "depends_on"), ("used by", "dependents")] {
        let links = rows(&r[key]);
        if links.is_empty() {
            continue;
        }
        println!("{label} ({} repos):", links.len());
        let width = links.iter().map(|e| e["repo"].as_str().unwrap_or("").len()).max().unwrap_or(0).min(28);
        for e in &links {
            println!("  {:<width$}  {}", e["repo"].as_str().unwrap_or(""), list(&e["sites"]));
        }
    }
    for e in rows(&r["entry_points"]) {
        println!("entry point: {}  {}:{}", e["symbol"].as_str().unwrap_or(""), e["file"].as_str().unwrap_or(""), e["line"]);
    }
    let key_types = rows(&r["key_types"]);
    if !key_types.is_empty() {
        println!("key types (files using them):");
        for t in key_types {
            let file = t["file"].as_str().unwrap_or("");
            let kind = crate::ops::kind_word(t["kind"].as_str().unwrap_or(""), crate::ops::file_lang(file));
            println!("  {:>4}  {kind} {}  {file}:{}", t["used_by_files"], t["qualified"].as_str().unwrap_or(""), t["line"]);
        }
    }
    if let Some(tables) = r["tables"].as_array().filter(|t| !t.is_empty()) {
        let total = r["tables_total"].as_u64().unwrap_or(tables.len() as u64);
        if total > tables.len() as u64 {
            println!("tables ({total}, the {} most used; knowcode tables for all and their accessors):", tables.len());
        } else {
            println!("tables ({total}; knowcode tables for accessors):");
        }
        let width = |k: &str, max: usize| tables.iter().map(|t| t[k].as_str().unwrap_or("").len()).max().unwrap_or(0).min(max);
        let (tw, ew) = (width("table", 40), width("entity", 32));
        for t in tables {
            println!(
                "  {:<tw$} {:<ew$} {}  read {}, write {}, other {}",
                t["table"].as_str().unwrap_or(""),
                t["entity"].as_str().unwrap_or(""),
                t["context"].as_str().unwrap_or("-"),
                t["reads"],
                t["writes"],
                t["other"]
            );
        }
    }
    let areas = rows(&r["areas"]);
    if !areas.is_empty() {
        let total = r["areas_total"].as_u64().unwrap_or(areas.len() as u64);
        let more = if total > areas.len() as u64 { format!(", the {} largest; knowcode areas for all", areas.len()) } else { String::new() };
        println!("areas ({total}{more}):");
        let width = areas.iter().map(|a| a["name"].as_str().unwrap_or("").len()).max().unwrap_or(0).min(40);
        for a in &areas {
            let deps = list(&a["depends_on"]);
            println!(
                "  #{:<2} {:<width$} {:>5} files  cohesion {:.2}  key: {}{}",
                a["id"],
                a["name"].as_str().unwrap_or(""),
                a["files"],
                a["cohesion"].as_f64().unwrap_or(0.0),
                list(&a["key_files"]),
                if deps.is_empty() { String::new() } else { format!("  uses: {deps}") },
            );
        }
    }
    let hot = rows(&r["top_hotspots"]);
    if !hot.is_empty() {
        println!("top hotspots:");
        for h in hot {
            println!("  risk {:>3}  concern {:>5}  {}", h["risk"], h["concern"], h["file"].as_str().unwrap_or(""));
        }
    }
    if r["map"]["files"].as_array().is_some_and(|f| !f.is_empty()) {
        println!("ranked map (most central files first; symbols other files use):");
        for f in r["map"]["files"].as_array().map(Vec::as_slice).unwrap_or_default() {
            println!("{}  (rank {})", f["file"].as_str().unwrap_or(""), f["rank"]);
            for s in f["symbols"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let sig = s["signature"].as_str().unwrap_or("");
                let sig: String = sig.chars().take(110).collect();
                println!("  {:>4}│ {sig}", s["line"]);
            }
        }
        let omitted = r["map"]["omitted_files"].as_u64().unwrap_or(0);
        if omitted > 0 {
            println!("… {omitted} more files (raise --map-budget or use --focus)");
        }
    }
}

/// `knowcode areas`: per repo, a header, then each area (with `--area`, the one picked and its files).
pub fn areas(repos: &[crate::ops::AreasRepo], picked: Option<&str>, env: &serde_json::Value) {
    let multi = repos.len() > 1;
    for r in repos {
        let a = &r.areas;
        if picked.is_some() && r.picked.is_none() && multi {
            continue;
        }
        println!(
            "{}{} areas over {} files (modularity {:.2}{})",
            if multi { format!("{}: ", repo_name(&r.root)) } else { String::new() },
            a.areas.len(),
            a.files,
            a.modularity,
            if a.cochange_pairs == 0 { "; no co-change history".to_string() } else { format!("; {} co-change pairs", a.cochange_pairs) },
        );
        for area in a.areas.iter().filter(|x| picked.is_none() || Some(x.id) == r.picked) {
            println!(
                "#{} {}  {} files, {} lines, cohesion {:.2}{}",
                area.id,
                area.name,
                area.files.len(),
                area.lines,
                area.cohesion,
                if area.tokens.is_empty() { String::new() } else { format!("  [{}]", area.tokens.join(", ")) },
            );
            if area.dir != area.name {
                println!("  dir: {}", if area.dir.is_empty() { "(repo root)" } else { &area.dir });
            }
            println!("  key files: {}", area.key_files.join(", "));
            for e in &area.entry_points {
                println!("  entry point: {}  {}:{}", e.symbol, e.file, e.line);
            }
            if !area.exposed.is_empty() {
                println!("  used from other areas: {}", area.exposed.join(", "));
            }
            if !area.depends_on.is_empty() {
                let deps: Vec<String> = area.depends_on.iter().map(|d| format!("#{} {} ({} file edges)", d.area, d.name, d.file_edges)).collect();
                println!("  depends on: {}", deps.join(", "));
            }
            if picked.is_some() {
                println!("  files:");
                for f in &area.files {
                    println!("    {f}");
                }
            }
        }
        if picked.is_none() && !a.ungrouped.is_empty() {
            let more = if a.ungrouped.len() > 10 { format!(", … {} more (--json lists them)", a.ungrouped.len() - 10) } else { String::new() };
            println!("ungrouped ({}): {}{more}", a.ungrouped.len(), a.ungrouped.iter().take(10).cloned().collect::<Vec<_>>().join(", "));
        }
    }
    if let Some(note) = env["note"].as_str() {
        println!("{note}");
    }
}

/// The area line of a context: `area: #3 Orders/Services (42 files, cohesion 0.61)`.
pub fn area_line(area: Option<&crate::ops::AreaRef>) {
    if let Some(a) = area {
        println!("  area: #{} {} ({} files, cohesion {:.2}; knowcode areas --area {} lists them)", a.id, a.name, a.files, a.cohesion, a.id);
    }
}

/// Notes on a symbol, one line each, stale and orphaned ones marked.
pub fn note_lines(notes: &[kc_index::notes::NoteView]) {
    for n in notes {
        println!("  {}", note_text(n));
    }
}

fn note_text(n: &kc_index::notes::NoteView) -> String {
    let status = match n.status {
        "stale" => format!(
            "STALE since {}{}",
            n.stale_since.as_deref().unwrap_or("?"),
            n.change.as_deref().map(|c| format!(", {c}")).unwrap_or_default()
        ),
        "orphaned" if n.candidates.is_empty() => "ORPHANED: no declaration with its identity now".to_string(),
        "orphaned" => format!("ORPHANED: no declaration with its identity now; candidates {}", n.candidates.join(", ")),
        _ => "current".to_string(),
    };
    let status = if n.identity == Some("unknown") { format!("{status}, identity unknown (noted before identities were recorded)") } else { status };
    let tag = n.note.tag.as_deref().map(|t| format!("[{t}] ")).unwrap_or_default();
    let moved = n.moved_from.as_deref().map(|f| format!(", moved from {f}")).unwrap_or_default();
    let by = n.note.author.as_deref().map(|a| format!(" by {a}")).unwrap_or_default();
    format!("note #{} ({status}{moved}; {}{by}): {tag}{}", n.note.id, n.note.date, n.note.text)
}

/// `knowcode note list` and `note add`: each note under its symbol.
pub fn notes(repos: &[crate::ops::NotesRepo]) {
    let multi = repos.len() > 1;
    for r in repos {
        for n in &r.notes {
            let at = n.now_at.clone().unwrap_or_else(|| format!("{}:{}", n.note.file, n.note.line));
            let symbol = n.note.namespace.as_deref().map_or_else(|| n.note.symbol.clone(), |ns| format!("{ns}.{}", n.note.symbol));
            println!("{} {symbol}  {}", n.note.kind, if multi { format!("{}/{at}", repo_name(&r.root)) } else { at });
            println!("  {}", note_text(n));
        }
    }
}

/// `context` on a file: its area and the notes on its symbols.
pub fn file_hit(f: &crate::ops::FileHit, multi: bool) {
    println!("file {}{}", if multi { format!("{}/", f.repo) } else { String::new() }, f.file);
    area_line(f.area.as_ref());
    if f.area.is_none() {
        println!("  in no area");
    }
    note_lines(&f.notes);
}

pub fn search(d: &crate::ops::SearchData) {
    let kinds: Vec<String> = d.by_kind.iter().map(|(k, v)| format!("{k} {v}")).collect();
    println!(
        "{} matches in {} files{}{}",
        d.total,
        d.total_files,
        if kinds.is_empty() { String::new() } else { format!(" ({})", kinds.join(", ")) },
        if d.capped { ", stopped at the line cap" } else { "" }
    );
    let file_label = |repo: usize, file: &str| {
        if d.multi { format!("{}/{file}", repo_name(&d.roots[repo])) } else { file.to_string() }
    };
    for f in &d.files {
        println!("{}  {} matches, best {} :{}", file_label(f.repo, &f.file), f.matches, f.best, f.line);
    }
    let mut last: Option<(usize, &str)> = None;
    for h in &d.hits {
        if last != Some((h.repo, h.file.as_str())) {
            println!("{}", file_label(h.repo, &h.file));
            last = Some((h.repo, h.file.as_str()));
        }
        let first = h.line as usize - h.before.len();
        for (i, l) in h.before.iter().enumerate() {
            println!("  {:>5}-        {l}", first + i);
        }
        let at = match (&h.symbol, &h.inside) {
            (Some(s), _) => format!("  [{s}]"),
            (None, Some(s)) => format!("  [in {s}{}]", h.in_params.as_deref().unwrap_or_default()),
            _ => String::new(),
        };
        println!("  {:>5}: {:<7} {}{at}", h.line, h.kind, h.text);
        for (i, l) in h.after.iter().enumerate() {
            println!("  {:>5}-        {l}", h.line as usize + 1 + i);
        }
    }
    if let Some(next) = d.next_offset {
        println!("… more: --offset {next}");
    }
}

pub fn grep_ast(d: &crate::ops::AstData) {
    println!("{} matches in {} files{}", d.total, d.files, if d.capped { ", stopped at the match cap" } else { "" });
    for h in &d.hits {
        let file = if d.multi { format!("{}/{}", repo_name(&d.roots[h.repo]), h.file) } else { h.file.clone() };
        let vars: Vec<String> = h.vars.iter().map(|(k, v)| format!("${k}={v}")).collect();
        let vars = if vars.is_empty() { String::new() } else { format!("  [{}]", vars.join(", ")) };
        println!("{file}:{}: {}{vars}", h.line, h.text);
    }
    if let Some(next) = d.next_offset {
        println!("… more: --offset {next}");
    }
}

/// Tables per DbContext; with `detail`, each table's accessors too.
pub fn tables(d: &crate::ops::TablesData, detail: bool, multi: bool) {
    if d.models.is_empty() && d.sql.is_empty() {
        println!("no Entity Framework tables or stored procedure calls (for a table or routine defined in .sql files: knowcode tables <name>)");
    }
    for (root, s) in &d.sql {
        let r = multi.then_some(root.as_path());
        let copies = if s.copies.is_empty() { String::new() } else { format!(" (+{} copies)", s.copies.len()) };
        println!("{} {}  {}{copies}", s.kind, s.name, loc(r, &s.file, s.line));
        for (what, list) in [("reads", &s.section.reads), ("writes", &s.section.writes), ("calls", &s.section.calls)] {
            if !list.is_empty() {
                println!("  {what}: {}", list.join(", "));
            }
        }
        if !s.used_by.is_empty() {
            println!("  used by ({}):", s.used_by.len());
            let shown = if detail { 50 } else { 10 };
            for u in s.used_by.iter().take(shown) {
                println!("    {:<8} {}  {}", u.mode, u.by.as_deref().unwrap_or("(top level)"), loc(r, &u.file, u.line));
            }
            if s.used_by.len() > shown {
                println!("    … {} more (knowcode refs {})", s.used_by.len() - shown, s.name);
            }
        }
    }
    for (root, m, sql) in &d.models {
        let r = multi.then_some(root.as_path());
        let mut group = None;
        for (i, t) in m.tables.iter().enumerate() {
            let header = match t.context.as_deref() {
                Some(c) => c,
                None if t.mapped_by == "sql" => "(tables named in SQL)",
                None => "(entities mapped without a DbSet)",
            };
            if group != Some(header) {
                group = Some(header);
                println!("{header}");
            }
            let x = sql.tables.get(i);
            table(r, t, x, detail, "  ");
            if let Some(x) = x {
                sql_links(&x.links, "    ");
            }
        }
        if !m.procedures.is_empty() {
            println!("stored procedures ({}):", m.procedures.len());
            for (i, p) in m.procedures.iter().enumerate() {
                println!("  {:<28} {}  {}", p.name, p.symbol, loc(r, &p.file, p.line));
                sql_links(sql.procedures.get(i).map_or(&[], |l| l.as_slice()), "    ");
            }
        }
    }
}

/// SQL definitions linked to a table or procedure call: `defined exact table shop.orders  schemas/shop.sql:12`.
fn sql_links(links: &[crate::ops::SqlLinkOut], indent: &str) {
    for l in links.iter().take(5) {
        let copies = if l.copies > 0 { format!(" (+{} copies)", l.copies) } else { String::new() };
        println!("{indent}defined {} {} {}  {}/{}:{}{copies}", l.confidence, l.kind, l.name, l.repo, l.file, l.line);
    }
    if links.len() > 5 {
        println!("{indent}… {} more definitions", links.len() - 5);
    }
}

/// The SQL side of a context bundle: a SQL object's columns, what it reads, writes and calls, and the code using it.
pub fn sql_facts(f: &crate::ops::SqlFacts) {
    let s = &f.section;
    if !s.columns.is_empty() {
        let cols: Vec<String> =
            s.columns.iter().map(|c| if c.decl.is_empty() { c.name.clone() } else { format!("{} {}", c.name, c.decl) }).collect();
        println!("columns ({}):", cols.len());
        for c in cols {
            println!("  {c}");
        }
    }
    for (label, list) in [("reads", &s.reads), ("writes", &s.writes), ("calls", &s.calls)] {
        if !list.is_empty() {
            println!("{label}: {}", list.join(", "));
        }
    }
    if !f.used_by.is_empty() {
        println!("used by code ({}):", f.used_by.len());
        for u in &f.used_by {
            match u.how {
                "entity" => {
                    let context = u.context.as_deref().map(|c| format!(" in {c}")).unwrap_or_default();
                    println!(
                        "  {:<11} entity {}{context} as {}  {}/{}:{}  read {}, write {}",
                        u.confidence,
                        u.symbol,
                        u.name,
                        u.repo,
                        u.file,
                        u.line,
                        u.reads.unwrap_or(0),
                        u.writes.unwrap_or(0)
                    );
                    for a in &u.accessors {
                        println!("    {:<6} {}  {}/{}:{}{}", a.mode, a.symbol, u.repo, a.file, a.line, a.note());
                    }
                }
                _ => println!("  {:<11} call {} by {}  {}/{}:{}", u.confidence, u.name, u.symbol, u.repo, u.file, u.line),
            }
        }
    }
}

/// A table of code: its name with its database when known (`shop.invoice`), mapping, counts, the database line
/// (how it was decided, and other databases with a table of that name), and with `detail` its accessors.
fn table(r: Option<&Path>, t: &kc_store::db::Table, x: Option<&crate::ops::TableSql>, detail: bool, indent: &str) {
    let via = t.dbset.as_deref().or(t.navigation.as_deref()).map(|d| format!(" via {d}")).unwrap_or_default();
    let mapping = t.mapping.as_deref().map(|m| format!(" at {m}")).unwrap_or_default();
    let through = if t.writes_via > 0 { format!(", write through a callee {}", t.writes_via) } else { String::new() };
    println!(
        "{indent}{:<28} {}{via}  {}  [{}{mapping}]  read {}, write {}, other {}{through}",
        x.map_or_else(|| t.table.clone(), |x| x.qualified(&t.table)),
        t.entity,
        loc(r, &t.file, t.line),
        t.mapped_by,
        t.reads,
        t.writes,
        t.other
    );
    if let Some(d) = x.and_then(|x| x.database.as_ref()) {
        let how = match (d.by, &d.connection) {
            ("connection", Some(c)) => format!("connection {c}"),
            ("", _) => String::new(),
            (by, _) => by.replace('_', " "),
        };
        let others = if d.same_name_in.is_empty() { String::new() } else { format!("; same name in {}", d.same_name_in.join(", ")) };
        match &d.name {
            Some(db) => println!("{indent}  database {db} ({}{}{how}){others}", d.confidence, if how.is_empty() { "" } else { ", by " }),
            None if d.confidence == "ambiguous" => println!("{indent}  database unknown (ambiguous: {})", d.same_name_in.join(", ")),
            None => println!("{indent}  database unnamed (the repo's own SQL){others}"),
        }
    }
    if detail {
        for a in &t.accessors {
            let conf = if a.confidence == "exact" { String::new() } else { format!(" ({})", a.confidence) };
            println!("{indent}  {:<6} {}  {}{conf}{}", a.mode, a.symbol, loc(r, &a.file, a.line), a.note());
        }
        // The counts cover each accessor once, except callers listed only for a callee's write (in `writes_via`).
        let total = t.reads + t.writes + t.other - t.read_writes;
        let shown = t.accessors.iter().filter(|a| !a.only_via || kc_store::db::writes(&a.mode)).count();
        if total > shown {
            println!("{indent}  … {} more (knowcode tables {})", total - shown, t.table);
        }
        for h in &t.hidden_via {
            let what = match (h.tests, h.more) {
                (0, m) => format!("{m} more callers"),
                (n, 0) => format!("{n} test callers"),
                (n, m) => format!("{n} test callers and {m} more"),
            };
            println!("{indent}  + {what} write via {} (knowcode tables {} --all-callers)", h.via, t.table);
        }
    }
}

/// The tables section of a context bundle, with the SQL defining each table and procedure.
pub fn symbol_tables(t: &kc_store::db::SymbolTables, sql: &crate::ops::SqlFacts) {
    if !t.tables.is_empty() {
        println!("tables ({}):", t.tables.len());
        for (i, x) in t.tables.iter().enumerate() {
            let s = sql.tables.get(i);
            table(None, x, s, true, "  ");
            if let Some(s) = s {
                sql_links(&s.links, "    ");
            }
        }
    }
    if !t.touches.is_empty() {
        println!("tables touched ({}; hop 0 is this symbol):", t.touches.len());
        for x in &t.touches {
            let name = match (&x.entity, &x.context) {
                (Some(e), Some(c)) => format!("{} ({e} in {c})", x.name),
                (Some(e), None) => format!("{} ({e})", x.name),
                _ if x.kind == "table" => x.name.clone(),
                _ => format!("{} (procedure)", x.name),
            };
            println!("  {:<6} {name}  hop {} via {}  {}", x.mode, x.hop, x.via, x.site);
            if let Some((_, links)) = sql.procedures.iter().find(|(n, _)| x.kind == "procedure" && *n == x.name) {
                sql_links(links, "    ");
            }
        }
    }
}

pub fn http(entries: &[crate::ops::HttpOut]) {
    for e in entries {
        let path = e.path.as_deref().unwrap_or("?");
        match (e.role, &e.operation) {
            ("requests", Some(op)) => match &e.service {
                Some(tag) => println!("http: {} calls operation {op} of a generated {tag} client", e.symbol),
                None => println!("http: {} calls operation {op} of a generated client", e.symbol),
            },
            ("requests", None) => match &e.service {
                Some(svc) => println!("http: {} sends {} /{path} to {svc}", e.symbol, e.verb),
                None => println!("http: {} sends {} /{path}", e.symbol, e.verb),
            },
            _ => println!("http: {} serves {} /{path}", e.symbol, e.verb),
        }
        if e.links.is_empty() {
            match &e.note {
                Some(n) => println!("  (no {} found: {n})", if e.role == "requests" { "handler" } else { "caller" }),
                None => println!("  (no {} found in indexed repos)", if e.role == "requests" { "handler" } else { "caller" }),
            }
        }
        for l in &e.links {
            println!("  {:<9} {}  {}  {}:{}-{}", l.confidence, l.symbol, l.repo, l.file, l.start_line, l.end_line);
        }
        for l in &e.commented_out {
            println!("  commented out in {}  {}  {}:{}", l.symbol, l.repo, l.file, l.line);
        }
        if e.unsent {
            println!("  unsent: no code in the indexed repos creates it");
        }
    }
}

/// A context's message-bus facts.
pub fn bus(entries: &[crate::ops::BusOut]) {
    let refs: Vec<&crate::ops::BusOut> = entries.iter().collect();
    bus_list(&refs);
}

/// Message-bus facts, each with the code on the other side.
pub fn bus_list(entries: &[&crate::ops::BusOut]) {
    for e in entries {
        let queue = e.queue.as_deref().map(|q| format!(" on {q}")).unwrap_or_default();
        let how = if e.resolution == "exact" { String::new() } else { format!(" [{}]", e.resolution) };
        match e.role {
            "message" => println!("bus: {} is message {}{queue}", e.symbol, e.message),
            "publishes" => println!("bus: {} publishes {}{how}{queue}  {}:{}", e.symbol, e.message, e.file, e.line),
            _ => println!("bus: {} consumes {}{how}  {}:{}", e.symbol, e.message, e.file, e.line),
        }
        if e.links.is_empty() && e.resolution != "unknown" {
            println!("  (no {} found in indexed repos)", if e.role == "consumes" { "publisher" } else { "consumer" });
        }
        for l in &e.links {
            println!("  {:<9} {:<9} {}  {}  {}:{}", l.confidence, l.role, l.symbol, l.repo, l.file, l.line);
        }
    }
}

fn deploy_line(d: &crate::ops::DeployOut) -> String {
    let resolved = d.resolved.as_deref().map(|r| format!(" = {r}")).unwrap_or_default();
    let at = d.symbol.as_deref().map(|s| format!("  in {s}")).unwrap_or_default();
    format!("{} {} {}{resolved}  {}:{}{at}", d.action, d.what, d.value, d.file, d.line)
}

/// The Terraform side of a context's symbol.
pub fn terraform(t: &crate::tf::TerraformOut) {
    if let Some(m) = &t.module {
        let version = m.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
        let target = match (&m.repo, &m.dir) {
            (Some(repo), Some(dir)) if dir.is_empty() => repo.clone(),
            (Some(repo), Some(dir)) => format!("{repo}/{dir}"),
            (None, Some(dir)) => format!("{}/", if dir.is_empty() { "." } else { dir }),
            _ => "(no indexed repo)".to_string(),
        };
        println!("terraform: module source {}{version} ({}) → {target}  [{}]", m.source, m.kind, m.confidence);
    }
    const SHOWN: usize = 10;
    for c in t.module_called_by.iter().take(SHOWN) {
        let version = c.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
        println!("terraform: module called by {}: {} (source {}{version})  {}:{}  [{}]", c.repo, c.module, c.source, c.file, c.line, c.confidence);
    }
    if t.module_called_by.len() > SHOWN {
        println!("terraform: … {} more module calls (--json lists them)", t.module_called_by.len() - SHOWN);
    }
}

/// The Ansible side of a context's symbol.
pub fn ansible(a: &crate::ops::AnsibleOut) {
    for r in &a.roles {
        let target = match (&r.repo, &r.defined_at) {
            (Some(repo), Some(at)) => format!("{repo}/{at}"),
            _ => "(no indexed repo)".to_string(),
        };
        println!("ansible: uses role {} → {target}  [{}]  {}:{}", r.name, r.confidence, r.file, r.line);
    }
    for u in &a.used_by {
        let from = u.from.as_deref().unwrap_or("?");
        println!("ansible: used by {}: {from} ({})  {}:{}  [{}]", u.repo, u.from_kind.as_deref().unwrap_or("?"), u.file, u.line, u.confidence);
    }
    for d in &a.deploys {
        println!("ansible: {}", deploy_line(d));
        for l in &d.links {
            println!("  {:<11} {:<4} {}  ({})", l.confidence, l.how, l.repo, l.matched);
        }
    }
    if let Some(d) = &a.deploys_apps {
        println!("deploys {} application repo{}:", d.count, if d.count == 1 { "" } else { "s" });
        for app in &d.apps {
            println!("  {}: {} {} → {}  [{}]  {}", app.repo, app.job, app.at, app.playbook, app.confidence, app.vars.join(" "));
        }
    }
}

/// A value a pipeline passes: `NAME=value (where it's set)`, or what is missing.
fn passed_line(v: &kc_index::ci::Passed) -> String {
    match (&v.value, &v.partial) {
        (Some(x), _) => match &v.from {
            Some(f) => format!("{}={x} ({f})", v.name),
            None => format!("{}={x}", v.name),
        },
        (None, p) => format!("{}={} (unresolved: {})", v.name, p.as_deref().unwrap_or(&v.written), v.unresolved.join(", ")),
    }
}

/// How a repo's CI pipeline deploys it.
pub fn deploy_chains(r: &crate::ops::RepoAnsibleOut) {
    if !r.deploy_chains.is_empty() {
        let ansible = r.deploy_chains.iter().filter(|c| c.kind == "ansible").count();
        let other = r.deploy_chains.len() - ansible;
        let mut parts = Vec::new();
        if ansible > 0 {
            parts.push(format!("{ansible} ansible-playbook run{}", if ansible == 1 { "" } else { "s" }));
        }
        if other > 0 {
            parts.push(format!("{other} other deploy job{}", if other == 1 { "" } else { "s" }));
        }
        println!("repo {}: deployed by its CI pipeline ({})", r.repo, parts.join(", "));
    }
    for t in &r.ci_templates {
        println!("  template {t}");
    }
    for c in &r.deploy_chains {
        let path: Vec<String> = c.path.iter().map(|h| if h.how == "trigger" { format!("trigger {}", h.at) } else { format!("{} {}", h.name, h.at) }).collect();
        println!("  {}", path.join(" → "));
        if !c.ci_files.is_empty() {
            println!("    ci files: {}", c.ci_files.join(", "));
        }
        for s in &c.steps {
            let via = s.via.as_deref().map(|v| format!(" (via {v})")).unwrap_or_default();
            println!("    {} {}  [{}]  at {}{via}", s.tool, s.action, c.confidence, s.at);
            for f in &s.files {
                println!("      {} {}{}", f.role, f.path, if f.found { "" } else { " (not in the checkout)" });
            }
            for v in &s.values {
                println!("      {}", passed_line(v));
            }
            for m in &s.modules {
                println!("      module {} = {} → {} [{}]  {}", m.name, m.source, m.repo.as_deref().unwrap_or("-"), m.confidence, m.at);
            }
        }
        if !c.steps.is_empty() {
            for w in &c.conditions {
                println!("    when {w}");
            }
            continue;
        }
        let Some(playbook) = &c.playbook else { continue };
        let inv = c.inventory.as_ref().and_then(|i| i.value.as_deref().or(i.partial.as_deref())).map(|i| format!(", inventory {i}")).unwrap_or_default();
        let clone = if playbook.missing { " (no such playbook there)" } else { "" };
        println!("    runs {}{clone}  [{}]  at {}{inv}", playbook.name, c.confidence, c.run);
        for v in &c.vars {
            println!("    {}", passed_line(v));
        }
        for r in &c.roles {
            let dir = if r.dir.is_empty() { String::new() } else { format!("/{}", r.dir) };
            println!("    role {} ({}{dir}) [{}]{}", r.name, r.repo.as_deref().unwrap_or("not indexed"), r.confidence, if r.depth > 1 { format!(" depth {}", r.depth) } else { String::new() });
            for f in r.tasks.iter().chain(&r.handlers).chain(&r.templates) {
                println!("      {f}");
            }
            if r.more_files > 0 {
                println!("      … {} more files", r.more_files);
            }
        }
        for d in &c.deploys {
            println!("    {}", deploy_line(d));
        }
        for w in &c.conditions {
            println!("    when {w}");
        }
    }
    if !r.ci_includes.is_empty() {
        println!("  CI includes:");
    }
    for i in &r.ci_includes {
        let from = i.repo.as_deref().map(|r| format!(" from {r}")).unwrap_or_default();
        println!("    {} at {}: {}{from}", i.named(), i.at, i.status);
        if let Some(w) = &i.why {
            println!("      {w}");
        }
        if !i.jobs.is_empty() {
            let more = i.jobs_total.map(|t| format!(" (of {t})")).unwrap_or_default();
            println!("      jobs{more}: {}", i.jobs.join(", "));
        }
        for d in &i.deploys {
            println!("      deploys: {d}");
        }
    }
    if !r.ci_skipped.is_empty() {
        println!("  CI includes their rules leave out of a default-branch push (not read):");
    }
    for s in &r.ci_skipped {
        println!("    {s}");
    }
    for u in &r.ci_unread {
        println!("  unread CI include: {u}");
    }
}

/// What deploys or configures a repo.
pub fn repo_ansible(r: &crate::ops::RepoAnsibleOut) {
    deploy_chains(r);
    if r.deployed_by.is_empty() && r.own.is_empty() {
        if r.deploy_chains.is_empty() && r.ci_unread.is_empty() && r.ci_templates.is_empty() && r.ci_includes.is_empty() && r.ci_skipped.is_empty() {
            println!("repo {}: no deploy found (no CI deploy chain, playbook or role deploys or configures it)", r.repo);
        }
        return;
    }
    println!("repo {}: deployed or configured by", r.repo);
    for d in &r.deployed_by {
        let role = d.role.as_deref().map(|r| format!(" (role {r})")).unwrap_or_default();
        println!("  {}{role}: {}  [{}]", d.repo, deploy_line(&d.deploy), d.confidence);
        for u in &d.role_used_by {
            println!("    used by {u}");
        }
    }
    for o in &r.own {
        println!("  own: {o}");
    }
}

pub fn angular(a: &kc_store::query::Angular) {
    println!("angular: selector {}  template {}", a.selector.as_deref().unwrap_or("-"), a.template.as_deref().unwrap_or("(inline)"));
    for r in &a.routes {
        println!("  route   /{r}");
    }
    for u in &a.rendered_by {
        let repo = u.repo.as_deref().map(|r| format!("{r}: ")).unwrap_or_default();
        println!("  used by {}  {repo}{}:{}", u.component.as_deref().unwrap_or("?"), u.file, u.line);
    }
    for line in renders_lines(a) {
        println!("  {line}");
    }
}

/// What a component's template uses, one line per element or directive and template: repeated uses (a layout
/// directive on every row) are one line with every line number.
fn renders_lines(a: &kc_store::query::Angular) -> Vec<String> {
    let mut groups: Vec<(String, &str, Vec<u32>)> = Vec::new();
    for u in &a.renders {
        let from = u.repo.as_deref().map(|r| format!(" (from {r})")).unwrap_or_default();
        // An element shows as `<tag>`, a directive attribute as `[name]`.
        let tag = if u.tag.starts_with('[') { u.tag.clone() } else { format!("<{}>", u.tag) };
        let what = format!("{tag}{}{from}", u.component.as_deref().map(|c| format!(" {c}")).unwrap_or_default());
        match groups.iter_mut().find(|(w, f, _)| *w == what && *f == u.file) {
            Some((_, _, lines)) => lines.push(u.line),
            None => groups.push((what, &u.file, vec![u.line])),
        }
    }
    groups
        .into_iter()
        .map(|(what, file, lines)| {
            let times = if lines.len() > 1 { format!(" ×{}", lines.len()) } else { String::new() };
            format!("renders {what}{times}  {file}:{}", lines.iter().map(u32::to_string).collect::<Vec<_>>().join(","))
        })
        .collect()
}

/// One step of a path or flow: its symbol, with the repo when it isn't the one before's.
fn step_line(s: &crate::walk::Step, repo_changed: bool) -> String {
    let repo = if repo_changed { format!("[{}] ", s.repo) } else { String::new() };
    format!("{repo}{} {}  {}:{}", kw(&s.kind, &s.file), s.qualified, s.file, s.line)
}

/// `repo/site` for a site in another repo than the one the answer reads from.
fn in_repo(repo: Option<&str>, site: &str) -> String {
    match repo {
        Some(r) => format!("{r}/{site}"),
        None => site.to_string(),
    }
}

/// How a step is reached: the relation from the step before (`calls`, `requests (http …)`), its confidence, the
/// site (in `site_repo`, the step before's, named when it isn't the step's own), and for `via_di` the binding.
fn step_edge(s: &crate::walk::Step, site_repo: &str) -> String {
    let edge = match &s.ngrx {
        Some(action) => format!("dispatches (ngrx {action})"),
        None => relation_label(s.edge, "out", s.http.as_ref(), s.bus.as_ref(), s.sql.as_ref(), None, None),
    };
    let site = s.site.as_deref().map(|x| format!(" at {}", in_repo((site_repo != s.repo).then_some(site_repo), x))).unwrap_or_default();
    let binding = match s.binding.as_deref() {
        // One of an effect's branches: not a registration.
        Some(b) if b.starts_with(crate::flow::ONE_OF) || b == crate::flow::LAZY_NOTE => format!(" ({b})"),
        Some(b) => format!(" (bound at {b})"),
        None => String::new(),
    };
    let through = if s.through.is_empty() { String::new() } else { format!(" (through {})", s.through.join(" → ")) };
    format!("{edge} [{}]{site}{binding}{through}", s.confidence)
}

/// A symbol kind in the words of the language of `file` (`interface` for a C# or TypeScript protocol, `constructor`
/// for an init): see [`crate::ops::kind_word`].
fn kw<'a>(kind: &'a str, file: &str) -> &'a str {
    crate::ops::kind_word(kind, crate::ops::file_lang(file))
}

/// Paths as indented chains: each step under the one before, with the relation reaching it.
pub fn path_text(a: &crate::walk::PathAnswer) -> String {
    let mut out = String::new();
    let mut seen = std::collections::HashSet::new();
    // A name meaning several definitions: say which were tried, never pick one silently.
    for (side, c) in [("from", &a.from_candidates), ("to", &a.to_candidates)] {
        if !c.is_empty() {
            let each: Vec<String> = c.iter().map(|s| format!("{} ({}/{}:{})", s.qualified, s.repo, s.file, s.line)).collect();
            w!(out, "{side}: {} definitions share the name, all tried: {}", c.len(), each.join(", "));
        }
    }
    let cut = if a.truncated { "; stopped at the node or repo limit (raise --max-nodes or --max-repos)" } else { "" };
    if a.paths.is_empty() {
        let repos = a.repos_opened;
        if a.allow_ambiguous {
            w!(out, "no path from {} to {} within {} hops (explored {} nodes, {repos} repos open{cut})", a.from.qualified, a.to.qualified, a.max_hops, a.explored);
        } else {
            w!(
                out,
                "no confident path from {} to {} within {} hops (explored {} nodes, {repos} repos open{cut}); --allow-ambiguous also follows ambiguous edges",
                a.from.qualified,
                a.to.qualified,
                a.max_hops,
                a.explored
            );
        }
        if let Some(n) = &a.next {
            w!(out, "continue: {n}");
        }
        return out;
    }
    for (i, p) in a.paths.iter().enumerate() {
        w!(out, "path {} of {}: {} hops, weight {}", i + 1, a.paths.len(), p.hops, p.weight);
        let mut repo = "";
        for (j, s) in p.steps.iter().enumerate() {
            let indent = "  ".repeat(j + 1);
            if j > 0 {
                w!(out, "{indent}↳ {}", step_edge(s, &p.steps[j - 1].repo));
                // The edge's site, under the edge.
                cite_text(&mut out, &indent, s.cite.as_ref(), &mut seen);
            }
            w!(out, "{indent}{}", step_line(s, s.repo != repo));
            if j == 0 {
                cite_text(&mut out, &indent, s.cite.as_ref(), &mut seen);
            }
            repo = &s.repo;
        }
    }
    w!(
        out,
        "explored {} nodes ({} forward, {} backward), {} repos open{cut}",
        a.explored,
        a.explored_forward,
        a.explored_backward,
        a.repos_opened
    );
    out
}

pub fn path(a: &crate::walk::PathAnswer) {
    print!("{}", path_text(a));
}

/// A flow as a tree of numbered steps grouped by repo, each with the relation reaching it.
pub fn flow_text(entry: &str, f: Option<&crate::flow::Flow>) -> String {
    let mut out = String::new();
    let mut seen = std::collections::HashSet::new();
    let Some(f) = f else {
        w!(out, "no Angular route, component or definition named {entry} in the repos searched");
        return out;
    };
    let route = f.route.as_ref().map(|r| format!("route {} → ", r.path)).unwrap_or_default();
    w!(out, "flow from {route}{}", step_line(&f.start, true));
    cite_text(&mut out, "", f.start.cite.as_ref(), &mut seen);
    let left: Vec<String> = f.left_out.iter().map(|(k, n)| format!("{n} {k}")).collect();
    let left = if left.is_empty() { String::new() } else { format!("; left out: {}", left.join(", ")) };
    let hidden: Vec<String> = f.hidden.iter().map(|(k, n)| format!("{n} {k}")).collect();
    let hidden = if hidden.is_empty() { String::new() } else { format!("; hidden, followed through: {}", hidden.join(", ")) };
    w!(out, "{} steps in {} repos ({}){left}{hidden}", f.steps.len(), f.repos.len(), f.repos.join(", "));
    if f.steps.is_empty() {
        w!(out, "nothing followed: no edge leaves it, or every one was left out (see left out)");
    }
    let n = f.steps.len();
    let mut repo = f.start.repo.as_str();
    for s in &f.steps {
        if s.step.repo != repo {
            repo = &s.step.repo;
            w!(out, "── {repo}");
        }
        let indent = "  ".repeat(s.depth as usize + 1);
        w!(out, "{indent}step {} of {n}  {} {}  {}:{}", s.index, kw(&s.step.kind, &s.step.file), s.step.qualified, s.step.file, s.step.line);
        let from = s.parent.and_then(|i| f.steps.get(i - 1)).map_or(f.start.repo.as_str(), |p| p.step.repo.as_str());
        w!(out, "{indent}  ↳ {}", step_edge(&s.step, from));
        cite_text(&mut out, &indent, s.step.cite.as_ref(), &mut seen);
        if let Some(stop) = &s.stop {
            w!(out, "{indent}    stops at the depth, {}+ steps past it; continue: {}", stop.beyond, stop.next);
        }
    }
    if f.truncated {
        w!(out, "… truncated at the step limit (raise --limit or --max-repos, or lower --depth)");
    }
    out
}

pub fn flow(entry: &str, f: Option<&crate::flow::Flow>) {
    print!("{}", flow_text(entry, f));
}

/// The time frame of a repo's hotspots: the anchor, and dormancy or a shallow history when they apply.
fn history_line(h: &kc_index::vitals::History, boundary_only: bool) {
    let mut notes = vec![format!("windows end {}{}", h.anchor, if h.anchor_explicit { " (as of)" } else { "" })];
    if boundary_only {
        notes.push("no countable history (shallow clone with only its boundary commit)".into());
    }
    if let Some(m) = h.dormant_months {
        notes.push(format!("dormant {m} months, last commit {}", h.last_commit.as_deref().unwrap_or("-")));
    }
    if h.shallow && !boundary_only {
        notes.push("shallow clone: history starts at its boundary, which isn't counted".into());
    }
    if h.bot_commits_365d > 0 {
        notes.push(format!("{} automated commits left out", h.bot_commits_365d));
    }
    println!("  {}", notes.join("; "));
}

/// Code-health biomarkers, one per line with its reason.
pub fn findings(list: &[kc_index::health::Finding]) {
    if list.is_empty() {
        return;
    }
    println!("health:");
    for f in list {
        println!("  {:<19} {}:{}  {}", f.kind, f.file, f.line, f.reason);
    }
}

fn owner(o: &kc_index::ownership::Owner) -> String {
    let gone = match o.former {
        Some(kc_index::ownership::Former::Listed) => " (former)",
        Some(kc_index::ownership::Former::PossiblyFormer) => " (possibly former: inactive 6+ months)",
        None => "",
    };
    format!("{} <{}> {:.0}%{gone}", o.name, o.email, o.share * 100.0)
}

fn knowledge_line(k: &kc_index::ownership::Knowledge) -> String {
    format!(
        "{}  {} files, {} lines; top contributor {}; truck factor {}; added by authors gone {:.0}% (possibly {:.0}%)",
        k.path,
        k.files,
        k.lines,
        k.top_contributor.as_ref().map(owner).unwrap_or_else(|| "-".into()),
        k.truck_factor,
        k.contributors_gone * 100.0,
        k.contributors_possibly_gone * 100.0
    )
}

pub fn ownership(repos: &[crate::ops::OwnershipRepo]) {
    if repos.is_empty() {
        println!("no indexed file or directory there");
    }
    for r in repos {
        let c = &r.coverage;
        println!(
            "{}: {} authors, last commit {}{}{}",
            repo_name(&r.root),
            c.authors,
            c.last_commit.as_deref().unwrap_or("-"),
            if c.shallow { "; shallow clone, history before its boundary unknown" } else { "" },
            match &c.former_file {
                Some(f) => format!("; {} former authors listed in {f}", c.former_listed),
                None => String::new(),
            }
        );
        println!("historical contribution (lines added over the history read; not who knows the code now):");
        println!("{}", knowledge_line(&r.target));
        for o in &r.target.contributors {
            println!("  {:>5} lines added, {:>4} commits, last {}  {}", o.added, o.commits, o.last, owner(o));
        }
        if let Some((authors, uncommitted, automated)) = &r.surviving {
            let apart: Vec<String> = [(*uncommitted, "uncommitted"), (*automated, "from automated commits")]
                .iter()
                .filter(|(n, _)| *n > 0)
                .map(|(n, what)| format!("{n} {what}"))
                .collect();
            println!("surviving lines (git blame){}:", if apart.is_empty() { String::new() } else { format!("; {}", apart.join(", ")) });
            for s in authors.iter().take(5) {
                println!("  {:>5} lines  {} <{}> {:.0}%", s.lines, s.name, s.email, s.share * 100.0);
            }
        }
        if !r.areas.is_empty() {
            println!("areas:");
            for a in &r.areas {
                println!("  {}", knowledge_line(a));
            }
        }
        if let Some(off) = &r.offboarding {
            println!(
                "without {}: top contributor of {} files ({} lines); added by authors gone {:.0}% -> {:.0}%",
                off.authors.join(", "),
                off.top_contributor_files,
                off.top_contributor_lines,
                off.gone_before * 100.0,
                off.gone_after * 100.0
            );
            if !off.unknown.is_empty() {
                println!("  not authors here: {}", off.unknown.join(", "));
            }
            for (f, n) in &off.files {
                println!("  {n:>6}  {f}");
            }
        }
    }
}

pub fn reviewers(repos: &[crate::ops::ReviewersRepo]) {
    for r in repos {
        println!(
            "{}: {} changed files{}{}{}",
            repo_name(&r.root),
            r.files.len(),
            if r.left_out.is_empty() { String::new() } else { format!(" ({} changelogs, lockfiles or generated files aside)", r.left_out.len()) },
            r.base.as_deref().map(|b| format!("; ownership as of the base {}", &b[..b.len().min(12)])).unwrap_or_default(),
            if r.excluded.is_empty() { String::new() } else { format!("; leaving out {}", r.excluded.join(", ")) }
        );
        if r.reviewers.is_empty() {
            println!("  no current author has history in these files");
        }
        for v in &r.reviewers {
            println!("  {:>8.1}  {} <{}>  {} of the files, last {}", v.score, v.name, v.email, v.files, v.last);
        }
    }
}

/// A page of a list continued by a cursor (`knowcode more`): one line per row, its place and what it is.
pub fn page(env: &serde_json::Value) {
    let list = env["list"].as_str().unwrap_or("items");
    let offset = env["offset"].as_u64().unwrap_or(0);
    let rows = env["items"].as_array().cloned().unwrap_or_default();
    let total = env["total"].as_u64().map(|t| format!(" of {t}")).unwrap_or_default();
    println!("{list}: {}-{}{total}", offset + 1, offset + rows.len() as u64);
    for r in &rows {
        let repo = r["repo"].as_str().map(|r| format!("{r}: ")).unwrap_or_default();
        let at = match (r["file"].as_str(), r["line"].as_u64()) {
            (Some(f), Some(l)) => format!("{f}:{l}"),
            (Some(f), None) => f.to_string(),
            _ => String::new(),
        };
        let what = r["qualified"].as_str().or(r["name"].as_str()).or(r["text"].as_str()).unwrap_or("");
        let kind = r["kind"].as_str().map(|k| format!(" [{k}]")).unwrap_or_default();
        let conf = r["confidence"].as_str().map(|c| format!(" ({c})")).unwrap_or_default();
        match r["steps"].as_array() {
            Some(steps) => println!("  {}", steps.iter().filter_map(|s| s["qualified"].as_str()).collect::<Vec<_>>().join(" -> ")),
            None => println!("  {repo}{at}  {what}{kind}{conf}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kc_store::query::{Angular, NgUse, Target};

    fn ng(tag: &str, component: &str, line: u32) -> NgUse {
        NgUse { tag: tag.into(), component: Some(component.into()), repo: None, file: "a.html".into(), line, on: Vec::new() }
    }

    #[test]
    fn repeated_directive_uses_are_one_line() {
        let a = Angular {
            selector: None,
            template: None,
            routes: Vec::new(),
            rendered_by: Vec::new(),
            renders: vec![ng("[layout]", "LayoutDirective", 3), ng("x-card", "CardComponent", 4), ng("[layout]", "LayoutDirective", 9)],
            unresolved_attrs: Vec::new(),
        };
        assert_eq!(renders_lines(&a), ["renders [layout] LayoutDirective ×2  a.html:3,9", "renders <x-card> CardComponent  a.html:4"]);
    }

    #[test]
    fn same_named_candidates_show_their_place_and_kinds_their_languages_word() {
        let t = |file: &str| Target { id: 1, qualified: "web.web".into(), kind: "role".into(), file: file.into(), line: 1, repo: None };
        let h = RefHit {
            name: "web".into(),
            kind: "role".into(),
            receiver: None,
            receiver_type: None,
            file: "site.yml".into(),
            line: 3,
            col: 0,
            from: None,
            confidence: "exact",
            targets: vec![t("roles/web/tasks/main.yml"), t("roles/web/meta/main.yml")],
            repo: None,
            receiver_chain: Vec::new(),
            target_ids: Vec::new(),
            targets_total: None,
            via_di: None,
            via_di_any: Vec::new(),
            usage: None,
            ctors: None,
            cite: None,
            args: None,
            arg_types: None,
            assigns: false,
            precise: false,
            corrected_from: None,
            code: None,
            factory: false,
            through: None,
            basis: None,
            evidence: None,
        };
        assert!(ref_line(None, &h).contains("2 candidates: web.web (roles/web/tasks/main.yml:1), web.web (roles/web/meta/main.yml:1)"), "{}", ref_line(None, &h));
        // A capped confidence says why.
        let guessed = RefHit { basis: Some(kc_store::query::GUESSED_MODULE), ..h.clone() };
        assert!(ref_line(None, &guessed).ends_with(&format!("]  ({})", kc_store::query::GUESSED_MODULE)), "{}", ref_line(None, &guessed));
        assert_eq!((kw("protocol", "src/IRepo.cs"), kw("protocol", "a.ts"), kw("protocol", "A.swift")), ("interface", "interface", "protocol"));
    }
}
