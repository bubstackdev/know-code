//! The capabilities contract: each op's entry in `kc {op: capabilities}` matches what the running server accepts and
//! returns. Every op is called through `kc` as the entry says (`q`, `qs`), its answer has the format and keys the
//! entry names, each bounded number is clamped past its range rather than refused, each closed set's words are
//! accepted and any other word is refused, and a value of the wrong type is refused. The document is the same as a
//! resource, from the CLI, and under every profile, apart from what that profile advertises.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-capabilities-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "user.name=Ann Writer", "-c", "user.email=ann@example.test"])
        .args(args)
        .env("GIT_AUTHOR_DATE", "2026-09-01T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-09-01T12:00:00Z")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// A small C# repo, committed twice so history ops have something to read.
fn fixture(base: &Path) -> PathBuf {
    let root = base.join("shop");
    let files = [
        ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("src/IOrders.cs", "namespace Shop;\npublic interface IOrders { void Place(); }\n"),
        ("src/Orders.cs", "namespace Shop;\npublic class Orders : IOrders\n{\n    public void Place() { Audit.Log(); }\n}\n"),
        ("src/Audit.cs", "namespace Shop;\npublic static class Audit\n{\n    public static void Log() { }\n}\n"),
        ("src/Checkout.cs", "namespace Shop;\npublic class Checkout\n{\n    private readonly IOrders _orders;\n    public void Run() { _orders.Place(); Audit.Log(); }\n}\n"),
        ("tests/CheckoutTests.cs", "namespace Shop.Tests;\npublic class CheckoutTests\n{\n    public void Runs() { new Shop.Checkout().Run(); }\n}\n"),
    ];
    for (p, t) in files {
        let path = root.join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, t).unwrap();
    }
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "Add the shop"]);
    std::fs::write(root.join("src/Orders.cs"), "namespace Shop;\npublic class Orders : IOrders\n{\n    public void Place() { Audit.Log(); Audit.Log(); }\n}\n").unwrap();
    std::fs::write(root.join("src/Checkout.cs"), "namespace Shop;\npublic class Checkout\n{\n    private readonly IOrders _orders;\n    public void Run() { _orders.Place(); }\n}\n").unwrap();
    git(&root, &["commit", "-qam", "Place logs twice"]);
    root
}

struct Env {
    base: PathBuf,
}

impl Env {
    fn cmd(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(KC);
        cmd.args(args)
            .current_dir(cwd)
            .env("KNOWCODE_CACHE_DIR", self.base.join("cache"))
            .env("KNOWCODE_CONFIG_DIR", self.base.join("config"))
            .env("KNOWCODE_DATA_DIR", self.base.join("data"))
            .env("KNOWCODE_NOW", "head")
            .env_remove("KNOWCODE_TOOLS")
            .env_remove("KNOWCODE_VERIFY");
        cmd
    }
}

/// A line-delimited JSON-RPC client of `knowcode serve`.
struct Mcp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    id: u64,
}

impl Mcp {
    fn start(env: &Env, cwd: &Path, tools: &str) -> Mcp {
        let mut child = env.cmd(cwd, &["serve", "--tools", tools]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp { child, out, id: 0 };
        m.call("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }));
        m.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        m
    }

    fn send(&mut self, v: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "the server closed");
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// A tool call's text, and whether it was an error.
    fn tool(&mut self, name: &str, args: Value) -> (String, bool) {
        let r = self.call("tools/call", json!({ "name": name, "arguments": args }));
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{name} {args}: {r}")).to_string();
        (text, r["result"]["isError"] == json!(true))
    }

    /// `kc {op, …args}`.
    fn kc(&mut self, op: &str, args: &Value) -> (String, bool) {
        let mut a = args.as_object().cloned().unwrap_or_default();
        a.insert("op".into(), Value::from(op));
        self.tool("kc", Value::Object(a))
    }

    fn tool_names(&mut self) -> Vec<String> {
        self.call("tools/list", json!({}))["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str().map(String::from)).collect()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A call each op answers on the fixture, its main input as `q` (or `qs`) where it takes one.
fn case(op: &str) -> Option<Value> {
    Some(match op {
        "impact" | "trace" | "history" | "notes" => json!({ "q": "Audit.Log" }),
        "context" => json!({ "q": "Orders.Place" }),
        "rename_preview" => json!({ "q": "Audit.Log", "new_name": "Write" }),
        "note_add" => json!({ "q": "Audit.Log", "text": "the one audit sink" }),
        "find" => json!({ "q": "Orders" }),
        "locate" => json!({ "q": "place orders" }),
        "refs" => json!({ "q": "Log" }),
        "search" => json!({ "q": "Log" }),
        "grep_ast" => json!({ "q": "Audit.Log()", "lang": "csharp" }),
        "more" => return None,
        "help" => json!({ "q": "impact" }),
        "capabilities" => json!({ "q": "find" }),
        "path" => json!({ "q": "Checkout.Run", "to": "Orders.Place" }),
        "flow" => json!({ "q": "Checkout.Run" }),
        "symbol_at" => json!({ "q": "src/Orders.cs", "line": 4 }),
        "coupled" => json!({ "q": "src/Orders.cs", "since": "all" }),
        "tests" => json!({ "q": "src/Checkout.cs" }),
        "ownership" => json!({ "q": "src" }),
        "reviewers" => json!({ "q": "src/Orders.cs" }),
        "overview" => json!({}),
        "areas" => json!({}),
        "unused" | "hotspots" | "status" => json!({}),
        other => panic!("no case for op {other}: add one"),
    })
}

/// The answer's format and keys are the ones the entry names.
fn check_answer(op: &str, entry: &Value, args: &Value, text: &str) {
    let out = &entry["output"];
    let format = args.get("format").and_then(Value::as_str).unwrap_or_else(|| out["format"].as_str().unwrap());
    match format {
        "tree" => assert!(text.starts_with("repo "), "{op} {args}: a tree starts with its header: {text}"),
        "text" => assert!(!text.is_empty(), "{op}"),
        _ => {
            let v: Value = serde_json::from_str(text).unwrap_or_else(|e| panic!("{op} {args}: {e}: {text}"));
            for k in out["keys"].as_array().unwrap() {
                assert!(v.get(k.as_str().unwrap()).is_some(), "{op} {args}: no {k}: {text}");
            }
            if let Some(list) = out["list"].as_str() {
                assert!(v[list].is_array(), "{op} {args}: {list} is a list: {text}");
            }
        }
    }
}

#[test]
fn every_op_accepts_and_answers_as_its_capabilities_say() {
    let base = scratch("contract");
    let root = fixture(&base);
    let env = Env { base: base.clone() };
    assert!(env.cmd(&base, &["index", root.to_str().unwrap()]).status().unwrap().success());
    let mut mcp = Mcp::start(&env, &root, "one");
    assert_eq!(mcp.tool_names(), vec!["kc".to_string()]);
    let (text, err) = mcp.kc("capabilities", &json!({}));
    assert!(!err, "{text}");
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["capabilities"], json!(1));
    assert_eq!(doc["profile"], json!("one"));
    for k in ["server", "protocol", "index_schema", "common", "evidence"] {
        assert!(doc.get(k).is_some(), "no {k}");
    }

    // The same document as a resource and from the CLI.
    let r = mcp.call("resources/read", json!({ "uri": "knowcode://capabilities" }));
    let res: Value = serde_json::from_str(r["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(res, doc);
    let cli = env.cmd(&root, &["capabilities"]).output().unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&cli.stdout).unwrap(), doc);

    let ops = doc["ops"].as_array().unwrap();
    // The ops the help's `kc` topic names are the ops described, in one order.
    let names: Vec<&str> = ops.iter().map(|o| o["op"].as_str().unwrap()).collect();
    let (help, _) = mcp.kc("help", &json!({ "q": "kc" }));
    assert!(help.contains(&names.join(", ")), "{help}");

    for entry in ops {
        let op = entry["op"].as_str().unwrap();
        let tool = entry["tool"].as_str().unwrap();
        assert_eq!(tool, format!("kc_{op}"));
        assert_eq!(entry["advertised"], json!(true), "{op}: every op is reached through kc");
        assert_eq!(entry["access"], json!(if op == "note_add" { "write" } else { "read" }), "{op}");
        let props = entry["input"]["properties"].as_object().unwrap_or_else(|| panic!("{op}: no input schema"));
        // `q` and `qs` fill parameters the schema has; an op taking no query refuses one.
        for k in ["q", "qs"] {
            if let Some(p) = entry[k].as_str() {
                assert!(props.contains_key(p), "{op}: {k} fills {p}, which its schema doesn't have");
            }
        }
        assert_eq!(entry["takes_query"], json!(entry["q"].is_string()), "{op}");
        if entry["q"].is_null() {
            assert!(mcp.kc(op, &json!({ "q": "x" })).1, "{op} takes no q");
        }
        for (alias, of) in entry["aliases"].as_object().unwrap() {
            assert!(props.contains_key(alias) && props.contains_key(of.as_str().unwrap()), "{op}: alias {alias}");
        }
        let Some(args) = case(op) else { continue };

        // A valid call answers, in the format and with the keys the entry names; every format it takes, too.
        let (text, err) = mcp.kc(op, &args);
        assert!(!err, "{op} {args}: {text}");
        check_answer(op, entry, &args, &text);
        for f in entry["output"]["formats"].as_array().unwrap() {
            if entry["choices"].get("format").is_some() {
                let mut a = args.clone();
                a["format"] = f.clone();
                let (text, err) = mcp.kc(op, &a);
                assert!(!err, "{op} {a}: {text}");
                check_answer(op, entry, &a, &text);
            }
        }
        if let Some(batch) = entry.get("batch") {
            assert_eq!(entry["qs"], batch["param"], "{op}");
            let mut a = args.clone();
            let q = a.as_object_mut().unwrap().remove("q").unwrap();
            a["qs"] = json!([q, q]);
            let (text, err) = mcp.kc(op, &a);
            assert!(!err, "{op} {a}: {text}");
            let v: Value = serde_json::from_str(&text).unwrap();
            for k in batch["keys"].as_array().unwrap() {
                assert!(v.get(k.as_str().unwrap()).is_some(), "{op} batch: no {k}: {text}");
            }
            let over: Vec<Value> = (0..=batch["max"].as_u64().unwrap()).map(|i| json!(format!("N{i}"))).collect();
            let mut a = args.clone();
            a.as_object_mut().unwrap().remove("q");
            a["qs"] = Value::from(over);
            assert!(mcp.kc(op, &a).1, "{op}: more than {} inputs are refused", batch["max"]);
        }

        // Each range: in the schema too; past either end, clamped, not refused.
        for (param, r) in entry["ranges"].as_object().unwrap() {
            let p = &props[param];
            assert_eq!((&p["minimum"], &p["maximum"], &p["default"]), (&r["min"], &r["max"], &r["default"]), "{op}.{param}");
            assert_eq!(r["beyond"], json!("clamped"));
            let (min, max) = (r["min"].as_u64().unwrap(), r["max"].as_u64().unwrap());
            let mut ends = vec![max + 1];
            if min > 0 {
                ends.push(min - 1);
            }
            for v in ends {
                let mut a = args.clone();
                a[param] = json!(v);
                let (text, err) = mcp.kc(op, &a);
                assert!(!err, "{op} {param}={v} is clamped, not refused: {text}");
            }
            // A walk asked deeper than it goes says what it walked.
            if param == "depth" && entry["choices"].get("format").is_some() {
                let mut a = args.clone();
                a[param] = json!(max + 5);
                a["format"] = json!("json");
                let v: Value = serde_json::from_str(&mcp.kc(op, &a).0).unwrap();
                if let Some(d) = v.get("depth") {
                    assert_eq!(d, &json!({ "asked": max + 5, "walked": max }), "{op}");
                }
            }
            // A number of the wrong type is refused.
            let mut a = args.clone();
            a[param] = json!("deep");
            assert!(mcp.kc(op, &a).1, "{op} {param}: a word for a number is refused");
        }

        // Each closed set: in the schema too; every word accepted, any other refused.
        for (param, c) in entry["choices"].as_object().unwrap() {
            let values = c["values"].as_array().unwrap();
            assert_eq!(&props[param]["enum"], &c["values"], "{op}.{param}");
            assert_eq!(&c["default"], &values[0], "{op}.{param}");
            for v in values {
                let mut a = args.clone();
                a[param] = v.clone();
                if param == "lang" {
                    a["q"] = json!("f(1)");
                }
                let (text, err) = mcp.kc(op, &a);
                assert!(!err, "{op} {param}={v}: {text}");
            }
            let mut a = args.clone();
            a[param] = json!("sideways");
            let (text, err) = mcp.kc(op, &a);
            assert!(err, "{op} {param}=sideways is refused: {text}");
            assert!(text.contains("sideways"), "{op}: the refusal names the word: {text}");
        }
    }

    // The write is the one the entry says: the note is there to read.
    let (notes, _) = mcp.kc("notes", &json!({ "q": "Audit.Log" }));
    assert!(notes.contains("the one audit sink"), "{notes}");

    // One op, by op or tool name; an unknown one is refused, naming the ops.
    for of in ["impact", "kc_impact"] {
        let v: Value = serde_json::from_str(&mcp.kc("capabilities", &json!({ "q": of })).0).unwrap();
        assert_eq!(v["ops"].as_array().unwrap().len(), 1);
        assert_eq!(v["ops"][0], ops.iter().find(|o| o["op"] == "impact").unwrap().clone());
    }
    let (text, err) = mcp.kc("capabilities", &json!({ "q": "nope" }));
    assert!(err && text.contains("impact, find"), "{text}");
    drop(mcp);

    // Under another profile: the same ops, each `advertised` as tools/list says; the tool itself only when named.
    for tools in ["core", "all", "all,kc_capabilities"] {
        let mut m = Mcp::start(&env, &root, tools);
        let listed = m.tool_names();
        assert_eq!(listed.contains(&"kc_capabilities".to_string()), tools.contains("kc_capabilities"), "{tools}: {listed:?}");
        let r = m.call("resources/read", json!({ "uri": "knowcode://capabilities" }));
        let d: Value = serde_json::from_str(r["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
        for e in d["ops"].as_array().unwrap() {
            let tool = e["tool"].as_str().unwrap();
            assert_eq!(e["advertised"], json!(listed.iter().any(|t| t == tool)), "{tools}: {tool}");
            let mut same = e.clone();
            same["advertised"] = json!(true);
            assert_eq!(&same, ops.iter().find(|o| o["tool"] == e["tool"]).unwrap(), "{tools}: {tool}");
        }
        assert_eq!(d["profile"].as_array().unwrap().len(), listed.len(), "{tools}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
