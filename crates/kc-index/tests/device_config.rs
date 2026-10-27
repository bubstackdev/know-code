//! A repo indexed with `GIT_CONFIG_GLOBAL=/dev/null` (sandboxes and CI set it) stays fresh after its sync. The
//! freshness check stamps the global git config; a device's mtime moves on every write to it anywhere on the machine,
//! so stamping it by mtime made every query call a just-indexed repo "changed since its last index". Alone in its own
//! test binary: it sets a process-wide environment variable.

use std::process::Command;

use kc_index::{SyncOptions, changed_since_sync, sync};
use kc_store::Store;

#[test]
fn a_device_as_global_git_config_does_not_make_a_synced_repo_stale() {
    // SAFETY: the only test in this binary, set before any other thread reads the environment.
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null") };
    let dir = std::env::temp_dir().join(format!("knowcode-test-devconfig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // No commit: an unborn HEAD, as a freshly created repo (or a source copy given new git metadata) has.
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success(), "git init");
    let root = std::fs::canonicalize(&dir).unwrap();
    std::fs::write(root.join("app.py"), "def app():\n    return 1\n").unwrap();
    let db = root.join(".git/kc-test.sqlite");
    let mut store = Store::open(&db).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, verify: true, ..Default::default() }).unwrap();
    assert!(!changed_since_sync(&root, &db), "unchanged right after a sync");

    // Any process writing to /dev/null moves its mtime.
    std::fs::write("/dev/null", b"noise").unwrap();
    assert!(!changed_since_sync(&root, &db), "a write to /dev/null is not a change to the repo");

    // A real edit still is.
    std::fs::write(root.join("app.py"), "def app():\n    return 22\n").unwrap();
    assert!(changed_since_sync(&root, &db), "an edit in place");
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
}
