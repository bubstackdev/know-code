//! Precise references from a SCIP index (Sourcegraph's code intelligence format), when the user has run an indexer
//! (`scip-typescript`, `scip-dotnet`, …). `kc_index::scip` reads the file; this module stores what it found and
//! answers from it at query time.
//!
//! A SCIP index is a snapshot of the tree when the indexer ran. It is used only for files whose content is still
//! what it was then: `scip_files` records each covered file's hash, and a row about a file applies only while the
//! file's current hash (`files.hash`) matches, and while nothing else its compilation reads has changed either (see
//! [`unit_inputs`]): a file's project whose inputs changed since is `stale`, and KnowCode's own resolution answers
//! there until the indexer runs again. References and implementations are stored by location (path, line and
//! the identifier written there), never by symbol id, so re-extracting a file never leaves them pointing at the wrong
//! row: a location is mapped to the innermost symbol of that name containing it when it is read.
//!
//! At query time a reference whose site matches a SCIP reference occurrence (same file, line and identifier) takes
//! the occurrence's target: the symbol at the SCIP symbol's definition, `exact` and `precise`, or no target in the
//! repo (`unresolved`, `precise`) when SCIP defines the symbol outside it. When that disagrees with what the
//! heuristics picked, the hit says what they had said (`corrected_from`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::query::{REF_COLS, RefHit, ref_row};

/// A location in the repo: path, 1-based line, and the identifier written there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Loc {
    pub path: String,
    pub line: u32,
    pub name: String,
}

/// What a SCIP index says about the repo, ready to store.
#[derive(Debug, Default)]
pub struct Facts {
    /// Covered files and their content hash when the indexer ran.
    pub files: Vec<(String, String)>,
    /// Reference sites and the definition each reaches; `None` for a symbol defined outside the repo.
    pub refs: Vec<(Loc, Option<Loc>)>,
    /// `is_implementation` relationships: (implementation, what it implements), both defined in the repo.
    pub impls: Vec<(Loc, Loc)>,
}

/// Replaces the stored SCIP facts with `facts`.
pub fn replace(tx: &Transaction, facts: &Facts) -> Result<()> {
    tx.execute_batch("DELETE FROM scip_files; DELETE FROM scip_refs; DELETE FROM scip_impls;")?;
    let mut f = tx.prepare_cached("INSERT OR REPLACE INTO scip_files(path, hash) VALUES (?1, ?2)")?;
    for (path, hash) in &facts.files {
        f.execute(params![path, hash])?;
    }
    let mut r = tx.prepare_cached(
        "INSERT INTO scip_refs(path, line, name, def_path, def_line, def_name) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (site, def) in &facts.refs {
        r.execute(params![
            site.path,
            site.line,
            site.name,
            def.as_ref().map(|d| &d.path),
            def.as_ref().map(|d| d.line),
            def.as_ref().map(|d| &d.name)
        ])?;
    }
    let mut i = tx.prepare_cached(
        "INSERT INTO scip_impls(path, line, name, base_path, base_line, base_name) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (imp, base) in &facts.impls {
        i.execute(params![imp.path, imp.line, imp.name, base.path, base.line, base.name])?;
    }
    Ok(())
}

/// What SCIP says a reference reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The symbol with this id.
    Target(i64),
    /// Something defined outside the repo.
    External,
}

/// Query-time lookups, cached for one resolution pass.
pub(crate) struct Precise {
    on: bool,
    /// Units whose inputs changed since the SCIP index was read, and how files map to units (loaded only when some
    /// are).
    stale: Option<(BTreeSet<String>, Units)>,
    covered: HashMap<String, bool>,
    defs: HashMap<Loc, Option<i64>>,
}

impl Precise {
    pub(crate) fn new(conn: &Connection) -> Result<Precise> {
        let on = conn.prepare_cached("SELECT 1 FROM scip_files LIMIT 1")?.exists([])?;
        let stale_units = if on { stale_units(conn)? } else { BTreeSet::new() };
        let stale = if stale_units.is_empty() { None } else { Some((stale_units, Units::load(conn)?)) };
        Ok(Precise { on, stale, covered: HashMap::new(), defs: HashMap::new() })
    }

    /// Whether `path` is covered, unchanged since the SCIP index was made, and in a unit whose inputs are unchanged
    /// too.
    fn covered(&mut self, conn: &Connection, path: &str) -> Result<bool> {
        if let Some(&c) = self.covered.get(path) {
            return Ok(c);
        }
        let lang: Option<String> = conn
            .prepare_cached("SELECT f.lang FROM scip_files s JOIN files f ON f.path = s.path AND f.hash = s.hash WHERE s.path = ?1")?
            .query_row([path], |r| r.get(0))
            .optional()?;
        let c = lang.is_some_and(|lang| match &self.stale {
            Some((stale, units)) => units.key(path, &lang).is_none_or(|k| !stale.contains(&k)),
            None => true,
        });
        self.covered.insert(path.to_string(), c);
        Ok(c)
    }

    /// The symbol a definition location names, if its file is still as SCIP saw it.
    fn symbol(&mut self, conn: &Connection, loc: &Loc) -> Result<Option<i64>> {
        if let Some(&id) = self.defs.get(loc) {
            return Ok(id);
        }
        let id = if self.covered(conn, &loc.path)? { symbol_at(conn, loc)? } else { None };
        self.defs.insert(loc.clone(), id);
        Ok(id)
    }

    /// What SCIP says the reference `name` at `file:line` reaches; `None` when SCIP says nothing usable (no
    /// occurrence there, a stale file, a definition KnowCode has no symbol for, or occurrences that disagree).
    pub(crate) fn verdict(&mut self, conn: &Connection, file: &str, line: u32, name: &str) -> Result<Option<Verdict>> {
        if !self.on || !self.covered(conn, file)? {
            return Ok(None);
        }
        let mut stmt = conn.prepare_cached(
            "SELECT def_path, def_line, def_name FROM scip_refs WHERE path = ?1 AND line = ?2 AND name = ?3",
        )?;
        let rows: Vec<(Option<String>, Option<u32>, Option<String>)> =
            stmt.query_map(params![file, line, name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        let mut out: Option<Verdict> = None;
        for row in rows {
            let v = match row {
                (Some(path), Some(line), Some(name)) => match self.symbol(conn, &Loc { path, line, name })? {
                    Some(id) => Verdict::Target(id),
                    None => return Ok(None),
                },
                _ => Verdict::External,
            };
            if out.is_some_and(|o| o != v) {
                return Ok(None);
            }
            out = Some(v);
        }
        Ok(out)
    }
}

impl Precise {
    /// For a creation `name` at `file:line` (`new Order(…)`), the declared constructor SCIP says it calls. `None` when
    /// SCIP says nothing usable there: no occurrence, a stale file, occurrences that disagree, or an occurrence of the
    /// type rather than a constructor (scip-dotnet's creations name the type), which says nothing of the overload.
    pub(crate) fn constructor(&mut self, conn: &Connection, file: &str, line: u32, name: &str) -> Result<Option<i64>> {
        if !self.on || !self.covered(conn, file)? {
            return Ok(None);
        }
        let rows: Vec<(String, u32)> = conn
            .prepare_cached(
                "SELECT def_path, def_line FROM scip_refs WHERE path = ?1 AND line = ?2 AND name = ?3 AND def_path IS NOT NULL AND def_line IS NOT NULL",
            )?
            .query_map(params![file, line, name], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = None;
        for (path, def_line) in rows {
            if !self.covered(conn, &path)? {
                return Ok(None);
            }
            let found: Option<i64> = conn
                .prepare_cached(
                    "SELECT s.id FROM symbols s JOIN files f ON f.id = s.file_id
                     WHERE f.path = ?1 AND s.start_line <= ?2 AND s.end_line >= ?2
                     AND s.kind = 'init' AND (s.container = ?3 OR s.container LIKE '%.' || ?3)
                     ORDER BY s.end_line - s.start_line LIMIT 1",
                )?
                .query_row(params![path, def_line, name], |r| r.get(0))
                .optional()?;
            let Some(id) = found else { return Ok(None) };
            if out.is_some_and(|o| o != id) {
                return Ok(None);
            }
            out = Some(id);
        }
        Ok(out)
    }
}

/// The innermost symbol named `loc.name` whose span holds `loc.line` in `loc.path`. A constructor's definition is the
/// type's name: it maps to the type, which is what KnowCode's `instantiate` references point at.
fn symbol_at(conn: &Connection, loc: &Loc) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT s.id, s.name, s.kind, s.parent_id, s.container FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.path = ?1 AND s.start_line <= ?2 AND s.end_line >= ?2
         ORDER BY s.end_line - s.start_line, s.start_line DESC",
    )?;
    type Row = (i64, String, String, Option<i64>, Option<String>);
    let rows: Vec<Row> = stmt
        .query_map(params![loc.path, loc.line], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if let Some(r) = rows.iter().find(|r| r.1 == loc.name) {
        return Ok(Some(r.0));
    }
    Ok(rows
        .iter()
        .find(|r| r.2 == "init" && r.4.as_deref().is_some_and(|c| c.rsplit('.').next() == Some(&loc.name)))
        .and_then(|r| r.3))
}

/// The symbols SCIP says implement `id` (an interface member or type), or that `id` implements (`bases`).
fn related(conn: &Connection, id: i64, bases: bool) -> Result<Vec<i64>> {
    let mut p = Precise::new(conn)?;
    if !p.on {
        return Ok(Vec::new());
    }
    let sym: Option<(String, String, u32, u32)> = conn
        .query_row(
            "SELECT f.path, s.name, s.start_line, s.end_line FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((path, name, start, end)) = sym else { return Ok(Vec::new()) };
    if !p.covered(conn, &path)? {
        return Ok(Vec::new());
    }
    let sql = if bases {
        "SELECT base_path, base_line, base_name, line FROM scip_impls WHERE path = ?1 AND name = ?2 AND line BETWEEN ?3 AND ?4"
    } else {
        "SELECT path, line, name, base_line FROM scip_impls WHERE base_path = ?1 AND base_name = ?2 AND base_line BETWEEN ?3 AND ?4"
    };
    let rows: Vec<(Loc, u32)> = conn
        .prepare_cached(sql)?
        .query_map(params![path, name, start, end], |r| Ok((Loc { path: r.get(0)?, line: r.get(1)?, name: r.get(2)? }, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (loc, own_line) in rows {
        // The row must be about `id` itself, not a symbol nested in it with the same name.
        if p.symbol(conn, &Loc { path: path.clone(), line: own_line, name: name.clone() })? != Some(id) {
            continue;
        }
        if let Some(other) = p.symbol(conn, &loc)?
            && !out.contains(&other)
        {
            out.push(other);
        }
    }
    Ok(out)
}

/// What SCIP says implements `id`: for an interface member, the members implementing it.
pub fn implementations(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    related(conn, id, false)
}

/// What SCIP says `id` implements: for a member, the interface members it implements.
pub fn implemented(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    related(conn, id, true)
}

/// SCIP coverage now: files it covered that are unchanged since, and its stored rows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Coverage {
    /// Files the SCIP index covered.
    pub files: usize,
    /// Of those, the ones unchanged since, in a project whose inputs are unchanged too: the files it is still
    /// applied to.
    pub current: usize,
    pub refs: usize,
    pub impls: usize,
    /// Projects (`csharp:src/App`, or `ts:` for a repo's TypeScript) whose compilation inputs changed since the
    /// SCIP index was made: KnowCode's own resolution answers for their files until the indexer runs again.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stale_projects: Vec<String>,
}

pub fn coverage(conn: &Connection) -> Result<Coverage> {
    let count = |sql: &str| -> Result<usize> { Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as usize) };
    let stale = stale_units(conn)?;
    let current = if stale.is_empty() {
        count("SELECT count(*) FROM scip_files s JOIN files f ON f.path = s.path AND f.hash = s.hash")?
    } else {
        let units = Units::load(conn)?;
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT s.path, f.lang FROM scip_files s JOIN files f ON f.path = s.path AND f.hash = s.hash")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows.iter().filter(|(p, l)| units.key(p, l).is_none_or(|k| !stale.contains(&k))).count()
    };
    Ok(Coverage {
        files: count("SELECT count(*) FROM scip_files")?,
        current,
        refs: count("SELECT count(*) FROM scip_refs")?,
        impls: count("SELECT count(*) FROM scip_impls")?,
        stale_projects: stale.into_iter().collect(),
    })
}

fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.prepare_cached("SELECT value FROM meta WHERE key = ?1")?.query_row([key], |r| r.get(0)).optional()?)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.prepare_cached("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value")?
        .execute(params![key, value])?;
    Ok(())
}

/// The units whose inputs changed since the SCIP index was read, as the last check found them.
fn stale_units(conn: &Connection) -> Result<BTreeSet<String>> {
    Ok(meta(conn, "scip.stale")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

/// Records the units' input fingerprints as the SCIP index saw them, when it is read: a unit with an input modified
/// after the SCIP file (`scip_mtime`) is stale from the start, since the indexer didn't see it.
pub fn record_inputs(conn: &Connection, root: &Path, scip_mtime: i64) -> Result<()> {
    let mut hashes = BuildHashes::new();
    let Inputs { units: mut fps, newer: mut stale, dirs } = unit_inputs(conn, root, Some(scip_mtime), &mut hashes)?;
    // Only the units holding a covered file matter.
    let units = Units::load(conn)?;
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT s.path, f.lang FROM scip_files s JOIN files f ON f.path = s.path")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let covered: BTreeSet<String> = rows.iter().filter_map(|(p, l)| units.key(p, l)).collect();
    fps.retain(|k, _| covered.contains(k));
    stale.retain(|k| covered.contains(k));
    set_meta(conn, "scip.build", &serde_json::to_string(&hashes)?)?;
    set_meta(conn, "scip.units", &serde_json::to_string(&fps)?)?;
    set_meta(conn, "scip.ingest_stale", &serde_json::to_string(&stale)?)?;
    set_meta(conn, "scip.stale", &serde_json::to_string(&stale)?)?;
    set_meta(conn, "scip.dirs", &serde_json::to_string(&dirs)?)?;
    set_meta(conn, "scip.checked", &checked_stamp(conn, root, &dirs)?)?;
    Ok(())
}

/// What a check of the inputs depends on: the index generation (every reparse changes it) and the stat of the build
/// files in `dirs`. While it holds, the inputs are as last checked, without reading every file's row.
fn checked_stamp(conn: &Connection, root: &Path, dirs: &BTreeSet<String>) -> Result<String> {
    // Stats only: each directory's (an added, removed or renamed build file changes it) and each known build file's
    // (an edit in place).
    let known: BuildHashes = meta(conn, "scip.build")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let stat = |p: &Path| {
        std::fs::metadata(p).ok().map(|m| {
            let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
            format!("{mtime}:{}", m.len())
        })
    };
    let mut h = blake3::Hasher::new();
    for p in dirs.iter().chain(known.keys()) {
        h.update(p.as_bytes());
        h.update(stat(&root.join(p)).unwrap_or_default().as_bytes());
    }
    Ok(format!("{}|{}", meta(conn, "tree_gen")?.unwrap_or_default(), h.finalize().to_hex()))
}

/// Whether [`check_inputs`] has anything to check: there are SCIP facts with recorded inputs, and the generation or a
/// build file's stat moved since the last check. Reads only; the caller takes the write lock only when it is so.
pub fn inputs_may_have_changed(conn: &Connection, root: &Path) -> Result<bool> {
    if !conn.prepare_cached("SELECT 1 FROM scip_files LIMIT 1")?.exists([])? || meta(conn, "scip.units")?.is_none() {
        return Ok(false);
    }
    let dirs: BTreeSet<String> = meta(conn, "scip.dirs")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    Ok(meta(conn, "scip.checked")? != Some(checked_stamp(conn, root, &dirs)?))
}

/// Compares every unit's inputs now with what the SCIP index saw (see [`record_inputs`]) and records the stale ones.
/// Returns whether that set changed, which changes resolution: then the caller moves the index generation on, and
/// either way calls [`mark_checked`]. `None` when nothing could have changed since the last check (same generation,
/// build files with the same stat), which reads no file rows.
pub fn check_inputs(conn: &Connection, root: &Path) -> Result<Option<bool>> {
    if !inputs_may_have_changed(conn, root)? {
        return Ok(None);
    }
    let Some(recorded) = meta(conn, "scip.units")?.and_then(|s| serde_json::from_str::<BTreeMap<String, String>>(&s).ok()) else {
        return Ok(None);
    };
    let at_ingest: BTreeSet<String> = meta(conn, "scip.ingest_stale")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut hashes: BuildHashes = meta(conn, "scip.build")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let before = hashes.clone();
    let now = unit_inputs(conn, root, None, &mut hashes)?;
    if hashes != before {
        set_meta(conn, "scip.build", &serde_json::to_string(&hashes)?)?;
    }
    let mut stale = at_ingest;
    // A unit new since (a project added) has no fingerprint to keep: none of its files were covered.
    stale.extend(recorded.iter().filter(|(k, fp)| now.units.get(*k) != Some(*fp)).map(|(k, _)| k.clone()));
    let changed = stale != stale_units(conn)?;
    if changed {
        set_meta(conn, "scip.stale", &serde_json::to_string(&stale)?)?;
    }
    set_meta(conn, "scip.dirs", &serde_json::to_string(&now.dirs)?)?;
    Ok(Some(changed))
}

/// Records that the inputs were checked at the index's current generation (see [`check_inputs`]).
pub fn mark_checked(conn: &Connection, root: &Path) -> Result<()> {
    let dirs: BTreeSet<String> = meta(conn, "scip.dirs")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    set_meta(conn, "scip.checked", &checked_stamp(conn, root, &dirs)?)
}

// ---- Compilation inputs ----------------------------------------------------------------------------------------
//
// A compiler's answer for one reference depends on every input of the compilation, not only the two files it joins:
// a `global using` alias in a third file, a new overload elsewhere in the project, a project file, a referenced
// project, a lock file. So SCIP facts apply per project ("unit"): each unit's inputs get a fingerprint when SCIP is
// read, and a unit whose inputs changed since loses its SCIP facts (KnowCode's own resolution answers) until the
// indexer runs again. Units: a C# project is the directory of a `.csproj` with the C# files under it (nearest
// project wins), its project file, the build files (`Directory.Build.props`, `global.json`, `packages.lock.json`,
// …) in it and above it, and the units it references (`<ProjectReference>`, transitively); C# files under no
// project are one unit holding every C# file and project file. Every other language is one unit per repo: its
// files, every manifest, and its lock and config files anywhere a manifest is (a relative import can reach any
// directory, so nothing smaller is sound).

/// The compilation family a file language belongs to, for the languages SCIP indexers cover.
fn family(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "csharp" => "csharp",
        "typescript" | "tsx" | "javascript" => "ts",
        "python" => "python",
        "rust" => "rust",
        "swift" => "swift",
        _ => return None,
    })
}

/// Manifest languages (`kc_extract::Lang::is_manifest`): inputs of every non-C# unit.
const MANIFEST_LANGS: &[&str] = &["swiftpm", "xcodegen", "cargo", "csproj", "npm"];

/// Whether a file name is a build input of `family` that KnowCode doesn't index: read by its stat.
fn is_build_file(family: &str, name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    match family {
        "csharp" => {
            matches!(lower.as_str(), "directory.build.props" | "directory.build.targets" | "directory.packages.props" | "global.json" | "nuget.config" | "packages.lock.json")
                || lower.ends_with(".sln")
                || lower.ends_with(".slnx")
        }
        "ts" => {
            matches!(lower.as_str(), "package-lock.json" | "npm-shrinkwrap.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb")
                || ((lower.starts_with("tsconfig") || lower.starts_with("jsconfig")) && lower.ends_with(".json"))
        }
        "python" => {
            matches!(lower.as_str(), "poetry.lock" | "pipfile.lock" | "pipfile" | "uv.lock" | "pyproject.toml" | "setup.cfg" | "setup.py")
                || (lower.starts_with("requirements") && lower.ends_with(".txt"))
        }
        "rust" => lower == "cargo.lock",
        "swift" => lower == "package.resolved",
        _ => false,
    }
}

/// The parent directory of a repo path (`""` at the root).
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Whether `path` lies in directory `dir` (the root holds everything).
fn under(path: &str, dir: &str) -> bool {
    dir.is_empty() || (path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/')
}

/// `dir/rel` with `.` and `..` resolved; `None` when it leaves the repo.
fn join_rel(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// Maps a file to its unit (see the section comment).
pub struct Units {
    /// Directories holding a `.csproj`, deepest first.
    projects: Vec<String>,
}

/// The unit of C# files under no project.
const LOOSE: &str = "csharp:*";

impl Units {
    pub fn load(conn: &Connection) -> Result<Units> {
        let paths: Vec<String> =
            conn.prepare_cached("SELECT path FROM files WHERE lang = 'csproj'")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut projects: Vec<String> = paths.iter().map(|p| parent_dir(p).to_string()).collect();
        projects.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        projects.dedup();
        Ok(Units { projects })
    }

    /// The unit of the file at `path` of language `lang`; `None` for a language no unit covers.
    pub fn key(&self, path: &str, lang: &str) -> Option<String> {
        let family = family(lang)?;
        if family != "csharp" {
            return Some(format!("{family}:"));
        }
        Some(self.projects.iter().find(|d| under(path, d)).map_or_else(|| LOOSE.to_string(), |d| format!("csharp:{d}")))
    }
}

/// Content hashes of build files by path, each with the stat (`mtime:size`) it was hashed at: a file is read again
/// only when its stat changes.
pub type BuildHashes = BTreeMap<String, (String, String)>;

/// Build files of any family in `dirs` of the checkout at `root`, by path, with their stat (`mtime:size`) and mtime;
/// sorted.
fn build_files(root: &Path, dirs: &BTreeSet<String>) -> Vec<(String, String, i64)> {
    let mut out = Vec::new();
    for d in dirs {
        let Ok(entries) = std::fs::read_dir(root.join(d)) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !["csharp", "ts", "python", "rust", "swift"].iter().any(|f| is_build_file(f, &name)) {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64);
            let path = if d.is_empty() { name } else { format!("{d}/{name}") };
            out.push((path, format!("{mtime}:{}", meta.len()), mtime));
        }
    }
    out.sort();
    out
}

/// What [`unit_inputs`] found.
pub struct Inputs {
    /// Each unit's fingerprint.
    pub units: BTreeMap<String, String>,
    /// Units with an input modified after the time given.
    pub newer: BTreeSet<String>,
    /// The directories build files were looked for in.
    pub dirs: BTreeSet<String>,
}

/// Fingerprints of every unit's inputs now, and the units with an input modified after `since` (nanoseconds; SCIP's
/// own modification time at ingest). Build files are found by listing the directories that may hold them, and
/// compared by content (`hashes` is the cache, updated in place).
pub fn unit_inputs(conn: &Connection, root: &Path, since: Option<i64>, hashes: &mut BuildHashes) -> Result<Inputs> {
    let units = Units::load(conn)?;
    let rows: Vec<(String, String, String, i64)> = conn
        .prepare_cached("SELECT path, hash, lang, mtime_ns FROM files ORDER BY path")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // Each unit's own entries (`path hash`), and whether one is newer than `since`.
    let mut own: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut newer: BTreeSet<String> = BTreeSet::new();
    let mut add = |own: &mut BTreeMap<String, Vec<String>>, key: &str, entry: String, mtime: i64| {
        own.entry(key.to_string()).or_default().push(entry);
        if since.is_some_and(|s| mtime > s) {
            newer.insert(key.to_string());
        }
    };
    let families: BTreeSet<&str> = rows.iter().filter_map(|r| family(&r.2)).collect();
    let mut manifest_dirs: BTreeSet<String> = BTreeSet::from([String::new()]);
    for (path, hash, lang, mtime) in &rows {
        let entry = format!("{path}\0{hash}");
        if let Some(key) = units.key(path, lang) {
            add(&mut own, &key, entry.clone(), *mtime);
        }
        if MANIFEST_LANGS.contains(&lang.as_str()) {
            manifest_dirs.insert(parent_dir(path).to_string());
            for f in families.iter().filter(|f| **f != "csharp") {
                add(&mut own, &format!("{f}:"), entry.clone(), *mtime);
            }
        }
        if lang == "csproj" {
            add(&mut own, &format!("csharp:{}", parent_dir(path)), entry.clone(), *mtime);
            if families.contains("csharp") {
                add(&mut own, LOOSE, entry, *mtime);
            }
        }
    }
    // Build files: in each project directory and above it (C#), and wherever a manifest is (the others).
    let mut dirs: BTreeSet<String> = manifest_dirs;
    for d in &units.projects {
        let mut d = d.as_str();
        loop {
            dirs.insert(d.to_string());
            if d.is_empty() {
                break;
            }
            d = parent_dir(d);
        }
    }
    let mut build: Vec<(String, String, i64)> = Vec::new();
    for (path, stat, mtime) in build_files(root, &dirs) {
        let name = path.rsplit('/').next().unwrap_or(&path);
        if !families.iter().any(|f| is_build_file(f, name)) {
            continue;
        }
        let hash = match hashes.get(&path) {
            Some((at, h)) if *at == stat => h.clone(),
            _ => {
                let Ok(bytes) = std::fs::read(root.join(&path)) else { continue };
                let h = blake3::hash(&bytes).to_hex().to_string();
                hashes.insert(path.clone(), (stat, h.clone()));
                h
            }
        };
        build.push((path, hash, mtime));
    }
    let seen: BTreeSet<&str> = build.iter().map(|(p, _, _)| p.as_str()).collect();
    hashes.retain(|p, _| seen.contains(p.as_str()));
    for (path, stat, mtime) in &build {
        let name = path.rsplit('/').next().unwrap_or(path);
        let dir = parent_dir(path);
        for f in families.iter().filter(|f| is_build_file(f, name)) {
            if *f == "csharp" {
                // A build file applies to the projects in and under its directory.
                for p in units.projects.iter().filter(|p| *p == dir || under(p, dir)) {
                    add(&mut own, &format!("csharp:{p}"), format!("{path}\0{stat}"), *mtime);
                }
                add(&mut own, LOOSE, format!("{path}\0{stat}"), *mtime);
            } else {
                add(&mut own, &format!("{f}:"), format!("{path}\0{stat}"), *mtime);
            }
        }
    }
    // Project references, by the referencing project's unit.
    let mut refs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT f.path, d.location FROM package_deps d JOIN packages p ON p.id = d.package_id JOIN files f ON f.id = p.file_id
         WHERE f.lang = 'csproj' AND d.source = 'path'",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (file, location) = row?;
        let Some(target) = join_rel(parent_dir(&file), &location) else { continue };
        refs.entry(format!("csharp:{}", parent_dir(&file))).or_default().insert(format!("csharp:{}", parent_dir(&target)));
    }
    // A unit's fingerprint covers its own entries and its references' fingerprints; so does its staleness.
    fn fold(
        key: &str,
        own: &BTreeMap<String, Vec<String>>,
        refs: &BTreeMap<String, BTreeSet<String>>,
        newer: &BTreeSet<String>,
        memo: &mut BTreeMap<String, (String, bool)>,
        visiting: &mut BTreeSet<String>,
    ) -> (String, bool) {
        if let Some(m) = memo.get(key) {
            return m.clone();
        }
        let mut h = blake3::Hasher::new();
        let mut stale = newer.contains(key);
        for e in own.get(key).into_iter().flatten() {
            h.update(e.as_bytes());
            h.update(b"\n");
        }
        visiting.insert(key.to_string());
        for r in refs.get(key).into_iter().flatten() {
            // A reference cycle is cut where it closes; every unit on it still covers the others' own inputs.
            if visiting.contains(r) || !own.contains_key(r) {
                continue;
            }
            let (fp, s) = fold(r, own, refs, newer, memo, visiting);
            h.update(r.as_bytes());
            h.update(fp.as_bytes());
            stale |= s;
        }
        visiting.remove(key);
        let out = (h.finalize().to_hex().to_string(), stale);
        memo.insert(key.to_string(), out.clone());
        out
    }
    let mut memo = BTreeMap::new();
    let mut fps = BTreeMap::new();
    let mut stale = BTreeSet::new();
    for key in own.keys() {
        let (fp, s) = fold(key, &own, &refs, &newer, &mut memo, &mut BTreeSet::new());
        if s {
            stale.insert(key.clone());
        }
        fps.insert(key.clone(), fp);
    }
    Ok(Inputs { units: fps, newer: stale, dirs })
}


/// How the heuristic resolution of the references in covered files compares with SCIP's.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Agreement {
    /// References in covered files.
    pub refs: usize,
    /// Of those, the ones a SCIP occurrence answers.
    pub matched: usize,
    /// Per heuristic confidence, how SCIP's answer compares:
    /// - `same`: the heuristics picked exactly SCIP's target,
    /// - `among`: SCIP's target is one of several the heuristics listed (overloads, ambiguous candidates),
    /// - `other`: the heuristics picked targets in the repo, none of them SCIP's,
    /// - `missed`: the heuristics found no target; SCIP's is in the repo,
    /// - `external_ok`: SCIP's symbol is outside the repo and the heuristics found no target,
    /// - `external_false`: SCIP's symbol is outside the repo and the heuristics picked one in it.
    pub matrix: BTreeMap<String, BTreeMap<String, usize>>,
    /// References made `exact` by SCIP that the heuristics had narrowed less (`unique_name`, `ambiguous` with the
    /// target among the candidates, several overloads).
    pub upgraded: usize,
    /// References whose heuristic answer SCIP contradicts (`other`, `missed`, `external_false` unless the heuristics
    /// had already said `external_likely`).
    pub corrected: usize,
    /// Members SCIP says implement another (`is_implementation`, both in the repo): interface members, and abstract
    /// or virtual members overridden.
    #[serde(default)]
    pub member_impls: usize,
    /// Of those, the ones KnowCode's own rule (a same-named member of a type declaring the interface) also finds.
    #[serde(default)]
    pub member_impls_declared: usize,
    /// Of the C# creations (`new T(…)`) of types declaring constructors that SCIP says call a declared (or primary)
    /// constructor, how the constructors the heuristics say the arguments pick compare: `same` (that one alone),
    /// `among` (several, SCIP's one of them: an ambiguous edge), `other` (not SCIP's), `implicit` (none: the
    /// heuristics took the implicit parameterless one).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub constructors: BTreeMap<String, usize>,
    /// The candidates listed by the `ambiguous` references SCIP answers, all together: with `matrix.ambiguous.among`,
    /// how often an ambiguous edge a walk could follow is SCIP's target.
    #[serde(default)]
    pub ambiguous_targets: usize,
    /// A sample of the contradictions, for finding their causes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disagreements: Vec<Disagreement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Disagreement {
    pub site: String,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    pub heuristic: String,
    /// What the heuristics picked (qualified names, at most 3).
    pub heuristic_targets: Vec<String>,
    /// SCIP's target, `file:line qualified`, or `external`.
    pub scip: String,
    pub class: String,
}

/// Classifies one heuristic answer against SCIP's verdict (see [`Agreement::matrix`]).
pub(crate) fn classify(ids: &[i64], v: Verdict) -> &'static str {
    match v {
        Verdict::Target(t) if ids == [t] => "same",
        Verdict::Target(t) if ids.contains(&t) => "among",
        Verdict::Target(_) if ids.is_empty() => "missed",
        Verdict::Target(_) => "other",
        Verdict::External if ids.is_empty() => "external_ok",
        Verdict::External => "external_false",
    }
}

/// Resolves every reference in the covered files by the heuristics alone and compares with SCIP, keeping up to
/// `samples` disagreements.
pub fn agreement(conn: &Connection, samples: usize) -> Result<Agreement> {
    let sql = format!(
        "SELECT {REF_COLS} FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols fs ON fs.id = r.from_id
         JOIN scip_files sf ON sf.path = f.path AND sf.hash = f.hash ORDER BY f.path, r.line"
    );
    let mut hits: Vec<RefHit> = conn.prepare(&sql)?.query_map([], ref_row)?.collect::<rusqlite::Result<_>>()?;
    crate::query::resolve_heuristic(conn, &mut hits)?;
    let mut p = Precise::new(conn)?;
    let mut out = Agreement { refs: hits.len(), ..Default::default() };
    let mut seen_causes: HashSet<(String, String, String)> = HashSet::new();
    let mut kept = Vec::new();
    for h in &hits {
        // A creation: the constructor its arguments pick, against SCIP's.
        if let Some(ctors) = &h.ctors
            && let Some(c) = p.constructor(conn, &h.file, h.line, &h.name)?
        {
            let class = match ctors.as_slice() {
                [] => "implicit",
                [one] if *one == c => "same",
                _ if ctors.contains(&c) => "among",
                _ => "other",
            };
            *out.constructors.entry(class.to_string()).or_default() += 1;
        }
        let Some(v) = p.verdict(conn, &h.file, h.line, &h.name)? else { continue };
        out.matched += 1;
        if h.confidence == "ambiguous" {
            out.ambiguous_targets += h.target_ids.len();
        }
        let class = classify(&h.target_ids, v);
        *out.matrix.entry(h.confidence.to_string()).or_default().entry(class.to_string()).or_default() += 1;
        match class {
            "same" if h.confidence != "exact" => out.upgraded += 1,
            "among" => out.upgraded += 1,
            "external_false" if h.confidence == "external_likely" => {}
            "other" | "missed" | "external_false" => {
                out.corrected += 1;
                kept.push((h, v, class));
            }
            _ => {}
        }
    }
    let rows: Vec<(Loc, Loc)> = conn
        .prepare("SELECT path, line, name, base_path, base_line, base_name FROM scip_impls")?
        .query_map([], |r| {
            Ok((Loc { path: r.get(0)?, line: r.get(1)?, name: r.get(2)? }, Loc { path: r.get(3)?, line: r.get(4)?, name: r.get(5)? }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (imp, base) in rows {
        let (Some(imp), Some(base)) = (p.symbol(conn, &imp)?, p.symbol(conn, &base)?) else { continue };
        let kind: String = conn.query_row("SELECT kind FROM symbols WHERE id = ?1", [base], |r| r.get(0))?;
        if crate::query::is_type_or_extension(&kind) {
            continue;
        }
        out.member_impls += 1;
        out.member_impls_declared += crate::db::declared_implementations(conn, base)?.contains(&imp) as usize;
    }
    // A spread of causes: the first disagreement per (class, confidence, name) before repeats.
    let first: Vec<bool> =
        kept.iter().map(|(h, _, class)| seen_causes.insert((class.to_string(), h.confidence.to_string(), h.name.clone()))).collect();
    let mut order: Vec<usize> = (0..kept.len()).collect();
    order.sort_by_key(|&i| !first[i]);
    for &(h, v, class) in order.into_iter().take(samples).map(|i| &kept[i]) {
        let scip = match v {
            Verdict::Target(id) => conn.query_row(
                "SELECT f.path || ':' || s.start_line || ' ' || coalesce(s.container || '.', '') || s.name FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
                [id],
                |r| r.get(0),
            )?,
            Verdict::External => "external".to_string(),
        };
        out.disagreements.push(Disagreement {
            site: format!("{}:{}", h.file, h.line),
            name: h.name.clone(),
            kind: h.kind.clone(),
            receiver: h.receiver.clone(),
            heuristic: h.confidence.to_string(),
            heuristic_targets: h.targets.iter().take(3).map(|t| format!("{}:{} {}", t.file, t.line, t.qualified)).collect(),
            scip,
            class: class.to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_compare_target_sets() {
        assert_eq!(classify(&[1], Verdict::Target(1)), "same");
        assert_eq!(classify(&[1, 2], Verdict::Target(2)), "among");
        assert_eq!(classify(&[1, 2], Verdict::Target(3)), "other");
        assert_eq!(classify(&[], Verdict::Target(3)), "missed");
        assert_eq!(classify(&[], Verdict::External), "external_ok");
        assert_eq!(classify(&[4], Verdict::External), "external_false");
    }
}
