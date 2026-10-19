//! Walks that don't overstate what they know (C#): a creation reaches the constructor its arguments pick, or every
//! candidate as an ambiguous edge; a type named only as a generic argument (or in `typeof`, `nameof`, a constraint)
//! is referenced there; a call through a base interface or a base class's member reaches the implementations.

use std::path::PathBuf;
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query::{self, RefHit};
use kc_store::trace::{self, Direction, TraceOptions};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/App")).unwrap();
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    std::fs::write(dir.join("App.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n").unwrap();
    dir
}

fn indexed(name: &str, files: &[(&str, &str)]) -> (PathBuf, Store) {
    let root = scratch(name);
    for (path, text) in files {
        std::fs::write(root.join(path), text).unwrap();
    }
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    (root, store)
}

/// The id of the definition of `qualified` starting at `line` (overloads share a name).
fn def_at(store: &Store, qualified: &str, line: u32) -> i64 {
    let defs = query::definitions(store.conn(), qualified).unwrap();
    defs.iter().find(|d| d.line == line).unwrap_or_else(|| panic!("{qualified} at {line}: {defs:?}")).id
}

/// What reaches the symbol `id` one hop in: (caller, edge, confidence).
fn callers_of(store: &Store, id: i64) -> Vec<(String, &'static str, &'static str)> {
    let sym = trace::symbol(store.conn(), id).unwrap().unwrap();
    let mut cache = trace::EdgeCache::default();
    let mut out: Vec<(String, &'static str, &'static str)> =
        trace::in_edges(store.conn(), &mut cache, &sym).unwrap().into_iter().map(|(s, e, c, _)| (s.qualified(), e, c)).collect();
    out.sort();
    out
}

/// [`callers_of`] as a walk searching both ways labels them: (caller, confidence).
fn callers_both(store: &Store, id: i64) -> Vec<(String, &'static str)> {
    callers_of(store, id).into_iter().map(|(q, _, c)| (q, c)).collect()
}

/// What `impact` lists for `roots` to `depth`: (qualified, confidence).
fn impacted(store: &Store, roots: &[i64], depth: u8) -> Vec<(String, &'static str)> {
    let i = trace::impact(store.conn(), roots, depth, 200).unwrap();
    let mut out: Vec<(String, &'static str)> = i.affected.iter().map(|a| (a.qualified.clone(), a.confidence)).collect();
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
    let (_root, store) = indexed("ctors", &[("src/App/Errors.cs", ERRORS), ("src/App/Creators.cs", CREATORS)]);
    let by_resp = def_at(&store, "Err.init", 7);
    let by_status = def_at(&store, "Err.init", 8);
    let by_params = def_at(&store, "Err.init", 9);
    let bare = def_at(&store, "Err.init", 10);
    let s = |q: &str, e: &'static str, c: &'static str| (q.to_string(), e, c);
    // `new Err(new Resp())` and the target-typed `new(new Resp())` call `Err(Resp)`; `new Err(o)` with an `object`
    // could be either one-argument constructor, so it is ambiguous; `Sub(Resp)` chains to it with `: base(r)`.
    assert_eq!(
        callers_of(&store, by_resp),
        [s("B.N", "instantiates", "unique_name"), s("E.Target", "instantiates", "unique_name"), s("F.Unknown", "instantiates", "ambiguous"), s("Sub.init", "calls", "exact")]
    );
    // Three arguments, and two named ones, fit only `Err(int, string, string = null)`; `: this(500, "x")` too.
    assert_eq!(callers_of(&store, by_status), [s("A.M", "instantiates", "unique_name"), s("C.Named", "instantiates", "unique_name"), s("Err.init", "calls", "exact")]);
    assert_eq!(callers_of(&store, by_params), [s("D.Params", "instantiates", "unique_name"), s("F.Unknown", "instantiates", "ambiguous")]);
    assert!(callers_of(&store, bare).is_empty(), "{:?}", callers_of(&store, bare));
    // `new Point()` is the implicit parameterless constructor; a primary constructor is told from a declared one.
    let point = def_at(&store, "Point.init", 21);
    assert_eq!(callers_of(&store, point), [s("G.Make", "instantiates", "unique_name")]);
    let holder = def_at(&store, "Holder.init", 26);
    assert_eq!(callers_of(&store, holder), [s("G.Make", "instantiates", "unique_name")]);
    let (made, _) = query::refs(store.conn(), "Holder", &["instantiate"], 10).unwrap();
    let ctors: Vec<Option<Vec<i64>>> = made.iter().map(|h| h.ctors.clone()).collect();
    let holder_type = query::definitions(store.conn(), "Holder").unwrap().into_iter().find(|d| d.kind == "class").unwrap().id;
    assert_eq!(ctors, [Some(vec![holder_type]), Some(vec![holder])]);

    // The diagnosis repro: only the creation calling `Err(Resp)` depends on `Code`.
    let code = def_at(&store, "Err.Code", 11);
    let got: Vec<String> = impacted(&store, &[code], 2).into_iter().filter(|(_, c)| *c != "ambiguous").map(|(q, _)| q).collect();
    assert_eq!(got, ["B.N", "E.Target", "Err.init", "Sub.init"]);
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
    let (_root, store) = indexed("generic-args", &[("src/App/Setup.cs", BUS)]);
    let (bus, _) = query::refs(store.conn(), "IOrdersBus", &["type_ref"], 50).unwrap();
    assert_eq!(uses(&bus), [(19, "unique_name", Some("type_argument")), (20, "unique_name", Some("type_argument"))]);
    let (svc, _) = query::refs(store.conn(), "Svc", &["type_ref"], 50).unwrap();
    assert_eq!(uses(&svc), [(21, "unique_name", Some("type_argument")), (22, "unique_name", Some("typeof")), (23, "unique_name", Some("nameof"))]);
    let (isvc, _) = query::refs(store.conn(), "ISvc", &["type_ref"], 50).unwrap();
    let lines: Vec<(u32, Option<&str>)> = isvc.iter().map(|h| (h.line, h.usage.as_deref())).collect();
    assert_eq!(lines, [(6, None), (21, Some("type_argument")), (26, Some("constraint"))]);
    // The registration site is a use the type's dependents see.
    let svc_type = def_at(&store, "Svc", 6);
    assert!(impacted(&store, &[svc_type], 1).iter().any(|(q, _)| q == "Setup.Run"));
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
    let (_root, store) = indexed("dispatch", &[("src/App/Dispatch.cs", DISPATCH)]);
    let get = def_at(&store, "Impl.Get", 5);
    let got: Vec<String> = impacted(&store, &[get], 2).into_iter().map(|(q, _)| q).collect();
    assert_eq!(got, ["Caller.Run", "Caller2.Run2", "Top.Go"]);
    let callers: Vec<(String, &str)> = callers_of(&store, get).into_iter().map(|(q, e, _)| (q, e)).collect();
    assert_eq!(callers, [("Caller.Run".to_string(), "via_protocol"), ("Caller2.Run2".to_string(), "via_protocol")]);
    // `trace in` names the same callers, as it does a call through the interface the type declares itself.
    let t = trace::trace(store.conn(), "Impl.Get", &TraceOptions { direction: Direction::In, depth: 1, max_nodes: 50 }).unwrap().unwrap();
    let nodes: Vec<(String, &str)> = t.nodes.iter().skip(1).map(|n| (n.qualified.clone(), n.edge)).collect();
    assert_eq!(nodes, [("Caller.Run".to_string(), "via_protocol"), ("Caller2.Run2".to_string(), "via_protocol")]);
    // A walk searching both ways (`path`, `flow`) calls them one of several implementations, as the forward
    // `dispatch` edge is.
    let both: Vec<(String, &str)> = callers_both(&store, get);
    assert_eq!(both, [("Caller.Run".to_string(), "ambiguous"), ("Caller2.Run2".to_string(), "ambiguous")]);
    for m in [def_at(&store, "Circle.Area", 18), def_at(&store, "Circle.Name", 19)] {
        let got: Vec<String> = impacted(&store, &[m], 1).into_iter().map(|(q, _)| q).collect();
        assert_eq!(got, ["Painter.Paint"]);
    }
    // Through a generic base interface, only the receivers this implementation is one of: not another closure's.
    let find = def_at(&store, "OrderRepo.Find", 28);
    let got: Vec<String> = impacted(&store, &[find], 1).into_iter().map(|(q, _)| q).collect();
    assert_eq!(got, ["UsesOrders.Go"]);
}
