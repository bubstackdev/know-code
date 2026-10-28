//! A Swift receiver typed by a bare name that only a type nested elsewhere carries (`decoder: Decoder`, where the repo
//! nests `Minimal.Decoder`) is the library's type of that name: its members are listed, never linked to the nested
//! type's. Inside the nesting type, the nested one is meant.

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
fn a_receiver_named_like_a_type_nested_elsewhere_is_the_librarys() {
    let root = scratch("swift-existential");
    write(
        &root,
        "Sources/Support/Minimal.swift",
        "enum Minimal {\n    final class Decoder {\n        func singleValueContainer() -> Int { 0 }\n    }\n    static func probe(_ d: Decoder) -> Int {\n        d.singleValueContainer()\n    }\n    static var current: Decoder?\n}\n",
    );
    write(
        &root,
        "Sources/Sets/Set+Codable.swift",
        "struct Bag: Decodable {\n    init(from decoder: Decoder) throws {\n        let container = try decoder.singleValueContainer()\n        _ = container\n    }\n}\n\nfunc peek() -> Int {\n    Minimal.current!.singleValueContainer()\n}\n\nfunc poke(_ d: Minimal.Decoder) -> Int {\n    d.singleValueContainer()\n}\n",
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let (hits, _) = query::refs(store.conn(), "singleValueContainer", &["call"], 50).unwrap();
    let got: Vec<(String, &str)> = hits.iter().map(|h| (format!("{}:{}", h.file, h.line), h.confidence)).collect();
    // Inside `Minimal`, `Decoder` is `Minimal.Decoder`.
    assert!(got.contains(&("Sources/Support/Minimal.swift:6".to_string(), "exact")), "{got:?}");
    // Elsewhere, Swift's `Decoder`: listed, not linked.
    assert!(got.contains(&("Sources/Sets/Set+Codable.swift:3".to_string(), "external_likely")), "{got:?}");
    // A type inferred from a declaration elsewhere (`Minimal.current`'s) is read where it was written.
    assert!(got.contains(&("Sources/Sets/Set+Codable.swift:9".to_string(), "exact")), "{got:?}");
    // A type written qualified (`Minimal.Decoder`) is the nested one.
    assert!(got.contains(&("Sources/Sets/Set+Codable.swift:13".to_string(), "exact")), "{got:?}");
    let _ = std::fs::remove_dir_all(&root);
}
