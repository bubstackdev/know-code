use know_code_core::live::Publisher;
use know_code_core::{
    control,
    protocol::{self, envelope::Payload, *},
    query::ReadView,
    recovery,
    resource::{Control, Governor, Limits, Session},
    settings,
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
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;
static REGISTRATION: AtomicU64 = AtomicU64::new(1);
struct Entry {
    registration: u64,
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
    pending: bool,
    cancel_pending: bool,
    recovery_blocked: bool,
    recovery_explicit: bool,
    recovering: bool,
    upgrade_format: Option<i32>,
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
    cursors: Arc<know_code_core::cursor::CursorService>,
    discovery: Mutex<crate::discovery::Discovery>,
}
enum Event {
    Wake,
    Files(Vec<PathBuf>, know_code_core::live::Input),
    Rescan(know_code_core::live::Input),
    Unwatch(PathBuf),
    Finished(JobResult),
}
struct Job {
    registration: u64,
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
    cancel_baseline: bool,
    recovering: bool,
    recovery_explicit: bool,
    upgrade_format: Option<i32>,
}
enum JobOutput {
    Indexed(u64, Snapshot, Vec<PathBuf>),
    CancellationBaseline(Snapshot, Vec<PathBuf>, bool),
}
struct JobResult {
    registration: u64,
    input_hash: String,
    database: String,
    epoch: u64,
    result: Result<JobOutput, Failure>,
    cancel_baseline: bool,
    recovering: bool,
}

struct Failure {
    code: String,
    message: String,
    index_fault: Option<(PathBuf, String)>,
}

fn selected_readiness(database: &Path) -> know_code_core::Result<(u64, bool)> {
    let guard = recovery::IndexReadGuard::open_unbudgeted(database)?;
    let result = (|| {
        let result = ReadView::readiness(guard.physical_path())?;
        guard.check()?;
        Ok(result)
    })();
    match result {
        Err(know_code_core::Error::NotReady) => Ok((0, false)),
        Err(know_code_core::Error::Sqlite(error)) => Err(guard.map_sqlite(error)),
        other => other,
    }
}

fn selected_upgrade(database: &Path) -> know_code_core::Result<Option<(i32, u64)>> {
    let Some((format, generation)) = recovery::selected_format(database)? else {
        return Ok(None);
    };
    match know_code_core::store::upgrade_action(format) {
        know_code_core::store::UpgradeAction::Rebuild { .. } => Ok(Some((format, generation))),
        know_code_core::store::UpgradeAction::Current
        | know_code_core::store::UpgradeAction::Migrate => Ok(None),
        know_code_core::store::UpgradeAction::Unsupported => {
            Err(know_code_core::Error::UpdateRequired(format!(
                "index format {format} has no supported upgrade path; install a compatible runtime; this index was left unchanged"
            )))
        }
    }
}
fn upgrade_failure(database: &Path) -> know_code_core::Result<bool> {
    Ok(matches!(
        recovery::failure_reason(database)?,
        Some(recovery::RebuildReason::Upgrade { .. })
    ))
}

fn restore_recovery(entry: &mut Entry) -> AnyResult<()> {
    let database = PathBuf::from(&entry.status.database);
    recovery::adopt(&database, entry.status.generation)?;
    recovery::reconcile(&database)?;
    if !recovery::is_bad(&database)? {
        match selected_upgrade(&database) {
            Ok(upgrade) => {
                entry.upgrade_format = upgrade.map(|(format, generation)| {
                    entry.status.generation = entry.status.generation.max(generation);
                    format
                })
            }
            Err(know_code_core::Error::IndexCorrupt { .. }) => {}
            Err(error) => return Err(error.into()),
        }
    }

    if let Some(generation) = recovery::selected_replacement_generation(&database)? {
        entry.status.generation = entry.status.generation.max(generation);
    }
    if let Some(error) = recovery::failure(&database)? {
        entry.recovery_blocked = true;
        entry.pending = false;
        entry.cancel_pending = false;
        entry.analysis_error = error.clone();
        entry.status.error = error;
        if !matches!(
            entry.status.outcome_code.as_str(),
            "resource_limited" | "cancelled"
        ) {
            entry.status.outcome_code = if upgrade_failure(&database)? {
                "index_upgrade_failed"
            } else {
                "index_recovering"
            }
            .into();
        }
        entry.status.phase = if entry.status.outcome_code == "resource_limited" {
            "resource_limited"
        } else if upgrade_failure(&database)? {
            "upgrade_failed"
        } else {
            "recovery_failed"
        }
        .into();
    } else {
        // This probes only owned file identity/loss. SQL corruption is detected
        // by the normal guarded readiness/read/write boundary.
        match recovery::IndexReadGuard::open_unbudgeted(&database) {
            Ok(_) => {}
            Err(know_code_core::Error::IndexCorrupt { .. }) => {
                if entry.status.outcome_code != "cancelled" && !entry.cancel_pending {
                    entry.pending = true;
                    entry.force = true;
                    entry.status.phase = "queued".into();
                    entry.status.outcome_code = "index_corrupt".into();
                    entry.status.error = "derived index recovery is queued".into();
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn observe_bad(shared: &Shared, entry: &mut Entry, incarnation: &str) -> AnyResult<bool> {
    let database = PathBuf::from(&entry.status.database);
    if !recovery::report_corruption(&database, incarnation)? {
        return Ok(false);
    }
    shared.cursors.invalidate_corrupt(&database, incarnation)?;
    if let Some(error) = recovery::failure(&database)? {
        // A late report/presence check must not revoke an explicit refresh
        // already queued behind another workspace. Dispatch consumes that
        // authorization once; notifications cannot create a new one.
        if entry.recovery_explicit {
            entry.recovery_blocked = false;
            entry.pending = true;
            entry.force = true;
            if !entry.recovering {
                entry.status.phase = "queued".into();
            }
            return Ok(true);
        }
        entry.recovery_blocked = true;
        entry.pending = false;
        entry.status.error = error.clone();
        entry.analysis_error = error;
        if !matches!(
            entry.status.outcome_code.as_str(),
            "resource_limited" | "cancelled"
        ) {
            entry.status.outcome_code = "index_recovering".into();
        }
        entry.status.phase = if entry.status.outcome_code == "resource_limited" {
            "resource_limited"
        } else {
            "recovery_failed"
        }
        .into();
    } else if !entry.recovering
        && !entry.cancel_pending
        && !(entry.status.outcome_code == "cancelled" && !entry.force)
    {
        if let Some((_, control)) = shared
            .active_job
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(db, _)| db == &entry.status.database)
        {
            control.cancel();
        }
        entry.status.error = "derived index is fenced; recovery is queued".into();
        entry.status.outcome_code = "index_corrupt".into();
        queue(entry, None, true);
    }
    Ok(true)
}

// Workspace-owned lifecycle/health faults fence only that workspace. They
// are not evidence that its derived SQLite data is corrupt and never authorize
// replacement or an automatic retry.
fn block_lifecycle_error(entry: &mut Entry, error: &(dyn std::error::Error + 'static)) {
    entry.pending = false;
    entry.force = false;
    entry.recovery_blocked = true;
    entry.recovery_explicit = false;
    entry.status.phase = "failed".into();
    entry.status.outcome_code = error
        .downcast_ref::<know_code_core::Error>()
        .map(|e| e.code())
        .unwrap_or("recovery_state_error")
        .into();
    entry.status.error = error.to_string();
    entry.analysis_error = entry.status.error.clone();
}

fn observe_selected_health(shared: &Shared, entry: &mut Entry) -> bool {
    let result = (|| -> AnyResult<bool> {
        let database = Path::new(&entry.status.database);
        if let Some(incarnation) = recovery::selected_incarnation(database)?
            && recovery::is_bad(database)?
        {
            return observe_bad(shared, entry, &incarnation);
        }
        Ok(false)
    })();
    match result {
        Ok(observed) => observed,
        Err(error) => {
            block_lifecycle_error(entry, error.as_ref());
            false
        }
    }
}

fn maintain_retired_indexes(entry: &mut Entry) {
    if entry.recovering {
        return;
    }
    let database = PathBuf::from(&entry.status.database);
    let result = (|| -> know_code_core::Result<bool> {
        if !recovery::has_retired(&database)? {
            return Ok(false);
        }
        recovery::cleanup(&database, entry.status.generation)?;
        recovery::has_retired(&database)
    })();
    match result {
        Ok(false) if entry.status.progress.as_ref().is_some_and(|p|p.stage=="cleanup_pending")=>entry.status.progress=None,
        Ok(false)=>{},
        Ok(true)=>entry.status.progress=Some(Progress{stage:"cleanup_pending".into(),project_path:String::new(),message:"retired index files await reader release or legacy-reader verification and remain charged".into()}),
        Err(error)=>entry.status.progress=Some(Progress{stage:"cleanup_pending".into(),project_path:String::new(),message:format!("retired index cleanup is pending and remains charged: {error}")}),
    }
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
    let mut _endpoint = Endpoint {
        socket: socket.clone(),
        _lock: lock,
        publisher: Arc::clone(&publisher),
        cursors: None,
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
    if !columns.contains("cancel_pending") {
        registry.execute_batch("BEGIN IMMEDIATE;
          ALTER TABLE workspaces ADD COLUMN cancel_pending INTEGER NOT NULL DEFAULT 0;
          UPDATE workspaces SET cancel_pending=1,failed_input_hash='' WHERE outcome_code='cancelled';
          COMMIT;")?;
    }
    let discovery = crate::discovery::Discovery::open(&registry)?;
    let mut entries = BTreeMap::new();
    {
        let mut query = registry.prepare(
            "SELECT w.database,w.root,w.configuration,w.generation,'{}',w.error,COALESCE(i.identity,''),w.failed_input_hash,w.outcome_code,w.cancel_pending FROM workspaces w LEFT JOIN identities i ON i.database=w.database",
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
                r.get::<_, bool>(9)?,
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
                pending_cancel,
            ) = row?;
            let cancel_pending =
                pending_cancel || (outcome_code == "cancelled" && failed_input_hash.is_empty());
            let identity = if identity.is_empty() {
                workspace::identity(Path::new(&root)).unwrap_or_default()
            } else {
                identity
            };
            let mut tracked =
                registry.prepare("SELECT path FROM tracked_inputs WHERE database=?")?;
            let mut special_inputs = tracked
                .query_map([&database], |r| r.get::<_, String>(0))?
                .map(|r| r.map(PathBuf::from))
                .collect::<std::result::Result<BTreeSet<_>, _>>()?;
            special_inputs.extend(settings::paths(Path::new(&root), data));
            let requested_settings = settings::resolve(Path::new(&root), data, &configuration).ok();
            entries.insert(
                database.clone(),
                Entry {
                    registration: REGISTRATION.fetch_add(1, Ordering::Relaxed),
                    analysis_error: error.clone(),
                    failed_input_hash,
                    status: LiveStatus::new(
                        WorkspaceStatus {
                            workspace_root: root,
                            database,
                            phase: if cancel_pending {
                                "cancelling"
                            } else if outcome_code == "cancelled" {
                                "cancelled"
                            } else {
                                "queued"
                            }
                            .into(),
                            observed_epoch: 1,
                            published_epoch: 0,
                            generation,
                            available: true,
                            error,
                            progress: None,
                            outcome_code,
                            requested_settings,
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
                    pending: true,
                    cancel_pending,
                    recovery_blocked: false,
                    recovery_explicit: false,
                    recovering: false,
                    upgrade_format: None,
                    queued: Instant::now(),
                    active: Instant::now() - Duration::from_secs(60),
                },
            );
        }
    }
    for entry in entries.values_mut() {
        if let Err(error) = restore_recovery(entry) {
            block_lifecycle_error(entry, error.as_ref());
        }
    }
    let cursors = Arc::new(know_code_core::cursor::CursorService::new(
        data,
        entries.keys().map(PathBuf::from),
    )?);
    _endpoint.cursors = Some(Arc::clone(&cursors));
    let shared = Arc::new(Shared {
        publisher,
        entries: Mutex::new(entries),
        changed: Condvar::new(),
        registry: Mutex::new(registry),
        discovery: Mutex::new(discovery),
        stop: AtomicBool::new(false),
        maintenance: Mutex::new(None),
        analysis_running: AtomicBool::new(false),
        resources: Governor::new(Limits::load(data)?)?,
        active_job: Mutex::new(None),
        overflow: AtomicBool::new(false),
        clients: AtomicUsize::new(0),
        cursors,
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
    let mut pending_unwatch = Vec::new();
    let mut running = false;
    let mut last_presence = Instant::now() - Duration::from_secs(10);
    loop {
        let mut removals = std::mem::take(&mut pending_unwatch);
        let _ = shared.cursors.sweep();
        if !maintenance_active(&shared) && !shared.stop.load(Ordering::Acquire) {
            let mut discovery = shared.discovery.lock().unwrap();
            if let Some((parent, repository)) = discovery.candidate() {
                let suppressed = shared.registry.lock().unwrap().query_row(
                    "SELECT EXISTS(SELECT 1 FROM discovery_suppressed WHERE root=?)",
                    [repository.to_string_lossy().as_ref()],
                    |r| r.get::<_, bool>(0),
                );
                match suppressed {
                    Ok(true) => discovery.outcome(&parent, true, None),
                    Ok(false) => {
                        let request = ControlRequest {
                            operation: "register".into(),
                            workspace_root: repository.to_string_lossy().into_owned(),
                            ..Default::default()
                        };
                        match register_workspace(&shared, data, &request, true) {
                            Ok(_) => {
                                discovery.outcome(&parent, false, None);
                                shared.changed.notify_all();
                            }
                            Err(error) => {
                                discovery.outcome(&parent, false, Some(&error.to_string()))
                            }
                        }
                    }
                    Err(error) => discovery.outcome(&parent, false, Some(&error.to_string())),
                }
            }
        }
        if shared.overflow.swap(false, Ordering::AcqRel) {
            let seen = shared.publisher.overflow_observed();
            mark_rescan(&shared);
            shared.publisher.overflow_handled(seen);
        }
        if last_presence.elapsed() >= Duration::from_secs(2) {
            let mut entries = shared.entries.lock().unwrap();
            for entry in entries.values_mut() {
                maintain_retired_indexes(entry);
                observe_selected_health(&shared, entry);
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
                        removals.push(root.clone());
                    }
                } else if matches && !entry.status.available {
                    entry.status.available = true;
                    entry.watch_pending = true;
                    // A returning workspace can repair an interrupted analysis,
                    // but only an actual publication may clear that failure.
                    let retry_analysis = !entry.analysis_error.is_empty()
                        && entry.status.outcome_code != "cancelled";
                    queue(entry, None, retry_analysis);
                }
            }
            last_presence = Instant::now();
            shared.changed.notify_all();
        }
        // FSEvents restarts its whole stream when paths change. Plan first,
        // mutate once, then reconcile every workspace across the restart gap.
        let mut additions = BTreeMap::<PathBuf, (bool, BTreeSet<String>)>::new();
        {
            let mut entries = shared.entries.lock().unwrap();
            // A removed root can also be another workspace's compiler-input
            // parent. Replan before the same batch removes that shared watch.
            if !removals.is_empty() {
                for entry in entries.values_mut().filter(|e| e.status.available) {
                    entry.watch_pending = true;
                }
            }
            for entry in entries
                .values_mut()
                .filter(|e| e.status.available && e.watch_pending)
            {
                entry.watch_pending = false;
                let root = PathBuf::from(&entry.status.workspace_root);
                let database = entry.status.database.clone();
                if watched.get(&root) != Some(&true) {
                    let plan = additions.entry(root.clone()).or_default();
                    plan.0 = true;
                    plan.1.insert(database.clone());
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
                        additions
                            .entry(parent.to_path_buf())
                            .or_default()
                            .1
                            .insert(database.clone());
                    }
                }
            }
        }
        if !additions.is_empty() || !removals.is_empty() {
            let input = shared.publisher.input();
            let mut batch = watcher.paths_mut();
            for path in removals {
                let _ = batch.remove(&path);
            }
            let mut failures = Vec::new();
            for (path, (recursive, owners)) in additions {
                if watched.contains_key(&path) {
                    let _ = batch.remove(&path);
                }
                match batch.add(
                    &path,
                    if recursive {
                        RecursiveMode::Recursive
                    } else {
                        RecursiveMode::NonRecursive
                    },
                ) {
                    Ok(()) => {
                        watched.insert(path, recursive);
                    }
                    Err(error) => failures.push((
                        owners,
                        format!(
                            "cannot watch saved compiler input {}: {error}",
                            path.display()
                        ),
                    )),
                }
            }
            // Always commit even after individual path errors; dropping an
            // uncommitted batch may leave the native watcher stopped.
            let started = batch.commit();
            if let Err(error) = started {
                return Err(format!("cannot restart native saved-file watcher: {error}").into());
            }
            {
                let mut entries = shared.entries.lock().unwrap();
                for (owners, message) in failures {
                    for owner in owners {
                        if let Some(entry) = entries.get_mut(&owner) {
                            entry.status.available = false;
                            entry.status.phase = "unavailable".into();
                            entry.status.error = message.clone();
                        }
                    }
                }
            }
            mark_rescan(&shared);
            drop(input);
            shared.changed.notify_all();
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
                                && e.pending
                                && e.queued.elapsed() >= Duration::from_millis(100)
                        })
                        .min_by_key(|(_, e)| priority(e, Instant::now()))
                        .map(|(db, _)| db.clone())
                };
                next.map(|database| -> AnyResult<Option<Job>> {
                    let entry = entries.get_mut(&database).unwrap();
                    let bad = match recovery::is_bad(Path::new(&database)) {
                        Ok(bad) => bad,
                        Err(error) => {
                            block_lifecycle_error(entry, &error);
                            persist(&shared, entry, None)?;
                            return Ok(None);
                        }
                    };
                    if !bad {
                        match selected_upgrade(Path::new(&database)) {
                            Ok(upgrade) => {
                                entry.upgrade_format = upgrade.map(|(format, generation)| {
                                    entry.status.generation =
                                        entry.status.generation.max(generation);
                                    format
                                });
                            }
                            Err(know_code_core::Error::IndexCorrupt { incarnation, .. }) => {
                                if let Err(error) = observe_bad(&shared, entry, &incarnation) {
                                    block_lifecycle_error(entry, error.as_ref());
                                }
                                persist(&shared, entry, None)?;
                                return Ok(None);
                            }
                            Err(error) => {
                                block_lifecycle_error(entry, &error);
                                persist(&shared, entry, None)?;
                                return Ok(None);
                            }
                        }
                    }
                    let session = shared.resources.start()?;
                    *shared.active_job.lock().unwrap() =
                        Some((database.clone(), session.control()));
                    shared.analysis_running.store(true, Ordering::Release);
                    let entry = entries.get_mut(&database).unwrap();
                    entry.pending = false;
                    entry.recovering = (bad || entry.upgrade_format.is_some())
                        && !entry.cancel_pending
                        && !(entry.status.outcome_code == "cancelled" && !entry.force);
                    entry.status.phase =
                        if entry.recovering && entry.upgrade_format.is_some() && !bad {
                            "upgrading"
                        } else if entry.recovering {
                            "recovering"
                        } else if entry.cancel_pending {
                            "cancelling"
                        } else if entry.status.outcome_code == "cancelled" && !entry.force {
                            "cancelled"
                        } else {
                            "analyzing"
                        }
                        .into();
                    entry.status.requested_settings = settings::resolve(
                        Path::new(&entry.status.workspace_root),
                        data,
                        &entry.configuration,
                    )
                    .ok();
                    let job = Job {
                        registration: entry.registration,
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
                        cancel_baseline: entry.cancel_pending,
                        recovering: entry.recovering,
                        upgrade_format: if entry.recovering && !bad {
                            entry.upgrade_format
                        } else {
                            None
                        },
                        recovery_explicit: std::mem::take(&mut entry.recovery_explicit),
                    };
                    Ok(Some(job))
                })
                .transpose()?
                .flatten()
            };
            if let Some(job) = job {
                running = true;
                let sender = sender.clone();
                let installation = installation.to_path_buf();
                let progress_shared = Arc::clone(&shared);
                thread::spawn(move || {
                    let database = job.database.clone();
                    let epoch = job.epoch;
                    let registration = job.registration;
                    let cancel_baseline = job.cancel_baseline;
                    let recovering = job.recovering;
                    let upgrading = job.upgrade_format.is_some();
                    let mut input_hash = String::new();
                    let result = execute(
                        job,
                        &installation,
                        &mut input_hash,
                        &mut |progress| {
                            let mut entries = progress_shared.entries.lock().unwrap();
                            if let Some(entry) = entries.get_mut(&database)
                                && entry.registration == registration
                                && entry.status.observed_epoch == epoch
                            {
                                if entry.cancel_pending && !cancel_baseline {
                                    return;
                                }
                                if progress.stage == "analyzing" {
                                    entry.status.phase = if upgrading {
                                        "upgrading"
                                    } else if recovering {
                                        "recovering"
                                    } else {
                                        "analyzing"
                                    }
                                    .into();
                                }
                                entry.status.progress = Some(progress);
                            }
                            progress_shared.changed.notify_all();
                        },
                        Some(&progress_shared.cursors),
                    )
                    .map_err(|e| Failure {
                        code: e
                            .downcast_ref::<know_code_core::Error>()
                            .map(|e| e.code())
                            .unwrap_or("analysis_error")
                            .into(),
                        index_fault: e.downcast_ref::<know_code_core::Error>().and_then(|error| {
                            match error {
                                know_code_core::Error::IndexCorrupt {
                                    database,
                                    incarnation,
                                    ..
                                } => Some((database.clone(), incarnation.clone())),
                                _ => None,
                            }
                        }),
                        message: e.to_string(),
                    });
                    let _ = sender.send(Event::Finished(JobResult {
                        registration,
                        database,
                        epoch,
                        result,
                        input_hash,
                        cancel_baseline,
                        recovering,
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
                    if entry.registration != done.registration {
                        continue;
                    }
                    entry.recovering = false;
                    entry.status.progress = None;
                    let manifest = if entry.cancel_pending && !done.cancel_baseline {
                        // The old compiler job has stopped. A fresh managed session
                        // now records the cancellation boundary without a worker.
                        entry.pending = true;
                        entry.rehash = true;
                        entry.status.phase = "cancelling".into();
                        None
                    } else if done.cancel_baseline && !entry.cancel_pending {
                        // Explicit refresh superseded this capture.
                        None
                    } else {
                        match done.result {
                            Ok(JobOutput::CancellationBaseline(inputs, newer, verified)) => {
                                entry
                                    .special_inputs
                                    .extend(special_inputs(&entry.status.workspace_root, &inputs));
                                entry.watch_pending = true;
                                if !workspace::identity(Path::new(&entry.status.workspace_root))
                                    .is_ok_and(|identity| identity == entry.identity)
                                {
                                    entry.status.available = false;
                                    entry.status.phase = "unavailable".into();
                                    entry.status.error = "workspace identity changed before cancellation acknowledgement".into();
                                    entry.pending = false;
                                } else if entry.pending
                                    || entry.status.observed_epoch != done.epoch
                                    || !newer.is_empty()
                                    || !verified
                                    || !shared.publisher.inputs_clear()
                                    || shared.overflow.load(Ordering::Acquire)
                                {
                                    entry.pending = true;
                                    entry.rehash = true;
                                    entry.status.phase = "cancelling".into();
                                } else {
                                    entry.failed_input_hash = done.input_hash.clone();
                                    entry.cancel_pending = false;
                                    entry.analysis_error =
                                        know_code_core::Error::Cancelled.to_string();
                                    entry.status.error = entry.analysis_error.clone();
                                    entry.status.outcome_code = "cancelled".into();
                                    entry.status.phase = if entry.status.available {
                                        "cancelled"
                                    } else {
                                        "unavailable"
                                    }
                                    .into();
                                }
                                None
                            }
                            Ok(JobOutput::Indexed(generation, inputs, newer)) => {
                                entry.recovery_blocked = false;
                                entry.upgrade_format = None;
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
                                } else if entry.pending {
                                    "queued"
                                } else {
                                    "idle"
                                }
                                .into();
                                Some(inputs)
                            }
                            Err(error) => {
                                let recovery_state = if done.recovering {
                                    recovery::failure(Path::new(&done.database))
                                } else {
                                    Ok(None)
                                };
                                let recovery_failed = matches!(recovery_state, Ok(Some(_)));
                                if recovery_failed {
                                    entry.recovery_blocked = !entry.recovery_explicit;
                                    entry.pending = entry.recovery_explicit;
                                    entry.force = entry.recovery_explicit;
                                    entry.cancel_pending = false;
                                }
                                entry.analysis_error = if entry.cancel_pending {
                                    format!(
                                        "cancellation is pending; saved-input baseline failed: {}",
                                        error.message
                                    )
                                } else {
                                    error.message.clone()
                                };
                                entry.status.outcome_code = error.code.clone();
                                entry.failed_input_hash = done.input_hash.clone();
                                entry.status.error = entry.analysis_error.clone();
                                entry.status.phase = if !entry.status.available {
                                    "unavailable"
                                } else if entry.cancel_pending {
                                    if error.code == "cancelled" {
                                        entry.pending = true;
                                    }
                                    if entry.pending {
                                        "cancelling"
                                    } else if error.code == "resource_limited" {
                                        "resource_limited"
                                    } else {
                                        "failed"
                                    }
                                } else if error.code == "cancelled" && entry.pending && entry.force
                                {
                                    "queued"
                                } else if error.code == "cancelled" {
                                    "cancelled"
                                } else if entry.pending {
                                    "queued"
                                } else {
                                    match error.code.as_str() {
                                        "cancelled" => "cancelled",
                                        "resource_limited" => "resource_limited",
                                        _ => "failed",
                                    }
                                }
                                .into();
                                if recovery_failed {
                                    entry.status.phase = if entry.recovery_explicit {
                                        "queued"
                                    } else if error.code == "resource_limited" {
                                        "resource_limited"
                                    } else if error.code == "cancelled" {
                                        "cancelled"
                                    } else if upgrade_failure(Path::new(&done.database))
                                        .unwrap_or(false)
                                    {
                                        "upgrade_failed"
                                    } else {
                                        "recovery_failed"
                                    }
                                    .into();
                                } else if let Some((database, incarnation)) = &error.index_fault
                                    && database == Path::new(&done.database)
                                    && let Err(error) = observe_bad(&shared, entry, incarnation)
                                {
                                    block_lifecycle_error(entry, error.as_ref());
                                }
                                if let Err(error) = recovery_state {
                                    block_lifecycle_error(entry, &error);
                                }
                                None
                            }
                        }
                    };
                    if let Err(error) = persist(&shared, entry, manifest.as_ref()) {
                        if done.cancel_baseline {
                            entry.cancel_pending = true;
                            entry.status.phase = "failed".into();
                            entry.status.error =
                                format!("cancellation baseline could not be recorded: {error}");
                        }
                        return Err(error);
                    }
                    maintain_retired_indexes(entry);
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
                    pending_unwatch.push(root);
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
    cursors: Option<&know_code_core::cursor::CursorService>,
) -> AnyResult<JobOutput> {
    let control = job.session.control();
    let mut candidate: Option<recovery::Candidate> = None;
    let mut disk: Option<Arc<recovery::DiskBudget>> = None;
    let result = (|| -> AnyResult<JobOutput> {
        control.check()?;
        if workspace::identity(&job.root)? != job.identity {
            return Err("workspace identity changed before saved-input reconciliation".into());
        }
        let database = Path::new(&job.database);
        let data = control::root_for_database(database)
            .ok_or("workspace database has no central data root")?;
        let settings_paths = settings::paths(&job.root, data);
        if job.recovering {
            candidate = Some(if let Some(format) = job.upgrade_format {
                recovery::begin_upgrade(
                    database,
                    job.recovery_explicit,
                    job.previous_generation,
                    format,
                )?
            } else {
                recovery::begin_rebuild(database, job.recovery_explicit, job.previous_generation)?
            });
            let guard = Arc::new(recovery::DiskBudget::new(database, data)?);
            guard.check(&job.session.limits(), false)?;
            let sampled = Arc::clone(&guard);
            control.guard_disk(move |limits| sampled.check(limits, false));
            disk = Some(guard);
            progress(Progress {
                stage: if job.upgrade_format.is_some() {
                    "upgrading"
                } else {
                    "recovering"
                }
                .into(),
                project_path: String::new(),
                message: if job.upgrade_format.is_some() {
                    "Building an upgraded index alongside the prior saved graph"
                } else {
                    "Rebuilding a fenced derived index from saved inputs"
                }
                .into(),
            });
        }
        let mut extras =
            if job.recovering || job.cancel_baseline || job.previous_error_code == "cancelled" {
                workspace::extra_inputs_at(None, &job.root)?
            } else {
                workspace::extra_inputs(database, &job.root)?
            };
        extras.extend(settings_paths.clone());
        let mut forced_paths = job.paths.clone();
        if job.rehash {
            forced_paths.push(job.root.clone());
            forced_paths.extend(extras.clone());
        }
        let before = workspace::snapshot_controlled(
            &job.root,
            &job.inputs,
            &forced_paths,
            &extras,
            &control,
        )?;
        let mut hash = Sha256::new();
        hash.update(b"know-code-saved-input-v2\0");
        hash.update(job.configuration.as_bytes());
        hash.update([0]);
        for (path, stamp) in &before {
            hash.update(path.as_bytes());
            hash.update([0]);
            hash.update(stamp.hash.as_bytes());
            hash.update([0]);
        }
        // Settings bytes and legacy selection are already part of this identity.
        // Cancellation must also work when those bytes are malformed.
        *attempt_hash = format!("{:x}", hash.finalize());
        control.check()?;
        if job.cancel_baseline {
            progress(Progress {
                stage: "cancelling".into(),
                project_path: String::new(),
                message: "Verifying saved-input cancellation baseline".into(),
            });
            let mut verify = vec![job.root.clone()];
            verify.extend(extras.clone());
            let after =
                workspace::snapshot_controlled(&job.root, &before, &verify, &extras, &control)?;
            if workspace::identity(&job.root)? != job.identity {
                return Err("workspace identity changed during cancellation capture".into());
            }
            let newer = workspace::differences(&before, &after);
            let verified = workspace::snapshot_metadata_matches(&job.root, &after, &control)?;
            return Ok(JobOutput::CancellationBaseline(after, newer, verified));
        }
        if !job.force
            && !job.recovering
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
        let effective_settings = settings::resolve(&job.root, data, &job.configuration)?;
        if job.previous_error_code == "cancelled" {
            progress(Progress {
                stage: "analyzing".into(),
                project_path: String::new(),
                message: "Saved inputs changed after cancellation; resuming analysis".into(),
            });
        }
        let changes = workspace::differences(&job.inputs, &before);
        let (recovered, classification_backfill) = if candidate.is_some() {
            (0, false)
        } else {
            selected_readiness(database)?
        };
        if !job.force
            && !classification_backfill
            && changes.is_empty()
            && recovered != 0
            && recovered == job.previous_generation
        {
            // Reconciliation proves only that saved inputs did not change. It
            // cannot repair an earlier analyzer/storage failure or certify facts
            // that the failed refresh never published.
            if job.previous_error.is_empty() {
                return Ok(JobOutput::Indexed(recovered, before, vec![]));
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
            || classification_backfill
            || !job.previous_error.is_empty()
            || recovered == 0
            || recovered != job.previous_generation
        {
            None
        } else {
            workspace::affected_projects(database, &job.root, &changes)?
        };
        let preparation_fingerprint = workspace::preparation_fingerprint(&before, &settings_paths);
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
                settings: &effective_settings,
                cursors,
                destination: candidate.as_ref(),
            },
            progress,
            &control,
        )?;
        let mut extras = if let Some(candidate) = &candidate {
            workspace::extra_inputs_at(Some(&candidate.database), &job.root)?
        } else {
            workspace::extra_inputs(database, &job.root)?
        };
        extras.extend(settings_paths);
        let after =
            workspace::snapshot_controlled(&job.root, &before, &forced_paths, &extras, &control)?;
        let newer = workspace::differences(&before, &after);
        // Newly discovered immutable inputs can safely enter the acknowledged manifest only
        // after a follow-up reconciliation; saves during analysis cannot disappear.
        let generation = if let Some(candidate) = &candidate {
            let prepared = recovery::prepare_rebuild(candidate, &control)?;
            let cutover = || {
                recovery::finish_prepared_rebuild(&prepared, &control, || {
                    control.check()?;
                    if workspace::identity(&job.root)? != job.identity {
                        return Err(know_code_core::Error::Provider(
                            "workspace identity changed before replacement cutover".into(),
                        ));
                    }
                    disk.as_ref()
                        .expect("candidate disk guard")
                        .check(&job.session.limits(), true)
                })
            };
            if job.upgrade_format.is_some()
                && let Some(cursors) = cursors
            {
                cursors.publish_replacement(database, &control, cutover)?
            } else {
                cutover()?
            }
        } else {
            generation
        };
        Ok(JobOutput::Indexed(generation, before, newer))
    })();
    if let Err(error) = &result
        && job.recovering
        && candidate.is_some()
    {
        // A rename can become visible before its directory fsync reports an
        // error. Re-observe selection: never reclassify a verified selected
        // incarnation as an abandoned pre-cutover candidate.
        let selected = candidate.as_ref().is_some_and(|c| {
            recovery::selected_incarnation(Path::new(&job.database))
                .ok()
                .flatten()
                .as_deref()
                == Some(c.incarnation.as_str())
        });
        if !selected
            && let Err(recording) =
                recovery::fail_rebuild(Path::new(&job.database), &error.to_string())
        {
            if matches!(recording, know_code_core::Error::UpdateRequired(_)) {
                return Err(recording.into());
            }
            return Err(know_code_core::Error::IndexRecovering(format!("replacement failed: {error}; failure receipt could not be recorded: {recording}; explicit refresh is required")).into());
        }
    }
    result
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
fn begin_cancellation(entry: &mut Entry) {
    // Cancel revokes earlier queued refresh authorization. A later refresh
    // sets it again under the same entries lock and remains valid.
    entry.recovery_explicit = false;
    entry.force = false;
    if entry.recovering {
        entry.pending = false;
        entry.recovery_blocked = true;
        entry.status.phase = "cancelling".into();
        entry.status.error =
            "index replacement cancellation requested; explicit refresh will be required".into();
        return;
    }
    entry.cancel_pending = true;
    entry.retry_preparation = false;
    entry.dirty.clear();
    entry.failed_input_hash.clear();
    entry.analysis_error = know_code_core::Error::Cancelled.to_string();
    entry.status.error = entry.analysis_error.clone();
    entry.status.outcome_code = "cancelled".into();
    queue(entry, None, false);
    entry.status.phase = "cancelling".into();
    entry.status.progress = None;
}
fn queue(entry: &mut Entry, paths: Option<Vec<PathBuf>>, force: bool) {
    if entry.recovery_blocked && !entry.recovery_explicit {
        entry.rehash = true;
        entry.status.observed_epoch += 1;
        return;
    }
    if !entry.pending {
        entry.queued = Instant::now();
    }
    entry.pending = true;
    entry.status.observed_epoch += 1;
    entry.force |= force;
    if let Some(paths) = paths {
        entry.dirty.extend(paths);
        if entry.dirty.len() > 4096 {
            entry.dirty.clear();
            entry.rehash = true;
        }
    } else {
        entry.rehash = true;
    }
    if !matches!(
        entry.status.phase.as_str(),
        "analyzing" | "recovering" | "upgrading"
    ) && entry.status.available
    {
        entry.status.phase = if entry.cancel_pending {
            "cancelling"
        } else if entry.status.outcome_code == "cancelled" && !force {
            "cancelled"
        } else {
            "queued"
        }
        .into();
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
fn persist(shared: &Shared, entry: &Entry, inputs: Option<&Snapshot>) -> AnyResult<()> {
    let status = &entry.status;
    let configuration = &entry.configuration;
    let identity = &entry.identity;
    let analysis_error = &entry.analysis_error;
    let failed_input_hash = &entry.failed_input_hash;
    let cancel_pending = entry.cancel_pending;
    let mut registry = shared.registry.lock().unwrap();
    let tx = registry.transaction()?;
    let encoded = inputs.map(serde_json::to_string).transpose()?;
    tx.execute("INSERT INTO workspaces(database,root,configuration,generation,inputs,error,failed_input_hash,outcome_code,cancel_pending) VALUES(?,?,?,?,'{}',?,?,?,?) ON CONFLICT(database) DO UPDATE SET root=excluded.root,configuration=excluded.configuration,generation=excluded.generation,error=excluded.error,failed_input_hash=excluded.failed_input_hash,outcome_code=excluded.outcome_code,cancel_pending=excluded.cancel_pending",params![status.database,status.workspace_root,configuration,status.generation,analysis_error,failed_input_hash,status.outcome_code,cancel_pending])?;
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
fn register_workspace(
    shared: &Shared,
    data: &Path,
    request: &ControlRequest,
    discovered: bool,
) -> AnyResult<WorkspaceStatus> {
    let root = std::fs::canonicalize(&request.workspace_root)?;
    let actual_identity = workspace::identity(&root)?;
    let database = workspace::database_path(&root, data)
        .to_string_lossy()
        .into_owned();
    let configuration = request.configuration.as_str();
    let persistent = {
        let mut entries = shared.entries.lock().unwrap();
        if discovered {
            let suppressed: bool = shared.registry.lock().unwrap().query_row(
                "SELECT EXISTS(SELECT 1 FROM discovery_suppressed WHERE root=?)",
                [root.to_string_lossy().as_ref()],
                |r| r.get(0),
            )?;
            if suppressed {
                return Err("repository enrollment is explicitly suppressed".into());
            }
            if let Some(entry) = entries.get(&database) {
                if entry.identity != actual_identity {
                    return Err(
                        "workspace identity changed; explicitly unregister the old workspace"
                            .into(),
                    );
                }
                return Ok(entry.status.snapshot());
            }
        }
        if entries
            .get(&database)
            .is_some_and(|entry| entry.identity != actual_identity)
        {
            return Err(
                "workspace identity changed; unwatch before registering the replacement".into(),
            );
        }
        std::fs::create_dir_all(
            Path::new(&database)
                .parent()
                .ok_or("workspace database parent missing")?,
        )?;
        // Never promote a new personal choice into legacy fallback state.
        let legacy = entries
            .get(&database)
            .map(|entry| entry.configuration.clone())
            .unwrap_or_default();
        let mut settings_changed = false;
        if !configuration.is_empty() {
            let mut personal = settings::personal(&root, data)?;
            if !matches!(&personal.csharp.configuration, settings::Selection::Named(value) if value == configuration)
            {
                personal.csharp.configuration = settings::Selection::Named(configuration.into());
                settings::save(&root, data, &legacy, &personal)?;
                settings_changed = true;
            }
        }
        let requested_settings = settings::resolve(&root, data, &legacy).ok();
        let existing_entry = entries.contains_key(&database);
        recovery::adopt(
            Path::new(&database),
            entries
                .get(&database)
                .map(|e| e.status.generation)
                .unwrap_or(0),
        )?;
        let entry = entries.entry(database.clone()).or_insert_with(|| Entry {
            registration: REGISTRATION.fetch_add(1, Ordering::Relaxed),
            analysis_error: String::new(),
            failed_input_hash: String::new(),
            status: LiveStatus::new(
                WorkspaceStatus {
                    workspace_root: root.to_string_lossy().into_owned(),
                    database,
                    phase: "queued".into(),
                    observed_epoch: 1,
                    available: true,
                    requested_settings,
                    ..Default::default()
                },
                &shared.publisher,
            ),
            configuration: legacy,
            identity: actual_identity.clone(),
            special_inputs: settings::paths(&root, data).into_iter().collect(),
            dirty: BTreeSet::new(),
            force: false,
            rehash: true,
            retry_preparation: false,
            watch_pending: true,
            pending: true,
            cancel_pending: false,
            recovery_blocked: false,
            recovery_explicit: false,
            recovering: false,
            upgrade_format: None,
            queued: Instant::now(),
            active: Instant::now(),
        });
        if entry.identity != actual_identity {
            return Err(
                "workspace identity changed; unwatch before registering the replacement".into(),
            );
        }
        if !existing_entry {
            restore_recovery(entry)?;
        }
        if settings_changed {
            queue(entry, None, true);
        }
        entry.status.requested_settings = settings::resolve(&root, data, &entry.configuration).ok();
        if request.operation == "refresh" {
            entry.recovery_explicit = true;
            entry.recovery_blocked = false;
            if entry.cancel_pending {
                entry.cancel_pending = false;
                if let Some((_, control)) = shared
                    .active_job
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|(db, _)| db == &entry.status.database)
                {
                    control.cancel();
                }
            }
            entry.retry_preparation = true;
            queue(entry, None, true);
        }
        persist(shared, entry, None)?;
        shared.cursors.register(Path::new(&entry.status.database))?;
        if !discovered {
            shared.registry.lock().unwrap().execute(
                "DELETE FROM discovery_suppressed WHERE root=?",
                [root.to_string_lossy().as_ref()],
            )?;
        }
        entry.status.snapshot()
    };
    Ok(persistent)
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
        if request.index_lifecycle_version != protocol::INDEX_LIFECYCLE_VERSION {
            return Err(know_code_core::Error::UpdateRequired("update the coordinator and native libraries together; index lifecycle safety is incompatible".into()).into());
        }
        if request.operation == "index-corrupt" {
            let report = request
                .corruption
                .ok_or("missing index corruption report")?;
            if report.detail.len() > 8192 {
                return Err("oversized corruption detail".into());
            }
            let mut entries = shared.entries.lock().unwrap();
            let entry = entries
                .get_mut(&request.database)
                .ok_or("workspace is not registered")?;
            observe_bad(shared, entry, &report.incarnation)?;
            let status = entry.status.snapshot();
            persist(shared, entry, None)?;
            drop(entries);
            wake(sender, shared, Event::Wake);
            shared.changed.notify_all();
            return Ok(ControlResponse {
                workspaces: vec![status],
                ..Default::default()
            });
        }
        if request.operation == "parents" {
            let request = request
                .parent_discovery
                .ok_or("missing parent discovery request")?;
            let mut discovery = shared.discovery.lock().unwrap();
            let parents = discovery.action(&shared.registry.lock().unwrap(), request)?;
            wake(sender, shared, Event::Wake);
            return Ok(ControlResponse {
                parents,
                ..Default::default()
            });
        }
        if request.operation == "cursor" {
            let start = know_code_core::execution::QueryStart::default();
            let cursor = request.cursor.ok_or("missing cursor request")?;
            let status = if cursor.action == "settings" {
                None
            } else {
                Some(
                    shared
                        .entries
                        .lock()
                        .unwrap()
                        .get(&request.database)
                        .ok_or(know_code_core::Error::CursorInvalidated)?
                        .status
                        .snapshot(),
                )
            };
            if cursor.action == "admit" && status.as_ref().is_some_and(|s| s.generation == 0) {
                return Err(know_code_core::Error::NotReady.into());
            }
            let reply = shared
                .cursors
                .request(Path::new(&request.database), cursor, start)?;
            return Ok(ControlResponse {
                cursor: Some(reply),
                workspaces: status.into_iter().collect(),
                ..Default::default()
            });
        }
        if request.operation == "activity" {
            one_way = true;
            let mut entries = shared.entries.lock().unwrap();
            if let Some(entry) = entries.get_mut(&request.database) {
                entry.active = Instant::now();
                if observe_selected_health(shared, entry) {
                    wake(sender, shared, Event::Wake);
                }
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
                    .filter(|e| {
                        matches!(
                            e.status.phase.as_str(),
                            "analyzing" | "recovering" | "cancelling"
                        )
                    })
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
                    rebuild_disk_mib: limits.rebuild_disk_mib,
                    min_free_disk_mib: limits.min_free_disk_mib,
                };
                limits.validate()?;
                if shared.resources.configure(data, limits)? {
                    let mut entries = shared.entries.lock().unwrap();
                    let active = shared.active_job.lock().unwrap();
                    for (db, entry) in entries.iter_mut() {
                        if !entry.recovery_blocked
                            && (entry.status.outcome_code == "resource_limited"
                                || active.as_ref().is_some_and(|(running, control)| {
                                    running == db && control.resource_limited()
                                }))
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
                    rebuild_disk_mib: limits.rebuild_disk_mib,
                    min_free_disk_mib: limits.min_free_disk_mib,
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
            let accepted = if entry.cancel_pending {
                false
            } else if let Some((_, control)) =
                active.as_ref().filter(|(db, _)| db == &request.database)
            {
                control.cancel() || !control.committed()
            } else {
                entry.pending && entry.status.available
            };
            if accepted {
                begin_cancellation(entry);
                persist(shared, entry, None)?;
            }
            drop(active);
            wake(sender, shared, Event::Wake);
            shared.changed.notify_all();
            return Ok(ControlResponse {
                workspaces: vec![entry.status.snapshot()],
                ..Default::default()
            });
        }
        if request.operation == "settings" {
            let mut entries = shared.entries.lock().unwrap();
            let entry = entries
                .get_mut(&request.database)
                .ok_or("workspace is not registered")?;
            let root = Path::new(&entry.status.workspace_root);
            let effective = if let Some(json) = request.settings_json {
                let value: settings::PersonalSettings =
                    serde_json::from_str(&json).map_err(know_code_core::Error::from)?;
                let effective = settings::save(root, data, &entry.configuration, &value)?;
                // Settings changes are analysis invalidation, not restore retries.
                queue(entry, None, true);
                effective
            } else {
                settings::resolve(root, data, &entry.configuration)?
            };
            entry.status.requested_settings = Some(effective);
            let status = entry.status.snapshot();
            wake(sender, shared, Event::Wake);
            shared.changed.notify_all();
            return Ok(ControlResponse {
                workspaces: vec![status],
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
            if entries[&database].status.phase == "analyzing" || entries[&database].recovering {
                return Err("wait for the active update before unregistering".into());
            }
            shared.cursors.unregister(Path::new(&database))?;
            settings::revoke_scripts(Path::new(&request.workspace_root), data)?;
            let mut registry = shared.registry.lock().unwrap();
            let tx = registry.transaction()?;
            tx.execute(
                "INSERT OR IGNORE INTO discovery_suppressed VALUES(?)",
                [&request.workspace_root],
            )?;
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
            let persistent = register_workspace(shared, data, &request, false)?;
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
                let entry = &entries[&request.database];
                let status = &entry.status;
                let reconciling = entry.pending
                    || shared
                        .active_job
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|(db, _)| db == &request.database);
                if status.published_epoch >= target
                    || matches!(
                        status.phase.as_str(),
                        "failed" | "unavailable" | "resource_limited" | "recovery_failed"
                    )
                    || (status.phase == "cancelled" && !reconciling)
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
        let workspaces = entries
            .values()
            .filter(|e| request.database.is_empty() || e.status.database == request.database)
            .map(|e| e.status.snapshot())
            .collect();
        drop(entries);
        Ok(ControlResponse {
            parents: if request.operation == "status" && request.database.is_empty() {
                shared.discovery.lock().unwrap().statuses()
            } else {
                vec![]
            },
            workspaces,
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
    response.index_lifecycle_version = protocol::INDEX_LIFECYCLE_VERSION;
    response.resource_control_version = know_code_core::resource::CONTROL_VERSION;
    response.analysis_settings_version = settings::VERSION;
    response.cursor_control_version = know_code_core::cursor::CONTROL_VERSION;
    response.parent_discovery_version = crate::discovery::VERSION;
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
    cursors: Option<Arc<know_code_core::cursor::CursorService>>,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        // Retire the certificate before releasing the root writer fence, even
        // if callback/handler Arcs keep Shared alive after run() returns.
        self.publisher.disable();
        if let Some(cursors) = &self.cursors {
            cursors.stop();
        }
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
    fn cancellation_job() -> Job {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/cancel-unit")
            .join(format!(
                "{}-{}",
                std::process::id(),
                REGISTRATION.fetch_add(1, Ordering::Relaxed)
            ))
            .join("w");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("value.ts"), "export const value = 1;\n").unwrap();
        let data = root.parent().unwrap().join("d");
        Job {
            registration: 1,
            database: workspace::database_path(&root, &data)
                .to_string_lossy()
                .into_owned(),
            identity: workspace::identity(&root).unwrap(),
            root,
            configuration: String::new(),
            inputs: Snapshot::new(),
            paths: vec![],
            force: false,
            rehash: true,
            retry_preparation: false,
            epoch: 1,
            previous_generation: 0,
            previous_error: know_code_core::Error::Cancelled.to_string(),
            previous_error_code: "cancelled".into(),
            previous_failed_input_hash: String::new(),
            session: Governor::new(Limits::default()).unwrap().start().unwrap(),
            cancel_baseline: true,
            recovering: false,
            upgrade_format: None,
            recovery_explicit: false,
        }
    }
    fn recovery_job() -> Job {
        let mut job = cancellation_job();
        job.cancel_baseline = false;
        job.recovering = true;
        job.previous_error.clear();
        job.previous_error_code.clear();
        let parent = Path::new(&job.database).parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        recovery::adopt(Path::new(&job.database), 0).unwrap();
        let mut writer = know_code_core::store::Writer::open(Path::new(&job.database)).unwrap();
        writer
            .stage(Payload::Compilation(
                know_code_core::protocol::Compilation {
                    id: "fixture".into(),
                    ..Default::default()
                },
            ))
            .unwrap();
        writer
            .stage(Payload::Report(know_code_core::protocol::AnalysisReport {
                compilation_id: "fixture".into(),
                status: "complete".into(),
                ..Default::default()
            }))
            .unwrap();
        job.previous_generation = writer.publish().unwrap();
        let guard = recovery::IndexReadGuard::open_unbudgeted(Path::new(&job.database)).unwrap();
        assert!(matches!(
            guard.note_corruption("fixture corruption proof"),
            Err(know_code_core::Error::IndexCorrupt { .. })
        ));
        job.session = Governor::new(Limits {
            min_free_disk_mib: 0,
            ..Default::default()
        })
        .unwrap()
        .start()
        .unwrap();
        job
    }
    #[test]
    fn fenced_index_cannot_bypass_an_unchanged_saved_cancellation_baseline() {
        let job = cancellation_job();
        let root = job.root.clone();
        let database = job.database.clone();
        let mut hash = String::new();
        let JobOutput::CancellationBaseline(inputs, _, _) = execute(
            job,
            Path::new("/no/compiler/installation"),
            &mut hash,
            &mut |_| {},
            None,
        )
        .unwrap() else {
            panic!("baseline indexed");
        };
        std::fs::create_dir_all(Path::new(&database).parent().unwrap()).unwrap();
        recovery::adopt(Path::new(&database), 0).unwrap();
        let guard = recovery::IndexReadGuard::open_unbudgeted(Path::new(&database)).unwrap();
        assert!(guard.note_corruption("fixture corruption proof").is_err());
        let mut retry = cancellation_job();
        retry.root = root;
        retry.database = database.clone();
        retry.identity = workspace::identity(&retry.root).unwrap();
        retry.inputs = inputs;
        retry.cancel_baseline = false;
        retry.previous_failed_input_hash = hash.clone();
        let mut next_hash = String::new();
        let error = execute(
            retry,
            Path::new("/no/compiler/installation"),
            &mut next_hash,
            &mut |_| {},
            None,
        )
        .err()
        .expect("cancelled data was reanalyzed");
        assert!(
            matches!(error.downcast_ref::<know_code_core::Error>(),Some(know_code_core::Error::Remote{code,..}) if code=="cancelled")
        );
        assert_eq!(next_hash, hash);
        assert!(
            !recovery::blocked(Path::new(&database)).unwrap(),
            "no recovery attempt was authorized or consumed"
        );
    }
    #[test]
    fn recovery_setup_failure_consumes_one_attempt_and_only_explicit_refresh_retries() {
        let job = recovery_job();
        let database = job.database.clone();
        let root = job.root.clone();
        std::fs::write(root.join(settings::PROJECT_FILE), "broken settings").unwrap();
        let mut hash = String::new();
        assert!(
            execute(
                job,
                Path::new("/no/compiler/installation"),
                &mut hash,
                &mut |_| {},
                None
            )
            .is_err()
        );
        assert!(recovery::blocked(Path::new(&database)).unwrap());
        let directories = || {
            std::fs::read_dir(Path::new(&database).parent().unwrap().join("indexes"))
                .unwrap()
                .count()
        };
        let first = directories();
        let retry = |explicit| {
            let mut job = cancellation_job();
            job.root = root.clone();
            job.database = database.clone();
            job.identity = workspace::identity(&root).unwrap();
            job.cancel_baseline = false;
            job.recovering = true;
            job.recovery_explicit = explicit;
            job.session = Governor::new(Limits {
                min_free_disk_mib: 0,
                ..Default::default()
            })
            .unwrap()
            .start()
            .unwrap();
            job
        };
        assert!(
            execute(
                retry(false),
                Path::new("/no/compiler/installation"),
                &mut hash,
                &mut |_| {},
                None
            )
            .is_err()
        );
        assert_eq!(
            directories(),
            first,
            "ordinary retry allocated a new candidate"
        );
        assert!(
            execute(
                retry(true),
                Path::new("/no/compiler/installation"),
                &mut hash,
                &mut |_| {},
                None
            )
            .is_err()
        );
        assert_eq!(
            directories(),
            first + 1,
            "explicit refresh did not authorize exactly one new attempt"
        );
    }
    #[test]
    fn explicit_recovery_retry_survives_duplicate_bad_observations_behind_busy_sibling() {
        let job = recovery_job();
        let database = PathBuf::from(&job.database);
        let data = job.root.parent().unwrap().join("d");
        let incarnation = recovery::selected_incarnation(&database).unwrap().unwrap();
        recovery::begin_rebuild(&database, false, job.previous_generation).unwrap();
        recovery::fail_rebuild(&database, "first attempt failed").unwrap();
        let publisher = Arc::new(Publisher::new(&data));
        let mut entry = Entry {
            registration: 1,
            analysis_error: String::new(),
            failed_input_hash: String::new(),
            status: LiveStatus::new(
                WorkspaceStatus {
                    database: job.database.clone(),
                    workspace_root: job.root.to_string_lossy().into_owned(),
                    available: true,
                    generation: job.previous_generation,
                    ..Default::default()
                },
                &publisher,
            ),
            configuration: String::new(),
            identity: job.identity.clone(),
            special_inputs: BTreeSet::new(),
            dirty: BTreeSet::new(),
            force: false,
            rehash: false,
            retry_preparation: false,
            queued: Instant::now(),
            active: Instant::now(),
            watch_pending: false,
            pending: false,
            cancel_pending: false,
            recovery_blocked: true,
            recovery_explicit: false,
            recovering: false,
            upgrade_format: None,
        };
        let shared = Shared {
            entries: Mutex::new(BTreeMap::new()),
            publisher,
            changed: Condvar::new(),
            registry: Mutex::new(Connection::open_in_memory().unwrap()),
            discovery: Mutex::new(
                crate::discovery::Discovery::open(&Connection::open_in_memory().unwrap()).unwrap(),
            ),
            stop: AtomicBool::new(false),
            maintenance: Mutex::new(None),
            analysis_running: AtomicBool::new(true),
            resources: Governor::new(Limits::default()).unwrap(),
            active_job: Mutex::new(Some(("healthy-sibling".into(), job.session.control()))),
            overflow: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
            cursors: Arc::new(
                know_code_core::cursor::CursorService::new(&data, Vec::new()).unwrap(),
            ),
        };
        // Ordinary presence/report/activity observations cannot authorize a retry.
        for _ in 0..3 {
            assert!(observe_bad(&shared, &mut entry, &incarnation).unwrap());
            queue(&mut entry, None, false);
            assert!(entry.recovery_blocked && !entry.pending && !entry.recovery_explicit);
        }
        // The refresh path queues exactly one authorization while the sibling
        // still owns the shared analysis slot.
        entry.recovery_explicit = true;
        entry.recovery_blocked = false;
        queue(&mut entry, None, true);
        let queued = entry.queued;
        for _ in 0..3 {
            assert!(observe_bad(&shared, &mut entry, &incarnation).unwrap());
            queue(&mut entry, None, false);
            assert!(
                !entry.recovery_blocked && entry.pending && entry.force && entry.recovery_explicit
            );
            assert_eq!(entry.status.phase, "queued");
            assert_eq!(entry.queued, queued);
            job.session.control().check().unwrap();
            assert!(recovery::failure(&database).unwrap().is_some());
        }
        // Dispatch consumes the authorization; duplicate wakes cannot allocate
        // a second candidate or revive this attempt after another failure.
        shared.active_job.lock().unwrap().take();
        let explicit = std::mem::take(&mut entry.recovery_explicit);
        entry.pending = false;
        recovery::begin_rebuild(&database, explicit, job.previous_generation).unwrap();
        assert!(recovery::begin_rebuild(&database, false, job.previous_generation).is_err());
        // Refresh during this active recovery can authorize a later retry, but
        // a subsequent accepted cancel must revoke that older authorization.
        entry.recovering = true;
        entry.recovery_explicit = true;
        queue(&mut entry, None, true);
        begin_cancellation(&mut entry);
        assert!(!entry.pending && !entry.force && !entry.recovery_explicit);
        recovery::fail_rebuild(&database, "explicit attempt failed").unwrap();
        for _ in 0..3 {
            assert!(observe_bad(&shared, &mut entry, &incarnation).unwrap());
            assert!(entry.recovery_blocked && !entry.pending && !entry.recovery_explicit);
        }
        // The opposite ordering is valid: a refresh received after cancellation
        // is new authorization and late notifications must preserve it.
        entry.recovery_explicit = true;
        entry.recovery_blocked = false;
        queue(&mut entry, None, true);
        assert!(observe_bad(&shared, &mut entry, &incarnation).unwrap());
        assert!(entry.pending && entry.force && entry.recovery_explicit);
    }
    #[test]
    fn selected_readiness_preserves_version_errors_but_fences_actual_index_header_corruption() {
        use std::os::unix::fs::FileExt;
        let job = recovery_job();
        let database = job.database.clone();
        drop(job);
        // A separate owned healthy workspace starts without a BAD latch.
        let mut other = cancellation_job();
        other.cancel_baseline = false;
        std::fs::create_dir_all(Path::new(&other.database).parent().unwrap()).unwrap();
        recovery::adopt(Path::new(&other.database), 0).unwrap();
        let mut writer = know_code_core::store::Writer::open(Path::new(&other.database)).unwrap();
        writer
            .stage(Payload::Compilation(
                know_code_core::protocol::Compilation {
                    id: "healthy".into(),
                    ..Default::default()
                },
            ))
            .unwrap();
        writer
            .stage(Payload::Report(know_code_core::protocol::AnalysisReport {
                compilation_id: "healthy".into(),
                status: "complete".into(),
                ..Default::default()
            }))
            .unwrap();
        writer.publish().unwrap();
        let physical = recovery::selected_path(Path::new(&other.database)).unwrap();
        let connection = Connection::open(&physical).unwrap();
        connection.execute_batch("PRAGMA user_version=999").unwrap();
        drop(connection);
        assert!(matches!(
            selected_readiness(Path::new(&other.database)),
            Err(know_code_core::Error::UpdateRequired(_))
        ));
        assert!(!recovery::is_bad(Path::new(&other.database)).unwrap());
        let connection = Connection::open(&physical).unwrap();
        connection
            .execute_batch(&format!(
                "PRAGMA user_version={}; PRAGMA wal_checkpoint(TRUNCATE)",
                know_code_core::store::SCHEMA_VERSION
            ))
            .unwrap();
        drop(connection);
        OpenOptions::new()
            .write(true)
            .open(&physical)
            .unwrap()
            .write_all_at(b"broken-db-header", 0)
            .unwrap();
        assert!(
            matches!(selected_readiness(Path::new(&other.database)),Err(know_code_core::Error::IndexCorrupt{database:logical,..}) if logical==Path::new(&other.database))
        );
        assert!(recovery::is_bad(Path::new(&other.database)).unwrap());
        assert!(recovery::is_bad(Path::new(&database)).unwrap());
    }
    #[test]
    fn cancellation_capture_handles_invalid_settings_without_compiler_or_publication() {
        let job = cancellation_job();
        std::fs::write(job.root.join(settings::PROJECT_FILE), "broken settings").unwrap();
        let database = job.database.clone();
        let mut hash = String::new();
        let result = execute(
            job,
            Path::new("/no/compiler/installation"),
            &mut hash,
            &mut |_| {},
            None,
        )
        .unwrap();
        let JobOutput::CancellationBaseline(inputs, newer, verified) = result else {
            panic!("capture indexed facts")
        };
        assert!(verified && newer.is_empty() && !inputs.is_empty());
        assert!(!hash.is_empty());
        assert!(!Path::new(&database).exists());
    }
    #[test]
    fn unchanged_cancelled_snapshot_stays_cancelled_during_classification_backfill() {
        let job = cancellation_job();
        let root = job.root.clone();
        let database = job.database.clone();
        let mut hash = String::new();
        let captured = execute(
            job,
            Path::new("/no/compiler/installation"),
            &mut hash,
            &mut |_| {},
            None,
        )
        .unwrap();
        let JobOutput::CancellationBaseline(inputs, _, _) = captured else {
            panic!("capture published");
        };
        let mut writer = know_code_core::store::Writer::open(Path::new(&database)).unwrap();
        writer
            .stage(Payload::Compilation(
                know_code_core::protocol::Compilation {
                    id: "legacy".into(),
                    language: "typescript".into(),
                    ..Default::default()
                },
            ))
            .unwrap();
        writer
            .stage(Payload::Report(know_code_core::protocol::AnalysisReport {
                compilation_id: "legacy".into(),
                status: "complete".into(),
                ..Default::default()
            }))
            .unwrap();
        let generation = writer.publish().unwrap();
        assert!(
            ReadView::open_in_scope(
                &database,
                &know_code_core::admission::AdmissionScope::at(
                    Path::new(&database)
                        .parent()
                        .unwrap()
                        .join("query-admission")
                )
                .unwrap()
            )
            .unwrap()
            .needs_test_classification()
            .unwrap()
        );
        let mut retry = cancellation_job();
        retry.root = root;
        retry.database = database.clone();
        retry.identity = workspace::identity(&retry.root).unwrap();
        retry.inputs = inputs;
        retry.cancel_baseline = false;
        retry.previous_generation = generation;
        retry.previous_failed_input_hash = hash.clone();
        let mut retry_hash = String::new();
        let error = execute(
            retry,
            Path::new("/no/compiler/installation"),
            &mut retry_hash,
            &mut |_| {},
            None,
        )
        .err()
        .expect("cancelled snapshot resumed");
        assert_eq!(hash, retry_hash);
        assert!(
            matches!(error.downcast_ref::<know_code_core::Error>(),Some(know_code_core::Error::Remote{code,..}) if code=="cancelled")
        );
        assert_eq!(
            ReadView::open_in_scope(
                &database,
                &know_code_core::admission::AdmissionScope::at(
                    Path::new(&database)
                        .parent()
                        .unwrap()
                        .join("query-admission")
                )
                .unwrap()
            )
            .unwrap()
            .generation,
            generation
        );
    }
    #[test]
    fn cancellation_capture_detects_undelivered_save_with_preserved_mtime() {
        let job = cancellation_job();
        let source = job.root.join("value.ts");
        let saved_time = std::fs::metadata(&source).unwrap().modified().unwrap();
        let mut hash = String::new();
        let result = execute(
            job,
            Path::new("/no/compiler/installation"),
            &mut hash,
            &mut |_| {
                std::fs::write(&source, "export const value = 2;\n").unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&source)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(saved_time))
                    .unwrap();
            },
            None,
        )
        .unwrap();
        let JobOutput::CancellationBaseline(_, newer, verified) = result else {
            panic!("capture indexed facts")
        };
        assert!(
            verified && newer.contains(&source),
            "capture must retry the changed saved inputs"
        );
    }
    #[test]
    fn maintenance_is_owned_expiring_and_reports_running_jobs_without_visible_rows() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
            .join(".artifacts/at")
            .join(format!("lease-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let shared = Arc::new(Shared {
            entries: Mutex::new(BTreeMap::new()),
            publisher: Arc::new(Publisher::new(&root)),
            changed: Condvar::new(),
            registry: Mutex::new(Connection::open_in_memory().unwrap()),
            discovery: Mutex::new(
                crate::discovery::Discovery::open(&Connection::open_in_memory().unwrap()).unwrap(),
            ),
            stop: AtomicBool::new(false),
            maintenance: Mutex::new(None),
            analysis_running: AtomicBool::new(true),
            resources: Governor::new(Limits::default()).unwrap(),
            active_job: Mutex::new(None),
            overflow: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
            cursors: Arc::new(
                know_code_core::cursor::CursorService::new(&root, Vec::new()).unwrap(),
            ),
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
                "{:x}",
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
                registration: 1,
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
                pending: false,
                cancel_pending: false,
                recovery_blocked: false,
                recovery_explicit: false,
                recovering: false,
                upgrade_format: None,
            },
        )]);
        let shared = Arc::new(Shared {
            entries: Mutex::new(entries),
            publisher,
            changed: Condvar::new(),
            registry: Mutex::new(Connection::open_in_memory().unwrap()),
            discovery: Mutex::new(
                crate::discovery::Discovery::open(&Connection::open_in_memory().unwrap()).unwrap(),
            ),
            stop: AtomicBool::new(false),
            maintenance: Mutex::new(None),
            analysis_running: AtomicBool::new(false),
            resources: Governor::new(Limits::default()).unwrap(),
            active_job: Mutex::new(None),
            overflow: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
            cursors: Arc::new(
                know_code_core::cursor::CursorService::new(&root, Vec::new()).unwrap(),
            ),
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
            cursors: None,
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
    #[test]
    fn refused_upgrade_before_attempt_preserves_source_and_previous_failure() {
        for previous_failure in [false, true] {
            let mut job = cancellation_job();
            job.cancel_baseline = false;
            job.recovering = true;
            job.upgrade_format = Some(2);
            job.recovery_explicit = previous_failure;
            job.previous_error.clear();
            job.previous_error_code.clear();
            let database = PathBuf::from(&job.database);
            std::fs::create_dir_all(database.parent().unwrap()).unwrap();
            let connection = Connection::open(&database).unwrap();
            connection
                .execute_batch(include_str!("../../../tests/fixtures/schema-v2.sql"))
                .unwrap();
            connection
                .execute_batch(
                    "INSERT INTO generations VALUES(42); UPDATE state SET current_generation=42;",
                )
                .unwrap();
            drop(connection);
            recovery::adopt(&database, 0).unwrap();
            assert_eq!(selected_upgrade(&database).unwrap(), Some((2, 42)));
            if previous_failure {
                recovery::begin_upgrade(&database, false, 0, 2).unwrap();
                recovery::fail_rebuild(&database, "prior failed upgrade").unwrap();
            }
            let physical = recovery::selected_path(&database).unwrap();
            let connection = Connection::open(&physical).unwrap();
            connection
                .execute_batch("PRAGMA user_version=999;")
                .unwrap();
            drop(connection);
            let receipt = std::fs::read(database.with_extension("lifecycle.json")).unwrap();
            let source = std::fs::read(&physical).unwrap();
            let error = execute(
                job,
                Path::new("/missing-provider-fixture"),
                &mut String::new(),
                &mut |_| {},
                None,
            )
            .err()
            .expect("future format refusal");
            assert!(
                matches!(
                    error.downcast_ref::<know_code_core::Error>(),
                    Some(know_code_core::Error::UpdateRequired(_))
                ),
                "{error}"
            );
            assert_eq!(std::fs::read(&physical).unwrap(), source);
            assert_eq!(
                std::fs::read(database.with_extension("lifecycle.json")).unwrap(),
                receipt
            );
            assert_eq!(
                recovery::failure(&database).unwrap().is_some(),
                previous_failure
            );
            assert!(!recovery::is_bad(&database).unwrap());
        }
    }
}
