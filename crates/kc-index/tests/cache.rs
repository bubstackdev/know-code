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
    assert!(fs::read_to_string(f.base.join("cache/workspace.json")).unwrap().contains("qzx_cache_internal_package"));
    fs::write(root.join(".knowcode-privacy.yml"), "level: 1\n").unwrap();
    let summaries = workspace::all_summaries().unwrap();
    let blocked = summaries.iter().find(|s| s.root == root).unwrap();
    assert!(blocked.blocked.whole);
    assert!(!blocked.may_define("handler_app"));
    assert_eq!(kc_index::notes::count().unwrap(), 0);
    assert!(!fs::read_to_string(f.base.join("cache/workspace.json")).unwrap().contains("qzx_cache_internal_package"));
    assert!(!fs::read(kc_index::notes::db_path().unwrap()).unwrap().windows(b"qzx_cache_private_note".len()).any(|b| b == b"qzx_cache_private_note"));
    assert!(!fs::read_to_string(f.base.join("cache/workspace.json")).unwrap().contains("handler_app"));
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
