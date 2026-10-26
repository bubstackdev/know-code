//! Discovery, freshness and incremental indexing.
//!
//! The working tree is the source of truth. [`sync`] lists the repo's files with git, stats them, and re-extracts
//! only files whose mtime or size changed and whose content hash differs (or whose extractor version is out of
//! date). Every query runs it first, so answers match the files on disk, or say which files they could not read
//! ([`SyncReport::failed`]). An edit that keeps a file's mtime and size is invisible to the stat: [`SyncOptions::verify`]
//! hashes every file, and showing a symbol's source compares its hash (see [`reindex`]).
//!
//! The check is cheap in the steady state: when the git index, the ignore files (KnowCode's, and git's: every
//! `.gitignore`, `info/exclude`, the global excludes file) and every directory listing seen at the last full discovery
//! are unchanged, the file list is reused and only file stats are compared; a YAML file whose content decides its
//! language is read again when its stat moves. Otherwise it falls back to `git ls-files`. Symbolic links are never
//! followed.

pub mod angular;
pub mod areas;
pub mod ansible;
pub mod ci;
pub mod ci_tools;
pub mod bus;
pub mod config;
pub mod cache;
pub mod content;
pub mod diff;
mod fresh;
mod git;
pub mod grams;
pub mod guess;
pub mod health;
pub mod light;
pub mod line_history;
pub mod machine;
pub mod meter;
pub mod http;
pub mod notes;
pub mod revision;
pub mod ownership;
pub mod privacy;
pub mod roots;
pub mod scip;
pub mod sdk;
pub mod search;
pub mod sql;
pub mod test_select;
pub mod textdocs;
pub mod structural;
pub mod terraform;
pub mod vitals;
pub mod workspace;
pub mod writer;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use kc_extract::{FileFacts, Lang};
use kc_store::{FileRecord, Store};
use serde::Serialize;

/// Files larger than this are left out of the content (search) index (generated code, fixtures).
pub const MAX_FILE_BYTES: u64 = 1 << 20;

/// ColdFusion files larger than this are not extracted. CFML pages are hand-written and grow past 1 MB (a page that
/// size can run over a thousand queries); in other languages a file over [`MAX_FILE_BYTES`] is a dump or generated
/// code, which can cost several times the memory of the rest of its repo to extract.
pub const MAX_CFML_BYTES: u64 = 8 << 20;

/// SQL files larger than this are not extracted. Schema dumps of large databases pass 1 MB (several MB is common)
/// and are the only definition of their tables. The scanner reads them linearly, so a repo holding several costs
/// about a third more instructions and memory to index cold than without them.
pub const MAX_SQL_BYTES: u64 = 8 << 20;

/// The largest source file of `lang` that is extracted; larger ones are listed by [`skipped_large`].
pub fn max_source_bytes(lang: Lang) -> u64 {
    match lang {
        Lang::Cfml => MAX_CFML_BYTES,
        Lang::Sql => MAX_SQL_BYTES,
        _ => MAX_FILE_BYTES,
    }
}

/// Source files left out for size at the last full discovery: (path, bytes).
pub fn skipped_large(store: &Store) -> Result<Vec<(String, u64)>> {
    Ok(store.meta("disc.large")?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
}

/// Source files left out at the last full discovery for being symbolic links (see [`is_symlink`]).
pub fn skipped_symlinks(store: &Store) -> Result<Vec<String>> {
    Ok(store.meta("disc.symlinks")?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
}

/// Files whose read failed at the last sync: their facts are as last indexed (see [`SyncReport::failed`]).
pub fn failed_reads(store: &Store) -> Result<Vec<String>> {
    Ok(store.meta("sync.failed")?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
}

/// Path components that are never indexed: build output, dependencies, virtualenvs, vendored trees.
const SKIP_COMPONENTS: &[&str] =
    &["node_modules", "Pods", "Carthage", "DerivedData", "vendor", "third_party", ".venv", "venv", ".git"];

/// The cache root: `~/Library/Caches/dev.bubstack.knowcode` on macOS. `KNOWCODE_CACHE_DIR` overrides it, then
/// `cache.dir` in `config.toml`, then the legacy `cache_dir` file in [`config_dir`] (see [`config::cache_dir`]), so a
/// plain `knowcode` uses another cache without the variable on every call.
pub fn cache_dir() -> Result<PathBuf> {
    Ok(config::cache_dir()?.0)
}

pub use git::repo_root;

/// A full rediscovery is forced at least this often, as a backstop for changes the cheap check can't see (a file
/// added to a directory that held no listed files).
const DISCOVERY_TTL_SECS: u64 = 600;

/// The `meta` key of the summary generation: moved by a sync that changes what the workspace summary reads (see
/// [`kc_store::summary_key`]), in the same commit, so a summary read at one is valid while it holds.
pub const SUMMARY_GEN: &str = "summary_gen";

/// Cached `dirty` is recomputed with `git status` at least this often.
const DIRTY_TTL_SECS: u64 = 60;

/// The index file for a repo root: `<cache>/repos/<name>-<hash>.sqlite`.
pub fn db_path(root: &Path) -> Result<PathBuf> {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    let hash = blake3::hash(root.to_string_lossy().as_bytes()).to_hex();
    Ok(cache_dir()?.join("repos").join(format!("{name}-{}.sqlite", &hash[..12])))
}

/// Opens (creating if needed) the index for a repo root and records the root in it.
pub fn open(root: &Path) -> Result<Store> {
    open_with(root, None).map(|(store, _)| store)
}

/// Opens an index with an eviction lease. Summary reads do not recreate an index removed since discovery.
pub(crate) fn open_index(db: &Path, create: bool) -> Result<Store> {
    let lease = writer::reader(db)?;
    if !create && !db.exists() { bail!("index no longer exists: {}", db.display()); }
    Store::open_leased(db, lease)
}

/// Checkouts a new index can be seeded from (see [`open`]): origin, root and when each was indexed.
///
/// [`open`] reads them from the workspace summary, which it refreshes, each time it creates an index. Indexing many
/// repos in one run loads them once and adds each repo as it is indexed instead: refreshing the summary for every
/// new repo rewrote it whole each time, which took most of the run's time and memory over a few hundred repos.
#[derive(Default)]
pub struct Seeds {
    /// Loaded when the first new index is created.
    checkouts: Option<Vec<(String, PathBuf, i64)>>,
}

impl Seeds {
    fn load() -> Result<Vec<(String, PathBuf, i64)>> {
        Ok(workspace::all_summaries()?.iter().filter_map(|s| Some((s.origin.clone()?, s.root.clone(), s.indexed_at))).collect())
    }

    /// [`open`], seeding a new index from these checkouts. A new index's repo becomes a candidate for the repos opened
    /// after it.
    pub fn open(&mut self, root: &Path) -> Result<Store> {
        if self.checkouts.is_none() && !db_path(root)?.exists() {
            self.checkouts = Some(Seeds::load().unwrap_or_default());
        }
        let (store, origin) = open_with(root, self.checkouts.as_deref())?;
        if let (Some(origin), Some(checkouts)) = (origin, self.checkouts.as_mut()) {
            checkouts.retain(|(_, r, _)| r != root);
            checkouts.push((origin, root.to_path_buf(), now_nanos() as i64));
        }
        Ok(store)
    }
}

/// [`open`], seeding from `checkouts` (or the workspace summary). Returns the repo's normalized origin when it was
/// looked up, which is only for a new index.
fn open_with(root: &Path, checkouts: Option<&[(String, PathBuf, i64)]>) -> Result<(Store, Option<String>)> {
    // A cancelled operation (see `kc_store::limits`) opens no further repo: a workspace query stops here.
    kc_store::limits::check()?;
    let db = db_path(root)?;
    // Schema creation, migration and seeding are writers too. Opening a current index remains independent of
    // the indexing lease, so readers can use their committed WAL snapshot while a sync is running.
    let mut writer = if Store::needs_migration(&db)? { Some(writer::WriterLock::acquire(root)?) } else { None };
    // The repo's AI privacy policy, before anything is read from its index: an index made under another policy may
    // hold what is blocked now, and is removed whole (with the side caches and notes in blocked files), so nothing
    // blocked survives in a table, a full-text index, a free page or the WAL.
    let privacy = privacy::Privacy::load(root);
    let key = privacy.key();
    if !db.exists() && privacy.blocks_anything() {
        // An evicted payload can still have a cached summary. Never reuse its old facts while opening under a
        // blocking policy; the replacement index records the policy before summaries can expose anything.
        workspace::forget_summary(&db)?;
        notes::purge(root, |file| privacy.blocks(file))?;
    }
    if db.exists() && !key.is_empty() {
        let recorded = open_index(&db, false).ok().and_then(|s| s.meta(privacy::META_KEY).ok().flatten()).unwrap_or_default();
        if recorded != key {
            if writer.is_none() {
                writer = Some(writer::WriterLock::acquire(root)?);
            }
            purge_blocked(root, &privacy)?;
        }
    }
    let mut origin = None;
    if !db.exists() && !privacy.blocks_anything() {
        // Best effort: without a seed, the first sync indexes from scratch. A repo with anything blocked is never
        // seeded: another checkout's index may hold what this one's policy blocks.
        origin = seed(root, &db, checkouts).ok().flatten();
    }
    let store = open_index(&db, true)?;
    let root_str = root.to_string_lossy();
    if store.meta("root")?.as_deref() != Some(root_str.as_ref()) {
        store.set_meta("root", &root_str)?;
    }
    if store.meta(privacy::META_KEY)?.unwrap_or_default() != key {
        store.set_meta(privacy::META_KEY, &key)?;
        store.set_meta(privacy::META_BLOCKED, &serde_json::to_string(&privacy.blocked())?)?;
    }
    // A query's reads see one state of the index (see `Store::begin_snapshot`); a sync releases it while it writes
    // and takes it again after, at the state it committed.
    store.begin_snapshot()?;
    cache::touch(root, &db);
    drop(writer);
    Ok((store, origin))
}

/// Starts a new checkout's index from a copy of another checkout's of the same repo (same `origin`: a worktree, a
/// second clone), the most recently indexed. The first sync then re-reads every file but extracts only those whose
/// content differs, and keeps the content index of the rest. Manifests are extracted again, since path dependencies
/// resolve against the checkout's location. Candidates come from `checkouts`, or else the workspace summary. Returns
/// the repo's normalized origin, if it has one.
fn seed(root: &Path, db: &Path, checkouts: Option<&[(String, PathBuf, i64)]>) -> Result<Option<String>> {
    let out = git::run(root, &["config", "--get", "remote.origin.url"])?;
    let Some(origin) = workspace::normalize_remote(String::from_utf8_lossy(&out).trim()) else { return Ok(None) };
    let loaded;
    let checkouts = match checkouts {
        Some(c) => c,
        None => {
            loaded = Seeds::load()?;
            &loaded
        }
    };
    // The checkout a worktree was made from shares its git directory and usually its files: the best seed. Otherwise
    // the most recently indexed checkout of the repo.
    let common = |dir: &Path| git::run(dir, &["rev-parse", "--git-common-dir"]).ok().map(|o| dir.join(String::from_utf8_lossy(&o).trim())).and_then(|p| std::fs::canonicalize(p).ok());
    // Never from a checkout whose own policy blocks anything: its index may not have been purged yet.
    let candidates: Vec<_> = checkouts
        .iter()
        .filter(|(o, r, _)| *o == origin && r != root)
        .filter(|(_, r, _)| db_path(r).is_ok_and(|p| p.exists()))
        .filter(|(_, r, _)| !privacy::Privacy::load(r).blocks_anything())
        .collect();
    // Only look up git directories when there is a choice to make.
    let mine = if candidates.is_empty() { None } else { common(root) };
    let Some((_, src, _)) = candidates.into_iter().max_by_key(|(_, r, at)| (mine.is_some() && common(r) == mine, *at)) else {
        return Ok(Some(origin));
    };
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // `VACUUM INTO` writes a consistent copy, even while another process uses the source.
    let source = open_index(&db_path(src)?, false)?;
    source.conn().execute("VACUUM INTO ?1", [db.to_string_lossy()])?;
    drop(source);
    let copy = open_index(db, true)?;
    copy.set_meta("root", &root.to_string_lossy())?;
    let manifests: Vec<String> = Lang::ALL.iter().filter(|l| l.is_manifest()).map(|l| format!("'{}'", l.as_str())).collect();
    copy.conn().execute_batch(&format!(
        "DELETE FROM meta WHERE key LIKE 'head.%' OR key LIKE 'disc.%' OR key = 'last_sync';
         DELETE FROM derived;
         UPDATE files SET extractor_version = 0 WHERE lang IN ({});",
        manifests.join(", ")
    ))?;
    Ok(Some(origin))
}

/// Removes the index of the checkout at `root`. Returns whether there was one.
pub fn forget(root: &Path) -> Result<bool> {
    let _writer = writer::WriterLock::acquire(root)?;
    let db = db_path(root)?;
    let _idle = writer::idle(&db)?.ok_or_else(|| anyhow::anyhow!("index is in use: {}; close its readers and retry", root.display()))?;
    let existed = db.exists();
    cache::remove_files(&db)?;
    drop(_idle); // never wait for the summary lock while holding an exclusive lifetime lease
    workspace::forget_summary(&db)?;
    light::forget(root)?;
    Ok(existed)
}

/// Removes what the cache holds from the blocked part of the repo at `root` (see [`privacy`]): its index whole (the
/// next sync rebuilds it without the blocked files; a repo blocked whole keeps an empty index saying so), with its
/// snapshot and status cache, its entry in the workspace summary file, and the notes on symbols in blocked files
/// (every note, for a repo blocked whole).
pub fn purge_blocked(root: &Path, privacy: &privacy::Privacy) -> Result<()> {
    forget(root)?;
    workspace::forget_summary(&db_path(root)?)?;
    notes::purge(root, |file| privacy.blocks(file))?;
    Ok(())
}

/// Removes the indexes of checkouts that no longer exist (a deleted worktree). Returns their roots.
pub fn prune() -> Result<Vec<PathBuf>> {
    Ok(cache::prune()?.removed.into_iter().map(|r| r.root).collect())
}

/// Roots of every indexed repo in the workspace: with workspace roots configured, the indexed repos under them;
/// without, every repo with an index in the cache. An index whose checkout is gone (until [`prune`]) is no repo of
/// the workspace: a workspace-wide answer has nothing there to read.
pub fn indexed_roots() -> Result<Vec<PathBuf>> {
    Ok(workspace::summaries(&[])?.iter().filter(|s| !s.blocked.whole && !s.gone).map(|s| s.root.clone()).collect())
}

/// Repos found under the workspace roots whose full index isn't built yet: no index (only discovered, or scanned for
/// names and manifests), or one never synced to the end. Neither those blocked whole by their AI privacy file (never
/// indexed) nor gone checkouts. Smallest first, by the size of their git index (a cheap measure of how many files
/// they track), so the most repos become answerable soonest.
pub fn unindexed_roots() -> Result<Vec<PathBuf>> {
    let mut roots: Vec<(u64, PathBuf)> = workspace::summaries(&[])?
        .iter()
        .filter(|s| s.light && !s.blocked.whole && !s.gone)
        .map(|s| (git_index_bytes(&s.root), s.root.clone()))
        .collect();
    roots.sort();
    Ok(roots.into_iter().map(|(_, r)| r).collect())
}

/// The size of a checkout's git index (`.git/index`, or a worktree's own), 0 when it can't be read.
fn git_index_bytes(root: &Path) -> u64 {
    git::GitDir::find(root).and_then(|g| std::fs::metadata(g.dir.join("index")).ok()).map_or(0, |m| m.len())
}

/// The repos in the workspace whose AI privacy policy blocks anything, with what it blocks: whole repos (left out of
/// [`indexed_roots`]) and repos with blocked paths.
pub fn blocked_roots() -> Result<Vec<(PathBuf, privacy::Blocked)>> {
    Ok(workspace::summaries(&[])?.iter().filter(|s| s.blocked.any()).map(|s| (s.root.clone(), s.blocked.clone())).collect())
}

/// Roots of every repo with an index in the cache, in the workspace or not.
pub fn all_indexed_roots() -> Result<Vec<PathBuf>> {
    Ok(workspace::all_summaries()?.iter().map(|s| s.root.clone()).collect())
}

/// Which of `roots` declares each package, by repo identity, from the workspace summary (see
/// [`workspace::package_owners`]).
pub fn package_owners(roots: &[PathBuf]) -> Result<kc_store::manifests::PackageOwners> {
    let summaries: Vec<workspace::RepoSummary> = workspace::all_summaries()?.iter().filter(|s| roots.contains(&s.root)).cloned().collect();
    Ok(workspace::package_owners(&summaries, &[], |s| &s.packages))
}

/// Every git repo directly under `dir` (one level deep), plus `dir` itself if it is one.
pub fn discover_repos(dir: &Path) -> Result<Vec<PathBuf>> {
    let dir = &std::fs::canonicalize(dir).with_context(|| format!("resolving {}", dir.display()))?;
    let mut repos = Vec::new();
    if dir.join(".git").exists() {
        repos.push(dir.to_path_buf());
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() && !path.is_symlink() && path.join(".git").exists() {
            repos.push(path);
        }
    }
    repos.sort();
    Ok(repos)
}

#[derive(Debug, Clone, Serialize)]
pub struct Head {
    pub commit: Option<String>,
    pub dirty: bool,
}

/// HEAD commit (read from `.git`) and whether tracked files differ from it. `dirty` comes from `git status`, cached
/// until the git index, HEAD or the indexed files change, or [`DIRTY_TTL_SECS`] pass.
///
/// The cache is a small file beside the index ([`head_cache_path`]), not a row in it: writing the index would change
/// its stamp and so make the workspace summary re-read the repo (and rewrite the summary file) once a minute.
pub fn head(root: &Path, store: &Store) -> Head {
    let Some(gd) = git::GitDir::find(root) else {
        return Head { commit: None, dirty: false };
    };
    let commit = gd.head_commit();
    let key = format!(
        "{}|{}|{}",
        commit.as_deref().unwrap_or("-"),
        gd.index_stamp(),
        store.meta("tree_gen").ok().flatten().unwrap_or_default()
    );
    let now = now_secs();
    let cache = head_cache_path(root).ok();
    let cached = (|| -> Option<bool> {
        let text = std::fs::read_to_string(cache.as_ref()?).ok()?;
        let mut lines = text.lines();
        let (k, at, dirty) = (lines.next()?, lines.next()?, lines.next()?);
        (k == key && now.saturating_sub(at.parse().ok()?) < DIRTY_TTL_SECS).then_some(dirty == "1")
    })();
    if let Some(dirty) = cached {
        return Head { commit, dirty };
    }
    match git::status(root) {
        Some((status_commit, dirty)) => {
            if let Some(path) = &cache && cache::durable_file(path).is_ok_and(|durable| !durable) {
                // Written whole and renamed into place, so a concurrent reader sees the old or the new entry.
                let tmp = path.with_extension(format!("head.{}", std::process::id()));
                if std::fs::write(&tmp, format!("{key}\n{now}\n{}\n", if dirty { "1" } else { "0" })).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
            Head { commit: commit.or(status_commit), dirty }
        }
        None => Head { commit, dirty: false },
    }
}

/// HEAD's commit, read from `.git` (no `git` process).
pub fn head_commit(root: &Path) -> Option<String> {
    git::GitDir::find(root)?.head_commit()
}

/// Where [`head`] caches `git status` for a checkout: beside its index, under a name the workspace summary skips.
fn head_cache_path(root: &Path) -> Result<PathBuf> {
    Ok(db_path(root)?.with_extension("head"))
}

/// The user config directory: `~/Library/Application Support/dev.bubstack.knowcode` on macOS.
/// `KNOWCODE_CONFIG_DIR` overrides it.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("KNOWCODE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(dirs::config_dir().context("no user config directory")?.join("dev.bubstack.knowcode"))
}

/// The user data directory, for what KnowCode keeps that isn't derived from the code (notes on symbols):
/// `~/Library/Application Support/dev.bubstack.knowcode` on macOS. `KNOWCODE_DATA_DIR` overrides it.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("KNOWCODE_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(dirs::data_dir().context("no user data directory")?.join("dev.bubstack.knowcode"))
}

/// Ignore rules, from the repo's `.knowcodeignore` and from the user-level rules: `[ignore]` in `config.toml`, else
/// the legacy `ignore` file in [`config_dir`] (see [`config::user_ignore`]).
///
/// Pattern syntax, one per line, `#` comments: a pattern with a `/` matches a path prefix; otherwise it matches any
/// path component, with `*` as a wildcard. User-level rules are scoped to repos: each names a repo directory name, an
/// absolute repo path, or `*` for every repo. They let you exclude paths without writing into repos you don't own.
struct Ignore {
    prefixes: Vec<String>,
    components: Vec<String>,
}

impl Ignore {
    fn load(root: &Path) -> Result<Ignore> {
        let mut ig = Ignore { prefixes: Vec::new(), components: SKIP_COMPONENTS.iter().map(|s| s.to_string()).collect() };
        if let Ok(text) = std::fs::read_to_string(root.join(".knowcodeignore")) {
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                ig.add(line);
            }
        }
        ig.add_user_rules(root, &config::user_ignore()?.0);
        Ok(ig)
    }

    fn add(&mut self, pattern: &str) {
        if pattern.contains('/') {
            self.prefixes.push(pattern.trim_start_matches('/').to_string());
        } else {
            self.components.push(pattern.to_string());
        }
    }

    /// The user-level `(repo, pattern)` rules that apply to the repo at `root`.
    fn add_user_rules(&mut self, root: &Path, rules: &[(String, String)]) {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for (repo, pattern) in rules {
            if repo == "*" || *repo == name || Path::new(repo) == root {
                self.add(pattern);
            }
        }
    }

    fn reason(&self, path: &str) -> Option<&'static str> {
        if self.prefixes.iter().any(|p| path.starts_with(p.as_str())) {
            return Some("ignore rule");
        }
        // A ColdFusion engine's own files under `WEB-INF` (Lucee's context, compiled classes, admin): not the app's.
        if let Some((_, rest)) = path.split_once("WEB-INF/")
            && let Some(engine) = rest.split('/').next()
            && ["lucee", "railo", "cftags", "lucee-server", "bluedragon", "cfclasses"].iter().any(|e| engine.eq_ignore_ascii_case(e))
        {
            return Some("engine files");
        }
        for comp in path.split('/') {
            if comp.starts_with(".build") || comp.ends_with(".noindex") {
                return Some("build output");
            }
            if self.components.iter().any(|p| glob(p, comp)) {
                return Some("ignored directory");
            }
        }
        None
    }
}

fn glob(pattern: &str, s: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == s,
        Some((pre, post)) => s.len() >= pre.len() + post.len() && s.starts_with(pre) && s.ends_with(post),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub path: String,
    pub reason: &'static str,
}

/// Source files (repo-relative path and language) the index should cover.
pub type Sources = Vec<(String, Lang)>;

/// Source files the index should cover, plus files that were deliberately skipped.
pub fn discover(root: &Path) -> Result<(Sources, Vec<Skipped>)> {
    let d = discover_full(root)?;
    Ok((d.files, d.skipped))
}

struct Discovery {
    files: Vec<(String, Lang)>,
    skipped: Vec<Skipped>,
    /// Every listed, non-ignored path, source or not: what the content index covers.
    content: Vec<String>,
    /// Every directory holding a listed, non-ignored file, plus its ancestors (`""` is the root).
    dirs: BTreeSet<String>,
    /// Every YAML file whose language only its content tells (see [`sniff_file`]), admitted or not.
    sniffed: Vec<Sniffed>,
    /// Source files that are symbolic links, which are never followed (see [`is_symlink`]).
    symlinks: Vec<String>,
}

fn discover_full(root: &Path) -> Result<Discovery> {
    let privacy = privacy::Privacy::load(root);
    let mut d = Discovery {
        files: Vec::new(),
        skipped: Vec::new(),
        content: Vec::new(),
        dirs: BTreeSet::from([String::new()]),
        sniffed: Vec::new(),
        symlinks: Vec::new(),
    };
    if privacy.whole() {
        return Ok(d);
    }
    let out = git::run(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"])?;
    let ignore = Ignore::load(root)?;
    let mut seen = HashSet::new();
    for raw in out.split(|&b| b == 0).filter(|p| !p.is_empty()) {
        let Ok(path) = std::str::from_utf8(raw) else { continue };
        if !seen.insert(path) {
            continue;
        }
        // Blocked by the repo's AI privacy file: not read, not listed, not counted, its directories not recorded.
        if privacy.blocks(path) {
            continue;
        }
        let reason = ignore.reason(path);
        if reason.is_none() {
            let mut dir = path;
            while let Some((parent, _)) = dir.rsplit_once('/') {
                if !d.dirs.insert(parent.to_string()) {
                    break;
                }
                dir = parent;
            }
            d.content.push(path.to_string());
        }
        let lang = match Lang::from_path(path) {
            Some(l) => Some(l),
            None if reason.is_none() && is_sniff_candidate(path) => {
                let s = sniff_file(root, path);
                let lang = s.lang();
                d.sniffed.push(s);
                lang
            }
            None => None,
        };
        let Some(lang) = lang else { continue };
        match reason {
            Some(reason) => d.skipped.push(Skipped { path: path.to_string(), reason }),
            None if is_symlink(&root.join(path)) => {
                d.skipped.push(Skipped { path: path.to_string(), reason: "symlink" });
                d.symlinks.push(path.to_string());
            }
            None => d.files.push((path.to_string(), lang)),
        }
    }
    Ok(d)
}

/// Whether `path` is a symbolic link. Links are never followed, wherever they point: one leaving the repo would index
/// (and show) files that aren't the repo's, and one inside it names a file indexed under its own path. Content search
/// skips them too (see [`content`]).
fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink())
}

/// A YAML file the path doesn't place, at the stat its content was read at, and the language its content gave it, if
/// any. Kept from one full discovery to the next, so a candidate that changes is read again (an ordinary YAML file
/// becoming a playbook) without waiting for one.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
struct Sniffed {
    path: String,
    mtime_ns: i64,
    size: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lang: Option<String>,
}

impl Sniffed {
    fn lang(&self) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| Some(l.as_str()) == self.lang.as_deref())
    }
}

/// Whether the content of a file decides whether it's indexed: YAML that may be an Ansible playbook or GitLab CI.
fn is_sniff_candidate(path: &str) -> bool {
    kc_extract::ansible::candidate(path) || kc_extract::gitlabci::candidate(path)
}

/// The language of a YAML file the path doesn't place, by its content: an Ansible playbook (see
/// [`kc_extract::ansible::sniff`]) or GitLab CI (see [`kc_extract::gitlabci::sniff`]). A link isn't followed.
fn sniff_file(root: &Path, path: &str) -> Sniffed {
    let full = root.join(path);
    let meta = std::fs::symlink_metadata(&full).ok();
    let (mtime_ns, size) = meta.as_ref().map_or((0, -1), |m| (git::mtime_ns(m), m.len() as i64));
    let lang = (|| {
        meta.as_ref().filter(|m| m.is_file() && m.len() <= MAX_FILE_BYTES)?;
        let bytes = std::fs::read(&full).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        if kc_extract::ansible::candidate(path) && kc_extract::ansible::sniff(&text) {
            Some(Lang::Ansible)
        } else if kc_extract::gitlabci::candidate(path) && kc_extract::gitlabci::sniff(&text) {
            Some(Lang::GitlabCi)
        } else {
            None
        }
    })();
    Sniffed { path: path.to_string(), mtime_ns, size, lang: lang.map(|l| l.as_str().to_string()) }
}

/// Hash of a directory's sorted entry names: unchanged means nothing was added, removed or renamed there, even if
/// its mtime moved (editors that save by rename touch the directory).
fn names_hash(dir: &Path) -> Option<String> {
    let mut names: Vec<Vec<u8>> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().into_encoded_bytes())
        .collect();
    names.sort();
    let mut h = blake3::Hasher::new();
    for n in &names {
        h.update(n);
        h.update(b"\0");
    }
    Some(h.finalize().to_hex()[..16].to_string())
}

/// What a full discovery depends on besides directory listings: the git index, KnowCode's ignore files, and git's:
/// every `.gitignore` the last discovery listed (`ignores`), `info/exclude`, the global excludes file and the git
/// config files that may name it. Each by its stamp (mtime and size), so checking costs a few `stat`s and the reads
/// of the config files.
fn discovery_inputs(root: &Path, ignores: &[String]) -> String {
    let gd = git::GitDir::find(root);
    let index = gd.as_ref().map(|g| g.index_stamp()).unwrap_or_default();
    let user = config::user_ignore_stamp();
    // The languages this build indexes: a new one must trigger a full discovery to find its files.
    let langs = Lang::ALL.map(|l| l.as_str()).join(",");
    let mut git_rules = blake3::Hasher::new();
    let mut add = |p: &Path| {
        git_rules.update(p.as_os_str().as_encoded_bytes());
        git_rules.update(git::stamp(p).as_bytes());
    };
    add(&root.join(".gitignore"));
    for p in ignores {
        add(&root.join(p));
    }
    let mut configs = git::global_config_files();
    if let Some(g) = &gd {
        for dir in [&g.dir, &g.common] {
            add(&dir.join("info").join("exclude"));
        }
        configs.push(g.common.join("config"));
    }
    for c in &configs {
        add(c);
    }
    for f in git::excludes_files(&configs) {
        add(&f);
    }
    let git_rules = &git_rules.finalize().to_hex()[..16];
    // What the AI privacy file blocks (empty when nothing): a change rediscovers, leaving newly blocked files out.
    let privacy = privacy::Privacy::load(root).key();
    format!("{index}|{}|{user}|{langs}|{git_rules}|{privacy}", git::stamp(&root.join(".knowcodeignore")))
}

/// The `.gitignore` files among a discovery's listed paths.
fn gitignores(content: &[String]) -> Vec<String> {
    content.iter().filter(|p| p.rsplit('/').next() == Some(".gitignore")).cloned().collect()
}

/// What a reusable discovery gives: directories whose mtime moved (path, new mtime), and the sniffed candidates.
type Cached = (Vec<(String, i64)>, Vec<Sniffed>);

/// Reuses the last discovery when nothing it depends on changed, `ttl` also requiring that it is recent (see
/// [`DISCOVERY_TTL_SECS`]). Returns the directories whose mtime moved without their listing changing, so the caller
/// can record the new mtimes, and the content-sniffed candidates as last read.
fn cached_discovery(root: &Path, store: &Store, inputs: &str, ttl: bool) -> Result<Option<Cached>> {
    if store.meta("disc.inputs")?.as_deref() != Some(inputs) {
        return Ok(None);
    }
    let fresh = store.meta("disc.at")?.and_then(|a| a.parse::<u64>().ok()).is_some_and(|at| now_secs().saturating_sub(at) < DISCOVERY_TTL_SECS);
    if ttl && !fresh {
        return Ok(None);
    }
    // An index from a build that didn't record its candidates: discover again to find them.
    let Some(sniffed) = store.meta("disc.sniff")?.and_then(|s| serde_json::from_str::<Vec<Sniffed>>(&s).ok()) else {
        return Ok(None);
    };
    let dirs = store.dirs()?;
    if dirs.is_empty() {
        return Ok(None);
    }
    let mut moved = Vec::new();
    for (path, mtime, names) in dirs {
        let full = if path.is_empty() { root.to_path_buf() } else { root.join(&path) };
        let Ok(meta) = std::fs::metadata(&full) else { return Ok(None) };
        let now = git::mtime_ns(&meta);
        if now == mtime {
            continue;
        }
        if names_hash(&full).as_deref() != Some(names.as_str()) {
            return Ok(None);
        }
        moved.push((path, now));
    }
    Ok(Some((moved, sniffed)))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    pub files: usize,
    pub parsed: usize,
    pub touched: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub skipped: usize,
    /// Files that could not be read. Their facts stay as last indexed (none, for a new file), and they are tried again
    /// on the next sync; answers drawn from them say so (see `freshness.failed` in the query envelope).
    pub failed: Vec<String>,
    /// Source files that are symbolic links, which are never followed (seen by this sync).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symlinks: Vec<String>,
    /// Every file was read and hashed, whatever its stat said (see [`SyncOptions::verify`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub verified: bool,
    /// `cached` when the last file list was reused, `full` when `git ls-files` ran.
    pub discovery: &'static str,
    pub discover_ms: f64,
    pub stat_ms: f64,
    /// Reading, parsing and writing changed files (writes overlap the parsing).
    pub parse_ms: f64,
    /// Committing, and the checkpoint after a large change.
    pub write_ms: f64,
    pub total_ms: f64,
    pub bytes_parsed: u64,
    /// Set when this sync read the repo's SCIP index (new or changed since the last read).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scip: Option<scip::Report>,
}

pub struct SyncOptions {
    pub threads: usize,
    /// Only report what is stale; change nothing.
    pub dry_run: bool,
    /// Always run `git ls-files` instead of reusing the cached file list.
    pub full_discovery: bool,
    /// Read and hash every file instead of trusting an unchanged mtime and size, which an edit can keep (a tool that
    /// restores mtimes, a copy within the clock's resolution). Implies a full discovery. Defaults to whether
    /// `KNOWCODE_VERIFY` is set (to anything but `0`).
    pub verify: bool,
    /// When a SCIP index is read, compare the heuristic resolution with it (see [`scip::Report::agreement`]).
    pub scip_compare: bool,
    /// For testing that answers don't depend on the order files were stored in: parse and store files in reverse
    /// path order. Defaults to whether `KNOWCODE_INSERT_REVERSED` is set.
    pub insert_reversed: bool,
}

impl Default for SyncOptions {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        SyncOptions {
            threads: (cpus / 2).max(1),
            dry_run: false,
            full_discovery: false,
            verify: std::env::var_os("KNOWCODE_VERIFY").is_some_and(|v| !v.is_empty() && v != "0"),
            scip_compare: false,
            insert_reversed: std::env::var_os("KNOWCODE_INSERT_REVERSED").is_some(),
        }
    }
}

enum Work {
    Parsed { path: String, lang: Lang, mtime_ns: i64, size: i64, hash: String, lines: u32, facts: Box<FileFacts> },
    Touched { id: i64, mtime_ns: i64, size: i64 },
    Failed { path: String },
}

/// A file to read: path, language, current mtime and size, and its state in the index if it has one.
type Candidate = (String, Lang, i64, i64, Option<kc_store::FileState>);

/// What bringing the index up to date takes, from the file list and `stat`s alone.
struct Plan {
    files: Vec<(String, Lang)>,
    /// Directories whose mtime moved while their listing didn't.
    moved_dirs: Vec<(String, i64)>,
    /// The discovery run, when the cached file list couldn't be reused.
    full: Option<Discovery>,
    /// The content-sniffed candidates, when one was read again since they were recorded.
    sniffed: Option<Vec<Sniffed>>,
    /// Paths a read failed for at the last sync, tried again.
    failed_before: Vec<String>,
    candidates: Vec<Candidate>,
    large: Vec<(String, u64)>,
    /// Indexed files to drop: no longer listed, gone from disk, too large, or now a link.
    removed: Vec<i64>,
    symlinks: Vec<String>,
    unchanged: usize,
    skipped: usize,
    discovery: &'static str,
    discover_ms: f64,
    stat_ms: f64,
}

/// The plan for `root`'s index.
fn plan(root: &Path, store: &Store, states: &HashMap<String, kc_store::FileState>, opts: &SyncOptions) -> Result<Plan> {
    let ignores: Vec<String> = store.meta("disc.ignores")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let inputs = discovery_inputs(root, &ignores);
    let reuse = !opts.full_discovery && !opts.verify;
    let mark = meter::Mark::now();
    let cached = if reuse { cached_discovery(root, store, &inputs, true)? } else { None };
    let t0 = Instant::now();
    let failed_before: Vec<String> = store.meta("sync.failed")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut full = None;
    let mut sniffed = None;
    let (files, moved_dirs, discovery, mut skipped) = match cached {
        Some((moved, mut sniff)) => {
            // A candidate whose stat moved is read again: its content decides whether (and as what) it's indexed.
            let mut changed = false;
            for s in sniff.iter_mut() {
                let now = std::fs::symlink_metadata(root.join(&s.path)).map_or((0, -1), |m| (git::mtime_ns(&m), m.len() as i64));
                if now != (s.mtime_ns, s.size) {
                    *s = sniff_file(root, &s.path);
                    changed = true;
                }
            }
            let mut files: BTreeMap<String, Lang> = states.keys().filter_map(|p| Some((p.clone(), Lang::from_path(p)?))).collect();
            files.extend(sniff.iter().filter_map(|s| Some((s.path.clone(), s.lang()?))));
            // A file a read failed for may never have been stored: keep trying it.
            for p in &failed_before {
                if let Some(l) = Lang::from_path(p).or_else(|| sniff.iter().find(|s| s.path == *p).and_then(Sniffed::lang)) {
                    files.insert(p.clone(), l);
                }
            }
            if changed {
                sniffed = Some(sniff);
            }
            let skipped = store.meta("disc.skipped")?.and_then(|s| s.parse().ok()).unwrap_or(0);
            (files.into_iter().collect::<Vec<_>>(), moved, "cached", skipped)
        }
        None => {
            let d = discover_full(root)?;
            let files = d.files.clone();
            let skipped = d.skipped.len();
            full = Some(d);
            (files, Vec::new(), "full", skipped)
        }
    };

    let discover_ms = ms(t0);
    mark.record("discovery");
    let mark = meter::Mark::now();
    let t1 = Instant::now();
    let listed: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let mut removed: Vec<i64> = states.iter().filter(|(p, _)| !listed.contains(p.as_str())).map(|(_, s)| s.id).collect();
    let mut candidates = Vec::new();
    let mut large: Vec<(String, u64)> = Vec::new();
    let mut symlinks = Vec::new();
    let mut unchanged = 0;
    for (path, lang) in &files {
        // Not followed: a file replaced by a link since the last full discovery is dropped here.
        let Ok(meta) = std::fs::symlink_metadata(root.join(path)) else {
            // Listed by git but deleted from the working tree.
            removed.extend(states.get(path).map(|s| s.id));
            continue;
        };
        if meta.is_symlink() {
            skipped += 1;
            symlinks.push(path.clone());
            removed.extend(states.get(path).map(|s| s.id));
            continue;
        }
        if meta.len() > max_source_bytes(*lang) {
            skipped += 1;
            large.push((path.clone(), meta.len()));
            removed.extend(states.get(path).map(|s| s.id));
            continue;
        }
        let mtime_ns = git::mtime_ns(&meta);
        let size = meta.len() as i64;
        match states.get(path) {
            Some(s) if !opts.verify && s.mtime_ns == mtime_ns && s.size == size && s.extractor_version == lang.extractor_version() => {
                unchanged += 1;
            }
            state => candidates.push((path.clone(), *lang, mtime_ns, size, state.cloned())),
        }
    }
    let stat_ms = ms(t1);
    mark.record("stat");
    Ok(Plan { files, moved_dirs, full, sniffed, failed_before, candidates, large, removed, symlinks, unchanged, skipped, discovery, discover_ms, stat_ms })
}

/// Whether `root`'s working tree may have changed since the index at `db` was last synced, by `stat` alone: no `git`
/// process, no write, no opening of the index (see [`fresh`]). The last file list is trusted however old it is,
/// unless something it depends on changed (an ignore file, the git index, a directory listing). Yes whenever it can't
/// be shown otherwise. For deciding whether the workspace summary of a repo, written at its last sync, still holds
/// (see [`changed_repos`]).
pub fn changed_since_sync(root: &Path, db: &Path) -> bool {
    fresh::changed(root, db)
}

/// Whether derived data an earlier query computed (the areas, the file ranking) is from an earlier index generation
/// or history than `store`'s. Never computed, it isn't stale: the query needing it computes it.
pub fn derived_stale(root: &Path, store: &Store) -> Result<bool> {
    let rank_key = format!("rank_graph:{}", kc_store::rank::RANK_VERSION);
    let generation = store.generation()?;
    Ok(areas::needs_refresh(root, store)? || store.derived_gen(&rank_key)?.is_some_and(|g| g != generation))
}

/// Whether the file ranking (the reference graph) was never computed for `store`'s index.
pub fn derived_missing(store: &Store) -> Result<bool> {
    Ok(store.derived_gen(&format!("rank_graph:{}", kc_store::rank::RANK_VERSION))?.is_none())
}

/// Brings stale derived data (see [`derived_stale`]) up to date, after a sync: the background indexer does, so the
/// next query finds it current.
pub fn refresh_derived(root: &Path, store: &Store) -> Result<()> {
    derive(root, store, false)
}

/// [`refresh_derived`], and computes the derived data when it is missing (see [`derived_missing`]): the file ranking,
/// then the areas (which cluster over the same reference graph). `index` does, and a server in the background for the
/// repos it indexes, so the first `overview`, `hotspots` or `areas` call finds them rather than computing them.
pub fn build_derived(root: &Path, store: &Store) -> Result<()> {
    derive(root, store, true)
}

fn derive(root: &Path, store: &Store, missing: bool) -> Result<()> {
    let rank_key = format!("rank_graph:{}", kc_store::rank::RANK_VERSION);
    let generation = store.generation()?;
    let rank = store.derived_gen(&rank_key)?.map_or(missing, |g| g != generation);
    let areas = (missing && store.derived_gen("areas")?.is_none()) || areas::needs_refresh(root, store)?;
    // The areas' history walk is a `git` child that needs nothing of the ranking: given a thread to spare (as the
    // ranking is), it runs beside the ranking rather than after it. On bitwarden/clients it took 11 of the 19 s of a
    // cold `index`, alone. The background ranks on one thread and keeps the walk after it.
    let _ahead = if rank && areas && kc_store::rank::workers().threads > 1 { Some(vitals::walk_ahead(root, store)?) } else { None };
    if rank {
        store.ref_graph()?;
        store.file_rank_at(&[], kc_store::rank::Flow::Uses, true)?;
    }
    if areas {
        areas::areas(root, store)?;
    }
    Ok(())
}

/// Within a [`ChangeMemo`], [`changed_repos`] checks each repo once (a query consults the workspace summary from
/// several places). Outside one, every call checks afresh.
static CHANGE_MEMO: std::sync::Mutex<(usize, Option<HashMap<PathBuf, bool>>)> = std::sync::Mutex::new((0, None));

/// Remembers [`changed_repos`] answers until dropped: hold one for the length of one query. A repo seen changed stays
/// so, which only means it is opened (and synced) again.
pub struct ChangeMemo(());

/// Starts (or joins) a [`ChangeMemo`].
pub fn memo_changes() -> ChangeMemo {
    let mut m = CHANGE_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    m.0 += 1;
    m.1.get_or_insert_with(HashMap::new);
    ChangeMemo(())
}

impl Drop for ChangeMemo {
    fn drop(&mut self) {
        let mut m = CHANGE_MEMO.lock().unwrap_or_else(|e| e.into_inner());
        m.0 = m.0.saturating_sub(1);
        if m.0 == 0 {
            m.1 = None;
        }
    }
}

/// Of `roots`, the indexed ones whose working tree changed since their last sync (see [`changed_since_sync`]), checked
/// on a few threads. One no longer on disk isn't (nothing there to sync).
pub fn changed_repos(roots: &[PathBuf]) -> HashSet<PathBuf> {
    let check = |root: &PathBuf| -> bool {
        if !root.join(".git").exists() {
            return false;
        }
        if !db_path(root).is_ok_and(|p| p.exists()) {
            return workspace::has_saved_index(root).unwrap_or(true) || !light::unchanged(root);
        }
        changed_since_sync(root, &db_path(root).expect("checked above"))
    };
    let known: HashMap<PathBuf, bool> = {
        let m = CHANGE_MEMO.lock().unwrap_or_else(|e| e.into_inner());
        m.1.as_ref().map(|k| roots.iter().filter_map(|r| Some((r.clone(), *k.get(r)?))).collect()).unwrap_or_default()
    };
    let todo: Vec<&PathBuf> = roots.iter().filter(|r| !known.contains_key(*r)).collect();
    let chunk = todo.len().div_ceil(8).max(1);
    let checked: Vec<(PathBuf, bool)> = std::thread::scope(|scope| {
        let parts: Vec<_> = todo
            .chunks(chunk)
            .map(|part| scope.spawn(move || part.iter().map(|r| ((*r).clone(), check(r))).collect::<Vec<_>>()))
            .collect();
        parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    if let Some(memo) = CHANGE_MEMO.lock().unwrap_or_else(|e| e.into_inner()).1.as_mut() {
        memo.extend(checked.iter().cloned());
    }
    known.into_iter().chain(checked).filter(|(_, changed)| *changed).map(|(r, _)| r).collect()
}

/// Indexes `paths` again however their stat looks: their content was seen to differ from what the index holds (a
/// source read whose hash didn't match, see `source_changed`), which an edit keeping the mtime and size hides.
pub fn reindex(root: &Path, store: &mut Store, paths: &[&str]) -> Result<SyncReport> {
    writer::with_store(root, store, |store| reindex_locked(root, store, paths))
}

fn reindex_locked(root: &Path, store: &mut Store, paths: &[&str]) -> Result<SyncReport> {
    let states = store.file_states()?;
    fresh::invalidate(store);
    {
        let batch = store.batch()?;
        for p in paths {
            if let Some(s) = states.get(*p) {
                batch.touch(s.id, 0, -1)?;
            }
        }
        batch.commit()?;
    }
    sync(root, store, &SyncOptions::default())
}

/// Brings the index for `root` up to date with the working tree.
pub fn sync(root: &Path, store: &mut Store, opts: &SyncOptions) -> Result<SyncReport> {
    // The plan reads the index as committed now, and the reads after the sync see what it committed: the snapshot
    // held (see `Store::begin_snapshot`) is released for the sync and taken again after.
    let report = writer::with_store(root, store, |store| sync_unpinned(root, store, opts))?;
    if !opts.dry_run { light::forget(root)?; }
    Ok(report)
}

fn sync_unpinned(root: &Path, store: &mut Store, opts: &SyncOptions) -> Result<SyncReport> {
    let t0 = Instant::now();
    let mut report = SyncReport { verified: opts.verify, ..SyncReport::default() };
    // The index was purged for the policy it records when it was opened (see `open_with`). A policy that changed
    // since is applied by the next open, never by indexing under it here.
    let privacy = privacy::Privacy::load(root);
    if store.meta(privacy::META_KEY)?.unwrap_or_default() != privacy.key() {
        bail!("{}: its {} changed while it was being opened; ask again", root.display(), privacy::file_name());
    }
    if privacy.whole() {
        // Blocked whole: nothing is listed, read or stored.
        report.discovery = "blocked";
        report.total_ms = ms(t0);
        return Ok(report);
    }
    let states = store.file_states()?;
    let p = plan(root, store, &states, opts)?;
    let Plan { files, moved_dirs, full, sniffed, failed_before, mut candidates, large, removed, .. } = p;
    // Whether this sync writes what the index holds (file states, the file list, directories, candidates): its
    // snapshot (see [`fresh`]) is removed first and written again after.
    let rewrites = full.is_some() || sniffed.is_some() || !moved_dirs.is_empty() || !removed.is_empty() || !candidates.is_empty();
    report.discovery = p.discovery;
    report.skipped = p.skipped;
    report.files = files.len();
    report.symlinks = p.symlinks;
    report.unchanged = p.unchanged;
    report.discover_ms = p.discover_ms;
    report.stat_ms = p.stat_ms;

    if opts.dry_run {
        report.parsed = if opts.verify {
            // Only what really differs: verifying reads every file, and most are unchanged.
            candidates
                .iter()
                .filter(|(path, lang, _, _, state)| match std::fs::read(root.join(path)) {
                    Ok(bytes) => state.as_ref().is_none_or(|s| s.hash != blake3::hash(&bytes).to_hex().as_str() || s.extractor_version != lang.extractor_version()),
                    Err(_) => true,
                })
                .count()
        } else {
            candidates.len()
        };
        report.removed = removed.len();
        report.total_ms = ms(t0);
        return Ok(report);
    }

    let t2 = Instant::now();
    // This thread writes; workers parse. What the process spent less what this thread did is the parsing.
    let (mark, writer) = (meter::Mark::now(), meter::ThreadMark::now());
    if rewrites {
        fresh::invalidate(store);
    }
    // Whether a file the workspace summary reads changed: one removed, added, or reparsed into another summary key.
    let mut summary_changed = !removed.is_empty();
    let store_lacks_summary_gen = store.meta(SUMMARY_GEN)?.is_none();
    let di_stale = store.meta("di.resolver")?.as_deref() != Some(kc_store::RESOLVER_VERSION.to_string().as_str());
    let batch = store.batch()?;
    if let Some(d) = &full {
        let dirs: Vec<(String, i64, String)> = d
            .dirs
            .iter()
            .filter_map(|p| {
                let path = if p.is_empty() { root.to_path_buf() } else { root.join(p) };
                let meta = std::fs::metadata(&path).ok()?;
                Some((p.clone(), git::mtime_ns(&meta), names_hash(&path)?))
            })
            .collect();
        batch.replace_dirs(&dirs)?;
        batch.set_cfile_paths(&d.content)?;
        // The inputs as they are after this discovery, whose list of `.gitignore` files may differ.
        let ignores = gitignores(&d.content);
        batch.set_meta("disc.inputs", &discovery_inputs(root, &ignores))?;
        batch.set_meta("disc.ignores", &serde_json::to_string(&ignores)?)?;
        batch.set_meta("disc.sniff", &serde_json::to_string(&d.sniffed)?)?;
        batch.set_meta("disc.symlinks", &serde_json::to_string(&d.symlinks)?)?;
        batch.set_meta("disc.at", &now_secs().to_string())?;
        batch.set_meta("disc.skipped", &report.skipped.to_string())?;
        batch.set_meta("disc.large", &serde_json::to_string(&large)?)?;
    }
    if let Some(s) = &sniffed {
        batch.set_meta("disc.sniff", &serde_json::to_string(s)?)?;
    }
    for (path, mtime) in &moved_dirs {
        batch.set_dir_mtime(path, *mtime)?;
    }
    for id in &removed {
        batch.remove(*id)?;
        report.removed += 1;
    }

    // Workers parse while this thread writes each file as it arrives. The bounded channel keeps only a few files'
    // facts in memory, instead of every file's until the end, which made a big repo's facts the peak of a cold index.
    if opts.insert_reversed {
        candidates.reverse();
    }
    let next = AtomicUsize::new(0);
    let threads = opts.threads.clamp(1, candidates.len().max(1));
    let (tx, rx) = mpsc::sync_channel::<Work>(threads * 4);
    std::thread::scope(|scope| -> Result<()> {
        for _ in 0..threads {
            let tx = tx.clone();
            let next = &next;
            let candidates = &candidates;
            // Deeply nested SwiftUI code makes for deep trees; give the recursive walk room.
            std::thread::Builder::new()
                .stack_size(16 << 20)
                .spawn_scoped(scope, move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((path, lang, mtime_ns, size, state)) = candidates.get(i) else { break };
                        let work = match std::fs::read(root.join(path)) {
                            Err(_) => Work::Failed { path: path.clone() },
                            Ok(bytes) => {
                                let hash = blake3::hash(&bytes).to_hex().to_string();
                                match state {
                                    Some(s) if s.hash == hash && s.extractor_version == lang.extractor_version() => {
                                        Work::Touched { id: s.id, mtime_ns: *mtime_ns, size: *size }
                                    }
                                    _ => {
                                        let source = kc_extract::decode(&bytes);
                                        let lines = source.lines().count() as u32;
                                        let mut facts = kc_extract::extract_file(*lang, path, &source);
                                        if lang.is_manifest() {
                                            resolve_path_deps(root, path, &mut facts);
                                        }
                                        if *lang == Lang::Ansible {
                                            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                            kc_extract::ansible::name_root_role(&mut facts, &name);
                                        }
                                        Work::Parsed {
                                            path: path.clone(),
                                            lang: *lang,
                                            mtime_ns: *mtime_ns,
                                            size: *size,
                                            hash,
                                            lines,
                                            facts: Box::new(facts),
                                        }
                                    }
                                }
                            }
                        };
                        // Fails only once the writer has stopped on an error.
                        if tx.send(work).is_err() {
                            break;
                        }
                    }
                })
                .expect("spawning index thread");
        }
        drop(tx);
        // On an error, `rx` is dropped on return, which stops the workers.
        for work in rx {
            match work {
                Work::Parsed { path, lang, mtime_ns, size, hash, lines, facts } => {
                    summary_changed |= batch.put(&FileRecord { path: &path, lang, mtime_ns, size, hash: &hash, lines, facts: &facts })?;
                    report.parsed += 1;
                    report.bytes_parsed += size as u64;
                }
                Work::Touched { id, mtime_ns, size } => {
                    batch.touch(id, mtime_ns, size)?;
                    report.touched += 1;
                }
                Work::Failed { path } => report.failed.push(path),
            }
        }
        Ok(())
    })?;
    report.failed.sort();
    if report.failed != failed_before {
        batch.set_meta("sync.failed", &serde_json::to_string(&report.failed)?)?;
    }
    report.parse_ms = ms(t2);
    // The writing overlaps the parsing: the phase's wall time goes to the parse.
    let (both, rows) = (mark.cost(), writer.cost());
    meter::record("parse", meter::Cost { wall_ms: both.wall_ms, ..both - rows });
    meter::record("write.rows", meter::Cost { wall_ms: 0.0, ..rows });

    let t3 = Instant::now();
    let mark = meter::Mark::now();
    // What DI factories make, from declarations in any file: again whenever a file changed, or resolution did.
    if report.parsed + report.removed > 0 || di_stale {
        batch.resolve_di_factories()?;
        batch.set_meta("di.resolver", &kc_store::RESOLVER_VERSION.to_string())?;
    }
    if report.parsed + report.removed > 0 {
        batch.set_meta("last_sync", &now_secs().to_string())?;
        // Invalidates the cached `dirty` flag.
        batch.set_meta("tree_gen", &now_nanos().to_string())?;
    }
    // In the same commit as the facts: a summary is reused while this generation holds (see `workspace`).
    if summary_changed || store_lacks_summary_gen {
        batch.set_meta(SUMMARY_GEN, &now_nanos().to_string())?;
    }
    batch.commit()?;
    if report.removed > 200 {
        store.vacuum()?;
    }
    if report.parsed > 200 || report.removed > 200 {
        store.checkpoint()?;
    }
    mark.record("write.commit");
    // SDK packages the repo declares as external clients, read from its `node_modules` when they changed.
    let mark = meter::Mark::now();
    sdk::refresh(root, store, &privacy)?;
    mark.record("sdk");
    let mark = meter::Mark::now();
    report.scip = scip::refresh(root, store, opts.scip_compare)?;
    mark.record("scip");
    let mark = meter::Mark::now();
    if !report.failed.is_empty() {
        fresh::invalidate(store);
    } else if rewrites || !fresh::exists(store) {
        // Best effort: without it, the repo only counts as changed.
        let _ = fresh::write(store);
    }
    mark.record("write.snapshot");
    report.write_ms = ms(t3);
    report.total_ms = ms(t0);
    Ok(report)
}

/// Points each path dependency at the repo it lands in, so repo-to-repo edges need no further lookup.
fn resolve_path_deps(root: &Path, manifest: &str, facts: &mut FileFacts) {
    let dir = Path::new(manifest).parent().map_or_else(|| root.to_path_buf(), |d| root.join(d));
    for dep in facts.packages.iter_mut().flat_map(|p| p.deps.iter_mut()) {
        if dep.source == kc_extract::DepSource::Path {
            dep.resolved_repo = repo_root(&dir.join(&dep.location)).ok().map(|r| r.to_string_lossy().into_owned());
        }
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn now_nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Total on-disk size of a repo's index, including the WAL.
pub fn index_bytes(root: &Path) -> Result<u64> {
    let db = db_path(root)?;
    let mut total = 0;
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", db.display()));
        total += std::fs::metadata(p).map_or(0, |m| m.len());
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_rules() {
        let ig = Ignore { prefixes: vec!["Generated/".into()], components: vec!["Pods".into(), "*.xcframework".into()] };
        assert_eq!(ig.reason("Generated/Foo.swift"), Some("ignore rule"));
        assert_eq!(ig.reason("App/Pods/Foo.swift"), Some("ignored directory"));
        assert_eq!(ig.reason("Lib/Bar.xcframework/X.swift"), Some("ignored directory"));
        assert_eq!(ig.reason(".build/checkouts/x.swift"), Some("build output"));
        assert_eq!(ig.reason(".build-ios/x.swift"), Some("build output"));
        assert_eq!(ig.reason("Cache.noindex/x.swift"), Some("build output"));
        assert_eq!(ig.reason("Sources/App/View.swift"), None);
        assert_eq!(ig.reason("WEB-INF/lucee/context/Application.cfc"), Some("engine files"));
        assert_eq!(ig.reason("site/WEB-INF/cftags/x.cfm"), Some("engine files"));
        assert_eq!(ig.reason("WEB-INF/web.xml"), None);
    }

    #[test]
    fn user_rules_are_scoped_to_repos_from_the_legacy_file_and_from_toml_alike() {
        let legacy = config::legacy_ignore_rules("# comment\naether-engine records/\nother-repo Fixtures/\n* Generated\n/abs/engine snapshots/\n");
        let toml = config::Config::parse(
            "[ignore]\naether-engine = [\"records/\"]\nother-repo = [\"Fixtures/\"]\n\"*\" = [\"Generated\"]\n\"/abs/engine\" = [\"snapshots/\"]\n",
            Path::new("config.toml"),
        )
        .unwrap();
        let toml = config::config_ignore_rules(&toml.ignore.unwrap());
        for rules in [&legacy, &toml] {
            let mut ig = Ignore { prefixes: Vec::new(), components: Vec::new() };
            ig.add_user_rules(Path::new("/x/aether-engine"), rules);
            assert_eq!(ig.reason("records/diagnostics/A.swift"), Some("ignore rule"));
            assert_eq!(ig.reason("Fixtures/A.swift"), None);
            assert_eq!(ig.reason("Sources/Generated/A.swift"), Some("ignored directory"));
            assert_eq!(ig.reason("snapshots/A.swift"), None);
            ig.add_user_rules(Path::new("/abs/engine"), rules);
            assert_eq!(ig.reason("snapshots/A.swift"), Some("ignore rule"));
        }
    }
}
