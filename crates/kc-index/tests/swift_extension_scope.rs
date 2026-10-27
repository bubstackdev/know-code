//! Swift code in `extension A.B`, declared at a file's top, sees B's members but not A's: its scope is the file. The
//! extension's header (its conformances) is read in the file's scope alone. Code lexically inside `A` still sees A's
//! members.

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
fn an_extension_of_a_nested_type_sees_the_file_not_the_outer_type() {
    let root = scratch("swift-extension-scope");
    write(
        &root,
        "Sources/Coding/Box.swift",
        "enum Box {\n    final class Decoder {}\n    final class Keyed {\n        func near() -> Decoder { Decoder() }\n    }\n}\n\nextension Box.Keyed {\n    func sup() -> Decoder { fatalError() }\n}\n\nextension Box.Decoder: Decoder {}\n",
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let (hits, _) = query::refs(store.conn(), "Decoder", &["type_ref"], 50).unwrap();
    let got: Vec<(u32, u32, &str, Vec<String>)> =
        hits.iter().map(|h| (h.line, h.col, h.confidence, h.targets.iter().map(|t| t.qualified.clone()).collect())).collect();
    let box_decoder = vec!["Box.Decoder".to_string()];
    // Inside `enum Box`, `Decoder` is Box's; so is the extended type `Box.Decoder`.
    assert!(got.contains(&(4, 24, "exact", box_decoder.clone())), "{got:?}");
    assert!(got.contains(&(12, 15, "exact", box_decoder)), "{got:?}");
    // In `extension Box.Keyed` and in the conformance `: Decoder`, it is Swift's `Decoder` protocol.
    assert!(got.iter().any(|g| g.0 == 9 && g.2 != "exact"), "{got:?}");
    assert!(got.iter().any(|g| g.0 == 12 && g.1 == 24 && g.2 != "exact"), "{got:?}");
    let _ = std::fs::remove_dir_all(&root);
}
