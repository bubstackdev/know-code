//! Same-named definitions: live code ranks before tests and copies (backups, old versions, template variants), and
//! one is picked by place (`path#name`, `file:line`, or a page's path).

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

fn files(rows: &[query::SymbolRow]) -> Vec<&str> {
    rows.iter().map(|r| r.file.as_str()).collect()
}

#[test]
fn live_code_ranks_first_and_a_place_picks_one() {
    let root = scratch("same-names");
    let page = "<cfquery name=\"q\">SELECT * FROM plans</cfquery>\n";
    // Sorted by path alone, every copy would come before the live page.
    write(&root, "_templates/blank/login.cfm", page);
    write(&root, "2020_backup/login.cfm", page);
    write(&root, "login.cfm", page);
    write(&root, "old/Billing.cs", "public class Billing { public void Run() { } }\n");
    write(&root, "src/Billing.cs", "public class Billing\n{\n    public void Run() { }\n}\n");
    write(&root, "src/Tests/Billing.cs", "public class Billing { public void Run() { } }\n");
    let store = index(&root);
    let conn = store.conn();

    assert_eq!(files(&query::definitions(conn, "login").unwrap()), ["login.cfm", "2020_backup/login.cfm", "_templates/blank/login.cfm"]);
    assert_eq!(files(&query::definitions(conn, "Billing").unwrap()), ["src/Billing.cs", "src/Tests/Billing.cs", "old/Billing.cs"]);

    // By place: `path#name` (the whole path or its end), `file:line` (the innermost definition there), a page's path.
    assert_eq!(files(&query::definitions(conn, "_templates/blank/login.cfm#login").unwrap()), ["_templates/blank/login.cfm"]);
    assert_eq!(files(&query::definitions(conn, "old/Billing.cs#Billing").unwrap()), ["old/Billing.cs"]);
    let at = query::definitions(conn, "src/Billing.cs:3").unwrap();
    assert_eq!((at[0].qualified().as_str(), at[0].file.as_str()), ("Billing.Run", "src/Billing.cs"));
    assert_eq!(files(&query::definitions(conn, "2020_backup/login.cfm").unwrap()), ["2020_backup/login.cfm"]);
    // A place naming nothing finds nothing; a plain name with no place is unaffected.
    assert!(query::definitions(conn, "nowhere/x.cs#Billing").unwrap().is_empty());
    assert_eq!(query::definitions(conn, "Billing.Run").unwrap()[0].file, "src/Billing.cs");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn copy_directories_are_told_from_live_ones() {
    for p in ["2020_backup/a.yml", "old/a.cs", "x/Copy of site/a.cfm", "_templates/v2/a.cfm", "src/legacy_old/a.cs", "db/archive/a.sql"] {
        assert!(kc_store::is_copy_path(p), "{p}");
    }
    for p in ["src/a.cs", "roles/web/templates/a.j2", "src/Old.cs", "golden/a.cs", "src/Backup.cs"] {
        assert!(!kc_store::is_copy_path(p), "{p}");
    }
    assert_eq!((kc_store::path_rank("src/a.cs"), kc_store::path_rank("tests/a.cs"), kc_store::path_rank("bak/a.cs")), (0, 1, 2));
}

#[test]
fn constructors_are_found_by_the_name_their_source_writes() {
    let root = scratch("ctor-names");
    // Python, TypeScript and C# constructors are indexed as `init`; each class also has a plain method named `init`.
    write(&root, "factory.py", "class Factory:\n    def __init__(self, size):\n        self.init()\n\n    def init(self):\n        pass\n");
    write(&root, "cart.ts", "export class Cart {\n  constructor() {\n    this.init();\n  }\n  init() {}\n}\n");
    write(&root, "Ledger.cs", "public class Ledger\n{\n    public Ledger() { }\n    public void init() { }\n}\n");
    write(&root, "Store.swift", "class Store {\n    init() {}\n}\n");
    let store = index(&root);
    let conn = store.conn();
    let lines = |q: &str| -> Vec<(String, u32)> { query::definitions(conn, q).unwrap().iter().map(|r| (r.kind.clone(), r.line)).collect() };
    let one = |kind: &str, line: u32| vec![(kind.to_string(), line)];

    // The written name finds the constructor alone, qualified, bare or by place.
    assert_eq!(lines("Factory.__init__"), one("init", 2));
    assert_eq!(lines("__init__"), one("init", 2));
    assert_eq!(lines("factory.py#__init__"), one("init", 2));
    assert_eq!(lines("Cart.constructor"), one("init", 2));
    assert_eq!(lines("Ledger.Ledger"), one("init", 3));
    // `init` is the plain method first, the constructor after it to pick by place; Swift's constructor is written `init`.
    assert_eq!(lines("Factory.init"), [("method".to_string(), 5), ("init".to_string(), 2)]);
    assert_eq!(lines("Cart.init"), [("method".to_string(), 5), ("init".to_string(), 2)]);
    assert_eq!(lines("Ledger.init"), [("method".to_string(), 4), ("init".to_string(), 3)]);
    assert_eq!(lines("Store.init"), one("init", 2));
    assert!(lines("Store.__init__").is_empty() && lines("Factory.constructor").is_empty());

    let (hits, _) = query::find(conn, "Factory.__init__", None, 5).unwrap();
    assert_eq!((hits[0].matched, hits[0].symbol.kind.as_str(), hits[0].symbol.line), ("exact", "init", 2));
    assert_eq!(query::written_name(&hits[0].symbol), "__init__");

    // `self.init()` and `this.init()` call the method, never the constructor.
    for q in ["Factory.init", "Cart.init"] {
        let (refs, _) = query::refs(conn, q, &["call"], 10).unwrap();
        let calls: Vec<(&str, Vec<u32>)> = refs.iter().map(|h| (h.confidence, h.targets.iter().map(|t| t.line).collect())).collect();
        assert_eq!(calls, [("exact", vec![5])], "{q}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Two modules each declaring a type of one name: a context lists the members and extensions of the declaration it
/// picks, never the other's, whether that one is another module's or a file's `private` type.
#[test]
fn a_types_members_are_its_declarations_not_a_same_named_ones() {
    let root = scratch("same-names-members");
    write(
        &root,
        "Sources/Grid/Tile.swift",
        "public struct Tile<Scalar> {\n    public var cells: [Scalar]\n    public init(cells: [Scalar]) { self.cells = cells }\n}\n",
    );
    write(&root, "Sources/Grid/TileOps.swift", "extension Tile {\n    public var count: Int { cells.count }\n}\n");
    write(
        &root,
        "Sources/Probe/Reader.swift",
        "private struct Tile {\n    var name: String\n    func file(base: String) -> String { base + name }\n}\n\nextension Tile {\n    var label: String { name }\n}\n",
    );
    write(&root, "Sources/Probe/Other.swift", "import Grid\n\nextension Tile {\n    var total: Int { 0 }\n}\n");
    let store = index(&root);
    let conn = store.conn();
    let limits = query::ContextLimits::from_budget(2000);
    let names = |c: &query::Context| -> (Vec<String>, Vec<String>) {
        (c.members.iter().map(|m| m.name.clone()).collect(), c.extensions.iter().map(|e| format!("{}:{}", e.file, e.line)).collect())
    };

    let grid = query::context(conn, &root, "Sources/Grid/Tile.swift:1", &limits).unwrap().unwrap();
    assert_eq!(grid.symbol.file, "Sources/Grid/Tile.swift");
    // The private type is seen from its own file only: the extension in Probe's other file extends Grid's.
    assert_eq!(
        names(&grid),
        (
            vec!["init".into(), "cells".into(), "count".into(), "total".into()],
            vec!["Sources/Grid/TileOps.swift:1".into(), "Sources/Probe/Other.swift:3".into()]
        )
    );

    let probe = query::context(conn, &root, "Sources/Probe/Reader.swift:1", &limits).unwrap().unwrap();
    assert_eq!(names(&probe), (vec!["name".into(), "file".into(), "label".into()], vec!["Sources/Probe/Reader.swift:6".into()]));
    let _ = std::fs::remove_dir_all(&root);
}

/// An extension in a third module goes to the type of the module its file imports; when the file imports both
/// modules declaring the name, nothing tells which it extends, and each context says the extension is ambiguous.
#[test]
fn an_extension_seeing_two_imported_types_of_its_name_is_ambiguous() {
    let root = scratch("same-names-imports");
    write(&root, "Sources/Grid/Tile.swift", "public struct Tile {\n    public var cells: Int\n}\n");
    write(&root, "Sources/Board/Tile.swift", "public struct Tile {\n    public var squares: Int\n}\n");
    write(&root, "Sources/Game/Both.swift", "import Grid\nimport Board\n\nextension Tile {\n    var either: Int { 0 }\n}\n");
    write(&root, "Sources/Game/OnlyGrid.swift", "import Grid\n\nextension Tile {\n    var gridOnly: Int { 0 }\n}\n");
    let store = index(&root);
    let conn = store.conn();
    let limits = query::ContextLimits::from_budget(2000);
    let context = |at: &str| query::context(conn, &root, at, &limits).unwrap().unwrap();
    let members = |c: &query::Context| -> Vec<String> { c.members.iter().map(|m| m.name.clone()).collect() };

    let board = context("Sources/Board/Tile.swift:1");
    assert_eq!(members(&board), ["squares", "either"]);
    assert_eq!(board.extensions_ambiguous, ["1 extension may extend this or Sources/Grid/Tile.swift:1 (Grid): Sources/Game/Both.swift:4"]);
    let grid = context("Sources/Grid/Tile.swift:1");
    assert_eq!(members(&grid), ["either", "gridOnly", "cells"]);
    assert_eq!(grid.extensions_ambiguous, ["1 extension may extend this or Sources/Board/Tile.swift:1 (Board): Sources/Game/Both.swift:4"]);
    let _ = std::fs::remove_dir_all(&root);
}
