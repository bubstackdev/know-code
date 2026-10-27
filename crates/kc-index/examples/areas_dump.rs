//! A repo's functional areas under each combination of signals, for ablation and stability checks:
//! `cargo run --release -p kc-index --example areas_dump -- <repo> [seed] > areas.json`. Reads the index as it is.
//! Prints `{"configs": {name: {"modularity", "areas", "assign": {file: area id}}}}`.
use std::path::Path;

use kc_index::areas::{self, Signals};
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = kc_index::repo_root(Path::new(&args.next().expect("repo path")))?;
    let seed: Option<u64> = args.next().map(|s| s.parse().expect("seed"));
    let store = kc_index::open(&root)?;
    let configs = [
        ("all", Signals { refs: true, cochange: true, dirs: true }),
        ("refs+dirs", Signals { refs: true, cochange: false, dirs: true }),
        ("cochange+dirs", Signals { refs: false, cochange: true, dirs: true }),
        ("refs+cochange", Signals { refs: true, cochange: true, dirs: false }),
        ("refs", Signals { refs: true, cochange: false, dirs: false }),
        ("dirs", Signals { refs: false, cochange: false, dirs: true }),
    ];
    let mut out = serde_json::Map::new();
    for (name, signals) in configs {
        let a = match seed {
            Some(s) => areas::compute_seeded(&root, &store, signals, s)?,
            None => areas::compute(&root, &store, signals)?,
        };
        let mut assign = serde_json::Map::new();
        for area in &a.areas {
            for f in &area.files {
                assign.insert(f.clone(), json!(area.id));
            }
        }
        out.insert(
            name.to_string(),
            json!({
                "modularity": a.modularity,
                "cochange_pairs": a.cochange_pairs,
                "files": a.files,
                "ungrouped": a.ungrouped.len(),
                "areas": a.areas.iter().map(|x| json!({"id": x.id, "name": x.name, "dir": x.dir, "files": x.files.len(), "cohesion": x.cohesion, "tokens": x.tokens, "key_files": x.key_files})).collect::<Vec<_>>(),
                "assign": assign,
            }),
        );
    }
    println!("{}", json!({ "repo": root, "configs": out }));
    Ok(())
}
