//! A call that none of the repo's declarations of its name can take (too many or too few arguments, other argument
//! labels) is another declaration's, outside the repo: `unresolved`, never the repo's only declaration of the name.

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

fn calls(store: &Store, name: &str) -> Vec<(String, &'static str, usize)> {
    let (hits, _) = query::refs(store.conn(), name, &["call"], 50).unwrap();
    hits.iter().map(|h| (format!("{}:{}", h.file, h.line), h.confidence, h.target_ids.len())).collect()
}

#[test]
fn a_call_no_overload_can_take_is_outside_the_repo() {
    let root = scratch("no-fitting-overload");
    write(
        &root,
        "src/Ext.cs",
        "using System;\nusing System.Collections.Generic;\nnamespace Shop;\npublic static class Ext\n{\n    public static bool Contains(this IEnumerable<string> source, ReadOnlySpan<char> value, StringComparison how) => false;\n}\n",
    );
    write(
        &root,
        "src/Item.cs",
        "using System.Collections.Generic;\nnamespace Shop;\npublic class Item\n{\n    public string[] Tags = new string[0];\n    public bool Has(string t) => Tags.Contains(t);\n    public override bool Equals(object? o) => o is Item i && Equals(Tags, i.Tags);\n}\n",
    );
    write(
        &root,
        "Sources/Pages/Pages.swift",
        "extension Array {\n    mutating func append(optional e: Element?) { if let e { append(e) } }\n}\n\nfunc build(_ xs: [Int], _ y: Int?) -> [Int] {\n    var xs = xs\n    xs.append(optional: y)\n    xs.append(1)\n    return xs\n}\n",
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    // One argument, where the repo's `Contains` takes two: LINQ's.
    assert!(calls(&store, "Contains").contains(&("src/Item.cs:6".to_string(), "unresolved", 0)), "{:?}", calls(&store, "Contains"));
    // `Equals(a, b)`: `object.Equals`, not the one-argument override.
    assert!(calls(&store, "Equals").contains(&("src/Item.cs:7".to_string(), "unresolved", 0)), "{:?}", calls(&store, "Equals"));
    // Labels: `append(optional:)` is the repo's, `append(_:)` the library's.
    let appends = calls(&store, "append");
    assert!(appends.contains(&("Sources/Pages/Pages.swift:7".to_string(), "exact", 1)), "{appends:?}");
    assert!(appends.contains(&("Sources/Pages/Pages.swift:8".to_string(), "unresolved", 0)), "{appends:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A call one declaration takes stays linked: a parameter list ending in a comma, and C# overloads of which the one
/// taking the call's argument count is there.
#[test]
fn a_call_some_overload_takes_stays_linked() {
    let root = scratch("some-fitting-overload");
    write(
        &root,
        "Sources/Run/Runner.swift",
        "struct Runner {\n    private static func step(\n        _ x: Int,\n        in y: Int,\n    ) {}\n    static func go() {\n        step(1, in: 2)\n    }\n}\n",
    );
    write(
        &root,
        "src/Check.cs",
        "namespace Shop;\npublic class Request {}\npublic class Rule {}\npublic interface IChecker\n{\n    bool Check(Request r);\n    bool Check(Request r, Rule rule);\n}\npublic class Gate\n{\n    private readonly IChecker _checker;\n    public Gate(IChecker c) { _checker = c; }\n    public bool Pass(Request r, Rule rule) => _checker.Check(\n        r,\n        rule);\n}\n",
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let steps = calls(&store, "step");
    assert!(steps.iter().any(|(at, c, n)| at == "Sources/Run/Runner.swift:7" && *c != "unresolved" && *n == 1), "{steps:?}");
    let checks = calls(&store, "Check");
    assert!(checks.iter().any(|(at, c, _)| at == "src/Check.cs:13" && *c != "unresolved"), "{checks:?}");
    let _ = std::fs::remove_dir_all(&root);
}
