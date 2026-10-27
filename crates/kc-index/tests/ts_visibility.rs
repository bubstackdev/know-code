//! A TypeScript name binds to the repo's declaration only where the file can name it: declared there, imported, or
//! declared in a script (a file with no import or export, whose declarations are global). Elsewhere it is the
//! platform's global of that name (`Response`), whatever a module of the repo exports.

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

/// `(file:line, confidence, targets)` of each reference to `name` of `kinds`.
fn uses(conn: &rusqlite::Connection, name: &str, kinds: &[&str]) -> Vec<(String, &'static str, Vec<String>)> {
    let (hits, _) = query::refs(conn, name, kinds, 50).unwrap();
    hits.iter().map(|h| (format!("{}:{}", h.file, h.line), h.confidence, h.targets.iter().map(|t| t.qualified.clone()).collect())).collect()
}

#[test]
fn a_name_the_file_neither_declares_nor_imports_is_the_global() {
    let root = scratch("ts-visibility");
    write(
        &root,
        "src/client/types.ts",
        "export interface ClientResponse {\n  status: number\n  json(): Promise<unknown>\n}\nexport interface Response extends ClientResponse {}\n",
    );
    write(
        &root,
        "src/server.test.ts",
        "export async function check(res: Response) {\n  const status = res.status\n  return Response.json({ status })\n}\n",
    );
    write(
        &root,
        "src/client/use.ts",
        "import type { Response } from './types'\n\nexport function read(res: Response) {\n  return res.status\n}\n",
    );
    write(&root, "src/globals.d.ts", "interface Shared {\n  id: string\n}\n");
    write(&root, "src/page.ts", "export function show(s: Shared) {\n  return s.id\n}\n");
    let store = index(&root);
    let conn = store.conn();
    assert_eq!(
        uses(conn, "status", &["member"]),
        [
            ("src/client/use.ts:4".to_string(), "exact", vec!["ClientResponse.status".to_string()]),
            // The platform's `Response`: the test file imports nothing.
            ("src/server.test.ts:2".to_string(), "unresolved", vec![]),
        ]
    );
    assert_eq!(uses(conn, "json", &["call"]), [("src/server.test.ts:3".to_string(), "unresolved", vec![])]);
    let types = uses(conn, "Response", &["type_ref"]);
    assert!(types.contains(&("src/server.test.ts:1".to_string(), "unresolved", vec![])), "{types:?}");
    assert!(types.contains(&("src/client/use.ts:3".to_string(), "exact", vec!["Response".to_string()])), "{types:?}");
    // A script's declarations are global.
    assert_eq!(uses(conn, "Shared", &["type_ref"]), [("src/page.ts:1".to_string(), "unique_name", vec!["Shared".to_string()])]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A re-export names the module's declaration, and a receiver typed by an imported alias is the alias's type.
#[test]
fn a_reexport_and_an_imported_alias_still_name_the_repo() {
    let root = scratch("ts-visibility-reexport");
    write(&root, "src/item.ts", "export interface Item {\n  tags: string[]\n}\n");
    write(&root, "src/index.ts", "export { Item } from './item'\n");
    write(&root, "src/like.ts", "import type { Item } from './item'\nexport type ItemLike = Item\n");
    write(&root, "src/use.ts", "import type { ItemLike } from './like'\n\nexport function tags(i: unknown) {\n  return (i as ItemLike).tags\n}\n");
    let store = index(&root);
    let conn = store.conn();
    let items = uses(conn, "Item", &["type_ref"]);
    assert!(!items.iter().any(|(at, c, _)| at == "src/index.ts:1" && *c == "unresolved"), "{items:?}");
    let tags = uses(conn, "tags", &["member"]);
    assert!(!tags.iter().any(|(_, c, _)| *c == "unresolved"), "{tags:?}");
    let _ = std::fs::remove_dir_all(&root);
}
