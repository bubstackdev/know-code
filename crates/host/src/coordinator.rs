use know_code_core::live::Publisher;
use know_code_core::{
    control,
    protocol::{self, envelope::Payload, *},
    query::ReadView,
    resource::{Control, Governor, Limits, Session},
    workspace::{self, Snapshot},
};
use notify::{RecursiveMode, Watcher};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;
struct Entry {
    analysis_error: String,
    failed_input_hash: String,
    status: LiveStatus,
    configuration: String,
    identity: String,
    special_inputs: BTreeSet<PathBuf>,
    dirty: BTreeSet<PathBuf>,
    force: bool,
    rehash: bool,
    retry_preparation: bool,
    queued: Instant,
    active: Instant,
    watch_pending: bool,
}
struct LiveStatus {
    value: WorkspaceStatus,
    publisher: Arc<Publisher>,
}
impl LiveStatus {
    fn new(value: WorkspaceStatus, publisher: &Arc<Publisher>) -> Self {
        Self {
            value,
            publisher: Arc::clone(publisher),
        }
    }
    fn snapshot(&self) -> WorkspaceStatus {
        self.value.clone()
    }
}
impl std::ops::Deref for LiveStatus {
    type Target = WorkspaceStatus;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}
impl std::ops::DerefMut for LiveStatus {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.publisher.invalidate(&self.value.database);
        &mut self.value
    }
}
impl Drop for LiveStatus {
    fn drop(&mut self) {
        self.publisher.invalidate(&self.value.database);
    }
}
struct Shared {
    entries: Mutex<BTreeMap<String, Entry>>,
    publisher: Arc<Publisher>,
    changed: Condvar,
    registry: Mutex<Connection>,
    stop: AtomicBool,
    maintenance: Mutex<Option<(String, Instant)>>,
    analysis_running: AtomicBool,
    resources: Arc<Governor>,
    active_job: Mutex<Option<(String, Control)>>,
    overflow: AtomicBool,
    clients: AtomicUsize,
}
enum Event {
    Wake,
    Files(Vec<PathBuf>, know_code_core::live::Input),
    Rescan(know_code_core::live::Input),
    Unwatch(PathBuf),
    Finished(JobResult),
}
struct Job {
    database: String,
    root: PathBuf,
    configuration: String,
    identity: String,
    inputs: Snapshot,
    paths: Vec<PathBuf>,
    force: bool,
    rehash: bool,
    retry_preparation: bool,
    epoch: u64,
    previous_generation: u64,
    previous_error: String,
    previous_error_code: String,
    session: Session,
    previous_failed_input_hash: String,
}
struct JobResult {
    input_hash: String,
    database: String,
    epoch: u64,
    result: Result<(u64, Snapshot, Vec<PathBuf>), Failure>,
}

struct Failure {
    code: String,
    message: String,
}

fn maintenance_active(shared: &Shared) -> bool {
    let mut lease = shared.maintenance.lock().unwrap();
    if lease
        .as_ref()
        .is_some_and(|(_, until)| *until <= Instant::now())
    {
        *lease = None;
    }
    lease.is_some()
}

pub fn run(data: &Path, installation: &Path) -> AnyResult<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join("coordinator.lock"))?;
    lock.try_lock()
        .map_err(|e| format!("a coordinator already owns this data directory: {e}"))?;
    let socket = control::socket_path(data);
    if socket.as_os_str().as_encoded_bytes().len() >= 104 {
        return Err("Unix socket path is too long; choose a shorter KNOW_CODE_DATA_DIR".into());
    }
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let publisher = Arc::new(Publisher::new(data));
    let _endpoint = Endpoint {
        socket: socket.clone(),
        _lock: lock,
        publisher: Arc::clone(&publisher),
    };
    let registry = Connection::open(data.join("registry.db"))?;
    registry.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS workspaces(database TEXT PRIMARY KEY,root TEXT UNIQUE NOT NULL,configuration TEXT NOT NULL,generation INTEGER NOT NULL,inputs TEXT NOT NULL,error TEXT NOT NULL); CREATE TABLE IF NOT EXISTS identities(database TEXT PRIMARY KEY, identity TEXT NOT NULL); CREATE TABLE IF NOT EXISTS tracked_inputs(database TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(database,path)); CREATE TABLE IF NOT EXISTS manifests(database TEXT PRIMARY KEY, inputs TEXT NOT NULL); INSERT OR IGNORE INTO manifests SELECT database,inputs FROM workspaces; UPDATE workspaces SET inputs='{}' WHERE inputs<>'{}';")?;
    let has_failure_hash = registry
        .prepare("PRAGMA table_info(workspaces)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "failed_input_hash");
    if !has_failure_hash {
        registry.execute_batch(
            "ALTER TABLE workspaces ADD COLUMN failed_input_hash TEXT NOT NULL DEFAULT '';",
        )?;
    }
    let columns = registry
        .prepare("PRAGMA table_info(workspaces)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    if !columns.contains("outcome_code") {
        registry.execute_batch(
            "ALTER TABLE workspaces ADD COLUMN outcome_code TEXT NOT NULL DEFAULT ''",
        )?;
    }
    let mut entries = BTreeMap::new();
    {
        let mut query = registry.prepare(
            "SELECT w.database,w.root,w.configuration,w.generation,'{}',w.error,COALESCE(i.identity,''),w.failed_input_hash,w.outcome_code FROM workspaces w LEFT JOIN identities i ON i.database=w.database",
        )?;
        for row in query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
            ))
        })? {
            let (
                database,
                root,
                configuration,
                generation,
                _inputs,
                error,
                identity,
                failed_input_hash,
                outcome_code,
            ) = row?;
            let identity = if identity.is_empty() {
                workspace::identity(Path::new(&root)).unwrap_or_default()
            } else {
                identity
            };
            let mut tracked =
                registry.prepare("SELECT path FROM tracked_inputs WHERE database=?")?;
            let special_inputs = tracked
                .query_map([&database], |r| r.get::<_, String>(0))?
                .map(|r| r.map(PathBuf::from))
                .collect::<std::result::Result<BTreeSet<_>, _>>()?;
            entries.insert(
                database.clone(),
                Entry {
                    analysis_error: error.clone(),
                    failed_input_hash,
                    status: LiveStatus::new(
                        WorkspaceStatus {
                            workspace_root: root,
                            database,
                            phase: "queued".into(),
                            observed_epoch: 1,
                            published_epoch: 0,
                            generation,
                            available: true,
                            error,
                            progress: None,
                            outcome_code,
                        },
                        &publisher,
                    ),
                    configuration,
                    identity,
                    special_inputs,
                    dirty: BTreeSet::new(),
                    force: false,
                    rehash: true,
                    retry_preparation: false,
                    watch_pending: true,
                    queued: Instant::now(),
                    active: Instant::now() - Duration::from_secs(60),
                },
            );
        }
    }
    let shared = Arc::new(Shared {
        publisher,
        entries: Mutex::new(entries),
        changed: Condvar::new(),
        registry: Mutex::new(registry),
        stop: AtomicBool::new(false),
        maintenance: Mutex::new(None),
        analysis_running: AtomicBool::new(false),
        resources: Governor::new(Limits::load(data)?)?,
        active_job: Mutex::new(None),
        overflow: AtomicBool::new(false),
        clients: AtomicUsize::new(0),
    });
    let (sender, receiver) = mpsc::sync_channel(4096);
    let accepting = Arc::clone(&shared);
    let signals = sender.clone();
    let data_owned = data.to_path_buf();
    thread::spawn(move || {
        for socket in listener.incoming() {
            if accepting.stop.load(Ordering::Acquire) {
                break;
            }
            let Ok(socket) = socket else {
                continue;
            };
            if accepting.clients.fetch_add(1, Ordering::AcqRel) >= 64 {
                accepting.clients.fetch_sub(1, Ordering::AcqRel);
                continue;
            }
            let state = Arc::clone(&accepting);
            let signals = signals.clone();
            let data = data_owned.clone();
            thread::spawn(move || {
                let _ = serve(socket, &state, &signals, &data);
                state.clients.fetch_sub(1, Ordering::AcqRel);
            });
        }
    });
    let changes = sender.clone();
    let callback_state = Arc::clone(&shared);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let message = match event {
            Ok(event) if event.need_rescan() => Event::Rescan(callback_state.publisher.input()),
            Ok(event) if matches!(event.kind, notify::EventKind::Access(_)) => return,
            Ok(mut event) => {
                // Drop dependency/build-output noise before it can overflow the event queue.
                // Never block the OS watcher while the owner installs/removes watches.
                if let Ok(entries) = callback_state.entries.try_lock() {
                    event
                        .paths
                        .retain(|p| entries.values().any(|entry| tracks(entry, p)));
                    if event.paths.is_empty() {
                        return;
                    }
                }
                Event::Files(event.paths, callback_state.publisher.input())
            }
            Err(_) => Event::Rescan(callback_state.publisher.input()),
        };
        wake(&changes, &callback_state, message);
    })?;
    let mut watched = BTreeMap::new();
    let mut running = false;
    let mut last_presence = Instant::now() - Duration::from_secs(10);
    loop {
        if shared.overflow.swap(false, Ordering::AcqRel) {
            let seen = shared.publisher.overflow_observed();
            mark_rescan(&shared);
            shared.publisher.overflow_handled(seen);
        }
        if last_presence.elapsed() >= Duration::from_secs(2) {
            let mut entries = shared.entries.lock().unwrap();
            for entry in entries.values_mut() {
                let root = PathBuf::from(&entry.status.workspace_root);
                let exists = root.is_dir();
                let matches =
                    exists && workspace::identity(&root).is_ok_and(|id| id == entry.identity);
                if !matches {
                    entry.status.error=if exists{"different workspace occupies the registered path; unwatch it before explicitly registering the replacement"}else{"workspace root is unavailable"}.into();
                    if entry.status.available {
                        entry.status.observed_epoch += 1;
                    }
                    entry.status.available = false;
                    entry.status.phase = "unavailable".into();
                    if watched.remove(&root).is_some() {
                        let _ = watcher.unwatch(&root);
                    }
                } else if matches && !entry.status.available {
                    entry.status.available = true;
                    entry.watch_pending = true;
                    // A returning workspace can repair an interrupted analysis,
                    // but only an actual publication may clear that failure.
                    let retry_analysis = !entry.analysis_error.is_empty();
                    queue(entry, None, retry_analysis);
                }
            }
            last_presence = Instant::now();
            shared.changed.notify_all();
        }
        // Install watches before starting analysis, including external compiler inputs.
        {
            let mut entries = shared.entries.lock().unwrap();
            for entry in entries
                .values_mut()
                .filter(|e| e.status.available && e.watch_pending)
            {
                entry.watch_pending = false;
                let root = PathBuf::from(&entry.status.workspace_root);
                if watched.get(&root) != Some(&true) {
                    match watcher.watch(&root, RecursiveMode::Recursive) {
                        Ok(()) => {
                            watched.insert(root.clone(), true);
                        }
                        Err(error) => {
                            entry.status.available = false;
                            entry.status.phase = "unavailable".into();
                            entry.status.error = format!("cannot watch workspace: {error}");
                            continue;
                        }
                    }
                }
                for input in entry
                    .special_inputs
                    .iter()
                    .filter(|p| !p.starts_with(&root))
                {
                    let mut parent = input.parent();
                    while parent.is_some_and(|p| !p.is_dir()) {
                        parent = parent.and_then(Path::parent);
                    }
                    if let Some(parent) = parent
                        && !watched.contains_key(parent)
                    {
                        match watcher.watch(parent, RecursiveMode::NonRecursive) {
                            Ok(()) => {
                                watched.insert(parent.to_path_buf(), false);
                            }
                            Err(error) => {
                                entry.status.available = false;
                                entry.status.phase = "unavailable".into();
                                entry.status.error = format!(
                                    "cannot watch compiler input {}: {error}",
                                    input.display()
                                );
                            }
                        }
                    }
                }
            }
        }
        if !running && !shared.stop.load(Ordering::Acquire) {
            let job = {
                let mut entries = shared.entries.lock().unwrap();
                let next = if shared.stop.load(Ordering::Acquire) || maintenance_active(&shared) {
                    None
                } else {
                    entries
                        .iter()
                        .filter(|(_, e)| {
                            e.status.available
                                && e.status.phase == "queued"
                                && e.queued.elapsed() >= Duration::from_millis(100)
                        })
                        .min_by_key(|(_, e)| priority(e, Instant::now()))
                        .map(|(db, _)| db.clone())
                };
                next.map(|database| -> AnyResult<Job> {
                    let session = shared.resources.start()?;
                    *shared.active_job.lock().unwrap() =
                        Some((database.clone(), session.control()));
                    shared.analysis_running.store(true, Ordering::Release);
                    let entry = entries.get_mut(&database).unwrap();
                    entry.status.phase = "analyzing".into();
                    let job = Job {
                        database: database.clone(),
                        root: PathBuf::from(&entry.status.workspace_root),
                        configuration: entry.configuration.clone(),
                        identity: entry.identity.clone(),
                        inputs: serde_json::from_str(&shared.registry.lock().unwrap().query_row(
                            "SELECT inputs FROM manifests WHERE database=?",
                            [&database],
                            |r| r.get::<_, String>(0),
                        )?)?,
                        paths: std::mem::take(&mut entry.dirty).into_iter().collect(),
                        force: std::mem::take(&mut entry.force),
                        rehash: std::mem::take(&mut entry.rehash),
                        retry_preparation: std::mem::take(&mut entry.retry_preparation),
                        epoch: entry.status.observed_epoch,
                        previous_generation: entry.status.generation,
                        previous_error: entry.analysis_error.clone(),
                        previous_error_code: entry.status.outcome_code.clone(),
                        session,
                        previous_failed_input_hash: entry.failed_input_hash.clone(),
                    };
                    Ok(job)
                })
                .transpose()?
            };
            if let Some(job) = job {
                running = true;
                let sender = sender.clone();
                let installation = installation.to_path_buf();
                let progress_shared = Arc::clone(&shared);
                thread::spawn(move || {
                    let database = job.database.clone();
                    let epoch = job.epoch;
                    let mut input_hash = String::new();
                    let result = execute(job, &installation, &mut input_hash, &mut |progress| {
                        let mut entries = progress_shared.entries.lock().unwrap();
                        if let Some(entry) = entries.get_mut(&database) {
                            entry.status.progress = Some(progress);
                        }
                        progress_shared.changed.notify_all();
                    })
                    .map_err(|e| Failure {
                        code: e
                            .downcast_ref::<know_code_core::Error>()
                            .map(|e| e.code())
                            .unwrap_or("analysis_error")
                            .into(),
                        message: e.to_string(),
                    });
                    let _ = sender.send(Event::Finished(JobResult {
                        database,
                        epoch,
                        result,
                        input_hash,
                    }));
                });
            }
        }
        if shared.stop.load(Ordering::Acquire) && !running {
            break;
        }
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(Event::Finished(done)) => {
                running = false;
                shared.analysis_running.store(false, Ordering::Release);
                shared.active_job.lock().unwrap().take();
                {
                    let mut entries = shared.entries.lock().unwrap();
                    let Some(entry) = entries.get_mut(&done.database) else {
                        continue;
                    };
                    entry.status.progress = None;
                    let manifest = match done.result {
                        Ok((generation, inputs, newer)) => {
                            entry.status.generation = generation;
                            entry.status.published_epoch = done.epoch;
                            entry.special_inputs =
                                special_inputs(&entry.status.workspace_root, &inputs);
                            entry.watch_pending = true;
                            entry.status.error.clear();
                            entry.analysis_error.clear();
                            entry.status.outcome_code.clear();
                            entry.failed_input_hash.clear();
                            if !newer.is_empty() {
                                queue(entry, Some(newer), false);
                            }
                            entry.status.phase = if !entry.status.available {
                                "unavailable"
                            } else if entry.status.observed_epoch > done.epoch {
                                "queued"
                            } else {
                                "idle"
                            }
                            .into();
                            Some(inputs)
                        }
                        Err(error) => {
                            entry.analysis_error = error.message.clone();
                            entry.status.outcome_code = error.code.clone();
                            entry.failed_input_hash = done.input_hash.clone();
                            entry.status.error = error.message;
                            entry.status.phase = if !entry.status.available {
                                "unavailable"
                            } else if entry.status.observed_epoch > done.epoch {
                                "queued"
                            } else {
                                match error.code.as_str() {
                                    "cancelled" => "cancelled",
                                    "resource_limited" => "resource_limited",
                                    _ => "failed",
                                }
                            }
                            .into();
                            None
                        }
                    };
                    persist(
                        &shared,
                        &entry.status,
                        &entry.configuration,
                        manifest.as_ref(),
                        &entry.identity,
                        &entry.analysis_error,
                        &entry.failed_input_hash,
                    )?;
                };
                {
                    let entries = shared.entries.lock().unwrap();
                    for entry in entries.values() {
                        shared.publisher.publish(&entry.status);
                    }
                }
                shared.changed.notify_all();
            }
            Ok(Event::Files(paths, _input)) => {
                let mut entries = shared.entries.lock().unwrap();
                for entry in entries.values_mut() {
                    let paths: Vec<_> =
                        paths.iter().filter(|p| tracks(entry, p)).cloned().collect();
                    if !paths.is_empty() {
                        queue(entry, Some(paths), false);
                        entry.active = Instant::now();
                    }
                }
                shared.changed.notify_all();
            }
            Ok(Event::Rescan(_input)) => mark_rescan(&shared),
            Ok(Event::Unwatch(root)) => {
                if watched.remove(&root).is_some() {
                    let _ = watcher.unwatch(&root);
                }
            }
            Ok(Event::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    shared.changed.notify_all();
    let _ = UnixStream::connect(&socket);
    Ok(())
}

fn execute(
    job: Job,
    installation: &Path,
    attempt_hash: &mut String,
    progress: &mut impl FnMut(Progress),
) -> AnyResult<(u64, Snapshot, Vec<PathBuf>)> {
    let control = job.session.control();
    control.check()?;
    let database = Path::new(&job.database);
    let extras = workspace::extra_inputs(database, &job.root).unwrap_or_default();
    let mut forced_paths = job.paths.clone();
    if job.rehash {
        forced_paths.push(job.root.clone());
        forced_paths.extend(extras.clone());
    }
    let before =
        workspace::snapshot_controlled(&job.root, &job.inputs, &forced_paths, &extras, &control)?;
    let mut hash = Sha256::new();
    for (path, stamp) in &before {
        hash.update(path.as_bytes());
        hash.update([0]);
        hash.update(stamp.hash.as_bytes());
        hash.update([0]);
    }
    *attempt_hash = format!("{:x}", hash.finalize());
    control.check()?;
    if !job.force
        && job.previous_error_code == "cancelled"
        && job.previous_failed_input_hash.is_empty()
    {
        return Err(know_code_core::Error::Cancelled.into());
    }
    if !job.force
        && !job.previous_failed_input_hash.is_empty()
        && job.previous_failed_input_hash == *attempt_hash
        && !job.previous_error.is_empty()
    {
        return Err(know_code_core::Error::Remote {
            code: job.previous_error_code.clone(),
            message: job.previous_error.clone(),
        }
        .into());
    }
    let changes = workspace::differences(&job.inputs, &before);
    let recovered = ReadView::open(database).map(|v| v.generation).unwrap_or(0);
    if !job.force && changes.is_empty() && recovered != 0 && recovered == job.previous_generation {
        // Reconciliation proves only that saved inputs did not change. It
        // cannot repair an earlier analyzer/storage failure or certify facts
        // that the failed refresh never published.
        if job.previous_error.is_empty() {
            return Ok((recovered, before, vec![]));
        }
        if !job.previous_failed_input_hash.is_empty()
            && job.previous_failed_input_hash == *attempt_hash
        {
            return Err(know_code_core::Error::Remote {
                code: job.previous_error_code.clone(),
                message: job.previous_error.clone(),
            }
            .into());
        }
        // A repair may restore the exact last-good bytes. It still needs an
        // actual analysis, while an unchanged failed snapshot must stay failed.
    }
    let scopes = if job.force
        || !job.previous_error.is_empty()
        || recovered == 0
        || recovered != job.previous_generation
    {
        None
    } else {
        workspace::affected_projects(database, &job.root, &changes)?
    };
    let preparation_fingerprint = workspace::preparation_fingerprint(&before);
    let generation = workspace::refresh_controlled(
        &job.root,
        database,
        workspace::RefreshOptions {
            configuration: &job.configuration,
            installation,
            replacements: scopes.as_deref(),
            preparation_fingerprint: &preparation_fingerprint,
            retry_preparation: job.retry_preparation,
            expected_identity: &job.identity,
        },
        progress,
        &control,
    )?;
    let extras = workspace::extra_inputs(database, &job.root)?;
    let after =
        workspace::snapshot_controlled(&job.root, &before, &forced_paths, &extras, &control)?;
    let newer = workspace::differences(&before, &after);
    // Newly discovered immutable inputs can safely enter the acknowledged manifest only
    // after a follow-up reconciliation; saves during analysis cannot disappear.
    Ok((generation, before, newer))
}

fn priority(entry: &Entry, now: Instant) -> (u8, Instant) {
    (
        if now.duration_since(entry.queued) >= Duration::from_secs(5) {
            0
        } else if now.duration_since(entry.active) < Duration::from_secs(30) {
            1
        } else {
            2
        },
        entry.queued,
    )
}
fn queue(entry: &mut Entry, paths: Option<Vec<PathBuf>>, force: bool) {
    if entry.status.phase != "queued" && entry.status.phase != "analyzing" {
        entry.queued = Instant::now();
    }
    entry.status.observed_epoch += 1;
    entry.force |= force || (entry.status.outcome_code == "cancelled" && paths.is_some());
    if let Some(paths) = paths {
        entry.dirty.extend(paths);
        if entry.dirty.len() > 4096 {
            entry.dirty.clear();
            entry.rehash = true;
        }
    } else {
        entry.rehash = true;
    }
    if entry.status.phase != "analyzing" && entry.status.available {
        entry.status.phase = "queued".into();
    }
}
fn tracks(entry: &Entry, path: &Path) -> bool {
    let root = Path::new(&entry.status.workspace_root);
    path == root
        || (path.starts_with(root)
            && !workspace::ignored(root, path)
            && (workspace::relevant(path) || path.is_dir()))
        || entry
            .special_inputs
            .iter()
            .any(|input| input == path || input.starts_with(path))
}

fn wake(sender: &SyncSender<Event>, shared: &Shared, event: Event) {
    if let Err(error) = sender.try_send(event) {
        shared.publisher.overflow();
        shared.overflow.store(true, Ordering::Release);
        drop(error);
    }
}
fn mark_rescan(shared: &Shared) {
    for entry in shared.entries.lock().unwrap().values_mut() {
        if entry.status.available {
            queue(entry, None, false);
        }
    }
    shared.changed.notify_all();
}
fn special_inputs(root: &str, inputs: &Snapshot) -> BTreeSet<PathBuf> {
    inputs
        .keys()
        .map(PathBuf::from)
        .filter(|p| {
            !p.starts_with(root)
                || workspace::ignored(Path::new(root), p)
                || !workspace::relevant(p)
        })
        .collect()
}
fn persist(
    shared: &Shared,
    status: &WorkspaceStatus,
    configuration: &str,
    inputs: Option<&Snapshot>,
    identity: &str,
    analysis_error: &str,
    failed_input_hash: &str,
) -> AnyResult<()> {
    let mut registry = shared.registry.lock().unwrap();
    let tx = registry.transaction()?;
    let encoded = inputs.map(serde_json::to_string).transpose()?;
    tx.execute("INSERT INTO workspaces(database,root,configuration,generation,inputs,error,failed_input_hash,outcome_code) VALUES(?,?,?,?,'{}',?,?,?) ON CONFLICT(database) DO UPDATE SET root=excluded.root,configuration=excluded.configuration,generation=excluded.generation,error=excluded.error,failed_input_hash=excluded.failed_input_hash,outcome_code=excluded.outcome_code",params![status.database,status.workspace_root,configuration,status.generation,analysis_error,failed_input_hash,status.outcome_code])?;
    tx.execute("INSERT INTO manifests VALUES(?,COALESCE(?,'{}')) ON CONFLICT(database) DO UPDATE SET inputs=COALESCE(?,manifests.inputs)",params![status.database,encoded,encoded])?;
    if let Some(inputs) = inputs {
        tx.execute(
            "DELETE FROM tracked_inputs WHERE database=?",
            [&status.database],
        )?;
        for path in special_inputs(&status.workspace_root, inputs) {
            tx.execute(
                "INSERT INTO tracked_inputs VALUES(?,?)",
                params![status.database, path.to_string_lossy()],
            )?;
        }
    }
    tx.execute(
        "INSERT OR IGNORE INTO identities VALUES(?,?)",
        params![status.database, identity],
    )?;
    tx.commit()?;
    Ok(())
}
fn serve(
    mut socket: UnixStream,
    shared: &Shared,
    sender: &SyncSender<Event>,
    data: &Path,
) -> AnyResult<()> {
    // Darwin can reject timeout setup after a one-way peer closes. Drain its
    // buffered frame without blocking, including any unexpected EINVAL case.
    if let Err(error) = socket.set_read_timeout(Some(Duration::from_secs(3))) {
        if error.kind() == std::io::ErrorKind::InvalidInput {
            socket.set_nonblocking(true)?;
        } else {
            return Err(error.into());
        }
    }
    let mut one_way = false;
    let answer = (|| -> AnyResult<ControlResponse> {
        let message = protocol::read_frame(&mut socket)?.ok_or("empty control request")?;
        let Some(Payload::ControlRequest(request)) = message.payload else {
            return Err("expected coordinator control request".into());
        };
        if request.operation == "activity" {
            one_way = true;
            if let Some(entry) = shared.entries.lock().unwrap().get_mut(&request.database) {
                entry.active = Instant::now();
            }
            return Ok(ControlResponse::default());
        }
        if matches!(request.operation.as_str(), "maintenance" | "resume") {
            if request.maintenance_token.is_empty() || request.maintenance_token.len() > 128 {
                return Err("maintenance requires a bounded owner token".into());
            }
            let _ = maintenance_active(shared);
            let mut lease = shared.maintenance.lock().unwrap();
            if lease
                .as_ref()
                .is_some_and(|(token, _)| token != &request.maintenance_token)
            {
                return Err(know_code_core::Error::Remote {
                    code: "busy".into(),
                    message: "another runtime operation holds the maintenance lease".into(),
                }
                .into());
            }
            if request.operation == "resume" {
                *lease = None;
            } else {
                if !(1..=30000).contains(&request.maintenance_lease_ms) {
                    return Err("maintenance lease must be 1 through 30000 ms".into());
                }
                *lease = Some((
                    request.maintenance_token,
                    Instant::now() + Duration::from_millis(request.maintenance_lease_ms.into()),
                ));
            }
            drop(lease);
            wake(sender, shared, Event::Wake);
            return Ok(ControlResponse {
                workspaces: shared
                    .entries
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|e| e.status.phase == "analyzing")
                    .map(|e| e.status.snapshot())
                    .collect(),
                analysis_running: shared.analysis_running.load(Ordering::Acquire),
                ..Default::default()
            });
        }
        if request.operation == "resources" {
            if let Some(limits) = request.resources {
                let limits = Limits {
                    cpu_cores: limits.cpu_cores,
                    memory_mib: limits.memory_mib,
                };
                limits.validate()?;
                if shared.resources.configure(data, limits)? {
                    let mut entries = shared.entries.lock().unwrap();
                    let active = shared.active_job.lock().unwrap();
                    for (db, entry) in entries.iter_mut() {
                        if entry.status.outcome_code == "resource_limited"
                            || active.as_ref().is_some_and(|(running, control)| {
                                running == db && control.resource_limited()
                            })
                        {
                            queue(entry, None, true);
                        }
                    }
                    wake(sender, shared, Event::Wake);
                }
            }
            let limits = shared.resources.limits();
            return Ok(ControlResponse {
                resources: Some(ResourceLimits {
                    cpu_cores: limits.cpu_cores,
                    memory_mib: limits.memory_mib,
                }),
                ..Default::default()
            });
        }
        if request.operation == "cancel" {
            let mut entries = shared.entries.lock().unwrap();
            let entry = entries
                .get_mut(&request.database)
                .ok_or("workspace is not registered")?;
            let active = shared.active_job.lock().unwrap();
            if let Some((db, control)) = active.as_ref().filter(|(db, _)| db == &request.database) {
                let _ = db;
                if control.cancel() {
                    entry.status.phase = "cancelling".into();
                    entry.status.outcome_code = "cancelled".into();
                }
            } else if entry.status.phase == "queued" {
                entry.analysis_error = "indexing cancelled; previous generation preserved".into();
                entry.failed_input_hash.clear();
                entry.status.error = entry.analysis_error.clone();
                entry.status.phase = "cancelled".into();
                entry.status.outcome_code = "cancelled".into();
                entry.force = false;
                entry.retry_preparation = false;
                persist(
                    shared,
                    &entry.status,
                    &entry.configuration,
                    None,
                    &entry.identity,
                    &entry.analysis_error,
                    &entry.failed_input_hash,
                )?;
            }
            shared.changed.notify_all();
            return Ok(ControlResponse {
                workspaces: vec![entry.status.snapshot()],
                ..Default::default()
            });
        }
        if request.operation == "shutdown" {
            shared.publisher.disable();
            shared.stop.store(true, Ordering::Release);
            wake(sender, shared, Event::Wake);
            shared.changed.notify_all();
            return Ok(ControlResponse::default());
        }
        if request.operation == "unregister" {
            let mut entries = shared.entries.lock().unwrap();
            let database = entries
                .iter()
                .find(|(_, e)| e.status.workspace_root == request.workspace_root)
                .map(|(db, _)| db.clone())
                .ok_or("workspace is not registered")?;
            if entries[&database].status.phase == "analyzing" {
                return Err("wait for the active update before unregistering".into());
            }
            let mut registry = shared.registry.lock().unwrap();
            let tx = registry.transaction()?;
            tx.execute("DELETE FROM workspaces WHERE database=?", [&database])?;
            tx.execute("DELETE FROM identities WHERE database=?", [&database])?;
            tx.execute("DELETE FROM manifests WHERE database=?", [&database])?;
            tx.execute("DELETE FROM tracked_inputs WHERE database=?", [&database])?;
            tx.commit()?;
            if let Some((db, control)) = shared.active_job.lock().unwrap().as_ref()
                && db == &database
            {
                control.cancel();
            }
            entries.remove(&database);
            wake(
                sender,
                shared,
                Event::Unwatch(PathBuf::from(&request.workspace_root)),
            );
            return Ok(ControlResponse::default());
        }
        if request.operation == "register" || request.operation == "refresh" {
            let root = std::fs::canonicalize(&request.workspace_root)?;
            let actual_identity = workspace::identity(&root)?;
            let database = workspace::database_path(&root, data)
                .to_string_lossy()
                .into_owned();
            let configuration = if request.configuration.is_empty() {
                "Debug"
            } else {
                &request.configuration
            };
            let persistent = {
                let mut entries = shared.entries.lock().unwrap();
                let entry = entries.entry(database.clone()).or_insert_with(|| Entry {
                    analysis_error: String::new(),
                    failed_input_hash: String::new(),
                    status: LiveStatus::new(
                        WorkspaceStatus {
                            workspace_root: root.to_string_lossy().into_owned(),
                            database,
                            phase: "queued".into(),
                            observed_epoch: 1,
                            available: true,
                            ..Default::default()
                        },
                        &shared.publisher,
                    ),
                    configuration: configuration.into(),
                    identity: actual_identity.clone(),
                    special_inputs: BTreeSet::new(),
                    dirty: BTreeSet::new(),
                    force: false,
                    rehash: true,
                    retry_preparation: false,
                    watch_pending: true,
                    queued: Instant::now(),
                    active: Instant::now(),
                });
                if entry.identity != actual_identity {
                    return Err(
                        "workspace identity changed; unwatch before registering the replacement"
                            .into(),
                    );
                }
                if !request.configuration.is_empty() && entry.configuration != configuration {
                    entry.configuration = configuration.into();
                    queue(entry, None, true);
                }
                if request.operation == "refresh" {
                    entry.retry_preparation = true;
                    queue(entry, None, true);
                }
                persist(
                    shared,
                    &entry.status,
                    &entry.configuration,
                    None,
                    &entry.identity,
                    &entry.analysis_error,
                    &entry.failed_input_hash,
                )?;
                entry.status.snapshot()
            };
            wake(sender, shared, Event::Wake);
            shared.changed.notify_all();
            return Ok(ControlResponse {
                workspaces: vec![persistent],
                ..Default::default()
            });
        }
        if request.operation != "status" && request.operation != "wait" {
            return Err("unknown control operation".into());
        }
        let mut entries = shared.entries.lock().unwrap();
        if request.operation == "wait" {
            let entry = entries
                .get_mut(&request.database)
                .ok_or("workspace is not registered")?;
            entry.active = Instant::now();
            let target = entry.status.observed_epoch;
            let until =
                Instant::now() + Duration::from_millis(u64::from(request.wait_ms.min(120000)));
            loop {
                let status = &entries[&request.database].status;
                if status.published_epoch >= target
                    || matches!(
                        status.phase.as_str(),
                        "failed" | "unavailable" | "cancelled" | "resource_limited"
                    )
                    || shared.stop.load(Ordering::Acquire)
                {
                    break;
                }
                let Some(remaining) = until.checked_duration_since(Instant::now()) else {
                    break;
                };
                let (next, timeout) = shared.changed.wait_timeout(entries, remaining).unwrap();
                entries = next;
                if timeout.timed_out() {
                    break;
                }
            }
        }
        Ok(ControlResponse {
            workspaces: entries
                .values()
                .filter(|e| request.database.is_empty() || e.status.database == request.database)
                .map(|e| e.status.snapshot())
                .collect(),
            ..Default::default()
        })
    })();
    if one_way {
        return Ok(());
    }
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut response = answer.unwrap_or_else(|error| ControlResponse {
        error_code: error
            .downcast_ref::<know_code_core::Error>()
            .map(|e| e.code())
            .unwrap_or("coordinator_error")
            .into(),
        error_message: error.to_string(),
        ..Default::default()
    });
    response.resource_control_version = know_code_core::resource::CONTROL_VERSION;
    response.analysis_running = shared.analysis_running.load(Ordering::Acquire);
    response.coordinator_executable = std::env::current_exe()?
        .canonicalize()?
        .to_string_lossy()
        .into_owned();
    protocol::write_frame(
        &mut socket,
        &Envelope {
            protocol_version: VERSION,
            job_id: String::new(),
            payload: Some(Payload::ControlResponse(response)),
        },
    )?;
    Ok(())
}
struct Endpoint {
    socket: PathBuf,
    _lock: File,
    publisher: Arc<Publisher>,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        // Retire the certificate before releasing the root writer fence, even
        // if callback/handler Arcs keep Shared alive after run() returns.
        self.publisher.disable();
        let _ = std::fs::remove_file(&self.socket);
    }
}

pub fn attach_workspace(
    data: &Path,
    root: &Path,
    configuration: String,
    refresh: bool,
) -> AnyResult<WorkspaceStatus> {
    Ok(control::attach_workspace(
        data,
        root,
        configuration,
        refresh,
        Some(&std::env::current_exe()?),
    )?)
}

pub fn login_plist(executable: &Path, data: &Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = format!(
        "{home}/.local/bin:{home}/.dotnet:{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
    );
    managed_login_plist(executable, data, "dev.bubstack.know-code", &path)
}
pub fn managed_login_plist(executable: &Path, data: &Path, label: &str, path: &str) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{}</string>\n<key>ProgramArguments</key><array><string>{}</string><string>coordinator</string><string>run</string></array>\n<key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer>\n<key>EnvironmentVariables</key><dict><key>KNOW_CODE_DATA_DIR</key><string>{}</string><key>PATH</key><string>{}</string><key>DOTNET_CLI_TELEMETRY_OPTOUT</key><string>1</string></dict>\n</dict></plist>\n",
        escape(label),
        escape(&executable.to_string_lossy()),
        escape(&data.to_string_lossy()),
        escape(path)
    )
}

#[cfg(test)]
mod live_lifetime_tests {
    use super::*;
    #[test]
    fn maintenance_is_owned_expiring_and_reports_running_jobs_without_visible_rows() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/at")
            .join(format!("lease-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let shared = Arc::new(Shared {
            entries: Mutex::new(BTreeMap::new()),
            publisher: Arc::new(Publisher::new(&root)),
            changed: Condvar::new(),
            registry: Mutex::new(Connection::open_in_memory().unwrap()),
            stop: AtomicBool::new(false),
            maintenance: Mutex::new(None),
            analysis_running: AtomicBool::new(true),
            resources: Governor::new(Limits::default()).unwrap(),
            active_job: Mutex::new(None),
            overflow: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
        });
        let listener = UnixListener::bind(control::socket_path(&root)).unwrap();
        let (sender, _receiver) = mpsc::sync_channel(16);
        let call = |operation: &str, token: &str| {
            let listener = listener.try_clone().unwrap();
            let shared = Arc::clone(&shared);
            let sender = sender.clone();
            let data = root.clone();
            let server = thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                serve(socket, &shared, &sender, &data).unwrap();
            });
            let result = control::request(
                &root,
                ControlRequest {
                    operation: operation.into(),
                    maintenance_token: token.into(),
                    maintenance_lease_ms: 30000,
                    ..Default::default()
                },
            );
            server.join().unwrap();
            result
        };
        let result = call("maintenance", "owner").unwrap();
        assert!(result.analysis_running);
        assert!(result.workspaces.is_empty());
        assert!(maintenance_active(&shared));
        assert!(call("maintenance", "other").is_err());
        assert!(call("resume", "other").is_err());
        assert!(maintenance_active(&shared));
        call("resume", "owner").unwrap();
        assert!(!maintenance_active(&shared));
        call("maintenance", "crashed-installer").unwrap();
        shared.maintenance.lock().unwrap().as_mut().unwrap().1 = Instant::now();
        assert!(!maintenance_active(&shared));
        shared.analysis_running.store(false, Ordering::Release);
        assert!(!call("maintenance", "new-owner").unwrap().analysis_running);
        std::fs::remove_file(control::socket_path(&root)).unwrap();
    }
    #[test]
    fn one_way_activity_preserves_status_and_cannot_reset_queue_aging() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
            .join(".artifacts/at")
            .join(format!(
                "host-{}-{:x}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let publisher = Arc::new(Publisher::new(&root));
        let now = Instant::now();
        let database = root
            .join("workspaces/test/index.db")
            .to_string_lossy()
            .into_owned();
        let status = WorkspaceStatus {
            database: database.clone(),
            workspace_root: root.join("repo").to_string_lossy().into(),
            phase: "idle".into(),
            available: true,
            generation: 1,
            observed_epoch: 1,
            published_epoch: 1,
            ..Default::default()
        };
        publisher.publish(&status);
        let entries = BTreeMap::from([(
            database.clone(),
            Entry {
                analysis_error: String::new(),
                failed_input_hash: String::new(),
                status: LiveStatus::new(status.clone(), &publisher),
                configuration: "Debug".into(),
                identity: "test".into(),
                special_inputs: BTreeSet::new(),
                dirty: BTreeSet::new(),
                force: false,
                rehash: false,
                retry_preparation: false,
                queued: now - Duration::from_secs(6),
                active: now - Duration::from_secs(60),
                watch_pending: false,
            },
        )]);
        let shared = Arc::new(Shared {
            entries: Mutex::new(entries),
            publisher,
            changed: Condvar::new(),
            registry: Mutex::new(Connection::open_in_memory().unwrap()),
            stop: AtomicBool::new(false),
            maintenance: Mutex::new(None),
            analysis_running: AtomicBool::new(false),
            resources: Governor::new(Limits::default()).unwrap(),
            active_job: Mutex::new(None),
            overflow: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
        });
        let listener = UnixListener::bind(control::socket_path(&root)).unwrap();
        let state = Arc::clone(&shared);
        let data = root.clone();
        let (sender, _receiver) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            serve(socket, &state, &sender, &data).unwrap();
        });
        let result = control::wait_for_database(Path::new(&database), 0, Some(100))
            .unwrap()
            .unwrap();
        assert_eq!(result, status);
        server.join().unwrap();
        let mut entries = shared.entries.lock().unwrap();
        let entry = entries.get_mut(&database).unwrap();
        assert!(entry.active > now);
        assert_eq!(entry.queued, now - Duration::from_secs(6));
        assert_eq!(entry.status.snapshot(), status);
        assert_eq!(
            know_code_core::live::current(&root, &database).unwrap(),
            status
        );
        assert_eq!(priority(entry, Instant::now()).0, 0); // aged jobs still outrank young active jobs
        entry.queued = now;
        assert_eq!(priority(entry, Instant::now()).0, 1);
        entry.active = now - Duration::from_secs(60);
        assert_eq!(priority(entry, Instant::now()).0, 2);
        assert!(
            !entry.force && !entry.rehash && !entry.retry_preparation && entry.dirty.is_empty()
        );
    }
    #[test]
    fn endpoint_retires_live_state_before_releasing_writer_fence() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/host-live-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(root.join("coordinator.lock"))
            .unwrap();
        writer.try_lock().unwrap();
        let publisher = Arc::new(Publisher::new(&root));
        let status = WorkspaceStatus {
            workspace_root: root.join("repo").to_string_lossy().into(),
            database: root
                .join("workspaces/test/index.db")
                .to_string_lossy()
                .into(),
            phase: "idle".into(),
            available: true,
            generation: 1,
            observed_epoch: 1,
            published_epoch: 1,
            ..Default::default()
        };
        publisher.publish(&status);
        assert!(know_code_core::live::current(&root, &status.database).is_some());
        let retained = Arc::clone(&publisher);
        let endpoint = Endpoint {
            socket: root.join("c.sock"),
            _lock: writer,
            publisher,
        };
        drop(endpoint);
        assert!(know_code_core::live::current(&root, &status.database).is_none());
        // A retained callback must not resurrect a retired incarnation.
        retained.publish(&status);
        assert!(know_code_core::live::current(&root, &status.database).is_none());
        File::open(root.join("coordinator.lock"))
            .unwrap()
            .try_lock()
            .unwrap();
    }
}
