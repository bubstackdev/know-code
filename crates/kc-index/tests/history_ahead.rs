//! The history walk started ahead (`vitals::walk_ahead`, beside the file ranking) gives the walk it would have given
//! after: the same co-change pairs and history. One test in its own binary, since it sets `KNOWCODE_CONFIG_DIR` for
//! the process.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync, vitals};
use kc_store::Store;

fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=Ann Writer", "-c", "user.email=ann@example.test"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-history-ahead-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("repo")).unwrap();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn commit(root: &Path, files: &[(&str, &str)], message: &str) {
    for (path, text) in files {
        std::fs::write(root.join(path), text).unwrap();
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", message]);
}

#[test]
fn invariant_a_walk_started_ahead_is_the_walk() {
    let base = scratch();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config")) };
    let root = base.join("repo");
    git(&root, &["init", "-q"]);
    commit(&root, &[("a.py", "def a():\n    return 1\n"), ("b.py", "def b():\n    return 1\n"), ("c.py", "x = 1\n")], "add");
    for i in 2..5 {
        commit(&root, &[("a.py", &format!("def a():\n    return {i}\n")), ("b.py", &format!("def b():\n    return {i}\n"))], "fix: both");
    }
    commit(&root, &[("a.py", "def a():\n    return 9\n"), ("c.py", "x = 2\n")], "tweak");
    let files = ["a.py", "b.py", "c.py"].into_iter().collect();
    let open = |name: &str| {
        let mut store = Store::open(&root.join(format!(".git/kc-test-{name}.sqlite"))).unwrap();
        sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
        store
    };
    let anchor = vitals::anchor(&root, None).unwrap();
    let walked = |store: &Store| (vitals::co_change_pairs(&root, store, &files, 1).unwrap(), vitals::history(&root, store, anchor).unwrap().commits_365d);

    // Walked after, as without the ranking beside it.
    let after = walked(&open("after"));
    assert_eq!(after.1, 5);
    assert!(after.0.iter().any(|p| p.a == "a.py" && p.b == "b.py" && p.shared >= 3), "{:?}", after.0);

    // Started ahead, then taken by the walk: the same.
    let store = open("ahead");
    let guard = vitals::walk_ahead(&root, &store).unwrap();
    assert_eq!(walked(&store), after);
    drop(guard);

    // Started ahead and never taken: dropped with its guard; the next walk runs as ever.
    let store = open("dropped");
    drop(vitals::walk_ahead(&root, &store).unwrap());
    assert_eq!(walked(&store), after);

    // HEAD moved after it started: that log is not this walk's, and the walk reads the new commit.
    let store = open("moved");
    let guard = vitals::walk_ahead(&root, &store).unwrap();
    commit(&root, &[("b.py", "def b():\n    return 10\n"), ("c.py", "x = 3\n")], "another");
    let moved = walked(&store);
    drop(guard);
    assert_eq!(moved.1, 6);
    assert_eq!(moved, walked(&open("moved-after")));
    let _ = std::fs::remove_dir_all(&base);
}
