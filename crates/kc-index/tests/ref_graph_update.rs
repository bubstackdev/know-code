//! The reference graph a sync's log lets `Store::ref_graph` update is the graph built afresh: after every edit of a
//! seeded random sequence (references swapped, lines duplicated, removed or added, files added, removed and put
//! back), the graph the store gives equals `rank::ref_graph` at the same generation. The edits are mostly inside
//! bodies, so most syncs take the update.
//!
//! `KNOWCODE_GRAPH_CORPUS=<dir of repos>` runs the same over a copy of each repo in it (ignored by default: slow).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A small repo in several languages whose files call each other: C# (with a delegate a factory invokes, and a
/// partial class), TypeScript, Python, Swift, Go and Rust.
fn fixture(root: &Path) {
    write(root, "src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");
    write(
        root,
        "src/Orders/Order.cs",
        "namespace Shop.Orders;\npublic partial class Order : Entity\n{\n    public int Total() { return Lines.Count; }\n    public Lines Lines { get; set; }\n}\n",
    );
    write(root, "src/Orders/Order.Audit.cs", "namespace Shop.Orders;\npublic partial class Order : IAudited\n{\n    public void Audit() { Log.Write(Total()); }\n}\n");
    write(root, "src/Orders/Lines.cs", "namespace Shop.Orders;\npublic class Lines\n{\n    public int Count;\n    public void Add(Order o) { Count++; o.Audit(); }\n}\n");
    write(root, "src/Common/Entity.cs", "namespace Shop;\npublic class Entity\n{\n    public int Id;\n    public void Save() { Log.Write(Id); }\n}\npublic interface IAudited { void Audit(); }\n");
    write(root, "src/Common/Log.cs", "namespace Shop;\npublic static class Log\n{\n    public static void Write(int x) { }\n    public static void Write(string s) { }\n}\n");
    write(
        root,
        "src/Orders/OrderService.cs",
        "namespace Shop.Orders;\nusing System;\npublic class OrderService\n{\n    private readonly Func<Order> _make;\n    public OrderService(Func<Order> make) { _make = make; }\n    public int Place()\n    {\n        var o = _make.Invoke();\n        o.Lines.Add(o);\n        o.Save();\n        return o.Total();\n    }\n    public void Audit(Order o) { o.Audit(); Log.Write(\"audit\"); }\n}\n",
    );
    write(root, "web/src/orders.service.ts", "export class OrdersService {\n  load(): number { return 1; }\n  save(n: number): void {}\n}\n");
    write(
        root,
        "web/src/orders.component.ts",
        "import { OrdersService } from './orders.service';\nimport { format } from './format';\n\nexport class OrdersComponent {\n  constructor(private orders: OrdersService) {}\n  run(): string {\n    this.orders.save(1);\n    const n = this.orders.load();\n    return format(n);\n  }\n}\n",
    );
    write(root, "web/src/format.ts", "export function format(n: number): string {\n  return String(n);\n}\n\nexport function pad(s: string): string {\n  return s.padStart(4);\n}\n");
    write(root, "tools/report/totals.py", "def total(rows):\n    return sum(rows)\n\n\ndef _clean(rows):\n    return [r for r in rows if r]\n");
    write(root, "tools/report/main.py", "from .totals import total, _clean\n\n\ndef run(rows):\n    clean = _clean(rows)\n    return total(clean) + total(rows)\n");
    write(root, "Package.swift", "// swift-tools-version:5.7\nimport PackageDescription\nlet package = Package(name: \"Kit\", targets: [.target(name: \"Kit\")])\n");
    write(
        root,
        "Sources/Kit/Buffer.swift",
        "public struct Buffer {\n    var bytes: [UInt8] = []\n    public mutating func write(_ b: UInt8) {\n        bytes.append(b)\n    }\n    public func count() -> Int {\n        return bytes.count\n    }\n}\n",
    );
    write(
        root,
        "Sources/Kit/Writer.swift",
        "public final class Writer {\n    var buffer = Buffer()\n    public func put(_ b: UInt8) {\n        buffer.write(b)\n        flushIfNeeded()\n    }\n    func flushIfNeeded() {\n        if buffer.count() > 8 { reset() }\n    }\n    func reset() {\n        buffer = Buffer()\n    }\n}\n",
    );
    write(root, "go.mod", "module example.com/kit\n\ngo 1.21\n");
    write(root, "kit/queue.go", "package kit\n\ntype Queue struct{ items []int }\n\nfunc (q *Queue) Push(x int) {\n\tq.items = append(q.items, x)\n}\n\nfunc (q *Queue) Len() int {\n\treturn len(q.items)\n}\n");
    write(root, "kit/drain.go", "package kit\n\nfunc Drain(q *Queue) int {\n\tn := q.Len()\n\tq.Push(n)\n\treturn Sum(n, q.Len())\n}\n\nfunc Sum(a, b int) int {\n\treturn a + b\n}\n");
    write(root, "Cargo.toml", "[package]\nname = \"kit\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    write(root, "src/lib.rs", "pub mod stack;\n\npub fn top(s: &stack::Stack) -> Option<i32> {\n    s.peek()\n}\n");
    write(
        root,
        "src/stack.rs",
        "pub struct Stack {\n    items: Vec<i32>,\n}\n\nimpl Stack {\n    pub fn peek(&self) -> Option<i32> {\n        self.items.last().copied()\n    }\n    pub fn push(&mut self, x: i32) {\n        self.items.push(x);\n        self.check();\n    }\n    fn check(&self) {\n        let _ = self.peek();\n    }\n}\n",
    );
}

/// A deterministic generator (xorshift): the edits follow from the seed alone.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn identifiers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.len() > 1 && w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_'))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// One edit to one source file under `root`: mostly inside bodies (indented lines), sometimes a file added, removed
/// or put back as it was. Returns what it did, for the failure message.
fn edit(root: &Path, files: &[String], originals: &BTreeMap<String, String>, rng: &mut Rng) -> String {
    let path = &files[rng.below(files.len())];
    let full = root.join(path);
    let roll = rng.below(100);
    if roll < 4 {
        let copy = format!("{}.copy{}", path.rsplit_once('.').map_or(path.as_str(), |(s, _)| s), path.rsplit_once('.').map_or("", |(_, e)| e));
        let copy = copy.replace(".copy", "_copy.");
        let _ = std::fs::copy(&full, root.join(&copy));
        return format!("copy {path} to {copy}");
    }
    if roll < 7 {
        let _ = std::fs::remove_file(&full);
        return format!("remove {path}");
    }
    if roll < 12 {
        write(root, path, &originals[path]);
        return format!("restore {path}");
    }
    let Ok(text) = std::fs::read_to_string(&full) else {
        write(root, path, &originals[path]);
        return format!("restore removed {path}");
    };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let body: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].starts_with("    ") || lines[i].starts_with('\t')).collect();
    let what = if body.is_empty() || roll < 20 {
        let at = rng.below(lines.len() + 1);
        lines.insert(at, String::new());
        format!("blank line at {at} of {path}")
    } else {
        let at = body[rng.below(body.len())];
        match rng.below(3) {
            0 => {
                let words = identifiers(&lines[at]);
                let pool = identifiers(&text);
                if words.is_empty() {
                    lines.insert(at, lines[at].clone());
                    format!("duplicate line {at} of {path}")
                } else {
                    let (from, to) = (&words[rng.below(words.len())], &pool[rng.below(pool.len())]);
                    lines[at] = lines[at].replacen(from.as_str(), to, 1);
                    format!("line {at} of {path}: {from} -> {to}")
                }
            }
            1 => {
                lines.insert(at, lines[at].clone());
                format!("duplicate line {at} of {path}")
            }
            _ => {
                lines.remove(at);
                format!("remove line {at} of {path}")
            }
        }
    };
    std::fs::write(&full, lines.join("\n") + "\n").unwrap();
    what
}

/// Runs `steps` seeded edits over the repo at `root`, checking after each sync that the graph the store gives is
/// the one built afresh. Returns how many syncs logged names (could update rather than rebuild).
fn check_updates(root: &Path, seed: u64, steps: usize, insert_reversed: bool) -> usize {
    let mut files: Vec<String> = Vec::new();
    let out = Command::new("git").args(["ls-files"]).current_dir(root).output().unwrap();
    for f in String::from_utf8(out.stdout).unwrap().lines() {
        if kc_extract::Lang::from_path(f).is_some() {
            files.push(f.to_string());
        }
    }
    let originals: BTreeMap<String, String> =
        files.iter().filter_map(|f| Some((f.clone(), std::fs::read_to_string(root.join(f)).ok()?))).collect();
    files.retain(|f| originals.contains_key(f));
    let mut store = Store::open(&root.join(".git").join("graph.sqlite")).unwrap();
    let opts = SyncOptions { threads: 1, insert_reversed, ..Default::default() };
    sync(root, &mut store, &opts).unwrap();
    store.ref_graph().unwrap();
    let json = |g: &kc_store::rank::RefGraph| serde_json::to_string(g).unwrap();
    let mut done = Vec::new();
    for step in 0..steps {
        done.push(edit(root, &files, &originals, &mut Rng(seed.wrapping_mul(1_000_003).wrapping_add(step as u64 + 1))));
        sync(root, &mut store, &opts).unwrap();
        let graph = store.ref_graph().unwrap();
        let fresh = kc_store::rank::ref_graph(store.conn()).unwrap();
        assert_eq!(json(&graph), json(&fresh), "seed {seed}, reversed {insert_reversed}, after: {done:#?}");
    }
    store.conn().query_row("SELECT COUNT(*) FROM graph_log WHERE names IS NOT NULL", [], |r| r.get::<_, i64>(0)).unwrap() as usize
}

#[test]
fn invariant_an_updated_reference_graph_is_the_graph_built_afresh() {
    for (seed, reversed) in [(1, false), (2, true), (3, false)] {
        let root = scratch(&format!("graph-update-{seed}"));
        git(&root, &["init", "-q"]);
        fixture(&root);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "fixture"]);
        let updated = check_updates(&root, seed, 60, reversed);
        // Most edits are inside bodies: the update, not a rebuild, must be what was checked.
        assert!(updated >= 20, "seed {seed}: only {updated} of 60 syncs could update the graph");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A context factory's `CreateDbContext()` makes the context: its hit is filed under the context's name, which the
/// file calling it need not name. An edit adding the call inside a body changes that name's edges, so the update
/// resolves the names factories file under (then and now) whatever the edited files reference.
#[test]
fn an_update_resolves_what_factories_make_wherever_they_are_called() {
    let root = scratch("graph-update-factory");
    git(&root, &["init", "-q"]);
    write(&root, "src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");
    write(&root, "src/Data/ShopContext.cs", "namespace Shop.Data;\npublic class ShopContext : DbContext\n{\n}\n");
    write(
        &root,
        "src/Reports/ReportsBase.cs",
        "namespace Shop.Reports;\nusing Shop.Data;\npublic class ReportsBase\n{\n    protected IDbContextFactory<ShopContext> _factory;\n}\n",
    );
    let body = |call: &str| format!("namespace Shop.Reports;\npublic class Reports : ReportsBase\n{{\n    public int Run()\n    {{\n        {call}\n        return 1;\n    }}\n}}\n");
    write(&root, "src/Reports/Reports.cs", &body("var n = 0;"));
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "fixture"]);
    let mut store = Store::open(&root.join(".git").join("graph.sqlite")).unwrap();
    let opts = SyncOptions { threads: 1, ..Default::default() };
    sync(&root, &mut store, &opts).unwrap();
    store.ref_graph().unwrap();
    let json = |g: &kc_store::rank::RefGraph| serde_json::to_string(g).unwrap();
    for call in ["var db = _factory.CreateDbContext();", "var n = 0;"] {
        write(&root, "src/Reports/Reports.cs", &body(call));
        sync(&root, &mut store, &opts).unwrap();
        let logged: Option<String> = store
            .conn()
            .query_row("SELECT names FROM graph_log WHERE gen = (SELECT value FROM meta WHERE key = 'tree_gen')", [], |r| r.get(0))
            .unwrap();
        assert!(logged.as_deref().is_some_and(|n| !n.contains("ShopContext")), "an update, without the context's name: {logged:?}");
        let graph = store.ref_graph().unwrap();
        let fresh = kc_store::rank::ref_graph(store.conn()).unwrap();
        let made = fresh.edges.iter().any(|(s, d, n)| s.ends_with("Reports.cs") && d.ends_with("ShopContext.cs") && n.iter().any(|u| u.name == "ShopContext"));
        assert_eq!(made, call.contains("CreateDbContext"), "the fixture's factory makes the context: {}", json(&fresh));
        assert_eq!(json(&graph), json(&fresh), "after {call}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "slow: set KNOWCODE_GRAPH_CORPUS to a directory of public repos (a copy of each is edited)"]
fn corpus_updated_reference_graphs_are_the_graphs_built_afresh() {
    let Some(corpus) = std::env::var_os("KNOWCODE_GRAPH_CORPUS") else { return };
    let steps: usize = std::env::var("KNOWCODE_GRAPH_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
    let mut repos: Vec<PathBuf> = std::fs::read_dir(&corpus).unwrap().map(|e| e.unwrap().path()).filter(|p| p.join(".git").exists()).collect();
    repos.sort();
    for (i, repo) in repos.iter().enumerate() {
        let root = scratch(&format!("graph-corpus-{i}"));
        let ok = Command::new("git").args(["clone", "-q", "--depth", "1", "--no-local"]).arg(repo).arg(&root).status().unwrap().success()
            || Command::new("cp").arg("-R").arg(format!("{}/.", repo.display())).arg(&root).status().unwrap().success();
        assert!(ok, "copy {}", repo.display());
        for reversed in [false, true] {
            let updated = check_updates(&root, 7 + i as u64, steps, reversed);
            eprintln!("{}: reversed {reversed}: {updated} syncs logged names", repo.display());
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
