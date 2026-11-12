//! The existing durable host registry. Workspace fact stores stay in the core.
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::{collections::BTreeSet, path::Path};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const VERSION: i64 = 1;

fn supported(connection: &Connection) -> Result<i64> {
    let version = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != 0 && version != VERSION {
        return Err(know_code_core::Error::UpdateRequired(format!(
            "registry format {version} is unsupported; update Know Code before opening this registry"
        ))
        .into());
    }
    Ok(version)
}

/// Installer inspection never migrates or changes registry durability settings.
pub fn open_read_only(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    supported(&connection)?;
    Ok(connection)
}

/// Called after the coordinator has acquired its existing sole-writer fence.
pub fn open(data: &Path) -> Result<Connection> {
    // SQLite must retain normal hot-journal recovery. The version check below
    // precedes our schema/journal/settings writes, not SQLite's own recovery
    // or shared-memory work required to read its committed version safely.
    let connection = Connection::open(data.join("registry.db"))?;
    initialize(&connection)?;
    Ok(connection)
}

fn columns(connection: &Connection, table: &str) -> Result<BTreeSet<String>> {
    // Only the fixed table names below reach this schema query.
    Ok(connection
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get(1))?
        .collect::<std::result::Result<_, _>>()?)
}

fn legacy_shape(connection: &Connection) -> Result<()> {
    let mut known_table = false;
    for (table, required) in [
        (
            "workspaces",
            &[
                "database",
                "root",
                "configuration",
                "generation",
                "inputs",
                "error",
            ][..],
        ),
        ("identities", &["database", "identity"][..]),
        ("tracked_inputs", &["database", "path"][..]),
        ("manifests", &["database", "inputs"][..]),
        ("discovery_parents", &["root", "identity", "exclusions"][..]),
        ("discovery_suppressed", &["root"][..]),
    ] {
        let existing = columns(connection, table)?;
        if !existing.is_empty() {
            known_table |= connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?)",
                [table],
                |row| row.get::<_, bool>(0),
            )?;
        }
        if !existing.is_empty() && required.iter().any(|column| !existing.contains(*column)) {
            return Err(know_code_core::Error::UpdateRequired(format!(
                "unrecognized legacy registry table {table}; registry migration was refused"
            ))
            .into());
        }
    }
    let nonempty: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type IN ('table','view') AND name NOT GLOB 'sqlite_*')",
        [], |row| row.get(0),
    )?;
    if nonempty && !known_table {
        return Err(know_code_core::Error::UpdateRequired(
            "unrecognized nonempty legacy registry; initialization was refused".into(),
        )
        .into());
    }
    Ok(())
}

/// Shared with discovery's existing connection-based open path. Version one
/// names exactly the released additive schema, not a new registration model.
pub fn initialize(connection: &Connection) -> Result<()> {
    let version = supported(connection)?;
    if version == 0 {
        legacy_shape(connection)?;
    }
    // Journal mode cannot change inside a transaction. Check compatibility
    // first; preserve FULL registry durability independently of derived stores.
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    if version == VERSION {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
    if supported(&transaction)? == VERSION {
        // Another initializer may have completed while this one awaited SQL.
        transaction.commit()?;
        return Ok(());
    }
    legacy_shape(&transaction)?;
    transaction.execute_batch("CREATE TABLE IF NOT EXISTS workspaces(database TEXT PRIMARY KEY,root TEXT UNIQUE NOT NULL,configuration TEXT NOT NULL,generation INTEGER NOT NULL,inputs TEXT NOT NULL,error TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS identities(database TEXT PRIMARY KEY,identity TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS tracked_inputs(database TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(database,path));
        CREATE TABLE IF NOT EXISTS manifests(database TEXT PRIMARY KEY,inputs TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS discovery_parents(root TEXT PRIMARY KEY,identity TEXT NOT NULL,exclusions TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS discovery_suppressed(root TEXT PRIMARY KEY);
        INSERT OR IGNORE INTO manifests SELECT database,inputs FROM workspaces;
        UPDATE workspaces SET inputs='{}' WHERE inputs<>'{}';")?;
    let columns = columns(&transaction, "workspaces")?;
    for (name, definition) in [
        ("failed_input_hash", "TEXT NOT NULL DEFAULT ''"),
        ("outcome_code", "TEXT NOT NULL DEFAULT ''"),
        ("cancel_pending", "INTEGER NOT NULL DEFAULT 0"),
        ("disk_pause", "TEXT NOT NULL DEFAULT ''"),
    ] {
        if !columns.contains(name) {
            transaction.execute_batch(&format!(
                "ALTER TABLE workspaces ADD COLUMN {name} {definition}"
            ))?;
            if name == "cancel_pending" {
                // Preserve the released one-time durable cancellation backfill.
                transaction.execute_batch("UPDATE workspaces SET cancel_pending=1,failed_input_hash='' WHERE outcome_code='cancelled';")?;
            }
        }
    }
    transaction.pragma_update(None, "user_version", VERSION)?;
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::types::Value;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    const LEGACY: &str = "CREATE TABLE workspaces(database TEXT PRIMARY KEY,root TEXT UNIQUE NOT NULL,configuration TEXT NOT NULL,generation INTEGER NOT NULL,inputs TEXT NOT NULL,error TEXT NOT NULL);";

    fn directory() -> std::path::PathBuf {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.artifacts/registry");
        std::fs::create_dir_all(&parent).unwrap();
        let root = parent.join(format!(
            "{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        root
    }

    fn rows(connection: &Connection) -> BTreeMap<String, Vec<Vec<Value>>> {
        [
            "workspaces",
            "identities",
            "tracked_inputs",
            "manifests",
            "discovery_parents",
            "discovery_suppressed",
        ]
        .into_iter()
        .map(|table| {
            // The suppression table has only one column.
            let order = if table == "discovery_suppressed" {
                "1"
            } else {
                "1,2"
            };
            let mut query = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
                .unwrap();
            let count = query.column_count();
            let values = query
                .query_map([], |row| {
                    (0..count)
                        .map(|column| row.get(column))
                        .collect::<rusqlite::Result<Vec<Value>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (table.to_owned(), values)
        })
        .collect()
    }

    #[test]
    fn legacy_zero_migration_preserves_rows_and_existing_cancel_backfill() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(LEGACY).unwrap();
        connection.execute_batch("ALTER TABLE workspaces ADD COLUMN failed_input_hash TEXT NOT NULL DEFAULT ''; ALTER TABLE workspaces ADD COLUMN outcome_code TEXT NOT NULL DEFAULT ''; INSERT INTO workspaces VALUES('db','/saved/root','Release',7,'{\"saved.ts\":{\"hash\":\"abc\"}}','saved cancellation','old-failure','cancelled');").unwrap();
        initialize(&connection).unwrap();
        assert_eq!(supported(&connection).unwrap(), VERSION);
        let saved: (String, String, u64, String, bool, String) = connection.query_row(
            "SELECT root,configuration,generation,error,cancel_pending,failed_input_hash FROM workspaces WHERE database='db'", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))
        ).unwrap();
        assert_eq!(
            saved,
            (
                "/saved/root".into(),
                "Release".into(),
                7,
                "saved cancellation".into(),
                true,
                String::new()
            )
        );
        let manifest: String = connection
            .query_row(
                "SELECT inputs FROM manifests WHERE database='db'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(manifest, r#"{"saved.ts":{"hash":"abc"}}"#);
        let before = rows(&connection);
        initialize(&connection).unwrap();
        assert_eq!(rows(&connection), before);
    }

    #[test]
    fn released_zero_registry_roundtrips_settings_and_remains_idempotent() {
        let data = directory();
        let settings = data.join("workspaces/owned/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let personal = br#"{"csharp":{"configuration":"Release"},"project_execution":{"allowed":true,"identity":"saved"}}"#;
        std::fs::write(&settings, personal).unwrap();
        let connection = open(&data).unwrap();
        connection.execute_batch("INSERT INTO workspaces VALUES('owned','/saved/root','Release',7,'{}','saved error','fingerprint','cancelled',1,'saved-disk-pause'); INSERT INTO identities VALUES('owned','saved-identity'); INSERT INTO tracked_inputs VALUES('owned','/linked/input'); INSERT INTO manifests VALUES('owned','{\"saved\":true}'); INSERT INTO discovery_parents VALUES('/saved/parent','parent-identity','[\"excluded\"]'); INSERT INTO discovery_suppressed VALUES('/saved/parent/removed'); PRAGMA user_version=0;").unwrap();
        let before = rows(&connection);
        drop(connection);
        let connection = open(&data).unwrap();
        assert_eq!(rows(&connection), before);
        assert_eq!(std::fs::read(&settings).unwrap(), personal);
        assert_eq!(supported(&connection).unwrap(), VERSION);
        let mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: i32 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!((mode.as_str(), synchronous), ("wal", 2));
        // Released0.0.1 ignores this marker and writes the same columns. Such
        // a compatible write must survive the next current-runtime open too.
        connection.execute_batch("UPDATE workspaces SET generation=8,error='older compatible write' WHERE database='owned'; UPDATE manifests SET inputs='{\"saved\":\"newer\"}' WHERE database='owned';").unwrap();
        let updated = rows(&connection);
        drop(connection);
        let connection = open(&data).unwrap();
        assert_eq!(rows(&connection), updated);
        assert_eq!(std::fs::read(settings).unwrap(), personal);
    }

    #[test]
    fn failed_legacy_migration_rolls_back_all_schema_and_row_changes() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(LEGACY).unwrap();
        connection.execute_batch("INSERT INTO workspaces VALUES('owned','/saved/root','Release',9,'saved-inputs','saved-error'); CREATE TRIGGER owned_migration_fault BEFORE UPDATE ON workspaces BEGIN SELECT RAISE(ABORT,'owned migration failure'); END;").unwrap();
        assert!(initialize(&connection).is_err());
        assert_eq!(supported(&connection).unwrap(), 0);
        assert!(
            !columns(&connection, "workspaces")
                .unwrap()
                .contains("failed_input_hash")
        );
        let tables: Vec<String> = connection
            .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(tables, ["workspaces"]);
        let inputs: String = connection
            .query_row("SELECT inputs FROM workspaces", [], |row| row.get(0))
            .unwrap();
        assert_eq!(inputs, "saved-inputs");
        connection
            .execute_batch("DROP TRIGGER owned_migration_fault")
            .unwrap();
        initialize(&connection).unwrap();
        let inputs: String = connection
            .query_row("SELECT inputs FROM manifests", [], |row| row.get(0))
            .unwrap();
        assert_eq!(inputs, "saved-inputs");
    }

    #[test]
    fn future_registry_versions_are_rejected_without_writes_by_all_open_paths() {
        for version in [-1, VERSION + 1] {
            let data = directory();
            let path = data.join("registry.db");
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("CREATE TABLE future_owned(value TEXT); INSERT INTO future_owned VALUES('preserve');").unwrap();
            connection
                .pragma_update(None, "user_version", version)
                .unwrap();
            drop(connection);
            let original = std::fs::read(&path).unwrap();
            let writer_error = open(&data).err().unwrap().to_string();
            assert!(writer_error.contains("registry format") && writer_error.contains("update"));
            assert!(open_read_only(&path).is_err());
            let connection = Connection::open(&path).unwrap();
            assert!(initialize(&connection).is_err());
            assert!(crate::discovery::Discovery::open(&connection).is_err());
            drop(connection);
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(!data.join("registry.db-wal").exists());
            assert!(!data.join("registry.db-shm").exists());
        }
    }

    #[test]
    fn foreign_only_zero_schema_is_not_adopted_but_discovery_subset_is_preserved() {
        for schema in [
            "CREATE TABLE foreign_state(value TEXT); INSERT INTO foreign_state VALUES('preserve');",
            "CREATE VIEW foreign_state AS SELECT 'preserve' AS value;",
        ] {
            let data = directory();
            let path = data.join("registry.db");
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(schema).unwrap();
            drop(connection);
            let original = std::fs::read(&path).unwrap();
            assert!(
                open(&data)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("unrecognized nonempty legacy registry")
            );
            let connection = open_read_only(&path).unwrap();
            assert_eq!(supported(&connection).unwrap(), 0);
            assert!(columns(&connection, "workspaces").unwrap().is_empty());
            drop(connection);
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(!data.join("registry.db-wal").exists());
        }
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE discovery_suppressed(root TEXT PRIMARY KEY); INSERT INTO discovery_suppressed VALUES('/saved/root');").unwrap();
        initialize(&connection).unwrap();
        let root: String = connection
            .query_row("SELECT root FROM discovery_suppressed", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(root, "/saved/root");
        assert_eq!(supported(&connection).unwrap(), VERSION);
    }

    #[test]
    fn read_only_legacy_inspection_does_not_initialize_or_modify_registry() {
        let data = directory();
        let path = data.join("registry.db");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(LEGACY).unwrap();
        connection.execute_batch("INSERT INTO workspaces VALUES('owned','/saved/root','Release',7,'saved-inputs','saved-error');").unwrap();
        drop(connection);
        let original = std::fs::read(&path).unwrap();
        let connection = open_read_only(&path).unwrap();
        assert_eq!(supported(&connection).unwrap(), 0);
        assert!(columns(&connection, "manifests").unwrap().is_empty());
        drop(connection);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn hot_rollback_journal_recovery_preserves_legacy_registry() {
        const CHILD: &str = "KNOW_CODE_REGISTRY_HOT_FIXTURE";
        if let Some(path) = std::env::var_os(CHILD) {
            let connection = Connection::open(Path::new(&path)).unwrap();
            connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=1; PRAGMA cache_spill=ON; BEGIN IMMEDIATE;").unwrap();
            connection
                .execute(
                    "UPDATE workspaces SET inputs=?",
                    ["uncommitted".repeat(131072)],
                )
                .unwrap();
            // Close the process without running SQLite's connection/transaction
            // destructors. Cache spill leaves actual uncommitted dirty pages
            // and a hot journal for the next owned opener to recover.
            std::process::exit(0);
        }
        let data = directory();
        let path = data.join("registry.db");
        let saved = format!(r#"{{"saved":"{}"}}"#, "x".repeat(1024 * 1024));
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(LEGACY).unwrap();
        connection
            .execute(
                "INSERT INTO workspaces VALUES('owned','/saved/root','Release',7,?,'saved-error')",
                [&saved],
            )
            .unwrap();
        drop(connection);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "registry::tests::hot_rollback_journal_recovery_preserves_legacy_registry",
                "--nocapture",
            ])
            .env(CHILD, &path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let exit = loop {
            if let Some(exit) = child.try_wait().unwrap() {
                break exit;
            }
            if std::time::Instant::now() >= until {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned hot-journal fixture did not finish");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(exit.success());
        assert!(
            std::fs::metadata(data.join("registry.db-journal"))
                .unwrap()
                .len()
                > 512
        );
        let readonly = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|connection| {
                connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            });
        assert!(
            matches!(readonly,
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ErrorCode::ReadOnly
            ),
            "fixture must require SQLite's writable recovery"
        );
        let connection = open(&data).unwrap();
        assert_eq!(supported(&connection).unwrap(), VERSION);
        let recovered: String = connection
            .query_row(
                "SELECT inputs FROM manifests WHERE database='owned'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(recovered, saved);
        let row: (String, u64) = connection
            .query_row(
                "SELECT configuration,generation FROM workspaces WHERE database='owned'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("Release".into(), 7));
    }
}
