//! Coordinator-owned generation retention and cursor lifecycle policy.
//! Portable control, replayable pages and byte accounting are not yet wired.
use crate::execution::Execution;
use crate::{Error, Result, query::ReadView};
mod accounting;
pub(crate) mod client;
mod pages;
pub mod result_store;
mod service;
pub use accounting::{ACCOUNTING_VERSION, RetainedUsage};
pub use client::settings;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
pub use service::{CONTROL_VERSION, CursorLimits, CursorService};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

pub const RETENTION_VERSION: u32 = 1;
pub const DEFAULT_MAX_CURSORS: usize = 128;
pub const DEFAULT_RETENTION_BYTES: u64 = 1024 * 1024 * 1024;
/// Private saved-result safety guards, within the shared retention allowance.
pub const MAX_RESULT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_RESULT_CHUNK: usize = 64 * 1024;
pub const IDLE_LIFETIME: Duration = Duration::from_secs(120);
pub const ABSOLUTE_LIFETIME: Duration = Duration::from_secs(600);

fn nonce() -> Result<[u8; 16]> {
    let mut id = [0; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut id)?;
    Ok(id)
}

/// Coordinator-owned logical cursor identity, not yet a wire continuation token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorHandle {
    owner: [u8; 16],
    id: [u8; 16],
}
fn hexadecimal(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                DIGITS[(b >> 4) as usize] as char,
                DIGITS[(b & 15) as usize] as char,
            ]
        })
        .collect()
}
fn unhex<const N: usize>(text: &str) -> Result<[u8; N]> {
    if text.len() != N * 2
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::CursorInvalidated);
    }
    let mut bytes = [0; N];
    for (out, pair) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        *out = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Ok(bytes)
}
impl CursorHandle {
    fn key(self) -> String {
        hexadecimal(&self.id)
    }
    fn prefix(self) -> String {
        format!("kc1{}{}.", hexadecimal(&self.owner), self.key())
    }
    fn parse_prefix(text: &str) -> Result<Self> {
        if text.len() != 68 || !text.starts_with("kc1") || !text.ends_with('.') || !text.is_ascii()
        {
            return Err(Error::CursorInvalidated);
        }
        Ok(Self {
            owner: unhex(&text[3..35])?,
            id: unhex(&text[35..67])?,
        })
    }
    fn parse_token(text: &str) -> Result<(Self, u32)> {
        if text.len() != 76 || !text.is_ascii() {
            return Err(Error::CursorInvalidated);
        }
        let handle = Self::parse_prefix(&text[..68])?;
        let position = u32::from_be_bytes(unhex::<4>(&text[68..])?);
        Ok((handle, position))
    }
}

/// A bounded page/source read lease. Dropping this descriptor does not write.
/// The coordinator must finish it or sweep its deadline after client loss.
pub struct CursorRead {
    handle: CursorHandle,
    id: [u8; 16],
    pin: GenerationPin,
    until: Duration,
    expires: Duration,
}
impl CursorRead {
    pub fn pin(&self) -> &GenerationPin {
        &self.pin
    }
    pub fn remaining(&self) -> Result<Duration> {
        let now = crate::execution::continuous_time()?;
        if now >= self.expires {
            return Err(Error::CursorInvalidated);
        }
        self.until
            .checked_sub(now)
            .filter(|remaining| !remaining.is_zero())
            .ok_or(Error::DeadlineExceeded)
    }
}

struct CursorEntry {
    pin: GenerationPin,
    created: Duration,
    touched: Duration,
    reads: BTreeMap<[u8; 16], Duration>,
    result_bytes_reserved: u64,
    retiring: bool,
    creator: Option<[u8; 16]>,
}
impl CursorEntry {
    fn expired(&self, now: Duration) -> bool {
        now < self.created
            || now < self.touched
            || now.saturating_sub(self.created) >= ABSOLUTE_LIFETIME
            || now.saturating_sub(self.touched) >= IDLE_LIFETIME
    }
}

/// One registry per coordinator/user, shared across all workspaces. Call only
/// under its serialized admission lock and sole-writer fence. This supplies
/// lifetime/count/read-lease and shared byte policy. Coordinator control,
/// publication and saved-result files still need integration before pagination.
pub struct CursorRegistry {
    owner: [u8; 16],
    maximum: usize,
    maximum_bytes: u64,
    entries: BTreeMap<[u8; 16], CursorEntry>,
    publication_waiting: bool,
    files: Option<result_store::ResultStore>,
}
impl CursorRegistry {
    pub fn new(owner: [u8; 16], maximum: usize) -> Result<Self> {
        Self::with_budget(owner, maximum, DEFAULT_RETENTION_BYTES)
    }
    pub fn with_budget(owner: [u8; 16], maximum: usize, maximum_bytes: u64) -> Result<Self> {
        if maximum == 0 || maximum_bytes == 0 {
            return Err(Error::InvalidRequest(
                "cursor count and retention byte limits must be positive".into(),
            ));
        }
        Ok(Self {
            owner,
            maximum,
            maximum_bytes,
            entries: BTreeMap::new(),
            publication_waiting: false,
            files: None,
        })
    }

    pub fn admit(&mut self, database: &Path) -> Result<CursorHandle> {
        self.admit_controlled(database, Execution::new(Default::default(), None)?)
    }
    pub(crate) fn admit_controlled(
        &mut self,
        database: &Path,
        execution: Execution,
    ) -> Result<CursorHandle> {
        self.admit_inner(database, crate::execution::continuous_time()?, execution)
    }
    fn admit_inner(
        &mut self,
        database: &Path,
        now: Duration,
        execution: Execution,
    ) -> Result<CursorHandle> {
        self.sweep_inner(now, execution)?;
        self.check_admission()?;
        while self.entries.len() >= self.maximum {
            if !self.remove_idle_except(None, execution)? {
                return Err(Error::CursorLimit);
            }
        }
        let retention =
            GenerationRetention::activate_controlled(database, self.owner, Some(execution))?;
        let pin = retention.pin_controlled(Some(execution))?;
        if self.entries.contains_key(&pin.id) {
            retention.release_controlled(&pin, Some(execution))?;
            return Err(Error::Protocol("cursor identity collision".into()));
        }
        let handle = CursorHandle {
            owner: self.owner,
            id: pin.id,
        };
        self.entries.insert(
            pin.id,
            CursorEntry {
                pin,
                created: now,
                touched: now,
                reads: BTreeMap::new(),
                result_bytes_reserved: 0,
                retiring: false,
                creator: None,
            },
        );
        if let Err(error) = self.make_space(None, Some(handle.id), execution) {
            // Cleanup shares this request budget; an expired one is retried by
            // periodic sweeping rather than starting an unbounded extra wait.
            let _ = self.retire(handle.id, execution);
            return Err(error);
        }
        Ok(handle)
    }

    /// Remaining overall request budget bounds abandoned borrows as well as
    /// execution. Multiple reads may share a cursor without changing its scope.
    pub fn borrow(
        &mut self,
        database: &Path,
        handle: CursorHandle,
        remaining: Duration,
    ) -> Result<CursorRead> {
        self.borrow_inner(
            database,
            handle,
            remaining,
            crate::execution::continuous_time()?,
            Execution::new(Default::default(), None)?,
        )
    }
    pub(crate) fn borrow_controlled(
        &mut self,
        database: &Path,
        handle: CursorHandle,
        execution: Execution,
    ) -> Result<CursorRead> {
        self.borrow_inner(
            database,
            handle,
            execution.remaining()?,
            crate::execution::continuous_time()?,
            execution,
        )
    }
    fn borrow_inner(
        &mut self,
        database: &Path,
        handle: CursorHandle,
        remaining: Duration,
        now: Duration,
        execution: Execution,
    ) -> Result<CursorRead> {
        self.sweep_inner(now, execution)?;
        self.check_admission()?;
        if remaining.is_zero() {
            return Err(Error::DeadlineExceeded);
        }
        if handle.owner != self.owner
            || !self.entries.get(&handle.id).is_some_and(|entry| {
                entry.pin.database == database && !entry.expired(now) && !entry.retiring
            })
        {
            return Err(Error::CursorInvalidated);
        }
        // Bound bookkeeping even if many clients concurrently retry one token.
        // This is admission, not a claim to bound query peak memory.
        if self
            .entries
            .values()
            .map(|entry| entry.reads.len())
            .sum::<usize>()
            >= self.maximum
        {
            return Err(Error::ResourceLimited(
                "cursor read concurrency limit reached; retry after an active read finishes".into(),
            ));
        }
        let entry = self.entries.get_mut(&handle.id).expect("checked cursor");
        execution.check()?;
        let id = nonce()?;
        if entry.reads.contains_key(&id) {
            return Err(Error::Protocol("cursor read identity collision".into()));
        }
        let until = now
            .saturating_add(remaining.min(Duration::from_millis(u64::from(
                crate::execution::MAX_DEADLINE_MS,
            ))))
            .min(entry.created.saturating_add(ABSOLUTE_LIFETIME));
        entry.reads.insert(id, until);
        entry.touched = now;
        Ok(CursorRead {
            handle,
            id,
            pin: entry.pin.clone(),
            until,
            expires: entry.created.saturating_add(ABSOLUTE_LIFETIME),
        })
    }

    pub fn finish(&mut self, read: &CursorRead) -> Result<()> {
        self.finish_controlled(read, Execution::new(Default::default(), None)?)
    }
    pub(crate) fn finish_controlled(
        &mut self,
        read: &CursorRead,
        execution: Execution,
    ) -> Result<()> {
        self.finish_inner(read, crate::execution::continuous_time()?, execution)
    }
    fn finish_inner(
        &mut self,
        read: &CursorRead,
        now: Duration,
        execution: Execution,
    ) -> Result<()> {
        self.sweep_inner(now, execution)?;
        if read.handle.owner != self.owner {
            return Err(Error::CursorInvalidated);
        }
        let entry = self
            .entries
            .get_mut(&read.handle.id)
            .ok_or(Error::CursorInvalidated)?;
        entry
            .reads
            .remove(&read.id)
            .ok_or(Error::CursorInvalidated)?;
        entry.touched = now;
        Ok(())
    }

    /// Host publication must set this before waiting for active reads, and
    /// clear it after cutover/abort. Never wait in a serving write transaction.
    pub fn set_publication_waiting(&mut self, waiting: bool) {
        self.publication_waiting = waiting;
    }
    fn check_admission(&self) -> Result<()> {
        if self.publication_waiting {
            return Err(Error::ResourceLimited(
                "cursor admission deferred for pending index publication; retry shortly".into(),
            ));
        }
        Ok(())
    }

    /// Release the least recently used idle cursor. False means active reads
    /// must finish/expire before publication can reclaim another cursor.
    pub fn evict_idle(&mut self) -> Result<bool> {
        let execution = Execution::new(Default::default(), None)?;
        self.sweep_inner(crate::execution::continuous_time()?, execution)?;
        self.remove_idle_except(None, execution)
    }
    fn remove_idle_except(
        &mut self,
        protected: Option<[u8; 16]>,
        execution: Execution,
    ) -> Result<bool> {
        let oldest = self
            .entries
            .iter()
            .filter(|(id, entry)| entry.reads.is_empty() && Some(**id) != protected)
            .min_by_key(|(id, entry)| (!entry.retiring, entry.touched, entry.created, **id))
            .map(|(id, _)| *id);
        if let Some(id) = oldest {
            self.retire(id, execution)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Reserve before retaining result files. Reservations only grow; they
    /// cover in-progress files too and remain charged until cursor cleanup.
    /// This is storage capacity, not permission to reset graph work budgets.
    pub fn reserve_result(&mut self, handle: CursorHandle, bytes: u64) -> Result<()> {
        self.reserve_controlled(handle, bytes, Execution::new(Default::default(), None)?)
    }
    pub(crate) fn reserve_controlled(
        &mut self,
        handle: CursorHandle,
        bytes: u64,
        execution: Execution,
    ) -> Result<()> {
        self.sweep_inner(crate::execution::continuous_time()?, execution)?;
        self.check_admission()?;
        if handle.owner != self.owner {
            return Err(Error::CursorInvalidated);
        }
        let entry = self
            .entries
            .get_mut(&handle.id)
            .ok_or(Error::CursorInvalidated)?;
        if bytes > self.maximum_bytes {
            return Err(Error::ResourceLimited(
                "saved graph result exceeds the shared cursor retention budget".into(),
            ));
        }
        let previous = entry.result_bytes_reserved;
        entry.result_bytes_reserved = previous.max(bytes);
        if let Err(error) = self.make_space(None, Some(handle.id), execution) {
            if let Some(entry) = self.entries.get_mut(&handle.id) {
                entry.result_bytes_reserved = previous;
            }
            return Err(error);
        }
        Ok(())
    }

    /// Index data is deduplicated within each database; result reservations
    /// share this same pool across all workspaces. A preview must remain
    /// serialized with pin admission/publication until the actual cutover.
    pub fn retained_bytes(&self) -> Result<u64> {
        self.budget_usage(
            None,
            crate::execution::Execution::new(Default::default(), None)?,
        )
    }
    fn budget_usage(
        &self,
        proposed: Option<(&Path, u64)>,
        execution: crate::execution::Execution,
    ) -> Result<u64> {
        let mut total = 0u64;
        let mut databases = std::collections::BTreeSet::new();
        for entry in self.entries.values() {
            total = accounting::add(total, entry.result_bytes_reserved)?;
            databases.insert(entry.pin.database.as_path());
        }
        for database in databases {
            execution.check()?;
            let retention = GenerationRetention {
                database: database.into(),
                owner: self.owner,
            };
            let candidate = proposed
                .filter(|(path, _)| *path == database)
                .map(|(_, generation)| generation);
            total = accounting::add(
                total,
                retention
                    .usage_controlled(candidate, execution)?
                    .total_bytes()?,
            )?;
        }
        execution.check()?;
        Ok(total)
    }
    fn make_space(
        &mut self,
        proposed: Option<(&Path, u64)>,
        protected: Option<[u8; 16]>,
        execution: crate::execution::Execution,
    ) -> Result<()> {
        while self.budget_usage(proposed, execution)? > self.maximum_bytes {
            if !self.remove_idle_except(protected, execution)? {
                return Err(Error::ResourceLimited(
                    "cursor retention budget is busy; retry after an active read finishes".into(),
                ));
            }
        }
        Ok(())
    }

    /// True permits a serialized cutover within the current budget. False
    /// fences new admissions and asks the host to wait for bounded reads.
    /// No serving write transaction may be held while calling or waiting.
    pub fn prepare_publication(&mut self, database: &Path, generation: u64) -> Result<bool> {
        self.prepare_controlled(
            database,
            generation,
            Execution::new(Default::default(), None)?,
        )
    }
    pub(crate) fn prepare_controlled(
        &mut self,
        database: &Path,
        generation: u64,
        execution: Execution,
    ) -> Result<bool> {
        self.sweep_inner(crate::execution::continuous_time()?, execution)?;
        loop {
            if self.budget_usage(Some((database, generation)), execution)? <= self.maximum_bytes {
                return Ok(true);
            }
            self.publication_waiting = true;
            if !self.remove_idle_except(None, execution)? {
                return Ok(false);
            }
        }
    }

    pub fn sweep(&mut self) -> Result<()> {
        self.sweep_controlled(Execution::new(Default::default(), None)?)
    }
    pub(crate) fn sweep_controlled(&mut self, execution: Execution) -> Result<()> {
        self.sweep_inner(crate::execution::continuous_time()?, execution)
    }
    fn sweep_inner(&mut self, now: Duration, execution: Execution) -> Result<()> {
        execution.check()?;
        let mut expired = Vec::new();
        for (id, entry) in &mut self.entries {
            if now < entry.created || now < entry.touched {
                entry.reads.clear();
            } else {
                entry.reads.retain(|_, until| *until > now);
            }
            if entry
                .creator
                .is_some_and(|id| !entry.reads.contains_key(&id))
            {
                entry.retiring = true;
            }
            if (entry.expired(now) || entry.retiring) && entry.reads.is_empty() {
                expired.push(*id);
            }
        }
        for id in expired {
            self.retire(id, execution)?;
        }
        Ok(())
    }
    fn retire(&mut self, id: [u8; 16], execution: Execution) -> Result<()> {
        self.entries.get_mut(&id).expect("selected cursor").retiring = true;
        execution.check()?;
        let entry = self.entries.get(&id).expect("selected cursor");
        if let Some(files) = &mut self.files {
            files.remove(&hexadecimal(&entry.pin.id))?;
        }
        let result = GenerationRetention {
            database: entry.pin.database.clone(),
            owner: self.owner,
        }
        .release_controlled(&entry.pin, Some(execution));
        match result {
            // A different owner has already invalidated this incarnation's
            // pins. Other cleanup failures keep the entry for a later retry.
            Ok(()) | Err(Error::CursorInvalidated) => {}
            Err(error) => return Err(error),
        }
        self.entries.remove(&id);
        Ok(())
    }
}

#[cfg(test)]
impl CursorRegistry {
    fn admit_at(&mut self, database: &Path, now: Duration) -> Result<CursorHandle> {
        self.admit_inner(database, now, Execution::new(Default::default(), None)?)
    }
    fn borrow_at(
        &mut self,
        database: &Path,
        handle: CursorHandle,
        remaining: Duration,
        now: Duration,
    ) -> Result<CursorRead> {
        self.borrow_inner(
            database,
            handle,
            remaining,
            now,
            Execution::new(Default::default(), None)?,
        )
    }
    fn finish_at(&mut self, read: &CursorRead, now: Duration) -> Result<()> {
        self.finish_inner(read, now, Execution::new(Default::default(), None)?)
    }
    fn sweep_at(&mut self, now: Duration) -> Result<()> {
        self.sweep_inner(now, Execution::new(Default::default(), None)?)
    }
    fn remove_oldest_idle(&mut self) -> Result<bool> {
        self.remove_idle_except(None, Execution::new(Default::default(), None)?)
    }
}

/// Call only while holding the coordinator's sole-writer fence. Ordinary
/// embedded query consumers never write retention state directly.
pub struct GenerationRetention {
    database: PathBuf,
    owner: [u8; 16],
}

/// Read-only descriptor. Owning/dropping this value does not perform writes.
#[derive(Clone)]
pub struct GenerationPin {
    pub(crate) database: PathBuf,
    pub(crate) owner: [u8; 16],
    pub(crate) id: [u8; 16],
    generation: u64,
}
impl GenerationPin {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn open_view(&self) -> Result<ReadView> {
        ReadView::open_pinned(self)
    }
}

pub(crate) fn storage_active(connection: &Connection) -> Result<bool> {
    let present: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='cursor_retention_state')", [], |r| r.get(0))?;
    if !present {
        return Ok(false);
    }
    let version: Option<u32> = connection
        .query_row(
            "SELECT version FROM cursor_retention_state WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if version != Some(RETENTION_VERSION) {
        return Err(Error::UpdateRequired(
            "cursor retention storage is incompatible; update the coordinator and query libraries"
                .into(),
        ));
    }
    Ok(true)
}

fn open_writer_controlled(path: &Path, execution: Option<Execution>) -> Result<Connection> {
    if !path.is_file() {
        return Err(Error::NotReady);
    }
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    if let Some(execution) = execution {
        execution.check()?;
        connection.progress_handler(1_000, Some(move || execution.check().is_err()))?;
        connection.busy_handler(Some(move |count| {
            if count >= 2_000 {
                return false;
            }
            match execution.remaining() {
                Ok(remaining) => {
                    std::thread::sleep(remaining.min(Duration::from_millis(1)));
                    true
                }
                Err(_) => false,
            }
        }))?;
    } else {
        connection.busy_timeout(Duration::from_secs(2))?;
    }
    connection.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-2048;",
    )?;
    let version: u32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if !(2..=crate::store::SCHEMA_VERSION as u32).contains(&version) {
        return Err(Error::UpdateRequired(format!(
            "index format {version} cannot retain cursor generations"
        )));
    }
    Ok(connection)
}

impl GenerationRetention {
    /// Activate a coordinator incarnation, invalidating the previous one's pins.
    /// The owner must be independent of optional live-freshness acceleration.
    pub fn activate(database: &Path, owner: [u8; 16]) -> Result<Self> {
        Self::activate_controlled(database, owner, None)
    }
    fn activate_controlled(
        database: &Path,
        owner: [u8; 16],
        execution: Option<Execution>,
    ) -> Result<Self> {
        let mut connection = open_writer_controlled(database, execution)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active = storage_active(&tx)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS staging_generations(generation INTEGER PRIMARY KEY REFERENCES generations(id) ON DELETE CASCADE);
          CREATE TABLE IF NOT EXISTS cursor_retention_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL,owner BLOB NOT NULL CHECK(length(owner)=16));
          CREATE TABLE IF NOT EXISTS cursor_generation_pins(id BLOB PRIMARY KEY CHECK(length(id)=16),generation INTEGER NOT NULL REFERENCES generations(id) ON DELETE CASCADE,owner BLOB NOT NULL CHECK(length(owner)=16));
          CREATE INDEX IF NOT EXISTS cursor_pin_generation ON cursor_generation_pins(generation);
          CREATE TABLE IF NOT EXISTS cursor_unit_costs_v1(owner_generation INTEGER,compilation_id TEXT,bytes INTEGER NOT NULL CHECK(bytes>=0),store_format INTEGER NOT NULL,PRIMARY KEY(owner_generation,compilation_id),FOREIGN KEY(owner_generation,compilation_id) REFERENCES compilations(generation,id) ON DELETE CASCADE);")?;
        let prior: Option<Vec<u8>> = if active {
            tx.query_row(
                "SELECT owner FROM cursor_retention_state WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?
        } else {
            None
        };
        if prior.as_deref() != Some(owner.as_slice()) {
            tx.execute("DELETE FROM cursor_generation_pins", [])?;
            tx.execute("INSERT INTO cursor_retention_state VALUES(1,?,?) ON CONFLICT(singleton) DO UPDATE SET version=excluded.version,owner=excluded.owner", params![RETENTION_VERSION, owner.as_slice()])?;
            tx.execute("DELETE FROM generation_units WHERE generation IS NOT (SELECT current_generation FROM state) AND generation NOT IN (SELECT generation FROM staging_generations)", [])?;
            crate::store::reclaim(&tx)?;
        }
        if let Some(execution) = execution {
            execution.check()?;
        }
        tx.commit()?;
        Ok(Self {
            database: database.to_path_buf(),
            owner,
        })
    }

    fn check_owner(&self, connection: &Connection) -> Result<()> {
        if !storage_active(connection)? {
            return Err(Error::CursorInvalidated);
        }
        let owner: Vec<u8> = connection.query_row(
            "SELECT owner FROM cursor_retention_state WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if owner.as_slice() != self.owner {
            return Err(Error::CursorInvalidated);
        }
        Ok(())
    }

    pub fn pin_current(&self) -> Result<GenerationPin> {
        self.pin_controlled(None)
    }
    fn pin_controlled(&self, execution: Option<Execution>) -> Result<GenerationPin> {
        let id = nonce()?;
        let mut connection = open_writer_controlled(&self.database, execution)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.check_owner(&tx)?;
        let generation: Option<u64> = tx.query_row(
            "SELECT current_generation FROM state WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let generation = generation.ok_or(Error::NotReady)?;
        tx.execute(
            "INSERT INTO cursor_generation_pins VALUES(?,?,?)",
            params![id.as_slice(), generation, self.owner.as_slice()],
        )?;
        if let Some(execution) = execution {
            execution.check()?;
        }
        tx.commit()?;
        Ok(GenerationPin {
            database: self.database.clone(),
            owner: self.owner,
            id,
            generation,
        })
    }

    pub fn release(&self, pin: &GenerationPin) -> Result<()> {
        self.release_controlled(pin, None)
    }
    fn release_controlled(&self, pin: &GenerationPin, execution: Option<Execution>) -> Result<()> {
        if pin.database != self.database || pin.owner != self.owner {
            return Err(Error::CursorInvalidated);
        }
        let mut connection = open_writer_controlled(&self.database, execution)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.check_owner(&tx)?;
        tx.execute(
            "DELETE FROM cursor_generation_pins WHERE id=? AND owner=?",
            params![pin.id.as_slice(), self.owner.as_slice()],
        )?;
        tx.execute("DELETE FROM generation_units WHERE generation=? AND generation IS NOT (SELECT current_generation FROM state) AND NOT EXISTS(SELECT 1 FROM cursor_generation_pins WHERE generation=?)", params![pin.generation, pin.generation])?;
        crate::store::reclaim(&tx)?;
        if let Some(execution) = execution {
            execution.check()?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::{
        protocol::{envelope::Payload, *},
        query::QueryRequest,
        store::Writer,
    };
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(super) fn database() -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/cursor-tests")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ))
            .join("index.db");
        publish(&path, "original source");
        path
    }
    pub(super) fn publish(path: &Path, text: &str) {
        staged(path, text).publish().unwrap();
    }
    pub(super) fn staged(path: &Path, text: &str) -> Writer {
        let mut writer = Writer::open(path).unwrap();
        writer
            .stage(Payload::Compilation(Compilation {
                id: "comp".into(),
                language: "typescript".into(),
                ..Default::default()
            }))
            .unwrap();
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
        writer
    }
    fn at(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }
    fn source(pin: &GenerationPin) -> String {
        let request: QueryRequest =
            serde_json::from_str(r#"{"op":"source","artifact_id":"source","freshness_wait_ms":0}"#)
                .unwrap();
        pin.open_view()
            .unwrap()
            .query(request)
            .unwrap()
            .source
            .unwrap()
            .text
    }

    #[test]
    fn count_and_lru_are_shared_across_workspaces_and_active_reads_are_protected() {
        let a = database();
        let b = database();
        let mut cursors = CursorRegistry::new([1; 16], 2).unwrap();
        let oldest = cursors.admit_at(&a, at(0)).unwrap();
        let newer = cursors.admit_at(&b, at(1)).unwrap();
        let read = cursors.borrow_at(&a, oldest, at(10), at(2)).unwrap();
        publish(&a, "replacement source");
        assert_eq!(source(read.pin()), "original source");
        let latest = cursors.admit_at(&b, at(3)).unwrap();
        assert!(matches!(
            cursors.borrow_at(&b, newer, at(1), at(3)),
            Err(Error::CursorInvalidated)
        ));
        assert!(matches!(
            cursors.borrow_at(&b, oldest, at(1), at(3)),
            Err(Error::CursorInvalidated)
        ));
        let latest_read = cursors.borrow_at(&b, latest, at(10), at(3)).unwrap();
        assert!(matches!(
            cursors.admit_at(&a, at(3)),
            Err(Error::CursorLimit)
        ));
        cursors.finish_at(&read, at(4)).unwrap();
        cursors.admit_at(&a, at(5)).unwrap();
        assert!(matches!(
            read.pin().open_view(),
            Err(Error::CursorInvalidated)
        ));
        assert_eq!(source(latest_read.pin()), "original source");
        cursors.finish_at(&latest_read, at(6)).unwrap();
    }

    #[test]
    fn idle_expiry_and_absolute_expiry_survive_activity_and_release_retained_source() {
        let path = database();
        let mut cursors = CursorRegistry::new([1; 16], 2).unwrap();
        let idle = cursors.admit_at(&path, at(0)).unwrap();
        let read = cursors.borrow_at(&path, idle, at(1), at(119)).unwrap();
        let saved = read.pin.clone();
        cursors
            .finish_at(&read, Duration::from_millis(119_500))
            .unwrap();
        publish(&path, "new source");
        cursors.sweep_at(Duration::from_millis(239_499)).unwrap();
        assert_eq!(source(&saved), "original source");
        cursors.sweep_at(Duration::from_millis(239_500)).unwrap();
        assert!(matches!(saved.open_view(), Err(Error::CursorInvalidated)));
        let active = cursors.admit_at(&path, at(300)).unwrap();
        for time in [400, 500, 600, 700, 800, 899] {
            let read = cursors.borrow_at(&path, active, at(1), at(time)).unwrap();
            cursors
                .finish_at(&read, Duration::from_millis(time * 1000 + 500))
                .unwrap();
        }
        assert!(matches!(
            cursors.borrow_at(&path, active, at(1), at(900)),
            Err(Error::CursorInvalidated)
        ));
        let connection = Connection::open(&path).unwrap();
        let retained: u64 = connection
            .query_row("SELECT count(*) FROM cursor_generation_pins", [], |r| {
                r.get(0)
            })
            .unwrap();
        let sources: u64 = connection
            .query_row("SELECT count(*) FROM contents", [], |r| r.get(0))
            .unwrap();
        assert_eq!((retained, sources), (0, 1));
    }

    #[test]
    fn publication_fence_blocks_admission_until_a_bounded_read_finishes_or_is_abandoned() {
        let path = database();
        let mut cursors = CursorRegistry::new([1; 16], 2).unwrap();
        let handle = cursors.admit_at(&path, at(0)).unwrap();
        let first = cursors.borrow_at(&path, handle, at(5), at(1)).unwrap();
        let abandoned = cursors.borrow_at(&path, handle, at(5), at(1)).unwrap();
        assert!(matches!(
            cursors.borrow_at(&path, handle, at(5), at(1)),
            Err(Error::ResourceLimited(_))
        ));
        cursors.set_publication_waiting(true);
        assert!(matches!(
            cursors.admit_at(&path, at(2)),
            Err(Error::ResourceLimited(_))
        ));
        assert!(matches!(
            cursors.borrow_at(&path, handle, at(5), at(2)),
            Err(Error::ResourceLimited(_))
        ));
        assert!(!cursors.remove_oldest_idle().unwrap());
        cursors.finish_at(&first, at(3)).unwrap();
        assert!(!cursors.remove_oldest_idle().unwrap());
        cursors.sweep_at(at(6)).unwrap();
        assert!(cursors.remove_oldest_idle().unwrap());
        assert!(matches!(
            cursors.finish_at(&abandoned, at(6)),
            Err(Error::CursorInvalidated)
        ));
        cursors.set_publication_waiting(false);
        cursors.admit_at(&path, at(7)).unwrap();
    }

    #[test]
    fn abandoned_read_has_request_deadline_and_never_extends_absolute_lifetime() {
        let path = database();
        let mut cursors = CursorRegistry::new([1; 16], 1).unwrap();
        let handle = cursors.admit_at(&path, at(0)).unwrap();
        assert!(matches!(
            cursors.borrow_at(&path, handle, Duration::ZERO, at(1)),
            Err(Error::DeadlineExceeded)
        ));
        let read = cursors.borrow_at(&path, handle, at(2), at(1)).unwrap();
        assert_eq!(read.until, at(3));
        cursors.sweep_at(at(3)).unwrap();
        assert!(matches!(
            cursors.finish_at(&read, at(3)),
            Err(Error::CursorInvalidated)
        ));
        for time in [100, 200, 300, 400, 500] {
            let read = cursors.borrow_at(&path, handle, at(1), at(time)).unwrap();
            cursors.finish_at(&read, at(time)).unwrap();
        }
        let read = cursors.borrow_at(&path, handle, at(120), at(599)).unwrap();
        assert_eq!(read.until, at(600));
        cursors.sweep_at(at(600)).unwrap();
        assert!(matches!(
            read.pin().open_view(),
            Err(Error::CursorInvalidated)
        ));
    }

    #[test]
    fn reversed_time_invalidates_borrows_and_failed_cleanup_keeps_its_accounting() {
        let path = database();
        let mut cursors = CursorRegistry::new([1; 16], 1).unwrap();
        let handle = cursors.admit_at(&path, at(100)).unwrap();
        let read = cursors.borrow_at(&path, handle, at(10), at(101)).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("UPDATE cursor_retention_state SET version=99", [])
            .unwrap();
        assert!(matches!(
            cursors.sweep_at(at(99)),
            Err(Error::UpdateRequired(_))
        ));
        assert_eq!(cursors.entries.len(), 1);
        assert!(cursors.entries[&handle.id].reads.is_empty());
        connection
            .execute("UPDATE cursor_retention_state SET version=1", [])
            .unwrap();
        cursors.sweep_at(at(99)).unwrap();
        assert!(cursors.entries.is_empty());
        assert!(matches!(
            read.pin().open_view(),
            Err(Error::CursorInvalidated)
        ));
        cursors.admit_at(&path, at(102)).unwrap();
    }

    fn units(writer: &mut Writer, definitions: &[(&str, &str)]) {
        for (id, text) in definitions {
            writer
                .stage(Payload::Compilation(Compilation {
                    id: (*id).into(),
                    project_path: (*id).into(),
                    language: "typescript".into(),
                    ..Default::default()
                }))
                .unwrap();
            writer
                .stage(Payload::Source(SourceArtifact {
                    id: format!("source-{id}"),
                    path: format!("{id}.ts"),
                    compilation_id: (*id).into(),
                    sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
                    utf8: text.as_bytes().into(),
                    ..Default::default()
                }))
                .unwrap();
            writer
                .stage(Payload::Facts(FactBatch {
                    compilation_id: (*id).into(),
                    symbols: vec![Symbol {
                        id: "symbol".into(),
                        name: "value".into(),
                        kind: "function".into(),
                        signature: format!("value(): {text}"),
                        ..Default::default()
                    }],
                    occurrences: vec![Occurrence {
                        symbol_id: "symbol".into(),
                        artifact_id: format!("source-{id}"),
                        role: "declaration".into(),
                        start_byte: 0,
                        end_byte: text.len() as u64,
                        ..Default::default()
                    }],
                    ..Default::default()
                }))
                .unwrap();
            writer
                .stage(Payload::Report(AnalysisReport {
                    compilation_id: (*id).into(),
                    status: "complete".into(),
                    ..Default::default()
                }))
                .unwrap();
        }
    }
    fn staged_generation(path: &Path) -> u64 {
        Connection::open(path)
            .unwrap()
            .query_row("SELECT generation FROM staging_generations", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    #[test]
    fn retention_projection_excludes_current_and_staged_shared_units_and_sources() {
        let path = database();
        let mut writer = Writer::open(&path).unwrap();
        units(
            &mut writer,
            &[("a", "shared old source"), ("b", "shared old source")],
        );
        writer.publish().unwrap();
        let retention = GenerationRetention::activate(&path, [1; 16]).unwrap();
        let pin = retention.pin_current().unwrap();
        let sibling = retention.pin_current().unwrap();
        let current = retention.retained_usage(None, None).unwrap();
        assert_eq!(
            (
                current.units,
                current.unit_bytes,
                current.artifact_bytes,
                current.source_bytes,
                current.manifest_bytes
            ),
            (0, 0, 0, 0, 0)
        );
        assert!(current.pin_bytes > 0);
        let mut replacement = Writer::open_incremental(&path, &["a".into()]).unwrap();
        units(&mut replacement, &[("a", "new a source")]);
        let generation = staged_generation(&path);
        let forecast = retention.retained_usage(Some(generation), None).unwrap();
        assert_eq!(forecast.units, 1);
        assert_eq!(forecast.source_bytes, 0); // b still needs the identical text
        assert!(forecast.unit_bytes > 0 && forecast.artifact_bytes > 0);
        assert!(forecast.manifest_bytes > 0);
        replacement.publish().unwrap();
        let actual = retention.retained_usage(None, None).unwrap();
        assert_eq!(
            forecast.total_bytes().unwrap(),
            actual.total_bytes().unwrap()
        );
        assert_eq!(actual.proposed_generation, generation);
        retention.release(&sibling).unwrap();
        let one = retention.retained_usage(None, None).unwrap();
        assert_eq!(
            (
                one.unit_bytes,
                one.source_bytes,
                one.artifact_bytes,
                one.manifest_bytes
            ),
            (
                actual.unit_bytes,
                actual.source_bytes,
                actual.artifact_bytes,
                actual.manifest_bytes
            )
        );
        assert_eq!(one.pin_bytes * 2, actual.pin_bytes);
        retention.release(&pin).unwrap();
        assert_eq!(
            retention
                .retained_usage(None, None)
                .unwrap()
                .total_bytes()
                .unwrap(),
            0
        );
    }

    #[test]
    fn retained_source_cost_uses_utf8_bytes_deduplicates_hashes_and_reclaims_summaries() {
        let path = database();
        let original = "😀é shared source";
        let mut writer = Writer::open(&path).unwrap();
        units(&mut writer, &[("a", original), ("b", original)]);
        writer.publish().unwrap();
        let retention = GenerationRetention::activate(&path, [1; 16]).unwrap();
        let pin = retention.pin_current().unwrap();
        let sibling = retention.pin_current().unwrap();
        let mut replacement = Writer::open(&path).unwrap();
        units(
            &mut replacement,
            &[("a", "replacement"), ("b", "replacement")],
        );
        replacement.publish().unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_second_summary BEFORE INSERT ON cursor_unit_costs_v1 WHEN (SELECT count(*) FROM cursor_unit_costs_v1)>0 BEGIN SELECT raise(ABORT,'fixture cache interruption'); END;").unwrap();
        assert!(matches!(
            retention.retained_usage(None, None),
            Err(Error::Sqlite(_))
        ));
        let completed: u64 = connection
            .query_row("SELECT count(*) FROM cursor_unit_costs_v1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(completed, 1);
        connection
            .execute_batch("DROP TRIGGER fail_second_summary")
            .unwrap();
        let usage = retention.retained_usage(None, None).unwrap();
        assert_eq!(usage.units, 2);
        assert_eq!(usage.source_bytes, 64 + 64 + original.len() as u64);
        assert_eq!(usage, retention.retained_usage(None, None).unwrap());
        let summaries: u64 = connection
            .query_row("SELECT count(*) FROM cursor_unit_costs_v1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(summaries, 2);
        connection
            .execute("UPDATE cursor_unit_costs_v1 SET store_format=5,bytes=0", [])
            .unwrap();
        assert_eq!(usage, retention.retained_usage(None, None).unwrap());
        retention.release(&pin).unwrap();
        assert_eq!(
            retention.retained_usage(None, None).unwrap().unit_bytes,
            usage.unit_bytes
        );
        retention.release(&sibling).unwrap();
        let summaries: u64 = connection
            .query_row("SELECT count(*) FROM cursor_unit_costs_v1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(summaries, 0);
        assert_eq!(
            retention
                .retained_usage(None, None)
                .unwrap()
                .total_bytes()
                .unwrap(),
            0
        );
    }

    #[test]
    fn retained_usage_rejects_expired_budgets_unknown_publications_and_old_owners() {
        let path = database();
        let retention = GenerationRetention::activate(&path, [1; 16]).unwrap();
        retention.pin_current().unwrap();
        assert!(matches!(
            retention.retained_usage(None, Some(0)),
            Err(Error::DeadlineExceeded)
        ));
        assert!(matches!(
            retention.retained_usage(Some(1_000_000), None),
            Err(Error::InvalidRequest(_))
        ));
        GenerationRetention::activate(&path, [2; 16]).unwrap();
        assert!(matches!(
            retention.retained_usage(None, None),
            Err(Error::CursorInvalidated)
        ));
    }

    #[test]
    fn result_reservations_share_one_budget_with_indexes_across_workspaces() {
        let a = database();
        let b = database();
        let mut cursors = CursorRegistry::new([1; 16], 3).unwrap();
        let first = cursors.admit(&a).unwrap();
        let second = cursors.admit(&b).unwrap();
        let pins = cursors.retained_bytes().unwrap();
        cursors.maximum_bytes = pins + 100;
        let active = cursors.borrow(&a, first, at(5)).unwrap();
        cursors.reserve_result(first, 80).unwrap();
        assert_eq!(cursors.retained_bytes().unwrap(), pins + 80);
        assert!(matches!(
            cursors.reserve_result(second, pins + 101),
            Err(Error::ResourceLimited(_))
        ));
        assert_eq!(cursors.entries.len(), 2);
        let other = cursors.borrow(&b, second, at(5)).unwrap();
        assert!(matches!(
            cursors.reserve_result(second, 80),
            Err(Error::ResourceLimited(_))
        ));
        assert_eq!(cursors.entries[&second.id].result_bytes_reserved, 0);
        cursors.finish(&active).unwrap();
        cursors.reserve_result(second, 80).unwrap();
        assert!(!cursors.entries.contains_key(&first.id));
        assert_eq!(cursors.retained_bytes().unwrap(), pins / 2 + 80);
        cursors.reserve_result(second, 10).unwrap();
        assert_eq!(cursors.entries[&second.id].result_bytes_reserved, 80);
        cursors.finish(&other).unwrap();
    }

    #[test]
    fn publication_forecast_waits_for_reads_then_reclaims_with_the_shared_result_pool() {
        let path = database();
        let mut cursors = CursorRegistry::new([1; 16], 1).unwrap();
        let handle = cursors.admit(&path).unwrap();
        cursors.reserve_result(handle, 100).unwrap();
        let active = cursors.borrow(&path, handle, at(5)).unwrap();
        let initial = cursors.retained_bytes().unwrap();
        let mut writer = Writer::open(&path).unwrap();
        units(&mut writer, &[("next", "replacement")]);
        let generation = staged_generation(&path);
        let execution = crate::execution::Execution::new(Default::default(), None).unwrap();
        let projected = cursors
            .budget_usage(Some((&path, generation)), execution)
            .unwrap();
        assert!(projected > initial);
        cursors.maximum_bytes = initial;
        assert!(!cursors.prepare_publication(&path, generation).unwrap());
        assert!(cursors.publication_waiting);
        assert!(matches!(
            cursors.borrow(&path, handle, at(5)),
            Err(Error::ResourceLimited(_))
        ));
        assert!(matches!(
            cursors.admit(&path),
            Err(Error::ResourceLimited(_))
        ));
        let old_view = active.pin().open_view().unwrap();
        cursors.finish(&active).unwrap();
        assert!(cursors.prepare_publication(&path, generation).unwrap());
        writer.publish().unwrap();
        cursors.set_publication_waiting(false);
        assert_eq!(cursors.retained_bytes().unwrap(), 0);
        assert!(matches!(
            active.pin().open_view(),
            Err(Error::CursorInvalidated)
        ));
        let request: QueryRequest =
            serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap();
        assert_eq!(
            old_view.query(request).unwrap().source.unwrap().text,
            "original source"
        );
    }
}
