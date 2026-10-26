//! A cached summary of every indexed repo: its root, packages and provided modules.
//!
//! Questions that span the workspace (which repo declares a package, which repos depend on this one) would
//! otherwise open every repo's index on every query. The summary lives in `<cache>/workspace.json`, one entry per
//! index file, reused while the index file and its WAL keep the same modification time and size. Size eviction
//! retains the last summary so cross-repo discovery survives; answers using it are stale until the index is
//! restored. Repos are read as last synced: manifests catch up when that repo is next queried or indexed.
//!
//! The workspace is every indexed repo, unless workspace roots are configured (`[workspace] roots` in `config.toml`,
//! see [`crate::roots`]): then it is the indexed repos under the roots, plus the repos a query answers for (the
//! `focus` each caller passes to [`summaries`]), so a query about a repo outside every root still answers for it and
//! links it to the roots' repos.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use kc_store::Store;
use kc_store::manifests::{self, PackageOwners, PackageRow};
use kc_store::query;
use serde::{Deserialize, Serialize};

use crate::{cache_dir, git};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoSummary {
    pub root: PathBuf,
    pub packages: Vec<PackageRow>,
    /// Modules the repo provides (its targets and products), matched against other repos' imports.
    pub modules: Vec<String>,
    /// HTTP routes the repo serves and requests.
    #[serde(default)]
    pub routes: Vec<query::RouteRow>,
    /// Hosts its config and code say it serves, or its clients send to (see [`crate::http`]).
    #[serde(default)]
    pub hosts: Vec<query::HostRow>,
    /// The `origin` remote, normalized to `host/path` (see [`normalize_remote`]): what makes two checkouts (a
    /// clone and a worktree, say) the same repo.
    #[serde(default)]
    pub origin: Option<String>,
    /// Selectors its Angular components and directives declare.
    #[serde(default)]
    pub selectors: Vec<query::NgSelector>,
    /// Message-bus facts: message types it declares, what it publishes, what it consumes.
    #[serde(default)]
    pub bus: Vec<kc_store::bus::BusRow>,
    /// SQL tables, views and routines it defines.
    #[serde(default)]
    pub sql: Vec<kc_store::sql::SqlDef>,
    /// Names its code may link to SQL definitions by: Entity Framework tables and called stored procedures.
    #[serde(default)]
    pub db_names: Vec<String>,
    /// Ansible roles it defines, what its tasks deploy, role references leaving it.
    #[serde(default)]
    pub ansible: kc_store::ansible::Summary,
    /// GitLab CI files, jobs and what pipelines are resolved from.
    #[serde(default)]
    pub ci: kc_store::ci::Summary,
    /// Terraform module calls and where their sources point.
    #[serde(default)]
    pub terraform: kc_store::terraform::Summary,
    /// When the index file was last written, in nanoseconds: among checkouts of one repo, the latest answers.
    #[serde(default)]
    pub indexed_at: i64,
    /// A Bloom filter of the names its references and its types' supertypes use, hex-encoded; see
    /// [`RepoSummary::may_reference`].
    #[serde(default)]
    names: String,
    /// A Bloom filter of the names its symbols define (SQL names also lower-cased); see [`RepoSummary::may_define`].
    #[serde(default)]
    defines: String,
    /// A Bloom filter of the lower-cased names `tables` matches: Entity Framework tables, entities, DbSets and
    /// procedure calls (with and without a schema), and SQL tables, views and routines; see
    /// [`RepoSummary::may_name_table`].
    #[serde(default)]
    tables: String,
    /// [`index_stamp`] when this summary was read, or last found still valid.
    stamp: String,
    /// What the summary was read from (see [`summary_generation`]): while the index's is the same, the summary is
    /// too, whatever else was written to the index since (a reparse changing nothing it reads, a cache).
    #[serde(default)]
    generation: String,
    /// Whether the checkout is in the workspace only as a query's focus, outside the configured roots: the checkout
    /// policy ([`choose`]) prefers one under them. Set by the workspace view, never stored.
    #[serde(skip)]
    pub outside_roots: bool,
    /// What the repo's AI privacy file blocks, as its index was purged for (see [`crate::privacy`]). A repo blocked
    /// whole has nothing else in its summary but its root and origin.
    #[serde(default)]
    pub blocked: crate::privacy::Blocked,
    /// [`crate::privacy::Privacy::key`] the index was purged for: a summary whose repo's policy has another one now
    /// is brought in line (see [`summaries`]) before anything is read from it.
    #[serde(default)]
    privacy_key: String,
    /// Whether the checkout is gone (no `.git` at its root) while its index is left: as [`summaries`] last found it,
    /// never stored. Cross-repo links leave it out (see [`distinct`]) and answers name it where it would have
    /// mattered (see [`note_missing`]).
    #[serde(skip)]
    pub gone: bool,
    /// Discovery contains manifests and name filters, not the complete graph facts used by cross-repo joins.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub light: bool,
}

/// Bumped whenever [`RepoSummary`] changes. With the index schema version, it names the summary file's format: a
/// file in any other format (written by an older or newer build sharing the cache) is rebuilt, not trusted, since
/// its entries' stamps would still match while their contents lack what this build reads.
const SUMMARY_FORMAT: u32 = 19;

#[derive(Serialize, Deserialize)]
struct SummaryFile {
    format: String,
    repos: HashMap<String, RepoSummary>,
}

fn format() -> String {
    format!("{SUMMARY_FORMAT}.{}", kc_store::SCHEMA_VERSION)
}

pub(crate) fn has_saved_index(root: &Path) -> Result<bool> {
    let path = cache_dir()?.join("workspace.json");
    Ok(std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<SummaryFile>(&b).ok())
        .filter(|f| f.format == format()).is_some_and(|f| f.repos.values().any(|s| s.root == root && !s.light && !s.generation.is_empty())))
}

/// The last summaries this process read, with the stamps they were read at. A query asks for the summary from
/// several places (dependencies, dependents, HTTP, Angular, the bus), and parsing the file each time cost more than
/// the rest of the query. Reused only while every index keeps its stamp, so answers are the same as re-reading.
struct Memo {
    dir: PathBuf,
    summary_stamp: String,
    stamps: HashMap<String, String>,
    /// Every indexed repo's summary.
    full: Arc<Vec<RepoSummary>>,
    /// What decided which of them are in the workspace (see [`view_key`]).
    view: Option<String>,
    /// The workspace's: `full` itself when no roots are configured.
    all: Arc<Vec<RepoSummary>>,
    graph: OnceLock<Arc<Graph>>,
}

static MEMO: Mutex<Option<Memo>> = Mutex::new(None);

/// What decides which indexed repos are in the workspace: `None` when no roots are configured (every one is, whatever
/// the focus), else the roots, the depth and the focus.
fn view_key(focus: &[PathBuf]) -> Result<Option<String>> {
    let (roots, depth) = crate::config::workspace()?;
    if roots.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!("{roots:?}|{depth}|{focus:?}")))
}

/// The workspace's summaries out of every indexed repo's (see the module docs).
fn view(full: &Arc<Vec<RepoSummary>>, key: &Option<String>, focus: &[PathBuf]) -> Result<Arc<Vec<RepoSummary>>> {
    let Some(mut roots) = key.as_ref().map(|_| crate::roots::Roots::configured()).transpose()?.flatten() else { return Ok(full.clone()) };
    let under: Vec<bool> = full.iter().map(|s| roots.contains(&s.root)).collect();
    if under.iter().all(|u| *u) {
        return Ok(full.clone());
    }
    Ok(Arc::new(
        full.iter()
            .zip(under)
            .filter(|(s, under)| *under || focus.contains(&s.root))
            .map(|(s, under)| RepoSummary { outside_roots: !under, ..s.clone() })
            .collect(),
    ))
}

/// One summary per repo in the workspace, sorted by root: every indexed repo, or with workspace roots configured,
/// those under the roots plus `focus`, the repos the caller answers for (see the module docs).
pub fn summaries(focus: &[PathBuf]) -> Result<Arc<Vec<RepoSummary>>> {
    let summaries = load_private(focus)?.1;
    let query = kc_store::limits::current();
    for s in summaries.iter().filter(|s| s.cached_only()) {
        query.note_cached_read(&s.root, s.generation());
    }
    Ok(summaries)
}

/// The summary snapshots this query read, by repo, with their generations: even if a later lookup has already
/// restored their indexes.
pub fn cached_reads() -> HashMap<PathBuf, String> {
    kc_store::limits::current().cached_reads()
}

/// One summary per indexed repo, in the workspace or not, sorted by root.
pub fn all_summaries() -> Result<Arc<Vec<RepoSummary>>> {
    Ok(load_private(&[])?.0)
}

/// [`load`], with every repo's summary made under its AI privacy policy as it is now: a repo whose privacy file
/// blocks something its index was not purged for (a file added or edited since its last sync) is opened, which
/// purges it (see `crate::open`), and the summaries read again. Without this, cross-repo answers would read what
/// the summary kept of a repo nobody queried since its file appeared. Each repo's file is read at most once a
/// second per process.
fn load_private(focus: &[PathBuf]) -> Result<(Shared, Shared)> {
    let loaded = load(focus)?;
    let stale: Vec<&PathBuf> = loaded.0.iter().filter(|s| !s.gone && privacy_key_now(&s.root) != s.privacy_key).map(|s| &s.root).collect();
    if stale.is_empty() {
        return Ok(loaded);
    }
    for root in stale {
        if loaded.0.iter().any(|s| &s.root == root && s.light) && !crate::db_path(root)?.exists() {
            crate::light::redact(root)?;
            let privacy = crate::privacy::Privacy::load(root);
            if privacy.blocks_anything() { crate::notes::purge(root, |p| privacy.blocks(p))?; }
        } else { drop(crate::open(root)?); }
        forget_key(root);
    }
    load(focus)
}

/// [`crate::privacy::Privacy::key`] of the repo at `root`, remembered for a second.
fn privacy_key_now(root: &Path) -> String {
    let now = std::time::Instant::now();
    use std::os::unix::fs::MetadataExt;
    let format = crate::config::privacy_format();
    let file = std::fs::symlink_metadata(root.join(&format.file))
        .map(|m| format!("{}:{}:{}:{}:{}:{}:{}:{}", m.dev(), m.ino(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec(), m.mode(), m.len()))
        .unwrap_or_else(|e| format!("{:?}", e.kind()));
    let (block_at, malformed, _) = crate::config::privacy();
    let stamp = format!("{file}|{block_at}|{malformed:?}|{format:?}");
    let mut memo = PRIVACY_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    let memo = memo.get_or_insert_with(HashMap::new);
    if let Some((at, previous, key)) = memo.get(root)
        && *previous == stamp
        && now.duration_since(*at) < std::time::Duration::from_secs(1)
    {
        return key.clone();
    }
    let key = crate::privacy::Privacy::load(root).key();
    memo.insert(root.to_path_buf(), (now, stamp, key.clone()));
    key
}

fn forget_key(root: &Path) {
    if let Some(m) = PRIVACY_KEYS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.remove(root);
    }
}

type KeyMemo = HashMap<PathBuf, (std::time::Instant, String, String)>;
static PRIVACY_KEYS: Mutex<Option<KeyMemo>> = Mutex::new(None);

/// Summaries as [`summaries`] shares them.
type Shared = Arc<Vec<RepoSummary>>;

/// Every indexed repo's summary, and the workspace's.
fn load(focus: &[PathBuf]) -> Result<(Shared, Shared)> {
    crate::light::ensure_discovered()?;
    let key = view_key(focus)?;
    let dir = cache_dir()?.join("repos");
    let summary_path = cache_dir()?.join("workspace.json");
    let summary_stamp = format!("{}|{}", git::stamp(&summary_path), crate::light::stamp()?);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries.collect::<std::io::Result<Vec<_>>>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let mut stamps: HashMap<String, (PathBuf, String)> = HashMap::new();
    for entry in entries {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "sqlite") {
            continue;
        }
        let key = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stamp = index_stamp(&path);
        stamps.insert(key, (path, stamp));
    }
    // Whether each checkout is gone (see [`RepoSummary::gone`]): one `stat` per repo per call, which the privacy check
    // (see [`load_private`]) needed anyway.
    let gone = |s: &RepoSummary| !s.root.join(".git").exists();
    let mut memo = MEMO.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = memo.as_mut().filter(|m| m.dir == dir)
        && m.summary_stamp == summary_stamp
        && m.stamps.len() == stamps.len()
        && stamps.iter().all(|(k, (_, s))| m.stamps.get(k) == Some(s))
    {
        // A checkout deleted (or back) since: the same summaries, marked anew.
        let flags: Vec<bool> = m.full.iter().map(gone).collect();
        let moved = m.full.iter().zip(&flags).any(|(s, g)| s.gone != *g);
        if moved {
            m.full = Arc::new(m.full.iter().zip(flags).map(|(s, gone)| RepoSummary { gone, ..s.clone() }).collect());
        }
        if moved || m.view != key {
            m.all = view(&m.full, &key, focus)?;
            m.view = key;
            m.graph = OnceLock::new();
        }
        return Ok((m.full.clone(), m.all.clone()));
    }
    let (mut full, read_stamps) = refresh(stamps)?;
    for summary in crate::light::summaries()? {
        if !full.iter().any(|s| s.root == summary.root && !s.light) {
            full.retain(|s| s.root != summary.root);
            full.push(summary);
        }
    }
    full.sort_by(|a, b| a.root.cmp(&b.root));
    for s in &mut full {
        s.gone = gone(s);
    }
    let full = Arc::new(full);
    let all = view(&full, &key, focus)?;
    *memo = Some(Memo { dir, summary_stamp: format!("{}|{}", git::stamp(&summary_path), crate::light::stamp()?), stamps: read_stamps, full: full.clone(), view: key, all: all.clone(), graph: OnceLock::new() });
    Ok((full, all))
}

/// Summaries for the index files in `stamps` (key → path and stamp): from the summary file where an entry's stamp
/// still matches, else read from the index, rewriting the file if anything changed. Also returns the stamp of each
/// summary returned.
fn refresh(stamps: HashMap<String, (PathBuf, String)>) -> Result<(Vec<RepoSummary>, HashMap<String, String>)> {
    let _metadata = crate::writer::workspace(true)?;
    let cache_path = cache_dir()?.join("workspace.json");
    let file: Option<SummaryFile> = std::fs::read(&cache_path).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let mut cached: HashMap<String, RepoSummary> = file.filter(|f| f.format == format()).map(|f| f.repos).unwrap_or_default();
    let mut fresh: HashMap<String, RepoSummary> = HashMap::new();
    let mut stale = Vec::new();
    let mut restamped = false;
    let read_stamps = stamps.iter().map(|(key, (_, stamp))| (key.clone(), stamp.clone())).collect();
    for (key, (path, stamp)) in stamps {
        match cached.remove(&key) {
            // A query has created the replacement database but its first sync is still running. Keep the last
            // summary (explicitly stale) until the new database has committed its first complete set of facts.
            Some(s) if s.stamp.is_empty() && summary_generation(&path).is_none() => { fresh.insert(key, s); },
            Some(s) if s.stamp == stamp => {
                fresh.insert(key, s);
            }
            // Written to since, but still the summary of the index as it is while the generation it was read at
            // holds: an open and one read of `meta` instead of reading every table again.
            Some(s) if !s.generation.is_empty() && summary_generation(&path).is_some_and(|g| g == s.generation) => {
                let indexed_at = std::fs::metadata(&path).map(|m| git::mtime_ns(&m)).unwrap_or(s.indexed_at);
                fresh.insert(key, RepoSummary { stamp, indexed_at, ..s });
                restamped = true;
            }
            _ => stale.push((key, path, stamp)),
        }
    }
    let changed = !stale.is_empty() || restamped || cached.values().any(|s| !s.stamp.is_empty() || !s.root.join(".git").exists());
    // Size eviction discards the heavy index, not knowledge that this checkout exists. Retain its last summary
    // for cross-repo joins; a missing database is always changed, so it cannot rule a repo out as fresh.
    for (key, mut s) in cached {
        if s.root.join(".git").exists() { s.stamp.clear(); fresh.insert(key, s); }
    }
    // After indexing many repos, every one is stale; each read waits mostly on a `git` process, so read a few at once.
    let chunk = stale.len().div_ceil(8).max(1);
    let read_all = std::thread::scope(|scope| {
        let parts: Vec<_> = stale
            .chunks(chunk)
            .map(|part| scope.spawn(move || part.iter().map(|(_, path, stamp)| read(path, stamp.clone())).collect::<Vec<_>>()))
            .collect();
        parts.into_iter().flat_map(|h| h.join().unwrap()).collect::<Vec<_>>()
    });
    for ((key, _, _), s) in stale.into_iter().zip(read_all) {
        if let Some(s) = s? {
            fresh.insert(key, s);
        }
    }
    if changed {
        // Written whole and renamed into place, so concurrent readers see the old or the new file.
        let file = SummaryFile { format: format(), repos: fresh };
        let _ = write_summary_file(&cache_path, &file);
        fresh = file.repos;
    }
    let mut out: Vec<RepoSummary> = fresh.into_values().collect();
    out.sort_by(|a, b| a.root.cmp(&b.root));
    Ok((out, read_stamps))
}

/// Removes the summary of the index at `db` from the summary file, so nothing read from an index before it was
/// purged (see `crate::purge_blocked`) stays on disk until the next refresh.
pub fn forget_summary(db: &Path) -> Result<()> {
    let _metadata = crate::writer::workspace(true)?;
    let cache_path = cache_dir()?.join("workspace.json");
    let Some(mut file) = std::fs::read(&cache_path).ok().and_then(|b| serde_json::from_slice::<SummaryFile>(&b).ok()) else { return Ok(()) };
    let key = db.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if file.repos.remove(&key).is_none() {
        return Ok(());
    }
    write_summary_file(&cache_path, &file)
}

/// Drop only a recognized summary file whose indexes no longer exist. Never remove an unrelated cache file.
pub(crate) fn clear_orphan_summary() -> Result<()> {
    let cache = cache_dir()?;
    let path = cache.join("workspace.json");
    if crate::cache::durable_file(&path)? { return Ok(()); }
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) { return Ok(()); }
    let _metadata = crate::writer::workspace(true)?;
    let Some(mut file) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<SummaryFile>(&b).ok()) else { return Ok(()) };
    file.repos.retain(|name, _| cache.join("repos").join(name).exists());
    if file.repos.is_empty() { std::fs::remove_file(path)?; }
    else { write_summary_file(&path, &file)?; }
    Ok(())
}

pub(crate) fn prune_missing_summaries() -> Result<()> {
    let cache = cache_dir()?;
    let path = cache.join("workspace.json");
    if crate::cache::durable_file(&path)? { return Ok(()); }
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) { return Ok(()); }
    let _metadata = crate::writer::workspace(true)?;
    let Some(mut file) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<SummaryFile>(&b).ok()) else { return Ok(()) };
    let before = file.repos.len();
    file.repos.retain(|name, s| cache.join("repos").join(name).exists() || s.root.join(".git").exists());
    if before == file.repos.len() { return Ok(()); }
    if file.repos.is_empty() { std::fs::remove_file(path)?; }
    else { write_summary_file(&path, &file)?; }
    Ok(())
}

fn write_summary_file(path: &Path, file: &SummaryFile) -> Result<()> {
    if crate::cache::durable_file(path)? { anyhow::bail!("workspace summary path overlaps durable notes: {}", path.display()); }
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(file)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Called with exclusive writer and lifetime leases: preserve joins before evicting the heavy database.
pub(crate) fn preserve_summary(db: &Path) -> Result<()> {
    let _metadata = crate::writer::workspace(true)?;
    let store = Store::open(db)?;
    let Some(mut summary) = read_store(db, String::new(), store)? else { anyhow::bail!("cannot preserve index identity before eviction"); };
    summary.stamp.clear();
    let path = cache_dir()?.join("workspace.json");
    let mut file = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<SummaryFile>(&b).ok())
        .filter(|f| f.format == format()).unwrap_or_else(|| SummaryFile { format: format(), repos: HashMap::new() });
    let key = db.file_name().ok_or_else(|| anyhow::anyhow!("index has no file name"))?.to_string_lossy().into_owned();
    file.repos.insert(key, summary);
    write_summary_file(&path, &file)
}

/// The index file's stamp, plus its WAL's while the WAL holds writes. Every open creates an empty WAL and the last
/// close removes it, so an empty or missing WAL is no change; a checkpoint moves writes into the index file, whose
/// own stamp then changes.
fn index_stamp(db: &Path) -> String {
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    match std::fs::metadata(&wal) {
        Ok(m) if m.len() > 0 => format!("{}|{}", git::stamp(db), git::stamp(&wal)),
        _ => git::stamp(db),
    }
}

/// The summary of the index at `db` read afresh from its tables, whatever the summary cache holds: for checking that
/// a kept summary is still the one the index gives.
pub fn read_summary(db: &Path) -> Result<Option<RepoSummary>> {
    read(db, index_stamp(db))
}

/// What a repo's summary depends on, besides the cache's format: the index's summary generation (moved by every sync
/// that changes a fact the summary reads, see [`crate::SUMMARY_GEN`]), its root, and the stamp of the git config
/// naming its origin. `None` when the index can't be read or has no summary generation yet.
fn summary_generation(db: &Path) -> Option<String> {
    let store = crate::open_index(db, false).ok()?;
    store.begin_snapshot().ok()?;
    generation_of(&store)
}

fn generation_of(store: &Store) -> Option<String> {
    let summary = store.meta(crate::SUMMARY_GEN).ok()??;
    let root = store.meta("root").ok()??;
    let config = git::GitDir::find(Path::new(&root)).map(|g| git::stamp(&g.common.join("config"))).unwrap_or_default();
    let privacy = store.meta(crate::privacy::META_KEY).ok().flatten().unwrap_or_default();
    Some(format!("{summary}|{root}|{config}|{privacy}"))
}

fn read(db: &Path, stamp: String) -> Result<Option<RepoSummary>> {
    let Ok(store) = crate::open_index(db, false) else { return Ok(None) };
    read_store(db, stamp, store)
}

fn read_store(db: &Path, stamp: String, store: Store) -> Result<Option<RepoSummary>> {
    // One state of the index for every table the summary reads, whatever a sync elsewhere commits meanwhile.
    store.begin_snapshot()?;
    let generation = generation_of(&store).unwrap_or_default();
    let Some(root) = store.meta("root")?.filter(|r| Path::new(r).is_absolute()) else { return Ok(None) };
    let privacy_key = store.meta(crate::privacy::META_KEY)?.unwrap_or_default();
    let blocked: crate::privacy::Blocked = store.meta(crate::privacy::META_BLOCKED)?.and_then(|b| serde_json::from_str(&b).ok()).unwrap_or_default();
    let packages = manifests::packages(store.conn())?;
    let modules = query::provided_modules(store.conn())?;
    let routes = query::http_routes(store.conn())?;
    let hosts = query::host_facts(store.conn())?;
    let selectors = query::ng_selectors(store.conn())?;
    let root = PathBuf::from(root);
    let origin = git::run(&root, &["config", "--get", "remote.origin.url"])
        .ok()
        .and_then(|o| normalize_remote(String::from_utf8_lossy(&o).trim()));
    let indexed_at = std::fs::metadata(db).map(|m| git::mtime_ns(&m)).unwrap_or(0);
    let bus = kc_store::bus::rows(store.conn())?;
    let sql = kc_store::sql::defs(store.conn())?;
    let db_names = kc_store::sql::linked_names(store.conn())?;
    let ansible = kc_store::ansible::summary(store.conn())?;
    let ci = kc_store::ci::summary(store.conn())?;
    let terraform = kc_store::terraform::summary(store.conn())?;
    // Supertypes count: a library's interface implemented here is named in a base list, which is no reference.
    let names: Vec<String> = store
        .conn()
        .prepare("SELECT name FROM refs UNION SELECT target_name FROM relations")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let names = name_filter(&names);
    let defines = name_filter(&defined_names(store.conn())?);
    let tables = name_filter(&table_names(store.conn())?);
    Ok(Some(RepoSummary {
        root, packages, modules, routes, hosts, selectors, bus, sql, db_names, ansible, ci, terraform, origin, indexed_at, names, defines, tables, stamp, generation,
        outside_roots: false,
        blocked,
        privacy_key,
        gone: false,
        light: generation_of(&store).is_none(),
    }))
}

/// Every name a symbol of the repo has, and SQL objects' names lower-cased too, since SQL names match without case.
fn defined_names(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut out: Vec<String> = conn.prepare("SELECT DISTINCT name COLLATE BINARY FROM symbols")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let sql: Vec<String> = conn
        .prepare("SELECT DISTINCT s.name FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.lang = 'sql'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    out.extend(sql.into_iter().map(|n| n.to_lowercase()));
    out.sort();
    out.dedup();
    Ok(out)
}

/// The names, lower-cased, a `tables <name>` query can match in the repo: each Entity Framework fact's name, its part
/// after the last dot, and its entity; and the SQL tables, views and routines its `.sql` files define.
fn table_names(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare("SELECT DISTINCT name, entity FROM db_facts")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))? {
        let (name, entity) = row?;
        let name = name.to_ascii_lowercase();
        if let Some((_, last)) = name.rsplit_once('.') {
            out.push(last.to_string());
        }
        out.push(name);
        out.extend(entity.map(|e| e.to_ascii_lowercase()));
    }
    let mut stmt = conn.prepare("SELECT DISTINCT s.name, f.path FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.kind IN ('table', 'view', 'procedure', 'function')")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (name, file) = row?;
        if kc_store::sql::is_sql_path(&file) {
            out.push(name.to_ascii_lowercase());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Bits per name and probes per lookup: about 1% false positives.
const FILTER_BITS_PER_NAME: usize = 10;
const FILTER_PROBES: u64 = 7;

/// The bits a name sets in a filter of `bits` bits.
fn filter_probes(name: &str, bits: usize) -> impl Iterator<Item = usize> {
    let h = blake3::hash(name.as_bytes());
    let b = h.as_bytes();
    let h1 = u64::from_le_bytes(b[..8].try_into().unwrap());
    let h2 = u64::from_le_bytes(b[8..16].try_into().unwrap()) | 1;
    (0..FILTER_PROBES).map(move |i| (h1.wrapping_add(i.wrapping_mul(h2)) % bits as u64) as usize)
}

/// A Bloom filter of `names`, as hex digits of four bits each: tested in place by [`RepoSummary::may_reference`],
/// without decoding, since a query tests a few names against a few repos.
pub(crate) fn name_filter(names: &[String]) -> String {
    let bits = (names.len() * FILTER_BITS_PER_NAME).max(64).next_multiple_of(4);
    let mut nibbles = vec![0u8; bits / 4];
    for n in names {
        for i in filter_probes(n, bits) {
            nibbles[i / 4] |= 1 << (i % 4);
        }
    }
    nibbles.into_iter().map(|n| char::from_digit(n as u32, 16).unwrap()).collect()
}

/// Whether `name` may be in `filter` (see [`name_filter`]); true for an empty filter (a summary without one).
fn may_hold(filter: &str, name: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let bits = filter.len() * 4;
    let digits = filter.as_bytes();
    filter_probes(name, bits).all(|i| (digits[i / 4] as char).to_digit(16).is_some_and(|d| d & (1 << (i % 4)) != 0))
}

/// The names a query for `query` may be stored under: whole and after the last dot, each as written and lower-cased
/// (CFML and SQL store and match names without case).
fn query_names(query: &str) -> [String; 4] {
    let whole = query.trim();
    let bare = query::split_dotted(whole).1;
    [whole.to_string(), whole.to_lowercase(), bare.to_string(), bare.to_lowercase()]
}

/// Whether a query picks a definition by place (`path#name`, `file:line`): not tested against a filter.
fn names_place(query: &str) -> bool {
    query.contains('#') || query.contains(':')
}

impl RepoSummary {
    pub(crate) fn discovery(root: PathBuf, packages: Vec<PackageRow>, modules: Vec<String>, names: &[String], defines: &[String], generation: String) -> Self {
        let privacy = crate::privacy::Privacy::load(&root);
        Self {
            root, packages, modules, names: if names.is_empty() { String::new() } else { name_filter(names) }, defines: if defines.is_empty() { String::new() } else { name_filter(defines) }, generation,
            privacy_key: privacy.key(), blocked: privacy.blocked(), light: true, ..Self::default()
        }
    }
    pub(crate) fn unknown_names(&mut self) { self.names.clear(); self.defines.clear(); }
    /// The index payload was evicted; these are the facts of its last sync, never a fresh negative result.
    pub fn cached_only(&self) -> bool { self.stamp.is_empty() }
    /// Whether a reference in the repo may use `name` (as `refs` matches it: the part after the last dot). False
    /// means none does as last indexed, so the repo needn't be opened to look; true may be a false positive, or mean
    /// the summary has no filter.
    pub fn may_reference(&self, name: &str) -> bool {
        may_hold(&self.names, query::split_dotted(name).1)
    }

    /// Whether `refs <query>` may find a reference in the repo: one using the query whole or the part after its last
    /// dot, as written or lower-cased. False means none does as last indexed.
    pub fn may_reference_query(&self, query: &str) -> bool {
        query_names(query).iter().any(|n| may_hold(&self.names, n))
    }

    /// Whether the repo may define a symbol `query` names (whole or after its last dot, as written or lower-cased).
    /// False means it defines none as last indexed.
    pub fn may_define(&self, query: &str) -> bool {
        names_place(query) || query_names(query).iter().any(|n| may_hold(&self.defines, n))
    }

    /// Whether `tables <name>` may find something in the repo: an Entity Framework table, entity, DbSet or procedure
    /// call, or a SQL table, view or routine, of that name. False means none as last indexed.
    pub fn may_name_table(&self, name: &str) -> bool {
        if names_place(name) {
            return true;
        }
        let lower = name.to_ascii_lowercase();
        let trimmed = lower.trim();
        [lower.as_str(), trimmed, query::split_dotted(trimmed).1].iter().any(|n| may_hold(&self.tables, n))
    }
}

/// `git@host:Group/Repo.git`, `ssh://git@host/group/repo`, `https://host/group/repo/` → `host/group/repo`, lower-case.
pub fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = match url.split_once("://") {
        Some((_, r)) => r.to_string(),
        // scp-like: `user@host:path`.
        None => url.replacen(':', "/", 1),
    };
    let rest = rest.rsplit_once('@').map_or(rest.as_str(), |(_, h)| h);
    let norm = rest.trim_end_matches('/').trim_end_matches(".git").to_ascii_lowercase();
    (!norm.is_empty()).then_some(norm)
}

impl RepoSummary {
    /// The summary generation it was read from: what an answer drawing on the summary names in its provenance. It
    /// moves only when something the summary reads changes (see [`summary_generation`]).
    pub fn generation(&self) -> &str {
        self.generation.split('|').next().unwrap_or("")
    }
}

/// The checkout policy, the one rule every cross-repo answer uses to pick which checkout answers for a repo identity
/// (see [`RepoSummary::identity`]): checkouts of one origin are one repo. Among them, a queried repo's own checkout
/// (`home`) first, then one that still exists, then one under the workspace roots (a repo outside them is in the
/// workspace only as a query's focus), then the most recently synced; ties by root, so the choice is stable.
pub fn choose<'a>(checkouts: &[&'a RepoSummary], home: &[PathBuf]) -> Option<&'a RepoSummary> {
    if let [one] = checkouts {
        return Some(one);
    }
    checkouts
        .iter()
        .copied()
        .max_by_key(|s| (home.contains(&s.root), s.root.join(".git").exists(), !s.outside_roots, s.indexed_at, s.root.clone()))
}

/// The checkouts of `summaries` grouped by repo identity, by position, in order of first appearance.
fn by_identity(summaries: &[RepoSummary]) -> Vec<(String, Vec<usize>)> {
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, s) in summaries.iter().enumerate() {
        let id = s.identity();
        match at.get(&id) {
            Some(&k) => out[k].1.push(i),
            None => {
                at.insert(id.clone(), out.len());
                out.push((id, vec![i]));
            }
        }
    }
    out
}

/// One summary per repo identity, the checkout [`choose`] picks with `home`, sorted by root: the repos a cross-repo
/// link may land in or count. A repo whose every checkout is gone (see [`RepoSummary::gone`]) is left out, unless it
/// is `home`: its index is left, but nothing it said is still there to link to, and counting it would make another
/// repo's link look ambiguous. [`gone`] lists those left out, for saying so where they would have mattered.
pub fn distinct<'a>(summaries: &'a [RepoSummary], home: &[PathBuf]) -> Vec<&'a RepoSummary> {
    picked(summaries, home).into_iter().filter(|s| !s.gone || home.contains(&s.root)).collect()
}

/// [`distinct`], gone checkouts included: for the dependency graph, whose answers name a gone dependency as missing.
fn picked<'a>(summaries: &'a [RepoSummary], home: &[PathBuf]) -> Vec<&'a RepoSummary> {
    let mut out: Vec<&RepoSummary> = by_identity(summaries)
        .into_iter()
        .filter_map(|(_, members)| choose(&members.iter().map(|&i| &summaries[i]).collect::<Vec<_>>(), home))
        .collect();
    out.sort_by(|a, b| a.root.cmp(&b.root));
    out
}

/// The repos [`distinct`] leaves out because every checkout of theirs is gone, one summary each, sorted by root.
/// Empty, and cheap, while no checkout is gone.
pub fn gone(summaries: &[RepoSummary]) -> Vec<&RepoSummary> {
    let mut out: Vec<&RepoSummary> = Vec::new();
    // Checkouts are one repo by origin (see [`RepoSummary::identity`]); one without an origin is a repo of its own.
    let same = |a: &RepoSummary, b: &RepoSummary| a.origin.is_some() && a.origin == b.origin;
    for g in summaries.iter().filter(|s| s.gone) {
        if summaries.iter().any(|s| !s.gone && same(s, g)) {
            continue;
        }
        match out.iter_mut().find(|o| same(o, g)) {
            // Of a repo's gone checkouts, the one the checkout policy picks.
            Some(o) => *o = choose(&[*o, g], &[]).unwrap_or(g),
            None => out.push(g),
        }
    }
    out
}

/// Records that the repo at `root`, left out of cross-repo links because its checkout is gone (see [`distinct`]),
/// would have mattered to the answer being worked out (the query this thread runs, see [`kc_store::limits::current`]):
/// it served a route the answer links, consumed a message, defined a table, named a module or role. The answer names
/// it as missing (see [`take_missing`]).
pub fn note_missing(root: &Path) {
    kc_store::limits::current().note_missing(root);
}

/// The repos [`note_missing`] recorded for the query this thread runs since the last call, sorted; each once.
pub fn take_missing() -> Vec<PathBuf> {
    kc_store::limits::current().take_missing()
}

/// `f`, with what it notes as missing (see [`note_missing`]) dropped: for work reading every repo's facts, most of
/// which the answer may not need.
pub fn quietly<T>(f: impl FnOnce() -> T) -> T {
    kc_store::limits::current().quietly(f)
}

/// Records each gone repo of `gone` that `matters` accepts (see [`note_missing`]).
pub fn note_missing_if<'a>(gone: impl IntoIterator<Item = &'a RepoSummary>, matters: impl Fn(&RepoSummary) -> bool) {
    for g in gone {
        if matters(g) {
            note_missing(&g.root);
        }
    }
}

/// The repo identity of the checkout at `root`: its summary's, else the root itself.
pub fn identity_of(root: &Path, summaries: &[RepoSummary]) -> String {
    summaries.iter().find(|s| s.root == root).map_or_else(|| root.to_string_lossy().into_owned(), |s| s.identity())
}

/// Package owners among `summaries`, by identity, each identity answered by the checkout [`choose`] picks with
/// `home`. `packages` gives each summary's manifests (a queried repo's may be fresher than its summary).
pub fn package_owners<'a>(
    summaries: &'a [RepoSummary],
    home: &[PathBuf],
    packages: impl Fn(&'a RepoSummary) -> &'a [PackageRow],
) -> PackageOwners {
    let mut owners = PackageOwners::default();
    let groups = by_identity(summaries);
    // A repo whose every checkout is gone owns a package only while no repo that is there declares it too: as the
    // sole owner, an answer resolving into it names it missing; beside one that is there, it would only make the
    // package's owner ambiguous.
    let gone = |members: &[usize]| members.iter().all(|&i| summaries[i].gone && !home.contains(&summaries[i].root));
    let live: std::collections::HashSet<&str> = if summaries.iter().any(|s| s.gone) {
        groups.iter().filter(|(_, m)| !gone(m)).flat_map(|(_, m)| m.iter().flat_map(|&i| packages(&summaries[i]).iter().map(|p| p.name.as_str()))).collect()
    } else {
        Default::default()
    };
    for (id, members) in groups.iter() {
        let checkouts: Vec<&RepoSummary> = members.iter().map(|&i| &summaries[i]).collect();
        let shadowed = gone(members);
        for s in &checkouts {
            for p in packages(s) {
                if !(shadowed && live.contains(p.name.as_str())) {
                    owners.add(&p.name, id);
                }
            }
        }
        if let Some(chosen) = choose(&checkouts, home) {
            let others = checkouts.iter().filter(|s| s.root != chosen.root).map(|s| s.root.to_string_lossy().into_owned()).collect();
            owners.set_checkout(id, &chosen.root.to_string_lossy(), others);
        }
    }
    owners
}

/// Package owners and repo-to-repo edges among a set of summaries, worked out once per set: every cross-repo
/// question needs them, and working them out from every manifest took longer than the rest of a query. Edges are
/// between repo identities, so indexing another checkout of a repo never removes one; each lands in the checkout
/// [`choose`] picks (without a home: a repo's own identity is never its dependency).
pub struct Graph {
    owners: PackageOwners,
    /// Each summary's identity, by position.
    ids: Vec<String>,
    /// Whether each summary is the checkout answering for its identity.
    chosen: Vec<bool>,
    /// For each summary, by position, the identities its manifests depend on.
    to_ids: Vec<Vec<String>>,
    /// For each summary, by position, the roots of the checkouts answering for them.
    to: Vec<Vec<String>>,
}

impl Graph {
    fn new(summaries: &[RepoSummary]) -> Graph {
        let owners = package_owners(summaries, &[], |s| &s.packages);
        let ids: Vec<String> = summaries.iter().map(RepoSummary::identity).collect();
        // Gone checkouts stay in the graph: an answer opening a gone dependency names it as missing.
        let chosen_roots: std::collections::HashSet<&Path> = picked(summaries, &[]).into_iter().map(|s| s.root.as_path()).collect();
        let chosen = summaries.iter().map(|s| chosen_roots.contains(s.root.as_path())).collect();
        let at_root: HashMap<&Path, &str> = summaries.iter().zip(&ids).map(|(s, id)| (s.root.as_path(), id.as_str())).collect();
        let mut to = Vec::with_capacity(summaries.len());
        let mut to_ids = Vec::with_capacity(summaries.len());
        for (s, id) in summaries.iter().zip(&ids) {
            let mut roots: Vec<String> = Vec::new();
            let mut idents: Vec<String> = Vec::new();
            for e in manifests::repo_edges(&s.packages, &s.root.to_string_lossy(), id, &owners) {
                // A path dependency into another checkout of a repo means that repo, answered by its chosen checkout.
                let target = at_root.get(Path::new(&e.to_repo)).map_or_else(|| e.to_repo.clone(), |t| t.to_string());
                if target == *id {
                    continue;
                }
                let root = owners.checkout(&target).to_string();
                let root = if at_root.contains_key(Path::new(&root)) { root } else { e.to_repo };
                if !idents.contains(&target) {
                    idents.push(target);
                }
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
            to.push(roots);
            to_ids.push(idents);
        }
        Graph { owners, ids, chosen, to_ids, to }
    }

    /// Which repo declares each package.
    pub fn owners(&self) -> &PackageOwners {
        &self.owners
    }

    /// The identity of the summary at `index`.
    pub fn identity(&self, index: usize) -> Option<&str> {
        self.ids.get(index).map(String::as_str)
    }

    /// Whether the summary at `index` depends on the repo `identity` through its manifests.
    pub fn depends_on(&self, index: usize, identity: &str) -> bool {
        self.to_ids.get(index).is_some_and(|to| to.iter().any(|t| t == identity))
    }

    /// The roots the summary at `index` depends on through its manifests: for each repo, its chosen checkout.
    pub fn dependencies(&self, index: usize) -> &[String] {
        self.to.get(index).map_or(&[], Vec::as_slice)
    }

    /// The summaries (by position) of the repos depending on the repo `identity`, one checkout per repo.
    pub fn dependents(&self, identity: &str) -> Vec<usize> {
        (0..self.ids.len()).filter(|&i| self.chosen[i] && self.ids[i] != identity && self.depends_on(i, identity)).collect()
    }
}

/// [`Graph`] of `summaries`: shared while they are the summaries [`summaries`] last returned, else worked out anew.
pub fn graph(summaries: &[RepoSummary]) -> Arc<Graph> {
    let memo = MEMO.lock().unwrap_or_else(|e| e.into_inner());
    match memo.as_ref().filter(|m| std::ptr::eq(m.all.as_slice(), summaries)) {
        Some(m) => m.graph.get_or_init(|| Arc::new(Graph::new(summaries))).clone(),
        None => Arc::new(Graph::new(summaries)),
    }
}

/// The repos `root` depends on through its manifests, and the repos depending on it, one checkout per repo (see
/// [`choose`]).
pub fn neighbours<'a>(root: &Path, summaries: &'a [RepoSummary]) -> (Vec<&'a RepoSummary>, Vec<&'a RepoSummary>) {
    let graph = graph(summaries);
    let me = identity_of(root, summaries);
    let deps: &[String] = summaries.iter().position(|s| s.root == root).map_or(&[], |i| graph.dependencies(i));
    let dependencies = summaries.iter().filter(|s| deps.iter().any(|d| Path::new(d) == s.root)).collect();
    let dependents = graph.dependents(&me).into_iter().map(|i| &summaries[i]).collect();
    (dependencies, dependents)
}

impl RepoSummary {
    /// A summary of a repo with no summary yet (indexed by this very query), knowing only its manifests: its identity
    /// is its root.
    pub fn stand_in(root: &Path, packages: Vec<PackageRow>) -> RepoSummary {
        RepoSummary {
            root: root.to_path_buf(),
            packages,
            modules: Vec::new(),
            routes: Vec::new(),
            hosts: Vec::new(),
            origin: None,
            selectors: Vec::new(),
            bus: Vec::new(),
            sql: Vec::new(),
            db_names: Vec::new(),
            ansible: Default::default(),
            ci: Default::default(),
            terraform: Default::default(),
            indexed_at: i64::MAX,
            names: String::new(),
            defines: String::new(),
            tables: String::new(),
            stamp: String::new(),
            generation: String::new(),
            outside_roots: false,
            blocked: Default::default(),
            privacy_key: String::new(),
            gone: false,
            light: false,
        }
    }

    /// What identifies the repo across checkouts: its origin, else its root.
    pub fn identity(&self) -> String {
        self.origin.clone().unwrap_or_else(|| self.root.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_rule_out_names_a_repo_neither_uses_defines_nor_maps() {
        let mut s: RepoSummary = serde_json::from_value(serde_json::json!({ "root": "/r", "packages": [], "modules": [], "stamp": "" })).unwrap();
        // No filters (an older summary): nothing is ruled out.
        assert!(s.may_reference_query("Anything") && s.may_define("Anything") && s.may_name_table("anything"));
        s.names = name_filter(&["Place".into(), "deploy.yml".into(), "orders".into()]);
        s.defines = name_filter(&["OrderService".into(), "Place".into()]);
        s.tables = name_filter(&["orders".into(), "shop.orders".into(), "order".into()]);
        // References: whole or after the last dot, as written or lower-cased (CFML and SQL store them lower-case).
        assert!(s.may_reference_query("Place") && s.may_reference_query("OrderService.Place") && s.may_reference_query(" deploy.yml "));
        assert!(s.may_reference_query("Orders"));
        assert!(!s.may_reference_query("Cancel") && !s.may_reference_query("OrderService.Cancel"));
        assert!(s.may_define("OrderService") && s.may_define("OrderService.Place") && !s.may_define("Cancel"));
        // A place (`path#name`, `file:line`) isn't tested.
        assert!(s.may_define("src/a.cs#Cancel") && s.may_define("src/a.cs:12"));
        assert!(s.may_name_table("Orders") && s.may_name_table("Shop.Orders") && s.may_name_table("dbo.orders") && s.may_name_table("ORDER"));
        assert!(!s.may_name_table("customers") && !s.may_name_table("shop.customers"));
    }

    #[test]
    fn index_stamp_ignores_read_only_opens_but_not_writes() {
        let dir = std::env::temp_dir().join(format!("knowcode-test-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("repo.sqlite");
        Store::open(&db).unwrap().set_meta("root", "/r").unwrap();
        let before = index_stamp(&db);
        // Opening and reading creates and removes an empty WAL: not a change.
        let store = Store::open(&db).unwrap();
        assert_eq!(store.meta("root").unwrap().as_deref(), Some("/r"));
        assert_eq!(index_stamp(&db), before, "while open");
        drop(store);
        assert_eq!(index_stamp(&db), before, "after close");
        // A write is a change, whether it still sits in the WAL or has been checkpointed.
        let store = Store::open(&db).unwrap();
        store.set_meta("root", "/s").unwrap();
        assert_ne!(index_stamp(&db), before, "write in WAL");
        drop(store);
        assert_ne!(index_stamp(&db), before, "after checkpoint");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
