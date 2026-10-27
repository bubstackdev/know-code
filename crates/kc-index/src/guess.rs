//! Structural guesses at the files that change with a file, for when co-change history has nothing to rank
//! (`coupled` on a new file, a dormant one, or `--as-of` a point before its history): the files structure relates
//! to it. Each guess is labelled as one, never as history: `basis` `structure`, `confidence` `low`, and per
//! relation the evidence it rests on (a [`Mechanism`] from the answer contract):
//!
//! - `uses`: the file references a name only that file declares (`name_unique`);
//! - `used_by`: that file references a name only this file declares (`name_unique`);
//! - `implements` / `implemented_by`: a type of one names a type the other declares as its supertype
//!   (`declaration`);
//! - `test_named` / `subject_named`: a test file named for the file, or the file a test is named for
//!   (`filename_guess`; see [`crate::test_select::tested_subject`]);
//! - `same_directory`: in the file's directory (`convention`, assuming `directory_convention`); alone, only in a
//!   directory of at most [`MAX_DIR_FILES`] files.
//!
//! Names shorter than three characters, or declared in more than one other file, link nothing.
//!
//! As of a commit (`--as-of`), the structure is the commit's, never later: candidates are the files of the tree
//! then; a file unchanged since is read from the index (its content is the commit's), and a changed one (the file
//! itself included) is read from the commit by the extractor, at most [`MAX_PIN_FILES`] of them (the rest are
//! counted in [`Guesses::not_read`] and link nothing; one that doesn't mention a name the file declares or uses then
//! can link nothing, so it isn't parsed). A link a later commit made is never seen.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use kc_store::Store;
use kc_store::answer::{Assumption, Evidence, Mechanism};
use rusqlite::Connection;
use serde::Serialize;

use crate::git;
use crate::test_select::{subject, tested_subject};

/// The most files of a directory for `same_directory` alone to make a guess.
pub const MAX_DIR_FILES: usize = 12;
/// The most files changed since an `--as-of` commit read at that commit.
pub const MAX_PIN_FILES: usize = 400;
/// The largest file read at a commit, in bytes.
const MAX_PIN_BYTES: usize = 1 << 20;
/// Names a relation lists at most.
const MAX_NAMES: usize = 3;

/// One way a guessed file relates to the file.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Relation {
    /// `uses`, `used_by`, `implements`, `implemented_by`, `test_named`, `subject_named` or `same_directory`.
    pub how: &'static str,
    /// What the relation rests on.
    pub evidence: Evidence,
    /// The names linking the two (at most [`MAX_NAMES`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

/// A file that may change with the file, by structure: a guess, not history.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Guess {
    pub file: String,
    /// Always `structure`: never co-change history.
    pub basis: &'static str,
    /// Always `low`.
    pub confidence: &'static str,
    pub relations: Vec<Relation>,
    /// A test file.
    pub test: bool,
    /// How strongly structure relates the two, for ranking.
    #[serde(skip)]
    pub score: u32,
}

/// The guesses for one file.
#[derive(Debug, Clone, Default)]
pub struct Guesses {
    /// Ranked, strongest first, at most the limit asked.
    pub list: Vec<Guess>,
    /// Guesses before the limit.
    pub total: usize,
    /// Files changed since the `--as-of` commit not read at it (past [`MAX_PIN_FILES`], too large, or unreadable):
    /// links through them are not known.
    pub not_read: usize,
}

/// What a file declares and uses: its symbols' names, the names it references, its types' supertypes.
#[derive(Debug, Default, Clone)]
struct Facts {
    defs: BTreeSet<String>,
    refs: BTreeSet<String>,
    supers: BTreeSet<String>,
}

/// Symbol kinds whose names don't link files: locals, enum cases, extensions (named for the type they extend).
const NO_LINK_KINDS: &[&str] = &["variable", "case", "extension", "task", "handler", "play"];

/// The repo as structure saw it: the index, or the index and the files read at a commit.
struct View<'a> {
    conn: &'a Connection,
    /// At a commit: the files of its tree; those changed since (read from the commit into `read`); what was read.
    pin: Option<Pin>,
}

struct Pin {
    tree: HashSet<String>,
    changed: HashSet<String>,
    read: HashMap<String, Facts>,
}

impl View<'_> {
    /// Whether the index's rows for `path` hold for the view.
    fn indexed_holds(&self, path: &str) -> bool {
        self.pin.as_ref().is_none_or(|p| p.tree.contains(path) && !p.changed.contains(path))
    }

    /// Files of the view whose `column` matches `name`, from the index (`sql` selects paths) and the files read.
    fn files_with(&self, sql: &str, name: &str, pick: impl Fn(&Facts) -> &BTreeSet<String>) -> Result<BTreeSet<String>> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        let mut stmt = self.conn.prepare_cached(sql)?;
        for p in stmt.query_map([name], |r| r.get::<_, String>(0))? {
            let p = p?;
            if self.indexed_holds(&p) {
                out.insert(p);
            }
        }
        if let Some(pin) = &self.pin {
            for (p, f) in &pin.read {
                if pick(f).iter().any(|n| n.eq_ignore_ascii_case(name)) {
                    out.insert(p.clone());
                }
            }
        }
        Ok(out)
    }

    fn definers(&self, name: &str) -> Result<BTreeSet<String>> {
        let kinds = NO_LINK_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ");
        let sql = format!("SELECT DISTINCT f.path FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.name = ?1 AND s.kind NOT IN ({kinds})");
        self.files_with(&sql, name, |f| &f.defs)
    }

    fn referrers(&self, name: &str) -> Result<BTreeSet<String>> {
        self.files_with("SELECT DISTINCT f.path FROM refs r JOIN files f ON f.id = r.file_id WHERE r.name = ?1", name, |f| &f.refs)
    }

    fn subtypes(&self, name: &str) -> Result<BTreeSet<String>> {
        self.files_with("SELECT DISTINCT f.path FROM relations r JOIN files f ON f.id = r.file_id WHERE r.target_name = ?1", name, |f| &f.supers)
    }

    /// The facts of `path` in the view.
    fn facts(&self, path: &str) -> Result<Facts> {
        if let Some(f) = self.pin.as_ref().and_then(|p| p.read.get(path)) {
            return Ok(f.clone());
        }
        if !self.indexed_holds(path) {
            return Ok(Facts::default());
        }
        indexed_facts(self.conn, path)
    }

    /// Every file of the view that may be a guess: an indexed file, or at a commit, a file of its tree the index
    /// would read.
    fn files(&self) -> Result<Vec<String>> {
        match &self.pin {
            Some(p) => {
                let mut all: Vec<String> = p.tree.iter().filter(|f| kc_extract::Lang::from_path(f).is_some_and(|l| !l.is_manifest())).cloned().collect();
                all.sort();
                Ok(all)
            }
            None => {
                let mut stmt = self.conn.prepare_cached("SELECT path FROM files ORDER BY path")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            }
        }
    }
}

/// What the index holds of `path`.
fn indexed_facts(conn: &Connection, path: &str) -> Result<Facts> {
    let col = |sql: &str| -> Result<BTreeSet<String>> {
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = stmt.query_map([path], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    };
    let kinds = NO_LINK_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ");
    Ok(Facts {
        defs: col(&format!("SELECT DISTINCT s.name FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.path = ?1 AND s.kind NOT IN ({kinds})"))?,
        refs: col("SELECT DISTINCT r.name FROM refs r JOIN files f ON f.id = r.file_id WHERE f.path = ?1")?,
        supers: col("SELECT DISTINCT r.target_name FROM relations r JOIN files f ON f.id = r.file_id WHERE f.path = ?1")?,
    })
}

/// Whether `text` holds any of `names` (lower case) as a whole identifier.
fn mentions(text: &str, names: &HashSet<String>) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).any(|w| !w.is_empty() && names.contains(&w.to_lowercase()))
}

/// What a file read at a commit declares and uses.
fn facts_of(path: &str, text: &str) -> Option<Facts> {
    let lang = kc_extract::Lang::from_path(path).filter(|l| !l.is_manifest())?;
    let f = kc_extract::extract_file(lang, path, text);
    Some(Facts {
        defs: f.symbols.iter().filter(|s| !NO_LINK_KINDS.contains(&s.kind.as_str())).map(|s| s.name.clone()).collect(),
        refs: f.refs.iter().map(|r| r.name.clone()).collect(),
        supers: f.relations.iter().map(|r| r.target.rsplit('.').next().unwrap_or(&r.target).to_string()).collect(),
    })
}

/// The tree of commit `sha`, the files changed between it and the working tree, and the changed ones read at it:
/// `file` itself, and of the others those that mention a name `file` declares or uses then (a file that doesn't can
/// link nothing to it, so it is read as linking nothing, unparsed).
fn pin(root: &Path, conn: &Connection, sha: &str, file: &str) -> Result<(Pin, usize)> {
    let privacy = crate::privacy::Privacy::load(root);
    let paths = |out: Vec<u8>| -> HashSet<String> {
        out.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).filter(|p| !privacy.blocks(p)).collect()
    };
    let tree = paths(git::run(root, &["ls-tree", "-r", "-z", "--name-only", sha])?);
    let changed = paths(git::run(root, &["diff", "--name-only", "--no-renames", "-z", sha, "--"])?);
    let mut wanted: Vec<&String> = changed.iter().filter(|p| tree.contains(*p) && kc_extract::Lang::from_path(p).is_some_and(|l| !l.is_manifest())).collect();
    wanted.sort();
    let not_read_past = wanted.len().saturating_sub(MAX_PIN_FILES);
    wanted.truncate(MAX_PIN_FILES);
    let mut read = HashMap::new();
    let mut not_read = not_read_past;
    if !wanted.is_empty() {
        let input: String = wanted.iter().map(|p| format!("{sha}:{p}\n")).collect();
        let out = git::run_with_input(root, &["cat-file", "--batch"], input.as_bytes())?;
        let mut rest: &[u8] = &out;
        let mut blobs: Vec<(&String, String)> = Vec::new();
        for p in &wanted {
            // `<oid> blob <size>\n<content>\n`, or `<name> missing\n`.
            let Some(nl) = rest.iter().position(|&b| b == b'\n') else { break };
            let header = String::from_utf8_lossy(&rest[..nl]).into_owned();
            rest = &rest[nl + 1..];
            let size = header.split(' ').nth(2).and_then(|s| s.parse::<usize>().ok());
            let Some(size) = size.filter(|_| header.split(' ').nth(1) == Some("blob")) else {
                not_read += 1;
                continue;
            };
            let body = &rest[..size.min(rest.len())];
            rest = &rest[(size + 1).min(rest.len())..];
            if size <= MAX_PIN_BYTES {
                blobs.push((p, kc_extract::decode(body).into_owned()));
            } else {
                not_read += 1;
            }
        }
        // The file as it was then: read at the commit when it changed since, else as indexed.
        let me = match blobs.iter().find(|(p, _)| p.as_str() == file) {
            Some((p, text)) => facts_of(p, text).unwrap_or_default(),
            None if tree.contains(file) && !changed.contains(file) => indexed_facts(conn, file)?,
            None => Facts::default(),
        };
        let names: HashSet<String> = me.defs.iter().chain(&me.refs).chain(&me.supers).map(|n| n.to_lowercase()).collect();
        for (p, text) in blobs {
            if p.as_str() != file && !mentions(&text, &names) {
                read.insert(p.clone(), Facts::default());
                continue;
            }
            match facts_of(p, &text) {
                Some(f) => {
                    read.insert(p.clone(), f);
                }
                None => not_read += 1,
            }
        }
    }
    Ok((Pin { tree, changed, read }, not_read))
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Structural guesses for `file` of the repo at `root`: as the working tree has it, or as of commit `at` (a full
/// object id), ranked, at most `limit`, leaving out the files of `skip` (those history already lists). A file the view
/// doesn't hold has none.
pub fn guess(root: &Path, store: &Store, file: &str, at: Option<&str>, skip: &HashSet<String>, limit: usize) -> Result<Guesses> {
    let (pin, not_read) = match at {
        Some(sha) => {
            let (p, n) = pin(root, store.conn(), sha, file)?;
            (Some(p), n)
        }
        None => (None, 0),
    };
    let view = View { conn: store.conn(), pin };
    let files = view.files()?;
    if !files.iter().any(|f| f == file) {
        return Ok(Guesses { not_read, ..Guesses::default() });
    }
    let me = view.facts(file)?;
    let mut found: HashMap<String, Vec<Relation>> = HashMap::new();
    let add = |found: &mut HashMap<String, Vec<Relation>>, other: &str, how: &'static str, evidence: Evidence, name: Option<&str>| {
        if other == file {
            return;
        }
        let rels = found.entry(other.to_string()).or_default();
        match rels.iter_mut().find(|r| r.how == how) {
            Some(r) => {
                if let Some(n) = name
                    && r.names.len() < MAX_NAMES
                    && !r.names.iter().any(|x| x == n)
                {
                    r.names.push(n.to_string());
                }
            }
            None => rels.push(Relation { how, evidence, names: name.map(|n| vec![n.to_string()]).unwrap_or_default() }),
        }
    };
    let linkable = |n: &str| n.chars().count() >= 3;
    // What it uses: a name it references that one other file declares.
    for n in me.refs.iter().filter(|n| linkable(n) && !me.defs.iter().any(|d| d.eq_ignore_ascii_case(n))) {
        let others: Vec<String> = view.definers(n)?.into_iter().filter(|f| f != file).collect();
        if let [one] = others.as_slice() {
            add(&mut found, one, "uses", Evidence::new(Mechanism::NameUnique), Some(n));
        }
    }
    // What uses it: a reference to a name only it declares.
    for n in me.defs.iter().filter(|n| linkable(n)) {
        if view.definers(n)?.iter().any(|f| f != file) {
            continue;
        }
        for other in view.referrers(n)? {
            add(&mut found, &other, "used_by", Evidence::new(Mechanism::NameUnique), Some(n));
        }
        for other in view.subtypes(n)? {
            add(&mut found, &other, "implemented_by", Evidence::new(Mechanism::Declaration), Some(n));
        }
    }
    // What it implements or extends.
    for n in me.supers.iter().filter(|n| linkable(n)) {
        let others: Vec<String> = view.definers(n)?.into_iter().filter(|f| f != file).collect();
        if let [one] = others.as_slice() {
            add(&mut found, one, "implements", Evidence::new(Mechanism::Declaration), Some(n));
        }
    }
    // Tests named for it, or the file it is named for; files beside it.
    let mine = subject(file);
    let my_subject = tested_subject(file).filter(|_| kc_store::is_test_path(file));
    let dir = dir_of(file);
    let beside: Vec<&String> = files.iter().filter(|f| dir_of(f) == dir && f.as_str() != file).collect();
    for f in &files {
        if kc_store::is_test_path(f) && tested_subject(f).as_deref() == Some(mine.as_str()) {
            add(&mut found, f, "test_named", Evidence::new(Mechanism::FilenameGuess), None);
        }
        if my_subject.as_deref().is_some_and(|s| subject(f) == s && !kc_store::is_test_path(f)) {
            add(&mut found, f, "subject_named", Evidence::new(Mechanism::FilenameGuess), None);
        }
    }
    let small_dir = beside.len() < MAX_DIR_FILES;
    for f in beside {
        if small_dir || found.contains_key(f.as_str()) {
            add(&mut found, f, "same_directory", Evidence::new(Mechanism::Convention).assuming(Assumption::DirectoryConvention), None);
        }
    }
    let mut list: Vec<Guess> = found
        .into_iter()
        .filter(|(f, _)| !skip.contains(f))
        .map(|(f, mut relations)| {
            relations.sort_by_key(|r| order(r.how));
            let score = relations.iter().map(|r| weight(r.how) + u32::from(r.names.len() >= MAX_NAMES)).sum();
            Guess { test: kc_store::is_test_path(&f), file: f, basis: "structure", confidence: "low", relations, score }
        })
        .collect();
    list.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.file.cmp(&b.file)));
    let total = list.len();
    list.truncate(limit);
    Ok(Guesses { list, total, not_read })
}

/// How much a relation says that the two change together.
fn weight(how: &str) -> u32 {
    match how {
        "implements" | "implemented_by" | "test_named" | "subject_named" => 4,
        "uses" | "used_by" => 3,
        _ => 1,
    }
}

fn order(how: &str) -> u8 {
    match how {
        "implements" => 0,
        "implemented_by" => 1,
        "uses" => 2,
        "used_by" => 3,
        "test_named" => 4,
        "subject_named" => 5,
        _ => 6,
    }
}
