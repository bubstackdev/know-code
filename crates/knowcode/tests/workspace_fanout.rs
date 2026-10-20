//! A workspace-wide answer survives one repo that can't be read: it answers with the other repos and names the one
//! left out (`missing_repo` for a checkout gone while the answer was worked out, `failed` for anything else).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-fanout-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} in {}", dir.display());
}

/// A repo at `dir` with one commit defining a Swift function `sharedName<suffix>`.
fn repo(dir: &Path, suffix: &str) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    fs::write(dir.join("a.swift"), format!("func sharedName{suffix}() {{}}\n")).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

/// Where runs keep their index and read their config.
struct Env {
    base: PathBuf,
    path: Option<String>,
}

impl Env {
    fn run(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let mut cmd = Command::new(KC);
        cmd.args(args)
            .current_dir(cwd)
            .env("KNOWCODE_CACHE_DIR", self.base.join("cache"))
            .env("KNOWCODE_CONFIG_DIR", self.base.join("config"))
            .env("KNOWCODE_DATA_DIR", self.base.join("data"))
            .env("KNOWCODE_UPDATE_CHECK", "0")
            // Every repo lists its files with `git`, whatever the cache holds.
            .env("KNOWCODE_VERIFY", "1");
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
        let out = cmd.output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn index(&self, root: &Path) {
        let (code, _, err) = self.run(&self.base, &["index", root.to_str().unwrap(), "--threads", "1"]);
        assert_eq!(code, 0, "index {}: {err}", root.display());
    }

    /// `find sharedName --json`, run outside every repo: the whole workspace.
    fn find(&self) -> Value {
        let (code, out, err) = self.run(&self.base.join("elsewhere"), &["find", "sharedName", "--json"]);
        assert_eq!(code, 0, "a workspace-wide find failed: {err}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    fn configure(&self, roots: &Path, depth: usize) {
        fs::create_dir_all(self.base.join("config")).unwrap();
        let toml = format!("[workspace]\nroots = [{:?}]\nmax_depth = {depth}\n", roots.to_str().unwrap());
        fs::write(self.base.join("config/config.toml"), toml).unwrap();
    }
}

fn repos_found(v: &Value) -> Vec<String> {
    let mut r: Vec<String> = v["items"].as_array().unwrap().iter().map(|i| i["repo"].as_str().unwrap_or_default().to_string()).collect();
    r.sort();
    r.dedup();
    r
}

fn incomplete(v: &Value, reason: &str) -> Vec<String> {
    v["incomplete"].as_array().into_iter().flatten().filter(|i| i["reason"] == reason).map(|i| i["detail"].as_str().unwrap().to_string()).collect()
}

/// The `git` on `PATH`.
fn real_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|d| d.join("git")).find(|p| p.is_file()).expect("git on PATH")
}

#[test]
fn a_worktree_deleted_under_a_workspace_root_leaves_the_rest_of_the_answer() {
    let base = scratch("gone");
    let ws = base.join("ws");
    let (app, lib) = (ws.join("app"), ws.join("lib"));
    repo(&app, "App");
    repo(&lib, "Lib");
    fs::create_dir_all(base.join("elsewhere")).unwrap();
    let wt = app.join(".worktrees/feature");
    git(&app, &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feature"]);
    let mut env = Env { base: base.clone(), path: None };
    env.configure(&ws, 4);
    for r in [&app, &lib, &wt] {
        env.index(r);
    }
    let before = env.find();
    assert_eq!(repos_found(&before), ["app", "feature", "lib"], "{before}");

    // Deleted before the query, with the config changed so discovery runs again, in a fresh process: the gone
    // checkout is no repo of the workspace any more.
    fs::remove_dir_all(&wt).unwrap();
    env.configure(&ws, 5);
    let after = env.find();
    assert_eq!(repos_found(&after), ["app", "lib"], "{after}");

    // Deleted while the answer is worked out, after the workspace was listed: here when the first repo lists its
    // files. The answer is the other repos', and names the one left out.
    git(&app, &["worktree", "prune"]);
    git(&app, &["worktree", "add", "-q", wt.to_str().unwrap(), "feature"]);
    env.index(&wt);
    env.configure(&ws, 4);
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\nif [ \"$3\" = {:?} ] && [ \"$4\" = ls-files ]; then rm -rf {:?}; fi\nexec {:?} \"$@\"\n",
        app.to_str().unwrap(),
        wt.to_str().unwrap(),
        real_git().to_str().unwrap()
    );
    fs::write(bin.join("git"), script).unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap()))).unwrap();
    env.path = Some(path.to_string_lossy().into_owned());
    let during = env.find();
    assert!(!wt.exists(), "the checkout was deleted during the query");
    assert_eq!(repos_found(&during), ["app", "lib"], "{during}");
    assert_eq!(during["complete"], false, "{during}");
    let missing = incomplete(&during, "missing_repo");
    assert!(missing.iter().any(|d| d.starts_with("feature: ") && d.contains(wt.to_str().unwrap())), "{during}");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn a_repo_whose_git_fails_leaves_the_rest_of_the_answer() {
    let base = scratch("perm");
    let ws = base.join("ws");
    let (app, lib, svc) = (ws.join("app"), ws.join("lib"), ws.join("svc"));
    repo(&app, "App");
    repo(&lib, "Lib");
    repo(&svc, "Svc");
    fs::create_dir_all(base.join("elsewhere")).unwrap();
    let env = Env { base: base.clone(), path: None };
    env.configure(&ws, 4);
    for r in [&app, &lib, &svc] {
        env.index(r);
    }
    // `git` can't read the repo's index file.
    let index = lib.join(".git/index");
    fs::set_permissions(&index, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&index).is_ok() {
        // Permissions don't hold for this user (root): nothing to test.
        fs::set_permissions(&index, fs::Permissions::from_mode(0o644)).unwrap();
        let _ = fs::remove_dir_all(base);
        return;
    }
    let v = env.find();
    fs::set_permissions(&index, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(repos_found(&v), ["app", "svc"], "{v}");
    assert_eq!(v["complete"], false, "{v}");
    let failed = incomplete(&v, "failed");
    assert!(failed.iter().any(|d| d.starts_with("lib: ") && d.contains("git ")), "{v}");
    assert!(incomplete(&v, "missing_repo").is_empty(), "{v}");
    let _ = fs::remove_dir_all(base);
}
