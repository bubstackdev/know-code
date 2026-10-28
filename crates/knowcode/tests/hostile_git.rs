//! A repo whose own git config names commands for git to run, and an environment pointing git at another repo: KnowCode
//! indexes and answers about the repo it is asked about, and runs none of those commands.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-hostile-git-{name}-{}", std::process::id()));
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
    assert!(ok, "git {args:?} in {}", dir.display());
}

fn repo(dir: &Path, file: &str, text: &str) -> String {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "Add it"]);
    let out = Command::new("git").arg("-C").arg(dir).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn kc(base: &Path, cwd: &Path, env: &[(&str, &Path)], args: &[&str]) -> String {
    let mut cmd = Command::new(KC);
    cmd.args(args)
        .current_dir(cwd)
        .env("KNOWCODE_CACHE_DIR", base.join("cache"))
        .env("KNOWCODE_CONFIG_DIR", base.join("config"))
        .env("KNOWCODE_DATA_DIR", base.join("data"))
        .env("KNOWCODE_UPDATE_CHECK", "0")
        .env_remove("KNOWCODE_VERIFY");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_repos_own_git_config_never_runs_a_command() {
    let base = scratch("config");
    std::fs::create_dir_all(base.join("config")).unwrap();
    let app = base.join("app");
    repo(&app, "lib.rs", "pub fn greet() -> u32 { 1 }\n");
    // Commands the repo's config names: a file system monitor (`git status` would run it) and hooks.
    let marker = base.join("ran");
    let script = base.join("record.sh");
    std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&app, &["config", "core.fsmonitor", script.to_str().unwrap()]);
    let hooks = base.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in ["post-checkout", "post-index-change", "reference-transaction"] {
        std::fs::copy(&script, hooks.join(hook)).unwrap();
    }
    git(&app, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    // Uncommitted, so syncing asks git what changed.
    std::fs::write(app.join("lib.rs"), "pub fn greet() -> u32 { 2 }\n").unwrap();

    kc(&base, &base, &[], &["index", app.to_str().unwrap()]);
    let found = kc(&base, &app, &[], &["find", "greet", "--json"]);
    assert!(found.contains("lib.rs"), "{found}");
    kc(&base, &app, &[], &["status", "--json"]);
    kc(&base, &app, &[], &["ownership", "lib.rs", "--json"]);
    assert!(!marker.exists(), "a command the repo's git config names ran");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn an_inherited_git_dir_never_points_git_at_another_repo() {
    let base = scratch("env");
    std::fs::create_dir_all(base.join("config")).unwrap();
    let app = base.join("app");
    let head = repo(&app, "lib.rs", "pub fn greet() -> u32 { 1 }\n");
    let other = base.join("other");
    let other_head = repo(&other, "other.rs", "pub fn elsewhere() -> u32 { 1 }\n");
    assert_ne!(head, other_head);
    let other_git = other.join(".git");
    let env: &[(&str, &Path)] = &[("GIT_DIR", &other_git), ("GIT_WORK_TREE", &other)];
    let v: Value = serde_json::from_str(&kc(&base, &app, env, &["find", "greet", "--json"])).unwrap();
    assert_eq!(v["total"], 1, "{v}");
    assert!(head.starts_with(v["commit"].as_str().unwrap()), "{v}");
    let _ = std::fs::remove_dir_all(&base);
}
