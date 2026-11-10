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

pub const CONTROL_VERSION: u32 = 1;

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
    initialized: BTreeSet<PathBuf>,
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
impl CursorService {
    pub fn new(root: &Path, databases: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let owner = nonce()?;
        let limits = CursorLimits::load(root)?;
        let mut initialized = BTreeSet::new();
        let mut cleanup_pending = BTreeMap::new();
        let startup = Execution::new(Default::default(), Some(100))?;
        // Retire previous-owner pins even if result-cache cleanup later fails.
        // A damaged cache must not retain obsolete index generations forever.
        for database in databases {
            if database.is_file() {
                let activated = startup
                    .phase(Duration::from_millis(20))
                    .and_then(|execution| {
                        GenerationRetention::activate_controlled(&database, owner, Some(execution))
                    });
                match activated {
                    Ok(_) => {
                        initialized.insert(database);
                    }
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
        let file_error = match result_store::ResultStore::open(&root.join("cursors")) {
            Ok(mut files) => match files.reset() {
                Ok(()) => {
                    registry.files = Some(files);
                    None
                }
                Err(error) => Some(error.to_string()),
            },
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
        if !state.initialized.contains(database) {
            GenerationRetention::activate_controlled(
                database,
                state.registry.owner,
                Some(execution),
            )?;
            state.initialized.insert(database.into());
            state.cleanup_pending.remove(database);
        }
        Ok(())
    }
    /// Fence tokens before the host removes a registration. Existing bounded
    /// reads retain their pins until they finish or their leases expire.
    pub fn unregister(&self, database: &Path) -> Result<()> {
        let mut state = self.lock()?;
        state.disabled.insert(database.into());
        state.initialized.remove(database);
        for entry in state.registry.entries.values_mut() {
            if entry.pin.database == database {
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
        let mut state = self.lock()?;
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
                "cursor result storage unavailable: {error}; repair the cursor cache permissions/contents and restart the coordinator"
            )));
        }
        Self::initialize(&mut state, database, execution.clone())?;
        if request.action == "admit" {
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
        let read = Self::read(&state.registry, database, handle, &request.lease)?;
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
    ) -> Result<CursorRead> {
        let entry = registry
            .entries
            .get(&handle.id)
            .ok_or(Error::CursorInvalidated)?;
        let now = crate::execution::continuous_time()?;
        let id = unhex::<16>(lease)?;
        let until = *entry.reads.get(&id).ok_or(Error::CursorInvalidated)?;
        if entry.pin.database != database || entry.retiring || entry.expired(now) || until <= now {
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
        let mut reply = CursorReply {
            cursor: read.handle.prefix(),
            lease: hexadecimal(&read.id),
            owner: read.pin.owner.to_vec(),
            pin: read.pin.id.to_vec(),
            generation: read.pin.generation,
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
            if let Err(error) = Self::initialize(&mut state, &database, execution) {
                state
                    .cleanup_pending
                    .insert(database.clone(), error.to_string());
                state.cleanup_queue.push_back(database);
            }
        }
        state.maintenance_error = result.as_ref().err().map(ToString::to_string);
        self.changed.notify_all();
        result
    }
    /// Analysis has completed before entry. Keep admission serialized through
    /// actual commit, but release the mutex while bounded active reads finish.
    pub fn publish(&self, database: &Path, writer: Writer, control: &Control) -> Result<u64> {
        let execution =
            Execution::new(Default::default(), Some(crate::execution::MAX_DEADLINE_MS))?;
        {
            let mut state = self.lock()?;
            self.running()?;
            state.pending_publications += 1;
            state.registry.set_publication_waiting(true);
        }
        let result = (|| {
            let mut state = self.lock()?;
            Self::initialize(&mut state, database, execution.clone())?;
            loop {
                self.running()?;
                if state.disabled.contains(database) {
                    return Err(Error::CursorInvalidated);
                }
                control.check()?;
                execution.check()?;
                if state.registry.prepare_controlled(
                    database,
                    writer.generation,
                    execution.clone(),
                )? {
                    return writer.publish_controlled(control);
                }
                let (next, _) = self
                    .changed
                    .wait_timeout(state, Duration::from_millis(20))
                    .map_err(|_| Error::Coordinator("cursor publication wait failed".into()))?;
                state = next;
            }
        })();
        if let Ok(mut state) = self.lock() {
            state.pending_publications -= 1;
            let pending = state.pending_publications > 0;
            state.registry.set_publication_waiting(pending);
        }
        self.changed.notify_all();
        result
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
        };
        let request = serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap();
        assert_eq!(
            pin.open_view()
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
        let replacement = CursorService::new(&root, [db.clone()]).unwrap();
        assert!(!Path::new(&stored.result_path).exists());
        assert!(matches!(
            call(&replacement, &db, action("borrow", Some(&stored))),
            Err(Error::CursorInvalidated)
        ));
        assert!(matches!(pin.open_view(), Err(Error::CursorInvalidated)));
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
            assert!(entry.pin.open_view().is_ok());
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
            wait_for_publication(&service);
            assert!(matches!(
                call(&service, &db, action("borrow", Some(&stored))),
                Err(Error::ResourceLimited(_))
            ));
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
        let result = ReadView::open(&db).unwrap().query(request).unwrap();
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
