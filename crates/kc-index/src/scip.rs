//! Optional SCIP ingest. When the user has run a SCIP indexer (`scip-typescript`, `scip-dotnet`, `scip-python`,
//! `rust-analyzer scip`), its `index.scip` holds the compiler's answer for every reference. KnowCode reads it when
//! present (at the repo root, or the file `knowcode index --scip FILE` named), stores the references and
//! implementations that land on its own sites and symbols (see `kc_store::scip`), and resolution uses them where
//! they apply. KnowCode never runs an indexer.
//!
//! The file is protobuf (`scip.proto`); the few messages needed are decoded here by hand, with no dependency.
//!
//! A SCIP index is a snapshot: a document is used only when its file is indexed by KnowCode, its content is what
//! KnowCode indexed, and it was last modified before the SCIP file was written. Each covered file's hash is stored,
//! so a later edit retires that file's SCIP facts until the indexer runs again; and since a binding depends on every
//! input of the compilation, each project's inputs are fingerprinted too (`kc_store::scip::unit_inputs`), and a change
//! to any of them (another source file, a project file, a lock file) retires the whole project's facts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Result, bail};
use kc_store::Store;
use kc_store::scip::{Agreement, Facts, Loc};
use serde::{Deserialize, Serialize};

/// Where a repo's SCIP index is looked for when none was named.
pub const DEFAULT_FILE: &str = "index.scip";

/// Bumped when an ingest stores something new, so an index read by an older build is read again: 2 records each
/// project's input fingerprints (without them, nothing would retire facts whose project changed).
const INGEST_FORMAT: u32 = 2;

/// Disagreements kept in an ingest's report, for finding their causes.
const SAMPLES: usize = 200;

const ROLE_DEFINITION: i32 = 0x1;
const ROLE_IMPORT: i32 = 0x2;

/// What one ingest found.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    /// The SCIP file read.
    pub source: String,
    /// The indexer that wrote it (`scip-typescript 0.3.x`).
    pub tool: String,
    /// When it was read (unix seconds).
    pub ingested_at: u64,
    /// Documents in the SCIP index.
    pub documents: usize,
    /// Of those, the files used: indexed by KnowCode and unchanged since the indexer ran.
    pub covered: usize,
    /// Documents whose file changed after the indexer ran, or is gone.
    pub stale: usize,
    /// Documents for files KnowCode doesn't index (generated code, ignored paths, other languages).
    pub unindexed: usize,
    /// KnowCode reference sites a SCIP occurrence answers.
    pub refs: usize,
    /// `is_implementation` relationships between symbols defined in covered files.
    pub impls: usize,
    /// The file couldn't be read as SCIP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The heuristics against SCIP, over the references in covered files: computed by `knowcode index`, not by the
    /// read a query's freshness check makes (it resolves every reference in the covered files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreement: Option<Agreement>,
}

/// Re-reads the repo's SCIP index if it changed since the last ingest (or was never read), and says what it found,
/// with how the heuristics compare when `compare`. Nothing happens when there is no SCIP file. A file that can't be
/// read is reported, never an error: the index works without it.
pub fn refresh(root: &Path, store: &mut Store, compare: bool) -> Result<Option<Report>> {
    let path = match store.meta("scip.path")? {
        Some(p) => PathBuf::from(p),
        None => root.join(DEFAULT_FILE),
    };
    let Ok(meta) = std::fs::metadata(&path) else { return Ok(None) };
    let mtime_ns = mtime_ns(&meta);
    // Re-extracting files with a new extractor changes the sites SCIP occurrences are matched to.
    let _ = crate::config::load(); // the configured conventions are part of the extractor versions
    let versions: Vec<String> = kc_extract::Lang::ALL.iter().map(|l| l.extractor_version().to_string()).collect();
    let stamp = format!("{}|{mtime_ns}|{}|{}|i{INGEST_FORMAT}", path.display(), meta.len(), versions.join("."));
    if store.meta("scip.stamp")?.as_deref() == Some(stamp.as_str()) {
        // The facts stand while their projects' inputs do: a change anywhere in one retires its facts. The retirement
        // and the generation it moves to commit together, so no reader sees one without the other.
        if !kc_store::scip::inputs_may_have_changed(store.conn(), root)? {
            return Ok(None);
        }
        let batch = store.batch()?;
        if let Some(changed) = kc_store::scip::check_inputs(batch.conn(), root)? {
            if changed {
                batch.set_meta("tree_gen", &now_nanos().to_string())?;
            }
            kc_store::scip::mark_checked(batch.conn(), root)?;
        }
        batch.commit()?;
        return Ok(None);
    }
    let mut report = Report { source: path.display().to_string(), ingested_at: now_secs(), ..Default::default() };
    let facts = match std::fs::read(&path).map_err(anyhow::Error::from).and_then(|b| decode(&b)) {
        Ok(mut index) => {
            // Documents of files the repo's AI privacy file blocks are dropped unread: their symbols, definitions and
            // relationships never reach the index.
            let privacy = crate::privacy::Privacy::load(root);
            if privacy.blocks_anything() {
                let prefix = project_prefix(root, &index.project_root);
                index.documents.retain(|d| !privacy.blocks(&format!("{prefix}{}", d.path)));
            }
            Some(collect(root, store, &index, mtime_ns, &mut report)?)
        }
        Err(e) => {
            report.error = Some(format!("{e:#}"));
            None
        }
    };
    let batch = store.batch()?;
    if let Some(facts) = &facts {
        batch.replace_scip(facts)?;
        // Resolution changes: cached answers derived from it are recomputed.
        batch.set_meta("tree_gen", &now_nanos().to_string())?;
        // After the generation moves, which the record of the check names.
        batch.record_scip_inputs(root, mtime_ns)?;
    }
    batch.set_meta("scip.stamp", &stamp)?;
    batch.commit()?;
    if facts.is_some() && compare {
        report.agreement = Some(kc_store::scip::agreement(store.conn(), SAMPLES)?);
    }
    let mut stored = report.clone();
    if let Some(a) = stored.agreement.as_mut() {
        a.disagreements.clear();
    }
    store.set_meta("scip.report", &serde_json::to_string(&stored)?)?;
    Ok(Some(report))
}

/// The last ingest's report, without its disagreement sample.
pub fn last_report(store: &Store) -> Result<Option<Report>> {
    Ok(store.meta("scip.report")?.and_then(|s| serde_json::from_str(&s).ok()))
}

/// Points the repo at a SCIP file other than `index.scip` at its root, kept for later syncs; read again at the next
/// sync even if unchanged.
pub fn set_source(store: &Store, path: &Path) -> Result<()> {
    let path = std::path::absolute(path)?;
    if !path.is_file() {
        bail!("no SCIP index at {}", path.display());
    }
    store.set_meta("scip.path", &path.display().to_string())?;
    store.set_meta("scip.stamp", "")
}

/// What the index says about the files KnowCode indexes, by location.
fn collect(root: &Path, store: &Store, index: &Index, scip_mtime: i64, report: &mut Report) -> Result<Facts> {
    report.tool = format!("{} {}", index.tool, index.tool_version).trim().to_string();
    report.documents = index.documents.len();
    let prefix = project_prefix(root, &index.project_root);
    let states = store.file_states()?;
    let mut sites: HashSet<(String, u32, String)> = HashSet::new();
    {
        let mut stmt = store.conn().prepare("SELECT f.path, r.line, r.name FROM refs r JOIN files f ON f.id = r.file_id")?;
        for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
            sites.insert(row?);
        }
    }

    // Which documents can be used, with their text.
    let mut texts: Vec<Option<String>> = Vec::with_capacity(index.documents.len());
    let mut facts = Facts::default();
    for doc in &index.documents {
        let path = format!("{prefix}{}", doc.path);
        let Some(state) = states.get(&path) else {
            report.unindexed += 1;
            texts.push(None);
            continue;
        };
        let full = root.join(&path);
        let fresh = std::fs::metadata(&full).is_ok_and(|m| mtime_ns(&m) <= scip_mtime);
        let bytes = std::fs::read(&full).ok().filter(|b| fresh && blake3::hash(b).to_hex().as_str() == state.hash);
        let Some(bytes) = bytes else {
            report.stale += 1;
            texts.push(None);
            continue;
        };
        facts.files.push((path, state.hash.clone()));
        texts.push(Some(kc_extract::decode(&bytes).into_owned()));
    }
    report.covered = facts.files.len();

    // Definitions: where each symbol is defined. A symbol defined in a file that isn't covered, or in several places
    // (a partial class, overload signatures, or two types an indexer gave the same symbol), has no usable one: `None`.
    let mut defs: HashMap<&str, Option<Loc>> = HashMap::new();
    let mut packages: HashSet<&str> = HashSet::new();
    for (doc, text) in index.documents.iter().zip(&texts) {
        let lines: Vec<&str> = text.as_deref().map(|t| t.split('\n').collect()).unwrap_or_default();
        for occ in doc.occurrences.iter().filter(|o| o.roles & ROLE_DEFINITION != 0 && !is_local(&o.symbol)) {
            packages.insert(package(&occ.symbol));
            let loc = text.as_ref().and_then(|_| {
                let name = name_at(&lines, &occ.range, doc.encoding)?;
                Some(Loc { path: format!("{prefix}{}", doc.path), line: occ.range[0] as u32 + 1, name })
            });
            defs.entry(&occ.symbol).and_modify(|d| if *d != loc { *d = None }).or_insert(loc);
        }
    }
    let def = |symbol: &str| -> Option<Option<Loc>> { defs.get(symbol).cloned() };
    // Where a reference to `symbol` lands: `Some(None)` outside the repo, `None` when unknown. A symbol of a project
    // package with no definition occurrence is an implicit constructor (its type), a record's positional property
    // (the primary constructor's parameter), or unknown.
    let target = |symbol: &str| -> Option<Option<Loc>> {
        if let Some(d) = def(symbol) {
            return d.map(Some);
        }
        if !packages.contains(package(symbol)) {
            return Some(None);
        }
        let (owner, member) = symbol.rfind('#').map(|i| (&symbol[..=i], &symbol[i + 1..]))?;
        if ["`.ctor`", "`<constructor>`", "<init>"].iter().any(|c| member.starts_with(c)) {
            return def(owner)?.map(Some);
        }
        let property = member.strip_suffix('.').filter(|m| !m.contains(['(', '.', '#']))?;
        def(&format!("{owner}`.ctor`().({property})"))?.map(Some)
    };

    for (doc, text) in index.documents.iter().zip(&texts) {
        let Some(text) = text else { continue };
        let lines: Vec<&str> = text.split('\n').collect();
        let path = format!("{prefix}{}", doc.path);
        // KnowCode's sites have no column: a site is matched only when SCIP has an occurrence for every place the
        // identifier is written on its line, so `x.Status == Status.Open` with only the type resolved isn't
        // mistaken for the property.
        let names: Vec<Option<String>> = doc.occurrences.iter().map(|o| name_at(&lines, &o.range, doc.encoding)).collect();
        let mut on_line: HashMap<(i32, &str), usize> = HashMap::new();
        for (occ, name) in doc.occurrences.iter().zip(&names) {
            if let Some(name) = name {
                *on_line.entry((occ.range[0], name.as_str())).or_default() += 1;
            }
        }
        let mut seen = HashSet::new();
        for (occ, name) in doc.occurrences.iter().zip(&names) {
            if occ.roles & (ROLE_DEFINITION | ROLE_IMPORT) != 0 || is_local(&occ.symbol) {
                continue;
            }
            let Some(name) = name else { continue };
            if on_line[&(occ.range[0], name.as_str())] < word_count(lines[occ.range[0] as usize], name) {
                continue;
            }
            let line = occ.range[0] as u32 + 1;
            let key = (path.clone(), line, name.clone());
            if !sites.contains(&key) {
                continue;
            }
            let Some(def) = target(&occ.symbol) else { continue };
            if seen.insert((key.1, key.2.clone(), def.clone())) {
                facts.refs.push((Loc { path: key.0, line: key.1, name: key.2 }, def));
            }
        }
        for info in &doc.symbols {
            let Some(Some(imp)) = def(&info.symbol) else { continue };
            for base in &info.implements {
                if let Some(Some(b)) = def(base) {
                    facts.impls.push((imp.clone(), b));
                }
            }
        }
    }
    report.refs = facts.refs.len();
    report.impls = facts.impls.len();
    Ok(facts)
}

/// The project root's path relative to the repo, as a prefix for document paths (`src/` when the indexer ran in
/// `src`); empty when it is the repo root or lies elsewhere (an index made on another machine).
fn project_prefix(root: &Path, project_root: &str) -> String {
    let Some(p) = project_root.strip_prefix("file://") else { return String::new() };
    let p = PathBuf::from(percent_decode(p));
    let (Ok(p), Ok(root)) = (p.canonicalize(), root.canonicalize()) else { return String::new() };
    match p.strip_prefix(&root) {
        Ok(rel) if !rel.as_os_str().is_empty() => format!("{}/", rel.display()),
        _ => String::new(),
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A local symbol (`local 12`) is scoped to its document.
fn is_local(symbol: &str) -> bool {
    symbol.starts_with("local ")
}

/// The scheme, manager, package name and version a global symbol starts with.
fn package(symbol: &str) -> &str {
    let mut end = 0;
    for (n, (i, _)) in symbol.match_indices(' ').enumerate() {
        end = i;
        if n == 3 {
            break;
        }
    }
    &symbol[..end]
}

/// The identifier an occurrence's range covers, when it is on one line. `enc` is the document's position encoding:
/// offsets count UTF-8 bytes (1), UTF-32 characters (3), or UTF-16 units (2, and unspecified).
fn name_at(lines: &[&str], range: &[i32], enc: u64) -> Option<String> {
    let (line, start, end) = match *range {
        [l, s, e] => (l, s, e),
        [l, s, el, e] if el == l => (l, s, e),
        _ => return None,
    };
    let text = lines.get(usize::try_from(line).ok()?)?.trim_end_matches('\r');
    let byte = |units: i32| -> Option<usize> {
        let units = usize::try_from(units).ok()?;
        if enc == 1 {
            return (units <= text.len()).then_some(units);
        }
        let mut u = 0;
        for (i, c) in text.char_indices() {
            if u == units {
                return Some(i);
            }
            u += if enc == 3 { 1 } else { c.len_utf16() };
        }
        (u == units).then_some(text.len())
    };
    let name = text.get(byte(start)?..byte(end)?)?;
    // A C# verbatim identifier (`@class`) is the name without the `@`.
    let name = name.strip_prefix('@').unwrap_or(name);
    (!name.is_empty()).then(|| name.to_string())
}

/// How many times `name` is written on `line` as a whole identifier.
fn word_count(line: &str, name: &str) -> usize {
    let ident = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '#' | '@');
    line.match_indices(name)
        .filter(|(i, _)| {
            !line[..*i].chars().next_back().is_some_and(ident) && !line[i + name.len()..].chars().next().is_some_and(ident)
        })
        .count()
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// The parts of a SCIP `Index` KnowCode uses.
#[derive(Debug, Default)]
pub struct Index {
    pub tool: String,
    pub tool_version: String,
    /// A `file://` URI.
    pub project_root: String,
    pub documents: Vec<Document>,
}

#[derive(Debug, Default)]
pub struct Document {
    pub path: String,
    /// `PositionEncoding`: 1 UTF-8, 2 UTF-16, 3 UTF-32, 0 unspecified.
    pub encoding: u64,
    pub occurrences: Vec<Occurrence>,
    pub symbols: Vec<SymbolInfo>,
}

#[derive(Debug, Default)]
pub struct Occurrence {
    /// `[line, start, end]` or `[line, start, end_line, end]`, 0-based.
    pub range: Vec<i32>,
    pub symbol: String,
    pub roles: i32,
}

#[derive(Debug, Default)]
pub struct SymbolInfo {
    pub symbol: String,
    /// Symbols this one implements (`Relationship.is_implementation`).
    pub implements: Vec<String>,
}

/// Decodes a SCIP index (protobuf wire format), keeping the fields above and skipping the rest.
pub fn decode(buf: &[u8]) -> Result<Index> {
    let mut index = Index::default();
    let mut w = Wire::new(buf);
    while let Some((field, wire)) = w.key()? {
        match (field, wire) {
            (1, 2) => {
                let mut m = Wire::new(w.bytes()?);
                while let Some((f, t)) = m.key()? {
                    match (f, t) {
                        (2, 2) => {
                            let mut tool = Wire::new(m.bytes()?);
                            while let Some((f, t)) = tool.key()? {
                                match (f, t) {
                                    (1, 2) => index.tool = tool.string()?,
                                    (2, 2) => index.tool_version = tool.string()?,
                                    _ => tool.skip(t)?,
                                }
                            }
                        }
                        (3, 2) => index.project_root = m.string()?,
                        _ => m.skip(t)?,
                    }
                }
            }
            (2, 2) => index.documents.push(document(w.bytes()?)?),
            _ => w.skip(wire)?,
        }
    }
    Ok(index)
}

fn document(buf: &[u8]) -> Result<Document> {
    let mut d = Document::default();
    let mut w = Wire::new(buf);
    while let Some((field, wire)) = w.key()? {
        match (field, wire) {
            (1, 2) => d.path = w.string()?,
            (2, 2) => d.occurrences.push(occurrence(w.bytes()?)?),
            (3, 2) => d.symbols.push(symbol_info(w.bytes()?)?),
            (6, 0) => d.encoding = w.varint()?,
            _ => w.skip(wire)?,
        }
    }
    Ok(d)
}

fn occurrence(buf: &[u8]) -> Result<Occurrence> {
    let mut o = Occurrence::default();
    let mut w = Wire::new(buf);
    while let Some((field, wire)) = w.key()? {
        match (field, wire) {
            // `repeated int32`, packed or not.
            (1, 2) => {
                let mut p = Wire::new(w.bytes()?);
                while !p.done() {
                    o.range.push(p.varint()? as i32);
                }
            }
            (1, 0) => o.range.push(w.varint()? as i32),
            (2, 2) => o.symbol = w.string()?,
            (3, 0) => o.roles = w.varint()? as i32,
            _ => w.skip(wire)?,
        }
    }
    Ok(o)
}

fn symbol_info(buf: &[u8]) -> Result<SymbolInfo> {
    let mut s = SymbolInfo::default();
    let mut w = Wire::new(buf);
    while let Some((field, wire)) = w.key()? {
        match (field, wire) {
            (1, 2) => s.symbol = w.string()?,
            (4, 2) => {
                let (mut symbol, mut implementation) = (String::new(), false);
                let mut r = Wire::new(w.bytes()?);
                while let Some((f, t)) = r.key()? {
                    match (f, t) {
                        (1, 2) => symbol = r.string()?,
                        (3, 0) => implementation = r.varint()? != 0,
                        _ => r.skip(t)?,
                    }
                }
                if implementation && !symbol.is_empty() {
                    s.implements.push(symbol);
                }
            }
            _ => w.skip(wire)?,
        }
    }
    Ok(s)
}

/// A protobuf wire-format reader over one message.
struct Wire<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Wire<'a> {
    fn new(buf: &'a [u8]) -> Wire<'a> {
        Wire { buf, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let Some(&b) = self.buf.get(self.pos) else { bail!("truncated SCIP index") };
            self.pos += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        bail!("malformed varint in SCIP index")
    }

    /// The next field number and wire type, or `None` at the end of the message.
    fn key(&mut self) -> Result<Option<(u64, u8)>> {
        if self.done() {
            return Ok(None);
        }
        let k = self.varint()?;
        Ok(Some((k >> 3, (k & 7) as u8)))
    }

    fn advance(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len());
        let Some(end) = end else { bail!("truncated SCIP index") };
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.varint()?;
        self.advance(usize::try_from(n)?)
    }

    fn string(&mut self) -> Result<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }

    fn skip(&mut self, wire: u8) -> Result<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.advance(8)?;
            }
            2 => {
                self.bytes()?;
            }
            5 => {
                self.advance(4)?;
            }
            _ => bail!("unsupported protobuf wire type {wire} in SCIP index"),
        }
        Ok(())
    }
}

/// Writes SCIP's wire format, for tests and fixtures.
#[doc(hidden)]
pub mod encode {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn bytes(out: &mut Vec<u8>, field: u64, b: &[u8]) {
        varint(out, field << 3 | 2);
        varint(out, b.len() as u64);
        out.extend_from_slice(b);
    }

    fn int(out: &mut Vec<u8>, field: u64, v: u64) {
        varint(out, field << 3);
        varint(out, v);
    }

    /// An occurrence: `range` 0-based, `roles` a SymbolRole bit set (1 = definition).
    pub fn occurrence(range: &[i32], symbol: &str, roles: i32) -> Vec<u8> {
        let mut o = Vec::new();
        let mut packed = Vec::new();
        for &r in range {
            varint(&mut packed, r as i64 as u64);
        }
        bytes(&mut o, 1, &packed);
        bytes(&mut o, 2, symbol.as_bytes());
        if roles != 0 {
            int(&mut o, 3, roles as u64);
        }
        o
    }

    /// A SymbolInformation with `is_implementation` relationships to `implements`.
    pub fn symbol(symbol: &str, implements: &[&str]) -> Vec<u8> {
        let mut s = Vec::new();
        bytes(&mut s, 1, symbol.as_bytes());
        for i in implements {
            let mut r = Vec::new();
            bytes(&mut r, 1, i.as_bytes());
            int(&mut r, 3, 1);
            bytes(&mut s, 4, &r);
        }
        s
    }

    pub fn document(path: &str, occurrences: &[Vec<u8>], symbols: &[Vec<u8>]) -> Vec<u8> {
        let mut d = Vec::new();
        bytes(&mut d, 1, path.as_bytes());
        for o in occurrences {
            bytes(&mut d, 2, o);
        }
        for s in symbols {
            bytes(&mut d, 3, s);
        }
        bytes(&mut d, 4, b"CSharp");
        d
    }

    /// An index of `documents` written by a tool named `tool`, rooted at `project_root` (a `file://` URI).
    pub fn index(tool: &str, project_root: &str, documents: &[Vec<u8>]) -> Vec<u8> {
        let mut info = Vec::new();
        bytes(&mut info, 1, tool.as_bytes());
        bytes(&mut info, 2, b"1.0");
        let mut meta = Vec::new();
        bytes(&mut meta, 2, &info);
        bytes(&mut meta, 3, project_root.as_bytes());
        let mut out = Vec::new();
        bytes(&mut out, 1, &meta);
        for d in documents {
            bytes(&mut out, 2, d);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_what_it_needs_and_skips_the_rest() {
        let doc = encode::document(
            "src/a.cs",
            &[encode::occurrence(&[3, 4, 7], "scip-dotnet nuget App 1.0 A#Run().", 1), encode::occurrence(&[9, 8, 2, 9, 11], "x", 0)],
            &[encode::symbol("scip-dotnet nuget App 1.0 A#Run().", &["scip-dotnet nuget App 1.0 IA#Run()."])],
        );
        let index = decode(&encode::index("scip-dotnet", "file:///w/app", &[doc])).unwrap();
        assert_eq!((index.tool.as_str(), index.tool_version.as_str(), index.project_root.as_str()), ("scip-dotnet", "1.0", "file:///w/app"));
        let d = &index.documents[0];
        assert_eq!(d.path, "src/a.cs");
        assert_eq!(d.occurrences[0].range, [3, 4, 7]);
        assert_eq!(d.occurrences[0].roles, 1);
        assert_eq!(d.occurrences[1].range, [9, 8, 2, 9, 11]);
        assert_eq!(d.symbols[0].implements, ["scip-dotnet nuget App 1.0 IA#Run()."]);
        assert!(decode(&[0x12, 0x05, 0x01]).is_err(), "a truncated message is an error");
    }

    #[test]
    fn names_come_from_the_source_in_the_documents_encoding() {
        let lines = ["  var é = Foo.Bar();", "  @class.Run();"];
        // UTF-16 and UTF-32 count `é` as one unit, UTF-8 as two bytes.
        assert_eq!(name_at(&lines, &[0, 10, 13], 2).as_deref(), Some("Foo"));
        assert_eq!(name_at(&lines, &[0, 14, 17], 3).as_deref(), Some("Bar"));
        assert_eq!(name_at(&lines, &[0, 11, 14], 1).as_deref(), Some("Foo"));
        assert_eq!(name_at(&lines, &[1, 2, 8], 0).as_deref(), Some("class"));
        assert_eq!(name_at(&lines, &[0, 2, 1, 3], 0), None, "multi-line ranges are skipped");
        assert_eq!(name_at(&lines, &[5, 0, 1], 0), None);
        assert_eq!(word_count("x.Status == Status.Open && StatusCode", "Status"), 2);
        assert_eq!(word_count("this.#status = status", "status"), 1);
    }

    #[test]
    fn symbols_split_into_package_and_descriptors() {
        assert_eq!(package("scip-dotnet nuget App 1.0 A#Run()."), "scip-dotnet nuget App 1.0");
        assert_eq!(package("scip-typescript npm app 0.0.0 src/`a.ts`/A#"), "scip-typescript npm app 0.0.0");
        assert!(is_local("local 4"));
        assert_eq!(percent_decode("/w/my%20app"), "/w/my app");
    }
}
