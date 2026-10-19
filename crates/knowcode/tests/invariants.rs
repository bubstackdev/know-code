//! Invariants of whole answers, through the binary as agents and the harness run it (`--json`):
//!
//! - the order an index stored its files in (and how many parser threads wrote it) never changes any command's
//!   answer;
//! - every input an answer depends on besides the source (ignore files, project files, tsconfig, manifests,
//!   `.mailmap`, `.knowcode-privacy.yml`, and the user's config: `bot_authors`, `former_authors`, `[ignore]`,
//!   `[privacy] block_at`, `workspace.roots`) changes the
//!   answer when it is edited, and the answer then equals a cold index's;
//! - a constructor call resolves to exactly one overload or is marked ambiguous;
//! - a deeper requested depth never returns fewer nodes, and a capped walk always says so;
//! - every walk frontier is either explored or reported.
//!
//! Every invariant test in the workspace is named `invariant_…`: `cargo test --workspace -- invariant` runs them all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-inv-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Runs git in `dir` as `author` (`Name <email>`), at a fixed time so history answers don't move between runs.
fn git(dir: &Path, author: &str, args: &[&str]) {
    let (name, email) = author.split_once(" <").unwrap();
    let email = email.trim_end_matches('>');
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", &format!("user.name={name}")])
        .args(["-c", &format!("user.email={email}")])
        .args(args)
        .env("GIT_AUTHOR_DATE", "2026-09-01T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-09-01T12:00:00Z")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} in {}", dir.display());
}

/// Where a run keeps its index and reads its config.
struct Env {
    cache: PathBuf,
    config: PathBuf,
    reversed: bool,
}

impl Env {
    fn new(dir: &Path, config: &Path) -> Env {
        Env { cache: dir.to_path_buf(), config: config.to_path_buf(), reversed: false }
    }

    /// Runs knowcode from `cwd`; returns (exit code, stdout, stderr).
    fn run(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let mut cmd = Command::new(KC);
        cmd.args(args)
            .current_dir(cwd)
            .env("KNOWCODE_CACHE_DIR", &self.cache)
            .env("KNOWCODE_CONFIG_DIR", &self.config)
            .env("KNOWCODE_DATA_DIR", self.cache.join("_data"))
            .env("KNOWCODE_NOW", "head")
            .env_remove("KNOWCODE_VERIFY")
            .env_remove("KNOWCODE_INSERT_REVERSED");
        if self.reversed {
            cmd.env("KNOWCODE_INSERT_REVERSED", "1");
        }
        let out = cmd.output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn index(&self, cwd: &Path, args: &[&str]) {
        let (code, _, err) = self.run(cwd, &[&["index"], args].concat());
        assert_eq!(code, 0, "index {args:?}: {err}");
    }

    /// The answer to `args` with `--json`, normalised (see [`normalize`]); a command without a JSON form answers
    /// `{"text": …}`. Exit 2 is an answer too (`rename` refusing).
    fn answer(&self, cwd: &Path, args: &[&str], text: bool) -> Value {
        let mut argv: Vec<&str> = args.to_vec();
        if !text {
            argv.push("--json");
        }
        let (code, out, err) = self.run(cwd, &argv);
        assert!(code == 0 || code == 2, "{args:?} exited {code}: {err}");
        let v = if text { Value::String(out) } else { serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}")) };
        normalize(v, &self.cache)
    }
}

/// Drops what changes between two runs without the answer changing (build info, freshness, timings, row ids, index
/// generations and sizes) and writes the cache directory as `<cache>`.
fn normalize(v: Value, cache: &Path) -> Value {
    let cache = cache.to_string_lossy();
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(k, v)| {
                    let volatile = matches!(k.as_str(), "knowcode" | "freshness" | "stats" | "elapsed" | "duration" | "generation" | "ms" | "bytes")
                        || k.ends_with("_ms")
                        || ((k == "id" || k.ends_with("_id")) && v.is_number());
                    !volatile
                })
                .map(|(k, v)| (k, normalize(v, Path::new(&*cache))))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(|v| normalize(v, Path::new(&*cache))).collect()),
        Value::String(s) => Value::String(s.replace(&*cache, "<cache>")),
        v => v,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Determinism: every command, two indexes stored in opposite orders.

/// A repo using most of what the index models: C# (a controller with a route, DI, a DbContext, a test project), an
/// Angular service and component calling the route, Python, SQL, a manifest, and three commits by two authors.
fn determinism_fixture(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    git(root, "Ann Writer <ann@example.test>", &["init", "-q"]);
    let files: &[(&str, &str)] = &[
        ("src/Api/Api.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"),
        (
            "src/Api/Order.cs",
            "namespace Shop.Api;\npublic class Order\n{\n    public int Id { get; set; }\n    public decimal Total { get; set; }\n}\n",
        ),
        (
            "src/Api/IOrderRepository.cs",
            "namespace Shop.Api;\npublic interface IOrderRepository\n{\n    void Save(Order order);\n    Order Find(int id);\n}\n",
        ),
        (
            "src/Api/OrderRepository.cs",
            r#"using Microsoft.EntityFrameworkCore;
namespace Shop.Api;
public class ShopContext : DbContext
{
    public DbSet<Order> Orders { get; set; }
}
public class OrderRepository : IOrderRepository
{
    private readonly ShopContext _db;
    public OrderRepository(ShopContext db) { _db = db; }
    public void Save(Order order) { _db.Orders.Add(order); _db.SaveChanges(); }
    public Order Find(int id) => _db.Orders.Find(id);
}
"#,
        ),
        (
            "src/Api/OrderService.cs",
            r#"namespace Shop.Api;
public class OrderService
{
    private readonly IOrderRepository _repo;
    public OrderService(IOrderRepository repo) { _repo = repo; }
    public Order Place(decimal total)
    {
        var order = new Order { Total = total };
        if (total > 100) { order.Total = Discount(total); }
        _repo.Save(order);
        return order;
    }
    private decimal Discount(decimal total) => total * 0.9m;
    public void Unreferenced() { }
}
"#,
        ),
        (
            "src/Api/OrdersController.cs",
            r#"using Microsoft.AspNetCore.Mvc;
namespace Shop.Api;
[ApiController]
[Route("api/orders")]
public class OrdersController : ControllerBase
{
    private readonly OrderService _orders;
    public OrdersController(OrderService orders) { _orders = orders; }
    [HttpPost]
    public Order Post(decimal total) => _orders.Place(total);
    [HttpGet("{id}")]
    public Order Get(int id, [FromServices] IOrderRepository repo) => repo.Find(id);
}
"#,
        ),
        (
            "src/Api/Startup.cs",
            "using Microsoft.Extensions.DependencyInjection;\nnamespace Shop.Api;\npublic class Startup\n{\n    public void ConfigureServices(IServiceCollection services)\n    {\n        services.AddScoped<IOrderRepository, OrderRepository>();\n        services.AddScoped<OrderService>();\n    }\n}\n",
        ),
        ("tests/Api.Tests/Api.Tests.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup><ProjectReference Include=\"..\\..\\src\\Api\\Api.csproj\" /></ItemGroup></Project>\n"),
        (
            "tests/Api.Tests/OrderServiceTests.cs",
            "using Shop.Api;\nnamespace Shop.Api.Tests;\npublic class OrderServiceTests\n{\n    public void PlacesAnOrder() { new OrderService(null).Place(5); }\n}\n",
        ),
        ("web/package.json", "{ \"name\": \"shop-web\", \"dependencies\": { \"@angular/core\": \"^17.0.0\" } }\n"),
        ("web/tsconfig.json", "{ \"compilerOptions\": { \"baseUrl\": \"src\" } }\n"),
        (
            "web/src/app/order.service.ts",
            "import { Injectable } from '@angular/core';\nimport { HttpClient } from '@angular/common/http';\n@Injectable({ providedIn: 'root' })\nexport class OrderClient {\n  constructor(private http: HttpClient) {}\n  place(total: number) { return this.http.post('/api/orders', { total }); }\n}\n",
        ),
        (
            "web/src/app/cart.component.ts",
            "import { Component } from '@angular/core';\nimport { OrderClient } from './order.service';\n@Component({ selector: 'app-cart', template: '<button (click)=\"checkout()\">Buy</button>' })\nexport class CartComponent {\n  constructor(private orders: OrderClient) {}\n  checkout() { this.orders.place(10); }\n}\n",
        ),
        ("tools/report.py", "def order_total(rows):\n    return sum(r['total'] for r in rows)\n\ndef main():\n    print(order_total([]))\n"),
        ("db/schema.sql", "CREATE TABLE Orders (Id INT PRIMARY KEY, Total DECIMAL(10,2));\n"),
    ];
    for (path, text) in files {
        write(&root.join(path), text);
    }
    git(root, "Ann Writer <ann@example.test>", &["add", "-A"]);
    git(root, "Ann Writer <ann@example.test>", &["commit", "-qm", "Add the shop"]);
    write(&root.join("src/Api/OrderService.cs"), &std::fs::read_to_string(root.join("src/Api/OrderService.cs")).unwrap().replace("0.9m", "0.85m"));
    write(&root.join("tests/Api.Tests/OrderServiceTests.cs"), "using Shop.Api;\nnamespace Shop.Api.Tests;\npublic class OrderServiceTests\n{\n    public void PlacesAnOrder() { new OrderService(null).Place(5); }\n    public void Discounts() { new OrderService(null).Place(500); }\n}\n");
    git(root, "Bo Fixer <bo@example.test>", &["commit", "-qam", "Fix the discount"]);
    write(&root.join("src/Api/OrderRepository.cs"), &std::fs::read_to_string(root.join("src/Api/OrderRepository.cs")).unwrap().replace("_db.SaveChanges();", "_db.SaveChanges(); /* saved */"));
    write(&root.join("src/Api/OrderService.cs"), &std::fs::read_to_string(root.join("src/Api/OrderService.cs")).unwrap().replace("return order;", "return order; // placed"));
    git(root, "Ann Writer <ann@example.test>", &["commit", "-qam", "Note the saves"]);
}

/// One question per command: (args, whether the command only prints text). Every command the CLI lists answers here.
const QUESTIONS: &[(&[&str], bool)] = &[
    (&["at", "src/Api/OrderService.cs:9"], false),
    (&["find", "Order"], false),
    (&["locate", "order total discount"], false),
    (&["refs", "Save"], false),
    (&["context", "OrderService.Place"], false),
    (&["trace", "OrderService.Place", "--depth", "3"], false),
    (&["path", "CartComponent.checkout", "OrderRepository.Save"], false),
    (&["flow", "CartComponent.checkout"], false),
    (&["impact", "OrderRepository.Save"], false),
    (&["impact", "--file", "src/Api/OrderService.cs"], false),
    (&["impact", "--diff", "HEAD~2"], false),
    (&["unused"], false),
    (&["rename", "OrderRepository.Save", "Store"], false),
    (&["coupled", "src/Api/OrderService.cs"], false),
    (&["coupled", "src/Api/OrderService.cs", "--as-of", "HEAD~1"], false),
    (&["tests", "src/Api/OrderService.cs"], false),
    (&["tests", "--diff", "HEAD~2"], false),
    (&["hotspots"], false),
    (&["hotspots", "src/Api/OrderService.cs"], false),
    (&["ownership"], false),
    (&["reviewers", "src/Api/OrderService.cs"], false),
    (&["reviewers", "--diff", "HEAD~2"], false),
    (&["overview"], false),
    (&["areas"], false),
    (&["note", "list"], false),
    (&["docs"], true),
    (&["search", "Order"], false),
    (&["grep-ast", "new $X($$$)", "-l", "cs"], false),
    (&["deps"], false),
    (&["routes"], false),
    (&["tables"], false),
    (&["events"], false),
    (&["status"], false),
];

/// Commands that answer no question about code: they index, serve, or manage the cache and config. `more` continues
/// another answer's list from its cursor (see the cursor invariants in `cursor.rs`).
const NOT_QUESTIONS: &[&str] = &["index", "forget", "prune", "serve", "purge", "config", "workspace", "setup", "uninstall", "self-update", "help", "more"];

/// The commands `knowcode --help` lists.
fn listed_commands() -> BTreeSet<String> {
    let out = Command::new(KC).arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout).into_owned();
    let section = help.split("Commands:").nth(1).expect("a Commands section").split("\n\n").next().unwrap().to_string();
    section.lines().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()
}

#[test]
fn invariant_index_order_never_changes_any_answer() {
    let base = scratch("order");
    let root = base.join("shop");
    determinism_fixture(&root);
    std::fs::create_dir_all(base.join("config")).unwrap();

    // Every command the CLI lists has a question here, so a new command can't skip this test.
    let asked: BTreeSet<String> = QUESTIONS.iter().map(|(a, _)| a[0].to_string()).collect();
    let missing: Vec<String> = listed_commands().into_iter().filter(|c| !asked.contains(c) && !NOT_QUESTIONS.contains(&c.as_str())).collect();
    assert!(missing.is_empty(), "commands with no determinism question: {missing:?}");

    let forward = Env::new(&base.join("forward"), &base.join("config"));
    let mut reversed = Env::new(&base.join("reversed"), &base.join("config"));
    reversed.reversed = true;
    forward.index(&root, &["--threads", "1"]);
    reversed.index(&root, &["--threads", "4"]);
    let mut answered = 0;
    for (args, text) in QUESTIONS {
        let a = forward.answer(&root, &[*args, &["--no-sync"]].concat(), *text);
        let b = reversed.answer(&root, &[*args, &["--no-sync"]].concat(), *text);
        assert_eq!(a, b, "{args:?} differs between the two indexes");
        answered += 1;
    }
    assert_eq!(answered, QUESTIONS.len());
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------------------------------------------
// Inputs: editing anything an answer depends on changes the answer, as a cold index would give it.

/// One input an answer depends on. Paths are relative to the case's directory, whose `config/` is the user config
/// directory and whose repos are the directories named in `repos`.
struct Input {
    /// What is edited.
    name: &'static str,
    /// Git repos to create, each with one commit of the `files` under it.
    repos: &'static [&'static str],
    /// Files committed, by (author, path, text).
    files: &'static [(&'static str, &'static str, &'static str)],
    /// Files written but not committed (what `.gitignore` can leave out).
    untracked: &'static [(&'static str, &'static str)],
    /// The question, asked from `cwd` (relative to the case's directory). `{dir}` expands to the case's directory.
    cwd: &'static str,
    question: &'static [&'static str],
    /// The edit: files written.
    edit: &'static [(&'static str, &'static str)],
    /// Text in the answer before the edit and not after (`gone`), or after and not before.
    marker: &'static str,
    gone: bool,
}

const ANN: &str = "Ann Writer <ann@example.test>";
const ROBO: &str = "Cy Porter <cy@example.test>";
const OLD_ANN: &str = "Ann Writer <ann@old.example.test>";

const SAVE_TS: &str = "export function save() {}\n";
const MAIN_TS: &str = "import { save } from '@x/save';\nexport function run() { save(); }\n";
const WORKER_CS: &str = "namespace Same; public class Worker { public void Run() {} }\n";
const CALLER_CS: &str = "namespace Same; public class Caller { public void Start() { new Worker().Run(); } }\n";
const EMPTY_PROJECT: &str = "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n";
const REFERENCING_PROJECT: &str =
    "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup><ProjectReference Include=\"..\\a\\A.csproj\" /></ItemGroup></Project>\n";

const INPUTS: &[Input] = &[
    Input {
        name: ".gitignore",
        repos: &["r1"],
        files: &[(ANN, "r1/app.py", "def kept():\n    return 1\n")],
        untracked: &[("r1/gen/out.py", "def generated_helper():\n    return 2\n")],
        cwd: "r1",
        question: &["find", "generated_helper"],
        edit: &[("r1/.gitignore", "gen/\n")],
        marker: "gen/out.py",
        gone: true,
    },
    Input {
        name: ".git/info/exclude",
        repos: &["r1"],
        files: &[(ANN, "r1/app.py", "def kept():\n    return 1\n")],
        untracked: &[("r1/gen/out.py", "def generated_helper():\n    return 2\n")],
        cwd: "r1",
        question: &["find", "generated_helper"],
        edit: &[("r1/.git/info/exclude", "gen/\n")],
        marker: "gen/out.py",
        gone: true,
    },
    Input {
        name: ".knowcodeignore",
        repos: &["r1"],
        files: &[(ANN, "r1/app.py", "def kept():\n    return 1\n"), (ANN, "r1/legacy/lib.py", "def legacy_helper():\n    return 2\n")],
        untracked: &[],
        cwd: "r1",
        question: &["find", "legacy_helper"],
        edit: &[("r1/.knowcodeignore", "legacy/\n")],
        marker: "legacy/lib.py",
        gone: true,
    },
    Input {
        name: "config.toml [ignore]",
        repos: &["r1"],
        files: &[(ANN, "r1/app.py", "def kept():\n    return 1\n"), (ANN, "r1/legacy/lib.py", "def legacy_helper():\n    return 2\n")],
        untracked: &[],
        cwd: "r1",
        question: &["find", "legacy_helper"],
        edit: &[("config/config.toml", "[ignore]\n\"*\" = [\"legacy/\"]\n")],
        marker: "legacy/lib.py",
        gone: true,
    },
    Input {
        name: "tsconfig.json",
        repos: &["r1"],
        files: &[
            (ANN, "r1/tsconfig.json", r#"{ "compilerOptions": { "paths": { "@x/*": ["./one/*"] } } }"#),
            (ANN, "r1/one/save.ts", SAVE_TS),
            (ANN, "r1/two/save.ts", SAVE_TS),
            (ANN, "r1/main.ts", MAIN_TS),
        ],
        untracked: &[],
        cwd: "r1",
        question: &["refs", "save", "-k", "call"],
        edit: &[("r1/tsconfig.json", r#"{ "compilerOptions": { "paths": { "@x/*": ["./two/*"] } } }"#)],
        marker: "two/save.ts",
        gone: false,
    },
    Input {
        name: "a .csproj",
        repos: &["r1"],
        files: &[(ANN, "r1/a/A.csproj", EMPTY_PROJECT), (ANN, "r1/b/B.csproj", EMPTY_PROJECT), (ANN, "r1/a/A.cs", WORKER_CS), (ANN, "r1/b/B.cs", CALLER_CS)],
        untracked: &[],
        cwd: "r1",
        question: &["refs", "Run", "-k", "call"],
        edit: &[("r1/b/B.csproj", REFERENCING_PROJECT)],
        marker: "a/A.cs",
        gone: false,
    },
    Input {
        name: "package.json",
        repos: &["r1"],
        files: &[(ANN, "r1/package.json", "{ \"name\": \"shop\", \"dependencies\": {} }\n"), (ANN, "r1/index.ts", "export const x = 1;\n")],
        untracked: &[],
        cwd: "r1",
        question: &["deps"],
        edit: &[("r1/package.json", "{ \"name\": \"shop\", \"dependencies\": { \"left-padder\": \"^1.0.0\" } }\n")],
        marker: "left-padder",
        gone: false,
    },
    Input {
        name: "bot_authors",
        repos: &["r1"],
        files: &[(ANN, "r1/app.py", "def kept():\n    return 1\n"), (ROBO, "r1/build.py", "def built():\n    return 1\n")],
        untracked: &[],
        cwd: "r1",
        question: &["ownership"],
        edit: &[("config/bot_authors", "cy@example.test\n")],
        marker: "cy@example.test",
        gone: true,
    },
    Input {
        name: "former_authors",
        repos: &["r1"],
        files: &[(ANN, "r1/app.py", "def kept():\n    return 1\n"), (ROBO, "r1/build.py", "def built():\n    return 1\n")],
        untracked: &[],
        cwd: "r1",
        question: &["ownership"],
        edit: &[("config/former_authors", "ann@example.test\n")],
        marker: "\"former\"",
        gone: false,
    },
    Input {
        name: ".mailmap",
        repos: &["r1"],
        files: &[(ANN, "r1/app.py", "def kept():\n    return 1\n"), (OLD_ANN, "r1/old.py", "def older():\n    return 1\n")],
        untracked: &[],
        cwd: "r1",
        question: &["ownership"],
        edit: &[("r1/.mailmap", "Ann Writer <ann@example.test> <ann@old.example.test>\n")],
        marker: "ann@old.example.test",
        gone: true,
    },
    Input {
        name: ".knowcode-privacy.yml",
        repos: &["r1"],
        files: &[(ANN, "r1/app.py", "def kept():\n    return 1\n"), (ANN, "r1/vault/lib.py", "def vault_helper():\n    return 2\n")],
        untracked: &[],
        cwd: "r1",
        question: &["find", "vault_helper"],
        edit: &[("r1/.knowcode-privacy.yml", "rules:\n  - path: /vault/\n    level: 1\n")],
        marker: "vault/lib.py",
        gone: true,
    },
    Input {
        name: "config.toml [privacy] block_at",
        repos: &["r1"],
        files: &[
            (ANN, "r1/app.py", "def kept():\n    return 1\n"),
            (ANN, "r1/conf/lib.py", "def conf_helper():\n    return 2\n"),
            (ANN, "r1/.knowcode-privacy.yml", "rules:\n  - path: conf/\n    level: 2\n"),
        ],
        untracked: &[],
        cwd: "r1",
        question: &["find", "conf_helper"],
        edit: &[("config/config.toml", "[privacy]\nblock_at = 2\n")],
        marker: "conf/lib.py",
        gone: true,
    },
    Input {
        name: "config.toml workspace.roots",
        repos: &["near/r1", "far/r2"],
        files: &[(ANN, "near/r1/a.py", "def near_helper():\n    return 1\n"), (ANN, "far/r2/b.py", "def far_helper():\n    return 1\n")],
        untracked: &[],
        cwd: ".",
        question: &["find", "far_helper"],
        edit: &[("config/config.toml", "[workspace]\nroots = [\"{dir}/near\"]\n")],
        marker: "far_helper",
        gone: true,
    },
];

/// Builds a case's repos in `dir` and indexes them into `env`.
fn build(input: &Input, dir: &Path, env: &Env) {
    for repo in input.repos {
        let root = dir.join(repo);
        std::fs::create_dir_all(&root).unwrap();
        git(&root, ANN, &["init", "-q"]);
        for (author, path, text) in input.files.iter().filter(|f| f.1.starts_with(&format!("{repo}/"))) {
            write(&dir.join(path), text);
            git(&root, author, &["add", "-A"]);
            git(&root, author, &["commit", "-qm", &format!("Add {path}")]);
        }
    }
    for (path, text) in input.untracked {
        write(&dir.join(path), text);
    }
    for repo in input.repos {
        env.index(dir, &[dir.join(repo).to_str().unwrap()]);
    }
}

fn ask(input: &Input, dir: &Path, env: &Env) -> String {
    serde_json::to_string_pretty(&env.answer(&dir.join(input.cwd), input.question, false)).unwrap()
}

#[test]
fn invariant_every_input_an_answer_depends_on_changes_it() {
    for input in INPUTS {
        let dir = scratch(&format!("input-{}", input.name.replace(|c: char| !c.is_ascii_alphanumeric(), "_")));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let warm = Env::new(&dir.join("cache"), &dir.join("config"));
        build(input, &dir, &warm);
        let before = ask(input, &dir, &warm);
        assert_eq!(before.contains(input.marker), input.gone, "{}: before the edit, `{}` in\n{before}", input.name, input.marker);

        for (path, text) in input.edit {
            write(&dir.join(path), &text.replace("{dir}", dir.to_str().unwrap()));
        }
        // The next question syncs the index it already has.
        let after = ask(input, &dir, &warm);
        assert_ne!(before, after, "{}: the edit didn't change the answer", input.name);
        assert_eq!(after.contains(input.marker), !input.gone, "{}: after the edit, `{}` in\n{after}", input.name, input.marker);

        // And it is the answer a cold index gives.
        let cold = Env::new(&dir.join("cold"), &dir.join("config"));
        for repo in input.repos {
            cold.index(&dir, &[dir.join(repo).to_str().unwrap()]);
        }
        let fresh = ask(input, &dir, &cold);
        assert_eq!(after, fresh, "{}: the synced answer isn't a cold index's", input.name);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The answer contract: every answer says whether it is complete, why not, and which repos it read.

/// Every way an answer says it cut a list: `truncated` anywhere, `capped`, a `<list>_total` beyond its list, a
/// `<list>_truncated` flag, `callers_summary.total` beyond `callers`; lists named in `sampled` (a summary's samples by
/// design) and the map within its budget aside, and `areas` filtered to the one asked for.
fn says_cut(v: &Value, sampled: &[&str]) -> bool {
    match v {
        Value::Array(a) => a.iter().any(|x| says_cut(x, sampled)),
        Value::Object(m) => {
            let len = |k: &str| m.get(k).and_then(Value::as_array).map(|a| a.len() as u64);
            let counts = |l: &str| !sampled.contains(&l) && !(l == "areas" && m.contains_key("modularity"));
            m.iter().any(|(k, x)| {
                (k == "capped" && x == &Value::Bool(true))
                    || (k == "truncated" && x == &Value::Bool(true) && !m.contains_key("omitted_files"))
                    || k.strip_suffix("_truncated").is_some_and(|l| counts(l) && x == &Value::Bool(true))
                    || k.strip_suffix("_total").is_some_and(|l| counts(l) && x.as_u64().zip(len(l)).is_some_and(|(t, n)| t > n))
                    || (k == "callers_summary" && x["total"].as_u64().zip(len("callers")).is_some_and(|(t, n)| t > n))
            }) || m.iter().filter(|(k, _)| !matches!(k.as_str(), "freshness" | "knowcode" | "provenance" | "incomplete")).any(|(_, x)| says_cut(x, sampled))
        }
        _ => false,
    }
}

/// The contract on one envelope: `complete` is `incomplete` being empty, each entry has a reason from the closed set
/// and a detail, a list said cut makes it incomplete, no dummy `cursor`, and the answered repos are in `provenance`.
fn contract(args: &[&str], env: &Value) {
    let complete = env["complete"].as_bool().unwrap_or_else(|| panic!("{args:?}: no `complete`: {env}"));
    let list = env["incomplete"].as_array().cloned().unwrap_or_default();
    assert_eq!(complete, list.is_empty(), "{args:?}: {env}");
    assert!(env.get("incomplete").is_none() || !list.is_empty(), "{args:?}: an empty `incomplete`");
    for i in &list {
        let r = i["reason"].as_str().unwrap_or("");
        assert!(["capped", "budget", "deadline", "stale", "failed", "missing_repo", "unsupported", "policy", "withheld", "depth"].contains(&r), "{args:?}: reason {r}");
        assert!(i["detail"].as_str().is_some_and(|d| !d.is_empty()), "{args:?}: {i}");
    }
    let sampled: Vec<&str> = env["sampled"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    // Only a summary's samples by design are sampled.
    for s in &sampled {
        let allowed = ["entry_points", "key_types", "top_hotspots", "key_files", "exposed", "depends_on", "contributors", "map", "symbols", "areas", "tables", "structural"];
        assert!(allowed.contains(s), "{args:?}: {s} sampled");
    }
    if says_cut(env, &sampled) {
        assert!(!complete, "{args:?}: a list is cut but the answer says it is complete: {env}");
    }
    assert!(env.get("cursor").is_none_or(Value::is_string), "{args:?}: a dummy cursor");
    assert!(env.get("_resume").is_none(), "{args:?}: what cursors are made from is output");
    // A list a limit cut says how to get the rest.
    for i in list.iter().filter(|i| i["reason"] == "capped") {
        assert!(i["hint"].as_str().is_some_and(|h| !h.is_empty()), "{args:?}: a capped list without a hint: {i}");
    }
    if args[0] == "coupled" {
        guesses_are_labelled(args, env);
    }
    if let Some(repo) = env["repo"].as_str().filter(|r| *r != "workspace") {
        let listed = env["provenance"].as_array().is_some_and(|p| p.iter().any(|p| p["repo"] == repo));
        // `status` reports on the index, not an answer drawn from it.
        assert!(listed || args[0] == "status", "{args:?}: {repo} not in provenance: {env}");
    }
}

/// A `coupled` answer never presents a structural guess as history: every partner is `basis: history` with its
/// shared commits; every guess is `basis: structure`, `confidence: low`, without co-change numbers, each relation
/// with its own evidence, and comes with a note that history had nothing to rank; no file is both.
fn guesses_are_labelled(args: &[&str], env: &Value) {
    for item in env["items"].as_array().into_iter().flatten() {
        let partners = item["partners"].as_array().cloned().unwrap_or_default();
        for p in &partners {
            assert_eq!(p["basis"], "history", "{args:?}: {p}");
            assert!(p["support"].as_u64().is_some_and(|n| n >= 1), "{args:?}: {p}");
        }
        let Some(guesses) = item["structural"].as_array() else { continue };
        assert!(item["note"].as_str().is_some_and(|n| n.starts_with("history had nothing")), "{args:?}: {item}");
        for g in guesses {
            assert_eq!((g["basis"].as_str(), g["confidence"].as_str()), (Some("structure"), Some("low")), "{args:?}: {g}");
            assert!(["support", "commits", "weight", "static_link"].iter().all(|k| g.get(*k).is_none()), "{args:?}: {g}");
            assert!(g["relations"].as_array().is_some_and(|r| !r.is_empty() && r.iter().all(|r| r["evidence"]["mechanism"].is_string())), "{args:?}: {g}");
            assert!(!partners.iter().any(|p| p["file"] == g["file"]), "{args:?}: {g} is a partner too");
        }
    }
}

#[test]
fn invariant_every_answer_says_whether_it_is_complete() {
    let base = scratch("contract");
    let root = base.join("shop");
    determinism_fixture(&root);
    std::fs::create_dir_all(base.join("config")).unwrap();
    let env = Env::new(&base.join("cache"), &base.join("config"));
    env.index(&root, &["--threads", "1"]);
    let (mut checked, mut cut) = (0, 0);
    for (args, text) in QUESTIONS.iter().filter(|(_, text)| !text) {
        // As asked, and with the smallest limit, where the command takes one: then lists are cut.
        let tight: Vec<&str> = [*args, &["-n", "1"]].concat();
        for argv in [args.to_vec(), tight] {
            let (code, out, err) = env.run(&root, &[&argv[..], &["--json"]].concat());
            if code != 0 && argv.contains(&"-n") {
                continue; // no `-n` for this command
            }
            assert!(code == 0 || code == 2, "{argv:?} exited {code}: {err}");
            let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{argv:?}: {e}: {out}"));
            match &v {
                // A workspace `status`: one report per repo.
                Value::Array(list) => list.iter().for_each(|e| contract(&argv, e)),
                e => contract(&argv, e),
            }
            checked += 1;
            cut += usize::from(v["complete"] == Value::Bool(false));
            let _ = text;
        }
    }
    assert!(checked > QUESTIONS.len(), "{checked}");
    // The tight limits cut lists, and those answers said so.
    assert!(cut >= 5, "{cut} incomplete answers");
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------------------------------------------
// Walks: a constructor call reaches one overload or is ambiguous; depth and the walk's frontier are honest.

/// A C# repo of constructor overloads (declared, optional, `params`, chained, primary, a struct's implicit one), a
/// call chain deeper than the walks go by default, a fan-out, an ambiguous edge with callers behind it, and calls
/// through a base interface.
fn walks_fixture(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    git(root, ANN, &["init", "-q"]);
    write(&root.join("App.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n");
    write(
        &root.join("src/Errors.cs"),
        "namespace App;\n\npublic class Resp { }\n\npublic class Err : System.Exception\n{\n    public Err(Resp r) { Code(); }\n    \
         public Err(int status, string reason, string msg = null) { }\n    public Err(string reason, params object[] details) { }\n    \
         public Err() : this(500, \"x\") { }\n    public static string Code() => \"\";\n}\n\npublic class Sub : Err\n{\n    \
         public Sub(Resp r) : base(r) { }\n    public Sub(int s) : base(s, \"sub\") { }\n}\n\npublic struct Point\n{\n    \
         public Point(int x, int y) { }\n}\n\npublic class Holder(int id, string name)\n{\n    public Holder(int id) : this(id, \"\") { }\n}\n",
    );
    write(
        &root.join("src/Creators.cs"),
        "namespace App;\n\npublic class A { public void M() { throw new Err(400, \"x\", \"y\"); } }\n\
         public class B { public void N() { throw new Err(new Resp()); } }\n\
         public class C { public void Named() { var e = new Err(status: 1, reason: \"r\"); } }\n\
         public class D { public void Params() { var e = new Err(\"r\", 1, 2, 3); } }\n\
         public class E { public Err Target() { Err e = new(new Resp()); return e; } }\n\
         public class F { public void Unknown(object o) { var e = new Err(o); } }\n\
         public class G\n{\n    public void Make()\n    {\n        var p = new Point();\n        var q = new Point(1, 2);\n        \
         var h = new Holder(1, \"a\");\n        var k = new Holder(2);\n        var s = new Sub(new Resp()) { };\n        var t = new Sub(3);\n    }\n}\n\
         public class H { public void Bare() { var e = new Err(); var f = new Err { }; } }\n",
    );
    let mut chain = String::from("namespace App;\n\npublic class Chain\n{\n    public void M0() { }\n");
    for i in 1..=19 {
        chain.push_str(&format!("    public void M{i}() {{ M{}(); }}\n", i - 1));
    }
    chain.push_str("    public void Fan1() { M3(); }\n    public void Fan2() { M3(); Fan1(); }\n}\n");
    write(&root.join("src/Chain.cs"), &chain);
    write(
        &root.join("src/Billing.cs"),
        "namespace App;\n\npublic class Billing\n{\n    public void Run() { }\n}\npublic class Shipping { public void Run() { } }\n\
         public class Use\n{\n    public void A() { var b = Lib.Make(); b.Run(); }\n    public void B() { A(); }\n}\n\
         public class Use2 { Use u; public void C() { u.B(); } }\n",
    );
    write(
        &root.join("src/Dispatch.cs"),
        "namespace App;\n\npublic interface IBase { string Get(string f); }\npublic interface IDerived : IBase { }\n\
         public class Impl : IDerived { public string Get(string f) => \"\"; }\npublic class Other : IBase { public string Get(string f) => \"\"; }\n\
         public class Caller { IBase b; public void Run() { b.Get(\"x\"); } }\npublic class Caller2 { IDerived d; public void Run2() { d.Get(\"y\"); } }\n\
         public class Top { Caller c; public void Go() { c.Run(); } }\n",
    );
    git(root, ANN, &["add", "-A"]);
    git(root, ANN, &["commit", "-qm", "Add the walks fixture"]);
}

fn walks_env(name: &str) -> (PathBuf, PathBuf, Env) {
    let base = scratch(name);
    let root = base.join("app");
    walks_fixture(&root);
    std::fs::create_dir_all(base.join("config")).unwrap();
    let env = Env::new(&base.join("cache"), &base.join("config"));
    env.index(&root, &["--threads", "1"]);
    (base, root, env)
}

/// The `file:line` sites of an answer's rows (`affected` of an impact, `nodes` of a trace past the root), with each
/// row's confidence.
fn rows_of(v: &Value) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for item in v["items"].as_array().into_iter().flatten() {
        for a in item["affected"].as_array().into_iter().flatten().chain(item["nodes"].as_array().into_iter().flatten().skip(1)) {
            let at = format!("{}:{}", a["file"].as_str().unwrap_or(""), a["line"]);
            out.push((at, a["site"].as_str().unwrap_or("").to_string(), a["confidence"].as_str().unwrap_or("").to_string()));
        }
    }
    out
}

#[test]
fn invariant_a_constructor_call_resolves_to_exactly_one_overload_or_is_marked_ambiguous() {
    let (base, root, env) = walks_env("ctors");
    // Every constructor the fixture declares, by `file:line`.
    let mut ctors = Vec::new();
    for file in ["src/Errors.cs"] {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        for (i, l) in text.lines().enumerate() {
            let t = l.trim_start();
            if ["public Err(", "public Sub(", "public Point(", "public Holder("].iter().any(|p| t.starts_with(p)) {
                ctors.push(format!("{file}:{}", i + 1));
            }
        }
    }
    assert_eq!(ctors.len(), 8, "{ctors:?}");
    // Each creation or chained call site, to the constructors listing it and how.
    let mut by_site: std::collections::BTreeMap<String, Vec<(String, String)>> = Default::default();
    for c in &ctors {
        let v = env.answer(&root, &["trace", c, "-d", "in", "--depth", "1"], false);
        assert_eq!(v["items"][0]["nodes"][0]["kind"], "init", "{c}: {v}");
        for (_, site, confidence) in rows_of(&v) {
            by_site.entry(site).or_default().push((c.clone(), confidence));
        }
    }
    assert!(by_site.len() >= 12, "{by_site:?}");
    for (site, listed) in &by_site {
        let confident: Vec<&(String, String)> = listed.iter().filter(|(_, c)| c != "ambiguous").collect();
        // One overload, confidently; or every overload listing it says it is ambiguous.
        assert!(confident.is_empty() || (confident.len() == 1 && listed.len() == 1), "{site}: {listed:?}");
    }
    // Some sites are ambiguous (`new Err(o)` with an `object`), and the rest are told apart.
    assert!(by_site.values().any(|l| l.len() > 1 && l.iter().all(|(_, c)| c == "ambiguous")), "{by_site:?}");
    assert!(by_site.values().filter(|l| l.len() == 1 && l[0].1 != "ambiguous").count() >= 10, "{by_site:?}");
    let _ = std::fs::remove_dir_all(&base);
}

/// The ids of the rows an answer lists, as `file:line`.
fn listed(v: &Value) -> BTreeSet<String> {
    rows_of(v).into_iter().map(|(at, _, _)| at).collect()
}

/// Whether the answer says it stopped short: an `incomplete` entry naming depth, the frontier, or ambiguous edges.
fn says_stopped(v: &Value) -> bool {
    v["incomplete"].as_array().is_some_and(|l| l.iter().any(|i| i["detail"].as_str().is_some_and(|d| d.contains("frontier") || d.contains("depth"))))
}

#[test]
fn invariant_deeper_requested_depth_never_returns_fewer_nodes_and_a_capped_walk_always_says_so() {
    let (base, root, env) = walks_env("depth");
    for (cmd, symbol, extra) in [
        ("impact", "Chain.M0", &["--order", "hop"][..]),
        ("impact", "Billing.Run", &[][..]),
        ("impact", "Impl.Get", &[][..]),
        ("trace", "Chain.M0", &["-d", "in"][..]),
        ("trace", "Chain.M3", &["-d", "both"][..]),
    ] {
        let mut before: Option<(u8, BTreeSet<String>, bool)> = None;
        for depth in 1..=20u8 {
            let d = depth.to_string();
            let args: Vec<&str> = [&[cmd, symbol, "--depth", d.as_str()][..], extra].concat();
            let v = env.answer(&root, &args, false);
            let rows = listed(&v);
            let stopped = says_stopped(&v);
            if let Some((was, prev, prev_stopped)) = &before {
                // Never fewer: what a shallower walk lists, a deeper one lists too.
                assert!(prev.is_subset(&rows), "{cmd} {symbol}: depth {depth} lists less than depth {was}: {prev:?} vs {rows:?}");
                // A walk that a deeper one goes past said where it stopped.
                if rows.len() > prev.len() {
                    assert!(prev_stopped, "{cmd} {symbol}: depth {was} stopped short without saying so; depth {depth} lists more");
                }
            }
            // Asked deeper than the walk goes: the answer says how deep it went.
            let max: u64 = if cmd == "impact" { 16 } else { 3 };
            if u64::from(depth) > max {
                assert_eq!(v["depth"]["walked"].as_u64(), Some(max), "{cmd} {symbol} --depth {depth}: {v}");
            }
            before = Some((depth, rows, stopped));
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn invariant_every_walk_frontier_is_either_explored_or_reported() {
    let (base, root, env) = walks_env("frontier");
    for (symbol, depth, evidence) in [
        ("Chain.M0", "3", "default"),
        ("Chain.M0", "1", "default"),
        ("Billing.Run", "2", "default"),
        ("Billing.Run", "2", "all"),
        ("Billing.Run", "2", "confirmed"),
        ("Impl.Get", "1", "default"),
        ("src/Errors.cs:11", "2", "default"),
    ] {
        let v = env.answer(&root, &["impact", symbol, "--depth", depth, "--evidence", evidence], false);
        let rows = listed(&v);
        let mut scope: BTreeSet<String> = BTreeSet::new();
        for s in v["items"][0]["scope"].as_array().into_iter().flatten() {
            scope.insert(format!("{}:{}", s["file"].as_str().unwrap_or(""), s["line"]));
        }
        let frontier: BTreeSet<String> = v["items"][0]["frontier"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| format!("{}:{}", f.get("file").and_then(Value::as_str).unwrap_or(""), f["line"]))
            .collect();
        // Each listed row's own direct dependents: listed, or the row is on the frontier (and the answer says so).
        for at in &rows {
            let next = env.answer(&root, &["impact", at, "--depth", "1", "--evidence", evidence], false);
            let beyond: Vec<String> = listed(&next).into_iter().filter(|d| !rows.contains(d) && !scope.contains(d)).collect();
            if !beyond.is_empty() {
                assert!(frontier.contains(at), "impact {symbol} --depth {depth} --evidence {evidence}: {at} leads to {beyond:?}, not listed or on the frontier: {v}");
                assert!(says_stopped(&v), "impact {symbol}: a frontier without an incomplete entry: {v}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
