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

/// A type named through a TypeScript namespace is the namespace's own declaration of the name: in a namespace of the
/// file, a nested one, or one `declare global` augments. Never a same-named declaration outside that namespace.
#[test]
fn a_typescript_type_named_through_a_namespace_is_the_namespaces() {
    let root = scratch("qualified-ts-namespace");
    write(&root, "src/ns.ts", "export namespace Shapes { export interface Circle { r: number } }\nexport let c: Shapes.Circle\n");
    write(&root, "src/other.ts", "export interface Circle { x: number }\nnamespace Outer { export namespace Inner { export type Leaf = 1 } }\nexport type Leaf = 2\nlet l: Outer.Inner.Leaf\n");
    write(
        &root,
        "src/global.d.ts",
        "declare global {\n  namespace Express {\n    interface Request { user?: string }\n  }\n}\nexport {}\n",
    );
    write(&root, "src/app.ts", "interface Request { url: string }\nexport function handle(r: Express.Request, q: Request) {}\n");
    let store = index(&root);
    let conn = store.conn();
    let circle: Vec<(String, &str, usize)> =
        query::refs(conn, "Circle", &["type_ref"], 50).unwrap().0.iter().map(|h| (format!("{}:{}", h.file, h.line), h.confidence, h.target_ids.len())).collect();
    assert_eq!(circle, [("src/ns.ts:2".to_string(), "exact", 1)]);
    let target = |name: &str, at: &str| -> Option<(&'static str, Vec<String>)> {
        let (hits, _) = query::refs(conn, name, &["type_ref"], 50).unwrap();
        hits.iter().find(|h| format!("{}:{}", h.file, h.line) == at).map(|h| (h.confidence, h.targets.iter().map(|t| format!("{}:{}", t.file, t.line)).collect()))
    };
    assert_eq!(target("Circle", "src/ns.ts:2"), Some(("exact", vec!["src/ns.ts:1".to_string()])));
    assert_eq!(target("Leaf", "src/other.ts:4"), Some(("exact", vec!["src/other.ts:2".to_string()])));
    // `Express.Request` is the augmented global's; the bare `Request` beside it, the file's own.
    let requests = query::refs(conn, "Request", &["type_ref"], 50).unwrap().0;
    let by = |receiver: Option<&str>| -> Vec<(&str, String)> {
        requests.iter().filter(|h| h.receiver.as_deref() == receiver).flat_map(|h| h.targets.iter().map(|t| (h.confidence, format!("{}:{}", t.file, t.line)))).collect()
    };
    assert_eq!(by(Some("Express")), [("exact", "src/global.d.ts:3".to_string())]);
    assert_eq!(by(None), [("exact", "src/app.ts:1".to_string())]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A namespace a file imports is the one in the module it imports it from, under an alias too; one imported from a
/// package is none of the repo's, however the repo names its own.
#[test]
fn an_imported_typescript_namespace_is_the_one_in_its_module() {
    let root = scratch("qualified-ts-imported-namespace");
    write(&root, "package.json", "{ \"name\": \"app\", \"dependencies\": { \"react\": \"18.3.1\" } }\n");
    write(&root, "src/jsx/base.ts", "export namespace JSX {\n  export type Element = 1\n}\n");
    write(&root, "src/jsx/other.ts", "export namespace JSX {\n  export type Element = 2\n}\n");
    write(&root, "src/use.ts", "import type { JSX } from './jsx/base'\nexport let a: JSX.Element\n");
    write(&root, "src/alias.ts", "import type { JSX as Other } from './jsx/other'\nexport let b: Other.Element\n");
    write(&root, "src/ext.ts", "import type { JSX } from 'react'\nexport let c: JSX.Element\n");
    let store = index(&root);
    let (hits, _) = query::refs(store.conn(), "Element", &["type_ref"], 50).unwrap();
    let got: Vec<(String, &str, Vec<String>)> =
        hits.iter().map(|h| (h.file.clone(), h.confidence, h.targets.iter().map(|t| format!("{}:{}", t.file, t.line)).collect())).collect();
    assert_eq!(
        got,
        [
            ("src/alias.ts".to_string(), "exact", vec!["src/jsx/other.ts:2".to_string()]),
            ("src/ext.ts".to_string(), "unresolved", vec![]),
            ("src/use.ts".to_string(), "exact", vec!["src/jsx/base.ts:2".to_string()]),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The qualifier is read as the code around it reads it, and only modules the file sees declare it.
#[test]
fn a_swift_qualifier_is_read_in_scope_and_in_the_modules_the_file_sees() {
    let root = scratch("qualified-scope");
    write(
        &root,
        "Sources/Game/Game.swift",
        "struct Parent {\n    struct Child { struct State {} }\n    var s: Child.State\n}\nstruct Other {\n    struct Child { struct State {} }\n}\n",
    );
    // Another module declares `Node.Handle`; this one names its own `Node` (a typealias to a type the repo lacks).
    write(&root, "Sources/Tree/Node.swift", "public struct Node {\n    public struct Handle {}\n}\n");
    write(&root, "Sources/Hash/Hash.swift", "typealias Node = LibraryNode\n\nstruct Hash {\n    var h: Node.Handle\n}\n");
    let store = index(&root);
    let conn = store.conn();
    assert_eq!(uses(conn, "State"), [("Sources/Game/Game.swift:3".to_string(), "exact", vec!["Parent.Child.State".to_string()])]);
    assert_eq!(uses(conn, "Handle"), [("Sources/Hash/Hash.swift:4".to_string(), "unresolved", vec![])]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A qualifier naming a repo type in scope whose nested name the source doesn't declare (a macro generates it) is
/// nowhere in the repo, not another type's same-named member; of copies of one declaration, the file's own is meant.
#[test]
fn a_qualifier_in_scope_without_the_name_leaves_it_unresolved() {
    let root = scratch("qualified-generated");
    write(&root, "Sources/App/Feature.swift", "struct Feature {\n    enum Destination {}\n    var d: Destination.State?\n}\n");
    write(&root, "Sources/App/Other.swift", "struct Other {\n    enum Destination { struct State {} }\n}\n");
    write(&root, "Sources/App/Step1.swift", "struct Card { enum Action { enum Alert {} } }\nlet a: Card.Action.Alert? = nil\n");
    write(&root, "Sources/App/Step2.swift", "struct Card { enum Action { enum Alert {} } }\nlet b: Card.Action.Alert? = nil\n");
    let store = index(&root);
    let conn = store.conn();
    let states = uses(conn, "State");
    assert_eq!(states, [("Sources/App/Feature.swift:3".to_string(), "unresolved", vec![])]);
    let alerts: Vec<(String, &str, usize)> =
        query::refs(conn, "Alert", &["type_ref"], 50).unwrap().0.iter().map(|h| (format!("{}:{}", h.file, h.line), h.confidence, h.target_ids.len())).collect();
    assert_eq!(alerts, [("Sources/App/Step1.swift:2".to_string(), "exact", 1), ("Sources/App/Step2.swift:2".to_string(), "exact", 1)]);
    let _ = std::fs::remove_dir_all(&root);
}
