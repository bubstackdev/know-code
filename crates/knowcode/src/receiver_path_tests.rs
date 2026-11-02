//! A TypeScript member reached through a property of a value whose type isn't known (`c.req.header(…)`): only the
//! candidates a type's property of that name can hold.

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, ImpactParams, Scope};

const FILES: &[(&str, &str)] = &[
    ("tsconfig.json", "{\"compilerOptions\": {}}\n"),
    ("src/request.ts", "export class Req {\n  header(name: string): string {\n    return name\n  }\n}\n"),
    (
        "src/context.ts",
        "import { Req } from './request'\n\nexport class Ctx {\n  req: Req = new Req()\n  header(name: string, value: string): void {}\n}\n",
    ),
    (
        "src/app.ts",
        "export function readHeader(c) {\n  return c.req.header('x')\n}\n\nexport function setHeader(c) {\n  c.header('a', 'b')\n}\n",
    ),
];

fn callers(root: &std::path::Path, at: &str) -> Vec<String> {
    let scope = Scope { repos: vec![root.to_path_buf()], no_sync: false };
    let p = ImpactParams { symbol: Some(at.into()), depth: 1, limit: 100, ..Default::default() };
    let out = ops::impact(&p, &scope).unwrap();
    let mut got: Vec<String> = out.data[0].impact.affected.iter().map(|a| a.qualified.clone()).collect();
    got.sort();
    got
}

#[test]
fn a_member_through_an_untyped_values_property_is_one_that_property_can_hold() {
    let cache = test_cache();
    let base = cache.dir("receiver-path");
    let root = repo(&base, "router", FILES);
    // `c.req.header('x')` is a `Req`'s, since the only `req` a type declares holds one; `c.header(…)` may be either.
    assert_eq!(callers(&root, "src/context.ts:5"), ["setHeader"], "Ctx.header's callers");
    assert_eq!(callers(&root, "src/request.ts:2"), ["readHeader", "setHeader"], "Req.header's callers");
    let _ = std::fs::remove_dir_all(&base);
}
