//! Answers whole enough to stop on, so that an agent needn't re-check them with grep:
//!
//! - a definition lookup (`find`) says whether it found every declaration of the name: a C# `partial` type's parts, a
//!   type's constructors, a method's overloads, and how many references in code name it (uses, for `refs`);
//! - references (`refs`) come grouped by caller, code before tests, test files marked, overloads told apart by
//!   signature, with a one-line tally of callers and sites;
//! - an interface or base type (`refs`, `context`) lists what reaches it through other types (subclasses of its
//!   implementations, derived interfaces), its DI registrations and theirs, and says "none found" with what was
//!   searched for each list that came back empty.
//!
//! Each addition is one compact field beside the list it summarizes; rows gain at most a `test` or `overload` mark.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use kc_store::query::{self, RefHit, SymbolRow};
use rusqlite::Connection;
use serde_json::{Value, json};

use crate::ops::{Opened, file_lang, kind_word, repo_name};

/// At most this many groups of declarations are named in a `declared` statement.
const GROUPS_NAMED: usize = 8;
/// At most this many parts, overloads or constructors are named in one group.
const SITES_NAMED: usize = 12;
/// At most this many rows are listed in each list of a `hierarchy` block.
const HIERARCHY_LISTED: usize = 10;
/// At most this many overloads are numbered per name in a `refs` answer, and this many in all: past them, rows keep
/// their `targets`.
const OVERLOADS_NAMED: usize = 20;
const OVERLOADS_LISTED: usize = 30;
/// A signature is quoted up to this many characters.
const SIGNATURE_CHARS: usize = 100;

fn site(s: &SymbolRow) -> String {
    let test = if kc_store::is_test_path(&s.file) { " (test)" } else { "" };
    format!("{}:{}{test}", s.file, s.line)
}

/// Several declarations' sites, each directory and file said once: `in src/Item/ (Repo.cs:18, Repo.Query.cs:24,90)`;
/// a test file marked `test`. One declaration: `at src/Item/Repo.cs:18`.
fn sites(rows: &[SymbolRow]) -> String {
    if let [one] = rows {
        return format!("at {}", site(one));
    }
    // (directory, [(file name, lines, test)]), in the rows' order.
    type Files<'r> = Vec<(&'r str, Vec<u32>, bool)>;
    let mut dirs: Vec<(&str, Files)> = Vec::new();
    for r in rows.iter().take(SITES_NAMED) {
        let (dir, name) = r.file.rsplit_once('/').unwrap_or(("", r.file.as_str()));
        let at = match dirs.iter().position(|(d, _)| *d == dir) {
            Some(i) => i,
            None => {
                dirs.push((dir, Vec::new()));
                dirs.len() - 1
            }
        };
        match dirs[at].1.iter_mut().find(|(n, ..)| *n == name) {
            Some((_, lines, _)) => lines.push(r.line),
            None => dirs[at].1.push((name, vec![r.line], kc_store::is_test_path(&r.file))),
        }
    }
    let mut out: Vec<String> = dirs
        .iter()
        .map(|(dir, files)| {
            let files: Vec<String> = files
                .iter()
                .map(|(n, lines, test)| {
                    let lines: Vec<String> = lines.iter().map(u32::to_string).collect();
                    format!("{n}:{}{}", lines.join(","), if *test { " test" } else { "" })
                })
                .collect();
            if dir.is_empty() { files.join(", ") } else { format!("{dir}/ ({})", files.join(", ")) }
        })
        .collect();
    if rows.len() > SITES_NAMED {
        out.push(format!("{} more", rows.len() - SITES_NAMED));
    }
    format!("in {}", out.join(", "))
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// A declaration's signature from its name on (modifiers and return type dropped): `Get<T>(int id) where T : class`;
/// a constructor's from its type's name.
pub fn from_name(s: &SymbolRow) -> String {
    let name = if s.kind == "init" { s.container.as_deref().map_or(s.name.as_str(), |c| c.rsplit('.').next().unwrap_or(c)) } else { s.name.as_str() };
    let sig = s.signature.as_str();
    let at = sig.match_indices(name).map(|(i, _)| i).find(|&i| {
        let before = sig[..i].chars().next_back();
        let after = sig[i + name.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    });
    cut(at.map_or(sig, |i| &sig[i..]), SIGNATURE_CHARS)
}

/// The parenthesized list `text` starts with, through its closing parenthesis.
fn parameters(text: &str) -> &str {
    let mut depth = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[..=i];
                }
            }
            _ => {}
        }
    }
    text
}

/// A C# `partial` type declaration.
fn is_partial(s: &SymbolRow) -> bool {
    s.signature.split(|c: char| !c.is_alphanumeric()).any(|w| w == "partial")
}

/// A type declared with a primary constructor (`record Order(int Id)`, `class Repo(Db db)`): its parameter list.
fn primary_constructor(s: &SymbolRow) -> Option<String> {
    let sig = from_name(s);
    let rest = sig.strip_prefix(s.name.as_str())?;
    let rest = match rest.strip_prefix('<') {
        Some(r) => r.split_once('>')?.1,
        None => rest,
    };
    rest.starts_with('(').then(|| parameters(rest).to_string())
}

/// The statement a definition lookup of `q` adds for one repo: every declaration of the name there, grouped (a partial
/// type's parts, overloads), a type's constructors, and how many references in code name it. `None` when the query
/// isn't a name or nothing there declares it exactly.
pub fn declared(o: &Opened, q: &str, kind: Option<&str>) -> Result<Option<String>> {
    let q = q.trim();
    let (_, bare) = query::split_dotted(q);
    if bare.is_empty() || q.contains(['#', ':', '/', ' ']) {
        return Ok(None);
    }
    let conn = o.store.conn();
    let defs: Vec<SymbolRow> = query::definitions(conn, q)?.into_iter().filter(|d| d.name == bare && query::kind_matches(kind, &d.kind)).collect();
    if defs.is_empty() {
        return Ok(None);
    }
    // One group per qualified name, namespace and kind: a partial type's parts, a method's overloads.
    let key = |s: &SymbolRow| (s.qualified(), s.namespace.clone(), s.kind.clone());
    let mut groups: Vec<Vec<SymbolRow>> = Vec::new();
    for d in &defs {
        match groups.iter_mut().find(|g| key(&g[0]) == key(d)) {
            Some(g) => g.push(d.clone()),
            None => groups.push(vec![d.clone()]),
        }
    }
    let mut descs = Vec::new();
    for g in groups.iter().take(GROUPS_NAMED) {
        let s = &g[0];
        let label = match &s.namespace {
            Some(ns) if !ns.is_empty() => format!("{ns}.{}", s.qualified()),
            _ => s.qualified(),
        };
        let kw = kind_word(&s.kind, file_lang(&s.file));
        let mut d = if query::is_type_or_extension(&s.kind) {
            if g.len() > 1 && g.iter().all(is_partial) {
                format!("{kw} {label}, partial in {} parts {}", g.len(), sites(g))
            } else if g.len() > 1 {
                format!("{kw} {label}, declared {} times {}", g.len(), sites(g))
            } else {
                format!("{kw} {label} at {}", site(s))
            }
        } else if g.len() > 1 {
            format!("{} overloads of {kw} {label} {}", g.len(), sites(g))
        } else {
            format!("{kw} {label} at {}", site(s))
        };
        // A static class has no constructor to call.
        let is_static = g.iter().any(|r| r.signature.split_whitespace().any(|w| w == "static"));
        if matches!(s.kind.as_str(), "class" | "struct" | "actor") && !is_static {
            // Another group of the same name (another namespace) has its own constructors: keep to this one's files.
            let shared = groups.iter().filter(|o| o[0].qualified() == s.qualified()).count() > 1;
            let files: Vec<&str> = if shared { g.iter().map(|r| r.file.as_str()).collect() } else { Vec::new() };
            let ctors = query::constructors(conn, s, &files)?;
            let primary = g.iter().find_map(primary_constructor);
            let mut made: Vec<String> = Vec::new();
            if let Some(p) = primary {
                made.push(format!("primary {p}"));
            }
            made.extend(ctors.iter().take(SITES_NAMED).map(|c| {
                let sig = from_name(c);
                let params = sig.find('(').map_or(sig.as_str(), |i| &sig[i..]);
                let params: String = parameters(params).split_whitespace().collect::<Vec<_>>().join(" ").replace("( ", "(");
                // In the type's own file (said just before): its line only.
                let at = if g.len() == 1 && c.file == s.file { format!("line {}", c.line) } else { site(c) };
                format!("{at} {params}")
            }));
            if ctors.len() > SITES_NAMED {
                made.push(format!("{} more", ctors.len() - SITES_NAMED));
            }
            let n = ctors.len() + usize::from(made.first().is_some_and(|m| m.starts_with("primary ")));
            d.push_str(&match n {
                0 => "; no constructor declared".to_string(),
                1 => format!("; 1 constructor: {}", made.join("; ")),
                n => format!("; {n} constructors: {}", made.join("; ")),
            });
        }
        descs.push(d);
    }
    if groups.len() > GROUPS_NAMED {
        descs.push(format!("{} more (in items)", groups.len() - GROUPS_NAMED));
    }
    let repo = repo_name(&o.root);
    let what = kind.map_or("declaration".to_string(), |k| format!("{k} declaration"));
    let n = defs.len();
    let head = if n == 1 { format!("the only {what} of {q} in {repo}") } else { format!("the only {n} {what}s of {q} in {repo}") };
    let mentions: i64 = conn.prepare_cached("SELECT COUNT(*) FROM refs WHERE name = ?1 AND name = ?1 COLLATE BINARY")?.query_row([bare], |r| r.get(0))?;
    let uses = match mentions {
        0 => format!("nothing in code references {bare}"),
        1 => format!("every other mention in code is a use: 1 reference (kc_refs {q})"),
        m => format!("every other mention in code is a use: {m} references (kc_refs {q})"),
    };
    Ok(Some(format!("{head}: {}. {}{}", descs.join("; "), uses[..1].to_uppercase(), &uses[1..])))
}

/// Who makes a reference, for grouping: its repo, file and enclosing symbol.
fn caller_key(h: &RefHit) -> (Option<String>, String, Option<i64>, Option<u32>) {
    let id = h.from.as_ref().map(|f| f.id);
    // A reference outside any symbol (a script's top level) is its own caller.
    (h.repo.clone(), h.file.clone(), id, id.is_none().then_some(h.line))
}

/// Orders references by caller: each caller's references together, at the place of its first; code before tests.
pub fn group_by_caller(hits: &mut Vec<RefHit>) {
    let mut first: HashMap<_, usize> = HashMap::new();
    for (i, h) in hits.iter().enumerate() {
        first.entry(caller_key(h)).or_insert(i);
    }
    let mut keyed: Vec<((bool, usize, usize), RefHit)> =
        hits.drain(..).enumerate().map(|(i, h)| ((kc_store::is_test_path(&h.file), first[&caller_key(&h)], i), h)).collect();
    keyed.sort_by_key(|(k, _)| *k);
    hits.extend(keyed.into_iter().map(|(_, h)| h));
}

/// A caller: the repo answering, then [`caller_key`].
type Caller = (PathBuf, Option<String>, String, Option<i64>, Option<u32>);

/// The callers and sites of a references answer, counted over every reference found (not only those listed).
#[derive(Default)]
pub struct Callers {
    callers: HashSet<Caller>,
    test_callers: HashSet<Caller>,
    sites: usize,
    test_sites: usize,
    /// The definitions the references resolve to, by qualified name (overloads are one).
    targets: BTreeSet<String>,
}

impl Callers {
    pub fn add<'h>(&mut self, root: &Path, hits: impl IntoIterator<Item = &'h RefHit>) {
        for h in hits {
            let (repo, file, id, line) = caller_key(h);
            let key = (root.to_path_buf(), repo, file, id, line);
            if kc_store::is_test_path(&h.file) {
                self.test_sites += 1;
                self.test_callers.insert(key.clone());
            }
            self.sites += 1;
            self.callers.insert(key);
            if !matches!(h.confidence, "unresolved" | "external_likely") {
                self.targets.extend(h.targets.iter().map(|t| t.qualified.clone()));
            }
        }
    }

    /// `7 callers at 10 sites; in tests: 3 callers at 3 sites`, and when the references resolve to several
    /// definitions of the name, which (`Type.name` narrows); `None` for fewer than two references.
    pub fn statement(&self, name: &str) -> Option<String> {
        if self.sites < 2 {
            return None;
        }
        let n = |k: usize, one: &str, many: &str| if k == 1 { format!("1 {one}") } else { format!("{k} {many}") };
        let mut s = format!("{} at {}", n(self.callers.len(), "caller", "callers"), n(self.sites, "site", "sites"));
        if self.test_sites > 0 {
            s.push_str(&format!("; in tests: {} at {}", n(self.test_callers.len(), "caller", "callers"), n(self.test_sites, "site", "sites")));
        }
        if self.targets.len() > 1 && !name.contains('.') {
            let named: Vec<&str> = self.targets.iter().take(5).map(String::as_str).collect();
            let more = if self.targets.len() > 5 { format!(" and {} more", self.targets.len() - 5) } else { String::new() };
            s.push_str(&format!("; resolving to {} definitions named {name}: {}{more} (Type.{name} keeps one)", self.targets.len(), named.join(", ")));
        }
        Some(s)
    }
}

/// The overloads a references answer resolves to, numbered: each row says which (`overload`), or which it may be.
#[derive(Default)]
pub struct Overloads {
    pub list: Vec<String>,
    index: HashMap<(PathBuf, i64), usize>,
}

/// The definitions a reference may reach: for a creation, the constructors its arguments pick; else its targets.
fn candidates(h: &RefHit) -> Vec<i64> {
    match &h.ctors {
        Some(c) if h.kind == "instantiate" && !c.is_empty() => c.clone(),
        _ => h.target_ids.clone(),
    }
}

impl Overloads {
    /// Numbers the overloads `hits` (references found in the repo at `root`) resolve to: the callables sharing a
    /// qualified name, namespace and (for a free function) file with another, each with how many references reach it
    /// alone. Same-named free functions of two modules are not overloads.
    pub fn add(&mut self, conn: &Connection, root: &Path, hits: &[RefHit]) -> Result<()> {
        let ids: BTreeSet<i64> = hits.iter().flat_map(candidates).take(200).collect();
        let mut names: BTreeSet<(String, String)> = BTreeSet::new();
        for id in &ids {
            if let Some(s) = kc_store::trace::symbol(conn, *id)?
                && matches!(s.kind.as_str(), "method" | "function" | "init")
            {
                names.insert((s.qualified(), s.kind));
            }
        }
        let mut alone: HashMap<i64, usize> = HashMap::new();
        for h in hits {
            if let [one] = candidates(h).as_slice() {
                *alone.entry(*one).or_default() += 1;
            }
        }
        for (qualified, kind) in names {
            let all: Vec<SymbolRow> = query::definitions(conn, &qualified)?.into_iter().filter(|d| d.kind == kind && d.qualified() == qualified).collect();
            // One overload set per namespace, and per file for free functions.
            let set = |d: &SymbolRow| (d.namespace.clone(), d.container.is_none().then(|| d.file.clone()));
            let mut sets: Vec<(_, Vec<SymbolRow>)> = Vec::new();
            for d in all {
                match sets.iter_mut().find(|(k, _)| *k == set(&d)) {
                    Some((_, v)) => v.push(d),
                    None => sets.push((set(&d), vec![d])),
                }
            }
            for (_, mut defs) in sets {
                if defs.len() < 2 || !defs.iter().any(|d| ids.contains(&d.id)) || self.list.len() + defs.len().min(OVERLOADS_NAMED) > OVERLOADS_LISTED {
                    continue;
                }
                defs.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
                for d in defs.iter().take(OVERLOADS_NAMED) {
                    let n = self.list.len() + 1;
                    self.index.insert((root.to_path_buf(), d.id), n);
                    let reached = match alone.get(&d.id).copied().unwrap_or(0) {
                        0 => String::new(),
                        1 => " (1 site)".to_string(),
                        k => format!(" ({k} sites)"),
                    };
                    self.list.push(format!("{n}: {} at {}{reached}", from_name(d), site(d)));
                }
            }
        }
        Ok(())
    }

    /// Marks a reference row: `overload` names the overload it calls, or those it may call. A call's `targets`, all
    /// overloads listed, go (the list says them once); a creation keeps its type as its target.
    pub fn mark(&self, root: &Path, h: &RefHit, row: &mut Value) {
        let c = candidates(h);
        if c.is_empty() {
            return;
        }
        let Some(mut ns) = c.iter().map(|id| self.index.get(&(root.to_path_buf(), *id)).copied()).collect::<Option<Vec<usize>>>() else { return };
        ns.sort_unstable();
        ns.dedup();
        if let Some(m) = row.as_object_mut() {
            if h.kind != "instantiate" {
                m.remove("targets");
                m.remove("targets_total");
            }
            m.insert("overload".into(), if ns.len() == 1 { json!(ns[0]) } else { json!(ns) });
        }
    }
}

/// How many of each list an interface or base type's answer found, for [`none_found`].
pub struct Found {
    pub implementers: usize,
    pub derived: usize,
    pub registrations: usize,
    /// A list was cut before its kinds were all seen: no "none found" can be said of the kinds it holds.
    pub cut: bool,
}

/// Whether `s` is an interface, or a class others derive from (or abstract): what `hierarchy` and `none_found`
/// speak of.
pub fn is_base(s: &SymbolRow, has_subtypes: bool) -> bool {
    s.kind == "protocol" || (s.kind == "class" && (has_subtypes || s.signature.split_whitespace().any(|w| w == "abstract")))
}

/// For an interface or base type `s` of the repo `repo`: each of its lists that came back empty, said as "none
/// found" with what was searched.
pub fn none_found(conn: &Connection, s: &SymbolRow, repo: &str, f: &Found) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let types = format!("every type's base list in {repo} and its indexed dependents");
    if s.kind == "protocol" {
        if f.implementers == 0 && !f.cut {
            out.push(format!("implementations: none found (searched: {types}, through base classes and derived interfaces)"));
        }
        if f.derived == 0 && !f.cut {
            out.push(format!("derived interfaces: none found (searched: {types})"));
        }
    } else if f.implementers == 0 && !f.cut {
        out.push(format!("subclasses: none found (searched: {types})"));
    }
    if s.file.ends_with(".cs") && f.registrations == 0 {
        out.push(if kc_store::di::any(conn)? {
            format!(
                "DI registrations: none found (searched: Add/TryAdd Scoped/Singleton/Transient (keyed too), AddHttpClient, Autofac Register*/As*/AsImplementedInterfaces in {repo}, of it, its derived interfaces and its implementations)"
            )
        } else {
            format!("DI registrations: none found ({repo} registers no services through Add/TryAdd Scoped/Singleton/Transient or Autofac Register*)")
        })
    }
    Ok(out)
}

/// What an interface or base type named `name` has around it, for `refs`: its implementations or subclasses (here and
/// in the indexed repos depending on this one), those reaching it through another type, its derived interfaces, its
/// DI registrations and theirs, and "none found" for each empty list. `None` when `name` names no interface or base
/// type in the repo.
pub fn hierarchy(o: &Opened, name: &str, no_sync: bool) -> Result<Option<Value>> {
    let conn = o.store.conn();
    let Some(ty) = query::definitions(conn, name)?.into_iter().find(|d| matches!(d.kind.as_str(), "class" | "protocol")) else { return Ok(None) };
    let direct = query::direct_conformers(conn, &ty.name)?;
    let (indirect, indirect_n) = query::indirect_conformers(conn, &ty.name, query::INDIRECT_READ)?;
    let elsewhere = crate::ops::dependent_conformers(o, &ty, None, no_sync)?;
    if !is_base(&ty, !direct.is_empty() || !elsewhere.is_empty()) {
        return Ok(None);
    }
    let mut implementers: Vec<String> = Vec::new();
    let mut derived: Vec<String> = Vec::new();
    for c in &direct {
        let row = format!("{} {}", c.qualified(), site(c));
        if c.kind == "protocol" { derived.push(row) } else { implementers.push(row) }
    }
    for e in &elsewhere {
        let row = format!("{} {}:{}", e.symbol.qualified(), e.repo, site(&e.symbol));
        if e.symbol.kind == "protocol" { derived.push(row) } else { implementers.push(row) }
    }
    let mut through: Vec<String> = Vec::new();
    for i in indirect {
        let protocol = i.kind == "protocol";
        let row = String::from(i);
        if protocol { derived.push(row) } else { through.push(row) }
    }
    let mut di: Vec<String> = Vec::new();
    if ty.file.ends_with(".cs") {
        for r in kc_store::di::registrations_of(conn, &ty.name)? {
            let service = r.service.as_deref().unwrap_or(&ty.name);
            let made = r.implementation.as_deref().unwrap_or("(factory)");
            di.push(format!("{}:{} {service} → {made} ({}{})", r.file, r.line, r.lifetime, if r.test { ", test" } else { "" }));
        }
        di.extend(kc_store::di::through_hierarchy(conn, &ty.name, usize::MAX)?);
    }
    let found = Found { implementers: implementers.len() + through.len(), derived: derived.len(), registrations: di.len(), cut: indirect_n >= query::INDIRECT_READ };
    let none = none_found(conn, &ty, &repo_name(&o.root), &found)?;
    let mut block = serde_json::Map::new();
    let listed = |mut rows: Vec<String>| -> Value {
        if rows.len() > HIERARCHY_LISTED {
            let more = rows.len() - HIERARCHY_LISTED;
            rows.truncate(HIERARCHY_LISTED);
            rows.push(format!("… {more} more: kc_context {name} lists them"));
        }
        json!(rows)
    };
    let own = if ty.kind == "protocol" { "implementers" } else { "subclasses" };
    for (k, rows) in [(own, implementers), ("through", through), ("derived", derived), ("di", di)] {
        if !rows.is_empty() {
            block.insert(k.into(), listed(rows));
        }
    }
    if !none.is_empty() {
        block.insert("none_found".into(), json!(none));
    }
    Ok(Some(Value::Object(block)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{repo, test_cache};
    use crate::ops::{self, Scope};

    fn scope(root: &Path) -> Scope {
        Scope { repos: vec![root.to_path_buf()], no_sync: false }
    }

    const PROJECT: (&str, &str) = ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");

    #[test]
    fn signatures_from_the_name_on() {
        let row = |name: &str, kind: &str, container: Option<&str>, sig: &str| SymbolRow {
            id: 1,
            name: name.into(),
            kind: kind.into(),
            container: container.map(str::to_string),
            signature: sig.into(),
            doc: None,
            attrs: String::new(),
            tags: String::new(),
            file: "src/a.cs".into(),
            line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 0,
            complexity: 0,
            namespace: None,
        };
        assert_eq!(from_name(&row("Get", "method", Some("Repo"), "public static T Get<T>(int id) where T : class")), "Get<T>(int id) where T : class");
        assert_eq!(from_name(&row("init", "init", Some("Ns.Repo"), "public Repo(Db db) : base(db)")), "Repo(Db db) : base(db)");
        // The name as a word, not inside another (`GetAll` before `Get`).
        assert_eq!(from_name(&row("Get", "method", None, "Task<GetResult> Get(string key)")), "Get(string key)");
        assert_eq!(primary_constructor(&row("Order", "class", None, "public record Order(int Id, string Name)")).as_deref(), Some("(int Id, string Name)"));
        assert_eq!(primary_constructor(&row("Box", "class", None, "public class Box<T>(T value)")).as_deref(), Some("(T value)"));
        assert_eq!(primary_constructor(&row("Order", "class", None, "public sealed class Order : Entity")), None);
    }

    /// A lookup by name says it has every declaration: a partial type's parts with their sites, its constructors, and
    /// that the other mentions are uses; a method's overloads.
    #[test]
    fn find_says_every_declaration_of_the_name() {
        let cache = test_cache();
        let base = cache.dir("whole-find");
        let root = repo(
            &base,
            "shop",
            &[
                PROJECT,
                ("src/Cart.cs", "namespace Shop;\npublic partial class Cart : ICart\n{\n    public Cart(int id) { }\n    public Cart() : this(0) { }\n    public void Add(int sku) { }\n    public void Add(int sku, int qty) { }\n}\n"),
                ("src/Cart.Pricing.cs", "namespace Shop;\npublic partial class Cart\n{\n    public decimal Total() => 0;\n}\n"),
                ("src/ICart.cs", "namespace Shop;\npublic interface ICart { }\n"),
                ("src/Checkout.cs", "namespace Shop;\npublic class Checkout\n{\n    public void Run() { var c = new Cart(1); c.Add(1); c.Add(1, 2); }\n}\n"),
                ("src/Order.cs", "namespace Shop;\npublic record Order(int Id);\n"),
            ],
        );
        let out = ops::find("Cart", None, 10, &scope(&root)).unwrap();
        let said = out.env["declared"].as_str().unwrap_or_else(|| panic!("{}", out.env));
        assert!(said.starts_with("the only 2 declarations of Cart in shop: class Shop.Cart, partial in 2 parts in src/ (Cart.Pricing.cs:2, Cart.cs:2)"), "{said}");
        assert!(said.contains("2 constructors: src/Cart.cs:4 (int id); src/Cart.cs:5 ()"), "{said}");
        assert!(said.ends_with(". Every other mention in code is a use: 1 reference (kc_refs Cart)"), "{said}");

        let said = ops::find("Cart.Add", None, 10, &scope(&root)).unwrap().env["declared"].as_str().unwrap().to_string();
        assert!(said.contains("2 overloads of method Shop.Cart.Add in src/ (Cart.cs:6,7)"), "{said}");

        let said = ops::find("Order", None, 10, &scope(&root)).unwrap().env["declared"].as_str().unwrap().to_string();
        assert!(said.contains("the only declaration of Order in shop") && said.contains("1 constructor: primary (int Id)"), "{said}");

        // A prefix or a name nothing declares exactly: no statement.
        assert!(ops::find("Car", None, 10, &scope(&root)).unwrap().env.get("declared").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `context` on a partial type lists its parts (not as other definitions), and its constructors first.
    #[test]
    fn context_names_a_partial_types_parts_and_its_constructors_first() {
        let cache = test_cache();
        let base = cache.dir("whole-parts");
        let mut big = String::from("namespace Shop;\npublic partial class Cart\n{\n");
        for i in 0..12 {
            big.push_str(&format!("    public int P{i} {{ get; set; }}\n"));
        }
        big.push_str("    public Cart(int id) { }\n}\n");
        let root = repo(&base, "shop", &[PROJECT, ("src/Cart.cs", &big), ("src/Cart.More.cs", "namespace Shop;\npublic partial class Cart\n{\n    public void Clear() { }\n}\n")]);
        let out = ops::context("Cart", 300, kc_store::skeleton::SourceMode::None, &scope(&root)).unwrap();
        let item = &out.env["items"][0];
        assert_eq!(item["parts"], json!(["src/Cart.More.cs:2", "src/Cart.cs:2"]), "{item}");
        assert!(item.get("alternatives").is_none(), "{item}");
        assert_eq!(item["members"][0]["kind"], "init", "{item}");
        let _ = std::fs::remove_dir_all(&base);
    }

    const HIERARCHY: &str = r#"
namespace Shop;
public interface IStore { void Save(); }
public interface IBulkStore : IStore { void SaveAll(); }
public abstract class StoreBase : IStore { public abstract void Save(); }
public class SqlStore : StoreBase { public override void Save() { } }
public class CachedSqlStore : SqlStore { }
public class BulkStore : IBulkStore { public void Save() { } public void SaveAll() { } }
public interface ILonely { }
"#;

    const STARTUP: &str = r#"
namespace Shop;
public class Startup
{
    public void Configure(IServiceCollection services, ContainerBuilder builder)
    {
        services.AddScoped<IStore, SqlStore>();
        services.AddSingleton<IBulkStore, BulkStore>();
        builder.RegisterType<CachedSqlStore>().AsSelf();
    }
}
"#;

    /// An interface's context and references list what reaches it through other types (a subclass of an
    /// implementation, a derived interface's implementation), the DI registrations of it and of those, and "none found"
    /// with what was searched for each empty list.
    #[test]
    fn interfaces_list_transitive_implementers_derived_interfaces_and_registrations() {
        let cache = test_cache();
        let base = cache.dir("whole-hierarchy");
        let root = repo(&base, "shop", &[PROJECT, ("src/Stores.cs", HIERARCHY), ("src/Startup.cs", STARTUP)]);
        let out = ops::context("IStore", 1200, kc_store::skeleton::SourceMode::None, &scope(&root)).unwrap();
        let item = &out.env["items"][0];
        assert_eq!(
            item["conformers_indirect"],
            json!([
                "BulkStore src/Stores.cs:8 via IBulkStore",
                "SqlStore src/Stores.cs:6 via StoreBase",
                "CachedSqlStore src/Stores.cs:7 via SqlStore"
            ]),
            "{item}"
        );
        let regs = item["di_implementers"].as_array().unwrap();
        assert_eq!(*regs, [json!("src/Startup.cs:8 IBulkStore → BulkStore (singleton)"), json!("src/Startup.cs:9 CachedSqlStore (default)")], "{item}");
        assert!(item.get("none_found").is_none(), "{item}");

        let refs = ops::refs("IStore", &[], 20, &scope(&root)).unwrap().env;
        let h = &refs["hierarchy"];
        assert_eq!(h["implementers"], json!(["StoreBase src/Stores.cs:5"]), "{h}");
        assert_eq!(h["derived"], json!(["IBulkStore src/Stores.cs:4"]), "{h}");
        assert_eq!(h["through"].as_array().unwrap().len(), 3, "{h}");
        assert!(h["di"].as_array().unwrap().iter().any(|r| r.as_str().unwrap().starts_with("src/Startup.cs:7 IStore → SqlStore")), "{h}");

        // An interface nothing implements, derives from or registers: each list said empty, with what was searched.
        let lonely = ops::context("ILonely", 1200, kc_store::skeleton::SourceMode::None, &scope(&root)).unwrap();
        let none: Vec<&str> = lonely.env["items"][0]["none_found"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(none.len(), 3, "{none:?}");
        assert!(none[0].starts_with("implementations: none found (searched: every type's base list in shop"), "{none:?}");
        assert!(none[1].starts_with("derived interfaces: none found (searched: "), "{none:?}");
        assert!(none[2].starts_with("DI registrations: none found (searched: ") && none[2].contains("Autofac"), "{none:?}");
        let refs = ops::refs("ILonely", &[], 20, &scope(&root)).unwrap().env;
        assert_eq!(refs["hierarchy"]["none_found"].as_array().map(Vec::len), Some(3), "{refs}");

        // A plain class others don't derive from has no hierarchy block.
        assert!(ops::refs("Startup", &[], 20, &scope(&root)).unwrap().env.get("hierarchy").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// References to a method come grouped by caller, code before tests, test rows marked, each row naming the
    /// overload it calls from a numbered list of their signatures, with a tally of callers and sites.
    #[test]
    fn refs_group_by_caller_mark_tests_and_tell_overloads_apart() {
        let cache = test_cache();
        let base = cache.dir("whole-refs");
        let root = repo(
            &base,
            "shop",
            &[
                PROJECT,
                (
                    "src/Audit.cs",
                    "namespace Shop;\npublic class Audit\n{\n    public void Log(string m) { }\n    public void Log(string m, int level) { }\n}\npublic class Journal\n{\n    public void Log(string m) { }\n}\n",
                ),
                // Sorts before `src/`: a test file comes last all the same.
                ("Shop.Tests/AuditTests.cs", "namespace Shop.Tests;\npublic class AuditTests\n{\n    private readonly Shop.Audit _a;\n    public void Logs() { _a.Log(\"t\"); }\n}\n"),
                (
                    "src/Checkout.cs",
                    "namespace Shop;\npublic class Checkout\n{\n    private readonly Audit _audit;\n    public void Run(dynamic any)\n    {\n        any.Log(\"q\");\n        _audit.Log(\"a\");\n        Pay();\n        _audit.Log(\"b\", 2);\n    }\n    public void Pay() { _audit.Log(\"p\"); _audit.Log(\"p2\"); }\n    public void Ship() { _audit.Log(\"s\"); }\n}\n",
                ),
            ],
        );
        let env = ops::refs("Log", &[], 20, &scope(&root)).unwrap().env;
        let rows = env["items"].as_array().unwrap();
        let order: Vec<(&str, u64, &str)> =
            rows.iter().map(|r| (r["from"]["name"].as_str().unwrap(), r["line"].as_u64().unwrap(), r["confidence"].as_str().unwrap())).collect();
        // A long list goes most certain first; `Run`'s unresolved call stays with its others all the same.
        assert_eq!(
            order,
            [("Run", 8, "exact"), ("Run", 10, "exact"), ("Run", 7, "unresolved"), ("Pay", 12, "exact"), ("Pay", 12, "exact"), ("Ship", 13, "exact"), ("Logs", 5, "exact")],
            "{env}"
        );
        assert!(rows[..6].iter().all(|r| r.get("test").is_none()) && rows[6]["test"] == true, "{env}");
        assert_eq!(env["callers"], "4 callers at 7 sites; in tests: 1 caller at 1 site", "{env}");
        let overloads: Vec<&str> = env["overloads"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(overloads, ["1: Log(string m) at src/Audit.cs:4 (5 sites)", "2: Log(string m, int level) at src/Audit.cs:5 (1 site)"], "{env}");
        let marks: Vec<Value> = rows.iter().map(|r| r["overload"].clone()).collect();
        assert_eq!(marks, [json!(1), json!(2), Value::Null, json!(1), json!(1), json!(1), json!(1)], "{env}");
        // The overload says the target: a resolved row's `targets` go.
        assert!(rows.iter().all(|r| r.get("targets").is_none()), "{env}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
