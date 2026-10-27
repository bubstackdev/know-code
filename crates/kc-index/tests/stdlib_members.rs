//! Members a repo adds to standard-library types (`extension Int32 { var isFinite … }`) and protocol requirements
//! standard-library types satisfy: a use of the name on another library type is the library's own member, not the
//! repo's.

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

const GRID: &str = "public protocol GridScalar {
    var isFinite: Bool { get }
}

extension Float: GridScalar {}

extension Int32: GridScalar {
    public var isFinite: Bool { true }
}

public struct Tile<Scalar: GridScalar> {
    public var cells: [Scalar]

    public func allFinite() -> Bool {
        cells.allSatisfy { $0.isFinite }
    }
}

func checkOne(_ v: Float) -> Bool {
    v.isFinite
}

func checkPair(_ a: [Float], _ b: [Float]) -> Bool {
    for (x, y) in zip(a, b) where !x.isFinite || !y.isFinite {
        return false
    }
    return true
}

func checkWide(_ n: Int32) -> Bool {
    n.isFinite
}
";

/// `(caller, line, confidence, targets, basis)` for each reference to `name` in `conn`.
fn uses(hits: &[query::RefHit]) -> Vec<(String, u32, &'static str, Vec<String>, Option<&'static str>)> {
    hits.iter()
        .map(|h| {
            let from = h.from.as_ref().map(|f| f.name.clone()).unwrap_or_default();
            (from, h.line, h.confidence, h.targets.iter().map(|t| t.qualified.clone()).collect(), h.basis)
        })
        .collect()
}

#[test]
fn a_member_added_to_a_library_struct_is_not_another_library_types() {
    let root = scratch("stdlib-members");
    write(&root, "Sources/Grid/FloatGrid.swift", GRID);
    let store = index(&root);
    let (hits, _) = query::refs(store.conn(), "isFinite", &["member"], 50).unwrap();
    let int32 = vec!["Int32.isFinite".to_string()];
    let both = vec!["GridScalar.isFinite".to_string(), "Int32.isFinite".to_string()];
    assert_eq!(
        uses(&hits),
        [
            // A generic parameter is no `Int32`: what it conforms to isn't read, so nothing is linked.
            ("allFinite".to_string(), 15, "unresolved", vec![], None),
            // `Float` conforms to `GridScalar`, and has `isFinite` of its own, outside the repo.
            ("checkOne".to_string(), 20, "exact", vec!["GridScalar.isFinite".to_string()], Some(query::VIA_PROTOCOL)),
            // Untyped: the requirement or the addition to `Int32`, never one picked.
            ("checkPair".to_string(), 24, "ambiguous", both.clone(), None),
            ("checkPair".to_string(), 24, "ambiguous", both, None),
            // The receiver's type is the extended one.
            ("checkWide".to_string(), 31, "exact", int32, None),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_dependencys_member_on_a_library_struct_is_not_another_library_types() {
    let grid = scratch("stdlib-members-grid");
    write(&grid, "Sources/Grid/FloatGrid.swift", GRID);
    let app = scratch("stdlib-members-app");
    write(
        &app,
        "Sources/App/Compare.swift",
        "import Grid\n\nfunc closeEnough(_ a: [Float], _ b: [Float]) -> Bool {\n    for (x, y) in zip(a, b) where !x.isFinite || !y.isFinite {\n        return false\n    }\n    return true\n}\n\nfunc finiteValue(_ v: Float) -> Bool {\n    v.isFinite\n}\n",
    );
    let (grid_store, app_store) = (index(&grid), index(&app));
    // The requirement isn't declared `public`, so only the addition to `Int32` is seen from the app: untyped, any
    // library type's `isFinite` is as likely.
    let dep = query::Dependency { repo: "grid".into(), conn: grid_store.conn(), modules: vec!["Grid".into()], assumptions: Vec::new() };
    let (hits, _) = query::refs_with(app_store.conn(), "isFinite", &["member"], 50, std::slice::from_ref(&dep)).unwrap();
    let int32 = vec!["Int32.isFinite".to_string()];
    assert_eq!(
        uses(&hits),
        [
            ("closeEnough".to_string(), 4, "external_likely", int32.clone(), None),
            ("closeEnough".to_string(), 4, "external_likely", int32, None),
            // `Float` is no `Int32`.
            ("finiteValue".to_string(), 11, "unresolved", vec![], None),
        ]
    );
    let _ = std::fs::remove_dir_all(&grid);
    let _ = std::fs::remove_dir_all(&app);
}
