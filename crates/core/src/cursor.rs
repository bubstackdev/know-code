//! Coordinator-owned generation retention and cursor lifecycle policy.
//! Portable control, replayable pages and byte accounting are not yet wired.
use crate::{Error, Result, query::ReadView};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

pub const RETENTION_VERSION: u32 = 1;
pub const DEFAULT_MAX_CURSORS: usize = 128;
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
/// lifetime/count/read-lease policy; byte accounting, control and pages still
/// need to be wired before advertising portable pagination.
pub struct CursorRegistry {
    owner: [u8; 16],
    maximum: usize,
    entries: BTreeMap<[u8; 16], CursorEntry>,
    publication_waiting: bool,
}
impl CursorRegistry {
    pub fn new(owner: [u8; 16], maximum: usize) -> Result<Self> {
        if maximum == 0 {
            return Err(Error::InvalidRequest(
                "cursor limit must be positive".into(),
            ));
        }
        Ok(Self {
            owner,
            maximum,
            entries: BTreeMap::new(),
            publication_waiting: false,
        })
    }

    pub fn admit(&mut self, database: &Path) -> Result<CursorHandle> {
        self.admit_at(database, crate::execution::continuous_time()?)
    }
    fn admit_at(&mut self, database: &Path, now: Duration) -> Result<CursorHandle> {
        self.sweep_at(now)?;
        self.check_admission()?;
        while self.entries.len() >= self.maximum {
            if !self.remove_oldest_idle()? {
                return Err(Error::CursorLimit);
            }
        }
        let retention = GenerationRetention::activate(database, self.owner)?;
        let pin = retention.pin_current()?;
        if self.entries.contains_key(&pin.id) {
            retention.release(&pin)?;
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
            },
        );
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
        self.borrow_at(
            database,
            handle,
            remaining,
            crate::execution::continuous_time()?,
        )
    }
    fn borrow_at(
        &mut self,
        database: &Path,
        handle: CursorHandle,
        remaining: Duration,
        now: Duration,
    ) -> Result<CursorRead> {
        self.sweep_at(now)?;
        self.check_admission()?;
        if remaining.is_zero() {
            return Err(Error::DeadlineExceeded);
        }
        if handle.owner != self.owner
            || !self
                .entries
                .get(&handle.id)
                .is_some_and(|entry| entry.pin.database == database && !entry.expired(now))
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
        self.finish_at(read, crate::execution::continuous_time()?)
    }
    fn finish_at(&mut self, read: &CursorRead, now: Duration) -> Result<()> {
        self.sweep_at(now)?;
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
        self.sweep()?;
        self.remove_oldest_idle()
    }
    fn remove_oldest_idle(&mut self) -> Result<bool> {
        let oldest = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.reads.is_empty())
            .min_by_key(|(id, entry)| (entry.touched, entry.created, **id))
            .map(|(id, _)| *id);
        if let Some(id) = oldest {
            self.retire(id)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn sweep(&mut self) -> Result<()> {
        self.sweep_at(crate::execution::continuous_time()?)
    }
    fn sweep_at(&mut self, now: Duration) -> Result<()> {
        let mut expired = Vec::new();
        for (id, entry) in &mut self.entries {
            if now < entry.created || now < entry.touched {
                entry.reads.clear();
            } else {
                entry.reads.retain(|_, until| *until > now);
            }
            if entry.expired(now) && entry.reads.is_empty() {
                expired.push(*id);
            }
        }
        for id in expired {
            self.retire(id)?;
        }
        Ok(())
    }
    fn retire(&mut self, id: [u8; 16]) -> Result<()> {
        let entry = self.entries.get(&id).expect("selected cursor");
        let result = GenerationRetention {
            database: entry.pin.database.clone(),
            owner: self.owner,
        }
        .release(&entry.pin);
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

fn open_writer(path: &Path) -> Result<Connection> {
    if !path.is_file() {
        return Err(Error::NotReady);
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(2))?;
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
        let mut connection = open_writer(database)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active = storage_active(&tx)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS staging_generations(generation INTEGER PRIMARY KEY REFERENCES generations(id) ON DELETE CASCADE);
          CREATE TABLE IF NOT EXISTS cursor_retention_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL,owner BLOB NOT NULL CHECK(length(owner)=16));
          CREATE TABLE IF NOT EXISTS cursor_generation_pins(id BLOB PRIMARY KEY CHECK(length(id)=16),generation INTEGER NOT NULL REFERENCES generations(id) ON DELETE CASCADE,owner BLOB NOT NULL CHECK(length(owner)=16));
          CREATE INDEX IF NOT EXISTS cursor_pin_generation ON cursor_generation_pins(generation);")?;
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
        let id = nonce()?;
        let mut connection = open_writer(&self.database)?;
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
        tx.commit()?;
        Ok(GenerationPin {
            database: self.database.clone(),
            owner: self.owner,
            id,
            generation,
        })
    }

    pub fn release(&self, pin: &GenerationPin) -> Result<()> {
        if pin.database != self.database || pin.owner != self.owner {
            return Err(Error::CursorInvalidated);
        }
        let mut connection = open_writer(&self.database)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.check_owner(&tx)?;
        tx.execute(
            "DELETE FROM cursor_generation_pins WHERE id=? AND owner=?",
            params![pin.id.as_slice(), self.owner.as_slice()],
        )?;
        tx.execute("DELETE FROM generation_units WHERE generation=? AND generation IS NOT (SELECT current_generation FROM state) AND NOT EXISTS(SELECT 1 FROM cursor_generation_pins WHERE generation=?)", params![pin.generation, pin.generation])?;
        crate::store::reclaim(&tx)?;
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

    fn database() -> PathBuf {
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
    fn publish(path: &Path, text: &str) {
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
        writer.publish().unwrap();
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
}
