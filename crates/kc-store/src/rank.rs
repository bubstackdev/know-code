//! PageRank over the file reference graph, for ordering long answers (after Aider's repo map).
//!
//! The graph has an edge from a file to each file its references resolve into (`exact` or `unique_name`). An edge's
//! weight sums, over the names it uses, `sqrt(references)`, scaled by the share of those references that resolved
//! `exact` (an `exact` reference counts 1, a `unique_name` one 0.5) and cut to a tenth for a private name (a leading
//! `_`, or a `private` declaration) and again for a name defined in more than five files: a call to a widely
//! defined `Save` says little about which file it lands in. The graph comes from the same resolution pass as
//! [`query::file_edges`] and is cached with it per index generation; the global rank is cached too. A personalized
//! rank, seeded by the files a query is about (the changed files of an impact, a symbol's own file), is computed
//! on demand from the cached graph: about a millisecond per thousand files.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet, btree_map};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::query::{self, FileEdge};

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 40;
/// A name defined in more files than this says little about where a reference to it lands.
const MANY_DEFINITIONS: u32 = 5;
const DOWN_WEIGHT: f64 = 0.1;
/// What a `unique_name` reference is worth next to an `exact` one.
const UNIQUE_NAME_WEIGHT: f64 = 0.5;

/// Bumped when the graph or the weights change, so cached ranks are recomputed.
pub const RANK_VERSION: u32 = 1;

/// One name a file uses from another: how many references to it resolved `exact` and `unique_name`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NameUse {
    pub name: String,
    pub exact: u32,
    pub unique: u32,
    /// A private name: `_name`, or a target declared `private`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// Defined in more than [`MANY_DEFINITIONS`] files.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub common: bool,
}

/// Every file-to-file dependency with its per-name reference counts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RefGraph {
    /// Indexed source files (manifests left out), the graph's nodes.
    pub files: Vec<String>,
    /// `(src, dst, names)`, sorted by `(src, dst)`, names sorted.
    pub edges: Vec<(String, String, Vec<NameUse>)>,
}

impl RefGraph {
    /// The edges as [`query::file_edges`] gives them: the distinct names `src` uses from `dst`.
    pub fn file_edges(&self) -> Vec<FileEdge> {
        self.edges
            .iter()
            .map(|(src, dst, names)| FileEdge {
                src: src.clone(),
                dst: dst.clone(),
                weight: names.len() as u32,
                names: names.iter().map(|n| n.name.clone()).collect(),
            })
            .collect()
    }

    /// The graph PageRank walks: each edge's weight is, per name, `sqrt(references)` times the confidence share,
    /// down-weighted for private and widely defined names.
    pub fn weighted(&self) -> RankGraph {
        let index: HashMap<&str, u32> = self.files.iter().enumerate().map(|(i, f)| (f.as_str(), i as u32)).collect();
        let edges = self
            .edges
            .iter()
            .filter_map(|(src, dst, names)| {
                let w: f64 = names.iter().map(name_weight).sum();
                (w > 0.0).then_some((*index.get(src.as_str())?, *index.get(dst.as_str())?, w as f32))
            })
            .collect();
        RankGraph { files: self.files.clone(), edges }
    }
}

/// The weighted file graph, compact: files, and edges `(src, dst, weight)` by file index. Small enough to read on
/// every query that orders a long list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RankGraph {
    pub files: Vec<String>,
    pub edges: Vec<(u32, u32, f32)>,
}

fn name_weight(n: &NameUse) -> f64 {
    let refs = (n.exact + n.unique) as f64;
    if refs == 0.0 {
        return 0.0;
    }
    let confidence = (n.exact as f64 + UNIQUE_NAME_WEIGHT * n.unique as f64) / refs;
    let mut w = refs.sqrt() * confidence;
    if n.private {
        w *= DOWN_WEIGHT;
    }
    if n.common {
        w *= DOWN_WEIGHT;
    }
    w
}

/// How many threads build the reference graph, and what each runs first: a background build lowers its threads'
/// priority. One thread unless the caller says otherwise (see [`with_workers`], [`set_default_threads`]).
#[derive(Debug, Clone, Copy)]
pub struct Workers {
    pub threads: usize,
    pub init: Option<fn()>,
}

static DEFAULT_THREADS: OnceLock<fn() -> usize> = OnceLock::new();

thread_local! {
    static WORKERS: Cell<Option<Workers>> = const { Cell::new(None) };
}

/// How many threads a reference graph built on a thread without [`with_workers`] uses: a process sets it from its
/// budget (`kc_index::machine::plan`), asked each time a graph is built. The first setting holds.
pub fn set_default_threads(threads: fn() -> usize) {
    let _ = DEFAULT_THREADS.set(threads);
}

/// Runs `f` with reference graphs it builds on this thread using `workers`.
pub fn with_workers<T>(workers: Workers, f: impl FnOnce() -> T) -> T {
    let before = WORKERS.with(|w| w.replace(Some(workers)));
    let out = f();
    WORKERS.with(|w| w.set(before));
    out
}

/// The workers a reference graph built on this thread now uses.
pub fn workers() -> Workers {
    WORKERS.with(Cell::get).unwrap_or_else(|| Workers { threads: DEFAULT_THREADS.get().map_or(1, |f| f()).max(1), init: None })
}

/// References resolved at a time per name: a name's references are many only in generated code (an Entity Framework
/// model's `Property`, `HasColumnType`), and each resolved reference carries its targets.
const CHUNK: usize = 2048;

/// The reference graph: every reference resolved, as for [`query::file_edges`]. Expensive; cache it per index
/// generation (see `Store::ref_graph`).
///
/// Names resolve independently of each other, so [`workers`] threads take them in turn, each on a connection of its
/// own reading the index at the same generation as `conn` (a thread whose connection reads another one takes no
/// names), and their edges are summed. The most used names go first, so that no thread is left with one at the end.
pub fn ref_graph(conn: &Connection) -> Result<RefGraph> {
    ref_graph_chunked(conn, CHUNK)
}

/// [`ref_graph`], resolving a name's references `chunk` at a time.
pub fn ref_graph_chunked(conn: &Connection, chunk: usize) -> Result<RefGraph> {
    let files: Vec<String> = conn
        .prepare("SELECT path FROM files WHERE lang NOT IN ('swiftpm', 'xcodegen', 'cargo', 'hostconfig') ORDER BY path")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // `symbols.name` groups without case; which spelling a group shows depends on row order, so compare lower-cased.
    let common: HashSet<String> = conn
        .prepare("SELECT lower(name) FROM symbols GROUP BY name HAVING COUNT(DISTINCT file_id) > ?1")?
        .query_map([MANY_DEFINITIONS], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut names: Vec<(String, i64)> = conn
        .prepare("SELECT name, COUNT(*) FROM refs GROUP BY name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let names: Vec<String> = names.into_iter().map(|(n, _)| n).collect();
    let generation = generation(conn)?;
    let path = conn.path().unwrap_or_default().to_string();
    let w = workers();
    let threads = if path.is_empty() { 1 } else { w.threads.clamp(1, names.len().max(1)) };
    // Kept per index generation: computed here once, not by every thread at once.
    query::factory_constructions(conn)?;
    let next = AtomicUsize::new(0);
    let parts = std::thread::scope(|scope| -> Result<Vec<Partial>> {
        let helpers: Vec<_> = (1..threads)
            .map(|_| {
                let (names, common, next, path, generation) = (&names, &common, &next, &path, &generation);
                scope.spawn(move || -> Result<Partial> {
                    if let Some(init) = w.init {
                        init();
                    }
                    let mut part = Partial::default();
                    // A thread that can't read the index at this generation takes no names: the others resolve them.
                    let Ok(conn) = open_at(path, generation) else { return Ok(part) };
                    part.resolve(&conn, names, next, common, chunk)?;
                    Ok(part)
                })
            })
            .collect();
        let mut own = Partial::default();
        let mine = own.resolve(conn, &names, &next, &common, chunk);
        let mut parts = vec![own];
        for h in helpers {
            parts.push(h.join().map_err(|_| anyhow::anyhow!("a reference graph thread panicked"))??);
        }
        mine?;
        Ok(parts)
    })?;
    let mut edges: BTreeMap<(String, String), BTreeMap<String, NameUse>> = BTreeMap::new();
    for part in parts {
        for (key, uses) in part.edges {
            let into = edges.entry(key).or_default();
            for (name, u) in uses {
                match into.entry(name) {
                    btree_map::Entry::Vacant(e) => {
                        e.insert(u);
                    }
                    btree_map::Entry::Occupied(mut e) => {
                        let n = e.get_mut();
                        n.exact += u.exact;
                        n.unique += u.unique;
                        n.private |= u.private;
                    }
                }
            }
        }
    }
    let edges = edges.into_iter().map(|((s, d), names)| (s, d, names.into_values().collect())).collect();
    Ok(RefGraph { files, edges })
}

/// The reference graphs being built in this process, by index file and generation: a second thread needing one waits
/// for the first's rather than building it again (and holding it twice). A server builds them in the background after
/// indexing; a query arriving meanwhile waits for that one.
static BUILDING: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static BUILT: Condvar = Condvar::new();

/// This thread's turn to build the reference graph of one index at one generation; the next waiting thread's when
/// dropped.
pub(crate) struct Turn(Option<(String, String)>);

impl Turn {
    /// Waits until no other thread of this process builds the graph of the index at `path` at `generation`, then
    /// takes the turn. An index in memory (no path) has no turns.
    pub(crate) fn take(path: &str, generation: &str) -> Turn {
        if path.is_empty() {
            return Turn(None);
        }
        let key = (path.to_string(), generation.to_string());
        let mut building = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
        while building.contains(&key) {
            building = BUILT.wait(building).unwrap_or_else(|e| e.into_inner());
        }
        building.push(key.clone());
        Turn(Some(key))
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(key) = self.0.take() {
            BUILDING.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| *k != key);
            BUILT.notify_all();
        }
    }
}

/// Waits until no thread of this process builds the graph of the index at `path` at `generation`, or until
/// `deadline`. Whether none does.
pub(crate) fn wait_built(path: &str, generation: &str, deadline: Instant) -> bool {
    let key = (path.to_string(), generation.to_string());
    let mut building = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
    while building.contains(&key) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        building = BUILT.wait_timeout(building, left).unwrap_or_else(|e| e.into_inner()).0;
    }
    true
}

/// What [`crate::Store::file_rank_at`] gives for the generation of the graph it ranked when that graph was being built
/// in the background and not ready by the call's freshness deadline: nothing was ranked (see
/// [`crate::answer::stale_derived`]).
pub const PENDING: &str = "pending";

/// The index generation a connection reads (see `Store::generation`).
fn generation(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("SELECT value FROM meta WHERE key = 'tree_gen'", [], |r| r.get(0)).optional()?.unwrap_or_default())
}

/// A read-only connection to the index at `path`, in a snapshot, when that reads `generation`.
fn open_at(path: &str, generation: &str) -> Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA cache_size = -2048; BEGIN DEFERRED")?;
    anyhow::ensure!(self::generation(&conn)? == generation, "the index moved on");
    Ok(conn)
}

/// A name used several times on one line from one symbol, as one kind of reference reaching one target: counted once.
type Site = (String, u32, Option<i64>, String, i64);

/// The edges one thread found, for the names it resolved.
#[derive(Default)]
struct Partial {
    edges: BTreeMap<(String, String), BTreeMap<String, NameUse>>,
    /// Per target symbol: its file, and whether it is declared `private`.
    target: HashMap<i64, (String, bool)>,
}

impl Partial {
    /// Resolves names from `names`, taking the next one from `next` until none is left. On an error, leaves none for
    /// the other threads either.
    fn resolve(&mut self, conn: &Connection, names: &[String], next: &AtomicUsize, common: &HashSet<String>, chunk: usize) -> Result<()> {
        let mut shared = query::Resolution::default();
        while let Some(name) = names.get(next.fetch_add(1, Ordering::Relaxed)) {
            // Weights count the lines using a name, as before references kept their columns.
            let mut counted: HashSet<Site> = HashSet::new();
            if let Err(e) = query::refs_in_chunks(conn, name, &mut shared, chunk, |hits| self.add(conn, name, hits, &mut counted, common)) {
                next.store(names.len(), Ordering::Relaxed);
                return Err(e);
            }
        }
        Ok(())
    }

    fn add(&mut self, conn: &Connection, name: &str, hits: Vec<query::RefHit>, counted: &mut HashSet<Site>, common: &HashSet<String>) -> Result<()> {
        let mut stmt = conn.prepare_cached("SELECT f.path, s.signature FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1")?;
        for h in hits {
            let exact = match h.confidence {
                "exact" => true,
                "unique_name" => false,
                _ => continue,
            };
            for &id in &h.target_ids {
                let (dst, private) = self.target.entry(id).or_insert_with(|| {
                    let (path, sig): (String, String) = stmt.query_row([id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap_or_default();
                    let private = sig.split_whitespace().any(|w| w == "private");
                    (path, private)
                });
                if dst.is_empty() || *dst == h.file || !counted.insert((h.file.clone(), h.line, h.from.as_ref().map(|f| f.id), h.kind.clone(), id)) {
                    continue;
                }
                let names = self.edges.entry((h.file.clone(), dst.clone())).or_default();
                let n = names.entry(name.to_string()).or_insert_with(|| NameUse {
                    name: name.to_string(),
                    exact: 0,
                    unique: 0,
                    private: false,
                    common: common.contains(&name.to_ascii_lowercase()),
                });
                if exact {
                    n.exact += 1;
                } else {
                    n.unique += 1;
                }
                n.private |= *private || name.starts_with('_');
            }
        }
        Ok(())
    }
}

/// Which way rank flows along a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// From a file to the files it uses (Aider's direction: what the seeds need).
    Uses,
    /// From a file to the files using it: seeded at a changed file, its dependents.
    UsedBy,
    /// Both ways.
    Both,
}

/// PageRank over `g`'s weighted edges, personalized toward `seeds` (uniform when none of them is a node), rank
/// flowing as `flow` says. Scores sum to 1.
pub fn pagerank(g: &RankGraph, seeds: &[&str], flow: Flow) -> HashMap<String, f64> {
    let n = g.files.len();
    if n == 0 {
        return HashMap::new();
    }
    let index: HashMap<&str, usize> = g.files.iter().enumerate().map(|(i, f)| (f.as_str(), i)).collect();
    let mut edges: Vec<(usize, usize, f64)> = Vec::new();
    for &(s, d, w) in &g.edges {
        let (a, b, w) = (s as usize, d as usize, w as f64);
        if a >= n || b >= n {
            continue;
        }
        if flow != Flow::UsedBy {
            edges.push((a, b, w));
        }
        if flow != Flow::Uses {
            edges.push((b, a, w));
        }
    }
    let seeded: Vec<usize> = seeds.iter().filter_map(|s| index.get(s).copied()).collect();
    let personal: Vec<f64> = if seeded.is_empty() {
        vec![1.0 / n as f64; n]
    } else {
        let mut p = vec![0.0; n];
        for &i in &seeded {
            p[i] += 1.0 / seeded.len() as f64;
        }
        p
    };
    let mut out_weight = vec![0.0; n];
    for (a, _, w) in &edges {
        out_weight[*a] += w;
    }
    let mut rank = personal.clone();
    for _ in 0..ITERATIONS {
        let dangling: f64 = rank.iter().zip(&out_weight).filter(|(_, w)| **w == 0.0).map(|(r, _)| r).sum();
        let mut next: Vec<f64> = personal.iter().map(|p| (1.0 - DAMPING + DAMPING * dangling) * p).collect();
        for (a, b, w) in &edges {
            next[*b] += DAMPING * rank[*a] * w / out_weight[*a];
        }
        rank = next;
    }
    g.files.iter().cloned().zip(rank).collect()
}

/// Ranks for ordering: a file's score, 0 for a file the graph doesn't know.
#[derive(Debug, Clone, Default)]
pub struct FileRank(pub HashMap<String, f64>);

impl FileRank {
    pub fn score(&self, file: &str) -> f64 {
        self.0.get(file).copied().unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(name, exact, unique)` references from one file to another.
    type Uses<'a> = &'a [(&'a str, u32, u32)];

    fn graph(edges: &[(&str, &str, Uses)]) -> RefGraph {
        let mut files: Vec<String> = edges.iter().flat_map(|(s, d, _)| [s.to_string(), d.to_string()]).collect();
        files.sort();
        files.dedup();
        RefGraph {
            files,
            edges: edges
                .iter()
                .map(|(s, d, names)| {
                    let names = names
                        .iter()
                        .map(|(n, e, u)| NameUse { name: n.to_string(), exact: *e, unique: *u, private: n.starts_with('_'), common: false })
                        .collect();
                    (s.to_string(), d.to_string(), names)
                })
                .collect(),
        }
    }

    #[test]
    fn weights_grow_with_the_square_root_of_references_and_shrink_for_private_names() {
        let g = graph(&[("a", "b", &[("Save", 4, 0)]), ("a", "c", &[("_cache", 4, 0)]), ("a", "d", &[("Load", 0, 4)])]);
        let r = g.weighted();
        let w: HashMap<&str, f64> = r.edges.iter().map(|(_, d, w)| (r.files[*d as usize].as_str(), *w as f64)).collect();
        assert!((w["b"] - 2.0).abs() < 1e-6);
        assert!((w["c"] - 0.2).abs() < 1e-6);
        assert!((w["d"] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_seed_pulls_rank_toward_its_neighbours() {
        // b and d are used once each; seeded at a (which uses b), b outranks d. Reversed, seeded at b, a (b's user)
        // outranks c (d's user).
        let g = graph(&[("a", "b", &[("B", 1, 0)]), ("c", "d", &[("D", 1, 0)])]);
        let g = g.weighted();
        let global = pagerank(&g, &[], Flow::Uses);
        assert!((global["b"] - global["d"]).abs() < 1e-9);
        let seeded = pagerank(&g, &["a"], Flow::Uses);
        assert!(seeded["b"] > seeded["d"]);
        let users = pagerank(&g, &["b"], Flow::UsedBy);
        assert!(users["a"] > users["c"]);
        let total: f64 = users.values().sum();
        assert!((total - 1.0).abs() < 1e-6);
    }
}
