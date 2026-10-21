//! Cited source: the lines each row of an answer rests on, quoted from the working tree when the answer is built
//! (see [`kc_store::answer::Cite`] and docs/answers.md#cited-source).
//!
//! A row reached by an edge cites the edge's site (the call, the request, the publish): [`SITE_LINES`] line. A row
//! without one (a walk's start, a path's ends) cites its declaration: from its first line through the line naming
//! it, at most [`HEAD_LINES`] lines. Each line is cut at [`LINE_CHARS`] characters. Whatever a cap leaves out, the
//! cite says in `cut`.
//!
//! How much rows quote is the query's (see [`kc_store::limits::Operation::cite`]). `brief`, the default, quotes the
//! first [`BRIEF_CITES`] cites one line each, up to [`BRIEF_CHARS`] characters, and gives the rest their place only,
//! saying so in the answer's `incomplete` ([`Citer::incomplete`]); `lines` quotes every row to the caps above;
//! `none` leaves `cite` out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use kc_store::answer::{Cite, CiteMode, Incomplete, Reason};
use kc_store::query::RefHit;
use kc_store::trace::{Impact, Trace};

use crate::ops::repo_name;
use crate::flow::FlowStep;
use crate::walk::{PathAnswer, Step};

/// Lines quoted for an edge's site.
pub const SITE_LINES: usize = 1;
/// Lines quoted at most for a declaration: from its first line (an attribute, a decorator) through the one naming it.
pub const HEAD_LINES: usize = 3;
/// Characters quoted at most per line.
pub const LINE_CHARS: usize = 160;
/// Under `cite: brief`: the cites that quote a line, in the answer's order.
pub const BRIEF_CITES: usize = 12;
/// Under `cite: brief`: characters quoted at most per line.
pub const BRIEF_CHARS: usize = 120;

/// A file as the working tree has it now, read once per answer.
enum Now {
    Lines(Vec<String>),
    /// Not read: why.
    Unread(String),
}

/// Quotes the lines an answer's rows rest on, reading each file once.
pub struct Citer {
    mode: CiteMode,
    /// The repos rows may name (`repo`), by their short name.
    roots: Vec<PathBuf>,
    /// Every indexed repo, read when a row names a repo `roots` doesn't hold (a dependent's reference).
    indexed: Option<Vec<PathBuf>>,
    files: HashMap<PathBuf, Rc<Now>>,
    privacy: HashMap<PathBuf, kc_index::privacy::Privacy>,
    /// Cites that quoted a line, and those `cite: brief` left without one past [`BRIEF_CITES`].
    quoted: usize,
    bare: usize,
    /// The places quoted so far: quoting one again doesn't count against [`BRIEF_CITES`].
    spans: std::collections::HashSet<String>,
}

impl Citer {
    /// A citer for the query this thread runs, whose rows may be in the repos at `roots`.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Citer {
        Citer {
            mode: kc_store::limits::current().cite(),
            roots: roots.into_iter().collect(),
            indexed: None,
            files: HashMap::new(),
            privacy: HashMap::new(),
            quoted: 0,
            bare: 0,
            spans: Default::default(),
        }
    }

    /// What the answer's `incomplete` says about its cites: under `cite: brief`, the ones past [`BRIEF_CITES`] that
    /// quote nothing.
    pub fn incomplete(&self) -> Vec<Incomplete> {
        if self.bare == 0 {
            return Vec::new();
        }
        let total = self.quoted + self.bare;
        vec![
            Incomplete::new(
                Reason::Capped,
                format!("cite text: {} of {total} cites quote their line; the other {} give only where it is", self.quoted, self.bare),
            )
            .with_hint("cite: lines (--cite lines) quotes every row"),
        ]
    }

    /// Whether the query's rows cite their lines.
    pub fn on(&self) -> bool {
        self.mode != CiteMode::None
    }

    /// The repo a row names (`repo`: its short name, or a path), `home` when it names none: one the answer read,
    /// else the one indexed repo of the name; `None` when none is, or several are.
    fn root_of(&mut self, home: &Path, repo: Option<&str>) -> Option<PathBuf> {
        let Some(name) = repo else { return Some(home.to_path_buf()) };
        if Path::new(name).is_absolute() {
            return Some(PathBuf::from(name));
        }
        let one = |roots: &[PathBuf]| {
            let mut named = roots.iter().filter(|r| repo_name(r) == name);
            match (named.next(), named.next()) {
                (Some(r), None) => Some(Some(r.clone())),
                (Some(_), Some(_)) => Some(None),
                _ => None,
            }
        };
        if let Some(found) = one(&self.roots) {
            return found;
        }
        let indexed = self.indexed.get_or_insert_with(|| kc_index::indexed_roots().unwrap_or_default());
        one(indexed).flatten()
    }

    fn read(&mut self, root: &Path, file: &str) -> Rc<Now> {
        let path = root.join(file);
        if let Some(now) = self.files.get(&path) {
            return now.clone();
        }
        let privacy = self.privacy.entry(root.to_path_buf()).or_insert_with(|| kc_index::privacy::Privacy::load(root));
        let now = if privacy.blocks(file) {
            Now::Unread("not quoted: the repo's AI privacy file blocks it".into())
        } else {
            // A symbolic link is not read: the index never follows one.
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_symlink() => Now::Unread("not quoted: a symbolic link".into()),
                Ok(_) => match std::fs::read(&path) {
                    Ok(bytes) => Now::Lines(kc_extract::decode(&bytes).lines().map(|l| l.trim_end().to_string()).collect()),
                    Err(e) => Now::Unread(format!("not quoted: the file could not be read ({e})")),
                },
                Err(_) => Now::Unread("not quoted: the file is not in the working tree".into()),
            }
        };
        let now = Rc::new(now);
        self.files.insert(path, now.clone());
        now
    }

    /// The cite of lines `start..start+n` of `file` in the repo at `root`, `cut` saying what the caller's cap left out.
    fn quote(&mut self, root: &Path, file: &str, start: u32, n: usize, mut cut: Vec<String>) -> Cite {
        let start = start.max(1);
        let span = |n: usize| if n <= 1 { format!("{file}:{start}") } else { format!("{file}:{start}-{}", start as usize + n - 1) };
        let brief = self.mode == CiteMode::Brief;
        // Past the brief cap: the place only.
        let again = self.spans.contains(&format!("{}|{}", root.display(), span(n)));
        if brief && self.quoted >= BRIEF_CITES && !again {
            self.bare += 1;
            return Cite { at: span(n), repo: None, text: None, cut: None };
        }
        let width = if brief { BRIEF_CHARS } else { LINE_CHARS };
        let lines = match &*self.read(root, file) {
            Now::Lines(lines) => lines.iter().skip(start as usize - 1).take(n).cloned().collect::<Vec<_>>(),
            Now::Unread(why) => return Cite { at: span(n), repo: None, text: None, cut: Some(why.clone()) },
        };
        if lines.is_empty() {
            let total = match &*self.read(root, file) {
                Now::Lines(l) => l.len(),
                Now::Unread(_) => 0,
            };
            return Cite { at: span(n), repo: None, text: None, cut: Some(format!("not quoted: line {start} is past the end of the file ({total} lines)")) };
        }
        let indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
        let mut text = Vec::with_capacity(lines.len());
        for (i, l) in lines.iter().enumerate() {
            let l = l.get(indent..).unwrap_or(l.trim_start());
            let chars = l.chars().count();
            if chars > width {
                let at = l.char_indices().nth(width).map_or(l.len(), |(i, _)| i);
                text.push(format!("{}…", &l[..at]));
                cut.push(format!("line {} cut at {width} of {chars} characters", start as usize + i));
            } else {
                text.push(l.to_string());
            }
        }
        if self.spans.insert(format!("{}|{}", root.display(), span(n))) {
            self.quoted += 1;
        }
        Cite { at: span(lines.len()), repo: None, text: Some(text.join("\n")), cut: (!cut.is_empty()).then(|| cut.join("; ")) }
    }

    /// The cite of an edge's site (`file:line`) in the repo at `root`.
    pub fn site(&mut self, root: &Path, site: &str) -> Option<Cite> {
        let (file, line) = site.rsplit_once(':')?;
        let line: u32 = line.parse().ok()?;
        Some(self.quote(root, file, line, SITE_LINES, Vec::new()))
    }

    /// The cite of a declaration starting at `line` of `file` in the repo at `root`: through the line naming `name`,
    /// at most [`HEAD_LINES`] lines.
    pub fn head(&mut self, root: &Path, file: &str, line: u32, name: &str) -> Cite {
        let start = line.max(1) as usize;
        let n = match &*self.read(root, file) {
            Now::Lines(lines) => {
                let window: Vec<&String> = lines.iter().skip(start - 1).take(HEAD_LINES).collect();
                match window.iter().position(|l| names(l, name)) {
                    Some(i) => i + 1,
                    None if window.len() < HEAD_LINES => window.len().max(1),
                    None => HEAD_LINES,
                }
            }
            Now::Unread(_) => 1,
        };
        // Brief: the one line naming it (else its first).
        if self.mode == CiteMode::Brief {
            let named = matches!(&*self.read(root, file), Now::Lines(lines) if lines.get(start + n - 2).is_some_and(|l| names(l, name)));
            let at = if named { line.max(1) + n as u32 - 1 } else { line };
            return self.quote(root, file, at, 1, Vec::new());
        }
        let cut = match &*self.read(root, file) {
            Now::Lines(lines) if n == HEAD_LINES && !lines.iter().skip(start - 1).take(HEAD_LINES).any(|l| names(l, name)) => {
                vec![format!("the declaration goes on past line {}", start + HEAD_LINES - 1)]
            }
            _ => Vec::new(),
        };
        self.quote(root, file, line, n, cut)
    }

    /// Cites each node of a trace of the repo at `home`: a node reached by an edge, the edge's site; the root, its
    /// declaration.
    pub fn trace(&mut self, home: &Path, t: &mut Trace) {
        if !self.on() {
            return;
        }
        for n in &mut t.nodes {
            let Some(root) = self.root_of(home, n.repo.as_deref()) else { continue };
            n.cite = match &n.site {
                Some(site) => {
                    let in_repo = n.site_repo.as_deref().or(n.repo.as_deref());
                    let Some(at) = self.root_of(home, in_repo) else { continue };
                    self.site(&at, site).map(|mut c| {
                        if n.site_repo.is_some() && n.site_repo != n.repo {
                            c.repo.clone_from(&n.site_repo);
                        }
                        c
                    })
                }
                None => Some(self.head(&root, &n.file, n.line, simple(&n.qualified))),
            };
        }
    }

    /// Cites an impact of the repo at `home`: the changed symbols' declarations, each dependent's site (else its
    /// declaration).
    pub fn impact(&mut self, home: &Path, i: &mut Impact) {
        if !self.on() {
            return;
        }
        for s in &mut i.scope {
            s.cite = Some(self.head(home, &s.file, s.line, simple(&s.qualified)));
        }
        for a in &mut i.affected {
            let Some(root) = self.root_of(home, a.repo.as_deref()) else { continue };
            a.cite = match &a.site {
                Some(site) => self.site(&root, site),
                None => Some(self.head(&root, &a.file, a.line, simple(&a.qualified))),
            };
        }
    }

    /// Cites references in the repo at `home` (each in its own `repo` when it names one): the line of each.
    pub fn refs<'h>(&mut self, home: &Path, hits: impl Iterator<Item = &'h mut RefHit>) {
        if !self.on() {
            return;
        }
        for h in hits {
            let Some(root) = self.root_of(home, h.repo.as_deref()) else { continue };
            h.cite = Some(self.quote(&root, &h.file, h.line, SITE_LINES, Vec::new()));
        }
    }

    /// Cites one step of a path or flow: its site, in `site_repo` (the step before's), else its declaration.
    pub fn step(&mut self, s: &mut Step, site_repo: &str) {
        if !self.on() {
            return;
        }
        let Some(root) = self.root_of(Path::new(""), Some(&s.repo)) else { return };
        s.cite = match &s.site {
            Some(site) => {
                let Some(at) = self.root_of(Path::new(""), Some(site_repo)) else { return };
                self.site(&at, site).map(|mut c| {
                    if site_repo != s.repo {
                        c.repo = Some(site_repo.to_string());
                    }
                    c
                })
            }
            None => Some(self.head(&root, &s.file, s.line, simple(&s.qualified))),
        };
    }
}

impl Citer {
    /// Cites a path search's paths, step by step (a step's site is in the step before's repo), and its ends.
    pub fn path(&mut self, a: &mut PathAnswer) {
        if !self.on() {
            return;
        }
        for p in &mut a.paths {
            let mut before = p.steps.first().map(|s| s.repo.clone()).unwrap_or_default();
            for s in &mut p.steps {
                self.step(s, &before);
                before.clone_from(&s.repo);
            }
        }
        for s in [&mut a.from, &mut a.to] {
            let repo = s.repo.clone();
            self.step(s, &repo);
        }
    }

    /// Cites a flow's start and each step (a step's site is in the repo of the step it is reached from).
    pub fn flow(&mut self, start: &mut Step, steps: &mut [FlowStep]) {
        if !self.on() {
            return;
        }
        let home = start.repo.clone();
        self.step(start, &home);
        let repos: Vec<String> = steps.iter().map(|s| s.step.repo.clone()).collect();
        for s in steps {
            let from = s.parent.and_then(|i| repos.get(i - 1)).unwrap_or(&home).clone();
            self.step(&mut s.step, &from);
        }
    }
}

/// The last part of a qualified name: what its declaration names.
fn simple(qualified: &str) -> &str {
    qualified.rsplit(['.', ':', '/', '#']).next().unwrap_or(qualified)
}

/// Whether `line` names `name` as a whole word, outside an attribute or decorator line.
fn names(line: &str, name: &str) -> bool {
    let t = line.trim_start();
    if name.is_empty() || t.starts_with('[') || t.starts_with('@') || t.starts_with("#[") {
        return false;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    line.match_indices(name).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + name.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citer(base: &Path) -> Citer {
        let mut c = Citer::new([base.to_path_buf()]);
        c.mode = CiteMode::Lines;
        c
    }

    #[test]
    fn a_declaration_is_quoted_through_the_line_naming_it_and_long_lines_say_they_were_cut() {
        let dir = std::env::temp_dir().join(format!("knowcode-cite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let long = format!("    var x = \"{}\";", "a".repeat(300));
        std::fs::write(
            dir.join("A.cs"),
            format!("public class A\n{{\n    [HttpPost(\"Orders\")]\n    public void Orders()\n    {{\n{long}\n    }}\n    [One]\n    [Two]\n    [Three]\n    public void Late() {{ }}\n}}\n"),
        )
        .unwrap();
        let mut c = citer(&dir);
        // An attribute naming the method doesn't stand for its declaration.
        let head = c.head(&dir, "A.cs", 3, "Orders");
        assert_eq!(head.at, "A.cs:3-4");
        assert_eq!(head.text.as_deref(), Some("[HttpPost(\"Orders\")]\npublic void Orders()"));
        assert_eq!(head.cut, None);
        // Past the cap, the cite says the declaration goes on.
        let late = c.head(&dir, "A.cs", 8, "Late");
        assert_eq!(late.at, "A.cs:8-10");
        assert_eq!(late.cut.as_deref(), Some("the declaration goes on past line 10"));
        let site = c.site(&dir, "A.cs:6").unwrap();
        assert_eq!(site.at, "A.cs:6");
        assert_eq!(site.text.as_ref().unwrap().chars().count(), LINE_CHARS + 1);
        assert_eq!(site.cut.as_deref(), Some(format!("line 6 cut at {LINE_CHARS} of {} characters", long.trim_start().chars().count()).as_str()));
        let past = c.site(&dir, "A.cs:99").unwrap();
        assert!(past.text.is_none() && past.cut.as_deref().is_some_and(|w| w.contains("past the end of the file (12 lines)")), "{past:?}");
        let gone = c.site(&dir, "B.cs:1").unwrap();
        assert_eq!(gone.cut.as_deref(), Some("not quoted: the file is not in the working tree"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The answers of every tool that lists locations, on the shop fixture (see `walk::tests::shop`).
    pub(crate) fn shop_answers(web: &Path, api: &Path) -> Vec<(&'static str, serde_json::Value, String)> {
        use crate::ops::{self, Scope};
        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };
        let filter = ops::WalkFilter::default();
        let trace = ops::trace("OrderService.place", "both", 3, 500, &filter, &scope(web)).unwrap();
        let trace_text = trace.data.found.iter().map(|(r, t)| crate::render::trace_text(r, t, false)).collect();
        let ip = ops::ImpactParams { symbol: Some("LoggerService.info".into()), depth: 5, limit: 500, ..Default::default() };
        let impact = ops::impact(&ip, &scope(web)).unwrap();
        let impact_text = crate::render::impact_text(&impact.data, false);
        let po = crate::walk::PathOptions {
            max_hops: 8,
            max_nodes: 5000,
            max_repos: 16,
            paths: 3,
            one_way: false,
            allow_ambiguous: false,
            edges: Default::default(),
            from_skip: 0,
            to_skip: 0,
        };
        let path = crate::walk::path("OrderPage.submit", "OrderPlacedConsumer.Consume", Some(api), &po, &scope(web)).unwrap();
        let path_text = crate::render::path_text(&path.data);
        let fo = crate::flow::FlowOptions { depth: 10, max_steps: 500, max_repos: 16, edges: Default::default(), children: crate::flow::DEFAULT_CHILDREN };
        let flow = crate::flow::flow("/orders/new/", &fo, &scope(web)).unwrap();
        let flow_text = crate::render::flow_text("/orders/new/", flow.data.as_ref());
        let refs = ops::refs("place", &[], 50, &scope(web)).unwrap();
        let context = ops::context("OrderService.place", 2000, kc_store::skeleton::SourceMode::Auto, &scope(web)).unwrap();
        vec![
            ("trace", trace.env, trace_text),
            ("impact", impact.env, impact_text),
            ("path", path.env, path_text),
            ("flow", flow.env, flow_text),
            ("refs", refs.env, String::new()),
            ("context", context.env, String::new()),
        ]
    }

    /// The rows of an answer that carry a location: trace nodes, impact rows, path and flow steps, references, a
    /// context's callers and callees.
    pub(crate) fn rows(env: &serde_json::Value) -> Vec<&serde_json::Value> {
        fn go<'v>(v: &'v serde_json::Value, key: &str, out: &mut Vec<&'v serde_json::Value>) {
            match v {
                serde_json::Value::Object(o) => {
                    for (k, x) in o {
                        let listed = matches!(k.as_str(), "nodes" | "affected" | "scope" | "steps" | "callers" | "callees" | "cross_repo");
                        match x {
                            serde_json::Value::Array(a) if listed => out.extend(a.iter().filter(|r| r.get("file").is_some())),
                            serde_json::Value::Object(_) if matches!(k.as_str(), "start" | "from" | "to") && x.get("qualified").is_some() => out.push(x),
                            _ => {}
                        }
                        go(x, k, out);
                    }
                }
                serde_json::Value::Array(a) => {
                    for x in a {
                        // A reference list's own items.
                        if key == "items" && x.get("name").is_some() && x.get("confidence").is_some() && x.get("line").is_some() {
                            out.push(x);
                        }
                        go(x, key, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        go(env, "", &mut out);
        out
    }

    #[test]
    fn every_row_of_every_walk_and_reference_list_quotes_the_lines_it_rests_on() {
        let cache = crate::ops::tests::test_cache();
        let base = cache.dir("cite-rows");
        let (web, api, schema) = crate::walk::tests::shop(&base, "ct");
        // Every row quoted to the full caps.
        kc_store::limits::current().set_cite(CiteMode::Lines);
        let root_named = |name: &str| [&web, &api, &schema].into_iter().find(|r| repo_name(r) == name).cloned();
        for (tool, env, text) in shop_answers(&web, &api) {
            let rows = rows(&env);
            assert!(!rows.is_empty(), "{tool}: {env:#}");
            for row in rows {
                let cite = &row["cite"];
                assert!(cite.is_object(), "{tool}: a row without a cite: {row:#}");
                let at = cite["at"].as_str().unwrap();
                // An edge's site, else the row's own place.
                let place = format!("{}:{}", row["file"].as_str().unwrap(), row["line"]);
                match row["site"].as_str() {
                    Some(site) => assert_eq!(at, site, "{tool}: {row:#}"),
                    None => assert!(at == place || at.starts_with(&format!("{place}-")), "{tool}: {at} for {place}"),
                }
                // The text is the file's lines, as the working tree has them.
                let repo = cite["repo"].as_str().or(row["site_repo"].as_str()).or(row["repo"].as_str()).unwrap_or("web");
                let root = root_named(repo).unwrap_or_else(|| web.to_path_buf());
                let (file, lines) = at.rsplit_once(':').unwrap();
                let first: usize = lines.split('-').next().unwrap().parse().unwrap();
                let source = std::fs::read_to_string(root.join(file)).unwrap();
                let quoted = cite["text"].as_str().unwrap_or_else(|| panic!("{tool}: no text: {row:#}"));
                assert_eq!(quoted.lines().next().unwrap().trim(), source.lines().nth(first - 1).unwrap().trim(), "{tool}: {row:#}");
                assert!(cite.get("cut").is_none(), "{tool}: {cite}");
            }
            // The text trees quote under each row.
            if !text.is_empty() {
                assert!(text.lines().any(|l| l.contains(" | ")), "{tool}: {text}");
            }
        }
        // `cite: none`: locations only.
        kc_store::limits::current().set_cite(CiteMode::None);
        for (tool, env, text) in shop_answers(&web, &api) {
            assert!(rows(&env).iter().all(|r| r.get("cite").is_none()), "{tool}: {env:#}");
            assert!(!text.lines().any(|l| l.contains(" | ")), "{tool}: {text}");
        }
        kc_store::limits::current().set_cite(CiteMode::default());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The default, `cite: brief`: the first rows quote one line each, shortened; the rest give their place only, and
    /// the answer says how many and how to get them all.
    #[test]
    fn brief_cites_quote_the_first_rows_one_line_each_and_say_the_rest_carry_none() {
        use crate::ops::{self, Scope};
        let cache = crate::ops::tests::test_cache();
        let base = cache.dir("cite-brief");
        let mut files: Vec<(String, String)> = vec![("src/Audit.cs".into(), "public static class Audit\n{\n    [Obsolete]\n    public static void Log() { }\n}\n".into())];
        let long = "x".repeat(200);
        for i in 0..20 {
            files.push((format!("src/C{i}.cs"), format!("public class C{i}\n{{\n    public void Run() {{ Audit.Log(); var s = \"{long}\"; }}\n}}\n")));
        }
        let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let root = crate::ops::tests::repo(&base, "app", &files);
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        assert_eq!(kc_store::limits::current().cite(), CiteMode::Brief);
        let r = ops::refs("Log", &[], 50, &scope).unwrap();
        let rows = r.env["items"].as_array().unwrap();
        assert_eq!(rows.len(), 20);
        let quoted: Vec<&serde_json::Value> = rows.iter().filter(|r| r["cite"].get("text").is_some()).collect();
        assert_eq!(quoted.len(), BRIEF_CITES, "{}", r.env);
        // The first rows, one line each, cut at the brief width; the rest, their place only.
        assert!(rows[..BRIEF_CITES].iter().all(|r| r["cite"]["text"].as_str().unwrap().chars().count() == BRIEF_CHARS + 1), "{}", r.env);
        assert!(rows[..BRIEF_CITES].iter().all(|r| r["cite"]["cut"].as_str().unwrap().contains(&format!("cut at {BRIEF_CHARS} of"))));
        assert!(rows[BRIEF_CITES..].iter().all(|r| r["cite"]["at"].is_string() && r["cite"].get("text").is_none() && r["cite"].get("cut").is_none()));
        let entry = crate::contract::incomplete_of(&r.env).into_iter().find(|i| i.detail.starts_with("cite text:")).expect("said");
        assert_eq!(entry.reason, kc_store::answer::Reason::Capped);
        assert_eq!(entry.detail, format!("cite text: {BRIEF_CITES} of 20 cites quote their line; the other 8 give only where it is"));
        // A declaration quotes the one line naming it, not its attributes.
        let ip = ops::ImpactParams { symbol: Some("Audit.Log".into()), depth: 1, limit: 50, ..Default::default() };
        let i = ops::impact(&ip, &scope).unwrap();
        let own = &i.env["items"][0]["scope"][0]["cite"];
        assert_eq!((own["at"].as_str(), own["text"].as_str()), (Some("src/Audit.cs:4"), Some("public static void Log() { }")), "{own}");
        // `cite: lines`: every row, nothing said.
        kc_store::limits::current().set_cite(CiteMode::Lines);
        let r = ops::refs("Log", &[], 50, &scope).unwrap();
        kc_store::limits::current().set_cite(CiteMode::default());
        assert!(r.env["items"].as_array().unwrap().iter().all(|r| r["cite"]["text"].is_string()));
        assert!(crate::contract::incomplete_of(&r.env).iter().all(|i| !i.detail.starts_with("cite text:")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn names_are_whole_words() {
        assert!(names("public void Orders()", "Orders"));
        assert!(!names("public void Orderss()", "Orders"));
        assert!(!names("@Component({ selector: 'OrderPage' })", "OrderPage"));
        assert_eq!(simple("Shop.OrdersController.Orders"), "Orders");
    }
}
