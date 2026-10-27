//! C# name resolution as the compiler scopes names: namespaces and `using`s tell same-named types apart (and a
//! library's type from the repo's), generic bases and factories carry their type arguments, and a few framework
//! idioms (`base.`, extension methods called statically, tuples, FluentValidation child rules) type their receivers.

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

/// The references named `name` of `kind` made from the symbol named `from`, in line order.
fn refs_from(conn: &rusqlite::Connection, name: &str, kind: &str, from: &str) -> Vec<RefHit> {
    let (mut refs, _) = query::refs(conn, name, &[kind], 50).unwrap();
    refs.retain(|r| r.from.as_ref().is_some_and(|f| f.name == from));
    refs.sort_by_key(|r| r.line);
    refs
}

/// Confidence and target files of a reference.
fn answer(r: &RefHit) -> (&'static str, Vec<String>) {
    (r.confidence, r.targets.iter().map(|t| format!("{}:{}", t.file, t.qualified)).collect())
}

#[test]
fn namespaces_and_usings_tell_same_named_types_apart() {
    let root = scratch("cs-namespaces");
    write(&root, "src/Models/Domain.cs", "namespace Shop.Models;\npublic class Domain { public string Name { get; set; } }\n");
    write(&root, "src/Entities/Domain.cs", "namespace Shop.Entities;\npublic class Domain { public string Name { get; set; } }\n");
    write(
        &root,
        "src/Services/Catalog.cs",
        r#"using System;
using Shop.Models;
using Inventory.Contracts;
using Ent = Shop.Entities;

namespace Shop.Services;

public class Catalog
{
    public void Show(
        Domain d,
        Ent.Domain e,
        Inventory.Contracts.Domain p)
    {
        var a = d.Name;
        var b = e.Name;
        var c = p.Name;
        var now = DateTime.UtcNow;
    }
}
"#,
    );
    // A file seeing no repo `Domain`: the type it names is a package's.
    write(
        &root,
        "src/Jobs/Sync.cs",
        "using Inventory.Contracts;\nnamespace Shop.Jobs;\npublic class Sync\n{\n    public void Run(Domain d) { var n = d.Name; }\n}\n",
    );
    write(&root, "src/Clock.cs", "namespace Shop;\npublic class Clock { public DateTime UtcNow => default; }\n");
    let store = index(&root);
    let conn = store.conn();

    let names = refs_from(conn, "Name", "member", "Show");
    // `Domain` through `using Shop.Models`; `Ent.Domain` through the alias; a namespace the repo doesn't declare.
    assert_eq!(answer(&names[0]), ("exact", vec!["src/Models/Domain.cs:Domain.Name".to_string()]));
    assert_eq!(answer(&names[1]), ("exact", vec!["src/Entities/Domain.cs:Domain.Name".to_string()]));
    assert_eq!(answer(&names[2]), ("unresolved", vec![]));
    assert_eq!(answer(&refs_from(conn, "Name", "member", "Run")[0]), ("unresolved", vec![]));
    // The type references themselves.
    let types = refs_from(conn, "Domain", "type_ref", "Show");
    assert_eq!(types.iter().map(|t| answer(t).0).collect::<Vec<_>>(), ["exact", "exact", "unresolved"]);
    assert_eq!(types[1].receiver.as_deref(), Some("Shop.Entities"));
    // A well-known .NET type is never the repo's lookalike member.
    assert_eq!(refs_from(conn, "UtcNow", "member", "Show")[0].confidence, "unresolved");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unqualified_calls_stay_in_the_callers_namespace() {
    let root = scratch("cs-same-class");
    for (ns, dir) in [("Billing.Import", "Import"), ("Billing.Export", "Export")] {
        write(
            &root,
            &format!("src/{dir}/TransactionParser.cs"),
            &format!("namespace {ns};\npublic class TransactionParser\n{{\n    public void Parse() {{ Clean(); }}\n    void Clean() {{ }}\n}}\n"),
        );
    }
    let store = index(&root);
    let conn = store.conn();
    let clean = refs_from(conn, "Clean", "call", "Parse");
    assert_eq!(clean.len(), 2);
    for c in &clean {
        assert_eq!(c.confidence, "exact");
        assert_eq!(c.targets.len(), 1, "{:?}", c.targets);
        assert_eq!(c.targets[0].file, c.file);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn generic_bases_factories_and_mappers_type_their_values() {
    let root = scratch("cs-generic");
    write(
        &root,
        "src/Data.cs",
        r#"namespace Shop.Data;
public class ShopContext { public DbSet<Order> Orders { get; set; } }
public class ArchiveContext { public DbSet<Order> Orders { get; set; } }
public class Order { public int Id { get; set; } }
public abstract class BaseRepository<TContext> { protected TContext Context { get; } }
public interface IUnitOfWork { ShopContext Shop { get; } void Complete(); }
public interface IReport { void Run(); }
public class DeleteOrder { public int Id { get; set; } }
public class PagedList<T> { public List<T> Items { get; set; } }
"#,
    );
    write(
        &root,
        "src/OrderRepository.cs",
        r#"namespace Shop.Data;
public class OrderRepository : BaseRepository<ShopContext>
{
    private readonly Func<Owned<IUnitOfWork>> _unitOfWorkFactory;
    private readonly IMapper _mapper;
    private readonly IContainer _scope;

    public void Load(PagedList<Order> page)
    {
        var all = Context.Orders;
        using var uow = _unitOfWorkFactory();
        uow.Value.Complete();
        _unitOfWorkFactory().Value.Shop.Orders.ToList();
        _scope.GetInstance<IReport>().Run();
        var request = _mapper.Map<DeleteOrder>(page);
        var id = request.Id;
        var first = page.Items.First().Id;
    }
}
"#,
    );
    let store = index(&root);
    let conn = store.conn();

    // `Context` is declared `TContext` on the generic base; this repository's base binds it to `ShopContext`.
    let orders = refs_from(conn, "Orders", "member", "Load");
    assert_eq!(answer(&orders[0]), ("exact", vec!["src/Data.cs:ShopContext.Orders".to_string()]));
    assert_eq!(answer(&orders[1]), ("exact", vec!["src/Data.cs:ShopContext.Orders".to_string()]));
    // `Func<Owned<IUnitOfWork>>` called, then `.Value`: an `IUnitOfWork`.
    assert_eq!(answer(&refs_from(conn, "Complete", "call", "Load")[0]), ("exact", vec!["src/Data.cs:IUnitOfWork.Complete".to_string()]));
    // A service locator's type argument, chained.
    assert_eq!(answer(&refs_from(conn, "Run", "call", "Load")[0]), ("exact", vec!["src/Data.cs:IReport.Run".to_string()]));
    // `Map<T>` gives a `T`, and creates one: the code sending it is a creator of the request.
    let ids = refs_from(conn, "Id", "member", "Load");
    assert_eq!(answer(&ids[0]).1, ["src/Data.cs:DeleteOrder.Id"]);
    assert_eq!(refs_from(conn, "DeleteOrder", "instantiate", "Load").len(), 1);
    // A repo generic's member has its type parameter replaced: `PagedList<Order>.Items` is a list of orders.
    assert_eq!(answer(&ids[1]).1, ["src/Data.cs:Order.Id"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn base_extensions_tuples_and_child_rules() {
    let root = scratch("cs-idioms");
    write(
        &root,
        "src/Types.cs",
        r#"namespace App;
public class Processor { public virtual string Name => "p"; }
public class Downloader : Processor { public override string Name => base.Name + "d"; }
public class Store { }
public static class StoreExtensions { public static void Touch(this Store s, int n) { } }
public record Credit(string Number, decimal Amount);
public class Line { public decimal Amount { get; set; } public string Description { get; set; } }
public class Invoice { public List<Line> Lines { get; set; } public string Number { get; set; } }
public class InvoiceValidator : AbstractValidator<Invoice>
{
    public InvoiceValidator()
    {
        RuleFor(i => i.Number).NotEmpty();
        RuleForEach(i => i.Lines).ChildRules(line => { line.RuleFor(l => l.Amount).NotEqual(0); });
    }
}
public class Jobs
{
    public void Run(Store store, IEnumerable<(string Number, decimal Amount)> credits)
    {
        StoreExtensions.Touch(store, 1);
        foreach (var credit in credits) { var a = credit.Amount; }
    }
}
"#,
    );
    let store = index(&root);
    let conn = store.conn();
    // `base.Name` is the base type's.
    assert_eq!(answer(&refs_from(conn, "Name", "member", "Name")[0]), ("exact", vec!["src/Types.cs:Processor.Name".to_string()]));
    // An extension method called through its static class.
    assert_eq!(answer(&refs_from(conn, "Touch", "call", "Run")[0]), ("exact", vec!["src/Types.cs:Store.Touch".to_string()]));
    // A tuple's element, never the record's property of the same name.
    assert_eq!(answer(&refs_from(conn, "Amount", "member", "Run")[0]), ("unresolved", vec![]));
    // `ChildRules` validates the collection's items.
    let amounts = refs_from(conn, "Amount", "member", "init");
    assert_eq!(answer(&amounts[0]), ("exact", vec!["src/Types.cs:Line.Amount".to_string()]));
    let numbers = refs_from(conn, "Number", "member", "init");
    assert_eq!(answer(&numbers[0]), ("exact", vec!["src/Types.cs:Invoice.Number".to_string()]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_binding_prefers_the_implementation_the_registering_project_references() {
    let root = scratch("cs-di-projects");
    let proj = |refs: &[&str]| {
        let items: String = refs.iter().map(|r| format!("<ProjectReference Include=\"..\\{r}\\{r}.csproj\" />")).collect();
        format!("<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup>{items}</ItemGroup></Project>\n")
    };
    write(&root, "src/Auth.Service/Auth.Service.csproj", &proj(&[]));
    write(&root, "src/Auth.Service/IUserRepository.cs", "namespace Auth;\npublic interface IUserRepository { void Find(); }\n");
    for lib in ["Users.Api", "Users.Web"] {
        write(&root, &format!("src/{lib}/{lib}.csproj"), &proj(&["Auth.Service"]));
        write(
            &root,
            &format!("src/{lib}/UserRepository.cs"),
            &format!("namespace {lib};\npublic class UserRepository : Auth.IUserRepository {{ public void Find() {{ }} }}\n"),
        );
    }
    write(&root, "src/Manager/Manager.csproj", &proj(&["Auth.Service", "Users.Web"]));
    write(
        &root,
        "src/Manager/Startup.cs",
        "using Auth;\nnamespace Manager;\npublic class Startup\n{\n    public void Build(IContainer container) { container.RegisterType<Users.Web.UserRepository>().As<IUserRepository>(); }\n}\n",
    );
    write(
        &root,
        "src/Manager/UsersController.cs",
        "using Auth;\nnamespace Manager;\npublic class UsersController\n{\n    private readonly IUserRepository _users;\n    public void Get() { _users.Find(); }\n}\n",
    );
    let store = index(&root);
    let conn = store.conn();
    let find = &refs_from(conn, "Find", "call", "Get")[0];
    let bound = find.via_di.as_ref().expect("bound");
    assert_eq!(bound.target.file, "src/Users.Web/UserRepository.cs");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_null_forgiving_operator_keeps_the_receivers_type() {
    let root = scratch("cs-bang");
    write(&root, "src/Groups.cs", "namespace Shop;\npublic class PaymentGroup { public void Authorize() { } }\npublic class Payment { public void Authorize() { } }\n");
    write(
        &root,
        "src/Driver.cs",
        "using PaymentGroup = Shop.PaymentGroup;\nnamespace Shop.Tests;\npublic class Driver\n{\n    public PaymentGroup? Group { get; set; }\n    private Payment? _last;\n    public void Run()\n    {\n        Group!.Authorize();\n        _last!.Authorize();\n    }\n}\n",
    );
    let store = index(&root);
    let refs = refs_from(store.conn(), "Authorize", "call", "Run");
    let got: Vec<_> = refs.iter().map(|r| (r.receiver.clone().unwrap_or_default(), answer(r))).collect();
    assert_eq!(
        got,
        [
            ("Group".to_string(), ("exact", vec!["src/Groups.cs:PaymentGroup.Authorize".to_string()])),
            ("_last".to_string(), ("exact", vec!["src/Groups.cs:Payment.Authorize".to_string()])),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_extension_on_enum_reaches_enum_receivers_and_the_one_in_scope_wins() {
    let root = scratch("cs-enum-ext");
    write(&root, "src/Common/EnumExt.cs", "using System;\nnamespace Shop.Common;\npublic static class EnumExt { public static string Describe(this Enum v) => \"\"; }\n");
    write(&root, "src/Legacy/EnumExt.cs", "using System;\nnamespace Shop.Legacy;\npublic static class LegacyExt { public static string Describe(this Enum v) => \"\"; }\n");
    write(&root, "src/Models/Order.cs", "namespace Shop.Models;\npublic enum Sha { One }\npublic class Order { public Sha Sha { get; set; } }\n");
    write(
        &root,
        "src/Service.cs",
        "using Shop.Common;\nusing Shop.Models;\nnamespace Shop.Services;\npublic class Service\n{\n    public string Run(Order o) => o.Sha.Describe();\n}\n",
    );
    let store = index(&root);
    let refs = refs_from(store.conn(), "Describe", "call", "Run");
    assert_eq!(answer(&refs[0]), ("exact", vec!["src/Common/EnumExt.cs:Enum.Describe".to_string()]));
    let _ = std::fs::remove_dir_all(&root);
}
