//! Occurrence identity: each place a name is written is a reference of its own, whatever else shares its line.
//! Invariant: reformatting code (joining statements onto one line, or splitting them) never changes the set of
//! resolved references, only their lines and columns.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query::{self, RefHit};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-occ-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    dir
}

fn index(root: &Path, files: &[(&str, String)]) -> Store {
    for (path, text) in files {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    store
}

/// A reference as resolved, without where it is written.
type Resolved = (Option<String>, String, String, Option<String>, &'static str, Vec<String>);

fn resolved(h: &RefHit) -> Resolved {
    let mut targets: Vec<String> = h.targets.iter().map(|t| t.qualified.clone()).collect();
    targets.sort();
    let from = h.from.as_ref().map(|f| format!("{}.{}", f.container.as_deref().unwrap_or(""), f.name));
    // The receiver as written, whitespace aside: a chain broken over lines keeps a space where the line broke
    // (`a .Save()`), which is display only.
    let receiver = h.receiver.as_ref().map(|r| r.split_whitespace().collect::<String>());
    (from, h.name.clone(), h.kind.clone(), receiver, h.confidence, targets)
}

/// The references to `names`, as resolved, sorted; and each one's (line, column).
fn refs(store: &Store, names: &[&str]) -> (Vec<Resolved>, Vec<(u32, u32)>) {
    let hits = query::refs_named(store.conn(), names).unwrap();
    let mut r: Vec<Resolved> = hits.iter().map(resolved).collect();
    r.sort();
    (r, hits.iter().map(|h| (h.line, h.col)).collect())
}

/// One case: definitions, then a caller written with its statements on separate lines (`split`) and joined onto one
/// line (`joined`).
struct Case {
    name: &'static str,
    caller: &'static str,
    defs: (&'static str, &'static str),
    split: &'static str,
    joined: &'static str,
    names: &'static [&'static str],
    /// How many calls (or Terraform references, or SQL reads and writes) of `names` the caller makes.
    calls: usize,
}

const CASES: &[Case] = &[
    Case {
        name: "csharp",
        caller: "Caller.cs",
        defs: ("Stores.cs", "public class First { public void Save() {} }\npublic class Second { public void Save() {} }\n"),
        split: "public class Caller\n{\n    public void Run(First a, Second b)\n    {\n        a.Save();\n        b.Save();\n        a.Save();\n    }\n}\n",
        joined: "public class Caller\n{\n    public void Run(First a, Second b)\n    {\n        a.Save(); b.Save(); a.Save();\n    }\n}\n",
        names: &["Save"],
        calls: 3,
    },
    Case {
        name: "typescript",
        caller: "caller.ts",
        defs: ("stores.ts", "export class First { save(): void {} }\nexport class Second { save(): void {} }\n"),
        split: "import { First, Second } from './stores';\nexport function run(a: First, b: Second): void {\n  a.save();\n  b.save();\n  a.save();\n}\n",
        joined: "import { First, Second } from './stores';\nexport function run(a: First, b: Second): void {\n  a.save(); b.save(); a.save();\n}\n",
        names: &["save"],
        calls: 3,
    },
    Case {
        name: "python",
        caller: "caller.py",
        defs: ("stores.py", "class First:\n    def save(self):\n        pass\n\nclass Second:\n    def save(self):\n        pass\n"),
        split: "from stores import First, Second\n\ndef run(a: First, b: Second):\n    a.save()\n    b.save()\n    a.save()\n",
        joined: "from stores import First, Second\n\ndef run(a: First, b: Second):\n    a.save(); b.save(); a.save()\n",
        names: &["save"],
        calls: 3,
    },
    Case {
        name: "rust",
        caller: "src/caller.rs",
        defs: ("src/stores.rs", "pub struct First;\nimpl First { pub fn save(&self) {} }\npub struct Second;\nimpl Second { pub fn save(&self) {} }\n"),
        split: "fn run(a: &First, b: &Second) {\n    a.save();\n    b.save();\n    a.save();\n}\n",
        joined: "fn run(a: &First, b: &Second) {\n    a.save(); b.save(); a.save();\n}\n",
        names: &["save"],
        calls: 3,
    },
    Case {
        name: "swift",
        caller: "Caller.swift",
        defs: ("Stores.swift", "class First { func save() {} }\nclass Second { func save() {} }\n"),
        split: "func run(a: First, b: Second) {\n    a.save()\n    b.save()\n    a.save()\n}\n",
        joined: "func run(a: First, b: Second) {\n    a.save(); b.save(); a.save()\n}\n",
        names: &["save"],
        calls: 3,
    },
    Case {
        name: "cfml",
        caller: "Caller.cfc",
        defs: ("First.cfc", "component {\n    function save() {}\n}\n"),
        split: "component {\n    function run() {\n        var a = new First();\n        a.save();\n        a.save();\n    }\n}\n",
        joined: "component {\n    function run() {\n        var a = new First();\n        a.save(); a.save();\n    }\n}\n",
        names: &["save"],
        calls: 2,
    },
    Case {
        name: "terraform",
        caller: "main.tf",
        defs: ("variables.tf", "variable \"zone\" {}\n"),
        split: "resource \"aws_thing\" \"x\" {\n  names = [\n    var.zone,\n    var.zone,\n  ]\n}\n",
        joined: "resource \"aws_thing\" \"x\" {\n  names = [var.zone, var.zone]\n}\n",
        names: &["var.zone"],
        calls: 2,
    },
    Case {
        name: "javascript",
        caller: "caller.js",
        defs: ("stores.js", "export class First { save() {} }\nexport class Second { save() {} }\n"),
        split: "import { First, Second } from './stores';\nexport function run() {\n  const a = new First();\n  a.save();\n  a.save();\n}\n",
        joined: "import { First, Second } from './stores';\nexport function run() {\n  const a = new First(); a.save(); a.save();\n}\n",
        names: &["save"],
        calls: 2,
    },
    Case {
        // A call chain broken across lines, as formatters lay out fluent calls.
        name: "csharp-chain",
        caller: "Caller.cs",
        defs: ("Stores.cs", "public class First { public First Save() { return this; } }\npublic class Second { public Second Save() { return this; } }\n"),
        split: "public class Caller\n{\n    public void Run(First a)\n    {\n        a\n            .Save()\n            .Save();\n    }\n}\n",
        joined: "public class Caller\n{\n    public void Run(First a)\n    {\n        a.Save().Save();\n    }\n}\n",
        names: &["Save"],
        calls: 2,
    },
    Case {
        // SQL: a routine's reads and writes of tables.
        name: "sql",
        caller: "procs.sql",
        defs: ("tables.sql", "CREATE TABLE orders (id INT);\nCREATE TABLE customers (id INT);\n"),
        split: "CREATE PROCEDURE place_order()\nBEGIN\n  SELECT id FROM customers;\n  INSERT INTO orders (id) VALUES (1);\nEND;\n",
        joined: "CREATE PROCEDURE place_order()\nBEGIN SELECT id FROM customers; INSERT INTO orders (id) VALUES (1); END;\n",
        names: &["orders", "customers"],
        calls: 2,
    },
];

#[test]
fn invariant_formatting_never_changes_references() {
    for case in CASES {
        let a = scratch(&format!("{}-split", case.name));
        let b = scratch(&format!("{}-joined", case.name));
        let defs = (case.defs.0, case.defs.1.to_string());
        let split = index(&a, &[defs.clone(), (case.caller, case.split.to_string())]);
        let joined = index(&b, &[defs, (case.caller, case.joined.to_string())]);
        let (rs, at_s) = refs(&split, case.names);
        let (rj, at_j) = refs(&joined, case.names);
        let calls = rs.iter().filter(|r| matches!(r.2.as_str(), "call" | "tf_ref" | "read" | "write")).count();
        assert_eq!(calls, case.calls, "{}: {rs:#?}", case.name);
        assert_eq!(rs, rj, "{}: the references changed with the layout", case.name);
        // Each occurrence keeps a place of its own (SQL references record a line, not a column).
        for at in [&at_s, &at_j].into_iter().filter(|_| case.name != "sql") {
            let mut uniq = at.clone();
            uniq.sort();
            uniq.dedup();
            assert_eq!(uniq.len(), at.len(), "{}: two references at one place: {at:?}", case.name);
        }
        let _ = (std::fs::remove_dir_all(&a), std::fs::remove_dir_all(&b));
    }
}

#[test]
fn two_calls_on_a_line_resolve_each_through_its_own_receiver() {
    let root = scratch("own-receiver");
    let store = index(
        &root,
        &[
            ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n".into()),
            ("Stores.cs", "public class FirstStore { public void Save() {} }\npublic class SecondStore { public void Save() {} }\n".into()),
            (
                "Caller.cs",
                "public class Caller { public void Run(FirstStore first, SecondStore second) { first.Save(); second.Save(); } }\n".into(),
            ),
        ],
    );
    let hits: Vec<RefHit> = query::refs_named(store.conn(), &["Save"]).unwrap().into_iter().filter(|h| h.kind == "call").collect();
    let got: Vec<(u32, &str, Vec<&str>)> =
        hits.iter().map(|h| (h.col, h.confidence, h.targets.iter().map(|t| t.qualified.as_str()).collect())).collect();
    assert_eq!(got, [(85, "exact", vec!["FirstStore.Save"]), (100, "exact", vec!["SecondStore.Save"])]);
    let _ = std::fs::remove_dir_all(&root);
}
