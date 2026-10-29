//! Kotlin name resolution, on Java's model and with Kotlin's own rules: top-level declarations by package and import,
//! import aliases, a class's companion, extensions only on a receiver of the repository's type, a bare name in an
//! extension's body read on its receiver, and Kotlin's default imports outside the repository. Java and Kotlin see
//! each other as one family.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query::{self, RefHit};

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

fn refs_in(conn: &rusqlite::Connection, name: &str, kind: &str, file: &str) -> Vec<RefHit> {
    let (mut refs, _) = query::refs(conn, name, &[kind], 50).unwrap();
    refs.retain(|r| r.file == file);
    refs.sort_by_key(|r| (r.line, r.col));
    refs
}

fn answer(r: &RefHit) -> (&'static str, Vec<String>) {
    (r.confidence, r.targets.iter().map(|t| format!("{}:{}", t.file, t.line)).collect())
}

#[test]
fn top_level_names_by_package_import_and_alias() {
    let root = scratch("kt-packages");
    write(&root, "src/shop/util/Strings.kt", "package shop.util\n\nfun slug(s: String): String = s\n");
    write(&root, "src/shop/web/Paths.kt", "package shop.web\n\nfun route(): String = \"/\"\n");
    write(&root, "src/shop/model/Item.kt", "package shop.model\n\nclass Item(val sku: String)\n");
    write(
        &root,
        "src/shop/web/Page.kt",
        "package shop.web\n\
         \n\
         import shop.util.slug\n\
         import shop.model.Item as Sku\n\
         \n\
         fun render(i: Sku): String {\n\
           require(i.sku.isNotEmpty())\n\
           return slug(route())\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    let page = "src/shop/web/Page.kt";
    assert_eq!(answer(&refs_in(c, "slug", "call", page)[0]), ("exact", vec!["src/shop/util/Strings.kt:3".to_string()]));
    assert_eq!(answer(&refs_in(c, "route", "call", page)[0]), ("exact", vec!["src/shop/web/Paths.kt:3".to_string()]), "same package");
    assert_eq!(answer(&refs_in(c, "Sku", "type_ref", page)[0]), ("exact", vec!["src/shop/model/Item.kt:3".to_string()]), "an import alias");
    assert_eq!(answer(&refs_in(c, "sku", "member", page)[0]), ("exact", vec!["src/shop/model/Item.kt:3".to_string()]));
    // Kotlin's default imports are outside the repository.
    assert_eq!(answer(&refs_in(c, "require", "call", page)[0]), ("unresolved", vec![]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn companions_extensions_and_extension_bodies() {
    let root = scratch("kt-extensions");
    write(&root, "src/a/Cart.kt", "package a\n\nclass Cart {\n  val size: Int = 0\n}\n");
    write(&root, "src/a/Carts.kt", "package a\n\nfun Cart.isEmpty(): Boolean = size == 0\n\nfun String.shout(): String = this\n");
    write(&root, "src/a/Other.kt", "package a\n\nconst val LIMIT = 9\n");
    write(
        &root,
        "src/a/Use.kt",
        "package a\n\
         \n\
         class Use {\n\
           fun run(c: Cart): Boolean {\n\
             \"x\".shout()\n\
             check(LIMIT > 0)\n\
             return c.isEmpty()\n\
           }\n\
           companion object {\n\
             private const val LIMIT = 3\n\
           }\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    let use_kt = "src/a/Use.kt";
    assert_eq!(answer(&refs_in(c, "isEmpty", "call", use_kt)[0]), ("exact", vec!["src/a/Carts.kt:3".to_string()]), "an extension on the repository's type");
    // On a library type the extension is a candidate: the type may declare a member of the name.
    assert_eq!(answer(&refs_in(c, "shout", "call", use_kt)[0]).0, "ambiguous");
    assert_eq!(answer(&refs_in(c, "LIMIT", "member", use_kt)[0]), ("exact", vec!["src/a/Use.kt:10".to_string()]), "the class's companion first");
    // In the extension's body, `size` is the receiver's member.
    assert_eq!(answer(&refs_in(c, "size", "member", "src/a/Carts.kt")[0]), ("exact", vec!["src/a/Cart.kt:4".to_string()]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn java_and_kotlin_resolve_as_one_family() {
    let root = scratch("kt-java");
    write(&root, "src/a/Store.kt", "package a\n\nclass Store {\n  fun save() {}\n}\n");
    write(&root, "src/a/Use.java", "package a;\n\nclass Use {\n  void run(Store s) { s.save(); }\n}\n");
    let store = index(&root);
    let c = store.conn();
    assert_eq!(answer(&refs_in(c, "save", "call", "src/a/Use.java")[0]), ("exact", vec!["src/a/Store.kt:4".to_string()]));
    let _ = std::fs::remove_dir_all(&root);
}
