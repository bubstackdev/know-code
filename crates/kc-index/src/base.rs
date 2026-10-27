//! Shared bases: an immutable copy of a repository's index at a commit, which a new worktree's index starts as.
//!
//! Worktrees of one repository (and other checkouts of its origin) share a base made from a checkout that was clean
//! at the base's commit. A new checkout's index is a copy-on-write clone of the nearest base (`clonefile` on APFS, a
//! reflink on btrfs and XFS, which [`std::fs::copy`] makes), so its unchanged pages stay the base's on disk. Its first
//! sync is the ordinary one: every file's stat is reset by the clone, so every listed file is hashed, and only those
//! whose content differs from the base are extracted. Queries read one index, as for any checkout. See
//! `docs/design/worktree-overlays.md`.
//!
//! A base is keyed by the repository's identity (its normalized origin, else its git common directory), the commit,
//! the build (schema, resolver and extractor versions) and the privacy policy's key. A checkout clones only a base
//! made under its own policy's key: an index made under a key holds nothing that key blocks, and `VACUUM INTO` leaves
//! no free pages or WAL frames behind. A repository blocked whole has no base.

use std::collections::HashSet;
use std::fs::{self, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use kc_extract::Lang;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::{git, privacy, workspace, writer};

/// Under the cache directory.
pub const DIR: &str = "bases";

/// Bases kept per repository: publishing another retires the least recently used past this.
const KEEP: usize = 3;

/// A new base is published only when the nearest one differs from the checkout's commit in more files than this share
/// of them (and at least [`MIN_DISTANCE`]): a base a few files away serves about as well.
const DISTANCE_SHARE: f64 = 0.02;
const MIN_DISTANCE: usize = 50;

/// `meta` keys a base records about itself, and the commit a clone started from.
const META_COMMIT: &str = "base.commit";
const META_BUILD: &str = "base.build";
const META_DIRNAME: &str = "base.dirname";
/// In a checkout's index: the base it was cloned from (`<file name>`), for provenance.
pub const META_FROM: &str = "base.from";

/// What a checkout's state at its last sync leaves in its index and a base must not carry: the checkout's root, its
/// discovery and `git status` caches, failed reads, when its `node_modules` was last read, and the SCIP file it was
/// told to read (a base is never made from an index that read one).
const CHECKOUT_META: &str =
    "key LIKE 'head.%' OR key LIKE 'disc.%' OR key IN ('root', 'last_sync', 'sync.failed', 'sdk.stamp', 'scip.path', 'scip.stamp', 'scip.report')";

/// Which repository a checkout is a checkout of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The normalized origin, else the canonical git common directory.
    pub key: String,
    /// The first 12 hex digits of the key's hash.
    pub hash: String,
    /// A readable name for file names: the origin's or common directory's last component.
    pub name: String,
}

impl Identity {
    pub fn of(root: &Path) -> Option<Identity> {
        let origin = git::run(root, &["config", "--get", "remote.origin.url"]).ok().and_then(|o| workspace::normalize_remote(String::from_utf8_lossy(&o).trim()));
        let key = match origin {
            Some(o) => o,
            None => fs::canonicalize(git::GitDir::find(root)?.common).ok()?.to_string_lossy().into_owned(),
        };
        Some(Identity::from_key(key))
    }

    fn from_key(key: String) -> Identity {
        let hash = blake3::hash(key.as_bytes()).to_hex()[..12].to_string();
        let last = key.trim_end_matches('/').trim_end_matches("/.git").rsplit(['/', '\\']).next().unwrap_or("repo");
        let name: String = last.trim_end_matches(".git").chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect();
        Identity { key, hash, name: if name.is_empty() { "repo".into() } else { name } }
    }

    fn prefix(&self) -> String {
        format!("{}-{}-", self.name, self.hash)
    }
}

/// A published base.
#[derive(Debug, Clone)]
pub struct Base {
    pub path: PathBuf,
    pub commit: String,
    build: String,
    privacy: String,
    dirname: String,
}

pub fn dir() -> Result<PathBuf> {
    Ok(crate::cache_dir()?.join(DIR))
}

/// Opens a base read-only without creating a `-wal` or `-shm` beside it: a base is never written after it is published.
fn open_base(path: &Path) -> rusqlite::Result<Connection> {
    let uri = format!("file:{}?immutable=1", path.to_string_lossy().replace('%', "%25").replace('?', "%3f").replace('#', "%23"));
    Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
}

fn read(path: &Path) -> Option<Base> {
    let conn = open_base(path).ok()?;
    let meta = |k: &str| conn.query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get::<_, String>(0)).optional().ok().flatten();
    Some(Base {
        path: path.to_path_buf(),
        commit: meta(META_COMMIT)?,
        build: meta(META_BUILD)?,
        privacy: meta(privacy::META_KEY).unwrap_or_default(),
        dirname: meta(META_DIRNAME).unwrap_or_default(),
    })
}

/// Every published base of `id`, whatever its build or policy.
pub fn of(id: &Identity) -> Result<Vec<Base>> {
    let prefix = id.prefix();
    let mut out = Vec::new();
    let items = match fs::read_dir(dir()?) {
        Ok(items) => items,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for item in items {
        let path = item?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if name.starts_with(&prefix) && name.ends_with(".sqlite") && let Some(base) = read(&path) {
            out.push(base);
        }
    }
    Ok(out)
}

/// How many files differ between two commits, or `None` when either isn't in the repository at `root`.
fn distance(root: &Path, from: &str, to: &str) -> Option<usize> {
    if from == to {
        return Some(0);
    }
    let out = git::run(root, &["diff", "--name-only", "--no-renames", "-z", from, to, "--"]).ok()?;
    Some(out.split(|b| *b == 0).filter(|p| !p.is_empty()).count())
}

/// The usable base nearest to `commit` for a checkout at `root` whose policy has `key`: same build, same key, its commit
/// in the repository. With its distance in files.
fn nearest(root: &Path, id: &Identity, commit: &str, key: &str) -> Result<Option<(Base, usize)>> {
    let build = crate::fresh::build();
    let mut best: Option<(Base, usize)> = None;
    for base in of(id)? {
        if base.build != build || base.privacy != key {
            continue;
        }
        let Some(d) = distance(root, &base.commit, commit) else { continue };
        if best.as_ref().is_none_or(|(_, bd)| d < *bd) {
            best = Some((base, d));
        }
    }
    Ok(best)
}

/// Records a use of a base, for eviction.
fn touch(base: &Path) {
    let _ = fs::write(base.with_extension("used"), b"");
}

/// Starts the index at `db` for the checkout at `root` as a clone of the nearest base, if there is one for its policy's
/// `key`. The caller holds the checkout's writer lock and `db` doesn't exist. Returns the base cloned.
pub(crate) fn clone_into(root: &Path, db: &Path, key: &str) -> Result<Option<Base>> {
    if !crate::config::shared_bases()?.0 {
        return Ok(None);
    }
    let Some(id) = Identity::of(root) else { return Ok(None) };
    let Some(commit) = crate::head_commit(root) else { return Ok(None) };
    let Some((base, _)) = nearest(root, &id, &commit, key)? else { return Ok(None) };
    if let Some(parent) = db.parent() {
        fs::create_dir_all(parent)?;
    }
    // A clone where the file system can (`clonefile`, a reflink), a copy elsewhere; renamed into place whole.
    let tmp = db.with_extension(format!("clone.{}", std::process::id()));
    if let Err(e) = fs::copy(&base.path, &tmp) {
        let _ = fs::remove_file(&tmp);
        // Evicted since it was listed: the caller falls back.
        if e.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(e.into());
    }
    let reset = (|| -> Result<()> {
        let conn = Connection::open(&tmp)?;
        let dirname = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        conn.execute_batch(&checkout_reset(dirname != base.dirname))?;
        relocate_path_deps(&conn, root)?;
        conn.execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)", [META_FROM, base.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().as_str()])?;
        Ok(())
    })();
    if let Err(e) = reset.and_then(|()| Ok(fs::rename(&tmp, db)?)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    touch(&base.path);
    Ok(Some(base))
}

/// What a clone can't vouch for in the checkout it starts: every file's stat (so the first sync hashes every file), a
/// repo that is one Ansible role when the checkout's directory has another name (the role is named after it), and the
/// base's own records.
fn checkout_reset(renamed: bool) -> String {
    let mut sql = format!(
        "DELETE FROM meta WHERE {CHECKOUT_META} OR key IN ('{META_COMMIT}', '{META_BUILD}', '{META_DIRNAME}');
         UPDATE files SET mtime_ns = 0, size = -1;
         INSERT OR REPLACE INTO meta(key, value) VALUES ('{}', '1');",
        crate::content::CARRY
    );
    if renamed {
        sql.push_str(&format!("UPDATE files SET extractor_version = 0 WHERE lang = '{}';", Lang::Ansible.as_str()));
    }
    sql
}

/// Resolves every path dependency again against the checkout at `root`, as extracting its manifest there does
/// (`resolve_path_deps`): the repo the path lands in is the checkout's own root, or another repo beside it. Without
/// extracting the manifests again, which would move the index's generation and so recompute its derived data. A
/// manifest the checkout changed is extracted by its first sync anyway.
fn relocate_path_deps(conn: &Connection, root: &Path) -> Result<()> {
    let deps: Vec<(i64, String, String)> = conn
        .prepare("SELECT d.rowid, f.path, d.location FROM package_deps d JOIN files f ON f.id = d.file_id WHERE d.source = 'path'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut put = conn.prepare("UPDATE package_deps SET resolved_repo = ?1 WHERE rowid = ?2")?;
    for (rowid, manifest, location) in deps {
        let dir = Path::new(&manifest).parent().map_or_else(|| root.to_path_buf(), |d| root.join(d));
        let resolved = crate::repo_root(&dir.join(&location)).ok().map(|r| r.to_string_lossy().into_owned());
        put.execute(rusqlite::params![resolved, rowid])?;
    }
    Ok(())
}

/// The checkouts [`publish`] looked at in this process, with their commit and index generation: one is looked at again
/// only once either moves.
static LOOKED: Mutex<Option<HashSet<(PathBuf, String, String)>>> = Mutex::new(None);

/// Publishes a base from the index of the checkout at `root`, when that pays: the repository has another checkout (see
/// [`shared`]), the checkout is clean at its commit (no change to a tracked file, no untracked file), its last sync
/// read every file, it read no SCIP index (whose facts come from a file outside the commit), and no base of the same
/// build and policy is within a few files of its commit.
/// Best effort, after the checkout's derived data is built so the base carries it. Returns the base's path.
pub fn publish(root: &Path) -> Result<Option<PathBuf>> {
    let published = publish_claimed(root);
    // Published or not, the processes waiting for it go on: to clone it, or to index on their own.
    release(root);
    published
}

fn publish_claimed(root: &Path) -> Result<Option<PathBuf>> {
    if !crate::config::shared_bases()?.0 {
        return Ok(None);
    }
    let privacy = privacy::Privacy::load(root);
    if privacy.whole() {
        return Ok(None);
    }
    let key = privacy.key();
    let db = crate::db_path(root)?;
    if !db.exists() {
        return Ok(None);
    }
    let Some(commit) = crate::head_commit(root) else { return Ok(None) };
    // Held through the copy and the adoption: a sync between them would be lost when the index is replaced. Never
    // waited for: another process indexing the checkout publishes after it.
    let Some(_writer) = writer::WriterLock::try_acquire(root)? else { return Ok(None) };
    let source = crate::open_index(&db, false)?;
    let generation = source.generation()?;
    let looked = (root.to_path_buf(), commit.clone(), generation);
    if LOOKED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).contains(&looked) {
        return Ok(None);
    }
    let result = publish_from(root, &source, &commit, &key);
    LOOKED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(looked);
    drop(source);
    if let Ok(Some(base)) = &result {
        // Best effort: unadopted, the index keeps its own copy of the pages.
        let _ = adopt(&db, base);
    }
    result
}

/// Replaces the index of the checkout a base was just published from with a clone of the base, so the two share their
/// pages on disk. The base was copied from this index under the writer lock the caller still holds, so it holds the
/// same facts, file states included; only the checkout's own records are carried over. Only while no connection has
/// the index open (replacing it under one would mix two files): otherwise the index stays as it is.
fn adopt(db: &Path, base: &Path) -> Result<bool> {
    let Some(_idle) = writer::idle(db)? else { return Ok(false) };
    let checkout: Vec<(String, String)> = {
        let conn = Connection::open(db)?;
        // Everything into the main file first, so the WAL beside it is empty when the file is replaced.
        let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
        if busy != 0 {
            return Ok(false);
        }
        let mut stmt = conn.prepare(&format!("SELECT key, value FROM meta WHERE {CHECKOUT_META}"))?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let tmp = db.with_extension(format!("adopt.{}", std::process::id()));
    let made = (|| -> Result<()> {
        fs::copy(base, &tmp)?;
        let conn = Connection::open(&tmp)?;
        conn.execute_batch(&format!("DELETE FROM meta WHERE key IN ('{META_COMMIT}', '{META_BUILD}', '{META_DIRNAME}');"))?;
        let mut put = conn.prepare("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)")?;
        for (k, v) in &checkout {
            put.execute([k, v])?;
        }
        put.execute([META_FROM, base.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().as_str()])?;
        drop(put);
        conn.close().map_err(|(_, e)| e)?;
        fs::rename(&tmp, db)?;
        Ok(())
    })();
    if let Err(e) = made {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    for suffix in ["-wal", "-shm"] {
        let mut name = db.as_os_str().to_os_string();
        name.push(suffix);
        let _ = fs::remove_file(PathBuf::from(name));
    }
    Ok(true)
}

fn publish_from(root: &Path, source: &kc_store::Store, commit: &str, key: &str) -> Result<Option<PathBuf>> {
    if source.meta(privacy::META_KEY)?.unwrap_or_default() != key
        || source.meta("last_sync")?.is_none() && source.meta("disc.at")?.is_none()
        || !crate::failed_reads(source)?.is_empty()
        || source.meta("scip.stamp")?.is_some_and(|s| !s.is_empty())
    {
        return Ok(None);
    }
    let Some(id) = Identity::of(root) else { return Ok(None) };
    if !shared(root, &id)? {
        return Ok(None);
    }
    let files: usize = source.conn().query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))? as usize;
    if let Some((_, d)) = nearest(root, &id, commit, key)?
        && (d == 0 || d < MIN_DISTANCE.max((files as f64 * DISTANCE_SHARE) as usize))
    {
        return Ok(None);
    }
    // Clean: what the index holds is the commit's tree. Untracked files count, since they are indexed.
    let status = git::run(root, &["status", "--porcelain=v1", "--untracked-files=normal", "-z"])?;
    if !status.is_empty() {
        return Ok(None);
    }
    let dir = dir()?;
    fs::create_dir_all(&dir)?;
    let locks = crate::cache_dir()?.join("locks");
    fs::create_dir_all(&locks)?;
    // Named like a repository's lock (`<name>-<12 hex digits>.lock`), so uninstall recognises it.
    let lock = writer::lock_file(&locks.join(format!("bases-{}.lock", id.hash)))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }
    let target = dir.join(format!("{}{commit}.sqlite", id.prefix()));
    let tmp = dir.join(format!("{}{commit}.tmp.{}", id.prefix(), std::process::id()));
    let _ = fs::remove_file(&tmp);
    let made = (|| -> Result<()> {
        // A consistent copy while other processes use the source, without its free pages or WAL.
        source.conn().execute("VACUUM INTO ?1", [tmp.to_string_lossy()])?;
        let conn = Connection::open(&tmp)?;
        let dirname = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        conn.execute_batch(&format!("PRAGMA secure_delete = ON; PRAGMA journal_mode = DELETE; DELETE FROM meta WHERE {CHECKOUT_META} OR key = '{META_FROM}';"))?;
        let mut put = conn.prepare("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)")?;
        for (k, v) in [(META_COMMIT, commit), (META_BUILD, crate::fresh::build().as_str()), (META_DIRNAME, dirname.as_str())] {
            put.execute([k, v])?;
        }
        drop(put);
        conn.close().map_err(|(_, e)| e)?;
        fs::rename(&tmp, &target)?;
        Ok(())
    })();
    if let Err(e) = made {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    touch(&target);
    retire(&id, &target)?;
    Ok(Some(target))
}

/// Whether the repository has another checkout a base would serve: a linked worktree (listed in the git common
/// directory's `worktrees`), or another indexed checkout of its identity. A repository checked out once gains nothing
/// from a base, and on a file system without clones it would cost a copy of its index.
fn shared(root: &Path, id: &Identity) -> Result<bool> {
    let linked = git::GitDir::find(root).and_then(|g| fs::read_dir(g.common.join("worktrees")).ok()).is_some_and(|mut d| d.next().is_some());
    if linked {
        return Ok(true);
    }
    Ok(workspace::all_summaries()?.iter().any(|s| s.root != root && identity_hash(s.origin.as_deref(), &s.root).as_deref() == Some(id.hash.as_str())))
}

/// What [`wait_or_claim`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Nothing to wait for or build: bases are off, the repo has no other checkout, or no commit.
    None,
    /// This process builds the commit's base: the checkout is clean, and no other process was building it.
    Building,
    /// Another process was building a base for the commit; it has published it, given up or run out of time.
    Waited,
}

/// How long a new checkout waits for another process to publish the base of its commit before indexing cold: about
/// what indexing the checkout cold takes, by its size (its git index holds about 100 bytes a tracked file; a cold
/// index with its derived data takes up to about 50 ms a file on a loaded machine), within a minute and 15 minutes.
fn wait_for(root: &Path) -> std::time::Duration {
    let files = crate::git_index_bytes(root) / 100;
    std::time::Duration::from_millis(files.saturating_mul(50).clamp(60_000, 900_000))
}

/// The claims this process holds on bases it is building (see [`wait_or_claim`]), by checkout: released by [`publish`],
/// whatever it decides, and with the process.
static BUILDING: Mutex<Vec<(PathBuf, fs::File)>> = Mutex::new(Vec::new());

/// For a checkout whose index starts with no base to clone (see [`clone_into`]): N jobs that start at once on a commit
/// should index it once, not N times. A clean checkout of the commit claims its base (a lock per repository and
/// commit) and builds it: its index, published when it is synced and its derived data built. A checkout of a commit
/// whose base another process has claimed waits for that process to let go (at most [`wait_for`]), then the caller tries
/// the clone again; a clean one that still finds no base claims it in turn.
pub fn wait_or_claim(root: &Path) -> Result<Turn> {
    if !crate::config::shared_bases()?.0 {
        return Ok(Turn::None);
    }
    let (Some(id), Some(commit)) = (Identity::of(root), crate::head_commit(root)) else { return Ok(Turn::None) };
    if !shared(root, &id)? {
        return Ok(Turn::None);
    }
    if BUILDING.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(r, _)| r == root) {
        return Ok(Turn::Building);
    }
    let locks = crate::cache_dir()?.join("locks");
    fs::create_dir_all(&locks)?;
    // Named like a repository's lock (`<name>-<12 hex digits>.lock`), so uninstall recognises it.
    let lock = writer::lock_file(&locks.join(format!("build{}-{}.lock", &commit[..commit.len().min(12)], id.hash)))?;
    let clean = git::run(root, &["status", "--porcelain=v1", "--untracked-files=normal", "-z"]).is_ok_and(|s| s.is_empty());
    let claim = |lock: fs::File| -> Turn {
        if clean {
            BUILDING.lock().unwrap_or_else(|e| e.into_inner()).push((root.to_path_buf(), lock));
            Turn::Building
        } else {
            Turn::None
        }
    };
    match lock.try_lock() {
        Ok(()) => return Ok(claim(lock)),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }
    let deadline = std::time::Instant::now() + wait_for(root);
    loop {
        kc_store::limits::check()?;
        std::thread::sleep(std::time::Duration::from_millis(200));
        match lock.try_lock() {
            Ok(()) => {
                drop(lock);
                return Ok(Turn::Waited);
            }
            Err(TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {}
            Err(TryLockError::WouldBlock) => return Ok(Turn::Waited),
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

/// Lets go of this process's claim on the base of `root`'s commit, if it holds one.
fn release(root: &Path) {
    BUILDING.lock().unwrap_or_else(|e| e.into_inner()).retain(|(r, _)| r != root);
}

/// When a base was last cloned or published.
fn used(base: &Path) -> std::time::SystemTime {
    fs::metadata(base.with_extension("used")).or_else(|_| fs::metadata(base)).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH)
}

/// Removes a base and its access record. A process cloning it meanwhile has its clone or fails and falls back.
pub fn remove(base: &Path) -> Result<()> {
    for p in [base.to_path_buf(), base.with_extension("used")] {
        match fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Keeps the [`KEEP`] most recently used bases of `id` (always `keep`), and those of another build or policy go.
fn retire(id: &Identity, keep: &Path) -> Result<()> {
    let build = crate::fresh::build();
    let mut bases = of(id)?;
    bases.sort_by_key(|b| std::cmp::Reverse(used(&b.path)));
    let mut kept = 0;
    for b in bases {
        if b.path == keep || (b.build == build && kept < KEEP - 1) {
            kept += usize::from(b.path != keep);
            continue;
        }
        remove(&b.path)?;
    }
    Ok(())
}

/// The identity hash of a checkout from its workspace summary: of its origin, else of its git common directory.
pub(crate) fn identity_hash(origin: Option<&str>, root: &Path) -> Option<String> {
    let key = match origin {
        Some(o) => o.to_string(),
        None => fs::canonicalize(git::GitDir::find(root)?.common).ok()?.to_string_lossy().into_owned(),
    };
    Some(Identity::from_key(key).hash)
}

/// The identity hash in a base's file name (`<name>-<hash>-<commit>.sqlite`).
pub(crate) fn hash_of(base: &Path) -> Option<&str> {
    let stem = base.file_stem()?.to_str()?;
    let (rest, _commit) = stem.rsplit_once('-')?;
    let (_, hash) = rest.rsplit_once('-')?;
    (hash.len() == 12).then_some(hash)
}

/// Whether a base was made by this build: one of another is never cloned.
pub(crate) fn current(base: &Path) -> bool {
    read(base).is_some_and(|b| b.build == crate::fresh::build())
}

/// Every published base file: for maintenance.
pub fn all() -> Result<Vec<(PathBuf, u64, std::time::SystemTime)>> {
    let mut out = Vec::new();
    let items = match fs::read_dir(dir()?) {
        Ok(items) => items,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for item in items {
        let path = item?.path();
        if path.extension().is_some_and(|e| e == "sqlite") {
            let bytes = fs::metadata(&path).map_or(0, |m| m.len());
            out.push((path.clone(), bytes, used(&path)));
        }
    }
    out.sort_by_key(|(_, _, at)| *at);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_names() {
        let id = Identity::from_key("github.com/example/widgets".into());
        assert_eq!(id.name, "widgets");
        assert_eq!(id.hash.len(), 12);
        assert_eq!(Identity::from_key("/work/widgets/.git".into()).name, "widgets");
        assert_eq!(Identity::from_key("host/a b/c?d".into()).name, "c_d");
    }
}
