//! Repos that depend on each other through package registries (NuGet, npm): the edge comes from which indexed
//! repo declares the package, and references resolve into it through namespace imports.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use kc_index::{SyncOptions, sync};
use kc_store::{manifests, query};

/// A test's turn at the cache, held for the whole test (see [`turn`]).
struct Turn {
    _held: MutexGuard<'static, ()>,
}

impl Turn {
    /// `<base>/<name>`, where the test makes its repos. It borrows the turn, so a test can't let go of its turn while
    /// it still uses the directory.
    fn dir(&self, name: &str) -> Dir<'_> {
        Dir(base().join(name), std::marker::PhantomData)
    }
}

struct Dir<'a>(PathBuf, std::marker::PhantomData<&'a Turn>);

impl std::ops::Deref for Dir<'_> {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Dir<'_> {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// The cache directory and environment are process-wide, so the tests here take turns, each from an empty cache:
/// with no workspace roots, the workspace is every indexed repo, so another test's repos (indexing meanwhile, or
/// deleted while their indexes stay in the cache) would be candidates for its links.
fn turn() -> Turn {
    static TURN: Mutex<()> = Mutex::new(());
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let base = base();
    for dir in ["cache", "config"] {
        let _ = std::fs::remove_dir_all(base.join(dir));
    }
    // The fixtures are written in a made-up code base's conventions, which the config names.
    std::fs::create_dir_all(base.join("config")).unwrap();
    std::fs::write(base.join("config/config.toml"), kc_index::config::SYNTHETIC_CONVENTIONS).unwrap();
    Turn { _held: turn }
}

fn base() -> &'static Path {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("knowcode-test-xrepo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // SAFETY: set once, before any test in this binary reads them. The config directory holds only the tests'
        // conventions, so the user's own settings don't reach them.
        unsafe {
            std::env::set_var("KNOWCODE_CACHE_DIR", base.join("cache"));
            std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config"));
            std::env::set_var("KNOWCODE_DATA_DIR", base.join("data"));
        }
        base
    })
}

fn scratch(base: &Path, name: &str) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    std::fs::write(root.join(path), text).unwrap();
}

#[test]
fn nuget_dependencies_link_repos_and_resolve_references() {
    let turn = turn();
    let base = turn.dir("nuget");

    let common = scratch(&base, "common");
    write(
        &common,
        "src/Acme.Common.Data.csproj",
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Acme.Common.Data</PackageId></PropertyGroup>\n</Project>\n",
    );
    write(&common, "src/CartStore.cs", "namespace Acme.Common.Data.Models;\n\npublic class CartStore { public void Save() { } }\n");

    let cart = scratch(&base, "cart");
    write(
        &cart,
        "src/Cart.csproj",
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <PackageReference Include=\"Acme.Common.Data\" Version=\"3.*\" />\n  </ItemGroup>\n</Project>\n",
    );
    write(
        &cart,
        "src/Checkout.cs",
        "using Acme.Common.Data.Models;\n\npublic class Checkout\n{\n    private readonly CartStore _store;\n    public void Run() { _store.Save(); }\n}\n",
    );

    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots = [kc_index::repo_root(&common).unwrap(), kc_index::repo_root(&cart).unwrap()];
    let mut stores: Vec<_> = roots.iter().map(|r| kc_index::open(r).unwrap()).collect();
    for (r, s) in roots.iter().zip(stores.iter_mut()) {
        sync(r, s, &opts).unwrap();
    }

    let owners = kc_index::package_owners(&roots).unwrap();
    let cart_root = roots[1].to_string_lossy();
    let edges = manifests::repo_edges(&manifests::packages(stores[1].conn()).unwrap(), &cart_root, &cart_root, &owners);
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].dependency.as_str(), Path::new(&edges[0].to_repo)), ("Acme.Common.Data", roots[0].as_path()));

    let dep = query::Dependency {
        repo: "common".into(),
        conn: stores[0].conn(),
        modules: query::provided_modules(stores[0].conn()).unwrap(),
        assumptions: Vec::new(),
    };
    let (hits, _) = query::refs_with(stores[1].conn(), "Save", &["call"], 10, std::slice::from_ref(&dep)).unwrap();
    let save = hits.iter().find(|h| h.from.as_ref().is_some_and(|f| f.name == "Run")).unwrap();
    assert_eq!(save.targets[0].qualified, "CartStore.Save");
    assert_eq!(save.targets[0].repo.as_deref(), Some("common"));
    // Only references in files importing the dependency can resolve into it, and those resolve the same way.
    let importing = query::refs_importing(stores[1].conn(), "Save", &["call"], std::slice::from_ref(&dep)).unwrap();
    assert_eq!(importing.len(), 1);
    assert_eq!(importing[0].targets[0].qualified, "CartStore.Save");

    // The summary records the names references use, and which repos depend on which.
    let all = kc_index::workspace::summaries(&[]).unwrap();
    let cart_summary = all.iter().find(|s| s.root == roots[1]).unwrap();
    assert!(cart_summary.may_reference("Save") && cart_summary.may_reference("CartStore.Save"));
    assert!(!cart_summary.may_reference("NeverReferencedAnywhere"));
    let (dependencies, dependents) = kc_index::workspace::neighbours(&roots[0], &all);
    assert!(dependencies.is_empty());
    assert_eq!(dependents.iter().map(|s| s.root.clone()).collect::<Vec<_>>(), [roots[1].clone()]);
    drop(all);
    // Reading HEAD caches `git status` beside the index, not in it, so the index (and its summary) stays as it was.
    let db = kc_index::db_path(&roots[1]).unwrap();
    let stamp = || {
        let len = |p: &Path| std::fs::metadata(p).map(|m| (m.len(), m.modified().ok())).ok();
        (len(&db), len(Path::new(&format!("{}-wal", db.display()))))
    };
    let before = stamp();
    kc_index::head(&roots[1], &stores[1]);
    assert_eq!(stamp(), before);

    // The workspace summary covers every indexed repo, and follows a repo's index when it changes.
    let names = |root: &Path| -> Vec<String> {
        let all = kc_index::workspace::summaries(&[]).unwrap();
        let s = all.iter().find(|s| s.root == root).expect("summary for root");
        s.packages.iter().map(|p| p.name.clone()).collect()
    };
    assert_eq!(names(&roots[0]), ["Acme.Common.Data"]);
    write(
        &common,
        "src/Acme.Common.Data.csproj",
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Acme.Common.Store</PackageId></PropertyGroup>\n</Project>\n",
    );
    sync(&roots[0], &mut stores[0], &opts).unwrap();
    assert_eq!(names(&roots[0]), ["Acme.Common.Store"]);
    assert!(kc_index::package_owners(&roots).unwrap().get("Acme.Common.Data").is_none());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn npm_dependencies_see_exported_members_only() {
    let turn = turn();
    let base = turn.dir("npm");
    let http = scratch(&base, "http");
    write(&http, "package.json", r#"{ "name": "@acme/http", "version": "6.0.0" }"#);
    write(
        &http,
        "src/http.service.ts",
        "export class HttpService {\n  send(url: string): void {}\n  private sign(): void {}\n}\nexport function buildUrl(p: string) { return p; }\nfunction internal() {}\n",
    );
    let app = scratch(&base, "app");
    write(&app, "package.json", r#"{ "name": "app", "dependencies": { "@acme/http": "6.0.0" } }"#);
    write(
        &app,
        "src/cart.ts",
        "import { HttpService, buildUrl } from '@acme/http/services';\n\nexport class Cart {\n  constructor(private http: HttpService) {}\n  load() {\n    this.http.send(buildUrl('x'));\n    this.http.sign();\n    internal();\n  }\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots = [kc_index::repo_root(&http).unwrap(), kc_index::repo_root(&app).unwrap()];
    let mut stores: Vec<_> = roots.iter().map(|r| kc_index::open(r).unwrap()).collect();
    for (r, s) in roots.iter().zip(stores.iter_mut()) {
        sync(r, s, &opts).unwrap();
    }
    let owners = kc_index::package_owners(&roots).unwrap();
    let edges = manifests::repo_edges(&manifests::packages(stores[1].conn()).unwrap(), &roots[1].to_string_lossy(), &roots[1].to_string_lossy(), &owners);
    assert_eq!(edges.iter().map(|e| e.dependency.as_str()).collect::<Vec<_>>(), ["@acme/http"]);

    let dep = query::Dependency { repo: "http".into(), conn: stores[0].conn(), modules: query::provided_modules(stores[0].conn()).unwrap(), assumptions: Vec::new() };
    let resolved = |name: &str| -> Option<String> {
        let (hits, _) = query::refs_with(stores[1].conn(), name, &["call"], 10, std::slice::from_ref(&dep)).unwrap();
        let h = hits.into_iter().find(|h| h.from.as_ref().is_some_and(|f| f.name == "load"))?;
        h.targets.iter().find(|t| t.repo.as_deref() == Some("http")).map(|t| t.qualified.clone())
    };
    assert_eq!(resolved("send").as_deref(), Some("HttpService.send"));
    assert_eq!(resolved("buildUrl").as_deref(), Some("buildUrl"));
    // Private members and unexported functions stay inside their repo.
    assert_eq!(resolved("sign"), None);
    assert_eq!(resolved("internal"), None);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn npm_references_resolve_in_the_dependency_their_import_names() {
    let turn = turn();
    let base = turn.dir("npm-import");
    let first = scratch(&base, "first");
    write(&first, "package.json", r#"{ "name": "@acme/first", "version": "1.0.0" }"#);
    write(&first, "src/a.ts", "export class Widget { save() {} }\nexport class Other { save() {} }\nexport function make() {}\n");
    let second = scratch(&base, "second");
    write(&second, "package.json", r#"{ "name": "@acme/second", "version": "1.0.0" }"#);
    write(&second, "src/b.ts", "export class Widget { save() {} }\nexport class Gadget { save() {} }\nexport function make() {}\n");
    let app = scratch(&base, "app");
    write(&app, "package.json", r#"{ "name": "app", "dependencies": { "@acme/first": "1.0.0", "@acme/second": "1.0.0" } }"#);
    // The parameter's type comes from the second package, though the file also imports from the first, which is
    // listed first and declares a same-named type.
    write(
        &app,
        "src/run.ts",
        "import { Widget, make } from '@acme/second';\nimport { Other } from '@acme/first';\nexport function run(x: Widget, y) {\n  x.save();\n  make();\n  y.save();\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&first, &second, &app].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    let mut stores: Vec<_> = roots.iter().map(|r| kc_index::open(r).unwrap()).collect();
    for (r, s) in roots.iter().zip(stores.iter_mut()) {
        sync(r, s, &opts).unwrap();
    }
    let dep = |name: &str, i: usize| query::Dependency {
        repo: name.into(),
        conn: stores[i].conn(),
        modules: query::provided_modules(stores[i].conn()).unwrap(),
        assumptions: Vec::new(),
    };
    let deps = [dep("first", 0), dep("second", 1)];
    let at = |name: &str, line: u32| -> (&'static str, Vec<String>, Option<&'static str>) {
        let (hits, _) = query::refs_with(stores[2].conn(), name, &["call"], 10, &deps).unwrap();
        let h = hits.into_iter().find(|h| h.line == line).unwrap_or_else(|| panic!("no {name} at line {line}"));
        let mut targets: Vec<String> = h.targets.iter().map(|t| format!("{}:{}", t.repo.as_deref().unwrap_or(""), t.qualified)).collect();
        targets.sort();
        (h.confidence, targets, h.basis)
    };
    assert_eq!(at("save", 4), ("exact", vec!["second:Widget.save".to_string()], None));
    assert_eq!(at("make", 5), ("exact", vec!["second:make".to_string()], None));
    // A receiver of unknown type: both imported packages declare `save`, and nothing says which.
    let (confidence, targets, basis) = at("save", 6);
    assert_eq!((confidence, basis), ("ambiguous", Some(query::AMBIGUOUS_DEPENDENCY)));
    assert!(targets.iter().any(|t| t.starts_with("first:")) && targets.iter().any(|t| t.starts_with("second:")), "{targets:?}");
    // Invariant: the order the dependencies come in never changes an answer.
    let reversed = [dep("second", 1), dep("first", 0)];
    let answers = |deps: &[query::Dependency], name: &str| -> Vec<(u32, &'static str, Vec<String>)> {
        let mut v: Vec<_> = query::refs_with(stores[2].conn(), name, &["call"], 10, deps)
            .unwrap()
            .0
            .into_iter()
            .map(|h| {
                let mut t: Vec<String> = h.targets.iter().map(|t| format!("{:?}:{}", t.repo, t.qualified)).collect();
                t.sort();
                (h.line, h.confidence, t)
            })
            .collect();
        v.sort();
        v
    };
    for name in ["save", "make"] {
        assert_eq!(answers(&deps, name), answers(&reversed, name), "{name}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn http_routes_link_frontend_contracts_to_backend_actions() {
    let turn = turn();
    let base = turn.dir("http");
    let web = scratch(&base, "web");
    write(&web, "src/Api.ts", "export class Api {\n  static readonly filters = 'Filters';\n  static readonly search = 'Search';\n  static readonly order = 'Orders/{id}';\n  static readonly gone = 'Gone';\n}\n");
    write(
        &web,
        "src/contracts.ts",
        "@RequestContract({ endpoint: Api.filters, httpMethod: HttpMethods.post })\nexport class ApiGetFilters {}\n\
         @RequestContract({ endpoint: Api.search, httpMethod: HttpMethods.post })\nexport class ApiSearch {}\n\
         @RequestContract({ endpoint: Api.order, httpMethod: HttpMethods.get })\nexport class ApiGetOrder {}\n\
         @RequestContract({ endpoint: Api.gone, httpMethod: HttpMethods.get })\nexport class ApiGone {}\n",
    );
    let main_api = scratch(&base, "main-api");
    write(
        &main_api,
        "src/SearchController.cs",
        "public class SearchController : Controller\n{\n    [HttpPost(nameof(Filters))]\n    public Task Filters() => null;\n\n    [HttpPost(\"Search\")]\n    public Task Search() => null;\n\n    [HttpGet(\"Orders/{orderId:int}\")]\n    public Task Order(int orderId) => null;\n}\n",
    );
    let other_api = scratch(&base, "other-api");
    write(&other_api, "src/OtherController.cs", "public class OtherController : Controller\n{\n    [HttpPost(\"search\")]\n    public Task Search() => null;\n}\n");

    write(
        &web,
        "src/filter.service.ts",
        "export class FilterService {\n  constructor(private http: HttpService) {}\n  load(): void {\n    this.http.send(new ApiGetFilters());\n  }\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&web, &main_api, &other_api].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let web_routes: Vec<_> = summaries.iter().find(|s| s.root == roots[0]).unwrap().routes.clone();
    let link = |class: &str| {
        let client = web_routes.iter().find(|r| r.symbol == class).unwrap_or_else(|| panic!("{class}"));
        assert_eq!(client.side, "client");
        kc_index::http::handlers(client, &roots[0], &summaries)
            .into_iter()
            .map(|l| (l.repo.file_name().unwrap().to_string_lossy().into_owned(), l.route.symbol, l.confidence))
            .collect::<Vec<_>>()
    };
    let main = |s: &str, c: &'static str| ("main-api".to_string(), s.to_string(), c);
    assert_eq!(link("ApiGetFilters"), [main("SearchController.Filters", "exact")]);
    // `Search` is served by both backends; main-api serves most of this app's routes.
    assert_eq!(link("ApiSearch"), [main("SearchController.Search", "dominant"), ("other-api".to_string(), "OtherController.Search".to_string(), "ambiguous")]);
    // Route parameters match whatever their names and constraints.
    assert_eq!(link("ApiGetOrder"), [main("SearchController.Order", "exact")]);
    assert!(link("ApiGone").is_empty());

    // And back: who calls an action.
    let main_routes = &summaries.iter().find(|s| s.root == roots[1]).unwrap().routes;
    let filters = main_routes.iter().find(|r| r.symbol == "SearchController.Filters").unwrap();
    let callers: Vec<_> = kc_index::http::callers(filters, &roots[1], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence)).collect();
    assert_eq!(callers, [("ApiGetFilters".to_string(), "exact")]);

    // A summary written by an older build (no `routes`, same index stamps) must not be trusted.
    let path = base.parent().unwrap().join("cache/workspace.json");
    let current: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let repos = current.get("repos").cloned().unwrap_or(current);
    let mut old = serde_json::Map::new();
    for (k, mut v) in repos.as_object().unwrap().clone() {
        v.as_object_mut().unwrap().remove("routes");
        old.insert(k, v);
    }
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let reread = kc_index::workspace::summaries(&[]).unwrap();
    assert!(!reread.iter().find(|s| s.root == roots[1]).unwrap().routes.is_empty(), "routes dropped by an old-format summary");

    // Per symbol, as context shows it: a method that sends a contract, the contract itself, and the action.
    let entries = |root: &PathBuf, name: &str, qualified: &str| {
        let store = kc_index::open(root).unwrap();
        let id: i64 = store.conn().query_row("SELECT id FROM symbols WHERE name = ?1", [name], |r| r.get(0)).unwrap();
        kc_index::http::for_symbol(store.conn(), root, qualified, id, &summaries)
            .unwrap()
            .into_iter()
            .map(|e| (e.role, e.symbol, e.verb, e.path, e.links.into_iter().map(|l| (l.route.symbol, l.confidence)).collect::<Vec<_>>()))
            .collect::<Vec<_>>()
    };
    let to_filters = vec![("SearchController.Filters".to_string(), "exact")];
    assert_eq!(entries(&roots[0], "load", "FilterService.load"), [("requests", "ApiGetFilters".to_string(), "POST".to_string(), Some("Filters".to_string()), to_filters.clone())]);
    assert_eq!(entries(&roots[0], "ApiGetFilters", "ApiGetFilters"), [("requests", "ApiGetFilters".to_string(), "POST".to_string(), Some("Filters".to_string()), to_filters)]);
    assert_eq!(
        entries(&roots[1], "Filters", "SearchController.Filters"),
        [("serves", "SearchController.Filters".to_string(), "POST".to_string(), Some("Filters".to_string()), vec![("ApiGetFilters".to_string(), "exact")])]
    );
    assert!(entries(&roots[0], "FilterService", "FilterService").is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn checkouts_of_one_repo_count_once_when_linking_routes() {
    let turn = turn();
    let base = turn.dir("checkouts");
    let git = |dir: &Path, args: &[&str]| assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
    let web = scratch(&base, "web");
    write(&web, "src/Api.ts", "export class Api { static readonly lookup = 'Lookup'; }\n");
    write(&web, "src/c.ts", "@RequestContract({ endpoint: Api.lookup, httpMethod: HttpMethods.post })\nexport class ApiGetLookup {}\n");
    // The same backend at two paths (a corpus clone and a developer checkout), its origin written two ways.
    let controller = "public class SearchController : Controller\n{\n    [HttpPost(nameof(Lookup))]\n    public Task Lookup() => null;\n}\n";
    let api_a = scratch(&base, "Acme.MainApi");
    write(&api_a, "src/SearchController.cs", controller);
    git(&api_a, &["remote", "add", "origin", "git@gitlab.example.com:Products/Acme.MainApi.git"]);
    let api_b = scratch(&base, "mainapi");
    write(&api_b, "src/SearchController.cs", controller);
    git(&api_b, &["remote", "add", "origin", "https://gitlab.example.com/products/acme.mainapi"]);

    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&web, &api_a, &api_b].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let web_summary = summaries.iter().find(|s| s.root == roots[0]).unwrap();
    let client = web_summary.routes.iter().find(|r| r.symbol == "ApiGetLookup").unwrap();
    let links = kc_index::http::handlers(client, &roots[0], &summaries);
    // One repo serves the route, whichever checkout answers for it.
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!((links[0].route.symbol.as_str(), links[0].confidence), ("SearchController.Lookup", "exact"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn service_dtos_link_backends_through_the_named_service() {
    let turn = turn();
    let base = turn.dir("services");
    // The internal service: hosts `MailingApi`, serves DeleteSubscriber, ships the DTO in its Models package.
    let plan = scratch(&base, "mailinglist");
    write(&plan, "src/Models.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Acme.MailingApi.Models</PackageId></PropertyGroup>\n</Project>\n");
    write(&plan, "src/ServiceNames.cs", "public static class ServiceNames { public const string ApiName = \"MailingApi\"; }\n");
    write(&plan, "src/DeleteSubscriber.cs", "[ShopRequest(ServiceNames.ApiName, nameof(DeleteSubscriber), HttpMethods.Delete)]\npublic class DeleteSubscriber { }\n");
    write(&plan, "src/Startup.cs", "public class Startup(IWebHostEnvironment env) : ShopServiceStartup<S>(ServiceNames.ApiName, 5000, env) { }\n");
    write(&plan, "src/MailingListController.cs", "public class MailingListController : Controller\n{\n    [HttpDelete(nameof(Models.DeleteSubscriber))]\n    public Task DeleteSubscriber() => null;\n}\n");
    // Another service with an action of the same name.
    let decoy = scratch(&base, "decoy");
    write(&decoy, "src/Startup.cs", "public class Startup(IWebHostEnvironment env) : ShopServiceStartup<S>(\"DecoyApi\", 5001, env) { }\n");
    write(&decoy, "src/DecoyController.cs", "public class DecoyController : Controller\n{\n    [HttpDelete(\"DeleteSubscriber\")]\n    public Task DeleteSubscriber() => null;\n}\n");
    // The BFF: depends on the Models package and sends the DTO through the gateway.
    let bff = scratch(&base, "bff");
    write(&bff, "src/Bff.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Acme.MailingApi.Models\" Version=\"1.*\" /></ItemGroup>\n</Project>\n");
    write(
        &bff,
        "src/MailService.cs",
        "using EP = Acme.MailingApi.Models;\npublic class MailService\n{\n    private readonly IShopGateway _gateway;\n    public Task Delete() => _gateway.SendAsync(new EP.DeleteSubscriber());\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&plan, &decoy, &bff].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let store = kc_index::open(&roots[2]).unwrap();
    let id: i64 = store.conn().query_row("SELECT id FROM symbols WHERE name = 'Delete'", [], |r| r.get(0)).unwrap();
    let entries = kc_index::http::for_symbol(store.conn(), &roots[2], "MailService.Delete", id, &summaries).unwrap();
    let got: Vec<_> = entries
        .iter()
        .map(|e| (e.role, e.symbol.as_str(), e.service.as_deref(), e.links.iter().map(|l| (l.repo.file_name().unwrap().to_string_lossy().into_owned(), l.route.symbol.as_str(), l.confidence)).collect::<Vec<_>>()))
        .collect();
    // The DTO names its service, and only mailinglist hosts it: the decoy's same-named action is not a candidate.
    assert_eq!(
        got,
        [("requests", "DeleteSubscriber", Some("MailingApi"), vec![("mailinglist".to_string(), "MailingListController.DeleteSubscriber", "exact")])]
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// A backend whose controllers take their prefix from a base controller in another file (`[controller]` filled with
/// each subclass's name) and from conventional routes; clients requesting it with fetch, axios, jQuery through a URL
/// builder, a generated client's operation, and C# `HttpClient` with a composed `Uri`. An action nothing requests
/// says what was searched, what wasn't read, and the nearest request by path.
#[test]
fn inherited_prefixes_and_client_forms_link_across_repos() {
    let turn = turn();
    let base = turn.dir("prefixes");
    let api = scratch(&base, "api");
    write(&api, "src/ApiControllerBase.cs", "[ApiController]\n[Route(\"api/v1/[controller]\")]\npublic abstract class ApiControllerBase : ControllerBase { }\n");
    write(
        &api,
        "src/ItemsController.cs",
        "public class ItemsController : ApiControllerBase\n{\n    [HttpGet(\"Latest\")]\n    public Task Latest() => null;\n    [HttpGet(\"{id}\")]\n    public Task GetItem(string id) => null;\n    [HttpDelete(\"{id}\")]\n    public Task Remove(string id) => null;\n    [HttpGet(\"Oldest\")]\n    public Task Oldest() => null;\n}\n",
    );
    write(
        &api,
        "src/UserLibraryController.cs",
        "public class UserLibraryController : ApiControllerBase\n{\n    [HttpGet(\"Users/{userId}/Items/Latest\")]\n    public Task GetLatestMedia(string userId) => null;\n}\n",
    );
    write(&api, "src/HomeController.cs", "public class HomeController : Controller\n{\n    public IActionResult Privacy() => null;\n    private void Hidden() { }\n}\n");
    write(&api, "src/Program.cs", "var app = builder.Build();\napp.MapControllerRoute(\"default\", \"{controller=Home}/{action=Index}/{id?}\");\n");
    let web = scratch(&base, "web");
    write(
        &web,
        "src/library.ts",
        "const API = 'https://api.example.test/api/v1';\nexport class Library {\n  latest() { return fetch(`${API}/Items/Latest`); }\n  remove(id: string) { return axios.delete(API + '/Items/' + id); }\n  media(api: Api) { return getUserLibraryApi(api).getLatestMedia({ userId }); }\n  privacy() { return $.ajax({ url: apiClient.getUrl('Home/Privacy'), type: 'GET' }); }\n  oldest() { return fetch('/Items/Oldest'); }\n  dynamic(url: string) { return fetch(url); }\n}\n",
    );
    let admin = scratch(&base, "admin");
    write(&admin, "src/ItemsClient.cs", "public class ItemsClient(HttpClient http)\n{\n    public Task One(string id) => http.GetAsync(new Uri(_base, $\"api/v1/Items/{id}\"));\n}\n");
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&api, &web, &admin].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let routes = |i: usize| summaries.iter().find(|s| s.root == roots[i]).unwrap().routes.clone();
    // The base controller's prefix, `[controller]` the subclass; conventional routes for the controller with none.
    let served: Vec<(String, String, String)> =
        routes(0).into_iter().filter(|r| r.side == "server").map(|r| (r.symbol, r.verb, r.path.unwrap_or_default())).collect();
    let s = |sym: &str, verb: &str, path: &str| (sym.to_string(), verb.to_string(), path.to_string());
    assert_eq!(
        served,
        [
            s("HomeController.Privacy", "*", "Home/Privacy"),
            s("HomeController.Privacy", "*", "Home/Privacy/{id}"),
            s("ItemsController.Latest", "GET", "api/v1/Items/Latest"),
            s("ItemsController.GetItem", "GET", "api/v1/Items/{id}"),
            s("ItemsController.Remove", "DELETE", "api/v1/Items/{id}"),
            s("ItemsController.Oldest", "GET", "api/v1/Items/Oldest"),
            s("UserLibraryController.GetLatestMedia", "GET", "api/v1/UserLibrary/Users/{userId}/Items/Latest"),
        ]
    );
    let link = |i: usize, symbol: &str| -> Vec<(String, &'static str, Vec<kc_store::answer::Assumption>)> {
        let r = routes(i).into_iter().find(|r| r.symbol == symbol).unwrap_or_else(|| panic!("{symbol}"));
        kc_index::http::handlers(&r, &roots[i], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence, l.assumptions)).collect()
    };
    use kc_store::answer::Assumption::{OperationName, SameDeployment};
    let one = |sym: &str| vec![(sym.to_string(), "exact", vec![SameDeployment])];
    assert_eq!(link(1, "Library.latest"), one("ItemsController.Latest"));
    assert_eq!(link(1, "Library.remove"), one("ItemsController.Remove"));
    assert_eq!(link(1, "Library.privacy"), one("HomeController.Privacy"));
    assert_eq!(link(2, "ItemsClient.One"), one("ItemsController.GetItem"));
    // A generated client's operation: the action of that name under the controller named like its tag.
    assert_eq!(link(1, "Library.media"), [("UserLibraryController.GetLatestMedia".to_string(), "exact", vec![SameDeployment, OperationName])]);

    // From the action's side.
    let server = |symbol: &str| routes(0).into_iter().find(|r| r.symbol == symbol).unwrap();
    let callers = |symbol: &str| -> Vec<String> { kc_index::http::callers(&server(symbol), &roots[0], &summaries).into_iter().map(|l| l.route.symbol).collect() };
    assert_eq!(callers("ItemsController.Latest"), ["Library.latest"]);
    assert_eq!(callers("UserLibraryController.GetLatestMedia"), ["Library.media"]);

    // Nothing requests `Oldest` under its prefix: the note says where requests were looked for and what is left.
    assert!(callers("ItemsController.Oldest").is_empty());
    let note = kc_index::http::unlinked_note(&server("ItemsController.Oldest"), &summaries, false).unwrap();
    for part in [
        "searched 2 repos with requests (",
        "generated clients (UserLibrary) by operation `Oldest`",
        "1 run-time URL unread (web src/library.ts:8)",
        "nearest by path: GET /Items/Oldest (web src/library.ts:7)",
    ] {
        assert!(note.contains(part), "{part:?} in {note}");
    }
    // And from the request's side, in a list: the nearest action only.
    let oldest = routes(1).into_iter().find(|r| r.symbol == "Library.oldest").unwrap();
    assert_eq!(
        kc_index::http::unlinked_note(&oldest, &summaries, true).as_deref(),
        Some("nearest by path: GET /api/v1/Items/Oldest (api ItemsController.Oldest); GET /api/v1/Items/{id} (api ItemsController.GetItem)")
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn angular_elements_resolve_into_library_repos_and_back() {
    let turn = turn();
    let base = turn.dir("angular");
    let lib = scratch(&base, "ui-lib");
    write(&lib, "package.json", r#"{ "name": "@acme/ui-button" }"#);
    write(&lib, "src/button.component.ts", "@Component({ selector: 'ui-button', template: '<button></button>' })\nexport class UiButtonComponent {}\n");
    let app = scratch(&base, "shop");
    write(&app, "package.json", r#"{ "name": "shop", "dependencies": { "@acme/ui-button": "1.0.0" } }"#);
    write(&app, "src/cart.component.ts", "@Component({ selector: 'shop-cart', templateUrl: './cart.component.html' })\nexport class CartComponent {}\n");
    write(&app, "src/cart.component.html", "<div>\n  <ui-button></ui-button>\n  <kendo-grid></kendo-grid>\n</div>\n");
    // The app's test stub of the library component must not stand in for it.
    std::fs::create_dir_all(app.join("src/testing")).unwrap();
    write(&app, "src/testing/button.stub.ts", "@Component({ selector: 'ui-button', template: '' })\nexport class UiButtonStubComponent {}\n");
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&lib, &app].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let angular_of = |root: &PathBuf, class: &str| {
        let store = kc_index::open(root).unwrap();
        let id: i64 = store.conn().query_row("SELECT id FROM symbols WHERE name = ?1", [class], |r| r.get(0)).unwrap();
        let mut a = query::angular(store.conn(), id).unwrap().unwrap();
        kc_index::angular::complete(&mut a, root, &summaries).unwrap();
        a
    };
    let uses = |v: &[query::NgUse]| v.iter().map(|u| (u.tag.clone(), u.component.clone(), u.repo.clone())).collect::<Vec<_>>();
    // Forward: the app's template uses a library element.
    let cart = angular_of(&roots[1], "CartComponent");
    assert_eq!(
        uses(&cart.renders),
        [
            ("ui-button".to_string(), Some("UiButtonComponent".to_string()), Some("ui-lib".to_string())),
            ("kendo-grid".to_string(), None, None),
        ]
    );
    // Back: the library component is used by the app's template.
    let button = angular_of(&roots[0], "UiButtonComponent");
    assert_eq!(uses(&button.rendered_by), [("ui-button".to_string(), Some("CartComponent".to_string()), Some("shop".to_string()))]);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn gateway_calls_by_name_link_through_the_service_host() {
    let turn = turn();
    let base = turn.dir("gateway-calls");
    let billing = scratch(&base, "billing");
    write(&billing, "src/Startup.cs", "public class Startup(IWebHostEnvironment env) : ShopServiceStartup<S>(\"BillingApi\", 5000, env) { }\n");
    write(&billing, "src/PurchaseOrderController.cs", "[Route(\"purchaseOrders\")]\npublic class PurchaseOrderController : Controller\n{\n    [HttpGet(\"number/{purchaseOrderNumber}\")]\n    public Task GetPurchaseOrder(string purchaseOrderNumber) => null;\n}\n");
    // A web backend hosting another service, with an action named like the cart service's.
    let web = scratch(&base, "web");
    write(&web, "src/Startup.cs", "public class Startup(IWebHostEnvironment env) : ShopServiceStartup<S>(\"WebApi\", 5001, env) { }\n");
    write(&web, "src/CartController.cs", "public class CartController : Controller\n{\n    [HttpPost(\"AddBasketItems\")]\n    public Task AddBasketItems() => null;\n}\n");
    let caller = scratch(&base, "caller");
    write(
        &caller,
        "src/Orders.cs",
        "public class Orders\n{\n    public async Task Load(string n)\n    {\n        await gateway.CallServiceAsync(\"BillingApi\", $\"purchaseOrders/number/{n}\", HttpMethod.Get);\n        await gateway.CallServiceAsync(\"CartApi\", nameof(AddBasketItems), HttpMethod.Post);\n    }\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&billing, &web, &caller].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let store = kc_index::open(&roots[2]).unwrap();
    let id: i64 = store.conn().query_row("SELECT id FROM symbols WHERE name = 'Load'", [], |r| r.get(0)).unwrap();
    let entries = kc_index::http::for_symbol(store.conn(), &roots[2], "Orders.Load", id, &summaries).unwrap();
    let got: Vec<_> = entries
        .iter()
        .map(|e| (e.role, e.path.as_deref(), e.service.as_deref(), e.links.iter().map(|l| (l.route.symbol.as_str(), l.confidence)).collect::<Vec<_>>()))
        .collect();
    assert_eq!(
        got,
        [
            ("requests", Some("purchaseOrders/number/{}"), Some("BillingApi"), vec![("PurchaseOrderController.GetPurchaseOrder", "exact")]),
            // No indexed repo hosts CartApi, and the web backend hosts another service: not a candidate.
            ("requests", Some("AddBasketItems"), Some("CartApi"), vec![]),
        ]
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn angular_attribute_directives_and_compound_selectors_resolve() {
    let turn = turn();
    let base = turn.dir("angular-attrs");
    let lib = scratch(&base, "ui-lib");
    write(&lib, "package.json", r#"{ "name": "@acme/ui-kit" }"#);
    write(&lib, "src/tooltip.directive.ts", "@Directive({ selector: '[uiTooltip], [ui-tooltip]' })\nexport class UiTooltipDirective {}\n");
    write(&lib, "src/button.component.ts", "@Component({ selector: 'button[uiButton], a[uiButton]', template: '<ng-content></ng-content>' })\nexport class UiButtonComponent {}\n");
    write(&lib, "src/paginator.component.ts", "@Component({ selector: 'ui-paginator:not([withCursors])', template: '' })\nexport class UiPaginatorComponent {}\n");
    write(&lib, "src/cursor.component.ts", "@Component({ selector: 'ui-paginator[withCursors]', template: '' })\nexport class UiCursorPaginatorComponent {}\n");
    write(&lib, "src/selectors.ts", "export const layoutSelectors = `[uiLayout], [fxLayout]`;\n");
    write(&lib, "src/layout.directive.ts", "import { layoutSelectors as selector } from './selectors';\n@Directive({ selector })\nexport class UiLayoutDirective {}\n");
    let app = scratch(&base, "shop");
    write(&app, "package.json", r#"{ "name": "shop", "dependencies": { "@acme/ui-kit": "1.0.0" } }"#);
    write(&app, "src/cart.component.ts", "@Component({ selector: 'shop-cart', templateUrl: './cart.component.html' })\nexport class CartComponent {}\n");
    write(
        &app,
        "src/cart.component.html",
        "<div fxLayout=\"row\" [ngClass]=\"c\">\n  <button uiButton [uiTooltip]=\"t\" (click)=\"go()\">Go</button>\n  <ui-paginator [withCursors]=\"true\"></ui-paginator>\n  <ui-paginator [total]=\"n\"></ui-paginator>\n  <span uiButton></span>\n</div>\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&lib, &app].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let angular_of = |root: &PathBuf, class: &str| {
        let store = kc_index::open(root).unwrap();
        let id: i64 = store.conn().query_row("SELECT id FROM symbols WHERE name = ?1", [class], |r| r.get(0)).unwrap();
        let mut a = query::angular(store.conn(), id).unwrap().unwrap();
        kc_index::angular::complete(&mut a, root, &summaries).unwrap();
        a
    };
    let uses = |v: &[query::NgUse]| v.iter().map(|u| (u.tag.clone(), u.component.clone(), u.line)).collect::<Vec<_>>();
    let s = |x: &str| Some(x.to_string());
    // Attributes resolve to directives (unresolved ones, inputs like `[total]`, are left out); the two paginators
    // are told apart by `withCursors`; `uiButton` on a span matches no `button[...]`/`a[...]` selector.
    assert_eq!(
        uses(&angular_of(&roots[1], "CartComponent").renders),
        [
            ("[fxLayout]".to_string(), s("UiLayoutDirective"), 1),
            ("[uiButton]".to_string(), s("UiButtonComponent"), 2),
            ("[uiTooltip]".to_string(), s("UiTooltipDirective"), 2),
            ("ui-paginator".to_string(), s("UiCursorPaginatorComponent"), 3),
            ("ui-paginator".to_string(), s("UiPaginatorComponent"), 4),
        ]
    );
    // Back: the directive lists the app template using it.
    let tooltip = angular_of(&roots[0], "UiTooltipDirective");
    assert_eq!(
        tooltip.rendered_by.iter().map(|u| (u.tag.as_str(), u.component.as_deref(), u.repo.as_deref())).collect::<Vec<_>>(),
        [("[uiTooltip]", Some("CartComponent"), Some("shop"))]
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_new_worktree_starts_from_its_repos_index() {
    let turn = turn();
    let base = turn.dir("worktrees");
    let git = |dir: &Path, args: &[&str]| assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
    let main = scratch(&base, "shop");
    for i in 0..20 {
        write(&main, &format!("src/s{i}.ts"), &format!("export class Store{i} {{ load(): number {{ return {i}; }} }}\n"));
    }
    git(&main, &["remote", "add", "origin", "git@gitlab.example.com:apps/shop.git"]);
    git(&main, &["add", "."]);
    git(&main, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let main_root = kc_index::repo_root(&main).unwrap();
    let first = sync(&main_root, &mut kc_index::open(&main_root).unwrap(), &opts).unwrap();
    assert_eq!(first.parsed, 20);

    // A second clone of the repo, at another commit and indexed more recently: not the worktree's best seed.
    let other = base.join("shop-other");
    assert!(Command::new("git").args(["clone", "-q"]).arg(&main).arg(&other).status().unwrap().success());
    git(&other, &["remote", "set-url", "origin", "git@gitlab.example.com:apps/shop.git"]);
    for i in 0..10 {
        write(&other, &format!("src/s{i}.ts"), &format!("export class Store{i} {{ other(): void {{}} }}\n"));
    }
    let other_root = kc_index::repo_root(&other).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    sync(&other_root, &mut kc_index::open(&other_root).unwrap(), &opts).unwrap();

    let wt = base.join("shop-wt");
    git(&main, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);
    write(&wt, "src/s3.ts", "export class Store3 { load(): number { return 33; } save(): void {} }\n");
    let wt_root = kc_index::repo_root(&wt).unwrap();
    let mut store = kc_index::open(&wt_root).unwrap();
    let report = sync(&wt_root, &mut store, &opts).unwrap();
    // Only the file that differs from the seed is parsed again.
    assert_eq!((report.files, report.parsed), (20, 1), "{report:?}");
    assert_eq!(store.meta("root").unwrap().as_deref(), Some(wt_root.to_string_lossy().as_ref()));
    let (hits, _) = query::find(store.conn(), "save", None, 5).unwrap();
    assert_eq!(hits[0].symbol.file, "src/s3.ts");
    // The main checkout's index is untouched.
    let main_store = kc_index::open(&main_root).unwrap();
    assert!(query::find(main_store.conn(), "save", None, 5).unwrap().0.is_empty());

    // Forgetting a checkout removes its index; pruning removes indexes whose checkout is gone.
    drop(store); // eviction requires all readers of this checkout to have closed
    assert!(kc_index::forget(&wt_root).unwrap());
    assert!(!kc_index::db_path(&wt_root).unwrap().exists());
    let gone = base.join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&gone).status().unwrap().success());
    let gone_root = kc_index::repo_root(&gone).unwrap();
    sync(&gone_root, &mut kc_index::open(&gone_root).unwrap(), &opts).unwrap();
    std::fs::remove_dir_all(&gone).unwrap();
    let pruned = kc_index::prune().unwrap();
    assert!(pruned.contains(&gone_root), "{pruned:?}");
    assert!(kc_index::db_path(&main_root).unwrap().exists());
    let _ = std::fs::remove_dir_all(&base);
}

/// The repo identities each summary's manifests reach, for comparing answers across checkouts: an edge into any
/// checkout of a repo is the same edge.
fn edge_identities(summaries: &[kc_index::workspace::RepoSummary], from: &Path) -> Vec<String> {
    let graph = kc_index::workspace::graph(summaries);
    let i = summaries.iter().position(|s| s.root == from).unwrap();
    let mut ids: Vec<String> =
        graph.dependencies(i).iter().map(|r| kc_index::workspace::identity_of(Path::new(r), summaries)).collect();
    ids.sort();
    ids
}

#[test]
fn invariant_another_checkout_of_a_package_repo_never_removes_dependency_edges() {
    let turn = turn();
    let base = turn.dir("dup-owner");
    let git = |dir: &Path, args: &[&str]| assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
    let lib_files = |dir: &Path| {
        write(dir, "package.json", r#"{ "name": "@dup/lib", "version": "1.0.0" }"#);
        write(dir, "src/Dup.Lib.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Dup.Lib</PackageId></PropertyGroup>\n</Project>\n");
        write(dir, "src/a.ts", "export class Thing {}\n");
    };
    let lib = scratch(&base, "lib");
    lib_files(&lib);
    git(&lib, &["remote", "add", "origin", "git@example.test:group/dup-lib.git"]);
    let app = scratch(&base, "app");
    write(&app, "package.json", r#"{ "name": "dup-app", "dependencies": { "@dup/lib": "1.0.0" } }"#);
    write(&app, "src/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup><PackageReference Include=\"Dup.Lib\" Version=\"1.*\" /></ItemGroup>\n</Project>\n");
    write(&app, "src/app.ts", "import { Thing } from '@dup/lib';\n");
    // A repo with no origin declaring a package another repo (with no origin) declares too: two repos, no owner.
    let fork_a = scratch(&base, "fork-a");
    write(&fork_a, "package.json", r#"{ "name": "@dup/forked", "version": "1.0.0" }"#);
    let fork_b = scratch(&base, "fork-b");
    write(&fork_b, "package.json", r#"{ "name": "@dup/forked", "version": "1.0.0" }"#);
    write(&app, "package.json", r#"{ "name": "dup-app", "dependencies": { "@dup/lib": "1.0.0", "@dup/forked": "1.0.0" } }"#);

    let opts = SyncOptions { threads: 1, ..Default::default() };
    let index = |dir: &Path| {
        let r = kc_index::repo_root(dir).unwrap();
        sync(&r, &mut kc_index::open(&r).unwrap(), &opts).unwrap();
        r
    };
    let roots: Vec<PathBuf> = [&lib, &app, &fork_a, &fork_b].iter().map(|d| index(d)).collect();
    let (lib_root, app_root) = (&roots[0], &roots[1]);
    let before = kc_index::workspace::summaries(&[]).unwrap();
    let before_ids = edge_identities(&before, app_root);
    assert_eq!(before_ids, ["example.test/group/dup-lib"], "the forked package has no owner");
    let dependents = |summaries: &[kc_index::workspace::RepoSummary], of: &Path| -> Vec<PathBuf> {
        kc_index::workspace::neighbours(of, summaries).1.iter().map(|s| s.root.clone()).collect()
    };
    assert_eq!(dependents(&before, lib_root), std::slice::from_ref(app_root));
    drop(before);

    // A second checkout of the library (a worktree, a second clone), its origin written another way.
    let worktree = scratch(&base, "lib-worktree");
    lib_files(&worktree);
    git(&worktree, &["remote", "add", "origin", "https://example.test/group/dup-lib"]);
    let worktree_root = index(&worktree);
    let after = kc_index::workspace::summaries(&[]).unwrap();
    // Invariant: every edge the first checkout's answers had is still there, into the same repo.
    assert_eq!(edge_identities(&after, app_root), before_ids);
    // Both package kinds keep their owner, answered by the most recently synced checkout, the other listed.
    let owners = kc_index::package_owners(&after.iter().map(|s| s.root.clone()).collect::<Vec<_>>()).unwrap();
    for package in ["@dup/lib", "Dup.Lib"] {
        assert_eq!(owners.get(package).map(Path::new), Some(worktree_root.as_path()), "{package}");
    }
    assert_eq!(owners.get("@dup/forked"), None);
    let app_summary = after.iter().find(|s| &s.root == app_root).unwrap();
    let edges = manifests::repo_edges(&app_summary.packages, &app_root.to_string_lossy(), &app_summary.identity(), &owners);
    assert_eq!(edges.len(), 2, "{edges:?}");
    assert!(edges.iter().all(|e| e.other_checkouts == [lib_root.to_string_lossy().into_owned()]), "{edges:?}");
    // Either checkout of the library has the app as its dependent, once.
    assert_eq!(dependents(&after, lib_root), std::slice::from_ref(app_root));
    assert_eq!(dependents(&after, &worktree_root), std::slice::from_ref(app_root));

    // The policy: a queried checkout answers for its own repo; else the most recently synced.
    let checkouts: Vec<&kc_index::workspace::RepoSummary> =
        after.iter().filter(|s| s.root == *lib_root || s.root == worktree_root).collect();
    assert_eq!(kc_index::workspace::choose(&checkouts, &[]).unwrap().root, worktree_root);
    assert_eq!(kc_index::workspace::choose(&checkouts, std::slice::from_ref(lib_root)).unwrap().root, *lib_root);
    let one_each = kc_index::workspace::distinct(&after, std::slice::from_ref(lib_root));
    assert_eq!(one_each.iter().filter(|s| s.identity() == "example.test/group/dup-lib").count(), 1);
    assert!(one_each.iter().any(|s| s.root == *lib_root));
    let _ = std::fs::remove_dir_all(&base);
}

/// Two unrelated backends with an action of one name, and generated clients calling an operation of that name with
/// no tag. A client known to call one backend (another of its requests is served there) links only there; a client
/// with nothing saying which backend it calls links to both, `ambiguous`, assuming `server_unverified`. From the other
/// backend's action, the first client is no caller.
#[test]
fn untagged_operations_link_to_the_servers_their_client_calls() {
    let turn = turn();
    let base = turn.dir("operations");
    let shop = scratch(&base, "shop-api");
    write(
        &shop,
        "src/ProductsController.cs",
        "[ApiController]\n[Route(\"api/products\")]\npublic class ProductsController : ControllerBase\n{\n    [HttpGet]\n    public Task ListItems() => null;\n    [HttpGet(\"featured\")]\n    public Task Featured() => null;\n}\n",
    );
    let wiki = scratch(&base, "wiki-api");
    write(
        &wiki,
        "src/PagesController.cs",
        "[ApiController]\n[Route(\"api/pages\")]\npublic class PagesController : ControllerBase\n{\n    [HttpGet]\n    public Task ListItems() => null;\n    [HttpGet(\"all\")]\n    public Task ListPages() => null;\n}\n",
    );
    let web = scratch(&base, "shop-web");
    write(
        &web,
        "src/catalog.ts",
        "export class Catalog {\n  featured() { return fetch('/api/products/featured'); }\n  all() { return this.apiClient.listItems(); }\n  pages() { return this.apiClient.listPages(); }\n}\n",
    );
    let kiosk = scratch(&base, "kiosk");
    write(&kiosk, "src/screen.ts", "export class Screen {\n  load() { return this.apiClient.listItems(); }\n}\n");
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&shop, &wiki, &web, &kiosk].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let routes = |i: usize| summaries.iter().find(|s| s.root == roots[i]).unwrap().routes.clone();
    let operation = |i: usize, symbol: &str| routes(i).into_iter().find(|r| r.symbol == symbol && r.side == "operation").unwrap_or_else(|| panic!("{symbol}"));
    use kc_store::answer::Assumption::{OperationName, SameDeployment, ServerUnverified};
    let link = |i: usize, symbol: &str| -> Vec<(String, String, &'static str, Vec<kc_store::answer::Assumption>)> {
        kc_index::http::handlers(&operation(i, symbol), &roots[i], &summaries)
            .into_iter()
            .map(|l| (l.repo.file_name().unwrap().to_string_lossy().into_owned(), l.route.symbol, l.confidence, l.assumptions))
            .collect()
    };
    // shop-web's other request is served by shop-api: its operation is shop-api's action only.
    assert_eq!(link(2, "Catalog.all"), [("shop-api".to_string(), "ProductsController.ListItems".to_string(), "exact", vec![SameDeployment, OperationName])]);
    // Nothing says which backend the kiosk calls: both, and the answer says so.
    let unverified = vec![SameDeployment, OperationName, ServerUnverified];
    assert_eq!(
        link(3, "Screen.load"),
        [
            ("shop-api".to_string(), "ProductsController.ListItems".to_string(), "ambiguous", unverified.clone()),
            ("wiki-api".to_string(), "PagesController.ListItems".to_string(), "ambiguous", unverified),
        ]
    );
    // From wiki-api's action: the kiosk may call it; shop-web doesn't.
    let pages = routes(1).into_iter().find(|r| r.symbol == "PagesController.ListItems").unwrap();
    let callers: Vec<String> = kc_index::http::callers(&pages, &roots[1], &summaries).into_iter().map(|l| l.route.symbol).collect();
    assert_eq!(callers, ["Screen.load"]);
    // An operation only the other backend has: no link, and the note says where the name is.
    assert!(link(2, "Catalog.pages").is_empty());
    let note = kc_index::http::unlinked_note(&operation(2, "Catalog.pages"), &summaries, false).unwrap();
    assert!(note.contains("actions named `listPages` only in repos it isn't known to call (wiki-api)"), "{note}");
    let _ = std::fs::remove_dir_all(&base);
}

/// One repo, two apps, a controller of one name in each: the API's has a `[Route]`, the admin app's none and is mapped
/// by `MapDefaultControllerRoute`. The admin app's actions are placed by the conventional template (`Async`
/// dropped), never under the API's prefix, so a request to the API's route links to the API's action only. A repo
/// mapping no conventional route leaves such an action unplaced and says why.
#[test]
fn a_conventionally_routed_controller_never_takes_a_namesakes_prefix() {
    let turn = turn();
    let base = turn.dir("namesakes");
    let server = scratch(&base, "store-server");
    for dir in ["src/Api", "src/Admin"] {
        std::fs::create_dir_all(server.join(dir)).unwrap();
    }
    write(
        &server,
        "src/Api/OrdersController.cs",
        "[ApiController]\n[Route(\"orders\")]\npublic class OrdersController : ControllerBase\n{\n    [HttpPost]\n    public Task Create() => null;\n}\n",
    );
    write(
        &server,
        "src/Admin/OrdersController.cs",
        "public class OrdersController : Controller\n{\n    public Task<IActionResult> Index() => null;\n    [HttpPost]\n    public Task<IActionResult> UnlinkAsync(Guid id) => null;\n}\n",
    );
    write(
        &server,
        "src/Admin/Startup.cs",
        "public class Startup\n{\n    public void Configure(IApplicationBuilder app)\n    {\n        app.UseEndpoints(endpoints => endpoints.MapDefaultControllerRoute());\n    }\n}\n",
    );
    let legacy = scratch(&base, "legacy-server");
    write(&legacy, "src/ReportsController.cs", "public class ReportsController : Controller\n{\n    [HttpPost]\n    public Task Rebuild() => null;\n}\n");
    let web = scratch(&base, "store-web");
    write(&web, "src/orders.ts", "export class Orders {\n  create(o: Order) { return fetch('/orders', { method: 'POST', body: JSON.stringify(o) }); }\n}\n");
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&server, &legacy, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let routes = |i: usize| summaries.iter().find(|s| s.root == roots[i]).unwrap().routes.clone();
    let served: Vec<(String, String, String, String)> =
        routes(0).into_iter().filter(|r| r.side == "server").map(|r| (r.file, r.symbol, r.verb, r.path.unwrap_or_default())).collect();
    let s = |file: &str, sym: &str, verb: &str, path: &str| (file.to_string(), sym.to_string(), verb.to_string(), path.to_string());
    assert_eq!(
        served,
        [
            s("src/Admin/OrdersController.cs", "OrdersController.Index", "*", "Orders"),
            s("src/Admin/OrdersController.cs", "OrdersController.Index", "*", "Orders/Index"),
            s("src/Admin/OrdersController.cs", "OrdersController.Index", "*", "Orders/Index/{id}"),
            s("src/Admin/OrdersController.cs", "OrdersController.UnlinkAsync", "POST", "Orders/Unlink"),
            s("src/Admin/OrdersController.cs", "OrdersController.UnlinkAsync", "POST", "Orders/Unlink/{id}"),
            s("src/Api/OrdersController.cs", "OrdersController.Create", "POST", "orders"),
        ]
    );
    let create = routes(2).into_iter().find(|r| r.symbol == "Orders.create").unwrap();
    let links: Vec<(String, &'static str)> =
        kc_index::http::handlers(&create, &roots[2], &summaries).into_iter().map(|l| (l.route.file, l.confidence)).collect();
    assert_eq!(links, [("src/Api/OrdersController.cs".to_string(), "exact")]);
    // No conventional route mapped: the action without a template is unplaced, not served at the root.
    let legacy: Vec<(String, Option<String>, Option<String>)> =
        routes(1).into_iter().filter(|r| r.symbol == "ReportsController.Rebuild").map(|r| (r.side, r.path, r.detail)).collect();
    assert_eq!(legacy.len(), 1, "{legacy:?}");
    assert_eq!(legacy[0].0, "unsupported");
    assert!(legacy[0].2.as_deref().is_some_and(|d| d.contains("no conventional route template read")), "{legacy:?}");
    let _ = std::fs::remove_dir_all(&base);
}

/// One repo, two apps, each with its own project and conventional routes: the admin app maps the default template,
/// the tools app `tools/{controller}/{action}`. Each app's `ReportsController` takes its own app's template only, so a
/// request to `/tools/Reports/Rebuild` links to the tools app's action, not the admin app's namesake.
#[test]
fn each_app_routes_its_controllers_by_its_own_conventional_template() {
    let turn = turn();
    let base = turn.dir("app-templates");
    let server = scratch(&base, "store-server");
    for dir in ["src/Admin", "src/Tools"] {
        std::fs::create_dir_all(server.join(dir)).unwrap();
    }
    let project = "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n";
    write(&server, "src/Admin/Admin.csproj", project);
    write(&server, "src/Tools/Tools.csproj", project);
    let controller = "public class ReportsController : Controller\n{\n    [HttpPost]\n    public Task<IActionResult> Rebuild() => null;\n}\n";
    write(&server, "src/Admin/ReportsController.cs", controller);
    write(&server, "src/Tools/ReportsController.cs", controller);
    write(
        &server,
        "src/Admin/Startup.cs",
        "public class Startup\n{\n    public void Configure(IApplicationBuilder app)\n    {\n        app.UseEndpoints(endpoints => endpoints.MapDefaultControllerRoute());\n    }\n}\n",
    );
    write(&server, "src/Tools/Program.cs", "var app = WebApplication.Create(args);\napp.MapControllerRoute(\"tools\", \"tools/{controller}/{action}\");\napp.Run();\n");
    let web = scratch(&base, "store-web");
    write(&web, "src/reports.ts", "export class Reports {\n  rebuild() { return fetch('/tools/Reports/Rebuild', { method: 'POST' }); }\n}\n");
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&server, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let routes = |i: usize| summaries.iter().find(|s| s.root == roots[i]).unwrap().routes.clone();
    let served: Vec<(String, String)> = routes(0).into_iter().filter(|r| r.side == "server").map(|r| (r.file, r.path.unwrap_or_default())).collect();
    let s = |file: &str, path: &str| (file.to_string(), path.to_string());
    assert_eq!(
        served,
        [
            s("src/Admin/ReportsController.cs", "Reports/Rebuild"),
            s("src/Admin/ReportsController.cs", "Reports/Rebuild/{id}"),
            s("src/Tools/ReportsController.cs", "tools/Reports/Rebuild"),
        ]
    );
    let rebuild = routes(1).into_iter().find(|r| r.symbol == "Reports.rebuild").unwrap();
    let links: Vec<(String, &'static str)> =
        kc_index::http::handlers(&rebuild, &roots[1], &summaries).into_iter().map(|l| (l.route.file, l.confidence)).collect();
    assert_eq!(links, [("src/Tools/ReportsController.cs".to_string(), "exact")]);
    let _ = std::fs::remove_dir_all(&base);
}

/// A conventional route's defaults, inline or given as `defaults: new { … }`: `Index`, the default action, is served
/// at its controller's route (`Orders` as well as `Orders/Index`), and the default controller's at the root. A
/// request to `/Orders` links to `OrdersController.Index`; one with a verb an action is constrained to links to that
/// action alone, not to an action answering every verb at the same path.
#[test]
fn default_actions_are_served_at_their_controllers_route() {
    let turn = turn();
    let base = turn.dir("default-actions");
    let server = scratch(&base, "shop-admin");
    write(&server, "src/Admin.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n");
    write(
        &server,
        "src/Program.cs",
        "var app = WebApplication.Create(args);\napp.MapControllerRoute(name: \"default\", pattern: \"{controller}/{action}/{id?}\", defaults: new { controller = \"Home\", action = \"Index\" });\napp.Run();\n",
    );
    write(&server, "src/HomeController.cs", "public class HomeController : Controller\n{\n    public IActionResult Index() => null;\n}\n");
    write(
        &server,
        "src/OrdersController.cs",
        "public class OrdersController : Controller\n{\n    public Task<IActionResult> IndexAsync() => null;\n    public IActionResult List() => null;\n}\n",
    );
    write(
        &server,
        "src/ReportsController.cs",
        "public class ReportsController : Controller\n{\n    public IActionResult Index() => null;\n    [HttpPost(\"~/Reports\")]\n    public IActionResult Create() => null;\n}\n",
    );
    let web = scratch(&base, "shop-web");
    write(
        &web,
        "src/admin.ts",
        "export class Admin {\n  orders() { return fetch('/Orders'); }\n  report() { return fetch('/Reports', { method: 'POST' }); }\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&server, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let routes = |i: usize| summaries.iter().find(|s| s.root == roots[i]).unwrap().routes.clone();
    let served: Vec<(String, String)> =
        routes(0).into_iter().filter(|r| r.side == "server" && r.symbol.contains(".Index")).map(|r| (r.symbol, r.path.unwrap_or_default())).collect();
    let s = |sym: &str, path: &str| (sym.to_string(), path.to_string());
    assert_eq!(
        served,
        [
            s("HomeController.Index", ""),
            s("HomeController.Index", "Home"),
            s("HomeController.Index", "Home/Index"),
            s("HomeController.Index", "Home/Index/{id}"),
            s("OrdersController.IndexAsync", "Orders"),
            s("OrdersController.IndexAsync", "Orders/Index"),
            s("OrdersController.IndexAsync", "Orders/Index/{id}"),
            s("ReportsController.Index", "Reports"),
            s("ReportsController.Index", "Reports/Index"),
            s("ReportsController.Index", "Reports/Index/{id}"),
        ]
    );
    let link = |sym: &str| -> Vec<(String, &'static str)> {
        let r = routes(1).into_iter().find(|r| r.symbol == sym).unwrap();
        kc_index::http::handlers(&r, &roots[1], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence)).collect()
    };
    assert_eq!(link("Admin.orders"), [("OrdersController.IndexAsync".to_string(), "exact")]);
    assert_eq!(link("Admin.report"), [("ReportsController.Create".to_string(), "exact")]);
    let _ = std::fs::remove_dir_all(&base);
}

/// A call through a class the client repo implements itself (`new Client(…).login()`) is no generated client's
/// operation, so it links to no action of its name; a client the repo only declares (`.d.ts`) still is, `ambiguous`
/// (its code isn't read). An untagged client's one-word operation (`update`) links `ambiguous` at most: the name alone
/// doesn't say whose method it is.
#[test]
fn only_generated_clients_link_by_operation_name() {
    let turn = turn();
    let base = turn.dir("operation-receivers");
    let api = scratch(&base, "vault-api");
    write(
        &api,
        "src/AccountsController.cs",
        "[ApiController]\n[Route(\"accounts\")]\npublic class AccountsController : ControllerBase\n{\n    [HttpPost(\"login\")]\n    public Task Login() => null;\n    [HttpGet(\"profile\")]\n    public Task GetProfile() => null;\n    [HttpPut(\"{id}\")]\n    public Task Update(string id) => null;\n    [HttpGet(\"devices\")]\n    public Task ListDevices() => null;\n}\n",
    );
    let web = scratch(&base, "vault-web");
    write(
        &web,
        "src/importer.ts",
        "export class Client {\n  login(user: string) { return this.rest.post(user); }\n}\nexport class Importer {\n  run(user: string) {\n    const client = new Client();\n    return client.login(user);\n  }\n}\n",
    );
    write(&web, "src/apiclient.d.ts", "declare class ApiClient {\n  getProfile(): Promise<any>;\n}\n");
    write(
        &web,
        "src/profile.ts",
        "export class Profile {\n  load(apiClient: ApiClient) { return apiClient.getProfile(); }\n  save(id: string) { return this.withSdk((client) => client.update(id)); }\n  devices() { return fetch('/accounts/devices'); }\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&api, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let ops: Vec<kc_store::query::RouteRow> = summaries.iter().find(|s| s.root == roots[1]).unwrap().routes.iter().filter(|r| r.side == "operation").cloned().collect();
    // `client.login` reaches the repo's own `Client.login`: not an operation.
    let names: Vec<&str> = ops.iter().filter_map(|r| r.path.as_deref()).collect();
    assert_eq!(names, ["getProfile", "update"]);
    let link = |op: &str| -> Vec<(String, &'static str)> {
        let r = ops.iter().find(|r| r.path.as_deref() == Some(op)).unwrap();
        kc_index::http::handlers(r, &roots[1], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence)).collect()
    };
    assert_eq!(link("getProfile"), [("AccountsController.GetProfile".to_string(), "ambiguous")]);
    assert_eq!(link("update"), [("AccountsController.Update".to_string(), "ambiguous")]);
    let _ = std::fs::remove_dir_all(&base);
}

/// A receiver reached through a typed field (`this.client` with `private client: Client`, a parameter property or a
/// field, declared here or in a base class) is judged by the field's type: a class of the repo's is its own code, and
/// a type named for no API client (`chrome.storage.StorageArea` behind `chromeStorageApi`) is no client, so neither
/// is an operation; a generated client's type (`ItemsApi`) still is, and so is an external client the repo declares.
#[test]
fn typed_field_receivers_are_judged_by_their_type() {
    let turn = turn();
    let base = turn.dir("typed-fields");
    let api = scratch(&base, "vault-api");
    write(
        &api,
        "src/ItemsController.cs",
        "[ApiController]\n[Route(\"items\")]\npublic class ItemsController : ControllerBase\n{\n    [HttpDelete(\"{id}\")]\n    public Task Remove(string id) => null;\n    [HttpGet]\n    public Task GetItems() => null;\n    [HttpPost(\"open\")]\n    public Task OpenVault() => null;\n    [HttpGet(\"profile\")]\n    public Task GetProfile() => null;\n}\n",
    );
    let web = scratch(&base, "vault-web");
    write(&web, "src/client.ts", "export class Client {\n  openVault() { return this.rest.post('x'); }\n}\n");
    write(&web, "src/vault.ts", "import { Client } from './client';\nexport class Vault {\n  private vaultClient: Client;\n  open() { return this.vaultClient.openVault(); }\n}\n");
    write(
        &web,
        "src/storage.ts",
        "export abstract class BaseStorage {\n  constructor(protected chromeStorageApi: chrome.storage.StorageArea) {}\n}\nexport class LocalStorage extends BaseStorage {\n  drop(key: string) { return this.chromeStorageApi.remove(key); }\n}\n",
    );
    write(&web, "src/apiclient.d.ts", "declare class ApiClient {\n  getProfile(): Promise<any>;\n}\n");
    write(
        &web,
        "src/items.ts",
        "import { ItemsApi } from '@media/sdk';\nexport class Items {\n  constructor(private itemsApi: ItemsApi, private apiClient: ApiClient) {}\n  load() { return this.itemsApi.getItems(); }\n  profile() { return this.apiClient.getProfile(); }\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&api, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let ops: Vec<kc_store::query::RouteRow> = summaries.iter().find(|s| s.root == roots[1]).unwrap().routes.iter().filter(|r| r.side == "operation").cloned().collect();
    let names: Vec<(&str, &str)> = ops.iter().map(|r| (r.symbol.as_str(), r.path.as_deref().unwrap_or_default())).collect();
    assert_eq!(names, [("Items.load", "getItems"), ("Items.profile", "getProfile")]);
    let link = |op: &str| -> Vec<(String, &'static str)> {
        let r = ops.iter().find(|r| r.path.as_deref() == Some(op)).unwrap();
        kc_index::http::handlers(r, &roots[1], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence)).collect()
    };
    assert_eq!(link("getItems"), [("ItemsController.GetItems".to_string(), "exact")]);
    // An external client the repo only declares: its code isn't read, so never `exact`.
    assert_eq!(link("getProfile"), [("ItemsController.GetProfile".to_string(), "ambiguous")]);
    let _ = std::fs::remove_dir_all(&base);
}

/// Requests whose path is built by a function of the repo, or chosen by a condition: `addQuery('/events', start)`,
/// with `addQuery` (in another file) appending only a query string, requests `events`; `all ? '/sync?all=true' :
/// '/sync'` requests `sync` either way. A function that may change the path (`withVersion`) leaves its request
/// unread, saying which function, never guessed.
#[test]
fn paths_given_to_url_functions_are_read_only_when_known() {
    let turn = turn();
    let base = turn.dir("path-params");
    let api = scratch(&base, "vault-api");
    write(
        &api,
        "src/EventsController.cs",
        "[ApiController]\npublic class EventsController : ControllerBase\n{\n    [HttpGet(\"events\")]\n    public Task List() => null;\n    [HttpGet(\"sync\")]\n    public Task Sync() => null;\n    [HttpGet(\"orders/{id}\")]\n    public Task Order(string id) => null;\n}\n",
    );
    let web = scratch(&base, "vault-web");
    write(
        &web,
        "src/query.ts",
        "export function addQuery(base: string, start: string, end: string): string {\n  if (start != null) {\n    base += \"?start=\" + start;\n  }\n  if (end != null) {\n    base += base.indexOf(\"?\") > -1 ? \"&\" : \"?\";\n    base += \"end=\" + end;\n  }\n  return base;\n}\nexport function withVersion(base: string): string {\n  return base + \"/v2\";\n}\n",
    );
    write(
        &web,
        "src/api.service.ts",
        "import { addQuery, withVersion } from './query';\nexport class ApiService {\n  getEvents(start: string, end: string) {\n    return this.send('GET', addQuery('/events', start, end));\n  }\n  getSync(all: boolean) {\n    const path = all ? '/sync?all=true' : '/sync';\n    return this.send('GET', path);\n  }\n  getOrder() {\n    return this.send('GET', withVersion('/orders/'));\n  }\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&api, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let routes: Vec<kc_store::query::RouteRow> = summaries.iter().find(|s| s.root == roots[1]).unwrap().routes.clone();
    let rows: Vec<(&str, &str, Option<&str>)> =
        routes.iter().filter(|r| r.side != "server").map(|r| (r.symbol.as_str(), r.side.as_str(), r.path.as_deref())).collect();
    assert_eq!(
        rows,
        [("ApiService.getEvents", "client", Some("events")), ("ApiService.getSync", "client", Some("sync")), ("ApiService.getOrder", "unresolved", None)]
    );
    let order = routes.iter().find(|r| r.symbol == "ApiService.getOrder").unwrap();
    assert!(order.detail.as_deref().is_some_and(|d| d.contains("withVersion")), "{order:?}");
    let link = |sym: &str| -> Vec<(String, &'static str)> {
        let r = routes.iter().find(|r| r.symbol == sym).unwrap();
        kc_index::http::handlers(r, &roots[1], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence)).collect()
    };
    assert_eq!(link("ApiService.getEvents"), [("EventsController.List".to_string(), "exact")]);
    assert_eq!(link("ApiService.getSync"), [("EventsController.Sync".to_string(), "exact")]);
    let _ = std::fs::remove_dir_all(&base);
}

/// A request whose URL is a `const` of its function (`{ url }`, `getUrl(url)`) links as the URL written in the call
/// would, assuming `url_local` and naming the local; a `let` assigned again doesn't link.
#[test]
fn urls_in_const_locals_link_naming_the_local() {
    let turn = turn();
    let base = turn.dir("url-locals");
    let api = scratch(&base, "album-api");
    write(
        &api,
        "src/AlbumsController.cs",
        "[ApiController]\npublic class AlbumsController : ControllerBase\n{\n    [HttpPost(\"Albums\")]\n    public Task Create() => null;\n    [HttpDelete(\"Tracks/{trackId}/Captions/{index}\")]\n    public Task DeleteCaption(string trackId, int index) => null;\n}\n",
    );
    let web = scratch(&base, "album-web");
    write(
        &web,
        "src/albums.js",
        "export function createAlbum(apiClient, dlg) {\n    const url = apiClient.getUrl('Albums', { Name: dlg.name });\n    return apiClient.ajax({ type: 'POST', url, dataType: 'json' });\n}\nexport function removeCaption(apiClient, trackId, index) {\n    const url = 'Tracks/' + trackId + '/Captions/' + index;\n    return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) });\n}\nexport function moved(apiClient, trackId, index) {\n    let url = 'Tracks/' + trackId + '/Captions/' + index;\n    url = url + '/moved';\n    return apiClient.ajax({ type: 'DELETE', url: apiClient.getUrl(url) });\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&api, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let routes: Vec<kc_store::query::RouteRow> = summaries.iter().find(|s| s.root == roots[1]).unwrap().routes.clone();
    let rows: Vec<(&str, &str, Option<&str>, Option<&str>)> =
        routes.iter().map(|r| (r.symbol.as_str(), r.side.as_str(), r.path.as_deref(), r.local.as_deref())).collect();
    assert_eq!(
        rows,
        [
            ("createAlbum", "client", Some("Albums"), Some("url at src/albums.js:2")),
            ("removeCaption", "client", Some("Tracks/{}/Captions/{}"), Some("url at src/albums.js:6")),
        ]
    );
    let link = |sym: &str| -> Vec<(String, &'static str, Vec<kc_store::answer::Assumption>)> {
        let r = routes.iter().find(|r| r.symbol == sym).unwrap();
        kc_index::http::handlers(r, &roots[1], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence, l.assumptions)).collect()
    };
    let assumed = vec![kc_store::answer::Assumption::SameDeployment, kc_store::answer::Assumption::UrlLocal];
    assert_eq!(link("createAlbum"), [("AlbumsController.Create".to_string(), "exact", assumed.clone())]);
    assert_eq!(link("removeCaption"), [("AlbumsController.DeleteCaption".to_string(), "exact", assumed)]);
    // From the action's side, the same links.
    let server = summaries.iter().find(|s| s.root == roots[0]).unwrap();
    let action = server.routes.iter().find(|r| r.symbol == "AlbumsController.Create").unwrap();
    let callers = kc_index::http::callers(action, &roots[0], &summaries);
    assert_eq!(callers.iter().map(|l| (l.route.symbol.as_str(), l.route.local.as_deref())).collect::<Vec<_>>(), [("createAlbum", Some("url at src/albums.js:2"))]);
    let _ = std::fs::remove_dir_all(&base);
}

/// A call to an external client the repo only declares (`declare module 'media-apiclient'` in a `.d.ts`) whose
/// method names no action: its route isn't in the workspace, and the answer says so, in a list too, instead of
/// leaving the call out silently. One named like an action links to it and to its `…Legacy` twin, `ambiguous`: the
/// client's code, which would say which route it requests, isn't read.
#[test]
fn an_external_clients_unlinked_call_says_where_it_is_declared() {
    let turn = turn();
    let base = turn.dir("sdk-boundary");
    let api = scratch(&base, "media-api");
    write(
        &api,
        "src/PlaystateController.cs",
        "[ApiController]\n[Route(\"UserPlayedItems\")]\npublic class PlaystateController : ControllerBase\n{\n    [HttpDelete(\"{itemId}\")]\n    public Task MarkUnplayedItem(string itemId) => null;\n    [HttpGet(\"{itemId}\")]\n    public Task GetPlayedState(string itemId) => null;\n    [HttpGet(\"/Users/{userId}/PlayedItems/{itemId}\")]\n    public Task GetPlayedStateLegacy(string userId, string itemId) => null;\n}\n",
    );
    let web = scratch(&base, "media-web");
    write(
        &web,
        "src/apiclient.d.ts",
        "declare module 'media-apiclient' {\n    class ApiClient {\n        getPlayedState(itemId: string): Promise<any>;\n        markUnplayed(userId: string, itemId: string): Promise<any>;\n    }\n}\n",
    );
    write(
        &web,
        "src/menu.ts",
        "export class Menu {\n  played(apiClient: ApiClient, id: string) { return apiClient.getPlayedState(id); }\n  unplay(apiClient: ApiClient, id: string) { return apiClient.markUnplayed(this.userId, id); }\n}\n",
    );
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&api, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let op = |name: &str| {
        summaries.iter().find(|s| s.root == roots[1]).unwrap().routes.iter().find(|r| r.side == "operation" && r.path.as_deref() == Some(name)).cloned().unwrap()
    };
    // A method named like an action still links by name, to its `…Legacy` twin too, neither `exact`.
    use kc_store::answer::Assumption::{ClientUnread, OperationName, SameDeployment, ServerUnverified};
    let unread = vec![SameDeployment, OperationName, ServerUnverified, ClientUnread];
    let played = kc_index::http::handlers(&op("getPlayedState"), &roots[1], &summaries);
    assert_eq!(
        played.iter().map(|l| (l.route.symbol.as_str(), l.confidence, l.assumptions.clone())).collect::<Vec<_>>(),
        [
            ("PlaystateController.GetPlayedState", "ambiguous", unread.clone()),
            ("PlaystateController.GetPlayedStateLegacy", "ambiguous", unread),
        ]
    );
    // From the twin's side, the same call.
    let server = summaries.iter().find(|s| s.root == roots[0]).unwrap();
    let twin = server.routes.iter().find(|r| r.symbol == "PlaystateController.GetPlayedStateLegacy").unwrap();
    let back: Vec<_> = kc_index::http::callers(twin, &roots[0], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence)).collect();
    assert_eq!(back, [("Menu.played".to_string(), "ambiguous")]);
    let unplay = op("markUnplayed");
    assert!(kc_index::http::handlers(&unplay, &roots[1], &summaries).is_empty());
    for brief in [true, false] {
        let note = kc_index::http::unlinked_note(&unplay, &summaries, brief).unwrap();
        assert!(note.contains("an external client: ApiClient.markUnplayed is declared at src/apiclient.d.ts:4"), "{note}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// A declared external client's package installed in `node_modules` at the version the lockfile pins is read (a
/// minified bundle through its source map's original sources): a call links through the package method's request,
/// assuming `package_code` and citing the package, its version and the method, even where no action is named like
/// the method; from the action's side, the same call. A method making two requests links `ambiguous`; one making
/// none says so. Installed at another version than the lockfile pins, the package isn't read, and the answer says
/// why.
#[test]
fn declared_clients_link_through_their_installed_package() {
    let turn = turn();
    let base = turn.dir("sdk-package");
    let api = scratch(&base, "media-api");
    write(
        &api,
        "src/PlaystateController.cs",
        "[ApiController]\n[Route(\"\")]\npublic class PlaystateController : ControllerBase\n{\n    [HttpPost(\"UserPlayedItems/{itemId}\")]\n    public Task MarkPlayedItem(string itemId) => null;\n    [HttpPost(\"Users/{userId}/PlayedItems/{itemId}\")]\n    public Task MarkPlayedItemLegacy(string userId, string itemId) => null;\n    [HttpGet(\"LiveTv/SeriesTimers\")]\n    public Task GetSeriesTimers() => null;\n    [HttpGet(\"LiveTv/Programs\")]\n    public Task GetLiveTvPrograms() => null;\n    [HttpPost(\"LiveTv/Programs\")]\n    public Task GetPrograms() => null;\n}\n",
    );
    let web = scratch(&base, "media-web");
    write(
        &web,
        "src/apiclient.d.ts",
        "declare module 'media-apiclient' {\n    class ApiClient {\n        markPlayed(userId: string, itemId: string): Promise<any>;\n        getLiveTvSeriesTimers(query: any): Promise<any>;\n        getLiveTvPrograms(query: any): Promise<any>;\n        serverId(): string;\n    }\n}\n",
    );
    write(
        &web,
        "src/menu.ts",
        "export class Menu {\n  play(apiClient: ApiClient, id: string) { return apiClient.markPlayed(this.userId, id); }\n  timers(apiClient: ApiClient) { return apiClient.getLiveTvSeriesTimers({}); }\n  guide(apiClient: ApiClient) { return apiClient.getLiveTvPrograms({}); }\n  server(apiClient: ApiClient) { return apiClient.serverId(); }\n}\n",
    );
    let source = "export default class ApiClient {\n  getUrl(name) { return this._base + '/' + name; }\n  getJSON(url) { return this.ajax({ type: 'GET', url: url, dataType: 'json' }); }\n  markPlayed(userId, itemId) {\n    const url = this.getUrl(`Users/${userId}/PlayedItems/${itemId}`);\n    return this.ajax({ type: 'POST', url: url });\n  }\n  getLiveTvSeriesTimers(options) {\n    const url = this.getUrl('LiveTv/SeriesTimers');\n    return this.getJSON(url);\n  }\n  getLiveTvPrograms(options) {\n    if (options.long) {\n      return this.ajax({ type: 'POST', url: this.getUrl('LiveTv/Programs') });\n    }\n    return this.getJSON(this.getUrl('LiveTv/Programs'));\n  }\n  serverId() { return this._serverInfo.Id; }\n}\n";
    let pkg = web.join("node_modules/media-apiclient");
    std::fs::create_dir_all(pkg.join("dist")).unwrap();
    std::fs::write(pkg.join("package.json"), r#"{ "name": "media-apiclient", "version": "1.2.0", "main": "dist/media-apiclient.js" }"#).unwrap();
    // A bundle on one long line, as a minifier writes it, and its map with the original source.
    std::fs::write(pkg.join("dist/media-apiclient.js"), format!("!function(){{{}}}();\n//# sourceMappingURL=media-apiclient.js.map\n", "var a=1;".repeat(200))).unwrap();
    let map = serde_json::json!({ "version": 3, "sources": ["webpack://media-apiclient/webpack/bootstrap", "webpack://media-apiclient/./src/apiClient.js"], "sourcesContent": ["(function(){})", source], "mappings": "" });
    std::fs::write(pkg.join("dist/media-apiclient.js.map"), map.to_string()).unwrap();
    let lock = |v: &str| write(&web, "package-lock.json", &format!(r#"{{ "lockfileVersion": 3, "packages": {{ "": {{}}, "node_modules/media-apiclient": {{ "version": "{v}" }} }} }}"#));
    lock("1.2.0");
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&api, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    // The package's files are no files of the repo's.
    assert!(!summaries.iter().find(|s| s.root == roots[1]).unwrap().routes.iter().any(|r| r.file.contains("node_modules")));
    let op = |summaries: &[kc_index::workspace::RepoSummary], name: &str| {
        summaries.iter().find(|s| s.root == roots[1]).unwrap().routes.iter().find(|r| r.side == "operation" && r.path.as_deref() == Some(name)).cloned().unwrap()
    };
    use kc_store::answer::Assumption::{PackageCode, SameDeployment};
    let links = |summaries: &[kc_index::workspace::RepoSummary], name: &str| -> Vec<(String, &'static str, Vec<kc_store::answer::Assumption>)> {
        kc_index::http::handlers(&op(summaries, name), &roots[1], summaries).into_iter().map(|l| (l.route.symbol, l.confidence, l.assumptions)).collect()
    };
    let one = |sym: &str| vec![(sym.to_string(), "exact", vec![SameDeployment, PackageCode])];
    // The request the package makes, not the action named like the method.
    assert_eq!(links(&summaries, "markPlayed"), one("PlaystateController.MarkPlayedItemLegacy"));
    // No action is named like `getLiveTvSeriesTimers`: the package's request links it.
    assert_eq!(links(&summaries, "getLiveTvSeriesTimers"), one("PlaystateController.GetSeriesTimers"));
    let l = kc_index::http::handlers(&op(&summaries, "getLiveTvSeriesTimers"), &roots[1], &summaries).remove(0);
    let p = l.package.unwrap();
    assert_eq!((p.package.as_str(), p.version.as_str(), p.method.as_str()), ("media-apiclient", "1.2.0", "ApiClient.getLiveTvSeriesTimers"));
    assert_eq!(p.at, "src/apiClient.js:8 (from node_modules/media-apiclient/dist/media-apiclient.js.map)");
    assert_eq!(p.request, "GET LiveTv/SeriesTimers");
    // Two requests: both, neither sure.
    let mut guide: Vec<_> = links(&summaries, "getLiveTvPrograms").into_iter().map(|(s, c, _)| (s, c)).collect();
    guide.sort();
    assert_eq!(guide, [("PlaystateController.GetLiveTvPrograms".to_string(), "ambiguous"), ("PlaystateController.GetPrograms".to_string(), "ambiguous")]);
    // From the action's side.
    let server = summaries.iter().find(|s| s.root == roots[0]).unwrap();
    let timers = server.routes.iter().find(|r| r.symbol == "PlaystateController.GetSeriesTimers").unwrap();
    let back: Vec<_> = kc_index::http::callers(timers, &roots[0], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence)).collect();
    assert_eq!(back, [("Menu.timers".to_string(), "exact")]);
    // No request: said so, never "requests nothing".
    let server_id = op(&summaries, "serverId");
    assert!(kc_index::http::handlers(&server_id, &roots[1], &summaries).is_empty());
    let note = kc_index::http::unlinked_note(&server_id, &summaries, true).unwrap();
    assert!(note.contains("media-apiclient 1.2.0's ApiClient.serverId (src/apiClient.js:18 (from node_modules/media-apiclient/dist/media-apiclient.js.map)) makes no request KnowCode reads"), "{note}");

    // Another version than the lockfile pins: not read, and the answer says why.
    lock("1.1.0");
    sync(&roots[1], &mut kc_index::open(&roots[1]).unwrap(), &opts).unwrap();
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    assert!(links(&summaries, "getLiveTvSeriesTimers").is_empty());
    let note = kc_index::http::unlinked_note(&op(&summaries, "getLiveTvSeriesTimers"), &summaries, true).unwrap();
    assert!(note.contains("media-apiclient wasn't read: installed 1.2.0, but package-lock.json pins 1.1.0"), "{note}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_seeded_worktree_hashes_files_whose_stat_matches_the_seed() {
    let turn = turn();
    let base = turn.dir("seed-stats");
    let git = |dir: &Path, args: &[&str]| assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
    let main = scratch(&base, "cart");
    for i in 0..6 {
        write(&main, &format!("src/s{i}.ts"), &format!("export class Store{i} {{ load(): number {{ return {i}; }} }}\n"));
    }
    git(&main, &["remote", "add", "origin", "https://example.test/group/cart.git"]);
    git(&main, &["add", "."]);
    git(&main, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let main_root = kc_index::repo_root(&main).unwrap();
    sync(&main_root, &mut kc_index::open(&main_root).unwrap(), &opts).unwrap();

    // A worktree whose file has the size and the mtime the seed recorded (a copy that kept mtimes), and other content.
    let wt = base.join("cart-wt");
    git(&main, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);
    let recorded = std::fs::metadata(main.join("src/s2.ts")).unwrap().modified().unwrap();
    write(&wt, "src/s2.ts", "export class Store9 { load(): number { return 2; } }\n");
    std::fs::File::options().write(true).open(wt.join("src/s2.ts")).unwrap().set_modified(recorded).unwrap();
    assert_eq!(std::fs::metadata(wt.join("src/s2.ts")).unwrap().len(), std::fs::metadata(main.join("src/s2.ts")).unwrap().len());
    let wt_root = kc_index::repo_root(&wt).unwrap();
    let mut store = kc_index::open(&wt_root).unwrap();
    let report = sync(&wt_root, &mut store, &opts).unwrap();
    assert_eq!(report.parsed, 1, "{report:?}");
    assert!(!query::find(store.conn(), "Store9", None, 5).unwrap().0.is_empty());
    assert!(query::find(store.conn(), "Store2", None, 5).unwrap().0.is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_seeded_worktree_names_its_root_role_after_its_own_directory() {
    let turn = turn();
    let base = turn.dir("seed-role");
    let git = |dir: &Path, args: &[&str]| assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
    let main = scratch(&base, "web-role");
    std::fs::create_dir_all(main.join("tasks")).unwrap();
    write(&main, "tasks/main.yml", "- name: Install the web server\n  ansible.builtin.package:\n    name: nginx\n");
    git(&main, &["remote", "add", "origin", "https://example.test/group/web-role.git"]);
    git(&main, &["add", "."]);
    git(&main, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let main_root = kc_index::repo_root(&main).unwrap();
    let store = {
        let mut s = kc_index::open(&main_root).unwrap();
        sync(&main_root, &mut s, &opts).unwrap();
        s
    };
    assert!(!query::find(store.conn(), "web-role", None, 5).unwrap().0.is_empty());
    drop(store);

    let wt = base.join("web-role-wt");
    git(&main, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);
    let wt_root = kc_index::repo_root(&wt).unwrap();
    let mut store = kc_index::open(&wt_root).unwrap();
    sync(&wt_root, &mut store, &opts).unwrap();
    assert!(!query::find(store.conn(), "web-role-wt", None, 5).unwrap().0.is_empty());
    assert!(query::find(store.conn(), "web-role", None, 5).unwrap().0.iter().all(|h| h.symbol.name != "web-role"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn go_routes_and_clients_link_across_repos() {
    let turn = turn();
    let base = turn.dir("go-http");
    let api = scratch(&base, "orchard-api");
    write(&api, "go.mod", "module example.com/orchard\n\ngo 1.22\n\nrequire github.com/gin-gonic/gin v1.10.0\n");
    write(
        &api,
        "src/routes.go",
        "package api\n\nimport \"github.com/gin-gonic/gin\"\n\nconst treesPath = \"/trees\"\n\n\
         func Router() *gin.Engine {\n\tr := gin.Default()\n\tv1 := r.Group(\"/api/v1\")\n\ttrees(v1.Group(treesPath))\n\treturn r\n}\n\n\
         func trees(g *gin.RouterGroup) {\n\tg.GET(\"/:id\", getTree)\n\tg.DELETE(\"/:id\", removeTree)\n\tg.GET(\"/:id/fruit\", listFruit)\n}\n\n\
         func getTree(c *gin.Context)    {}\nfunc removeTree(c *gin.Context) {}\nfunc listFruit(c *gin.Context)  {}\n",
    );
    let picker = scratch(&base, "picker");
    write(&picker, "go.mod", "module example.com/picker\n\ngo 1.22\n");
    write(
        &picker,
        "src/client.go",
        "package picker\n\nimport (\n\t\"context\"\n\t\"fmt\"\n\t\"net/http\"\n)\n\n\
         type Client struct {\n\tbase string\n\thttp *http.Client\n}\n\n\
         func (p *Client) Tree(id string) {\n\thttp.Get(p.base + \"/api/v1/trees/\" + id)\n}\n\n\
         func (p *Client) Remove(ctx context.Context, id string) {\n\treq, _ := http.NewRequestWithContext(ctx, http.MethodDelete, fmt.Sprintf(\"%s/api/v1/trees/%s\", p.base, id), nil)\n\tp.http.Do(req)\n}\n",
    );
    let web = scratch(&base, "web");
    write(&web, "src/fruit.ts", "export function fruit(id: string) { return fetch(`/api/v1/trees/${id}/fruit`); }\n");
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let roots: Vec<PathBuf> = [&api, &picker, &web].iter().map(|r| kc_index::repo_root(r).unwrap()).collect();
    for r in &roots {
        sync(r, &mut kc_index::open(r).unwrap(), &opts).unwrap();
    }
    let summaries = kc_index::workspace::summaries(&[]).unwrap();
    let routes = |i: usize| summaries.iter().find(|s| s.root == roots[i]).unwrap().routes.clone();
    // The group's prefix and the constant reach the routes the helper registers.
    let served: Vec<(String, String, String)> =
        routes(0).into_iter().filter(|r| r.side == "server").map(|r| (r.symbol, r.verb, r.path.unwrap_or_default())).collect();
    let s = |sym: &str, verb: &str, path: &str| (sym.to_string(), verb.to_string(), path.to_string());
    // Each route belongs to its handler, under the group's prefix and the constant the helper's group was given.
    assert_eq!(
        served,
        [s("getTree", "GET", "api/v1/trees/:id"), s("removeTree", "DELETE", "api/v1/trees/:id"), s("listFruit", "GET", "api/v1/trees/:id/fruit")]
    );
    let link = |i: usize, symbol: &str| -> Vec<(String, &'static str)> {
        let r = routes(i).into_iter().find(|r| r.symbol == symbol).unwrap_or_else(|| panic!("{symbol}"));
        kc_index::http::handlers(&r, &roots[i], &summaries).into_iter().map(|l| (l.route.symbol, l.confidence)).collect()
    };
    let one = |sym: &str| vec![(sym.to_string(), "exact")];
    // Go requests, their base addresses dropped and `:id` a parameter, and a TypeScript one.
    assert_eq!(link(1, "Client.Tree"), one("getTree"));
    assert_eq!(link(1, "Client.Remove"), one("removeTree"));
    assert_eq!(link(2, "fruit"), one("listFruit"));
    let _ = std::fs::remove_dir_all(&base);
}
