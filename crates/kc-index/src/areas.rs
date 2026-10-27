//! Functional areas: a repo's files clustered into the parts an engineer would name, after GitNexus and
//! codebase-memory-mcp's community detection, with no model in the loop.
//!
//! The graph's nodes are the indexed files, plus one node per directory. Edges, undirected, add three signals:
//!
//! - resolved references, weighted as PageRank weighs them ([`kc_store::rank`]: `sqrt(references)` per name,
//!   `unique_name` at half an `exact` reference, private and widely defined names at a tenth), both directions summed,
//!   times [`REF_WEIGHT`];
//! - co-change from history (the hotspots walk: commits of at most 50 files over the 365 days before HEAD, automated
//!   commits and pairs only ever created together left out), pairs sharing at least 2 commits, weighted by
//!   [`COCHANGE_WEIGHT`] times the cosine of their commit sets (`shared / sqrt(commits_a · commits_b)`);
//! - the directory tree: each file tied to its directory's node by [`DIR_WEIGHT`], each directory to its parent's by
//!   [`DIR_PARENT_WEIGHT`] times the square root of the files under it.
//!
//! The weights were set on a few multi-project solutions against their own project and module structure and checked
//! on others: references alone cut across projects (a layered
//! solution's controllers, services and repositories of one feature reference each other more than their
//! neighbours), so they weigh least.
//!
//! Clustering is Louvain modularity optimisation with a connectivity refinement (each community split into its
//! connected parts before aggregation, the guarantee Leiden adds), deterministic for a seed: nodes are visited in a
//! seeded order and ties go to the lowest community. Eight runs with different orders vote, and a last run clusters
//! the edges most of them agree on (see [`cluster`]). Communities under [`MIN_AREA`] files join the community they
//! connect to most; files with no edge, or in a part that small tied to nothing else, are left ungrouped.
//!
//! Each area gets a cohesion score (the share of the reference and co-change weight touching it that stays inside), a name from its dominant directory
//! and, when several areas share a directory, its most distinctive file-name tokens; its key files (PageRank over
//! its own references), entry points, the files other areas use most, and the areas it depends on. The answer is
//! cached per index generation and HEAD.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use kc_store::{Store, query, rank};
use serde::{Deserialize, Serialize};

/// Bumped when the graph, the algorithm or the answer's shape changes, so cached areas are recomputed.
pub const VERSION: u32 = 3;
/// Modularity resolution: higher splits finer.
const RESOLUTION: f64 = 1.0;
const SEED: u64 = 0x6b63_6172_6561_7331;
/// Communities smaller than this join their best-connected neighbour.
pub const MIN_AREA: usize = 3;
/// The weight of a co-change pair that always changes together.
pub const COCHANGE_WEIGHT: f64 = 2.0;
const COCHANGE_MIN_SHARED: u32 = 2;
/// What a reference edge's PageRank weight counts for.
pub const REF_WEIGHT: f64 = 0.25;
/// A file's tie to its directory.
pub const DIR_WEIGHT: f64 = 1.0;
/// A directory's tie to its parent, per square root of the files under it.
pub const DIR_PARENT_WEIGHT: f64 = 1.0;
/// Louvain runs behind each consensus.
const RUNS: usize = 8;
/// Levels of aggregation at most.
const MAX_LEVELS: usize = 32;

/// Which signals build the graph (all by default; the others are for ablation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signals {
    pub refs: bool,
    pub cochange: bool,
    pub dirs: bool,
}

impl Default for Signals {
    fn default() -> Signals {
        Signals { refs: true, cochange: true, dirs: true }
    }
}

/// A repo's functional areas.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Areas {
    /// Largest first.
    pub areas: Vec<Area>,
    /// Files in no area: no reference, co-change or directory neighbour, or only a part of fewer than [`MIN_AREA`]
    /// files tied to nothing else.
    pub ungrouped: Vec<String>,
    /// Files clustered (the graph's nodes).
    pub files: usize,
    /// Modularity of the partition over the combined graph (0–1; above 0.3 is a clear structure).
    pub modularity: f64,
    /// Co-change pairs used: 0 means history added nothing (no git history, or a shallow one).
    pub cochange_pairs: usize,
    pub signals: Signals,
}

/// One functional area.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Area {
    /// 1-based, largest area first.
    pub id: u32,
    /// Unique within the repo: the dominant directory's last two components, with a distinctive token when another
    /// area shares the directory.
    pub name: String,
    /// The deepest directory holding more than half the area's code, by lines (`""` for the repo root).
    pub dir: String,
    /// Member files, sorted.
    pub files: Vec<String>,
    pub lines: u64,
    /// Reference and co-change weight inside the area over the weight of every such edge touching it, 0–1 (0 for
    /// an area held together by its directory alone).
    pub cohesion: f64,
    /// The most distinctive file-name words (TF-IDF across areas).
    pub tokens: Vec<String>,
    /// The most central files: PageRank over the references inside the area, among files the area references.
    pub key_files: Vec<String>,
    /// Entry points (`Main`, `Program`, `@main`, …) in the area.
    pub entry_points: Vec<EntryPoint>,
    /// The files other areas reference most.
    pub exposed: Vec<String>,
    /// Areas this one's references resolve into, by file-to-file dependencies, most first.
    pub depends_on: Vec<AreaDep>,
    /// How many there are of each list above, when more than are listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_files_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_points_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposed_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on_total: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryPoint {
    pub symbol: String,
    pub file: String,
    pub line: u32,
}

/// A dependency of one area on another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaDep {
    pub area: u32,
    pub name: String,
    /// File-to-file dependencies from the area into the other.
    pub file_edges: u32,
    /// Their reference weight.
    pub weight: f64,
}

impl Areas {
    /// The area holding `file`.
    pub fn of(&self, file: &str) -> Option<&Area> {
        self.areas.iter().find(|a| a.files.binary_search_by(|f| f.as_str().cmp(file)).is_ok())
    }

    /// The area named `query`: its id, its name or its directory (case-insensitive), else the only one whose name
    /// contains it.
    pub fn find(&self, query: &str) -> Option<&Area> {
        let q = query.trim().to_lowercase();
        if let Ok(id) = q.trim_start_matches('#').parse::<u32>() {
            return self.areas.iter().find(|a| a.id == id);
        }
        if let Some(a) = self.areas.iter().find(|a| a.name.to_lowercase() == q || a.dir.to_lowercase() == q) {
            return Some(a);
        }
        let mut hits = self.areas.iter().filter(|a| a.name.to_lowercase().contains(&q));
        match (hits.next(), hits.next()) {
            (Some(a), None) => Some(a),
            _ => None,
        }
    }
}

/// The repo's areas, cached per index generation and HEAD. Computes the reference graph and walks the history when
/// they aren't cached (as `overview` and `hotspots` do).
pub fn areas(root: &Path, store: &Store) -> Result<Areas> {
    store.derived("areas", &generation(root, store)?, || compute(root, store, Signals::default()))
}

fn generation(root: &Path, store: &Store) -> Result<String> {
    let head = crate::git::GitDir::find(root).and_then(|g| g.head_commit()).unwrap_or_default();
    Ok(format!("{VERSION}|{head}|{}|{}", store.generation()?, crate::vitals::history_inputs(root)))
}

/// Whether already-computed areas need warming after an edit or history change. An absent cache needs no work.
pub fn needs_refresh(root: &Path, store: &Store) -> Result<bool> {
    match store.derived_gen("areas")? {
        Some(g) => Ok(g != generation(root, store)?),
        None => Ok(false),
    }
}

/// Areas for a query that shouldn't pay for them on every edit (`context`): the cached ones, from whichever
/// generation computed them last (a few edits move few files; `areas` and `overview` bring them up to date), else
/// computed now.
pub fn cached(root: &Path, store: &Store) -> Result<Areas> {
    Ok(cached_at(root, store)?.0)
}

/// [`cached`], and the index generation it was computed at when that is an earlier one than the index's (see
/// [`Store::generation`]): an answer drawing on it says so.
pub fn cached_at(root: &Path, store: &Store) -> Result<(Areas, Option<String>)> {
    match store.derived_latest::<Areas>("areas")? {
        Some((g, a)) if g.starts_with(&format!("{VERSION}|")) => {
            let now = generation(root, store)?;
            // The key's parts: the version, HEAD, the index generation (`tree_gen|r<resolver>`), the history inputs.
            let index_gen = |k: &str| {
                let parts: Vec<&str> = k.split('|').collect();
                parts.get(2..4).map(|p| p.join("|"))
            };
            Ok((a, (g != now).then(|| index_gen(&g).unwrap_or_else(|| g.clone()))))
        }
        _ => Ok((areas(root, store)?, None)),
    }
}

/// Clusters the repo's files with the given signals (uncached; [`areas`] caches the default).
pub fn compute(root: &Path, store: &Store, signals: Signals) -> Result<Areas> {
    compute_seeded(root, store, signals, SEED)
}

/// [`compute`] with another seed, to measure how much the answer depends on visiting order.
pub fn compute_seeded(root: &Path, store: &Store, signals: Signals, seed: u64) -> Result<Areas> {
    let graph = store.ref_graph()?;
    let files = graph.files.clone();
    let index: HashMap<&str, u32> = files.iter().enumerate().map(|(i, f)| (f.as_str(), i as u32)).collect();
    let weighted = graph.weighted();
    let mut edges: Vec<(u32, u32, f64)> = Vec::new();
    if signals.refs {
        edges.extend(weighted.edges.iter().map(|&(a, b, w)| (a, b, REF_WEIGHT * w as f64)));
    }
    let mut cochange_pairs = 0;
    if signals.cochange {
        let set: HashSet<&str> = files.iter().map(String::as_str).collect();
        for p in crate::vitals::co_change_pairs(root, store, &set, COCHANGE_MIN_SHARED)? {
            let (Some(&a), Some(&b)) = (index.get(p.a.as_str()), index.get(p.b.as_str())) else { continue };
            let denom = (p.a_commits.max(1) as f64 * p.b_commits.max(1) as f64).sqrt();
            edges.push((a, b, COCHANGE_WEIGHT * (p.shared as f64 / denom).min(1.0)));
            cochange_pairs += 1;
        }
    }
    // File-to-file coupling, for cohesion; the directory tree is a prior, not coupling.
    let coupling = edges.len();
    let nodes = if signals.dirs { dir_tree(&files, &mut edges) } else { files.len() };
    let mut comm = cluster(nodes, &edges, RESOLUTION, seed);
    let modularity = Graph::new(nodes, &edges).modularity(&comm, RESOLUTION);
    comm.truncate(files.len());

    let mut by_comm: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut ungrouped = Vec::new();
    for (i, &c) in comm.iter().enumerate() {
        if c == UNGROUPED {
            ungrouped.push(files[i].clone());
        } else {
            by_comm.entry(c).or_default().push(i as u32);
        }
    }
    let mut groups: Vec<Vec<u32>> = by_comm.into_values().collect();
    groups.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| files[a[0] as usize].cmp(&files[b[0] as usize])));
    let mut area_of = vec![u32::MAX; files.len()];
    for (k, members) in groups.iter().enumerate() {
        for &m in members {
            area_of[m as usize] = k as u32;
        }
    }

    let conn = store.conn();
    let lines: HashMap<String, u64> = conn
        .prepare("SELECT path, lines FROM files")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?
        .collect::<rusqlite::Result<_>>()?;
    let (entry, _) = query::tagged(conn, "entrypoint", usize::MAX)?;

    // Reference and co-change weight inside and touching each area.
    let mut inside = vec![0.0; groups.len()];
    let mut touching = vec![0.0; groups.len()];
    for &(a, b, w) in &edges[..coupling] {
        let (ka, kb) = (area_of[a as usize], area_of[b as usize]);
        if ka == u32::MAX || kb == u32::MAX || a == b {
            continue;
        }
        if ka == kb {
            inside[ka as usize] += w;
            touching[ka as usize] += w;
        } else {
            touching[ka as usize] += w;
            touching[kb as usize] += w;
        }
    }
    // Directed reference weight between areas, and into each file from other areas.
    let mut deps: Vec<BTreeMap<u32, (u32, f64)>> = vec![BTreeMap::new(); groups.len()];
    let mut from_outside: HashMap<u32, f64> = HashMap::new();
    for &(a, b, w) in &weighted.edges {
        let (ka, kb) = (area_of[a as usize], area_of[b as usize]);
        if ka == u32::MAX || kb == u32::MAX || ka == kb {
            continue;
        }
        let d = deps[ka as usize].entry(kb).or_default();
        d.0 += 1;
        d.1 += w as f64;
        *from_outside.entry(b).or_default() += w as f64;
    }

    let tokens = distinctive_tokens(&groups, &files);
    let mut areas: Vec<Area> = Vec::new();
    for (k, members) in groups.iter().enumerate() {
        let mut paths: Vec<String> = members.iter().map(|&m| files[m as usize].clone()).collect();
        paths.sort();
        // PageRank over the references inside the area.
        let local: HashMap<u32, u32> = members.iter().enumerate().map(|(i, &m)| (m, i as u32)).collect();
        let sub = rank::RankGraph {
            files: members.iter().map(|&m| files[m as usize].clone()).collect(),
            edges: weighted
                .edges
                .iter()
                .filter_map(|&(a, b, w)| Some((*local.get(&a)?, *local.get(&b)?, w)))
                .collect(),
        };
        let pr = rank::pagerank(&sub, &[], rank::Flow::Uses);
        // Only files something in the area uses: with no edge, rank says nothing.
        let used: HashSet<u32> = sub.edges.iter().map(|e| e.1).collect();
        let mut central: Vec<&String> = sub.files.iter().enumerate().filter(|(i, _)| used.contains(&(*i as u32))).map(|(_, f)| f).collect();
        central.sort_by(|a, b| pr[*b].total_cmp(&pr[*a]).then_with(|| a.cmp(b)));
        let mut eps: Vec<&query::SymbolRow> = entry.iter().filter(|s| paths.binary_search(&s.file).is_ok()).collect();
        eps.sort_by(|a, b| pr.get(&b.file).unwrap_or(&0.0).total_cmp(pr.get(&a.file).unwrap_or(&0.0)).then_with(|| a.file.cmp(&b.file)));
        let mut exposed: Vec<(u32, f64)> = members.iter().filter_map(|m| Some((*m, *from_outside.get(m)?))).collect();
        exposed.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| files[a.0 as usize].cmp(&files[b.0 as usize])));
        let mut depends: Vec<(u32, (u32, f64))> = deps[k].iter().map(|(a, d)| (*a, *d)).collect();
        depends.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(b.1.1.total_cmp(&a.1.1)).then(a.0.cmp(&b.0)));
        let over = |n: usize, shown: usize| (n > shown).then_some(n);
        areas.push(Area {
            key_files_total: over(central.len(), 5),
            entry_points_total: over(eps.len(), 3),
            exposed_total: over(exposed.len(), 3),
            depends_on_total: over(depends.len(), 5),
            id: k as u32 + 1,
            name: String::new(),
            dir: dominant_dir(&paths, &lines),
            lines: paths.iter().map(|p| lines.get(p).copied().unwrap_or(0)).sum(),
            cohesion: if touching[k] > 0.0 { round2(inside[k] / touching[k]) } else { 0.0 },
            tokens: tokens[k].clone(),
            key_files: central.into_iter().take(5).cloned().collect(),
            entry_points: eps.iter().take(3).map(|s| EntryPoint { symbol: s.qualified(), file: s.file.clone(), line: s.line }).collect(),
            exposed: exposed.iter().take(3).map(|(m, _)| files[*m as usize].clone()).collect(),
            depends_on: depends
                .iter()
                .take(5)
                .map(|(a, (n, w))| AreaDep { area: a + 1, name: String::new(), file_edges: *n, weight: round2(*w) })
                .collect(),
            files: paths,
        });
    }
    name_areas(&mut areas);
    let names: HashMap<u32, String> = areas.iter().map(|a| (a.id, a.name.clone())).collect();
    for a in &mut areas {
        for d in &mut a.depends_on {
            d.name = names.get(&d.area).cloned().unwrap_or_default();
        }
    }
    ungrouped.sort();
    Ok(Areas { areas, ungrouped, files: files.len(), modularity: round3(modularity), cochange_pairs, signals })
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Adds the directory tree to `edges`: a node per directory (numbered after the files, in path order), each file tied
/// to its directory by [`DIR_WEIGHT`] and each directory to its parent by [`DIR_PARENT_WEIGHT`]` · sqrt(files under
/// it)`. Returns the node count.
pub fn dir_tree(files: &[String], edges: &mut Vec<(u32, u32, f64)>) -> usize {
    let mut under: BTreeMap<&str, usize> = BTreeMap::new();
    for f in files {
        let mut d = parent(f);
        loop {
            *under.entry(d).or_default() += 1;
            if d.is_empty() {
                break;
            }
            d = parent(d);
        }
    }
    let id: HashMap<&str, u32> = under.keys().enumerate().map(|(i, d)| (*d, (files.len() + i) as u32)).collect();
    for (i, f) in files.iter().enumerate() {
        edges.push((i as u32, id[parent(f)], DIR_WEIGHT));
    }
    for (d, n) in &under {
        if !d.is_empty() {
            edges.push((id[d], id[parent(d)], DIR_PARENT_WEIGHT * (*n as f64).sqrt()));
        }
    }
    files.len() + under.len()
}

/// The deepest directory holding more than half of `files`' code: lines (one per file at least) of the source files
/// outside tests (not SQL or data), or of all when there are none. Weighed by lines, a service with its many one-screen request types sits
/// in the service's directory, not the types'.
fn dominant_dir(files: &[String], lines: &HashMap<String, u64>) -> String {
    // Code: source outside tests, not SQL dumps or data (translation JSON) whose size says nothing of the area.
    let code = |f: &str| {
        !kc_store::is_test_path(f)
            && kc_extract::Lang::from_path(f).is_some_and(|l| l.as_str() != "sql" && kc_extract::Lang::SOURCE.contains(&l.as_str()))
    };
    let any_code = files.iter().any(|f| code(f));
    let mut counts: HashMap<&str, u64> = HashMap::new();
    let mut total = 0;
    for f in files.iter().filter(|f| !any_code || code(f)) {
        let w = lines.get(f).copied().unwrap_or(0).max(1);
        total += w;
        let mut d = parent(f);
        loop {
            *counts.entry(d).or_default() += w;
            if d.is_empty() {
                break;
            }
            d = parent(d);
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| 2 * n > total)
        .max_by(|a, b| depth(a.0).cmp(&depth(b.0)).then(a.1.cmp(&b.1)).then_with(|| b.0.cmp(a.0)))
        .map(|(d, _)| d.to_string())
        .unwrap_or_default()
}

fn depth(dir: &str) -> usize {
    if dir.is_empty() { 0 } else { dir.split('/').count() }
}

/// File-name words that say nothing about what an area does.
const STOP_WORDS: &[&str] = &[
    "spec", "specs", "test", "tests", "index", "component", "components", "service", "services", "module", "modules",
    "model", "models", "controller", "controllers", "interface", "base", "impl", "helper", "helpers", "util", "utils",
    "extensions", "extension", "dto", "dtos", "the", "and", "for", "with", "from", "html", "scss", "css", "json",
    "yml", "yaml", "sql", "cfm", "cfc", "swift", "rs", "py", "cs", "ts", "tsx", "js", "main", "app", "src", "lib",
    "get", "set", "new", "abstract", "default", "type", "types", "config", "constants", "page", "view", "views",
    "assembly", "info", "properties", "jest", "package", "program", "startup", "readme", "aspx", "ascx", "designer",
];

/// Words of a file name (its stem split at camelCase, `_`, `-`, `.` and digits), lowercased, stop words left out.
pub fn name_tokens(path: &str) -> Vec<String> {
    let file = path.rsplit('/').next().unwrap_or(path);
    // The stem before the first dot: `Orders.aspx.designer.cs` and `order-list.component.ts` name what they are first.
    let file = file.split('.').next().unwrap_or(file);
    let mut out = Vec::new();
    let mut word = String::new();
    let mut prev_lower = false;
    let flush = |word: &mut String, out: &mut Vec<String>| {
        let w = std::mem::take(word).to_lowercase();
        if w.len() >= 3 && !STOP_WORDS.contains(&w.as_str()) && !out.contains(&w) {
            out.push(w);
        }
    };
    for c in file.chars() {
        if !c.is_alphabetic() {
            flush(&mut word, &mut out);
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            flush(&mut word, &mut out);
        }
        prev_lower = c.is_lowercase();
        word.push(c);
    }
    flush(&mut word, &mut out);
    out
}

/// Per group, up to three file-name words by TF-IDF across groups, each in at least two files (or the only file).
fn distinctive_tokens(groups: &[Vec<u32>], files: &[String]) -> Vec<Vec<String>> {
    let per_group: Vec<HashMap<String, usize>> = groups
        .iter()
        .map(|members| {
            let mut tf: HashMap<String, usize> = HashMap::new();
            for &m in members {
                for t in name_tokens(&files[m as usize]) {
                    *tf.entry(t).or_default() += 1;
                }
            }
            tf
        })
        .collect();
    let mut df: HashMap<&str, usize> = HashMap::new();
    for tf in &per_group {
        for t in tf.keys() {
            *df.entry(t.as_str()).or_default() += 1;
        }
    }
    let n = groups.len().max(1) as f64;
    per_group
        .iter()
        .zip(groups)
        .map(|(tf, members)| {
            let mut scored: Vec<(&String, f64)> = tf
                .iter()
                .filter(|(_, c)| **c >= 2 || members.len() == 1)
                .map(|(t, c)| (t, (*c as f64 / members.len() as f64) * (1.0 + (n / df[t.as_str()] as f64).ln())))
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            scored.into_iter().take(3).map(|(t, _)| t.clone()).collect()
        })
        .collect()
}

/// Directory names that say where code sits, not what it does: left out of names.
const GENERIC_DIRS: &[&str] = &["src", "source", "sources", "lib", "libs", "app", "apps", "main", "java", "packages", "projects"];

/// Names each area after its directory's last two components (generic ones like `src` and `lib` left out), adding its first distinctive word not already in the
/// name when another area has the same directory, and its id as a last resort.
fn name_areas(areas: &mut [Area]) {
    let base = |a: &Area| -> String {
        let parts: Vec<&str> = a.dir.split('/').filter(|p| !p.is_empty() && !GENERIC_DIRS.contains(&p.to_lowercase().as_str())).collect();
        if parts.is_empty() {
            return a.tokens.first().cloned().unwrap_or_else(|| "(root)".to_string());
        }
        parts[parts.len().saturating_sub(2)..].join("/")
    };
    let bases: Vec<String> = areas.iter().map(base).collect();
    let mut count: HashMap<&str, usize> = HashMap::new();
    for b in &bases {
        *count.entry(b.as_str()).or_default() += 1;
    }
    let mut names: Vec<String> = Vec::new();
    for (a, b) in areas.iter().zip(&bases) {
        let mut name = b.clone();
        if count[b.as_str()] > 1 {
            let low = name.to_lowercase();
            if let Some(t) = a.tokens.iter().find(|t| !low.contains(t.as_str())) {
                name = format!("{name} ({t})");
            }
        }
        names.push(name);
    }
    let mut seen: HashSet<String> = HashSet::new();
    for (a, mut name) in areas.iter_mut().zip(names) {
        if !seen.insert(name.clone()) {
            name = format!("{name} #{}", a.id);
            seen.insert(name.clone());
        }
        a.name = name;
    }
}

/// The community of a node left out of every area.
pub const UNGROUPED: u32 = u32::MAX;

/// Clusters `n` nodes joined by undirected weighted `edges` (duplicates add up), by consensus (after Lancichinetti
/// and Fortunato): [`RUNS`] Louvain runs with seeds drawn from `seed`, then one more over the edges whose ends at
/// least half the runs put together, each weighted by that share. One run's answer depends on the order it visits
/// nodes (another seed can move a quarter of a repo's files); the consensus far fewer, and a one-method edit a few
/// percent. Labels are dense from 0, numbered by each community's lowest node; nodes in no area get
/// [`UNGROUPED`] (see [`louvain`]).
pub fn cluster(n: usize, edges: &[(u32, u32, f64)], resolution: f64, seed: u64) -> Vec<u32> {
    let runs: Vec<Vec<u32>> =
        (0..RUNS as u64).map(|k| louvain(n, edges, resolution, seed.wrapping_add((k + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)))).collect();
    let consensus: Vec<(u32, u32, f64)> = edges
        .iter()
        .filter_map(|&(a, b, w)| {
            let (a_, b_) = (a as usize, b as usize);
            let together = runs.iter().filter(|r| r[a_] == r[b_] && r[a_] != UNGROUPED).count() as f64 / RUNS as f64;
            (together >= 0.5).then_some((a, b, w * together))
        })
        .collect();
    louvain(n, &consensus, resolution, seed)
}

/// One Louvain run: local moving and aggregation, each community split into its connected parts before aggregating,
/// nodes visited in an order seeded by `seed`. Communities under [`MIN_AREA`] nodes then join the community they
/// connect to most; nodes with no edge out of a community that small get [`UNGROUPED`].
fn louvain(n: usize, edges: &[(u32, u32, f64)], resolution: f64, seed: u64) -> Vec<u32> {
    let original = Graph::new(n, edges);
    let mut g = original.clone();
    // Each original node's node in the current (aggregated) graph.
    let mut membership: Vec<u32> = (0..n as u32).collect();
    let mut comm: Vec<u32> = (0..n as u32).collect();
    let mut rng = Rng(seed | 1);
    for _ in 0..MAX_LEVELS {
        let mut order: Vec<usize> = (0..g.n).collect();
        rng.shuffle(&mut order);
        g.local_moving(&mut comm, &order, resolution);
        let refined = g.components_within(&comm);
        let parts = refined.iter().copied().max().map_or(0, |m| m as usize + 1);
        if parts == g.n {
            break;
        }
        // Aggregate by the refined partition; each aggregate node starts in its members' community.
        let mut start = vec![0u32; parts];
        for (v, &r) in refined.iter().enumerate() {
            start[r as usize] = comm[v];
        }
        g = g.aggregate(&refined, parts);
        for m in &mut membership {
            *m = refined[*m as usize];
        }
        comm = relabel(&start);
    }
    let mut out: Vec<u32> = membership.iter().map(|&m| comm[m as usize]).collect();
    // A community whose parts the last level joined without an edge between them in the original graph: split it.
    out = original.components_within(&out);
    merge_small(&original, &mut out);
    let kept: Vec<u32> = out.iter().copied().filter(|&c| c != UNGROUPED).collect();
    let dense = relabel(&kept);
    let mut it = dense.into_iter();
    out.iter().map(|&c| if c == UNGROUPED { UNGROUPED } else { it.next().unwrap() }).collect()
}

/// Joins communities under [`MIN_AREA`] nodes to the community they connect to most (ties to the lowest label),
/// smallest first, until none can join any; what is left that small has no edge out and is [`UNGROUPED`].
fn merge_small(g: &Graph, comm: &mut [u32]) {
    let mut members: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (v, &c) in comm.iter().enumerate() {
        members.entry(c).or_default().push(v);
    }
    let mut stuck: HashSet<u32> = HashSet::new();
    while let Some(c) =
        members.iter().filter(|(c, m)| m.len() < MIN_AREA && !stuck.contains(*c)).min_by_key(|(c, m)| (m.len(), **c)).map(|(c, _)| *c)
    {
        let mut links: BTreeMap<u32, f64> = BTreeMap::new();
        for &v in &members[&c] {
            for &(u, w) in &g.adj[v] {
                let cu = comm[u as usize];
                if cu != c {
                    *links.entry(cu).or_default() += w;
                }
            }
        }
        // No edge out now means none later: merges elsewhere never add one.
        let Some((&to, _)) = links.iter().max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0))) else {
            stuck.insert(c);
            continue;
        };
        let moved = members.remove(&c).unwrap_or_default();
        for &v in &moved {
            comm[v] = to;
        }
        members.entry(to).or_default().extend(moved);
    }
    for c in stuck {
        for &v in &members[&c] {
            comm[v] = UNGROUPED;
        }
    }
}

/// Labels renumbered densely from 0 in order of first appearance.
fn relabel(labels: &[u32]) -> Vec<u32> {
    let mut map: HashMap<u32, u32> = HashMap::new();
    labels
        .iter()
        .map(|l| {
            let next = map.len() as u32;
            *map.entry(*l).or_insert(next)
        })
        .collect()
}

/// xorshift64*: a small deterministic generator for the visiting order.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

/// An undirected weighted graph: adjacency without self-loops, self-loop weight apart, and each node's degree
/// (`Σ` adjacent weights `+ 2 ·` self-loop).
#[derive(Clone)]
struct Graph {
    n: usize,
    adj: Vec<Vec<(u32, f64)>>,
    self_loop: Vec<f64>,
    degree: Vec<f64>,
    /// `Σ degree`, twice the total edge weight.
    two_m: f64,
}

impl Graph {
    fn new(n: usize, edges: &[(u32, u32, f64)]) -> Graph {
        let mut maps: Vec<BTreeMap<u32, f64>> = vec![BTreeMap::new(); n];
        let mut self_loop = vec![0.0; n];
        for &(a, b, w) in edges {
            if w <= 0.0 || a as usize >= n || b as usize >= n {
                continue;
            }
            if a == b {
                self_loop[a as usize] += w;
            } else {
                *maps[a as usize].entry(b).or_default() += w;
                *maps[b as usize].entry(a).or_default() += w;
            }
        }
        let adj: Vec<Vec<(u32, f64)>> = maps.into_iter().map(|m| m.into_iter().collect()).collect();
        let degree: Vec<f64> = adj.iter().zip(&self_loop).map(|(a, s)| a.iter().map(|(_, w)| w).sum::<f64>() + 2.0 * s).collect();
        let two_m = degree.iter().sum();
        Graph { n, adj, self_loop, degree, two_m }
    }

    /// Moves nodes, in `order`, to the neighbouring community with the best modularity gain until no move helps.
    fn local_moving(&self, comm: &mut [u32], order: &[usize], resolution: f64) {
        if self.two_m == 0.0 {
            return;
        }
        let mut tot = vec![0.0; self.n];
        for v in 0..self.n {
            tot[comm[v] as usize] += self.degree[v];
        }
        let mut to_comm = vec![0.0; self.n];
        let mut touched: Vec<u32> = Vec::new();
        for _ in 0..1000 {
            let mut moved = false;
            for &v in order {
                let cv = comm[v];
                let k = self.degree[v];
                for &(u, w) in &self.adj[v] {
                    let cu = comm[u as usize];
                    if to_comm[cu as usize] == 0.0 {
                        touched.push(cu);
                    }
                    to_comm[cu as usize] += w;
                }
                tot[cv as usize] -= k;
                let gain = |c: u32, w: f64| w - resolution * tot[c as usize] * k / self.two_m;
                // Staying wins ties; among the others, the lowest label.
                let (mut best, mut best_gain) = (cv, gain(cv, to_comm[cv as usize]));
                touched.sort_unstable();
                for &c in &touched {
                    let g = gain(c, to_comm[c as usize]);
                    if g > best_gain + 1e-12 {
                        best = c;
                        best_gain = g;
                    }
                }
                tot[best as usize] += k;
                if best != cv {
                    comm[v] = best;
                    moved = true;
                }
                for c in touched.drain(..) {
                    to_comm[c as usize] = 0.0;
                }
            }
            if !moved {
                break;
            }
        }
    }

    /// Each community split into its connected parts: dense labels, numbered by each part's lowest node.
    fn components_within(&self, comm: &[u32]) -> Vec<u32> {
        let mut out = vec![u32::MAX; self.n];
        let mut next = 0;
        let mut stack = Vec::new();
        for s in 0..self.n {
            if out[s] != u32::MAX {
                continue;
            }
            out[s] = next;
            stack.push(s);
            while let Some(v) = stack.pop() {
                for &(u, _) in &self.adj[v] {
                    let u = u as usize;
                    if out[u] == u32::MAX && comm[u] == comm[v] {
                        out[u] = next;
                        stack.push(u);
                    }
                }
            }
            next += 1;
        }
        out
    }

    /// The graph of `parts` nodes, one per label of `part`, edges summed, inner edges as self-loops.
    fn aggregate(&self, part: &[u32], parts: usize) -> Graph {
        let mut edges: Vec<(u32, u32, f64)> = Vec::new();
        for v in 0..self.n {
            let pv = part[v];
            if self.self_loop[v] > 0.0 {
                edges.push((pv, pv, self.self_loop[v]));
            }
            for &(u, w) in &self.adj[v] {
                if (u as usize) > v {
                    edges.push((pv, part[u as usize], w));
                }
            }
        }
        Graph::new(parts, &edges)
    }

    /// Modularity of a partition (nodes labelled [`UNGROUPED`] count as their own communities).
    fn modularity(&self, comm: &[u32], resolution: f64) -> f64 {
        if self.two_m == 0.0 {
            return 0.0;
        }
        // Ordered maps: the sum below runs in a fixed order, so it doesn't vary in its last bits from run to run.
        let mut inner: BTreeMap<u32, f64> = BTreeMap::new();
        let mut tot: BTreeMap<u32, f64> = BTreeMap::new();
        for v in 0..self.n {
            if comm[v] == UNGROUPED {
                continue;
            }
            *tot.entry(comm[v]).or_default() += self.degree[v];
            *inner.entry(comm[v]).or_default() += 2.0 * self.self_loop[v];
            for &(u, w) in &self.adj[v] {
                if comm[u as usize] == comm[v] {
                    *inner.entry(comm[v]).or_default() += w;
                }
            }
        }
        tot.iter().map(|(c, t)| inner.get(c).copied().unwrap_or(0.0) / self.two_m - resolution * (t / self.two_m).powi(2)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clique of `size` nodes from `start`, each edge `w`.
    fn clique(start: u32, size: u32, w: f64) -> Vec<(u32, u32, f64)> {
        let mut out = Vec::new();
        for a in start..start + size {
            for b in a + 1..start + size {
                out.push((a, b, w));
            }
        }
        out
    }

    #[test]
    fn two_cliques_joined_by_one_edge_are_two_communities() {
        let mut edges = clique(0, 5, 1.0);
        edges.extend(clique(5, 5, 1.0));
        edges.push((4, 5, 0.5));
        let c = cluster(10, &edges, 1.0, SEED);
        assert!(c[..5].iter().all(|&x| x == c[0]));
        assert!(c[5..].iter().all(|&x| x == c[5]));
        assert_ne!(c[0], c[5]);
    }

    #[test]
    fn a_ring_of_cliques_finds_every_clique_and_is_deterministic() {
        let mut edges = Vec::new();
        for k in 0..8 {
            edges.extend(clique(k * 4, 4, 1.0));
            edges.push((k * 4, ((k + 1) % 8) * 4 + 1, 0.2));
        }
        let a = cluster(32, &edges, 1.0, SEED);
        let b = cluster(32, &edges, 1.0, SEED);
        assert_eq!(a, b);
        for k in 0..8usize {
            assert!(a[k * 4..k * 4 + 4].iter().all(|&x| x == a[k * 4]), "clique {k} split: {a:?}");
        }
        assert_eq!(a.iter().collect::<HashSet<_>>().len(), 8);
        // Another seed visits nodes in another order but finds the same cliques.
        let c = cluster(32, &edges, 1.0, 7);
        let same = |x: &[u32], i: usize, j: usize| x[i] == x[j];
        for i in 0..32 {
            for j in 0..32 {
                assert_eq!(same(&a, i, j), same(&c, i, j));
            }
        }
    }

    #[test]
    fn small_communities_join_their_neighbour_and_isolated_nodes_are_ungrouped() {
        let mut edges = clique(0, 6, 1.0);
        edges.push((5, 6, 0.1)); // a pendant pair, weakly attached
        edges.push((6, 7, 1.0));
        let c = cluster(9, &edges, 1.0, SEED);
        assert_eq!(c[6], c[0]);
        assert_eq!(c[7], c[0]);
        assert_eq!(c[8], UNGROUPED);
    }

    #[test]
    fn disconnected_parts_are_never_one_community() {
        // Two triangles with no edge between them, plus a big clique: the triangles stay apart.
        let mut edges = clique(0, 3, 1.0);
        edges.extend(clique(3, 3, 1.0));
        edges.extend(clique(6, 6, 1.0));
        let c = cluster(12, &edges, 1.0, SEED);
        assert_ne!(c[0], c[3]);
        assert_ne!(c[0], c[6]);
    }

    #[test]
    fn modularity_of_two_cliques_is_high() {
        let mut edges = clique(0, 5, 1.0);
        edges.extend(clique(5, 5, 1.0));
        edges.push((4, 5, 0.5));
        let g = Graph::new(10, &edges);
        let c = cluster(10, &edges, 1.0, SEED);
        assert!(g.modularity(&c, 1.0) > 0.4);
        assert!(g.modularity(&[0; 10], 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_directory_tree_ties_files_to_directories_and_directories_to_parents() {
        let files: Vec<String> = ["a/b/x.cs", "a/b/y.cs", "a/z.cs", "top.cs"].iter().map(|s| s.to_string()).collect();
        let mut edges = Vec::new();
        // Directories in path order after the files: "" (4), "a" (5), "a/b" (6).
        assert_eq!(dir_tree(&files, &mut edges), 7);
        assert!(edges.contains(&(0, 6, DIR_WEIGHT)));
        assert!(edges.contains(&(2, 5, DIR_WEIGHT)));
        assert!(edges.contains(&(3, 4, DIR_WEIGHT)));
        assert!(edges.contains(&(6, 5, DIR_PARENT_WEIGHT * 2f64.sqrt())));
        assert!(edges.contains(&(5, 4, DIR_PARENT_WEIGHT * 3f64.sqrt())));
        assert_eq!(edges.len(), 6);
    }

    #[test]
    fn files_of_two_directories_with_no_reference_between_them_split_by_directory() {
        let mut files: Vec<String> = (0..6).map(|i| format!("orders/o{i}.cs")).collect();
        files.extend((0..6).map(|i| format!("billing/b{i}.cs")));
        files.sort();
        let mut edges = Vec::new();
        let n = dir_tree(&files, &mut edges);
        let c = cluster(n, &edges, RESOLUTION, SEED);
        let (b, o) = (&c[..6], &c[6..12]);
        assert!(b.iter().all(|x| *x == b[0]) && o.iter().all(|x| *x == o[0]));
        assert_ne!(b[0], o[0]);
    }

    #[test]
    fn names_come_from_the_dominant_directory_and_distinctive_words() {
        let none = HashMap::new();
        assert_eq!(dominant_dir(&["src/Orders/A.cs".into(), "src/Orders/B.cs".into(), "src/Shared/C.cs".into()], &none), "src/Orders");
        assert_eq!(dominant_dir(&["a/x.cs".into(), "b/y.cs".into()], &none), "");
        // Weighed by lines, tests left out: a service among many small request types names the area.
        let files: Vec<String> = ["src/Models/Dtos/A.cs", "src/Models/Dtos/B.cs", "src/Models/Dtos/C.cs", "src/Service/OrderService.cs", "src/Tests/OrderServiceTests.cs"]
            .into_iter()
            .map(String::from)
            .collect();
        let lines: HashMap<String, u64> = files.iter().zip([12, 15, 20, 700, 1300]).map(|(f, n)| (f.clone(), n)).collect();
        assert_eq!(dominant_dir(&files, &lines), "src/Service");
        assert_eq!(name_tokens("src/app/order-list/OrderListComponent.spec.ts"), vec!["order", "list"]);
        let area = |id, dir: &str, tokens: &[&str]| Area {
            id,
            dir: dir.into(),
            tokens: tokens.iter().map(|t| t.to_string()).collect(),
            ..Default::default()
        };
        let mut areas = vec![
            area(1, "src/Shop.Api/Controllers", &["order"]),
            area(2, "src/Shop.Api/Controllers", &["refund", "order"]),
            area(3, "src/Billing", &[]),
            area(4, "", &["deploy"]),
        ];
        name_areas(&mut areas);
        let names: Vec<&str> = areas.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Shop.Api/Controllers (order)", "Shop.Api/Controllers (refund)", "Billing", "deploy"]);
    }

    #[test]
    fn find_takes_an_id_a_name_a_directory_or_a_unique_fragment() {
        let a = |id, name: &str, dir: &str| Area { id, name: name.into(), dir: dir.into(), ..Default::default() };
        let areas = Areas { areas: vec![a(1, "Shop.Api/Orders", "src/Shop.Api/Orders"), a(2, "Shop.Api/Billing", "src/Shop.Api/Billing")], ..Default::default() };
        assert_eq!(areas.find("2").unwrap().id, 2);
        assert_eq!(areas.find("#1").unwrap().id, 1);
        assert_eq!(areas.find("shop.api/billing").unwrap().id, 2);
        assert_eq!(areas.find("src/Shop.Api/Orders").unwrap().id, 1);
        assert_eq!(areas.find("orders").unwrap().id, 1);
        assert!(areas.find("shop").is_none());
    }
}
