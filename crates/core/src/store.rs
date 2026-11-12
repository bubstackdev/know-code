use crate::{Error, Result, protocol::*};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, config::DbConfig, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};

pub const SCHEMA_VERSION: i32 = 8;
/// Closed managed-upgrade catalog. Unknown formats never authorize replacement.
/// Unmanaged Writer migrations retain their existing behavior.
pub const READABLE_FORMATS: &[i32] = &[2, 3, 4, 5, 6, 7, 8];
pub const MIGRATION_FORMATS: &[i32] = &[3, 4, 5, 6, 7];
pub const REBUILD_FORMATS: &[i32] = &[1, 2];

struct WriterFence(File);
impl Drop for WriterFence {
    fn drop(&mut self) {
        // A duplicated/inherited descriptor can outlive this owner. Closing
        // alone would leave its flock held on the shared open-file description.
        let _ = self.0.unlock();
    }
}

/// Managed write connections must not checkpoint on commit/close while their
/// caller owns a shared coordinator fence. Maintenance is explicit instead.
pub(crate) fn defer_checkpoints(connection: &Connection) -> Result<()> {
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    connection.execute_batch("PRAGMA wal_autocheckpoint=0;")?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CheckpointProgress {
    pub busy: bool,
    /// SQLite reports -1 when frame counts are unavailable or no WAL exists.
    pub log_frames: i32,
    pub checkpointed_frames: i32,
}

fn checkpoint(connection: &Connection) -> Result<CheckpointProgress> {
    connection.busy_timeout(Duration::ZERO)?;
    match connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok(CheckpointProgress {
            busy: row.get(0)?,
            log_frames: row.get(1)?,
            checkpointed_frames: row.get(2)?,
        })
    }) {
        Ok(progress) => Ok(progress),
        Err(rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            Ok(CheckpointProgress {
                busy: true,
                log_frames: -1,
                checkpointed_frames: -1,
            })
        }
        Err(error) => Err(error.into()),
    }
}

/// Attempt WAL reclamation for one adopted workspace without consumer query
/// admission, schema migration or waiting for its writer/readers. Call outside
/// coordinator cursor/entry/settings mutexes. Busy is workspace-local deferral.
pub fn maintain_index(logical: &Path) -> Result<CheckpointProgress> {
    let ownership = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(logical.with_extension("writer.lock"))?;
    let metadata = ownership.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(Error::InvalidRequest(
            "index writer fence is not an owned regular file".into(),
        ));
    }
    match ownership.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(Error::Busy(
                "index writer is active; checkpoint deferred".into(),
            ));
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    let _ownership = WriterFence(ownership);
    let health = crate::recovery::IndexReadGuard::open_unbudgeted(logical)?;
    if health.incarnation().is_empty() {
        return Err(Error::InvalidRequest(
            "checkpoint maintenance requires an adopted workspace index".into(),
        ));
    }
    let result = (|| {
        health.check()?;
        let mut connection = Connection::open_with_flags(
            health.physical_path(),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        defer_checkpoints(&connection)?;
        connection.busy_timeout(Duration::ZERO)?;
        let progress_health = health.clone();
        connection.progress_handler(1_000, Some(move || progress_health.check().is_err()))?;
        let format: i32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if !READABLE_FORMATS.contains(&format) {
            return Err(Error::UpdateRequired(format!(
                "index format {format} cannot be checkpointed by this runtime"
            )));
        }
        let tables: u32 = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('state','generation_units','compilations','reports')", [], |r| r.get(0))?;
        if tables != 4 {
            return Err(Error::InvalidRequest(
                "checkpoint target is not a workspace fact store".into(),
            ));
        }
        let generation: Option<u64> = connection.query_row(
            "SELECT current_generation FROM state WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        generation.ok_or(Error::NotReady)?;
        let mode: String = connection.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        if mode != "wal" {
            return Err(Error::InvalidRequest(
                "checkpoint target is not in WAL mode".into(),
            ));
        }
        health.check()?;
        let progress = checkpoint(&connection)?;
        health.check()?;
        Ok(progress)
    })();
    crate::index_result(&health, result)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeAction {
    Current,
    Migrate,
    Rebuild { readable: bool },
    Unsupported,
}

pub fn upgrade_action(format: i32) -> UpgradeAction {
    if format == SCHEMA_VERSION {
        UpgradeAction::Current
    } else if REBUILD_FORMATS.contains(&format) {
        UpgradeAction::Rebuild {
            readable: READABLE_FORMATS.contains(&format),
        }
    } else if MIGRATION_FORMATS.contains(&format) {
        UpgradeAction::Migrate
    } else {
        UpgradeAction::Unsupported
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS generations(id INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS staging_generations(
 generation INTEGER PRIMARY KEY REFERENCES generations(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS state(singleton INTEGER PRIMARY KEY CHECK(singleton=1), current_generation INTEGER);
INSERT OR IGNORE INTO state VALUES(1,NULL);
CREATE TABLE IF NOT EXISTS contents(hash TEXT PRIMARY KEY, utf8 BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS compilations(
 generation INTEGER REFERENCES generations ON DELETE CASCADE, id TEXT, language TEXT,
 project_path TEXT, target TEXT, configuration TEXT, inferred INTEGER, compiler_version TEXT, options_json TEXT,
 dependencies_json TEXT NOT NULL DEFAULT '[]', input_paths_json TEXT NOT NULL DEFAULT '[]',
 PRIMARY KEY(generation,id));
CREATE TABLE IF NOT EXISTS artifacts(
 rowid INTEGER PRIMARY KEY, generation INTEGER REFERENCES generations ON DELETE CASCADE, id TEXT, path TEXT,
 content_hash TEXT REFERENCES contents, generated INTEGER, UNIQUE(generation,id));
CREATE INDEX IF NOT EXISTS artifact_path ON artifacts(path,generation);
CREATE TABLE IF NOT EXISTS generation_units(
 generation INTEGER REFERENCES generations ON DELETE CASCADE, compilation_id TEXT, owner_generation INTEGER,
 PRIMARY KEY(generation,compilation_id),
 FOREIGN KEY(owner_generation,compilation_id) REFERENCES compilations(generation,id));
CREATE TABLE IF NOT EXISTS unit_artifacts(
 owner_generation INTEGER, compilation_id TEXT, artifact_id TEXT,
 PRIMARY KEY(owner_generation,compilation_id,artifact_id),
 FOREIGN KEY(owner_generation,compilation_id) REFERENCES compilations(generation,id) ON DELETE CASCADE,
 FOREIGN KEY(owner_generation,artifact_id) REFERENCES artifacts(generation,id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS symbols(
 rowid INTEGER PRIMARY KEY, generation INTEGER, compilation_id TEXT, id TEXT, name TEXT, kind TEXT, signature TEXT, external INTEGER,
 UNIQUE(generation,compilation_id,id),
 FOREIGN KEY(generation,compilation_id) REFERENCES compilations ON DELETE CASCADE);
CREATE INDEX IF NOT EXISTS symbol_name ON symbols(generation,name);
CREATE TABLE IF NOT EXISTS occurrences(
 symbol_key INTEGER REFERENCES symbols(rowid) ON DELETE CASCADE,
 role TEXT, artifact_key INTEGER REFERENCES artifacts(rowid) ON DELETE CASCADE,
 start_byte INTEGER, end_byte INTEGER CHECK(end_byte>=start_byte AND start_byte>=0), enclosing_symbol_id TEXT,
 PRIMARY KEY(symbol_key,role,artifact_key,start_byte,end_byte)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS relationships(
 source_key INTEGER REFERENCES symbols(rowid) ON DELETE CASCADE,
 target_key INTEGER REFERENCES symbols(rowid) ON DELETE CASCADE, kind TEXT,
 PRIMARY KEY(source_key,target_key,kind)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS reports(
 generation INTEGER, compilation_id TEXT, status TEXT, diagnostics TEXT,
 PRIMARY KEY(generation,compilation_id),
 FOREIGN KEY(generation,compilation_id) REFERENCES compilations ON DELETE CASCADE);
CREATE VIRTUAL TABLE IF NOT EXISTS symbol_search USING fts5(name,signature,content='symbols',content_rowid='rowid');
CREATE TRIGGER IF NOT EXISTS symbols_insert AFTER INSERT ON symbols BEGIN
 INSERT INTO symbol_search(rowid,name,signature) VALUES(new.rowid,new.name,new.signature); END;
CREATE TRIGGER IF NOT EXISTS symbols_delete AFTER DELETE ON symbols BEGIN
 INSERT INTO symbol_search(symbol_search,rowid,name,signature) VALUES('delete',old.rowid,old.name,old.signature); END;

"#;

fn migrate_keys(connection: &Connection) -> Result<()> {
    let before: (i64, i64) = connection.query_row(
        "SELECT (SELECT count(*) FROM occurrences),(SELECT count(*) FROM relationships)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    connection.execute_batch(r#"
CREATE TABLE artifacts_v3(rowid INTEGER PRIMARY KEY,generation INTEGER REFERENCES generations ON DELETE CASCADE,id TEXT,path TEXT,content_hash TEXT REFERENCES contents,generated INTEGER,UNIQUE(generation,id));
INSERT INTO artifacts_v3 SELECT rowid,generation,id,path,content_hash,generated FROM artifacts;
CREATE TABLE occurrences_v3(symbol_key INTEGER REFERENCES symbols(rowid) ON DELETE CASCADE,role TEXT,artifact_key INTEGER REFERENCES artifacts_v3(rowid) ON DELETE CASCADE,start_byte INTEGER,end_byte INTEGER CHECK(end_byte>=start_byte AND start_byte>=0),enclosing_symbol_id TEXT,PRIMARY KEY(symbol_key,role,artifact_key,start_byte,end_byte)) WITHOUT ROWID;
INSERT INTO occurrences_v3 SELECT s.rowid,o.role,a.rowid,o.start_byte,o.end_byte,o.enclosing_symbol_id FROM occurrences o JOIN symbols s ON s.generation=o.generation AND s.compilation_id=o.compilation_id AND s.id=o.symbol_id JOIN artifacts a ON a.generation=o.generation AND a.id=o.artifact_id;
CREATE TABLE relationships_v3(source_key INTEGER REFERENCES symbols(rowid) ON DELETE CASCADE,target_key INTEGER REFERENCES symbols(rowid) ON DELETE CASCADE,kind TEXT,PRIMARY KEY(source_key,target_key,kind)) WITHOUT ROWID;
INSERT INTO relationships_v3 SELECT s.rowid,t.rowid,r.kind FROM relationships r JOIN symbols s ON s.generation=r.generation AND s.compilation_id=r.compilation_id AND s.id=r.source_id JOIN symbols t ON t.generation=r.generation AND t.compilation_id=r.compilation_id AND t.id=r.target_id;
CREATE TABLE unit_artifacts_v3(owner_generation INTEGER,compilation_id TEXT,artifact_id TEXT,PRIMARY KEY(owner_generation,compilation_id,artifact_id),FOREIGN KEY(owner_generation,compilation_id) REFERENCES compilations(generation,id) ON DELETE CASCADE,FOREIGN KEY(owner_generation,artifact_id) REFERENCES artifacts_v3(generation,id) ON DELETE CASCADE);
INSERT INTO unit_artifacts_v3 SELECT * FROM unit_artifacts;
DROP TABLE occurrences;
DROP TABLE relationships;
DROP TABLE unit_artifacts;
DROP TABLE artifacts;
ALTER TABLE artifacts_v3 RENAME TO artifacts;
ALTER TABLE occurrences_v3 RENAME TO occurrences;
ALTER TABLE relationships_v3 RENAME TO relationships;
ALTER TABLE unit_artifacts_v3 RENAME TO unit_artifacts;
CREATE INDEX artifact_path ON artifacts(path,generation);
"#)?;
    let after: (i64, i64) = connection.query_row(
        "SELECT (SELECT count(*) FROM occurrences),(SELECT count(*) FROM relationships)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if before != after {
        return Err(Error::Protocol(
            "existing index contains incoherent facts; key migration discarded".into(),
        ));
    }
    Ok(())
}

fn migrate_evidence(connection: &Connection) -> Result<()> {
    connection.execute_batch(r#"
CREATE TABLE relationships_v4(
 rowid INTEGER PRIMARY KEY, source_key INTEGER NOT NULL REFERENCES symbols(rowid) ON DELETE CASCADE,
 target_key INTEGER REFERENCES symbols(rowid) ON DELETE CASCADE, kind TEXT NOT NULL,
 resolution TEXT NOT NULL, artifact_key INTEGER REFERENCES artifacts(rowid) ON DELETE CASCADE,
 start_byte INTEGER, end_byte INTEGER, implicit INTEGER NOT NULL DEFAULT 0, dispatch TEXT NOT NULL DEFAULT '',
 CHECK(start_byte IS NULL OR (start_byte>=0 AND end_byte>=start_byte)));
INSERT INTO relationships_v4(source_key,target_key,kind,resolution)
 SELECT source_key,target_key,kind,'legacy_unknown' FROM relationships;
DROP TABLE relationships;
ALTER TABLE relationships_v4 RENAME TO relationships;
ALTER TABLE reports ADD COLUMN capabilities_json TEXT NOT NULL DEFAULT '{}';
"#)?;
    Ok(())
}

fn migrate_project_links(connection: &Connection) -> Result<()> {
    connection.execute_batch("ALTER TABLE compilations ADD COLUMN references_json TEXT NOT NULL DEFAULT '[]';
      ALTER TABLE compilations ADD COLUMN input_fingerprint TEXT NOT NULL DEFAULT '';
      ALTER TABLE symbols ADD COLUMN definition_compilation_id TEXT NOT NULL DEFAULT '';
      ALTER TABLE symbols ADD COLUMN definition_symbol_id TEXT NOT NULL DEFAULT '';
      ALTER TABLE symbols ADD COLUMN definition_fingerprint TEXT NOT NULL DEFAULT '';
      CREATE INDEX symbol_definition ON symbols(definition_compilation_id,definition_symbol_id) WHERE definition_compilation_id<>'';")?;
    Ok(())
}

/// These child/probe keys are shared by publication and cursor reclamation.
/// Compatible retained stores need the same repair as newly created stores.
pub(crate) fn ensure_cleanup_indexes(connection: &Connection) -> Result<()> {
    connection.execute_batch("CREATE INDEX IF NOT EXISTS unit_artifact_owner ON unit_artifacts(owner_generation,artifact_id);
        CREATE INDEX IF NOT EXISTS artifact_content ON artifacts(content_hash);
        CREATE INDEX IF NOT EXISTS generation_unit_owner ON generation_units(owner_generation,compilation_id);")?;
    Ok(())
}

pub(crate) fn reclaim(connection: &Connection) -> Result<()> {
    connection.execute_batch("DELETE FROM compilations WHERE NOT EXISTS(SELECT 1 FROM generation_units u WHERE u.owner_generation=compilations.generation AND u.compilation_id=compilations.id);
      DELETE FROM artifacts WHERE NOT EXISTS(SELECT 1 FROM unit_artifacts u WHERE u.owner_generation=artifacts.generation AND u.artifact_id=artifacts.id);
      DELETE FROM generations WHERE id NOT IN (SELECT generation FROM generation_units UNION SELECT owner_generation FROM generation_units UNION SELECT generation FROM staging_generations);
      DELETE FROM contents WHERE hash NOT IN (SELECT content_hash FROM artifacts);")?;
    Ok(())
}

fn discard_unretained_manifests(connection: &Connection) -> Result<()> {
    if crate::cursor::storage_active(connection)? {
        connection.execute("DELETE FROM generation_units WHERE generation IS NOT (SELECT current_generation FROM state) AND generation NOT IN (SELECT generation FROM cursor_generation_pins)", [])?;
    } else {
        connection.execute("DELETE FROM generation_units WHERE generation IS NOT (SELECT current_generation FROM state)", [])?;
    }
    Ok(())
}

fn migrate_dispatch_keys(connection: &Connection) -> Result<()> {
    connection.execute_batch("ALTER TABLE relationships ADD COLUMN dispatch_key TEXT NOT NULL DEFAULT ''; DROP INDEX IF EXISTS relationship_evidence; CREATE UNIQUE INDEX relationship_evidence ON relationships(source_key,ifnull(target_key,0),kind,resolution,ifnull(artifact_key,0),ifnull(start_byte,0),ifnull(end_byte,0),implicit,dispatch,dispatch_key);")?;
    Ok(())
}

pub struct Writer {
    // Keep the connection's close/cleanup inside the writer fence lifetime.
    connection: Connection,
    _ownership: WriterFence,
    pub generation: u64,
    published: bool,
    health: Option<crate::recovery::IndexReadGuard>,
}

impl Writer {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_scoped(path, None)
    }

    pub fn open_incremental(path: &Path, replacements: &[String]) -> Result<Self> {
        Self::open_scoped(path, Some(replacements))
    }

    fn open_scoped(path: &Path, replacements: Option<&[String]>) -> Result<Self> {
        Self::open_controlled(path, replacements, &crate::resource::Control::default())
    }

    pub fn open_controlled(
        path: &Path,
        replacements: Option<&[String]>,
        control: &crate::resource::Control,
    ) -> Result<Self> {
        Self::open_target(path, None, replacements, 0, control)
    }

    /// Recovery writes only its unpublished physical destination while holding
    /// the same stable logical workspace writer fence as ordinary updates.
    pub fn open_destination(
        logical: &Path,
        physical: &Path,
        replacements: Option<&[String]>,
        minimum_generation: u64,
        control: &crate::resource::Control,
    ) -> Result<Self> {
        Self::open_target(
            logical,
            Some(physical),
            replacements,
            minimum_generation,
            control,
        )
    }

    fn open_target(
        logical: &Path,
        destination: Option<&Path>,
        replacements: Option<&[String]>,
        minimum_generation: u64,
        control: &crate::resource::Control,
    ) -> Result<Self> {
        control.check()?;
        if let Some(parent) = logical.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let ownership = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(logical.with_extension("writer.lock"))?;
        ownership
            .try_lock()
            .map_err(|e| Error::Provider(format!("workspace index already has a writer: {e}")))?;
        let mut ownership = Some(WriterFence(ownership));
        // Resolve after the logical fence. A candidate is unpublished and may
        // be written even though the selected serving incarnation is BAD.
        let health = if destination.is_none() {
            Some(crate::recovery::IndexReadGuard::open_unbudgeted(logical)?)
        } else {
            None
        };
        let path = destination.unwrap_or_else(|| {
            health
                .as_ref()
                .expect("selected index guard")
                .physical_path()
        });
        let result = (|| -> Result<Self> {
            let mut connection = Connection::open(path)?;
            defer_checkpoints(&connection)?;
            let interrupt = connection.get_interrupt_handle();
            // Borrow the outer fence until a complete Writer takes ownership.
            // The callback owns the connection, so early errors close it first;
            // post-action cancellation drops Writer and cleans its staged rows.
            control.interruptible(interrupt, || {
        connection.busy_timeout(Duration::from_secs(2))?;
        let version: i32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if ![0, 1, 2, 3, 4, 5, 6, 7, SCHEMA_VERSION].contains(&version) {
            return Err(Error::UpdateRequired(format!(
                "index format {version} is unsupported; expected {SCHEMA_VERSION}"
            )));
        }
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA cache_size=-2048;")?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if version == 1 {
            tx.execute_batch("ALTER TABLE compilations ADD COLUMN dependencies_json TEXT NOT NULL DEFAULT '[]'; ALTER TABLE compilations ADD COLUMN input_paths_json TEXT NOT NULL DEFAULT '[]'")?;
        }
        tx.execute_batch(SCHEMA)?;
        if version == 1 {
            tx.execute_batch("INSERT INTO generation_units SELECT generation,id,generation FROM compilations;
                INSERT OR IGNORE INTO unit_artifacts SELECT generation,compilation_id,artifact_id FROM occurrences;")?;
        }
        if version == 1 || version == 2 {
            migrate_keys(&tx)?;
        }
        if version < 4 {
            migrate_evidence(&tx)?;
        }
        if version < 5 {
            migrate_project_links(&tx)?;
        }
        if version < 6 {
            migrate_dispatch_keys(&tx)?;
        }
        if version < 7 {
            tx.execute_batch("ALTER TABLE compilations ADD COLUMN test_classification_json TEXT NOT NULL DEFAULT ''; ALTER TABLE unit_artifacts ADD COLUMN test_classification_json TEXT NOT NULL DEFAULT ''; ALTER TABLE symbols ADD COLUMN test_mask INTEGER NOT NULL DEFAULT 1; CREATE INDEX symbol_test_mask ON symbols(test_mask); PRAGMA user_version=7;")?;
        }
        if version < 8 {
            tx.execute_batch("ALTER TABLE compilations ADD COLUMN target_family TEXT NOT NULL DEFAULT ''; ALTER TABLE compilations ADD COLUMN target_equivalence_policy INTEGER NOT NULL DEFAULT 0; ALTER TABLE symbols ADD COLUMN declaration_key TEXT NOT NULL DEFAULT ''; ALTER TABLE symbols ADD COLUMN grouping_key TEXT NOT NULL DEFAULT ''; CREATE INDEX symbol_grouping ON symbols(grouping_key) WHERE grouping_key<>''; PRAGMA user_version=8;")?;
        }
        ensure_cleanup_indexes(&tx)?;
        crate::testing::create_views(&tx, SCHEMA_VERSION)?;
        tx.execute_batch("CREATE INDEX IF NOT EXISTS occurrence_artifact ON occurrences(artifact_key);
          CREATE INDEX IF NOT EXISTS relationship_source ON relationships(source_key,resolution,kind);
          CREATE INDEX IF NOT EXISTS relationship_target ON relationships(target_key,resolution,kind);
          CREATE INDEX IF NOT EXISTS relationship_artifact ON relationships(artifact_key);
          CREATE UNIQUE INDEX IF NOT EXISTS relationship_evidence ON relationships(source_key,ifnull(target_key,0),kind,resolution,ifnull(artifact_key,0),ifnull(start_byte,0),ifnull(end_byte,0),implicit,dispatch,dispatch_key);")?;
        // Only this owner can recover abandoned staging. Readers use committed snapshots.
        // Sole workspace-writer ownership proves these are abandoned stages.
        tx.execute("DELETE FROM staging_generations", [])?;
        discard_unretained_manifests(&tx)?;
        reclaim(&tx)?;
        let generation: u64 = tx.query_row(
            "SELECT COALESCE(MAX(id),(SELECT current_generation FROM state),0)+1 FROM generations",
            [],
            |r| r.get(0),
        )?;
        let generation = generation.max(minimum_generation).max(crate::recovery::generation_floor(logical, 0)?);
        crate::recovery::reserve_generation(logical,generation)?;
        tx.execute("INSERT INTO generations VALUES(?)", [generation])?;
        tx.execute("INSERT INTO staging_generations VALUES(?)", [generation])?;
        if let Some(replacements) = replacements {
            tx.execute("INSERT INTO generation_units SELECT ?,compilation_id,owner_generation FROM generation_units WHERE generation=(SELECT current_generation FROM state)",[generation])?;
            for project in replacements {
                tx.execute("DELETE FROM generation_units WHERE generation=? AND compilation_id IN(SELECT id FROM compilations WHERE project_path=?)",params![generation,project])?;
            }
        }
        tx.commit()?;
        if version == 1 || version == 2 {
            // Recode atomically first; compaction is best-effort maintenance.
            // Existing WAL readers keep their coherent pre-migration snapshot.
            connection.busy_timeout(Duration::ZERO)?;
            if let Err(error) = connection.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
            {
                eprintln!("index keys migrated; compaction deferred: {error}");
            }
            connection.busy_timeout(Duration::from_secs(2))?;
        }
        Ok(Self {
            connection,
            _ownership: ownership.take().expect("writer fence transferred once"),
            generation,
            published: false,
            health: health.clone(),
        })
        })
        })();
        if let Some(guard) = &health {
            crate::index_result(guard, result)
        } else {
            result
        }
    }

    pub fn stage(&mut self, payload: envelope::Payload) -> Result<()> {
        if let Some(health) = &self.health {
            health.check()?;
        }
        let result = self.stage_inner(payload);
        if let Some(health) = &self.health {
            crate::index_result(health, result)
        } else {
            result
        }
    }

    fn stage_inner(&mut self, payload: envelope::Payload) -> Result<()> {
        // A frame is already received before this short transaction begins.
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let gen_id = self.generation;
        match payload {
            envelope::Payload::Compilation(c) => {
                if c.id.is_empty() {
                    return Err(Error::Protocol("compilation identity is empty".into()));
                }
                if c.target_equivalence_policy > crate::grouping::POLICY_VERSION
                    || (c.target_equivalence_policy == 0) != c.target_family.is_empty()
                    || c.target_family.len() > 512
                {
                    return Err(Error::Protocol(
                        "invalid target-equivalence family/policy".into(),
                    ));
                }
                let mut selected = std::collections::BTreeSet::new();
                if c.references.iter().any(|r| {
                    r.compilation_id.is_empty()
                        || r.project_path.is_empty()
                        || r.compilation_id == c.id
                        || !selected.insert(&r.compilation_id)
                }) {
                    return Err(Error::Protocol(
                        "invalid compiler project-reference context".into(),
                    ));
                }
                tx.execute(
                    "INSERT INTO compilations VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    params![
                        gen_id,
                        c.id,
                        c.language,
                        c.project_path,
                        c.target,
                        c.configuration,
                        c.inferred,
                        c.compiler_version,
                        c.options_json,
                        serde_json::to_string(&c.dependencies)?,
                        serde_json::to_string(&c.input_paths)?,
                        serde_json::to_string(&c.references)?,
                        c.input_fingerprint,
                        crate::testing::encode(c.test_classification.as_ref())?,
                        c.target_family,
                        c.target_equivalence_policy
                    ],
                )?;
                tx.execute("INSERT INTO generation_units VALUES(?,?,?) ON CONFLICT(generation,compilation_id) DO UPDATE SET owner_generation=excluded.owner_generation",params![gen_id,c.id,gen_id])?;
            }
            envelope::Payload::Source(s) => {
                if s.id.is_empty() {
                    return Err(Error::Protocol("source identity is empty".into()));
                }
                std::str::from_utf8(&s.utf8)
                    .map_err(|_| Error::Protocol("source is not canonical UTF-8".into()))?;
                if format!("{:x}", Sha256::digest(&s.utf8)) != s.sha256 {
                    return Err(Error::Protocol("source content hash mismatch".into()));
                }
                tx.execute(
                    "INSERT OR IGNORE INTO contents VALUES(?,?)",
                    params![s.sha256, s.utf8],
                )?;
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO artifacts(generation,id,path,content_hash,generated) VALUES(?,?,?,?,?)",
                    params![gen_id, s.id, s.path, s.sha256, s.generated],
                )?;
                if inserted == 0 {
                    let existing: (String,String,bool) = tx.query_row("SELECT path,content_hash,generated FROM artifacts WHERE generation=? AND id=?",params![gen_id,s.id],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                    if existing != (s.path, s.sha256, s.generated) {
                        return Err(Error::Protocol("conflicting artifact identity".into()));
                    }
                }
                let classification = crate::testing::encode(s.test_classification.as_ref())?;
                tx.execute(
                    "INSERT INTO unit_artifacts(owner_generation,compilation_id,artifact_id,test_classification_json) VALUES(?,?,?,?) ON CONFLICT(owner_generation,compilation_id,artifact_id) DO NOTHING",
                    params![gen_id, s.compilation_id, s.id, classification],
                )?;
                let existing: String=tx.query_row("SELECT test_classification_json FROM unit_artifacts WHERE owner_generation=? AND compilation_id=? AND artifact_id=?",params![gen_id,s.compilation_id,s.id],|r|r.get(0))?;
                if existing != classification {
                    return Err(Error::Protocol(
                        "conflicting source test classification".into(),
                    ));
                }
            }
            envelope::Payload::Facts(batch) => {
                let selected = if batch
                    .symbols
                    .iter()
                    .any(|s| !s.definition_compilation_id.is_empty())
                {
                    let json: String = tx.query_row(
                        "SELECT references_json FROM compilations WHERE generation=? AND id=?",
                        params![gen_id, batch.compilation_id],
                        |r| r.get(0),
                    )?;
                    serde_json::from_str::<Vec<ProjectReferenceContext>>(&json)?
                        .into_iter()
                        .map(|r| r.compilation_id)
                        .collect::<std::collections::BTreeSet<_>>()
                } else {
                    std::collections::BTreeSet::new()
                };
                let declaration_keys = batch.symbols.iter().any(|s| !s.declaration_key.is_empty());
                if declaration_keys && tx.query_row("SELECT target_equivalence_policy FROM compilations WHERE generation=? AND id=?",params![gen_id,batch.compilation_id],|r|r.get::<_,u32>(0))? != crate::grouping::POLICY_VERSION {
                    return Err(Error::Protocol("declaration equivalence evidence requires an acknowledged target family".into()));
                }
                let mut insert = tx.prepare_cached("INSERT OR IGNORE INTO symbols(generation,compilation_id,id,name,kind,signature,external,definition_compilation_id,definition_symbol_id,definition_fingerprint,declaration_key) VALUES(?,?,?,?,?,?,?,?,?,?,?)")?;
                for s in batch.symbols {
                    // Empty target IDs encode unresolved relationships, never
                    // a real symbol that could accidentally satisfy that join.
                    if s.id.is_empty() {
                        return Err(Error::Protocol("symbol identity is empty".into()));
                    }
                    if s.declaration_key.len() > 512 || s.external && !s.declaration_key.is_empty()
                    {
                        return Err(Error::Protocol(
                            "invalid declaration equivalence key".into(),
                        ));
                    }
                    let has_definition = !s.definition_compilation_id.is_empty();
                    if has_definition != !s.definition_symbol_id.is_empty()
                        || has_definition != !s.definition_fingerprint.is_empty()
                        || has_definition
                            && (!s.external || !selected.contains(&s.definition_compilation_id))
                    {
                        return Err(Error::Protocol(
                            "definition must select a compiler-referenced context and snapshot"
                                .into(),
                        ));
                    }
                    let inserted = insert.execute(params![
                        gen_id,
                        batch.compilation_id,
                        s.id,
                        s.name,
                        s.kind,
                        s.signature,
                        s.external,
                        s.definition_compilation_id,
                        s.definition_symbol_id,
                        s.definition_fingerprint,
                        s.declaration_key
                    ])?;
                    if inserted == 0 {
                        let existing:(String,String,String,String)=tx.query_row("SELECT definition_compilation_id,definition_symbol_id,definition_fingerprint,declaration_key FROM symbols WHERE generation=? AND compilation_id=? AND id=?",params![gen_id,batch.compilation_id,s.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
                        if existing
                            != (
                                s.definition_compilation_id,
                                s.definition_symbol_id,
                                s.definition_fingerprint,
                                s.declaration_key,
                            )
                        {
                            return Err(Error::Protocol(
                                "conflicting definition binding for symbol".into(),
                            ));
                        }
                    }
                }
                drop(insert);
                let mut insert = tx.prepare_cached("INSERT OR IGNORE INTO occurrences SELECT s.rowid,?,a.rowid,?,?,? FROM symbols s JOIN artifacts a ON a.generation=s.generation JOIN contents c ON c.hash=a.content_hash WHERE s.generation=? AND s.compilation_id=? AND s.id=? AND a.id=? AND ?<=length(c.utf8)")?;
                for o in batch.occurrences {
                    if o.end_byte < o.start_byte {
                        return Err(Error::Protocol("reversed source range".into()));
                    }
                    let inserted = insert.execute(params![
                        o.role,
                        o.start_byte,
                        o.end_byte,
                        o.enclosing_symbol_id,
                        gen_id,
                        batch.compilation_id,
                        o.symbol_id,
                        o.artifact_id,
                        o.end_byte
                    ])?;
                    if inserted == 0 {
                        let duplicate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM occurrences o JOIN symbols s ON s.rowid=o.symbol_key JOIN artifacts a ON a.rowid=o.artifact_key WHERE s.generation=? AND s.compilation_id=? AND s.id=? AND a.id=? AND o.start_byte=? AND o.end_byte=? AND o.role=?)",params![gen_id,batch.compilation_id,o.symbol_id,o.artifact_id,o.start_byte,o.end_byte,o.role],|r| r.get(0))?;
                        if !duplicate {
                            return Err(Error::Protocol(
                                "occurrence has no symbol/source or exceeds its byte length".into(),
                            ));
                        }
                    }
                }
                drop(insert);
                let mut insert = tx.prepare_cached("INSERT OR IGNORE INTO relationships(source_key,target_key,kind,resolution,artifact_key,start_byte,end_byte,implicit,dispatch,dispatch_key)
                  SELECT s.rowid,t.rowid,?1,?2,a.rowid,?3,?4,?5,?11,?12 FROM symbols s
                  LEFT JOIN symbols t ON t.generation=s.generation AND t.compilation_id=s.compilation_id AND t.id=?6
                  LEFT JOIN artifacts a ON a.generation=s.generation AND a.id=?7 LEFT JOIN contents c ON c.hash=a.content_hash
                  WHERE s.generation=?8 AND s.compilation_id=?9 AND s.id=?10
                  AND (?6='' OR t.rowid IS NOT NULL)
                  AND (?7='' OR (a.rowid IS NOT NULL AND ?4<=length(c.utf8) AND EXISTS(SELECT 1 FROM unit_artifacts ua WHERE ua.owner_generation=s.generation AND ua.compilation_id=s.compilation_id AND ua.artifact_id=a.id)))")?;
                for r in batch.relationships {
                    let resolution = if r.resolution.is_empty() {
                        "legacy_unknown"
                    } else {
                        &r.resolution
                    };
                    if resolution != "legacy_unknown" {
                        crate::graph::DependencyKind::parse(&r.kind)?;
                        if !["compiler_bound", "possible_dispatch", "unresolved"]
                            .contains(&resolution)
                            || !["", "direct", "virtual", "interface", "indirect"]
                                .contains(&r.dispatch.as_str())
                            || r.artifact_id.is_empty()
                            || r.end_byte < r.start_byte
                            || (resolution == "unresolved") != r.target_id.is_empty()
                        {
                            return Err(Error::Protocol(
                                "invalid relationship evidence/resolution".into(),
                            ));
                        }
                    } else if r.target_id.is_empty() || !r.artifact_id.is_empty() {
                        return Err(Error::Protocol(
                            "legacy relationship requires two endpoints and no callsite".into(),
                        ));
                    }
                    let inserted = insert.execute(params![
                        r.kind,
                        resolution,
                        r.start_byte,
                        r.end_byte,
                        r.implicit,
                        r.target_id,
                        r.artifact_id,
                        gen_id,
                        batch.compilation_id,
                        r.source_id,
                        r.dispatch,
                        r.dispatch_key
                    ])?;
                    if inserted == 0 {
                        // Successful inserts already validate endpoints/source/range in SQL.
                        // Only ignored inserts need a lookup to distinguish a real
                        // duplicate from invalid evidence; avoid quadratic scans of
                        // a large callable's adjacency on every newly inserted row.
                        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM relationships r JOIN symbols s ON s.rowid=r.source_key
                      LEFT JOIN symbols t ON t.rowid=r.target_key LEFT JOIN artifacts a ON a.rowid=r.artifact_key
                      WHERE s.generation=? AND s.compilation_id=? AND s.id=? AND ifnull(t.id,'')=? AND r.kind=? AND r.resolution=?
                      AND ifnull(a.id,'')=? AND r.start_byte=? AND r.end_byte=? AND r.implicit=? AND r.dispatch=? AND r.dispatch_key=?)",
                        params![gen_id,batch.compilation_id,r.source_id,r.target_id,r.kind,resolution,r.artifact_id,r.start_byte,r.end_byte,r.implicit,r.dispatch,r.dispatch_key],|row| row.get(0))?;
                        if !valid {
                            return Err(Error::Protocol("relationship endpoint/source unavailable or range invalid in its compilation".into()));
                        }
                    }
                }
            }
            envelope::Payload::Report(r) => {
                if !["complete", "partial", "unavailable"].contains(&r.status.as_str()) {
                    return Err(Error::Protocol("invalid analysis status".into()));
                }
                if r.capabilities.iter().any(|(name, status)| {
                    ![
                        "calls",
                        "dependencies",
                        "dispatch",
                        "project_links",
                        "test_classification",
                        "target_equivalence",
                    ]
                    .contains(&name.as_str())
                        || !["complete", "partial", "unavailable", "unknown"]
                            .contains(&status.as_str())
                }) {
                    return Err(Error::Protocol("invalid analysis capability".into()));
                }
                tx.execute(
                    "INSERT INTO reports VALUES(?,?,?,?,?)",
                    params![
                        gen_id,
                        r.compilation_id,
                        r.status,
                        serde_json::to_string(&r.diagnostics)?,
                        serde_json::to_string(&r.capabilities)?
                    ],
                )?;
            }
            _ => return Err(Error::Protocol("unexpected staging message".into())),
        }
        tx.commit()?;
        Ok(())
    }

    pub fn publish(self) -> Result<u64> {
        self.publish_controlled(&crate::resource::Control::default())
    }

    pub fn publish_controlled(mut self, control: &crate::resource::Control) -> Result<u64> {
        let result = self.commit_controlled(control);
        self.maintain_published();
        result
    }
    /// Commit only. The caller keeps Writer ownership outside any publication
    /// fence, including on errors/panic, so cleanup and close occur afterward.
    pub(crate) fn commit_controlled(&mut self, control: &crate::resource::Control) -> Result<u64> {
        let health = self.health.clone();
        if let Some(guard) = &health {
            guard.check()?;
        }
        let interrupt = self.connection.get_interrupt_handle();
        let result = control.interruptible(interrupt, || self.finish_publication(control));
        if let Some(guard) = &health {
            crate::index_result(guard, result)
        } else {
            result
        }
    }
    fn finish_publication(&mut self, control: &crate::resource::Control) -> Result<u64> {
        control.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (count,missing): (u64,u64) = tx.query_row("SELECT count(*),count(*)-count(r.compilation_id) FROM generation_units u LEFT JOIN reports r ON u.owner_generation=r.generation AND u.compilation_id=r.compilation_id WHERE u.generation=?",[self.generation],|r| Ok((r.get(0)?,r.get(1)?)))?;
        if count == 0 || missing != 0 {
            return Err(Error::Protocol(
                "analysis has no compilations or lacks completion reports".into(),
            ));
        }
        {
            let mut q = tx.prepare("SELECT id FROM compilations WHERE generation=?")?;
            let ids = q.query_map([self.generation], |r| r.get::<_, String>(0))?;
            for id in ids {
                control.check()?;
                let id = id?;
                crate::testing::finalize(&tx, self.generation, &id)?;
                crate::grouping::finalize(&tx, self.generation, &id)?;
            }
        }
        tx.execute(
            "UPDATE state SET current_generation=? WHERE singleton=1",
            [self.generation],
        )?;
        tx.execute(
            "DELETE FROM staging_generations WHERE generation=?",
            [self.generation],
        )?;
        discard_unretained_manifests(&tx)?;
        reclaim(&tx)?;
        control.publish(|| {
            if let Some(health) = &self.health {
                health.check()?;
            }
            tx.commit()?;
            Ok(())
        })?;
        self.published = true;
        Ok(self.generation)
    }
    pub(crate) fn maintain_published(&self) {
        if !self.published {
            return;
        }
        let result = if let Some(health) = &self.health {
            crate::index_result(
                health,
                health.check().and_then(|_| checkpoint(&self.connection)),
            )
        } else {
            checkpoint(&self.connection)
        };
        if let Err(error) = result {
            // The generation already committed. Never misreport it as failed
            // publication because best-effort maintenance could not complete.
            eprintln!("index publication committed; checkpoint deferred: {error}");
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if !self.published
            && self
                .health
                .as_ref()
                .is_none_or(|health| health.check().is_ok())
        {
            let _ = self.connection.execute(
                "DELETE FROM staging_generations WHERE generation=?",
                [self.generation],
            );
            let _ = self.connection.execute(
                "DELETE FROM generation_units WHERE generation=?",
                [self.generation],
            );
            let _ = reclaim(&self.connection);
        }
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc,
        },
    };
    fn fixture() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/checkpoint-tests")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .join("index.db")
    }
    fn populate(writer: &mut Writer, text: &str, completed: bool) {
        writer
            .stage(envelope::Payload::Compilation(Compilation {
                id: "comp".into(),
                language: "typescript".into(),
                ..Default::default()
            }))
            .unwrap();
        writer
            .stage(envelope::Payload::Source(SourceArtifact {
                id: "src".into(),
                compilation_id: "comp".into(),
                path: "fixture.ts".into(),
                utf8: text.as_bytes().into(),
                sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
                ..Default::default()
            }))
            .unwrap();
        if completed {
            writer
                .stage(envelope::Payload::Report(AnalysisReport {
                    compilation_id: "comp".into(),
                    status: "complete".into(),
                    ..Default::default()
                }))
                .unwrap();
        }
    }
    fn managed() -> PathBuf {
        let path = fixture();
        let mut writer = Writer::open(&path).unwrap();
        populate(&mut writer, "old", true);
        let generation = writer.publish().unwrap();
        crate::recovery::adopt(&path, generation).unwrap();
        path
    }
    fn wal_bytes(path: &Path) -> u64 {
        std::fs::metadata(format!("{}-wal", path.display())).map_or(0, |m| m.len())
    }
    fn source(view: &crate::query::ReadView) -> String {
        view.query(
            serde_json::from_value(
                serde_json::json!({"op":"source","artifact_id":"src","max_bytes":262144}),
            )
            .unwrap(),
        )
        .unwrap()
        .source
        .unwrap()
        .text
    }
    fn staged_document(number: usize, bytes: usize) -> envelope::Payload {
        let text = format!("// saved document {number}\n{}", "a".repeat(bytes));
        envelope::Payload::Source(SourceArtifact {
            id: format!("stage-{number}"),
            compilation_id: "comp".into(),
            path: format!("stage-{number}.ts"),
            sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
            utf8: text.into_bytes(),
            ..Default::default()
        })
    }
    #[test]
    fn staging_checkpoint_bounds_wal_without_publishing_or_changing_busy_policy() {
        let path = managed();
        let scope =
            crate::admission::AdmissionScope::at(path.parent().unwrap().join("scope")).unwrap();
        let old = crate::query::ReadView::open_in_scope(&path, &scope).unwrap();
        let generation = old.generation;
        assert_eq!(source(&old), "old");
        drop(old);
        let mut writer = Writer::open(&path).unwrap();
        populate(&mut writer, "next", true);
        writer
            .connection
            .busy_timeout(Duration::from_millis(1777))
            .unwrap();
        let mut peak = 0;
        for number in 0..256 {
            writer
                .stage(staged_document(number, 256 * 1024))
                .unwrap();
            peak = peak.max(wal_bytes(&path));
        }
        // This finite fixture stages over 64 MiB, without a pinned reader.
        // Reusing its WAL before publication must not require a larger pool.
        assert!(peak < 48 * 1024 * 1024, "staging WAL grew to {peak} bytes");
        assert_eq!(
            writer
                .connection
                .query_row("PRAGMA busy_timeout", [], |row| row.get::<_, u64>(0))
                .unwrap(),
            1777
        );
        assert!(Writer::open(&path).is_err(), "checkpoint retired active writer");
        let old = crate::query::ReadView::open_in_scope(&path, &scope).unwrap();
        assert_eq!(old.generation, generation);
        assert_eq!(source(&old), "old");
        drop(old);
        let published = writer.publish().unwrap();
        assert!(published > generation);
        let current = crate::query::ReadView::open_in_scope(&path, &scope).unwrap();
        assert_eq!(current.generation, published);
        assert_eq!(source(&current), "next");
    }
    #[test]
    fn writer_retirement_unlocks_duplicated_fence_after_publish_and_abort() {
        for published in [true, false] {
            let path = fixture();
            let mut writer = Writer::open(&path).unwrap();
            populate(&mut writer, "retained descriptor", true);
            let duplicate = writer._ownership.0.try_clone().unwrap();
            let identity = duplicate.metadata().unwrap();
            assert!(Writer::open(&path).is_err(), "live writer lost exclusion");
            if published {
                writer.publish().unwrap();
            } else {
                drop(writer);
            }
            // A duplicate shares the original open-file description. It must
            // not keep a retired Writer's lock alive until its eventual close.
            let replacement = Writer::open(&path).unwrap();
            let selected = replacement._ownership.0.metadata().unwrap();
            assert_eq!(
                (selected.dev(), selected.ino()),
                (identity.dev(), identity.ino())
            );
            assert!(Writer::open(&path).is_err(), "replacement lost exclusion");
            drop(duplicate);
            assert!(
                Writer::open(&path).is_err(),
                "closing a retired duplicate released the replacement's lock"
            );
            drop(replacement);
            drop(Writer::open(&path).unwrap());
        }
    }
    #[test]
    fn managed_checkpoint_preserves_full_scope_raw_reader_then_truncates_and_reuses() {
        let path = managed();
        let old = "// old π😀\n".repeat(4096);
        let new = "// new π😀\n".repeat(4096);
        let mut writer = Writer::open(&path).unwrap();
        populate(&mut writer, &old, true);
        let original = writer.publish().unwrap();
        let scope =
            crate::admission::AdmissionScope::at(path.parent().unwrap().join("scope")).unwrap();
        scope.configure(1).unwrap();
        let held = crate::query::ReadView::open_in_scope(&path, &scope).unwrap();
        assert_eq!(held.generation, original);
        assert_eq!(source(&held), old);
        assert!(matches!(scope.try_reserve(), Err(Error::Busy(_))));
        let mut writer = Writer::open(&path).unwrap();
        populate(&mut writer, &new, true);
        let published = writer.publish().unwrap();
        assert!(published > original);
        assert!(wal_bytes(&path) > 0);
        // Maintenance needs no consumer reservation even while the only slot
        // belongs to the actual coherent old ReadView.
        let blocked = maintain_index(&path).unwrap();
        assert!(blocked.busy, "{blocked:?}");
        assert_eq!(source(&held), old);
        assert_eq!(held.generation, original);
        drop(held);
        let reclaimed = maintain_index(&path).unwrap();
        assert!(!reclaimed.busy, "{reclaimed:?}");
        assert_eq!(wal_bytes(&path), 0);
        let current = crate::query::ReadView::open_in_scope(&path, &scope).unwrap();
        assert_eq!(current.generation, published);
        assert_eq!(source(&current), new);
        drop(current);
        let mut writer = Writer::open(&path).unwrap();
        populate(&mut writer, "reused", true);
        let next = writer.publish().unwrap();
        assert!(next > published);
        assert!(!maintain_index(&path).unwrap().busy);
        assert_eq!(wal_bytes(&path), 0);
    }
    #[test]
    fn maintenance_defers_active_writer_and_refuses_foreign_future_missing_and_bad() {
        let missing = fixture();
        assert!(matches!(maintain_index(&missing), Err(Error::Io(_))));
        assert!(!missing.parent().unwrap().exists());
        let path = managed();
        let writer = Writer::open(&path).unwrap();
        assert!(matches!(maintain_index(&path), Err(Error::Busy(_))));
        drop(writer);
        let connection = Connection::open(&path).unwrap();
        defer_checkpoints(&connection).unwrap();
        connection
            .execute_batch("PRAGMA user_version=999;")
            .unwrap();
        drop(connection);
        let before = (
            std::fs::read(&path).unwrap(),
            std::fs::read(format!("{}-wal", path.display())).unwrap(),
        );
        assert!(matches!(
            maintain_index(&path),
            Err(Error::UpdateRequired(_))
        ));
        assert_eq!(
            before,
            (
                std::fs::read(&path).unwrap(),
                std::fs::read(format!("{}-wal", path.display())).unwrap()
            )
        );
        assert!(!crate::recovery::is_bad(&path).unwrap());
        let foreign = fixture();
        std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
        File::create(foreign.with_extension("writer.lock")).unwrap();
        let connection = Connection::open(&foreign).unwrap();
        connection
            .execute_batch("PRAGMA user_version=8; CREATE TABLE foreign_data(value);")
            .unwrap();
        drop(connection);
        let bytes = std::fs::read(&foreign).unwrap();
        assert!(matches!(
            maintain_index(&foreign),
            Err(Error::InvalidRequest(_))
        ));
        assert_eq!(bytes, std::fs::read(&foreign).unwrap());
        assert!(!foreign.with_extension("lifecycle.json").exists());
        let bad = managed();
        let guard = crate::recovery::IndexReadGuard::open_unbudgeted(&bad).unwrap();
        std::fs::write(&bad, b"actual owned NOTADB fixture").unwrap();
        let connection = Connection::open(&bad).unwrap();
        let error = connection
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))
            .unwrap_err();
        assert!(matches!(
            guard.map_sqlite(error),
            Error::IndexCorrupt { .. }
        ));
        drop(connection);
        let bytes = std::fs::read(&bad).unwrap();
        assert!(matches!(
            maintain_index(&bad),
            Err(Error::IndexCorrupt { .. })
        ));
        assert_eq!(bytes, std::fs::read(&bad).unwrap());
    }
    #[test]
    fn publication_checkpoint_and_failed_writer_cleanup_release_the_cursor_fence() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        for failed in [false, true] {
            let path = managed();
            let root = path.parent().unwrap().join("coordinator");
            std::fs::create_dir_all(&root).unwrap();
            let service =
                Arc::new(crate::cursor::CursorService::new(&root, [path.clone()]).unwrap());
            service.sweep().unwrap();
            let mut writer = Writer::open(&path).unwrap();
            populate(&mut writer, "next", !failed);
            let (start, begin) = mpsc::channel();
            let (reply, response) = mpsc::channel();
            let observed = Arc::new(AtomicBool::new(false));
            let fired = Arc::new(AtomicBool::new(false));
            let check = observed.clone();
            let called = fired.clone();
            let sibling = service.clone();
            let database = path.clone();
            let probe = std::thread::spawn(move || {
                if begin.recv_timeout(Duration::from_secs(5)).is_err() {
                    return;
                }
                let execution =
                    crate::execution::Execution::new(Default::default(), Some(5000)).unwrap();
                let result = sibling.request(
                    &database,
                    CursorRequest {
                        action: "settings".into(),
                        remaining_ms: 5000,
                        deadline_at_ms: execution.deadline_ms().unwrap(),
                        ..Default::default()
                    },
                    Default::default(),
                );
                let _ = reply.send(result.is_ok());
            });
            writer
                .connection
                .authorizer(Some(move |action: AuthContext<'_>| {
                    let target = if failed {
                        matches!(
                            action.action,
                            AuthAction::Delete {
                                table_name: "staging_generations",
                                ..
                            }
                        )
                    } else {
                        matches!(
                            action.action,
                            AuthAction::Pragma {
                                pragma_name: "wal_checkpoint",
                                ..
                            }
                        )
                    };
                    if target && !called.swap(true, Ordering::AcqRel) {
                        start.send(()).unwrap();
                        check.store(
                            response
                                .recv_timeout(Duration::from_secs(2))
                                .unwrap_or(false),
                            Ordering::Release,
                        );
                    }
                    // A real postcommit checkpoint failure must not change the
                    // already committed publication into an error outcome.
                    if target && !failed {
                        Authorization::Deny
                    } else {
                        Authorization::Allow
                    }
                }))
                .unwrap();
            let result = service.publish(&path, writer, &crate::resource::Control::default());
            probe.join().unwrap();
            assert!(fired.load(Ordering::Acquire));
            assert!(
                observed.load(Ordering::Acquire),
                "cursor mutex held during checkpoint/failed cleanup"
            );
            if failed {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap() > 1);
            }
            assert!(!maintain_index(&path).unwrap().busy);
        }
    }
}

#[cfg(test)]
mod retention_overlap_tests {
    use super::*;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };

    fn compilation(writer: &mut Writer) {
        writer
            .stage(envelope::Payload::Compilation(Compilation {
                id: "comp".into(),
                references: vec![ProjectReferenceContext {
                    compilation_id: "dependency".into(),
                    project_path: "dependency.csproj".into(),
                    input_fingerprint: "dep".into(),
                }],
                ..Default::default()
            }))
            .unwrap();
    }
    fn complete(writer: &mut Writer) {
        writer
            .stage(envelope::Payload::Report(AnalysisReport {
                compilation_id: "comp".into(),
                status: "complete".into(),
                ..Default::default()
            }))
            .unwrap();
    }
    fn overlap(publication: bool) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/retention-overlap")
            .join(format!(
                "{}-{}-{:x}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("index.db");
        let mut first = Writer::open(&path).unwrap();
        compilation(&mut first);
        complete(&mut first);
        first.publish().unwrap();
        let _retention = crate::cursor::GenerationRetention::activate(&path, [1; 16]).unwrap();
        let mut writer = Writer::open(&path).unwrap();
        compilation(&mut writer);
        if publication {
            complete(&mut writer);
        }
        let (start, starting) = mpsc::sync_channel(1);
        let (event, events) = mpsc::sync_channel(2);
        let child = std::thread::spawn(move || {
            starting.recv().unwrap();
            let mut connection = Connection::open(path).unwrap();
            connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
            let busy_event = event.clone();
            let mut notified = false;
            connection
                .busy_handler(Some(move |count| {
                    if !notified {
                        busy_event.send(true).unwrap();
                        notified = true;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                    count < 2000
                }))
                .unwrap();
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let generation: u64 = tx
                .query_row("SELECT current_generation FROM state", [], |r| r.get(0))
                .unwrap();
            tx.execute(
                "INSERT INTO cursor_generation_pins VALUES(?,?,?)",
                params![[99u8; 16].as_slice(), generation, [1u8; 16].as_slice()],
            )
            .unwrap();
            tx.commit().unwrap();
            let _ = event.send(false);
            generation
        });
        let blocked = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&blocked);
        let mut fired = false;
        writer
            .connection
            .authorizer(Some(move |ctx: AuthContext<'_>| {
                let target = if publication {
                    matches!(
                        ctx.action,
                        AuthAction::Update {
                            table_name: "state",
                            ..
                        }
                    )
                } else {
                    matches!(
                        ctx.action,
                        AuthAction::Insert {
                            table_name: "symbols"
                        }
                    )
                };
                if target && !fired {
                    fired = true;
                    start.send(()).unwrap();
                    observed.store(
                        events.recv_timeout(Duration::from_secs(5)).unwrap(),
                        Ordering::Relaxed,
                    );
                }
                Authorization::Allow
            }))
            .unwrap();
        let published = if publication {
            Some(writer.publish().unwrap())
        } else {
            writer
                .stage(envelope::Payload::Facts(FactBatch {
                    compilation_id: "comp".into(),
                    symbols: vec![Symbol {
                        id: "ref".into(),
                        name: "ref".into(),
                        external: true,
                        definition_compilation_id: "dependency".into(),
                        definition_symbol_id: "target".into(),
                        definition_fingerprint: "dep".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }))
                .unwrap();
            None
        };
        let pinned = child.join().unwrap();
        assert!(
            blocked.load(Ordering::Relaxed),
            "retention committed inside writer's read/write window"
        );
        if let Some(published) = published {
            assert_eq!(pinned, published);
        }
    }
    #[test]
    fn publication_serializes_concurrent_pin_transactions() {
        overlap(true);
    }
    #[test]
    fn reference_validation_and_fact_insertion_serialize_concurrent_pins() {
        overlap(false);
    }
}

#[cfg(test)]
mod cleanup_index_tests {
    use super::*;
    #[test]
    fn cleanup_indexes_cover_owner_and_foreign_key_probes_and_preserve_retained_facts() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        ensure_cleanup_indexes(&connection).unwrap();
        ensure_cleanup_indexes(&connection).unwrap();
        for (query, index) in [
            (
                "SELECT 1 FROM unit_artifacts WHERE owner_generation=1 AND artifact_id='live'",
                "unit_artifact_owner",
            ),
            (
                "SELECT 1 FROM artifacts WHERE content_hash='live'",
                "artifact_content",
            ),
            (
                "SELECT 1 FROM generation_units WHERE owner_generation=1 AND compilation_id='a'",
                "generation_unit_owner",
            ),
        ] {
            let mut plan = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap();
            let rows = plan
                .query_map([], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                rows.iter()
                    .any(|row| row.starts_with("SEARCH") && row.contains(index)),
                "{rows:?}"
            );
            assert!(!rows.iter().any(|row| row.starts_with("SCAN")), "{rows:?}");
        }
        connection.execute_batch("INSERT INTO generations VALUES(1),(2);
            INSERT INTO compilations(generation,id) VALUES(1,'a'),(1,'b');
            INSERT INTO generation_units VALUES(2,'a',1),(1,'b',1);
            INSERT INTO contents VALUES('live',X'61'),('old',X'62');
            INSERT INTO artifacts(generation,id,content_hash) VALUES(1,'live','live'),(1,'old','old');
            INSERT INTO unit_artifacts VALUES(1,'a','live'),(1,'b','old');
            UPDATE state SET current_generation=2;").unwrap();
        reclaim(&connection).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM contents", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            2,
            "retained old manifest must preserve its source"
        );
        connection
            .execute("DELETE FROM generation_units WHERE generation=1", [])
            .unwrap();
        reclaim(&connection).unwrap();
        let remaining: String = connection
            .query_row("SELECT hash FROM contents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, "live");
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM compilations", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            1
        );
        assert!(
            connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
    }
}
