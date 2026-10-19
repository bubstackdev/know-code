//! ColdFusion end to end on a scratch repo: includes, user-defined functions, components through dotted paths,
//! `extends` and `this.mappings`, custom tags, tables named in queries, and `<cfhttp>` requests as client routes.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::{Store, query, trace};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-cfml-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const PAGE: &str = r##"<cfinclude template="/_inc/header.cfm">
<cfinclude template="../_inc/functions.cfm">
<cfset svc = createObject("component", "model.UserService").init()>
<cfset u = svc.getUser(1)>
<cfset svc.save()>
<cfoutput>#FormatName(u.name)#</cfoutput>
<cf_box title="x">
<cfhttp url="#request.api.orders#/lines" method="post" result="r"></cfhttp>
"##;

fn setup(root: &Path) {
    write(root, "Application.cfc", "component {\n    this.name = \"shop\";\n    this.mappings[\"/lib\"] = expandPath(\"./shared/lib\");\n}\n");
    write(root, "shared/lib/Cache.cfc", "component {\n    public any function put(k, v) { return this; }\n}\n");
    write(
        root,
        "model/Base.cfc",
        "<cfcomponent>\n<cffunction name=\"save\" access=\"public\">\n  <cfquery name=\"q\">UPDATE Users SET n = 1</cfquery>\n</cffunction>\n</cfcomponent>\n",
    );
    write(
        root,
        "model/UserService.cfc",
        "component extends=\"model.Base\" {\n    public any function init() {\n        variables.cache = new lib.Cache();\n        return this;\n    }\n    public any function getUser(required numeric id) {\n        cache.put(id, 1);\n        return queryExecute(\"SELECT * FROM dbo.Users WHERE id = :id\", {id: id});\n    }\n}\n",
    );
    write(root, "_inc/functions.cfm", "<cffunction name=\"formatName\" returntype=\"string\">\n  <cfargument name=\"n\">\n  <cfreturn ucase(n)>\n</cffunction>\n");
    write(root, "_inc/header.cfm", "<cfoutput>header</cfoutput>\n");
    write(root, "customtags/box.cfm", "<cfoutput>box</cfoutput>\n");
    write(root, "admin/users.cfm", PAGE);
    write(root, "config.cfm", "<cfset request.api.orders = \"https://api.example.com/v1/orders\">\n");
}

/// (confidence, target qualified names) of each reference to `name`.
fn refs(store: &Store, name: &str) -> Vec<(String, Vec<String>)> {
    let (hits, _) = query::refs(store.conn(), name, &[], 100).unwrap();
    hits.into_iter().map(|h| (h.confidence.to_string(), h.targets.into_iter().map(|t| t.qualified).collect())).collect()
}

fn one(conf: &str, target: &str) -> Vec<(String, Vec<String>)> {
    vec![(conf.to_string(), vec![target.to_string()])]
}

#[test]
fn includes_functions_components_tables_and_routes() {
    let root = scratch("app");
    setup(&root);
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 2, full_discovery: true, ..Default::default() }).unwrap();

    // The page is a template; including it is a call from the includer.
    let page = query::definitions(store.conn(), "users").unwrap();
    assert_eq!(page[0].kind, "template");
    assert_eq!(refs(&store, "header"), one("exact", "header"));
    // A function the page's include defines, called in another case.
    assert_eq!(refs(&store, "formatName"), one("exact", "formatName"));
    // Components by dotted path, a method through `extends`, and a variable typed in another method via a mapping.
    assert_eq!(refs(&store, "UserService"), one("exact", "UserService"));
    assert_eq!(refs(&store, "getUser"), one("exact", "UserService.getUser"));
    assert_eq!(refs(&store, "save"), one("exact", "Base.save"));
    assert_eq!(refs(&store, "put"), one("exact", "Cache.put"));
    // A custom tag found in a custom tags directory, not beside the caller.
    assert_eq!(refs(&store, "box"), one("unique_name", "box"));

    // Who reaches getUser: the page.
    let t = trace::trace(store.conn(), "UserService.getUser", &trace::TraceOptions { direction: trace::Direction::In, depth: 2, max_nodes: 50 })
        .unwrap()
        .unwrap();
    assert!(t.nodes.iter().any(|n| n.qualified == "users" && n.kind == "template"), "{:?}", t.nodes.iter().map(|n| &n.qualified).collect::<Vec<_>>());

    // `dbo.Users` and `Users` are one table, written by Base.save and read by getUser.
    let model = store.db_model().unwrap();
    let users = model.named("users");
    assert_eq!(users.len(), 1);
    let accessors: Vec<(&str, &str)> = users[0].accessors.iter().map(|a| (a.symbol.as_str(), a.mode.as_str())).collect();
    assert_eq!(accessors, [("Base.save", "write"), ("UserService.getUser", "read")]);
    assert_eq!(users[0].mapped_by, "sql");

    // The request's URL comes from a constant set in another template.
    let routes = query::http_routes(store.conn()).unwrap();
    let r: Vec<(&str, &str, Option<&str>, &str)> = routes.iter().map(|r| (r.side.as_str(), r.verb.as_str(), r.path.as_deref(), r.symbol.as_str())).collect();
    assert_eq!(r, [("client", "POST", Some("v1/orders/lines"), "users")]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A ColdFusion page over 1 MB is extracted; a C# file that size is left out and listed.
#[test]
fn large_files_are_listed_and_cfml_pages_kept() {
    let root = scratch("large");
    let filler = "<cfset x = 1>\n".repeat(80_000);
    write(&root, "big.cfm", &format!("<cfset y = go()>\n{filler}"));
    write(&root, "lib.cfm", "<cffunction name=\"go\"><cfreturn 1></cffunction>\n");
    write(&root, "Big.cs", &format!("class Big {{}}\n{}", "// filler line of a generated file\n".repeat(40_000)));
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 2, full_discovery: true, ..Default::default() }).unwrap();
    assert_eq!(refs(&store, "go"), one("unique_name", "go"));
    let large = kc_index::skipped_large(&store).unwrap();
    assert_eq!(large.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["Big.cs"]);
    let _ = std::fs::remove_dir_all(&root);
}
