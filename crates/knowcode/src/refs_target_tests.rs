//! References to one declaration (asked by its place): its own rows in `items`, rows the answer binds to another
//! declaration of the name counted apart in `elsewhere`, rows it can't place listed apart in `unplaced`.

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, Scope};
use serde_json::Value;

const FILES: &[(&str, &str)] = &[
    ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
    ("src/Crate.cs", "namespace Shop;\npublic class Crate\n{\n    public void Seal() { }\n    public void Seal(int times) { }\n}\n"),
    ("src/Parcel.cs", "namespace Shop;\npublic class Parcel\n{\n    public void Seal() { }\n}\n"),
    (
        "src/Packer.cs",
        "namespace Shop;\npublic class Packer\n{\n    public void Pack(Crate c, Parcel p)\n    {\n        c.Seal();\n        c.Seal(2);\n        c.Seal(3);\n        p.Seal();\n    }\n}\n",
    ),
];

fn lines(v: &Value, list: &str) -> Vec<u64> {
    v[list].as_array().map(|a| a.iter().filter_map(|r| r["line"].as_u64()).collect()).unwrap_or_default()
}

#[test]
fn a_placed_declarations_references_keep_other_declarations_rows_apart() {
    let cache = test_cache();
    let base = cache.dir("refs-target");
    let root = repo(&base, "shop", FILES);
    let scope = Scope { repos: vec![root.clone()], no_sync: false };
    let env = ops::refs("src/Crate.cs:4", &[], 50, &scope).unwrap().env;
    // The parameterless overload's own reference only, counted alone.
    assert_eq!(lines(&env, "items"), [6], "{env}");
    assert_eq!(env["total"], 1, "{env}");
    // The other overload's two, counted under that declaration (Parcel.Seal is another name, and isn't asked).
    let elsewhere = env["elsewhere"].as_array().unwrap_or_else(|| panic!("{env}"));
    assert_eq!(elsewhere.len(), 1, "{env}");
    assert_eq!((elsewhere[0]["qualified"].as_str(), elsewhere[0]["line"].as_u64(), elsewhere[0]["rows"].as_u64()), (Some("Crate.Seal"), Some(5), Some(2)), "{env}");
    assert!(env["elsewhere_note"].as_str().is_some_and(|n| n.contains("kc_refs")), "{env}");
    assert!(env.get("unplaced").is_none(), "{env}");
    // A page of a placed declaration's references continues its own rows, not the name's.
    let first = ops::refs("src/Crate.cs:5", &[], 1, &scope).unwrap().env;
    assert_eq!(lines(&first, "items"), [7], "{first}");
    let page = crate::cursor::more(first["cursor"].as_str().unwrap_or_else(|| panic!("{first}"))).unwrap();
    assert_eq!(lines(&page, "items"), [8], "{page}");
    assert!(page.get("cursor").is_none(), "{page}");
    // A bare name's references are every use of the name, as before.
    let all = ops::refs("Seal", &[], 50, &scope).unwrap().env;
    assert_eq!(lines(&all, "items").len(), 4, "{all}");
    assert!(all.get("elsewhere").is_none() && all.get("unplaced").is_none(), "{all}");
    let _ = std::fs::remove_dir_all(&base);
}

/// Uses of a free function's name the answer can't place (`box.seal()` on a value of no known type) aren't the
/// declaration's references, nor another's: listed apart in `unplaced`, continuable with their own cursor, and said
/// in an `unsupported` entry, since some may be the declaration's.
#[test]
fn a_placed_declarations_unplaceable_uses_are_listed_apart_and_continue() {
    let cache = test_cache();
    let base = cache.dir("refs-target-unplaced");
    let files: &[(&str, &str)] = &[
        ("pack/seal.py", "def seal():\n    pass\n"),
        ("pack/wrap.py", "def seal():\n    pass\n"),
        ("pack/run.py", "from pack.seal import seal\n\ndef run(box):\n    seal()\n    box.seal()\n    box.seal()\n    box.seal()\n"),
    ];
    let root = repo(&base, "shop", files);
    let scope = Scope { repos: vec![root.clone()], no_sync: false };
    let env = ops::refs("pack/seal.py:1", &[], 1, &scope).unwrap().env;
    assert_eq!((lines(&env, "items"), env["total"].as_u64()), (vec![4], Some(1)), "{env}");
    assert_eq!((lines(&env, "unplaced"), env["unplaced_total"].as_u64()), (vec![5], Some(3)), "{env}");
    let unsupported = crate::contract::incomplete_of(&env).into_iter().filter(|i| i.reason == kc_store::answer::Reason::Unsupported).count();
    assert_eq!(unsupported, 1, "{env}");
    // The rest of `unplaced`, page by page, from its own cursor.
    let mut rows = Vec::new();
    let mut next = env["unplaced_cursor"].as_str().map(str::to_string);
    while let Some(c) = next {
        let page = crate::cursor::more(&c).unwrap();
        // A page holds the list it continues in `items`, named in `list`.
        assert_eq!(page["list"], "unplaced", "{page}");
        rows.extend(lines(&page, "items"));
        next = page["cursor"].as_str().map(str::to_string);
        assert!(rows.len() <= 3, "{page}");
    }
    assert_eq!(rows, [6, 7]);
    let _ = std::fs::remove_dir_all(&base);
}

/// A member asked by its place is asked as `Type.member`, which keeps only references that can point at it: a use on a
/// receiver whose type isn't known (`Fetch().Seal()`) is still said, in `unplaced`, since it may be the member's.
#[test]
fn a_placed_members_uses_on_unknown_receivers_are_unplaced() {
    let cache = test_cache();
    let base = cache.dir("refs-target-receivers");
    let mut files = FILES.to_vec();
    files.push(("src/Loader.cs", "namespace Shop;\npublic class Loader\n{\n    public void Run()\n    {\n        Fetch().Seal();\n        Fetch().Seal();\n    }\n}\n"));
    let root = repo(&base, "shop", &files);
    let scope = Scope { repos: vec![root.clone()], no_sync: false };
    let env = ops::refs("src/Crate.cs:4", &[], 50, &scope).unwrap().env;
    // The member's own rows and the other overload's count are as before.
    assert_eq!((lines(&env, "items"), env["elsewhere"][0]["rows"].as_u64()), (vec![6], Some(2)), "{env}");
    let unplaced: Vec<(String, u64)> =
        env["unplaced"].as_array().unwrap_or_else(|| panic!("{env}")).iter().map(|r| (r["file"].as_str().unwrap().to_string(), r["line"].as_u64().unwrap())).collect();
    assert_eq!(unplaced, [("src/Loader.cs".to_string(), 6), ("src/Loader.cs".to_string(), 7)], "{env}");
    assert_eq!(env["unplaced_total"], 2, "{env}");
    // `p.Seal()` on a `Parcel` is another type's member, known: not among them.
    assert!(!unplaced.iter().any(|(f, _)| f == "src/Packer.cs"), "{env}");
    let _ = std::fs::remove_dir_all(&base);
}

/// Two test projects each declare a `Util.Tracker` (one qualified name, two namespaces, two projects). Asked by the
/// place of one, the other's uses are another declaration's rows (`elsewhere`), not its own: a qualified name carries
/// no namespace, so it alone doesn't make two declarations one.
#[test]
fn a_placed_declarations_twin_in_another_namespace_is_elsewhere() {
    let cache = test_cache();
    let base = cache.dir("refs-target-twins");
    let tracker = |ns: &str| format!("namespace {ns};\npublic class Tracker\n{{\n    public bool IsDone {{ get; set; }}\n}}\n");
    let user = |ns: &str| format!("namespace {ns};\npublic class Check\n{{\n    public bool Run(Tracker t)\n    {{\n        return t.IsDone;\n    }}\n}}\n");
    let (a, b, ua, ub) = (tracker("Spec.Util"), tracker("Unit.Util"), user("Spec.Util"), user("Unit.Util"));
    let files: Vec<(&str, &str)> = vec![
        ("spec/Spec.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("spec/Tracker.cs", &a),
        ("spec/Check.cs", &ua),
        ("unit/Unit.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("unit/Tracker.cs", &b),
        ("unit/Check.cs", &ub),
    ];
    let root = repo(&base, "twins", &files);
    let scope = Scope { repos: vec![root.clone()], no_sync: false };
    for (place, own, other) in [("spec/Tracker.cs:4", "spec/Check.cs", "unit/Tracker.cs"), ("unit/Tracker.cs:4", "unit/Check.cs", "spec/Tracker.cs")] {
        let env = ops::refs(place, &[], 50, &scope).unwrap().env;
        let items: Vec<&str> = env["items"].as_array().unwrap_or_else(|| panic!("{env}")).iter().filter_map(|r| r["file"].as_str()).collect();
        assert_eq!(items, [own], "{env}");
        let elsewhere = env["elsewhere"].as_array().unwrap_or_else(|| panic!("{env}"));
        assert_eq!((elsewhere.len(), elsewhere[0]["file"].as_str()), (1, Some(other)), "{env}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// A partial type's parts are one declaration: asked by the place of one part, a use bound to another part is its own.
#[test]
fn a_placed_partial_types_other_parts_rows_are_its_own() {
    let cache = test_cache();
    let base = cache.dir("refs-target-partial");
    let files: &[(&str, &str)] = &[
        ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("src/Order.cs", "namespace Shop;\npublic partial class Order\n{\n    public int Id { get; set; }\n}\n"),
        ("src/Order.Lines.cs", "namespace Shop;\npublic partial class Order\n{\n    public int Count { get; set; }\n}\n"),
        ("src/Desk.cs", "namespace Shop;\npublic class Desk\n{\n    public Order Open(Order o)\n    {\n        return o;\n    }\n}\n"),
    ];
    let root = repo(&base, "shop", files);
    let scope = Scope { repos: vec![root.clone()], no_sync: false };
    for place in ["src/Order.cs:2", "src/Order.Lines.cs:2"] {
        let env = ops::refs(place, &[], 50, &scope).unwrap().env;
        // The return type and the parameter's.
        assert_eq!(lines(&env, "items"), [4, 4], "{place}: {env}");
        assert!(env.get("elsewhere").is_none(), "{place}: {env}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// `Handler` and `Handler<T>` share a qualified name and a namespace, yet are two types: asked by the place of one, a
/// use of the other is elsewhere.
#[test]
fn a_placed_types_other_arity_is_elsewhere() {
    let cache = test_cache();
    let base = cache.dir("refs-target-arity");
    let files: &[(&str, &str)] = &[
        ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("src/Handler.cs", "namespace Shop;\npublic interface IHandler { }\n"),
        ("src/HandlerOfT.cs", "namespace Shop;\npublic interface IHandler<T> { }\n"),
        ("src/Uses.cs", "namespace Shop;\npublic class Plain : IHandler { }\npublic class Typed : IHandler<int> { }\n"),
    ];
    let root = repo(&base, "shop", files);
    let scope = Scope { repos: vec![root.clone()], no_sync: false };
    for (place, own, other) in [("src/Handler.cs:2", 2, "src/HandlerOfT.cs"), ("src/HandlerOfT.cs:2", 3, "src/Handler.cs")] {
        let env = ops::refs(place, &[], 50, &scope).unwrap().env;
        assert_eq!(lines(&env, "items"), [own], "{place}: {env}");
        assert_eq!(env["elsewhere"][0]["file"].as_str(), Some(other), "{place}: {env}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
