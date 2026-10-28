//! A Swift `private` or `fileprivate` declaration is named only in its own file: elsewhere a use of its name is
//! another declaration's, often the library's (`fileprivate func append(optional:)` on `Array` doesn't capture
//! `results.append(x)` in another file).

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn a_fileprivate_member_is_named_only_in_its_file() {
    let root = scratch("swift-private");
    write(
        &root,
        "Sources/Pages/Macro.swift",
        "extension Array {\n    fileprivate mutating func append(optional e: Element?) {\n        if let e { append(e) }\n    }\n}\n\nfunc build(_ items: [Int], _ y: Int?) -> [Int] {\n    var items = items\n    items.append(optional: y)\n    return items\n}\n",
    );
    write(&root, "Sources/Pages/Help.swift", "func help(_ results: [String]) -> [String] {\n    var results = results\n    results.append(\"x\")\n    return results\n}\n");
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let (hits, _) = query::refs(store.conn(), "append", &["call"], 50).unwrap();
    let got: Vec<(String, &str, usize)> = hits.iter().map(|h| (format!("{}:{}", h.file, h.line), h.confidence, h.target_ids.len())).collect();
    assert!(got.contains(&("Sources/Pages/Macro.swift:9".to_string(), "exact", 1)), "{got:?}");
    // Another file never sees the fileprivate member: the library's `append`.
    assert!(got.contains(&("Sources/Pages/Help.swift:3".to_string(), "unresolved", 0)), "{got:?}");
    let _ = std::fs::remove_dir_all(&root);
}
