//! A repo's state at its last sync, kept beside its index (`<index>.fresh`), so that whether the working tree changed
//! since can be told by `stat` alone, without opening the index: a workspace query asks it of every repo its summary
//! rules out, and opening hundreds of SQLite files cost more than the stats.
//!
//! The snapshot only ever says "unchanged" when it proves it; anything else (missing, unreadable, from another build,
//! for another index file) counts as changed, which costs a sync and nothing more. It is built from the index itself
//! after a sync commits, removed before a sync that changes the index commits, and not written while a file can't be
//! read, so it never describes a state the index doesn't hold.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use kc_extract::Lang;
use kc_store::Store;
use serde::{Deserialize, Serialize};

use crate::{Sniffed, discovery_inputs, git, names_hash};

/// Bumped when [`Snapshot`] changes.
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Snapshot {
    format: u32,
    /// The schema and every extractor's version: an index another build wrote may be stale for this one.
    build: String,
    /// Device and inode of the index file: a rebuilt or replaced index is a different file.
    index: (u64, u64),
    /// [`discovery_inputs`] and the `.gitignore` files they cover.
    inputs: String,
    ignores: Vec<String>,
    /// Directories of the last full discovery: path, mtime, names hash.
    dirs: Vec<(String, i64, String)>,
    /// Content-sniffed candidates, as last read.
    sniffed: Vec<Sniffed>,
    /// Indexed files: path, mtime, size.
    files: Vec<(String, i64, i64)>,
}

pub(crate) fn build() -> String {
    let _ = crate::config::load(); // the configured conventions are part of the extractor versions

    let versions: Vec<String> = Lang::ALL.iter().map(|l| format!("{}{}", l.as_str(), l.extractor_version())).collect();
    format!("{}|r{}|{}", kc_store::SCHEMA_VERSION, kc_store::RESOLVER_VERSION, versions.join(","))
}

/// Where the snapshot of the index at `db` lives.
pub(crate) fn path(db: &Path) -> PathBuf {
    db.with_extension("fresh")
}

/// The index file `store` was opened from.
pub(crate) fn db_of(store: &Store) -> Option<PathBuf> {
    store.conn().path().filter(|p| !p.is_empty()).map(PathBuf::from)
}

fn identity(db: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(db).ok().map(|m| (m.dev(), m.ino()))
}

/// Removes the snapshot: before a write that changes what the index holds, or when it can't be trusted.
pub(crate) fn invalidate(store: &Store) {
    if let Some(db) = db_of(store) {
        let target = path(&db);
        if crate::cache::durable_file(&target).is_ok_and(|durable| !durable) { let _ = std::fs::remove_file(target); }
    }
}

/// Whether a snapshot exists for `store`'s index.
pub(crate) fn exists(store: &Store) -> bool {
    db_of(store).is_some_and(|db| path(&db).exists())
}

/// Writes the snapshot of `store`'s index, read from the index, unless a read failed at the last sync.
pub(crate) fn write(store: &Store) -> Result<()> {
    let Some(db) = db_of(store) else { return Ok(()) };
    if crate::cache::durable_file(&path(&db))? { return Ok(()); }
    if !crate::failed_reads(store)?.is_empty() {
        let _ = std::fs::remove_file(path(&db));
        return Ok(());
    }
    let Some(index) = identity(&db) else { return Ok(()) };
    let ignores: Vec<String> = store.meta("disc.ignores")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let (Some(inputs), Some(sniffed)) = (store.meta("disc.inputs")?, store.meta("disc.sniff")?.and_then(|s| serde_json::from_str(&s).ok())) else {
        return Ok(());
    };
    let files: Vec<(String, i64, i64)> = store
        .conn()
        .prepare("SELECT path, mtime_ns, size FROM files")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let snap = Snapshot { format: FORMAT, build: build(), index, inputs, ignores, dirs: store.dirs()?, sniffed, files };
    // Written whole and renamed into place, so a reader sees the old snapshot, the new one, or none.
    let target = path(&db);
    let tmp = target.with_extension(format!("fresh.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(&snap)?)?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

/// Whether `root`'s working tree may differ from what the index at `db` holds: false only when its snapshot proves
/// nothing changed. Every listed directory, content-sniffed candidate and indexed file is `stat`ed; a directory whose
/// mtime moved is listed again, and counts as changed only if its entries did.
pub fn changed(root: &Path, db: &Path) -> bool {
    let Some(snap) = std::fs::read(path(db)).ok().and_then(|b| serde_json::from_slice::<Snapshot>(&b).ok()) else { return true };
    if snap.format != FORMAT || snap.build != build() || identity(db) != Some(snap.index) {
        return true;
    }
    if discovery_inputs(root, &snap.ignores) != snap.inputs {
        return true;
    }
    for (dir, mtime, names) in &snap.dirs {
        let full = if dir.is_empty() { root.to_path_buf() } else { root.join(dir) };
        let Ok(meta) = std::fs::metadata(&full) else { return true };
        if git::mtime_ns(&meta) != *mtime && names_hash(&full).as_deref() != Some(names.as_str()) {
            return true;
        }
    }
    let stat = |p: &str| std::fs::symlink_metadata(root.join(p)).ok().filter(|m| !m.is_symlink()).map(|m| (git::mtime_ns(&m), m.len() as i64));
    let mut admitted: HashSet<&str> = HashSet::new();
    for s in &snap.sniffed {
        if stat(&s.path).unwrap_or((0, -1)) != (s.mtime_ns, s.size) {
            return true;
        }
        if s.lang().is_some() {
            admitted.insert(&s.path);
        }
    }
    let mut indexed_admitted = 0;
    for (p, mtime, size) in &snap.files {
        if Lang::from_path(p).is_none() {
            if !admitted.contains(p.as_str()) {
                return true; // indexed by content, no longer admitted
            }
            indexed_admitted += 1;
        }
        if stat(p) != Some((*mtime, *size)) {
            return true;
        }
    }
    // A candidate admitted but not indexed (new, or too large: counted as changed, which only costs a sync).
    indexed_admitted != admitted.len()
}
