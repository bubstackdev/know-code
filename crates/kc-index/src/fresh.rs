//! A repo's state at its last sync, kept beside its index (`<index>.fresh`), so that whether the working tree changed
//! since can be told by `stat` alone, without opening the index: a workspace query asks it of every repo its summary
//! rules out, and opening hundreds of SQLite files cost more than the stats.
//!
//! The snapshot only ever says "unchanged" when it proves it; anything else (missing, unreadable, from another build,
//! for another index file) counts as changed, which costs a sync and nothing more. It is built from the index itself
//! after a sync commits, removed before a sync that changes the index commits, and not written while a file can't be
//! read, so it never describes a state the index doesn't hold.
//!
//! A sync also reads files that aren't indexed: the SCIP index, the build files its facts depend on, and a declared
//! client package's `package.json` in `node_modules` (see [`Outside`]). The snapshot stamps those too, each before it
//! is read, so a change to one is a change to the repo.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use kc_extract::Lang;
use kc_store::Store;
use serde::{Deserialize, Serialize};

use crate::{Sniffed, discovery_inputs, git, names_hash};

/// Bumped when [`Snapshot`] changes, or what a written one implies: since 4, that the files indexed by content are
/// the candidates admitted by content (see [`admitted_indexed`]).
const FORMAT: u32 = 4;

#[derive(Serialize, Deserialize)]
struct Snapshot {
    format: u32,
    /// The schema and every extractor's version: an index another build wrote may be stale for this one.
    build: String,
    /// [`settings`] at the sync.
    settings: String,
    /// What the sync read outside the indexed files, each with its stamp as taken before the read (see [`Outside`]).
    outside: Vec<(String, String)>,
    /// Device, inode and creation time of the index file: a rebuilt or replaced index is a different file. The
    /// creation time because a file recreated at once can get the inode just freed (ext4 does).
    index: (u64, u64, u128),
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

/// The settings a sync reads besides those the extractor versions carry: whether declared client packages are read.
pub(crate) fn settings(node_modules: bool) -> String {
    format!("node_modules={node_modules}")
}

/// The first fields of a [`Snapshot`], serialized as it is: a snapshot whose file starts with these was written for
/// this build, these settings and these outside stamps.
#[derive(Serialize)]
struct Head<'a> {
    format: u32,
    build: String,
    settings: String,
    outside: &'a [(String, String)],
}

/// The stat a snapshot keeps of a file outside the indexed ones: none (`-`), a symbolic link (marked, then what it
/// leads to), or what [`git::stamp`] says.
fn stamp(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Err(_) => "-".to_string(),
        Ok(m) if m.is_symlink() => format!("link:{}", git::stamp(path)),
        Ok(_) => git::stamp(path),
    }
}

/// What a sync reads that isn't an indexed file, with each path's [`stamp`] taken before the read: a later change
/// then shows as a stamp that moved, and one made during the read too (the index may hold either content, and the
/// snapshot claims neither). `None` once something read can't be stamped (a path that isn't UTF-8, a set of inputs
/// that moved while it was read): the snapshot is then not written, and the repo counts as changed until a sync can
/// stamp everything. With them, the [`settings`] the sync read under.
pub(crate) struct Outside {
    seen: Option<Vec<(String, String)>>,
    settings: String,
}

impl Outside {
    pub(crate) fn new() -> Self {
        Self { seen: Some(Vec::new()), settings: String::new() }
    }

    /// Stamps `path` (repo-relative, or absolute), before the caller reads it.
    pub(crate) fn note(&mut self, root: &Path, path: &Path) {
        let Some(seen) = &mut self.seen else { return };
        match path.to_str() {
            Some(p) => seen.push((p.to_string(), stamp(&root.join(path)))),
            None => self.seen = None,
        }
    }

    pub(crate) fn unprovable(&mut self) {
        self.seen = None;
    }

    /// Records the settings the sync read under (see [`settings`]).
    pub(crate) fn settings(&mut self, settings: String) {
        self.settings = settings;
    }
}

/// Where the snapshot of the index at `db` lives.
pub(crate) fn path(db: &Path) -> PathBuf {
    db.with_extension("fresh")
}

/// The index file `store` was opened from.
pub(crate) fn db_of(store: &Store) -> Option<PathBuf> {
    store.conn().path().filter(|p| !p.is_empty()).map(PathBuf::from)
}

pub(crate) fn identity(db: &Path) -> Option<(u64, u64, u128)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(db).ok()?;
    // 0 where the file system keeps no creation time: device and inode alone, as before.
    let created = m.created().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    Some((m.dev(), m.ino(), created))
}

/// Removes the snapshot: before a write that changes what the index holds, or when it can't be trusted.
pub(crate) fn invalidate(store: &Store) {
    if let Some(db) = db_of(store) {
        let target = path(&db);
        if crate::cache::durable_file(&target).is_ok_and(|durable| !durable) { let _ = std::fs::remove_file(target); }
    }
}

/// Whether `store`'s index has a snapshot written by this build, under these settings, with these outside stamps: a
/// sync that changed nothing in the index writes one only when it hasn't (an older build's, or one from before the
/// SCIP index changed, would otherwise call the repo changed at every query).
pub(crate) fn current(store: &Store, outside: &Outside) -> bool {
    use std::io::Read as _;
    let (Some(db), Some(seen)) = (db_of(store), &outside.seen) else { return false };
    let Ok(head) = serde_json::to_vec(&Head { format: FORMAT, build: build(), settings: outside.settings.clone(), outside: seen }) else { return false };
    // The serialized head less its closing brace is how the snapshot's own serialization starts.
    let head = &head[..head.len() - 1];
    let mut start = vec![0; head.len()];
    std::fs::File::open(path(&db)).and_then(|mut f| f.read_exact(&mut start)).is_ok() && start == head
}

/// Writes the snapshot of `store`'s index, read from the index, unless a read failed at the last sync, or something
/// read outside the indexed files couldn't be stamped (then removes it).
pub(crate) fn write(store: &Store, outside: &Outside) -> Result<()> {
    let Some(db) = db_of(store) else { return Ok(()) };
    if crate::cache::durable_file(&path(&db))? { return Ok(()); }
    let Some(seen) = &outside.seen else {
        let _ = std::fs::remove_file(path(&db));
        return Ok(());
    };
    if !crate::failed_reads(store)?.is_empty() {
        let _ = std::fs::remove_file(path(&db));
        return Ok(());
    }
    let Some(index) = identity(&db) else { return Ok(()) };
    let ignores: Vec<String> = store.meta("disc.ignores")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let (Some(inputs), Some(sniffed)) = (store.meta("disc.inputs")?, store.meta("disc.sniff")?.and_then(|s| serde_json::from_str::<Vec<Sniffed>>(&s).ok())) else {
        return Ok(());
    };
    let files: Vec<(String, i64, i64)> = store
        .conn()
        .prepare("SELECT path, mtime_ns, size FROM files")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if !admitted_indexed(&sniffed, &files) {
        let _ = std::fs::remove_file(path(&db));
        return Ok(());
    }
    let snap = Snapshot { format: FORMAT, build: build(), settings: outside.settings.clone(), outside: seen.clone(), index, inputs, ignores, dirs: store.dirs()?, sniffed, files };
    // Written whole and renamed into place, so a reader sees the old snapshot, the new one, or none.
    let target = path(&db);
    let tmp = target.with_extension(format!("fresh.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(&snap)?)?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

/// Whether `root`'s working tree may differ from what the index at `db` holds: false only when its snapshot proves
/// nothing changed. Every listed directory, content-sniffed candidate, indexed file and file read outside the indexed
/// ones is `stat`ed; a directory whose mtime moved is listed again, and counts as changed only if its entries did.
pub fn changed(root: &Path, db: &Path) -> bool {
    unchanged(root, db).is_none()
}

/// [`changed`]'s proof that nothing changed: the identity of the index file the snapshot proved current (see
/// [`identity`]), or `None` when it doesn't prove it.
pub(crate) fn unchanged(root: &Path, db: &Path) -> Option<(u64, u64, u128)> {
    let snap = std::fs::read(path(db)).ok().and_then(|b| serde_json::from_slice::<Snapshot>(&b).ok())?;
    (!differs(root, db, &snap)).then_some(snap.index)
}

/// Whether `root`'s working tree, or the index file at `db`, may differ from what `snap` recorded.
fn differs(root: &Path, db: &Path, snap: &Snapshot) -> bool {
    if snap.format != FORMAT || snap.build != build() || snap.settings != settings(crate::sdk::enabled()) || identity(db) != Some(snap.index) {
        return true;
    }
    if snap.outside.iter().any(|(p, s)| stamp(&root.join(p)) != *s) {
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
    // One buffer for every path stat-ed: joining each to the root allocated a path per file.
    use std::os::unix::ffi::OsStrExt;
    let mut full = root.as_os_str().as_bytes().to_vec();
    full.push(b'/');
    let base = full.len();
    let mut stat = |p: &str| {
        full.truncate(base);
        full.extend_from_slice(p.as_bytes());
        let path = Path::new(std::ffi::OsStr::from_bytes(&full));
        std::fs::symlink_metadata(path).ok().filter(|m| !m.is_symlink()).map(|m| (git::mtime_ns(&m), m.len() as i64))
    };
    for s in &snap.sniffed {
        if stat(&s.path).unwrap_or((0, -1)) != (s.mtime_ns, s.size) {
            return true;
        }
    }
    // Which files are indexed by content was checked against the candidates when the snapshot was written.
    snap.files.iter().any(|(p, mtime, size)| stat(p) != Some((*mtime, *size)))
}

/// Whether the files indexed by content (a path no extractor claims) are exactly the candidates admitted by content.
/// One indexed by content and no longer admitted, or one admitted and not indexed (new, or too large), counts as
/// changed, which only costs a sync. Both lists are the snapshot's, so this is decided when it is written, not at
/// every check: each path's language took a sixth of a check's instructions on a repo of 32,000 files.
fn admitted_indexed(sniffed: &[Sniffed], files: &[(String, i64, i64)]) -> bool {
    let admitted: HashSet<&str> = sniffed.iter().filter(|s| s.lang().is_some()).map(|s| s.path.as_str()).collect();
    let mut indexed_admitted = 0;
    for (p, _, _) in files {
        if Lang::from_path(p).is_none() {
            if !admitted.contains(p.as_str()) {
                return false;
            }
            indexed_admitted += 1;
        }
    }
    indexed_admitted == admitted.len()
}
