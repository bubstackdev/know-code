//! What the index covers stays what a full discovery would find, and an answer that can't be fresh says so: ignore
//! files, content-sniffed languages, symbolic links, unreadable files, and edits that keep a file's mtime and size.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, changed_since_sync, discover, sync};
use kc_store::Store;
use kc_store::query;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-cov-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    // Canonical, as `repo_root` gives it.
    std::fs::canonicalize(&dir).unwrap()
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn opts() -> SyncOptions {
    SyncOptions { threads: 1, verify: false, ..Default::default() }
}

/// The paths the index holds.
fn indexed(store: &Store) -> BTreeSet<String> {
    store.file_states().unwrap().into_keys().collect()
}

/// The paths a full discovery admits, read afresh, less those too large to extract.
fn discovered(root: &Path) -> BTreeSet<String> {
    discover(root).unwrap().0.into_iter().map(|(p, _)| p).collect()
}

const PLAY: &str = "- name: DeployWidget\n  hosts: all\n  tasks:\n    - name: Start it\n      ansible.builtin.service:\n        name: widget\n        state: started\n";

/// The invariant behind the cached file list: after any edit, a sync that reuses it covers exactly what a full
/// discovery would. Each step changes something a cheap check must notice: an ignore file, `info/exclude`, a YAML file
/// becoming a playbook and back, a file replaced by a link.
#[test]
fn invariant_a_cached_sync_covers_what_a_full_discovery_finds() {
    let root = scratch("invariant");
    let outside = root.with_extension("outside");
    std::fs::create_dir_all(&outside).unwrap();
    write(&outside, "secret.py", "def secret_outside():\n    return 1\n");
    write(&root, ".gitignore", "# nothing yet\n");
    write(&root, "app.py", "def app():\n    return 1\n");
    write(&root, "later.py", "def later():\n    return 1\n");
    write(&root, "sub/.gitignore", "# nothing yet\n");
    write(&root, "sub/inner.py", "def inner():\n    return 1\n");
    write(&root, "deploy.yml", "name: plain\nvalue: 1\n");
    std::os::unix::fs::symlink(outside.join("secret.py"), root.join("linked.py")).unwrap();
    let db = root.join(".git/kc-test.sqlite");
    let mut store = Store::open(&db).unwrap();
    sync(&root, &mut store, &opts()).unwrap();
    assert_eq!(indexed(&store), discovered(&root));
    assert!(!changed_since_sync(&root, &db), "unchanged right after a sync");
    assert!(!indexed(&store).contains("linked.py"), "a link out of the repo is not followed");
    assert_eq!(kc_index::skipped_symlinks(&store).unwrap(), ["linked.py"]);

    type Step = Box<dyn Fn(&Path)>;
    let steps: Vec<(&str, Step)> = vec![
        ("root .gitignore", Box::new(|r: &Path| write(r, ".gitignore", "later.py\n"))),
        ("nested .gitignore", Box::new(|r: &Path| write(r, "sub/.gitignore", "inner.py\n"))),
        ("info/exclude", Box::new(|r: &Path| write(r, ".git/info/exclude", "app.py\n"))),
        ("yaml becomes a playbook", Box::new(|r: &Path| write(r, "deploy.yml", PLAY))),
        ("playbook becomes plain yaml", Box::new(|r: &Path| write(r, "deploy.yml", "name: plain again\nvalue: 2\n"))),
        ("ignore rules lifted", Box::new(|r: &Path| {
            write(r, ".gitignore", "# none\n");
            write(r, "sub/.gitignore", "# none\n");
            write(r, ".git/info/exclude", "# none\n");
        })),
        ("file replaced by a link", Box::new(|r: &Path| {
            std::fs::remove_file(r.join("later.py")).unwrap();
            std::os::unix::fs::symlink(r.join("app.py"), r.join("later.py")).unwrap();
        })),
    ];
    for (what, step) in steps {
        step(&root);
        assert!(changed_since_sync(&root, &db), "{what}: the change is seen without a sync");
        let report = sync(&root, &mut store, &opts()).unwrap();
        assert_eq!(report.discovery == "cached", what.starts_with("yaml") || what.starts_with("playbook") || what.starts_with("file replaced"), "{what}");
        assert_eq!(indexed(&store), discovered(&root), "{what}");
        assert!(!changed_since_sync(&root, &db), "{what}: unchanged after a sync");
    }
    // The playbook was indexed while it was one, and is gone now that it isn't.
    assert!(query::definitions(store.conn(), "DeployWidget").unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

/// A YAML file becoming a playbook is admitted by the next query's sync, without waiting for a full discovery.
#[test]
fn a_changed_yaml_file_is_sniffed_again() {
    let root = scratch("sniff");
    write(&root, "deploy.yml", "name: plain\nvalue: 1\n");
    let mut store = Store::open(&root.join(".git/kc-test.sqlite")).unwrap();
    sync(&root, &mut store, &opts()).unwrap();
    assert!(query::definitions(store.conn(), "DeployWidget").unwrap().is_empty());
    write(&root, "deploy.yml", PLAY);
    let r = sync(&root, &mut store, &opts()).unwrap();
    assert_eq!((r.discovery, r.parsed), ("cached", 1));
    assert_eq!(query::definitions(store.conn(), "DeployWidget").unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// A file that can't be read keeps its last facts and is reported, on every sync until it can be read again.
#[test]
fn an_unreadable_file_is_reported_until_it_is_read() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch("unreadable");
    write(&root, "a.py", "def old_name():\n    return 1\n");
    write(&root, "b.py", "def other():\n    return 1\n");
    let db = root.join(".git/kc-test.sqlite");
    let mut store = Store::open(&db).unwrap();
    assert!(sync(&root, &mut store, &opts()).unwrap().failed.is_empty());
    write(&root, "a.py", "def new_name():\n    return 1\n");
    write(&root, "new.py", "def brand_new():\n    return 1\n");
    for p in ["a.py", "new.py"] {
        std::fs::set_permissions(root.join(p), std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    for _ in 0..2 {
        let r = sync(&root, &mut store, &opts()).unwrap();
        assert_eq!(r.failed, ["a.py", "new.py"], "reported on every sync, cached discovery or not");
        assert_eq!(kc_index::failed_reads(&store).unwrap(), ["a.py", "new.py"]);
        assert!(changed_since_sync(&root, &db), "a failed file is never taken as current");
    }
    assert_eq!(query::definitions(store.conn(), "old_name").unwrap().len(), 1, "as last indexed");
    for p in ["a.py", "new.py"] {
        std::fs::set_permissions(root.join(p), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let r = sync(&root, &mut store, &opts()).unwrap();
    assert!(r.failed.is_empty() && kc_index::failed_reads(&store).unwrap().is_empty());
    assert_eq!(r.parsed, 2);
    assert!(query::definitions(store.conn(), "old_name").unwrap().is_empty());
    assert_eq!(query::definitions(store.conn(), "brand_new").unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// An edit keeping a file's mtime and size is invisible to `stat`: verifying finds it, and so does reading the source
/// (its hash differs from the index's), after which re-indexing the file brings the index level.
#[test]
fn an_edit_keeping_mtime_and_size_is_found_by_hashing() {
    let root = scratch("mtime");
    write(&root, "a.py", "def old_name():\n    return 1\n");
    let mut store = Store::open(&root.join(".git/kc-test.sqlite")).unwrap();
    sync(&root, &mut store, &opts()).unwrap();
    let path = root.join("a.py");
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    write(&root, "a.py", "def new_name():\n    return 1\n");
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(before).unwrap();

    assert_eq!(sync(&root, &mut store, &opts()).unwrap().parsed, 0, "stat can't tell");
    let ctx = query::context(store.conn(), &root, "old_name", &query::ContextLimits::from_budget(1000)).unwrap().unwrap();
    assert!(ctx.source_changed, "the source shown is not what was indexed");
    let dry = sync(&root, &mut store, &SyncOptions { dry_run: true, verify: true, ..opts() }).unwrap();
    assert_eq!((dry.parsed, dry.verified), (1, true));
    let verified = sync(&root, &mut store, &SyncOptions { verify: true, ..opts() }).unwrap();
    assert_eq!((verified.parsed, verified.discovery), (1, "full"));
    assert_eq!(query::definitions(store.conn(), "new_name").unwrap().len(), 1);
    let ctx = query::context(store.conn(), &root, "new_name", &query::ContextLimits::from_budget(1000)).unwrap().unwrap();
    assert!(!ctx.source_changed);

    // The same through `reindex`, which a query showing changed source uses.
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    write(&root, "a.py", "def mid_name():\n    return 1\n");
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(before).unwrap();
    assert_eq!(kc_index::reindex(&root, &mut store, &["a.py"]).unwrap().parsed, 1);
    assert_eq!(query::definitions(store.conn(), "mid_name").unwrap().len(), 1);
    // A verified sync of an unchanged tree reads everything and changes nothing.
    let again = sync(&root, &mut store, &SyncOptions { verify: true, ..opts() }).unwrap();
    assert_eq!((again.parsed, again.removed), (0, 0));
    let _ = std::fs::remove_dir_all(&root);
}

/// The stat snapshot beside an index only ever proves a repo unchanged: without it, unreadable, or for another index
/// file, the repo counts as changed; any edit in place (no new file, no directory change) does too.
#[test]
fn the_freshness_snapshot_proves_unchanged_or_says_changed() {
    let root = scratch("snapshot");
    write(&root, "a.py", "def a():\n    return 1\n");
    write(&root, "pkg/b.py", "def b():\n    return 1\n");
    let db = root.join(".git/kc-test.sqlite");
    let snapshot = root.join(".git/kc-test.fresh");
    let mut store = Store::open(&db).unwrap();
    sync(&root, &mut store, &opts()).unwrap();
    assert!(!changed_since_sync(&root, &db));

    write(&root, "pkg/b.py", "def b():\n    return 2\n");
    assert!(changed_since_sync(&root, &db), "an edit in place");
    sync(&root, &mut store, &opts()).unwrap();
    assert!(!changed_since_sync(&root, &db));

    std::fs::write(&snapshot, b"not a snapshot").unwrap();
    assert!(changed_since_sync(&root, &db), "unreadable");
    std::fs::remove_file(&snapshot).unwrap();
    assert!(changed_since_sync(&root, &db), "missing");
    // A sync that changes nothing writes a missing snapshot.
    assert_eq!(sync(&root, &mut store, &opts()).unwrap().parsed, 0);
    assert!(!changed_since_sync(&root, &db));

    // Another index file at the same path (rebuilt): the snapshot is not its.
    drop(store);
    let kept = std::fs::read(&snapshot).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    let store = Store::open(&db).unwrap();
    std::fs::write(&snapshot, kept).unwrap();
    assert!(changed_since_sync(&root, &db), "a different index file");
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
}
