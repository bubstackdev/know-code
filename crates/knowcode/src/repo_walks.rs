//! Walks in one repo on the symbol graph ([`crate::walk::Graph`], as `trace` and `impact` walk it): calls through a
//! service the container binds, through protocols and base members, an entity's impact through its DbSet, Terraform
//! outputs, ColdFusion templates, and creations reaching the constructor their arguments pick. Each case is a small
//! synthetic repo.

use std::path::Path;

use kc_store::query::{self, RefHit};
use kc_store::trace::{self, Direction, Impact, Trace};

use crate::ops::tests::{repo, test_cache};
use crate::ops::{self, Opened};
use crate::walk::Graph;

/// The repo at `root`, opened as a query opens it (not synced again: the fixture indexed it).
fn opened(root: &Path) -> Opened {
    ops::open_fresh(root, true).unwrap()
}

/// The trace of the first definition of `symbol`, `direction`, `depth` hops, at most 50 nodes.
fn traced(o: &Opened, symbol: &str, direction: Direction, depth: u8) -> Trace {
    let root = query::definitions(o.store.conn(), symbol).unwrap().into_iter().next().unwrap_or_else(|| panic!("{symbol}"));
    crate::tree::trace(&mut Graph::new(o, &[], true, usize::MAX), &root, direction, depth, 50).unwrap()
}

/// What depends on `roots`, to `depth` hops, at most `max` of them.
fn impact(o: &Opened, roots: &[i64], depth: u8, max: usize) -> Impact {
    crate::tree::impact(&mut Graph::new(o, &[], true, usize::MAX), roots, depth, max).unwrap()
}

/// The id of the first definition of `qualified`.
fn def(o: &Opened, qualified: &str) -> i64 {
    query::definitions(o.store.conn(), qualified).unwrap()[0].id
}

const SERVICES: &str = r#"
namespace App;

public interface IOrderService { void Place(); }
public class OrderService : IOrderService { public void Place() { } }

public interface IRepo { void Save(); }
public class SqlRepo : IRepo { public void Save() { } }
public class MemRepo : IRepo { public void Save() { } }
"#;

const STARTUP: &str = r#"
namespace App;

public class Startup
{
    public void Build(IContainer container)
    {
        container.RegisterType<OrderService>().As<IOrderService>().SingleInstance();
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
    private readonly IRepo _repo;

    public OrdersController(IOrderService orders, IRepo repo) { _orders = orders; _repo = repo; }

    public void Post()
    {
        _orders.Place();
        _repo.Save();
    }
}
"#;

/// `trace -d out` goes to the implementation the container binds the service to; `-d in` comes back from it.
#[test]
fn a_trace_goes_through_the_bound_implementation() {
    let cache = test_cache();
    let base = cache.dir("walk-di");
    let root = repo(
        &base,
        "di",
        &[
            // One project compiling every source: a binding across files is the compiler's, not an assumption.
            ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
            ("src/App/Services.cs", SERVICES),
            ("src/App/Startup.cs", STARTUP),
            ("src/App/OrdersController.cs", CONTROLLER),
        ],
    );
    let o = opened(&root);
    let t = traced(&o, "OrdersController.Post", Direction::Out, 1);
    let place = t.nodes.iter().find(|n| n.qualified == "OrderService.Place").expect("bound implementation");
    assert_eq!(place.edge, "via_di");
    assert_eq!(place.binding.as_deref(), Some("src/App/Startup.cs:8, singleton"));
    // Two implementations registered: the call stays on the interface.
    assert!(t.nodes.iter().any(|n| n.qualified == "IRepo.Save" && n.edge == "calls"));
    assert!(!t.nodes.iter().any(|n| n.qualified == "IOrderService.Place"));
    let t = traced(&o, "OrderService.Place", Direction::In, 1);
    assert!(t.nodes.iter().any(|n| n.qualified == "OrdersController.Post" && n.edge == "via_di"));
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}

/// Swift: a call through a protocol reaches the conformer's method (`via_protocol`), and the requirement lists it as a
/// `witness`.
#[test]
fn a_trace_finds_callers_through_protocols() {
    let cache = test_cache();
    let base = cache.dir("walk-protocols");
    let src = r#"
protocol Fetching { func fetch() -> Int }
struct Fetcher: Fetching { func fetch() -> Int { 1 } }
final class Model {
    let source: any Fetching
    init(source: any Fetching = Fetcher()) { self.source = source }
    func load() -> Int { source.fetch() }
}
func main() { _ = Model().load() }
"#;
    let root = repo(&base, "swift", &[("Sources/T.swift", src)]);
    let o = opened(&root);
    let t = traced(&o, "Fetcher.fetch", Direction::In, 3);
    let edges: Vec<(&str, &str, u8)> = t.nodes.iter().skip(1).map(|n| (n.edge, n.qualified.as_str(), n.depth)).collect();
    // Called through the protocol by Model.load, which main calls.
    assert_eq!(edges[0], ("via_protocol", "Model.load", 1));
    assert_eq!(edges[1], ("calls", "main", 2));
    let req = traced(&o, "Fetching.fetch", Direction::In, 3);
    assert!(req.nodes.iter().any(|n| n.edge == "witness" && n.qualified == "Fetcher.fetch"));
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}

/// Swift: what depends on a method, hop by hop.
#[test]
fn an_impact_lists_dependents_by_hop() {
    let cache = test_cache();
    let base = cache.dir("walk-impact");
    let src = r#"
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
"#;
    let root = repo(&base, "swift", &[("Sources/I.swift", src)]);
    let o = opened(&root);
    let i = impact(&o, &[def(&o, "Store.save")], 3, 50);
    let got: Vec<(&str, u8)> = i.affected.iter().map(|a| (a.qualified.as_str(), a.hop)).collect();
    assert_eq!(got, [("Model.commit", 1), ("View.tap", 2)]);
    assert_eq!(i.nearest, "direct");
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}

const DATA: &str = r#"using Microsoft.EntityFrameworkCore;

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
"#;

/// An entity's impact goes through its context only to the code using its DbSet; at the limit, a direct call outranks
/// a call through the container's binding at the same hop.
#[test]
fn an_entitys_impact_goes_through_its_dbset_and_direct_uses_outrank_bindings() {
    let cache = test_cache();
    let base = cache.dir("walk-entity");
    let root = repo(
        &base,
        "shop",
        &[
            ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
            ("src/Data.cs", DATA),
            ("src/A.cs", "namespace Shop;\npublic class Billing(Shop.Data.IFeeRepo fees)\n{\n    public decimal Due() => fees.Total();\n}\n"),
            ("src/B.cs", "namespace Shop;\npublic class Audit\n{\n    public decimal Check() => new Shop.Data.FeeRepo(null).Total();\n}\n"),
            ("src/Module.cs", "using Autofac;\nnamespace Shop;\npublic class M { void Load(ContainerBuilder b) { b.RegisterType<Shop.Data.FeeRepo>().As<Shop.Data.IFeeRepo>(); } }\n"),
        ],
    );
    let o = opened(&root);
    let fee = def(&o, "Fee");
    let names = |i: &Impact| i.affected.iter().map(|a| (a.qualified.clone(), a.edge)).collect::<Vec<_>>();
    let all = impact(&o, &[fee], 3, 100);
    let affected = names(&all);
    assert!(affected.iter().any(|(q, _)| q == "FeeRepo.Total"), "{affected:?}");
    assert!(!affected.iter().any(|(q, _)| q.starts_with("OrderRepo")), "{affected:?}");
    let hop2 = all.affected.iter().filter(|a| a.hop <= 2).count();
    let capped = impact(&o, &[fee], 3, hop2 + 1);
    assert!(capped.truncated);
    let kept = names(&capped);
    assert!(kept.contains(&("Audit.Check".to_string(), "calls")) && !kept.iter().any(|(q, _)| q == "Billing.Due"), "{kept:?}");
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}

const ENV: &str = r#"
variable "region" {}

module "net" {
  source = "../../../modules/net"
  cidr   = "10.0.0.0/16"
}

resource "aws_instance" "web" {
  count     = 2
  subnet_id = module.net.subnet_id
}
"#;

const MODULE: &str = r#"
variable "cidr" {
  description = "The VPC's range"
}

resource "aws_vpc" "this" {
  cidr_block = var.cidr
}

output "subnet_id" {
  value = aws_vpc.this.id
}
"#;

/// Terraform: the impact of a module's resource crosses its output into every configuration calling the module.
#[test]
fn an_impact_crosses_a_modules_output() {
    let cache = test_cache();
    let base = cache.dir("walk-terraform");
    let envs = ["envs/prod/us-east-1", "envs/prod/us-west-2", "envs/staging/us-east-1"];
    let mut files: Vec<(String, &str)> = envs.iter().map(|e| (format!("{e}/main.tf"), ENV)).collect();
    files.push(("modules/net/main.tf".into(), MODULE));
    let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), *t)).collect();
    let root = repo(&base, "infra", &files);
    let o = opened(&root);
    let i = impact(&o, &[def(&o, "aws_vpc.this")], 3, 100);
    let reached: Vec<(String, String)> = i.affected.iter().map(|a| (a.qualified.clone(), a.file.clone())).collect();
    assert!(reached.contains(&("output.subnet_id".into(), "modules/net/main.tf".into())), "{reached:?}");
    for env in envs {
        assert!(reached.contains(&("aws_instance.web".into(), format!("{env}/main.tf"))), "{reached:?}");
    }
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}

/// Two calls on one line each resolve through their own receiver: changing the second one's target affects the caller.
#[test]
fn the_second_call_on_a_line_is_a_use_of_its_own() {
    let cache = test_cache();
    let base = cache.dir("walk-own-receiver");
    let root = repo(
        &base,
        "stores",
        &[
            ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
            ("Stores.cs", "public class FirstStore { public void Save() {} }\npublic class SecondStore { public void Save() {} }\n"),
            ("Caller.cs", "public class Caller { public void Run(FirstStore first, SecondStore second) { first.Save(); second.Save(); } }\n"),
        ],
    );
    let o = opened(&root);
    let i = impact(&o, &[def(&o, "SecondStore.Save")], 1, 100);
    let affected: Vec<(&str, &str)> = i.affected.iter().map(|a| (a.qualified.as_str(), a.confidence)).collect();
    assert_eq!(affected, [("Caller.Run", "exact")]);
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}

/// ColdFusion: a component's method is reached from the page calling it, a template.
#[test]
fn a_cfml_trace_reaches_the_page() {
    let cache = test_cache();
    let base = cache.dir("walk-cfml");
    let root = repo(
        &base,
        "cfml",
        &[
            ("Application.cfc", "component {\n    this.name = \"shop\";\n}\n"),
            (
                "model/UserService.cfc",
                "component {\n    public any function init() {\n        return this;\n    }\n    public any function getUser(required numeric id) {\n        return id;\n    }\n}\n",
            ),
            (
                "admin/users.cfm",
                "<cfset svc = createObject(\"component\", \"model.UserService\").init()>\n<cfset u = svc.getUser(1)>\n",
            ),
        ],
    );
    let o = opened(&root);
    let t = traced(&o, "UserService.getUser", Direction::In, 2);
    assert!(t.nodes.iter().any(|n| n.qualified == "users" && n.kind == "template"), "{:?}", t.nodes.iter().map(|n| &n.qualified).collect::<Vec<_>>());
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}

// Walks that don't overstate what they know (C#): a creation reaches the constructor its arguments pick, or every
// candidate as an ambiguous edge; a type named only as a generic argument (or in `typeof`, `nameof`, a constraint) is
// referenced there; a call through a base interface or a base class's member reaches the implementations.

/// The repo `name` holding `files` under `src/App`, in one C# project.
fn indexed(base: &Path, name: &str, files: &[(&str, &str)]) -> Opened {
    let mut all = vec![("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n")];
    all.extend_from_slice(files);
    opened(&repo(base, name, &all))
}

/// The id of the definition of `qualified` starting at `line` (overloads share a name).
fn def_at(o: &Opened, qualified: &str, line: u32) -> i64 {
    let defs = query::definitions(o.store.conn(), qualified).unwrap();
    defs.iter().find(|d| d.line == line).unwrap_or_else(|| panic!("{qualified} at {line}: {defs:?}")).id
}

/// What reaches the symbol `id` one hop in, as `trace -d in` and `impact` read it: (caller, edge, confidence).
fn callers_of(o: &Opened, id: i64) -> Vec<(String, &'static str, &'static str)> {
    let sym = trace::symbol(o.store.conn(), id).unwrap().unwrap();
    let mut cache = trace::EdgeCache::default();
    let mut out: Vec<(String, &'static str, &'static str)> =
        trace::in_edges(o.store.conn(), &mut cache, &sym).unwrap().into_iter().map(|(s, e, c, _)| (s.qualified(), e, c)).collect();
    out.sort();
    out
}

/// What `impact` lists for `roots` to `depth`: (qualified, confidence).
fn impacted(o: &Opened, roots: &[i64], depth: u8) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = impact(o, roots, depth, 200).affected.iter().map(|a| (a.qualified.clone(), a.confidence)).collect();
    out.sort();
    out
}

const ERRORS: &str = r#"namespace App;

public class Resp { }

public class Err : System.Exception
{
    public Err(Resp r) { Code(); }
    public Err(int status, string reason, string msg = null) { }
    public Err(string reason, params object[] details) { }
    public Err() : this(500, "x") { }
    public static string Code() => "";
}

public class Sub : Err
{
    public Sub(Resp r) : base(r) { }
}

public struct Point
{
    public Point(int x, int y) { }
}

public class Holder(int id, string name)
{
    public Holder(int id) : this(id, "") { }
}
"#;

const CREATORS: &str = r#"namespace App;

public class A { public void M() { throw new Err(400, "x", "y"); } }
public class B { public void N() { throw new Err(new Resp()); } }
public class C { public void Named() { var e = new Err(status: 1, reason: "r"); } }
public class D { public void Params() { var e = new Err("r", 1, 2, 3); } }
public class E { public Err Target() { Err e = new(new Resp()); return e; } }
public class F { public void Unknown(object o) { var e = new Err(o); } }
public class G
{
    public void Make()
    {
        var p = new Point();
        var q = new Point(1, 2);
        var h = new Holder(1, "a");
        var k = new Holder(2);
        var s = new Sub(new Resp()) { };
    }
}
"#;

#[test]
fn a_creation_reaches_the_constructor_its_arguments_pick() {
    let cache = test_cache();
    let base = cache.dir("walk-ctors");
    let o = indexed(&base, "ctors", &[("src/App/Errors.cs", ERRORS), ("src/App/Creators.cs", CREATORS)]);
    let by_resp = def_at(&o, "Err.init", 7);
    let by_status = def_at(&o, "Err.init", 8);
    let by_params = def_at(&o, "Err.init", 9);
    let bare = def_at(&o, "Err.init", 10);
    let s = |q: &str, e: &'static str, c: &'static str| (q.to_string(), e, c);
    // `new Err(new Resp())` and the target-typed `new(new Resp())` call `Err(Resp)`; `new Err(o)` with an `object`
    // could be either one-argument constructor, so it is ambiguous; `Sub(Resp)` chains to it with `: base(r)`.
    assert_eq!(
        callers_of(&o, by_resp),
        [s("B.N", "instantiates", "unique_name"), s("E.Target", "instantiates", "unique_name"), s("F.Unknown", "instantiates", "ambiguous"), s("Sub.init", "calls", "exact")]
    );
    // Three arguments, and two named ones, fit only `Err(int, string, string = null)`; `: this(500, "x")` too.
    assert_eq!(callers_of(&o, by_status), [s("A.M", "instantiates", "unique_name"), s("C.Named", "instantiates", "unique_name"), s("Err.init", "calls", "exact")]);
    assert_eq!(callers_of(&o, by_params), [s("D.Params", "instantiates", "unique_name"), s("F.Unknown", "instantiates", "ambiguous")]);
    assert!(callers_of(&o, bare).is_empty(), "{:?}", callers_of(&o, bare));
    // `new Point()` is the implicit parameterless constructor; a primary constructor is told from a declared one.
    let point = def_at(&o, "Point.init", 21);
    assert_eq!(callers_of(&o, point), [s("G.Make", "instantiates", "unique_name")]);
    let holder = def_at(&o, "Holder.init", 26);
    assert_eq!(callers_of(&o, holder), [s("G.Make", "instantiates", "unique_name")]);
    let (made, _) = query::refs(o.store.conn(), "Holder", &["instantiate"], 10).unwrap();
    let ctors: Vec<Option<Vec<i64>>> = made.iter().map(|h| h.ctors.clone()).collect();
    let holder_type = query::definitions(o.store.conn(), "Holder").unwrap().into_iter().find(|d| d.kind == "class").unwrap().id;
    assert_eq!(ctors, [Some(vec![holder_type]), Some(vec![holder])]);

    // The diagnosis repro: only the creation calling `Err(Resp)` depends on `Code`.
    let code = def_at(&o, "Err.Code", 11);
    let got: Vec<String> = impacted(&o, &[code], 2).into_iter().filter(|(_, c)| *c != "ambiguous").map(|(q, _)| q).collect();
    assert_eq!(got, ["B.N", "E.Target", "Err.init", "Sub.init"]);
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}

const BUS: &str = r#"namespace App;

public interface IBus { }
public interface IOrdersBus : IBus { }
public interface ISvc { }
public class Svc : ISvc { }
public class Reg { public Reg As<T>() => this; }
public class Container
{
    public void AddBus<T>() { }
    public T Get<T>() => default;
    public Reg RegisterType<T>() => null;
}

public class Setup
{
    public void Run(Container c)
    {
        c.AddBus<IOrdersBus>();
        var x = c?.Get<IOrdersBus>();
        c.RegisterType<Svc>().As<ISvc>();
        var t = typeof(Svc);
        var n = nameof(Svc);
    }

    public void Constrained<T>() where T : ISvc { }
}
"#;

fn uses(hits: &[RefHit]) -> Vec<(u32, &'static str, Option<&str>)> {
    hits.iter().map(|h| (h.line, h.confidence, h.usage.as_deref())).collect()
}

#[test]
fn a_type_named_as_a_generic_argument_is_referenced_there() {
    let cache = test_cache();
    let base = cache.dir("walk-generic-args");
    let o = indexed(&base, "generic-args", &[("src/App/Setup.cs", BUS)]);
    let (bus, _) = query::refs(o.store.conn(), "IOrdersBus", &["type_ref"], 50).unwrap();
    assert_eq!(uses(&bus), [(19, "unique_name", Some("type_argument")), (20, "unique_name", Some("type_argument"))]);
    let (svc, _) = query::refs(o.store.conn(), "Svc", &["type_ref"], 50).unwrap();
    assert_eq!(uses(&svc), [(21, "unique_name", Some("type_argument")), (22, "unique_name", Some("typeof")), (23, "unique_name", Some("nameof"))]);
    let (isvc, _) = query::refs(o.store.conn(), "ISvc", &["type_ref"], 50).unwrap();
    let lines: Vec<(u32, Option<&str>)> = isvc.iter().map(|h| (h.line, h.usage.as_deref())).collect();
    assert_eq!(lines, [(6, None), (21, Some("type_argument")), (26, Some("constraint"))]);
    // The registration site is a use the type's dependents see.
    let svc_type = def_at(&o, "Svc", 6);
    assert!(impacted(&o, &[svc_type], 1).iter().any(|(q, _)| q == "Setup.Run"));
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}

const DISPATCH: &str = r#"namespace App;

public interface IBase { string Get(string f); }
public interface IDerived : IBase { }
public class Impl : IDerived { public string Get(string f) => ""; }
public class Other : IBase { public string Get(string f) => ""; }
public class Caller { IBase b; public void Run() { b.Get("x"); } }
public class Caller2 { IDerived d; public void Run2() { d.Get("y"); } }
public class Top { Caller c; public void Go() { c.Run(); } }

public abstract class Shape
{
    public abstract double Area();
    public virtual string Name() => "";
}
public class Circle : Shape
{
    public override double Area() => 1;
    public override string Name() => "c";
}
public class Painter { Shape s; public void Paint() { s.Area(); s.Name(); } }

public interface IRepo<T> { T Find(int id); }
public class Order { }
public class Plan { }
public interface IOrders : IRepo<Order> { }
public interface IPlans : IRepo<Plan> { }
public class OrderRepo : IOrders { public Order Find(int id) => null; }
public class PlanRepo : IPlans { public Plan Find(int id) => null; }
public class UsesOrders { IOrders o; public void Go() { o.Find(1); } }
public class UsesPlans { IPlans p; public void Go() { p.Find(1); } }
"#;

#[test]
fn a_call_through_a_base_interface_or_class_reaches_the_implementation() {
    let cache = test_cache();
    let base = cache.dir("walk-dispatch");
    let o = indexed(&base, "dispatch", &[("src/App/Dispatch.cs", DISPATCH)]);
    let get = def_at(&o, "Impl.Get", 5);
    let got: Vec<String> = impacted(&o, &[get], 2).into_iter().map(|(q, _)| q).collect();
    assert_eq!(got, ["Caller.Run", "Caller2.Run2", "Top.Go"]);
    let callers: Vec<(String, &str)> = callers_of(&o, get).into_iter().map(|(q, e, _)| (q, e)).collect();
    assert_eq!(callers, [("Caller.Run".to_string(), "via_protocol"), ("Caller2.Run2".to_string(), "via_protocol")]);
    // `trace in` names the same callers, as it does a call through the interface the type declares itself.
    let t = traced(&o, "Impl.Get", Direction::In, 1);
    let nodes: Vec<(String, &str)> = t.nodes.iter().skip(1).map(|n| (n.qualified.clone(), n.edge)).collect();
    assert_eq!(nodes, [("Caller.Run".to_string(), "via_protocol"), ("Caller2.Run2".to_string(), "via_protocol")]);
    // A walk searching both ways (`path`, `flow`) calls them one of several implementations, as the forward
    // `dispatch` edge is.
    let sym = trace::symbol(o.store.conn(), get).unwrap().unwrap();
    let mut both: Vec<(String, &str)> = trace::in_edges(o.store.conn(), &mut trace::EdgeCache::default(), &sym)
        .unwrap()
        .into_iter()
        .map(|(s, _, c, _)| (s.qualified(), c))
        .collect();
    both.sort();
    assert_eq!(both, [("Caller.Run".to_string(), "ambiguous"), ("Caller2.Run2".to_string(), "ambiguous")]);
    for m in [def_at(&o, "Circle.Area", 18), def_at(&o, "Circle.Name", 19)] {
        let got: Vec<String> = impacted(&o, &[m], 1).into_iter().map(|(q, _)| q).collect();
        assert_eq!(got, ["Painter.Paint"]);
    }
    // Through a generic base interface, only the receivers this implementation is one of: not another closure's.
    let find = def_at(&o, "OrderRepo.Find", 28);
    let got: Vec<String> = impacted(&o, &[find], 1).into_iter().map(|(q, _)| q).collect();
    assert_eq!(got, ["UsesOrders.Go"]);
    drop(o);
    let _ = std::fs::remove_dir_all(&base);
}
