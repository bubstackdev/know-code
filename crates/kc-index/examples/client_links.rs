//! The client requests of a workspace's repos and the server routes each links to, for route-links' scorer
//! (scripts/eval/route_links.py `score`): `cargo run -p kc-index --example client_links -- <repo>...`. Not a product
//! path: the `routes` answer names each request by the symbol making it, and route-links labels call sites, two of
//! which one function can hold, so the scorer reads each request's file and line here. The links are
//! `kc_index::http::handlers`', the function the `routes` answer calls, over the summaries of the repos given (index
//! them first: it reads the index as it is). Prints one JSON object per request: its `repo`, `file`, `line`, `symbol`,
//! `verb` and `path`, and `links`, each with the linked route's `repo`, `file`, `line`, `start_line`, `end_line`,
//! `symbol`, `verb` and `path`.
use std::path::Path;

use serde_json::json;

fn main() -> anyhow::Result<()> {
    let roots = std::env::args().skip(1).map(|a| kc_index::repo_root(Path::new(&a))).collect::<anyhow::Result<Vec<_>>>()?;
    anyhow::ensure!(!roots.is_empty(), "usage: client_links <repo>...");
    let summaries = kc_index::workspace::summaries(&roots)?;
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for s in summaries.iter().filter(|s| roots.contains(&s.root)) {
        for r in s.routes.iter().filter(|r| matches!(r.side.as_str(), "client" | "operation")) {
            let links: Vec<_> = kc_index::http::handlers(r, &s.root, &summaries)
                .into_iter()
                .map(|l| {
                    json!({
                        "repo": name(&l.repo),
                        "file": l.route.file,
                        "line": l.route.line,
                        "start_line": l.route.start_line,
                        "end_line": l.route.end_line,
                        "symbol": l.route.symbol,
                        "verb": l.route.verb,
                        "path": l.route.path,
                    })
                })
                .collect();
            println!(
                "{}",
                json!({
                    "repo": name(&s.root),
                    "file": r.file,
                    "line": r.line,
                    "symbol": r.symbol,
                    "verb": r.verb,
                    "path": r.path,
                    "links": links,
                })
            );
        }
    }
    Ok(())
}
