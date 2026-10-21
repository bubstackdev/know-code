//! Cited source: the lines each row of an answer rests on, quoted from the working tree when the answer is built and
//! checked against the index the answer read (see [`kc_store::answer::Cite`] and docs/answers.md#cited-source).
//!
//! A row reached by an edge cites the edge's site (the call, the request, the publish): [`SITE_LINES`] line. A row
//! without one (a walk's start, a path's ends) cites its declaration: from its first line through the line naming
//! it, at most [`HEAD_LINES`] lines. Each line is cut at [`LINE_CHARS`] characters. Whatever a cap leaves out, the
//! cite says in `cut`.
//!
//! The check: a file whose bytes hash to what the index holds for it is `verified`. Otherwise the cite looks for what
//! the row names (the symbol a declaration declares, the name a site references) on the cited line: still there is
//! `changed`, on another line `moved` (quoted there), nowhere `gone`; a gone file is `gone`. Any drift makes the answer
//! incomplete with reason `stale` ([`Citer::drift`]).
//!
//! How much rows quote is the query's (see [`kc_store::limits::Operation::cite`]). `brief`, the default, quotes the
//! first [`BRIEF_CITES`] cites one line each, up to [`BRIEF_CHARS`] characters, and gives the rest their place and
//! check only, saying so in the answer's `incomplete` ([`Citer::incomplete`]); `lines` quotes every row to the caps
//! above; `check` gives the check without the text; `none` leaves `cite` out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use kc_store::Store;
use kc_store::answer::{Check, Cite, CiteMode, Incomplete, Reason};
use kc_store::query::RefHit;
use kc_store::trace::{Impact, Trace};

use crate::flow::FlowStep;
use crate::ops::repo_name;
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
/// Drifted cites named in the answer's `stale` entry; the rest are counted.
const DRIFT_NAMED: usize = 5;

/// A file as the working tree has it now, read once per answer.
enum Now {
    /// Its lines, and the hash of its bytes (as the index hashes a file).
    Lines(Vec<String>, String),
    /// Not in the working tree.
    Missing,
    /// Not read: why.
    Unread(String),
}

/// The edges whose site names the symbol at its other end: a site of another kind (a request, a publish, a mapping)
/// names something else, and is checked by its file alone.
const NAMED_EDGES: &[&str] = &["calls", "instantiates", "uses", "reads", "writes", "via_di", "via_protocol", "conforms_to", "constructs"];

/// What a site of an `edge` names, the symbol at its far end being `qualified` (of `kind`, when known).
fn anchor<'q>(edge: &str, qualified: &'q str, kind: Option<&str>) -> Option<&'q str> {
    NAMED_EDGES.contains(&edge).then(|| declared(qualified, kind))
}

/// Quotes the lines an answer's rows rest on, reading each file once, and checks them against the index.
pub struct Citer<'s> {
    mode: CiteMode,
    /// The repos rows may name (`repo`), by their short name.
    roots: Vec<PathBuf>,
    /// Every indexed repo, read when a row names a repo `roots` doesn't hold (a dependent's reference).
    indexed: Option<Vec<PathBuf>>,
    /// The indexes the answer read, by repo.
    stores: Vec<(PathBuf, &'s Store)>,
    /// Indexes opened here for repos the answer read without keeping them open (a dependent's references).
    owned: HashMap<PathBuf, Option<Store>>,
    files: HashMap<PathBuf, Rc<Now>>,
    /// The hash the index holds for each file, by its path.
    hashes: HashMap<PathBuf, Option<String>>,
    privacy: HashMap<PathBuf, kc_index::privacy::Privacy>,
    /// Cites that drifted, `file:line (check)`, and how many were checked.
    drifted: Vec<String>,
    checked: usize,
    /// Cites that quoted a line, and those `cite: brief` left without one past [`BRIEF_CITES`].
    quoted: usize,
    bare: usize,
    /// The places quoted so far: quoting one again doesn't count against [`BRIEF_CITES`].
    spans: std::collections::HashSet<String>,
}

impl<'s> Citer<'s> {
    /// A citer for the query this thread runs, whose rows may be in the repos at `roots`.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Citer<'s> {
        Citer {
            mode: kc_store::limits::current().cite(),
            roots: roots.into_iter().collect(),
            indexed: None,
            stores: Vec::new(),
            owned: HashMap::new(),
            files: HashMap::new(),
            hashes: HashMap::new(),
            privacy: HashMap::new(),
            drifted: Vec::new(),
            checked: 0,
            quoted: 0,
            bare: 0,
            spans: Default::default(),
        }
    }

    /// Checks against these indexes (the ones the answer read), by repo; a repo without one is checked against its
    /// index as it is now.
    pub fn with_stores(mut self, stores: impl IntoIterator<Item = (PathBuf, &'s Store)>) -> Citer<'s> {
        for (root, store) in stores {
            if !self.roots.contains(&root) {
                self.roots.push(root.clone());
            }
            self.stores.push((root, store));
        }
        self
    }

    /// Whether the query's rows cite their lines.
    pub fn on(&self) -> bool {
        self.mode != CiteMode::None
    }

    /// What the answer's `incomplete` says about its cites: under `cite: brief`, the ones past [`BRIEF_CITES`] that
    /// quote nothing (`capped`); any that drifted from the index (`stale`).
    pub fn incomplete(&self) -> Vec<Incomplete> {
        let mut out = Vec::new();
        if self.bare > 0 {
            let total = self.quoted + self.bare;
            out.push(
                Incomplete::new(
                    Reason::Capped,
                    format!("cite text: {} of {total} cites quote their line; the other {} give only where it is", self.quoted, self.bare),
                )
                .with_hint("cite: lines (--cite lines) quotes every row"),
            );
        }
        out.extend(self.drift());
        out
    }

    /// The `stale` entry for the cites that drifted from the index, if any did.
    fn drift(&self) -> Option<Incomplete> {
        let n = self.drifted.len();
        if n == 0 {
            return None;
        }
        let named: Vec<&str> = self.drifted.iter().take(DRIFT_NAMED).map(String::as_str).collect();
        let more = if n > DRIFT_NAMED { format!(" and {} more", n - DRIFT_NAMED) } else { String::new() };
        Some(
            Incomplete::new(
                Reason::Stale,
                format!("cited lines: {n} of {} differ from the index the answer was built from: {}{more}", self.checked, named.join(", ")),
            )
            .with_hint("each cite quotes the working tree; ask again once the files are indexed (without --no-sync, or after indexing finishes)"),
        )
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
                    Ok(bytes) => Now::Lines(
                        kc_extract::decode(&bytes).lines().map(|l| l.trim_end().to_string()).collect(),
                        kc_store::content_hash(&bytes),
                    ),
                    Err(e) => Now::Unread(format!("not quoted: the file could not be read ({e})")),
                },
                Err(_) => Now::Missing,
            }
        };
        let now = Rc::new(now);
        self.files.insert(path, now.clone());
        now
    }

    /// The hash the index the answer read holds for `file` of the repo at `root`; `None` when it has none.
    fn indexed(&mut self, root: &Path, file: &str) -> Option<String> {
        let path = root.join(file);
        if let Some(h) = self.hashes.get(&path) {
            return h.clone();
        }
        let store = match self.stores.iter().find(|(r, _)| r == root) {
            Some((_, s)) => Some(*s),
            // An index that exists only: none is ever made here.
            None => self
                .owned
                .entry(root.to_path_buf())
                .or_insert_with(|| kc_index::db_path(root).ok().filter(|db| db.exists()).and_then(|_| kc_index::open(root).ok()))
                .as_ref(),
        };
        let hash = store.and_then(|s| {
            use rusqlite::OptionalExtension;
            s.conn().prepare_cached("SELECT hash FROM files WHERE path = ?1").ok()?.query_row([file], |r| r.get::<_, String>(0)).optional().ok()?
        });
        self.hashes.insert(path, hash.clone());
        hash
    }

    /// The cite of lines `start..start+n` of `file` in the repo at `root`, `cut` saying what the caller's cap left out,
    /// checked against the index: `name` is what the lines name (the symbol declared, the name referenced), when known.
    fn quote(&mut self, root: &Path, file: &str, start: u32, n: usize, cut: Vec<String>, name: Option<&str>) -> Cite {
        let start = start.max(1);
        let width = if self.mode == CiteMode::Brief { BRIEF_CHARS } else { LINE_CHARS };
        let now = self.read(root, file);
        let mut c = match &*now {
            Now::Missing => unquoted(file, start, n, Check::Gone, "the file is gone from the working tree"),
            Now::Unread(why) => {
                let mut c = unquoted(file, start, n, Check::Unchecked, why.trim_start_matches("not quoted: "));
                c.cut = Some(why.clone());
                c
            }
            Now::Lines(lines, hash) => match self.indexed(root, file) {
                None => {
                    let mut c = text(file, lines, start, n, cut, width);
                    c.check = Check::Unchecked;
                    c.drift = Some("the file isn't in the index this answer read".into());
                    c
                }
                Some(h) if h == *hash => text(file, lines, start, n, cut, width),
                Some(_) => drifted(file, lines, start, n, cut, name, width),
            },
        };
        self.checked += 1;
        if c.check.drifted() {
            let place = c.was.clone().unwrap_or_else(|| format!("{file}:{start}"));
            self.drifted.push(format!("{place} ({})", c.check.as_str()));
        }
        if self.mode == CiteMode::Check {
            c.text = None;
            c.cut = None;
        }
        // Brief: past the cap, the place and check only; a place quoted already is quoted again for free.
        if self.mode == CiteMode::Brief && c.text.is_some() {
            let key = format!("{}|{}", root.display(), c.at);
            if !self.spans.contains(&key) {
                if self.quoted >= BRIEF_CITES {
                    c.text = None;
                    c.cut = None;
                    self.bare += 1;
                } else {
                    self.spans.insert(key);
                    self.quoted += 1;
                }
            }
        }
        c
    }

    /// The cite of an edge's site (`file:line`) in the repo at `root`, naming `name` when known.
    pub fn site(&mut self, root: &Path, site: &str, name: Option<&str>) -> Option<Cite> {
        let (file, line) = site.rsplit_once(':')?;
        let line: u32 = line.parse().ok()?;
        Some(self.quote(root, file, line, SITE_LINES, Vec::new(), name))
    }

    /// The cite of a declaration of `name` starting at `line` of `file` in the repo at `root`: through the line naming
    /// it, at most [`HEAD_LINES`] lines.
    pub fn head(&mut self, root: &Path, file: &str, line: u32, name: &str) -> Cite {
        let start = line.max(1) as usize;
        let (n, cut) = match &*self.read(root, file) {
            Now::Lines(lines, _) => {
                let window: Vec<&String> = lines.iter().skip(start - 1).take(HEAD_LINES).collect();
                match window.iter().position(|l| names(l, name)) {
                    // Only attributes, decorators, comments or blank lines come before the name: the declaration's.
                    Some(i) if window[..i].iter().all(|l| preamble(l)) => (i + 1, Vec::new()),
                    // Other code first (a file edited since it was indexed): the declaration starts further down.
                    Some(_) => (1, Vec::new()),
                    None if window.len() < HEAD_LINES => (window.len().max(1), Vec::new()),
                    None => (HEAD_LINES, vec![format!("the declaration goes on past line {}", start + HEAD_LINES - 1)]),
                }
            }
            _ => (1, Vec::new()),
        };
        // Brief: the one line naming it (else its first).
        if self.mode == CiteMode::Brief {
            let named = matches!(&*self.read(root, file), Now::Lines(lines, _) if lines.get(start + n - 2).is_some_and(|l| names(l, name)));
            let at = if named { line.max(1) + n as u32 - 1 } else { line };
            return self.quote(root, file, at, 1, Vec::new(), Some(name));
        }
        self.quote(root, file, line, n, cut, Some(name))
    }

    /// The check of the whole of symbol `s` of the repo at `root`, without its text (a context's `source` quotes it),
    /// counting against no quote cap.
    pub fn span(&mut self, root: &Path, s: &kc_store::query::SymbolRow) -> Cite {
        let n = (s.end_line.max(s.line) - s.line + 1) as usize;
        let mode = std::mem::replace(&mut self.mode, CiteMode::Check);
        let c = self.quote(root, &s.file, s.line, n, Vec::new(), Some(declared(&s.qualified(), Some(&s.kind))));
        self.mode = mode;
        c
    }

    /// Cites each node of a trace of the repo at `home`: a node reached by an edge, the edge's site; the root, its
    /// declaration.
    pub fn trace(&mut self, home: &Path, t: &mut Trace) {
        if !self.on() {
            return;
        }
        for i in 0..t.nodes.len() {
            let n = &t.nodes[i];
            let Some(root) = self.root_of(home, n.repo.as_deref()) else { continue };
            let cite = match &n.site {
                Some(site) => {
                    // Out, the site names the node; in, the node it hangs off.
                    let named = match (n.direction, n.parent) {
                        ("in", Some(p)) => anchor(n.edge, &t.nodes[p].qualified, Some(&t.nodes[p].kind)),
                        _ => anchor(n.edge, &n.qualified, Some(&n.kind)),
                    }
                    .map(str::to_string);
                    let in_repo = n.site_repo.clone().or_else(|| n.repo.clone());
                    let Some(at) = self.root_of(home, in_repo.as_deref()) else { continue };
                    let (site, other, site_repo) = (site.clone(), n.site_repo.is_some() && n.site_repo != n.repo, n.site_repo.clone());
                    self.site(&at, &site, named.as_deref()).map(|mut c| {
                        if other {
                            c.repo = site_repo;
                        }
                        c
                    })
                }
                None => {
                    let (file, line, name) = (n.file.clone(), n.line, declared(&n.qualified, Some(&n.kind)).to_string());
                    Some(self.head(&root, &file, line, &name))
                }
            };
            t.nodes[i].cite = cite;
        }
    }

    /// Cites an impact of the repo at `home`: the changed symbols' declarations, each dependent's site (else its
    /// declaration).
    pub fn impact(&mut self, home: &Path, i: &mut Impact) {
        if !self.on() {
            return;
        }
        for s in &mut i.scope {
            s.cite = Some(self.head(home, &s.file, s.line, declared(&s.qualified, Some(&s.kind))));
        }
        for a in &mut i.affected {
            let Some(root) = self.root_of(home, a.repo.as_deref()) else { continue };
            a.cite = match &a.site {
                // The site, in the dependent, names what it depends on.
                Some(site) => self.site(&root, site, anchor(a.edge, &a.via, None)),
                None => Some(self.head(&root, &a.file, a.line, declared(&a.qualified, Some(&a.kind)))),
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
            h.cite = Some(self.quote(&root, &h.file, h.line, SITE_LINES, Vec::new(), Some(&h.name)));
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
                // A dispatch names the action; a call, the step.
                let named = match &s.ngrx {
                    Some(action) => Some(action.as_str()),
                    None => anchor(s.edge, &s.qualified, Some(&s.kind)),
                };
                self.site(&at, site, named).map(|mut c| {
                    if site_repo != s.repo {
                        c.repo = Some(site_repo.to_string());
                    }
                    c
                })
            }
            None => Some(self.head(&root, &s.file, s.line, declared(&s.qualified, Some(&s.kind)))),
        };
    }
}

/// A cite with nothing quoted.
fn unquoted(file: &str, start: u32, n: usize, check: Check, why: &str) -> Cite {
    Cite { at: span(file, start, n), check, drift: Some(why.to_string()), was: None, repo: None, text: None, cut: None }
}

/// The cite of a file that changed since it was indexed: still naming `name` on the cited lines (`changed`), on
/// another line (`moved`, quoted there), or nowhere (`gone`).
fn drifted(file: &str, lines: &[String], start: u32, n: usize, cut: Vec<String>, name: Option<&str>, width: usize) -> Cite {
    let changed = "the file changed since it was indexed";
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        let mut c = text(file, lines, start, n, cut, width);
        if c.text.is_some() {
            c.check = Check::Changed;
            c.drift = Some(changed.into());
        } else {
            c.check = Check::Gone;
            c.drift = Some(format!("{changed}: line {start} is past its end ({} lines)", lines.len()));
        }
        return c;
    };
    if lines.iter().skip(start as usize - 1).take(n).any(|l| mentions(l, name)) {
        let mut c = text(file, lines, start, n, cut, width);
        c.check = Check::Changed;
        c.drift = Some(format!("{changed}; line {start} still names {name}"));
        return c;
    }
    // The nearest line naming it now.
    let nearest = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| mentions(l, name))
        .min_by_key(|(i, _)| (*i as i64 - (start as i64 - 1)).unsigned_abs())
        .map(|(i, _)| i as u32 + 1);
    let mut c = text(file, lines, nearest.unwrap_or(start), if nearest.is_some() { 1 } else { n }, cut, width);
    match nearest {
        Some(now) => {
            c.check = Check::Moved;
            c.drift = Some(format!("{changed}; {name} is at line {now} now, not line {start}"));
            c.was = Some(format!("{file}:{start}"));
        }
        None => {
            c.check = Check::Gone;
            c.drift = Some(format!("{changed} and no longer names {name}"));
        }
    }
    c
}

/// A verified cite quoting lines `start..start+n` of `lines`.
fn text(file: &str, lines: &[String], start: u32, n: usize, mut cut: Vec<String>, width: usize) -> Cite {
    let quoted: Vec<&String> = lines.iter().skip(start as usize - 1).take(n).collect();
    let mut c = Cite { at: span(file, start, n), check: Check::Verified, drift: None, was: None, repo: None, text: None, cut: None };
    if quoted.is_empty() {
        c.cut = Some(format!("not quoted: line {start} is past the end of the file ({} lines)", lines.len()));
        return c;
    }
    let indent = quoted.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(quoted.len());
    for (i, l) in quoted.iter().enumerate() {
        let l = l.get(indent..).unwrap_or(l.trim_start());
        let chars = l.chars().count();
        if chars > width {
            let at = l.char_indices().nth(width).map_or(l.len(), |(i, _)| i);
            out.push(format!("{}…", &l[..at]));
            cut.push(format!("line {} cut at {width} of {chars} characters", start as usize + i));
        } else {
            out.push(l.to_string());
        }
    }
    c.at = span(file, start, quoted.len());
    c.text = Some(out.join("\n"));
    c.cut = (!cut.is_empty()).then(|| cut.join("; "));
    c
}

/// `file:start` or `file:start-end` for `n` lines.
fn span(file: &str, start: u32, n: usize) -> String {
    if n <= 1 { format!("{file}:{start}") } else { format!("{file}:{start}-{}", start as usize + n - 1) }
}

/// The name a declaration of `qualified` writes: its last part, or for a constructor its type's.
fn declared<'q>(qualified: &'q str, kind: Option<&str>) -> &'q str {
    let last = simple(qualified);
    let ctor = kind == Some("init") || matches!(last, "init" | "ctor" | "constructor" | "__init__");
    if ctor
        && let Some(ty) = qualified.strip_suffix(last).map(|q| q.trim_end_matches(['.', ':', '#'])).filter(|t| !t.is_empty())
    {
        return simple(ty);
    }
    last
}

/// Whether `line` can come before a declaration's name as part of it: an attribute, a decorator, a comment, blank.
fn preamble(line: &str) -> bool {
    let t = line.trim_start();
    t.is_empty() || ["[", "@", "#[", "//", "/*", "*", "#"].iter().any(|p| t.starts_with(p))
}

/// Whether `line` names `name` as a whole word, anywhere on it.
fn mentions(line: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    !name.is_empty()
        && line.match_indices(name).any(|(i, _)| {
            let before = line[..i].chars().next_back();
            let after = line[i + name.len()..].chars().next();
            !before.is_some_and(word) && !after.is_some_and(word)
        })
}

impl Citer<'_> {
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

    fn citer(base: &Path) -> Citer<'static> {
        let mut c = Citer::new([base.to_path_buf()]);
        c.mode = CiteMode::Lines;
        c
    }

    #[test]
    fn a_declaration_is_quoted_through_the_line_naming_it_and_long_lines_say_they_were_cut() {
        let cache = crate::ops::tests::test_cache();
        let dir = cache.dir("cite-caps").to_path_buf();
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
        // Not indexed: quoted, unchecked.
        assert_eq!(head.check, Check::Unchecked);
        assert_eq!(head.drift.as_deref(), Some("the file isn't in the index this answer read"));
        let site = c.site(&dir, "A.cs:6", None).unwrap();
        assert_eq!(site.at, "A.cs:6");
        assert_eq!(site.text.as_ref().unwrap().chars().count(), LINE_CHARS + 1);
        assert_eq!(site.cut.as_deref(), Some(format!("line 6 cut at {LINE_CHARS} of {} characters", long.trim_start().chars().count()).as_str()));
        let past = c.site(&dir, "A.cs:99", None).unwrap();
        assert!(past.text.is_none() && past.cut.as_deref().is_some_and(|w| w.contains("past the end of the file (12 lines)")), "{past:?}");
        let gone = c.site(&dir, "B.cs:1", None).unwrap();
        assert!(gone.text.is_none() && gone.check == Check::Gone, "{gone:?}");
        assert_eq!(gone.drift.as_deref(), Some("the file is gone from the working tree"));
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
                // Answered right after a sync: every cite matches the index.
                assert_eq!(cite["check"], "verified", "{tool}: {cite}");
                assert!(cite.get("drift").is_none(), "{tool}: {cite}");
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

    /// Answered from an index the working tree has moved past (`--no-sync`), every cite says how its lines drifted,
    /// and the answer is `stale`: a line moved, a line still there in a changed file, a file gone.
    #[test]
    fn cites_say_how_the_working_tree_drifted_from_the_index() {
        use crate::ops::{self, Scope};
        let cache = crate::ops::tests::test_cache();
        let base = cache.dir("cite-drift");
        let (web, _api, _schema) = crate::walk::tests::shop(&base, "cd");
        // Two lines inserted above the calls, which swap; the logger's file deleted.
        let service = "export class OrderService {\n  // one\n  // two\n  constructor(private http: HttpService, private log: LoggerService) {}\n  place(): void {\n    this.http.send(new ApiPlaceOrder());\n    this.log.info('placing');\n  }\n}\n";
        std::fs::write(web.join("src/order.service.ts"), service).unwrap();
        std::fs::remove_file(web.join("src/logger.service.ts")).unwrap();
        let stale = Scope { repos: vec![web.to_path_buf()], no_sync: true };
        let t = ops::trace("OrderService.place", "out", 1, 50, &ops::WalkFilter::default(), &stale).unwrap();
        let nodes = t.env["items"][0]["nodes"].as_array().unwrap().clone();
        let cite = |q: &str| nodes.iter().find(|n| n["qualified"] == q).map(|n| n["cite"].clone()).unwrap();
        // The method's declaration: two lines further down now.
        let root = cite("OrderService.place");
        assert_eq!(
            (root["check"].as_str(), root["at"].as_str(), root["was"].as_str()),
            (Some("moved"), Some("src/order.service.ts:5"), Some("src/order.service.ts:3")),
            "{root}"
        );
        // The call to the logger: on line 7 now, quoted there.
        let info = cite("LoggerService.info");
        assert_eq!((info["check"].as_str(), info["at"].as_str()), (Some("moved"), Some("src/order.service.ts:7")), "{info}");
        assert_eq!(info["text"], "this.log.info('placing');");
        // The request: line 5 is the declaration now; the request is on line 6.
        let request = cite("ApiPlaceOrder");
        assert_eq!((request["check"].as_str(), request["at"].as_str()), (Some("moved"), Some("src/order.service.ts:6")), "{request}");
        let stale_entry = crate::contract::incomplete_of(&t.env).into_iter().find(|i| i.reason == kc_store::answer::Reason::Stale).expect("stale");
        assert!(stale_entry.detail.starts_with("cited lines: 3 of 3 differ from the index"), "{}", stale_entry.detail);
        assert_eq!(t.env["complete"], false);
        // The logger's declaration: its file is gone.
        let i = ops::impact(&ops::ImpactParams { symbol: Some("LoggerService.info".into()), depth: 1, limit: 50, ..Default::default() }, &stale).unwrap();
        let scope = &i.env["items"][0]["scope"][0]["cite"];
        assert_eq!((scope["check"].as_str(), scope["drift"].as_str()), (Some("gone"), Some("the file is gone from the working tree")), "{scope}");
        assert!(scope.get("text").is_none());
        // A line still naming what it cites, in a changed file: `changed`.
        let edited = "export class OrderService {\n  constructor(private http: HttpService, private log: LoggerService) {}\n  place(n = 1): void {\n    this.log.info('placing');\n    this.http.send(new ApiPlaceOrder());\n  }\n}\n";
        std::fs::write(web.join("src/order.service.ts"), edited).unwrap();
        let r = ops::refs("send", &[], 10, &stale).unwrap();
        let send = &r.env["items"][0]["cite"];
        assert_eq!((send["check"].as_str(), send["at"].as_str()), (Some("changed"), Some("src/order.service.ts:5")), "{send}");
        // `cite: check`: the check, without the lines.
        kc_store::limits::current().set_cite(CiteMode::Check);
        let r = ops::refs("send", &[], 10, &stale).unwrap();
        kc_store::limits::current().set_cite(CiteMode::default());
        let send = &r.env["items"][0]["cite"];
        assert_eq!(send["check"], "changed", "{send}");
        assert!(send.get("text").is_none(), "{send}");
        // Synced again: verified.
        let fresh = Scope { repos: vec![web.to_path_buf()], no_sync: false };
        let r = ops::refs("send", &[], 10, &fresh).unwrap();
        assert_eq!(r.env["items"][0]["cite"]["check"], "verified", "{}", r.env);
        assert!(crate::contract::incomplete_of(&r.env).iter().all(|i| i.reason != kc_store::answer::Reason::Stale), "{}", r.env);
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
