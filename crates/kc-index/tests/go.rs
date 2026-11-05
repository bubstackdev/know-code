//! Go resolution end to end (`kc_store::go`): a Go name binds as the compiler binds it, or isn't linked. A package
//! is a directory's files sharing a clause; imports are placed by the repo's `go.mod` files, across a module boundary
//! only through a `replace`; members come from the receiver's type and what it embeds.

use std::path::PathBuf;
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

fn repo(name: &str, files: &[(&str, &str)]) -> (PathBuf, Store) {
    let dir = std::env::temp_dir().join(format!("knowcode-test-go-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let mut store = Store::open(&dir.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&dir, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    (dir, store)
}

/// Each reference to `name` in `file`: its line, confidence and targets as `file:line`.
fn refs(store: &Store, name: &str, file: &str) -> Vec<(u32, &'static str, Vec<String>)> {
    let (hits, _) = query::refs(store.conn(), name, &[], 100).unwrap();
    hits.into_iter()
        .filter(|h| h.file == file)
        .map(|h| (h.line, h.confidence, h.targets.iter().map(|t| format!("{}:{}", t.file, t.line)).collect()))
        .collect()
}

fn linked(c: &str) -> bool {
    matches!(c, "exact" | "unique_name")
}

const ORCHARD_MOD: &str = "module example.com/orchard\n\ngo 1.22\n";
const CRATE: &str = "package orchard\n\n// Crate holds items.\ntype Crate struct {\n\tApples []string\n}\n\nfunc NewCrate() *Crate { return &Crate{} }\n\nfunc (c *Crate) Count() int { return len(c.Apples) }\n";

#[test]
fn a_nested_module_without_a_replace_uses_a_released_copy() {
    let (root, store) = repo(
        "nested-released",
        &[
            ("go.mod", ORCHARD_MOD),
            ("crate.go", CRATE),
            ("examples/go.mod", "module example.com/orchard/examples\n\ngo 1.22\n\nrequire example.com/orchard v1.4.0\n"),
            ("examples/main.go", "package main\n\nimport \"example.com/orchard\"\n\nfunc main() {\n\tbox := orchard.NewCrate()\n\t_ = box.Count()\n}\n"),
        ],
    );
    // The toolchain binds both to the module cache's copy of orchard: outside the repo, never linked to crate.go.
    for name in ["NewCrate", "Count"] {
        let got = refs(&store, name, "examples/main.go");
        assert!(!got.is_empty() && got.iter().all(|g| !linked(g.1) && g.2.is_empty()), "{name}: {got:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_nested_module_replacing_the_repos_module_uses_the_repos_code() {
    let (root, store) = repo(
        "nested-replaced",
        &[
            ("go.mod", ORCHARD_MOD),
            ("crate.go", CRATE),
            (
                "examples/go.mod",
                "module example.com/orchard/examples\n\ngo 1.22\n\nrequire example.com/orchard v1.4.0\n\nreplace example.com/orchard => ../\n",
            ),
            ("examples/main.go", "package main\n\nimport \"example.com/orchard\"\n\nfunc main() {\n\tbox := orchard.NewCrate()\n\t_ = box.Count()\n}\n"),
        ],
    );
    assert_eq!(refs(&store, "NewCrate", "examples/main.go"), [(6, "exact", vec!["crate.go:8".to_string()])]);
    assert_eq!(refs(&store, "Count", "examples/main.go"), [(7, "exact", vec!["crate.go:10".to_string()])]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn packages_imports_receivers_and_embedding() {
    let (root, store) = repo(
        "packages",
        &[
            ("go.mod", ORCHARD_MOD),
            ("crate.go", CRATE),
            (
                "market/market.go",
                "package market\n\nimport (\n\t\"fmt\"\n\n\t\"example.com/orchard\"\n)\n\ntype Stall struct {\n\t*orchard.Crate\n\tID string\n}\n\nfunc Println(a ...any) {}\n\nfunc Sell(o Stall, x interface{ Count() int }) {\n\tfmt.Println(o.Count(), o.ID)\n\tPrintln(o.Apples)\n\t_ = x.Count()\n\t_ = orchard.NewCrate().Count()\n}\n",
            ),
        ],
    );
    let file = "market/market.go";
    // `fmt.Println` is the standard library's, though the package declares a `Println`; the bare one is the package's.
    let println = refs(&store, "Println", file);
    assert!(println.iter().any(|g| g.0 == 17 && g.1 == "unresolved"), "{println:?}");
    assert!(println.iter().any(|g| g.0 == 18 && g.1 == "exact" && g.2 == ["market/market.go:14"]), "{println:?}");
    // Promoted through the embedded `*orchard.Crate`, and through a call's declared result.
    let total = refs(&store, "Count", file);
    assert!(total.iter().any(|g| g.0 == 17 && g.1 == "exact" && g.2 == ["crate.go:10"]), "{total:?}");
    assert!(total.iter().any(|g| g.0 == 20 && g.1 == "exact" && g.2 == ["crate.go:10"]), "{total:?}");
    // A receiver whose type the syntax doesn't place in the repo: never linked.
    assert!(total.iter().any(|g| g.0 == 19 && !linked(g.1)), "{total:?}");
    assert!(refs(&store, "Apples", file).iter().any(|g| g.1 == "exact" && g.2 == ["crate.go:5"]));
    assert!(refs(&store, "ID", file).iter().any(|g| g.1 == "exact" && g.2 == ["market/market.go:11"]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tests_and_other_builds_stay_apart() {
    let (root, store) = repo(
        "builds",
        &[
            ("go.mod", ORCHARD_MOD),
            ("open_unix.go", "package orchard\n\nfunc open() int { return 1 }\n"),
            ("open_windows.go", "package orchard\n\nfunc open() int { return 2 }\n"),
            ("fixture_test.go", "package orchard\n\nfunc fixture() int { return open() }\n"),
            ("use.go", "package orchard\n\nfunc use() int { return open() + fixture() }\n"),
        ],
    );
    // The default build is Linux: the Windows variant gives way.
    assert_eq!(refs(&store, "open", "use.go"), [(3, "exact", vec!["open_unix.go:3".to_string()])]);
    assert_eq!(refs(&store, "open", "fixture_test.go"), [(3, "exact", vec!["open_unix.go:3".to_string()])]);
    // A test file's declarations are only the tests'.
    let fixture = refs(&store, "fixture", "use.go");
    assert!(fixture.iter().all(|g| !linked(g.1)), "{fixture:?}");
    let _ = std::fs::remove_dir_all(&root);
}
