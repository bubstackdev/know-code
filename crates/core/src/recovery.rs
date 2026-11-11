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
    collections::BTreeSet,
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
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub const VERSION: u32 = 1;
const PAGE: usize = 4096;
const MAGIC: &[u8; 8] = b"KCIDX001";
const MAX_STATE: u64 = 256 * 1024;
const MAX_INCARNATIONS: usize = 256;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Incarnation {
    id: String,
    legacy: bool,
    file_identity: Option<(u64, u64)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    index: Incarnation,
    minimum_generation: u64,
    ready_generation: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    selected: Incarnation,
    high_water: u64,
    selected_generation: Option<u64>,
    pending: Option<Pending>,
    retired: Vec<Incarnation>,
    failed: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Candidate {
    pub database: PathBuf,
    pub incarnation: String,
    pub minimum_generation: u64,
    logical: PathBuf,
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
    let state: State = serde_json::from_slice(&bytes)?;
    if state.version != VERSION {
        return Err(Error::UpdateRequired(
            "unsupported index lifecycle state".into(),
        ));
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
    let bytes = serde_json::to_vec(state)?;
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
    if load(logical)?.is_none() {
        let identity = file_identity(logical)?;
        let legacy = identity.is_some() || family_bytes(logical)? != 0 || trusted_generation != 0;
        let index = Incarnation {
            id: nonce()?,
            legacy,
            file_identity: identity,
        };

        create_incarnation(logical, &index)?;
        save(
            logical,
            &State {
                version: VERSION,
                selected: index,
                high_water: trusted_generation,
                selected_generation: None,
                pending: None,
                retired: vec![],
                failed: None,
            },
        )?;
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
    let _lock = lock(logical)?;
    let mut state = load(logical)?.ok_or_else(|| invalid("workspace index is not adopted"))?;
    if state.pending.is_some() || (state.failed.is_some() && !explicit) {
        return Err(Error::IndexRecovering(
            "automatic recovery already attempted; explicit refresh is required".into(),
        ));
    }
    if state.retired.len() >= MAX_INCARNATIONS {
        return Err(Error::ResourceLimited(
            "retained index incarnation count reached limit; cleanup is required".into(),
        ));
    }
    let minimum_generation = state
        .high_water
        .max(trusted_generation)
        .checked_add(1)
        .ok_or_else(|| invalid("index generation exhausted"))?;
    if lease(logical, &state.selected)?
        .bad()
        .load(Ordering::Acquire)
        == 0
    {
        return Err(invalid("recovery requires a fenced index"));
    }
    let index = Incarnation {
        id: nonce()?,
        legacy: false,
        file_identity: None,
    };
    let candidate = Candidate {
        database: physical(logical, &index)?,
        incarnation: index.id.clone(),
        minimum_generation,
        logical: logical.into(),
    };
    state.pending = Some(Pending {
        index: index.clone(),
        minimum_generation,
        ready_generation: None,
    });
    state.failed = None;
    save(logical, &state)?;
    create_incarnation(logical, &index)?;
    Ok(candidate)
}
pub fn fail_rebuild(logical: &Path, message: &str) -> Result<()> {
    let _lock = lock(logical)?;
    let mut state = load(logical)?.ok_or_else(|| invalid("missing lifecycle state"))?;
    if let Some(p) = state.pending.take() {
        state.retired.push(p.index);
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
fn validate_candidate(candidate: &Candidate, control: &Control) -> Result<u64> {
    control.check()?;
    regular(&candidate.database)?;
    let mut connection = rusqlite::Connection::open_with_flags(
        &candidate.database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    connection.progress_handler(4096, Some(move || std::time::Instant::now() >= deadline))?;
    control
        .interruptible(connection.get_interrupt_handle(), || {
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
pub fn finish_rebuild(
    candidate: &Candidate,
    control: &Control,
    before_cutover: impl FnOnce() -> Result<()>,
) -> Result<u64> {
    let generation = validate_candidate(candidate, control)?;
    let _lock = lock(&candidate.logical)?;
    let mut state = load(&candidate.logical)?.ok_or_else(|| invalid("missing lifecycle state"))?;
    let pending = state
        .pending
        .as_mut()
        .filter(|p| p.index.id == candidate.incarnation)
        .ok_or_else(|| invalid("replacement is no longer pending"))?;
    validate_identity(&candidate.database, &pending.index)?;
    pending.index.file_identity = file_identity(&candidate.database)?;
    pending.ready_generation = Some(generation);
    state.high_water = state.high_water.max(generation);
    save(&candidate.logical, &state)?;
    before_cutover()?;
    control.check()?;
    let pending = state.pending.take().unwrap();
    let old = std::mem::replace(&mut state.selected, pending.index);
    state.retired.push(old);
    state.selected_generation = Some(generation);
    state.failed = None;
    control.publish(|| save(&candidate.logical, &state))?;
    Ok(generation)
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
                regular(&path)?;
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
pub fn extra_bytes(logical: &Path, cutover: bool) -> Result<u64> {
    let Some(state) = load(logical)? else {
        return Ok(0);
    };
    let mut total = 0u64;
    for index in state
        .retired
        .iter()
        .chain(state.pending.iter().filter(|_| !cutover).map(|p| &p.index))
        .chain(cutover.then_some(&state.selected))
    {
        total = total
            .checked_add(family_bytes(&physical(logical, index)?)?)
            .ok_or_else(|| invalid("disk accounting overflow"))?;
        // Include the fixed health page and lease rather than count them as free.
        total = total
            .checked_add(PAGE as u64)
            .ok_or_else(|| invalid("disk accounting overflow"))?;
    }
    Ok(total)
}
/// Other workspaces cannot grow candidate/retired data while this job holds
/// the coordinator's only analysis slot. Cache their charge once per attempt.
#[derive(Clone)]
pub struct DiskBudget {
    logical: PathBuf,
    volume: PathBuf,
    other_bytes: u64,
}
impl DiskBudget {
    pub fn new(logical: &Path, data: &Path) -> Result<Self> {
        let workspaces = data.join("workspaces");
        directory(&workspaces, false)?;
        let mut other_bytes = 0u64;
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
            let database = entry.path().join("index.db");
            if database != logical {
                other_bytes = other_bytes
                    .checked_add(extra_bytes(&database, false)?)
                    .ok_or_else(|| invalid("disk accounting overflow"))?;
            }
        }
        Ok(Self {
            logical: logical.into(),
            volume: data.into(),
            other_bytes,
        })
    }
    pub fn check(&self, limits: &Limits, cutover: bool) -> Result<()> {
        let used = self
            .other_bytes
            .checked_add(extra_bytes(&self.logical, cutover)?)
            .ok_or_else(|| invalid("disk accounting overflow"))?;
        if used > limits.rebuild_disk_mib * 1024 * 1024 {
            return Err(Error::ResourceLimited(format!(
                "temporary indexes use {used} bytes, above rebuild_disk_mib={}",
                limits.rebuild_disk_mib
            )));
        }
        let path = std::ffi::CString::new(self.volume.as_os_str().as_bytes())
            .map_err(|_| invalid("invalid volume path"))?;
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let free = (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64);
        if free < limits.min_free_disk_mib * 1024 * 1024 {
            return Err(Error::ResourceLimited(format!(
                "available disk {free} bytes is below min_free_disk_mib={}",
                limits.min_free_disk_mib
            )));
        }
        Ok(())
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
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE state(current_generation INTEGER); CREATE TABLE facts(value TEXT); INSERT INTO facts VALUES('saved source');").unwrap();
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
            Err(Error::ResourceLimited(_))
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
            Err(Error::ResourceLimited(_))
        ));
        drop(old);
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
