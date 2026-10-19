//! A ranked map of a repo within a token budget: the files that matter most (by PageRank over the file reference
//! graph) and, in each, the symbols other files use.
//!
//! Edges run from a file to the files its references resolve into (`exact` or `unique_name`), weighted by the
//! number of distinct names used, so the graph never invents a dependency. A focus (a name or a path fragment) personalizes the rank toward
//! matching files.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::query::FileEdge;

/// Bumped when the map's content changes, so cached maps are recomputed.
pub const MAP_VERSION: u32 = 3;

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapSymbol {
    pub qualified: String,
    pub kind: String,
    pub line: u32,
    pub signature: String,
    /// Distinct other files that reference the name.
    pub used_by: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapFile {
    pub file: String,
    /// PageRank, scaled so the files sum to 100.
    pub rank: f64,
    pub symbols: Vec<MapSymbol>,
    /// How many symbols qualified for the map, when fewer are shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbols_total: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedMap {
    pub files: Vec<MapFile>,
    /// Files left out to stay within the budget.
    pub omitted_files: usize,
    pub truncated: bool,
    /// With a focus: whether any symbol name or path matched it (when none did, the map isn't biased).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_matched: Option<bool>,
}

/// Builds the map within about `budget` tokens (4 characters per token).
pub fn ranked_map(conn: &Connection, file_edges: &[FileEdge], focus: Option<&str>, budget: usize) -> Result<RankedMap> {
    let mut stmt = conn.prepare("SELECT id, path FROM files WHERE lang NOT IN ('swiftpm', 'xcodegen', 'cargo') ORDER BY path")?;
    let files: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    if files.is_empty() {
        return Ok(RankedMap { files: Vec::new(), omitted_files: 0, truncated: false, focus_matched: focus.map(|_| false) });
    }
    let index: HashMap<i64, usize> = files.iter().enumerate().map(|(i, (id, _))| (*id, i)).collect();
    let by_path: HashMap<&str, usize> = files.iter().enumerate().map(|(i, (_, p))| (p.as_str(), i)).collect();
    let edges: Vec<(usize, usize, f64)> = file_edges
        .iter()
        .filter_map(|e| Some((*by_path.get(e.src.as_str())?, *by_path.get(e.dst.as_str())?, e.weight as f64)))
        .collect();

    // Personalization: uniform, or concentrated on files that match the focus.
    let n = files.len();
    let mut personal = vec![1.0 / n as f64; n];
    let mut focus_matched = focus.map(|_| false);
    if let Some(f) = focus.filter(|f| !f.trim().is_empty()) {
        let f = f.trim();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT file_id FROM symbols WHERE name = ?1 OR name LIKE ?1 || '%' OR container = ?1",
        )?;
        let mut hit: Vec<usize> =
            stmt.query_map([f], |r| r.get::<_, i64>(0))?.filter_map(|r| r.ok()).filter_map(|id| index.get(&id).copied()).collect();
        let lower = f.to_lowercase();
        hit.extend(files.iter().enumerate().filter(|(_, (_, p))| p.to_lowercase().contains(&lower)).map(|(i, _)| i));
        hit.sort();
        hit.dedup();
        if !hit.is_empty() {
            focus_matched = Some(true);
            personal = vec![0.0; n];
            for i in &hit {
                personal[*i] = 1.0 / hit.len() as f64;
            }
        }
    }

    let mut out_weight = vec![0.0; n];
    for (a, _, w) in &edges {
        out_weight[*a] += w;
    }
    let mut rank = personal.clone();
    for _ in 0..ITERATIONS {
        let mut next: Vec<f64> = personal.iter().map(|p| (1.0 - DAMPING) * p).collect();
        let mut dangling = 0.0;
        for (i, r) in rank.iter().enumerate() {
            if out_weight[i] == 0.0 {
                dangling += r;
            }
        }
        for (i, p) in personal.iter().enumerate() {
            next[i] += DAMPING * dangling * p;
        }
        for (a, b, w) in &edges {
            next[*b] += DAMPING * rank[*a] * w / out_weight[*a];
        }
        rank = next;
    }

    // Per file: how many other files use each of its names (from the resolved edges).
    let mut used: Vec<HashMap<String, u64>> = vec![HashMap::new(); n];
    for e in file_edges {
        if let Some(&b) = by_path.get(e.dst.as_str()) {
            for name in &e.names {
                *used[b].entry(name.clone()).or_default() += 1;
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| rank[*b].partial_cmp(&rank[*a]).unwrap());
    let total: f64 = rank.iter().sum::<f64>().max(f64::EPSILON);
    let mut budget_chars = budget * 4;
    let mut out = Vec::new();
    let mut truncated = false;
    let mut sym_stmt = conn.prepare(
        "SELECT name, container, kind, start_line, signature FROM symbols WHERE file_id = ?1 AND kind != 'case'
         ORDER BY start_line",
    )?;
    for &i in &order {
        let (id, path) = &files[i];
        let mut symbols: Vec<MapSymbol> = sym_stmt
            .query_map([id], |r| {
                let name: String = r.get(0)?;
                let container: Option<String> = r.get(1)?;
                Ok(MapSymbol {
                    used_by: 0,
                    qualified: match &container {
                        Some(c) => format!("{c}.{name}"),
                        None => name.clone(),
                    },
                    kind: r.get(2)?,
                    line: r.get(3)?,
                    signature: r.get(4)?,
                })
            })?
            .filter_map(|s| s.ok())
            .collect();
        for s in &mut symbols {
            let short = s.qualified.rsplit('.').next().unwrap_or(&s.qualified);
            s.used_by = used[i].get(short).copied().unwrap_or(0);
        }
        // Types always; members only when another file uses them.
        let is_type = |s: &MapSymbol| matches!(s.kind.as_str(), "class" | "struct" | "enum" | "actor" | "protocol");
        symbols.retain(|s| is_type(s) || s.used_by > 0);
        // A member declared on its type's own line (a record's positional parameters) is in the type's signature.
        let type_lines: Vec<u32> = symbols.iter().filter(|s| is_type(s)).map(|s| s.line).collect();
        symbols.retain(|s| is_type(s) || !type_lines.contains(&s.line));
        if symbols.is_empty() {
            continue;
        }
        symbols.sort_by(|a, b| b.used_by.cmp(&a.used_by).then(a.line.cmp(&b.line)));
        let qualified = symbols.len();
        symbols.truncate(12);
        symbols.sort_by_key(|s| s.line);
        let cost = path.len() + 8 + symbols.iter().map(|s| s.signature.len().min(120) + 8).sum::<usize>();
        if cost > budget_chars {
            truncated = true;
            if out.is_empty() {
                // Always show the top file, trimmed.
                let keep = budget_chars / 80;
                symbols.truncate(keep.max(1));
            } else {
                break;
            }
        }
        budget_chars = budget_chars.saturating_sub(cost);
        let symbols_total = (qualified > symbols.len()).then_some(qualified);
        out.push(MapFile { file: path.clone(), rank: (rank[i] / total * 10_000.0).round() / 100.0, symbols, symbols_total });
    }
    let omitted = n - out.len();
    Ok(RankedMap { files: out, omitted_files: omitted, truncated, focus_matched })
}
