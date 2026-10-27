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
            #[cfg(not(test))]
            {
                let queued = worker_queued.clone();
                std::thread::Builder::new().name("knowcode-new-repos".into()).spawn(move || new_repos(queued)).expect("starting background indexer");
            }
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
                                // Names and manifests only, for repos the cache has no room to index in full (see
                                // `new_repos`): a workspace query can still tell which of them may hold a name.
                                if worker_queued.load(Ordering::Relaxed) == 0 && !kc_index::cache::room_for_background().unwrap_or(true) {
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
                    let outcome = index_in_background(&root, 1);
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

/// Indexes `root` as the background does, with `threads` parser threads, under an operation of its own (separate from
/// a query's cancellation: work continues when the query's freshness budget expires).
fn index_in_background(root: &Path, threads: usize) -> Outcome {
    let result = std::panic::catch_unwind(|| -> Result<SyncReport> {
        let _operation = kc_store::limits::enter(kc_store::limits::Operation::new());
        let mut store = kc_index::open(root)?;
        let report = kc_index::sync(root, &mut store, &SyncOptions { threads, ..Default::default() })?;
        kc_index::content::sync_content(root, &mut store, threads)?;
        // Context intentionally reuses old derived caches for latency. Bring existing caches up to
        // date here so repeated queries eventually become complete without a manual overview call.
        kc_index::refresh_derived(root, &store)?;
        // Those never computed (a repo new to the index) follow on a thread of their own: a query waiting for this
        // index doesn't wait for them too, nor does the next repo's indexing.
        if kc_index::derived_missing(&store)? {
            Deriver::shared().queue(root);
        } else {
            drop(store);
            // Best effort, as after a derive (see `derive_in_background`).
            let _ = kc_index::base::publish(root);
        }
        Ok(report)
    });
    match result {
        Ok(r) => r.map_err(|e| format!("{e:#}")),
        Err(_) => Err("background indexer panicked".into()),
    }
}

/// Repos whose derived data (the file ranking, the areas) was never computed, built one at a time on a thread of its
/// own, at background priority, with as many threads as the background may use (see
/// [`kc_index::machine::rank_threads`]): one repo's reference graph at a time bounds the memory it takes. Queued when
/// the server first indexes a repo (a new one under a root, or one a query asks about), and when a query finds a
/// repo's never computed, so the first `overview`, `hotspots` or `areas` call finds it built. A query needing the graph
/// while it is built waits for this build rather than building it again (see `kc_store::Store::ref_graph`): its answer
/// is the same as ever. One only ordering a long list by it waits until its freshness deadline, then answers in index
/// order and says so. (Derived data behind the index is the indexing worker's to bring up to date, as before.)
struct Deriver {
    state: Mutex<Derivations>,
    changed: Condvar,
}

#[derive(Default)]
struct Derivations {
    queue: std::collections::VecDeque<PathBuf>,
    running: Option<PathBuf>,
    /// When each repo whose build failed did: not tried again within [`NEW_REPO_RETRY`].
    failed: HashMap<PathBuf, Instant>,
}

impl Deriver {
    fn shared() -> &'static Self {
        static DERIVER: OnceLock<Deriver> = OnceLock::new();
        DERIVER.get_or_init(|| {
            std::thread::Builder::new().name("knowcode-derive".into()).spawn(|| Deriver::shared().run()).expect("starting background ranking");
            Deriver { state: Mutex::default(), changed: Condvar::new() }
        })
    }

    fn run(&self) {
        background_priority();
        loop {
            let root = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.running = None;
                self.changed.notify_all();
                loop {
                    if let Some(root) = state.queue.pop_front() {
                        state.running = Some(root.clone());
                        break root;
                    }
                    state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            };
            match derive_in_background(&root) {
                Ok(true) => {},
                // Being indexed: again after the others queued, once the indexing is likely done.
                Ok(false) => {
                    std::thread::sleep(Duration::from_millis(200));
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if !state.queue.contains(&root) {
                        state.queue.push_back(root);
                    }
                },
                Err(e) => {
                    eprintln!("knowcode: file ranking and areas of {}: {e}", root.display());
                    self.state.lock().unwrap_or_else(|e| e.into_inner()).failed.insert(root, Instant::now());
                },
            }
        }
    }

    /// Queues `root`, unless it is queued or being built already, or its last build failed recently.
    fn queue(&self, root: &Path) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.queue.iter().any(|r| r == root)
            || state.running.as_deref() == Some(root)
            || state.failed.get(root).is_some_and(|at| at.elapsed() < NEW_REPO_RETRY)
        {
            return;
        }
        state.queue.push_back(root.to_path_buf());
        self.changed.notify_all();
    }

    /// Waits until nothing is queued or being built, up to `timeout`. Whether nothing is.
    #[cfg(test)]
    fn wait_idle(&self, timeout: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self.changed.wait_timeout_while(state, timeout, |s| !s.queue.is_empty() || s.running.is_some()).unwrap_or_else(|e| e.into_inner());
        state.queue.is_empty() && state.running.is_none()
    }
}

/// Brings `root`'s derived data up to its index (see [`kc_index::refresh_derived`]) as the background does. False when
/// the repo is being indexed, and nothing was done.
fn derive_in_background(root: &Path) -> std::result::Result<bool, String> {
    let result = std::panic::catch_unwind(|| -> Result<bool> {
        if !kc_index::db_path(root)?.exists() {
            return Ok(true);
        }
        // Not while the repo is being indexed (a query may queue it between the index's sync and its contents): the
        // build's writes would wait on the indexer's and the indexer's on them, past SQLite's busy timeout, failing
        // both and the queries reading the repo meanwhile. Held through the build, so indexing that starts meanwhile
        // waits for it instead.
        let Some(_writer) = kc_index::writer::WriterLock::try_acquire(root)? else { return Ok(false) };
        let _operation = kc_store::limits::enter(kc_store::limits::Operation::new());
        let store = kc_index::open(root)?;
        store.begin_snapshot()?;
        if !kc_index::derived_missing(&store)? && !kc_index::derived_stale(root, &store)? {
            return Ok(true);
        }
        use kc_index::machine::{Mode, plan, rank_threads, sniff};
        let workers = kc_store::rank::Workers { threads: rank_threads(plan(&sniff(), Mode::Background, 1, 0)), init: Some(background_priority) };
        kc_store::rank::with_workers(workers, || kc_index::build_derived(root, &store))?;
        drop(store);
        // A base other checkouts of the repo start from, carrying the derived data just built. Best effort: they
        // index from a sibling or cold without it.
        let _ = kc_index::base::publish(root);
        Ok(true)
    });
    match result {
        Ok(r) => r.map_err(|e| format!("{e:#}")),
        Err(_) => Err("panicked".into()),
    }
}

/// How long a repo whose background indexing failed waits before it is tried again.
const NEW_REPO_RETRY: Duration = Duration::from_secs(600);

/// How often the background reads the machine's load, memory and power again.
#[cfg(not(test))]
const SNIFF_EVERY: Duration = Duration::from_secs(5);

/// Repos found under the workspace roots without a full index (see [`kc_index::unindexed_roots`]) are indexed in the
/// background, on threads of their own at background priority: a query's indexing (the worker's) never waits behind
/// them, and none starts while a query's is queued. How many at once, and with how many threads, follows the machine
/// (see [`kc_index::machine::plan`]): one repo on one thread under load, on battery or under memory pressure, more
/// while it is idle and on power, none under critical memory pressure. A repo already started finishes. Roots are
/// checked first thing, so a server notices repos added while it wasn't running before its first query; after that,
/// every workspace query checks them (a `stat` of each directory the last walk read) and maintenance walks them again
/// every minute.
#[cfg(not(test))]
fn new_repos(queries: Arc<AtomicUsize>) {
    use kc_index::machine;
    background_priority();
    if let Err(e) = kc_index::light::discover_roots() {
        eprintln!("knowcode: root discovery: {e:#}");
    }
    let slots: Arc<Slots> = Arc::default();
    let mut tried = HashMap::new();
    let mut sniffed: Option<(Instant, machine::Machine)> = None;
    let mut last = None;
    loop {
        let (busy, finished) = slots.busy();
        let running = busy.len();
        if queries.load(Ordering::Relaxed) == 0 {
            let m = match sniffed {
                Some((at, m)) if at.elapsed() < SNIFF_EVERY => m,
                _ => {
                    let m = machine::sniff();
                    sniffed = Some((Instant::now(), m));
                    m
                }
            };
            let pending = match pending_new_repos(&mut tried, &busy) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("knowcode: new repos: {e:#}");
                    Vec::new()
                }
            };
            let plan = machine::plan(&m, machine::Mode::Background, pending.len() + running, running);
            if !pending.is_empty() && last != Some(plan) {
                eprintln!("knowcode: background indexing: {} repos at once, {} thread(s) each ({} waiting; load {:.1} on {} cores, {:?}, {:?} memory pressure)", plan.repos, plan.threads, pending.len(), m.load, m.cores, m.power, m.pressure);
                last = Some(plan);
            }
            for root in pending.into_iter().take(plan.repos.saturating_sub(running)) {
                tried.insert(root.clone(), Instant::now());
                slots.start(&root);
                let slots = slots.clone();
                let threads = plan.threads.max(1);
                let spawned = std::thread::Builder::new().name("knowcode-new-repo".into()).spawn(move || {
                    background_priority();
                    eprintln!("knowcode: indexing new repo {}", root.display());
                    match index_in_background(&root, threads) {
                        Ok(_) => eprintln!("knowcode: indexed {}", root.display()),
                        Err(e) => eprintln!("knowcode: indexing {} failed: {e}", root.display()),
                    }
                    kc_index::cache::maintain_and_report(std::slice::from_ref(&root));
                    slots.finish(&root);
                });
                if let Err(e) = spawned {
                    eprintln!("knowcode: background indexing: {e}");
                    break;
                }
            }
        }
        // The next repo starts as soon as one finishes, not on the next tick: most repos of a workspace index in
        // well under the tick, and a slot left free until then halved the repos indexed at once.
        slots.wait(finished, Duration::from_secs(2));
    }
}

/// The repos [`new_repos`] is indexing, and how many it has finished: finishing one wakes it to start the next.
#[derive(Default)]
struct Slots {
    state: Mutex<(std::collections::HashSet<PathBuf>, u64)>,
    finished: Condvar,
}

impl Slots {
    /// The repos being indexed, and how many have finished so far (for [`Slots::wait`]).
    fn busy(&self) -> (std::collections::HashSet<PathBuf>, u64) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.0.clone(), state.1)
    }

    fn start(&self, root: &Path) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0.insert(root.to_path_buf());
    }

    fn finish(&self, root: &Path) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0.remove(root);
        state.1 += 1;
        self.finished.notify_all();
    }

    /// Waits until a repo finishes after `seen` of them had (at once if one already has), or `timeout` passes.
    fn wait(&self, seen: u64, timeout: Duration) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        drop(self.finished.wait_timeout_while(state, timeout, |s| s.1 == seen).unwrap_or_else(|e| e.into_inner()));
    }
}

/// The repos found under the workspace roots without a full index that may be indexed in the background now, smallest
/// first: not `busy` being indexed, not failed within [`NEW_REPO_RETRY`] (`tried`), none while the cache has no room
/// (see [`kc_index::cache::room_for_background`]). Each one's AI privacy file is read again first: a repo it now
/// blocks whole is never indexed (and counts as tried).
fn pending_new_repos(tried: &mut HashMap<PathBuf, Instant>, busy: &std::collections::HashSet<PathBuf>) -> Result<Vec<PathBuf>> {
    if !kc_index::cache::room_for_background()? {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for root in kc_index::unindexed_roots()? {
        if busy.contains(&root) || tried.get(&root).is_some_and(|at| at.elapsed() < NEW_REPO_RETRY) {
            continue;
        }
        if kc_index::privacy::Privacy::load(&root).whole() {
            tried.insert(root, Instant::now());
            continue;
        }
        out.push(root);
    }
    Ok(out)
}

/// Indexes the next repo [`pending_new_repos`] gives, on one thread, here. Returns it, if any.
#[cfg(test)]
pub(crate) fn index_next_new_repo(tried: &mut HashMap<PathBuf, Instant>) -> Result<Option<PathBuf>> {
    let Some(root) = pending_new_repos(tried, &Default::default())?.into_iter().next() else { return Ok(None) };
    tried.insert(root.clone(), Instant::now());
    if let Err(e) = index_in_background(&root, 1) {
        eprintln!("knowcode: indexing {} failed: {e}", root.display());
    }
    Ok(Some(root))
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
    if !SyncOptions::default().verify && !kc_index::changed_since_sync(root, &db) {
        // The index matches the working tree. Derived data an earlier query computed may be from an earlier index
        // (after a CLI command synced it): the worker brings it up to date, but the index answers now, and an answer
        // drawing on the older data says so itself (`stale_derived`). Waiting for it, the first call of a session
        // ran out of its budget and called a current index "queued or running". Derived data never computed (an
        // index a query or an older build made) is built in the background.
        if kc_index::derived_stale(root, &store)? {
            Worker::shared().schedule(root)?;
        } else if kc_index::derived_missing(&store)? {
            Deriver::shared().queue(root);
        }
        // The worker may have committed after open() pinned its snapshot, before the stat check.
        store.end_snapshot()?;
        store.begin_snapshot()?;
        return Ok((store, Some(SyncReport::default())));
    }
    // Do not hold the pre-index read snapshot while waiting: it would delay the worker's WAL checkpoint.
    store.end_snapshot()?;
    let mut report = None;
    // The job this call joins may have read the working tree before its latest change (a job already running, about
    // to finish): its report then answers for an older tree. Still changed when it finishes, the tree is indexed once
    // more, by a job that starts after this call did (a finished job is never joined), within the same budget.
    for joined in [true, false] {
        let job = Worker::shared().schedule(root)?;
        let mut done = job.done.lock().unwrap_or_else(|e| e.into_inner());
        while done.is_none() && Instant::now() < deadline {
            kc_store::limits::check()?;
            let remaining = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(25));
            done = job.changed.wait_timeout(done, remaining).unwrap_or_else(|e| e.into_inner()).0;
        }
        match done.as_ref() {
            Some(Ok(done)) if joined && kc_index::changed_since_sync(root, &db) => continue,
            Some(Ok(done)) => report = Some(done.clone()),
            Some(Err(e)) => return Err(anyhow!("background indexing: {e}")),
            None => {}
        }
        break;
    }
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

/// The `stale` entry for a repo whose working tree changed since its index and whose indexing didn't finish within
/// the call's budget. Named by the repo's name: the answer never shows the local path.
pub fn pending(root: &Path) -> kc_store::answer::Incomplete {
    kc_store::answer::Incomplete::new(
        kc_store::answer::Reason::Stale,
        format!("{}: changed since its last index, and indexing is queued or running; this answer uses the available committed snapshot and may be empty or incomplete", crate::ops::repo_name(root)),
    )
    .with_code(kc_store::answer::Code::Indexing)
    .with_hint("indexing continues in the background; ask again after it finishes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{self, Scope};
    use crate::ops::tests::{repo, test_cache};
    use serde_json::json;

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
    fn invariant_a_job_that_read_the_tree_before_an_edit_does_not_answer_for_it() {
        let cache = test_cache();
        let base = cache.dir("refresh-joined");
        let root = repo(&base, "app", &[("src/app.py", "def old_handler():\n    return 1\n")]);
        // A job already running for the repo, as the background's own was when the edit came: it read the tree
        // before the edit, and finishes while this call waits on it, with a report of what it read.
        let running = Arc::new(Job::default());
        let db = kc_index::db_path(&root).unwrap();
        Worker::shared().jobs.lock().unwrap().insert(db, Arc::downgrade(&running));
        std::fs::write(root.join("src/app.py"), "def new_api_handler():\n    return 42\n").unwrap();
        let writer = kc_index::writer::WriterLock::acquire(&root).unwrap();
        let finisher = {
            let running = running.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                *running.done.lock().unwrap() = Some(Ok(SyncReport { parsed: 1, ..SyncReport::default() }));
                running.changed.notify_all();
            })
        };
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let budget = Budget::enter(Duration::from_millis(600));
        let out = ops::find("new_api_handler", None, 10, &scope).unwrap();
        drop(budget);
        finisher.join().unwrap();
        // Not a complete answer from the index before the edit: the edit is still being indexed, and the answer says so.
        assert_eq!(out.env["complete"], false, "{}", out.env);
        assert_eq!(out.env["freshness"]["status"], "indexing", "{}", out.env);
        assert_eq!(out.env["incomplete"][0]["reason"], "stale", "{}", out.env);
        drop(writer);
        // With the writer gone the edit is indexed: asked again until it is (other tests share the worker, so on a
        // loaded machine that can take more than one budget), the answer has the new symbol.
        let until = Instant::now() + Duration::from_secs(120);
        let out = loop {
            let _budget = Budget::enter(Duration::from_secs(10));
            let out = ops::find("new_api_handler", None, 10, &scope).unwrap();
            if out.env["complete"] == true || Instant::now() > until {
                break out;
            }
        };
        assert_eq!(out.env["complete"], true, "{}", out.env);
        assert_eq!(out.env["items"].as_array().unwrap().len(), 1, "{}", out.env);
    }

    #[test]
    fn a_current_index_with_older_derived_data_answers_at_once_and_index_brings_it_up_to_date() {
        let cache = test_cache();
        let base = cache.dir("refresh-derived");
        let root = repo(&base, "app", &[("src/app.py", "def handler():\n    return helper()\n\ndef helper():\n    return 1\n")]);
        // Derived data computed by an earlier query, then an edit a CLI command synced (no budget: it waits).
        let store = kc_index::open(&root).unwrap();
        kc_index::areas::areas(&root, &store).unwrap();
        store.file_rank_at(&[], kc_store::rank::Flow::Uses, true).unwrap();
        drop(store);
        std::fs::write(root.join("src/app.py"), "def handler():\n    return helper() + 1\n\ndef helper():\n    return 1\n").unwrap();
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        ops::find("handler", None, 10, &scope).unwrap();
        let store = kc_index::open(&root).unwrap();
        assert!(kc_index::derived_stale(&root, &store).unwrap());
        drop(store);

        // The index matches the working tree: a query within a budget answers from it, whatever the derived data,
        // even while the worker can't start (another writer holds the repo).
        let writer = kc_index::writer::WriterLock::acquire(&root).unwrap();
        let budget = Budget::enter(Duration::from_millis(1));
        let out = ops::find("handler", None, 10, &scope).unwrap();
        drop(budget);
        assert_eq!(out.env["complete"], true, "{}", out.env);
        assert_eq!(out.env["provenance"][0]["fresh"], true);
        assert!(out.env["freshness"]["status"].is_null(), "{}", out.env);
        // The background brings the derived data up to date once the writer is gone, after the worker's index.
        let job = Worker::shared().schedule(&root).unwrap();
        drop(writer);
        let done = job.done.lock().unwrap();
        let (done, timeout) = job.changed.wait_timeout_while(done, Duration::from_secs(60), |d| d.is_none()).unwrap();
        assert!(!timeout.timed_out());
        assert!(done.as_ref().unwrap().is_ok(), "{:?}", *done);
        drop(done);
        assert!(Deriver::shared().wait_idle(Duration::from_secs(60)));
        assert!(!kc_index::derived_stale(&root, &kc_index::open(&root).unwrap()).unwrap());

        // What `index` does after its sync: nothing derived is left behind the index.
        std::fs::write(root.join("src/app.py"), "def handler():\n    return helper() + 2\n\ndef helper():\n    return 1\n").unwrap();
        ops::find("handler", None, 10, &scope).unwrap();
        let store = kc_index::open(&root).unwrap();
        assert!(kc_index::derived_stale(&root, &store).unwrap());
        kc_index::refresh_derived(&root, &store).unwrap();
        assert!(!kc_index::derived_stale(&root, &store).unwrap());
    }

    #[test]
    fn a_repo_still_indexing_is_flagged_where_it_bears_on_the_answer() {
        let cache = test_cache();
        let base = cache.dir("refresh-relevant");
        let a = repo(&base, "alpha", &[("src/a.py", "def alpha_handler():\n    return 1\n")]);
        let b = repo(&base, "beta", &[("src/b.py", "def beta_handler():\n    return 2\n")]);
        for (root, text) in [(&a, "def alpha_handler():\n    return 10\n"), (&b, "def beta_handler():\n    return 20\n")] {
            std::fs::write(root.join(if root == &a { "src/a.py" } else { "src/b.py" }), text).unwrap();
        }
        let writers = [kc_index::writer::WriterLock::acquire(&a).unwrap(), kc_index::writer::WriterLock::acquire(&b).unwrap()];
        let scope = Scope { repos: vec![a.clone(), b.clone()], no_sync: false };
        let budget = Budget::enter(Duration::from_millis(1));
        let out = ops::find("alpha_handler", None, 10, &scope).unwrap();
        drop(budget);
        drop(writers);
        let stale: Vec<String> = out.env["incomplete"].as_array().into_iter().flatten().filter(|i| i["reason"] == "stale").map(|i| i["detail"].as_str().unwrap().to_string()).collect();
        // Both are behind; only the one the answer comes from is said to be.
        assert_eq!(out.env["repos"][1]["freshness"]["status"], "indexing", "{}", out.env);
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(stale[0].starts_with("alpha: "), "{stale:?}");
        assert!(!stale[0].contains(&base.to_string_lossy().into_owned()), "{stale:?}");
    }

    /// A git repo at `dir` holding `files`, staged and not indexed.
    fn clone_into(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        assert!(std::process::Command::new("git").args(["init", "-q"]).arg(dir).status().unwrap().success());
        for (path, text) in files {
            std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.join(path), text).unwrap();
        }
        assert!(std::process::Command::new("git").arg("-C").arg(dir).args(["add", "-A"]).status().unwrap().success());
        kc_index::repo_root(dir).unwrap()
    }

    #[test]
    fn invariant_a_repo_cloned_under_a_root_is_said_not_indexed_until_the_background_indexes_it() {
        let cache = test_cache();
        let base = cache.dir("new-repos");
        let roots = base.join("roots");
        let old = repo(&roots, "old", &[("src/a.py", "def shared_name():\n    return 1\n")]);
        kc_index::config::add_root(&roots).unwrap();
        let new = clone_into(&roots.join("fresh"), &[("src/b.py", "def shared_name_too():\n    return 2\n")]);
        // Bigger than `fresh`, by its git index: indexed after it.
        let big = clone_into(&roots.join("big"), &[("src/c.py", "def other():\n    return 3\n"), ("src/d.py", "x = 1\n")]);
        let privacy = kc_index::privacy::file_name();
        let locked = clone_into(&roots.join("locked"), &[(privacy.as_str(), "level: 1\n"), ("src/e.py", "def secret():\n    return 4\n")]);

        // Noticed, its privacy file read: the one blocked whole is never in the queue.
        assert_eq!(kc_index::unindexed_roots().unwrap(), vec![new.clone(), big.clone()]);
        assert!(kc_index::indexed_roots().unwrap().contains(&old));
        // A workspace-wide MCP answer reads the indexed ones; the others are the background's.
        assert_eq!(ops::workspace_repos().unwrap(), vec![old.clone()]);

        // A workspace-wide answer that couldn't index them within its budget says so once, naming them.
        let writers = [kc_index::writer::WriterLock::acquire(&new).unwrap(), kc_index::writer::WriterLock::acquire(&big).unwrap()];
        let scope = Scope { repos: kc_index::indexed_roots().unwrap(), no_sync: false };
        let budget = Budget::enter(Duration::from_millis(1));
        let mut env = ops::find("shared_name", None, 10, &scope).unwrap().env;
        drop(budget);
        drop(writers);
        ops::add_missing(&mut env);
        let entries = crate::contract::incomplete_of(&env);
        let notes: Vec<&str> = entries.iter().filter(|i| i.detail.starts_with("workspace: ")).map(|i| i.detail.as_str()).collect();
        assert_eq!(notes, ["workspace: 2 repositories found, not yet indexed (big, fresh); nothing from them is in this answer"], "{env}");
        assert!(!entries.iter().any(|i| i.detail.starts_with("fresh: ") || i.detail.starts_with("big: ")), "one entry, not one per repo: {env}");
        assert_eq!(env["complete"], false);

        // `status` lists them as queued, or indexed when the query's own worker has taken one since.
        let status = ops::status(&Scope { repos: vec![new.clone(), big.clone()], no_sync: false }).unwrap().data;
        assert!(status.iter().all(|v| v["background"] == "queued" || v["complete"] == true), "{status:?}");

        // The background indexes them (the query worker may have taken some first), never the blocked one.
        // Until they are indexed, however long a loaded machine takes, within a bound that only a hang reaches.
        let mut tried = HashMap::new();
        let until = Instant::now() + Duration::from_secs(60);
        while !kc_index::unindexed_roots().unwrap().is_empty() && Instant::now() < until {
            if index_next_new_repo(&mut tried).unwrap().is_none() {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        assert!(kc_index::unindexed_roots().unwrap().is_empty());
        assert!(!kc_index::db_path(&locked).unwrap().exists());
        // A repo leaves that list when its sync commits; a query worker job that took it goes on to sync its contents,
        // then queues its ranking. Wait for that job (joined if it is still queued or running, else a sync with
        // nothing to do), so what the answer queued is done, and before the next test takes its turn with the worker.
        for root in [&new, &big] {
            let job = Worker::shared().schedule(root).unwrap();
            let done = job.done.lock().unwrap();
            let (done, timeout) = job.changed.wait_timeout_while(done, Duration::from_secs(60), |d| d.is_none()).unwrap();
            assert!(!timeout.timed_out(), "{}", root.display());
            assert!(done.as_ref().unwrap().is_ok(), "{:?}", *done);
        }
        // Their file ranking and areas follow in the background: the first overview doesn't compute them.
        assert!(Deriver::shared().wait_idle(Duration::from_secs(60)));
        for root in [&new, &big] {
            let store = kc_index::open(root).unwrap();
            assert!(!kc_index::derived_missing(&store).unwrap(), "{}", root.display());
            assert!(!kc_index::derived_stale(root, &store).unwrap(), "{}", root.display());
            assert!(store.derived_gen("areas").unwrap().is_some(), "{}", root.display());
        }
        let mut env = json!({ "repo": "workspace", "items": [] });
        ops::add_missing(&mut env);
        assert!(env.get("incomplete").is_none(), "{env}");

        // A repo removed from the root drops out: no longer listed, nor counted.
        std::fs::remove_dir_all(&big).unwrap();
        assert!(!kc_index::indexed_roots().unwrap().contains(&big));
        assert!(kc_index::unindexed_roots().unwrap().is_empty());
    }

    #[test]
    fn a_query_answers_while_an_indexer_holds_the_write_lock_past_the_busy_timeout() {
        let cache = test_cache();
        let base = cache.dir("query-while-writing");
        let root = repo(&base, "app", &[("src/refund.py", "def approve_refund():\n    return 1\n")]);
        assert!(std::process::Command::new("git").arg("-C").arg(&root).args(["-c", "user.name=Ann", "-c", "user.email=ann@example.test", "add", "-A"]).status().unwrap().success());
        assert!(std::process::Command::new("git").arg("-C").arg(&root).args(["-c", "user.name=Ann", "-c", "user.email=ann@example.test", "commit", "-qm", "refund approval"]).status().unwrap().success());
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        ops::find("approve_refund", None, 10, &scope).unwrap();
        let _budget = Budget::enter(Duration::from_secs(2));
        // An indexer's write transaction (the background's, starved of CPU at its priority) outlasting the query's
        // busy timeout: the caches the query would write are left for later; its answer is the same.
        let writer = rusqlite::Connection::open(kc_index::db_path(&root).unwrap()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let busy = ops::locate("refund approval", 10, &scope);
        writer.execute_batch("ROLLBACK").unwrap();
        let busy = busy.unwrap();
        let free = ops::locate("refund approval", 10, &scope).unwrap();
        assert_eq!(busy.env["items"], free.env["items"]);
        assert!(!busy.env["items"].as_array().unwrap().is_empty(), "{}", busy.env);
    }

    #[test]
    fn a_ranking_queued_while_the_repo_is_indexed_waits_for_the_indexing() {
        let cache = test_cache();
        let base = cache.dir("derive-while-indexing");
        let root = repo(&base, "app", &[("src/app.py", "def handler():\n    return helper()\n\ndef helper():\n    return 1\n")]);
        assert!(kc_index::derived_missing(&kc_index::open(&root).unwrap()).unwrap());
        // An indexer holds the repo (as the background does between its sync and its contents) when a query queues
        // the ranking: it is neither built against the indexer's writes nor given up as failed.
        let writer = kc_index::writer::WriterLock::acquire(&root).unwrap();
        Deriver::shared().queue(&root);
        std::thread::sleep(Duration::from_millis(500));
        assert!(kc_index::derived_missing(&kc_index::open(&root).unwrap()).unwrap());
        assert!(!Deriver::shared().state.lock().unwrap().failed.contains_key(&root));
        // Built once the indexing is done.
        drop(writer);
        assert!(Deriver::shared().wait_idle(Duration::from_secs(60)));
        assert!(!kc_index::derived_missing(&kc_index::open(&root).unwrap()).unwrap());
    }

    #[test]
    fn a_workspace_answer_says_once_which_repos_it_left_out() {
        let cache = test_cache();
        let base = cache.dir("one-note");
        let roots = base.join("roots");
        repo(&roots, "old", &[("src/a.py", "def a():\n    return 1\n")]);
        kc_index::config::add_root(&roots).unwrap();
        let first = clone_into(&roots.join("first"), &[("src/b.py", "def b():\n    return 2\n")]);
        clone_into(&roots.join("second"), &[("src/c.py", "def c():\n    return 3\n"), ("src/d.py", "x = 1\n")]);
        // The answer notes the repos not indexed as it finishes; the server notes every workspace answer again as it
        // sends it. A repo indexed in between leaves the first note as it was, not a second one beside it.
        let mut env = json!({ "repo": "workspace", "items": [] });
        ops::add_missing(&mut env);
        kc_index::sync(&first, &mut kc_index::open(&first).unwrap(), &kc_index::SyncOptions { threads: 1, ..Default::default() }).unwrap();
        ops::add_unindexed(&mut env);
        let notes: Vec<String> = crate::contract::incomplete_of(&env).into_iter().map(|i| i.detail).filter(|d| d.contains("not yet indexed")).collect();
        assert_eq!(notes, ["workspace: 2 repositories found, not yet indexed (first, second); nothing from them is in this answer"], "{env}");
    }

    #[test]
    fn a_repo_finishing_wakes_the_new_repo_loop_at_once() {
        let slots = Arc::new(Slots::default());
        let (a, b) = (PathBuf::from("/w/alpha"), PathBuf::from("/w/beta"));
        slots.start(&a);
        slots.start(&b);
        let (busy, seen) = slots.busy();
        assert_eq!(busy.len(), 2);

        // Nothing finishes: the loop waits out its tick.
        let start = Instant::now();
        slots.wait(seen, Duration::from_millis(100));
        assert!(start.elapsed() >= Duration::from_millis(100));

        // One finishes while it waits: it wakes then, well before the tick.
        let finisher = slots.clone();
        let done = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            finisher.finish(&a);
        });
        let start = Instant::now();
        slots.wait(seen, Duration::from_secs(30));
        assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
        done.join().unwrap();
        let (busy, now) = slots.busy();
        assert_eq!(busy.into_iter().collect::<Vec<_>>(), vec![b.clone()]);
        assert_eq!(now, seen + 1);

        // One that finished after the loop last looked, before it waits: no wait at all.
        slots.finish(&b);
        let start = Instant::now();
        slots.wait(now, Duration::from_secs(30));
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(slots.busy().0.is_empty());
    }

    #[test]
    fn the_queue_waits_while_the_cache_has_no_room() {
        let cache = test_cache();
        let base = cache.dir("new-repos-full");
        let roots = base.join("roots");
        repo(&roots, "old", &[("src/a.py", "def a():\n    return 1\n")]);
        kc_index::config::add_root(&roots).unwrap();
        let new = clone_into(&roots.join("fresh"), &[("src/b.py", "def b():\n    return 2\n")]);
        // A budget the one index already fills.
        let config = cache.base().join("config/config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, format!("{text}\n[cache]\nmax_size = 1\n")).unwrap();
        assert_eq!(kc_index::unindexed_roots().unwrap(), vec![new.clone()]);
        assert!(index_next_new_repo(&mut HashMap::new()).unwrap().is_none());
        // With nothing else indexed, a workspace answer reads the smallest of those not indexed yet.
        std::fs::remove_dir_all(roots.join("old")).unwrap();
        assert_eq!(ops::workspace_repos().unwrap(), vec![new.clone()]);
        assert!(!kc_index::db_path(&new).unwrap().exists());
        let note = ops::unindexed_note().unwrap();
        assert!(note.hint.as_deref().unwrap().contains("cache.max_size"), "{note:?}");
    }

    #[test]
    fn the_pending_note_names_the_repo_not_its_local_path() {
        let entry = pending(Path::new("/home/someone/src/shop-api"));
        assert!(entry.detail.starts_with("shop-api: changed since its last index"), "{}", entry.detail);
        assert!(!entry.detail.contains("/home/"), "{}", entry.detail);
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
        let scope = Scope { repos: vec![root], no_sync: false };
        // Each call within a 10 s budget. On a loaded machine (other tests share the worker) indexing can outlast one,
        // and the answer then says it is still indexing; asking again converges. Only a hang reaches the bound.
        let converged = |scope: &Scope| {
            let until = Instant::now() + Duration::from_secs(120);
            loop {
                let _budget = Budget::enter(Duration::from_secs(10));
                let out = ops::context("apiHandler", 2000, kc_store::skeleton::SourceMode::Auto, scope).unwrap();
                if out.env["complete"] == true || Instant::now() > until {
                    return out;
                }
                // Still indexing, or its derived data a moment behind: said, never answered as if complete.
                let entries = out.env["incomplete"].as_array().unwrap();
                assert!(!entries.is_empty() && entries.iter().all(|i| i["reason"] == "stale"), "{}", out.env);
            }
        };
        let out = converged(&scope);
        assert_eq!(out.env["complete"], true, "{}", out.env);
        assert_eq!(out.env["provenance"][0]["fresh"], true);
        assert!(!out.env["items"].as_array().unwrap().is_empty());
        std::fs::write(scope.repos[0].join("app.ts"), "export function apiHandler() { return 4242; }\n").unwrap();
        let out = converged(&scope);
        assert_eq!(out.env["complete"], true, "derived caches must converge too: {}", out.env);
        assert!(out.env["items"][0]["source"].as_str().unwrap().contains("4242"));
    }
}
