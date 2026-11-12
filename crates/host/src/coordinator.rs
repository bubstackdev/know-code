mod jobs;
mod wait;
use jobs::finish_job;
use wait::wait_for_workspace;

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
    project_trusted: bool,
    trust_epoch: u64,
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
    disk_pause: Option<DiskPause>,
    recovery_blocked: bool,
    recovery_explicit: bool,
    recovering: bool,
    upgrade_format: Option<i32>,
}
impl Entry {
    /// Permission is a presentation/scheduling overlay, never an analysis
    /// failure or a saved cancellation baseline.
    fn snapshot(&self) -> WorkspaceStatus {
        let mut status = self.status.snapshot();
        if let Some(settings) = status.requested_settings.as_mut() {
            settings.project_execution_allowed = self.project_trusted;
        }
        if !self.project_trusted && !self.cancel_pending && status.available {
            status.phase = "trust_required".into();
            status.outcome_code = "trust_required".into();
            status.error = format!(
                "project execution is not trusted for {}; grant personal trust with know-code trust WORKSPACE before analysis (package install scripts remain separately controlled)",
                status.workspace_root
            );
            status.progress = None;
        }
        status
    }
}
/// Durable deferred work, separate from a failed saved-input baseline. A
/// post-cleanup sample prevents our own rollback from authorizing a retry loop.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskPause {
    message: String,
    previous_outcome: String,
    force: bool,
    retry_preparation: bool,
    recovery_explicit: bool,
    used_bytes: u64,
    free_bytes: u64,
    used_reclaim: u64,
    free_reclaim: u64,
    limit_bytes: u64,
    reserve_bytes: u64,
    baseline_pending: bool,
}
impl DiskPause {
    fn improved(&self, usage: &recovery::DiskUsage) -> bool {
        let used = self.used_reclaim == 0
            || self
                .used_bytes
                .checked_sub(self.used_reclaim)
                .is_some_and(|ceiling| usage.used_bytes <= ceiling);
        let free = self.free_reclaim == 0
            || self
                .free_bytes
                .checked_add(self.free_reclaim)
                .is_some_and(|floor| usage.free_bytes >= floor);
        used && free
    }
}
#[derive(Clone)]
struct DiskOrigin {
    used_reclaim: u64,
    free_reclaim: u64,
    limit_bytes: u64,
    reserve_bytes: u64,
}
impl DiskOrigin {
    fn capture(usage: &recovery::DiskUsage, limits: &Limits) -> Self {
        let ceiling = limits.rebuild_disk_mib * 1024 * 1024;
        let floor = limits.min_free_disk_mib * 1024 * 1024;
        let mut used_reclaim = if usage.used_bytes >= ceiling {
            usage.used_bytes - ceiling + 1
        } else {
            0
        };
        let free_reclaim = if usage.free_bytes <= floor {
            floor - usage.free_bytes + 1
        } else {
            0
        };
        // If a concurrent reclamation overtook the failing sample, require
        // charged-byte progress rather than granting a free-space-jitter retry.
        if used_reclaim == 0 && free_reclaim == 0 {
            used_reclaim = 1;
        }
        Self {
            used_reclaim,
            free_reclaim,
            limit_bytes: ceiling,
            reserve_bytes: floor,
        }
    }
}
fn display_disk_pause(entry: &mut Entry) {
    if let Some(pause) = &entry.disk_pause {
        entry.status.phase = "resource_limited".into();
        entry.status.outcome_code = "disk_pressure".into();
        entry.status.error = pause.message.clone();
        entry.status.progress = Some(Progress {
            stage: "disk_pressure".into(),
            project_path: String::new(),
            message: pause.message.clone(),
        });
    }
}
fn record_disk_pause(entry: &mut Entry, pause: DiskPause) {
    // Cancellation can be accepted after execute returns, so checking only its
    // Control is insufficient. Never restore a consumed grant revoked at the
    // shared result handoff; a later explicit refresh deliberately clears this
    // block and remains authorized.
    if entry.recovery_blocked && !entry.recovery_explicit {
        entry.pending = false;
        entry.disk_pause = None;
        if entry.status.phase == "cancelling" {
            entry.status.phase = "cancelled".into();
            entry.status.outcome_code = "cancelled".into();
            entry.status.error = "index replacement cancelled; explicit refresh is required".into();
        }
        return;
    }
    entry.pending = true;
    entry.disk_pause = Some(pause);
    display_disk_pause(entry);
}
fn release_disk_pause(entry: &mut Entry) {
    if let Some(pause) = entry.disk_pause.take() {
        entry.status.outcome_code = pause.previous_outcome;
        entry.status.error = entry.analysis_error.clone();
        entry.status.progress = None;
        entry.force |= pause.force;
        entry.retry_preparation |= pause.retry_preparation;
        entry.recovery_explicit |= pause.recovery_explicit;
        if pause.recovery_explicit {
            entry.recovery_blocked = false;
        }
    }
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
    JobPanicked(String),
}
struct Job {
    registration: u64,
    trust_epoch: u64,
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
    DiskPaused(DiskPause),
    Indexed(u64, Snapshot, Vec<PathBuf>),
    CancellationBaseline(Snapshot, Vec<PathBuf>, bool),
}
#[derive(Clone, Copy, Default)]
struct DeferredAuthorization {
    force: bool,
    retry_preparation: bool,
    recovery_explicit: bool,
}
struct JobResult {
    registration: u64,
    trust_epoch: u64,
    authorization: DeferredAuthorization,
    analysis_started: bool,
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
            "resource_limited" | "disk_pressure" | "cancelled"
        ) {
            entry.status.outcome_code = if upgrade_failure(&database)? {
                "index_upgrade_failed"
            } else {
                "index_recovering"
            }
            .into();
        }
        entry.status.phase = if matches!(
            entry.status.outcome_code.as_str(),
            "resource_limited" | "disk_pressure"
        ) {
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
            "resource_limited" | "disk_pressure" | "cancelled"
        ) {
            entry.status.outcome_code = "index_recovering".into();
        }
        entry.status.phase = if matches!(
            entry.status.outcome_code.as_str(),
            "resource_limited" | "disk_pressure"
        ) {
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
    entry.disk_pause = None;
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
    if !columns.contains("disk_pause") {
        registry.execute_batch(
            "ALTER TABLE workspaces ADD COLUMN disk_pause TEXT NOT NULL DEFAULT ''",
        )?;
    }
    let discovery = crate::discovery::Discovery::open(&registry)?;
    let mut entries = BTreeMap::new();
    {
        let mut query = registry.prepare(
            "SELECT w.database,w.root,w.configuration,w.generation,'{}',w.error,COALESCE(i.identity,''),w.failed_input_hash,w.outcome_code,w.cancel_pending,w.disk_pause FROM workspaces w LEFT JOIN identities i ON i.database=w.database",
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
                r.get::<_, String>(10)?,
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
                encoded_disk_pause,
            ) = row?;
            let disk_pause: Option<DiskPause> = if encoded_disk_pause.is_empty() {
                None
            } else {
                Some(serde_json::from_str(&encoded_disk_pause)?)
            };
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
                    project_trusted: settings::project_execution_allowed(Path::new(&root), data)
                        .unwrap_or(false),
                    trust_epoch: 0,
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
                    disk_pause,
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
        if let Some(pause) = entry.disk_pause.as_ref() {
            if !entry.cancel_pending && (!entry.recovery_blocked || pause.recovery_explicit) {
                entry.pending = true;
                entry.recovery_explicit |= pause.recovery_explicit;
                if pause.recovery_explicit {
                    entry.recovery_blocked = false;
                }
                display_disk_pause(entry);
            } else {
                entry.disk_pause = None;
            }
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
    let mut last_disk = Instant::now() - Duration::from_secs(10);
    let mut disk_after = String::new();
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
        if last_disk.elapsed() >= Duration::from_secs(2) {
            maintain_disk(&shared, data, &mut disk_after)?;
            last_disk = Instant::now();
        }
        if last_presence.elapsed() >= Duration::from_secs(2) {
            let mut entries = shared.entries.lock().unwrap();
            for entry in entries.values_mut() {
                observe_selected_health(&shared, entry);
                let root = PathBuf::from(&entry.status.workspace_root);
                let trusted = settings::project_execution_allowed(&root, data).unwrap_or(false);
                if entry.project_trusted && !trusted {
                    revoke_entry_execution(shared.as_ref(), entry);
                } else {
                    entry.project_trusted = trusted;
                }
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
                                && (e.project_trusted || e.cancel_pending)
                                && e.disk_pause.is_none()
                                && e.queued.elapsed() >= Duration::from_millis(100)
                        })
                        .min_by_key(|(_, e)| priority(e, Instant::now()))
                        .map(|(db, _)| db.clone())
                };
                next.map(|database| -> AnyResult<Option<Job>> {
                    let entry = entries.get_mut(&database).unwrap();
                    entry.project_trusted = settings::project_execution_allowed(
                        Path::new(&entry.status.workspace_root),
                        data,
                    )
                    .unwrap_or(false);
                    if !entry.project_trusted && !entry.cancel_pending {
                        return Ok(None);
                    }
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
                        trust_epoch: entry.trust_epoch,
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
                    finish_job(&sender, database.clone(), || {
                        let epoch = job.epoch;
                        let registration = job.registration;
                        let trust_epoch = job.trust_epoch;
                        let authorization = DeferredAuthorization {
                            force: job.force,
                            retry_preparation: job.retry_preparation,
                            recovery_explicit: job.recovery_explicit,
                        };
                        let cancel_baseline = job.cancel_baseline;
                        let recovering = job.recovering;
                        let upgrading = job.upgrade_format.is_some();
                        let mut input_hash = String::new();
                        let mut analysis_started = false;
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
                            &mut analysis_started,
                        )
                        .map_err(|e| Failure {
                            code: e
                                .downcast_ref::<know_code_core::Error>()
                                .map(|e| e.code())
                                .unwrap_or("analysis_error")
                                .into(),
                            index_fault: e.downcast_ref::<know_code_core::Error>().and_then(
                                |error| match error {
                                    know_code_core::Error::IndexCorrupt {
                                        database,
                                        incarnation,
                                        ..
                                    } => Some((database.clone(), incarnation.clone())),
                                    _ => None,
                                },
                            ),
                            message: e.to_string(),
                        });
                        JobResult {
                            registration,
                            trust_epoch,
                            authorization,
                            analysis_started,
                            database,
                            epoch,
                            result,
                            input_hash,
                            cancel_baseline,
                            recovering,
                        }
                    });
                });
            }
        }
        if shared.stop.load(Ordering::Acquire) && !running {
            break;
        }
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(Event::JobPanicked(database)) => {
                shared.publisher.disable();
                shared.stop.store(true, Ordering::Release);
                shared.analysis_running.store(false, Ordering::Release);
                shared.changed.notify_all();
                return Err(format!("internal analysis panic for {database}; coordinator stopped safely; restart and refresh the workspace").into());
            }
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
                    } else if done.result.as_ref().is_err_and(|error| {
                        error.code == "trust_required"
                            || (done.trust_epoch != entry.trust_epoch && error.code == "cancelled")
                    }) {
                        if done
                            .result
                            .as_ref()
                            .is_err_and(|error| error.code == "trust_required")
                            && done.trust_epoch == entry.trust_epoch
                        {
                            entry.project_trusted = false;
                        }
                        if let Err(error) = defer_permission_interruption(entry, &done) {
                            block_lifecycle_error(entry, error.as_ref());
                        }
                        None
                    } else {
                        match done.result {
                            Ok(JobOutput::DiskPaused(pause)) => {
                                record_disk_pause(entry, pause);
                                None
                            }
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
                                entry.disk_pause = None;
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
                                    } else if matches!(
                                        error.code.as_str(),
                                        "resource_limited" | "disk_pressure"
                                    ) {
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
                                        "resource_limited" | "disk_pressure" => "resource_limited",
                                        _ => "failed",
                                    }
                                }
                                .into();
                                if recovery_failed {
                                    entry.status.phase = if entry.recovery_explicit {
                                        "queued"
                                    } else if matches!(
                                        error.code.as_str(),
                                        "resource_limited" | "disk_pressure"
                                    ) {
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
                };
                {
                    let entries = shared.entries.lock().unwrap();
                    for entry in entries.values() {
                        shared.publisher.publish(&entry.snapshot());
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

fn guard_temporary_disk(
    job: &Job,
    data: &Path,
    origin: &Arc<Mutex<Option<DiskOrigin>>>,
    disk: &mut Option<Arc<recovery::DiskBudget>>,
) -> AnyResult<()> {
    let guard = Arc::new(recovery::DiskBudget::new(Path::new(&job.database), data)?);
    *disk = Some(Arc::clone(&guard));
    if let Err(error) = guard.check(&job.session.limits(), false) {
        if matches!(error, know_code_core::Error::DiskPressure(_)) {
            let usage = guard.usage(false)?;
            *origin.lock().unwrap() = Some(DiskOrigin::capture(&usage, &job.session.limits()));
        }
        return Err(error.into());
    }
    let stopped = Arc::clone(origin);
    job.session.control().guard_disk(move |limits| {
        let result = guard.check_sampled(limits);
        if matches!(result, Err(know_code_core::Error::DiskPressure(_))) {
            let usage = guard.usage(false)?;
            stopped
                .lock()
                .unwrap()
                .get_or_insert_with(|| DiskOrigin::capture(&usage, limits));
        }
        result
    });
    Ok(())
}

fn execute(
    job: Job,
    installation: &Path,
    attempt_hash: &mut String,
    progress: &mut impl FnMut(Progress),
    cursors: Option<&know_code_core::cursor::CursorService>,
    analysis_started: &mut bool,
) -> AnyResult<JobOutput> {
    let control = job.session.control();
    let mut candidate: Option<recovery::Candidate> = None;
    let mut disk: Option<Arc<recovery::DiskBudget>> = None;
    *analysis_started = false;
    let origin = Arc::new(Mutex::new(None::<DiskOrigin>));
    let result = (|| -> AnyResult<JobOutput> {
        control.check()?;
        if workspace::identity(&job.root)? != job.identity {
            return Err("workspace identity changed before saved-input reconciliation".into());
        }
        let database = Path::new(&job.database);
        let data = control::root_for_database(database)
            .ok_or("workspace database has no central data root")?;
        let settings_paths = settings::paths(&job.root, data);
        if !job.cancel_baseline {
            settings::project_execution_grant(&job.root, data)?;
            control.check()?;
        }
        if job.recovering && !job.cancel_baseline {
            guard_temporary_disk(&job, data, &origin, &mut disk)?;
        }
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
        if !job.recovering {
            // No-op reconciliation and unchanged failed/cancelled inputs above
            // are not write work and must not acquire new retry authorization.
            guard_temporary_disk(&job, data, &origin, &mut disk)?;
        }
        let check_disk = || -> know_code_core::Result<()> {
            let pool = disk.as_ref().expect("analysis disk guard");
            let limits = job.session.limits();
            if let Err(error) = pool.check(&limits, false) {
                if matches!(error, know_code_core::Error::DiskPressure(_)) {
                    let usage = pool.usage(false)?;
                    origin
                        .lock()
                        .unwrap()
                        .get_or_insert_with(|| DiskOrigin::capture(&usage, &limits));
                }
                return Err(error);
            }
            Ok(())
        };
        *analysis_started = true;
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
                before_publish: Some(&check_disk),
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
            // Full pool preview is outside cursor admission/publication locks.
            // The sampled active guard remains live while admission waits.
            disk.as_ref()
                .expect("candidate disk guard")
                .check(&job.session.limits(), true)?;
            let publication = control.publication();
            let cutover = || {
                recovery::finish_prepared_rebuild(&prepared, &publication, || {
                    publication.check()?;
                    if workspace::identity(&job.root)? != job.identity {
                        return Err(know_code_core::Error::Provider(
                            "workspace identity changed before replacement cutover".into(),
                        ));
                    }
                    Ok(())
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
    // Writer/staging owners have unwound before this sample. A deferred job
    // never overwrites analyzer/cancellation fingerprints or allocates another
    // recovery attempt. Allocated replacements continue through fail_rebuild.
    if candidate.is_none()
        && let Err(error) = &result
        && let Some(know_code_core::Error::DiskPressure(message)) =
            error.downcast_ref::<know_code_core::Error>()
    {
        // Closing/rolling back a writer deliberately leaves its WAL. Settle
        // freely reclaimable own staging first, before retaining the baseline.
        // A real held snapshot may keep the checkpoint busy until its owner
        // releases it; that is genuine externally reclaimed headroom.
        let baseline_pending = if !*analysis_started {
            false
        } else {
            match know_code_core::store::maintain_index(Path::new(&job.database)) {
                Ok(_) | Err(know_code_core::Error::NotReady) => false,
                Err(know_code_core::Error::Busy(_)) => true,
                Err(error) => return Err(error.into()),
            }
        };
        let usage = disk
            .as_ref()
            .expect("pressure has a pool guard")
            .usage(false)?;
        let cause = origin
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| DiskOrigin::capture(&usage, &job.session.limits()));
        return Ok(JobOutput::DiskPaused(DiskPause {
            baseline_pending,
            used_reclaim: if *analysis_started {
                cause.used_reclaim
            } else {
                0
            },
            free_reclaim: if *analysis_started {
                cause.free_reclaim
            } else {
                0
            },
            limit_bytes: cause.limit_bytes,
            reserve_bytes: cause.reserve_bytes,
            message: message.clone(),
            previous_outcome: job.previous_error_code.clone(),
            force: job.force,
            retry_preparation: !*analysis_started && job.retry_preparation,
            recovery_explicit: job.recovery_explicit,
            used_bytes: usage.used_bytes,
            free_bytes: usage.free_bytes,
        }));
    }
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
    release_disk_pause(entry);
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
    if entry.disk_pause.is_some() {
        display_disk_pause(entry);
        return;
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
struct PersistedEntry {
    status: WorkspaceStatus,
    configuration: String,
    identity: String,
    analysis_error: String,
    failed_input_hash: String,
    cancel_pending: bool,
    disk_pause: String,
}
impl PersistedEntry {
    fn capture(entry: &Entry) -> AnyResult<Self> {
        Ok(Self {
            status: entry.status.snapshot(),
            configuration: entry.configuration.clone(),
            identity: entry.identity.clone(),
            analysis_error: entry.analysis_error.clone(),
            failed_input_hash: entry.failed_input_hash.clone(),
            cancel_pending: entry.cancel_pending,
            disk_pause: entry
                .disk_pause
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?
                .unwrap_or_default(),
        })
    }
}
fn persist(shared: &Shared, entry: &Entry, inputs: Option<&Snapshot>) -> AnyResult<()> {
    persist_record(
        &mut shared.registry.lock().unwrap(),
        &PersistedEntry::capture(entry)?,
        inputs,
    )
}
fn persist_record(
    registry: &mut Connection,
    entry: &PersistedEntry,
    inputs: Option<&Snapshot>,
) -> AnyResult<()> {
    let status = &entry.status;
    let configuration = &entry.configuration;
    let identity = &entry.identity;
    let analysis_error = &entry.analysis_error;
    let failed_input_hash = &entry.failed_input_hash;
    let cancel_pending = entry.cancel_pending;
    let tx = registry.transaction()?;
    let encoded = inputs.map(serde_json::to_string).transpose()?;
    tx.execute("INSERT INTO workspaces(database,root,configuration,generation,inputs,error,failed_input_hash,outcome_code,cancel_pending,disk_pause) VALUES(?,?,?,?,'{}',?,?,?,?,?) ON CONFLICT(database) DO UPDATE SET root=excluded.root,configuration=excluded.configuration,generation=excluded.generation,error=excluded.error,failed_input_hash=excluded.failed_input_hash,outcome_code=excluded.outcome_code,cancel_pending=excluded.cancel_pending,disk_pause=excluded.disk_pause",params![status.database,status.workspace_root,configuration,status.generation,analysis_error,failed_input_hash,status.outcome_code,cancel_pending,entry.disk_pause])?;
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
/// One bounded round-robin maintenance batch. File census and SQLite never
/// run under entries/cursor/settings locks. Registry ordering is retained by a
/// nonblocking registry handoff before dropping entries for the durable write.
fn maintain_disk(shared: &Shared, data: &Path, after: &mut String) -> AnyResult<()> {
    // This event loop alone starts/finishes jobs, so the reservation remains
    // stable through this synchronous pass, including pre-writer planning.
    // Release its mutex before taking entries or doing filesystem/SQLite work.
    let active_database = shared
        .active_job
        .lock()
        .unwrap()
        .as_ref()
        .map(|(database, _)| database.clone());
    let requests = {
        let entries = shared.entries.lock().unwrap();
        entries
            .range((
                std::ops::Bound::Excluded(after.clone()),
                std::ops::Bound::Unbounded,
            ))
            .chain(entries.range(..=after.clone()))
            .filter(|(database, entry)| {
                Some(database.as_str()) != active_database.as_deref()
                    && entry.status.available
                    && !entry.recovering
            })
            .take(16)
            .map(|(db, entry)| {
                (
                    db.clone(),
                    entry.registration,
                    entry.status.generation,
                    entry.disk_pause.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    if let Some((last, ..)) = requests.last() {
        *after = last.clone();
    }
    let limits = shared.resources.limits();
    let mut sample = None;
    for (database, registration, generation, pause) in requests {
        let path = Path::new(&database);
        let cleanup = recovery::cleanup(path, generation).and_then(|_| recovery::has_retired(path));
        let checkpoint = know_code_core::store::maintain_index(path);
        let checkpoint_finished = checkpoint.is_ok();
        let retired = cleanup.as_ref().is_ok_and(|retained| *retained);
        let failure = match cleanup {
            Err(error) => Some(error),
            Ok(_) => match checkpoint {
                Err(
                    know_code_core::Error::Busy(_)
                    | know_code_core::Error::NotReady
                    | know_code_core::Error::UpdateRequired(_)
                    | know_code_core::Error::IndexCorrupt { .. },
                ) => None,
                Err(error) => Some(error),
                Ok(_) => None,
            },
        };
        let rebaseline = pause.as_ref().is_some_and(|p| p.baseline_pending)
            && checkpoint_finished
            && failure.is_none();
        let decision = if let Some(pause) = &pause {
            if sample.is_none() {
                sample =
                    Some(recovery::DiskBudget::new(path, data).and_then(|pool| pool.usage(false)));
            }
            match sample.as_ref().unwrap() {
                Ok(usage) => Ok(!pause.baseline_pending
                    && (pause.improved(usage)
                        || pause.limit_bytes != limits.rebuild_disk_mib * 1024 * 1024
                        || pause.reserve_bytes != limits.min_free_disk_mib * 1024 * 1024)
                    && usage.used_bytes < limits.rebuild_disk_mib * 1024 * 1024
                    && usage.free_bytes > limits.min_free_disk_mib * 1024 * 1024),
                Err(error) => Err(error.to_string()),
            }
        } else {
            Ok(false)
        };
        if pause.is_none() {
            let mut entries = shared.entries.lock().unwrap();
            if let Some(entry) = entries
                .get_mut(&database)
                .filter(|e| e.registration == registration && e.disk_pause.is_none())
            {
                if retired || failure.is_some() {
                    entry.status.progress = Some(Progress {
                        stage: "cleanup_pending".into(),
                        project_path: String::new(),
                        message: failure.as_ref().map_or_else(
                            || "retired index files await readers and remain charged".into(),
                            |error| format!("temporary files remain charged: {error}"),
                        ),
                    });
                } else if entry
                    .status
                    .progress
                    .as_ref()
                    .is_some_and(|p| p.stage == "cleanup_pending")
                {
                    entry.status.progress = None;
                }
            }
            continue;
        }
        if !rebaseline
            && failure.is_none()
            && !decision.as_ref().is_ok_and(|resume| *resume)
            && decision.is_ok()
        {
            continue;
        }
        let mut entries = shared.entries.lock().unwrap();
        let Some(entry) = entries
            .get_mut(&database)
            .filter(|e| e.registration == registration && e.disk_pause == pause)
        else {
            continue;
        };
        let Ok(mut registry) = shared.registry.try_lock() else {
            continue;
        };
        if let Some(error) = failure {
            // Accounting/access failures are not evidence of available space.
            // Preserve the private pause and show the typed failure, no launch.
            entry.status.error = error.to_string();
            entry.status.outcome_code = error.code().into();
        } else if let Err(message) = decision {
            entry.status.error = format!("temporary disk accounting failed: {message}");
            entry.status.outcome_code = "io_error".into();
        } else if rebaseline {
            let usage = sample.as_ref().unwrap().as_ref().unwrap();
            let pause = entry.disk_pause.as_mut().unwrap();
            pause.used_bytes = usage.used_bytes;
            pause.free_bytes = usage.free_bytes;
            pause.baseline_pending = false;
            display_disk_pause(entry);
        } else if entry.disk_pause.is_some()
            && !entry.cancel_pending
            && (!entry.recovery_blocked
                || entry
                    .disk_pause
                    .as_ref()
                    .is_some_and(|p| p.recovery_explicit))
        {
            release_disk_pause(entry);
            entry.pending = true;
            entry.status.phase = "queued".into();
        } else {
            continue;
        }
        let saved = PersistedEntry::capture(entry)?;
        drop(entries);
        persist_record(&mut registry, &saved, None)?;
        shared.changed.notify_all();
    }
    Ok(())
}
// Revocation is not a user cancellation. Preserve its saved-input baseline,
// retry flags and failure history; only the active control is interrupted.
fn revoke_entry_execution(shared: &Shared, entry: &mut Entry) {
    entry.project_trusted = false;
    entry.trust_epoch = entry.trust_epoch.wrapping_add(1);
    entry.status.publisher.invalidate(&entry.status.database);
    if let Some((_, control)) = shared
        .active_job
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(database, _)| database == &entry.status.database)
    {
        control.cancel();
        entry.pending = true;
    }
}

fn defer_permission_interruption(entry: &mut Entry, done: &JobResult) -> AnyResult<()> {
    // A user cancellation may be accepted after permission stopped the worker
    // but before its result is handed off. Regranting permission cannot revive
    // that revoked work authorization; a later explicit refresh can.
    if entry.recovery_blocked && !entry.recovery_explicit {
        entry.pending = false;
        if entry.status.phase == "cancelling" {
            entry.status.phase = "cancelled".into();
            entry.status.outcome_code = "cancelled".into();
            entry.status.error = "index replacement cancelled; explicit refresh is required".into();
        }
        return Ok(());
    }
    let allocated_failure =
        done.recovering && recovery::failure(Path::new(&done.database))?.is_some();
    if allocated_failure {
        // An allocated replacement still consumes its attempt even when trust
        // was revoked. Only a later explicit refresh can retry that failure.
        entry.recovery_blocked = !entry.recovery_explicit;
        entry.pending = entry.recovery_explicit;
        entry.force |= entry.recovery_explicit;
        entry.status.phase = if entry.pending {
            "queued"
        } else {
            "recovery_failed"
        }
        .into();
    } else {
        // No publication/failed-input baseline was produced. Return only the
        // exact authorizations consumed by this interrupted job.
        entry.pending = true;
        entry.force |= done.authorization.force;
        entry.retry_preparation |= !done.analysis_started && done.authorization.retry_preparation;
        if !entry.recovery_blocked || entry.recovery_explicit {
            entry.recovery_explicit |= done.authorization.recovery_explicit;
        }
        entry.status.phase = "queued".into();
    }
    entry.status.error = entry.analysis_error.clone();
    entry.status.progress = None;
    Ok(())
}

fn set_workspace_execution(
    shared: &Shared,
    data: &Path,
    request: &ControlRequest,
) -> AnyResult<WorkspaceStatus> {
    let allowed: bool =
        serde_json::from_str(request.settings_json.as_deref().ok_or_else(|| {
            know_code_core::Error::InvalidRequest("trust requires an explicit boolean grant".into())
        })?)
        .map_err(know_code_core::Error::from)?;
    let mut entries = shared.entries.lock().unwrap();
    let root = if !request.database.is_empty() {
        PathBuf::from(
            &entries
                .get(&request.database)
                .ok_or("workspace is not registered")?
                .status
                .workspace_root,
        )
    } else {
        let root = PathBuf::from(&request.workspace_root);
        if root.as_os_str().is_empty() {
            return Err(know_code_core::Error::InvalidRequest(
                "trust requires a workspace root or registered database".into(),
            )
            .into());
        }
        if allowed {
            std::fs::canonicalize(&root)?
        } else {
            settings::project_execution_root(&root)?
        }
    };
    let database = workspace::database_path(&root, data)
        .to_string_lossy()
        .into_owned();
    if let Some(entry) = entries.get_mut(&database) {
        if allowed {
            if workspace::identity(&root)? != entry.identity {
                return Err("workspace identity changed; cannot grant execution to the previous registration".into());
            }
            settings::set_project_execution(&root, data, true)?;
            entry.project_trusted = true;
            entry.status.publisher.invalidate(&entry.status.database);
        } else {
            settings::revoke_project_execution(&root, data)?;
            revoke_entry_execution(shared, entry);
        }
        entry.status.requested_settings = settings::resolve(&root, data, &entry.configuration).ok();
        persist(shared, entry, None)?;
        return Ok(entry.snapshot());
    }
    // Granting permission is intentionally independent of enrollment: parent
    // discovery can later register this identity through its normal path.
    if allowed {
        settings::set_project_execution(&root, data, true)?;
    } else {
        settings::revoke_project_execution(&root, data)?;
    }
    Ok(WorkspaceStatus {
        workspace_root: root.to_string_lossy().into_owned(),
        database,
        phase: "unregistered".into(),
        available: root.is_dir(),
        requested_settings: settings::resolve(&root, data, "").ok(),
        ..Default::default()
    })
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
                return Ok(entry.snapshot());
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
            project_trusted: settings::project_execution_allowed(&root, data).unwrap_or(false),
            trust_epoch: 0,
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
            disk_pause: None,
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
            release_disk_pause(entry);
            queue(entry, None, true);
        }
        entry.project_trusted = settings::project_execution_allowed(&root, data).unwrap_or(false);
        entry.status.requested_settings = settings::resolve(&root, data, &entry.configuration).ok();
        if request.operation == "refresh" {
            release_disk_pause(entry);
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
        entry.snapshot()
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
        if request.operation == "trust" {
            let status = set_workspace_execution(shared, data, &request)?;
            wake(sender, shared, Event::Wake);
            shared.changed.notify_all();
            return Ok(ControlResponse {
                workspaces: vec![status],
                ..Default::default()
            });
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
            let status = entry.snapshot();
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
                    .map(|e| e.snapshot())
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
                        if entry.disk_pause.is_some()
                            && !entry.cancel_pending
                            && !entry.recovery_blocked
                        {
                            release_disk_pause(entry);
                            entry.pending = true;
                            entry.status.phase = "queued".into();
                        }
                        if !entry.recovery_blocked
                            && (matches!(
                                entry.status.outcome_code.as_str(),
                                "resource_limited" | "disk_pressure"
                            ) || active.as_ref().is_some_and(|(running, control)| {
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
                workspaces: vec![entry.snapshot()],
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
            let status = entry.snapshot();
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
            settings::revoke_project_execution(Path::new(&request.workspace_root), data)?;
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
            shared.changed.notify_all();
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
        if request.operation == "wait" {
            return Ok(ControlResponse {
                workspaces: vec![wait_for_workspace(shared, &request)?],
                ..Default::default()
            });
        }
        let entries = shared.entries.lock().unwrap();
        let workspaces = entries
            .values()
            .filter(|e| request.database.is_empty() || e.status.database == request.database)
            .map(|e| e.snapshot())
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
    fn permission_entry(root: &Path, database: &str) -> Entry {
        let publisher = Arc::new(Publisher::new(root));
        Entry {
            registration: 1,
            project_trusted: false,
            trust_epoch: 0,
            analysis_error: "prior dependency setup failed".into(),
            failed_input_hash: "saved-failure".into(),
            status: LiveStatus::new(
                WorkspaceStatus {
                    workspace_root: root.to_string_lossy().into(),
                    database: database.into(),
                    phase: "failed".into(),
                    available: true,
                    outcome_code: "analysis_error".into(),
                    error: "prior dependency setup failed".into(),
                    ..Default::default()
                },
                &publisher,
            ),
            configuration: String::new(),
            identity: String::new(),
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
            disk_pause: None,
            recovery_blocked: false,
            recovery_explicit: false,
            recovering: false,
            upgrade_format: None,
        }
    }
    #[test]
    fn disk_maintenance_respects_reserved_analysis_before_writer_open() {
        let data = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/maintenance-reservation")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&data).unwrap();
        let managed = |name: &str| {
            let path = data.join("workspaces").join(name).join("index.db");
            let mut writer = know_code_core::store::Writer::open(&path).unwrap();
            writer
                .stage(Payload::Compilation(Compilation {
                    id: "fixture".into(),
                    language: "typescript".into(),
                    ..Default::default()
                }))
                .unwrap();
            writer
                .stage(Payload::Report(AnalysisReport {
                    compilation_id: "fixture".into(),
                    status: "complete".into(),
                    ..Default::default()
                }))
                .unwrap();
            let generation = writer.publish().unwrap();
            recovery::adopt(&path, generation).unwrap();
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("PRAGMA wal_autocheckpoint=0; CREATE TABLE maintenance_probe(value); INSERT INTO maintenance_probe VALUES(zeroblob(262144));").unwrap();
            (path.to_string_lossy().into_owned(), connection)
        };
        let (active, _active_connection) = managed(&"a".repeat(24));
        let (sibling, _sibling_connection) = managed(&"b".repeat(24));
        let wal_bytes =
            |database: &str| std::fs::metadata(format!("{database}-wal")).unwrap().len();
        let before = wal_bytes(&active);
        assert!(before > 0 && wal_bytes(&sibling) > 0);
        let publisher = Arc::new(Publisher::new(&data));
        let mut entries = BTreeMap::new();
        for database in [&active, &sibling] {
            let mut entry = permission_entry(&data, database);
            entry.project_trusted = true;
            entry.status.phase = "queued".into();
            entry.status.generation = 1;
            entries.insert(database.clone(), entry);
        }
        let shared = Shared {
            entries: Mutex::new(entries),
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
            active_job: Mutex::new(Some((active.clone(), Control::default()))),
            overflow: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
            cursors: Arc::new(
                know_code_core::cursor::CursorService::new(&data, Vec::new()).unwrap(),
            ),
        };
        let mut after = String::new();
        maintain_disk(&shared, &data, &mut after).unwrap();
        assert_eq!(
            wal_bytes(&sibling),
            0,
            "idle sibling maintenance was skipped"
        );
        assert_eq!(
            wal_bytes(&active),
            before,
            "maintenance entered the reserved pre-writer analysis window"
        );
        assert_eq!(
            shared.entries.lock().unwrap()[&active].status.phase,
            "queued"
        );
        drop(know_code_core::store::Writer::open(Path::new(&active)).unwrap());
        shared.active_job.lock().unwrap().take();
        shared.analysis_running.store(false, Ordering::Release);
        maintain_disk(&shared, &data, &mut after).unwrap();
        assert_eq!(wal_bytes(&active), 0, "idle maintenance did not resume");
    }
    #[test]
    fn permission_overlay_preserves_failed_input_and_durable_outcome() {
        let job = cancellation_job();
        let mut entry = permission_entry(&job.root, &job.database);
        let queued = entry.queued;
        assert_eq!(entry.snapshot().phase, "trust_required");
        let saved = PersistedEntry::capture(&entry).unwrap();
        assert_eq!(saved.status.outcome_code, "analysis_error");
        assert_eq!(saved.analysis_error, "prior dependency setup failed");
        assert_eq!(saved.failed_input_hash, "saved-failure");
        entry.project_trusted = true;
        assert_eq!(entry.snapshot().phase, "failed");
        assert!(!entry.pending && !entry.force && !entry.retry_preparation);
        assert_eq!(entry.queued, queued);
        entry.project_trusted = false;
        begin_cancellation(&mut entry);
        assert_eq!(entry.snapshot().phase, "cancelling");
        assert!(entry.cancel_pending && entry.pending);
    }
    #[test]
    fn revoked_job_after_regrant_defers_only_its_original_authorization() {
        let job = cancellation_job();
        let mut entry = permission_entry(&job.root, &job.database);
        entry.trust_epoch = 1;
        entry.project_trusted = true;
        let done = JobResult {
            registration: 1,
            trust_epoch: 0,
            analysis_started: false,
            authorization: DeferredAuthorization {
                force: true,
                retry_preparation: false,
                recovery_explicit: true,
            },
            input_hash: "incomplete-work".into(),
            database: job.database,
            epoch: 1,
            result: Err(Failure {
                code: "cancelled".into(),
                message: "permission revoked".into(),
                index_fault: None,
            }),
            cancel_baseline: false,
            recovering: false,
        };
        defer_permission_interruption(&mut entry, &done).unwrap();
        assert!(entry.pending && entry.force && entry.recovery_explicit);
        assert!(!entry.retry_preparation && !entry.cancel_pending);
        assert_eq!(entry.failed_input_hash, "saved-failure");
        assert_eq!(entry.analysis_error, "prior dependency setup failed");
        assert_eq!(entry.status.outcome_code, "analysis_error");
    }
    #[test]
    fn permission_handoff_preserves_only_unspent_restore_retry_and_newer_refresh() {
        let job = cancellation_job();
        for (analysis_started, newer_retry, expected) in [
            (false, false, true),
            (true, false, false),
            (true, true, true),
        ] {
            let mut entry = permission_entry(&job.root, &job.database);
            entry.project_trusted = true;
            entry.trust_epoch = 1;
            entry.retry_preparation = newer_retry;
            let done = JobResult {
                registration: 1,
                trust_epoch: 0,
                analysis_started,
                authorization: DeferredAuthorization {
                    force: true,
                    retry_preparation: true,
                    recovery_explicit: false,
                },
                input_hash: "interrupted-input".into(),
                database: job.database.clone(),
                epoch: 1,
                result: Err(Failure {
                    code: "cancelled".into(),
                    message: "execution trust revoked".into(),
                    index_fault: None,
                }),
                cancel_baseline: false,
                recovering: false,
            };
            defer_permission_interruption(&mut entry, &done).unwrap();
            assert_eq!(
                entry.retry_preparation, expected,
                "started={analysis_started}, newer={newer_retry}"
            );
            assert!(entry.pending && entry.force && !entry.cancel_pending);
            assert_eq!(entry.failed_input_hash, "saved-failure");
            assert_eq!(entry.analysis_error, "prior dependency setup failed");
        }
    }
    #[test]
    fn permission_handoff_cannot_revive_cancelled_unallocated_recovery() {
        let job = cancellation_job();
        for later_refresh in [false, true] {
            let mut entry = permission_entry(&job.root, &job.database);
            entry.recovering = true;
            entry.pending = true;
            entry.trust_epoch = 1;
            begin_cancellation(&mut entry);
            entry.project_trusted = true; // permission regranted before Finished
            entry.recovering = false;
            if later_refresh {
                entry.recovery_explicit = true;
                entry.recovery_blocked = false;
                entry.retry_preparation = true;
                queue(&mut entry, None, true);
            }
            let done = JobResult {
                registration: 1,
                trust_epoch: 0,
                analysis_started: false,
                authorization: DeferredAuthorization {
                    force: true,
                    retry_preparation: true,
                    recovery_explicit: true,
                },
                input_hash: String::new(),
                database: job.database.clone(),
                epoch: 1,
                result: Err(Failure {
                    code: "trust_required".into(),
                    message: "permission revoked before allocation".into(),
                    index_fault: None,
                }),
                cancel_baseline: false,
                recovering: true,
            };
            defer_permission_interruption(&mut entry, &done).unwrap();
            assert_eq!(entry.pending, later_refresh);
            assert_eq!(entry.force, later_refresh);
            assert_eq!(entry.recovery_explicit, later_refresh);
            assert_eq!(entry.retry_preparation, later_refresh);
            if !later_refresh {
                assert!(entry.recovery_blocked);
                assert_eq!(entry.snapshot().phase, "cancelled");
            }
        }
    }
    #[test]
    fn revoked_allocated_replacement_requires_explicit_retry_after_regrant() {
        let job = recovery_job();
        let candidate =
            recovery::begin_rebuild(Path::new(&job.database), false, job.previous_generation)
                .unwrap();
        recovery::fail_rebuild(Path::new(&job.database), "permission revoked").unwrap();
        let mut entry = permission_entry(&job.root, &job.database);
        entry.project_trusted = true;
        entry.trust_epoch = 1;
        let done = JobResult {
            registration: 1,
            trust_epoch: 0,
            analysis_started: false,
            authorization: DeferredAuthorization {
                force: true,
                retry_preparation: true,
                recovery_explicit: true,
            },
            input_hash: String::new(),
            database: job.database.clone(),
            epoch: 1,
            result: Err(Failure {
                code: "cancelled".into(),
                message: "permission revoked".into(),
                index_fault: None,
            }),
            cancel_baseline: false,
            recovering: true,
        };
        defer_permission_interruption(&mut entry, &done).unwrap();
        assert!(entry.recovery_blocked && !entry.pending && !entry.recovery_explicit);
        assert!(!entry.force && !entry.retry_preparation);
        assert_eq!(entry.failed_input_hash, "saved-failure");
        assert!(
            recovery::failure(Path::new(&job.database))
                .unwrap()
                .is_some()
        );
        drop(candidate);
    }
    #[test]
    fn untrusted_rebuild_is_fenced_before_attempt_allocation() {
        let job = recovery_job();
        let database = job.database.clone();
        let data = control::root_for_database(Path::new(&database)).unwrap();
        settings::revoke_project_execution(&job.root, data).unwrap();
        let mut analysis_started = true;
        let error = execute(
            job,
            Path::new("/no/compiler/installation"),
            &mut String::new(),
            &mut |_| {},
            None,
            &mut analysis_started,
        )
        .err()
        .unwrap();
        assert_eq!(
            error
                .downcast_ref::<know_code_core::Error>()
                .unwrap()
                .code(),
            "trust_required"
        );
        assert!(!analysis_started);
        assert!(recovery::failure(Path::new(&database)).unwrap().is_none());
        assert!(
            recovery::replacement_reason(Path::new(&database))
                .unwrap()
                .is_none()
        );
    }
    fn pause_fixture() -> DiskPause {
        DiskPause {
            message: "owned disk pressure".into(),
            previous_outcome: "provider_error".into(),
            force: false,
            retry_preparation: false,
            recovery_explicit: false,
            used_bytes: 100,
            free_bytes: 1000,
            used_reclaim: 20,
            free_reclaim: 0,
            limit_bytes: 200,
            reserve_bytes: 500,
            baseline_pending: false,
        }
    }
    #[test]
    fn disk_pause_does_not_retry_on_free_jitter_own_cleanup_or_insufficient_reclamation() {
        let pause = pause_fixture();
        assert!(!pause.improved(&recovery::DiskUsage {
            used_bytes: 100,
            free_bytes: 1000000
        }));
        assert!(!pause.improved(&recovery::DiskUsage {
            used_bytes: 99,
            free_bytes: 1000000
        }));
        assert!(pause.improved(&recovery::DiskUsage {
            used_bytes: 80,
            free_bytes: 1000
        }));
        let intrinsic = DiskPause {
            used_bytes: 0,
            ..pause.clone()
        };
        assert!(!intrinsic.improved(&recovery::DiskUsage {
            used_bytes: 0,
            free_bytes: 1000000
        }));
        // Preflight allocated no writer/staging; an external reader can release
        // before the final sample is captured at zero. Healthy space suffices,
        // while the active intrinsic-footprint pause above remains fenced.
        let preflight = DiskPause {
            used_reclaim: 0,
            free_reclaim: 0,
            ..intrinsic
        };
        assert!(preflight.improved(&recovery::DiskUsage {
            used_bytes: 0,
            free_bytes: 1000000
        }));
        let reserve = DiskPause {
            used_reclaim: 0,
            free_reclaim: 100,
            ..pause
        };
        assert!(!reserve.improved(&recovery::DiskUsage {
            used_bytes: 0,
            free_bytes: 1099
        }));
        assert!(reserve.improved(&recovery::DiskUsage {
            used_bytes: 0,
            free_bytes: 1100
        }));
        let encoded = serde_json::to_vec(&reserve).unwrap();
        let restored: DiskPause = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(reserve, restored);
    }
    #[test]
    fn paused_work_preserves_failed_baseline_queue_age_and_cancel_revokes_grants() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/disk-pause-state")
            .join(format!("{}-{:?}", std::process::id(), Instant::now()));
        std::fs::create_dir_all(&root).unwrap();
        let publisher = Arc::new(Publisher::new(&root));
        let mut entry = Entry {
            registration: 1,
            project_trusted: true,
            trust_epoch: 0,
            analysis_error: "provider failed".into(),
            failed_input_hash: "failed-saved-input".into(),
            status: LiveStatus::new(
                WorkspaceStatus {
                    available: true,
                    phase: "queued".into(),
                    outcome_code: "provider_error".into(),
                    ..Default::default()
                },
                &publisher,
            ),
            configuration: String::new(),
            identity: String::new(),
            special_inputs: BTreeSet::new(),
            dirty: BTreeSet::new(),
            force: false,
            rehash: false,
            retry_preparation: false,
            queued: Instant::now(),
            active: Instant::now(),
            watch_pending: false,
            pending: true,
            cancel_pending: false,
            disk_pause: Some(DiskPause {
                force: true,
                retry_preparation: true,
                recovery_explicit: true,
                ..pause_fixture()
            }),
            recovery_blocked: false,
            recovery_explicit: false,
            recovering: false,
            upgrade_format: None,
        };
        display_disk_pause(&mut entry);
        let age = entry.queued;
        for _ in 0..3 {
            queue(&mut entry, None, false);
        }
        assert_eq!(entry.queued, age);
        assert_eq!(entry.status.outcome_code, "disk_pressure");
        assert_eq!(entry.failed_input_hash, "failed-saved-input");
        assert_eq!(entry.analysis_error, "provider failed");
        release_disk_pause(&mut entry);
        assert_eq!(entry.status.outcome_code, "provider_error");
        assert!(entry.force && entry.retry_preparation && entry.recovery_explicit);
        entry.disk_pause = Some(DiskPause {
            force: true,
            retry_preparation: true,
            recovery_explicit: true,
            ..pause_fixture()
        });
        begin_cancellation(&mut entry);
        assert!(entry.disk_pause.is_none() && entry.cancel_pending && entry.pending);
        assert!(!entry.force && !entry.retry_preparation && !entry.recovery_explicit);
        entry.cancel_pending = false;
        entry.recovering = true;
        entry.pending = false;
        entry.recovery_explicit = false;
        entry.recovery_blocked = false;
        let stale = DiskPause {
            recovery_explicit: true,
            force: true,
            ..pause_fixture()
        };
        begin_cancellation(&mut entry);
        record_disk_pause(&mut entry, stale.clone());
        assert!(
            entry.disk_pause.is_none()
                && entry.recovery_blocked
                && !entry.pending
                && !entry.recovery_explicit
        );
        assert_eq!(entry.status.phase, "cancelled");
        // A refresh after cancel is a new authorization, not stale resurrection.
        entry.recovery_blocked = false;
        entry.recovery_explicit = true;
        entry.pending = true;
        entry.force = true;
        record_disk_pause(&mut entry, stale);
        assert!(entry.disk_pause.is_some() && entry.pending && entry.recovery_explicit);
    }
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
            trust_epoch: 0,
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
        let data = control::root_for_database(Path::new(&job.database)).unwrap();
        settings::set_project_execution(&job.root, data, true).unwrap();
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
            &mut false,
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
        settings::set_project_execution(
            &retry.root,
            control::root_for_database(Path::new(&retry.database)).unwrap(),
            true,
        )
        .unwrap();
        retry.previous_failed_input_hash = hash.clone();
        let mut next_hash = String::new();
        let error = execute(
            retry,
            Path::new("/no/compiler/installation"),
            &mut next_hash,
            &mut |_| {},
            None,
            &mut false,
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
                None,
                &mut false,
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
                None,
                &mut false,
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
                None,
                &mut false,
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
            project_trusted: true,
            trust_epoch: 0,
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
            disk_pause: None,
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
            &mut false,
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
            &mut false,
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
        settings::set_project_execution(
            &retry.root,
            control::root_for_database(Path::new(&retry.database)).unwrap(),
            true,
        )
        .unwrap();
        retry.previous_generation = generation;
        retry.previous_failed_input_hash = hash.clone();
        let mut retry_hash = String::new();
        let error = execute(
            retry,
            Path::new("/no/compiler/installation"),
            &mut retry_hash,
            &mut |_| {},
            None,
            &mut false,
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
            &mut false,
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
                project_trusted: true,
                trust_epoch: 0,
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
                disk_pause: None,
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
        assert_eq!(entry.snapshot(), status);
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
            settings::set_project_execution(
                &job.root,
                control::root_for_database(Path::new(&job.database)).unwrap(),
                true,
            )
            .unwrap();
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
                &mut false,
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
