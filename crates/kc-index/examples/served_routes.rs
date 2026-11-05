//! The routes a repo serves, as the workspace summary every HTTP answer reads holds them, for routes-v1's scorer
//! (scripts/eval/routes_gate.py): `cargo run --release -p kc-index --example served_routes -- <repo>`. Not a product
//! path: no answer lists every route a repo serves, so the gate reads them here, and checks them against `context`'s
//! "serves" rows (eval/routes-v1/README.md, Scoring). Reads the index as it is: index the repo first.
//! Prints one JSON object per route: `verb`, `path`, `symbol`, `kind`, `file`, `line`, `start_line`.
use std::path::Path;

use serde_json::json;

fn main() -> anyhow::Result<()> {
    let root = kc_index::repo_root(Path::new(&std::env::args().nth(1).expect("repo path")))?;
    let summaries = kc_index::workspace::summaries(std::slice::from_ref(&root))?;
    let summary = summaries.iter().find(|s| s.root == root).ok_or_else(|| anyhow::anyhow!("{}: not indexed", root.display()))?;
    for r in summary.routes.iter().filter(|r| r.side == "server") {
        println!(
            "{}",
            json!({
                "verb": r.verb,
                "path": r.path,
                "symbol": r.symbol,
                "kind": r.kind,
                "file": r.file,
                "line": r.line,
                "start_line": r.start_line,
            })
        );
    }
    Ok(())
}
