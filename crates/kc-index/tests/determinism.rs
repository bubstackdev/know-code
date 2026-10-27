//! Answers don't depend on the order files were stored in: two indexes of one tree, one storing its files in reverse
//! order, resolve every reference alike and give the same areas and hotspots. And same-named aliases and nested types
//! (a delegate nested in a class, Swift typealiases nested in types) are scoped as the language scopes them, never
//! picked by row order.

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

fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn index(root: &Path, db: &str, insert_reversed: bool) -> Store {
    let mut store = Store::open(&root.join(".git").join(db)).unwrap();
    sync(root, &mut store, &SyncOptions { threads: 1, insert_reversed, ..Default::default() }).unwrap();
    store
}

/// Every reference in the index, resolved: site, confidence and targets, one line each, in a fixed order.
fn resolution(store: &Store) -> Vec<String> {
    let conn = store.conn();
    let names: Vec<String> =
        conn.prepare("SELECT DISTINCT name FROM refs ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    let mut out = Vec::new();
    for name in names {
        let (hits, _) = query::refs(conn, &name, &[], usize::MAX).unwrap();
        for h in hits {
            let targets: Vec<String> = h.targets.iter().map(|t| format!("{}@{}:{}", t.qualified, t.file, t.line)).collect();
            out.push(format!(
                "{}:{} {} {} {:?} {} [{}]",
                h.file,
                h.line,
                h.kind,
                h.name,
                h.receiver_type,
                h.confidence,
                targets.join(", ")
            ));
        }
    }
    out
}

/// A repo whose answers once depended on which of two same-named things was stored first.
fn fixture(root: &Path) {
    write(root, "src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");
    // Entities, and a repository and a factory each nesting a delegate named like the entity.
    write(root, "src/Data/Entities/Order.cs", "namespace Shop.Data.Entities;\npublic class Order\n{\n    public OrderOwner OrderOwner { get; set; }\n}\n");
    write(
        root,
        "src/Data/Entities/OrderOwner.cs",
        "using System;\nnamespace Shop.Data.Entities;\npublic class OrderOwner\n{\n    public DateTime UpdatedDate { get; set; }\n}\n",
    );
    write(root, "src/Data/Entities/Basket.cs", "namespace Shop.Data.Entities;\npublic class Basket\n{\n    public Order Order { get; set; }\n}\n");
    write(root, "src/Data/IOrderRepository.cs", "namespace Shop.Data;\npublic interface IOrderRepository\n{\n    void Save();\n}\n");
    write(
        root,
        "src/Data/OrderRepository.cs",
        r#"using System;
using System.Linq;
using Shop.Data.Entities;

namespace Shop.Data;

public class OrderRepository : IOrderRepository
{
    public delegate IOrderRepository Order(int? id);

    public void Save() { }

    public DateTime Owned(Basket basket)
    {
        return basket.Order.OrderOwner.UpdatedDate;
    }
}
"#,
    );
    write(
        root,
        "src/Service/SettingFactory.cs",
        r#"namespace Shop.Service;

public interface ICreateFactory<TIn, TOut> { TOut Create(TIn input); }

public class SettingFactory
{
    public delegate ICreateFactory<int, string> Order(int? id);
}
"#,
    );
    // Two namespace blocks in one file, each with its own `using` alias of one name.
    write(
        root,
        "src/Service/Blocks.cs",
        r#"namespace Shop.Service.Left
{
    using M = Shop.Data.Entities;

    public class LeftReader
    {
        public M.Order Read(M.Basket b) => b.Order;
    }
}

namespace Shop.Service.Right
{
    using M = Shop.Data;

    public class RightReader
    {
        public void Save(M.IOrderRepository repo) => repo.Save();
    }
}
"#,
    );
    // Swift typealiases of one name nested in two types: each applies inside its own type.
    write(root, "Sources/App/Item.swift", "struct Item {\n    var title: String\n}\nstruct Thing {\n    var label: String\n}\n");
    write(
        root,
        "Sources/App/Lists.swift",
        r#"struct ItemList {
    typealias Element = Item
    func first(_ e: Element) -> String {
        return e.title
    }
}

struct ThingList {
    typealias Element = Thing
    func first(_ e: Element) -> String {
        return e.label
    }
}
"#,
    );
    // A factory registration whose implementation is a factory method's declared return type, and a test double
    // beside the one real implementation.
    write(
        root,
        "src/Service/Startup.cs",
        "using Shop.Data;\nnamespace Shop.Service;\npublic class Startup\n{\n    static OrderRepository Make(int n) => new OrderRepository();\n    public void Add(IServiceCollection services) { services.AddScoped<IOrderRepository>(sp => Make(1)); }\n}\n",
    );
    write(root, "tests/Shop.Tests/FakeOrders.cs", "namespace Shop.Data;\npublic class FakeOrders : IOrderRepository\n{\n    public void Save() { }\n}\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "Add the shop"]);
}

/// Every DI registration, as the index resolved it.
fn registrations(store: &Store) -> Vec<String> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT f.path, d.line, d.service, d.implementation FROM di_bindings d JOIN files f ON f.id = d.file_id
             ORDER BY f.path, d.line, d.service, d.implementation",
        )
        .unwrap();
    let rows = stmt.query_map([], |r| {
        Ok(format!("{}:{} {:?} {:?}", r.get::<_, String>(0)?, r.get::<_, u32>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?))
    });
    rows.unwrap().map(|r| r.unwrap()).collect()
}

#[test]
fn invariant_answers_do_not_depend_on_the_order_files_were_stored_in() {
    let root = scratch("determinism");
    fixture(&root);
    let forward = index(&root, "forward.sqlite", false);
    let reversed = index(&root, "reversed.sqlite", true);

    // Stored in opposite orders: the same files get different row ids.
    let first = |s: &Store| -> String { s.conn().query_row("SELECT path FROM files ORDER BY id LIMIT 1", [], |r| r.get(0)).unwrap() };
    assert_ne!(first(&forward), first(&reversed));

    let (a, b) = (resolution(&forward), resolution(&reversed));
    assert!(!a.is_empty());
    assert_eq!(a, b);

    let regs = registrations(&forward);
    assert_eq!(regs, ["src/Service/Startup.cs:6 Some(\"IOrderRepository\") Some(\"OrderRepository\")"]);
    assert_eq!(regs, registrations(&reversed));
    let save = |s: &Store| {
        let ctx = query::context(s.conn(), &root, "OrderRepository.Save", &query::ContextLimits::from_budget(2000)).unwrap().unwrap();
        ctx.callers.iter().map(|h| format!("{}:{} {} {:?}", h.file, h.line, h.confidence, h.through)).collect::<Vec<_>>()
    };
    assert_eq!(save(&forward), save(&reversed));

    let areas = |s: &Store| serde_json::to_string(&kc_index::areas::areas(&root, s).unwrap()).unwrap();
    assert_eq!(areas(&forward), areas(&reversed));
    let hotspots = |s: &Store| serde_json::to_string(&kc_index::vitals::hotspots(&root, s).unwrap()).unwrap();
    assert_eq!(hotspots(&forward), hotspots(&reversed));
}

#[test]
fn nested_delegates_and_aliases_apply_only_where_the_language_says() {
    let root = scratch("nested-aliases");
    fixture(&root);
    for reversed in [false, true] {
        let store = index(&root, if reversed { "reversed.sqlite" } else { "forward.sqlite" }, reversed);
        let lines = resolution(&store);
        let at = |site: &str, kind: &str, name: &str| -> String {
            let found = lines.iter().find(|l| {
                let mut parts = l.split(' ');
                parts.next().is_some_and(|s| s.starts_with(site)) && parts.next() == Some(kind) && parts.next() == Some(name)
            });
            found.cloned().unwrap_or_else(|| panic!("no {kind} {name} at {site}"))
        };
        // `basket.Order` is the entity: a delegate named `Order` nested in a class is that class's, and is a type of
        // its own rather than an alias of what it returns.
        let owner = at("src/Data/OrderRepository.cs:15", "member", "OrderOwner");
        assert!(owner.contains(" exact [Order.OrderOwner@src/Data/Entities/Order.cs:4]"), "{owner}");
        let updated = at("src/Data/OrderRepository.cs:15", "member", "UpdatedDate");
        assert!(updated.contains(" exact [OrderOwner.UpdatedDate@src/Data/Entities/OrderOwner.cs:5]"), "{updated}");
        // A `using` alias declared in a namespace block applies in that block only.
        let order = at("src/Service/Blocks.cs:7", "type_ref", "Order");
        assert!(order.ends_with(" [Order@src/Data/Entities/Order.cs:2]") && !order.contains("unresolved"), "{order}");
        let member = at("src/Service/Blocks.cs:7", "member", "Order");
        assert!(member.contains(" exact [Basket.Order@src/Data/Entities/Basket.cs:4]"), "{member}");
        let repo = at("src/Service/Blocks.cs:17", "type_ref", "IOrderRepository");
        assert!(repo.ends_with(" [IOrderRepository@src/Data/IOrderRepository.cs:2]") && !repo.contains("unresolved"), "{repo}");
        let save = at("src/Service/Blocks.cs:17", "call", "Save");
        assert!(save.contains(" exact [IOrderRepository.Save@src/Data/IOrderRepository.cs:4]"), "{save}");
        // Each nested Swift typealias inside its own type.
        let title = at("Sources/App/Lists.swift:4", "member", "title");
        assert!(title.contains(" exact [Item.title@Sources/App/Item.swift:2]"), "{title}");
        let label = at("Sources/App/Lists.swift:11", "member", "label");
        assert!(label.contains(" exact [Thing.label@Sources/App/Item.swift:5]"), "{label}");
    }
}

/// The reference graph as it was built before names shared what resolution reads about the repo, before threads and
/// chunks: every name's references resolved on their own (`query::refs`), one name after another.
fn ref_graph_one_name_at_a_time(store: &Store) -> kc_store::rank::RefGraph {
    use std::collections::{BTreeMap, HashMap, HashSet};
    let conn = store.conn();
    let files: Vec<String> = conn
        .prepare("SELECT path FROM files WHERE lang NOT IN ('swiftpm', 'xcodegen', 'cargo', 'hostconfig') ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let common: HashSet<String> = conn
        .prepare("SELECT lower(name) FROM symbols GROUP BY name HAVING COUNT(DISTINCT file_id) > 5")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let names: Vec<String> = conn.prepare("SELECT DISTINCT name FROM refs").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    let mut target: HashMap<i64, (String, bool)> = HashMap::new();
    let mut edges: BTreeMap<(String, String), BTreeMap<String, kc_store::rank::NameUse>> = BTreeMap::new();
    for name in names {
        let (hits, _) = query::refs(conn, &name, &[], usize::MAX).unwrap();
        let mut counted = HashSet::new();
        for h in hits {
            let exact = match h.confidence {
                "exact" => true,
                "unique_name" => false,
                _ => continue,
            };
            for &id in &h.target_ids {
                let (dst, private) = target.entry(id).or_insert_with(|| {
                    let (path, sig): (String, String) = conn
                        .query_row("SELECT f.path, s.signature FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
                        .unwrap_or_default();
                    let private = sig.split_whitespace().any(|w| w == "private");
                    (path, private)
                });
                if dst.is_empty() || *dst == h.file || !counted.insert((h.file.clone(), h.line, h.from.as_ref().map(|f| f.id), h.kind.clone(), id)) {
                    continue;
                }
                let n = edges.entry((h.file.clone(), dst.clone())).or_default().entry(name.clone()).or_insert_with(|| kc_store::rank::NameUse {
                    name: name.clone(),
                    exact: 0,
                    unique: 0,
                    private: false,
                    common: common.contains(&name.to_ascii_lowercase()),
                });
                if exact {
                    n.exact += 1;
                } else {
                    n.unique += 1;
                }
                n.private |= *private || name.starts_with('_');
            }
        }
    }
    kc_store::rank::RefGraph { files, edges: edges.into_iter().map(|((s, d), names)| (s, d, names.into_values().collect())).collect() }
}

#[test]
fn invariant_the_reference_graph_is_the_same_however_it_is_built() {
    let root = scratch("ref-graph");
    fixture(&root);
    // TypeScript and Python imports too, and a name used often enough to resolve over several chunks.
    write(&root, "web/src/app/orders.service.ts", "export class OrdersService {\n  load(): number { return 1; }\n  save(): void {}\n}\n");
    write(
        &root,
        "web/src/app/orders.component.ts",
        "import { OrdersService } from './orders.service';\n\nexport class OrdersComponent {\n  constructor(private orders: OrdersService) {}\n  run(): number {\n    this.orders.save();\n    this.orders.save();\n    return this.orders.load();\n  }\n}\n",
    );
    write(&root, "tools/report/totals.py", "def total(rows):\n    return sum(rows)\n\n\ndef _clean(rows):\n    return [r for r in rows if r]\n");
    write(&root, "tools/report/main.py", "from .totals import total, _clean\n\n\ndef run(rows):\n    return total(_clean(rows)) + total(rows) + total([])\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "Add the web app and the report"]);
    let store = index(&root, "graph.sqlite", false);

    let json = |g: &kc_store::rank::RefGraph| serde_json::to_string(g).unwrap();
    let before = json(&ref_graph_one_name_at_a_time(&store));
    assert!(before.contains("orders.service.ts") && before.contains("totals.py") && before.contains("OrderRepository.cs"), "{before}");
    for threads in [1, 3] {
        for chunk in [1, 2, 2048] {
            let graph = kc_store::rank::with_workers(kc_store::rank::Workers { threads, init: None }, || {
                kc_store::rank::ref_graph_chunked(store.conn(), chunk).unwrap()
            });
            assert_eq!(json(&graph), before, "{threads} threads, {chunk} references at a time");
        }
    }
}

/// The code a method's `touches_tables` names for a table it reaches two ways at one hop is the first by file and
/// line, whichever the index stored first.
#[test]
fn invariant_a_touched_tables_way_does_not_depend_on_the_order_files_were_stored_in() {
    let root = scratch("determinism-touched");
    write(&root, "src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");
    write(
        &root,
        "src/Data/ShopContext.cs",
        "namespace Shop.Data;\npublic class Order { }\npublic class ShopContext : DbContext\n{\n    public DbSet<Order> Orders { get; set; }\n}\n",
    );
    for q in ["AQuery", "BQuery"] {
        write(
            &root,
            &format!("src/Queries/{q}.cs"),
            &format!("namespace Shop.Queries;\npublic class {q}\n{{\n    private Shop.Data.ShopContext _ctx;\n    public int Run() => _ctx.Orders.Count();\n}}\n"),
        );
    }
    write(
        &root,
        "src/Service.cs",
        "namespace Shop;\npublic class Service\n{\n    private Shop.Queries.BQuery _b;\n    private Shop.Queries.AQuery _a;\n    public void Go()\n    {\n        _b.Run();\n        _a.Run();\n    }\n}\n",
    );
    let touches = |store: &Store| -> Vec<String> {
        let model = store.db_model().unwrap();
        let go = query::definitions(store.conn(), "Service.Go").unwrap().remove(0).id;
        let (touches, _) = kc_store::db::touched(store.conn(), &model, go, 2).unwrap();
        touches.iter().map(|t| format!("{} {} {} hop {} via {} at {}", t.kind, t.name, t.mode, t.hop, t.via, t.site)).collect()
    };
    let forward = touches(&index(&root, "forward.sqlite", false));
    assert_eq!(forward, ["table Orders read hop 1 via AQuery.Run at src/Queries/AQuery.cs:5"]);
    assert_eq!(touches(&index(&root, "reversed.sqlite", true)), forward);
    let _ = std::fs::remove_dir_all(&root);
}
