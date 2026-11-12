//! Coordinator ownership of cursor leases and saved result files. No queries.
use super::*;
use crate::{
    execution::QueryStart,
    protocol::{CursorReply, CursorRequest, CursorSettings},
    resource::Control,
    store::Writer,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, VecDeque},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    sync::{
        Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

pub const CONTROL_VERSION: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CursorLimits {
    pub max_cursors: u32,
    pub retention_bytes: u64,
}

impl Default for CursorLimits {
    fn default() -> Self {
        Self {
            max_cursors: DEFAULT_MAX_CURSORS as u32,
            retention_bytes: DEFAULT_RETENTION_BYTES,
        }
    }
}
impl CursorLimits {
    fn validate(&self) -> Result<()> {
        if self.max_cursors == 0 || self.retention_bytes == 0 {
            return Err(Error::InvalidRequest(
                "cursor count and retention bytes must be positive".into(),
            ));
        }
        Ok(())
    }
    fn load(root: &Path) -> Result<Self> {
        let limits: Self = match std::fs::read(root.join("cursors.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error.into()),
        };
        limits.validate()?;
        Ok(limits)
    }
    fn save(&self, root: &Path) -> Result<()> {
        self.validate()?;
        let staged = root.join("cursors.json.next");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&staged)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.sync_all()?;
        std::fs::rename(staged, root.join("cursors.json"))?;
        std::fs::File::open(root)?.sync_all()?;
        Ok(())
    }
}

struct State {
    registry: CursorRegistry,
    initialized: BTreeSet<(PathBuf, String)>,
    file_error: Option<String>,
    pending_publications: usize,
    disabled: BTreeSet<PathBuf>,
    cleanup_pending: BTreeMap<PathBuf, String>,
    cleanup_queue: VecDeque<PathBuf>,
    pressure: Option<String>,
    maintenance_error: Option<String>,
}
pub struct CursorService {
    root: PathBuf,
    state: Mutex<State>,
    changed: Condvar,
    stopped: AtomicBool,
}
// This owns only publication bookkeeping. Poison is recovered solely to drain
// ownership during an unwind; ordinary operations still reject poisoned state.
struct Publication<'a>(&'a CursorService);
impl Drop for Publication<'_> {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.pending_publications -= 1;
        let pending = state.pending_publications > 0;
        state.registry.set_publication_waiting(pending);
        self.0.changed.notify_all();
    }
}
impl CursorService {
    fn open_files(root: &Path) -> Result<result_store::ResultStore> {
        let mut files = result_store::ResultStore::open(&root.join("cursors"))?;
        files.reset()?;
        Ok(files)
    }
    pub fn new(root: &Path, databases: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let owner = nonce()?;
        let limits = CursorLimits::load(root)?;
        let mut initialized = BTreeSet::new();
        let mut cleanup_pending = BTreeMap::new();
        let startup = Execution::new(Default::default(), Some(100))?;
        // Retire previous-owner pins even if result-cache cleanup later fails.
        // A damaged cache must not retain obsolete index generations forever.
        for database in databases {
            if crate::recovery::selected_path(&database).is_ok_and(|path| path.is_file()) {
                let activated = startup
                    .phase(Duration::from_millis(20))
                    .and_then(|execution| {
                        GenerationRetention::activate_controlled(&database, owner, Some(execution))
                    });
                match activated {
                    Ok(retention) => {
                        initialized.insert((database, retention.health.incarnation().to_owned()));
                    }
                    // Unreadable stores cannot be initialized by this runtime.
                    // Keep their version error local; retrying them as shared
                    // pin cleanup would block healthy sibling pagination.
                    Err(
                        Error::IndexCorrupt { .. }
                        | Error::IndexRecovering(_)
                        | Error::UpdateRequired(_),
                    ) => {}
                    Err(error) => {
                        cleanup_pending.insert(database, error.to_string());
                    }
                }
            }
        }
        let mut registry = CursorRegistry::with_budget(
            owner,
            limits.max_cursors as usize,
            limits.retention_bytes,
        )?;
        let file_error = match Self::open_files(root) {
            Ok(files) => {
                registry.files = Some(files);
                None
            }
            Err(error) => Some(error.to_string()),
        };
        Ok(Self {
            root: root.into(),
            state: Mutex::new(State {
                registry,
                initialized,
                file_error,
                pending_publications: 0,
                disabled: BTreeSet::new(),
                cleanup_queue: cleanup_pending.keys().cloned().collect(),
                cleanup_pending,
                pressure: None,
                maintenance_error: None,
            }),
            changed: Condvar::new(),
            stopped: AtomicBool::new(false),
        })
    }
    fn lock(&self) -> Result<MutexGuard<'_, State>> {
        self.state.lock().map_err(|_| {
            Error::Coordinator(
                "cursor state is unavailable after an internal failure; restart the coordinator"
                    .into(),
            )
        })
    }
    fn lock_controlled(&self, execution: &Execution) -> Result<MutexGuard<'_, State>> {
        loop {
            execution.check()?;
            self.running()?;
            match self.state.try_lock() {
                Ok(state) => return Ok(state),
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(Error::Coordinator(
                        "cursor state is unavailable after an internal failure; restart the coordinator".into(),
                    ));
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(execution.remaining()?.min(Duration::from_millis(1)));
                }
            }
        }
    }
    fn running(&self) -> Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            Err(Error::CursorInvalidated)
        } else {
            Ok(())
        }
    }
    /// Drain in-flight mutations before the host releases its writer fence.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.changed.notify_all();
        if let Ok(mut state) = self.state.lock() {
            while state.pending_publications > 0 {
                match self.changed.wait(state) {
                    Ok(next) => state = next,
                    Err(_) => return,
                }
            }
        }
    }
    fn initialize(state: &mut State, database: &Path, execution: Execution) -> Result<()> {
        let incarnation = crate::recovery::selected_incarnation(database)?.unwrap_or_default();
        if !state
            .initialized
            .contains(&(database.to_owned(), incarnation.clone()))
        {
            GenerationRetention::activate_controlled(
                database,
                state.registry.owner,
                Some(execution),
            )?;
            state.initialized.insert((database.into(), incarnation));
            state.cleanup_pending.remove(database);
        }
        Ok(())
    }
    /// Fence tokens before the host removes a registration. Existing bounded
    /// reads retain their pins until they finish or their leases expire.
    pub fn unregister(&self, database: &Path) -> Result<()> {
        let mut state = self.lock()?;
        state.disabled.insert(database.into());
        state.initialized.retain(|(path, _)| path != database);
        for entry in state.registry.entries.values_mut() {
            if entry.pin.database == database {
                entry.retiring = true;
            }
        }
        self.changed.notify_all();
        Ok(())
    }
    /// Corrupt pin SQL is abandoned with the physical incarnation. Results
    /// still retire through normal file cleanup, retaining failed charges.
    pub fn invalidate_corrupt(&self, database: &Path, incarnation: &str) -> Result<()> {
        let mut state = self.lock()?;
        state.initialized.retain(|(path, _)| path != database);
        state.cleanup_pending.remove(database);
        state.cleanup_queue.retain(|path| path != database);
        for entry in state.registry.entries.values_mut() {
            if entry.pin.database == database && entry.pin.incarnation() == incarnation {
                entry.retiring = true;
            }
        }
        self.changed.notify_all();
        Ok(())
    }
    pub fn register(&self, database: &Path) -> Result<()> {
        self.lock()?.disabled.remove(database);
        Ok(())
    }
    fn enforce_limits(state: &mut State, execution: Execution) -> Result<()> {
        let result = (|| {
            while state.registry.entries.len() > state.registry.maximum {
                if !state.registry.remove_idle_except(None, execution.clone())? {
                    return Err(Error::ResourceLimited(
                        "cursor count pressure is waiting for active reads".into(),
                    ));
                }
            }
            state.registry.make_space(None, None, execution)
        })();
        state.pressure = result.as_ref().err().map(ToString::to_string);
        result
    }
    pub fn request(
        &self,
        database: &Path,
        request: CursorRequest,
        start: QueryStart,
    ) -> Result<CursorReply> {
        let execution = Execution::new(start, Some(request.remaining_ms))?
            .cap_at(Duration::from_millis(request.deadline_at_ms))?;
        execution.check()?;
        self.running()?;
        let result = self.request_inner(database, request, execution.clone());
        execution.check()?;
        self.running()?;
        self.changed.notify_all();
        result
    }
    fn request_inner(
        &self,
        database: &Path,
        request: CursorRequest,
        execution: Execution,
    ) -> Result<CursorReply> {
        let mut state = self.lock_controlled(&execution)?;
        execution.check()?;
        self.running()?;
        if request.action == "settings" {
            if state.pressure.is_some() {
                let _ = Self::enforce_limits(&mut state, execution.clone());
            }
            if let Some(value) = request.settings {
                let limits = CursorLimits {
                    max_cursors: value.max_cursors,
                    retention_bytes: value.retention_bytes,
                };
                limits.save(&self.root)?;
                state.registry.maximum = limits.max_cursors as usize;
                state.registry.maximum_bytes = limits.retention_bytes;
                let _ = Self::enforce_limits(&mut state, execution);
            }
            let warning = state
                .file_error
                .clone()
                .or_else(|| {
                    state
                        .cleanup_pending
                        .values()
                        .next()
                        .map(|error| format!("previous cursor pin cleanup is pending: {error}"))
                })
                .or_else(|| state.registry.retirement_warning.clone())
                .or_else(|| state.pressure.clone())
                .or_else(|| state.maintenance_error.clone())
                .unwrap_or_default();
            return Ok(CursorReply {
                settings: Some(CursorSettings {
                    max_cursors: state.registry.maximum as u32,
                    retention_bytes: state.registry.maximum_bytes,
                }),
                pending_pressure: !warning.is_empty()
                    || state.registry.entries.len() > state.registry.maximum,
                warning,
                ..Default::default()
            });
        }
        if state.disabled.contains(database) {
            return Err(Error::CursorInvalidated);
        }
        if let Err(error) = state.registry.sweep_controlled(execution.clone()) {
            state.maintenance_error = Some(error.to_string());
            return Err(error);
        }
        if let Some(error) = &state.file_error {
            return Err(Error::ResourceLimited(format!(
                "cursor result storage unavailable: {error}; repair the cursor cache permissions/contents; cursor maintenance will retry"
            )));
        }
        if request.action == "admit" {
            Self::initialize(&mut state, database, execution.clone())?;
            if !state.cleanup_pending.is_empty() {
                return Err(Error::ResourceLimited("previous cursor pins are still being cleaned up; retry when cursor settings no longer report pending cleanup".into()));
            }
            let handle = state
                .registry
                .admit_controlled(database, execution.clone())?;
            let read = match state
                .registry
                .borrow_controlled(database, handle, execution.clone())
            {
                Ok(read) => read,
                Err(error) => {
                    let _ = state.registry.retire(handle.id, execution);
                    return Err(error);
                }
            };
            state
                .registry
                .entries
                .get_mut(&handle.id)
                .expect("new cursor")
                .creator = Some(read.id);
            return Self::reply(&state, &read, None);
        }
        let handle = CursorHandle::parse_prefix(&request.cursor)?;
        if handle.owner != state.registry.owner {
            return Err(Error::CursorInvalidated);
        }
        if request.action == "borrow" {
            let entry = state
                .registry
                .entries
                .get(&handle.id)
                .ok_or(Error::CursorInvalidated)?;
            if entry.pin.database != database || entry.retiring || entry.pin.health.check().is_err()
            {
                return Err(Error::CursorInvalidated);
            }
            if entry.creator.is_some() {
                return Err(Error::ResourceLimited(
                    "cursor result is not complete yet".into(),
                ));
            }
            let stored = state
                .registry
                .files
                .as_ref()
                .expect("available files")
                .get(&handle.key())?;
            let read = state
                .registry
                .borrow_controlled(database, handle, execution)?;
            return Self::reply(&state, &read, Some(&stored));
        }
        let cleanup = matches!(request.action.as_str(), "finish" | "discard" | "invalidate");
        let read = Self::read(&state.registry, database, handle, &request.lease, cleanup)?;
        let creator = state.registry.entries[&handle.id].creator == Some(read.id);
        match request.action.as_str() {
            "reserve" if creator => {
                if request.bytes > MAX_RESULT_BYTES {
                    return Err(Error::ResourceLimited(
                        "saved cursor result exceeds the 16 MiB safety limit".into(),
                    ));
                }
                state
                    .registry
                    .reserve_controlled(handle, request.bytes, execution)?;
                state
                    .registry
                    .files
                    .as_mut()
                    .expect("available files")
                    .begin(&handle.key(), request.bytes)?;
                Self::reply(&state, &read, None)
            }
            "chunk" if creator => {
                let offset = state
                    .registry
                    .files
                    .as_mut()
                    .expect("available files")
                    .append(&handle.key(), request.offset, &request.chunk)?;
                let mut reply = Self::reply(&state, &read, None)?;
                reply.offset = offset;
                Ok(reply)
            }
            "complete" if creator => {
                let stored = state
                    .registry
                    .files
                    .as_mut()
                    .expect("available files")
                    .complete_with(&handle.key(), &request.sha256, || {
                        self.running()?;
                        execution.check()
                    })?;
                Self::reply(&state, &read, Some(&stored))
            }
            "finish" => {
                if read.pin.health.check().is_err() {
                    let entry = state
                        .registry
                        .entries
                        .get_mut(&handle.id)
                        .expect("read cursor");
                    entry.reads.remove(&read.id);
                    entry.retiring = true;
                    if entry.reads.is_empty() {
                        state.registry.retire(handle.id, execution)?;
                    }
                    return Ok(CursorReply::default());
                }
                if creator {
                    state
                        .registry
                        .files
                        .as_ref()
                        .expect("available files")
                        .get(&handle.key())?;
                    state
                        .registry
                        .entries
                        .get_mut(&handle.id)
                        .expect("read cursor")
                        .creator = None;
                }
                state.registry.finish_controlled(&read, execution)?;
                Ok(CursorReply::default())
            }
            "discard" if creator => {
                state
                    .registry
                    .entries
                    .get_mut(&handle.id)
                    .expect("read cursor")
                    .reads
                    .remove(&read.id);
                state.registry.retire(handle.id, execution)?;
                Ok(CursorReply::default())
            }
            "invalidate" => {
                let entry = state
                    .registry
                    .entries
                    .get_mut(&handle.id)
                    .expect("read cursor");
                entry.reads.remove(&read.id);
                entry.retiring = true;
                if entry.reads.is_empty() {
                    state.registry.retire(handle.id, execution)?;
                }
                Ok(CursorReply::default())
            }
            _ => Err(Error::InvalidRequest(
                "unknown or inapplicable cursor action".into(),
            )),
        }
    }
    fn read(
        registry: &CursorRegistry,
        database: &Path,
        handle: CursorHandle,
        lease: &str,
        cleanup: bool,
    ) -> Result<CursorRead> {
        let entry = registry
            .entries
            .get(&handle.id)
            .ok_or(Error::CursorInvalidated)?;
        let now = crate::execution::continuous_time()?;
        let id = unhex::<16>(lease)?;
        let until = *entry.reads.get(&id).ok_or(Error::CursorInvalidated)?;
        if entry.pin.database != database
            || (!cleanup && (entry.retiring || entry.pin.health.check().is_err()))
            || entry.expired(now)
            || until <= now
        {
            return Err(Error::CursorInvalidated);
        }
        Ok(CursorRead {
            handle,
            id,
            pin: entry.pin.clone(),
            until,
            expires: entry.created.saturating_add(ABSOLUTE_LIFETIME),
        })
    }
    fn reply(
        _state: &State,
        read: &CursorRead,
        stored: Option<&result_store::StoredResult>,
    ) -> Result<CursorReply> {
        read.pin.health.check()?;
        let mut reply = CursorReply {
            cursor: read.handle.prefix(),
            lease: hexadecimal(&read.id),
            owner: read.pin.owner.to_vec(),
            pin: read.pin.id.to_vec(),
            generation: read.pin.generation,
            index_incarnation: read.pin.incarnation().into(),
            lease_deadline_ms: read.until.as_millis().try_into().map_err(|_| {
                Error::InvalidRequest("cursor deadline is not representable".into())
            })?,
            ..Default::default()
        };
        if let Some(stored) = stored {
            reply.result_path = stored.path.to_string_lossy().into_owned();
            reply.result_bytes = stored.bytes;
            reply.sha256 = stored.sha256.clone();
        }
        Ok(reply)
    }
    pub fn sweep(&self) -> Result<()> {
        let mut state = self.lock()?;
        self.running()?;
        // Startup refuses foreign/unsafe cache entries. Once the owner repairs
        // them, retry the same bounded reset without discarding any live state.
        if state.file_error.is_some()
            && state.registry.files.is_none()
            && state.registry.entries.is_empty()
        {
            match Self::open_files(&self.root) {
                Ok(files) => {
                    state.registry.files = Some(files);
                    state.file_error = None;
                }
                Err(error) => state.file_error = Some(error.to_string()),
            }
        }
        let execution = Execution::new(Default::default(), None)?;
        // Existing leases/retiring files make progress before old-incarnation
        // retries. Rotate retries so one locked index cannot starve the rest.
        let result = state
            .registry
            .sweep_controlled(execution.clone())
            .and_then(|()| {
                if state.pressure.is_some() {
                    Self::enforce_limits(&mut state, execution)
                } else {
                    Ok(())
                }
            });
        // Owner activation reclaims old manifests atomically. Repeating the
        // tiny startup probe can roll back legitimate cleanup forever. Retry
        // one queued index with the existing maximum cooperative budget.
        for _ in 0..state.cleanup_queue.len().min(1) {
            let execution =
                Execution::new(Default::default(), Some(crate::execution::MAX_DEADLINE_MS))?;
            let database = state.cleanup_queue.pop_front().expect("queued cleanup");
            if !state.cleanup_pending.contains_key(&database) {
                continue;
            }
            match Self::initialize(&mut state, &database, execution) {
                Ok(()) => {}
                Err(Error::UpdateRequired(_)) => {
                    // Startup may exhaust its cooperative budget before it can
                    // diagnose this store. Once known, its version boundary is
                    // workspace-local, not shared cleanup that blocks siblings.
                    state.cleanup_pending.remove(&database);
                }
                Err(error) => {
                    state
                        .cleanup_pending
                        .insert(database.clone(), error.to_string());
                    state.cleanup_queue.push_back(database);
                }
            }
        }
        state.maintenance_error = result.as_ref().err().map(ToString::to_string);
        self.changed.notify_all();
        result
    }
    /// Analysis has completed before entry. Keep admission serialized through
    /// actual commit, but release the mutex while bounded active reads finish.
    pub fn publish(&self, database: &Path, mut writer: Writer, control: &Control) -> Result<u64> {
        let execution =
            Execution::new(Default::default(), Some(crate::execution::MAX_DEADLINE_MS))?;
        let ownership = self.own_publication(&execution)?;
        let generation = writer.generation;
        let publication = control.publication();
        let result = self.publish_inner(database, Some(generation), control, execution, || {
            writer.commit_controlled(&publication)
        });
        writer.maintain_published();
        // Failed staging cleanup and sqlite close also happen after the global
        // cursor fence has been released, not inside the publication callback.
        drop(writer);
        drop(ownership);
        result
    }
    /// Serialize a selector cutover with cursor admission and shared retention
    /// pressure. Healthy old pins remain valid on their original incarnation.
    /// Prevalidate/checkpoint/fsync the candidate before entering this fence.
    /// The callback is only the short selector commit; never scan SQLite or
    /// wait for lifecycle locks before calling this method.
    pub fn publish_replacement<T>(
        &self,
        database: &Path,
        control: &Control,
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.publish_with(database, None, control, publish)
    }
    fn publish_with<T>(
        &self,
        database: &Path,
        generation: Option<u64>,
        control: &Control,
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let execution =
            Execution::new(Default::default(), Some(crate::execution::MAX_DEADLINE_MS))?;
        let ownership = self.own_publication(&execution)?;
        let result = self.publish_inner(database, generation, control, execution, publish);
        drop(ownership);
        result
    }
    fn own_publication(&self, execution: &Execution) -> Result<Publication<'_>> {
        let mut state = self.lock_controlled(execution)?;
        state.pending_publications += 1;
        Ok(Publication(self))
    }
    fn publish_inner<T>(
        &self,
        database: &Path,
        generation: Option<u64>,
        control: &Control,
        execution: Execution,
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        // Poll background throttling before taking the global cursor fence.
        // Ownership is already registered so shutdown drains the writer, but
        // paused analysis neither blocks page reads nor parks under the fence.
        while !control.publication_ready()? {
            self.running()?;
            std::thread::sleep(execution.remaining()?.min(Duration::from_millis(1)));
        }
        (|| {
            let mut state = self.lock_controlled(&execution)?;
            state.registry.set_publication_waiting(true);
            if generation.is_some() {
                Self::initialize(&mut state, database, execution.clone())?;
            }
            let incarnation = crate::recovery::selected_incarnation(database)?.unwrap_or_default();
            loop {
                self.running()?;
                if state.disabled.contains(database) {
                    return Err(Error::CursorInvalidated);
                }
                control.check_stopped()?;
                execution.check()?;
                if state.registry.prepare_selected(
                    database,
                    &incarnation,
                    generation,
                    execution.clone(),
                )? {
                    let result = publish();
                    if generation.is_none() {
                        // Selection may become visible even if its fsync acknowledgement failed.
                        // A new incarnation initializes independently; never reactivate old pins.
                        state.initialized.retain(|(path, _)| path != database);
                    }
                    return result;
                }
                let (next, _) = self
                    .changed
                    .wait_timeout(state, Duration::from_millis(20))
                    .map_err(|_| Error::Coordinator("cursor publication wait failed".into()))?;
                state = next;
            }
        })()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    fn fixture() -> (PathBuf, PathBuf, CursorService) {
        let database = crate::cursor::lifecycle_tests::database()
            .canonicalize()
            .unwrap();
        let root = database.parent().unwrap().join("coordinator");
        std::fs::create_dir_all(&root).unwrap();
        let service = CursorService::new(&root, [database.clone()]).unwrap();
        (root, database, service)
    }
    fn call(service: &CursorService, db: &Path, mut request: CursorRequest) -> Result<CursorReply> {
        let start = QueryStart::default();
        let execution = Execution::new(start, None).unwrap();
        request.remaining_ms = 5000;
        request.deadline_at_ms = execution.deadline_ms().unwrap();
        service.request(db, request, start)
    }
    fn action(action: &str, lease: Option<&CursorReply>) -> CursorRequest {
        CursorRequest {
            action: action.into(),
            cursor: lease.map(|r| r.cursor.clone()).unwrap_or_default(),
            lease: lease.map(|r| r.lease.clone()).unwrap_or_default(),
            ..Default::default()
        }
    }
    fn ready(service: &CursorService, db: &Path) -> CursorReply {
        let lease = call(service, db, action("admit", None)).unwrap();
        call(
            service,
            db,
            CursorRequest {
                bytes: 5,
                ..action("reserve", Some(&lease))
            },
        )
        .unwrap();
        call(
            service,
            db,
            CursorRequest {
                chunk: b"saved".into(),
                ..action("chunk", Some(&lease))
            },
        )
        .unwrap();
        let result = call(
            service,
            db,
            CursorRequest {
                sha256: format!("{:x}", Sha256::digest(b"saved")),
                ..action("complete", Some(&lease))
            },
        )
        .unwrap();
        call(service, db, action("finish", Some(&lease))).unwrap();
        result
    }
    fn legacy_fixture() -> (PathBuf, CursorService) {
        let db = crate::cursor::lifecycle_tests::legacy_database()
            .canonicalize()
            .unwrap();
        crate::recovery::adopt(&db, 1).unwrap();
        let root = db.parent().unwrap().join("coordinator");
        std::fs::create_dir_all(&root).unwrap();
        let service = CursorService::new(&root, [db.clone()]).unwrap();
        (db, service)
    }
    fn ready_symbols(service: &CursorService, db: &Path) -> CursorReply {
        let lease = call(service, db, action("admit", None)).unwrap();
        let handle = CursorHandle::parse_prefix(&lease.cursor).unwrap();
        let pin = service.lock().unwrap().registry.entries[&handle.id]
            .pin
            .clone();
        let result = pin
            .open_fixture_view()
            .unwrap()
            .query(serde_json::from_str(r#"{"op":"symbols"}"#).unwrap())
            .unwrap();
        let encoded = super::super::pages::encode(
            serde_json::to_value(result).unwrap(),
            super::super::pages::EncodeOptions {
                op: "symbols",
                total_limit: 1000,
                page_size: 1,
                cursor_prefix: &lease.cursor,
                response_max_bytes: 256 * 1024,
            },
            Execution::new(Default::default(), None).unwrap(),
        )
        .unwrap();
        call(
            service,
            db,
            CursorRequest {
                bytes: encoded.len() as u64,
                ..action("reserve", Some(&lease))
            },
        )
        .unwrap();
        for (i, chunk) in encoded.chunks(MAX_RESULT_CHUNK).enumerate() {
            call(
                service,
                db,
                CursorRequest {
                    offset: (i * MAX_RESULT_CHUNK) as u64,
                    chunk: chunk.to_vec(),
                    ..action("chunk", Some(&lease))
                },
            )
            .unwrap();
        }
        let result = call(
            service,
            db,
            CursorRequest {
                sha256: format!("{:x}", Sha256::digest(&encoded)),
                ..action("complete", Some(&lease))
            },
        )
        .unwrap();
        call(service, db, action("finish", Some(&lease))).unwrap();
        result
    }
    fn read_page(reply: &CursorReply) -> serde_json::Value {
        super::super::pages::read(
            &mut std::fs::File::open(&reply.result_path).unwrap(),
            super::super::pages::ReadOptions {
                position: 0,
                cursor_prefix: &reply.cursor,
                expected_generation: reply.generation,
                response_max_bytes: 256 * 1024,
                freshness: "stale",
                coordination: None,
            },
            Execution::new(Default::default(), None).unwrap(),
        )
        .unwrap()
    }
    fn pin_for(service: &CursorService, reply: &CursorReply) -> GenerationPin {
        let handle = CursorHandle::parse_prefix(&reply.cursor).unwrap();
        service.lock().unwrap().registry.entries[&handle.id]
            .pin
            .clone()
    }
    fn indexed_replacement(db: &Path, explicit: bool) -> crate::recovery::Candidate {
        use crate::protocol::{AnalysisReport, Compilation, SourceArtifact, envelope::Payload};
        let candidate = crate::recovery::begin_upgrade(db, explicit, 1, 2).unwrap();
        let mut writer = Writer::open_destination(
            db,
            &candidate.database,
            None,
            candidate.minimum_generation,
            &Control::default(),
        )
        .unwrap();
        writer
            .stage(Payload::Compilation(Compilation {
                id: "comp".into(),
                language: "typescript".into(),
                ..Default::default()
            }))
            .unwrap();
        let text = "replacement saved 😀";
        writer
            .stage(Payload::Source(SourceArtifact {
                id: "source".into(),
                path: "a.ts".into(),
                compilation_id: "comp".into(),
                sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
                utf8: text.as_bytes().into(),
                ..Default::default()
            }))
            .unwrap();
        writer
            .stage(Payload::Report(AnalysisReport {
                compilation_id: "comp".into(),
                status: "complete".into(),
                ..Default::default()
            }))
            .unwrap();
        writer.publish().unwrap();
        candidate
    }
    fn saved_source(view: &ReadView) -> String {
        view.query(serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap())
            .unwrap()
            .source
            .unwrap()
            .text
    }
    fn drain_startup_cleanup(service: &CursorService) {
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while !service.lock().unwrap().cleanup_pending.is_empty() {
            assert!(
                std::time::Instant::now() < until,
                "startup cleanup did not settle"
            );
            service.sweep().unwrap();
        }
        assert!(service.lock().unwrap().cleanup_pending.is_empty());
    }
    #[test]
    fn unreadable_format1_startup_does_not_block_healthy_sibling_paging() {
        let legacy = crate::cursor::lifecycle_tests::legacy_database()
            .canonicalize()
            .unwrap();
        let connection = rusqlite::Connection::open(&legacy).unwrap();
        // Actual historical schema1, using the same conversion as the existing
        // publication migration fixture; not a fake label on a modern store.
        connection.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE generation_units; DROP TABLE unit_artifacts; ALTER TABLE compilations DROP COLUMN dependencies_json; ALTER TABLE compilations DROP COLUMN input_paths_json; PRAGMA user_version=1;").unwrap();
        crate::recovery::adopt(&legacy, 1).unwrap();
        let healthy = crate::cursor::lifecycle_tests::database()
            .canonicalize()
            .unwrap();
        let root = legacy.parent().unwrap().join("coordinator");
        std::fs::create_dir_all(&root).unwrap();
        let service = CursorService::new(&root, [legacy.clone(), healthy.clone()]).unwrap();
        drain_startup_cleanup(&service);
        assert!(service.lock().unwrap().cleanup_pending.is_empty());
        let page = ready_symbols(&service, &healthy);
        assert!(read_page(&page)["symbols"].as_array().unwrap().is_empty());
        assert!(matches!(
            call(&service, &legacy, action("admit", None)),
            Err(Error::UpdateRequired(_))
        ));
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='cursor_generation_pins'",
                    [],
                    |r| r.get::<_, i32>(0)
                )
                .unwrap(),
            0
        );
        let again = call(&service, &healthy, action("borrow", Some(&page))).unwrap();
        assert_eq!(read_page(&again), read_page(&page));
        call(&service, &healthy, action("finish", Some(&again))).unwrap();
    }

    #[test]
    fn deferred_startup_probe_later_localizes_unreadable_store_and_unblocks_sibling() {
        let legacy = crate::cursor::lifecycle_tests::legacy_database()
            .canonicalize()
            .unwrap();
        let connection = rusqlite::Connection::open(&legacy).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE generation_units; DROP TABLE unit_artifacts; ALTER TABLE compilations DROP COLUMN dependencies_json; ALTER TABLE compilations DROP COLUMN input_paths_json; PRAGMA user_version=1;").unwrap();
        crate::recovery::adopt(&legacy, 1).unwrap();
        // In rollback-journal mode EXCLUSIVE blocks even user_version reads,
        // so startup really defers before discovering the unsupported format.
        connection
            .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
            .unwrap();
        let healthy = crate::cursor::lifecycle_tests::database()
            .canonicalize()
            .unwrap();
        let root = legacy.parent().unwrap().join("coordinator");
        std::fs::create_dir_all(&root).unwrap();
        let service = CursorService::new(&root, [legacy.clone(), healthy.clone()]).unwrap();
        assert!(
            service
                .lock()
                .unwrap()
                .cleanup_pending
                .contains_key(&legacy)
        );
        connection.execute_batch("ROLLBACK;").unwrap();
        drain_startup_cleanup(&service);
        assert!(!service.lock().unwrap().cleanup_queue.contains(&legacy));
        assert!(matches!(
            call(&service, &legacy, action("admit", None)),
            Err(Error::UpdateRequired(_))
        ));
        assert!(service.lock().unwrap().cleanup_pending.is_empty());
        let page = ready_symbols(&service, &healthy);
        assert!(read_page(&page)["symbols"].as_array().unwrap().is_empty());
        let again = call(&service, &healthy, action("borrow", Some(&page))).unwrap();
        assert_eq!(read_page(&again), read_page(&page));
        call(&service, &healthy, action("finish", Some(&again))).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='cursor_generation_pins'",
                    [],
                    |r| r.get::<_, i32>(0)
                )
                .unwrap(),
            0
        );
        assert!(!crate::recovery::is_bad(&legacy).unwrap());
    }

    #[test]
    fn format2_pages_remain_available_during_failed_upgrade_without_migration() {
        let (db, service) = legacy_fixture();
        let initial = ready_symbols(&service, &db);
        let before = read_page(&initial);
        assert_eq!(before["symbols"][0]["signature"], "needle legacy π😀");
        let candidate = crate::recovery::begin_upgrade(&db, false, 1, 2).unwrap();
        let during = ready_symbols(&service, &db);
        assert_eq!(read_page(&during)["symbols"], before["symbols"]);
        crate::recovery::fail_rebuild(&db, "owned fixture provider failure").unwrap();
        let failed = ready_symbols(&service, &db);
        assert_eq!(read_page(&failed)["symbols"], before["symbols"]);
        assert!(crate::recovery::begin_upgrade(&db, false, 1, 2).is_err());
        let pin = pin_for(&service, &initial);
        assert_eq!(
            saved_source(&pin.open_fixture_view().unwrap()),
            "needle legacy π😀"
        );
        assert_eq!(
            rusqlite::Connection::open(crate::recovery::selected_path(&db).unwrap())
                .unwrap()
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            2
        );
        assert_ne!(candidate.incarnation, initial.index_incarnation);
    }
    #[test]
    fn healthy_cutover_retains_old_pages_context_and_baseline_through_next_publication() {
        let (db, service) = legacy_fixture();
        let stored = ready_symbols(&service, &db);
        let before = read_page(&stored);
        let pin = pin_for(&service, &stored);
        let original_view = pin.open_fixture_view().unwrap();
        let old_lookup = original_view
            .query(serde_json::from_str(r#"{"op":"symbols"}"#).unwrap())
            .unwrap();
        let initial = service.lock().unwrap().registry.retained_bytes().unwrap();
        let candidate = indexed_replacement(&db, false);
        let prepared = crate::recovery::prepare_rebuild(&candidate, &Control::default()).unwrap();
        let control = Control::test_managed();
        let publication = control.publication();
        let generation = service
            .publish_replacement(&db, &control, || {
                crate::recovery::finish_prepared_rebuild(&prepared, &publication, || {
                    // This is the actual nested cutover boundary under the
                    // cursor fence. A new CPU pause must not park it here.
                    control.test_pause(true);
                    publication.check()
                })
            })
            .unwrap();
        assert!(control.committed());
        assert!(
            !control.cancel(),
            "completed selector commit wins late cancellation"
        );
        assert!(generation > stored.generation);
        assert!(service.lock().unwrap().registry.retained_bytes().unwrap() > initial);
        let borrowed = call(&service, &db, action("borrow", Some(&stored))).unwrap();
        assert_eq!(read_page(&borrowed), before);
        call(&service, &db, action("finish", Some(&borrowed))).unwrap();
        let old = pin.open_fixture_view().unwrap();
        assert_eq!(saved_source(&old), "needle legacy π😀");
        let context = old
            .query(serde_json::from_str(r#"{"op":"symbol_context","symbol_id":"symbol"}"#).unwrap())
            .unwrap();
        assert_eq!(
            context.contexts[0].declarations[0]
                .source
                .as_ref()
                .unwrap()
                .text,
            "needle legacy π😀"
        );
        let current = crate::query::fixture_view(&db).unwrap();
        assert_eq!(saved_source(&current), "replacement saved 😀");
        let request = serde_json::from_value(serde_json::json!({"op":"source","artifact_id":"source","snapshot":old_lookup.snapshot})).unwrap();
        assert!(matches!(
            current.query(request),
            Err(Error::SnapshotInvalidated)
        ));
        drop(current);
        let writer = crate::cursor::lifecycle_tests::staged(&db, "next publication");
        let next = service.publish(&db, writer, &Control::default()).unwrap();
        assert!(next > generation);
        assert_eq!(
            saved_source(&crate::query::fixture_view(&db).unwrap()),
            "next publication"
        );
        // No replacement cursor has been admitted yet: publication itself must
        // initialize retention on the newly selected physical incarnation.
        let connection =
            rusqlite::Connection::open(crate::recovery::selected_path(&db).unwrap()).unwrap();
        let owner: Vec<u8> = connection
            .query_row("SELECT owner FROM cursor_retention_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owner, service.lock().unwrap().registry.owner);
        let fresh = ready_symbols(&service, &db);
        assert_eq!(fresh.index_incarnation, candidate.incarnation);
        assert!(read_page(&fresh)["symbols"].as_array().unwrap().is_empty());
        let again = call(&service, &db, action("borrow", Some(&stored))).unwrap();
        assert_eq!(read_page(&again), before);
        call(&service, &db, action("finish", Some(&again))).unwrap();
        assert_eq!(
            saved_source(&pin.open_fixture_view().unwrap()),
            "needle legacy π😀"
        );
    }
    #[test]
    fn healthy_cutover_pressure_waits_for_active_read_then_evicts_idle_cursor() {
        let (db, service) = legacy_fixture();
        let stored = ready_symbols(&service, &db);
        let read = call(&service, &db, action("borrow", Some(&stored))).unwrap();
        let held = pin_for(&service, &stored).open_fixture_view().unwrap();
        assert_eq!(saved_source(&held), "needle legacy π😀");
        let baseline = service.lock().unwrap().registry.retained_bytes().unwrap();
        service.lock().unwrap().registry.maximum_bytes = baseline;
        let candidate = indexed_replacement(&db, false);
        let prepared = crate::recovery::prepare_rebuild(&candidate, &Control::default()).unwrap();
        let called = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let publisher = scope.spawn(|| {
                service.publish_replacement(&db, &Control::default(), || {
                    called.store(true, Ordering::Release);
                    crate::recovery::finish_prepared_rebuild(&prepared, &Control::default(), || {
                        Ok(())
                    })
                })
            });
            wait_for_publication_fence(&service);
            assert!(!called.load(Ordering::Acquire));
            assert_eq!(
                crate::recovery::selected_incarnation(&db)
                    .unwrap()
                    .as_deref(),
                Some(stored.index_incarnation.as_str())
            );
            let denied = call(&service, &db, action("borrow", Some(&stored)));
            assert!(
                matches!(&denied, Err(Error::ResourceLimited(_))),
                "unexpected cursor admission result: {denied:?}"
            );
            let connection =
                rusqlite::Connection::open(crate::recovery::selected_path(&db).unwrap()).unwrap();
            connection.busy_timeout(Duration::from_millis(100)).unwrap();
            connection
                .execute_batch("BEGIN IMMEDIATE; ROLLBACK")
                .unwrap();
            assert_eq!(saved_source(&held), "needle legacy π😀");
            call(&service, &db, action("finish", Some(&read))).unwrap();
            assert!(publisher.join().unwrap().is_ok());
        });
        assert!(called.load(Ordering::Acquire));
        assert!(matches!(
            call(&service, &db, action("borrow", Some(&stored))),
            Err(Error::CursorInvalidated)
        ));
        assert_eq!(saved_source(&held), "needle legacy π😀");
        assert!(!Path::new(&stored.result_path).exists());
    }
    #[test]
    fn removed_initial_borrow_cleanup_is_idempotent_after_bad_sweep() {
        let (root, db, prior) = fixture();
        drop(prior);
        crate::recovery::adopt(&db, 1).unwrap();
        let healthy = crate::cursor::lifecycle_tests::database()
            .canonicalize()
            .unwrap();
        let service = CursorService::new(&root, [db.clone(), healthy.clone()]).unwrap();
        service.sweep().unwrap();
        service.sweep().unwrap();
        let execution = Execution::new(Default::default(), None).unwrap();
        {
            let mut state = service.lock().unwrap();
            let handle = state
                .registry
                .admit_controlled(&db, execution.clone())
                .unwrap();
            let guard = state.registry.entries[&handle.id].pin.health.clone();
            let checkpoint = rusqlite::Connection::open(&db).unwrap();
            assert_eq!(
                checkpoint
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, i32>(2)?
                    )))
                    .unwrap(),
                (0, 0, 0)
            );
            drop(checkpoint);
            std::fs::write(&db, b"owned corrupt header between admit and borrow").unwrap();
            let connection = rusqlite::Connection::open(&db).unwrap();
            let error = connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap_err();
            assert!(matches!(
                guard.map_sqlite(error),
                Error::IndexCorrupt { .. }
            ));
            assert!(matches!(
                state
                    .registry
                    .borrow_controlled(&db, handle, execution.clone()),
                Err(Error::CursorInvalidated)
            ));
            assert!(!state.registry.entries.contains_key(&handle.id));
            // request_inner uses exactly this cleanup when first borrow fails.
            state.registry.retire(handle.id, execution).unwrap();
        }
        let result = ready(&service, &healthy);
        let borrowed = call(&service, &healthy, action("borrow", Some(&result))).unwrap();
        call(&service, &healthy, action("finish", Some(&borrowed))).unwrap();
    }
    #[test]
    fn healthy_cleanup_failure_keeps_debt_and_allows_sibling_read_and_publish() {
        let (root, db, prior) = fixture();
        drop(prior);
        let healthy = crate::cursor::lifecycle_tests::database()
            .canonicalize()
            .unwrap();
        let service = CursorService::new(&root, [db.clone(), healthy.clone()]).unwrap();
        service.sweep().unwrap();
        service.sweep().unwrap();
        let blocked = ready(&service, &db);
        let sibling = ready(&service, &healthy);
        let reclaimable = ready(&service, &healthy);
        let blocked_id = CursorHandle::parse_prefix(&blocked.cursor).unwrap().id;
        let reclaimable_id = CursorHandle::parse_prefix(&reclaimable.cursor).unwrap().id;
        std::fs::remove_file(&blocked.result_path).unwrap();
        std::fs::create_dir(&blocked.result_path).unwrap();
        {
            let mut state = service.lock().unwrap();
            state
                .registry
                .entries
                .get_mut(&blocked_id)
                .unwrap()
                .retiring = true;
            state
                .registry
                .entries
                .get_mut(&reclaimable_id)
                .unwrap()
                .retiring = true;
        }
        let read = call(&service, &healthy, action("borrow", Some(&sibling))).unwrap();
        assert_eq!(std::fs::read(&read.result_path).unwrap(), b"saved");
        call(&service, &healthy, action("finish", Some(&read))).unwrap();
        {
            let state = service.lock().unwrap();
            assert_eq!(state.registry.entries[&blocked_id].result_bytes_reserved, 5);
            assert!(state.registry.retirement_warning.is_some());
            assert!(!state.registry.entries.contains_key(&reclaimable_id));
        }
        let writer = crate::cursor::lifecycle_tests::staged(&healthy, "saved sibling replacement");
        let generation = writer.generation;
        assert_eq!(
            service
                .publish(&healthy, writer, &Control::default())
                .unwrap(),
            generation
        );
        let read = call(&service, &healthy, action("borrow", Some(&sibling))).unwrap();
        call(&service, &healthy, action("finish", Some(&read))).unwrap();
        std::fs::remove_dir(&blocked.result_path).unwrap();
        service.sweep().unwrap();
        assert!(
            !service
                .lock()
                .unwrap()
                .registry
                .entries
                .contains_key(&blocked_id)
        );
    }
    #[test]
    fn bad_pin_retirement_preserves_other_workspaces_and_failed_file_charges() {
        for fail_cleanup in [false, true] {
            let (root, db, prior) = fixture();
            drop(prior);
            let healthy = crate::cursor::lifecycle_tests::database()
                .canonicalize()
                .unwrap();
            crate::recovery::adopt(&db, 1).unwrap();
            let service = CursorService::new(&root, [db.clone(), healthy.clone()]).unwrap();
            // Startup's20ms probes may defer under concurrent Rust tests;
            // exercise the ordinary maintenance path before this fault case.
            service.sweep().unwrap();
            service.sweep().unwrap();
            assert!(service.lock().unwrap().cleanup_pending.is_empty());
            let bad_result = ready(&service, &db);
            let healthy_result = ready(&service, &healthy);
            let guard = crate::recovery::IndexReadGuard::open_unbudgeted(&db).unwrap();
            assert_eq!(bad_result.index_incarnation, guard.incarnation());
            let checkpoint = rusqlite::Connection::open(&db).unwrap();
            assert_eq!(
                checkpoint
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, i32>(2)?
                    )))
                    .unwrap(),
                (0, 0, 0)
            );
            drop(checkpoint);
            std::fs::write(&db, b"owned corrupt SQLite header").unwrap();
            let connection = rusqlite::Connection::open(&db).unwrap();
            let error = connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                .unwrap_err();
            assert!(matches!(
                guard.map_sqlite(error),
                Error::IndexCorrupt { .. }
            ));
            drop(connection);
            if fail_cleanup {
                std::fs::remove_file(&bad_result.result_path).unwrap();
                std::fs::create_dir(&bad_result.result_path).unwrap();
            }
            service
                .invalidate_corrupt(&db, guard.incarnation())
                .unwrap();
            assert!(matches!(
                call(&service, &db, action("borrow", Some(&bad_result))),
                Err(Error::CursorInvalidated)
            ));
            let borrowed =
                call(&service, &healthy, action("borrow", Some(&healthy_result))).unwrap();
            assert_eq!(std::fs::read(&borrowed.result_path).unwrap(), b"saved");
            call(&service, &healthy, action("finish", Some(&borrowed))).unwrap();
            let handle = CursorHandle::parse_prefix(&bad_result.cursor).unwrap();
            {
                let state = service.lock().unwrap();
                assert_eq!(
                    state.registry.entries.contains_key(&handle.id),
                    fail_cleanup
                );
                if fail_cleanup {
                    assert_eq!(state.registry.entries[&handle.id].result_bytes_reserved, 5);
                    assert!(state.registry.retirement_warning.is_some());
                }
            }
            if fail_cleanup {
                std::fs::remove_dir(&bad_result.result_path).unwrap();
                service.sweep().unwrap();
                assert!(
                    !service
                        .lock()
                        .unwrap()
                        .registry
                        .entries
                        .contains_key(&handle.id)
                );
            }
            assert!(
                db.exists(),
                "a legacy physical family was deleted while safety was unknown"
            );
        }
    }
    #[test]
    fn streamed_result_survives_publication_but_not_restart() {
        let (root, db, service) = fixture();
        let stored = ready(&service, &db);
        crate::cursor::lifecycle_tests::publish(&db, "new source");
        let borrowed = call(&service, &db, action("borrow", Some(&stored))).unwrap();
        assert_eq!(borrowed.generation, stored.generation);
        assert_eq!(std::fs::read(&borrowed.result_path).unwrap(), b"saved");
        let handle = CursorHandle::parse_prefix(&borrowed.cursor).unwrap();
        let pin = GenerationPin {
            database: db.clone(),
            owner: handle.owner,
            id: handle.id,
            generation: borrowed.generation,
            health: crate::recovery::IndexReadGuard::open_unbudgeted(&db).unwrap(),
        };
        let request = serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap();
        assert_eq!(
            pin.open_fixture_view()
                .unwrap()
                .query(request)
                .unwrap()
                .source
                .unwrap()
                .text,
            "original source"
        );
        call(&service, &db, action("finish", Some(&borrowed))).unwrap();
        service.stop();
        // Complete deferred owner activation before probing the private old pin.
        let replacement = CursorService::new(&root, [db.clone()]).unwrap();
        replacement.sweep().unwrap();
        assert!(!Path::new(&stored.result_path).exists());
        assert!(matches!(
            call(&replacement, &db, action("borrow", Some(&stored))),
            Err(Error::CursorInvalidated)
        ));
        assert!(matches!(
            pin.open_fixture_view(),
            Err(Error::CursorInvalidated)
        ));
    }
    #[test]
    fn repaired_startup_result_cache_recovers_without_deleting_foreign_entries() {
        let (root, db, old) = fixture();
        old.stop();
        let foreign = root.join("cursors/.DS_Store");
        std::fs::write(&foreign, b"owned test simulating foreign metadata").unwrap();
        let service = CursorService::new(&root, [db.clone()]).unwrap();
        assert!(service.lock().unwrap().file_error.is_some());
        service.sweep().unwrap();
        assert!(matches!(
            call(&service, &db, action("admit", None)),
            Err(Error::ResourceLimited(_))
        ));
        assert_eq!(
            std::fs::read(&foreign).unwrap(),
            b"owned test simulating foreign metadata"
        );
        // Simulate explicit owner repair; the service never removes this file.
        std::fs::remove_file(&foreign).unwrap();
        service.sweep().unwrap();
        assert!(service.lock().unwrap().file_error.is_none());
        let stored = ready(&service, &db);
        let borrowed = call(&service, &db, action("borrow", Some(&stored))).unwrap();
        assert_eq!(std::fs::read(&borrowed.result_path).unwrap(), b"saved");
        call(&service, &db, action("finish", Some(&borrowed))).unwrap();
    }
    #[test]
    fn failed_file_cleanup_keeps_reservation_pin_and_retries() {
        let (_, db, service) = fixture();
        let stored = ready(&service, &db);
        let path = Path::new(&stored.result_path);
        let moved = path.with_extension("preserved");
        std::fs::rename(path, &moved).unwrap();
        std::fs::create_dir(path).unwrap();
        let handle = CursorHandle::parse_prefix(&stored.cursor).unwrap();
        {
            let mut state = service.lock().unwrap();
            let execution = Execution::new(Default::default(), None).unwrap();
            assert!(state.registry.retire(handle.id, execution).is_err());
            let entry = &state.registry.entries[&handle.id];
            assert!(entry.retiring);
            assert_eq!(entry.result_bytes_reserved, 5);
            assert!(entry.pin.open_fixture_view().is_ok());
        }
        assert!(call(&service, &db, action("borrow", Some(&stored))).is_err());
        std::fs::remove_dir(path).unwrap();
        std::fs::rename(moved, path).unwrap();
        service.sweep().unwrap();
        assert!(service.lock().unwrap().registry.entries.is_empty());
        assert!(!path.exists());
    }
    #[test]
    fn abandoned_upload_and_unregister_retire_without_reusing_tokens() {
        let (_, db, service) = fixture();
        let lease = call(&service, &db, action("admit", None)).unwrap();
        call(
            &service,
            &db,
            CursorRequest {
                bytes: 9,
                ..action("reserve", Some(&lease))
            },
        )
        .unwrap();
        call(
            &service,
            &db,
            CursorRequest {
                chunk: b"short".into(),
                ..action("chunk", Some(&lease))
            },
        )
        .unwrap();
        let handle = CursorHandle::parse_prefix(&lease.cursor).unwrap();
        service
            .lock()
            .unwrap()
            .registry
            .entries
            .get_mut(&handle.id)
            .unwrap()
            .reads
            .values_mut()
            .for_each(|until| *until = Duration::ZERO);
        service.sweep().unwrap();
        assert!(service.lock().unwrap().registry.entries.is_empty());
        assert_eq!(
            std::fs::read_dir(service.root.join("cursors"))
                .unwrap()
                .count(),
            0
        );
        let stored = ready(&service, &db);
        let active = call(&service, &db, action("borrow", Some(&stored))).unwrap();
        service.unregister(&db).unwrap();
        assert!(matches!(
            call(&service, &db, action("admit", None)),
            Err(Error::CursorInvalidated)
        ));
        service.register(&db).unwrap();
        assert!(matches!(
            call(&service, &db, action("borrow", Some(&stored))),
            Err(Error::CursorInvalidated)
        ));
        assert!(Path::new(&active.result_path).exists());
        let handle = CursorHandle::parse_prefix(&active.cursor).unwrap();
        service
            .lock()
            .unwrap()
            .registry
            .entries
            .get_mut(&handle.id)
            .unwrap()
            .reads
            .clear();
        service.sweep().unwrap();
        assert!(!Path::new(&active.result_path).exists());
    }
    #[test]
    fn absolute_request_deadline_rejects_stale_work_before_mutation() {
        let (_, db, service) = fixture();
        let start = QueryStart::default();
        let request = CursorRequest {
            action: "admit".into(),
            remaining_ms: 5000,
            deadline_at_ms: 1,
            ..Default::default()
        };
        assert!(matches!(
            service.request(&db, request, start),
            Err(Error::DeadlineExceeded)
        ));
        assert!(service.lock().unwrap().registry.entries.is_empty());
        let execution = Execution::new(start, Some(0)).unwrap();
        assert!(matches!(
            service
                .lock()
                .unwrap()
                .registry
                .admit_controlled(&db, execution),
            Err(Error::DeadlineExceeded)
        ));
    }
    #[test]
    fn lowered_limits_remain_pending_then_evict_after_active_reads() {
        let (root, db, service) = fixture();
        let first = ready(&service, &db);
        let second = ready(&service, &db);
        let a = call(&service, &db, action("borrow", Some(&first))).unwrap();
        let b = call(&service, &db, action("borrow", Some(&second))).unwrap();
        let limits = CursorSettings {
            max_cursors: 1,
            retention_bytes: 1,
        };
        let reply = call(
            &service,
            &db,
            CursorRequest {
                settings: Some(limits),
                ..action("settings", None)
            },
        )
        .unwrap();
        assert!(reply.pending_pressure);
        assert!(
            call(&service, &db, action("settings", None))
                .unwrap()
                .pending_pressure
        );
        call(&service, &db, action("finish", Some(&a))).unwrap();
        call(&service, &db, action("finish", Some(&b))).unwrap();
        service.sweep().unwrap();
        assert!(service.lock().unwrap().registry.entries.is_empty());
        assert!(
            !call(&service, &db, action("settings", None))
                .unwrap()
                .pending_pressure
        );
        assert_eq!(CursorLimits::load(&root).unwrap().retention_bytes, 1);
    }
    #[test]
    fn failed_startup_pin_cleanup_is_retried_without_queries() {
        let (root, db, service) = fixture();
        let old = ready(&service, &db);
        service.stop();
        // Construct the successor while the index is temporarily unavailable;
        // the pending set must survive until ordinary periodic maintenance.
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        let replacement = CursorService::new(&root, [db.clone()]).unwrap();
        assert_eq!(replacement.lock().unwrap().cleanup_pending.len(), 1);
        let other = crate::cursor::lifecycle_tests::database()
            .canonicalize()
            .unwrap();
        assert!(matches!(
            call(&replacement, &other, action("admit", None)),
            Err(Error::ResourceLimited(_))
        ));
        // Hold a legitimate writer beyond the 20 ms startup probe. Periodic
        // retry must finish rather than repeatedly abandon the same work.
        let unlock = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            connection.execute_batch("ROLLBACK").unwrap();
        });
        replacement.sweep().unwrap();
        unlock.join().unwrap();
        assert!(replacement.lock().unwrap().cleanup_pending.is_empty());
        assert!(matches!(
            call(&replacement, &db, action("borrow", Some(&old))),
            Err(Error::CursorInvalidated)
        ));
    }
    fn wait_for_publication(service: &CursorService) {
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while service.lock().unwrap().pending_publications == 0 {
            assert!(
                std::time::Instant::now() < until,
                "publisher never reached the cursor gate"
            );
            std::thread::yield_now();
        }
    }
    fn wait_for_publication_fence(service: &CursorService) {
        // Ownership precedes the fence so CPU-throttled publishers still
        // permit reads. Pressure tests require the actual admission fence.
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while !service.lock().unwrap().registry.publication_waiting {
            assert!(
                std::time::Instant::now() < until,
                "publisher never entered the cursor admission fence"
            );
            std::thread::yield_now();
        }
    }
    #[test]
    fn cursor_mutex_wait_observes_deadline_without_acquiring_the_fence() {
        let (_, db, service) = fixture();
        let stored = ready(&service, &db);
        let held = service.lock().unwrap();
        std::thread::scope(|scope| {
            let (send, receive) = std::sync::mpsc::channel();
            let service = &service;
            let db = &db;
            let stored = &stored;
            scope.spawn(move || {
                let start = QueryStart::default();
                let execution = Execution::new(start, Some(20)).unwrap();
                let request = CursorRequest {
                    remaining_ms: 20,
                    deadline_at_ms: execution.deadline_ms().unwrap(),
                    ..action("borrow", Some(stored))
                };
                send.send(service.request(db, request, start)).unwrap();
            });
            assert!(matches!(
                receive.recv_timeout(Duration::from_secs(1)).unwrap(),
                Err(Error::DeadlineExceeded)
            ));
            drop(held);
        });
        let read = call(&service, &db, action("borrow", Some(&stored))).unwrap();
        call(&service, &db, action("finish", Some(&read))).unwrap();
    }
    #[test]
    fn cpu_paused_publication_leaves_pages_usable_and_shutdown_drains_writer() {
        let (_, db, service) = fixture();
        let stored = ready(&service, &db);
        let writer = crate::cursor::lifecycle_tests::staged(&db, "paused unpublished source");
        let control = Control::test_managed();
        control.test_pause(true);
        std::thread::scope(|scope| {
            let publisher = scope.spawn(|| service.publish(&db, writer, &control));
            wait_for_publication(&service);
            assert!(!service.lock().unwrap().registry.publication_waiting);
            let read = call(&service, &db, action("borrow", Some(&stored))).unwrap();
            call(&service, &db, action("finish", Some(&read))).unwrap();
            service.stop();
            assert!(matches!(
                publisher.join().unwrap(),
                Err(Error::CursorInvalidated)
            ));
        });
        let connection = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM staging_generations", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn publication_control_does_not_cpu_park_inside_cursor_fence() {
        let (_, db, service) = fixture();
        let control = Control::test_managed();
        let critical = control.publication();
        let outcome = service
            .publish_with(&db, None, &control, || {
                control.test_pause(true);
                critical.check()?;
                Ok(7)
            })
            .unwrap();
        assert_eq!(outcome, 7);
        // The clone must still stop for cancellation, even while paused.
        assert!(control.cancel());
        assert!(matches!(critical.check(), Err(Error::Cancelled)));
        assert!(service.lock().is_ok());
    }
    #[test]
    fn publication_unwind_drains_ownership_without_reusing_poisoned_state() {
        let (_, db, service) = fixture();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            service.publish_with::<()>(&db, None, &Control::default(), || {
                panic!("owned publication unwind")
            })
        }));
        assert!(result.is_err());
        assert!(matches!(service.lock(), Err(Error::Coordinator(_))));
        assert_eq!(
            service
                .state
                .lock()
                .err()
                .unwrap()
                .into_inner()
                .pending_publications,
            0
        );
        // Fatal coordinator teardown must not wait on a leaked counter.
        service.stop();
    }
    #[test]
    fn publication_waits_without_writer_transaction_then_evicts_idle_result() {
        let (_, db, service) = fixture();
        let stored = ready(&service, &db);
        let read = call(&service, &db, action("borrow", Some(&stored))).unwrap();
        service.lock().unwrap().registry.maximum_bytes = 1;
        let writer = crate::cursor::lifecycle_tests::staged(&db, "replacement source");
        let next_generation = writer.generation;
        std::thread::scope(|scope| {
            let publish = scope.spawn(|| service.publish(&db, writer, &Control::default()));
            wait_for_publication_fence(&service);
            let denied = call(&service, &db, action("borrow", Some(&stored)));
            assert!(
                matches!(&denied, Err(Error::ResourceLimited(_))),
                "unexpected cursor admission result: {denied:?}"
            );
            // A competing writer can acquire IMMEDIATE while publication waits.
            let connection = rusqlite::Connection::open(&db).unwrap();
            connection.busy_timeout(Duration::from_millis(100)).unwrap();
            connection
                .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
                .unwrap();
            assert!(Path::new(&stored.result_path).exists());
            call(&service, &db, action("finish", Some(&read))).unwrap();
            assert_eq!(publish.join().unwrap().unwrap(), next_generation);
        });
        assert!(!Path::new(&stored.result_path).exists());
        assert!(service.lock().unwrap().registry.entries.is_empty());
        let request = serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap();
        let result = crate::query::fixture_view(&db)
            .unwrap()
            .query(request)
            .unwrap();
        assert_eq!(result.generation, next_generation);
        assert_eq!(result.source.unwrap().text, "replacement source");
    }
    #[test]
    fn stop_drains_waiting_publication_and_its_staging_before_returning() {
        let (_, db, service) = fixture();
        let stored = ready(&service, &db);
        let _read = call(&service, &db, action("borrow", Some(&stored))).unwrap();
        service.lock().unwrap().registry.maximum_bytes = 1;
        let writer = crate::cursor::lifecycle_tests::staged(&db, "must not publish");
        std::thread::scope(|scope| {
            let publish = scope.spawn(|| service.publish(&db, writer, &Control::default()));
            wait_for_publication(&service);
            service.stop();
            assert_eq!(service.lock().unwrap().pending_publications, 0);
            let connection = rusqlite::Connection::open(&db).unwrap();
            let staged: i64 = connection
                .query_row("SELECT count(*) FROM staging_generations", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(staged, 0);
            assert!(matches!(
                publish.join().unwrap(),
                Err(Error::CursorInvalidated)
            ));
        });
        let connection = rusqlite::Connection::open(&db).unwrap();
        let generation: u64 = connection
            .query_row("SELECT current_generation FROM state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(generation, stored.generation);
    }
}
