//! Coordinator-owned generation retention. This storage primitive is not yet
//! the portable pagination API: lifetime/admission and page replay belong above it.
use crate::{Error, Result, query::ReadView};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

pub const RETENTION_VERSION: u32 = 1;

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
        let mut id = [0; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut id)?;
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
