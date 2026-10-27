//! History caches are keyed on what the walk read besides HEAD: the user's `bot_authors` and `former_authors`, the
//! repo's `.mailmap`, and a shallow clone's boundary. One test in its own binary, since it sets
//! `KNOWCODE_CONFIG_DIR` for the process.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::ownership::Ownership;
use kc_index::{SyncOptions, sync, vitals};
use kc_store::Store;

fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=Robo Writer", "-c", "user.email=robo@example.test"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-history-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("repo")).unwrap();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

#[test]
fn invariant_history_caches_follow_their_configuration() {
    let base = scratch();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config")) };
    let root = base.join("repo");
    git(&root, &["init", "-q"]);
    std::fs::write(root.join("a.py"), "def run():\n    return 1\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "add run"]);
    let mut store = Store::open(&root.join(".git/kc-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let anchor = vitals::anchor(&root, None).unwrap();
    let authors = |store: &Store| Ownership::load(&root, store, anchor, &[]).unwrap().coverage.authors;
    let commits = |store: &Store| vitals::history(&root, store, anchor).unwrap().commits_365d;
    assert_eq!((authors(&store), commits(&store)), (1, 1));

    // The author becomes a bot: the cached walks are stale and are walked again without them.
    let inputs = vitals::history_inputs(&root);
    std::fs::write(base.join("config/bot_authors"), "robo@example.test\n").unwrap();
    assert_ne!(vitals::history_inputs(&root), inputs);
    assert_eq!((authors(&store), commits(&store)), (0, 0));

    // And back.
    std::fs::write(base.join("config/bot_authors"), "# nobody\n").unwrap();
    assert_eq!((authors(&store), commits(&store)), (1, 1));

    // Every other input moves the key too.
    for (what, path) in [
        ("former_authors", base.join("config/former_authors")),
        (".mailmap", root.join(".mailmap")),
        ("shallow", root.join(".git/shallow")),
    ] {
        let before = vitals::history_inputs(&root);
        std::fs::write(&path, "# changed\n").unwrap();
        assert_ne!(vitals::history_inputs(&root), before, "{what}");
        if what == "shallow" {
            std::fs::remove_file(&path).unwrap();
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
