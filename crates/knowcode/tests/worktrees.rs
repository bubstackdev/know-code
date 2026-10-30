//! Worktrees of one repo indexed side by side: an answer about one never depends on how many of the others are
//! indexed.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-worktrees-{name}-{}", std::process::id()));
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
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} in {}", dir.display());
}

fn kc(base: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(KC)
        .args(args)
        .current_dir(cwd)
        .env("KNOWCODE_CACHE_DIR", base.join("cache"))
        .env("KNOWCODE_CONFIG_DIR", base.join("config"))
        .env("KNOWCODE_DATA_DIR", base.join("data"))
        .env_remove("KNOWCODE_VERIFY")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn unused_never_counts_another_worktree_as_a_user_of_its_packages() {
    let base = scratch("unused");
    std::fs::create_dir_all(base.join("config")).unwrap();
    let main = base.join("shop");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q"]);
    write(&main.join("packages/core/package.json"), "{ \"name\": \"@shop/core\", \"version\": \"1.0.0\" }\n");
    write(&main.join("packages/core/src/price.ts"), "export function roundPrice(p: number): number { return Math.round(p); }\n");
    write(&main.join("packages/web/package.json"), "{ \"name\": \"@shop/web\", \"dependencies\": { \"@shop/core\": \"^1.0.0\" } }\n");
    write(&main.join("packages/web/src/cart.ts"), "export class Cart { total(): number { return 1; } }\n");
    git(&main, &["remote", "add", "origin", "https://example.test/group/shop.git"]);
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-qm", "Add the shop"]);
    let alone: Value = serde_json::from_str(&kc(&base, &main, &["unused", "--json"])).unwrap();

    // A worktree of the same repo, indexed too: its `@shop/web` depends on `@shop/core` as this checkout's does.
    let wt = base.join("shop-wt");
    git(&main, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);
    kc(&base, &wt, &["index", wt.to_str().unwrap()]);
    let beside: Value = serde_json::from_str(&kc(&base, &main, &["unused", "--json"])).unwrap();
    assert_eq!(alone.get("published"), beside.get("published"), "{beside}");
    assert_eq!(alone["items"], beside["items"], "{beside}");
    let _ = std::fs::remove_dir_all(&base);
}

/// The index files in the cache, by name.
fn indexes(base: &Path) -> Vec<String> {
    // In the build's own tree, `cache/v/<key>/repos`: the only one these runs make.
    let trees: Vec<std::path::PathBuf> = std::fs::read_dir(base.join("cache/v")).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    let mut out: Vec<String> = trees.iter().flat_map(|t| std::fs::read_dir(t.join("repos")).into_iter().flatten().flatten())
        .map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".sqlite")).collect();
    out.sort();
    out
}

#[test]
fn a_worktrees_index_goes_with_the_worktree() {
    let base = scratch("gone");
    std::fs::create_dir_all(base.join("config")).unwrap();
    let main = base.join("depot");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q"]);
    write(&main.join("src/shelf.ts"), "export class Shelf { count(): number { return 1; } }\n");
    git(&main, &["remote", "add", "origin", "https://example.test/group/depot.git"]);
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-qm", "Add the depot"]);
    let (one, two) = (base.join("depot-one"), base.join("depot-two"));
    for wt in [&one, &two] {
        git(&main, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);
    }
    kc(&base, &main, &["index", main.to_str().unwrap(), one.to_str().unwrap(), two.to_str().unwrap()]);
    assert_eq!(indexes(&base).len(), 3, "{:?}", indexes(&base));
    // A worktree removed: the next status in any checkout drops its index.
    git(&main, &["worktree", "remove", "--force", one.to_str().unwrap()]);
    kc(&base, &main, &["status"]);
    let left = indexes(&base);
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.iter().all(|n| !n.starts_with("depot-one-")), "{left:?}");
    // And `prune`, the explicit sweep.
    git(&main, &["worktree", "remove", "--force", two.to_str().unwrap()]);
    kc(&base, &main, &["prune"]);
    let left = indexes(&base);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].starts_with("depot-"), "{left:?}");
    let _ = std::fs::remove_dir_all(&base);
}
