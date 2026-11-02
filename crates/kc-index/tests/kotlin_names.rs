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

#[test]
fn imports_over_own_name_this_on_library_receivers_and_private_members() {
    let root = scratch("kt-heldout-shapes");
    // An explicit import wins over a top-level class's own name in its body.
    write(
        &root,
        "src/cfg/Loader.kt",
        "package cfg\n\
         \n\
         import org.lib.Loader\n\
         \n\
         class Loader(val inner: Loader) {\n\
           class Part {\n\
             fun copy(p: Part): Part = p\n\
           }\n\
         }\n",
    );
    // `this.text` in an extension on a library type is the library's member, never a repository one; a private member,
    // or a member of a private type, is never seen from another file.
    write(
        &root,
        "src/node/Nodes.kt",
        "package node\n\
         \n\
         import org.lib.Node\n\
         import rules.Rule\n\
         \n\
         fun Node.rename(text: String) {\n\
           if (this.text != text) { }\n\
         }\n\
         \n\
         fun read(r: Rule, x: Any): String = r.label + x.code\n",
    );
    write(
        &root,
        "src/rules/Rule.kt",
        "package rules\n\
         \n\
         class Rule {\n\
           private val label: String = \"\"\n\
           private data class Ref(val text: String, val code: String)\n\
           fun own(): String = label\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    let loader = "src/cfg/Loader.kt";
    assert_eq!(answer(&refs_in(c, "Loader", "type_ref", loader)[0]), ("unresolved", vec![]), "the imported Loader");
    assert_eq!(answer(&refs_in(c, "Part", "type_ref", loader)[0]), ("exact", vec!["src/cfg/Loader.kt:6".to_string()]), "nested types unchanged");
    let nodes = "src/node/Nodes.kt";
    assert_eq!(answer(&refs_in(c, "text", "member", nodes)[0]), ("unresolved", vec![]), "a library receiver's member");
    assert_eq!(answer(&refs_in(c, "label", "member", nodes)[0]), ("unresolved", vec![]), "a private member");
    assert_eq!(answer(&refs_in(c, "code", "member", nodes)[0]), ("unresolved", vec![]), "a private type's member");
    assert_eq!(answer(&refs_in(c, "label", "member", "src/rules/Rule.kt")[0]), ("exact", vec!["src/rules/Rule.kt:4".to_string()]), "seen in its own type");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn inherited_members_by_declaration_and_receiver_lambdas() {
    let root = scratch("kt-exposed-shapes");
    // Two modules each declare `OrderTests`, on different bases: a bare call reads the caller's own base.
    write(&root, "a/src/shop/a/BaseA.kt", "package shop.a\n\nopen class BaseA {\n  fun prefix(): String = \"a\"\n}\n");
    write(&root, "b/src/shop/b/BaseB.kt", "package shop.b\n\nopen class BaseB {\n  fun prefix(): String = \"b\"\n}\n");
    write(&root, "a/src/shop/a/OrderTests.kt", "package shop.a\n\nclass OrderTests : BaseA() {\n  fun t() = prefix()\n}\n");
    write(&root, "b/src/shop/b/OrderTests.kt", "package shop.b\n\nclass OrderTests : BaseB() {\n  fun t() = prefix()\n}\n");
    // A supertype that can't be followed (a library's) may declare the name too: a candidate, not a link.
    write(
        &root,
        "a/src/shop/a/Mixed.kt",
        "package shop.a\n\nimport org.lib.Remote\n\ninterface Named {\n  fun label(): String = \"\"\n}\n\nclass Mixed : Remote(), Named {\n  fun t() = label()\n}\n",
    );
    // In a lambda with a receiver, the receiver's members come first: the enclosing class's are candidates only.
    write(
        &root,
        "a/src/shop/a/Svc.kt",
        "package shop.a\n\
         \n\
         class Conn\n\
         class Meta\n\
         \n\
         fun <T> meta(body: Meta.() -> T): T = TODO()\n\
         \n\
         class Svc {\n\
           val url: String = \"\"\n\
           fun close() {}\n\
           fun go(c: Conn) {\n\
             c.apply { close() }\n\
             meta { url }\n\
             listOf(1).forEach { close() }\n\
           }\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    assert_eq!(answer(&refs_in(c, "prefix", "call", "a/src/shop/a/OrderTests.kt")[0]), ("exact", vec!["a/src/shop/a/BaseA.kt:4".to_string()]));
    assert_eq!(answer(&refs_in(c, "prefix", "call", "b/src/shop/b/OrderTests.kt")[0]), ("exact", vec!["b/src/shop/b/BaseB.kt:4".to_string()]));
    assert_eq!(answer(&refs_in(c, "label", "call", "a/src/shop/a/Mixed.kt")[0]).0, "ambiguous", "an unfollowed supertype");
    let svc = "a/src/shop/a/Svc.kt";
    let closes = refs_in(c, "close", "call", svc);
    assert_eq!(answer(&closes[0]).0, "ambiguous", "in `apply`");
    assert_eq!(answer(&closes[1]), ("exact", vec![format!("{svc}:10")]), "a lambda without a receiver");
    assert_eq!(answer(&refs_in(c, "url", "member", svc)[0]).0, "ambiguous", "a repo function's `Meta.() -> T`");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_member_shadows_an_extension_only_where_it_fits_the_call() {
    let root = scratch("kt-member-arity");
    write(
        &root,
        "src/q/Query.kt",
        "package q\n\
         \n\
         import kotlin.reflect.KClass\n\
         \n\
         interface Query {\n\
           interface Factory {\n\
             fun <Q : Query> newQuery(queryClass: KClass<Q>): Q\n\
           }\n\
         }\n\
         \n\
         inline fun <reified T : Query> Query.Factory.newQuery(): T = newQuery(T::class)\n",
    );
    write(
        &root,
        "src/q/Use.kt",
        "package q\n\
         \n\
         class Use(private val factory: Query.Factory) {\n\
           fun go() {\n\
             factory.newQuery<Query>()\n\
             factory.newQuery(Query::class)\n\
           }\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    let calls = refs_in(c, "newQuery", "call", "src/q/Use.kt");
    assert_eq!(answer(&calls[0]), ("exact", vec!["src/q/Query.kt:11".to_string()]), "no argument: the extension");
    assert_eq!(answer(&calls[1]), ("exact", vec!["src/q/Query.kt:7".to_string()]), "one argument: the member");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_value_typed_by_a_call_sees_only_the_functions_its_file_can() {
    let root = scratch("kt-private-return");
    write(
        &root,
        "src/t/Transports.kt",
        "package t\n\nclass HttpTransport {\n  fun start() {}\n}\n\nclass SocketTransport {\n  fun start() {}\n}\n",
    );
    write(
        &root,
        "src/t/HttpTest.kt",
        "package t\n\
         \n\
         class HttpTest {\n\
           private fun build(retries: Int? = null) =\n\
             HttpTransport()\n\
         \n\
           fun go() {\n\
             val transport = build()\n\
             transport.start()\n\
           }\n\
         }\n",
    );
    write(
        &root,
        "src/t/SocketTest.kt",
        "package t\n\
         \n\
         class SocketTest {\n\
           private fun build(): SocketTransport {\n\
             return SocketTransport()\n\
           }\n\
         \n\
           fun go() {\n\
             val transport = build()\n\
             transport.start()\n\
           }\n\
         }\n",
    );
    // No written type and no constructor in the body: untyped here, never typed by the other file's `build`.
    write(
        &root,
        "src/t/OtherTest.kt",
        "package t\n\
         \n\
         class OtherTest {\n\
           private fun build() = Factory.make()\n\
         \n\
           fun go() {\n\
             val transport = build()\n\
             transport.start()\n\
           }\n\
         }\n",
    );
    let store = index(&root);
    let c = store.conn();
    assert_eq!(
        answer(&refs_in(c, "start", "call", "src/t/HttpTest.kt")[0]),
        ("exact", vec!["src/t/Transports.kt:4".to_string()]),
        "the constructor its own `build` returns"
    );
    let other = answer(&refs_in(c, "start", "call", "src/t/OtherTest.kt")[0]);
    assert_eq!(other.0, "ambiguous", "candidates, never typed by another file's private `build`: {other:?}");
    assert_eq!(
        answer(&refs_in(c, "start", "call", "src/t/SocketTest.kt")[0]),
        ("exact", vec!["src/t/Transports.kt:8".to_string()]),
        "its own file's `build`"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_member_inherited_by_a_typed_receiver_is_read_through_its_own_supertypes() {
    let root = scratch("kt-same-named");
    write(&root, "m/src/m/flags/Flags.kt", "package m.flags\n\ninterface Flags {\n  fun getDouble(key: String, scope: String): Double\n}\n");
    // w's `Flags` inherits the method from an interface declared beside it; m's declares its own.
    write(
        &root,
        "w/src/w/flags/Flags.kt",
        "package w.flags\n\ninterface Flags : Legacy\n\ninterface Legacy {\n  fun getDouble(key: String, scope: String): Double\n}\n",
    );
    write(&root, "w/src/w/flags/Dynamic.kt", "package w.flags\n\ninterface Dynamic {\n  fun getDouble(key: String): Double\n}\n");
    write(&root, "m/src/m/flags/Dynamic.kt", "package m.flags\n\ninterface Dynamic {\n  fun getDouble(key: String): Double\n}\n");
    write(
        &root,
        "m/src/m/ld/Config.kt",
        "package m.ld\n\nimport m.flags.Dynamic\nimport m.flags.Flags\n\nclass Config(private val flags: Flags) : Dynamic {\n  override fun getDouble(key: String): Double = flags.getDouble(key, \"s\")\n}\n",
    );
    write(
        &root,
        "w/src/w/ld/Config.kt",
        "package w.ld\n\nimport w.flags.*\n\nclass Config(private val flags: Flags) : Dynamic {\n  override fun getDouble(key: String): Double = flags.getDouble(key, \"s\")\n}\n",
    );
    let store = index(&root);
    let c = store.conn();
    assert_eq!(
        answer(&refs_in(c, "getDouble", "call", "w/src/w/ld/Config.kt")[0]),
        ("exact", vec!["w/src/w/flags/Flags.kt:6".to_string()]),
        "the star import's Flags, through its supertype"
    );
    assert_eq!(
        answer(&refs_in(c, "getDouble", "call", "m/src/m/ld/Config.kt")[0]),
        ("exact", vec!["m/src/m/flags/Flags.kt:4".to_string()])
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn super_reaches_only_the_supertypes_members() {
    let root = scratch("kt-super");
    // On a library supertype, `super.close()` is the library's member, never the class's own override.
    write(
        &root,
        "src/io/Pipe.kt",
        "package io\n\nimport org.lib.Stream\n\nclass Pipe : Stream() {\n  override fun close() {\n    super.close()\n  }\n}\n",
    );
    write(&root, "src/io/Base.kt", "package io\n\nopen class Base {\n  open fun flush() {}\n}\n");
    write(
        &root,
        "src/io/Child.kt",
        "package io\n\nclass Child : Base() {\n  override fun flush() {\n    super.flush()\n  }\n}\n",
    );
    let store = index(&root);
    let c = store.conn();
    assert_eq!(answer(&refs_in(c, "close", "call", "src/io/Pipe.kt")[0]), ("unresolved", vec![]), "a library supertype's member");
    assert_eq!(
        answer(&refs_in(c, "flush", "call", "src/io/Child.kt")[0]),
        ("exact", vec!["src/io/Base.kt:4".to_string()]),
        "the repository supertype's member"
    );
    let _ = std::fs::remove_dir_all(&root);
}
