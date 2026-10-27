//! Registered-root discovery: manifests and name filters, without persistent symbol or content indexes.
//! Full graph facts remain unavailable until a query touches a checkout and performs its normal sync.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{git, workspace::{self, RepoSummary}};
use kc_store::manifests::{PackageRow, PackageDepRow, TargetRow, TargetDepRow};

#[derive(Clone, Default, Serialize, Deserialize)]
struct Catalog {
    format: String,
    entries: BTreeMap<PathBuf, Entry>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    summary: RepoSummary,
    snapshot: Option<Snapshot>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    inputs: String,
    ignores: Vec<String>,
    dirs: Vec<(String, String)>,
    files: Vec<(String, String)>,
    privacy: String,
}

fn format() -> String {
    // The extractor versions hold the configured conventions (see `kc_extract::conventions`): read them first.
    let _ = crate::config::load();
    format!("1.{}.{}", kc_store::SCHEMA_VERSION, kc_extract::Lang::ALL.map(|l| l.extractor_version().to_string()).join("."))
}
pub fn path() -> Result<PathBuf> { Ok(crate::cache_dir()?.join("discovery.json")) }
pub(crate) fn stamp() -> Result<String> { Ok(git::stamp(&path()?)) }
fn shared() -> Result<std::sync::Arc<Catalog>> {
    let path = path()?;
    type Memo = Option<(PathBuf, String, std::sync::Arc<Catalog>)>;
    static MEMO: std::sync::Mutex<Memo> = std::sync::Mutex::new(None);
    let mut memo = MEMO.lock().unwrap_or_else(|e| e.into_inner());
    let stamp = git::stamp(&path);
    if let Some((previous, stored, catalog)) = memo.as_ref() && *previous == path && *stored == stamp { return Ok(catalog.clone()); }
    let catalog = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Catalog>(&bytes).ok().filter(|c| c.format == format()).unwrap_or_default(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Catalog::default(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let catalog = std::sync::Arc::new(catalog);
    *memo = Some((path, stamp, catalog.clone()));
    Ok(catalog)
}
fn read() -> Result<Catalog> { Ok((*shared()?).clone()) }
fn write(catalog: &mut Catalog) -> Result<()> {
    let path = path()?;
    if crate::cache::durable_file(&path)? { bail!("discovery path overlaps durable notes; retained {}", path.display()); }
    catalog.format = format();
    let parent = path.parent().context("discovery path has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    let result = (|| -> Result<()> {
        file.write_all(&serde_json::to_vec(catalog)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(tmp); }
    result
}

/// Refresh the cheap checkout inventory once per profile/configuration per minute. Source scanning remains in
/// the background worker; CLI queries also discover roots registered by editing config.toml directly.
pub(crate) fn ensure_discovered() -> Result<()> {
    let (roots, depth) = crate::config::workspace()?;
    if roots.is_empty() { return Ok(()); }
    type Last = Option<(PathBuf, Vec<PathBuf>, usize, std::time::Instant, Vec<(PathBuf, String)>)>;
    static LAST: std::sync::Mutex<Last> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let cache = crate::cache_dir()?;
    if path()?.exists() && last.as_ref().is_some_and(|(previous, dirs, d, at, stamps)| *previous == cache && *dirs == roots && *d == depth
        && at.elapsed() < std::time::Duration::from_secs(60) && stamps.iter().all(|(p, stamp)| git::stamp(p) == *stamp)) { return Ok(()); }
    discover_roots()?;
    let stamps = roots.iter().flat_map(|r| crate::roots::inventory(r, depth).1).map(|p| { let stamp = git::stamp(&p); (p, stamp) }).collect();
    *last = Some((cache, roots, depth, std::time::Instant::now(), stamps));
    Ok(())
}

/// Publish root-only placeholders before source scanning, so a query cannot mistake discovery in progress for
/// an empty workspace. Repository enumeration does not parse source or create any SQLite database.
pub fn discover_roots() -> Result<usize> {
    let configured = crate::roots::configured()?;
    let roots: BTreeSet<_> = configured.into_iter().flat_map(|(_, repos)| repos).collect();
    if roots.is_empty() && !path()?.exists() { return Ok(0); }
    let _metadata = crate::writer::workspace(true)?;
    let mut catalog = read()?;
    let before: Vec<_> = catalog.entries.keys().cloned().collect();
    catalog.entries.retain(|root, _| roots.contains(root) && !crate::db_path(root).is_ok_and(|p| p.exists()));
    for root in &roots {
        if crate::db_path(root)?.exists() || workspace::has_saved_index(root)? { continue; }
        catalog.entries.entry(root.clone()).or_insert_with(|| Entry {
            summary: RepoSummary::discovery(root.clone(), Vec::new(), Vec::new(), &[], &[], String::new()), snapshot: None,
        });
    }
    if before != catalog.entries.keys().cloned().collect::<Vec<_>>() { write(&mut catalog)?; }
    Ok(catalog.entries.len())
}

pub(crate) fn summaries() -> Result<Vec<RepoSummary>> {
    let Some(mut roots) = crate::roots::Roots::configured()? else { return Ok(Vec::new()) };
    Ok(shared()?.entries.values().map(|e| &e.summary).filter(|s| roots.contains(&s.root) && s.root.join(".git").exists()).cloned().collect())
}

/// True only when a completed name scan still describes this tree. Missing/failed scans never rule a repo out.
/// Used only for workspace candidate pruning, never to declare a full query index fresh.
pub fn unchanged(root: &Path) -> bool {
    if crate::SyncOptions::default().verify { return false; }
    let Ok(catalog) = shared() else { return false };
    let Some(entry) = catalog.entries.get(root) else { return false };
    current(entry)
}

fn current(entry: &Entry) -> bool {
    let root = &entry.summary.root;
    let Some(snapshot) = &entry.snapshot else { return false };
    snapshot.privacy == crate::privacy::Privacy::load(root).key()
        && snapshot.inputs == crate::discovery_inputs(root, &snapshot.ignores)
        && snapshot.dirs.iter().all(|(p, stamp)| git::stamp(&root.join(p)) == *stamp)
        && snapshot.files.iter().all(|(p, stamp)| !crate::is_symlink(&root.join(p)) && git::stamp(&root.join(p)) == *stamp)
}

pub fn pending() -> Result<Vec<PathBuf>> {
    let Some(mut roots) = crate::roots::Roots::configured()? else { return Ok(Vec::new()) };
    Ok(shared()?.entries.values().filter(|e| !e.summary.blocked.whole && roots.contains(&e.summary.root) && !current(e)).map(|e| e.summary.root.clone()).collect())
}

/// Scan one checkout with one extractor at a time. Facts are discarded after collecting names and manifests;
/// there is no resolver, reference graph, history, content index, or persistent SQLite database.
pub fn scan(root: &Path) -> Result<()> {
    scan_until(root, || false).map(|_| ())
}

/// A live server yields discovery between files when a query needs the indexing worker.
pub fn scan_until(root: &Path, mut should_yield: impl FnMut() -> bool) -> Result<bool> {
    let privacy = crate::privacy::Privacy::load(root);
    let discovery = crate::discover_full(root)?;
    let ignores: Vec<_> = discovery.content.iter().filter(|p| p.rsplit('/').next() == Some(".gitignore")).cloned().collect();
    let inputs = crate::discovery_inputs(root, &ignores);
    let dirs: Vec<_> = discovery.dirs.iter().map(|p| (p.clone(), git::stamp(&root.join(p)))).collect();
    let mut files = Vec::new();
    let mut packages = Vec::new();
    let mut names = Vec::new();
    let mut defines = Vec::new();
    let mut failed = false;
    for (relative, lang) in discovery.files {
        if should_yield() { return Ok(false); }
        kc_store::limits::check()?;
        let path = root.join(&relative);
        let before = git::stamp(&path);
        files.push((relative.clone(), before.clone()));
        let Ok(meta) = fs::symlink_metadata(&path) else { failed = true; continue };
        if !meta.is_file() || meta.is_symlink() { failed = true; continue; }
        if meta.len() > crate::max_source_bytes(lang) { continue; }
        // Recheck policy immediately before the read; policy changes also invalidate publication below.
        if crate::privacy::Privacy::load(root).blocks(&relative) { failed = true; break; }
        let bytes = match fs::read(&path) { Ok(bytes) => bytes, Err(_) => { failed = true; continue; } };
        if git::stamp(&path) != before { failed = true; }
        let text = kc_extract::decode(&bytes);
        let mut facts = kc_extract::extract_file(lang, &relative, &text);
        crate::resolve_path_deps(root, &relative, &mut facts);
        if lang == kc_extract::Lang::Ansible {
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            kc_extract::ansible::name_root_role(&mut facts, &name);
        }
        defines.extend(facts.symbols.iter().flat_map(|s| [s.name.clone(), s.name.to_lowercase()]));
        names.extend(facts.refs.iter().flat_map(|r| [r.name.clone(), r.name.to_lowercase()]));
        names.extend(facts.relations.iter().flat_map(|r| [r.target.clone(), r.target.to_lowercase()]));
        for package in facts.packages {
            packages.push(PackageRow {
                name: package.name, kind: package.kind.into(), file: relative.clone(), line: package.line,
                targets: package.targets.into_iter().map(|t| TargetRow {
                    name: t.name, kind: t.kind, line: t.line,
                    deps: t.deps.into_iter().map(|d| TargetDepRow { name: d.name, product: d.product, package: d.package, kind: d.kind.into(), line: d.line }).collect(),
                }).collect(),
                deps: package.deps.into_iter().filter(|d| !matches!(d.source, kc_extract::DepSource::Compile | kc_extract::DepSource::Using)).map(|d| PackageDepRow {
                    name: d.name, source: d.source.as_str().into(), location: d.location, requirement: d.requirement, resolved_repo: d.resolved_repo, line: d.line,
                }).collect(),
            });
        }
    }
    names.sort(); names.dedup(); defines.sort(); defines.dedup();
    packages.sort_by(|a, b| (&a.file, a.line, &a.name).cmp(&(&b.file, b.line, &b.name)));
    let mut modules: Vec<_> = packages.iter().flat_map(|p| p.targets.iter().map(|t| t.name.clone())).collect();
    modules.sort(); modules.dedup();
    let snapshot = Snapshot { inputs, ignores, dirs, files, privacy: privacy.key() };
    let generation = blake3::hash(&serde_json::to_vec(&snapshot)?).to_hex()[..16].to_string();
    let mut summary = RepoSummary::discovery(root.to_path_buf(), packages, modules, &names, &defines, format!("light:{generation}"));
    summary.origin = git::run(root, &["config", "--get", "remote.origin.url"]).ok()
        .and_then(|b| workspace::normalize_remote(String::from_utf8_lossy(&b).trim()));
    if failed { summary.unknown_names(); }
    let entry = Entry { summary, snapshot: (!failed).then_some(snapshot) };
    let _metadata = crate::writer::workspace(true)?;
    let mut catalog = read()?;
    // A query may have indexed, pruned, or unregistered this checkout while the source scan was running.
    if !catalog.entries.contains_key(root) { return Ok(true); }
    if crate::db_path(root)?.exists() || workspace::has_saved_index(root)? {
        catalog.entries.remove(root);
    } else if crate::privacy::Privacy::load(root).key() != privacy.key() {
        catalog.entries.insert(root.to_path_buf(), Entry { summary: RepoSummary::discovery(root.to_path_buf(), Vec::new(), Vec::new(), &[], &[], String::new()), snapshot: None });
    } else { catalog.entries.insert(root.to_path_buf(), entry); }
    write(&mut catalog)?;
    Ok(true)
}

/// Remove only the checkout's light entry once a full sync has committed (also used by explicit forget).
pub fn forget(root: &Path) -> Result<()> {
    if !path()?.exists() { return Ok(()); }
    let _metadata = crate::writer::workspace(true)?;
    let mut catalog = read()?;
    if catalog.entries.remove(root).is_some() { write(&mut catalog)?; }
    Ok(())
}

/// Fail closed immediately on a policy change, without reading source on a query's foreground path.
pub(crate) fn redact(root: &Path) -> Result<()> {
    if !path()?.exists() { return Ok(()); }
    let _metadata = crate::writer::workspace(true)?;
    let mut catalog = read()?;
    if catalog.entries.contains_key(root) {
        catalog.entries.insert(root.to_path_buf(), Entry { summary: RepoSummary::discovery(root.to_path_buf(), Vec::new(), Vec::new(), &[], &[], String::new()), snapshot: None });
        write(&mut catalog)?;
    }
    Ok(())
}

pub(crate) fn clear() -> Result<()> {
    let path = path()?;
    if !path.exists() || crate::cache::durable_file(&path)? { return Ok(()); }
    let _metadata = crate::writer::workspace(true)?;
    // Unknown formats belong to another build; do not delete them.
    if fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Catalog>(&b).ok()).is_some_and(|c| c.format == format()) {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn prune_missing() -> Result<()> {
    if !path()?.exists() { return Ok(()); }
    let _metadata = crate::writer::workspace(true)?;
    let mut catalog = read()?;
    let before = catalog.entries.len();
    catalog.entries.retain(|root, _| root.join(".git").exists());
    if before != catalog.entries.len() { write(&mut catalog)?; }
    Ok(())
}
