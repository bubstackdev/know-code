//! End to end on a scratch git repo: index, query, edit, and check that queries see the edit.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query::{self, ContextLimits};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Sources")).unwrap();
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    std::fs::write(root.join(path), text).unwrap();
}

const STORE: &str = r#"
protocol Archiving { func load() throws -> [String] }

final class QueueArchive: Archiving {
    func load() throws -> [String] { [] }
}

final class PairingArchive {
    func load() throws -> [String] { [] }
}
"#;

const MODEL: &str = r#"
final class AppModel {
    let queue: QueueArchive
    init() {
        queue = QueueArchive()
        let pairing = PairingArchive()
        _ = try? queue.load()
        _ = try? pairing.load()
    }
}
"#;

#[test]
fn index_query_edit_requery() {
    let root = scratch("fresh");
    write(&root, "Sources/Store.swift", STORE);
    write(&root, "Sources/Model.swift", MODEL);
    write(&root, "README.md", "not swift");
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    let opts = SyncOptions { threads: 2, ..Default::default() };

    let first = sync(&root, &mut store, &opts).unwrap();
    assert_eq!((first.files, first.parsed), (2, 2));

    let (hits, _) = query::find(store.conn(), "archive", Some("type"), 10).unwrap();
    let names: Vec<_> = hits.iter().map(|h| h.symbol.name.as_str()).collect();
    assert_eq!(names, ["QueueArchive", "PairingArchive"]);

    // Member calls resolve through the property's declared type and the local's constructor.
    let (refs, summary) = query::refs(store.conn(), "load", &["call"], 10).unwrap();
    assert_eq!(summary.by_confidence.get("exact"), Some(&2));
    let targets: Vec<_> = refs.iter().map(|r| r.targets[0].qualified.as_str()).collect();
    assert_eq!(targets, ["QueueArchive.load", "PairingArchive.load"]);

    let ctx = query::context(store.conn(), &root, "QueueArchive", &ContextLimits::from_budget(1000)).unwrap().unwrap();
    assert_eq!(ctx.relations[0].kind, "conforms_to");
    assert_eq!(ctx.callers_summary.total, 2); // `let queue: QueueArchive` and `QueueArchive()`

    // No-op: nothing reparsed.
    let again = sync(&root, &mut store, &opts).unwrap();
    assert_eq!((again.parsed, again.unchanged), (0, 2));
    assert_eq!(again.discovery, "cached");

    // A new file changes its directory's listing, which forces a full discovery that finds it.
    write(&root, "Sources/Extra.swift", "struct Extra {}\n");
    let added = sync(&root, &mut store, &opts).unwrap();
    assert_eq!((added.discovery, added.parsed), ("full", 1));
    assert_eq!(query::definitions(store.conn(), "Extra").unwrap().len(), 1);
    std::fs::remove_file(root.join("Sources/Extra.swift")).unwrap();
    assert_eq!(sync(&root, &mut store, &opts).unwrap().removed, 1);

    // Edit one file: only it is reparsed, and queries see the change.
    write(&root, "Sources/Model.swift", &MODEL.replace("pairing.load()", "pairing.reload()"));
    let edited = sync(&root, &mut store, &opts).unwrap();
    assert_eq!((edited.discovery, edited.parsed, edited.unchanged), ("cached", 1, 1));
    let (_, summary) = query::refs(store.conn(), "load", &["call"], 10).unwrap();
    assert_eq!(summary.total, 1);

    // Delete a file: its facts go away.
    std::fs::remove_file(root.join("Sources/Store.swift")).unwrap();
    let removed = sync(&root, &mut store, &opts).unwrap();
    assert_eq!(removed.removed, 1);
    assert!(query::definitions(store.conn(), "QueueArchive").unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn resolves_call_results_supertypes_and_reachability() {
    let root = scratch("resolve");
    write(
        &root,
        "Sources/A.swift",
        r#"
protocol Describing {}
extension Describing { func describe() -> String { "" } }

final class Engine: Describing {
    static func make() -> Self { Self() }
    func start() {}
}

final class Other {
    func start() {}
    func helper() {}
}

func makeEngine() -> Engine { Engine() }

enum Mode { case start }

func run() {
    let e = makeEngine()
    e.start()
    let f = Engine.make()
    _ = f.describe()
    helper()
    let names = ["a"]
    _ = names.map { $0 }
    let m: Mode = .start
}
"#,
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();
    let one = |name: &str| {
        let (refs, _) = query::refs(conn, name, &["call"], 10).unwrap();
        refs.into_iter().find(|r| r.from.as_ref().is_some_and(|f| f.name == "run")).unwrap()
    };
    // `let e = makeEngine()` types `e` from the return type.
    let start = one("start");
    assert_eq!((start.confidence, start.targets[0].qualified.as_str()), ("exact", "Engine.start"));
    // `Engine.make() -> Self`, then the protocol-extension default via the supertype chain.
    let describe = one("describe");
    assert_eq!((describe.confidence, describe.targets[0].qualified.as_str()), ("exact", "Describing.describe"));
    // An unqualified call can't reach a method of an unrelated type.
    assert_eq!(one("helper").confidence, "unresolved");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn traces_callers_through_protocols() {
    let root = scratch("trace");
    write(
        &root,
        "Sources/T.swift",
        r#"
protocol Fetching { func fetch() -> Int }
struct Fetcher: Fetching { func fetch() -> Int { 1 } }
final class Model {
    let source: any Fetching
    init(source: any Fetching = Fetcher()) { self.source = source }
    func load() -> Int { source.fetch() }
}
func main() { _ = Model().load() }
"#,
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let opts = kc_store::trace::TraceOptions { direction: kc_store::trace::Direction::In, depth: 3, max_nodes: 50 };
    let t = kc_store::trace::trace(store.conn(), "Fetcher.fetch", &opts).unwrap().unwrap();
    let edges: Vec<(&str, &str, u8)> = t.nodes.iter().skip(1).map(|n| (n.edge, n.qualified.as_str(), n.depth)).collect();
    // Called through the protocol by Model.load, which main calls.
    assert_eq!(edges[0], ("via_protocol", "Model.load", 1));
    assert_eq!(edges[1], ("calls", "main", 2));
    let req = kc_store::trace::trace(store.conn(), "Fetching.fetch", &opts).unwrap().unwrap();
    assert!(req.nodes.iter().any(|n| n.edge == "witness" && n.qualified == "Fetcher.fetch"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn impact_from_symbol_and_working_diff() {
    let root = scratch("impact");
    write(
        &root,
        "Sources/I.swift",
        r#"
struct Store {
    func save() {}
}
struct Model {
    let store = Store()
    func commit() { store.save() }
}
struct View {
    let model = Model()
    func tap() { model.commit() }
}
"#,
    );
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();

    let save = query::definitions(store.conn(), "Store.save").unwrap()[0].id;
    let i = kc_store::trace::impact(store.conn(), &[save], 3, 50).unwrap();
    let got: Vec<(&str, u8)> = i.affected.iter().map(|a| (a.qualified.as_str(), a.hop)).collect();
    assert_eq!(got, [("Model.commit", 1), ("View.tap", 2)]);
    assert_eq!(i.nearest, "direct");

    // Edit inside Store.save: the diff maps to that method, and its dependents follow.
    write(&root, "Sources/I.swift", &std::fs::read_to_string(root.join("Sources/I.swift")).unwrap().replace("func save() {}", "func save() { print(1) }"));
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let changes = kc_index::diff::changes(&root, None).unwrap();
    assert_eq!(changes.len(), 1);
    let ids = kc_store::trace::symbols_in_ranges(store.conn(), &changes[0].file, &changes[0].ranges).unwrap();
    assert_eq!(ids, [save]);

    // Hotspots run on the same repo without git errors.
    let v = kc_index::vitals::hotspots(&root, &store).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].commits_365d, 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn indexes_csharp_and_resolves_members_through_fields() {
    let root = scratch("csharp");
    write(&root, "Sources/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");
    write(
        &root,
        "Sources/Repo.cs",
        r#"
namespace Acme.Cart;

public interface ICartRepository { Task<Cart> GetAsync(int id); }

public class CartRepository : ICartRepository
{
    public async Task<Cart> GetAsync(int id) { return new Cart(); }
}

public class Cart { public void Close() { } }
"#,
    );
    write(
        &root,
        "Sources/Service.cs",
        r#"
namespace Acme.Cart;

public class CartService
{
    private readonly ICartRepository _repo;
    public CartService(ICartRepository repo) { _repo = repo; }

    public async Task Checkout(int id)
    {
        var cart = await _repo.GetAsync(id);
        if (cart != null) { cart.Close(); }
    }
}
"#,
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    let report = sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    assert_eq!((report.files, report.parsed), (3, 3), "two sources and the project");
    let conn = store.conn();

    let (hits, _) = query::find(conn, "CartRepository", Some("type"), 10).unwrap();
    assert_eq!(hits[0].symbol.name, "CartRepository");

    let call = |name: &str| {
        let (refs, _) = query::refs(conn, name, &["call"], 10).unwrap();
        refs.into_iter().find(|r| r.from.as_ref().is_some_and(|f| f.name == "Checkout")).unwrap()
    };
    // `_repo` is a field typed by the interface.
    let get = call("GetAsync");
    assert_eq!((get.confidence, get.targets[0].qualified.as_str()), ("exact", "ICartRepository.GetAsync"));
    // `await _repo.GetAsync(id)` unwraps Task<Cart>, so `cart.Close()` lands on Cart.
    let close = call("Close");
    assert_eq!((close.confidence, close.targets[0].qualified.as_str()), ("exact", "Cart.Close"));

    // Hotspots cover C# files once they are committed.
    let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
    git(&["add", "."]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    let vitals = kc_index::vitals::hotspots(&root, &store).unwrap();
    assert!(vitals.iter().any(|v| v.file == "Sources/Service.cs"), "{:?}", vitals.iter().map(|v| &v.file).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn indexes_typescript_and_resolves_injected_services() {
    let root = scratch("typescript");
    write(
        &root,
        "Sources/cart.repository.ts",
        r#"
@Injectable({ providedIn: 'root' })
export class CartRepository {
  load(id: string): Promise<Cart> { return fetchCart(id); }
}

export class Cart { close(): void {} }
"#,
    );
    write(
        &root,
        "Sources/cart.component.tsx",
        r#"
export class CartComponent {
  constructor(private repo: CartRepository) {}

  async open(id: string) {
    const cart = await this.repo.load(id);
    cart.close();
    return <div>{id}</div>;
  }
}
"#,
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    let report = sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    assert_eq!((report.files, report.parsed), (2, 2));
    let conn = store.conn();
    let call = |name: &str| {
        let (refs, _) = query::refs(conn, name, &["call"], 10).unwrap();
        refs.into_iter().find(|r| r.from.as_ref().is_some_and(|f| f.name == "open")).unwrap()
    };
    let load = call("load");
    assert_eq!((load.confidence, load.targets[0].qualified.as_str()), ("exact", "CartRepository.load"));
    let close = call("close");
    assert_eq!((close.confidence, close.targets[0].qualified.as_str()), ("exact", "Cart.close"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typescript_names_resolve_through_their_imports() {
    let root = scratch("ts-imports");
    std::fs::create_dir_all(root.join("Sources/actions")).unwrap();
    write(&root, "package.json", r#"{ "name": "web", "dependencies": { "rxjs": "^7.8.0", "@ngrx/store": "^17.0.0" } }"#);
    write(&root, "Sources/cart.actions.ts", "export const LoadCart = createAction('[Cart] Load');\nexport class LoadCartType {}\n");
    write(&root, "Sources/actions/index.ts", "export * as fromCart from '../cart.actions';\n");
    // Test doubles and helpers sharing names with library members.
    write(
        &root,
        "Sources/stubs.spec.ts",
        "export class StoreStub { dispatch(a: unknown) {} }\nexport class Logger { log(m: string) {} }\nexport function first() {}\n",
    );
    write(
        &root,
        "Sources/cart.service.ts",
        "export class CartService { load(): void {} }\nexport const CART = new InjectionToken<CartService>('cart');\n",
    );
    write(
        &root,
        "Sources/cart.component.ts",
        r#"
import * as CartActions from './cart.actions';
import { fromCart } from './actions';
import { Store } from '@ngrx/store';
import { first } from 'rxjs/operators';
import { CART } from './cart.service';

export class CartComponent {
  private readonly store = inject(Store<State>);
  private readonly cart = inject(CART);

  run(): void {
    this.store.dispatch(CartActions.LoadCart());
    fromCart.LoadCart();
    this.store.pipe(first());
    console.log('ran');
    this.cart.load();
  }
}
"#,
    );
    write(
        &root,
        "Sources/cart.component.spec.ts",
        r#"
import { TestBed } from '@angular/core/testing';
import { CartService } from './cart.service';

describe('CartComponent', () => {
  let service;
  beforeEach(() => {
    service = TestBed.inject(CartService);
  });
  it('loads', () => {
    service.load();
  });
});
"#,
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();
    let from_run = |name: &str| -> Vec<(&'static str, Vec<String>)> {
        let (refs, _) = query::refs(conn, name, &["call"], 10).unwrap();
        refs.into_iter()
            .filter(|r| r.from.as_ref().is_some_and(|f| f.name == "run"))
            .map(|r| (r.confidence, r.targets.iter().map(|t| format!("{}:{}", t.file, t.qualified)).collect()))
            .collect()
    };
    // A namespace import, directly or re-exported by a barrel, reaches the module's export.
    let action = ("exact", vec!["Sources/cart.actions.ts:LoadCart".to_string()]);
    assert_eq!(from_run("LoadCart"), [action.clone(), action]);
    // Library receivers and library functions never reach the repo's same-named test doubles.
    assert_eq!(from_run("dispatch"), [("unresolved", vec![])]);
    assert_eq!(from_run("first"), [("unresolved", vec![])]);
    assert_eq!(from_run("log"), [("unresolved", vec![])]);
    // An injection token gives the type it was declared for.
    assert_eq!(from_run("load"), [("exact", vec!["Sources/cart.service.ts:CartService.load".to_string()])]);
    // A suite variable assigned `TestBed.inject(T)` is a T.
    let (refs, _) = query::refs(conn, "load", &["call"], 10).unwrap();
    let spec = refs.iter().find(|r| r.file.ends_with("cart.component.spec.ts")).unwrap();
    assert_eq!((spec.confidence, spec.targets[0].qualified.as_str()), ("exact", "CartService.load"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn csharp_extension_methods_resolve_on_foreign_and_local_types() {
    let root = scratch("csharp-ext");
    write(&root, "Sources/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");
    write(
        &root,
        "Sources/Extensions.cs",
        r#"
public static class ResultExtensions
{
    public static void ThrowIfError(this ApiResult response) { }
}

public interface IAccessor { int Id(); }

public static class AccessorExtensions
{
    public static int GetActingAccountId(this IAccessor a) => a.Id();
}
"#,
    );
    write(
        &root,
        "Sources/Service.cs",
        r#"
public class Service
{
    private readonly IAccessor _accessor;
    public void Run(ApiResult response)
    {
        response.ThrowIfError();
        var id = _accessor.GetActingAccountId();
    }
}
"#,
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();
    let call = |name: &str| {
        let (refs, _) = query::refs(conn, name, &["call"], 10).unwrap();
        refs.into_iter().find(|r| r.from.as_ref().is_some_and(|f| f.name == "Run")).unwrap()
    };
    // `ApiResult` is defined elsewhere (a package); the repo's extension of it is the target.
    let t = call("ThrowIfError");
    assert_eq!((t.confidence, t.targets[0].qualified.as_str()), ("exact", "ApiResult.ThrowIfError"));
    let g = call("GetActingAccountId");
    assert_eq!((g.confidence, g.targets[0].qualified.as_str()), ("exact", "IAccessor.GetActingAccountId"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn csharp_lambda_parameters_resolve_through_linq_and_moq() {
    let root = scratch("csharp-lambda");
    write(&root, "Sources/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");
    write(
        &root,
        "Sources/Model.cs",
        r#"
public interface IGateway { Task SendAsync(int id); }
public class Order { public bool IsOpen() => true; }
public class Other { public bool IsOpen() => false; public Task SendAsync(int id) => null; }
"#,
    );
    write(
        &root,
        "Sources/Tests.cs",
        r#"
public class GatewayTests
{
    private readonly Mock<IGateway> _gateway = new();
    public void Run(List<Order> orders)
    {
        var open = orders.Where(o => o.IsOpen()).ToList();
        _gateway.Verify(g => g.SendAsync(1), Times.Once);
    }
}
"#,
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();
    let call = |name: &str| {
        let (refs, _) = query::refs(conn, name, &["call"], 10).unwrap();
        refs.into_iter().find(|r| r.from.as_ref().is_some_and(|f| f.name == "Run")).unwrap()
    };
    let open = call("IsOpen");
    assert_eq!((open.confidence, open.targets[0].qualified.as_str()), ("exact", "Order.IsOpen"));
    let send = call("SendAsync");
    assert_eq!((send.confidence, send.targets[0].qualified.as_str()), ("exact", "IGateway.SendAsync"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn framework_collection_calls_on_unknown_receivers_are_external_likely() {
    let root = scratch("csharp-bcl");
    write(&root, "Sources/Classifier.cs", "public class Classifier { public bool Contains(string s) => true; }\n");
    write(
        &root,
        "Sources/Job.cs",
        "public class Job\n{\n    public void Run(Registry registry)\n    {\n        var ok = _items.Contains(1);\n        var known = registry.Ids.Contains(2);\n    }\n}\n",
    );
    write(&root, "Sources/list.ts", "export class Queue { push(x: number) {} }\nexport function run(items) { items.push(1); }\n");
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();
    let confidences = |name: &str, from: &str| -> Vec<&'static str> {
        let (refs, _) = query::refs(conn, name, &["call"], 10).unwrap();
        refs.into_iter().filter(|r| r.from.as_ref().is_some_and(|f| f.name == from)).map(|r| r.confidence).collect()
    };
    let confidence = |name: &str, from: &str| confidences(name, from)[0];
    // `_items` is of an unknown type: the repo's only `Contains` is probably not the target. `registry.Ids` is a
    // member of a type the repo doesn't define, so a library's, and so is its `Contains`.
    assert_eq!(confidences("Contains", "Run"), ["external_likely", "unresolved"]);
    assert_eq!(confidence("push", "run"), "external_likely");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn refs_to_a_base_type_include_its_subclasses() {
    let root = scratch("heritage");
    write(&root, "Sources/base.ts", "export abstract class DnsRecordBaseDirective<T> { load(): void {} }\n");
    write(&root, "Sources/mx.ts", "export class MxRecords extends DnsRecordBaseDirective<MxRecord> {}\n");
    write(&root, "Sources/Base.cs", "public abstract class RecordBase { }\npublic class TxtRecord : RecordBase { }\n");
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();
    let from = |name: &str| -> Vec<String> {
        let (refs, _) = query::refs(conn, name, &["type_ref"], 10).unwrap();
        refs.into_iter().filter_map(|r| r.from.map(|f| f.name)).collect()
    };
    assert_eq!(from("DnsRecordBaseDirective"), ["MxRecords"]);
    assert_eq!(from("RecordBase"), ["TxtRecord"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn symbols_at_a_line_innermost_first() {
    let root = scratch("at");
    write(
        &root,
        "Sources/filter.service.ts",
        "export class FilterService {\n  private sub = 1;\n\n  initAggregationStream(): void {\n    this.sub = load(() => {\n      return 2;\n    });\n  }\n}\n\nconst x = 1;\n",
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();
    let chain = |line: u32| -> Vec<(String, u32, u32)> {
        query::symbols_at(conn, "Sources/filter.service.ts", line)
            .unwrap()
            .into_iter()
            .map(|s| (s.qualified, s.start_line, s.end_line))
            .collect()
    };
    // Inside a callback: the method is the innermost symbol, then its class.
    assert_eq!(
        chain(6),
        [("FilterService.initAggregationStream".to_string(), 4, 8), ("FilterService".to_string(), 1, 9)]
    );
    assert_eq!(chain(2), [("FilterService.sub".to_string(), 2, 2), ("FilterService".to_string(), 1, 9)]);
    // Between declarations, and outside every symbol.
    assert_eq!(chain(3), [("FilterService".to_string(), 1, 9)]);
    assert!(chain(10).is_empty());
    // `./` and absolute paths name the same file.
    assert_eq!(query::symbols_at(conn, "./Sources/filter.service.ts", 6).unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn angular_components_routes_and_templates() {
    let root = scratch("angular");
    std::fs::create_dir_all(root.join("Sources/dns/mx")).unwrap();
    std::fs::create_dir_all(root.join("Sources/account")).unwrap();
    write(&root, "Sources/app.routes.ts", "export const routes: Routes = [\n  { path: 'domains', loadChildren: () => import('./dns/dns.routes').then(m => m.DnsRoutesModule) },\n];\n");
    write(&root, "Sources/dns/dns.routes.ts", "export class DnsRoutesModule {}\nexport const DNS: Routes = [\n  { path: ':id/dns', component: DnsShellComponent, children: [{ path: 'mx', component: MxRecordsComponent }] },\n];\n");
    write(&root, "Sources/dns/shell.component.ts", "@Component({ selector: 'nb-dns-shell', templateUrl: './shell.component.html' })\nexport class DnsShellComponent {}\n");
    write(&root, "Sources/dns/shell.component.html", "<div>\n  <nb-mx-records></nb-mx-records>\n  <kendo-grid></kendo-grid>\n</div>\n");
    write(&root, "Sources/dns/mx/mx-records.component.ts", "@Component({ selector: 'nb-mx-records', templateUrl: './mx-records.component.html' })\nexport class MxRecordsComponent {}\n");
    write(&root, "Sources/dns/mx/mx-records.component.html", "<nb-badge></nb-badge>\n");
    write(&root, "Sources/badge.component.ts", "@Component({ selector: 'nb-badge, [nbBadge]', template: '<b></b>' })\nexport class BadgeComponent {}\n");
    // A test stub with the same selector must not shadow the real component.
    write(&root, "Sources/aaa-stubs.spec.ts", "@Component({ selector: 'nb-badge', template: '' })\nexport class BadgeStubComponent {}\n");
    write(&root, "Sources/account/root.service.ts", "export class RootService {\n  open(): void { this.dialogs.open({ content: MxRecordsComponent }); }\n}\n");
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();
    let id = |name: &str| -> i64 { conn.query_row("SELECT id FROM symbols WHERE name = ?1 AND kind = 'class'", [name], |r| r.get(0)).unwrap() };

    let mx = query::angular(conn, id("MxRecordsComponent")).unwrap().expect("a component");
    assert_eq!(mx.selector.as_deref(), Some("nb-mx-records"));
    assert_eq!(mx.template.as_deref(), Some("Sources/dns/mx/mx-records.component.html"));
    // The lazy module's routes sit under the path that loads them.
    assert_eq!(mx.routes, ["domains/:id/dns/mx"]);
    let names = |v: &[query::NgUse]| v.iter().map(|u| (u.component.clone(), u.tag.clone())).collect::<Vec<_>>();
    assert_eq!(names(&mx.rendered_by), [(Some("DnsShellComponent".to_string()), "nb-mx-records".to_string())]);
    assert_eq!(names(&mx.renders), [(Some("BadgeComponent".to_string()), "nb-badge".to_string())]);

    let shell = query::angular(conn, id("DnsShellComponent")).unwrap().unwrap();
    assert_eq!(shell.routes, ["domains/:id/dns"]);
    // A tag no indexed component declares (a library's) is listed without one.
    assert_eq!(
        names(&shell.renders),
        [(Some("MxRecordsComponent".to_string()), "nb-mx-records".to_string()), (None, "kendo-grid".to_string())]
    );
    assert!(query::angular(conn, id("RootService")).unwrap().is_none());

    // The dialog that opens it is a type reference from the service method.
    let (refs, _) = query::refs(conn, "MxRecordsComponent", &["type_ref"], 20).unwrap();
    let from: Vec<String> = refs.into_iter().filter_map(|r| r.from.map(|f| f.name)).collect();
    assert!(from.contains(&"open".to_string()) && from.contains(&"DNS".to_string()), "{from:?}");
    let _ = std::fs::remove_dir_all(&root);
}
