//! A shared freshness budget per MCP call and one background indexing worker per server process.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use kc_index::{SyncOptions, SyncReport};
use kc_store::Store;

/// How long the query this thread runs (see [`kc_store::limits::current`]) waits for indexing, across every repo it
/// opens, until the guard drops; the query's earlier wait is restored then.
pub struct Budget(Arc<kc_store::limits::Operation>, Option<Instant>);

impl Budget {
    pub fn enter(duration: Duration) -> Self {
        let query = kc_store::limits::current();
        let before = query.set_freshness_deadline(Some(Instant::now() + duration));
        Self(query, before)
    }
}

impl Drop for Budget {
    fn drop(&mut self) {
        self.0.set_freshness_deadline(self.1);
    }
}

/// Until when the query this thread runs waits for indexing; `None` waits until it is done (a CLI command).
fn deadline() -> Option<Instant> {
    kc_store::limits::current().freshness_deadline()
}

type Outcome = std::result::Result<SyncReport, String>;

#[derive(Default)]
struct Job {
    done: Mutex<Option<Outcome>>,
    changed: Condvar,
}

struct Worker {
    sender: mpsc::Sender<(PathBuf, Arc<Job>)>,
    jobs: Mutex<HashMap<PathBuf, Weak<Job>>>,
    watched: Arc<Mutex<BTreeMap<PathBuf, Instant>>>,
    queued: Arc<AtomicUsize>,
}

impl Worker {
    fn shared() -> &'static Self {
        static WORKER: OnceLock<Worker> = OnceLock::new();
        WORKER.get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<(PathBuf, Arc<Job>)>();
            let watched = Arc::new(Mutex::new(BTreeMap::<PathBuf, Instant>::new()));
            let worker_watched = watched.clone();
            let queued = Arc::new(AtomicUsize::new(0));
            let worker_queued = queued.clone();
            std::thread::Builder::new().name("knowcode-index".into()).spawn(move || {
                background_priority();
                #[cfg(not(test))]
                let mut maintained = Instant::now();
                #[cfg(not(test))]
                let mut scanned = BTreeMap::<PathBuf, Instant>::new();
                loop {
                    let (root, job) = match receiver.recv_timeout(Duration::from_secs(2)) {
                        Ok(job) => { worker_queued.fetch_sub(1, Ordering::Relaxed); job },
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            // Unit fixtures share a process and replace its cache/config between cases. Real MCP
                            // servers have one profile for their lifetime; protocol integration tests use that binary.
                            #[cfg(not(test))]
                            {
                                Worker::shared().poll();
                                if maintained.elapsed() >= Duration::from_secs(60) {
                                    crate::update::start();
                                    if let Err(e) = kc_index::light::discover_roots() { eprintln!("knowcode: root discovery: {e:#}"); }
                                    kc_index::cache::maintain_and_report(&[]);
                                    maintained = Instant::now();
                                }
                                if worker_queued.load(Ordering::Relaxed) == 0 {
                                    match kc_index::light::pending() {
                                        Ok(roots) => if let Some(root) = roots.into_iter().find(|r| scanned.get(r).is_none_or(|at| at.elapsed() >= Duration::from_secs(60))) {
                                            eprintln!("knowcode: scanning names and manifests in {}", root.display());
                                            match kc_index::light::scan_until(&root, || worker_queued.load(Ordering::Relaxed) > 0) {
                                                Ok(true) => { scanned.insert(root, Instant::now()); },
                                                Ok(false) => {}, // yield to a queued query; keep its placeholder
                                                Err(e) => { eprintln!("knowcode: light scan {}: {e:#}", root.display()); scanned.insert(root, Instant::now()); },
                                            }
                                        },
                                        Err(e) => eprintln!("knowcode: light discovery: {e:#}"),
                                    }
                                }
                            }
                            continue;
                        },
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    eprintln!("knowcode: indexing {}", root.display());
                    // Separate from a query's cancellation: work continues when its freshness budget expires.
                    // Only one repository and one extraction thread run in the background at a time.
                    let result = std::panic::catch_unwind(|| -> Result<SyncReport> {
                        let _operation = kc_store::limits::enter(kc_store::limits::Operation::new());
                        let mut store = kc_index::open(&root)?;
                        let report = kc_index::sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() })?;
                        kc_index::content::sync_content(&root, &mut store, 1)?;
                        // Context intentionally reuses old derived caches for latency. Bring existing caches up to
                        // date here so repeated queries eventually become complete without a manual overview call.
                        if kc_index::areas::needs_refresh(&root, &store)? {
                            kc_index::areas::areas(&root, &store)?;
                        }
                        let rank_key = format!("rank_graph:{}", kc_store::rank::RANK_VERSION);
                        let generation = store.generation()?;
                        if store.derived_gen(&rank_key)?.is_some_and(|g| g != generation) {
                            store.ref_graph()?;
                            store.file_rank_at(&[], kc_store::rank::Flow::Uses, true)?;
                        }
                        Ok(report)
                    });
                    let outcome = match result {
                        Ok(r) => r.map_err(|e| format!("{e:#}")),
                        Err(_) => Err("background indexer panicked".into()),
                    };
                    match &outcome {
                        Ok(_) => eprintln!("knowcode: indexed {}", root.display()),
                        Err(e) => eprintln!("knowcode: indexing {} failed: {e}", root.display()),
                    }
                    let retry = if !outcome.as_ref().is_ok_and(|r| r.failed.is_empty()) || kc_index::privacy::Privacy::load(&root).whole() { 60 } else { 2 };
                    worker_watched.lock().unwrap_or_else(|e| e.into_inner()).insert(root.clone(), Instant::now() + Duration::from_secs(retry));
                    kc_index::cache::maintain_and_report(&[root]);
                    *job.done.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
                    job.changed.notify_all();
                }
            }).expect("starting background indexer");
            Worker { sender, jobs: Mutex::new(HashMap::new()), watched, queued }
        })
    }

    fn schedule(&self, root: &Path) -> Result<Arc<Job>> {
        self.watch(root);
        let db = kc_index::db_path(root)?;
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = jobs.get(&db).and_then(Weak::upgrade)
            && job.done.lock().unwrap_or_else(|e| e.into_inner()).is_none()
        {
            return Ok(job);
        }
        jobs.retain(|_, job| job.strong_count() > 0);
        let job = Arc::new(Job::default());
        self.queued.fetch_add(1, Ordering::Relaxed);
        if self.sender.send((root.to_path_buf(), job.clone())).is_err() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            return Err(anyhow!("background indexer stopped"));
        }
        jobs.insert(db, Arc::downgrade(&job));
        Ok(job)
    }

    fn watch(&self, root: &Path) {
        self.watched.lock().unwrap_or_else(|e| e.into_inner()).entry(root.to_path_buf())
            .or_insert_with(|| Instant::now() + Duration::from_secs(2));
    }

    /// Poll one queried checkout per idle tick, using only freshness stats. Missing and evicted indexes are
    /// removed from the watch list; warming must not fight cache eviction or walk every registered repository.
    #[cfg(not(test))]
    fn poll(&self) {
        let root = {
            let mut watched = self.watched.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let Some(root) = watched.iter().filter(|(_, due)| **due <= now).min_by_key(|(_, due)| **due).map(|(r, _)| r.clone()) else { return };
            watched.insert(root.clone(), now + Duration::from_secs(2));
            root
        };
        let db = match kc_index::db_path(&root) { Ok(db) => db, Err(_) => return };
        if !root.join(".git").exists() || !db.exists() {
            self.watched.lock().unwrap_or_else(|e| e.into_inner()).remove(&root);
        } else if kc_index::changed_since_sync(&root, &db) && let Err(e) = self.schedule(&root) {
            eprintln!("knowcode: background refresh {}: {e:#}", root.display());
        }
    }
}

/// Start discovery when MCP starts, even before its first query. No standalone daemon is created.
pub fn start() { Worker::shared(); }

fn background_priority() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(class: u32, relative_priority: i32) -> i32;
        }
        // SAFETY: changes only this newly created worker thread. QOS_CLASS_BACKGROUND = 0x09.
        if unsafe { pthread_set_qos_class_self_np(0x09, 0) } != 0 {
            eprintln!("knowcode: could not lower background indexer priority");
        }
    }
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn setpriority(which: u32, who: u32, priority: i32) -> i32;
        }
        // SAFETY: Linux PRIO_PROCESS (0) with who = 0 adjusts only the calling worker thread.
        if unsafe { setpriority(0, 0, 10) } != 0 {
            eprintln!("knowcode: could not lower background indexer priority");
        }
    }
}

/// Returns a pinned committed snapshot and its sync report, or no report when freshness was not established.
/// CLI calls retain their blocking behavior. MCP calls share one deadline across all repositories they consult.
pub fn open(root: &Path, no_sync: bool) -> Result<(Store, Option<SyncReport>)> {
    let mut store = kc_index::open(root)?;
    if no_sync {
        return Ok((store, None));
    }
    let Some(deadline) = deadline() else {
        let report = kc_index::sync(root, &mut store, &SyncOptions::default())?;
        return Ok((store, Some(report)));
    };
    Worker::shared().watch(root); // watch even when this call finds an already-fresh index
    let db = kc_index::db_path(root)?;
    let rank_key = format!("rank_graph:{}", kc_store::rank::RANK_VERSION);
    let generation = store.generation()?;
    let stale_rank = store.derived_gen(&rank_key)?.is_some_and(|g| g != generation);
    if !SyncOptions::default().verify && !kc_index::changed_since_sync(root, &db)
        && !kc_index::areas::needs_refresh(root, &store)? && !stale_rank
    {
        // The worker may have committed after open() pinned its snapshot, before the stat check.
        store.end_snapshot()?;
        store.begin_snapshot()?;
        return Ok((store, Some(SyncReport::default())));
    }
    // Do not hold the pre-index read snapshot while waiting: it would delay the worker's WAL checkpoint.
    store.end_snapshot()?;
    let job = Worker::shared().schedule(root)?;
    let mut done = job.done.lock().unwrap_or_else(|e| e.into_inner());
    while done.is_none() && Instant::now() < deadline {
        kc_store::limits::check()?;
        let remaining = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(25));
        done = job.changed.wait_timeout(done, remaining).unwrap_or_else(|e| e.into_inner()).0;
    }
    let report = match done.as_ref() {
        Some(Ok(report)) => Some(report.clone()),
        Some(Err(e)) => return Err(anyhow!("background indexing: {e}")),
        None => None,
    };
    store.end_snapshot()?;
    store.begin_snapshot()?;
    Ok((store, report))
}

/// A cross-repo answer used an evicted checkout's summary: restore it without passing that snapshot off as fresh.
pub fn restore_cached(root: &Path) -> Result<()> {
    if deadline().is_some() { Worker::shared().schedule(root)?; }
    else { drop(open(root, false)?); }
    Ok(())
}

pub fn pending(root: &Path) -> kc_store::answer::Incomplete {
    kc_store::answer::Incomplete::new(
        kc_store::answer::Reason::Stale,
        format!("{}: indexing is queued or running; this answer uses the available committed snapshot and may be empty or incomplete", root.display()),
    ).with_hint("indexing continues in the background; ask again after it finishes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{self, Scope};
    use crate::ops::tests::{repo, test_cache};

    #[test]
    fn invariant_budget_returns_stale_without_waiting_for_writer_then_converges() {
        let cache = test_cache();
        let base = cache.dir("refresh-stale");
        let root = repo(&base, "app", &[("src/app.py", "def old_handler():\n    return 1\n")]);
        std::fs::write(root.join("src/app.py"), "def new_api_handler():\n    return 42\n").unwrap();
        let writer = kc_index::writer::WriterLock::acquire(&root).unwrap();
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let start = Instant::now();
        let budget = Budget::enter(Duration::from_millis(50));
        let out = ops::find("new_api_handler", None, 10, &scope).unwrap();
        assert!(start.elapsed() < Duration::from_secs(2), "read waited for another writer");
        assert_eq!(out.env["complete"], false);
        assert_eq!(out.env["freshness"]["status"], "indexing");
        assert_eq!(out.env["incomplete"][0]["reason"], "stale");
        assert_eq!(out.env["provenance"][0]["fresh"], false);
        assert!(out.env["items"].as_array().unwrap().is_empty());
        drop(budget);
        // A second call shares the pending job, rather than scheduling another extraction.
        let a = Worker::shared().schedule(&root).unwrap();
        let b = Worker::shared().schedule(&root).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        drop(writer);
        let done = a.done.lock().unwrap();
        let (done, timeout) = a.changed.wait_timeout_while(done, Duration::from_secs(20), |d| d.is_none()).unwrap();
        assert!(!timeout.timed_out());
        assert!(done.as_ref().unwrap().is_ok(), "{:?}", *done);
        drop(done);
        let _budget = Budget::enter(Duration::from_secs(2));
        let out = ops::find("new_api_handler", None, 10, &scope).unwrap();
        assert_eq!(out.env["complete"], true);
        assert_eq!(out.env["provenance"][0]["fresh"], true);
        assert_eq!(out.env["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn invariant_first_query_indexes_fresh_clone_without_manual_index() {
        let cache = test_cache();
        let base = cache.dir("refresh-cold");
        let root = base.join("app");
        std::fs::create_dir_all(&root).unwrap();
        assert!(std::process::Command::new("git").args(["init", "-q"]).arg(&root).status().unwrap().success());
        std::fs::write(root.join("app.ts"), "export function apiHandler() { return 42; }\n").unwrap();
        let root = kc_index::repo_root(&root).unwrap();
        assert!(!kc_index::db_path(&root).unwrap().exists());
        let _budget = Budget::enter(Duration::from_secs(10));
        let scope = Scope { repos: vec![root], no_sync: false };
        let out = ops::context("apiHandler", 2000, kc_store::skeleton::SourceMode::Auto, &scope).unwrap();
        assert_eq!(out.env["complete"], true, "{}", out.env);
        assert_eq!(out.env["provenance"][0]["fresh"], true);
        assert!(!out.env["items"].as_array().unwrap().is_empty());
        std::fs::write(scope.repos[0].join("app.ts"), "export function apiHandler() { return 4242; }\n").unwrap();
        let out = ops::context("apiHandler", 2000, kc_store::skeleton::SourceMode::Auto, &scope).unwrap();
        assert_eq!(out.env["complete"], true, "derived caches must converge too: {}", out.env);
        assert!(out.env["items"][0]["source"].as_str().unwrap().contains("4242"));
    }
}
