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
