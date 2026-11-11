//! Physical index lifetimes. The coordinator selects; readers only fence BAD.
//!
//! State changes are serialized independently from SQLite. A writer must reserve
//! its generation while holding the logical writer lock, before publication.
use crate::{
    Error, Result,
    execution::Execution,
    resource::{Control, Limits},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub const VERSION: u32 = 2;
const PAGE: usize = 4096;
const MAGIC: &[u8; 8] = b"KCIDX001";
const MAX_STATE: u64 = 256 * 1024;
const MAX_INCARNATIONS: usize = 256;
#[cfg(test)]
thread_local! {
    static FAIL_NEXT_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Incarnation {
    id: String,
    legacy: bool,
    file_identity: Option<(u64, u64)>,
}
/// A replacement cause is durable evidence, not permission to reinterpret an
/// arbitrary version mismatch as corruption or a supported upgrade.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RebuildReason {
    #[default]
    Corruption,
    Upgrade {
        source_format: i32,
        target_format: i32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    index: Incarnation,
    reason: RebuildReason,
    minimum_generation: u64,
    ready_generation: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    #[serde(default)]
    initializing: bool,
    selected: Incarnation,
    high_water: u64,
    selected_generation: Option<u64>,
    pending: Option<Pending>,
    retired: Vec<Incarnation>,
    failed: Option<String>,
    failed_reason: Option<RebuildReason>,
}
#[derive(Clone, Debug)]
pub struct Candidate {
    pub database: PathBuf,
    pub incarnation: String,
    pub minimum_generation: u64,
    pub reason: RebuildReason,
    logical: PathBuf,
}

/// Verified, durable candidate receipt. Its fields are private so the short
/// publication phase can consume only the exact state prepared beforehand.
#[derive(Debug)]
pub struct PreparedRebuild {
    candidate: Candidate,
    generation: u64,
    source_incarnation: String,
    file_identity: (u64, u64),
}
impl PreparedRebuild {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidRequest(message.into())
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn parent(logical: &Path) -> Result<&Path> {
    logical
        .parent()
        .ok_or_else(|| invalid("index has no parent"))
}
fn state_path(logical: &Path) -> PathBuf {
    logical.with_extension("lifecycle.json")
}
fn inc_dir(logical: &Path, id: &str) -> Result<PathBuf> {
    if !valid_id(id) {
        return Err(invalid("invalid index incarnation"));
    }
    Ok(parent(logical)?.join("indexes").join(id))
}
fn physical(logical: &Path, index: &Incarnation) -> Result<PathBuf> {
    Ok(if index.legacy {
        logical.to_owned()
    } else {
        inc_dir(logical, &index.id)?.join("index.db")
    })
}
fn check_regular(path: &Path, meta: &fs::Metadata) -> Result<()> {
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } {
        return Err(invalid(format!(
            "index state is not an owned regular file: {}",
            path.display()
        )));
    }
    Ok(())
}
fn regular(path: &Path) -> Result<()> {
    check_regular(path, &fs::symlink_metadata(path)?)
}
fn file_identity(path: &Path) -> Result<Option<(u64, u64)>> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            check_regular(path, &m)?;
            Ok(Some((m.dev(), m.ino())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn validate_identity(path: &Path, index: &Incarnation) -> Result<()> {
    let actual = file_identity(path)?;
    if index.file_identity.is_some() && actual.is_some() && actual != index.file_identity {
        return Err(invalid(
            "index file identity changed outside the coordinator",
        ));
    }
    Ok(())
}
fn directory(path: &Path, create: bool) -> Result<()> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(invalid(
            "index state directory is not owned or is a symlink",
        ));
    }
    Ok(())
}
fn open_file(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    check_regular(path, &file.metadata()?)?;
    Ok(file)
}
fn load(logical: &Path) -> Result<Option<State>> {
    let path = state_path(logical);
    let mut file = match open_file(&path, false) {
        Ok(f) => f,
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if file.metadata()?.len() > MAX_STATE {
        return Err(invalid("index lifecycle state exceeds bound"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_STATE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE {
        return Err(invalid("index lifecycle state exceeds bound"));
    }
    // Inspect the envelope before strict payload decoding. A future receipt
    // may introduce unknown fields; it must remain an update-required outcome.
    let mut payload: serde_json::Value = serde_json::from_slice(&bytes)?;
    let version = payload
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid("missing or invalid index lifecycle version"))?;
    if version != 1 && version != u64::from(VERSION) {
        return Err(Error::UpdateRequired(
            "unsupported index lifecycle state".into(),
        ));
    }
    if version == 1 {
        if payload.get("failed_reason").is_some()
            || payload
                .get("pending")
                .is_some_and(|p| p.get("reason").is_some())
        {
            return Err(invalid(
                "version1 lifecycle state cannot contain version2 replacement evidence",
            ));
        }
        if let Some(pending) = payload
            .get_mut("pending")
            .and_then(serde_json::Value::as_object_mut)
        {
            pending.insert(
                "reason".into(),
                serde_json::to_value(RebuildReason::Corruption)?,
            );
        }
        if payload.get("failed").is_some_and(|value| !value.is_null()) {
            payload
                .as_object_mut()
                .unwrap()
                .entry("failed_reason")
                .or_insert(serde_json::to_value(RebuildReason::Corruption)?);
        }
    }
    let state: State = serde_json::from_value(payload)?;
    if state.failed.is_some() != state.failed_reason.is_some() {
        return Err(invalid("index failure receipt is missing its cause"));
    }
    for reason in state
        .pending
        .iter()
        .map(|p| p.reason)
        .chain(state.failed_reason)
    {
        validate_reason(reason)?;
    }
    let mut ids = BTreeSet::new();
    let mut legacy = 0;
    for i in std::iter::once(&state.selected)
        .chain(&state.retired)
        .chain(state.pending.iter().map(|p| &p.index))
    {
        if !valid_id(&i.id) || !ids.insert(&i.id) {
            return Err(invalid("invalid or duplicate index incarnation"));
        }
        legacy += usize::from(i.legacy);
    }
    if legacy > 1
        || state.retired.len() > MAX_INCARNATIONS
        || state.pending.as_ref().is_some_and(|p| p.index.legacy)
    {
        return Err(invalid("invalid index lifecycle state"));
    }
    Ok(Some(state))
}
fn lock(logical: &Path) -> Result<File> {
    directory(parent(logical)?, false)?;
    let file = open_file(&logical.with_extension("lifecycle.lock"), true)?;
    file.lock()?;
    Ok(file)
}
fn save(logical: &Path, state: &State) -> Result<()> {
    // Reading legacy state must not pretend it was rewritten. Every actual
    // durable mutation deliberately advances the lifecycle contract.
    let state = State {
        version: VERSION,
        ..state.clone()
    };
    let bytes = serde_json::to_vec(&state)?;
    if bytes.len() as u64 > MAX_STATE {
        return Err(invalid("index lifecycle state exceeds bound"));
    }
    let path = state_path(logical);
    let next = logical.with_extension("lifecycle.next");
    let mut file = open_file(&next, true)?;
    file.set_len(0)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(next, path)?;
    #[cfg(test)]
    if FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(
            std::io::Error::other("injected directory sync failure after selector rename").into(),
        );
    }
    File::open(parent(logical)?)?.sync_all()?;
    Ok(())
}
fn nonce() -> Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn create_incarnation(logical: &Path, index: &Incarnation) -> Result<()> {
    let base = parent(logical)?.join("indexes");
    directory(&base, true)?;
    let dir = inc_dir(logical, &index.id)?;
    fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let mut health = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("health"))?;
    let mut bytes = [0; PAGE];
    bytes[..8].copy_from_slice(MAGIC);
    health.write_all(&bytes)?;
    health.sync_all()?;
    open_file(&dir.join("readers.lock"), true)?.sync_all()?;
    File::open(&dir)?.sync_all()?;
    File::open(base)?.sync_all()?;
    Ok(())
}

struct Lease {
    mapping: NonNull<u8>,
    _health: File,
    _reader: File,
}
// The mapped atomic is naturally aligned and is the only mutable location.
unsafe impl Send for Lease {}
unsafe impl Sync for Lease {}
impl Lease {
    fn bad(&self) -> &AtomicU64 {
        unsafe { &*self.mapping.as_ptr().add(16).cast::<AtomicU64>() }
    }
    fn mark(&self) -> Result<()> {
        self.bad().store(1, Ordering::Release);
        if unsafe { libc::msync(self.mapping.as_ptr().cast(), PAGE, libc::MS_SYNC) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        self._health.sync_all()?;
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.mapping.as_ptr().cast(), PAGE);
        }
    }
}
fn lease(logical: &Path, index: &Incarnation) -> Result<Arc<Lease>> {
    let dir = inc_dir(logical, &index.id)?;
    directory(parent(logical)?, false)?;
    directory(dir.parent().unwrap(), false)?;
    directory(&dir, false)?;
    let reader = open_file(&dir.join("readers.lock"), false)?;
    match reader.try_lock_shared() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(Error::IndexRecovering(
                "index incarnation is being retired; restart the query".into(),
            ));
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    let mut health = open_file(&dir.join("health"), false)?;
    if health.metadata()?.len() != PAGE as u64 {
        return Err(invalid("invalid index health page size"));
    }
    let mut magic = [0; 8];
    health.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid("invalid index health page"));
    }
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            PAGE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            health.as_raw_fd(),
            0,
        )
    };
    if raw == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(Arc::new(Lease {
        mapping: NonNull::new(raw.cast()).unwrap(),
        _health: health,
        _reader: reader,
    }))
}

/// The clone retains a shared cross-process reader lease and mapped BAD latch.
#[derive(Clone)]
pub struct IndexReadGuard {
    logical: PathBuf,
    physical: PathBuf,
    incarnation: String,
    lease: Option<Arc<Lease>>,
}
impl IndexReadGuard {
    pub(crate) fn open(logical: &Path, execution: Option<Execution>) -> Result<Self> {
        if let Some(e) = &execution {
            e.check()?;
        }
        let guard = Self::open_unbudgeted(logical)?;
        if let Some(e) = &execution {
            e.check()?;
        }
        Ok(guard)
    }
    pub fn open_unbudgeted(logical: &Path) -> Result<Self> {
        match load(logical)? {
            Some(state) if state.initializing => Err(Error::IndexRecovering(
                "index adoption is not durably initialized".into(),
            )),
            Some(state) => Self::from_index(logical, &state.selected),
            None => {
                // Registered managed paths must be adopted before consumers open.
                if crate::control::root_for_database(logical)
                    .is_some_and(|r| r.join("registry.db").exists())
                {
                    return Err(Error::IndexRecovering(
                        "coordinator has not adopted this workspace index".into(),
                    ));
                }
                Ok(Self {
                    logical: logical.into(),
                    physical: logical.into(),
                    incarnation: String::new(),
                    lease: None,
                })
            }
        }
    }
    pub(crate) fn open_incarnation(logical: &Path, id: &str, execution: Execution) -> Result<Self> {
        execution.check()?;
        let guard = Self::open_incarnation_unbudgeted(logical, id)?;
        execution.check()?;
        Ok(guard)
    }
    pub fn open_incarnation_unbudgeted(logical: &Path, id: &str) -> Result<Self> {
        let state = load(logical)?.ok_or(Error::CursorInvalidated)?;
        if state.initializing {
            return Err(Error::IndexRecovering(
                "index adoption is incomplete".into(),
            ));
        }
        let index = std::iter::once(&state.selected)
            .chain(&state.retired)
            .find(|i| i.id == id)
            .ok_or(Error::CursorInvalidated)?;
        Self::from_index(logical, index)
    }
    fn from_index(logical: &Path, index: &Incarnation) -> Result<Self> {
        let lease = lease(logical, index)?;
        let path = physical(logical, index)?;
        let guard = Self {
            logical: logical.into(),
            physical: path,
            incarnation: index.id.clone(),
            lease: Some(lease),
        };
        guard.check()?;
        validate_identity(&guard.physical, index)?;
        if index.file_identity.is_some() {
            let missing_or_empty = match fs::symlink_metadata(&guard.physical) {
                Ok(metadata) => metadata.len() == 0,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => return Err(error.into()),
            };
            if missing_or_empty {
                return guard
                    .note_corruption(
                        "owned index file is missing or empty; replacement is required",
                    )
                    .and(Ok(guard));
            }
        }
        Ok(guard)
    }
    pub fn logical_path(&self) -> &Path {
        &self.logical
    }
    pub fn physical_path(&self) -> &Path {
        &self.physical
    }
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub fn check(&self) -> Result<()> {
        if self
            .lease
            .as_ref()
            .is_some_and(|l| l.bad().load(Ordering::Acquire) != 0)
        {
            return Err(self.corrupt("index incarnation has been fenced as corrupt"));
        }
        Ok(())
    }
    fn corrupt(&self, message: impl Into<String>) -> Error {
        Error::IndexCorrupt {
            database: self.logical.clone(),
            incarnation: self.incarnation.clone(),
            message: message.into(),
        }
    }
    /// Used only after actual index SQLite corruption or a completed integrity check.
    pub fn note_corruption(&self, message: &str) -> Result<()> {
        let message = match self.lease.as_ref().map(|l| l.mark()) {
            Some(Err(e)) => format!("{message}; persisting BAD latch failed: {e}"),
            _ => message.into(),
        };
        Err(self.corrupt(message))
    }
    pub fn map_sqlite(&self, error: rusqlite::Error) -> Error {
        if sqlite_corruption(&error) && self.lease.is_some() {
            let message = error.to_string();
            match self.note_corruption(&message) {
                Err(e) => e,
                Ok(()) => unreachable!(),
            }
        } else {
            Error::Sqlite(error)
        }
    }
}
pub fn sqlite_corruption(error: &rusqlite::Error) -> bool {
    matches!(error,rusqlite::Error::SqliteFailure(code,_) if matches!(code.code,rusqlite::ErrorCode::DatabaseCorrupt|rusqlite::ErrorCode::NotADatabase))
}

/// Coordinator-only adoption; never moves the legacy SQLite/WAL family.
pub fn adopt(logical: &Path, trusted_generation: u64) -> Result<()> {
    let _lock = lock(logical)?;
    let mut state = match load(logical)? {
        Some(state) => state,
        None => {
            let identity = file_identity(logical)?;
            let legacy =
                identity.is_some() || family_bytes(logical)? != 0 || trusted_generation != 0;
            let state = State {
                version: VERSION,
                initializing: true,
                selected: Incarnation {
                    id: nonce()?,
                    legacy,
                    file_identity: identity,
                },
                high_water: trusted_generation,
                selected_generation: None,
                pending: None,
                retired: vec![],
                failed: None,
                failed_reason: None,
            };
            // Reference the nonce before creating anything. Restart can finish
            // this exact owned setup; it must never allocate another orphan.
            save(logical, &state)?;
            state
        }
    };
    if state.initializing {
        let base = parent(logical)?.join("indexes");
        directory(&base, true)?;
        let dir = inc_dir(logical, &state.selected.id)?;
        directory(&dir, true)?;
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_name() != "health" && entry.file_name() != "readers.lock" {
                return Err(invalid("foreign file in initializing index directory"));
            }
            regular(&entry.path())?;
        }
        let mut health = open_file(&dir.join("health"), true)?;
        let length = health.metadata()?.len();
        if length > PAGE as u64 {
            return Err(invalid("invalid initializing health page size"));
        }
        let mut current = Vec::new();
        health.read_to_end(&mut current)?;
        let mut expected = [0u8; PAGE];
        expected[..8].copy_from_slice(MAGIC);
        if length < PAGE as u64 {
            if current != expected[..current.len()] {
                return Err(invalid("invalid partial index health page"));
            }
            health.write_all(&expected[current.len()..])?;
        } else if &current[..8] != MAGIC {
            return Err(invalid("invalid initializing health page"));
        }
        health.sync_all()?;
        open_file(&dir.join("readers.lock"), true)?.sync_all()?;
        File::open(&dir)?.sync_all()?;
        File::open(&base)?.sync_all()?;
        state.initializing = false;
        save(logical, &state)?;
    }
    Ok(())
}
/// Called under the logical writer lock BEFORE a generation can be published.
pub fn reserve_generation(logical: &Path, generation: u64) -> Result<()> {
    if load(logical)?.is_none() {
        return Ok(());
    }
    let _lock = lock(logical)?;
    if let Some(mut state) = load(logical)? {
        let index = state
            .pending
            .as_mut()
            .map_or(&mut state.selected, |p| &mut p.index);
        let physical = physical(logical, index)?;
        validate_identity(&physical, index)?;
        let identity = file_identity(&physical)?;
        if generation > state.high_water || index.file_identity != identity {
            index.file_identity = identity;
            state.high_water = state.high_water.max(generation);
            save(logical, &state)?;
        }
    }
    Ok(())
}
pub fn lifecycle_version(logical: &Path) -> Result<Option<u32>> {
    Ok(load(logical)?.map(|s| s.version))
}
pub fn selected_path(logical: &Path) -> Result<PathBuf> {
    Ok(match load(logical)? {
        Some(s) => physical(logical, &s.selected)?,
        None => logical.into(),
    })
}
pub fn generation_floor(logical: &Path, registry_generation: u64) -> Result<u64> {
    let high = load(logical)?.map_or(registry_generation, |s| {
        s.high_water.max(registry_generation)
    });
    high.checked_add(1)
        .ok_or_else(|| invalid("index generation exhausted"))
}
pub fn failure(logical: &Path) -> Result<Option<String>> {
    Ok(load(logical)?.and_then(|s| s.failed))
}
pub fn replacement_reason(logical: &Path) -> Result<Option<RebuildReason>> {
    Ok(load(logical)?.and_then(|s| s.pending.map(|p| p.reason).or(s.failed_reason)))
}
pub fn failure_reason(logical: &Path) -> Result<Option<RebuildReason>> {
    Ok(load(logical)?.and_then(|s| s.failed_reason))
}
/// Probe an adopted selection without migrating it or reading an unknown
/// layout. Unknown versions are returned with generation zero for the closed
/// store catalog to reject. No published generation means not ready.
pub fn selected_format(logical: &Path) -> Result<Option<(i32, u64)>> {
    let Some(state) = load(logical)? else {
        return Err(invalid(
            "index format probing requires an adopted workspace",
        ));
    };
    if state.initializing {
        return Err(Error::IndexRecovering(
            "index adoption is not initialized".into(),
        ));
    }
    probe_format(&IndexReadGuard::from_index(logical, &state.selected)?)
}
fn probe_format(guard: &IndexReadGuard) -> Result<Option<(i32, u64)>> {
    match fs::symlink_metadata(guard.physical_path()) {
        Ok(meta) if meta.len() == 0 => return Ok(None),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let result = (|| -> Result<Option<(i32, u64)>> {
        guard.check()?;
        let connection = rusqlite::Connection::open_with_flags(
            guard.physical_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_millis(100))?;
        connection.execute_batch("BEGIN DEFERRED;")?;
        let format: i32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if !(1..=crate::store::SCHEMA_VERSION).contains(&format) {
            guard.check()?;
            return Ok(Some((format, 0)));
        }
        let generation: Option<u64> =
            connection.query_row("SELECT current_generation FROM state", [], |r| r.get(0))?;
        guard.check()?;
        Ok(match generation {
            Some(generation) => Some((format, generation)),
            None if matches!(
                crate::store::upgrade_action(format),
                crate::store::UpgradeAction::Rebuild { .. }
            ) =>
            {
                Some((format, 0))
            }
            None => None,
        })
    })();
    crate::index_result(guard, result)
}
fn validate_reason(reason: RebuildReason) -> Result<()> {
    if let RebuildReason::Upgrade {
        source_format,
        target_format,
    } = reason
        && (target_format != crate::store::SCHEMA_VERSION
            || !matches!(
                crate::store::upgrade_action(source_format),
                crate::store::UpgradeAction::Rebuild { .. }
            ))
    {
        return Err(Error::UpdateRequired(format!(
            "index upgrade {source_format} to {target_format} is unsupported by this runtime"
        )));
    }
    Ok(())
}
pub fn blocked(logical: &Path) -> Result<bool> {
    Ok(load(logical)?.is_some_and(|s| s.failed.is_some() || s.pending.is_some()))
}
pub fn has_retired(logical: &Path) -> Result<bool> {
    Ok(load(logical)?.is_some_and(|s| !s.retired.is_empty()))
}
pub fn selected_replacement_generation(logical: &Path) -> Result<Option<u64>> {
    Ok(load(logical)?.and_then(|s| s.selected_generation))
}
pub fn selected_incarnation(logical: &Path) -> Result<Option<String>> {
    Ok(load(logical)?.map(|s| s.selected.id))
}
pub fn is_bad(logical: &Path) -> Result<bool> {
    let Some(state) = load(logical)? else {
        return Ok(false);
    };
    Ok(lease(logical, &state.selected)?
        .bad()
        .load(Ordering::Acquire)
        != 0)
}
/// The control report is a wakeup, not authority to mark a healthy index BAD.
pub fn report_corruption(logical: &Path, id: &str) -> Result<bool> {
    let _lock = lock(logical)?;
    let Some(state) = load(logical)? else {
        return Ok(false);
    };
    if state.selected.id != id {
        return Ok(false);
    }
    let latch = lease(logical, &state.selected)?;
    if latch.bad().load(Ordering::Acquire) == 0 {
        return Ok(false);
    }
    latch.mark()?;
    Ok(true)
}
pub fn begin_rebuild(logical: &Path, explicit: bool, trusted_generation: u64) -> Result<Candidate> {
    begin_replacement(
        logical,
        explicit,
        trusted_generation,
        RebuildReason::Corruption,
    )
}
pub fn begin_upgrade(
    logical: &Path,
    explicit: bool,
    trusted_generation: u64,
    source_format: i32,
) -> Result<Candidate> {
    let reason = RebuildReason::Upgrade {
        source_format,
        target_format: crate::store::SCHEMA_VERSION,
    };
    validate_reason(reason)?;
    // Do not create lifecycle artifacts for an unmanaged caller path.
    if load(logical)?.is_none() {
        return Err(invalid("index upgrades require an adopted workspace"));
    }
    begin_replacement(logical, explicit, trusted_generation, reason)
}
fn begin_replacement(
    logical: &Path,
    explicit: bool,
    trusted_generation: u64,
    reason: RebuildReason,
) -> Result<Candidate> {
    let _lock = lock(logical)?;
    let mut state = load(logical)?.ok_or_else(|| invalid("workspace index is not adopted"))?;
    if state.pending.is_some() || (state.failed.is_some() && !explicit) {
        return Err(Error::IndexRecovering(
            "automatic recovery already attempted; explicit refresh is required".into(),
        ));
    }
    if state.initializing {
        return Err(Error::IndexRecovering(
            "index adoption is not initialized".into(),
        ));
    }
    let actual_generation = match reason {
        RebuildReason::Corruption => {
            if lease(logical, &state.selected)?
                .bad()
                .load(Ordering::Acquire)
                == 0
            {
                return Err(invalid("recovery requires a fenced index"));
            }
            0
        }
        RebuildReason::Upgrade { source_format, .. } => {
            let guard = IndexReadGuard::from_index(logical, &state.selected)?;
            let (actual_format, generation) = probe_format(&guard)?.ok_or(Error::NotReady)?;
            if actual_format != source_format {
                return Err(Error::UpdateRequired(format!(
                    "selected index format {actual_format} does not match requested upgrade source {source_format}"
                )));
            }
            generation
        }
    };
    // An authorized source probe can observe a publication ahead of the
    // registry acknowledgement. Preserve that known floor with this attempt,
    // even if resources/setup fail before a candidate Writer reserves a value.
    state.high_water = state
        .high_water
        .max(trusted_generation)
        .max(actual_generation);
    if state.retired.len() >= MAX_INCARNATIONS {
        let message = "retained index incarnation count reached limit; cleanup is required";
        state.failed = Some(message.into());
        state.failed_reason = Some(reason);
        save(logical, &state)?;
        return Err(Error::ResourceLimited(message.into()));
    }
    let minimum_generation = state
        .high_water
        .checked_add(1)
        .ok_or_else(|| invalid("index generation exhausted"))?;
    let index = Incarnation {
        id: nonce()?,
        legacy: false,
        file_identity: None,
    };
    let candidate = Candidate {
        database: physical(logical, &index)?,
        incarnation: index.id.clone(),
        minimum_generation,
        reason,
        logical: logical.into(),
    };
    state.pending = Some(Pending {
        index: index.clone(),
        reason,
        minimum_generation,
        ready_generation: None,
    });
    state.failed = None;
    state.failed_reason = None;
    let started = (|| -> Result<()> {
        save(logical, &state)?;
        create_incarnation(logical, &index)
    })();
    if let Err(error) = started {
        // A selector rename may be visible even when its directory sync failed.
        // Still under the lifecycle lock, settle only this exact owned receipt;
        // a refusal before publication must never rewrite an older failure.
        if let Err(recording) = fail_owned_start(&candidate, &error.to_string()) {
            return Err(Error::Remote {
                code: error.code().into(),
                message: format!(
                    "{error}; replacement failure receipt could not be recorded: {recording}; explicit refresh is required"
                ),
            });
        }
        return Err(error);
    }
    Ok(candidate)
}
fn fail_owned_start(candidate: &Candidate, message: &str) -> Result<()> {
    let Some(mut state) = load(&candidate.logical)? else {
        return Ok(());
    };
    let Some(pending) = state
        .pending
        .as_ref()
        .filter(|p| p.index.id == candidate.incarnation)
    else {
        return Ok(());
    };
    check_candidate_receipt(candidate, pending)?;
    let pending = state.pending.take().unwrap();
    state.failed = Some(message.chars().take(4096).collect());
    state.failed_reason = Some(pending.reason);
    state.retired.push(pending.index);
    save(&candidate.logical, &state)
}
pub fn fail_rebuild(logical: &Path, message: &str) -> Result<()> {
    let _lock = lock(logical)?;
    let mut state = load(logical)?.ok_or_else(|| invalid("missing lifecycle state"))?;
    if let Some(p) = state.pending.take() {
        state.failed_reason = Some(p.reason);
        state.retired.push(p.index);
    } else if state.failed_reason.is_none() {
        // Preserve the corruption wrapper's pre-candidate failure path only
        // when the selected durable latch proves its cause.
        if lease(logical, &state.selected)?
            .bad()
            .load(Ordering::Acquire)
            == 0
        {
            return Err(invalid("no replacement attempt exists to fail"));
        }
        state.failed_reason = Some(RebuildReason::Corruption);
    }
    state.failed = Some(message.chars().take(4096).collect());
    save(logical, &state)
}
/// A crash before selector publication consumes the attempt, including a crash
/// after a verified ready receipt. Startup never runs unbudgeted cutover work.
pub fn reconcile(logical: &Path) -> Result<()> {
    if load(logical)?.is_some_and(|s| s.pending.is_some()) {
        fail_rebuild(
            logical,
            "recovery interrupted before the replacement was selected; explicit refresh is required",
        )?;
    }
    Ok(())
}
fn check_candidate_receipt(candidate: &Candidate, pending: &Pending) -> Result<()> {
    if pending.index.id != candidate.incarnation
        || pending.reason != candidate.reason
        || pending.minimum_generation != candidate.minimum_generation
        || physical(&candidate.logical, &pending.index)? != candidate.database
    {
        return Err(invalid("replacement does not match its durable receipt"));
    }
    Ok(())
}
fn validate_candidate(candidate: &Candidate, control: &Control) -> Result<u64> {
    control.check()?;
    let state = load(&candidate.logical)?.ok_or_else(|| invalid("missing lifecycle state"))?;
    let pending = state
        .pending
        .as_ref()
        .ok_or_else(|| invalid("replacement is no longer pending"))?;
    check_candidate_receipt(candidate, pending)?;
    validate_identity(&candidate.database, &pending.index)?;
    regular(&candidate.database)?;
    let mut connection = rusqlite::Connection::open_with_flags(
        &candidate.database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    connection.progress_handler(4096, Some(move || std::time::Instant::now() >= deadline))?;
    control
        .interruptible(connection.get_interrupt_handle(), || {
            let format: i32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if format != crate::store::SCHEMA_VERSION {
                return Err(Error::UpdateRequired(format!(
                    "replacement format {format} does not match target {}",
                    crate::store::SCHEMA_VERSION
                )));
            }
            let result: String =
                connection.query_row("PRAGMA integrity_check(1)", [], |r| r.get(0))?;
            if result != "ok" {
                return Err(Error::IndexCorrupt {
                    database: candidate.logical.clone(),
                    incarnation: candidate.incarnation.clone(),
                    message: format!("replacement integrity check failed: {result}"),
                });
            }
            let generation: u64 =
                connection.query_row("SELECT current_generation FROM state", [], |r| r.get(0))?;
            if generation < candidate.minimum_generation {
                return Err(invalid("replacement generation is below reserved floor"));
            }
            let (busy, log, checkpoint): (i64, i64, i64) =
                connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
            if busy != 0 || log != 0 || checkpoint != 0 {
                return Err(Error::IndexRecovering(
                    "replacement checkpoint has active readers".into(),
                ));
            }
            Ok(generation)
        })
        .and_then(|generation| {
            drop(connection);
            File::open(&candidate.database)?.sync_all()?;
            File::open(candidate.database.parent().unwrap())?.sync_all()?;
            Ok(generation)
        })
}
/// Run SQLite integrity/checkpoint and fsync work before entering the shared
/// cursor publication fence. A ready receipt still consumes an interrupted
/// attempt; only the short selector phase below can expose this candidate.
pub fn prepare_rebuild(candidate: &Candidate, control: &Control) -> Result<PreparedRebuild> {
    let generation = validate_candidate(candidate, control)?;
    let _lock = lock(&candidate.logical)?;
    let mut state = load(&candidate.logical)?.ok_or_else(|| invalid("missing lifecycle state"))?;
    let source_incarnation = state.selected.id.clone();
    let pending = state
        .pending
        .as_mut()
        .ok_or_else(|| invalid("replacement is no longer pending"))?;
    check_candidate_receipt(candidate, pending)?;
    validate_identity(&candidate.database, &pending.index)?;
    let identity = file_identity(&candidate.database)?
        .ok_or_else(|| invalid("replacement file disappeared before preparation"))?;
    pending.index.file_identity = Some(identity);
    pending.ready_generation = Some(generation);
    state.high_water = state.high_water.max(generation);
    control.check()?;
    save(&candidate.logical, &state)?;
    Ok(PreparedRebuild {
        candidate: candidate.clone(),
        generation,
        source_incarnation,
        file_identity: identity,
    })
}
/// Only bounded receipt/identity checks and the selector save occur here. The
/// coordinator may run this inside cursor publication admission; no SQLite
/// integrity scan, checkpoint or candidate fsync belongs inside that fence.
pub fn finish_prepared_rebuild(
    prepared: &PreparedRebuild,
    control: &Control,
    before_cutover: impl FnOnce() -> Result<()>,
) -> Result<u64> {
    control.check()?;
    let candidate = &prepared.candidate;
    let _lock = lock(&candidate.logical)?;
    let mut state = load(&candidate.logical)?.ok_or_else(|| invalid("missing lifecycle state"))?;
    let pending = state
        .pending
        .as_ref()
        .ok_or_else(|| invalid("replacement is no longer pending"))?;
    check_candidate_receipt(candidate, pending)?;
    if state.selected.id != prepared.source_incarnation
        || pending.ready_generation != Some(prepared.generation)
        || pending.index.file_identity != Some(prepared.file_identity)
        || file_identity(&candidate.database)? != Some(prepared.file_identity)
    {
        return Err(invalid("prepared replacement identity or receipt changed"));
    }
    before_cutover()?;
    control.check()?;
    let pending = state.pending.take().unwrap();
    let old = std::mem::replace(&mut state.selected, pending.index);
    state.retired.push(old);
    state.selected_generation = Some(prepared.generation);
    state.failed = None;
    state.failed_reason = None;
    control.publish(|| save(&candidate.logical, &state))?;
    Ok(prepared.generation)
}
pub fn finish_rebuild(
    candidate: &Candidate,
    control: &Control,
    before_cutover: impl FnOnce() -> Result<()>,
) -> Result<u64> {
    let prepared = prepare_rebuild(candidate, control)?;
    finish_prepared_rebuild(&prepared, control, before_cutover)
}

fn family_path(path: &Path, suffix: &str) -> PathBuf {
    let mut path = path.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}
fn family_bytes(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = family_path(path, suffix);
        match fs::symlink_metadata(&path) {
            Ok(m) => {
                check_regular(&path, &m)?;
                total = total
                    .checked_add(m.len())
                    .ok_or_else(|| invalid("disk accounting overflow"))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(total)
}
fn extra_families(logical: &Path, cutover: bool) -> Result<Vec<PathBuf>> {
    let Some(state) = load(logical)? else {
        return Ok(vec![]);
    };
    state
        .retired
        .iter()
        .chain(state.pending.iter().filter(|_| !cutover).map(|p| &p.index))
        .chain(cutover.then_some(&state.selected))
        .map(|index| physical(logical, index))
        .collect()
}
fn charged_family(path: &Path) -> Result<u64> {
    // The health page and lease remain conservatively charged even when
    // cleanup removes a captured family during a running replacement.
    family_bytes(path)?
        .checked_add(PAGE as u64)
        .ok_or_else(|| invalid("disk accounting overflow"))
}
pub fn extra_bytes(logical: &Path, cutover: bool) -> Result<u64> {
    extra_families(logical, cutover)?
        .iter()
        .try_fold(0u64, |sum, path| {
            sum.checked_add(charged_family(path)?)
                .ok_or_else(|| invalid("disk accounting overflow"))
        })
}
/// Apparent on-disk temporary-file bytes, not logical cursor-retention estimates. The
/// normal selected database payload is permanent index state and is excluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskUsage {
    pub used_bytes: u64,
    pub free_bytes: u64,
}
const DISK_SAMPLE_FAMILIES: usize = 16;
const DISK_CENSUS_INTERVAL: Duration = Duration::from_secs(5);
#[derive(Debug)]
struct SampledFamily {
    path: PathBuf,
    full: bool,
    bytes: u64,
}
#[derive(Debug)]
struct DiskSample {
    families: Vec<SampledFamily>,
    active: Vec<(PathBuf, bool)>,
    next: usize,
    refreshed: Instant,
}
impl DiskSample {
    fn used(&self) -> Result<u64> {
        self.families.iter().try_fold(0u64, |sum, family| {
            sum.checked_add(family.bytes)
                .ok_or_else(|| invalid("disk accounting overflow"))
        })
    }
}
fn insert_family(families: &mut BTreeMap<PathBuf, bool>, path: PathBuf, full: bool) {
    // A physical family is charged once; retirement's full-family charge wins
    // over serving-sidecar accounting if a cutover snapshot sees both roles.
    *families.entry(path).or_default() |= full;
}
fn disk_family(logical: &Path, index: &Incarnation, allow_missing: bool) -> Result<PathBuf> {
    let path = physical(logical, index)?;
    if !index.legacy {
        for directory_path in [
            parent(logical)?.join("indexes"),
            inc_dir(logical, &index.id)?,
        ] {
            match directory(&directory_path, false) {
                Ok(()) => {}
                // Publication/cleanup can remove a captured family, and a new
                // pending receipt precedes creation of its private directory.
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    let actual = file_identity(&path)?;
    if index.file_identity.is_some() && actual.is_some() && actual != index.file_identity {
        return Err(invalid(
            "index file identity changed outside the coordinator",
        ));
    }
    if !allow_missing && index.file_identity.is_some() && actual.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "selected index file disappeared during disk accounting",
        )
        .into());
    }
    Ok(path)
}
struct WorkspaceFamilies {
    families: BTreeMap<PathBuf, bool>,
    active: Vec<(PathBuf, bool)>,
}
fn serving_families(logical: &Path, cutover: bool) -> Result<WorkspaceFamilies> {
    let mut families = BTreeMap::new();
    let Some(state) = load(logical)? else {
        insert_family(&mut families, logical.to_owned(), false);
        return Ok(WorkspaceFamilies {
            families,
            active: vec![(logical.to_owned(), false)],
        });
    };
    let selected = disk_family(logical, &state.selected, state.initializing)?;
    let mut active = vec![(selected.clone(), false)];
    let replacing = cutover && state.pending.is_some();
    insert_family(&mut families, selected, replacing);
    for index in &state.retired {
        insert_family(&mut families, disk_family(logical, index, true)?, true);
    }
    if let Some(pending) = &state.pending {
        let path = disk_family(logical, &pending.index, true)?;
        active.push((path.clone(), true));
        insert_family(&mut families, path, !replacing);
    }
    active.sort();
    Ok(WorkspaceFamilies { families, active })
}
/// An unreadable sibling receipt cannot prove which family is serving. Keep
/// that metadata failure local by charging every recognized owned family in
/// full. Never use this fallback to repair/delete files or follow unknown paths.
fn quarantined_families(logical: &Path) -> Result<BTreeMap<PathBuf, bool>> {
    let mut families = BTreeMap::new();
    if family_bytes(logical)? != 0 {
        insert_family(&mut families, logical.to_owned(), true);
    }
    let indexes = parent(logical)?.join("indexes");
    match directory(&indexes, false) {
        Ok(()) => {}
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(families);
        }
        Err(error) => return Err(error),
    }
    for entry in fs::read_dir(&indexes)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_str().is_some_and(valid_id) {
            return Err(invalid(
                "unknown index directory prevents conservative disk accounting",
            ));
        }
        let path = entry.path();
        match directory(&path, false) {
            Ok(()) => {}
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
        for file in fs::read_dir(&path)? {
            let file = file?;
            let name = file.file_name();
            if ![
                "index.db",
                "index.db-wal",
                "index.db-shm",
                "index.db-journal",
                "health",
                "readers.lock",
            ]
            .iter()
            .any(|allowed| name == *allowed)
            {
                return Err(invalid(
                    "unknown index file prevents conservative disk accounting",
                ));
            }
            match fs::symlink_metadata(file.path()) {
                Ok(metadata) => {
                    check_regular(&file.path(), &metadata)?;
                    if (name == "health" && metadata.len() > PAGE as u64)
                        || (name == "readers.lock" && metadata.len() != 0)
                    {
                        return Err(invalid(
                            "unknown index metadata size prevents conservative disk accounting",
                        ));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        insert_family(&mut families, path.join("index.db"), true);
    }
    Ok(families)
}
fn active_families(logical: &Path) -> Result<Vec<(PathBuf, bool)>> {
    let Some(state) = load(logical)? else {
        return Ok(vec![(logical.to_owned(), false)]);
    };
    let mut paths = vec![(
        disk_family(logical, &state.selected, state.initializing)?,
        false,
    )];
    if let Some(pending) = &state.pending {
        paths.push((disk_family(logical, &pending.index, true)?, true));
    }
    paths.sort();
    Ok(paths)
}
fn sampled_family_bytes(path: &Path, full: bool) -> Result<u64> {
    if full {
        return charged_family(path);
    }
    let mut bytes = 0u64;
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = family_path(path, suffix);
        match fs::symlink_metadata(&sidecar) {
            Ok(metadata) => {
                check_regular(&sidecar, &metadata)?;
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| invalid("disk accounting overflow"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(bytes)
}
/// One coordinator data-root pool covers serving WALs and unpublished/retired families.
/// Admission and final checks take a fresh census. During active analysis the
/// selected/pending families are sampled each time, with bounded rotation over
/// other families and a full refresh every five seconds. Cached entries hold
/// paths and last observed sizes, never permanent retired-size reservations.
#[derive(Clone, Debug)]
pub struct DiskBudget {
    logical: PathBuf,
    volume: PathBuf,
    sample: Arc<Mutex<Option<DiskSample>>>,
}
impl DiskBudget {
    pub fn new(logical: &Path, data: &Path) -> Result<Self> {
        directory(&data.join("workspaces"), false)?;
        Ok(Self {
            logical: logical.into(),
            volume: data.into(),
            sample: Arc::new(Mutex::new(None)),
        })
    }
    fn census(&self, cutover: bool) -> Result<DiskSample> {
        let workspaces = self.volume.join("workspaces");
        directory(&workspaces, false)?;
        let mut families = BTreeMap::new();
        let mut logicals = BTreeSet::from([self.logical.clone()]);
        for entry in fs::read_dir(&workspaces)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.as_encoded_bytes();
            if name.len() != 24
                || !name
                    .iter()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            {
                continue;
            }
            directory(&entry.path(), false)?;
            logicals.insert(entry.path().join("index.db"));
        }
        let mut active = Vec::new();
        for logical in logicals {
            let workspace = match serving_families(&logical, cutover && logical == self.logical) {
                Ok(workspace) => workspace,
                Err(Error::Json(_) | Error::InvalidRequest(_) | Error::UpdateRequired(_))
                    if logical != self.logical =>
                {
                    WorkspaceFamilies {
                        families: quarantined_families(&logical)?,
                        active: Vec::new(),
                    }
                }
                Err(error) => return Err(error),
            };
            if logical == self.logical {
                active = workspace.active;
            }
            for (path, full) in workspace.families {
                insert_family(&mut families, path, full);
            }
        }
        let families = families
            .into_iter()
            .map(|(path, full)| {
                let bytes = sampled_family_bytes(&path, full)?;
                Ok(SampledFamily { path, full, bytes })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(DiskSample {
            families,
            active,
            next: 0,
            refreshed: Instant::now(),
        })
    }
    fn free_bytes(&self) -> Result<u64> {
        let path = std::ffi::CString::new(self.volume.as_os_str().as_bytes())
            .map_err(|_| invalid("invalid volume path"))?;
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
    }
    pub fn usage(&self, cutover: bool) -> Result<DiskUsage> {
        let mut cache = self
            .sample
            .lock()
            .map_err(|_| invalid("disk sample lock poisoned"))?;
        let sample = self.census(cutover)?;
        let used_bytes = sample.used()?;
        if !cutover {
            *cache = Some(sample);
        }
        Ok(DiskUsage {
            used_bytes,
            free_bytes: self.free_bytes()?,
        })
    }
    fn enforce(limits: &Limits, usage: DiskUsage) -> Result<()> {
        limits.validate()?;
        if usage.used_bytes > limits.rebuild_disk_mib * 1024 * 1024 {
            return Err(Error::DiskPressure(format!(
                "temporary index files use {} bytes, above temporary disk budget (rebuild_disk_mib={})",
                usage.used_bytes, limits.rebuild_disk_mib
            )));
        }
        if usage.free_bytes < limits.min_free_disk_mib * 1024 * 1024 {
            return Err(Error::DiskPressure(format!(
                "available disk {} bytes is below min_free_disk_mib={}",
                usage.free_bytes, limits.min_free_disk_mib
            )));
        }
        Ok(())
    }
    pub fn check(&self, limits: &Limits, cutover: bool) -> Result<()> {
        Self::enforce(limits, self.usage(cutover)?)
    }
    pub fn check_sampled(&self, limits: &Limits) -> Result<()> {
        let mut cache = self
            .sample
            .lock()
            .map_err(|_| invalid("disk sample lock poisoned"))?;
        let active = active_families(&self.logical)?;
        if cache.as_ref().is_none_or(|sample| {
            sample.refreshed.elapsed() >= DISK_CENSUS_INTERVAL || sample.active != active
        }) {
            *cache = Some(self.census(false)?);
        }
        let sample = cache.as_mut().expect("initialized disk sample");
        // Only these two current-workspace families can grow through analysis.
        // Other serving/retired cursor bookkeeping gets bounded round-robin
        // sampling, followed by periodic discovery of newly owned paths.
        let mut indices = BTreeSet::new();
        for (path, _) in active {
            if let Ok(index) = sample
                .families
                .binary_search_by(|family| family.path.cmp(&path))
            {
                indices.insert(index);
            }
        }
        for _ in 0..sample.families.len().min(DISK_SAMPLE_FAMILIES) {
            indices.insert(sample.next);
            sample.next = (sample.next + 1) % sample.families.len();
        }
        for index in indices {
            let family = &mut sample.families[index];
            family.bytes = sampled_family_bytes(&family.path, family.full)?;
        }
        let usage = DiskUsage {
            used_bytes: sample.used()?,
            free_bytes: self.free_bytes()?,
        };
        drop(cache);
        Self::enforce(limits, usage)
    }
}
/// Only known owned families are removed. A lease or cleanup error retains state
/// and its disk charge; no reservation can be released on a partial failure.
pub fn cleanup(logical: &Path, registry_generation: u64) -> Result<()> {
    let _lock = lock(logical)?;
    let Some(mut state) = load(logical)? else {
        return Ok(());
    };
    if state.retired.is_empty()
        || state
            .selected_generation
            .is_some_and(|g| g > registry_generation)
    {
        return Ok(());
    }
    let original_len = state.retired.len();
    let mut first = None;
    let mut retained = vec![];
    for index in state.retired.drain(..) {
        let action = (|| -> Result<bool> {
            // Pre-lifecycle SQLite descriptors could still exist without leases.
            if index.legacy {
                return Ok(false);
            }
            let dir = inc_dir(logical, &index.id)?;
            let base = dir.parent().unwrap();
            directory(base, false)?;
            match directory(&dir, false) {
                Ok(()) => {}
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    File::open(base)?.sync_all()?;
                    return Ok(true);
                }
                Err(e) => return Err(e),
            }
            let reader = open_file(&dir.join("readers.lock"), true)?;
            match reader.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
            let allowed = [
                "index.db",
                "index.db-wal",
                "index.db-shm",
                "index.db-journal",
                "health",
                "readers.lock",
            ];
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                if !allowed.iter().any(|n| entry.file_name() == *n) {
                    return Err(invalid("foreign file in retired index directory"));
                }
                regular(&entry.path())?;
            }
            let database = physical(logical, &index)?;
            validate_identity(&database, &index)?;
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let path = family_path(&database, suffix);
                match fs::symlink_metadata(&path) {
                    Ok(_) => {
                        regular(&path)?;
                        fs::remove_file(path)?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            // Keep the lock descriptor until its name and directory are gone.
            match fs::remove_file(dir.join("health")) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            fs::remove_file(dir.join("readers.lock"))?;
            fs::remove_dir(&dir)?;
            File::open(parent(logical)?.join("indexes"))?.sync_all()?;
            Ok(true)
        })();
        match action {
            Ok(true) => {}
            Ok(false) => retained.push(index),
            Err(e) => {
                retained.push(index);
                if first.is_none() {
                    first = Some(e);
                }
            }
        }
    }
    state.retired = retained;
    if state.retired.len() != original_len {
        save(logical, &state)?;
    }
    first.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture() -> (PathBuf, PathBuf) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/recovery-unit")
            .join(nonce().unwrap());
        let logical = root
            .join("workspaces")
            .join("a".repeat(24))
            .join("index.db");
        fs::create_dir_all(logical.parent().unwrap()).unwrap();
        (root, logical)
    }
    fn indexed(path: &Path, generation: u64) {
        let db = Connection::open(path).unwrap();
        db.execute_batch("PRAGMA user_version=8; PRAGMA journal_mode=WAL; CREATE TABLE state(current_generation INTEGER); CREATE TABLE facts(value TEXT); INSERT INTO facts VALUES('saved source');").unwrap();
        db.execute("INSERT INTO state VALUES(?)", [generation])
            .unwrap();
    }
    fn fence(logical: &Path) -> IndexReadGuard {
        let first = IndexReadGuard::open_unbudgeted(logical).unwrap();
        assert!(matches!(
            first.note_corruption("fixture corruption proof"),
            Err(Error::IndexCorrupt { .. })
        ));
        first
    }
    #[test]
    fn sqlite_codes_fence_exact_incarnation_only_and_protect_live_reader_files() {
        let (root, logical) = fixture();
        adopt(&logical, 0).unwrap();
        let first = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        let old = first.physical_path().to_owned();
        let id = first.incarnation().to_owned();
        assert_ne!(old, logical);
        indexed(&old, 5);
        reserve_generation(&logical, 5).unwrap();
        let second = IndexReadGuard::open_incarnation_unbudgeted(&logical, &id).unwrap();
        let reader = Connection::open(&old).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        assert_eq!(
            reader
                .query_row("SELECT value FROM facts", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "saved source"
        );
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_INTERRUPT,
            rusqlite::ffi::SQLITE_CANTOPEN,
        ] {
            let error = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
            assert!(matches!(first.map_sqlite(error), Error::Sqlite(_)));
            assert!(second.check().is_ok());
        }
        assert!(
            !report_corruption(&logical, &id).unwrap(),
            "unproved control report cannot poison a healthy index"
        );
        let error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT_INDEX),
            None,
        );
        assert!(matches!(
            first.map_sqlite(error),
            Error::IndexCorrupt { .. }
        ));
        assert!(matches!(second.check(), Err(Error::IndexCorrupt { .. })));
        assert!(report_corruption(&logical, &id).unwrap());
        let next = begin_rebuild(&logical, false, 3).unwrap();
        assert_eq!(next.minimum_generation, 6);
        indexed(&next.database, 6);
        reserve_generation(&logical, 6).unwrap();
        assert_eq!(
            finish_rebuild(&next, &Control::default(), || Ok(())).unwrap(),
            6
        );
        assert!(!report_corruption(&logical, &id).unwrap());
        assert_eq!(
            IndexReadGuard::open_unbudgeted(&logical)
                .unwrap()
                .incarnation(),
            next.incarnation
        );
        cleanup(&logical, 5).unwrap();
        assert!(
            old.is_file(),
            "retirement must wait for FULL registry acknowledgement"
        );
        cleanup(&logical, 6).unwrap();
        assert!(
            old.is_file(),
            "shared reader leases still own the old family"
        );
        drop((reader, first, second));
        cleanup(&logical, 6).unwrap();
        assert!(!old.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interrupted_initial_adoption_reuses_its_durable_nonce_and_blocks_reads() {
        for partial_health in [false, true] {
            let (root, logical) = fixture();
            let state = State {
                version: VERSION,
                initializing: true,
                selected: Incarnation {
                    id: nonce().unwrap(),
                    legacy: false,
                    file_identity: None,
                },
                high_water: 0,
                selected_generation: None,
                pending: None,
                retired: vec![],
                failed: None,
                failed_reason: None,
            };
            save(&logical, &state).unwrap();
            if partial_health {
                let base = parent(&logical).unwrap().join("indexes");
                directory(&base, true).unwrap();
                let dir = inc_dir(&logical, &state.selected.id).unwrap();
                directory(&dir, true).unwrap();
                fs::write(dir.join("health"), &MAGIC[..4]).unwrap();
            }
            assert!(matches!(
                IndexReadGuard::open_unbudgeted(&logical),
                Err(Error::IndexRecovering(_))
            ));
            adopt(&logical, 0).unwrap();
            let guard = IndexReadGuard::open_unbudgeted(&logical).unwrap();
            assert_eq!(guard.incarnation(), state.selected.id);
            assert_eq!(
                fs::read_dir(parent(&logical).unwrap().join("indexes"))
                    .unwrap()
                    .count(),
                1
            );
            assert!(!load(&logical).unwrap().unwrap().initializing);
            drop(guard);
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn removed_and_zeroed_owned_selected_files_are_fenced_without_reusing_identity() {
        for remove in [false, true] {
            let (root, logical) = fixture();
            adopt(&logical, 0).unwrap();
            let guard = IndexReadGuard::open_unbudgeted(&logical).unwrap();
            let physical = guard.physical_path().to_owned();
            indexed(&physical, 7);
            reserve_generation(&logical, 7).unwrap();
            let id = guard.incarnation().to_owned();
            if remove {
                fs::remove_file(&physical).unwrap();
            } else {
                OpenOptions::new()
                    .write(true)
                    .open(&physical)
                    .unwrap()
                    .set_len(0)
                    .unwrap();
            }
            assert!(matches!(
                IndexReadGuard::open_unbudgeted(&logical),
                Err(Error::IndexCorrupt { .. })
            ));
            assert!(matches!(guard.check(), Err(Error::IndexCorrupt { .. })));
            assert!(report_corruption(&logical, &id).unwrap());
            let next = begin_rebuild(&logical, false, 7).unwrap();
            assert_ne!(next.incarnation, id);
            assert!(next.minimum_generation > 7);
            drop(guard);
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn selected_ready_candidate_is_not_abandoned_when_directory_sync_ack_fails() {
        let (root, logical) = fixture();
        adopt(&logical, 0).unwrap();
        let old = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        indexed(old.physical_path(), 3);
        reserve_generation(&logical, 3).unwrap();
        assert!(old.note_corruption("fixture corruption proof").is_err());
        let candidate = begin_rebuild(&logical, false, 3).unwrap();
        indexed(&candidate.database, 4);
        reserve_generation(&logical, 4).unwrap();
        let error = finish_rebuild(&candidate, &Control::default(), || {
            FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("directory sync failure"));
        assert_eq!(
            selected_incarnation(&logical).unwrap().as_deref(),
            Some(candidate.incarnation.as_str())
        );
        assert_eq!(selected_replacement_generation(&logical).unwrap(), Some(4));
        assert!(!blocked(&logical).unwrap());
        reconcile(&logical).unwrap();
        let selected = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        assert_eq!(selected.incarnation(), candidate.incarnation);
        assert!(selected.check().is_ok());
        assert!(old.check().is_err());
        assert!(load(&logical).unwrap().unwrap().pending.is_none());
        drop((old, selected));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interrupted_build_and_verified_but_unselected_candidate_require_explicit_refresh() {
        let (root, logical) = fixture();
        adopt(&logical, 3).unwrap();
        let old = fence(&logical);
        let interrupted = begin_rebuild(&logical, false, 3).unwrap();
        indexed(&interrupted.database, 4);
        reserve_generation(&logical, 4).unwrap();
        reconcile(&logical).unwrap();
        assert!(failure(&logical).unwrap().is_some());
        assert!(matches!(
            begin_rebuild(&logical, false, 3),
            Err(Error::IndexRecovering(_))
        ));
        cleanup(&logical, 3).unwrap();
        assert!(!interrupted.database.exists());
        let verified = begin_rebuild(&logical, true, 3).unwrap();
        assert_eq!(verified.minimum_generation, 5);
        indexed(&verified.database, 5);
        let stopped = finish_rebuild(&verified, &Control::default(), || {
            Err(Error::ResourceLimited("cutover preflight".into()))
        });
        assert!(matches!(stopped, Err(Error::ResourceLimited(_))));
        assert!(
            load(&logical)
                .unwrap()
                .unwrap()
                .pending
                .unwrap()
                .ready_generation
                .is_some()
        );
        assert_eq!(
            selected_incarnation(&logical).unwrap().unwrap(),
            old.incarnation()
        );
        reconcile(&logical).unwrap();
        assert!(matches!(
            begin_rebuild(&logical, false, 3),
            Err(Error::IndexRecovering(_))
        ));
        let retry = begin_rebuild(&logical, true, 3).unwrap();
        assert_eq!(retry.minimum_generation, 6);
        indexed(&retry.database, 6);
        finish_rebuild(&retry, &Control::default(), || Ok(())).unwrap();
        reconcile(&logical).unwrap();
        assert!(failure(&logical).unwrap().is_none());
        assert_eq!(selected_replacement_generation(&logical).unwrap(), Some(6));
        drop(old);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn physical_budget_charges_other_workspaces_and_retained_legacy_without_sql_reads() {
        let (root, logical) = fixture();
        // A real SQLite NOTADB error proves physical corruption; no fake SQL flag.
        fs::write(&logical, vec![b'!'; 768 * 1024]).unwrap();
        adopt(&logical, 5).unwrap();
        let guard = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        let sqlite = Connection::open(guard.physical_path()).unwrap();
        let e = sqlite
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap_err();
        assert!(matches!(guard.map_sqlite(e), Error::IndexCorrupt { .. }));
        drop((sqlite, guard));
        let replacement = begin_rebuild(&logical, false, 5).unwrap();
        indexed(&replacement.database, 6);
        finish_rebuild(&replacement, &Control::default(), || Ok(())).unwrap();
        cleanup(&logical, 6).unwrap();
        assert!(
            logical.exists(),
            "old unleased pre-lifecycle descriptors cannot be excluded"
        );
        assert!(extra_bytes(&logical, false).unwrap() >= 768 * 1024);
        let sibling = root
            .join("workspaces")
            .join("b".repeat(24))
            .join("index.db");
        fs::create_dir_all(sibling.parent().unwrap()).unwrap();
        adopt(&sibling, 0).unwrap();
        let old = fence(&sibling);
        let next = begin_rebuild(&sibling, false, 0).unwrap();
        fs::write(&next.database, vec![0; 512 * 1024]).unwrap();
        let budget = DiskBudget::new(&sibling, &root).unwrap();
        let limits = Limits {
            rebuild_disk_mib: 1,
            min_free_disk_mib: 0,
            ..Limits::default()
        };
        assert!(matches!(
            budget.check(&limits, false),
            Err(Error::DiskPressure(_))
        ));
        assert!(
            budget.check(&limits, true).is_ok(),
            "post-cutover charge excludes selected candidate"
        );
        assert!(matches!(
            budget.check(
                &Limits {
                    min_free_disk_mib: u64::MAX / (1024 * 1024),
                    ..limits
                },
                true
            ),
            Err(Error::DiskPressure(_))
        ));
        drop(old);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn shared_pool_counts_serving_sidecars_and_cutover_roles_exactly_once() {
        let (root, logical) = fixture();
        adopt(&logical, 0).unwrap();
        let old = fence(&logical);
        let selected = old.physical_path();
        // Pure physical-accounting fixtures: no SQL execution interprets these
        // owned file payloads as facts, compiler output, or schema evidence.
        fs::write(selected, vec![0; 1024 * 1024]).unwrap();
        fs::write(family_path(selected, "-wal"), [0; 300]).unwrap();
        fs::write(family_path(selected, "-shm"), [0; 100]).unwrap();
        fs::write(family_path(selected, "-journal"), [0; 50]).unwrap();
        let sibling = root
            .join("workspaces")
            .join("b".repeat(24))
            .join("index.db");
        fs::create_dir_all(sibling.parent().unwrap()).unwrap();
        adopt(&sibling, 0).unwrap();
        let sibling_guard = IndexReadGuard::open_unbudgeted(&sibling).unwrap();
        fs::write(sibling_guard.physical_path(), vec![0; 2 * 1024 * 1024]).unwrap();
        fs::write(family_path(sibling_guard.physical_path(), "-wal"), [0; 200]).unwrap();
        let budget = DiskBudget::new(&logical, &root).unwrap();
        assert_eq!(budget.usage(false).unwrap().used_bytes, 650);
        assert_eq!(
            budget.usage(true).unwrap().used_bytes,
            650,
            "without a pending candidate, a preview cannot retire serving data"
        );
        let candidate = begin_rebuild(&logical, false, 0).unwrap();
        fs::write(&candidate.database, vec![0; 64 * 1024]).unwrap();
        fs::write(family_path(&candidate.database, "-wal"), [0; 128]).unwrap();
        fs::write(family_path(&candidate.database, "-shm"), [0; 256]).unwrap();
        assert_eq!(
            budget.usage(false).unwrap().used_bytes,
            650 + 64 * 1024 + 384 + PAGE as u64
        );
        assert_eq!(
            budget.usage(true).unwrap().used_bytes,
            650 + 1024 * 1024 + PAGE as u64 + 384,
            "old main becomes temporary, candidate main becomes serving; every WAL remains charged once"
        );
        // The pool is global, not a new allowance per active workspace.
        assert_eq!(
            DiskBudget::new(&sibling, &root)
                .unwrap()
                .usage(false)
                .unwrap()
                .used_bytes,
            budget.usage(false).unwrap().used_bytes
        );
        fail_rebuild(&logical, "owned fixture failed candidate").unwrap();
        assert_eq!(
            budget.usage(false).unwrap().used_bytes,
            650 + 64 * 1024 + 384 + PAGE as u64,
            "failed candidates remain charged as retired families until cleanup"
        );
        cleanup(&logical, 0).unwrap();
        assert!(!candidate.database.exists());
        assert_eq!(budget.usage(false).unwrap().used_bytes, 650);
        drop((old, sibling_guard));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn sampled_pool_rotates_sizes_and_full_census_discovers_new_workspaces() {
        let (root, logical) = fixture();
        adopt(&logical, 0).unwrap();
        let budget = DiskBudget::new(&logical, &root).unwrap();
        let mut sidecars = Vec::new();
        for n in 0..40 {
            let sibling = root
                .join("workspaces")
                .join(format!("{n:024x}"))
                .join("index.db");
            fs::create_dir_all(sibling.parent().unwrap()).unwrap();
            adopt(&sibling, 0).unwrap();
            let guard = IndexReadGuard::open_unbudgeted(&sibling).unwrap();
            let path = family_path(guard.physical_path(), "-wal");
            fs::write(&path, [0; 1]).unwrap();
            sidecars.push(path);
        }
        assert_eq!(budget.usage(false).unwrap().used_bytes, 40);
        for path in &sidecars {
            fs::write(path, [0; 4]).unwrap();
        }
        let limits = Limits {
            min_free_disk_mib: 0,
            ..Limits::default()
        };
        for _ in 0..3 {
            budget.check_sampled(&limits).unwrap();
        }
        assert_eq!(
            budget
                .sample
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .used()
                .unwrap(),
            160,
            "bounded rotation must resample every cached family, not freeze other workspace sizes"
        );
        let new_logical = root
            .join("workspaces")
            .join("c".repeat(24))
            .join("index.db");
        fs::create_dir_all(new_logical.parent().unwrap()).unwrap();
        adopt(&new_logical, 0).unwrap();
        let guard = IndexReadGuard::open_unbudgeted(&new_logical).unwrap();
        fs::write(
            family_path(guard.physical_path(), "-wal"),
            vec![0; 2 * 1024 * 1024],
        )
        .unwrap();
        let tight = Limits {
            rebuild_disk_mib: 1,
            ..limits
        };
        assert!(
            matches!(budget.check(&tight, false), Err(Error::DiskPressure(_))),
            "preflight always discovers a new registered family immediately"
        );
        assert!(matches!(
            budget.check_sampled(&tight),
            Err(Error::DiskPressure(_))
        ));
        fs::remove_file(family_path(guard.physical_path(), "-wal")).unwrap();
        // Force the cadence boundary without sleeps or throughput assumptions.
        budget.sample.lock().unwrap().as_mut().unwrap().refreshed =
            Instant::now() - DISK_CENSUS_INTERVAL;
        budget.check_sampled(&tight).unwrap();
        assert_eq!(
            budget
                .sample
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .used()
                .unwrap(),
            160
        );
        drop(guard);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pool_access_and_version_failures_are_distinct_from_pressure() {
        let (root, logical) = fixture();
        adopt(&logical, 0).unwrap();
        let guard = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        let wal = family_path(guard.physical_path(), "-wal");
        let outside = root.join("foreign-payload");
        fs::write(&outside, [0; 16]).unwrap();
        std::os::unix::fs::symlink(&outside, &wal).unwrap();
        let budget = DiskBudget::new(&logical, &root).unwrap();
        assert!(matches!(
            budget.check(&Limits::default(), false),
            Err(Error::InvalidRequest(_))
        ));
        assert_eq!(fs::read(&outside).unwrap(), [0; 16]);
        fs::remove_file(wal).unwrap();
        let receipt = state_path(&logical);
        let mut payload: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        payload["version"] = 999.into();
        fs::write(&receipt, serde_json::to_vec(&payload).unwrap()).unwrap();
        assert!(matches!(
            budget.check(&Limits::default(), false),
            Err(Error::UpdateRequired(_))
        ));
        drop(guard);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn malformed_sibling_metadata_stays_local_without_omitting_its_disk_charge() {
        let (root, logical) = fixture();
        adopt(&logical, 0).unwrap();
        let guard = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        fs::write(guard.physical_path(), [0; 1024]).unwrap();
        fs::write(family_path(guard.physical_path(), "-wal"), [0; 200]).unwrap();
        let sibling = root
            .join("workspaces")
            .join("b".repeat(24))
            .join("index.db");
        fs::create_dir_all(sibling.parent().unwrap()).unwrap();
        adopt(&sibling, 0).unwrap();
        let budget = DiskBudget::new(&sibling, &root).unwrap();
        let active = DiskBudget::new(&logical, &root).unwrap();
        let receipt = state_path(&logical);
        let valid = fs::read(&receipt).unwrap();
        for future in [false, true] {
            let damaged = if future {
                let mut payload: serde_json::Value = serde_json::from_slice(&valid).unwrap();
                payload["version"] = 999.into();
                serde_json::to_vec(&payload).unwrap()
            } else {
                b"{broken lifecycle".to_vec()
            };
            fs::write(&receipt, &damaged).unwrap();
            let active_error = active.check(&Limits::default(), false).unwrap_err();
            assert!(if future {
                matches!(active_error, Error::UpdateRequired(_))
            } else {
                matches!(active_error, Error::Json(_))
            });
            budget.check(&Limits::default(), false).unwrap();
            assert_eq!(
                budget.usage(false).unwrap().used_bytes,
                1024 + 200 + PAGE as u64,
                "unknown serving role requires a full family charge, not omission"
            );
            budget.check_sampled(&Limits::default()).unwrap();
            assert_eq!(
                fs::read(&receipt).unwrap(),
                damaged,
                "accounting cannot repair or reinterpret a quarantined sibling"
            );
        }
        let foreign = root.join("foreign-dir");
        fs::create_dir(&foreign).unwrap();
        let unsafe_path = guard
            .physical_path()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("f".repeat(32));
        std::os::unix::fs::symlink(&foreign, &unsafe_path).unwrap();
        assert!(
            matches!(
                budget.check(&Limits::default(), false),
                Err(Error::InvalidRequest(_))
            ),
            "quarantine never swallows unsafe fallback paths"
        );
        drop(guard);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cleanup_retries_owned_partial_families_without_deleting_foreign_entries() {
        let (root, logical) = fixture();
        adopt(&logical, 0).unwrap();
        let old = fence(&logical);
        let old_dir = old.physical_path().parent().unwrap().to_owned();
        let replacement = begin_rebuild(&logical, false, 0).unwrap();
        indexed(&replacement.database, 1);
        finish_rebuild(&replacement, &Control::default(), || Ok(())).unwrap();
        drop(old);
        let foreign = old_dir.join("foreign");
        fs::write(&foreign, "owned outside index lifecycle").unwrap();
        assert!(cleanup(&logical, 1).is_err());
        assert!(foreign.exists());
        assert!(has_retired(&logical).unwrap());
        fs::remove_file(foreign).unwrap();
        fs::remove_file(old_dir.join("health")).unwrap();
        cleanup(&logical, 1).unwrap();
        assert!(!has_retired(&logical).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[path = "recovery_upgrade_tests.rs"]
mod upgrade_tests;
