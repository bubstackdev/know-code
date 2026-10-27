//! C# gaps: tables of Entity Framework owned types, overloads told apart by their arguments' types, and `?.` reads.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
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

/// Per reference to `name` of `kind`: line, confidence and the targets' `qualified:line`.
fn answers(conn: &rusqlite::Connection, name: &str, kind: &str) -> Vec<(u32, &'static str, Vec<String>)> {
    let (mut hits, _) = query::refs(conn, name, &[kind], 100).unwrap();
    hits.sort_by_key(|h| h.line);
    hits.iter().map(|h| (h.line, h.confidence, h.targets.iter().map(|t| format!("{}:{}", t.qualified, t.line)).collect())).collect()
}

const DATA: &str = r#"using System.Collections.Generic;
using System.Linq;
using Microsoft.EntityFrameworkCore;

namespace Shop.Data;

public class Plan
{
    public int Id { get; set; }
    public List<Payment> Payments { get; set; }
    public Address Billing { get; set; }
}

public class Payment { public decimal Amount { get; set; } }
public class Address { public string City { get; set; } }

public class ShopContext : DbContext
{
    public DbSet<Plan> Plans { get; set; }

    protected override void OnModelCreating(ModelBuilder modelBuilder)
    {
        modelBuilder.Entity<Plan>(b =>
        {
            b.ToTable("payment_plan");
            b.OwnsMany(p => p.Payments, pb => pb.ToTable("invoice_line"));
            b.OwnsOne(p => p.Billing).ToTable("plan_address");
        });
    }
}

public class Plans(ShopContext ctx)
{
    public decimal Total(int id) => ctx.Plans.First(p => p.Id == id).Payments.Sum(p => p.Amount);
    public string City(Plan plan) => plan?.Billing?.City;
}
"#;

#[test]
fn an_owned_types_table_is_used_through_its_owners_navigation() {
    let root = scratch("owned-tables");
    write(&root, "src/Data.cs", DATA);
    let store = index(&root);
    let model = store.db_model().unwrap();
    let table = |name: &str| model.tables.iter().find(|t| t.table == name).unwrap_or_else(|| panic!("no table {name}"));
    let payments = table("invoice_line");
    assert_eq!((payments.entity.as_str(), payments.navigation.as_deref()), ("Payment", Some("Plan.Payments")));
    assert_eq!(payments.context.as_deref(), Some("ShopContext"));
    // The code reading `plan.Payments` reads the table; the model configuration naming it doesn't.
    let accessors: Vec<(&str, &str)> = payments.accessors.iter().map(|a| (a.symbol.as_str(), a.mode.as_str())).collect();
    assert_eq!(accessors, [("Plans.Total", "read")]);
    let address = table("plan_address");
    assert_eq!((address.entity.as_str(), address.navigation.as_deref()), ("Address", Some("Plan.Billing")));
    assert_eq!(address.accessors.iter().map(|a| a.symbol.as_str()).collect::<Vec<_>>(), ["Plans.City"]);
}

const OWNERS: &[(&str, &str)] = &[
    ("src/Models/Plan.cs", "using System.Collections.Generic;\nnamespace Shop.Models;\npublic class Plan\n{\n    public List<string> Payments { get; set; }\n}\n"),
    ("src/Models/Enums/Kind.cs", "namespace Shop.Models.Enums;\npublic enum Kind { Monthly }\n"),
    (
        "src/Data/Entities/Plan.cs",
        "using System.Collections.Generic;\nnamespace Shop.Data.Entities;\npublic class Payment { public decimal Amount { get; set; } }\npublic class Plan\n{\n    public List<Payment> Payments { get; set; }\n    public void Pay(Payment p)\n    {\n        Payments.Add(p);\n    }\n}\n",
    ),
    (
        "src/Data/ShopContext.cs",
        "using Microsoft.EntityFrameworkCore;\nusing Shop.Models.Enums;\nusing Shop.Data.Entities;\nnamespace Shop.Data;\npublic class ShopContext : DbContext\n{\n    public DbSet<Plan> Plans { get; set; }\n    protected override void OnModelCreating(ModelBuilder modelBuilder)\n    {\n        modelBuilder.Entity<Plan>(b => b.OwnsMany(p => p.Payments, pb => pb.ToTable(\"plan_payment\")));\n    }\n}\n",
    ),
];

#[test]
fn an_owned_types_owner_is_the_entity_the_context_sees_and_its_own_methods_write_it() {
    let root = scratch("owned-owner");
    for (path, text) in OWNERS {
        write(&root, path, text);
    }
    let store = index(&root);
    let model = store.db_model().unwrap();
    let t = model.tables.iter().find(|t| t.table == "plan_payment").unwrap();
    // `using Shop.Models.Enums` doesn't make `Shop.Models.Plan` visible: the owner is the entity.
    assert_eq!((t.entity.as_str(), t.file.as_str()), ("Payment", "src/Data/Entities/Plan.cs"));
    // `Payments.Add(p)` inside the owner writes the table.
    let accessors: Vec<(&str, &str)> = t.accessors.iter().map(|a| (a.symbol.as_str(), a.mode.as_str())).collect();
    assert_eq!(accessors, [("Plan.Pay", "write")]);
}

#[test]
fn conditional_reads_are_member_references() {
    let root = scratch("conditional-reads");
    write(&root, "src/Data.cs", DATA);
    let store = index(&root);
    let conn = store.conn();
    // `plan?.Billing?.City`: both reads, each on the type before it.
    assert!(answers(conn, "Billing", "member").contains(&(35, "exact", vec!["Plan.Billing:11".to_string()])));
    assert_eq!(answers(conn, "City", "member"), [(35, "exact", vec!["Address.City:15".to_string()])]);
}

#[test]
fn overloads_are_told_apart_by_their_arguments_types() {
    let root = scratch("overload-types");
    write(
        &root,
        "src/Over.cs",
        r#"using System.Collections.Generic;

namespace Shop.Query;

public enum Env { Dev, Prod }
public class Doc { public string Id { get; set; } public int Rank { get; set; } }
public class Animal { }
public class Dog : Animal { }
public interface IShape { }
public class Square : IShape { }

public interface IQuery
{
    string Get(string id);
    string Get(IEnumerable<string> ids);
}

public static class Filters
{
    public static string By(this IQuery q, Env env) => "";
    public static string By(this IQuery q, string env) => "";
    public static void Feed(Animal a) { }
    public static void Feed(string s) { }
    public static void Draw(IShape s) { }
    public static void Draw(int n) { }
    public static void Scale(long n) { }
    public static void Scale(string s) { }
    public static void Keep<T>(T value) { }
    public static void Keep(string s) { }
    public static void Cell(int row, string value) { }
    public static void Cell(int row, int value) { }
    public static void Cell(int row, double value) { }
    public static void Find(bool load, string name) { }
    public static void Find(string name, bool check = false, int domainId = 0) { }
    public static void Find(string name, int mustBeAccountId) { }
}

public class Shape : IShape
{
    public void Show() => Filters.Draw(this);
}

public class Use
{
    public void Run(IQuery q, Doc doc, List<string> ids, int count, object anything)
    {
        q.Get(doc.Id);
        q.Get(ids);
        q.By(Env.Prod);
        q.By("staging");
        Filters.Feed(new Dog());
        Filters.Feed("x");
        Filters.Draw(new Square());
        Filters.Draw(count);
        Filters.Scale(3);
        Filters.Scale(doc.Rank);
        Filters.Scale($"a{count}");
        Filters.Keep(count);
        Filters.Feed(anything);
        Filters.Cell(1, anything.ToString());
        Filters.Cell(1, ids.Count);
        Filters.Cell(1, string.Format("{0}", count));
        Filters.Find("", domainId: count);
    }
}
"#,
    );
    let store = index(&root);
    let conn = store.conn();
    let targets = |name: &str| answers(conn, name, "call").into_iter().map(|(l, _, t)| (l, t)).collect::<Vec<_>>();
    let one = |l: u32, t: &str| (l, vec![t.to_string()]);
    // A string isn't a sequence of strings; a list of them is.
    assert_eq!(targets("Get"), [one(47, "IQuery.Get:14"), one(48, "IQuery.Get:15")]);
    // An enum member, a string literal.
    assert_eq!(targets("By"), [one(49, "IQuery.By:20"), one(50, "IQuery.By:21")]);
    // Inheritance, interface implementation, implicit numeric conversions and literals.
    // `this` is the enclosing class (a `Shape`, an `IShape`).
    assert_eq!(targets("Draw"), [one(40, "Filters.Draw:24"), one(53, "Filters.Draw:24"), one(54, "Filters.Draw:25")]);
    assert_eq!(targets("Scale"), [one(55, "Filters.Scale:26"), one(56, "Filters.Scale:26"), one(57, "Filters.Scale:27")]);
    // A generic parameter takes anything; a string doesn't take an int. An argument of unknown fit leaves both.
    assert_eq!(targets("Keep"), [one(58, "Filters.Keep:28")]);
    // `ToString()` and `string.Format` give strings; a `Count` an `int`, which the `int` overload takes as is.
    assert_eq!(targets("Cell"), [one(60, "Filters.Cell:30"), one(61, "Filters.Cell:31"), one(62, "Filters.Cell:30")]);
    // A named argument goes to the parameter of that name.
    assert_eq!(targets("Find"), [one(63, "Filters.Find:34")]);
    let feeds = targets("Feed");
    assert_eq!(feeds[..2], [one(51, "Filters.Feed:22"), one(52, "Filters.Feed:23")]);
    assert_eq!(feeds[2].1.len(), 2);
}

const WRITES: &str = r#"using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.EntityFrameworkCore;

namespace Shop.Data;

public class Cancellation { public string Reason { get; set; } }
public class Line { public int Qty { get; set; } }
public class Log { public int Id { get; set; } public string Text { get; set; } }

public class Payout
{
    public int Id { get; set; }
    public string Status { get; set; }
    public Cancellation CancellationDetails { get; set; }
    public List<Line> Lines { get; set; }

    public void Cancel(string reason)
    {
        CancellationDetails = new Cancellation { Reason = reason };
    }

    public void Clear() => Lines.Clear();
}

public class ShopContext : DbContext
{
    public DbSet<Payout> Payouts { get; set; }
    public DbSet<Log> Logs { get; set; }

    protected override void OnModelCreating(ModelBuilder modelBuilder)
    {
        modelBuilder.Entity<Payout>(b =>
        {
            b.OwnsOne(p => p.CancellationDetails, c => c.ToTable("payout_cancellation"));
            b.OwnsMany(p => p.Lines, l => l.ToTable("payout_line"));
        });
    }
}

public interface IPayoutStore { Task SaveAsync(); }

public class PayoutStore(ShopContext ctx) : IPayoutStore
{
    public Task SaveAsync() => ctx.SaveChangesAsync();
}

public class Payouts(ShopContext ctx, IPayoutStore store)
{
    public void Reject(int id) => ctx.Payouts.First(p => p.Id == id).CancellationDetails = new Cancellation();
    public void RejectAndSave(int id)
    {
        ctx.Payouts.First(p => p.Id == id).CancellationDetails = new Cancellation();
        ctx.SaveChanges();
    }
    public Payout Draft() => new Payout { CancellationDetails = new Cancellation(), Lines = new List<Line>() };
    public void Create()
    {
        ctx.Payouts.Add(new Payout { CancellationDetails = new Cancellation(), Lines = new List<Line>() });
        ctx.SaveChanges();
    }
    public async Task Settle(int id)
    {
        var payout = ctx.Payouts.First(p => p.Id == id);
        payout.Status = "settled";
        await store.SaveAsync();
    }
    public void Mark(int id)
    {
        var payout = ctx.Payouts.Find(id);
        payout.Status = "marked";
        ctx.Entry(payout).State = EntityState.Modified;
    }
    public Payout Load(int id)
    {
        var payout = ctx.Payouts.First(p => p.Id == id);
        payout.Status = "loaded";
        return payout;
    }
    private void Expire(int id)
    {
        ctx.Payouts.First(p => p.Id == id).Status = "expired";
    }
    public void ExpireAndLog(int id)
    {
        var open = ctx.Payouts.Count();
        Expire(id);
        ctx.Logs.Add(new Log { Text = "expired" });
        ctx.SaveChanges();
    }
    public void Audit(int id)
    {
        var payout = ctx.Payouts.Single(p => p.Id == id);
        ctx.Logs.Add(new Log { Text = payout.Status });
        ctx.SaveChanges();
    }
    public int Count(int id) => ctx.Payouts.First(p => p.Id == id).Lines.Count;
    public void ExpireOnly(int id)
    {
        Expire(id);
        ctx.SaveChanges();
    }
    public void Reload(int id)
    {
        Load(id);
        ctx.SaveChanges();
    }
    public int Close(int id)
    {
        var open = ctx.Payouts.Count();
        RejectAndSave(id);
        return open;
    }
    public void Note(string text) => ctx.Logs.Add(new Log { Text = text });
    public void NoteAndSave()
    {
        Note("saved");
        ctx.SaveChanges();
    }
    public void NoteOnly() => Note("staged");
}
"#;

const WRITES_TESTS: &str = r#"namespace Shop.Data.Tests;

public class PayoutsTests
{
    private readonly Payouts payouts = null;
    public void One() => payouts.Audit(1);
    public void Two() { payouts.Note("a"); payouts.Audit(2); }
}
"#;

#[test]
fn an_assignment_writes_the_table_only_where_the_change_is_saved() {
    let root = scratch("owned-writes");
    write(&root, "src/Data.cs", WRITES);
    write(&root, "tests/PayoutsTests.cs", WRITES_TESTS);
    let store = index(&root);
    let model = store.db_model().unwrap();
    let modes = |name: &str| -> Vec<(String, String)> {
        let t = model.tables.iter().find(|t| t.table == name).unwrap_or_else(|| panic!("no table {name}"));
        t.accessors.iter().map(|a| (a.symbol.clone(), a.mode.clone())).collect()
    };
    let pair = |s: &str, m: &str| (s.to_string(), m.to_string());
    // The owner's own assignment writes; an assignment through a query writes only where it is saved; an object
    // initializer only when the object is saved.
    assert_eq!(
        modes("payout_cancellation"),
        [
            pair("Payout.Cancel", "write"),
            pair("Payouts.RejectAndSave", "write"),
            pair("Payouts.Create", "write"),
            pair("Payouts.Reject", "access"),
            pair("Payouts.Draft", "access"),
            // Through `RejectAndSave`, which saves: listed for that write, its own mode `access`.
            pair("Payouts.Close", "access"),
        ]
    );
    // `Lines.Clear()` in the owner writes; a new payout's empty list adds no rows; reading `Count` only accesses it.
    assert_eq!(
        modes("payout_line"),
        [pair("Payout.Clear", "write"), pair("Payouts.Draft", "access"), pair("Payouts.Create", "access"), pair("Payouts.Count", "access")]
    );
    let payouts = modes("Payouts");
    let mode = |s: &str| payouts.iter().find(|(n, _)| n == s).map(|(_, m)| m.as_str()).unwrap_or_else(|| panic!("no {s}: {payouts:?}"));
    // Saved through a repository behind an interface, marked modified, saved after a helper changed it: read and
    // written.
    assert_eq!(mode("Payouts.Settle"), "read_write");
    assert_eq!(mode("Payouts.Mark"), "read_write");
    assert_eq!(mode("Payouts.ExpireAndLog"), "read_write");
    assert_eq!(mode("Payouts.RejectAndSave"), "read_write");
    assert_eq!(mode("Payouts.Create"), "write");
    // Changed and never saved here (hydration, or a helper leaving the save to its caller), or read beside a save of
    // other rows only: read.
    for s in ["Payouts.Load", "Payouts.Expire", "Payouts.Reject", "Payouts.Audit", "Payouts.Count"] {
        assert_eq!(mode(s), "read", "{s}");
    }
    let t = model.tables.iter().find(|t| t.table == "Payouts").unwrap();
    // `Close` reads here and writes through `RejectAndSave`, which saves: counted apart, in `writes_via`.
    assert_eq!(mode("Payouts.Close"), "read");
    assert_eq!((t.reads, t.writes, t.read_writes, t.other, t.writes_via), (10, 6, 4, 0, 1));
    let access = |s: &str| t.accessors.iter().find(|a| a.symbol == s).unwrap_or_else(|| panic!("no {s}"));
    let via = |s: &str| access(s).via.as_ref().map(|v| (v.symbol.as_str(), v.saved.as_str()));
    assert_eq!(via("Payouts.Close"), Some(("Payouts.RejectAndSave", "callee")));
    // A helper changing rows without saving them is written through its callers that save, even one not touching
    // the table itself (`ExpireOnly`); the helper is marked. `Load` returns what it read: hydration, no credit.
    assert_eq!(via("Payouts.ExpireAndLog"), Some(("Payouts.Expire", "here")));
    assert_eq!(mode("Payouts.ExpireOnly"), "write");
    assert_eq!(via("Payouts.ExpireOnly"), Some(("Payouts.Expire", "here")));
    assert!(access("Payouts.ExpireOnly").only_via && !access("Payouts.Close").only_via);
    assert!(access("Payouts.Expire").saved_by_caller && !access("Payouts.Load").saved_by_caller);
    assert!(payouts.iter().all(|(s, _)| s != "Payouts.Reload"), "{payouts:?}");
    assert_eq!(access("Payouts.Settle").via, None);

    // A helper only staging a row (`Add`, no save) writes it, and is saved by its callers: only those that save are
    // credited (`saved: here`), not counted in `writes`.
    let logs = model.tables.iter().find(|t| t.table == "Logs").unwrap();
    let log = |s: &str| logs.accessors.iter().find(|a| a.symbol == s);
    let audit = log("Payouts.Note").unwrap();
    assert!(audit.mode == "write" && audit.saved_by_caller, "{:#?}", logs.accessors);
    let saving = log("Payouts.NoteAndSave").unwrap();
    assert_eq!(saving.via.as_ref().map(|v| (v.symbol.as_str(), v.saved.as_str())), Some(("Payouts.Note", "here")));
    assert!(saving.only_via && saving.mode == "access");
    assert!(log("Payouts.NoteOnly").is_none());
    // A writer saving by itself is credited to every caller, `saved: callee`; test callers are counted, not listed,
    // once compacted, and counts don't change. `other` leaves out callers listed only for a callee's write.
    let one = log("PayoutsTests.One").unwrap();
    assert_eq!(one.via.as_ref().map(|v| (v.symbol.as_str(), v.saved.as_str())), Some(("Payouts.Audit", "callee")));
    assert_eq!(logs.other, 0);
    let mut compacted = logs.clone();
    kc_store::db::compact(&mut compacted, 5);
    assert!(compacted.accessors.iter().all(|a| !a.file.starts_with("tests/")), "{:?}", compacted.accessors);
    let hidden: usize = compacted.hidden_via.iter().map(|h| h.tests).sum();
    assert_eq!((hidden, compacted.writes_via), (2, logs.writes_via));
    // Through calls, a read here and a write there make `read_write`.
    let (touches, _) = kc_store::db::touched(store.conn(), &model, query::definitions(store.conn(), "Payouts.ExpireAndLog").unwrap().remove(0).id, 2).unwrap();
    assert!(touches.iter().any(|t| t.name == "Payouts" && t.mode == "read_write"), "{touches:?}");
}

#[test]
fn a_name_the_code_binds_hides_the_field_of_that_name() {
    let root = scratch("bare-shadowing");
    write(
        &root,
        "src/Basket.cs",
        r#"using System;
using System.Collections.Generic;
using System.Linq;

namespace Shop;

public class Basket
{
    private List<string> items = new();

    public void Lambda(List<List<string>> all) => all.ForEach(items => items.Clear());
    public void Typed(Action<List<string>> f) => f = (List<string> items) => items.Clear();
    public void Local() { var items = new List<string>(); items.Clear(); }
    public void Loop(List<List<string>> all) { foreach (var items in all) items.Clear(); }
    public void Using(Func<List<string>> make) { using var items = (IDisposable)make(); items.Dispose(); }
    public void Out(Dictionary<int, List<string>> d) { if (d.TryGetValue(1, out var items)) items.Clear(); }
    public void Pattern(object o) { if (o is List<string> items) items.Clear(); }
    public void Catch() { try { } catch (Exception items) { items.ToString(); } }
    public void LocalFunction() { void Clear(List<string> items) => items.Clear(); }
    public int Query(List<List<string>> all) => (from items in all select items.Count).Sum();
    public void Field() => items.Clear();
}
"#,
    );
    let store = index(&root);
    let conn = store.conn();
    // Only the method using no name of its own reads the field.
    let (hits, _) = query::refs(conn, "items", &["member"], 100).unwrap();
    let from: Vec<String> = hits.iter().filter(|h| !h.targets.is_empty()).map(|h| h.from.as_ref().unwrap().name.clone()).collect();
    assert_eq!(from, ["Field"]);
}
