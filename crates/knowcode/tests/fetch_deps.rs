//! `fetch-deps` and `history deepen`, against local bare repos standing in for remotes. The manifests name
//! `https://example.test/…` remotes; git's own `url.<base>.insteadOf`, in a git config only these runs read, sends
//! them to `file://` bare repos in the test's directory. KnowCode itself refuses `file://` remotes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-fetch-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

struct Env {
    base: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let base = scratch(name);
        let remotes = base.join("remotes");
        fs::create_dir_all(&remotes).unwrap();
        let config = format!("[url \"file://{}/\"]\n\tinsteadOf = https://example.test/\n", remotes.display());
        fs::write(base.join("gitconfig"), config).unwrap();
        Env { base }
    }

    fn git(&self, dir: &Path, args: &[&str], date: Option<&str>) {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-c", "init.defaultBranch=main"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", self.base.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1");
        if let Some(d) = date {
            cmd.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr));
    }

    /// A bare repo at `remotes/<path>.git` holding `files`, one commit per `dates` entry (the last one adds the files).
    fn remote(&self, path: &str, files: &[(&str, &str)], dates: &[&str]) -> PathBuf {
        let work = self.base.join("work").join(path);
        fs::create_dir_all(&work).unwrap();
        self.git(&work, &["init", "-q"], None);
        for (i, d) in dates.iter().enumerate() {
            fs::write(work.join("log.txt"), format!("{i}\n")).unwrap();
            if i + 1 == dates.len() {
                for (f, text) in files {
                    fs::write(work.join(f), text).unwrap();
                }
            }
            self.git(&work, &["add", "-A"], None);
            self.git(&work, &["commit", "-q", "-m", &format!("change {i}")], Some(d));
        }
        let bare = self.base.join("remotes").join(format!("{path}.git"));
        fs::create_dir_all(bare.parent().unwrap()).unwrap();
        self.git(&self.base, &["clone", "-q", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()], None);
        bare
    }

    /// A checkout at `ws/<name>` with `files`, its origin `https://example.test/team/<name>.git`.
    fn repo(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = self.base.join("ws").join(name);
        fs::create_dir_all(&dir).unwrap();
        self.git(&dir, &["init", "-q"], None);
        for (f, text) in files {
            fs::write(dir.join(f), text).unwrap();
        }
        self.git(&dir, &["add", "-A"], None);
        self.git(&dir, &["commit", "-q", "-m", "init"], None);
        self.git(&dir, &["remote", "add", "origin", &format!("https://example.test/team/{name}.git")], None);
        dir
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(KC)
            .args(args)
            .current_dir(cwd)
            .env("KNOWCODE_CACHE_DIR", self.base.join("cache"))
            .env("KNOWCODE_CONFIG_DIR", self.base.join("config"))
            .env("KNOWCODE_DATA_DIR", self.base.join("data"))
            .env("KNOWCODE_UPDATE_CHECK", "0")
            .env("GIT_CONFIG_GLOBAL", self.base.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn json(&self, cwd: &Path, args: &[&str]) -> Value {
        let (code, out, err) = self.run(cwd, args);
        assert_eq!(code, 0, "{args:?}: {err}{out}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }
}

const PACKAGE: &str = r#"// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "App",
    dependencies: [
        .package(url: "https://example.test/group/lib.git", from: "1.0.0"),
        .package(url: "https://example.test/group/locked.git", from: "1.0.0"),
        .package(url: "http://example.test/group/plain.git", from: "1.0.0"),
        .package(url: "file:///srv/git/local.git", from: "1.0.0"),
    ],
    targets: [
        .target(name: "App", dependencies: [.product(name: "Lib", package: "lib")]),
    ]
)
"#;

fn item<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["items"].as_array().unwrap().iter().find(|i| i["name"] == name).unwrap_or_else(|| panic!("no item {name}: {v}"))
}

fn reasons(v: &Value) -> Vec<String> {
    v["incomplete"].as_array().into_iter().flatten().map(|i| i["reason"].as_str().unwrap().to_string()).collect()
}

fn fixture(name: &str) -> (Env, PathBuf) {
    let env = Env::new(name);
    env.remote("group/lib", &[("Lib.swift", "public func lib() {}\n")], &["2024-01-01T00:00:00Z"]);
    env.remote("group/locked", &[(".knowcode-privacy.yml", "level: 1\n"), ("Secret.swift", "func secret() {}\n")], &["2024-01-01T00:00:00Z"]);
    let app = env.repo("app", &[("Package.swift", PACKAGE), ("App.swift", "func app() {}\n")]);
    (env, app)
}

#[test]
fn dry_run_prints_the_plan_and_clones_nothing() {
    let (env, app) = fixture("dry");
    let into = env.base.join("deps");
    let v = env.json(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--dry-run", "--json"]);
    assert_eq!(v["dry_run"], true);
    let lib = item(&v, "lib");
    assert_eq!(lib["status"], "would_clone", "{v}");
    assert_eq!(lib["url"], "https://example.test/group/lib.git");
    assert_eq!(lib["path"], into.join("lib").to_str().unwrap());
    assert_eq!(lib["sites"][0]["file"], "Package.swift");
    assert_eq!(lib["sites"][0]["via"], "manifest");
    // Its policy is only known once cloned: the plan lists it.
    assert_eq!(item(&v, "locked")["status"], "would_clone");
    for refused in ["http://example.test/group/plain.git", "file:///srv/git/local.git"] {
        let i = v["items"].as_array().unwrap().iter().find(|i| i["url"] == refused).unwrap_or_else(|| panic!("{refused}: {v}"));
        assert_eq!(i["status"], "refused", "{i}");
    }
    assert!(reasons(&v).contains(&"unsupported".to_string()), "{v}");
    assert!(!into.exists(), "a dry run creates nothing");
    let (code, text, _) = env.run(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--dry-run"]);
    assert_eq!(code, 0);
    assert!(text.contains("would clone") && text.contains("https://example.test/group/lib.git") && text.contains("dry run"), "{text}");
}

#[test]
fn clones_shallow_skips_a_blocked_repo_and_finds_them_present_again() {
    let (env, app) = fixture("clone");
    let into = env.base.join("deps");
    let v = env.json(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--json"]);
    let lib = item(&v, "lib");
    assert_eq!(lib["status"], "cloned", "{v}");
    assert!(into.join("lib/Lib.swift").is_file(), "checked out");
    assert!(into.join("lib/.git/shallow").is_file(), "shallow by default");
    let locked = item(&v, "locked");
    assert_eq!(locked["status"], "blocked", "{v}");
    assert!(!into.join("locked").exists(), "a blocked clone is removed");
    assert_eq!(v["complete"], false);
    let r = reasons(&v);
    assert!(r.contains(&"withheld".to_string()) && r.contains(&"unsupported".to_string()), "{r:?}");
    // No roots configured: the clones are to be indexed.
    assert_eq!(v["workspace"]["status"], "no_roots", "{v}");
    // The app repo is untouched.
    let status = Command::new("git").arg("-C").arg(&app).args(["status", "--porcelain"]).output().unwrap();
    assert!(status.stdout.is_empty(), "{}", String::from_utf8_lossy(&status.stdout));

    let again = env.json(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--json"]);
    assert_eq!(item(&again, "lib")["status"], "present", "{again}");

    let with = env.json(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--include-blocked", "--json"]);
    assert_eq!(item(&with, "locked")["status"], "cloned", "{with}");
    assert!(into.join("locked/Secret.swift").is_file());
}

/// [`fixture`], with `ws` (where the app is) the one workspace root.
fn rooted(name: &str) -> (Env, PathBuf) {
    let (env, app) = fixture(name);
    fs::create_dir_all(env.base.join("config")).unwrap();
    let ws = env.base.join("ws");
    fs::write(env.base.join("config/config.toml"), format!("[workspace]\nroots = [{:?}]\nmax_depth = 2\n", ws.to_str().unwrap())).unwrap();
    (env, app)
}

#[test]
fn full_clones_outside_the_roots_say_how_to_add_them() {
    let (env, app) = rooted("outside");
    let into = env.base.join("deps");
    let v = env.json(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--full", "--json"]);
    assert_eq!(item(&v, "lib")["status"], "cloned", "{v}");
    assert!(!into.join("lib/.git/shallow").exists(), "--full clones all history");
    assert_eq!(v["workspace"]["status"], "outside_roots", "{v}");
    assert!(v["workspace"]["hint"].as_str().unwrap().contains("knowcode workspace add"), "{v}");
}

#[test]
fn clones_under_a_root_need_nothing_more() {
    let (env, app) = rooted("under");
    let v = env.json(&app, &["fetch-deps", "--into", env.base.join("ws/deps").to_str().unwrap(), "--json"]);
    assert_eq!(item(&v, "lib")["status"], "cloned", "{v}");
    assert_eq!(v["workspace"]["status"], "under_root", "{v}");
}

#[test]
fn add_root_adds_the_directory() {
    let (env, app) = rooted("add");
    let other = env.base.join("more");
    let v = env.json(&app, &["fetch-deps", "--into", other.to_str().unwrap(), "--add-root", "--dry-run", "--json"]);
    assert_eq!(v["workspace"]["status"], "would_add", "{v}");
    let v = env.json(&app, &["fetch-deps", "--into", other.to_str().unwrap(), "--add-root", "--json"]);
    assert_eq!(v["workspace"]["status"], "added", "{v}");
    let config = fs::read_to_string(env.base.join("config/config.toml")).unwrap();
    assert!(config.contains(other.to_str().unwrap()), "{config}");
}

#[test]
fn refuses_a_directory_inside_a_repo_and_needs_into() {
    let (env, app) = fixture("refuse");
    for into in [app.join("deps"), app.join("src/deeper")] {
        let (code, _, err) = env.run(&app, &["fetch-deps", "--into", into.to_str().unwrap()]);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("never writes into a repo"), "{err}");
        assert!(!into.exists());
    }
    let (code, _, err) = env.run(&app, &["fetch-deps"]);
    assert_eq!(code, 2, "--into is required: {err}");
    // A directory of the dependency's name that is something else: left alone.
    let into = env.base.join("deps");
    fs::create_dir_all(into.join("lib")).unwrap();
    fs::write(into.join("lib/mine.txt"), "mine\n").unwrap();
    let v = env.json(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--json"]);
    assert_eq!(item(&v, "lib")["status"], "skipped", "{v}");
    assert!(reasons(&v).contains(&"failed".to_string()), "{v}");
    assert_eq!(fs::read_dir(into.join("lib")).unwrap().count(), 1);
    let elsewhere = env.base.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let (code, _, err) = env.run(&elsewhere, &["fetch-deps", "--into", env.base.join("deps").to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.contains("not inside a git repo"), "{err}");
}

#[test]
fn ci_includes_terraform_sources_and_gone_checkouts_resolve_too() {
    let env = Env::new("sources");
    env.remote("infra/ci-templates", &[("build.yml", "build:\n  script: [make]\n")], &["2024-01-01T00:00:00Z"]);
    env.remote("team/lib", &[("Lib.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><PackageId>Acme.Lib</PackageId></PropertyGroup></Project>\n")], &["2024-01-01T00:00:00Z"]);
    // A checkout of the library, indexed and then deleted: answers name it `missing_repo`.
    let lib = env.repo("lib", &[("Lib.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><PackageId>Acme.Lib</PackageId></PropertyGroup></Project>\n")]);
    let app = env.repo(
        "app",
        &[
            (".gitlab-ci.yml", "include:\n  - project: 'infra/ci-templates'\n    file: '/build.yml'\n"),
            ("main.tf", "module \"vpc\" {\n  source = \"git::https://example.test/infra/tf-vpc.git//modules/vpc?ref=v1\"\n}\n"),
            ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup><PackageReference Include=\"Acme.Lib\" Version=\"1.0.0\" /></ItemGroup></Project>\n"),
        ],
    );
    let (code, _, err) = env.run(&env.base, &["index", lib.to_str().unwrap(), app.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    let into = env.base.join("deps");
    let v = env.json(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--dry-run", "--json"]);
    // While its checkout is there, the package dependency lands in it: nothing to fetch.
    assert!(v["items"].as_array().unwrap().iter().all(|i| i["name"] != "lib"), "{v}");
    assert_eq!(v["registry_dependencies"], 0, "{v}");
    let ci = item(&v, "ci-templates");
    assert_eq!((ci["status"].as_str(), ci["url"].as_str(), ci["sites"][0]["via"].as_str()), (Some("would_clone"), Some("https://example.test/infra/ci-templates.git"), Some("ci_include")), "{v}");
    let tf = item(&v, "tf-vpc");
    assert_eq!((tf["url"].as_str(), tf["sites"][0]["via"].as_str()), (Some("https://example.test/infra/tf-vpc.git"), Some("terraform")), "{v}");

    // Deleted: until cache maintenance drops its index (after the next command), it resolves by its origin.
    fs::remove_dir_all(&lib).unwrap();
    let v = env.json(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--json"]);
    let gone = item(&v, "lib");
    assert_eq!((gone["status"].as_str(), gone["url"].as_str(), gone["sites"][0]["via"].as_str()), (Some("cloned"), Some("https://example.test/team/lib.git"), Some("missing_repo")), "{v}");
    assert_eq!(item(&v, "ci-templates")["status"], "cloned", "{v}");
    // No such remote: the clone fails, said as `failed`, and the rest are still cloned.
    assert_eq!(item(&v, "tf-vpc")["status"], "failed", "{v}");
    assert!(reasons(&v).contains(&"failed".to_string()), "{v}");
    assert!(into.join("lib/Lib.csproj").is_file() && into.join("ci-templates/build.yml").is_file());
    assert!(!into.join("tf-vpc").exists());
}

#[test]
fn nothing_to_fetch_is_a_no_op() {
    let env = Env::new("noop");
    let app = env.repo("plain", &[("App.swift", "func app() {}\n")]);
    let into = env.base.join("deps");
    let v = env.json(&app, &["fetch-deps", "--into", into.to_str().unwrap(), "--json"]);
    assert_eq!(v["items"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(v["complete"], true, "{v}");
    assert!(v["note"].as_str().unwrap().contains("nothing to clone"));
    assert!(!into.exists());
}

#[test]
fn history_deepen_reports_depth_before_and_after() {
    let env = Env::new("deepen");
    let dates = ["2023-01-01T00:00:00Z", "2023-06-01T00:00:00Z", "2024-01-01T00:00:00Z", "2024-03-01T00:00:00Z", "2024-04-01T00:00:00Z"];
    let bare = env.remote("group/hist", &[("a.txt", "a\n")], &dates);
    let clone = env.base.join("clones/hist");
    fs::create_dir_all(clone.parent().unwrap()).unwrap();
    env.git(&env.base, &["clone", "-q", "--depth", "1", &format!("file://{}", bare.display()), clone.to_str().unwrap()], None);
    let head_before = fs::read_to_string(clone.join(".git/HEAD")).unwrap();

    let v = env.json(&clone, &["history", "deepen", "--by", "2", "--json"]);
    assert_eq!(v["before"]["commits"], 1, "{v}");
    assert_eq!(v["before"]["counted"], 0, "{v}");
    assert_eq!(v["fetched"], "--deepen=2");
    assert_eq!(v["after"]["commits"], 3, "{v}");
    assert_eq!(v["after"]["shallow"], true);
    assert_eq!(v["after"]["since"], "2024-03-01", "{v}");
    let status = |v: &Value, a: &str| v["answers"].as_array().unwrap().iter().find(|x| x["answer"] == a).unwrap()["status"].as_str().unwrap().to_string();
    assert_eq!(status(&v, "hotspots"), "partial", "{v}");
    assert_eq!(status(&v, "ownership"), "partial");
    assert_eq!(v["complete"], true);

    let v = env.json(&clone, &["history", "deepen", "--json"]);
    assert_eq!(v["fetched"], "--deepen=1000");
    assert_eq!(v["after"]["shallow"], false, "{v}");
    assert_eq!(v["after"]["commits"], 5);
    for a in ["hotspots", "coupled", "ownership", "reviewers"] {
        assert_eq!(status(&v, a), "complete", "{a}: {v}");
    }
    assert_eq!(fs::read_to_string(clone.join(".git/HEAD")).unwrap(), head_before, "HEAD is not moved");

    // A full clone: said, and nothing fetched.
    let v = env.json(&clone, &["history", "deepen", "--json"]);
    assert!(v.get("fetched").is_none(), "{v}");
    assert!(v["note"].as_str().unwrap().contains("not a shallow clone"), "{v}");
    let (code, text, _) = env.run(&clone, &["history", "deepen"]);
    assert_eq!(code, 0);
    assert!(text.contains("nothing to deepen"), "{text}");

    let (code, _, err) = env.run(&clone, &["history", "deepen", "--by", "3", "--since", "2024-01-01"]);
    assert_eq!(code, 2, "--by and --since conflict: {err}");
}

#[test]
fn history_deepen_since_a_date() {
    let env = Env::new("since");
    let dates = ["2023-01-01T00:00:00Z", "2023-06-01T00:00:00Z", "2024-01-01T00:00:00Z", "2024-03-01T00:00:00Z", "2024-04-01T00:00:00Z"];
    let bare = env.remote("group/hist", &[("a.txt", "a\n")], &dates);
    let clone = env.base.join("clones/hist");
    fs::create_dir_all(clone.parent().unwrap()).unwrap();
    env.git(&env.base, &["clone", "-q", "--depth", "1", &format!("file://{}", bare.display()), clone.to_str().unwrap()], None);
    let v = env.json(&clone, &["history", "deepen", "--since", "2023-05-01", "--json"]);
    assert_eq!(v["fetched"], "--shallow-since=2023-05-01", "{v}");
    assert_eq!(v["after"]["commits"], 4, "{v}");
}
