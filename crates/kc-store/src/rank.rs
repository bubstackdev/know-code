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

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;
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

/// The reference graph: every reference resolved, as for [`query::file_edges`]. Expensive; cache it per index
/// generation (see `Store::ref_graph`).
pub fn ref_graph(conn: &Connection) -> Result<RefGraph> {
    let files: Vec<String> = conn
        .prepare("SELECT path FROM files WHERE lang NOT IN ('swiftpm', 'xcodegen', 'cargo', 'hostconfig') ORDER BY path")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // `symbols.name` groups without case; which spelling a group shows depends on row order, so compare lower-cased.
    let common: HashSet<String> = conn
        .prepare("SELECT lower(name) FROM symbols GROUP BY name HAVING COUNT(DISTINCT file_id) > ?1")?
        .query_map([MANY_DEFINITIONS], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let names: Vec<String> = conn.prepare("SELECT DISTINCT name FROM refs")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut target: HashMap<i64, (String, bool)> = HashMap::new();
    let mut stmt = conn.prepare_cached("SELECT f.path, s.signature FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1")?;
    let mut edges: BTreeMap<(String, String), BTreeMap<String, NameUse>> = BTreeMap::new();
    for name in names {
        let (hits, _) = query::refs(conn, &name, &[], usize::MAX)?;
        // A name used several times on one line from one symbol, as one kind of reference reaching one target, counts once: weights count the
        // lines using a name, as before references kept their columns.
        let mut counted: HashSet<(String, u32, Option<i64>, String, i64)> = HashSet::new();
        for h in hits {
            let exact = match h.confidence {
                "exact" => true,
                "unique_name" => false,
                _ => continue,
            };
            for &id in &h.target_ids {
                let (dst, private) = target.entry(id).or_insert_with(|| {
                    let (path, sig): (String, String) = stmt.query_row([id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap_or_default();
                    let private = sig.split_whitespace().any(|w| w == "private");
                    (path, private)
                });
                if dst.is_empty() || *dst == h.file || !counted.insert((h.file.clone(), h.line, h.from.as_ref().map(|f| f.id), h.kind.clone(), id)) {
                    continue;
                }
                let names = edges.entry((h.file.clone(), dst.clone())).or_default();
                let n = names.entry(name.clone()).or_insert_with(|| NameUse {
                    name: name.clone(),
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
    }
    let edges = edges.into_iter().map(|((s, d), names)| (s, d, names.into_values().collect())).collect();
    Ok(RefGraph { files, edges })
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
