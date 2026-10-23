//! Dependency-injection bindings end to end: registrations are indexed, calls through a service type bound to one
//! implementation carry it as `via_di`, and `context` lists registrations and flags (`trace` following it: see
//! knowcode's `repo_walks`).

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query::{self, ContextLimits};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/App")).unwrap();
    std::fs::create_dir_all(dir.join("tests/App.Tests")).unwrap();
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    // One project compiling every source: a binding across files is the compiler's, not an assumption.
    std::fs::write(dir.join("App.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n").unwrap();
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    std::fs::write(root.join(path), text).unwrap();
}

const SERVICES: &str = r#"
namespace App;

public interface IOrderService { void Place(); }
public class OrderService : IOrderService { public void Place() { } }

public interface IMailer { void Send(); }
public class SmtpMailer : IMailer { public void Send() { } }
public class FakeMailer : IMailer { public void Send() { } }

public interface IAudit { void Log(); }
public class Audit : IAudit { public void Log() { } }

public interface IRepo { void Save(); }
public class SqlRepo : IRepo { public void Save() { } }
public class MemRepo : IRepo { public void Save() { } }

public interface IUnwired { void Go(); }
"#;

const STARTUP: &str = r#"
namespace App;

public class Startup
{
    public void Build(IContainer container, IServiceCollection services)
    {
        container.RegisterType<OrderService>().As<IOrderService>().SingleInstance();
        services.AddScoped<IMailer, SmtpMailer>();
        container.RegisterType<Audit>().AsImplementedInterfaces();
        container.RegisterType<SqlRepo>().As<IRepo>();
        container.RegisterType<MemRepo>().As<IRepo>();
    }
}
"#;

const CONTROLLER: &str = r#"
namespace App;

public class OrdersController
{
    private readonly IOrderService _orders;
    private readonly IMailer _mailer;
    private readonly IAudit _audit;
    private readonly IRepo _repo;
    private readonly IUnwired _unwired;

    public OrdersController(IOrderService orders, IMailer mailer, IAudit audit, IRepo repo, IUnwired unwired)
    {
        _orders = orders; _mailer = mailer; _audit = audit; _repo = repo; _unwired = unwired;
    }

    public void Post()
    {
        _orders.Place();
        _mailer.Send();
        _audit.Log();
        _repo.Save();
        _unwired.Go();
    }
}
"#;

const TEST_HOOKS: &str = r#"
namespace App.Tests;

public class Hooks
{
    public void Wire(IContainer container) { container.RegisterType<FakeMailer>().As<IMailer>(); }
}
"#;

#[test]
fn calls_through_bound_services_reach_the_implementation() {
    let root = scratch("di");
    write(&root, "src/App/Services.cs", SERVICES);
    write(&root, "src/App/Startup.cs", STARTUP);
    write(&root, "src/App/OrdersController.cs", CONTROLLER);
    write(&root, "tests/App.Tests/Hooks.cs", TEST_HOOKS);
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let conn = store.conn();

    let call = |name: &str| {
        let (refs, _) = query::refs(conn, name, &["call"], 10).unwrap();
        refs.into_iter().find(|r| r.from.as_ref().is_some_and(|f| f.name == "Post")).unwrap()
    };
    let bound = |name: &str| call(name).via_di.map(|b| (b.target.qualified, b.lifetime));
    // Resolution itself is unchanged: the call lands on the interface's member.
    let place = call("Place");
    assert_eq!((place.confidence, place.targets[0].qualified.as_str()), ("exact", "IOrderService.Place"));
    assert_eq!(bound("Place"), Some(("OrderService.Place".to_string(), "singleton".to_string())));
    // A test registration of a fake doesn't count against the production one.
    assert_eq!(bound("Send"), Some(("SmtpMailer.Send".to_string(), "scoped".to_string())));
    // `AsImplementedInterfaces` binds every interface the implementation declares.
    assert_eq!(bound("Log"), Some(("Audit.Log".to_string(), "default".to_string())));
    // Two implementations registered: no binding is picked.
    assert_eq!(bound("Save"), None);
    assert_eq!(bound("Go"), None);

    // The implementation's callers include calls through the bound interface.
    let limits = ContextLimits::from_budget(2000);
    let ctx = query::context(conn, &root, "OrderService.Place", &limits).unwrap().unwrap();
    assert!(ctx.callers.iter().any(|h| h.from.as_ref().is_some_and(|f| f.name == "Post")));

    // `context` on a service: registrations, the bound implementation, and flags.
    let di = |name: &str| query::context(conn, &root, name, &limits).unwrap().unwrap().di;
    let mailer = di("IMailer").expect("di section");
    assert_eq!(mailer.bound, ["SmtpMailer"]);
    assert_eq!(mailer.injection_sites, 1);
    assert_eq!(mailer.registrations.iter().map(|r| (r.implementation.as_deref(), r.test)).collect::<Vec<_>>(), [
        (Some("SmtpMailer"), false),
        (Some("FakeMailer"), true)
    ]);
    assert!(mailer.flags.is_empty());
    let repo = di("IRepo").unwrap();
    assert_eq!(repo.flags, ["conflicting: IRepo → SqlRepo, MemRepo"]);
    let unwired = di("IUnwired").unwrap();
    assert_eq!((unwired.injection_sites, unwired.flags.as_slice()), (1, ["unregistered".to_string()].as_slice()));
    let audit = di("Audit").unwrap();
    assert_eq!(audit.registered_as.iter().map(|r| r.service.as_deref()).collect::<Vec<_>>(), [Some("IAudit")]);
    assert!(di("Startup").is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn factory_registrations_make_what_their_factory_returns() {
    let root = scratch("di-factories");
    write(
        &root,
        "src/App/Services.cs",
        "namespace App;\npublic interface IPay { void Charge(); }\npublic class CardPay : IPay { public void Charge() { } }\npublic class MailSettings { }\npublic class Settings { public MailSettings Mail { get; set; } }\n",
    );
    write(&root, "src/App/PayFactory.cs", "namespace App;\npublic static class PayFactory { public static CardPay Create(int n) => new CardPay(); }\n");
    write(
        &root,
        "src/App/Startup.cs",
        "namespace App;\npublic class Startup\n{\n    private Settings Current { get; set; }\n    public void Build(IContainer c, IServiceCollection services)\n    {\n        services.AddScoped<IPay>(sp => PayFactory.Create(1));\n        c.Register(_ => Current.Mail);\n    }\n}\n",
    );
    write(&root, "src/App/Checkout.cs", "namespace App;\npublic class Checkout\n{\n    private readonly IPay _pay;\n    public void Run() { _pay.Charge(); }\n}\n");
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let limits = ContextLimits::from_budget(2000);
    let bound = |store: &Store| query::context(store.conn(), &root, "IPay", &limits).unwrap().unwrap().di.unwrap().bound;
    // The factory method's declared return type, and the call through the service bound to it.
    assert_eq!(bound(&store), ["CardPay"]);
    let (refs, _) = query::refs(store.conn(), "Charge", &["call"], 10).unwrap();
    assert_eq!(refs[0].via_di.as_ref().map(|b| b.target.qualified.as_str()), Some("CardPay.Charge"));
    // A factory naming no service is registered as what it returns: here a property's type.
    let mail = query::context(store.conn(), &root, "MailSettings", &limits).unwrap().unwrap().di.unwrap();
    let as_of: Vec<(Option<&str>, &str)> = mail.registered_as.iter().map(|r| (r.service.as_deref(), r.kind.as_str())).collect();
    assert_eq!(as_of, [(Some("MailSettings"), "factory")]);
    // Another file changing the factory's return type is seen on the next sync.
    write(
        &root,
        "src/App/PayFactory.cs",
        "namespace App;\npublic class OtherPay : IPay { public void Charge() { } }\npublic static class PayFactory { public static OtherPay Create(int n) => new OtherPay(); }\n",
    );
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    assert_eq!(bound(&store), ["OtherPay"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_binding_shows_the_registration_of_the_application_making_the_call() {
    let root = scratch("di-hosts");
    for dir in ["src/Shop.Jobs", "src/Shop.Api", "src/Shop.Api.Service"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    write(&root, "src/App/Repo.cs", "namespace App;\npublic interface IRepo { void Save(); }\npublic class SqlRepo : IRepo { public void Save() { } }\n");
    // Two applications in one repo register the same binding; the jobs host sorts first.
    write(&root, "src/Shop.Jobs/Startup.cs", "public class Startup { public void Build(IContainer c) { c.RegisterType<SqlRepo>().As<IRepo>(); } }\n");
    write(&root, "src/Shop.Api/Startup.cs", "public class Startup { public void Build(IContainer c) { c.RegisterType<SqlRepo>().As<IRepo>(); } }\n");
    write(&root, "src/Shop.Api.Service/Orders.cs", "public class Orders\n{\n    private readonly IRepo _repo;\n    public void Run() { _repo.Save(); }\n}\n");
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let (hits, _) = query::refs(store.conn(), "Save", &["call"], 10).unwrap();
    let bound = hits[0].via_di.as_ref().expect("bound");
    assert_eq!(bound.registered, "src/Shop.Api/Startup.cs:1");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn only_plain_registrations_conflict() {
    let root = scratch("di-modes");
    write(
        &root,
        "src/App/Services.cs",
        "namespace App;
public interface IClock { } public class SystemClock : IClock { } public class FixedClock : IClock { }
public interface IHandler { } public class OrderHandler : IHandler { } public class RefundHandler : IHandler { }
public interface IMailer { } public class SmtpMailer : IMailer { } public class QueueMailer : IMailer { }
public interface IStore { } public class SqlStore : IStore { } public class FileStore : IStore { }
",
    );
    write(
        &root,
        "src/App/Startup.cs",
        "namespace App;
public class Startup
{
    public void Build(IServiceCollection services)
    {
        services.AddSingleton<IClock, SystemClock>();
        services.TryAddSingleton<IClock, FixedClock>();
        services.TryAddEnumerable(ServiceDescriptor.Scoped<IHandler, OrderHandler>());
        services.TryAddEnumerable(ServiceDescriptor.Scoped<IHandler, RefundHandler>());
        services.AddScoped<IMailer, SmtpMailer>();
        services.Replace(ServiceDescriptor.Scoped<IMailer, QueueMailer>());
        services.AddScoped<IStore, SqlStore>();
        services.AddScoped<IStore, FileStore>();
    }
}
",
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let limits = ContextLimits::from_budget(2000);
    let di = |name: &str| query::context(store.conn(), &root, name, &limits).unwrap().unwrap().di.unwrap();
    let modes = |name: &str| di(name).registrations.iter().map(|r| r.mode.clone()).collect::<Vec<_>>();
    assert_eq!(modes("IClock"), [None, Some("try".to_string())]);
    assert_eq!(modes("IHandler"), [Some("enumerable".to_string()), Some("enumerable".to_string())]);
    assert_eq!(modes("IMailer"), [None, Some("replace".to_string())]);
    // A registration made only when there is none, one of several meant together, or one replacing another is no
    // second implementation competing for the service; two plain ones are.
    for name in ["IClock", "IHandler", "IMailer"] {
        assert!(di(name).flags.is_empty(), "{name}: {:?}", di(name).flags);
    }
    assert_eq!(di("IStore").flags, ["conflicting: IStore → SqlStore, FileStore"]);
    let _ = std::fs::remove_dir_all(&root);
}
