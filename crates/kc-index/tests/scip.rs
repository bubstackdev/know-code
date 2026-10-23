//! End to end with a SCIP index: a scratch C# repo, an `index.scip` written for it, then precise references,
//! corrections, implementations, and staleness after an edit.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::scip::encode;
use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query::{self, RefHit};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    dir
}

const IGREETER: &str = "namespace Demo;
public interface IGreeter
{
    string Greet();
}
";

// `Hidden` implements `IGreeter` only through `Base`, which name-based dispatch doesn't follow.
const BASE: &str = "namespace Demo;
public abstract class Base : IGreeter
{
    public abstract string Greet();
}
public class Hidden : Base
{
    public override string Greet() => \"hidden\";
}
public class Mailer
{
    public void Send() { }
}
";

const OTHER: &str = "namespace Demo;
public class Other
{
    public string Greet() => \"other\";
}
";

// `client` comes from a library: its `Send` isn't `Mailer.Send`, though that's the only `Send` in the repo.
const APP: &str = "namespace Demo;
public class App
{
    public string Run(IGreeter g)
    {
        var client = Connect();
        client.Send();
        Pick().Greet();
        return g.Greet();
    }
}
";

const PKG: &str = "scip-dotnet nuget Demo 1.0 ";

/// An occurrence of `token` on 0-based `line` of `text` (its first, or its `nth`).
fn occ(text: &str, line: usize, token: &str, nth: usize, symbol: &str, roles: i32) -> Vec<u8> {
    let l = text.lines().nth(line).unwrap();
    let col = l.match_indices(token).nth(nth).unwrap().0 as i32;
    encode::occurrence(&[line as i32, col, col + token.len() as i32], symbol, roles)
}

fn sym(descriptors: &str) -> String {
    format!("{PKG}{descriptors}")
}

fn scip_index(root: &Path) -> Vec<u8> {
    let def = |text, line, token, s: &str| occ(text, line, token, 0, &sym(s), 1);
    let docs = [
        encode::document("IGreeter.cs", &[def(IGREETER, 1, "IGreeter", "Demo/IGreeter#"), def(IGREETER, 3, "Greet", "Demo/IGreeter#Greet().")], &[]),
        encode::document(
            "Base.cs",
            &[
                def(BASE, 1, "Base", "Demo/Base#"),
                occ(BASE, 1, "IGreeter", 0, &sym("Demo/IGreeter#"), 0),
                def(BASE, 3, "Greet", "Demo/Base#Greet()."),
                def(BASE, 5, "Hidden", "Demo/Hidden#"),
                def(BASE, 7, "Greet", "Demo/Hidden#Greet()."),
                def(BASE, 9, "Mailer", "Demo/Mailer#"),
                def(BASE, 11, "Send", "Demo/Mailer#Send()."),
            ],
            &[
                encode::symbol(&sym("Demo/Base#Greet()."), &[&sym("Demo/IGreeter#Greet().")]),
                encode::symbol(&sym("Demo/Hidden#Greet()."), &[&sym("Demo/IGreeter#Greet().")]),
            ],
        ),
        encode::document("Other.cs", &[def(OTHER, 1, "Other", "Demo/Other#"), def(OTHER, 3, "Greet", "Demo/Other#Greet().")], &[]),
        encode::document(
            "App.cs",
            &[
                def(APP, 1, "App", "Demo/App#"),
                occ(APP, 3, "IGreeter", 0, &sym("Demo/IGreeter#"), 0),
                def(APP, 3, "Run", "Demo/App#Run()."),
                occ(APP, 5, "client", 0, "local 0", 1),
                occ(APP, 6, "client", 0, "local 0", 0),
                occ(APP, 6, "Send", 0, "scip-dotnet nuget Mail 2.0 Mail/Client#Send().", 0),
                occ(APP, 7, "Greet", 0, &sym("Demo/Other#Greet()."), 0),
                occ(APP, 8, "Greet", 0, &sym("Demo/IGreeter#Greet()."), 0),
            ],
            &[],
        ),
    ];
    encode::index("scip-dotnet", &format!("file://{}", root.display()), &docs)
}

fn hit(conn: &rusqlite::Connection, name: &str, line: u32) -> RefHit {
    let (hits, _) = query::refs(conn, name, &[], 50).unwrap();
    hits.into_iter().find(|h| h.file == "App.cs" && h.line == line).unwrap_or_else(|| panic!("no {name} at App.cs:{line}"))
}

#[test]
fn scip_answers_win_until_the_file_changes() {
    let root = scratch("scip");
    for (path, text) in [("IGreeter.cs", IGREETER), ("Base.cs", BASE), ("Other.cs", OTHER), ("App.cs", APP)] {
        std::fs::write(root.join(path), text).unwrap();
    }
    std::fs::write(root.join("App.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n").unwrap();
    // Written after the sources, as an indexer would.
    std::fs::write(root.join("index.scip"), scip_index(&root)).unwrap();
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    let opts = SyncOptions { threads: 1, scip_compare: true, ..Default::default() };
    let report = sync(&root, &mut store, &opts).unwrap().scip.expect("index.scip is read");
    assert_eq!((report.documents, report.covered, report.stale, report.error.as_deref()), (4, 4, 0, None));
    assert_eq!(report.impls, 2);
    let a = report.agreement.expect("compared");
    let cell = |conf: &str, class: &str| a.matrix.get(conf).and_then(|r| r.get(class)).copied().unwrap_or(0);
    assert_eq!(cell("unique_name", "external_false"), 1, "Mailer.Send was a guess: {:?}", a.matrix);
    assert_eq!(cell("unresolved", "missed"), 1, "Pick() has no definition to type it: {:?}", a.matrix);
    assert_eq!(cell("exact", "same"), 1, "{:?}", a.matrix);
    assert_eq!((a.corrected, a.member_impls, a.member_impls_declared), (2, 2, 1), "only Base declares IGreeter");
    let sites: Vec<&str> = a.disagreements.iter().map(|d| d.site.as_str()).collect();
    assert_eq!(sites, ["App.cs:7", "App.cs:8"]);

    // Unchanged: no second read.
    assert!(sync(&root, &mut store, &opts).unwrap().scip.is_none());

    let conn = store.conn();
    // A library call the heuristics pinned on the repo's only `Send`: corrected, and says what it had been.
    let send = hit(conn, "Send", 7);
    assert_eq!((send.confidence, send.precise, send.corrected_from), ("unresolved", true, Some("unique_name")));
    assert!(send.targets.is_empty());
    // A receiver the heuristics can't type: found.
    let pick = hit(conn, "Greet", 8);
    assert_eq!((pick.confidence, pick.precise, pick.corrected_from), ("exact", true, Some("unresolved")));
    assert_eq!(pick.targets[0].qualified, "Other.Greet");
    // Agreement: still precise.
    let greet = hit(conn, "Greet", 9);
    assert_eq!((greet.confidence, greet.precise, greet.targets[0].qualified.as_str()), ("exact", true, "IGreeter.Greet"));

    // The edges walks read carry it too.
    let run = query::definitions(conn, "App.Run").unwrap().remove(0);
    let mut cache = kc_store::trace::EdgeCache::default();
    let out = kc_store::trace::out_edges(conn, &mut cache, &run).unwrap();
    let (other, _, _, site) = out.iter().find(|e| e.0.qualified() == "Other.Greet").expect("Run calls Other.Greet");
    assert_eq!(site.as_deref(), Some("App.cs:8"));
    assert!(cache.precise(other.id, site.as_deref()));
    // Interface dispatch: `Hidden.Greet` implements `IGreeter.Greet` through `Base`.
    let member = query::definitions(conn, "IGreeter.Greet").unwrap().into_iter().find(|s| s.kind == "method").unwrap();
    let impls: Vec<String> = kc_store::trace::implementations(conn, &member).unwrap().iter().map(|s| s.qualified()).collect();
    assert!(impls.contains(&"Hidden.Greet".to_string()), "{impls:?}");

    let cov = kc_store::scip::coverage(conn).unwrap();
    assert_eq!((cov.files, cov.current), (4, 4));

    // An edit retires the SCIP facts of the file's whole project (here every C# file: the one project at the root),
    // since any input may change a binding; the heuristics answer again.
    std::fs::write(root.join("App.cs"), format!("{APP}// edited\n")).unwrap();
    sync(&root, &mut store, &opts).unwrap();
    let conn = store.conn();
    let send = hit(conn, "Send", 7);
    assert_eq!((send.confidence, send.precise, send.corrected_from), ("unique_name", false, None));
    let cov = kc_store::scip::coverage(conn).unwrap();
    assert_eq!((cov.current, cov.stale_projects.as_slice()), (0, ["csharp:".to_string()].as_slice()));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unreadable_scip_file_is_reported_not_fatal() {
    let root = scratch("scip-bad");
    std::fs::write(root.join("App.cs"), APP).unwrap();
    std::fs::write(root.join("other.scip"), [0x12, 0x40, 0x01]).unwrap();
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    kc_index::scip::set_source(&store, &root.join("other.scip")).unwrap();
    let report = sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap().scip.unwrap();
    assert!(report.error.as_deref().is_some_and(|e| e.contains("truncated")), "{:?}", report.error);
    assert!(!hit(store.conn(), "Send", 7).precise);
    assert!(kc_index::scip::set_source(&store, &root.join("missing.scip")).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

/// A document with one definition occurrence per `(line, token, symbol)`.
fn defs_doc(path: &str, text: &str, defs: &[(usize, &str, &str)]) -> Vec<u8> {
    encode::document(path, &defs.iter().map(|(l, t, s)| occ(text, *l, t, 0, &sym(s), 1)).collect::<Vec<_>>(), &[])
}

#[test]
fn a_change_to_any_compilation_input_retires_the_scip_facts() {
    // The review's case: a `global using` alias in a third file decides what the caller's `Save` binds to.
    let root = scratch("scip-inputs");
    let a = "namespace A;\npublic class Worker { public void Save() {} }\n";
    let b = "namespace B;\npublic class Worker { public void Save() {} }\n";
    let caller = "public class Caller { public void Run(Selected w) { w.Save(); } }\n";
    for (path, text) in [("A.cs", a), ("B.cs", b), ("Caller.cs", caller), ("Global.cs", "global using Selected = A.Worker;\n")] {
        std::fs::write(root.join(path), text).unwrap();
    }
    let docs = [
        defs_doc("A.cs", a, &[(1, "Save", "A/Worker#Save().")]),
        defs_doc("B.cs", b, &[(1, "Save", "B/Worker#Save().")]),
        encode::document("Caller.cs", &[occ(caller, 0, "Save", 0, &sym("A/Worker#Save()."), 0)], &[]),
    ];
    std::fs::write(root.join("index.scip"), encode::index("scip-dotnet", &format!("file://{}", root.display()), &docs)).unwrap();
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    let opts = SyncOptions { threads: 1, ..Default::default() };
    sync(&root, &mut store, &opts).unwrap();
    let save = |store: &Store| {
        let (hits, _) = query::refs(store.conn(), "Save", &[], 50).unwrap();
        hits.into_iter().find(|h| h.file == "Caller.cs").unwrap()
    };
    let h = save(&store);
    assert_eq!((h.precise, h.targets[0].file.as_str()), (true, "A.cs"));
    // Only the alias changes: neither endpoint file does.
    std::fs::write(root.join("Global.cs"), "global using Selected = B.Worker;\n").unwrap();
    sync(&root, &mut store, &opts).unwrap();
    let h = save(&store);
    assert!(!h.precise && h.targets.iter().all(|t| t.file != "A.cs"), "{h:?}");
    assert_eq!(kc_store::scip::coverage(store.conn()).unwrap().stale_projects, ["csharp:*"]);
    // Put back as the indexer saw it, the facts apply again.
    std::fs::write(root.join("Global.cs"), "global using Selected = A.Worker;\n").unwrap();
    sync(&root, &mut store, &opts).unwrap();
    assert!(save(&store).precise);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn invariant_projects_go_stale_with_their_own_inputs_and_their_references() {
    // Lib; App references Lib; Tool stands alone.
    let root = scratch("scip-projects");
    let csproj = |refs: &str| format!("<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>{refs}</ItemGroup>\n</Project>\n");
    let files: Vec<(&str, String)> = vec![
        ("Directory.Build.props", "<Project />\n".into()),
        ("lib/Lib.csproj", csproj("")),
        ("lib/Store.cs", "namespace Lib;\npublic class Store { public void Save() {} }\n".into()),
        ("app/App.csproj", csproj("<ProjectReference Include=\"..\\lib\\Lib.csproj\" />")),
        ("app/packages.lock.json", "{}\n".into()),
        ("app/Main.cs", "namespace App;\npublic class Main { public void Run() {} }\n".into()),
        ("tool/Tool.csproj", csproj("")),
        ("tool/Cli.cs", "namespace Tool;\npublic class Cli { public void Go() {} }\n".into()),
    ];
    for (path, text) in &files {
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join(path), text).unwrap();
    }
    let docs = [
        defs_doc("lib/Store.cs", &files[2].1, &[(1, "Save", "Lib/Store#Save().")]),
        defs_doc("app/Main.cs", &files[5].1, &[(1, "Run", "App/Main#Run().")]),
        defs_doc("tool/Cli.cs", &files[7].1, &[(1, "Go", "Tool/Cli#Go().")]),
    ];
    std::fs::write(root.join("index.scip"), encode::index("scip-dotnet", &format!("file://{}", root.display()), &docs)).unwrap();
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    let opts = SyncOptions { threads: 1, ..Default::default() };
    sync(&root, &mut store, &opts).unwrap();
    let stale = |store: &mut Store| {
        sync(&root, store, &opts).unwrap();
        kc_store::scip::coverage(store.conn()).unwrap().stale_projects
    };
    assert!(stale(&mut store).is_empty());
    // Invariant: changing any input of a project retires it and every project referencing it, and nothing else;
    // putting the input back restores them.
    let cases: [(&str, &[&str]); 7] = [
        ("tool/Cli.cs", &["csharp:tool"]),
        ("app/Main.cs", &["csharp:app"]),
        ("app/App.csproj", &["csharp:app"]),
        ("app/packages.lock.json", &["csharp:app"]),
        ("lib/Store.cs", &["csharp:app", "csharp:lib"]),
        ("lib/Lib.csproj", &["csharp:app", "csharp:lib"]),
        ("Directory.Build.props", &["csharp:app", "csharp:lib", "csharp:tool"]),
    ];
    for (path, expected) in cases {
        let text = files.iter().find(|(p, _)| *p == path).unwrap().1.clone();
        let changed = if path.ends_with(".cs") { format!("{text}public class Added {{}}\n") } else { format!("{text}\n") };
        std::fs::write(root.join(path), changed).unwrap();
        assert_eq!(stale(&mut store), expected, "{path}");
        std::fs::write(root.join(path), &text).unwrap();
        assert!(stale(&mut store).is_empty(), "{path} restored");
    }
    let _ = std::fs::remove_dir_all(&root);
}
