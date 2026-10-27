//! In-process latency measurements, for development.
//!
//! For each repo: the no-op freshness check (what every query pays first), then `find`, `refs` and `context` on a
//! deterministic sample of symbol names, timed without process startup.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use kc_index::SyncOptions;
use kc_store::query::{self, ContextLimits};
use serde::Serialize;
use serde_json::json;

#[derive(Serialize)]
struct Dist {
    n: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

fn dist(mut v: Vec<f64>) -> Dist {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |q: f64| v.get(((v.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
    let r = |x: f64| (x * 100.0).round() / 100.0;
    Dist { n: v.len(), p50_ms: r(pick(0.5)), p95_ms: r(pick(0.95)), max_ms: r(v.last().copied().unwrap_or(0.0)) }
}

fn time<T>(f: impl FnOnce() -> Result<T>) -> Result<f64> {
    let t = Instant::now();
    f()?;
    Ok(t.elapsed().as_secs_f64() * 1000.0)
}

pub fn run(roots: &[PathBuf], samples: usize, json_out: bool) -> Result<()> {
    let mut all = Vec::new();
    let (mut all_fresh, mut all_find, mut all_refs, mut all_ctx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for root in roots {
        let r = bench_repo(root, samples)?;
        all_fresh.extend(r.1.clone());
        all_find.extend(r.2.clone());
        all_refs.extend(r.3.clone());
        all_ctx.extend(r.4.clone());
        all.push(r.0);
    }
    let overall = json!({
        "freshness_check": dist(all_fresh),
        "find": dist(all_find),
        "refs": dist(all_refs),
        "context": dist(all_ctx),
    });
    if json_out {
        println!("{}", serde_json::to_string_pretty(&json!({ "repos": all, "overall": overall }))?);
    } else {
        for r in &all {
            println!("{}", serde_json::to_string(r)?);
        }
        println!("overall {}", serde_json::to_string(&overall)?);
    }
    Ok(())
}

type Samples = Vec<f64>;

fn bench_repo(root: &Path, samples: usize) -> Result<(serde_json::Value, Samples, Samples, Samples, Samples)> {
    let mut store = kc_index::open(root)?;
    kc_index::sync(root, &mut store, &SyncOptions::default())?;

    let mut fresh = Vec::new();
    for _ in 0..10 {
        fresh.push(time(|| kc_index::sync(root, &mut store, &SyncOptions::default()))?);
    }

    let names = query::symbol_names(store.conn())?;
    let step = (names.len() / samples.max(1)).max(1);
    let picked: Vec<&String> = names.iter().step_by(step).take(samples).collect();
    let conn = store.conn();
    let limits = ContextLimits::from_budget(2000);
    let (mut find, mut refs, mut ctx) = (Vec::new(), Vec::new(), Vec::new());
    let (mut found, mut ref_rows) = (0, 0);
    for name in &picked {
        // Prefix queries are the realistic case for find: an agent types part of a name.
        let prefix: String = name.chars().take(4.max(name.chars().count() / 2)).collect();
        find.push(time(|| query::find(conn, &prefix, None, 20))?);
        refs.push(time(|| {
            ref_rows += query::refs(conn, name, &[], 50)?.1.total;
            Ok(())
        })?);
        ctx.push(time(|| {
            found += query::context(conn, root, name, &limits)?.is_some() as usize;
            Ok(())
        })?);
    }
    // Worst case: the most-referenced names (`init`, `append`, SwiftUI modifiers).
    let (mut heavy_refs, mut heavy_ctx) = (Vec::new(), Vec::new());
    let heavy = query::most_referenced(conn, 10)?;
    for (name, _) in &heavy {
        heavy_refs.push(time(|| query::refs(conn, name, &[], 50))?);
        heavy_ctx.push(time(|| query::context(conn, root, name, &limits))?);
    }
    let t = Instant::now();
    let edges = query::file_edges(conn)?;
    let file_edges_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let calls = query::resolution_summary(conn, &["call"])?;
    let types = query::resolution_summary(conn, &["instantiate", "type_ref"])?;
    let resolve_all_ms = t.elapsed().as_secs_f64() * 1000.0;
    let summary = json!({
        "repo": root.file_name().map(|n| n.to_string_lossy().into_owned()),
        "names": names.len(),
        "sampled": picked.len(),
        "contexts_found": found,
        "refs_returned": ref_rows,
        "freshness_check": dist(fresh.clone()),
        "find": dist(find.clone()),
        "refs": dist(refs.clone()),
        "context": dist(ctx.clone()),
        "resolution": { "calls": calls, "type_uses": types, "resolve_all_ms": resolve_all_ms.round() },
        "file_edges": { "count": edges.len(), "ms": file_edges_ms.round() },
        "heavy_names": heavy.iter().map(|(n, c)| format!("{n} ({c})")).collect::<Vec<_>>(),
        "heavy_refs": dist(heavy_refs),
        "heavy_context": dist(heavy_ctx),
    });
    Ok((summary, fresh, find, refs, ctx))
}
