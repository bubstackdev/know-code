//! Shared bases: worktrees of one repo start their index as a clone of an immutable base at a commit, index only what
//! differs from it, and hold the same facts as a cold index of the same tree.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use kc_index::{SyncOptions, sync};
use kc_store::{Store, query};

fn base() -> &'static Path {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("knowcode-test-bases-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // SAFETY: set once, before any test in this binary reads them.
        unsafe {
            std::env::set_var("KNOWCODE_CACHE_DIR", base.join("cache"));
            std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config"));
            std::env::set_var("KNOWCODE_DATA_DIR", base.join("data"));
        }
        base
    })
}

/// The cache is process-wide: the tests here take turns, each from an empty cache.
fn turn() -> std::sync::MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_dir_all(base().join("cache"));
    turn
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success(), "git {args:?}");
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A repo of `n` TypeScript files with an origin, committed, and a linked worktree of it at `name-wt0` (a repo with
/// another checkout, which a base serves).
fn repo(name: &str, n: usize) -> PathBuf {
    let dir = base().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    for i in 0..n {
        write(&dir, &format!("src/s{i}.ts"), &format!("export class Store{i} {{ load(): number {{ return Store{}.make(); }} static make(): number {{ return {i}; }} }}\n", (i + 1) % n));
    }
    git(&dir, &["remote", "add", "origin", &format!("https://example.test/group/{name}.git")]);
    git(&dir, &["add", "."]);
    git(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    worktree(&dir, "wt0");
    kc_index::repo_root(&dir).unwrap()
}

fn worktree(main: &Path, name: &str) -> PathBuf {
    let path = base().join(format!("{}-{name}", main.file_name().unwrap().to_string_lossy()));
    git(main, &["worktree", "add", "-q", "--detach", path.to_str().unwrap()]);
    kc_index::repo_root(&path).unwrap()
}

fn opts() -> SyncOptions {
    SyncOptions { threads: 2, ..Default::default() }
}

/// What an index holds about the code, without row ids: symbols and references by place.
fn facts(store: &Store) -> Vec<String> {
    let mut out: Vec<String> = store
        .conn()
        .prepare("SELECT s.name, s.kind, f.path, s.start_line, ifnull(s.container, '') FROM symbols s JOIN files f ON f.id = s.file_id")
        .unwrap()
        .query_map([], |r| Ok(format!("sym {} {} {}:{} in {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    out.extend(
        store
            .conn()
            .prepare("SELECT r.name, f.path, r.line, r.col FROM refs r JOIN files f ON f.id = r.file_id")
            .unwrap()
            .query_map([], |r| Ok(format!("ref {} {}:{}:{}", r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))
            .unwrap()
            .map(Result::unwrap),
    );
    out.extend(
        store
            .conn()
            .prepare("SELECT f.path, d.name, d.location, ifnull(d.resolved_repo, '') FROM package_deps d JOIN files f ON f.id = d.file_id")
            .unwrap()
            .query_map([], |r| Ok(format!("dep {} {} {} -> {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
            .unwrap()
            .map(Result::unwrap),
    );
    out.extend(
        store
            .conn()
            .prepare("SELECT path, hash, size FROM files")
            .unwrap()
            .query_map([], |r| Ok(format!("file {} {} {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
            .unwrap()
            .map(Result::unwrap),
    );
    out.sort();
    out
}

/// A cold index of `root`'s tree, in a file of its own outside the cache.
fn cold(root: &Path, name: &str) -> Store {
    let db = base().join(format!("cold-{name}.sqlite"));
    let _ = std::fs::remove_file(&db);
    let mut store = Store::open(&db).unwrap();
    sync(root, &mut store, &opts()).unwrap();
    store
}

fn indexed(root: &Path) -> Store {
    let mut store = kc_index::open(root).unwrap();
    sync(root, &mut store, &opts()).unwrap();
    kc_index::content::sync_content(root, &mut store, 1).unwrap();
    store
}

#[test]
fn a_worktree_starts_from_the_base_and_holds_what_a_cold_index_holds() {
    let _turn = turn();
    let main = repo("shop", 30);
    drop(indexed(&main));
    let published = kc_index::base::publish(&main).unwrap().expect("a clean checkout with a worktree publishes a base");
    assert!(published.starts_with(base().join("cache/bases")), "{published:?}");
    // The checkout it was published from adopts it: its index is a clone of the base, with its own records.
    let store = kc_index::open(&main).unwrap();
    assert_eq!(store.meta(kc_index::base::META_FROM).unwrap().as_deref(), published.file_name().and_then(|n| n.to_str()));
    assert_eq!(store.meta("root").unwrap().as_deref(), Some(main.to_string_lossy().as_ref()));
    let report = sync(&main, &mut kc_index::open(&main).unwrap(), &opts()).unwrap();
    assert_eq!((report.parsed, report.removed), (0, 0), "{report:?}");
    drop(store);
    // Published once: the same commit again is no new base.
    assert!(kc_index::base::publish(&main).unwrap().is_none());

    // A worktree with a file changed, one added and one deleted indexes those only.
    let wt = worktree(&main, "wt1");
    write(&wt, "src/s3.ts", "export class Store3 { load(): number { return 33; } save(): void {} }\n");
    write(&wt, "src/extra.ts", "export class Extra { run(): void { new Store3().save(); } }\n");
    std::fs::remove_file(wt.join("src/s4.ts")).unwrap();
    let mut store = kc_index::open(&wt).unwrap();
    assert_eq!(store.meta(kc_index::base::META_FROM).unwrap().as_deref(), published.file_name().and_then(|n| n.to_str()));
    let report = sync(&wt, &mut store, &opts()).unwrap();
    assert_eq!((report.parsed, report.removed, report.touched), (2, 1, 28), "{report:?}");
    assert_eq!(store.meta("root").unwrap().as_deref(), Some(wt.to_string_lossy().as_ref()));
    assert_eq!(facts(&store), facts(&cold(&wt, "wt1")));
    let (hits, _) = query::find(store.conn(), "save", None, 5).unwrap();
    assert_eq!(hits[0].symbol.file, "src/s3.ts");
    // The base and the checkout it was published from are untouched.
    assert!(query::find(kc_index::open(&main).unwrap().conn(), "save", None, 5).unwrap().0.is_empty());
}

#[test]
fn a_clean_worktree_at_the_base_commit_extracts_nothing() {
    let _turn = turn();
    let dir = base().join("crates");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    write(&dir, "app/Cargo.toml", "[package]\nname = \"app\"\n\n[dependencies]\ncore = { path = \"../core\" }\n");
    write(&dir, "app/src/main.rs", "fn main() { core::run(); }\n");
    write(&dir, "core/Cargo.toml", "[package]\nname = \"core\"\n");
    write(&dir, "core/src/lib.rs", "pub fn run() {}\n");
    git(&dir, &["remote", "add", "origin", "https://example.test/group/crates.git"]);
    git(&dir, &["add", "."]);
    git(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    worktree(&dir, "wt0");
    let main = kc_index::repo_root(&dir).unwrap();
    drop(indexed(&main));
    kc_index::base::publish(&main).unwrap().expect("published");
    let generation = kc_index::open(&main).unwrap().generation().unwrap();

    let wt = worktree(&main, "wt1");
    let mut store = kc_index::open(&wt).unwrap();
    let report = sync(&wt, &mut store, &opts()).unwrap();
    assert_eq!(report.parsed, 0, "{report:?}");
    // The path dependency lands in this checkout, not the one the base was made from; the generation (and so the
    // derived data the base carries) stands.
    let resolved: String = store.conn().query_row("SELECT resolved_repo FROM package_deps WHERE source = 'path'", [], |r| r.get(0)).unwrap();
    assert_eq!(resolved, wt.to_string_lossy());
    assert_eq!(store.generation().unwrap(), generation);
    assert_eq!(facts(&store), facts(&cold(&wt, "crates-wt1")));
    // The content index takes the stats the sync just took for the files it hashed: none is read again.
    let content = kc_index::content::sync_content(&wt, &mut store, 1).unwrap();
    assert_eq!((content.reread, content.rebuilt), (0, false), "{content:?}");
}

#[test]
fn a_clone_hashes_every_file_whatever_its_stat() {
    let _turn = turn();
    let main = repo("cart", 12);
    drop(indexed(&main));
    kc_index::base::publish(&main).unwrap().expect("published");
    // An edit keeping the file's size, with the mtime the base recorded for it (a copy that kept mtimes): the base's
    // stat matches, its content doesn't.
    let wt = worktree(&main, "wt1");
    let recorded = std::fs::metadata(main.join("src/s5.ts")).unwrap().modified().unwrap();
    write(&wt, "src/s5.ts", &std::fs::read_to_string(main.join("src/s5.ts")).unwrap().replace("Store5", "Store9"));
    std::fs::File::options().write(true).open(wt.join("src/s5.ts")).unwrap().set_modified(recorded).unwrap();
    assert_eq!(std::fs::metadata(wt.join("src/s5.ts")).unwrap().len(), std::fs::metadata(main.join("src/s5.ts")).unwrap().len());
    let mut store = kc_index::open(&wt).unwrap();
    assert!(store.meta(kc_index::base::META_FROM).unwrap().is_some());
    let report = sync(&wt, &mut store, &opts()).unwrap();
    assert_eq!(report.parsed, 1, "{report:?}");
    assert_eq!(facts(&store), facts(&cold(&wt, "cart-wt1")));
}

#[test]
fn a_checkout_under_another_privacy_policy_never_clones_the_base() {
    let _turn = turn();
    let main = repo("vault", 8);
    drop(indexed(&main));
    kc_index::base::publish(&main).unwrap().expect("published");
    let wt = worktree(&main, "wt1");
    write(&wt, &kc_index::privacy::file_name(), "rules:\n  - path: src/s2.ts\n    level: 1\n");
    let mut store = kc_index::open(&wt).unwrap();
    assert_eq!(store.meta(kc_index::base::META_FROM).unwrap(), None);
    sync(&wt, &mut store, &opts()).unwrap();
    assert!(query::find(store.conn(), "Store2", None, 5).unwrap().0.is_empty());
    assert!(!query::find(store.conn(), "Store3", None, 5).unwrap().0.is_empty());
}

#[test]
fn a_repo_checked_out_once_or_dirty_publishes_no_base() {
    let _turn = turn();
    let main = repo("solo", 4);
    // Dirty: an untracked file is indexed, so the index isn't the commit's.
    write(&main, "src/scratch.ts", "export class Scratch {}\n");
    drop(indexed(&main));
    assert!(kc_index::base::publish(&main).unwrap().is_none());
    // Clean, without another checkout: nothing to serve.
    std::fs::remove_file(main.join("src/scratch.ts")).unwrap();
    git(&main, &["worktree", "remove", "--force", base().join("solo-wt0").to_str().unwrap()]);
    git(&main, &["worktree", "prune"]);
    drop(indexed(&main));
    assert!(kc_index::base::publish(&main).unwrap().is_none());
    assert!(!base().join("cache/bases").exists() || std::fs::read_dir(base().join("cache/bases")).unwrap().next().is_none());
}

#[test]
fn purging_the_cache_removes_the_bases_and_pruning_those_no_checkout_uses() {
    let _turn = turn();
    let main = repo("menu", 4);
    drop(indexed(&main));
    let published = kc_index::base::publish(&main).unwrap().expect("published");
    kc_index::cache::prune().unwrap();
    assert!(published.exists(), "a base whose repo has a checkout stays");
    kc_index::cache::purge().unwrap();
    assert!(!published.exists());
}

#[test]
fn checkouts_started_together_wait_for_one_to_build_the_base() {
    let _turn = turn();
    let main = repo("till", 10);
    let (a, b) = (worktree(&main, "a"), worktree(&main, "b"));
    // `a` is clean and first: it claims the commit's base.
    assert_eq!(kc_index::base::wait_or_claim(&a).unwrap(), kc_index::base::Turn::Building);
    let waiter = std::thread::spawn(move || {
        let mut store = kc_index::open(&b).unwrap();
        let from = store.meta(kc_index::base::META_FROM).unwrap();
        (from, sync(&b, &mut store, &opts()).unwrap().parsed)
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(!waiter.is_finished(), "`b` waits for the base `a` builds");
    drop(indexed(&a));
    let published = kc_index::base::publish(&a).unwrap().expect("published");
    let (from, parsed) = waiter.join().unwrap();
    assert_eq!(from.as_deref(), published.file_name().and_then(|n| n.to_str()));
    assert_eq!(parsed, 0);
}
