//! Every call in a repo with how it resolved, one JSON object per line, for finding what resolution misses:
//! `cargo run --release -p kc-index --example resolution_dump -- <repo> > calls.jsonl`. Reads the index as it is.
use std::path::Path;

use kc_store::query;

fn main() -> anyhow::Result<()> {
    let root = kc_index::repo_root(Path::new(&std::env::args().nth(1).expect("repo path")))?;
    let store = kc_index::open(&root)?;
    let conn = store.conn();
    let mut stmt = conn.prepare("SELECT DISTINCT name FROM refs WHERE kind = 'call'")?;
    let names: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for name in names {
        let (hits, _) = query::refs(conn, &name, &["call"], usize::MAX)?;
        for h in hits {
            let line = serde_json::json!({
                "name": h.name,
                "confidence": h.confidence,
                "receiver": h.receiver,
                "receiver_type": h.receiver_type,
                "file": h.file,
                "line": h.line,
                "targets": h.targets.iter().map(|t| &t.qualified).collect::<Vec<_>>(),
            });
            println!("{line}");
        }
    }
    Ok(())
}
