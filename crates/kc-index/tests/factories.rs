//! Calls through factories the container injects: an Autofac delegate factory (`Repo.Factory make`), a `Func<…, T>`,
//! `Lazy<T>` and `Owned<T>`, called directly, through `.Invoke` or `?.Invoke`, give the factory's result type; and the
//! call is found as a construction of the class it makes, directly or through the container's binding.

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
    // One project compiling every source: a binding across files is the compiler's, not an assumption.
    std::fs::write(dir.join("App.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n").unwrap();
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

/// Per line: confidence, targets, and the implementation bound through the container.
fn answers(conn: &rusqlite::Connection, name: &str, kind: &str) -> Vec<(u32, &'static str, Vec<String>, Option<String>)> {
    let (mut hits, _) = query::refs(conn, name, &[kind], 100).unwrap();
    hits.sort_by_key(|h| h.line);
    hits.iter()
        .map(|h: &RefHit| {
            let targets = h.targets.iter().map(|t| t.qualified.clone()).collect();
            let di = h.via_di.as_ref().map(|b| format!("{} @ {}", b.target.qualified, b.registered));
            (h.line, h.confidence, targets, di)
        })
        .collect()
}

const SHOP: &str = r#"using System;
using Autofac.Features.OwnedInstances;

namespace Shop;

public interface IRepo { void Get(); }

public class Repo : IRepo
{
    public delegate IRepo Factory(int? id);
    public Repo(int? id) { }
    public void Get() { }
}

public class Widget
{
    public delegate Widget Factory(string name);
    public Widget(string name) { }
    public void Spin() { }
}

public class Service
{
    private readonly Repo.Factory _repoFactory;
    private readonly Func<int, IRepo> _make;
    private readonly Lazy<IRepo> _lazy;
    private readonly Func<Owned<IRepo>> _owned;
    private readonly Widget.Factory _widgets;
    public Repo.Factory RepoFactory { get; }

    public Service(Repo.Factory repoFactory, Func<int, IRepo> make, Lazy<IRepo> lazy, Func<Owned<IRepo>> owned, Widget.Factory widgets)
    {
        _repoFactory = repoFactory;
        _make = make ?? throw new ArgumentNullException(nameof(make));
        _lazy = lazy;
        this._owned = owned;
        _widgets = widgets;
        RepoFactory = repoFactory;
    }

    public void Run(Repo.Factory param, Func<IRepo> thunk)
    {
        _repoFactory(1).Get();
        _repoFactory.Invoke(2).Get();
        _repoFactory?.Invoke(3).Get();
        param(4).Get();
        param.Invoke(5).Get();
        RepoFactory(6).Get();
        _make(7).Get();
        _make.Invoke(8).Get();
        _make?.Invoke(9).Get();
        thunk().Get();
        _lazy.Value.Get();
        _owned().Value.Get();
        _widgets("a").Spin();
        Repo.Factory local = _repoFactory;
        local(10).Get();
        var w = _widgets.Invoke("b");
        w.Spin();
    }
}
"#;

const MODULE: &str = r#"using Autofac;

namespace Shop;

public class ShopModule : Module
{
    protected override void Load(ContainerBuilder builder)
    {
        builder.RegisterType<Repo>().As<IRepo>();
        builder.RegisterType<Widget>();
        builder.RegisterGeneratedFactory<Widget.Factory>();
    }
}
"#;

#[test]
fn calls_through_injected_factories_reach_what_they_make() {
    let root = scratch("factories");
    write(&root, "src/Shop.cs", SHOP);
    write(&root, "src/Module.cs", MODULE);
    let store = index(&root);
    let conn = store.conn();

    // Every call on what a factory returns is the interface's member, bound by the container to the repository's.
    let gets = answers(conn, "Get", "call");
    let lines: Vec<u32> = gets.iter().map(|g| g.0).collect();
    assert_eq!(lines, [43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 57]);
    for g in &gets {
        assert_eq!((g.1, g.2.clone(), g.3.clone()), ("exact", vec!["IRepo.Get".to_string()], Some("Repo.Get @ src/Module.cs:9".to_string())), "{g:?}");
    }
    let spins = answers(conn, "Spin", "call");
    assert_eq!(spins.iter().map(|s| (s.0, s.1)).collect::<Vec<_>>(), [(55, "exact"), (59, "exact")]);

    // A factory returning the class constructs it; the delegate type's own registration is the binding.
    let widgets = answers(conn, "Widget", "instantiate");
    assert_eq!(
        widgets,
        [
            (55, "exact", vec!["Widget".to_string()], Some("Widget @ src/Module.cs:11".to_string())),
            (58, "exact", vec!["Widget".to_string()], Some("Widget @ src/Module.cs:11".to_string())),
        ]
    );
    // One returning the interface constructs what the container binds it to (`Lazy<T>.Value` is no call). Only a
    // factory the constructor was given is the container's: not a method's parameter or a local.
    let repos = answers(conn, "Repo", "instantiate");
    assert_eq!(repos.iter().map(|r| r.0).collect::<Vec<_>>(), [43, 44, 45, 48, 49, 50, 51, 54]);
    for r in &repos {
        assert_eq!((r.1, r.2.clone(), r.3.clone()), ("unique_name", vec!["Repo".to_string()], Some("Repo @ src/Module.cs:9".to_string())), "{r:?}");
    }
}

#[test]
fn same_named_delegates_are_told_apart_by_their_type() {
    let root = scratch("factory-names");
    write(
        &root,
        "src/Shop.cs",
        r#"namespace Shop;

public class Order { public void Ship() { } }
public class Invoice { public void Send() { } }

public class Orders
{
    public delegate Order Create(int id);
}

public class Invoices
{
    public delegate Invoice Create(int id);
}

public class Clerk
{
    private readonly Orders.Create _orders;
    private readonly Invoices.Create _invoices;

    public Clerk(Orders.Create orders, Invoices.Create invoices)
    {
        _orders = orders;
        _invoices = invoices;
    }

    public void Work()
    {
        _orders(1).Ship();
        _invoices(2).Send();
    }
}
"#,
    );
    // A mock's setup records the delegate's invocation; it doesn't make an order. A `Func` a test fills with its own
    // lambda isn't the container's: only the lambda's `new` constructs.
    write(
        &root,
        "tests/ClerkTests.cs",
        r#"using System;
using Moq;
namespace Shop.Tests;

public class ClerkTests
{
    private readonly Func<Order> _make = () => new Order();

    public void Works()
    {
        var orders = new Mock<Orders.Create>();
        orders.Setup(f => f(1)).Returns(new Order());
        _make().Ship();
    }
}
"#,
    );
    let store = index(&root);
    let conn = store.conn();
    let made: Vec<(String, u32)> = query::refs(conn, "Order", &["instantiate"], 100).unwrap().0.iter().map(|h| (h.file.clone(), h.line)).collect();
    assert_eq!(made, [("src/Shop.cs".to_string(), 29), ("tests/ClerkTests.cs".to_string(), 7), ("tests/ClerkTests.cs".to_string(), 12)]);
    assert_eq!(answers(conn, "Ship", "call").iter().map(|a| (a.0, a.1)).collect::<Vec<_>>(), [(13, "exact"), (29, "exact")]);
    assert_eq!(answers(conn, "Send", "call").iter().map(|a| (a.0, a.1)).collect::<Vec<_>>(), [(30, "exact")]);
    assert_eq!(answers(conn, "Invoice", "instantiate").iter().map(|a| (a.0, a.1)).collect::<Vec<_>>(), [(30, "exact")]);
}

#[test]
fn contexts_from_the_container_and_factories_nested_in_their_class() {
    let root = scratch("factory-contexts");
    write(
        &root,
        "src/Data.cs",
        r#"using System;
using Microsoft.EntityFrameworkCore;

namespace Shop.Data;

public class Fee { public decimal Amount { get; set; } }

public class ShopContext : DbContext
{
    public DbSet<Fee> Fees { get; set; }
    public void Touch() { }
}

public class Cart
{
    public delegate Cart Factory(string client);
    public Cart(string client) { }
}
"#,
    );
    write(&root, "src/Models/Cart.cs", "namespace Shop.Models;\npublic class Cart { }\n");
    write(
        &root,
        "src/Startup.cs",
        r#"using Microsoft.Extensions.DependencyInjection;
using Shop.Data;

namespace Shop;

public class Startup
{
    public void ConfigureServices(IServiceCollection services)
    {
        services.AddDbContext<ShopContext>(o => o.UseMySql());
        services.AddDbContextFactory<ShopContext>();
    }
}
"#,
    );
    write(
        &root,
        "src/Billing.cs",
        r#"using System;
using Microsoft.EntityFrameworkCore;
using Shop.Data;

namespace Shop;

public class Billing(Func<ShopContext> contexts, IDbContextFactory<ShopContext> factory, Cart.Factory carts)
{
    public void Run()
    {
        contexts().Touch();
        using var ctx = factory.CreateDbContext();
        ctx.Touch();
        carts("a");
    }
}
"#,
    );
    let store = index(&root);
    let conn = store.conn();
    // `AddDbContext<T>` registers the context, so an injected `Func<T>` makes it; a context factory makes it too.
    assert_eq!(answers(conn, "ShopContext", "instantiate").iter().map(|a| (a.0, a.1)).collect::<Vec<_>>(), [(11, "exact"), (12, "exact")]);
    assert_eq!(answers(conn, "Touch", "call").iter().map(|a| (a.0, a.1)).collect::<Vec<_>>(), [(11, "exact"), (13, "exact")]);
    // A delegate factory nested in a class makes that class, not another of its name.
    assert_eq!(
        answers(conn, "Cart", "instantiate").iter().map(|a| (a.0, a.1, a.2.clone())).collect::<Vec<_>>(),
        [(14, "exact", vec!["Cart".to_string()])]
    );
    let (hits, _) = query::refs(conn, "Cart", &["instantiate"], 10).unwrap();
    assert_eq!(hits[0].targets[0].file, "src/Data.cs");
}

#[test]
fn an_entitys_impact_goes_through_its_dbset_and_direct_uses_outrank_bindings() {
    let root = scratch("factory-impact");
    write(
        &root,
        "src/Data.cs",
        r#"using Microsoft.EntityFrameworkCore;

namespace Shop.Data;

public class Fee { public decimal Amount { get; set; } }
public class Order { public int Id { get; set; } }

public class ShopContext : DbContext
{
    public DbSet<Fee> Fees { get; set; }
    public DbSet<Order> Orders { get; set; }
}

public interface IFeeRepo { decimal Total(); }

public class FeeRepo(ShopContext ctx) : IFeeRepo
{
    public decimal Total() => ctx.Fees.Sum(f => f.Amount);
}

public class OrderRepo(ShopContext ctx)
{
    public int Count() => ctx.Orders.Count();
}
"#,
    );
    write(
        &root,
        "src/A.cs",
        "namespace Shop;\npublic class Billing(Shop.Data.IFeeRepo fees)\n{\n    public decimal Due() => fees.Total();\n}\n",
    );
    write(
        &root,
        "src/B.cs",
        "namespace Shop;\npublic class Audit\n{\n    public decimal Check() => new Shop.Data.FeeRepo(null).Total();\n}\n",
    );
    write(&root, "src/Module.cs", "using Autofac;\nnamespace Shop;\npublic class M { void Load(ContainerBuilder b) { b.RegisterType<Shop.Data.FeeRepo>().As<Shop.Data.IFeeRepo>(); } }\n");
    let store = index(&root);
    let conn = store.conn();
    let fee = query::definitions(conn, "Fee").unwrap()[0].id;
    let names = |i: &kc_store::trace::Impact| i.affected.iter().map(|a| (a.qualified.clone(), a.edge)).collect::<Vec<_>>();
    // Through the context only to the code using its DbSet of fees: never the repository of orders sharing it.
    let all = kc_store::trace::impact(conn, &[fee], 3, 100).unwrap();
    let affected = names(&all);
    assert!(affected.iter().any(|(q, _)| q == "FeeRepo.Total"), "{affected:?}");
    assert!(!affected.iter().any(|(q, _)| q.starts_with("OrderRepo")), "{affected:?}");
    // At the limit, a direct call outranks a call through the container's binding at the same hop.
    let hop2 = all.affected.iter().filter(|a| a.hop <= 2).count();
    let capped = kc_store::trace::impact(conn, &[fee], 3, hop2 + 1).unwrap();
    assert!(capped.truncated);
    let kept = names(&capped);
    assert!(kept.contains(&("Audit.Check".to_string(), "calls")) && !kept.iter().any(|(q, _)| q == "Billing.Due"), "{kept:?}");
}
