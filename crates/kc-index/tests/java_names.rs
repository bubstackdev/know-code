//! Java name resolution as javac scopes names: packages and imports tell same-named types apart, a type or static
//! member from outside the repository never links to the repository's declaration of the same name, a local class
//! is seen only in its method, and a static import is shadowed by the caller's own members.

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

/// The references named `name` of `kind` written in `file`, in line order.
fn refs_in(conn: &rusqlite::Connection, name: &str, kind: &str, file: &str) -> Vec<RefHit> {
    let (mut refs, _) = query::refs(conn, name, &[kind], 50).unwrap();
    refs.retain(|r| r.file == file);
    refs.sort_by_key(|r| (r.line, r.col));
    refs
}

/// Confidence and target `file:line` of a reference.
fn answer(r: &RefHit) -> (&'static str, Vec<String>) {
    (r.confidence, r.targets.iter().map(|t| format!("{}:{}", t.file, t.line)).collect())
}

#[test]
fn packages_and_imports_tell_same_named_types_apart() {
    let root = scratch("java-packages");
    write(&root, "src/shop/model/Store.java", "package shop.model;\npublic class Store { public void save() {} }\n");
    write(&root, "src/shop/legacy/Store.java", "package shop.legacy;\npublic class Store { public void save() {} }\n");
    write(&root, "src/shop/legacy/Ledger.java", "package shop.legacy;\npublic class Ledger {}\n");
    write(
        &root,
        "src/shop/web/Checkout.java",
        "package shop.web;\n\
         import shop.model.Store;\n\
         import shop.legacy.*;\n\
         class Checkout {\n\
           Store store;\n\
           Ledger ledger;\n\
           void run(Store s) { s.save(); }\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    let types = refs_in(c, "Store", "type_ref", "src/shop/web/Checkout.java");
    assert_eq!(answer(&types[0]), ("exact", vec!["src/shop/model/Store.java:2".to_string()]));
    let ledger = refs_in(c, "Ledger", "type_ref", "src/shop/web/Checkout.java");
    assert_eq!(answer(&ledger[0]), ("exact", vec!["src/shop/legacy/Ledger.java:2".to_string()]), "on-demand import");
    let save = refs_in(c, "save", "call", "src/shop/web/Checkout.java");
    assert_eq!(answer(&save[0]), ("exact", vec!["src/shop/model/Store.java:2".to_string()]), "the parameter's imported type");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_name_from_outside_never_links_to_the_repositorys_namesake() {
    let root = scratch("java-outside");
    write(&root, "src/util/List.java", "package util;\npublic class List { public static List of() { return null; } }\n");
    write(&root, "src/math/Fraction.java", "package math;\npublic class Fraction { public static final Fraction ONE = null; }\n");
    write(&root, "src/test/Asserts.java", "package test;\npublic class Asserts { public static void assertEquals(Object a, Object b) {} }\n");
    write(
        &root,
        "src/app/Main.java",
        "package app;\n\
         import java.util.List;\n\
         import java.math.BigInteger;\n\
         import static org.junit.Assert.assertEquals;\n\
         class Main {\n\
           List<String> names = List.of();\n\
           void run() { assertEquals(BigInteger.ONE, null); }\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    for (name, kind) in [("List", "type_ref"), ("of", "call"), ("ONE", "member"), ("assertEquals", "call")] {
        let hits = refs_in(c, name, kind, "src/app/Main.java");
        assert!(!hits.is_empty(), "{name}");
        for h in &hits {
            assert_eq!(answer(h), ("unresolved", vec![]), "{name} at line {}", h.line);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_static_import_is_shadowed_by_the_callers_own_members() {
    let root = scratch("java-static");
    write(&root, "src/test/Checks.java", "package test;\npublic class Checks { public static void check(Object o) {} }\n");
    write(
        &root,
        "src/app/Own.java",
        "package app;\n\
         import static test.Checks.check;\n\
         class Own {\n\
           void check(Object o) {}\n\
           void run() { check(this); }\n\
         }\n",
    );
    write(
        &root,
        "src/app/Imported.java",
        "package app;\n\
         import static test.Checks.check;\n\
         class Imported {\n\
           void run() { check(this); }\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    let own = refs_in(c, "check", "call", "src/app/Own.java");
    assert_eq!(answer(&own[0]), ("exact", vec!["src/app/Own.java:4".to_string()]));
    let imported = refs_in(c, "check", "call", "src/app/Imported.java");
    assert_eq!(answer(&imported[0]), ("exact", vec!["src/test/Checks.java:2".to_string()]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_local_class_is_seen_only_in_its_method() {
    let root = scratch("java-local");
    write(
        &root,
        "src/app/Tests.java",
        "package app;\n\
         class Tests {\n\
           void first() {\n\
             class Dummy {}\n\
             Object a = new Dummy();\n\
           }\n\
           void second() {\n\
             class Dummy {}\n\
             Object b = new Dummy();\n\
           }\n\
         }\n",
    );
    let store = index(&root);
    let made = refs_in(store.conn(), "Dummy", "instantiate", "src/app/Tests.java");
    assert_eq!(answer(&made[0]), ("exact", vec!["src/app/Tests.java:4".to_string()]));
    assert_eq!(answer(&made[1]), ("exact", vec!["src/app/Tests.java:8".to_string()]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_field_access_never_means_a_method() {
    // A class extending a library type reads the library's inherited field while declaring a method of that name.
    let root = scratch("java-namespaces");
    write(
        &root,
        "src/app/Rows.java",
        "package app;\n\
         import java.util.AbstractList;\n\
         abstract class Rows extends AbstractList<String> {\n\
           int modCount() { return this.modCount; }\n\
           void touch() { this.modCount++; modCount(); }\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    for h in refs_in(c, "modCount", "member", "src/app/Rows.java") {
        assert_eq!(answer(&h), ("unresolved", vec![]), "line {}", h.line);
    }
    let call = refs_in(c, "modCount", "call", "src/app/Rows.java");
    assert_eq!(answer(&call[0]), ("exact", vec!["src/app/Rows.java:4".to_string()]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_chain_is_typed_through_declarations_and_member_types() {
    let root = scratch("java-chains");
    write(&root, "src/b/Builder.java", "package b;\npublic interface Builder<T> {\n  T build();\n}\n");
    write(
        &root,
        "src/b/Report.java",
        "package b;\n\
         public class Report {\n\
           public static Builder builder() { return new Builder(); }\n\
           public static final class Builder {\n\
             public Builder title(String t) { return this; }\n\
             public Report build() { return new Report(); }\n\
           }\n\
         }\n",
    );
    write(&root, "src/a/Store.java", "package a;\npublic class Store { public void save() {} }\n");
    write(&root, "src/c/Store.java", "package c;\npublic class Store { public void save() {} }\n");
    write(
        &root,
        "src/b/Use.java",
        "package b;\n\
         import c.Store;\n\
         class Use {\n\
           Store store;\n\
           Report make() { store.save(); return Report.builder().title(\"x\").build(); }\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    let build = refs_in(c, "build", "call", "src/b/Use.java");
    assert_eq!(answer(&build[0]), ("exact", vec!["src/b/Report.java:6".to_string()]), "the nested Builder's, not the interface's");
    let save = refs_in(c, "save", "call", "src/b/Use.java");
    assert_eq!(answer(&save[0]), ("exact", vec!["src/c/Store.java:2".to_string()]), "the field's imported type");
    let _ = std::fs::remove_dir_all(&root);
}
