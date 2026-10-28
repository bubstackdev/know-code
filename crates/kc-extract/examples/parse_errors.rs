//! Parse errors in the Swift files under each directory given, as JSON lines: per file with any, its error count
//! and its first outermost error sites (line, column, the text there). For ranking the constructs behind them
//! (eval/swift-parse-coverage.md). With `KC_NAMES` set, each file's declarations too.
//!
//!     cargo run --release -p kc-extract --example parse_errors -- DIR [DIR ...] > errors.jsonl
use std::path::Path;

use kc_extract::Lang;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "swift") {
            out.push(p);
        }
    }
}

fn main() {
    for root in std::env::args().skip(1) {
        let mut files = Vec::new();
        walk(Path::new(&root), &mut files);
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else { continue };
            let facts = kc_extract::extract(Lang::Swift, &src);
            let rel = f.strip_prefix(&root).unwrap_or(&f).display().to_string();
            let sites = if facts.parse_errors > 0 { kc_extract::parse_error_sites(Lang::Swift, &src, 20) } else { Vec::new() };
            let mut row = serde_json::json!({ "root": root, "file": rel, "errors": facts.parse_errors, "symbols": facts.symbols.len(), "sites": sites });
            // With KC_NAMES set, each file's declarations too, for telling which a change gains or loses.
            if std::env::var_os("KC_NAMES").is_some() {
                let names: Vec<String> = facts.symbols.iter().map(|s| format!("{}:{}", s.container.as_deref().map_or(s.name.clone(), |c| format!("{c}.{}", s.name)), s.start_line)).collect();
                row["names"] = serde_json::json!(names);
            }
            println!("{row}");
        }
    }
}
