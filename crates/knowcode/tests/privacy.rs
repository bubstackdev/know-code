//! AI privacy files (`.knowcode-privacy.yml`): content at or below `[privacy] block_at` never reaches an answer, through
//! the binary as agents and the MCP server run it, and never stays in the cache.
//!
//! The fixture is a workspace of two repos: `app`, whose `vault/` folder is level 1 (with a child folder marked
//! public, which can't loosen its parent), and `locked-lib`, level 1 whole, which `app` depends on by package name.
//! Every blocked file holds names, text and paths containing the marker `zqx`, which nothing allowed contains: no
//! output of any command or tool, and no byte of the cache, may contain it.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const KC: &str = env!("CARGO_BIN_EXE_knowcode");
const MARK: &str = "zqx";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-privacy-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
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
    assert!(ok, "git {args:?} in {}", dir.display());
}

const APP_PRIVACY: &str = "level: 3\nrules:\n  - path: /vault/\n    level: 1\n  - path: /vault/open/\n    level: 4\n";

/// The app's files, allowed and blocked; nothing allowed contains the marker.
fn app_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("package.json", "{ \"name\": \"app\", \"dependencies\": { \"locked-lib\": \"^1.0.0\" } }\n"),
        ("src/main.py", "from helpers import shared_helper\n\ndef run_app():\n    return shared_helper()\n"),
        ("src/helpers.py", "def shared_helper():\n    return 1\n"),
        ("docs/readme.md", "# App\n\nThe shared helper runs the app.\n"),
        (
            "vault/zqx_secret_store.py",
            "class ZqxVaultCipher:\n    def zqx_seal(self):\n        return \"ZQXCONTENTMARK\"\n\ndef shared_helper():\n    return ZqxVaultCipher().zqx_seal()\n",
        ),
        ("vault/open/zqx_open_note.md", "# Zqx open note\n\nZQXOPENMARK: a public folder under a restricted one stays restricted.\n"),
    ]
}

/// The workspace: `app` (committed in three commits, one touching only blocked files) and `locked-lib`.
fn fixture(base: &Path, with_privacy: bool) -> (PathBuf, PathBuf) {
    let app = base.join("app");
    std::fs::create_dir_all(&app).unwrap();
    git(&app, &["init", "-q"]);
    for (p, t) in app_files() {
        write(&app.join(p), t);
    }
    if with_privacy {
        write(&app.join(".knowcode-privacy.yml"), APP_PRIVACY);
    }
    git(&app, &["add", "-A"]);
    git(&app, &["commit", "-qm", "Add the app"]);
    write(&app.join("vault/zqx_secret_store.py"), &format!("{}# zqx tweak\n", app_files()[4].1));
    git(&app, &["commit", "-qam", "zqxsubjectmark: tweak the vault"]);
    write(&app.join("src/helpers.py"), "def shared_helper():\n    return 2\n");
    write(&app.join("src/main.py"), "from helpers import shared_helper\n\ndef run_app():\n    return shared_helper() + 1\n");
    git(&app, &["commit", "-qam", "Tune the helpers"]);

    let lib = base.join("locked-lib");
    std::fs::create_dir_all(&lib).unwrap();
    git(&lib, &["init", "-q"]);
    write(&lib.join(".knowcode-privacy.yml"), "level: 1\n");
    write(&lib.join("package.json"), "{ \"name\": \"locked-lib\", \"version\": \"1.0.0\" }\n");
    write(&lib.join("index.ts"), "export class ZqxLockedType {\n  zqxLockedMethod() { return 'ZQXLOCKEDCONTENT'; }\n}\nexport function shared_helper() { return new ZqxLockedType(); }\n");
    git(&lib, &["add", "-A"]);
    git(&lib, &["commit", "-qm", "Add the zqx library"]);
    (app, lib)
}

struct Env {
    cache: PathBuf,
    config: PathBuf,
    /// The data directory, where notes live.
    data: PathBuf,
}

impl Env {
    fn new(base: &Path) -> Env {
        let env = Env { cache: base.join("cache"), config: base.join("config"), data: base.join("data") };
        std::fs::create_dir_all(&env.config).unwrap();
        env
    }

    fn cmd(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(KC);
        cmd.args(args)
            .current_dir(cwd)
            .env("KNOWCODE_CACHE_DIR", &self.cache)
            .env("KNOWCODE_CONFIG_DIR", &self.config)
            .env("KNOWCODE_DATA_DIR", &self.data)
            .env("KNOWCODE_NOW", "head")
            .env_remove("KNOWCODE_VERIFY");
        cmd
    }

    /// (exit code, stdout and stderr together).
    fn run(&self, cwd: &Path, args: &[&str]) -> (i32, String) {
        let out = self.cmd(cwd, args).output().unwrap();
        (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }

    /// The JSON answer on stdout (warnings go to stderr).
    fn json(&self, cwd: &Path, args: &[&str]) -> Value {
        let out = self.cmd(cwd, args).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_clean(&format!("{args:?} stderr"), &String::from_utf8_lossy(&out.stderr));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{args:?}: {e}: {text}"))
    }

    /// Every byte the cache and the data directory (notes) hold, lower-cased as text, by file.
    fn cache_files(&self) -> Vec<(PathBuf, String)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if let Ok(b) = std::fs::read(&p) {
                    out.push((p, String::from_utf8_lossy(&b).to_lowercase()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.cache, &mut out);
        walk(&self.data, &mut out);
        out
    }

    fn cache_holds_marker(&self) -> Vec<PathBuf> {
        self.cache_files().into_iter().filter(|(_, t)| t.contains(MARK)).map(|(p, _)| p).collect()
    }
}

/// Questions over every command, none naming anything blocked (args, whether text-only).
const QUESTIONS: &[&[&str]] = &[
    &["at", "src/main.py:4"],
    &["find", "shared_helper"],
    &["find", "Cipher"],
    &["locate", "secret store vault cipher seal"],
    &["task", "secret store vault cipher seal"],
    &["refs", "shared_helper"],
    &["context", "shared_helper"],
    &["context", "run_app"],
    &["trace", "run_app", "--depth", "3"],
    &["trace", "shared_helper", "-d", "in", "--depth", "3"],
    &["path", "run_app", "shared_helper"],
    &["flow", "run_app"],
    &["impact", "shared_helper"],
    &["impact", "--file", "src/helpers.py"],
    &["impact", "--diff", "HEAD~2"],
    &["unused"],
    &["rename", "shared_helper", "common_helper"],
    &["coupled", "src/helpers.py"],
    &["coupled", "src/helpers.py", "--since", "all"],
    &["tests", "src/helpers.py"],
    &["tests", "--diff", "HEAD~2"],
    &["test-targets"],
    &["hotspots"],
    &["hotspots", "src/helpers.py"],
    &["ownership"],
    &["ownership", "src"],
    &["reviewers", "src/helpers.py"],
    &["reviewers", "--diff", "HEAD~2"],
    &["overview"],
    &["areas"],
    &["note", "list"],
    &["docs"],
    &["search", "shared"],
    &["search", "MARK", "-i"],
    &["search", "seal", "--files"],
    &["grep-ast", "def $F($$$): $$$", "-l", "python"],
    &["grep-ast", "class $C: $$$", "-l", "python"],
    &["rules", "--regex", "(?i)zqx"],
    &["rules", "--regex", "(?i)zqx", "--new-since", "HEAD~2"],
    &["review", "--diff", "HEAD~2"],
    &["deps"],
    &["routes"],
    &["tables"],
    &["events"],
    &["status"],
    &["files"],
    &["fetch-deps", "--into", "../fetched", "--dry-run"],
    &["history", "shared_helper"],
    &["history", "deepen"],
];

/// Commands that answer no question about code (see `invariants.rs`).
const NOT_QUESTIONS: &[&str] = &["index", "forget", "prune", "serve", "purge", "config", "workspace", "setup", "uninstall", "self-update", "help", "more", "hook", "bench", "capabilities"];

#[test]
fn invariant_setup_never_indexes_or_exposes_a_blocked_repository() {
    let base = scratch("setup");
    let (_app, lib) = fixture(&base, true);
    let env = Env::new(&base);
    for client in ["claude", "codex", "cursor"] {
        let (code, out) = env.run(&lib, &["setup", client, "--print"]);
        assert_eq!(code, 0, "{out}");
        assert_clean("setup snippet", &out);
    }
    for client in ["codex", "cursor"] {
        let directory = base.join(format!("client-{client}"));
        let (code, out) = env.run(&lib, &["setup", client, "--client-dir", directory.to_str().unwrap()]);
        assert_eq!(code, 0, "{out}");
        assert_clean("client setup", &out);
    }
    assert!(!env.cache.join("repos").exists(), "client setup must not create an index");
    assert_eq!(env.cache_holds_marker(), Vec::<PathBuf>::new());
    let _ = std::fs::remove_dir_all(base);
}

fn listed_commands() -> BTreeSet<String> {
    let out = Command::new(KC).arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout).into_owned();
    let section = help.split("Commands:").nth(1).expect("a Commands section").split("\n\n").next().unwrap().to_string();
    section.lines().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()
}

fn assert_clean(what: &str, out: &str) {
    assert!(!out.to_lowercase().contains(MARK), "{what} shows blocked content:\n{out}");
}

#[test]
fn invariant_no_command_shows_blocked_content() {
    let base = scratch("cli");
    let (app, lib) = fixture(&base, true);
    let env = Env::new(&base);
    let (code, out) = env.run(&base, &["index", app.to_str().unwrap(), lib.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert_clean("index", &out);
    assert!(out.contains("blocked"), "index says the library is blocked: {out}");

    let asked: BTreeSet<&str> = QUESTIONS.iter().map(|q| q[0]).collect();
    let missing: Vec<String> = listed_commands().into_iter().filter(|c| !asked.contains(c.as_str()) && !NOT_QUESTIONS.contains(&c.as_str())).collect();
    assert!(missing.is_empty(), "commands with no privacy question: {missing:?}");

    let mut withheld = 0;
    for q in QUESTIONS {
        // In the repo, and over the workspace (outside any repo); as text and as JSON.
        for cwd in [&app, &base] {
            for json in [false, true] {
                let mut argv: Vec<&str> = q.to_vec();
                if json && !matches!(q[0], "docs" | "files") {
                    argv.push("--json");
                }
                let (code, out) = env.run(cwd, &argv);
                let what = format!("{argv:?} from {}", cwd.display());
                assert!(code == 0 || code == 2 || code == 1, "{what}: {out}");
                assert_clean(&what, &out);
                withheld += usize::from(out.contains("withheld"));
            }
        }
    }
    assert!(withheld > QUESTIONS.len(), "answers drawing on the app say what they withheld ({withheld})");
    // Each JSON answer in the app carries the contract's entry, naming the repo and never a blocked path.
    let v = env.json(&app, &["refs", "shared_helper", "--json"]);
    let details: Vec<&str> = v["incomplete"].as_array().unwrap().iter().filter(|i| i["reason"] == "withheld").filter_map(|i| i["detail"].as_str()).collect();
    // The app's own blocked folder, and the blocked library it depends on (never opened, so in no provenance).
    assert_eq!(details, ["AI privacy level 1 in app", "AI privacy level 1 in locked-lib"], "{v}");
    assert_eq!(v["complete"], false);
    // Allowed code still answers.
    assert!(v["items"].as_array().unwrap().iter().any(|i| i["file"] == "src/main.py"), "{v}");

    // Questions naming blocked things: a blocked path is refused without being repeated; the repo blocked whole is
    // refused; a blocked symbol's name is unknown.
    for q in [
        &["at", "vault/zqx_secret_store.py:2"][..],
        &["context", "vault/zqx_secret_store.py#ZqxVaultCipher"],
        &["coupled", "vault/zqx_secret_store.py"],
        &["hotspots", "vault/zqx_secret_store.py"],
        &["ownership", "vault/"],
        &["tests", "vault/zqx_secret_store.py"],
        &["impact", "--file", "vault/zqx_secret_store.py"],
        &["reviewers", "vault/zqx_secret_store.py"],
        &["search", "seal", "-g", "vault/**"],
        &["find", "x", "--repo", "vault/open/zqx_open_note.md"],
    ] {
        let (code, out) = env.run(&app, q);
        assert_eq!(code, 1, "{q:?}: {out}");
        assert!(out.contains("withheld: AI privacy level 1 in app"), "{q:?}: {out}");
        assert_clean(&format!("{q:?}"), &out);
    }
    for q in [&["find", "shared_helper"][..], &["status"], &["refs", "shared_helper"]] {
        let (code, out) = env.run(&lib, q);
        let refused = code == 1 && out.contains("withheld: AI privacy level 1 in locked-lib");
        assert!(refused || q[0] == "status", "{q:?} in the blocked repo: {out}");
        assert_clean(&format!("{q:?} in locked-lib"), &out);
    }
    for q in [&["find", "ZqxVaultCipher", "--json"][..], &["context", "ZqxLockedType", "--json"], &["refs", "zqx_seal", "--json"]] {
        let (_, out) = env.run(&app, q);
        assert!(!out.contains("zqx_secret_store") && !out.contains("ZQXCONTENTMARK") && !out.contains("index.ts"), "{q:?}: {out}");
    }

    // Status lists what is blocked: the rule paths and the repo blocked whole, nothing below them.
    let all = env.json(&base, &["status", "--json"]);
    let by = |name: &str| all.as_array().unwrap().iter().find(|s| s["repo"].as_str().is_some_and(|r| r.ends_with(name))).cloned().unwrap_or(Value::Null);
    assert_eq!(by("/app")["privacy"]["blocked"], json!(["vault/"]), "{all}");
    assert_eq!(by("/locked-lib")["privacy"]["blocked"], "repo", "{all}");

    // Nothing blocked is stored anywhere in the cache.
    assert_eq!(env.cache_holds_marker(), Vec::<PathBuf>::new());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_dependency_on_a_blocked_repo_is_named_only_as_blocked() {
    let base = scratch("deps");
    let (app, lib) = fixture(&base, true);
    let env = Env::new(&base);
    assert_eq!(env.run(&base, &["index", app.to_str().unwrap(), lib.to_str().unwrap()]).0, 0);
    let v = env.json(&app, &["deps", "--json"]);
    let dep = &v["packages"]["app"][0]["deps"].as_array().unwrap().iter().find(|d| d["name"] == "locked-lib").cloned().unwrap();
    assert_eq!(dep["blocked"], "blocked by AI privacy policy", "{v}");
    let details: Vec<&str> = v["incomplete"].as_array().unwrap().iter().filter(|i| i["reason"] == "withheld").filter_map(|i| i["detail"].as_str()).collect();
    assert!(details.contains(&"AI privacy level 1 in locked-lib"), "{details:?}");
    let (_, text) = env.run(&app, &["deps"]);
    assert!(text.contains("dep locked-lib") && text.contains("(blocked by AI privacy policy)"), "{text}");
    assert_clean("deps", &text);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_privacy_file_added_later_purges_the_cache_on_the_next_query() {
    let base = scratch("later");
    let (app, _) = fixture(&base, false);
    let env = Env::new(&base);
    assert_eq!(env.run(&base, &["index", app.to_str().unwrap()]).0, 0);
    // A note on a symbol of a file about to be blocked: its text and recorded body are of blocked code.
    let (code, out) = env.run(&app, &["note", "add", "ZqxVaultCipher", "zqx: rotate monthly"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(env.run(&app, &["hotspots"]).0, 0);
    let (_, before) = env.run(&app, &["find", "ZqxVaultCipher"]);
    assert!(before.contains("vault/zqx_secret_store.py"), "{before}");
    assert!(!env.cache_holds_marker().is_empty(), "the unblocked index holds the file");

    write(&app.join(".knowcode-privacy.yml"), APP_PRIVACY);
    let (_, after) = env.run(&app, &["find", "shared_helper"]);
    assert_clean("find after the privacy file", &after);
    assert!(after.contains("src/helpers.py"), "{after}");
    assert_eq!(env.cache_holds_marker(), Vec::<PathBuf>::new(), "purged from the index, its caches, the summary and the notes");
    for q in [&["hotspots"][..], &["note", "list"], &["search", "seal"], &["locate", "vault cipher"]] {
        let (_, out) = env.run(&app, q);
        assert_clean(&format!("{q:?}"), &out);
    }
    assert_eq!(env.cache_holds_marker(), Vec::<PathBuf>::new());

    // And again after removing it: the vault comes back.
    std::fs::remove_file(app.join(".knowcode-privacy.yml")).unwrap();
    let (_, back) = env.run(&app, &["find", "ZqxVaultCipher"]);
    assert!(back.contains("vault/zqx_secret_store.py"), "{back}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_threshold_comes_from_the_config() {
    let base = scratch("threshold");
    let (app, _) = fixture(&base, false);
    write(&app.join(".knowcode-privacy.yml"), "rules:\n  - path: vault/\n    level: 2\n");
    let env = Env::new(&base);
    let find = |env: &Env| env.run(&app, &["find", "ZqxVaultCipher"]).1;
    assert!(find(&env).contains("vault/zqx_secret_store.py"), "level 2 is allowed by default");
    write(&env.config.join("config.toml"), "[privacy]\nblock_at = 2\n");
    assert_clean("block_at = 2", &find(&env));
    write(&env.config.join("config.toml"), "[privacy]\nblock_at = 0\n");
    assert!(find(&env).contains("vault/zqx_secret_store.py"), "0 blocks nothing");
    write(&env.config.join("config.toml"), "[privacy]\nblock_at = 9\n");
    let (code, out) = env.run(&app, &["find", "shared_helper"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("privacy.block_at"), "{out}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn malformed_and_empty_files() {
    let base = scratch("malformed");
    let (app, _) = fixture(&base, false);
    let env = Env::new(&base);
    // Empty: the default level, nothing blocked.
    write(&app.join(".knowcode-privacy.yml"), "");
    let (_, out) = env.run(&app, &["find", "ZqxVaultCipher", "--json"]);
    assert!(out.contains("vault/zqx_secret_store.py") && !out.contains("withheld"), "{out}");
    // Malformed: warned about, and the whole repo blocked until it is fixed.
    write(&app.join(".knowcode-privacy.yml"), "rules:\n  - path: vault/\n    level: one\n");
    let (code, out) = env.run(&app, &["find", "shared_helper"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("withheld: AI privacy level 1 in app") && out.contains("level"), "{out}");
    assert_eq!(env.cache_holds_marker(), Vec::<PathBuf>::new());
    let v = env.json(&app, &["status", "--json"]);
    assert_eq!(v["privacy"]["file"], "malformed", "{v}");
    assert_eq!(v["privacy"]["blocked"], "repo", "{v}");
    // Configured to take it as absent.
    write(&env.config.join("config.toml"), "[privacy]\nmalformed = \"default\"\n");
    let (code, out) = env.run(&app, &["find", "ZqxVaultCipher"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("vault/zqx_secret_store.py"), "{out}");
    let _ = std::fs::remove_dir_all(&base);
}

/// `status` always shows the policy it applies, indexed or not: the file's state, the name it looked for, the level,
/// the threshold and how many paths are blocked; and the CLI text says it in one line.
#[test]
fn status_always_shows_the_privacy_policy() {
    let base = scratch("status");
    let env = Env::new(&base);
    let rules: &'static str = "level: 3\nrules:\n  - path: secret/\n    level: 1\n  - path: keys.txt\n    level: 1\n";
    // (case, the privacy file's text if any, its state, the global level, blocked paths, whole repo blocked, text line)
    type Case = (&'static str, Option<&'static str>, &'static str, u8, usize, bool, &'static str);
    let cases: [Case; 3] = [
        ("absent", None, "absent", 3, 0, false, "privacy  .knowcode-privacy.yml absent; level 3, blocks at 1 and below; nothing blocked"),
        ("read", Some(rules), "read", 3, 2, false, "privacy  .knowcode-privacy.yml read; level 3, blocks at 1 and below; 2 paths blocked"),
        (
            "malformed",
            Some("rules: [unclosed\n"),
            "malformed",
            1,
            0,
            true,
            "privacy  .knowcode-privacy.yml malformed, taken as restricted; level 1, blocks at 1 and below; the whole repo blocked",
        ),
    ];
    for indexed in [false, true] {
        for (case, text, state, level, paths, whole, line) in cases {
            let repo = base.join(format!("{case}-{indexed}"));
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            write(&repo.join("src/main.py"), "def run():\n    return 1\n");
            write(&repo.join("secret/store.py"), "def seal():\n    return 2\n");
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-qm", "Add the app"]);
            if indexed {
                let (code, out) = env.run(&base, &["index", repo.to_str().unwrap()]);
                assert_eq!(code, 0, "{out}");
            }
            if let Some(text) = text {
                write(&repo.join(".knowcode-privacy.yml"), text);
            }
            let what = format!("{case}, indexed: {indexed}");
            let v = env.json(&repo, &["status", "--json"]);
            let p = &v["privacy"];
            assert_eq!(v["index"]["exists"] == json!(false), !indexed, "{what}: {v}");
            assert_eq!(p["file"], state, "{what}: {v}");
            assert_eq!(p["name"], ".knowcode-privacy.yml", "{what}: {v}");
            assert_eq!(p["global_level"], level, "{what}: {v}");
            assert_eq!(p["block_at"], 1, "{what}: {v}");
            assert_eq!(p["blocked_paths"], paths, "{what}: {v}");
            assert_eq!(p["blocked"] == "repo", whole, "{what}: {v}");
            let (code, out) = env.run(&repo, &["status"]);
            assert_eq!(code, 0, "{what}: {out}");
            assert!(out.lines().any(|l| l == line), "{what}: {out}");
        }
    }
    // A configured file name: the default-named file is not read, and status says which name it looked for.
    write(&env.config.join("config.toml"), "[privacy]\nfile = \".team-policy.yml\"\n");
    let repo = base.join("read-true");
    let v = env.json(&repo, &["status", "--json"]);
    assert_eq!(v["privacy"]["file"], "absent", "{v}");
    assert_eq!(v["privacy"]["name"], ".team-policy.yml", "{v}");
    assert_eq!(v["privacy"]["blocked_paths"], 0, "{v}");
    std::fs::rename(repo.join(".knowcode-privacy.yml"), repo.join(".team-policy.yml")).unwrap();
    let v = env.json(&repo, &["status", "--json"]);
    assert_eq!(v["privacy"]["file"], "read", "{v}");
    assert_eq!(v["privacy"]["name"], ".team-policy.yml", "{v}");
    assert_eq!(v["privacy"]["blocked_paths"], 2, "{v}");
    let (_, out) = env.run(&repo, &["status"]);
    assert!(out.contains("privacy  .team-policy.yml read; level 3, blocks at 1 and below; 2 paths blocked"), "{out}");
    let _ = std::fs::remove_dir_all(&base);
}

/// A line-delimited JSON-RPC client of `knowcode serve`.
struct Mcp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    id: u64,
}

impl Mcp {
    fn start(env: &Env, cwd: &Path) -> Mcp {
        let mut child = env.cmd(cwd, &["serve", "--tools", "all"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
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
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn invariant_no_mcp_tool_shows_blocked_content() {
    let base = scratch("mcp");
    let (app, lib) = fixture(&base, true);
    let env = Env::new(&base);
    assert_eq!(env.run(&base, &["index", app.to_str().unwrap(), lib.to_str().unwrap()]).0, 0);
    let mut mcp = Mcp::start(&env, &app);
    let tools: Vec<String> = mcp.call("tools/list", json!({}))["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str().map(String::from)).collect();
    let calls: Vec<(&str, Value)> = vec![
        ("kc_find", json!({ "query": "shared_helper" })),
        ("kc_find", json!({ "query": "Cipher" })),
        ("kc_locate", json!({ "query": "secret vault cipher seal" })),
        ("kc_locate", json!({ "query": "secret vault cipher seal", "workspace": true })),
        ("kc_task", json!({ "goal": "secret vault cipher seal shared helper" })),
        ("kc_symbol_at", json!({ "file": "src/main.py", "line": 4 })),
        ("kc_symbol_at", json!({ "file": "vault/zqx_secret_store.py", "line": 2 })),
        ("kc_refs", json!({ "name": "shared_helper" })),
        ("kc_refs", json!({ "names": ["shared_helper", "run_app"] })),
        ("kc_context", json!({ "symbol": "shared_helper" })),
        ("kc_context", json!({ "symbols": ["run_app", "shared_helper"] })),
        ("kc_trace", json!({ "symbol": "shared_helper", "direction": "in", "depth": 3 })),
        ("kc_path", json!({ "from": "run_app", "to": "shared_helper" })),
        ("kc_flow", json!({ "entry": "run_app" })),
        ("kc_impact", json!({ "symbol": "shared_helper" })),
        ("kc_impact", json!({ "diff": "HEAD~2", "format": "json" })),
        ("kc_rename_preview", json!({ "symbol": "shared_helper", "new_name": "common_helper" })),
        ("kc_unused", json!({})),
        ("kc_tests", json!({ "targets": ["src/helpers.py"] })),
        ("kc_tests", json!({ "diff": "HEAD~2" })),
        ("kc_coupled", json!({ "file": "src/helpers.py", "since": "all" })),
        ("kc_coupled", json!({ "file": "vault/zqx_secret_store.py" })),
        ("kc_hotspots", json!({})),
        ("kc_hotspots", json!({ "workspace": true })),
        ("kc_ownership", json!({})),
        ("kc_reviewers", json!({ "files": ["src/helpers.py"] })),
        ("kc_history", json!({ "symbol": "shared_helper" })),
        ("kc_overview", json!({})),
        ("kc_overview", json!({ "workspace": true })),
        ("kc_areas", json!({})),
        ("kc_notes", json!({})),
        ("kc_note_add", json!({ "symbol": "run_app", "text": "entry point" })),
        ("kc_search", json!({ "pattern": "MARK", "case": "insensitive" })),
        ("kc_search", json!({ "pattern": "seal", "workspace": true })),
        ("kc_grep_ast", json!({ "pattern": "def $F($$$): $$$", "lang": "python" })),
        ("kc_grep_ast", json!({ "pattern": "class $C { $$$ }", "lang": "typescript", "workspace": true })),
        ("kc_status", json!({})),
        ("kc_status", json!({ "workspace": true })),
        ("kc_find", json!({ "query": "shared_helper", "repo": "locked-lib" })),
        ("kc_more", json!({ "cursor": "kc1.notacursor" })),
        ("kc_more", json!({ "cursor": "kc~zz" })),
        ("kc_help", json!({})),
        ("kc_help", json!({ "topic": "privacy" })),
        ("kc_help", json!({ "topic": "kc_status" })),
    ];
    let called: BTreeSet<&str> = calls.iter().map(|(t, _)| *t).collect();
    let missing: Vec<&String> = tools.iter().filter(|t| !called.contains(t.as_str())).collect();
    assert!(missing.is_empty(), "tools with no privacy call: {missing:?}");
    let (mut withheld, mut answered) = (0, 0);
    for (tool, args) in &calls {
        let r = mcp.call("tools/call", json!({ "name": tool, "arguments": args }));
        let text = r.to_string();
        assert!(r.get("result").is_some() || r.get("error").is_some(), "{tool}: {r}");
        assert_clean(&format!("{tool} {args}"), &text);
        withheld += usize::from(text.contains("withheld"));
        answered += usize::from(r["result"]["isError"] != json!(true));
        // Every refusal has its code; a privacy refusal and every `withheld` entry carry the same policy.
        let policy = |p: &Value, what: &str| {
            assert_eq!(p["file"], json!(".knowcode-privacy.yml"), "{tool} {args} {what}: {r}");
            assert_eq!(p["block_at"], json!(1), "{tool} {args} {what}: {r}");
            let whole = p["repo"] == json!("locked-lib");
            assert!(whole || p["repo"] == json!("app"), "{tool} {args} {what}: {r}");
            assert_eq!(p["blocks"], json!(if whole { "repo" } else { "paths" }), "{tool} {args} {what}: {r}");
        };
        if r["result"]["isError"] == json!(true) {
            let e = &r["result"]["structuredContent"]["error"];
            let body = r["result"]["content"][0]["text"].as_str().unwrap_or_default();
            if body.starts_with("withheld") {
                assert_eq!(e["code"], json!("withheld"), "{tool} {args}: {r}");
                policy(&e["privacy"], "refusal");
            } else {
                assert!(e["code"].is_string() && e["code"] != json!("internal"), "{tool} {args}: {r}");
            }
        } else if let Ok(v) = serde_json::from_str::<Value>(r["result"]["content"][0]["text"].as_str().unwrap_or_default()) {
            fn entries(v: &Value, out: &mut Vec<Value>) {
                match v {
                    Value::Object(m) => {
                        for (k, x) in m {
                            if k == "incomplete" && let Some(list) = x.as_array() {
                                out.extend(list.iter().filter(|e| e["reason"] == json!("withheld")).cloned());
                            }
                            entries(x, out);
                        }
                    }
                    Value::Array(a) => a.iter().for_each(|x| entries(x, out)),
                    _ => {}
                }
            }
            let mut found = Vec::new();
            entries(&v, &mut found);
            for e in &found {
                policy(&e["privacy"], "entry");
            }
        }
        if *tool == "kc_find" && args["query"] == "shared_helper" && args.get("repo").is_none() {
            assert!(text.contains("src/helpers.py"), "{text}");
        }
    }
    assert!(withheld >= calls.len() / 2, "{withheld}");
    // The refused ones: two blocked paths, the repo blocked whole, the bad cursor and the unknown handle.
    assert!(answered + 5 == calls.len(), "{answered} of {} answered", calls.len());
    drop(mcp);
    assert_eq!(env.cache_holds_marker(), Vec::<PathBuf>::new());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_new_checkout_of_a_repo_with_blocked_paths_is_never_seeded_with_them() {
    let base = scratch("seed");
    let (app, _) = fixture(&base, false);
    let env = Env::new(&base);
    git(&app, &["remote", "add", "origin", "https://example.test/acme/app.git"]);
    assert_eq!(env.run(&base, &["index", app.to_str().unwrap()]).0, 0);
    // A worktree on a branch whose privacy file blocks the vault: its index is not a copy of the main checkout's.
    let wt = base.join("app-wt");
    git(&app, &["worktree", "add", "-q", "-b", "private", wt.to_str().unwrap()]);
    write(&wt.join(".knowcode-privacy.yml"), APP_PRIVACY);
    let (_, out) = env.run(&wt, &["find", "shared_helper", "--json"]);
    assert_clean("find in the new worktree", &out);
    let db = env.cache.join("repos");
    for (p, text) in env.cache_files() {
        if p.starts_with(&db) && p.file_name().unwrap().to_string_lossy().starts_with("app-wt") {
            assert!(!text.contains(MARK), "{} holds blocked content", p.display());
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_provenance_record_never_names_blocked_changes() {
    let base = scratch("record");
    let (app, _) = fixture(&base, true);
    let env = Env::new(&base);
    // Uncommitted: an allowed file and a blocked one.
    write(&app.join("src/helpers.py"), "def shared_helper():\n    return 3\n");
    write(&app.join("vault/zqx_secret_store.py"), "class ZqxVaultCipher:\n    pass\n");
    let v = env.json(&app, &["find", "shared_helper", "--provenance"]);
    assert_clean("find --provenance", &v.to_string());
    let repo = &v["record"]["repos"][0];
    assert_eq!(repo["uncommitted"]["files"], json!(1), "{v}");
    assert!(repo["privacy"]["key"].as_str().unwrap().starts_with("blake3:"), "{v}");
    assert!(repo["privacy"]["file"].as_str().unwrap().starts_with("blake3:"), "{v}");
    assert_eq!(repo["commit"].as_str().unwrap().len(), 40, "{v}");
    // The same question gives the same evidence; the uncommitted digest moves with the content, blocked or not only
    // when allowed content moves.
    let again = env.json(&app, &["find", "shared_helper", "--provenance"]);
    assert_eq!(again["record"]["answer"]["evidence"], v["record"]["answer"]["evidence"]);
    write(&app.join("vault/zqx_secret_store.py"), "class ZqxVaultCipher:\n    zqx = 2\n");
    let blocked_edit = env.json(&app, &["find", "shared_helper", "--provenance"]);
    assert_eq!(blocked_edit["record"]["repos"][0]["uncommitted"], repo["uncommitted"]);
    write(&app.join("src/helpers.py"), "def shared_helper():\n    return 4\n");
    let edited = env.json(&app, &["find", "shared_helper", "--provenance"]);
    assert_ne!(edited["record"]["repos"][0]["uncommitted"]["digest"], repo["uncommitted"]["digest"]);
    // Over MCP: the answer as it is, and the record in a block of its own.
    let mut mcp = Mcp::start(&env, &app);
    let r = mcp.call("tools/call", json!({ "name": "kc_find", "arguments": { "query": "shared_helper", "provenance": true } }));
    assert_clean("kc_find provenance", &r.to_string());
    let content = r["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{r}");
    let record: Value = serde_json::from_str(content[1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(record["record"]["answer"]["bytes"], json!(content[0]["text"].as_str().unwrap().len()));
    assert_eq!(record["record"]["call"]["query"], json!({ "tool": "kc_find", "arguments": { "query": "shared_helper" } }));
    let plain = mcp.call("tools/call", json!({ "name": "kc_find", "arguments": { "query": "shared_helper" } }));
    assert_eq!(plain["result"]["content"].as_array().unwrap().len(), 1, "{plain}");
    drop(mcp);
    let _ = std::fs::remove_dir_all(&base);
}
