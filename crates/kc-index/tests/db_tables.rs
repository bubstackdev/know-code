//! Entity Framework table access end to end on a scratch C# repo: tables per DbContext, who reads and writes each,
//! and which tables a service method reaches through an interface.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::{Store, db, query};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-db-{name}-{}", std::process::id()));
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

const CONTEXT: &str = r#"
public class ShopContext : DbContext
{
    public DbSet<Order> Orders { get; set; }
    public DbSet<Customer> Customers { get; set; }
    public DbSet<AuditEntry> AuditEntries { get; set; }

    protected override void OnModelCreating(ModelBuilder mb)
    {
        mb.Entity<Order>(e => { e.ToTable("shop_order", "sales"); });
    }
}

[Table("customer")]
public class Customer { public int Id { get; set; } public string Name { get; set; } }

public class Order { public int Id { get; set; } }

public class AuditEntry { }
"#;

const REPO: &str = r#"
public interface IOrderRepository
{
    Task<Order> Find(int id);
}

public class OrderRepository : IOrderRepository
{
    private readonly ShopContext _db;

    public OrderRepository(ShopContext db) { _db = db; }

    public async Task<Order> Find(int id) => await _db.Orders.FirstOrDefaultAsync(o => o.Id == id);

    public async Task Save(Order o)
    {
        _db.Orders.Add(o);
        await _db.SaveChangesAsync();
    }

    public void Rename(int id)
    {
        var c = _db.Customers.Find(id);
        c.Name = "x";
        _db.SaveChanges();
    }

    public void Log() { _db.Set<AuditEntry>().Add(new AuditEntry()); }

    public IQueryable<Order> All() { return _db.Orders; }

    public void Purge() { _db.Database.ExecuteSqlRaw("EXEC usp_purge"); }

    public List<int> Ids() => (from o in _db.Orders select o.Id).ToList();
}

public class OrderPage { public List<Order> Orders { get; set; } }

public class Printer
{
    // `page`'s type is unknown here, and two types have an `Orders`: not a table access.
    public int Count(dynamic page) => page.Orders.Count;
}
"#;

const SERVICE: &str = r#"
public class OrderService
{
    private readonly IOrderRepository _repo;

    public OrderService(IOrderRepository repo) { _repo = repo; }

    public async Task<Order> Get(int id)
    {
        var o = await GetOrThrow(id);
        return o;
    }

    private async Task<Order> GetOrThrow(int id)
    {
        var o = await _repo.Find(id);
        if (o == null) throw new InvalidOperationException();
        return o;
    }
}
"#;

#[test]
fn tables_accessors_and_what_a_method_touches() {
    let root = scratch("tables");
    write(&root, "src/Data/ShopContext.cs", CONTEXT);
    write(&root, "src/Data/OrderRepository.cs", REPO);
    write(&root, "src/Services/OrderService.cs", SERVICE);
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 2, full_discovery: true, ..Default::default() }).unwrap();

    let model = store.db_model().unwrap();
    let tables: Vec<(&str, &str, &str, Option<&str>, &str)> = model
        .tables
        .iter()
        .map(|t| (t.context.as_deref().unwrap_or(""), t.table.as_str(), t.entity.as_str(), t.dbset.as_deref(), t.mapped_by.as_str()))
        .collect();
    assert_eq!(
        tables,
        [
            ("ShopContext", "AuditEntries", "AuditEntry", Some("AuditEntries"), "convention"),
            ("ShopContext", "customer", "Customer", Some("Customers"), "attribute"),
            ("ShopContext", "sales.shop_order", "Order", Some("Orders"), "to_table"),
        ]
    );
    let accessors = |table: &str| -> Vec<(String, String)> {
        model.named(table)[0].accessors.iter().map(|a| (a.symbol.clone(), a.mode.clone())).collect()
    };
    let pairs = |v: &[(&str, &str)]| -> Vec<(String, String)> { v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() };
    assert_eq!(
        accessors("shop_order"),
        pairs(&[
            ("OrderRepository.Save", "write"),
            ("OrderRepository.Find", "read"),
            ("OrderRepository.Ids", "read"),
            ("OrderRepository.All", "access"),
        ])
    );
    // Read through Find, then saved: a read and a write.
    assert_eq!(accessors("customer"), pairs(&[("OrderRepository.Rename", "read_write")]));
    // `Set<AuditEntry>()` reaches the set without its property.
    assert_eq!(accessors("AuditEntry"), pairs(&[("OrderRepository.Log", "write")]));
    assert_eq!(model.procedures.iter().map(|p| (p.name.as_str(), p.symbol.as_str())).collect::<Vec<_>>(), [("usp_purge", "OrderRepository.Purge")]);

    // The service reaches the order table through the repository interface's implementation.
    let get = query::definitions(store.conn(), "OrderService.Get").unwrap().remove(0);
    let touches = db::for_symbol(store.conn(), &model, &get, 10).unwrap().touches;
    let t: Vec<(&str, &str, u8, &str)> = touches.iter().map(|t| (t.name.as_str(), t.mode.as_str(), t.hop, t.via.as_str())).collect();
    assert_eq!(t, [("sales.shop_order", "read", 2, "OrderRepository.Find")]);

    // The entity class lists its table and the code touching it.
    let order = query::definitions(store.conn(), "Order").unwrap().into_iter().find(|d| d.kind == "class").unwrap();
    let for_order = db::for_symbol(store.conn(), &model, &order, 10).unwrap();
    assert_eq!(for_order.tables.len(), 1);
    assert_eq!(for_order.tables[0].writes, 1);
    // The context lists its tables without accessors.
    let ctx = query::definitions(store.conn(), "ShopContext").unwrap().remove(0);
    let for_ctx = db::for_symbol(store.conn(), &model, &ctx, 10).unwrap();
    assert_eq!(for_ctx.tables.len(), 3);
    assert!(for_ctx.tables.iter().all(|t| t.accessors.is_empty()));
    let _ = std::fs::remove_dir_all(&root);
}

const NAV_CONTEXT: &str = r#"
public class StoreContext : DbContext
{
    public DbSet<Cart> Carts { get; set; }
    public DbSet<CartLine> CartLines { get; set; }
    public DbSet<Coupon> Coupons { get; set; }

    protected override void OnModelCreating(ModelBuilder mb)
    {
        mb.Entity<Cart>().ToTable("cart");
        mb.Entity<CartLine>().ToTable("cart_line");
        mb.Entity<Coupon>().ToTable("coupon");
        mb.Entity<Cart>().Navigation(c => c.Coupon).AutoInclude();
    }
}

public class Startup
{
    public void ConfigureServices(IServiceCollection s) =>
        s.AddDbContext<StoreContext>(o => o.UseMySql(AppSettings.StoreDbConnection, v));
}

public class Cart { public int Id { get; set; } public List<CartLine> Lines { get; set; } public Coupon Coupon { get; set; } }
public class CartLine { public int Id { get; set; } public int Qty { get; set; } }
public class Coupon { public int Id { get; set; } }
"#;

const NAV_REPO: &str = r#"
public class CartRepository
{
    private readonly StoreContext _db;
    private readonly IDbConnection _conn;
    private const string HistorySql = "SELECT id, total FROM audit_history WHERE cart_id = @id";

    public Cart Load(int id) => _db.Carts.Include(c => c.Lines).First(c => c.Id == id);

    public void AddLine(int id, CartLine line)
    {
        var cart = _db.Carts.Find(id);
        cart.Lines.Add(line);
        _db.SaveChanges();
    }

    // Changes the lines; the caller saves.
    public void Empty(Cart cart) { cart.Lines.Clear(); }

    public int Total(Cart cart) => cart.Lines.Sum(l => l.Qty);

    // A query on carts naming their lines joins the line table.
    public int Big() => _db.Carts.Count(c => c.Lines.Any(l => l.Qty > 9));

    public Task Prune() => _db.CartLines.Where(l => l.Qty == 0).DeleteAsync();

    public IEnumerable<decimal> History(int id) => _conn.Query<decimal>(HistorySql, new { id });

    public void Archive(int id)
    {
        _conn.Execute($"REPLACE INTO archive.audit_history (cart_id) SELECT id FROM cart WHERE id = {id}");
        _conn.Execute("DELETE FROM cart_line WHERE cart_id = @id", new { id });
    }
}
"#;

#[test]
fn navigations_auto_includes_and_sql_text() {
    let root = scratch("navigations");
    write(&root, "src/Data/StoreContext.cs", NAV_CONTEXT);
    write(&root, "src/Data/CartRepository.cs", NAV_REPO);
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 2, full_discovery: true, ..Default::default() }).unwrap();
    let model = store.db_model().unwrap();
    let accessors = |table: &str| -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> =
            model.named(table).iter().flat_map(|t| &t.accessors).map(|a| (a.symbol.clone(), a.mode.clone())).collect();
        v.sort();
        v
    };
    let pairs = |v: &[(&str, &str)]| -> Vec<(String, String)> { v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() };
    // `Include(c => c.Lines)` reads the lines; a change to the collection, saved, writes them; one left to the
    // caller doesn't; summing a loaded collection is no access. Raw SQL deletes from it.
    assert_eq!(
        accessors("cart_line"),
        pairs(&[
            ("CartRepository.AddLine", "write"),
            ("CartRepository.Archive", "write"),
            ("CartRepository.Big", "read"),
            ("CartRepository.Empty", "access"),
            ("CartRepository.Load", "read"),
            ("CartRepository.Prune", "write"),
        ])
    );
    // The coupon is auto-included: every reader of the cart reads it.
    assert_eq!(
        accessors("coupon"),
        pairs(&[("CartRepository.AddLine", "read"), ("CartRepository.Big", "read"), ("CartRepository.Load", "read")])
    );
    assert_eq!(
        accessors("cart"),
        pairs(&[
            ("CartRepository.AddLine", "read_write"),
            ("CartRepository.Archive", "read"),
            ("CartRepository.Big", "read"),
            ("CartRepository.Load", "read"),
        ])
    );
    // SQL in a constant is run by the method using it; `archive.audit_history` is another database's table.
    assert_eq!(accessors("audit_history"), pairs(&[("CartRepository.Archive", "write"), ("CartRepository.History", "read")]));
    let names: Vec<&str> = model.named("audit_history").iter().map(|t| t.table.as_str()).collect();
    assert_eq!(names, ["archive.audit_history", "audit_history"]);
    // The context's connection setting.
    assert_eq!(model.named("cart")[0].connection.as_deref(), Some("StoreDbConnection"));
    assert_eq!(db::connection_database("StoreDbConnection").as_deref(), Some("store"));
    let _ = std::fs::remove_dir_all(&root);
}

const MIXED_CONTEXT: &str = r#"
public class Basket { public int Id { get; set; } public List<BasketLine> Lines { get; set; } }
public class BasketLine { public int Id { get; set; } }
public class Ticket { public int Id { get; set; } }

public class TicketContext : DbContext
{
    public DbSet<Basket> Baskets { get; set; }
    public DbSet<BasketLine> BasketLines { get; set; }
    public DbSet<Ticket> Tickets { get; set; }
}

public class Startup
{
    public void ConfigureServices(IServiceCollection s) =>
        s.AddDbContext<TicketContext>(o => o.UseMySql(AppSettings.TicketConnection, v));
}
"#;

const MIXED_REPO: &str = r#"
public class BasketService
{
    private readonly TicketContext _db;
    private readonly HttpClient _http;
    private readonly IDbConnection _conn;
    public const string ByIdSql = "SELECT id FROM audit_history WHERE id = @id";

    // Staged changes to the lines, left to the caller: deleting a file or an HTTP resource saves nothing.
    public void StagedAndFile(Basket b) { b.Lines.Clear(); System.IO.File.Delete("/tmp/x"); }
    public async Task StagedAndHttp(Basket b) { b.Lines.Clear(); await _http.DeleteAsync("https://example.test/x"); }
    // A set-based delete on a query writes at once, and is credited to its callers.
    public void PruneTickets() => _db.Tickets.Where(t => t.Id == 0).Delete();

    public int Count() => _conn.Query<int>(ByIdSql).First();

    // Another database whose name starts like the connection's: not the context's tables.
    public void Archive() => _conn.Execute("DELETE FROM tickets_archive.tickets WHERE id = 1");
    public void Purge() => _conn.Execute("DELETE FROM ticket.tickets WHERE id = 1");
}

public class Nightly
{
    private readonly BasketService _svc;
    public void Run() => _svc.PruneTickets();
}
"#;

const MIXED_REPORT: &str = r#"
public class AuditReport
{
    private readonly IDbConnection _conn;
    public int Run() => _conn.Query<int>(BasketService.ByIdSql).First();
}
"#;

#[test]
fn saves_databases_and_held_sql_across_files() {
    let root = scratch("mixed");
    write(&root, "src/Data/TicketContext.cs", MIXED_CONTEXT);
    write(&root, "src/Data/BasketService.cs", MIXED_REPO);
    write(&root, "src/Reports/AuditReport.cs", MIXED_REPORT);
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 2, full_discovery: true, ..Default::default() }).unwrap();
    let model = store.db_model().unwrap();
    let accessors = |table: &str| -> Vec<(String, String, Option<String>)> {
        let mut v: Vec<(String, String, Option<String>)> = model
            .named(table)
            .iter()
            .filter(|t| t.table.eq_ignore_ascii_case(table))
            .flat_map(|t| &t.accessors)
            .map(|a| (a.symbol.clone(), a.mode.clone(), a.via.as_ref().map(|v| v.saved.clone())))
            .collect();
        v.sort();
        v
    };
    let row = |s: &str, m: &str, v: Option<&str>| (s.to_string(), m.to_string(), v.map(str::to_string));
    // `File.Delete` and `_http.DeleteAsync` don't save the staged change to the lines.
    assert_eq!(
        accessors("BasketLines"),
        [row("BasketService.StagedAndFile", "access", None), row("BasketService.StagedAndHttp", "access", None)]
    );
    // Z.EntityFramework's `Delete` on a query of the set writes at once: its caller is credited, saved by the callee.
    let tickets = accessors("Tickets");
    assert!(tickets.contains(&row("BasketService.PruneTickets", "write", None)), "{tickets:?}");
    assert!(tickets.iter().any(|r| r.0 == "Nightly.Run" && r.2.as_deref() == Some("callee")), "{tickets:?}");
    // `ticket.tickets` is the context's table; `tickets_archive.tickets` is another database's.
    assert!(tickets.contains(&row("BasketService.Purge", "write", None)), "{tickets:?}");
    assert!(!tickets.iter().any(|r| r.0 == "BasketService.Archive"), "{tickets:?}");
    assert_eq!(accessors("tickets_archive.tickets"), [row("BasketService.Archive", "write", None)]);
    assert!(!db::same_database("ticket", "tickets_archive"));
    assert!(db::same_database("orderexportapi", "order_export"));
    assert!(db::same_database("shop", "shop_db") && !db::same_database("shop", "shopping"));
    assert!(db::same_database("shopapplication", "shop") && db::same_database("shopreplica", "shop"));
    assert!(!db::same_database("shopprice", "shop") && !db::same_database("shop", "shop_archive"));
    // SQL in a constant is run by its users in its own file and in others, never by the constant itself.
    assert_eq!(accessors("audit_history"), [row("AuditReport.Run", "read", None), row("BasketService.Count", "read", None)]);
    let _ = std::fs::remove_dir_all(&root);
}
