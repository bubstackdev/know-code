//! A type written through a qualifier (`Outer.Inner`, `Module.Type`, `Array<T>.Index`, `globalThis.Event`) is looked up
//! in that qualifier: a type nested there, or one at the top of that module. A qualifier the repo doesn't declare
//! leaves the name unresolved, never the repo's same-named type in scope.

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

fn index(root: &Path) -> Store {
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    store
}

/// `(file:line, confidence, targets)` of each type reference to `name`.
fn uses(conn: &rusqlite::Connection, name: &str) -> Vec<(String, &'static str, Vec<String>)> {
    let (hits, _) = query::refs(conn, name, &["type_ref"], 50).unwrap();
    hits.iter().map(|h| (format!("{}:{}", h.file, h.line), h.confidence, h.targets.iter().map(|t| t.qualified.clone()).collect())).collect()
}

#[test]
fn a_swift_type_named_through_a_qualifier_is_looked_up_there() {
    let root = scratch("qualified-swift");
    write(
        &root,
        "Sources/Rings/Ring.swift",
        "public struct Ring<Element> {
    public struct Index {}
    public typealias Indices = Array<Element>.Indices
    func copy() -> UnsafeMutableBufferPointer<Element>.Index { 0 }
    func first() -> Index { Index() }
    func view(_ s: String.UnicodeScalarView) -> Ring.Index { Index() }
}
",
    );
    write(
        &root,
        "Sources/Board/Board.swift",
        "import Rings\n\nstruct Board {\n    var at: Rings.Ring<Int>.Index\n    var ring: Rings.Ring<Int>\n}\n",
    );
    let store = index(&root);
    let conn = store.conn();
    let ring_index = vec!["Ring.Index".to_string()];
    assert_eq!(
        uses(conn, "Index"),
        [
            ("Sources/Board/Board.swift:4".to_string(), "exact", ring_index.clone()),
            // The library's buffer index, not the repo's `Ring.Index` in scope.
            ("Sources/Rings/Ring.swift:4".to_string(), "unresolved", vec![]),
            ("Sources/Rings/Ring.swift:5".to_string(), "exact", ring_index.clone()),
            ("Sources/Rings/Ring.swift:6".to_string(), "exact", ring_index),
        ]
    );
    // `Array<Element>.Indices` is the library's, never the typealias declaring it.
    assert_eq!(uses(conn, "Indices"), [("Sources/Rings/Ring.swift:3".to_string(), "unresolved", vec![])]);
    assert_eq!(uses(conn, "Ring")[0], ("Sources/Board/Board.swift:5".to_string(), "exact", vec!["Ring".to_string()]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_typescript_type_named_through_globalthis_is_the_global() {
    let root = scratch("qualified-ts");
    write(&root, "src/types.ts", "export type FocusEvent = globalThis.FocusEvent\nexport type Handler = (e: FocusEvent) => void\n");
    let store = index(&root);
    let conn = store.conn();
    assert_eq!(
        uses(conn, "FocusEvent"),
        [
            ("src/types.ts:1".to_string(), "unresolved", vec![]),
            ("src/types.ts:2".to_string(), "exact", vec!["FocusEvent".to_string()]),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}
