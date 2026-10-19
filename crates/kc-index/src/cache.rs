//! Bounded, disposable index storage. Never walk or remove unrelated cache subdirectories or durable notes.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::{config, writer};

thread_local! {
    static TOUCHED: RefCell<BTreeSet<PathBuf>> = const { RefCell::new(BTreeSet::new()) };
}

#[derive(Deserialize, Serialize)]
struct Access { format: u32, root: PathBuf, schema: u32 }

struct Entry { root: PathBuf, db: PathBuf, bytes: u64, used: u128, newer: bool }

#[derive(Debug, Serialize)]
pub struct Removed { pub root: PathBuf, pub reason: &'static str }

#[derive(Debug, Serialize)]
pub struct Kept { pub root: PathBuf, pub reason: &'static str }

#[derive(Debug, Default, Serialize)]
pub struct Report {
    /// Another process is already cleaning this cache; byte counts were not observed.
    pub deferred: bool,
    pub limit: Option<u64>,
    pub bytes_before: Option<u64>,
    pub bytes_after: Option<u64>,
    pub removed: Vec<Removed>,
    pub kept: Vec<Kept>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Maintain, Prune, Purge }

fn family(db: &Path) -> Vec<PathBuf> {
    let mut paths = vec![db.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        let mut name = db.as_os_str().to_os_string();
        name.push(suffix);
        paths.push(name.into());
    }
    paths.extend([db.with_extension("head"), db.with_extension("fresh"), db.with_extension("used")]);
    paths
}

fn resolved(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn notes_paths() -> Result<Vec<PathBuf>> {
    Ok(vec![resolved(&config::notes_path()?.0), resolved(&crate::notes::legacy_path()?)])
}

fn is_notes(path: &Path, notes: &[PathBuf]) -> bool {
    let path = resolved(path);
    notes.iter().any(|n| path == *n || path.as_os_str().to_string_lossy().starts_with(&format!("{}.migrated", n.display())))
}

pub fn durable_file(path: &Path) -> Result<bool> { Ok(is_notes(path, &notes_paths()?)) }

/// Records actual use, separately from SQLite's mtime (summary reads and derived writes are not user access).
pub(crate) fn touch(root: &Path, db: &Path) {
    TOUCHED.with(|r| { r.borrow_mut().insert(root.to_path_buf()); });
    let target = db.with_extension("used");
    let Ok(notes) = notes_paths() else { return };
    if is_notes(&target, &notes) || fs::symlink_metadata(&target).is_ok_and(|m| m.is_symlink()) { return; }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = target.with_extension(format!("used.{}.{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let result = (|| -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new().create_new(true).write(true).mode(0o600).open(&tmp)?;
        let access = Access { format: 1, root: root.to_path_buf(), schema: kc_store::SCHEMA_VERSION };
        file.write_all(&serde_json::to_vec(&access)?)?;
        fs::rename(&tmp, &target)?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(tmp); }
}

/// Successful CLI operations clean only after their stores have closed. Configuration-only commands do no work.
pub fn after_command() {
    let roots = take_touched();
    if !roots.is_empty() { maintain_and_report(&roots); }
}

pub fn take_touched() -> Vec<PathBuf> {
    TOUCHED.with(|r| std::mem::take(&mut *r.borrow_mut()).into_iter().collect())
}

pub fn maintain_and_report(protect: &[PathBuf]) {
    let mut roots = protect.to_vec();
    roots.extend(take_touched());
    match maintain(&roots) {
        Ok(report) => {
            if !report.removed.is_empty() { eprintln!("knowcode: removed {} unused cached indexes", report.removed.len()); }
            if let (Some(bytes), Some(limit)) = (report.bytes_after, report.limit) && bytes > limit {
                eprintln!("knowcode: index cache is over its {limit}-byte limit ({bytes} bytes); current, in-use, or protected indexes were retained");
            }
        }
        Err(e) => eprintln!("knowcode: cache maintenance: {e:#}"),
    }
}

fn entries() -> Result<Vec<Entry>> {
    let dir = crate::cache_dir()?.join("repos");
    let mut out = Vec::new();
    let items = match fs::read_dir(dir) {
        Ok(items) => items,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for item in items {
        let item = item?;
        let db = item.path();
        if db.extension().is_none_or(|s| s != "sqlite") || !item.file_type()?.is_file() { continue; }
        let used = db.with_extension("used");
        let access: Option<Access> = fs::symlink_metadata(&used).ok().filter(|m| m.is_file())
            .and_then(|_| fs::read(&used).ok()).and_then(|b| serde_json::from_slice(&b).ok()).filter(|a: &Access| a.format == 1);
        // Older indexes have no access record. Read only identity metadata, never migrate or parse their source.
        let identity = access.map(|a| (a.root, a.schema)).or_else(|| {
            let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
            let root: String = conn.query_row("SELECT value FROM meta WHERE key = 'root'", [], |r| r.get(0)).ok()?;
            let schema: String = conn.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0)).ok()?;
            Some((PathBuf::from(root), schema.parse().ok()?))
        });
        let Some((root, schema)) = identity else { continue };
        if !root.is_absolute() || crate::db_path(&root)?.file_name() != db.file_name() { continue; }
        let stamp = access_stamp(&db);
        let bytes = family(&db).iter().filter_map(|p| fs::symlink_metadata(p).ok()).map(|m| m.len()).sum();
        out.push(Entry { root, db, bytes, used: stamp, newer: schema > kc_store::SCHEMA_VERSION });
    }
    out.sort_by(|a, b| a.used.cmp(&b.used).then_with(|| a.root.cmp(&b.root)));
    Ok(out)
}

fn global_bytes() -> Result<u64> {
    let cache = crate::cache_dir()?;
    Ok(["workspace.json", "discovery.json"].iter().map(|name| fs::symlink_metadata(cache.join(name)).ok().filter(|m| m.is_file()).map_or(0, |m| m.len())).sum())
}

fn gone(root: &Path) -> bool {
    fs::symlink_metadata(root.join(".git")).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

fn access_stamp(db: &Path) -> u128 {
    fs::metadata(db.with_extension("used")).or_else(|_| fs::metadata(db)).ok()
        .and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

/// Removes only recognized index payloads. The caller holds the writer and exclusive lifetime leases.
pub(crate) fn remove_files(db: &Path) -> Result<()> {
    remove_payload(db)?;
    Ok(())
}

fn remove_payload(db: &Path) -> Result<()> {
    let notes = notes_paths()?;
    let paths = family(db);
    for path in &paths {
        if fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) { bail!("index has a symlinked cache file; retained {}", path.display()); }
        if path.exists() && is_notes(path, &notes) { bail!("index files overlap durable notes; retained {}", path.display()); }
    }
    for path in paths {
        match fs::remove_file(&path) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }
    Ok(())
}

fn run(mode: Mode, protect: &[PathBuf]) -> Result<Report> {
    let cache = crate::cache_dir()?;
    let limit = if mode == Mode::Maintain { Some(config::max_cache_size()?.0).filter(|n| *n > 0) } else { None };
    let mut report = Report { limit, ..Default::default() };
    if !cache.join("repos").is_dir() {
        report.bytes_before = Some(global_bytes()?);
        if mode == Mode::Purge { crate::light::clear()?; } else { crate::light::prune_missing()?; }
        if !is_notes(&cache.join("workspace.json"), &notes_paths()?) {
            if mode == Mode::Purge { crate::workspace::clear_orphan_summary()?; }
            else { crate::workspace::prune_missing_summaries()?; }
        }
        report.bytes_after = Some(global_bytes()?);
        return Ok(report);
    }
    fs::create_dir_all(cache.join("locks"))?;
    let lock = writer::lock_file(&cache.join("locks/maintenance.lock"))?;
    match lock.try_lock() {
        Ok(()) => {},
        Err(TryLockError::WouldBlock) => { report.deferred = true; return Ok(report); },
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }
    let Some(_metadata) = writer::workspace(false)? else { report.deferred = true; return Ok(report); };
    if mode == Mode::Purge { crate::light::clear()?; } else { crate::light::prune_missing()?; }
    let entries = entries()?;
    let mut total: u64 = entries.iter().map(|e| e.bytes).sum::<u64>() + global_bytes()?;
    report.bytes_before = Some(total);
    // An oversized checkout must remain usable: never evict the most recently queried index just to rebuild it
    // on the next call. Active stores and explicitly queried roots are protected independently.
    let newest = entries.last().map(|e| e.root.clone());
    let notes = notes_paths()?;
    for entry in entries {
        let missing = gone(&entry.root);
        let over = limit.is_some_and(|n| total > n);
        if !(mode == Mode::Purge || missing || (mode == Mode::Maintain && over)) { continue; }
        let keep = if entry.newer { Some("newer_schema") }
            else if mode == Mode::Maintain && cache.join("workspace.json").exists() && is_notes(&cache.join("workspace.json"), &notes) { Some("notes") }
            else if family(&entry.db).iter().any(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_symlink())) { Some("symlink") }
            else if family(&entry.db).iter().any(|p| p.exists() && is_notes(p, &notes)) { Some("notes") }
            else if mode == Mode::Maintain && !missing && (protect.contains(&entry.root) || newest.as_ref() == Some(&entry.root)) { Some("current") }
            else { None };
        if let Some(reason) = keep { report.kept.push(Kept { root: entry.root, reason }); continue; }
        let Some(_writer) = writer::WriterLock::try_acquire(&entry.root)? else {
            report.kept.push(Kept { root: entry.root, reason: "in_use" }); continue;
        };
        let Some(_idle) = writer::idle(&entry.db)? else {
            report.kept.push(Kept { root: entry.root, reason: "in_use" }); continue;
        };
        // Another process may have queried and closed this index since the LRU scan. Let the next maintenance
        // pass rank that use instead of evicting a just-used index from an obsolete ordering.
        if mode == Mode::Maintain && !missing && access_stamp(&entry.db) != entry.used {
            report.kept.push(Kept { root: entry.root, reason: "recently_used" }); continue;
        }
        let previous_global = global_bytes()?;
        if mode == Mode::Maintain && !missing {
            crate::workspace::preserve_summary(&entry.db)?;
            remove_payload(&entry.db)?;
        } else {
            remove_files(&entry.db)?;
            crate::workspace::forget_summary(&entry.db)?;
        }
        total = total.saturating_sub(entry.bytes).saturating_sub(previous_global) + global_bytes()?;
        report.removed.push(Removed { root: entry.root, reason: if missing { "deleted" } else if mode == Mode::Purge { "purge" } else { "size" } });
    }
    if mode == Mode::Purge && !is_notes(&cache.join("workspace.json"), &notes) {
        crate::workspace::clear_orphan_summary()?;
    } else if !is_notes(&cache.join("workspace.json"), &notes) {
        crate::workspace::prune_missing_summaries()?;
    }
    let _ = fs::remove_dir(cache.join("repos")); // only if empty; unknown files and subdirectories stay
    report.bytes_after = Some(self::entries()?.iter().map(|e| e.bytes).sum::<u64>() + global_bytes()?);
    Ok(report)
}

/// What [`remove_owned_state`] removed and what it retained, with why.
#[derive(Debug, Default)]
pub struct OwnedState { pub removed: Vec<PathBuf>, pub retained: Vec<(PathBuf, &'static str)> }

/// A coordination lock KnowCode creates in `<cache>/locks`: the maintenance and summary locks, and a repository's
/// writer (`.lock`) and reader (`.use`) leases, named like its index (`<name>-<12 hex digits>`).
fn owned_lock(name: &str) -> bool {
    if name == "maintenance.lock" || name == "workspace-meta.lock" { return true; }
    let Some(stem) = name.strip_suffix(".lock").or_else(|| name.strip_suffix(".use")) else { return false };
    stem.rsplit_once('-').is_some_and(|(repo, hash)| !repo.is_empty() && hash.len() == 12 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// For uninstall, after [`purge`]: the coordination locks in `<cache>/locks` and the update check's state, which
/// pruning and purging keep on purpose. A lock another process holds (a running client) is retained, since unlinking
/// it would let the next process lock a different file. Unrecognized files are never touched.
pub fn remove_owned_state() -> Result<OwnedState> {
    let cache = crate::cache_dir()?;
    let notes = notes_paths()?;
    let mut out = OwnedState::default();
    let locks = cache.join("locks");
    if let Ok(items) = fs::read_dir(&locks) {
        for item in items {
            let item = item?;
            let path = item.path();
            if !item.file_type()?.is_file() || !owned_lock(&item.file_name().to_string_lossy()) { continue; }
            if is_notes(&path, &notes) { out.retained.push((path, "notes")); continue; }
            let file = writer::lock_file(&path)?;
            match file.try_lock() {
                Ok(()) => {
                    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
                    out.removed.push(path);
                }
                Err(TryLockError::WouldBlock) => out.retained.push((path, "in_use")),
                Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("locking {}", path.display())),
            }
        }
        let _ = fs::remove_dir(&locks); // only if empty
    }
    let updates = cache.join("updates");
    let state = updates.join("state.json");
    if fs::symlink_metadata(&state).is_ok_and(|m| m.is_file()) {
        if is_notes(&state, &notes) { out.retained.push((state, "notes")); }
        else {
            // The update check writes its state under this lock; it stays, as does anything else in the directory.
            let lock = writer::lock_file(&updates.join("check.lock"))?;
            match lock.try_lock() {
                Ok(()) => { fs::remove_file(&state).with_context(|| format!("removing {}", state.display()))?; out.removed.push(state); }
                Err(TryLockError::WouldBlock) => out.retained.push((state, "in_use")),
                Err(TryLockError::Error(e)) => return Err(e).context("locking the update check"),
            }
        }
    }
    Ok(out)
}

/// Prune missing checkouts, then evict idle least-recently-used indexes until the configured budget is met.
pub fn maintain(protect: &[PathBuf]) -> Result<Report> { run(Mode::Maintain, protect) }
pub fn prune() -> Result<Report> { run(Mode::Prune, &[]) }
pub fn purge() -> Result<Report> { run(Mode::Purge, &[]) }
