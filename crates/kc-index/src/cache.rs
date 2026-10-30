//! Bounded, disposable index storage. Never walk or remove unrelated cache subdirectories or durable notes.
//!
//! The cache holds one data tree per build that reads different data (see [`crate::data_dir`]): this build's, other
//! versions' under `v/`, and the top-level files of 0.6.1 and older. The budget counts them all. Maintenance evicts the
//! other trees' files first, least recently used first, and only deletes them, never rewrites them: the build that
//! owns one indexes it again if it needs it.

use std::fs::{self, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::{config, writer};

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
    kc_store::limits::current().note_touched(root);
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

/// The repos whose indexes this query used since the last call (see [`kc_store::limits::Operation::take_touched`]).
pub fn take_touched() -> Vec<PathBuf> {
    kc_store::limits::current().take_touched()
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
    let dir = crate::tree_dir()?.join("repos");
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
        let bytes = family(&db).iter().filter_map(|p| Some(own_bytes(p, fs::symlink_metadata(p).ok()?.len()))).sum();
        out.push(Entry { root, db, bytes, used: stamp, newer: schema > kc_store::SCHEMA_VERSION });
    }
    out.sort_by(|a, b| a.used.cmp(&b.used).then_with(|| a.root.cmp(&b.root)));
    Ok(out)
}

/// The bytes a cache file holds on disk of its own. On APFS, an index cloned from a shared base (see [`crate::base`])
/// shares the base's blocks until it writes them, and they are counted with the base. Elsewhere, its length.
pub fn own_bytes(path: &Path, len: u64) -> u64 {
    #[cfg(target_os = "macos")]
    if let Some(n) = private_size(path) {
        return n.min(len);
    }
    let _ = path;
    len
}

/// Whether [`own_bytes`] counts what a file shares with a base apart (APFS), or its whole length (elsewhere).
pub fn counts_shared_apart(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    return private_size(path).is_some();
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

/// The bytes of `path` no other file shares (`ATTR_CMNEXT_PRIVATESIZE`), when the file system says.
#[cfg(target_os = "macos")]
fn private_size(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    #[repr(C, packed(4))]
    struct Out {
        length: u32,
        size: libc::off_t,
    }
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `attrlist` is plain data; zeroed is a valid empty request.
    let mut list: libc::attrlist = unsafe { std::mem::zeroed() };
    list.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    list.forkattr = libc::ATTR_CMNEXT_PRIVATESIZE;
    let mut out = Out { length: 0, size: 0 };
    // SAFETY: `c` is a NUL-terminated path; `list` and `out` outlive the call, and `out`'s size is passed.
    let r = unsafe {
        libc::getattrlist(
            c.as_ptr(),
            (&raw mut list).cast(),
            (&raw mut out).cast(),
            std::mem::size_of::<Out>(),
            libc::FSOPT_ATTR_CMN_EXTENDED,
        )
    };
    let (length, size) = (out.length, out.size);
    (r == 0 && length as usize >= std::mem::size_of::<Out>() && size >= 0).then_some(size as u64)
}

fn global_bytes() -> Result<u64> {
    let cache = crate::tree_dir()?;
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
    let cache = crate::tree_dir()?;
    let limit = if mode == Mode::Maintain { Some(config::max_cache_size()?.0).filter(|n| *n > 0) } else { None };
    let mut report = Report { limit, ..Default::default() };
    // The lock every build takes, 0.6.1 included, at the cache's top: one cleaner at a time across all trees.
    let top = crate::cache_dir()?;
    fs::create_dir_all(top.join("locks"))?;
    let lock = writer::lock_file(&top.join("locks/maintenance.lock"))?;
    match lock.try_lock() {
        Ok(()) => {},
        Err(TryLockError::WouldBlock) => { report.deferred = true; return Ok(report); },
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }
    let mine = if cache.join("repos").is_dir() { entries()?.iter().map(|e| e.bytes).sum::<u64>() } else { 0 } + bases_bytes()? + global_bytes()?;
    let others = other_trees(mode, limit, mine, &mut report)?;
    if !cache.join("repos").is_dir() {
        report.bytes_before = Some(global_bytes()? + others);
        if mode == Mode::Purge { crate::light::clear()?; } else { crate::light::prune_missing()?; }
        if mode == Mode::Purge {
            for (path, _, _) in crate::base::all()? { crate::base::remove(&path)?; }
            let _ = fs::remove_dir(cache.join(crate::base::DIR));
        }
        if !is_notes(&cache.join("workspace.json"), &notes_paths()?) {
            if mode == Mode::Purge { crate::workspace::clear_orphan_summary()?; }
            else { crate::workspace::prune_missing_summaries()?; }
        }
        report.bytes_after = Some(global_bytes()? + others);
        return Ok(report);
    }
    let Some(_metadata) = writer::workspace(false)? else { report.deferred = true; return Ok(report); };
    if mode == Mode::Purge { crate::light::clear()?; } else { crate::light::prune_missing()?; }
    leftovers(&cache);
    let entries = entries()?;
    let bases = bases(mode)?;
    let mut total: u64 = entries.iter().map(|e| e.bytes).sum::<u64>() + bases.kept + global_bytes()? + others;
    report.bytes_before = Some(total);
    // Shared bases are evicted like indexes, in the same least-recently-used order and only while the cache is over
    // its budget, but never one a worktree still uses (see `crate::base::in_use`).
    let mut bases = bases.idle.into_iter().peekable();
    // An oversized checkout must remain usable: never evict the most recently queried index just to rebuild it
    // on the next call. Active stores and explicitly queried roots are protected independently.
    let newest = entries.last().map(|e| e.root.clone());
    let notes = notes_paths()?;
    for entry in entries {
        while let Some((path, bytes, _)) = bases.next_if(|(_, _, at)| *at <= entry.used) {
            if mode == Mode::Maintain && limit.is_some_and(|n| total > n) && !crate::base::in_use(&path) {
                crate::base::remove(&path)?;
                total = total.saturating_sub(bytes);
                report.removed.push(Removed { root: path, reason: "base" });
            }
        }
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
    for (path, bytes, _) in bases {
        if mode == Mode::Maintain && limit.is_some_and(|n| total > n) && !crate::base::in_use(&path) {
            crate::base::remove(&path)?;
            total = total.saturating_sub(bytes);
            report.removed.push(Removed { root: path, reason: "base" });
        }
    }
    if mode == Mode::Purge && !is_notes(&cache.join("workspace.json"), &notes) {
        crate::workspace::clear_orphan_summary()?;
    } else if !is_notes(&cache.join("workspace.json"), &notes) {
        crate::workspace::prune_missing_summaries()?;
    }
    let _ = fs::remove_dir(cache.join("repos")); // only if empty; unknown files and subdirectories stay
    let _ = fs::remove_dir(cache.join(crate::base::DIR));
    report.bytes_after = Some(self::entries()?.iter().map(|e| e.bytes).sum::<u64>() + bases_bytes()? + global_bytes()? + others);
    Ok(report)
}

/// Whether maintenance may evict the indexes of 0.6.1 and older, at the cache's top, when the cache is over budget.
const EVICT_LEGACY: bool = true;

/// The data trees of other builds: other versions' under `v/`, and the top level's (0.6.1 and older) when it holds
/// indexes.
fn other_tree_dirs() -> Result<Vec<(PathBuf, bool)>> {
    let top = crate::cache_dir()?;
    let mine = crate::tree_dir()?;
    let mut out = Vec::new();
    if let Ok(items) = fs::read_dir(top.join(crate::DATA_TREES)) {
        for item in items.flatten() {
            if item.path() != mine && item.file_type().is_ok_and(|t| t.is_dir()) {
                out.push((item.path(), false));
            }
        }
    }
    out.sort();
    if EVICT_LEGACY && top.join("repos").is_dir() {
        out.push((top, true));
    }
    Ok(out)
}

/// An index or a base in another build's tree, with what eviction weighs.
struct Foreign { path: PathBuf, base: bool, root: Option<PathBuf>, bytes: u64, used: u128 }

/// The indexes and bases in the tree at `dir`, read without opening any for writing.
fn foreign(dir: &Path) -> Vec<Foreign> {
    let mut out = Vec::new();
    for (sub, base) in [("repos", false), (crate::base::DIR, true)] {
        let Ok(items) = fs::read_dir(dir.join(sub)) else { continue };
        for item in items.flatten() {
            let path = item.path();
            if path.extension().is_none_or(|e| e != "sqlite") || !item.file_type().is_ok_and(|t| t.is_file()) { continue; }
            // Only KnowCode's own files: an index records its root and is named for it as every version names it
            // (`<name>-<12 hex>.sqlite`); a base has the metadata it is published with. Anything else stays.
            let Ok(conn) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) else { continue };
            let meta = |k: &str| conn.query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get::<_, String>(0)).ok();
            let root = if base {
                if meta(crate::base::META_BUILD).is_none() { continue; }
                None
            } else {
                let Some(root) = meta("root").map(PathBuf::from).filter(|r| r.is_absolute()) else { continue };
                if Some(path.file_name()) != Some(index_name(&root).as_deref().map(std::ffi::OsStr::new)) { continue; }
                Some(root)
            };
            drop(conn);
            let files = if base { vec![path.clone()] } else { family(&path) };
            let bytes = files.iter().filter_map(|p| Some(own_bytes(p, fs::symlink_metadata(p).ok()?.len()))).sum();
            out.push(Foreign { used: access_stamp(&path), path, base, root, bytes });
        }
    }
    out
}

/// The file name every version gives the index of `root`: `<name>-<12 hex digits of its path's hash>.sqlite`.
fn index_name(root: &Path) -> Option<String> {
    crate::db_path(root).ok()?.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The bytes a tree's summary and discovery files hold.
fn tree_globals(dir: &Path) -> u64 {
    ["workspace.json", "discovery.json"].iter().filter_map(|n| fs::symlink_metadata(dir.join(n)).ok().filter(|m| m.is_file())).map(|m| m.len()).sum()
}

/// Evicts other builds' indexes and bases: all of them with a purge, those of deleted checkouts with a prune, and
/// with maintenance those of deleted checkouts and then the least recently used while the whole cache (this build's
/// `mine` bytes included) is over `limit`. An index or base another process holds is kept. A tree under `v/` left
/// with neither goes whole. Returns the bytes the other trees still hold.
fn other_trees(mode: Mode, limit: Option<u64>, mine: u64, report: &mut Report) -> Result<u64> {
    let trees = other_tree_dirs()?;
    let mut items: Vec<(usize, Foreign)> = trees.iter().enumerate().flat_map(|(i, (dir, _))| foreign(dir).into_iter().map(move |f| (i, f))).collect();
    items.sort_by(|a, b| a.1.used.cmp(&b.1.used).then_with(|| a.1.path.cmp(&b.1.path)));
    let globals: u64 = trees.iter().map(|(dir, _)| tree_globals(dir)).sum();
    let mut total = mine + globals + items.iter().map(|(_, f)| f.bytes).sum::<u64>();
    let notes = notes_paths()?;
    let mut kept = 0u64;
    for (_, f) in items {
        let gone = f.root.as_deref().is_some_and(gone);
        let evict = match mode {
            Mode::Purge => true,
            Mode::Prune => gone,
            Mode::Maintain => gone || limit.is_some_and(|n| total > n),
        };
        let label = f.root.clone().unwrap_or_else(|| f.path.clone());
        if !evict || evict_foreign(&f, &notes)?.is_none() {
            if evict { report.kept.push(Kept { root: label, reason: "in_use" }); }
            kept += f.bytes;
            continue;
        }
        total = total.saturating_sub(f.bytes);
        report.removed.push(Removed { root: label, reason: if gone { "deleted" } else if mode == Mode::Purge { "purge" } else { "other_version" } });
    }
    for (dir, legacy) in &trees {
        if !legacy { remove_empty_tree(dir); }
    }
    Ok(kept + other_tree_dirs()?.iter().map(|(dir, _)| tree_globals(dir)).sum::<u64>())
}

/// Removes another build's index (with the writer lock and no reader, as that build names them) or base. `None`
/// when it is in use or overlaps durable notes, and is kept.
fn evict_foreign(f: &Foreign, notes: &[PathBuf]) -> Result<Option<()>> {
    if f.base {
        crate::base::remove(&f.path)?;
        return Ok(Some(()));
    }
    if family(&f.path).iter().any(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_symlink()) || (p.exists() && is_notes(p, notes))) {
        return Ok(None);
    }
    let Some(_writer) = writer::WriterLock::try_acquire_db(&f.path)? else { return Ok(None) };
    let Some(_idle) = writer::idle(&f.path)? else { return Ok(None) };
    remove_payload(&f.path)?;
    Ok(Some(()))
}

/// Removes a tree under `v/` that holds no index or base any more: its summary and discovery files, the lock files
/// no process holds, and its directories once empty. Anything else in it stays.
fn remove_empty_tree(dir: &Path) {
    if !foreign(dir).is_empty() { return; }
    for name in ["workspace.json", "discovery.json"] {
        let _ = fs::remove_file(dir.join(name));
    }
    for sub in ["locks", "repos"] {
        if let Ok(items) = fs::read_dir(dir.join(sub)) {
            for item in items.flatten() {
                let path = item.path();
                let lock = path.extension().is_some_and(|e| e == "lock" || e == "use");
                if lock && writer::lock_file(&path).is_ok_and(|f| f.try_lock().is_ok()) {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        let _ = fs::remove_dir(dir.join(sub));
    }
    let _ = fs::remove_dir(dir.join(crate::base::DIR));
    let _ = fs::remove_dir(dir);
}

/// The shared bases after [`run`] removed what `mode` removes outright: every base with a purge, else those no
/// worktree uses that are of another build or whose repository has no checkout left. What stays, with its bytes and
/// when it was last used (on the scale of [`access_stamp`]), least recently used first.
struct Bases { kept: u64, idle: Vec<(PathBuf, u64, u128)> }

fn bases(mode: Mode) -> Result<Bases> {
    let all = crate::base::all()?;
    let mut out = Bases { kept: 0, idle: Vec::new() };
    if all.is_empty() { return Ok(out); }
    let live: Option<std::collections::HashSet<String>> = if mode == Mode::Purge { None } else {
        Some(crate::workspace::all_summaries()?.iter().filter(|s| !gone(&s.root)).filter_map(|s| crate::base::identity_hash(s.origin.as_deref(), &s.root)).collect())
    };
    for (path, bytes, _) in all {
        let keep = live.as_ref().is_some_and(|live| crate::base::hash_of(&path).is_some_and(|h| live.contains(h)) && crate::base::current(&path))
            || (mode != Mode::Purge && crate::base::in_use(&path));
        if !keep {
            crate::base::remove(&path)?;
            continue;
        }
        let bytes = own_bytes(&path, bytes);
        out.kept += bytes;
        out.idle.push((path.clone(), bytes, access_stamp(&path)));
    }
    Ok(out)
}

/// Removes the temporary files a clone, an adoption or a publication of a base (see [`crate::base`]) left when its
/// process died: `<index>.clone.<pid>`, `<index>.adopt.<pid>` in `repos/`, `<base>.tmp.<pid>` in `bases/`, an hour
/// old. Nothing else.
fn leftovers(cache: &Path) {
    let old = |p: &Path| fs::symlink_metadata(p).ok().filter(|m| m.is_file()).and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 3600);
    for (dir, kinds) in [("repos", &["clone", "adopt"][..]), (crate::base::DIR, &["tmp"][..])] {
        let Ok(items) = fs::read_dir(cache.join(dir)) else { continue };
        for item in items.flatten() {
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            let Some((rest, pid)) = name.rsplit_once('.') else { continue };
            let ours = !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) && kinds.iter().any(|k| rest.ends_with(&format!(".{k}")));
            if ours && old(&path) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

fn bases_bytes() -> Result<u64> {
    Ok(crate::base::all()?.iter().map(|(p, bytes, _)| own_bytes(p, *bytes)).sum())
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
/// it would let the next process lock a different file. Unrecognized files are never touched. Directories left empty
/// go, the cache directory itself only when it is the default one.
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
    // Every build's tree (see `crate::tree_dir`): the locks it holds no more, then the tree once empty.
    if let Ok(items) = fs::read_dir(cache.join(crate::DATA_TREES)) {
        for tree in items.flatten().filter(|t| t.file_type().is_ok_and(|t| t.is_dir())) {
            remove_empty_tree(&tree.path());
        }
        let _ = fs::remove_dir(cache.join(crate::DATA_TREES));
    }
    let updates = cache.join("updates");
    let state = updates.join("state.json");
    let check = updates.join("check.lock");
    let file = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
    if file(&state) || file(&check) {
        if let Some(path) = [&state, &check].into_iter().find(|p| is_notes(p, &notes)) { out.retained.push((path.clone(), "notes")); }
        else if fs::symlink_metadata(&check).is_ok_and(|m| m.is_symlink()) { out.retained.push((check, "symlink")); }
        else {
            // The update check writes its state under this lock. Holding it, the state and then the lock go, as the
            // coordination locks above do; anything else in the directory stays.
            let lock = writer::lock_file(&check)?;
            match lock.try_lock() {
                Ok(()) => for path in [state, check] {
                    if !file(&path) { continue; }
                    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
                    out.removed.push(path);
                },
                Err(TryLockError::WouldBlock) => out.retained.push((state, "in_use")),
                Err(TryLockError::Error(e)) => return Err(e).context("locking the update check"),
            }
        }
    }
    let _ = fs::remove_dir(&updates); // only if empty
    // The default cache directory is KnowCode's own; one the user chose stays even when empty.
    if config::cache_dir()?.1 == config::Source::Default { let _ = fs::remove_dir(&cache); }
    Ok(out)
}

/// Whether the cache has room to index another repo in the background: below nine tenths of `cache.max_size` (any
/// size when it is 0). Past that, indexing a new repo would only make maintenance evict another, which a query then
/// indexes again.
/// Read at most every few seconds per cache and budget: it reads every index's access record.
pub fn room_for_background() -> Result<bool> {
    type Memo = Option<(PathBuf, u64, std::time::Instant, bool)>;
    static MEMO: std::sync::Mutex<Memo> = std::sync::Mutex::new(None);
    let limit = config::max_cache_size()?.0;
    if limit == 0 {
        return Ok(true);
    }
    let cache = crate::cache_dir()?;
    let mut memo = MEMO.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((c, l, at, room)) = memo.as_ref()
        && *c == cache
        && *l == limit
        && at.elapsed() < std::time::Duration::from_secs(5)
    {
        return Ok(*room);
    }
    let others: u64 = other_tree_dirs()?.iter().map(|(dir, _)| foreign(dir).iter().map(|f| f.bytes).sum::<u64>() + tree_globals(dir)).sum();
    let total = entries()?.iter().map(|e| e.bytes).sum::<u64>() + global_bytes()? + others;
    let room = total < limit / 10 * 9;
    *memo = Some((cache, limit, std::time::Instant::now(), room));
    Ok(room)
}

/// Prune missing checkouts, then evict idle least-recently-used indexes until the configured budget is met.
pub fn maintain(protect: &[PathBuf]) -> Result<Report> { run(Mode::Maintain, protect) }
pub fn prune() -> Result<Report> { run(Mode::Prune, &[]) }
pub fn purge() -> Result<Report> {
    let report = run(Mode::Purge, &[])?;
    // This build's own tree too, now that its locks are let go of.
    if !report.deferred {
        remove_empty_tree(&crate::tree_dir()?);
        let _ = fs::remove_dir(crate::cache_dir()?.join(crate::DATA_TREES));
    }
    Ok(report)
}
