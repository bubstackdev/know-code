//! Maintenance must preserve answers, notes, unrelated files, and another process's live index connection.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use kc_index::{SyncOptions, cache, config, workspace};

static TURN: Mutex<()> = Mutex::new(());

struct Fixture { base: &'static Path, _turn: MutexGuard<'static, ()> }

impl Fixture {
    fn new() -> Self {
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        static BASE: OnceLock<PathBuf> = OnceLock::new();
        let base = BASE.get_or_init(|| {
            let base = std::env::temp_dir().join(format!("knowcode-test-cache-{}", std::process::id()));
            // SAFETY: set once, before this binary's tests read settings; fixture mutations take TURN.
            unsafe {
                std::env::set_var("KNOWCODE_CACHE_DIR", base.join("cache"));
                std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config"));
                std::env::remove_var("KNOWCODE_DATA_DIR");
            }
            base
        });
        let _ = fs::remove_dir_all(base);
        fs::create_dir_all(base).unwrap();
        config::set("notes.path", base.join("data/notes.sqlite").to_str().unwrap()).unwrap();
        drop(cache::take_touched());
        Fixture { base, _turn: turn }
    }

    fn repo(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        fs::create_dir_all(&root).unwrap();
        assert!(Command::new("git").args(["init", "-q"]).arg(&root).status().unwrap().success());
        fs::write(root.join("app.py"), format!("def handler_{name}():\n    return 42\n")).unwrap();
        let root = kc_index::repo_root(&root).unwrap();
        let mut store = kc_index::open(&root).unwrap();
        kc_index::sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
        root
    }
}

#[test]
fn invariant_lru_eviction_keeps_cross_repo_knowledge_and_restores_on_demand() {
    let f = Fixture::new();
    let a = f.repo("a");
    std::thread::sleep(Duration::from_millis(10));
    let b = f.repo("b");
    std::thread::sleep(Duration::from_millis(10));
    let c = f.repo("c");
    workspace::all_summaries().unwrap();
    let before = cache::maintain(&[]).unwrap().bytes_before.unwrap();
    config::set("cache.max_size", &(before - kc_index::index_bytes(&a).unwrap()).to_string()).unwrap();
    let report = cache::maintain(&[]).unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert_eq!(report.removed[0].root, a);
    assert!(report.bytes_after.unwrap() <= report.limit.unwrap());
    assert!(!kc_index::db_path(&a).unwrap().exists());
    assert!(kc_index::db_path(&b).unwrap().exists() && kc_index::db_path(&c).unwrap().exists());
    let summaries = workspace::all_summaries().unwrap();
    let cached = summaries.iter().find(|s| s.root == a).unwrap();
    assert!(cached.cached_only() && cached.may_define("handler_a"));
    assert!(kc_index::indexed_roots().unwrap().contains(&a));

    let mut store = kc_index::open(&a).unwrap();
    // Creating an empty replacement must not erase the last known dependency/name facts.
    let summaries = workspace::all_summaries().unwrap();
    let cached = summaries.iter().find(|s| s.root == a).unwrap();
    assert!(cached.cached_only() && cached.may_define("handler_a"));
    kc_index::sync(&a, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let summaries = workspace::all_summaries().unwrap();
    assert!(!summaries.iter().find(|s| s.root == a).unwrap().cached_only());
    assert_eq!(kc_store::query::find(store.conn(), "handler_a", None, 10).unwrap().0.len(), 1);
}

#[test]
fn invariant_pruning_and_purge_preserve_notes_and_unrelated_cache_files() {
    let f = Fixture::new();
    let root = f.repo("app");
    let cache_dir = f.base.join("cache");
    fs::create_dir_all(cache_dir.join("corpus/work")).unwrap();
    fs::write(cache_dir.join("corpus/work/keep"), b"unrelated work").unwrap();
    fs::write(cache_dir.join("notes.sqlite"), b"legacy durable notes").unwrap();
    fs::write(kc_index::tree_dir().unwrap().join("repos/foreign.sqlite"), b"not a KnowCode index").unwrap();
    // And one where 0.6.1 and older keep their indexes, which another build's maintenance must leave too.
    fs::create_dir_all(cache_dir.join("repos")).unwrap();
    fs::write(cache_dir.join("repos/foreign.sqlite"), b"not a KnowCode index").unwrap();
    config::set("cache.max_size", "0").unwrap();
    let unlimited = cache::maintain(&[]).unwrap();
    assert!(unlimited.limit.is_none() && unlimited.removed.is_empty(), "{unlimited:?}");
    fs::remove_dir_all(&root).unwrap();
    let report = cache::maintain(&[]).unwrap();
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].reason, "deleted");
    f.repo("other");
    cache::purge().unwrap();
    assert_eq!(fs::read(cache_dir.join("corpus/work/keep")).unwrap(), b"unrelated work");
    assert_eq!(fs::read(cache_dir.join("notes.sqlite")).unwrap(), b"legacy durable notes");
    assert_eq!(fs::read(cache_dir.join("repos/foreign.sqlite")).unwrap(), b"not a KnowCode index");
    assert_eq!(fs::read(kc_index::tree_dir().unwrap().join("repos/foreign.sqlite")).unwrap(), b"not a KnowCode index");
}

#[test]
fn a_note_path_overlapping_a_sidecar_prevents_its_removal() {
    let f = Fixture::new();
    let root = f.repo("app");
    let notes = kc_index::db_path(&root).unwrap().with_extension("fresh");
    config::set("notes.path", notes.to_str().unwrap()).unwrap();
    fs::write(&notes, b"durable user data").unwrap();
    fs::write(root.join("app.py"), "def handler_app():\n    return 4242\n").unwrap();
    kc_index::sync(&root, &mut kc_index::open(&root).unwrap(), &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    assert_eq!(fs::read(&notes).unwrap(), b"durable user data", "reindexing must not replace durable notes with a freshness sidecar");
    fs::remove_dir_all(&root).unwrap();
    let report = cache::purge().unwrap();
    assert_eq!(report.kept[0].reason, "notes");
    assert_eq!(fs::read(notes).unwrap(), b"durable user data");
}

#[test]
fn invariant_blocking_policy_purges_an_evicted_repositories_cached_facts() {
    let f = Fixture::new();
    let root = f.repo("app");
    {
        fs::write(root.join("package.json"), "{\"name\":\"qzx_cache_internal_package\"}\n").unwrap();
        let mut store = kc_index::open(&root).unwrap();
        kc_index::sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
        kc_index::notes::add(&root, &store, "handler_app", "qzx_cache_private_note", None).unwrap();
    }
    f.repo("other");
    workspace::all_summaries().unwrap();
    config::set("cache.max_size", "1").unwrap();
    cache::maintain(&[]).unwrap();
    assert!(!kc_index::db_path(&root).unwrap().exists());
    assert!(workspace::all_summaries().unwrap().iter().find(|s| s.root == root).unwrap().may_define("handler_app"));
    assert!(fs::read_to_string(kc_index::tree_dir().unwrap().join("workspace.json")).unwrap().contains("qzx_cache_internal_package"));
    fs::write(root.join(".knowcode-privacy.yml"), "level: 1\n").unwrap();
    let summaries = workspace::all_summaries().unwrap();
    let blocked = summaries.iter().find(|s| s.root == root).unwrap();
    assert!(blocked.blocked.whole);
    assert!(!blocked.may_define("handler_app"));
    assert_eq!(kc_index::notes::count().unwrap(), 0);
    assert!(!fs::read_to_string(kc_index::tree_dir().unwrap().join("workspace.json")).unwrap().contains("qzx_cache_internal_package"));
    assert!(!fs::read(kc_index::notes::db_path().unwrap()).unwrap().windows(b"qzx_cache_private_note".len()).any(|b| b == b"qzx_cache_private_note"));
    assert!(!fs::read_to_string(kc_index::tree_dir().unwrap().join("workspace.json")).unwrap().contains("handler_app"));
}

struct Reader(Child);
impl Reader {
    fn finish(&mut self) -> std::process::ExitStatus {
        drop(self.0.stdin.take());
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() { return status; }
            assert!(Instant::now() < until, "reader did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if self.0.try_wait().ok().flatten().is_some() { return; }
            if Instant::now() > until { let _ = self.0.kill(); let _ = self.0.wait(); return; }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn cache_reader_child() {
    let Some(root) = std::env::var_os("KNOWCODE_TEST_CACHE_READER") else { return };
    let store = kc_index::open(Path::new(&root)).unwrap();
    fs::write(std::env::var_os("KNOWCODE_TEST_CACHE_READY").unwrap(), b"ready").unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    assert!(store.meta("root").unwrap().is_some());
}

#[test]
fn invariant_another_processes_live_reader_survives_purge() {
    let f = Fixture::new();
    let root = f.repo("app");
    let ready = f.base.join("reader-ready");
    let child = Command::new(std::env::current_exe().unwrap()).args(["--exact", "cache_reader_child"])
        .env("KNOWCODE_TEST_CACHE_READER", &root).env("KNOWCODE_TEST_CACHE_READY", &ready)
        .stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
    let mut child = Reader(child);
    let until = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    let report = cache::purge().unwrap();
    assert!(report.removed.is_empty());
    assert_eq!(report.kept[0].reason, "in_use");
    assert!(kc_index::db_path(&root).unwrap().exists());
    assert!(kc_index::forget(&root).is_err());
    assert!(child.finish().success());
    drop(child);
    assert_eq!(cache::purge().unwrap().removed[0].root, root);
    assert!(!kc_index::db_path(&root).unwrap().exists());
}

#[test]
fn invariant_a_new_policy_waits_for_a_reader_instead_of_refusing() {
    let f = Fixture::new();
    let root = f.repo("app");
    // A query still reading the index (a cross-repo link step) when a privacy file arrives and another query opens it.
    let reader = kc_index::open(&root).unwrap();
    fs::write(root.join(".knowcode-privacy.yml"), "level: 1\n").unwrap();
    let opener = {
        let root = root.clone();
        std::thread::spawn(move || kc_index::open(&root).map(|store| store.meta("root").unwrap()))
    };
    // The opener takes the writer lease only to remove the index made under the old policy; the reader closes after
    // that, so the removal always meets it.
    let until = Instant::now() + Duration::from_secs(20);
    while kc_index::writer::WriterLock::try_acquire(&root).unwrap().is_some() {
        assert!(Instant::now() < until && !opener.is_finished(), "the opener never reached the removal");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(reader);
    assert_eq!(opener.join().unwrap().unwrap().as_deref(), Some(root.to_str().unwrap()));
    assert!(workspace::all_summaries().unwrap().iter().find(|s| s.root == root).unwrap().blocked.whole);
}

/// An index another build wrote for `root`, at `tree` (`<cache>` for 0.6.1 and older, `<cache>/v/<key>` for others):
/// named as every version names it, with the root and schema it records.
fn other_index(tree: &Path, root: &Path, schema: u32) -> PathBuf {
    let db = tree.join("repos").join(kc_index::db_path(root).unwrap().file_name().unwrap());
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT); CREATE TABLE padding(x BLOB);").unwrap();
    conn.execute("INSERT INTO meta VALUES ('root', ?1), ('schema_version', ?2)", [root.to_str().unwrap(), &schema.to_string()]).unwrap();
    conn.execute("INSERT INTO padding VALUES (zeroblob(65536))", []).unwrap();
    db
}

/// Sets when a file counts as last used: its `.used` record's (or its own) modification time.
fn used_at(file: &Path, secs_ago: u64) {
    let at = std::time::SystemTime::now() - Duration::from_secs(secs_ago);
    let used = file.with_extension("used");
    let target = if used.exists() { used } else { file.to_path_buf() };
    fs::File::options().write(true).open(&target).unwrap().set_modified(at).unwrap();
}

#[test]
fn a_builds_data_lives_in_its_own_tree_named_for_what_it_reads() {
    let f = Fixture::new();
    let key = kc_index::data_key();
    assert_eq!(key, format!("s{}", kc_store::SCHEMA_VERSION), "named for the schema alone");
    let tree = f.base.join("cache/v").join(key);
    assert_eq!(kc_index::tree_dir().unwrap(), tree);
    let root = f.repo("app");
    assert!(kc_index::db_path(&root).unwrap().starts_with(tree.join("repos")));
    workspace::all_summaries().unwrap();
    assert!(tree.join("workspace.json").is_file());
    // Nothing of this build's data at the top level, where 0.6.1 and older keep theirs.
    for name in ["repos", "bases", "workspace.json", "discovery.json"] {
        assert!(!f.base.join("cache").join(name).exists(), "{name}");
    }
}

#[test]
fn maintenance_evicts_other_builds_files_first_and_never_rewrites_them() {
    let f = Fixture::new();
    let cache = f.base.join("cache");
    let mine = f.repo("mine");
    let (legacy_root, other_root, held_root) = (f.repo("legacy"), f.repo("other"), f.repo("held"));
    // 0.6.1's index at the top level, another version's tree with an index and a base, and an index of that version
    // a process holds.
    let legacy = other_index(&cache, &legacy_root, 21);
    let tree = cache.join("v/s99");
    let other = other_index(&tree, &other_root, 99);
    let held = other_index(&tree, &held_root, 99);
    let base = tree.join("bases/widgets-0123456789ab-1-0.sqlite");
    fs::create_dir_all(base.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&base).unwrap();
    conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT); INSERT INTO meta VALUES ('base.build', '99');").unwrap();
    drop(conn);
    fs::write(cache.join("workspace.json"), b"{\"format\":\"21.21\"}").unwrap();
    for (file, ago) in [(&legacy, 400), (&other, 300), (&base, 200), (&held, 100)] {
        used_at(file, ago);
    }
    let lock = fs::File::options().create(true).truncate(false).write(true).open({
        fs::create_dir_all(tree.join("locks")).unwrap();
        tree.join("locks").join(held.with_extension("lock").file_name().unwrap())
    }).unwrap();
    lock.lock().unwrap();
    let legacy_bytes = fs::read(&legacy).unwrap();

    // Under budget: nothing goes, and the budget counts every tree.
    config::set("cache.max_size", "1GiB").unwrap();
    let report = cache::maintain(&[]).unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    let all = report.bytes_before.unwrap();
    assert!(all > kc_index::index_bytes(&mine).unwrap() + 4 * 65536, "{all}");
    assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes, "another build's index is never rewritten");

    // Over budget by what the other builds hold: theirs go, least recently used first; the held one and this build's
    // stay.
    config::set("cache.max_size", &(all - 1).to_string()).unwrap();
    let report = cache::maintain(&[]).unwrap();
    let removed: Vec<&Path> = report.removed.iter().map(|r| r.root.as_path()).collect();
    assert_eq!(removed, [legacy_root.as_path()], "{report:?}");
    config::set("cache.max_size", "1").unwrap();
    let report = cache::maintain(&[]).unwrap();
    let removed: Vec<(&Path, &str)> = report.removed.iter().map(|r| (r.root.as_path(), r.reason)).collect();
    assert_eq!(&removed[..2], [(other_root.as_path(), "other_version"), (base.as_path(), "other_version")], "{report:?}");
    assert!(report.kept.iter().any(|k| k.root == held_root && k.reason == "in_use"), "{report:?}");
    assert!(held.exists() && !other.exists() && !base.exists() && !legacy.exists());
    // Then this build's own, least recently used first, but never its most recent (the fixture indexed `held` last).
    assert!(removed[2..].iter().all(|(_, reason)| *reason == "size") && removed.iter().any(|(root, _)| *root == mine.as_path()), "{report:?}");
    assert!(kc_index::db_path(&held_root).unwrap().exists(), "this build's most recent index stays");
    assert!(cache.join("workspace.json").exists(), "0.6.1's summary file is its own");

    // Let go of, the held index goes too, and its tree, now empty, with it.
    drop(lock);
    cache::maintain(&[]).unwrap();
    assert!(!tree.exists(), "an emptied tree of another version goes whole");
    assert!(kc_index::tree_dir().unwrap().exists());
}
