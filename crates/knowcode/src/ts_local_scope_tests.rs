//! A TypeScript declaration inside a function body, a test's callback included, is named only there: of several
//! same-named ones, each reference is the innermost's that holds it.

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, Scope};
use serde_json::Value;

const FILES: &[(&str, &str)] = &[
    ("tsconfig.json", "{\"compilerOptions\": {}}\n"),
    (
        "src/shapes.test.ts",
        "describe('one', () => {\n  it('first', () => {\n    type Actual = { a: number }\n    const x: Actual = { a: 1 }\n    expectTypeOf<Actual>().toEqualTypeOf<{ a: number }>()\n  })\n  it('second', () => {\n    type Actual = { b: string }\n    const y: Actual = { b: '' }\n  })\n})\n\nfunction named() {\n  type Actual = { c: boolean }\n  const z: Actual = { c: true }\n  return z\n}\n\ntype Actual = { d: number }\nexport const top: Actual = { d: 1 }\n",
    ),
];

/// Each reference's line, its confidence and the lines of the declarations it is linked to.
fn links(env: &Value) -> Vec<(u64, String, Vec<u64>)> {
    let mut out: Vec<(u64, String, Vec<u64>)> = env["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let targets = r["targets"].as_array().map(|t| t.iter().filter_map(|t| t["line"].as_u64()).collect()).unwrap_or_default();
            (r["line"].as_u64().unwrap(), r["confidence"].as_str().unwrap().to_string(), targets)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn a_local_type_in_a_callback_is_the_one_its_block_names() {
    let cache = test_cache();
    let base = cache.dir("ts-local-scope");
    let root = repo(&base, "shapes", FILES);
    let scope = Scope { repos: vec![root.clone()], no_sync: false };
    let got = links(&ops::refs("Actual", &[], 50, &scope).unwrap().env);
    let want: Vec<(u64, String, Vec<u64>)> =
        [(4, 3), (5, 3), (9, 8), (15, 14), (20, 19)].iter().map(|(at, to)| (*at, "exact".to_string(), vec![*to])).collect();
    // Each test's own, the function's own, and the module's where no block shadows it.
    assert_eq!(got, want);
    let _ = std::fs::remove_dir_all(&base);
}
