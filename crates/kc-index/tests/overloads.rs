//! C# resolution without a compiler: overloads are told apart by how many arguments a call passes, and a lambda's
//! parameter takes its type from the delegate the called method declares.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query::{self, RefHit};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/App")).unwrap();
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    // One project compiling every source: a binding across files is the compiler's, not an assumption.
    std::fs::write(dir.join("App.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n").unwrap();
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    std::fs::write(root.join(path), text).unwrap();
}

const REPO: &str = r#"
namespace App;

public interface IStore
{
    void Upsert(Product p);
    void Upsert(Product p, bool force);
    void Note(string format, params object[] args);
    void Note(int level);
    void Save(Product p, int retries = 3);
    Task<Order> FindWhere(Expression<Func<Order, bool>> where);
    void Pair(int n, Action<Order, Product> each);
}

public class SqlStore : IStore
{
    public void Upsert(Product p) { }
    public void Upsert(Product p, bool force) { }
    public void Note(string format, params object[] args) { }
    public void Note(int level) { }
    public void Save(Product p, int retries = 3) { }
    public Task<Order> FindWhere(Expression<Func<Order, bool>> where) => null;
    public void Pair(int n, Action<Order, Product> each) { }
}

public static class StoreExtensions
{
    public static void Touch(this IStore s, Product p) { }
    public static void Touch(this IStore s, Product p, Product q) { }
}

public class Order { public int AccountId { get; set; } }
public class Product { public int AccountId { get; set; } public string Sku { get; set; } }
public class Account { public int AccountId { get; set; } }

public class Startup
{
    public void Build(IContainer container) { container.RegisterType<SqlStore>().As<IStore>(); }
}
"#;

const SERVICE: &str = r#"
namespace App;

public class Service
{
    private readonly IStore _store;

    public void Load(Product x)
    {
        _store.Upsert(x);
    }

    public void LoadForced(Product x)
    {
        _store.Upsert(x, true);
    }

    public void Log()
    {
        _store.Note("a {0} {1}", 1, 2);
        _store.Note(format: "a");
    }

    public void Keep(Product x)
    {
        _store.Save(x);
        _store.Save(retries: 1, p: x);
    }

    public void Ext(Product x)
    {
        _store.Touch(x);
    }

    public async Task Find(int id)
    {
        var order = await _store.FindWhere(o => o.AccountId == id);
        _store.Pair(1, (a, b) => b.Sku.ToString());
        var accounts = new List<Account>();
        accounts.ShouldAllBe(c => c.AccountId > 0);
        var top = accounts.Where(c => true).OrderBy(c => 1).First();
        var id = top.AccountId;
    }

    public void Wrong(Product x)
    {
        _store.Upsert(x, true, false);
    }
}
"#;

fn refs_from(conn: &rusqlite::Connection, name: &str, kind: &str, from: &str) -> Vec<RefHit> {
    let (refs, _) = query::refs(conn, name, &[kind], 50).unwrap();
    refs.into_iter().filter(|r| r.from.as_ref().is_some_and(|f| f.name == from)).collect()
}

fn lines(r: &RefHit) -> Vec<u32> {
    r.targets.iter().map(|t| t.line).collect()
}

#[test]
fn overloads_by_argument_count_and_lambdas_by_delegate() {
    let root = scratch("overloads");
    write(&root, "src/App/Store.cs", REPO);
    write(&root, "src/App/Service.cs", SERVICE);
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();

    // One argument: the one-parameter overload, and with a single target the container's binding applies.
    let load = &refs_from(conn, "Upsert", "call", "Load")[0];
    assert_eq!((load.confidence, lines(load)), ("exact", vec![6]));
    let bound = load.via_di.as_ref().expect("bound implementation");
    assert_eq!((bound.target.qualified.as_str(), bound.target.line), ("SqlStore.Upsert", 17));
    let forced = &refs_from(conn, "Upsert", "call", "LoadForced")[0];
    assert_eq!(lines(forced), vec![7]);
    assert_eq!(forced.via_di.as_ref().map(|b| b.target.line), Some(18));

    // `params` takes any number past the fixed ones; a named argument counts once, and only an overload with a
    // parameter of that name takes it.
    let notes = refs_from(conn, "Note", "call", "Log");
    assert_eq!(notes.iter().map(lines).collect::<Vec<_>>(), [vec![8], vec![8]]);
    // Optional parameters may be left out, and named arguments in any order.
    for r in refs_from(conn, "Save", "call", "Keep") {
        assert_eq!(lines(&r), vec![10], "line {}", r.line);
    }
    // An extension method's receiver isn't an argument.
    let ext = &refs_from(conn, "Touch", "call", "Ext")[0];
    assert_eq!((ext.confidence, lines(ext)), ("exact", vec![28]));
    // No overload takes three arguments: the call is another declaration's, outside the repo, not any of these.
    let wrong = &refs_from(conn, "Upsert", "call", "Wrong")[0];
    assert_eq!((wrong.confidence, lines(wrong)), ("unresolved", vec![]));
    assert!(wrong.via_di.is_none());

    // `o` is an `Order` because `FindWhere` takes an `Expression<Func<Order, bool>>`; the second parameter of the
    // `Action<Order, Product>` is a `Product`.
    let account = refs_from(conn, "AccountId", "member", "Find");
    assert_eq!((account[0].confidence, account[0].targets[0].qualified.as_str()), ("exact", "Order.AccountId"));
    // A library method on a collection passes the lambda its elements.
    assert_eq!((account[1].confidence, account[1].targets[0].qualified.as_str()), ("exact", "Account.AccountId"));
    // LINQ keeps a sequence's elements.
    assert_eq!((account[2].confidence, account[2].targets[0].qualified.as_str()), ("exact", "Account.AccountId"));
    let sku = &refs_from(conn, "Sku", "member", "Find")[0];
    assert_eq!((sku.confidence, sku.targets[0].qualified.as_str()), ("exact", "Product.Sku"));
    let _ = std::fs::remove_dir_all(&root);
}

const BASES: &str = r#"
namespace App;

public class Processor { public virtual string Name => "p"; }

public class Downloader : Processor
{
    public override string Name => base.Name + "d";
}

public class OutboxTests
{
    private class FakeBus { public void Publish(object e) { } }
}

public class Job
{
    private readonly IServiceBus _bus;
    public void Run() { _bus.Publish(1); }
}

public class StatementJob { public Task ExecuteAsync(int n) => null; }

public class Client
{
    private readonly StatementJob _job;
    public async Task Fetch()
    {
        await Policy.Handle<Exception>().RetryAsync(3).ExecuteAsync(async () => await Send());
        await _job.ExecuteAsync(1);
    }
    private Task Send() => null;
}
"#;

#[test]
fn base_members_and_foreign_receivers_do_not_land_on_lookalikes() {
    let root = scratch("bases");
    write(&root, "src/App/Bases.cs", BASES);
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();

    // `base.Name` is the base type's, never the override reading it.
    let name = &refs_from(conn, "Name", "member", "Name")[0];
    assert!(name.targets.iter().any(|t| t.qualified == "Processor.Name"), "{:?}", name.targets);
    assert!(!(name.targets.len() == 1 && name.targets[0].qualified == "Downloader.Name"), "{:?}", name.targets);
    // A receiver typed by an interface the repo doesn't declare isn't resolved to a nested test double's member.
    let publish = &refs_from(conn, "Publish", "call", "Run")[0];
    assert!(!matches!(publish.confidence, "exact" | "unique_name"), "{} {:?}", publish.confidence, publish.targets);
    // A chain off a library's static type (Polly's `Policy`) never reaches the repo's same-named method; a field
    // the repo types still does.
    let exec = refs_from(conn, "ExecuteAsync", "call", "Fetch");
    assert_eq!(exec.iter().map(|r| r.confidence).collect::<Vec<_>>(), ["unresolved", "exact"]);
    let _ = std::fs::remove_dir_all(&root);
}

const EXTENSIONS: &str = r#"
namespace App;

public class ContainerBuilder { }
public class ServiceCollection { }
public interface IService { }
public class Decorator : IService { }

public static partial class RegistrationExtensions
{
    public static void RegisterDecorator<TDecorator, TService>(this ContainerBuilder builder) { }
    public static void RegisterDecorator<TService>(this ContainerBuilder builder, Func<TService, TService> d) { }
    public static void RegisterDecorator<TService>(this ContainerBuilder builder, Func<TService, TService> d, object key) { }
    public static void RegisterDecorator(this ContainerBuilder builder, Type decorator, Type service) { }
    public static void RegisterDecorator(this ContainerBuilder builder, Type decorator, Type service, object key) { }
    public static void RegisterDecorator(this ContainerBuilder builder, Func<object, object> d, Type service, object key) { }
}

public static class CollectionExtensions
{
    public static void RegisterDecorator(this ServiceCollection services, Type decorator) { }
}

public class Module
{
    public void Load(ContainerBuilder builder, ServiceCollection services)
    {
        builder.RegisterDecorator<Decorator, IService>();
        builder.RegisterDecorator(typeof(Decorator), typeof(IService));
        builder.RegisterDecorator<IService>(s => s, "key");
        services.RegisterDecorator(typeof(Decorator));
    }
}
"#;

#[test]
fn extension_methods_are_found_by_the_static_class_declaring_them() {
    let root = scratch("extension-hosts");
    write(&root, "src/App/Extensions.cs", EXTENSIONS);
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();

    // Written after the static class declaring them, every overload is a definition, and the other class's is not.
    let defs = query::definitions(conn, "RegistrationExtensions.RegisterDecorator").unwrap();
    assert_eq!(defs.len(), 6, "{defs:?}");
    assert!(defs.iter().all(|d| d.container.as_deref() == Some("ContainerBuilder")));
    let found = query::find_counted(conn, "RegistrationExtensions.RegisterDecorator", None, 20, &Default::default()).unwrap();
    assert_eq!(found.hits.iter().filter(|h| h.matched == "exact").count(), 6);
    assert_eq!(query::definitions(conn, "App.RegistrationExtensions.RegisterDecorator").unwrap().len(), 6);

    // Their references, as by the extended type: none of the other class's.
    let lines_of = |q: &str| -> Vec<u32> { query::refs(conn, q, &[], 50).unwrap().0.iter().map(|r| r.line).collect() };
    assert_eq!(lines_of("RegistrationExtensions.RegisterDecorator"), vec![28, 29, 30]);
    assert_eq!(lines_of("ContainerBuilder.RegisterDecorator"), vec![28, 29, 30]);
    assert_eq!(lines_of("CollectionExtensions.RegisterDecorator"), vec![31]);
    let _ = std::fs::remove_dir_all(&root);
}
