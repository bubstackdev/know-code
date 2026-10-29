//! The scope a name resolves in: a reference binds only to definitions in a language it sees, in C# only to those of
//! the projects its own references, and a generic service registered per type argument binds per type argument.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query::{self, RefHit};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-scope-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    dir
}

fn index(root: &Path, files: &[(&str, &str)]) -> Store {
    for (path, text) in files {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    store
}

fn project(refs: &[&str]) -> String {
    let items: String = refs.iter().map(|r| format!("<ProjectReference Include=\"{r}\" />")).collect();
    format!("<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup>{items}</ItemGroup></Project>\n")
}

/// The calls of `name` made from `caller`'s file.
fn call(store: &Store, name: &str, file: &str) -> RefHit {
    let (hits, _) = query::refs(store.conn(), name, &["call"], 50).unwrap();
    hits.into_iter().find(|h| h.file == file).unwrap_or_else(|| panic!("no call of {name} in {file}"))
}

fn targets(h: &RefHit) -> Vec<&str> {
    h.targets.iter().map(|t| t.file.as_str()).collect()
}

#[test]
fn a_name_never_resolves_to_another_languages_definition() {
    let root = scratch("languages");
    let store = index(
        &root,
        &[
            ("only.py", "def foreign():\n    return 1\n"),
            ("main.rs", "fn run() { foreign(); }\n"),
            // TypeScript and JavaScript share one module system: a bare call sees the other's definitions.
            ("util.js", "function shared() { return 1; }\n"),
            ("app.ts", "function start() { shared(); }\n"),
        ],
    );
    let h = call(&store, "foreign", "main.rs");
    assert_eq!((h.confidence, targets(&h)), ("unresolved", vec![]));
    let h = call(&store, "shared", "app.ts");
    assert_eq!(targets(&h), ["util.js"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// The family of languages that see each other by name, from a path.
fn family(path: &str) -> &str {
    match path.rsplit_once('.').map(|(_, e)| e) {
        Some("ts" | "tsx" | "js") => "js",
        Some(e) => e,
        None => "",
    }
}

#[test]
fn invariant_every_edge_resolved_by_name_stays_within_its_language_family() {
    // The same names defined and called in seven languages.
    let root = scratch("families");
    let store = index(
        &root,
        &[
            ("App.csproj", &project(&[])),
            ("a.py", "class Store:\n    pass\n\ndef shared():\n    return 1\n\ndef helper():\n    Store()\n    return shared()\n"),
            // One name only Python defines.
            ("b.rs", "struct Store;\nfn shared() {}\nfn helper() { shared(); lonely(); }\n"),
            ("g.py", "def lonely():\n    return 1\n"),
            ("c.swift", "class Store {}\nfunc shared() {}\nfunc helper() { _ = Store(); shared() }\n"),
            ("d.ts", "export class Store {}\nfunction shared() {}\nexport function helper() { new Store(); shared(); }\n"),
            ("e.js", "function shared() {}\nfunction helper() { shared(); lonely(); }\n"),
            ("F.cs", "public class Store { public static void shared() {} public void helper() { new Store(); shared(); } }\n"),
            ("X.java", "class Store { static void shared() {} void helper() { new Store(); shared(); lonely(); } }\n"),
        ],
    );
    let hits = query::refs_named(store.conn(), &["shared", "Store", "lonely"]).unwrap();
    assert!(hits.iter().filter(|h| !h.targets.is_empty()).count() >= 10, "{hits:#?}");
    for h in &hits {
        for t in &h.targets {
            assert_eq!(family(&h.file), family(&t.file), "{} at {}:{} resolved to {}", h.name, h.file, h.line, t.file);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

const WORKER: &str = "namespace Same; public class Worker { public void Run() {} }\n";
const CALLER: &str = "namespace Same; public class Caller { public void Start() { new Worker().Run(); } }\n";

#[test]
fn a_csharp_project_sees_only_itself_and_the_projects_it_references() {
    // Independent projects: the caller's project can't compile a call to the other's class.
    let root = scratch("independent");
    let store = index(&root, &[("a/A.csproj", &project(&[])), ("b/B.csproj", &project(&[])), ("a/A.cs", WORKER), ("b/B.cs", CALLER)]);
    let h = call(&store, "Run", "b/B.cs");
    assert_eq!((h.confidence, targets(&h)), ("unresolved", vec![]));
    let _ = std::fs::remove_dir_all(&root);

    // A reference, through another project: exact.
    let root = scratch("referenced");
    let store = index(
        &root,
        &[
            ("a/A.csproj", &project(&[])),
            ("m/M.csproj", &project(&["..\\a\\A.csproj"])),
            ("b/B.csproj", &project(&["..\\m\\M.csproj"])),
            ("a/A.cs", WORKER),
            ("b/B.cs", CALLER),
        ],
    );
    let h = call(&store, "Run", "b/B.cs");
    assert_eq!((h.confidence, targets(&h)), ("exact", vec!["a/A.cs"]));
    let _ = std::fs::remove_dir_all(&root);

    // Of two same-named classes, the one in the project referenced, never the other.
    let root = scratch("two-workers");
    let store = index(
        &root,
        &[
            ("a/A.csproj", &project(&[])),
            ("c/C.csproj", &project(&[])),
            ("b/B.csproj", &project(&["..\\a\\A.csproj"])),
            ("a/A.cs", WORKER),
            ("c/C.cs", WORKER),
            ("b/B.cs", CALLER),
        ],
    );
    let h = call(&store, "Run", "b/B.cs");
    assert_eq!((h.confidence, targets(&h)), ("exact", vec!["a/A.cs"]));
    // The type, the scope having ruled out its namesake, is the compiler's pick as when namespaces narrow.
    let (made, _) = query::refs(store.conn(), "Worker", &["instantiate"], 10).unwrap();
    assert_eq!((made[0].confidence, targets(&made[0])), ("exact", vec!["a/A.cs"]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn invariant_without_a_project_to_decide_a_csharp_binding_is_never_exact() {
    // No project file at all: the repo's classes may be what the caller means, but no compiler said so.
    let root = scratch("no-project");
    let store = index(&root, &[("a/A.cs", WORKER), ("b/B.cs", CALLER)]);
    let h = call(&store, "Run", "b/B.cs");
    assert_eq!((h.confidence, targets(&h)), ("unique_name", vec!["a/A.cs"]));
    let _ = std::fs::remove_dir_all(&root);

    // A reference through an MSBuild property the index can't place: the project may see more than it knows.
    let root = scratch("open-reference");
    let store = index(
        &root,
        &[("a/A.csproj", &project(&[])), ("b/B.csproj", &project(&["$(Root)\\a\\A.csproj"])), ("a/A.cs", WORKER), ("b/B.cs", CALLER)],
    );
    let h = call(&store, "Run", "b/B.cs");
    assert_eq!((h.confidence, targets(&h)), ("unique_name", vec!["a/A.cs"]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_linked_source_and_a_shared_project_are_compiled_by_the_project_linking_them() {
    let root = scratch("linked");
    let linking = "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup><Compile Include=\"..\\shared\\Worker.cs\" Link=\"Worker.cs\" /></ItemGroup>\
                   <Import Project=\"..\\common\\Common.projitems\" Label=\"Shared\" /></Project>\n";
    let store = index(
        &root,
        &[
            ("b/B.csproj", linking),
            ("shared/Worker.cs", WORKER),
            ("common/Common.projitems", "<Project></Project>\n"),
            ("common/Clock.cs", "namespace Same; public class Clock { public void Tick() {} }\n"),
            ("b/B.cs", "namespace Same; public class Caller { public void Start() { new Worker().Run(); new Clock().Tick(); } }\n"),
        ],
    );
    let h = call(&store, "Run", "b/B.cs");
    assert_eq!((h.confidence, targets(&h)), ("exact", vec!["shared/Worker.cs"]));
    let h = call(&store, "Tick", "b/B.cs");
    assert_eq!((h.confidence, targets(&h)), ("exact", vec!["common/Clock.cs"]));
    let _ = std::fs::remove_dir_all(&root);
}

const HANDLERS: &str = r#"
public interface IHandler<T> { void Handle(); }
public interface IRepo<T> { void Save(); }
public class Order {}
public class Invoice {}
public class OrderHandler : IHandler<Order> { public void Handle() {} }
public class InvoiceHandler : IHandler<Invoice> { public void Handle() {} }
public class Repo<T> : IRepo<T> { public void Save() {} }
public class Startup
{
    public void Setup(IServiceCollection s)
    {
        s.AddScoped<IHandler<Order>, OrderHandler>();
        s.AddScoped<IHandler<Invoice>, InvoiceHandler>();
        s.AddScoped(typeof(IRepo<>), typeof(Repo<>));
    }
}
public class Caller
{
    IHandler<Order> orders;
    IHandler<Invoice> invoices;
    IRepo<Order> repo;
    public void Run() { orders.Handle(); invoices.Handle(); repo.Save(); }
}
"#;

#[test]
fn a_generic_service_is_bound_per_type_argument() {
    let root = scratch("generic-di");
    let store = index(&root, &[("App.csproj", &project(&[])), ("App.cs", HANDLERS)]);
    let (hits, _) = query::refs(store.conn(), "Handle", &["call"], 10).unwrap();
    let bound: Vec<Option<String>> = hits.iter().map(|h| h.via_di.as_ref().map(|b| b.target.qualified.clone())).collect();
    assert_eq!(bound, [Some("OrderHandler.Handle".to_string()), Some("InvoiceHandler.Handle".to_string())]);
    // An open generic registration serves every closed one.
    let h = call(&store, "Save", "App.cs");
    assert_eq!(h.via_di.map(|b| b.target.qualified), Some("Repo.Save".to_string()));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn same_named_services_in_two_namespaces_are_two_services() {
    let root = scratch("namespaced-di");
    let store = index(
        &root,
        &[
            ("App.csproj", &project(&[])),
            ("Billing.cs", "namespace Billing;\npublic interface INotifier { void Notify(); }\npublic class Mailer : INotifier { public void Notify() {} }\n"),
            ("Shipping.cs", "namespace Shipping;\npublic interface INotifier { void Notify(); }\npublic class Pager : INotifier { public void Notify() {} }\n"),
            (
                "Startup.cs",
                "using Billing;\nnamespace Host;\npublic class Startup { public void Setup(IServiceCollection s) { s.AddScoped<INotifier, Mailer>(); s.AddScoped<Shipping.INotifier, Shipping.Pager>(); } }\n",
            ),
            ("Invoices.cs", "namespace Billing;\npublic class Invoices { INotifier n; public void Run() { n.Notify(); } }\n"),
        ],
    );
    let h = call(&store, "Notify", "Invoices.cs");
    assert_eq!(h.targets[0].qualified, "INotifier.Notify");
    assert_eq!(h.via_di.map(|b| b.target.qualified), Some("Mailer.Notify".to_string()));
    let _ = std::fs::remove_dir_all(&root);
}
