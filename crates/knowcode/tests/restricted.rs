//! Restricted mode's acceptance check (`knowcode serve --restricted`): bound to one repo and an op allowlist, the
//! server says its scope when it starts, answers about that repo with the ops allowed, refuses any other op, repo or
//! path with `not_allowed`, writes nothing outside its cache, and opens no network connection.
//!
//! On macOS the server runs under a sandbox profile that kills it on any write outside its cache (and `/dev`) and on
//! any network connection or bind, so a server still answering at the end has done neither. Elsewhere the same calls
//! run unsandboxed, and the files outside the cache are compared before and after.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

/// Kills the process on a write outside the cache or `/dev`, and on any IP connection or bind.
const PROFILE: &str = r#"(version 1)
(allow default)
(deny network-outbound (remote ip) (with send-signal SIGKILL))
(deny network-bind (with send-signal SIGKILL))
(deny file-write* (with send-signal SIGKILL))
(allow file-write* (subpath (param "CACHE")) (subpath "/dev"))
"#;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-restricted-{name}-{}", std::process::id()));
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
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn repo(base: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = base.join(name);
    for (p, t) in files {
        let path = root.join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, t).unwrap();
    }
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "Add it"]);
    root
}

/// Every file under `dir` but `skip`, with its length and modification time.
fn files(dir: &Path, skip: &Path, out: &mut BTreeMap<PathBuf, (u64, std::time::SystemTime)>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.starts_with(skip) {
            continue;
        }
        let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
        if m.is_dir() {
            files(&p, skip, out);
        } else {
            out.insert(p, (m.len(), m.modified().unwrap()));
        }
    }
}

/// The directory holding the real `git`, not macOS's `/usr/bin/git` shim, which runs `xcrun` and may write its own
/// cache files: the headless recipe puts it first on `PATH`.
fn real_git_dir() -> Option<PathBuf> {
    let out = Command::new("git").arg("--exec-path").output().ok()?;
    let exec = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let bin = exec.parent()?.parent()?.join("bin");
    bin.join("git").is_file().then_some(bin)
}

struct Server {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    id: u64,
    init: Value,
}

impl Server {
    fn start(cmd: &mut Command) -> Server {
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Server { child, out, id: 0, init: Value::Null };
        s.init = s.call("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }));
        s.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        s
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
            // A sandbox violation kills the server: it closes.
            assert!(self.out.read_line(&mut line).unwrap() > 0, "the server closed (killed by the sandbox?): {:?}", self.child.try_wait());
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// `kc {op, …}`: its text, and its code when refused.
    fn kc(&mut self, op: &str, args: Value) -> (String, Option<String>) {
        let mut a = args.as_object().cloned().unwrap_or_default();
        a.insert("op".into(), Value::from(op));
        let r = self.call("tools/call", json!({ "name": "kc", "arguments": a }));
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{op}: {r}")).to_string();
        let code = (r["result"]["isError"] == json!(true)).then(|| r["result"]["structuredContent"]["error"]["code"].as_str().unwrap_or("none").to_string());
        (text, code)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_restricted_server_stays_in_its_repos_ops_and_cache() {
    let base = scratch("scope");
    let shop = repo(&base, "shop", &[("src/Orders.cs", "namespace Shop;\npublic class Orders\n{\n    public void Place() { Audit.Log(); }\n}\n"), ("src/Audit.cs", "namespace Shop;\npublic static class Audit\n{\n    public static void Log() { }\n}\n")]);
    let other = repo(&base, "other", &[("src/Ledger.cs", "namespace Books;\npublic class Ledger\n{\n    public void Post() { }\n}\n")]);
    let (cache, config) = (base.join("cache"), base.join("config"));
    std::fs::create_dir_all(cache.join("tmp")).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    // The other repo is indexed in the same cache: there to be found, if the server looked.
    let indexed = Command::new(KC)
        .args(["index", other.to_str().unwrap()])
        .env("KNOWCODE_CACHE_DIR", &cache)
        .env("KNOWCODE_CONFIG_DIR", &config)
        .env("KNOWCODE_DATA_DIR", cache.join("data"))
        .env("KNOWCODE_UPDATE_CHECK", "0")
        .output()
        .unwrap();
    assert!(indexed.status.success(), "{}", String::from_utf8_lossy(&indexed.stderr));

    // Without what it needs, restricted mode won't start.
    for (args, says) in [
        (vec!["serve", "--restricted", "--ops", "find"], "--repo"),
        (vec!["serve", "--restricted", "--repo", shop.to_str().unwrap()], "--ops"),
        (vec!["serve", "--restricted", "--repo", shop.to_str().unwrap(), "--ops", "find,nope"], "no op \"nope\""),
    ] {
        let out = Command::new(KC).args(&args).current_dir(&shop).env("KNOWCODE_CACHE_DIR", &cache).env("KNOWCODE_CONFIG_DIR", &config).output().unwrap();
        assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains(says), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let out = Command::new(KC).args(["serve", "--restricted", "--repo", shop.to_str().unwrap(), "--ops", "find"]).env_clear().env("HOME", &base).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("explicit cache directory"), "{}", String::from_utf8_lossy(&out.stderr));

    let profile = base.join("restricted.sb");
    std::fs::write(&profile, PROFILE).unwrap();
    let mut before = BTreeMap::new();
    files(&base, &cache, &mut before);
    let stderr = std::fs::File::create(cache.join("stderr.txt")).unwrap();
    let serve: Vec<String> = ["serve", "--restricted", "--repo", shop.to_str().unwrap(), "--ops", "find,search,symbol_at,more", "--cache-dir", cache.to_str().unwrap(), "--config-dir", config.to_str().unwrap()]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let path = match real_git_dir() {
        Some(git) => format!("{}:/usr/bin:/bin", git.display()),
        None => "/usr/bin:/bin".into(),
    };
    let sandboxed = cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").is_file();
    let mut cmd = if sandboxed {
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.arg("-f").arg(&profile).arg("-D").arg(format!("CACHE={}", cache.display())).arg(KC).args(&serve);
        c
    } else {
        let mut c = Command::new(KC);
        c.args(&serve);
        c
    };
    // A clean environment, as the headless recipe has it: no user config, temporary files in the cache.
    cmd.current_dir(&shop).env_clear().env("HOME", &base).env("PATH", path).env("TMPDIR", cache.join("tmp")).stderr(stderr);
    let mut s = Server::start(&mut cmd);

    // The first reply states the scope.
    let scope = s.init["result"]["capabilities"]["experimental"]["knowcode/scope"].clone();
    assert_eq!(scope["restricted"], json!(true), "{}", s.init);
    assert_eq!(scope["repos"], json!([{ "name": "shop", "path": shop }]), "{scope}");
    assert_eq!(scope["ops"], json!(["capabilities", "find", "search", "symbol_at", "more"]), "{scope}");
    assert_eq!(scope["cache_dir"], json!(cache), "{scope}");
    for off in ["discovery", "update_checks", "plugin_refresh", "git_network"] {
        assert_eq!(scope[off], json!(false), "{off}: {scope}");
    }
    let tools = s.call("tools/list", json!({}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 1, "{tools}");

    // The ops allowed answer about the bound repo. The server indexes it in the background on first use: until that
    // finishes, an answer comes from the committed snapshot and says so (never `complete`); then it finds the file.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (text, code) = s.kc("find", json!({ "q": "Orders" }));
        assert!(code.is_none(), "{text}");
        if text.contains("src/Orders.cs") {
            break;
        }
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| panic!("{text}"));
        let indexing = v["incomplete"].as_array().is_some_and(|i| i.iter().any(|x| x["code"] == "indexing"));
        assert!(v["complete"] == json!(false) && indexing, "an answer before the repo is indexed must say so: {text}");
        assert!(std::time::Instant::now() < until, "the bound repo was never indexed: {text}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let (text, code) = s.kc("symbol_at", json!({ "q": shop.join("src/Audit.cs").to_str().unwrap(), "line": 4 }));
    assert!(code.is_none() && text.contains("Audit.Log"), "{text}");
    // The workspace is the bound repo: the other repo's names aren't found.
    let (text, code) = s.kc("search", json!({ "q": "Ledger", "workspace": true }));
    assert!(code.is_none() && !text.contains("Ledger.cs"), "{text}");
    let (text, code) = s.kc("find", json!({ "q": "Ledger" }));
    assert!(code.is_none() && !text.contains("Ledger.cs"), "{text}");
    // The capabilities say the scope and which ops are allowed.
    let (text, _) = s.kc("capabilities", json!({}));
    let caps: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(caps["scope"], scope);
    let allowed: Vec<&str> = caps["ops"].as_array().unwrap().iter().filter(|o| o["allowed"] == json!(true)).map(|o| o["op"].as_str().unwrap()).collect();
    assert_eq!(allowed, ["find", "search", "more", "symbol_at", "capabilities"]);

    // Anything else is refused, with its code.
    for (op, args) in [
        ("refs", json!({ "q": "Log" })),
        ("note_add", json!({ "q": "Audit.Log", "text": "x" })),
        ("status", json!({})),
        ("find", json!({ "q": "Ledger", "repo": "other" })),
        ("find", json!({ "q": "Ledger", "repo": other.to_str().unwrap() })),
        ("search", json!({ "q": "Ledger", "repos": ["shop", "other"] })),
        ("symbol_at", json!({ "q": other.join("src/Ledger.cs").to_str().unwrap(), "line": 2 })),
    ] {
        let (text, code) = s.kc(op, args.clone());
        assert_eq!(code.as_deref(), Some("not_allowed"), "{op} {args}: {text}");
        assert!(!text.contains("Ledger.cs"), "{op} {args}: {text}");
    }

    // Under separate tools, only those allowed are listed, and the others are refused all the same.
    let mut cmd2 = Command::new(KC);
    cmd2.args(&serve).args(["--tools", "all"]).current_dir(&shop).env_clear().env("HOME", &base).env("PATH", "/usr/bin:/bin").env("TMPDIR", cache.join("tmp"));
    let mut all = Server::start(cmd2.stderr(Stdio::null()));
    let listed: Vec<String> = all.call("tools/list", json!({}))["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(listed, ["kc_find", "kc_more", "kc_search", "kc_symbol_at"], "{listed:?}");
    let r = all.call("tools/call", json!({ "name": "kc_refs", "arguments": { "name": "Log" } }));
    assert_eq!(r["result"]["structuredContent"]["error"]["code"], json!("not_allowed"), "{r}");
    drop(all);

    // Still running: nothing it did broke the sandbox.
    let (text, code) = s.kc("find", json!({ "q": "Audit" }));
    assert!(code.is_none() && text.contains("src/Audit.cs"), "{text}");
    drop(s);
    let said = std::fs::read_to_string(cache.join("stderr.txt")).unwrap();
    assert!(said.lines().next().is_some_and(|l| l.starts_with("knowcode: restricted: {")), "{said}");

    // Nothing outside the cache was written.
    let mut after = BTreeMap::new();
    files(&base, &cache, &mut after);
    let changed: Vec<&PathBuf> = before.keys().chain(after.keys()).filter(|p| before.get(*p) != after.get(*p)).collect();
    assert!(changed.is_empty(), "files outside the cache changed: {changed:?}");
    let _ = std::fs::remove_dir_all(&base);
}

/// A cursor and a path name a repo too: a restricted server answers from neither unless it is bound, nor continues a
/// cursor of an op it doesn't run, nor one it didn't make. A bound repo's name is that repo, never a directory of
/// that name under the current one.
#[test]
fn a_restricted_server_takes_no_repo_from_a_cursor_a_relative_path_or_a_name_under_its_directory() {
    let base = scratch("bypass");
    let shop = repo(&base.join("bound"), "app", &[("src/Orders.cs", "namespace Shop;\npublic class Orders\n{\n    public void Place() { }\n    public void PlaceTwice() { Place(); Place(); }\n}\n")]);
    // The server's current directory, with a repo of the bound one's name and another repo beside it.
    let cwd = base.join("cwd");
    let decoy = repo(&cwd, "app", &[("src/Decoy.cs", "namespace Decoy;\npublic class Orders\n{\n    public void Place() { }\n}\n")]);
    let other = repo(&cwd, "other", &[("src/Ledger.cs", "namespace Books;\npublic class Ledger\n{\n    public void Post() { Ledger.Mark(); Ledger.Mark(); }\n    public static void Mark() { }\n}\n")]);
    let (cache, config) = (base.join("cache"), base.join("config"));
    std::fs::create_dir_all(&config).unwrap();
    let cli = |args: &[&str]| -> Value {
        let out = Command::new(KC)
            .args(args)
            .current_dir(&base)
            .env("KNOWCODE_CACHE_DIR", &cache)
            .env("KNOWCODE_CONFIG_DIR", &config)
            .env("KNOWCODE_DATA_DIR", cache.join("data"))
            .env("KNOWCODE_UPDATE_CHECK", "0")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    };
    // Cursors this KnowCode made, with the same cache: one of an op the server doesn't run, about its own repo, and
    // one of an op it runs, about the other repo.
    let search = cli(&["search", "Place", "--repo", shop.to_str().unwrap(), "--limit", "1", "--json"]);
    let search_cursor = search["cursor"].as_str().unwrap_or_else(|| panic!("{search}")).to_string();
    let refs = cli(&["refs", "Mark", "--repo", other.to_str().unwrap(), "--limit", "1", "--json"]);
    let refs_cursor = refs["cursor"].as_str().unwrap_or_else(|| panic!("{refs}")).to_string();

    let mut cmd = Command::new(KC);
    cmd.args(["serve", "--restricted", "--repo", shop.to_str().unwrap(), "--ops", "find,refs,path,more", "--cache-dir", cache.to_str().unwrap(), "--config-dir", config.to_str().unwrap()])
        .current_dir(&cwd)
        .env("KNOWCODE_UPDATE_CHECK", "0")
        .stderr(Stdio::null());
    let mut s = Server::start(&mut cmd);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    // The bound repo by its name: the bound one, never `./app`.
    loop {
        let (text, code) = s.kc("find", json!({ "q": "Orders", "repo": "app" }));
        assert!(code.is_none(), "{text}");
        assert!(!text.contains("Decoy"), "{text}");
        if text.contains("src/Orders.cs") {
            break;
        }
        assert!(std::time::Instant::now() < until, "the bound repo was never indexed: {text}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // With no repo named, from a directory in none, the other repo indexed in the same cache is not one it reads.
    let (text, code) = s.kc("find", json!({ "q": "Ledger" }));
    assert!(code.is_none() && !text.contains("Ledger.cs"), "{text}");
    let (text, code) = s.kc("find", json!({ "q": "Orders", "repos": ["app"] }));
    assert!(code.is_none() && text.contains("src/Orders.cs") && !text.contains("Decoy"), "{text}");
    // A repo named by a path relative to the current directory.
    let (text, code) = s.kc("path", json!({ "from": "Orders.Place", "to": "Ledger.Post", "to_repo": "other" }));
    assert_eq!(code.as_deref(), Some("not_allowed"), "{text}");
    // A cursor of an op it doesn't run.
    let (text, code) = s.kc("more", json!({ "q": search_cursor }));
    assert_eq!(code.as_deref(), Some("not_allowed"), "{text}");
    // A cursor of an op it runs, about a repo it isn't bound to.
    let (text, code) = s.kc("more", json!({ "q": refs_cursor }));
    assert_eq!(code.as_deref(), Some("not_allowed"), "{text}");
    assert!(!text.contains("Ledger.cs"), "{text}");
    // A cursor it didn't make: the same request with another repo, or as written by hand.
    let (body, mac) = search_cursor.trim_start_matches("kc1.").rsplit_once('.').unwrap();
    for forged in [format!("kc1.{body}"), format!("kc1.{body}x.{mac}"), "kc1.e30.AAAAAAAAAAAAAAAAAAAAAA".to_string()] {
        let (text, code) = s.kc("more", json!({ "q": forged }));
        assert_eq!(code.as_deref(), Some("invalid_argument"), "{forged}: {text}");
    }
    drop(s);
    let _ = (decoy, std::fs::remove_dir_all(&base));
}

/// A repo named in a call is the bound repo of that name, never a same-named directory under the server's working
/// directory: that is another repo, outside the scope.
#[test]
fn a_bound_repo_name_never_resolves_against_the_working_directory() {
    let base = scratch("cwd-name");
    let shop = repo(&base.join("bound"), "shop", &[("src/Orders.cs", "namespace Shop;\npublic class Orders\n{\n    public void Place() { }\n}\n")]);
    let decoy = repo(&base.join("cwd"), "shop", &[("src/Ledger.cs", "namespace Books;\npublic class Ledger\n{\n    public void Post() { }\n}\n")]);
    let (cache, config) = (base.join("cache"), base.join("config"));
    std::fs::create_dir_all(cache.join("tmp")).unwrap();
    let mut cmd = Command::new(KC);
    cmd.args(["serve", "--restricted", "--repo", shop.to_str().unwrap(), "--ops", "find", "--cache-dir", cache.to_str().unwrap(), "--config-dir", config.to_str().unwrap()])
        .current_dir(decoy.parent().unwrap())
        .env("KNOWCODE_UPDATE_CHECK", "0")
        .env("TMPDIR", cache.join("tmp"))
        .stderr(Stdio::null());
    let mut s = Server::start(&mut cmd);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (text, code) = s.kc("find", json!({ "q": "Orders", "repo": "shop" }));
        assert!(code.is_none() && !text.contains("Ledger"), "{text}");
        if text.contains("src/Orders.cs") {
            break;
        }
        assert!(std::time::Instant::now() < until, "the bound repo was never answered for: {text}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let (text, code) = s.kc("find", json!({ "q": "Ledger", "repo": "shop" }));
    assert!(code.is_none() && !text.contains("Ledger.cs"), "{text}");
    drop(s);
    // The directory under the working directory was never indexed.
    assert!(!String::from_utf8_lossy(&Command::new("find").arg(&cache).output().unwrap().stdout).contains("Ledger"));
    let _ = std::fs::remove_dir_all(&base);
}
