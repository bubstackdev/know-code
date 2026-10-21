//! Query operations shared by the CLI and the MCP server. Each returns the spec's JSON envelope plus typed data
//! for the CLI's text rendering.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use kc_index::content;
use kc_index::search;
use kc_index::structural;
use kc_index::{Head, SyncOptions, SyncReport};
use kc_index::ownership::{self, Ownership};
use kc_index::vitals::{self, FileVitals};
use kc_store::Store;
use kc_store::answer::{Assumption, Evidence, Incomplete, Mechanism, Provenance, Reason, Verdict};
use kc_store::db::{self, DbModel, SymbolTables};
use kc_store::locate as query_locate;
use kc_store::manifests::{self, ManifestHit, RepoEdge};
use kc_store::query::{self, Context, ContextLimits, FindHit, RefHit};
use kc_store::trace::{self, Impact, Trace};
use serde::Serialize;
use serde_json::{Value, json};

use crate::cursor;

/// Which repos an operation covers, and whether to skip the freshness check.
#[derive(Clone, Default)]
pub struct Scope {
    pub repos: Vec<PathBuf>,
    pub no_sync: bool,
}

pub struct Out<T> {
    pub env: Value,
    pub data: T,
}

/// Repos a query covers: the `--repo` args, else the repo around the current directory, else every indexed repo in
/// the workspace (see [`kc_index::indexed_roots`]).
pub fn target_repos(args: &[PathBuf]) -> Result<Vec<PathBuf>> {
    Ok(scope_repos(args)?.0)
}

/// [`target_repos`], and whether they are the whole workspace (no `--repo`, and the current directory in no repo).
/// Cross-repo lookups pass the repos they answer for to [`kc_index::workspace::summaries`], which keeps them in the
/// workspace even outside every workspace root. A repo named (or around the current directory) that its AI privacy
/// file blocks whole is refused: there is nothing KnowCode may answer from it. The workspace leaves such repos out.
fn scope_repos(args: &[PathBuf]) -> Result<(Vec<PathBuf>, bool)> {
    let (roots, workspace) = scope_repos_any(args)?;
    for r in &roots {
        let p = kc_index::privacy::Privacy::load(r);
        if p.whole() {
            // Opening purges what an index made before the policy holds.
            drop(kc_index::open(r)?); // also replaces an evicted index's cached summary with the blocked marker
            bail!(
                "withheld: {}: its {} blocks the whole repo, so KnowCode indexes and answers nothing from it (`knowcode status` shows the policy)",
                p.detail(&repo_name(r)),
                kc_index::privacy::file_name()
            );
        }
    }
    Ok((roots, workspace))
}

/// Razor files (`.razor`, `.cshtml`) in the repo `o`, which KnowCode doesn't read: the `unsupported` entry saying a
/// C# answer may lack the uses in their markup and `@code` blocks, naming the repo when `named`. `None` without any.
pub fn razor_unread(o: &Opened, named: bool) -> Result<Option<Incomplete>> {
    let mut stmt = o.store.conn().prepare_cached("SELECT path FROM cfiles WHERE path LIKE '%.razor' OR path LIKE '%.cshtml' ORDER BY path")?;
    let paths: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let Some(first) = paths.first() else { return Ok(None) };
    let repo = if named { format!("{}: ", repo_name(&o.root)) } else { String::new() };
    let n = paths.len();
    let files = if n == 1 { "1 Razor file".to_string() } else { format!("{n} Razor files") };
    Ok(Some(
        Incomplete::new(
            Reason::Unsupported,
            format!("{repo}{files} (.razor, .cshtml) not read, such as {first}: uses of C# code in their markup and @code blocks are not listed"),
        )
        .with_hint("kc_search finds them as text (paths: [\"*.razor\", \"*.cshtml\"])"),
    ))
}

/// The `withheld` entry for the repo at `root` when its AI privacy file blocks anything: an answer drawing on it may
/// lack what is blocked. Names the repo and the level, never a blocked path.
pub fn withheld(root: &Path) -> Option<Incomplete> {
    let p = kc_index::privacy::Privacy::load(root);
    p.blocks_anything().then(|| {
        let what = if p.whole() { "the whole repo" } else { "paths in it" };
        Incomplete::new(Reason::Withheld, p.detail(&repo_name(root)))
            .with_hint(format!("its {} blocks {what}; KnowCode never reads or answers from blocked content", kc_index::privacy::file_name()))
    })
}

/// Refuses a question that names a path its repo's AI privacy file blocks (a file, `file:line`, `file#name`, a
/// directory or glob under a blocked folder), without repeating the path: the CLI and the MCP server check every
/// argument against the repos asked about (`repos`, else the one around the current directory) before answering.
pub fn guard_inputs(repos: &[PathBuf], inputs: &[String]) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let roots: Vec<PathBuf> = if repos.is_empty() {
        kc_index::repo_root(&cwd).ok().into_iter().collect()
    } else {
        repos.iter().filter_map(|r| kc_index::repo_root(r).ok()).collect()
    };
    for root in roots {
        let p = kc_index::privacy::Privacy::load(&root);
        // A repo blocked whole is refused by `scope_repos`, which also covers what `status` may say.
        if !p.blocks_anything() || p.whole() {
            continue;
        }
        let here = cwd.strip_prefix(&root).ok().map(Path::to_path_buf);
        for s in inputs {
            if names_blocked(&root, here.as_deref(), &p, s) {
                bail!(
                    "withheld: {}: the path asked about is in an area its {} blocks, so KnowCode answers nothing about it",
                    p.detail(&repo_name(&root)),
                    kc_index::privacy::file_name()
                );
            }
        }
    }
    Ok(())
}

/// Whether the argument `s` names a path `p` blocks in the repo at `root`, as written, relative to the repo, or
/// relative to the current directory `here` within it.
fn names_blocked(root: &Path, here: Option<&Path>, p: &kc_index::privacy::Privacy, s: &str) -> bool {
    let mut c = s.split('#').next().unwrap_or(s).trim();
    for _ in 0..2 {
        if let Some((a, b)) = c.rsplit_once(':')
            && !b.is_empty()
            && b.bytes().all(|x| x.is_ascii_digit())
        {
            c = a;
        }
    }
    if c.is_empty() {
        return false;
    }
    let mut candidates = vec![Path::new(c).strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| c.to_string())];
    if let Some(h) = here.filter(|h| !h.as_os_str().is_empty()) {
        candidates.push(h.join(c).to_string_lossy().into_owned());
    }
    candidates.iter().any(|rel| {
        let pathlike = rel.contains('/') || rel.contains('.') || root.join(rel).exists();
        pathlike && !rel.is_empty() && p.blocks(rel)
    })
}

/// [`scope_repos`] without refusing blocked repos: for `status`, which reports what is blocked.
fn scope_repos_any(args: &[PathBuf]) -> Result<(Vec<PathBuf>, bool)> {
    let (roots, workspace) = if !args.is_empty() {
        (args.iter().map(|p| kc_index::repo_root(p)).collect::<Result<_>>()?, false)
    } else if let Ok(root) = kc_index::repo_root(&std::env::current_dir()?) {
        (vec![root], false)
    } else {
        let roots = kc_index::indexed_roots()?;
        if roots.is_empty() {
            if kc_index::config::workspace()?.0.is_empty() {
                bail!("not in a git repo and nothing is indexed; run `knowcode index <repo>` or pass --repo");
            }
            bail!("not in a git repo and no repo under the workspace roots is indexed; run `knowcode index <repo>` or pass --repo");
        }
        (roots, true)
    };
    Ok((roots, workspace))
}

/// Of `roots` (every indexed repo), those whose workspace summary `keep` accepts, those without a summary, and those
/// whose working tree changed since their summary was written. A workspace-wide query for one name opens only the
/// repos that can answer it: opening and syncing every repo's index cost far more than the query. The summary is a
/// repo's as of its last sync, so it only rules a repo out while the repo is unchanged since (checked by `stat`, see
/// [`kc_index::changed_repos`]); a changed one is opened and synced like any other. With `no_sync`, summaries are
/// trusted as they are.
fn workspace_roots(roots: Vec<PathBuf>, no_sync: bool, keep: impl Fn(&kc_index::workspace::RepoSummary) -> bool) -> Result<Vec<PathBuf>> {
    let summaries = kc_index::workspace::summaries(&roots)?;
    let at: HashMap<&Path, &kc_index::workspace::RepoSummary> = summaries.iter().map(|s| (s.root.as_path(), s)).collect();
    let ruled_out: Vec<PathBuf> = roots.iter().filter(|r| at.get(r.as_path()).is_some_and(|s| !keep(s))).cloned().collect();
    let changed = changed_unless(no_sync, &ruled_out);
    Ok(roots.into_iter().filter(|r| changed.contains(r) || at.get(r.as_path()).is_none_or(|s| keep(s))).collect())
}

/// Of `roots`, those changed since their last sync (see [`kc_index::changed_repos`]); none with `no_sync`, whose
/// answers are the index as it is.
fn changed_unless(no_sync: bool, roots: &[PathBuf]) -> HashSet<PathBuf> {
    if no_sync || roots.is_empty() { HashSet::new() } else { kc_index::changed_repos(roots) }
}

/// Of every indexed repo, those `refs <name>` may find something in, by the workspace summary: a reference by the
/// name in the repo itself; a definition of the name with a dependent repo that may reference it (the dependents
/// [`dependent_refs`] opens); or an Ansible role of that name ([`role_refs_elsewhere`]). A repo changed since its
/// summary was written may reference anything: it is kept, and so are its dependencies defining the name (see
/// [`workspace_roots`]).
fn refs_roots(roots: Vec<PathBuf>, name: &str, kinds: &[&str], no_sync: bool) -> Result<Vec<PathBuf>> {
    let summaries = kc_index::workspace::summaries(&roots)?;
    let graph = kc_index::workspace::graph(&summaries);
    let bare = name.trim();
    let changed = changed_unless(no_sync, &roots);
    let mut depended: HashSet<&str> = HashSet::new();
    for (i, s) in summaries.iter().enumerate() {
        if (s.may_reference(bare) || changed.contains(&s.root)) && s.root.join(".git").exists() {
            depended.extend(graph.dependencies(i).iter().map(String::as_str));
        }
    }
    // Repos holding a module or role that another repo uses by the name: a module call setting the variable or
    // reading the output (`crate::tf::module_refs_elsewhere`), a role reference (`role_refs_elsewhere`). Neither is a
    // manifest edge, and a module's outputs are never referenced in its own repo.
    let names = [bare, query::split_dotted(bare).1];
    let named = |n: &str| names.contains(&n);
    let tf = kc_index::terraform::Terraform::new(&summaries);
    let ansible = kc_index::ansible::Ansible::new(&summaries);
    let mut used: HashSet<PathBuf> = HashSet::new();
    for s in summaries.iter() {
        for m in s.terraform.modules.iter().filter(|m| m.inputs.iter().any(|(n, _)| named(n)) || m.outputs.iter().any(|u| named(&u.name))) {
            used.extend(tf.link(&m.source).into_iter().map(|l| l.repo).filter(|r| *r != s.root));
        }
        for u in s.ansible.uses.iter().filter(|u| named(&u.name)) {
            used.extend(ansible.role(&s.root, &u.name, &u.file).into_iter().map(|l| l.repo).filter(|r| *r != s.root));
        }
    }
    let roles = kinds.is_empty() || kinds.contains(&"role");
    // Every repo's change was checked above.
    workspace_roots(roots, true, |s| {
        changed.contains(&s.root)
            || (s.light && (roles || s.may_define(name)))
            || s.may_reference_query(name)
            || (s.may_define(name) && depended.contains(s.root.to_string_lossy().as_ref()))
            || used.contains(&s.root)
            || (roles && s.ansible.roles.iter().any(|r| r.name == bare || r.dir.rsplit('/').next() == Some(bare)))
    })
}

/// An open, freshness-checked repo.
pub struct Opened {
    pub root: PathBuf,
    pub store: Store,
    pub head: Head,
    pub sync: Option<SyncReport>,
    pub refresh_pending: bool,
    /// The index generation the answer reads (see [`kc_store::Store::generation`]): the store's reads are pinned to
    /// it for the rest of the query (see [`kc_store::Store::begin_snapshot`]).
    pub generation: String,
    /// Other repos opened on its behalf (dependencies, dependents, repos crossed), for the answer's `provenance`.
    pub consulted: Consulted,
}

/// Opens `root`'s index, syncs it unless `no_sync`, and pins the query's reads to the state the sync left: every
/// later read sees that one generation, whatever another process reindexes meanwhile.
pub fn open_fresh(root: &Path, no_sync: bool) -> Result<Opened> {
    let (store, sync) = crate::refresh::open(root, no_sync)?;
    store.begin_snapshot()?;
    let generation = store.generation()?;
    let head = kc_index::head(root, &store);
    let refresh_pending = !no_sync && sync.is_none();
    Ok(Opened { root: root.to_path_buf(), store, head, sync, refresh_pending, generation, consulted: Consulted::default() })
}

/// [`open_fresh`] for `root`, one of the `roots` a query fans out over: when there are several, a repo that can't be
/// read is left out and named in the answer (see [`left_out`]) instead of failing it. A query for one repo fails with
/// its error, as does a cancelled query.
pub fn open_member(roots: &[PathBuf], root: &Path, no_sync: bool) -> Result<Option<Opened>> {
    match open_fresh(root, no_sync) {
        Ok(o) => Ok(Some(o)),
        Err(e) => left_out(roots, root, e).map(|()| None),
    }
}

/// Records that the answer leaves out the repo at `root`, one of the `roots` it fans out over, because reading it
/// failed with `e`: `missing_repo` when its checkout is gone (deleted after the workspace was listed), `failed`
/// otherwise, each naming the repo and the error. The query's context keeps it (see
/// [`kc_store::limits::Operation::leave_out`]) and the envelope says so (see [`add_missing`]). Returns `e` when there
/// is no other repo to answer with, or the query was cancelled.
pub fn left_out(roots: &[PathBuf], root: &Path, e: anyhow::Error) -> Result<()> {
    let query = kc_store::limits::current();
    if roots.len() < 2 || query.is_cancelled() {
        return Err(e);
    }
    query.leave_out(unreadable(root, &e));
    Ok(())
}

/// The `incomplete` entry for a repo left out of an answer because reading it failed with `e` (see [`left_out`]).
pub fn unreadable(root: &Path, e: &anyhow::Error) -> Incomplete {
    let name = repo_name(root);
    if !root.join(".git").exists() {
        Incomplete::new(Reason::MissingRepo, format!("{name}: its checkout at {} is gone; left out of this answer ({e:#})", root.display()))
            .with_hint("clone it there again, or `knowcode prune` to forget its index")
    } else {
        Incomplete::new(Reason::Failed, format!("{name}: could not be read at {}; left out of this answer: {e:#}", root.display()))
            .with_hint("the other repos answered; fix the error and ask again")
    }
}

impl Opened {
    /// The provenance of this repo's reading.
    pub fn provenance(&self) -> Provenance {
        Provenance {
            repo: self.root.to_string_lossy().into_owned(),
            commit: self.head.commit.clone(),
            generation: Some(self.generation.clone()),
            dirty: Some(self.head.dirty),
            fresh: self.sync.as_ref().is_some_and(|s| s.failed.is_empty()),
        }
    }
}

/// The repos an answer read on the side, recorded as they are opened: an answer lists every repo it consulted, not
/// just the ones it answers for. Interior mutability, since the repos are opened from code holding the queried repo
/// shared.
#[derive(Default)]
pub struct Consulted {
    provenance: std::cell::RefCell<Vec<Provenance>>,
    missing: std::cell::RefCell<Vec<String>>,
    pending: std::cell::RefCell<Vec<PathBuf>>,
    partial: std::cell::RefCell<Vec<Incomplete>>,
    withheld: std::cell::RefCell<Vec<PathBuf>>,
}

impl Consulted {
    /// Records a repo opened (and synced unless `no_sync`) on the side. Its HEAD is read from `.git` (no `git`
    /// process); whether it is dirty is not checked (`git status`, or even finding its cached answer, costs more than a
    /// side repo's part in the answer: ~1.4 M instructions each, measured on a reference answer opening 30).
    pub fn record(&self, root: &Path, store: &Store, synced: Option<&SyncReport>) {
        let repo = root.to_string_lossy().into_owned();
        let mut list = self.provenance.borrow_mut();
        if list.iter().any(|p| p.repo == repo && (p.fresh || synced.is_none())) {
            return;
        }
        list.retain(|p| p.repo != repo);
        list.push(Provenance {
            repo,
            commit: kc_index::head_commit(root),
            generation: store.generation().ok(),
            dirty: None,
            fresh: synced.is_some_and(|s| s.failed.is_empty()),
        });
    }

    pub fn list(&self) -> Vec<Provenance> {
        self.provenance.borrow().clone()
    }

    /// Records a repo the answer needed but could not open: indexed once, its checkout is gone.
    pub fn missing(&self, root: &Path) {
        let mut m = self.missing.borrow_mut();
        let r = root.to_string_lossy().into_owned();
        if !m.contains(&r) {
            m.push(r);
        }
    }

    /// The `missing_repo` entries for the repos [`Consulted::missing`] recorded.
    pub fn incomplete(&self) -> Vec<Incomplete> {
        let mut entries: Vec<_> = self.missing
            .borrow()
            .iter()
            .map(|r| {
                Incomplete::new(Reason::MissingRepo, format!("{}: indexed, but its checkout at {r} is gone; references into it are not resolved", repo_name(Path::new(r))))
                    .with_hint("clone it there again, or `knowcode purge` and re-index")
            })
            .collect();
        entries.extend(self.pending.borrow().iter().map(|root| crate::refresh::pending(root)));
        entries.extend(self.partial.borrow().iter().cloned());
        entries
    }
}

#[derive(Serialize)]
struct Freshness {
    reparsed: usize,
    removed: usize,
    check_ms: f64,
    /// Files whose read failed: their facts are as last indexed (none, for a new file). Items drawn from them have
    /// `stale: true`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failed: Vec<String>,
    /// `incomplete` when a file could not be read; absent when the index matches the working tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'static str>,
}

/// Marks `stale: true` on every object in `v` whose `file` is one of `failed` (repo name → paths), in the repo its
/// `repo` names, or in any repo when it names none.
fn mark_stale(v: &mut Value, failed: &[(String, HashSet<&str>)]) {
    match v {
        Value::Array(list) => list.iter_mut().for_each(|x| mark_stale(x, failed)),
        Value::Object(map) => {
            let repo = map.get("repo").and_then(Value::as_str).map(|r| Path::new(r).file_name().map_or(r.to_string(), |n| n.to_string_lossy().into_owned()));
            let hit = map.get("file").and_then(Value::as_str).is_some_and(|f| {
                failed.iter().any(|(r, paths)| repo.as_deref().is_none_or(|x| x == r) && paths.contains(f))
            });
            for x in map.values_mut() {
                mark_stale(x, failed);
            }
            if hit {
                map.insert("stale".into(), Value::Bool(true));
            }
        }
        _ => {}
    }
}

/// What an answer's envelope says beyond the answer, as lines for its text rendering: why it is incomplete (files
/// that could not be read, lists cut), and the other repos it consulted. See [`crate::contract::notes`].
pub fn freshness_notes(env: &Value) -> Vec<String> {
    crate::contract::notes(env)
}

pub use crate::contract::{finish, push_incomplete};


/// Says in the envelope which child processes (git commands, a search child) were stopped while answering it, at
/// their deadline (see [`kc_store::limits`]): an `incomplete` entry with reason `deadline` each. Values computed from
/// a cut child are not cached, so asking again (with a higher deadline) computes them again.
pub fn add_cuts(env: &mut Value) {
    add_notes(&kc_store::limits::current(), env);
}

/// What the query `query` collected on the side, said in its envelope: the cut children (see [`add_cuts`]), the repos
/// left out and the gone checkouts (see [`add_missing`]); each taken from it once.
pub fn add_notes(query: &kc_store::limits::Operation, env: &mut Value) {
    for cut in query.take_cuts() {
        if cut.reason == "deadline" {
            push_incomplete(env, Incomplete::new(Reason::Deadline, cut.detail).with_hint(cut.hint));
        }
    }
    add_missing_of(query, env);
}

/// Says in the envelope which repos the answer's cross-repo links left out because their checkout is gone, where
/// they would have mattered (see [`kc_index::workspace::note_missing`]): an `incomplete` entry with reason
/// `missing_repo` each, unless the envelope names the repo missing already. Also the repos a fan-out left out because
/// they could not be read (see [`left_out`]).
pub fn add_missing(env: &mut Value) {
    add_missing_of(&kc_store::limits::current(), env);
}

/// [`add_missing`] for the query `query`.
fn add_missing_of(query: &kc_store::limits::Operation, env: &mut Value) {
    for i in query.take_left_out() {
        push_incomplete(env, i);
    }
    for r in query.take_missing() {
        let path = r.to_string_lossy();
        if crate::contract::incomplete_of(env).iter().any(|i| i.reason == Reason::MissingRepo && i.detail.contains(path.as_ref())) {
            continue;
        }
        let detail = format!("{}: indexed, but its checkout at {path} is gone; links into it are left out", repo_name(&r));
        push_incomplete(env, Incomplete::new(Reason::MissingRepo, detail).with_hint("clone it there again, or `knowcode prune` to forget its index"));
    }
}

/// The response envelope from the spec: repo, commit, dirty, items, truncated, cursor.
pub fn envelope(opened: &[Opened], items: serde_json::Value, total: usize, shown: usize) -> serde_json::Value {
    workspace_envelope(opened, false, items, total, shown)
}

/// [`envelope`]; with `workspace`, the workspace's form whatever the number of repos opened (a workspace-wide query
/// opening only the repos that can answer it).
fn workspace_envelope(opened: &[Opened], workspace: bool, items: serde_json::Value, total: usize, shown: usize) -> serde_json::Value {
    let fresh = |o: &Opened| {
        if o.refresh_pending {
            return Some(Freshness { reparsed: 0, removed: 0, check_ms: 0.0, failed: Vec::new(), status: Some("indexing") });
        }
        o.sync.as_ref().map(|s| Freshness {
            reparsed: s.parsed,
            removed: s.removed,
            check_ms: round1(s.total_ms),
            failed: s.failed.clone(),
            status: (!s.failed.is_empty()).then_some("incomplete"),
        })
    };
    let mut env = if let (false, [one]) = (workspace, opened) {
        json!({
            "repo": one.root,
            "commit": one.head.commit,
            "dirty": one.head.dirty,
            "freshness": fresh(one),
        })
    } else {
        json!({
            "repo": "workspace",
            "commit": null,
            "dirty": opened.iter().any(|o| o.head.dirty),
            "repos": opened.iter().map(|o| json!({
                "repo": o.root, "commit": o.head.commit, "dirty": o.head.dirty, "freshness": fresh(o),
            })).collect::<Vec<_>>(),
        })
    };
    env["items"] = items;
    let failed: Vec<(String, HashSet<&str>)> = opened
        .iter()
        .filter_map(|o| o.sync.as_ref().filter(|s| !s.failed.is_empty()).map(|s| (repo_name(&o.root), s.failed.iter().map(String::as_str).collect())))
        .collect();
    if !failed.is_empty() {
        mark_stale(&mut env["items"], &failed);
    }
    env["total"] = json!(total);
    env["truncated"] = json!(shown < total);
    env["knowcode"] = build_info();
    // Every repo consulted: the ones answered for, then those read on their behalf.
    let mut provenance: Vec<Provenance> = opened.iter().map(Opened::provenance).collect();
    for p in opened.iter().flat_map(|o| o.consulted.list()) {
        if !provenance.iter().any(|q| q.repo == p.repo) {
            provenance.push(p);
        }
    }
    // Content the repos read (or, for a workspace-wide answer, any repo of the workspace) hold back by their AI
    // privacy files: the answer may lack it.
    let mut held: Vec<PathBuf> = provenance.iter().map(|p| PathBuf::from(&p.repo)).collect();
    // A repo blocked whole that an answered repo depends on: never opened (it declares nothing), so it is in no
    // provenance, but references into it, its conformers and its dependency edges are what the answer lacks.
    let blocked = blocked_repo_names().unwrap_or_default();
    if !blocked.is_empty() {
        for o in opened {
            for p in manifests::packages(o.store.conn()).unwrap_or_default() {
                for d in &p.deps {
                    if let Some((r, _)) = blocked_dependency(&blocked, &d.name, d.resolved_repo.as_deref()) {
                        held.push(r.clone());
                    }
                }
            }
        }
    }
    held.extend(opened.iter().flat_map(|o| o.consulted.withheld.borrow().clone()));
    let all = kc_index::indexed_roots().unwrap_or_default();
    if workspace || (opened.len() > 1 && opened.len() >= all.len()) {
        held.extend(kc_index::blocked_roots().unwrap_or_default().into_iter().map(|(r, _)| r));
    }
    env["provenance"] = json!(provenance);
    for o in opened.iter().filter(|o| o.refresh_pending) {
        push_incomplete(&mut env, crate::refresh::pending(&o.root));
    }
    for i in opened.iter().flat_map(|o| o.consulted.incomplete()) {
        push_incomplete(&mut env, i);
    }
    add_missing(&mut env);
    let mut seen = HashSet::new();
    for r in held.iter().filter(|r| seen.insert(r.to_path_buf())) {
        if let Some(i) = withheld(r) {
            push_incomplete(&mut env, i);
        }
    }
    finish(&mut env);
    env
}

/// Which KnowCode answered: its version, the git commit it was built from, and the index schema.
fn build_info() -> Value {
    json!({ "version": env!("CARGO_PKG_VERSION"), "build": env!("KNOWCODE_BUILD"), "schema": kc_store::SCHEMA_VERSION })
}

pub fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

pub fn repo_name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub struct FindData {
    pub items: Vec<(PathBuf, FindHit)>,
    pub total: usize,
    /// The candidate cap was reached: `total` is a lower bound.
    pub capped: bool,
    pub multi: bool,
    pub manifests: Vec<ManifestHit>,
}

/// Every client route (request contract) in the selected repos, with the actions serving it. Reads the workspace
/// summary: the selected repos are synced first, the others are as last synced.
pub fn routes(args: &Scope) -> Result<Out<Vec<(PathBuf, HttpOut)>>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    for root in &roots {
        opened.extend(open_member(&roots, root, args.no_sync)?);
    }
    let summaries = full_workspace(&opened, &roots)?;
    let mut out = Vec::new();
    for s in summaries.iter().filter(|s| roots.contains(&s.root)) {
        let link_out = |l: kc_index::http::RouteLink| HttpLinkOut {
            repo: repo_name(&l.repo),
            symbol: l.route.symbol,
            kind: l.route.kind,
            file: l.route.file,
            start_line: l.route.start_line,
            end_line: l.route.end_line,
            line: l.route.line,
            confidence: l.confidence,
            assumptions: l.assumptions,
            host: l.host,
            source: None,
            source_truncated: false,
        };
        for r in s.routes.iter().filter(|r| matches!(r.side.as_str(), "client" | "operation")) {
            let links: Vec<HttpLinkOut> = kc_index::http::handlers(r, &s.root, &summaries).into_iter().map(link_out).collect();
            let commented_out: Vec<HttpLinkOut> = if links.is_empty() && matches!(r.kind.as_str(), "class" | "struct") {
                kc_index::http::commented(r, &summaries).into_iter().map(link_out).collect()
            } else {
                Vec::new()
            };
            let operation = r.side == "operation";
            out.push((
                s.root.clone(),
                HttpOut {
                    role: "requests",
                    symbol: r.symbol.clone(),
                    verb: r.verb.clone(),
                    path: if operation { None } else { r.path.clone() },
                    operation: if operation { r.path.clone() } else { None },
                    service: r.service.clone(),
                    note: if links.is_empty() && commented_out.is_empty() { kc_index::http::unlinked_note(r, &summaries, true) } else { None },
                    links,
                    unsent: false,
                    commented_out,
                },
            ));
        }
    }
    let items: Vec<Value> = out
        .iter()
        .map(|(root, h)| {
            let mut v = json!(h);
            v["repo"] = json!(repo_name(root));
            v
        })
        .collect();
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    // Registrations the selected repos make whose routes can't be placed: what they serve, and links to it, are missing.
    for s in summaries.iter().filter(|s| roots.contains(&s.root)) {
        for i in unsupported_routes(s, roots.len() > 1) {
            push_incomplete(&mut env, i);
        }
    }
    Ok(Out { env, data: out })
}

/// An endpoint registration whose route KnowCode recognises but can't place (an `unsupported` route row, see
/// `kc_extract::HttpRoute`) as a construct met but not read, area `http`: its style and why.
fn unplaced_site(r: &query::RouteRow) -> kc_store::unsupported::Site {
    kc_store::unsupported::Site {
        area: "http".into(),
        construct: format!("{}: {}", r.verb, r.detail.as_deref().unwrap_or("not read")),
        file: r.file.clone(),
        line: r.line,
    }
}

/// The `unsupported` entry for a repo's endpoint registrations whose routes can't be placed.
pub(crate) fn unsupported_routes(s: &kc_index::workspace::RepoSummary, named: bool) -> Vec<Incomplete> {
    let sites: Vec<kc_store::unsupported::Site> = s.routes.iter().filter(|r| r.side == "unsupported").map(unplaced_site).collect();
    let repo = repo_name(&s.root);
    kc_store::unsupported::incomplete(&sites, named.then_some(repo.as_str()))
}

/// A SQL table, view, procedure, function or trigger defined in `.sql` files, as `tables <name>` lists it: what its
/// body reads, writes and calls, and the SQL reading, writing or calling it.
#[derive(Debug, Clone, Serialize)]
pub struct SqlObjectOut {
    pub kind: String,
    /// `schema.name`, or the name.
    pub name: String,
    pub file: String,
    pub line: u32,
    /// Further definitions of the same kind and name in the repo (a schema dumped in several places), `file:line`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub copies: Vec<String>,
    #[serde(flatten)]
    pub section: kc_store::sql::SqlSection,
    /// SQL reading or writing it (a table or view) or calling it (a routine), with the enclosing definition.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<SqlAccessOut>,
}

/// One SQL statement using a SQL object.
#[derive(Debug, Clone, Serialize)]
pub struct SqlAccessOut {
    /// `read`, `write`, `call` or `type_ref` (a trigger or index naming it).
    pub mode: String,
    /// The definition the statement is in (`schema.proc`), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub file: String,
    pub line: u32,
}

/// `tables`' answer: Entity Framework models and procedure calls per repo, and SQL objects named by the query.
pub struct TablesData {
    pub models: Vec<(PathBuf, DbModel, ModelSql)>,
    pub sql: Vec<(PathBuf, SqlObjectOut)>,
}

/// The SQL objects named `name` in a repo's `.sql` files, copies of one (same kind and `schema.name`) listed once.
fn sql_objects(o: &Opened, name: &str) -> Result<Vec<SqlObjectOut>> {
    let conn = o.store.conn();
    let defs: Vec<query::SymbolRow> = query::definitions(conn, name)?.into_iter().filter(kc_store::sql::is_object).collect();
    let mut groups: Vec<Vec<query::SymbolRow>> = Vec::new();
    for d in defs {
        let key = |s: &query::SymbolRow| (s.kind.clone(), s.qualified().to_lowercase());
        match groups.iter_mut().find(|g| key(&g[0]) == key(&d)) {
            Some(g) => g.push(d),
            None => groups.push(vec![d]),
        }
    }
    let mut out = Vec::new();
    for g in groups {
        let s = &g[0];
        let text = std::fs::read(o.root.join(&s.file)).ok();
        let text = text.as_deref().map(kc_extract::decode);
        let source = text.as_deref().and_then(|t| t.as_bytes().get(s.start_byte..s.end_byte.min(t.len()))).map(String::from_utf8_lossy).unwrap_or_default();
        let mut section = kc_store::sql::section(conn, s, &source)?;
        section.columns.clear();
        let kinds: &[&str] = kc_store::sql::relation_ref_kinds(&s.kind, &s.file).unwrap_or(&["call"]);
        let (hits, _) = query::refs(conn, &s.name, kinds, usize::MAX)?;
        let mut used_by: Vec<SqlAccessOut> = Vec::new();
        for h in hits.iter().filter(|h| g.iter().any(|d| h.may_target(d.id))) {
            let by = h.from.as_ref().map(|f| f.container.as_ref().map_or_else(|| f.name.clone(), |c| format!("{c}.{}", f.name)));
            if !used_by.iter().any(|u| u.file == h.file && u.line == h.line && u.mode == h.kind) {
                used_by.push(SqlAccessOut { mode: h.kind.clone(), by, file: h.file.clone(), line: h.line });
            }
        }
        // Writes, then reads and calls, then mere mentions.
        used_by.sort_by_key(|u| (match u.mode.as_str() { "write" => 0, "read" | "call" => 1, _ => 2 }, u.file.clone(), u.line));
        out.push(SqlObjectOut {
            kind: s.kind.clone(),
            name: s.qualified(),
            file: s.file.clone(),
            line: s.line,
            copies: g[1..].iter().map(|d| format!("{}:{}", d.file, d.line)).collect(),
            section,
            used_by,
        });
    }
    Ok(out)
}

/// Entity Framework tables per DbContext in the selected repos, with their accessors, and stored procedure calls.
/// With `name`, only tables whose name, entity or DbSet is `name`, and procedures with that name, and the SQL tables,
/// views and routines of that name the repos' `.sql` files define (`sql`).
/// A table as answers show it: without row ids (the DbSet's, the entity's, each accessor's), which differ between two
/// indexes of the same code; the DbSet, entity and accessors are named, with `file:line`.
fn table_json(t: &db::Table) -> Value {
    let mut v = json!(t);
    if let Some(o) = v.as_object_mut() {
        o.remove("dbset_id");
        o.remove("entity_id");
        if let Some(accessors) = o.get_mut("accessors").and_then(Value::as_array_mut) {
            for a in accessors.iter_mut().filter_map(Value::as_object_mut) {
                a.remove("id");
            }
        }
    }
    v
}

#[cfg(test)]
pub fn tables(name: Option<&str>, args: &Scope) -> Result<Out<TablesData>> {
    tables_with(name, args, false)
}

/// [`tables`], keeping every caller listed only for a write through a callee when `all_callers` (by default test
/// callers are counted, not listed, and others capped per callee: see [`db::compact`]).
pub fn tables_with(name: Option<&str>, args: &Scope, all_callers: bool) -> Result<Out<TablesData>> {
    let _memo = kc_index::memo_changes();
    let (mut roots, workspace) = scope_repos(&args.repos)?;
    if let (true, Some(n)) = (workspace, name) {
        roots = workspace_roots(roots, args.no_sync, |s| s.may_name_table(n))?;
    }
    let mut opened = Vec::new();
    let mut models = Vec::new();
    let mut sql_hits: Vec<(PathBuf, SqlObjectOut)> = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        if let Some(n) = name {
            sql_hits.extend(sql_objects(&o, n)?.into_iter().map(|s| (o.root.clone(), s)));
        }
        let full = o.store.db_model()?;
        let mut model = full.clone();
        if let Some(n) = name {
            let named = |p: &db::Procedure| p.name.eq_ignore_ascii_case(n) || p.name.rsplit('.').next().is_some_and(|x| x.eq_ignore_ascii_case(n));
            let keep: Vec<_> = model.named(n).into_iter().cloned().collect();
            model.tables = keep;
            if !all_callers {
                for t in &mut model.tables {
                    db::compact(t, db::VIA_ROWS);
                }
            }
            model.procedures.retain(named);
            model.wrapped.retain(named);
        }
        if !model.is_empty() || !model.wrapped.is_empty() {
            models.push((o.root.clone(), model, full));
        }
        opened.push(o);
    }
    let summaries = if models.is_empty() { Default::default() } else { full_workspace(&opened, &roots)? };
    let out: Vec<(PathBuf, DbModel, ModelSql)> = models
        .into_iter()
        .filter_map(|(root, mut m, full)| {
            let sql = kc_index::sql::Sql::new(&summaries, Some(&root));
            known_wrapped(&sql, &root, &mut m);
            let links = model_sql(&sql, &root, &m, &full);
            (!m.is_empty()).then_some((root, m, links))
        })
        .collect();
    let items: Vec<Value> = out
        .iter()
        .flat_map(|(root, m, sql)| {
            m.tables.iter().zip(&sql.tables).map(move |(t, links)| {
                let mut v = table_json(t);
                v["repo"] = json!(repo_name(root));
                // Without a name, counts per table; the accessors themselves with one.
                if name.is_none() {
                    v.as_object_mut().map(|o| o.remove("accessors"));
                }
                links.annotate(&mut v);
                v
            })
        })
        .collect();
    let procedures: Vec<Value> = out
        .iter()
        .flat_map(|(root, m, sql)| {
            m.procedures.iter().zip(&sql.procedures).map(move |(p, links)| {
                let mut v = json!(p);
                // A row id differs between two indexes of the same code; the symbol and its `file:line` don't.
                v.as_object_mut().map(|o| o.remove("id"));
                v["repo"] = json!(repo_name(root));
                if !links.is_empty() {
                    v["definition"] = json!(links);
                }
                v
            })
        })
        .collect();
    let n = items.len();
    let mut env = workspace_envelope(&opened, workspace, json!(items), n, n);
    if !procedures.is_empty() {
        env["procedures"] = json!(procedures);
    }
    if !sql_hits.is_empty() {
        env["sql"] = json!(sql_hits
            .iter()
            .map(|(root, s)| {
                let mut v = json!(s);
                v["repo"] = json!(repo_name(root));
                v
            })
            .collect::<Vec<_>>());
    }
    finish(&mut env);
    Ok(Out { env, data: TablesData { models: out, sql: sql_hits } })
}

/// A SQL definition that code links to: the `CREATE TABLE` of an Entity Framework table, the `CREATE PROCEDURE` a call
/// runs.
#[derive(Debug, Clone, Serialize)]
pub struct SqlLinkOut {
    pub repo: String,
    pub kind: String,
    /// `schema.name`, or the name.
    pub name: String,
    pub file: String,
    pub line: u32,
    /// Further copies of the definition in that repo (one schema dumped in several places).
    #[serde(skip_serializing_if = "is_zero")]
    pub copies: u32,
    /// `exact`, `unique_name`, `dominant` or `ambiguous` (see `kc_index::sql`).
    pub confidence: &'static str,
    /// What picked the schema among several (see `kc_index::sql::SqlLink::by`).
    #[serde(skip_serializing_if = "str::is_empty")]
    pub by: &'static str,
    /// What the link did not check (see `kc_store::answer::Assumption`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
}

/// The database a table of code is in, as `tables` and `context` show it (`shop.invoice`).
#[derive(Debug, Clone, Serialize)]
pub struct DatabaseOut {
    /// The database (schema), lower-cased; `None` when it is ambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `exact`, `unique_name`, `dominant` or `ambiguous`.
    pub confidence: &'static str,
    /// What decided it: `schema` (the mapping or SQL text names it), `connection` (the context's connection setting),
    /// `context` (the context's name), `context_tables`, `repo` (the repo's own SQL files), `repo_tables` (the database
    /// defining most of the repo's tables), `only` (the one database defining the name); empty when ambiguous.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub by: &'static str,
    /// The connection setting, when the context names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// Other databases defining a table of the same name: a different table, not this one (all candidates when
    /// ambiguous).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub same_name_in: Vec<String>,
}

/// A table's SQL definitions and database.
#[derive(Debug, Clone, Default)]
pub struct TableSql {
    pub links: Vec<SqlLinkOut>,
    pub database: Option<DatabaseOut>,
}

impl TableSql {
    /// Adds `sql` and `database` to a table's JSON.
    fn annotate(&self, v: &mut Value) {
        if !self.links.is_empty() {
            v["sql"] = json!(self.links);
        }
        if let Some(d) = &self.database {
            v["database"] = json!(d);
        }
    }

    /// The table's name with its database: `shop.invoice`, or as mapped when the database is unknown or already
    /// named.
    pub fn qualified(&self, table: &str) -> String {
        match self.database.as_ref().and_then(|d| d.name.as_deref()) {
            Some(db) if kc_store::sql::split_name(table).0.is_none() => format!("{db}.{table}"),
            _ => table.to_string(),
        }
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl From<kc_index::sql::SqlLink> for SqlLinkOut {
    fn from(l: kc_index::sql::SqlLink) -> SqlLinkOut {
        let assumptions = l.assumptions();
        SqlLinkOut {
            repo: repo_name(&l.repo),
            kind: l.def.kind.clone(),
            name: l.def.qualified(),
            file: l.def.file,
            line: l.def.line,
            copies: l.def.copies,
            confidence: l.confidence,
            assumptions,
            by: l.by,
        }
    }
}

/// The SQL definitions of a model's tables and called procedures, in the model's order.
#[derive(Debug, Clone, Default)]
pub struct ModelSql {
    pub tables: Vec<TableSql>,
    pub procedures: Vec<Vec<SqlLinkOut>>,
}

fn sql_links(sql: &kc_index::sql::Sql, root: &Path, written: &str, kinds: &[&str]) -> Vec<SqlLinkOut> {
    sql.definitions(root, written, kinds).into_iter().map(SqlLinkOut::from).collect()
}

/// The definitions a table of code links to, picked among schemas by its DbContext in `model` (the repo's whole
/// model), and the database it is in: the schema its mapping or SQL text names, else the one its definitions are in
/// (one schema picked), else a literal connection string's; `ambiguous` among several.
fn table_links(sql: &kc_index::sql::Sql, root: &Path, t: &db::Table, model: &DbModel) -> TableSql {
    let context = kc_index::sql::DbContextTables::of(model, t);
    let links: Vec<SqlLinkOut> =
        sql.definitions_in(root, &t.table, kc_index::sql::TABLE_KINDS, context.as_ref()).into_iter().map(SqlLinkOut::from).collect();
    let mut schemas: Vec<Option<String>> = links.iter().map(|l| kc_store::sql::split_name(&l.name).0).collect();
    schemas.sort();
    schemas.dedup();
    let (named, _) = kc_store::sql::split_name(&t.table);
    let literal = t.connection.as_deref().and_then(|c| c.strip_prefix("Database=")).map(str::to_ascii_lowercase);
    let elsewhere = || sql.schemas_named(&t.table, kc_index::sql::TABLE_KINDS).into_iter().flatten().collect::<Vec<String>>();
    let (name, confidence, by) = match (&named, links.first(), schemas.as_slice()) {
        (Some(s), _, _) => (Some(s.clone()), "exact", "schema"),
        (None, Some(l), [Some(s)]) if l.confidence != "ambiguous" => (Some(s.clone()), l.confidence, l.by),
        // The repo's own SQL defines it without naming a database (a migration): the database the connection
        // names, else the one database elsewhere defining the name, else unnamed.
        (None, Some(l), [None]) if l.confidence != "ambiguous" => {
            let others = elsewhere();
            let conn = t.connection.as_deref().and_then(db::connection_database);
            let by_conn: Vec<&String> = others.iter().filter(|s| conn.as_deref().is_some_and(|c| db::same_database(c, s))).collect();
            match (by_conn.as_slice(), others.as_slice()) {
                ([one], _) => (Some((*one).clone()), "dominant", "connection"),
                (_, [one]) => (Some(one.clone()), "unique_name", "only"),
                _ => (None, l.confidence, "repo"),
            }
        }
        (None, Some(_), _) => (None, "ambiguous", ""),
        (None, None, _) => match literal {
            Some(db) => (Some(db), "exact", "connection"),
            None => return TableSql { links, database: None },
        },
    };
    let same_name_in: Vec<String> = elsewhere().into_iter().filter(|s| Some(s) != name.as_ref()).collect();
    if name.is_none() && same_name_in.is_empty() {
        return TableSql { links, database: None };
    }
    TableSql { links, database: Some(DatabaseOut { name, confidence, by, connection: t.connection.clone(), same_name_in }) }
}

/// The SQL definitions of `m`'s tables and called procedures, in its order; `full` is the repo's whole model (`m`
/// may be a selection of it).
fn model_sql(sql: &kc_index::sql::Sql, root: &Path, m: &DbModel, full: &DbModel) -> ModelSql {
    ModelSql {
        tables: m.tables.iter().map(|t| table_links(sql, root, t, full)).collect(),
        procedures: m.procedures.iter().map(|p| sql_links(sql, root, &p.name, kc_index::sql::ROUTINE_KINDS)).collect(),
    }
}

/// Moves the names `m` passes to procedure-running wrappers that a procedure is defined for into its procedure calls;
/// the rest are dropped.
fn known_wrapped(sql: &kc_index::sql::Sql, root: &Path, m: &mut DbModel) {
    for p in std::mem::take(&mut m.wrapped) {
        if !sql.definitions(root, &p.name, kc_index::sql::ROUTINE_KINDS).is_empty()
            && !m.procedures.iter().any(|q| q.id == p.id && q.name.eq_ignore_ascii_case(&p.name))
        {
            m.procedures.push(p);
        }
    }
}

/// Code using a SQL table, view or routine, in any indexed repo: an Entity Framework table mapped to it, or a call
/// running it.
#[derive(Debug, Clone, Serialize)]
pub struct SqlUseOut {
    pub repo: String,
    /// `entity` (an Entity Framework table mapped to it) or `call` (a stored procedure call).
    pub how: &'static str,
    /// The entity class, or the method making the call.
    pub symbol: String,
    /// The DbContext declaring the entity's set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// The table as the code maps it, or the procedure as the call names it.
    pub name: String,
    pub file: String,
    pub line: u32,
    /// How sure the link from that name to this definition is (see `kc_index::sql`).
    pub confidence: &'static str,
    /// For an entity: how many places read and write its table, and the first few.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reads: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writes: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub accessors: Vec<db::Access>,
    /// How many accessors there are, when fewer are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessors_total: Option<usize>,
}

/// What `context` says about a symbol's SQL side: for a SQL table, view or routine, its columns, what it reads,
/// writes and calls, and the code using it; for code, the definitions of the tables and procedures it touches.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SqlFacts {
    #[serde(flatten)]
    pub section: kc_store::sql::SqlSection,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<SqlUseOut>,
    /// Definitions and databases of the context's `tables`, in order.
    #[serde(skip)]
    pub tables: Vec<TableSql>,
    /// Definitions of the procedures among the context's `touches_tables`, by procedure name.
    #[serde(skip)]
    pub procedures: Vec<(String, Vec<SqlLinkOut>)>,
}

impl SqlFacts {
    fn is_empty(&self) -> bool {
        self.section.is_empty() && self.used_by.is_empty()
    }
}

fn sql_facts(o: &Opened, ctx: &Context, tables: &SymbolTables, max_accessors: usize, no_sync: bool) -> Result<SqlFacts> {
    let s = &ctx.symbol;
    let mut out = SqlFacts::default();
    if kc_store::sql::is_sql_path(&s.file) {
        let text = std::fs::read(o.root.join(&s.file)).ok();
        let text = text.as_deref().map(kc_extract::decode);
        let source = text.as_deref().and_then(|t| t.as_bytes().get(s.start_byte..s.end_byte.min(t.len()))).map(String::from_utf8_lossy).unwrap_or_default();
        out.section = kc_store::sql::section(o.store.conn(), s, &source)?;
        if kc_store::sql::is_object(s) {
            let summaries = full_summaries(o)?;
            let mut crossing = HttpCrossing::new(o, no_sync);
            for u in crossing.sql_uses(&o.root, s, &summaries)? {
                out.used_by.push(u.out(max_accessors));
            }
        }
        return Ok(out);
    }
    let procedures: Vec<&str> = tables.touches.iter().filter(|t| t.kind == "procedure").map(|t| t.name.as_str()).collect();
    if tables.tables.is_empty() && procedures.is_empty() {
        return Ok(out);
    }
    let summaries = full_summaries(o)?;
    let sql = kc_index::sql::Sql::new(&summaries, Some(&o.root));
    let model = o.store.db_model()?;
    out.tables = tables.tables.iter().map(|t| table_links(&sql, &o.root, t, &model)).collect();
    for p in procedures {
        if !out.procedures.iter().any(|(n, _)| n == p) {
            out.procedures.push((p.to_string(), sql_links(&sql, &o.root, p, kc_index::sql::ROUTINE_KINDS)));
        }
    }
    Ok(out)
}

/// The symbols enclosing `line` of `file`, innermost first. An absolute `file` picks its repo; otherwise it is
/// relative to the one repo in scope.
pub fn at(file: &str, line: u32, args: &Scope) -> Result<Out<Vec<query::SymbolAt>>> {
    let path = Path::new(file);
    let root = if path.is_absolute() && args.repos.is_empty() {
        target_repos(&[path.parent().unwrap_or(path).to_path_buf()])?.remove(0)
    } else {
        match target_repos(&args.repos)?.as_slice() {
            [one] => one.clone(),
            _ => bail!("`at` answers for one repo: pass --repo, or an absolute file path"),
        }
    };
    let rel = if path.is_absolute() {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        abs.strip_prefix(&root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| file.to_string())
    } else {
        file.trim_start_matches("./").to_string()
    };
    let o = open_fresh(&root, args.no_sync)?;
    let chain = query::symbols_at(o.store.conn(), &rel, line)?;
    let indexed: bool = o.store.conn().query_row("SELECT EXISTS (SELECT 1 FROM files WHERE path = ?1)", [&rel], |r| r.get(0))?;
    let n = chain.len();
    let mut env = envelope(std::slice::from_ref(&o), json!(chain), n, n);
    env["file"] = json!(rel);
    env["line"] = json!(line);
    // False when the file has no symbol index (an unsupported language, ignored, or not in the repo).
    env["file_indexed"] = json!(indexed);
    finish(&mut env);
    Ok(Out { env, data: chain })
}

pub fn find(q: &str, kind: Option<&str>, limit: usize, args: &Scope) -> Result<Out<FindData>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut items = Vec::new();
    let mut total = 0;
    let mut capped = false;
    let mut manifest_hits = Vec::new();
    let want_manifests = kind.is_none_or(|k| matches!(k, "package" | "target" | "product"));
    let mut manifests_total = 0;
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        if kind.is_none_or(|k| !matches!(k, "package" | "target" | "product")) {
            // Ties go by path and line, not by a rank: the rank was only used when cached, so the order depended on
            // cache state, and ranking ties didn't help (concept-find on replayed commits: 0 better, 0 worse).
            let found = query::find_counted(o.store.conn(), q, kind, limit, &kc_store::rank::FileRank::default())?;
            total += found.total;
            capped |= found.capped;
            items.extend(found.hits.into_iter().map(|h| (o.root.clone(), h)));
        }
        if want_manifests {
            let (hits, n) = manifests::find(o.store.conn(), q, 10)?;
            manifests_total += n;
            manifest_hits.extend(hits);
        }
        opened.push(o);
    }
    // Across repos, keep each repo's ranking but interleave by match quality.
    if roots.len() > 1 {
        let rank = |m: &str| match m {
            "exact" => 0,
            "prefix" => 1,
            _ => 2,
        };
        items.sort_by_key(|(_, h)| rank(h.matched));
    }
    items.truncate(limit);
    let multi = roots.len() > 1;
    let list: Vec<_> = items.iter().map(|(root, h)| tag_repo(h, root, multi)).collect();
    let mut env = envelope(&opened, json!(list), total, items.len());
    // Past the candidate cap, the total is a lower bound and a match may be missing: never a complete answer.
    if capped {
        env["truncated"] = json!(true);
        env["capped"] = json!(true);
        env["hint"] = json!(format!(
            "more than {} definitions match; total counts those read. Narrow it: Outer.name, a longer name, or kind",
            query::FIND_CANDIDATES
        ));
    }
    if !manifest_hits.is_empty() {
        env["manifests"] = json!(manifest_hits);
    }
    if manifests_total > manifest_hits.len() {
        env["manifests_total"] = json!(manifests_total);
    }
    finish(&mut env);
    Ok(Out { env, data: FindData { items, total, capped, multi, manifests: manifest_hits } })
}

pub struct LocateData {
    pub items: Vec<(PathBuf, query_locate::LocateHit)>,
    pub total: usize,
    pub multi: bool,
    pub terms: Vec<String>,
}

/// Files ranked for a plain-language query by BM25 over their entities' words (path, symbol names, doc comments,
/// referenced names). Across repos, by score.
pub fn locate(text: &str, limit: usize, args: &Scope) -> Result<Out<LocateData>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut items = Vec::new();
    let mut total = 0;
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        // Ties keep path order: breaking them by PageRank (the more central file first) lowered concept-search
        // recall on replayed commits (recall@10 -0.8 pts [-1.5, -0.2], 0 tasks better and 5 worse of 189).
        let texts = kc_index::textdocs::docs(root, &o.store)?;
        let history = vitals::subject_words(root, &o.store)?;
        let extra = query_locate::Extra { texts: &texts, history: Some(&history) };
        let (hits, n) = query_locate::locate_with(o.store.conn(), text, limit, &extra)?;
        total += n;
        items.extend(hits.into_iter().map(|h| (o.root.clone(), h)));
        opened.push(o);
    }
    if roots.len() > 1 {
        items.sort_by(|(ra, a), (rb, b)| b.score.total_cmp(&a.score).then_with(|| (ra, &a.file).cmp(&(rb, &b.file))));
    }
    items.truncate(limit);
    let multi = roots.len() > 1;
    let terms = query_locate::query_terms(text);
    let list: Vec<_> = items.iter().map(|(root, h)| tag_repo(h, root, multi)).collect();
    let mut env = envelope(&opened, json!(list), total, items.len());
    env["terms"] = json!(terms);
    finish(&mut env);
    Ok(Out { env, data: LocateData { items, total, multi, terms } })
}

fn tag_repo<T: Serialize>(item: &T, root: &Path, multi: bool) -> Value {
    let mut v = serde_json::to_value(item).unwrap_or(Value::Null);
    if multi {
        v["repo"] = json!(repo_name(root));
    }
    v
}

/// Orders a long list of references by confidence, then within a confidence by the rank of each one's file,
/// personalized toward the files defining the name (references from other repos last). Short lists keep file order.
/// Returns the generation of the graph ranked when it is an earlier one than the index's.
fn order_refs(store: &Store, name: &str, hits: &mut [RefHit]) -> Result<Option<String>> {
    if hits.len() <= LONG_LIST {
        return Ok(None);
    }
    let seeds: Vec<String> = query::definitions(store.conn(), name)?.into_iter().map(|d| d.file).collect();
    let seeds: Vec<&str> = seeds.iter().map(String::as_str).collect();
    let (rank, stale) = store.file_rank_at(&seeds, kc_store::rank::Flow::UsedBy, true)?;
    let conf = |c: &str| ["exact", "unique_name", "ambiguous", "external_likely", "unresolved"].iter().position(|x| *x == c).unwrap_or(5);
    let score = |h: &RefHit| if h.repo.is_some() { 0.0 } else { rank.score(&h.file) };
    hits.sort_by(|a, b| conf(a.confidence).cmp(&conf(b.confidence)).then_with(|| score(b).total_cmp(&score(a))));
    Ok(stale)
}

pub struct RefsData {
    pub items: Vec<(PathBuf, RefHit)>,
    pub total: usize,
    pub by_confidence: BTreeMap<&'static str, usize>,
    pub multi: bool,
}

pub fn refs(name: &str, kinds: &[String], limit: usize, args: &Scope) -> Result<Out<RefsData>> {
    let (roots, workspace) = scope_repos(&args.repos)?;
    let mut out = refs_in(name, kinds, limit, roots, workspace, args.no_sync)?;
    let more = out.data.total > out.data.items.len();
    let req = cursor::Request::Refs { name: name.to_string(), kinds: kinds.to_vec(), limit };
    cursor::offer(&mut out.env, req, args, vec![cursor::List::new("/items", limit, more)], &[]);
    finish(&mut out.env);
    Ok(out)
}

/// [`refs`] over `roots`; `workspace` when they are every indexed repo (the query names no repo and runs outside one).
fn refs_in(name: &str, kinds: &[String], limit: usize, mut roots: Vec<PathBuf>, workspace: bool, no_sync: bool) -> Result<Out<RefsData>> {
    // Each repo's change since its summary is checked once for the whole query.
    let _memo = kc_index::memo_changes();
    let args = &Scope { repos: Vec::new(), no_sync };
    let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    let multi = roots.len() > 1;
    if workspace {
        roots = refs_roots(roots, name, &kinds, no_sync)?;
    }
    let mut opened = Vec::new();
    let mut items = Vec::new();
    let mut total = 0;
    let mut by_conf = BTreeMap::new();
    let (mut precise, mut corrected) = (0, 0);
    let mut stale_rank: Vec<Incomplete> = Vec::new();
    let mut unread_imports: Vec<Incomplete> = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let (_, bare) = query::split_dotted(name.trim());
        let deps = open_dependencies(&o, Some(bare), args.no_sync)?;
        let dep_refs = dependency_refs(&deps);
        let (mut hits, mut summary) = query::refs_with(o.store.conn(), name, &kinds, usize::MAX, &dep_refs)?;
        // Uses of a role another repo defines, linked to it.
        if link_role_uses(&o, &mut hits, args.no_sync)? {
            summary.by_confidence = BTreeMap::new();
            for h in &hits {
                *summary.by_confidence.entry(h.confidence).or_default() += 1;
            }
        }
        // References from repos that depend on this one, resolved into it.
        for h in dependent_refs(&o, name, &kinds, args.no_sync)? {
            *summary.by_confidence.entry(h.confidence).or_default() += 1;
            summary.total += 1;
            hits.push(h);
        }
        // An Ansible role used by plays and roles in other repos.
        if kinds.is_empty() || kinds.contains(&"role") {
            for h in role_refs_elsewhere(&o, name.trim(), args.no_sync)? {
                *summary.by_confidence.entry(h.confidence).or_default() += 1;
                summary.total += 1;
                hits.push(h);
            }
        }
        // A Terraform module's variable or output, set or read by module calls in other repos.
        if kinds.is_empty() || kinds.iter().any(|k| matches!(*k, "tf_input" | "tf_output")) {
            for h in crate::tf::module_refs_elsewhere(&o, name.trim(), args.no_sync)? {
                if !kinds.is_empty() && !kinds.contains(&h.kind.as_str()) {
                    continue;
                }
                *summary.by_confidence.entry(h.confidence).or_default() += 1;
                summary.total += 1;
                hits.push(h);
            }
        }
        if let Some(g) = order_refs(&o.store, name, &mut hits)? {
            stale_rank.push(kc_store::answer::stale_derived(&format!("order of references ({}): file ranking", repo_name(&o.root)), &g, &o.generation));
        }
        if !multi {
            hits.truncate(limit);
        }
        total += summary.total;
        precise += summary.precise;
        corrected += summary.corrected;
        for (k, v) in summary.by_confidence {
            *by_conf.entry(k).or_insert(0) += v;
        }
        // C# projects whose imports can't be read: a reference in them may be ambiguous, or unresolved, for it.
        if hits.iter().any(|h| h.file.ends_with(".cs")) {
            // Only the projects holding a reference of the name: another project's imports don't bear on it.
            let dir = |f: &str| f.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
            let holds = |project: &str| hits.iter().any(|h| project.is_empty() || h.file.starts_with(&format!("{project}/")));
            let sites: Vec<_> = kc_store::unsupported::in_area(o.store.conn(), "imports")?.into_iter().filter(|u| holds(&dir(&u.file))).collect();
            let label = multi.then(|| repo_name(&o.root));
            unread_imports.extend(kc_store::unsupported::incomplete(&sites, label.as_deref()));
        }
        // C# used from Razor markup, which isn't read: references there aren't listed.
        let csharp = hits.iter().any(|h| h.file.ends_with(".cs")) || query::definitions(o.store.conn(), name)?.iter().any(|d| d.file.ends_with(".cs"));
        if csharp {
            unread_imports.extend(razor_unread(&o, multi)?);
        }
        items.extend(hits.into_iter().map(|h| (o.root.clone(), h)));
        opened.push(o);
    }
    if multi {
        items = dedupe_refs(items);
        total = items.len();
        by_conf = BTreeMap::new();
        for (_, h) in &items {
            *by_conf.entry(h.confidence).or_insert(0) += 1;
        }
    }
    items.truncate(limit);
    let mut citer = crate::cite::Citer::new([]).with_stores(opened.iter().map(|o: &Opened| (o.root.clone(), &o.store)));
    for (root, h) in &mut items {
        citer.refs(root, std::iter::once(h));
    }
    let cite_notes = citer.incomplete();
    // A hit from another repo (a dependent, a play using a role, a module call) keeps that repo.
    let list: Vec<_> = items
        .iter()
        .map(|(root, h)| {
            let mut v = tag_repo(h, root, multi);
            if let Some(r) = &h.repo {
                v["repo"] = json!(r);
            }
            v
        })
        .collect();
    let mut env = workspace_envelope(&opened, workspace, json!(list), total, items.len());
    for i in cite_notes {
        push_incomplete(&mut env, i);
    }
    for i in unread_imports {
        push_incomplete(&mut env, i);
    }
    for i in stale_rank {
        push_incomplete(&mut env, i);
    }
    env["by_confidence"] = json!(by_conf);
    // References a SCIP index answered, and those whose heuristic answer it corrected.
    if precise > 0 {
        env["precise"] = json!(precise);
        env["corrected"] = json!(corrected);
    }
    finish(&mut env);
    Ok(Out { env, data: RefsData { items, total, by_confidence: by_conf, multi } })
}

/// References found from several repos, each reference site once: the same site (repo, file, line, name) is found
/// unlinked from the repo it is in and linked from the repo it resolves into (a module's output read by a caller).
/// The linked hit is kept, at the first one's place.
fn dedupe_refs(items: Vec<(PathBuf, RefHit)>) -> Vec<(PathBuf, RefHit)> {
    let linked = |h: &RefHit| !h.targets.is_empty() && !matches!(h.confidence, "unresolved" | "external_likely");
    let mut at: HashMap<(String, String, u32, String), usize> = HashMap::new();
    let mut out: Vec<(PathBuf, RefHit)> = Vec::with_capacity(items.len());
    for (root, h) in items {
        let key = (h.repo.clone().unwrap_or_else(|| repo_name(&root)), h.file.clone(), h.line, h.name.clone());
        match at.get(&key) {
            Some(&i) => {
                if linked(&h) && !linked(&out[i].1) {
                    out[i] = (root, h);
                }
            }
            None => {
                at.insert(key, out.len());
                out.push((root, h));
            }
        }
    }
    out
}

/// One repo's context: the symbol's bundle, then its HTTP, Angular and message-bus facts.
pub type ContextFound = (PathBuf, Context, Vec<HttpOut>, Option<query::Angular>, Vec<BusOut>);

pub struct ContextData {
    pub found: Vec<ContextFound>,
    /// Entity Framework tables for each of `found`, in the same order.
    pub tables: Vec<SymbolTables>,
    /// Tables named by the query when no symbol is (`ssl_order`), per repo, with their SQL definitions.
    pub table_hits: Vec<(PathBuf, Vec<db::Table>, Vec<TableSql>)>,
    /// The SQL side of each of `found`, in the same order.
    pub sql: Vec<SqlFacts>,
    /// Code-health biomarkers of each of `found` (a function's own, or a type's and its members'), in the same order.
    pub health: Vec<Vec<kc_index::health::Finding>>,
    /// For each of `found` with no resolved caller: how many references share its name (resolving elsewhere or not
    /// at all), in the same order.
    pub named_like: Vec<Option<usize>>,
    /// The Ansible side of each of `found`, in the same order.
    pub ansible: Vec<Option<AnsibleOut>>,
    /// The Terraform side of each of `found` (module sources, module calls using its configuration), in the same order.
    pub terraform: Vec<Option<crate::tf::TerraformOut>>,
    /// When the query names a repo (its directory name): the roles and playbooks deploying or configuring it.
    pub repo_hits: Vec<(PathBuf, RepoAnsibleOut)>,
    /// The functional area holding each of `found`'s files, in the same order.
    pub areas: Vec<Option<AreaRef>>,
    /// Notes on each of `found`, in the same order.
    pub notes: Vec<Vec<kc_index::notes::NoteView>>,
    /// When the query names an indexed file: its area and the notes on its symbols.
    pub file_hits: Vec<FileHit>,
    pub multi: bool,
}

/// The functional area a file is in (see `kc_index::areas`).
#[derive(Debug, Clone, Serialize)]
pub struct AreaRef {
    pub id: u32,
    pub name: String,
    pub files: usize,
    pub cohesion: f64,
}

impl AreaRef {
    fn of(areas: &kc_index::areas::Areas, file: &str) -> Option<AreaRef> {
        areas.of(file).map(|a| AreaRef { id: a.id, name: a.name.clone(), files: a.files.len(), cohesion: a.cohesion })
    }
}

/// `context` on a file path.
#[derive(Debug, Clone, Serialize)]
pub struct FileHit {
    pub repo: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area: Option<AreaRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<kc_index::notes::NoteView>,
}

/// The Ansible side of a context's symbol: roles it uses from other repos, the plays and roles in other repos using
/// it (for a role), and what it deploys or configures, with what that links to.
#[derive(Serialize, Default)]
pub struct AnsibleOut {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RoleLinkOut>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<RoleUserOut>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deploys: Vec<DeployOut>,
    /// For a playbook, a role or a CI job: the application repos whose pipelines deploy with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploys_apps: Option<DeploysAppsOut>,
}

impl AnsibleOut {
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty() && self.used_by.is_empty() && self.deploys.is_empty() && self.deploys_apps.is_none()
    }
}

/// The application repos a playbook, role or CI template deploys, with how.
#[derive(Serialize)]
pub struct DeploysAppsOut {
    /// Distinct application repos.
    pub count: usize,
    pub apps: Vec<AppDeployOut>,
}

/// One application's deploy through the symbol: its job, the playbook, and the key values passed.
#[derive(Serialize)]
pub struct AppDeployOut {
    pub repo: String,
    /// The application's job starting the deploy, `file:line`.
    pub job: String,
    pub at: String,
    pub playbook: String,
    /// Extra vars with a value, `NAME=value`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<String>,
    pub confidence: &'static str,
}

/// How an application repo is deployed: its CI job, through shared templates and child pipelines, to the playbook,
/// its roles, the values passed and what they become; or to the job's other deploy commands, the files configuring
/// them and what they deploy to (see `kc_index::ci`).
#[derive(Serialize)]
pub struct ChainOut {
    /// `ansible`, a deploy tool (`wrangler`, `aws`, `terraform`, `docker`, `kubectl`, `helm`, `sam`, `cdk`), or
    /// `release` (a package published, a GitLab release made).
    pub kind: &'static str,
    /// From the application's job to the job running the command.
    pub path: Vec<HopOut>,
    /// The CI files the chain goes through: its jobs' and templates' files and the files including them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ci_files: Vec<String>,
    /// The `parallel: matrix:` values of the job run giving the chain.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matrix: Vec<String>,
    /// The `ansible-playbook` command (the first deploy command otherwise), `repo:file:line` (`file:line` in the
    /// application repo).
    pub run: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playbook: Option<PlaybookOut>,
    /// The deploy commands, for kinds other than `ansible`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<kc_index::ci::Passed>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<kc_index::ci::Passed>,
    /// The roles the playbook runs, and the roles they run (to a bounded depth), each with its task, handler and
    /// template files.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RoleOut>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deploys: Vec<DeployOut>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<String>,
    pub confidence: &'static str,
}

/// A role a deploy chain's playbook runs: where it is, how sure the link is, and its files (paths in its repo).
#[derive(Serialize)]
pub struct RoleOut {
    pub name: String,
    /// The repo holding it; none for a role no indexed repo holds (a Galaxy role, `external_likely`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Its directory in that repo (`""` for a repo that is one role).
    pub dir: String,
    pub confidence: &'static str,
    /// 1 for a role the playbook runs, 2 for one that role runs, ….
    pub depth: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub handlers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub meta: Vec<String>,
    /// Files left out by the per-role cap.
    #[serde(skip_serializing_if = "is_zero")]
    pub more_files: u32,
}

/// One deploy command of a job: what it is, where it is written, the files it reads and what it deploys to.
#[derive(Serialize)]
pub struct StepOut {
    pub tool: &'static str,
    pub action: String,
    pub command: String,
    /// `repo:file:line`, or `file:line` in the application repo (the package.json script, when run through one).
    pub at: String,
    /// The package script run, and where: `pnpm run deploy at .gitlab-ci.yml:40`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<kc_index::ci_tools::FileRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<kc_index::ci::Passed>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<ModuleOut>,
}

/// A Terraform module block and the repo its source names.
#[derive(Serialize)]
pub struct ModuleOut {
    pub name: String,
    pub source: String,
    /// `file:line` of the source.
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub confidence: &'static str,
}

#[derive(Serialize)]
pub struct HopOut {
    /// `job`, `template` (the job extended that supplies the step) or `trigger` (a child pipeline).
    pub how: &'static str,
    pub name: String,
    /// `repo:file:line`, or `file:line` in the application repo.
    pub at: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub included: Vec<String>,
}

#[derive(Serialize)]
pub struct PlaybookOut {
    /// `repo/file`, or the path as written when no indexed repo holds it.
    pub name: String,
    pub confidence: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clone: Option<String>,
    /// The repo has no playbook at that path.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub missing: bool,
    /// Files the playbook reads (templates, included task files, requirements), by path in its repo.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// `file:line` in the application repo, `repo:file:line` elsewhere; no line for a GitLab built-in template (line 0).
fn site_in(app: &Path, repo: &Path, file: &str, line: u32) -> String {
    let at = if line == 0 { file.to_string() } else { format!("{file}:{line}") };
    if repo == app { at } else { format!("{}:{at}", repo_name(repo)) }
}

impl ChainOut {
    fn new(c: kc_index::ci::Chain) -> ChainOut {
        let app = c.app.clone();
        let playbook = c.playbook.as_ref().map(|p| PlaybookOut {
            name: c.target(),
            confidence: p.confidence,
            clone: p.clone.clone(),
            missing: p.repo.is_some() && !p.found,
            files: p.files.clone(),
        });
        let steps = c
            .steps
            .iter()
            .map(|s| StepOut {
                tool: s.tool,
                action: s.action.clone(),
                command: s.command.clone(),
                at: site_in(&app, &s.repo, &s.file, s.line),
                via: s.via.as_ref().map(|v| format!("{} at {}", v.command, site_in(&app, &v.repo, &v.file, v.line))),
                files: s.files.clone(),
                values: s.values.clone(),
                modules: s
                    .modules
                    .iter()
                    .map(|m| ModuleOut {
                        name: m.name.clone(),
                        source: m.source.clone(),
                        at: format!("{}:{}", m.file, m.line),
                        repo: m.repo.as_deref().map(repo_name),
                        confidence: m.confidence,
                    })
                    .collect(),
            })
            .collect();
        ChainOut {
            kind: c.kind,
            ci_files: c.ci_files.clone(),
            matrix: c.matrix.clone(),
            path: c
                .hops
                .iter()
                .map(|h| HopOut {
                    how: h.how,
                    name: h.name.clone(),
                    at: site_in(&app, &h.repo, &h.file, if h.how == "trigger" { 0 } else { h.line }),
                    included: h.included.clone(),
                })
                .collect(),
            run: site_in(&app, &c.run.repo, &c.run.file, c.run.line),
            playbook,
            steps,
            inventory: c.inventory,
            vars: c.vars,
            roles: c
                .roles
                .iter()
                .map(|r| RoleOut {
                    name: r.name.clone(),
                    repo: (!r.repo.as_os_str().is_empty()).then(|| repo_name(&r.repo)),
                    dir: r.dir.clone(),
                    confidence: r.confidence,
                    depth: r.depth,
                    tasks: r.tasks.clone(),
                    handlers: r.handlers.clone(),
                    templates: r.templates.clone(),
                    meta: r.meta.clone(),
                    more_files: r.more_files as u32,
                })
                .collect(),
            deploys: c.deploys.into_iter().map(DeployOut::new).collect(),
            conditions: c.conditions,
            confidence: c.confidence,
        }
    }
}

/// The application repos whose deploy chains `keep` accepts, one row per (repo, job, playbook).
fn deploys_apps(chains: &[kc_index::ci::Chain], keep: impl Fn(&kc_index::ci::Chain) -> bool) -> Option<DeploysAppsOut> {
    let mut apps: Vec<AppDeployOut> = Vec::new();
    for c in chains.iter().filter(|c| keep(c)) {
        let Some(job) = c.hops.first() else { continue };
        let playbook = c.target();
        let repo = repo_name(&c.app);
        if apps.iter().any(|a| a.repo == repo && a.job == job.name && a.playbook == playbook) {
            continue;
        }
        let vars = c.vars.iter().filter_map(|v| v.value.as_ref().map(|x| format!("{}={x}", v.name))).collect();
        apps.push(AppDeployOut { repo, job: job.name.clone(), at: format!("{}:{}", job.file, job.line), playbook, vars, confidence: c.confidence });
    }
    if apps.is_empty() {
        return None;
    }
    apps.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.job.cmp(&b.job)));
    let mut repos: Vec<&str> = apps.iter().map(|a| a.repo.as_str()).collect();
    repos.dedup();
    Some(DeploysAppsOut { count: repos.len(), apps })
}

/// A role used here and defined in another repo.
#[derive(Serialize)]
pub struct RoleLinkOut {
    pub name: String,
    pub file: String,
    pub line: u32,
    /// The repo defining it; nothing when no indexed repo does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The role's main file there, `file:line`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defined_at: Option<String>,
    /// `exact` (the requirement's `src` is that repo), `unique_name`, `ambiguous`, or `unlinked`.
    pub confidence: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
}

/// A play, task or role in another repo using a role.
#[derive(Serialize)]
pub struct RoleUserOut {
    pub repo: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_kind: Option<String>,
    pub file: String,
    pub line: u32,
    pub confidence: &'static str,
}

/// What a task deploys or configures, with what it links to.
#[derive(Serialize)]
pub struct DeployOut {
    /// What the task does: `deploys`, `undeploys`, `restarts` or `stops`.
    pub action: String,
    pub what: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<DeployLinkOut>,
}

#[derive(Serialize)]
pub struct DeployLinkOut {
    pub repo: String,
    /// `repo` (an application repo by name), `bus` (its message types declare the vhost), `sql` (it defines the schema).
    pub how: &'static str,
    pub matched: String,
    pub confidence: &'static str,
}

impl DeployOut {
    fn new(d: kc_index::ansible::Deploy) -> DeployOut {
        DeployOut {
            action: d.action,
            what: d.what,
            value: d.value,
            resolved: d.resolved,
            symbol: d.symbol,
            file: d.file,
            line: d.line,
            links: d
                .links
                .into_iter()
                .map(|l| DeployLinkOut { repo: repo_name(&l.repo), how: l.how, matched: l.matched, confidence: l.confidence })
                .collect(),
        }
    }
}

/// What deploys or configures a repo: tasks in other repos naming it, and the repo's own playbooks and roles.
#[derive(Serialize)]
pub struct RepoAnsibleOut {
    pub repo: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deployed_by: Vec<RepoDeployOut>,
    /// The repo's own playbooks and roles.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub own: Vec<String>,
    /// How its CI pipeline deploys it: each `ansible-playbook` command a job runs, through shared CI templates.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deploy_chains: Vec<ChainOut>,
    /// CI includes and child pipelines that couldn't be read, so chains through them are missing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ci_unread: Vec<String>,
    /// What a limit left out of the pipeline (see `kc_index::ci::Pipelines::cut`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ci_cut: Vec<String>,
    /// GitLab built-in templates the pipeline includes (known, not fetched), with the jobs each provides.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ci_templates: Vec<String>,
    /// Every `include:` of every CI file the pipeline read, and what came of it: followed (the repo, files, jobs with
    /// their stages, and deploy steps it brings, recursively) or why not (`missing`: the repo holding it isn't
    /// indexed). See `kc_index::ci::Include`. Those its rules leave out are in `ci_skipped` instead.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ci_includes: Vec<kc_index::ci::Include>,
    /// The includes whose `rules` leave them out of a pipeline for a push to the default branch, one line each
    /// (`file:line: kind target`): not read, so nothing in the chains comes from them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ci_skipped: Vec<String>,
}

#[derive(Serialize)]
pub struct RepoDeployOut {
    /// The repo holding the role or playbook.
    pub repo: String,
    /// The role the task is in, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The plays and roles in other repos using that role.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub role_used_by: Vec<String>,
    #[serde(flatten)]
    pub deploy: DeployOut,
    /// The link's confidence for this repo.
    pub confidence: &'static str,
}

/// The Ansible side of a context's symbol, or `None` for a symbol that isn't Ansible or has none.
fn ansible_for(o: &Opened, ctx: &Context) -> Result<Option<AnsibleOut>> {
    let s = &ctx.symbol;
    let ci_job = s.kind == "job";
    let ansible_file = kc_extract::ansible::place(&s.file).is_some() || (kc_store::ansible::is_ansible_kind(&s.kind) && !kc_store::terraform::is_terraform_path(&s.file));
    if !ansible_file && !ci_job {
        return Ok(None);
    }
    let summaries = full_summaries(o)?;
    if ci_job {
        let chains = kc_index::ci::Ci::new(&summaries).all();
        let out = AnsibleOut { deploys_apps: deploys_apps(&chains, |c| c.through_job(&o.root, &s.file, &s.name)), ..AnsibleOut::default() };
        return Ok((!out.is_empty()).then_some(out));
    }
    let a = kc_index::ansible::Ansible::new(&summaries);
    let Some(me) = summaries.iter().find(|r| r.root == o.root) else { return Ok(None) };
    let role_dir = kc_extract::ansible::place(&s.file).and_then(|p| p.role_dir);
    let whole_role = s.kind == "role";
    let within = |file: &str, line: u32| -> bool {
        if whole_role {
            kc_extract::ansible::place(file).and_then(|p| p.role_dir) == role_dir
        } else {
            file == s.file && line >= s.line && line <= s.end_line
        }
    };
    let mut out = AnsibleOut::default();
    for u in me.ansible.uses.iter().filter(|u| within(&u.file, u.line)) {
        let links = a.role(&o.root, &u.name, &u.file);
        if links.is_empty() {
            out.roles.push(RoleLinkOut { name: u.name.clone(), file: u.file.clone(), line: u.line, repo: None, defined_at: None, confidence: "unlinked", src: None });
        }
        for l in links {
            out.roles.push(RoleLinkOut {
                name: u.name.clone(),
                file: u.file.clone(),
                line: u.line,
                repo: Some(repo_name(&l.repo)),
                defined_at: Some(match &u.tasks_from {
                    Some(t) if !l.role.dir.is_empty() => format!("{}/tasks/{t}:1", l.role.dir),
                    Some(t) => format!("tasks/{t}:1"),
                    None => format!("{}:{}", l.role.file, l.role.line),
                }),
                confidence: l.confidence,
                src: l.src,
            });
        }
    }
    if whole_role {
        for (repo, u, confidence) in a.role_users(&o.root, role_dir.as_deref()) {
            out.used_by.push(RoleUserOut { repo: repo_name(&repo), name: u.name, from: u.from, from_kind: u.from_kind, file: u.file, line: u.line, confidence });
        }
    }
    out.deploys = a.deploys(&o.root, |f| within(&f.file, f.line)).into_iter().map(DeployOut::new).collect();
    if s.kind == "playbook" || whole_role {
        let chains = kc_index::ci::Ci::new(&summaries).all();
        let dir = role_dir.clone().unwrap_or_default();
        out.deploys_apps = deploys_apps(&chains, |c| if whole_role { c.uses_role(&o.root, &dir) } else { c.runs_playbook(&o.root, &s.file) });
    }
    Ok((!out.is_empty()).then_some(out))
}

/// When `query` names the repo at `o.root` (its directory name, without case): what deploys or configures it.
fn repo_ansible(o: &Opened, query: &str) -> Result<Option<RepoAnsibleOut>> {
    let name = repo_name(&o.root);
    if !name.eq_ignore_ascii_case(query.trim()) {
        return Ok(None);
    }
    let summaries = full_summaries(o)?;
    let a = kc_index::ansible::Ansible::new(&summaries);
    let mut deployed_by = Vec::new();
    for (repo, d) in a.deployed_by(&o.root) {
        let confidence = d.links.iter().find(|l| l.repo == o.root).map_or("ambiguous", |l| l.confidence);
        let role_used_by = match (&d.role, kc_extract::ansible::place(&d.file).and_then(|p| p.role_dir)) {
            (Some(_), Some(dir)) => a
                .role_users(&repo, Some(&dir))
                .into_iter()
                .map(|(r, u, _)| format!("{}: {}", repo_name(&r), u.from.unwrap_or(u.file)))
                .collect(),
            _ => Vec::new(),
        };
        deployed_by.push(RepoDeployOut { repo: repo_name(&repo), role: d.role.clone(), role_used_by, deploy: DeployOut::new(d), confidence });
    }
    let own: Vec<String> = o
        .store
        .conn()
        .prepare(
            "SELECT s.kind || ' ' || f.path FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.lang = 'ansible' AND s.kind IN ('playbook', 'role') AND s.tags = '' ORDER BY f.path",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let pipelines = kc_index::ci::Ci::new(&summaries).chains(&o.root);
    let deploy_chains: Vec<ChainOut> = pipelines.chains.into_iter().map(ChainOut::new).collect();
    let (ci_unread, ci_templates, ci_cut) = (pipelines.unread, pipelines.templates, pipelines.cut);
    // Includes the rules leave out are named apart, in a line each: nothing the answer lists comes through them.
    let (skipped, ci_includes): (Vec<_>, Vec<_>) = pipelines.includes.into_iter().partition(|i| i.status == "skipped");
    let ci_skipped = skipped.iter().map(|i| format!("{}: {}", i.at, i.named())).collect();
    // A query naming the repo answers with the repo even when nothing deploys it, so the answer says so instead of
    // "no definition".
    Ok(Some(RepoAnsibleOut { repo: name, deployed_by, own, deploy_chains, ci_unread, ci_cut, ci_templates, ci_includes, ci_skipped }))
}


/// The source line of each caller in this repo (`code`), read once per file.
fn fill_call_lines(root: &Path, callers: &mut [RefHit]) {
    fill_lines(root, callers.iter_mut().filter(|h| h.repo.is_none()));
}

/// [`fill_call_lines`] for references in the repo at `root`, whatever their `repo`.
fn fill_lines<'h>(root: &Path, hits: impl Iterator<Item = &'h mut RefHit>) {
    let mut files: HashMap<String, Option<String>> = HashMap::new();
    for h in hits {
        let text = files.entry(h.file.clone()).or_insert_with(|| std::fs::read_to_string(root.join(&h.file)).ok());
        if let Some(line) = text.as_deref().and_then(|t| t.lines().nth(h.line.saturating_sub(1) as usize)) {
            let line = line.trim();
            let cut = (0..=line.len().min(200)).rev().find(|&i| line.is_char_boundary(i)).unwrap_or(0);
            h.code = Some(if cut < line.len() { format!("{}…", &line[..cut]) } else { line.to_string() });
        }
    }
}

/// References named like `s` (calls or member uses) that resolve elsewhere or not at all: when none resolves to
/// `s`, these are where an agent looks next (`kc_refs`). A text search for the name was tried here and dropped: few
/// of the lines it found were calls to the symbol.
fn refs_named_like(conn: &rusqlite::Connection, s: &query::SymbolRow) -> Result<usize> {
    let kinds: &[&str] = if s.kind == "property" { &["member"] } else { &["call", "member"] };
    let name = if s.kind == "init" { s.container.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c)).unwrap_or(&s.name) } else { &s.name };
    Ok(query::refs(conn, name, kinds, 0)?.1.total)
}

/// An HTTP fact about a context's symbol: the routes it requests, with the actions serving them (and their
/// source), or the route it serves, with the contracts requesting it.
#[derive(Serialize)]
pub struct HttpOut {
    pub role: &'static str,
    pub symbol: String,
    /// Empty for a generated client's operation, which names no verb or path.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub verb: String,
    pub path: Option<String>,
    /// A generated API client's operation the symbol calls (`getItems`), linked by name to the action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// The service a gateway contract is sent to (`[ShopRequest]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub links: Vec<HttpLinkOut>,
    /// The symbol is a contract that no code in the indexed repos creates, and whose route no other contract or call
    /// requests: likely left over.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unsent: bool,
    /// For a contract no action serves: commented-out `[Http*]` attributes that served its route.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commented_out: Vec<HttpLinkOut>,
    /// No links: what was searched and what is left to check (see `kc_index::http::unlinked_note`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Serialize)]
pub struct HttpLinkOut {
    pub repo: String,
    pub symbol: String,
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    /// The line of the attribute, call or comment making the link.
    pub line: u32,
    /// `exact`, `dominant` or `ambiguous`; see `kc_index::http`.
    pub confidence: &'static str,
    /// What the link did not check (see `kc_store::answer::Assumption`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
    /// The host evidence the link rests on: the host, where the client names it, where it is mapped to the repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<kc_index::http::HostLink>,
    /// The handler's source, capped, for links to actions that aren't ambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `source` stops before the handler ends (at 60 lines or 4000 characters).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub source_truncated: bool,
}

/// Lines `start..=end` of a file, at most 60 lines and 4000 characters, and whether that cut them.
fn excerpt(root: &Path, file: &str, start: u32, end: u32) -> Option<(String, bool)> {
    let bytes = std::fs::read(root.join(file)).ok()?;
    let text = kc_extract::decode(&bytes);
    let span = (end + 1).saturating_sub(start);
    let lines: Vec<&str> = text.lines().skip(start.saturating_sub(1) as usize).take(span.min(60) as usize).collect();
    let mut cut = span > 60;
    let mut out = lines.join("\n");
    if out.len() > 4000 {
        let at = (0..=4000).rev().find(|&i| out.is_char_boundary(i)).unwrap_or(0);
        out.truncate(at);
        out.push_str("\n…");
        cut = true;
    }
    Some((out, cut))
}

/// `unsupported` entries for a symbol's HTTP part: an endpoint registration it handles or makes whose route KnowCode
/// can't place; or, for a controller action with no route of its own and no callers in a repo mapping conventional
/// routes, those routes, which may serve it.
fn http_unsupported(o: &Opened, ctx: &Context) -> Result<Vec<kc_store::unsupported::Site>> {
    let summaries = planned_summaries(o)?;
    let Some(mine) = summaries.iter().find(|s| s.root == o.root) else { return Ok(Vec::new()) };
    let s = &ctx.symbol;
    let qualified = s.qualified();
    let unsupported: Vec<&query::RouteRow> = mine.routes.iter().filter(|r| r.side == "unsupported").collect();
    let mut out: Vec<kc_store::unsupported::Site> =
        unsupported.iter().filter(|r| r.symbol == qualified && r.file == s.file).map(|r| unplaced_site(r)).collect();
    let action = s.kind == "method" && s.container.as_deref().is_some_and(|c| c.ends_with("Controller"));
    let routed = mine.routes.iter().any(|r| r.side == "server" && r.symbol == qualified && r.file == s.file);
    if action
        && !routed
        && ctx.callers_summary.total == 0
        && let Some(c) = unsupported.iter().find(|r| r.detail.as_deref().is_some_and(|d| d.starts_with("conventional")))
    {
        let mut site = unplaced_site(c);
        site.construct.push_str(", which may serve this action");
        out.push(site);
    }
    Ok(out)
}

fn http_for(o: &Opened, ctx: &Context, no_sync: bool) -> Result<Vec<HttpOut>> {
    let mut summaries = planned_summaries(o)?;
    let s = &ctx.symbol;
    let qualified = s.container.as_ref().map_or_else(|| s.name.clone(), |c| format!("{c}.{}", s.name));
    let mut entries = kc_index::http::for_symbol(o.store.conn(), &o.root, &qualified, s.id, &summaries)?;
    let created: Vec<String> = o.store.conn().prepare_cached("SELECT DISTINCT name FROM refs WHERE from_id = ?1 AND kind = 'instantiate'")?
        .query_map([s.id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let may_have_contract = kc_index::workspace::neighbours(&o.root, &summaries).0.iter()
        .any(|d| d.light && created.iter().any(|name| d.may_define(name)));
    if !entries.is_empty() || may_have_contract {
        summaries = full_summaries(o)?;
        entries = kc_index::http::for_symbol(o.store.conn(), &o.root, &qualified, s.id, &summaries)?;
    }
    // A contract is sent where it's created: in its repo, or in a repo depending on it (a gateway DTO's package).
    let unsent = if entries.iter().any(|e| e.role == "requests" && e.symbol == qualified) && matches!(s.kind.as_str(), "class" | "struct") {
        let (own, _) = query::refs(o.store.conn(), &s.name, &["instantiate"], usize::MAX)?;
        let me = repo_name(&o.root);
        let route = summaries.iter().find(|r| r.root == o.root).and_then(|r| r.routes.iter().find(|r| r.side == "client" && r.symbol == qualified));
        !own.iter().any(|h| h.may_target(s.id))
            && !route.is_some_and(|r| kc_index::http::requested_elsewhere(r, &o.root, &summaries))
            && !dependent_refs(o, &s.name, &["instantiate"], no_sync)?
                .iter()
                .any(|h| h.targets.iter().any(|t| t.id == s.id && t.repo.as_deref() == Some(me.as_str())))
    } else {
        false
    };
    let link_out = |l: kc_index::http::RouteLink, source: bool| {
        let source = (source && l.confidence != "ambiguous").then(|| excerpt(&l.repo, &l.route.file, l.route.start_line, l.route.end_line)).flatten();
        HttpLinkOut {
        source_truncated: source.as_ref().is_some_and(|s| s.1),
        source: source.map(|s| s.0),
        repo: repo_name(&l.repo),
        symbol: l.route.symbol,
        kind: l.route.kind,
        file: l.route.file,
        start_line: l.route.start_line,
        end_line: l.route.end_line,
        line: l.route.line,
        confidence: l.confidence,
        assumptions: l.assumptions,
        host: l.host,
        }
    };
    Ok(entries
        .into_iter()
        .map(|e| {
            let no_links = e.links.is_empty() && e.commented_out.is_empty();
            HttpOut {
                unsent: unsent && e.role == "requests" && e.symbol == qualified,
                commented_out: e.commented_out.into_iter().map(|l| link_out(l, false)).collect(),
                role: e.role,
                links: e.links.into_iter().map(|l| link_out(l, e.role == "requests")).collect(),
                symbol: e.symbol,
                operation: e.verb.is_empty().then(|| e.path.clone()).flatten(),
                path: if e.verb.is_empty() { None } else { e.path },
                verb: e.verb,
                service: e.service,
                note: if no_links { kc_index::http::unlinked_note(&e.route, &summaries, false) } else { None },
            }
        })
        .collect())
}

/// A message-bus fact about a symbol: the message type it is, a message it publishes, or one it consumes, with the
/// code on the other side.
#[derive(Serialize)]
pub struct BusOut {
    /// `message`, `publishes` or `consumes`.
    pub role: &'static str,
    /// The message type, the publishing method or the consumer class.
    pub symbol: String,
    /// The message type: its full name when it resolves, else as written; `?` when unknown.
    pub message: String,
    /// How the name resolved: `exact` (one declared message type), `ambiguous` (several), `unresolved` (none
    /// declared in indexed repos), `unknown` (the published expression's type isn't known).
    pub resolution: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// The call (`Publish`, `Send`, …) or the consumer's base (`ShopConsumer`, `IConsumer`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    pub file: String,
    pub line: u32,
    pub links: Vec<BusLinkOut>,
}

#[derive(Serialize)]
pub struct BusLinkOut {
    pub repo: String,
    /// `publisher` or `consumer`.
    pub role: &'static str,
    /// The publishing method, or the consumer's `Consume` method.
    pub symbol: String,
    pub file: String,
    pub line: u32,
    /// `exact` or `ambiguous`; see `kc_index::bus`.
    pub confidence: &'static str,
    /// What the link did not check (see `kc_store::answer::Assumption`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
}

/// The line of a consumer's `Consume` method, as `trace` and `impact` give it, looked up in the consumer's repo; each
/// repo's index is opened once per answer. `None` when the method isn't found (a consumer without one).
#[derive(Default)]
struct ConsumeLines(HashMap<PathBuf, Option<Store>>);

impl ConsumeLines {
    fn line(&mut self, link: &kc_index::bus::BusLink) -> Option<u32> {
        let store = self.0.entry(link.repo.clone()).or_insert_with(|| kc_index::open(&link.repo).ok()).as_ref()?;
        let handler = link.row.handler();
        query::definitions(store.conn(), &handler).ok()?.into_iter().find(|d| d.file == link.row.file && d.qualified() == handler).map(|d| d.line)
    }
}

impl BusOut {
    fn new(e: kc_index::bus::BusEntry, lines: &mut ConsumeLines) -> BusOut {
        BusOut {
            role: e.role,
            message: e.message(),
            queue: e.queue(),
            resolution: e.resolution.as_ref().map_or("unknown", |r| r.confidence),
            symbol: e.row.symbol.clone(),
            via: e.row.via.clone(),
            file: e.row.file.clone(),
            line: e.row.line,
            links: e
                .links
                .into_iter()
                .map(|l| BusLinkOut {
                    repo: repo_name(&l.repo),
                    role: if l.row.kind == "publish" { "publisher" } else { "consumer" },
                    symbol: l.row.handler(),
                    // A consumer class: its `Consume` method; a state machine or a registration: where it names the message.
                    line: if l.row.kind == "consume" && l.row.handler() != l.row.symbol { lines.line(&l).unwrap_or(l.row.line) } else { l.row.line },
                    file: l.row.file,
                    confidence: l.confidence,
                    assumptions: l.assumptions,
                })
                .collect(),
        }
    }
}

/// The bus facts about a context's symbol. The workspace summary is read only when the repo has a bus fact for it.
fn bus_for(o: &Opened, ctx: &Context) -> Result<(Vec<BusOut>, Vec<kc_store::unsupported::Site>)> {
    let s = &ctx.symbol;
    let has: bool = o.store.conn().query_row(
        "SELECT EXISTS(SELECT 1 FROM bus_facts WHERE symbol_id = ?1
                        OR (kind = 'consume' AND symbol_id = (SELECT parent_id FROM symbols WHERE id = ?1)))",
        [s.id],
        |r| r.get(0),
    )?;
    if !has {
        return Ok((Vec::new(), Vec::new()));
    }
    let summaries = full_summaries(o)?;
    let qualified = s.container.as_ref().map_or_else(|| s.name.clone(), |c| format!("{c}.{}", s.name));
    let mut lines = ConsumeLines::default();
    let entries = kc_index::bus::for_symbol(&o.root, &qualified, &s.file, s.line, &summaries);
    // Publishes whose message type isn't known that may publish this message: its publishers may be missing.
    let unknown = kc_index::bus::unknown_publishes(&o.root, &entries, &summaries)
        .into_iter()
        .map(|(root, r)| kc_store::unsupported::Site {
            area: "bus".into(),
            construct: format!("{} of a message whose type isn't known", r.via.as_deref().unwrap_or("publish")),
            file: if root == o.root { r.file } else { format!("{}:{}", repo_name(&root), r.file) },
            line: r.line,
        })
        .collect();
    Ok((entries.into_iter().map(|e| BusOut::new(e, &mut lines)).collect(), unknown))
}

/// Constructs met but not read (see `kc_store::unsupported`) that the context of `s` draws on: those inside it, a
/// role's in its files, a Terraform address's in its directory naming it, the repo's DI registrations it can't read
/// for a type they may register, and `bus` (publishes of unknown messages). One `unsupported` entry per area.
fn unread_for(o: &Opened, s: &kc_store::query::SymbolRow, bus: Vec<kc_store::unsupported::Site>) -> Result<Vec<Incomplete>> {
    use kc_store::unsupported;
    let conn = o.store.conn();
    let mut sites = bus;
    sites.extend(unsupported::in_span(conn, &s.file, s.line, s.end_line)?);
    if s.kind == "role"
        && let Some(dir) = kc_extract::ansible::place(&s.file).and_then(|p| p.role_dir)
    {
        let mine = |f: &str| kc_extract::ansible::place(f).and_then(|p| p.role_dir).as_deref() == Some(dir.as_str());
        sites.extend(unsupported::in_area(conn, "ansible")?.into_iter().filter(|u| mine(&u.file)));
    }
    if kc_store::terraform::is_terraform_path(&s.file) {
        let dir = |f: &str| f.rsplit_once('/').map_or("", |(d, _)| d).to_string();
        let names = |c: &str| c.split(|ch: char| !(ch.is_alphanumeric() || matches!(ch, '_' | '.' | '-'))).any(|w| w == s.name);
        sites.extend(unsupported::in_area(conn, "terraform")?.into_iter().filter(|u| dir(&u.file) == dir(&s.file) && names(&u.construct)));
    }
    // A C# project whose imports can't be read: the symbol's callers there may be ambiguous or unresolved.
    // Only a project with a reference of the symbol's name (a candidate caller) bears on it, as in `refs`.
    if s.file.ends_with(".cs") {
        let name = if s.kind == "init" { s.container.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c)).unwrap_or(&s.name) } else { s.name.as_str() };
        let mut named = conn.prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM refs r JOIN files f ON f.id = r.file_id WHERE r.name = ?1 AND (?2 = '' OR f.path LIKE ?2 || '/%'))",
        )?;
        for u in unsupported::in_area(conn, "imports")? {
            let project = u.file.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
            if named.query_row(rusqlite::params![name, project], |r| r.get::<_, bool>(0))? {
                sites.push(u);
            }
        }
    }
    if matches!(s.kind.as_str(), "class" | "protocol") && s.file.ends_with(".cs") {
        let word = |c: &str| c.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).any(|w| w == s.name);
        for u in unsupported::in_area(conn, "di")? {
            let applies = match u.construct.strip_prefix("assembly scan ") {
                // A scan registers what its name filter lets through (`[*Service]`), every type without one.
                Some(rest) => match rest.rsplit_once('[').map(|(_, f)| f.trim_end_matches(']')) {
                    Some(f) if s.kind == "class" => match (f.strip_prefix('*'), f.strip_suffix('*')) {
                        (Some(end), _) => s.name.ends_with(end),
                        (_, Some(start)) => s.name.starts_with(start),
                        _ => s.name == f,
                    },
                    _ => true,
                },
                None => word(&u.construct),
            };
            if applies {
                sites.push(u);
            }
        }
    }
    sites.sort_by(|a, b| (&a.area, &a.file, a.line).cmp(&(&b.area, &b.file, b.line)));
    sites.dedup_by(|a, b| a.area == b.area && a.file == b.file && a.line == b.line && a.construct == b.construct);
    Ok(unsupported::incomplete(&sites, None))
}

/// Every message the selected repos publish or consume, outside tests, with the code on the other side across indexed
/// repos. Reads the workspace summary: the selected repos are synced first, the others are as last synced.
pub fn events(args: &Scope) -> Result<Out<Vec<(PathBuf, BusOut)>>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    for root in &roots {
        opened.extend(open_member(&roots, root, args.no_sync)?);
    }
    let summaries = full_workspace(&opened, &roots)?;
    let mut out = Vec::new();
    let mut lines = ConsumeLines::default();
    for root in &roots {
        out.extend(kc_index::bus::listing(root, &summaries).into_iter().map(|e| (root.clone(), BusOut::new(e, &mut lines))));
    }
    let items: Vec<Value> = out
        .iter()
        .map(|(root, b)| {
            let mut v = json!(b);
            v["repo"] = json!(repo_name(root));
            v
        })
        .collect();
    let n = items.len();
    Ok(Out { env: envelope(&opened, json!(items), n, n), data: out })
}

#[cfg(test)]
pub fn context(symbol: &str, budget: usize, source: kc_store::skeleton::SourceMode, args: &Scope) -> Result<Out<ContextData>> {
    context_raised(symbol, budget, source, None, None, args)
}

/// [`context`], with a repo's deploy answer's lists cut at `limit` rows each (every one cut continues by its cursor).
pub fn context_limited(symbol: &str, budget: usize, source: kc_store::skeleton::SourceMode, limit: Option<usize>, args: &Scope) -> Result<Out<ContextData>> {
    context_raised(symbol, budget, source, limit, None, args)
}

/// The lists of a repo's deploy answer (`items[0].ansible`) that continue by cursor, in the order the MCP size cap
/// cuts them (see `mcp::shrink`): the long tails first, the deploy steps last. `ci_unread` and `ci_cut` are never
/// cut: they say what the answer is missing.
pub const DEPLOY_LISTS: &[&str] = &["ci_includes", "ci_skipped", "ci_templates", "own", "deployed_by", "deploy_chains"];

/// Rows per page of a deploy answer's list continued by cursor, when no `limit` was asked.
const DEPLOY_PAGE: usize = 50;

/// [`context`], with its lists (callers, callees, members, conformers, accessors) cut at `raise` at the least: what
/// a cursor continuing one of them reads; a repo's deploy answer's lists cut at `limit` (raised to `raise`).
pub fn context_raised(
    symbol: &str,
    budget: usize,
    source: kc_store::skeleton::SourceMode,
    limit: Option<usize>,
    raise: Option<usize>,
    args: &Scope,
) -> Result<Out<ContextData>> {
    let _memo = kc_index::memo_changes();
    let roots = target_repos(&args.repos)?;
    let mut limits = ContextLimits::from_budget(budget);
    limits.source = source;
    let mut elsewhere_max = CONFORMERS_ELSEWHERE;
    if let Some(r) = raise {
        limits.callers = limits.callers.max(r);
        limits.callees = limits.callees.max(r);
        limits.members = limits.members.max(r);
        limits.conformers = limits.conformers.max(r);
        elsewhere_max = elsewhere_max.max(r);
    }
    let mut named_like = Vec::new();
    let mut found = Vec::new();
    let mut tables = Vec::new();
    let mut table_hits = Vec::new();
    let mut sql = Vec::new();
    let mut health = Vec::new();
    let mut ansible = Vec::new();
    let mut terraform = Vec::new();
    let mut repo_hits = Vec::new();
    let mut areas = Vec::new();
    let mut notes = Vec::new();
    let mut file_hits = Vec::new();
    let mut opened = Vec::new();
    // Derived data from an earlier index generation the answer used (areas, computed on the side).
    let mut stale_derived: Vec<Incomplete> = Vec::new();
    // What the answer draws on that the extractors met but didn't read (`unsupported`).
    let mut unread: Vec<Incomplete> = Vec::new();
    for root in &roots {
        let Some(mut o) = open_member(&roots, root, args.no_sync)? else { continue };
        if let Some(r) = repo_ansible(&o, symbol)? {
            repo_hits.push((o.root.clone(), r));
        }
        let mut imported = query::context_imports(o.store.conn(), symbol)?;
        let mut deps = open_dependencies_importing(&o, Some(&imported), args.no_sync)?;
        let mut found_ctx = query::context_with(o.store.conn(), &o.root, symbol, &limits, &dependency_refs(&deps))?;
        // The file's content differs from the index although its stat didn't move (an edit keeping mtime and size):
        // index it again and answer from that.
        if !args.no_sync && !o.refresh_pending && let Some(file) = found_ctx.as_ref().filter(|c| c.source_changed).map(|c| c.symbol.file.clone()) {
            let again = kc_index::reindex(&o.root, &mut o.store, &[file.as_str()])?;
            if let Some(s) = o.sync.as_mut() {
                s.parsed += again.parsed;
                s.removed += again.removed;
                s.failed.extend(again.failed);
            }
            imported = query::context_imports(o.store.conn(), symbol)?;
            deps = open_dependencies_importing(&o, Some(&imported), args.no_sync)?;
            found_ctx = query::context_with(o.store.conn(), &o.root, symbol, &limits, &dependency_refs(&deps))?;
        }
        // A stored procedure (or table) the repo's code calls but another repo defines: its definition there.
        if found_ctx.is_none()
            && indexed_file(&o, symbol)?.is_none()
            && o.store.db_model()?.named(symbol).is_empty()
            && let Some((droot, at)) = sql_defined_elsewhere(&o, symbol)?
        {
            let d = open_fresh(&droot, args.no_sync)?;
            found_ctx = query::context(d.store.conn(), &d.root, &at, &limits)?;
            if found_ctx.is_some() {
                opened.push(std::mem::replace(&mut o, d));
            }
        }
        // A role the repo's plays use but another repo defines: the role there.
        if found_ctx.is_none()
            && let Some((droot, at)) = role_defined_elsewhere(&o, symbol)?
        {
            let d = open_fresh(&droot, args.no_sync)?;
            found_ctx = query::context(d.store.conn(), &d.root, &at, &limits)?;
            if found_ctx.is_some() {
                opened.push(std::mem::replace(&mut o, d));
            }
        }
        if let Some(mut ctx) = found_ctx {
            // Callers in repos that depend on this one.
            let s = &ctx.symbol;
            let (name, kinds): (String, Vec<&str>) = if s.kind == "init" {
                let ty = s.container.as_deref().unwrap_or("");
                (ty.rsplit('.').next().unwrap_or(ty).to_string(), vec!["instantiate"])
            } else if matches!(s.kind.as_str(), "class" | "struct" | "enum" | "actor" | "protocol" | "extension") {
                (s.name.clone(), vec!["instantiate", "type_ref"])
            } else {
                (s.name.clone(), vec!["call", "member"])
            };
            let target_id = if s.kind == "init" {
                query::definitions(o.store.conn(), s.container.as_deref().unwrap_or(""))?.first().map(|d| d.id)
            } else {
                Some(s.id)
            };
            let mut elsewhere = dependent_refs(&o, &name, &kinds, args.no_sync)?;
            if kc_store::terraform::is_terraform_path(&s.file) {
                elsewhere.extend(crate::tf::module_refs_elsewhere(&o, &s.name, args.no_sync)?);
            }
            // Calls through the interface member it implements, or the base member it overrides.
            let members = if matches!(s.kind.as_str(), "method" | "property") && s.container.is_some() {
                query::implemented_members(o.store.conn(), s)?
            } else {
                Vec::new()
            };
            // An override of a dependency's base class member (or an implementation of its interface's): the
            // dependency's own calls through that member, a base class calling its virtual member, say. Their ids are
            // the dependency's, so they skip the matching below.
            let mut through_deps = Vec::new();
            if matches!(s.kind.as_str(), "method" | "property") && s.container.is_some() {
                for d in &deps {
                    let theirs = query::implemented_members_in(o.store.conn(), d.store.conn(), s)?;
                    if theirs.is_empty() {
                        continue;
                    }
                    let mut found: Vec<RefHit> = query::refs(d.store.conn(), &s.name, &["call", "member"], usize::MAX)?
                        .0
                        .iter()
                        .filter(|h| h.confidence != "external_likely")
                        .filter_map(|h| theirs.iter().find_map(|m| m.through_member(h)))
                        .collect();
                    if source != kc_store::skeleton::SourceMode::None {
                        fill_lines(&d.root, found.iter_mut());
                    }
                    for h in &mut found {
                        h.repo = Some(repo_name(&d.root));
                    }
                    through_deps.extend(found);
                }
            }
            let elsewhere = elsewhere.into_iter().filter_map(|h| match target_id {
                Some(id) if !h.may_target(id) => members.iter().find_map(|m| m.through(&h, id)),
                _ => Some(h),
            });
            for h in elsewhere.chain(through_deps) {
                if target_id.is_some() {
                    *ctx.callers_summary.by_confidence.entry(h.confidence).or_default() += 1;
                    ctx.callers_summary.total += 1;
                    if ctx.callers.len() < limits.callers {
                        ctx.callers.push(h);
                    }
                }
            }
            // A library's interface or base class: its implementations and subclasses in the repos using it.
            ctx.conformers_elsewhere = dependent_conformers(&o, &ctx.symbol, None, args.no_sync)?;
            let n = ctx.conformers_elsewhere.len();
            ctx.conformers_elsewhere_total = (n > elsewhere_max).then_some(n);
            ctx.conformers_elsewhere.truncate(elsewhere_max);
            // Each caller and callee quotes its line; without cites, callers show it as `code`.
            let mut citer = crate::cite::Citer::new([])
                .with_stores(std::iter::once((o.root.clone(), &o.store)).chain(deps.iter().map(|d| (d.root.clone(), &d.store))));
            if citer.on() {
                // The symbol's own lines: checked, not quoted again (`source` has them).
                let mut own = citer.span(&o.root, &ctx.symbol);
                own.text = None;
                own.cut = None;
                ctx.cite = Some(own);
                citer.refs(&o.root, ctx.callers.iter_mut().chain(ctx.callees.iter_mut()));
                unread.extend(citer.incomplete());
                for h in &mut ctx.callers {
                    if h.cite.as_ref().is_some_and(|c| c.text.is_some()) {
                        h.code = None;
                    }
                }
            } else if source != kc_store::skeleton::SourceMode::None {
                fill_call_lines(&o.root, &mut ctx.callers);
            }
            // No resolved caller of a callable: say so, and how many references by its name resolve elsewhere.
            let callable = matches!(ctx.symbol.kind.as_str(), "method" | "function" | "init" | "property");
            named_like.push(if callable && ctx.callers_summary.total == 0 { Some(refs_named_like(o.store.conn(), &ctx.symbol)?) } else { None });
            let http = http_for(&o, &ctx, args.no_sync)?;
            let unplaced = http_unsupported(&o, &ctx)?;
            let angular = match query::angular(o.store.conn(), ctx.symbol.id)? {
                Some(mut a) => {
                    kc_index::angular::complete(&mut a, &o.root, &full_summaries(&o)?)?;
                    Some(a)
                }
                None => None,
            };
            let mut model = o.store.db_model()?;
            if !model.wrapped.is_empty() {
                let summaries = full_summaries(&o)?;
                known_wrapped(&kc_index::sql::Sql::new(&summaries, Some(&o.root)), &o.root, &mut model);
            }
            let t = db::for_symbol(o.store.conn(), &model, &ctx.symbol, limits.callers)?;
            sql.push(sql_facts(&o, &ctx, &t, limits.callers, args.no_sync)?);
            tables.push(t);
            let (bus, mut unknown_publishes) = bus_for(&o, &ctx)?;
            unknown_publishes.extend(unplaced);
            unread.extend(unread_for(&o, &ctx.symbol, unknown_publishes)?);
            ansible.push(ansible_for(&o, &ctx)?);
            terraform.push(crate::tf::terraform_for(&o, &ctx.symbol)?);
            let s = &ctx.symbol;
            let mut h = kc_index::health::findings(&o.root, &o.store, Some(&s.file))?;
            h.retain(|f| f.kind != "large_file" && f.line >= s.line && f.end_line <= s.end_line);
            health.push(h);
            let (cached_areas, from) = kc_index::areas::cached_at(&o.root, &o.store)?;
            if let Some(g) = from {
                stale_derived.push(kc_store::answer::stale_derived(&format!("context area ({})", repo_name(&o.root)), &g, &o.generation));
            }
            areas.push(AreaRef::of(&cached_areas, &ctx.symbol.file));
            notes.push(if kc_index::notes::any(&o.root)? {
                kc_index::notes::on_symbols(&o.root, &o.store, std::slice::from_ref(&ctx.symbol))?
            } else {
                Vec::new()
            });
            found.push((o.root.clone(), ctx, http, angular, bus));
        } else if let Some(file) = indexed_file(&o, symbol)? {
            // A file: its area, and the notes on its symbols.
            let (cached_areas, from) = kc_index::areas::cached_at(&o.root, &o.store)?;
            if let Some(g) = from {
                stale_derived.push(kc_store::answer::stale_derived(&format!("context area ({})", repo_name(&o.root)), &g, &o.generation));
            }
            let area = AreaRef::of(&cached_areas, &file);
            let mut on_file = Vec::new();
            if kc_index::notes::any(&o.root)? {
                on_file = kc_index::notes::all(&o.root, &o.store)?;
                on_file.retain(|n| n.note.file == file || n.now_at.as_deref().is_some_and(|at| at.rsplit_once(':').is_some_and(|(f, _)| f == file)));
            }
            file_hits.push(FileHit { repo: repo_name(&o.root), file, area, notes: on_file });
        } else {
            // Not a symbol: perhaps a table.
            let model = o.store.db_model()?;
            let hits: Vec<db::Table> = model.named(symbol).into_iter().cloned().collect();
            if !hits.is_empty() {
                let summaries = full_summaries(&o)?;
                let defs = kc_index::sql::Sql::new(&summaries, Some(&o.root));
                let links: Vec<TableSql> = hits.iter().map(|t| table_links(&defs, &o.root, t, &model)).collect();
                // SQL naming its table at run time, or run as dynamic SQL: any of it may touch this table.
                unread.extend(kc_store::unsupported::incomplete(&kc_store::unsupported::in_area(o.store.conn(), "sql")?, None));
                table_hits.push((o.root.clone(), hits, links));
            }
        }
        opened.push(o);
    }
    let mut items: Vec<_> = found
        .iter()
        .zip(&tables)
        .zip(&sql)
        .zip(&health)
        .zip(&named_like)
        .zip(&ansible)
        .enumerate()
        .map(|(i, ((((((root, c, http, angular, bus), t), q), h), sites), an))| {
            let mut v = tag_repo(c, root, true);
            if let Some(a) = &areas[i] {
                v["area"] = json!(a);
            }
            if !notes[i].is_empty() {
                v["notes"] = json!(notes[i]);
            }
            if let Some(a) = an {
                v["ansible"] = json!(a);
            }
            if let Some(t) = &terraform[i] {
                v["terraform"] = json!(t);
            }
            if !h.is_empty() {
                v["health"] = json!(h);
            }
            if let Some(n) = sites {
                v["note"] = json!(format!(
                    "no reference resolves to {}{}",
                    c.symbol.qualified(),
                    if *n > 0 {
                        format!("; {n} references named {} resolve elsewhere or not at all (kc_refs {} lists them)", c.symbol.name, c.symbol.name)
                    } else {
                        String::new()
                    }
                ));
            }
            if !http.is_empty() {
                v["http"] = json!(http);
            }
            if !bus.is_empty() {
                v["bus"] = json!(bus);
            }
            if let Some(a) = angular {
                v["angular"] = json!(a);
            }
            if !t.tables.is_empty() {
                v["tables"] = json!(t.tables.iter().map(table_json).collect::<Vec<_>>());
                for (i, x) in q.tables.iter().enumerate() {
                    x.annotate(&mut v["tables"][i]);
                }
            }
            if t.touches_truncated {
                v["touches_tables_truncated"] = json!(true);
            }
            if !t.touches.is_empty() {
                v["touches_tables"] = json!(t.touches);
                for (i, touch) in t.touches.iter().enumerate() {
                    if let Some((_, links)) = q.procedures.iter().find(|(n, l)| touch.kind == "procedure" && *n == touch.name && !l.is_empty()) {
                        v["touches_tables"][i]["definition"] = json!(links);
                    }
                }
            }
            if !q.is_empty() {
                v["sql"] = json!(q);
            }
            v
        })
        .collect();
    // A query naming an indexed repo answers how that repo is deployed: that item comes first, ahead of any
    // same-named symbol, and is also the envelope's `deploy` (`repo` stays the repo's path, as in every envelope).
    // Each list cut at `limit` says its total, and continues by cursor (below).
    let deploy_limit = limit.map(|l| raise.map_or(l, |r| l.max(r)).max(1));
    let mut deploy_cut: Vec<(usize, &str)> = Vec::new();
    let repo_items: Vec<serde_json::Value> = repo_hits
        .iter()
        .enumerate()
        .map(|(i, (_, r))| {
            let mut v = json!({ "repo": r.repo, "ansible": r });
            if let Some(l) = deploy_limit {
                for k in DEPLOY_LISTS {
                    // `get_mut`: indexing would add the lists the answer doesn't have, as nulls.
                    if let Some(list) = v["ansible"].get_mut(*k).and_then(Value::as_array_mut)
                        && list.len() > l
                    {
                        let total = list.len();
                        list.truncate(l);
                        v["ansible"][format!("{k}_total")] = json!(total);
                        deploy_cut.push((i, k));
                    }
                }
            }
            v
        })
        .collect();
    let repo_items_n = repo_items.len();
    items.splice(0..0, repo_items.iter().cloned());
    items.extend(file_hits.iter().map(|f| json!(f)));
    for (root, hits, links) in &table_hits {
        items.extend(hits.iter().zip(links).map(|(t, l)| {
            let mut v = json!({ "repo": repo_name(root), "table": t });
            l.annotate(&mut v["table"]);
            v
        }));
    }
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    for i in stale_derived.into_iter().chain(unread) {
        push_incomplete(&mut env, i);
    }
    if let Some(first) = repo_items.into_iter().next() {
        env["deploy"] = first;
    }
    // A definition found in another repo than the one asked names its repo.
    let multi = roots.len() > 1 || found.iter().any(|f| !roots.contains(&f.0));
    // Each bundle's lists continue: the found symbols' items follow the repo items.
    let mut lists = Vec::new();
    // A repo's deploy answer: every list continues, whether `limit` cut it here or the MCP size cap cuts it later.
    for i in 0..repo_items_n {
        for k in DEPLOY_LISTS {
            let page = limit.unwrap_or(DEPLOY_PAGE);
            lists.push(cursor::List::new(format!("/items/{i}/ansible/{k}"), page, deploy_cut.contains(&(i, *k))));
        }
    }
    for (i, (_, c, ..)) in found.iter().enumerate() {
        let at = format!("/items/{}", repo_items_n + i);
        lists.push(cursor::List::new(format!("{at}/callers"), limits.callers, c.callers_summary.total > c.callers.len()));
        lists.push(cursor::List::new(format!("{at}/callees"), limits.callees, c.callees_total > c.callees.len()));
        lists.push(cursor::List::new(format!("{at}/members"), limits.members, c.members_total > c.members.len()));
        lists.push(cursor::List::new(format!("{at}/conformers"), limits.conformers, c.conformers_total.is_some()));
        lists.push(cursor::List::new(format!("{at}/conformers_elsewhere"), elsewhere_max, c.conformers_elsewhere_total.is_some()));
        for (j, t) in tables[i].tables.iter().enumerate() {
            let cut = t.accessors_total.is_some_and(|n| n > t.accessors.len());
            lists.push(cursor::List::new(format!("{at}/tables/{j}/accessors"), limits.callers, cut));
        }
    }
    let source_word = match source {
        kc_store::skeleton::SourceMode::Full => "full",
        kc_store::skeleton::SourceMode::Skeleton => "skeleton",
        kc_store::skeleton::SourceMode::None => "none",
        kc_store::skeleton::SourceMode::Auto => "auto",
    };
    let req = cursor::Request::Context { symbol: symbol.to_string(), budget, source: source_word.to_string(), limit };
    cursor::offer(&mut env, req, args, lists, &[]);
    finish(&mut env);
    Ok(Out { env, data: ContextData { found, tables, table_hits, sql, health, named_like, ansible, terraform, repo_hits, areas, notes, file_hits, multi } })
}

/// Where another indexed repo defines the SQL procedure, function, table or view `name` that code in `o` would link
/// to (as `tables` and `flow` link it): that repo and the definition's `file:line`.
fn sql_defined_elsewhere(o: &Opened, name: &str) -> Result<Option<(PathBuf, String)>> {
    if name.contains('/') || name.contains(char::is_whitespace) || name.contains(':') || name.contains('#') {
        return Ok(None);
    }
    let summaries = full_summaries(o)?;
    let sql = kc_index::sql::Sql::new(&summaries, Some(&o.root));
    let kinds: Vec<&str> = kc_index::sql::ROUTINE_KINDS.iter().chain(kc_index::sql::TABLE_KINDS).copied().collect();
    Ok(sql
        .definitions(&o.root, name, &kinds)
        .into_iter()
        .find(|l| l.repo != o.root)
        .map(|l| (l.repo, format!("{}:{}", l.def.file, l.def.line))))
}

/// The role another repo defines that this repo's plays use by `name`, as its first use links (see
/// `file:line`. Of several same-named role repos (`ambiguous`), the first by repo name.
fn role_defined_elsewhere(o: &Opened, name: &str) -> Result<Option<(PathBuf, String)>> {
    let name = name.trim();
    if name.is_empty() || name.contains(['/', ':', '#']) {
        return Ok(None);
    }
    let summaries = full_summaries(o)?;
    let Some(me) = summaries.iter().find(|s| s.root == o.root) else { return Ok(None) };
    let Some(u) = me.ansible.uses.iter().find(|u| u.name == name) else { return Ok(None) };
    let a = kc_index::ansible::Ansible::new(&summaries);
    let mut links = a.role(&o.root, name, &u.file);
    links.sort_by(|x, y| x.repo.cmp(&y.repo));
    Ok(links.into_iter().find(|l| l.repo != o.root && l.repo.join(".git").exists()).map(|l| (l.repo, format!("{}:{}", l.role.file, l.role.line))))
}

/// The indexed file `query` names (repo-relative, or absolute under the repo), if it is one.
fn indexed_file(o: &Opened, query: &str) -> Result<Option<String>> {
    if !query.contains('/') && !query.contains('.') {
        return Ok(None);
    }
    let rel = Path::new(query).strip_prefix(&o.root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| query.to_string());
    let rel = rel.trim_start_matches("./").to_string();
    Ok(o.store.conn().prepare_cached("SELECT 1 FROM files WHERE path = ?1")?.exists([&rel])?.then_some(rel))
}

/// Which edges and symbol kinds a walk (`trace`, `impact`) keeps: `--edges calls,http`, `--kinds method`.
#[derive(Clone, Debug, Default)]
pub struct WalkFilter {
    /// Edge kinds to list and follow (see [`trace::EdgeFilter`]); empty for all.
    pub edges: Vec<String>,
    /// Symbol kinds to list (`method`, `class`, or `type` and `func` for groups); empty for all. The walk still goes
    /// through the others: a kept symbol hangs off its nearest kept ancestor.
    pub kinds: Vec<String>,
    /// The evidence policy (see [`kc_store::answer::EvidencePolicy`]); empty for `default`.
    pub evidence: String,
}

/// The edge filter `edges` with the evidence policy `evidence` (see [`kc_store::answer::EvidencePolicy`]).
pub fn edge_filter(edges: &[String], evidence: &str) -> Result<trace::EdgeFilter> {
    let policy = kc_store::answer::EvidencePolicy::parse(evidence).map_err(anyhow::Error::msg)?;
    Ok(trace::EdgeFilter::new(edges)?.with_policy(policy))
}

/// Says in `env` what the evidence policy of `filter` left out: `policy`, and an `incomplete` entry when it left out
/// anything (see [`kc_store::answer::PolicyReport`]). Nothing under `default`.
pub fn report_policy(env: &mut Value, filter: &trace::EdgeFilter) {
    if let Some(r) = filter.report() {
        env["policy"] = json!(r);
    }
}

/// The evidence policy of `filter` as a cursor keeps it: empty for `default`, however it was written.
pub fn policy_word(filter: &trace::EdgeFilter) -> String {
    if filter.policy().is_default() { String::new() } else { filter.policy().name().to_string() }
}

impl WalkFilter {
    /// The filter as a walk's parameters, for a call continuing it: `edges`, `kinds` and `evidence` when set.
    pub fn args(&self, edges: &trace::EdgeFilter) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        if !self.edges.is_empty() {
            m.insert("edges".into(), json!(self.edges));
        }
        if !self.kinds.is_empty() {
            m.insert("kinds".into(), json!(self.kinds));
        }
        let policy = policy_word(edges);
        if !policy.is_empty() {
            m.insert("evidence".into(), policy.into());
        }
        m
    }

    fn edges(&self) -> Result<trace::EdgeFilter> {
        edge_filter(&self.edges, &self.evidence)
    }

    fn kinds(&self) -> Result<Vec<String>> {
        let kinds: Vec<String> = self.kinds.iter().flat_map(|k| k.split(',')).map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
        if let Some(bad) = kinds.iter().find(|k| !SYMBOL_KINDS.contains(&k.as_str())) {
            bail!("unknown symbol kind {bad}; one of {}", SYMBOL_KINDS.join(", "));
        }
        Ok(kinds)
    }

    /// Whether a symbol kind is listed.
    fn keeps(kinds: &[String], kind: &str) -> bool {
        kinds.is_empty() || kinds.iter().any(|k| query::kind_matches(Some(k), kind))
    }

    /// The filter as the answer reports it, when there is one.
    fn describe(&self) -> Option<Value> {
        let kinds = self.kinds().unwrap_or_default();
        let edges: Vec<&str> = self.edges.iter().flat_map(|e| e.split(',')).map(str::trim).filter(|e| !e.is_empty()).collect();
        (!edges.is_empty() || !kinds.is_empty()).then(|| json!({ "edges": edges, "kinds": kinds }))
    }
}

/// The symbol kinds `--kinds` takes: the extractors' kinds, and the groups `type` and `func` (or `callable`).
const SYMBOL_KINDS: &[&str] = &[
    "type", "func", "callable", "class", "struct", "enum", "actor", "protocol", "extension", "function", "method", "init",
    "subscript", "property", "case", "typealias", "template", "table", "view", "procedure", "trigger", "event", "interface",
    "trait", "constructor",
];

/// The word a language uses for a symbol kind: the index names kinds in Swift's terms (`protocol`, `init`), which
/// C#, TypeScript and Python call `interface` and `constructor`, and Rust calls a trait. `lang` is a file's
/// language (`swift`, `csharp`, …) as [`file_lang`] or a repo's [`repo_lang`] gives it.
pub fn kind_word<'a>(kind: &'a str, lang: &str) -> &'a str {
    match (kind, lang) {
        (_, "swift") => kind,
        ("protocol", "rust") => "trait",
        ("protocol", _) => "interface",
        ("init", _) => "constructor",
        ("typealias", "typescript") => "type",
        _ => kind,
    }
}

/// A file's language for [`kind_word`], by extension.
pub fn file_lang(file: &str) -> &'static str {
    match file.rsplit('.').next().unwrap_or("") {
        "swift" => "swift",
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => "typescript",
        "cs" => "csharp",
        "py" => "python",
        _ => "other",
    }
}

/// A repo's language for [`kind_word`], from its file counts by language: `swift` when it has any Swift (whose
/// words the index uses), else the code language with the most files.
pub fn repo_lang(files_by_lang: &serde_json::Map<String, Value>) -> String {
    if files_by_lang.contains_key("swift") {
        return "swift".into();
    }
    files_by_lang
        .iter()
        .filter(|(l, _)| matches!(l.as_str(), "rust" | "typescript" | "csharp" | "python" | "cfml"))
        .max_by_key(|(_, n)| n.as_u64().unwrap_or(0))
        .map_or_else(|| "other".into(), |(l, _)| l.clone())
}

pub struct TraceData {
    pub found: Vec<(PathBuf, Trace)>,
    pub multi: bool,
    /// Nodes the kind filter left out, per found trace.
    pub hidden: Vec<usize>,
}

pub fn trace(symbol: &str, direction: &str, depth: u8, limit: usize, filter: &WalkFilter, args: &Scope) -> Result<Out<TraceData>> {
    let direction_word = direction;
    let direction = match direction {
        "in" => trace::Direction::In,
        "out" => trace::Direction::Out,
        "both" => trace::Direction::Both,
        other => bail!("direction must be in, out or both, not {other}"),
    };
    let edges = filter.edges()?;
    let kinds = filter.kinds()?;
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    let mut hidden = Vec::new();
    let mut ci_cuts: Vec<Vec<String>> = Vec::new();
    let mut frontiers: Vec<(Vec<trace::Frontier>, usize)> = Vec::new();
    let mut cite_notes: Vec<Incomplete> = Vec::new();
    for root in &roots {
        let Some(mut o) = open_member(&roots, root, args.no_sync)? else { continue };
        let mut start = query::definitions(o.store.conn(), symbol)?.into_iter().next();
        // A role the repo's plays use but another repo defines: traced there.
        if start.is_none()
            && let Some((droot, at)) = role_defined_elsewhere(&o, symbol)?
        {
            let d = open_fresh(&droot, args.no_sync)?;
            start = query::definitions(d.store.conn(), &at)?.into_iter().next();
            if start.is_some() {
                opened.push(std::mem::replace(&mut o, d));
            }
        }
        if let Some(start) = start {
            // The walk reads every edge through the symbol graph and its gate, as `path` and `flow` do.
            let mut g = crate::walk::Graph::new(&o, &[], args.no_sync, usize::MAX).with_edges(edges.clone());
            let mut t = crate::tree::trace(&mut g, &start, direction, depth, limit)?;
            // Where the tree stopped short: nodes it listed without growing that lead to symbols it doesn't list.
            let listed: HashSet<(Option<String>, i64)> = t.nodes.iter().map(|n| (n.repo.clone(), n.id)).collect();
            let stops = std::mem::take(&mut t.stopped);
            let budget = if t.truncated { 0 } else { FRONTIER_CHECKS };
            let mut fr = g.crossing().frontier(&stops, &|repo, id| listed.contains(&(repo.map(str::to_string), id)), budget)?;
            // Each stop's exact call going on from it, the way the walk went.
            let mut go_on = filter.args(&edges);
            go_on.insert("depth".into(), depth.clamp(1, trace::MAX_TRACE_DEPTH).into());
            crate::tree::continuations(&mut g, &mut fr.0, "kc_trace", &go_on, |f, call| {
                call.insert("direction".into(), f.dir.into());
            })?;
            frontiers.push(fr);
            ci_cuts.push(std::mem::take(&mut g.crossing().ci_cut));
            let mut citer = crate::cite::Citer::new([]).with_stores(g.stores());
            citer.trace(&o.root, &mut t);
            cite_notes.extend(citer.incomplete());
            drop(citer);
            drop(g);
            hidden.push(if kinds.is_empty() { 0 } else { t.retain_kinds(|k| WalkFilter::keeps(&kinds, k)) });
            found.push((o.root.clone(), t));
        }
        opened.push(o);
    }
    let items: Vec<_> = found
        .iter()
        .zip(&hidden)
        .zip(&ci_cuts)
        .zip(&frontiers)
        .map(|((((root, t), h), cut), (frontier, _))| {
            let mut v = json!({ "repo": repo_name(root), "nodes": t.nodes, "truncated": t.truncated });
            if !frontier.is_empty() {
                v["frontier"] = json!(frontier);
            }
            if t.omitted > 0 {
                v["omitted"] = json!(t.omitted);
            }
            // Deploy chains a CI limit cut: a deploy hop may be missing.
            if !cut.is_empty() {
                v["ci_cut"] = json!(cut);
            }
            if *h > 0 {
                v["hidden"] = json!(h);
            }
            v
        })
        .collect();
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    if let Some(f) = filter.describe() {
        env["filter"] = f;
    }
    if found.iter().any(|(_, t)| t.truncated) {
        env["truncated"] = json!(true);
    }
    report_policy(&mut env, &edges);
    for i in cite_notes {
        push_incomplete(&mut env, i);
    }
    let walked = depth.clamp(1, trace::MAX_TRACE_DEPTH);
    if depth > trace::MAX_TRACE_DEPTH {
        // The tree is cut at its deepest: said, and its frontier named below.
        env["depth"] = json!({ "asked": depth, "walked": walked });
    }
    for ((root, t), (frontier, unchecked)) in found.iter().zip(&frontiers) {
        let asked = (depth > walked).then_some(depth);
        for i in frontier_incomplete(&repo_name(root), frontier, *unchecked, (walked, trace::MAX_TRACE_DEPTH, asked, t.truncated), "trace", "neighbours") {
            push_incomplete(&mut env, i);
        }
    }
    let lists = found.iter().enumerate().map(|(i, (_, t))| cursor::List::new(format!("/items/{i}/nodes"), limit, t.truncated)).collect();
    let req = cursor::Request::Trace {
        symbol: symbol.to_string(),
        direction: direction_word.to_string(),
        depth,
        limit,
        edges: filter.edges.clone(),
        kinds: filter.kinds.clone(),
        evidence: policy_word(&edges),
    };
    cursor::offer(&mut env, req, args, lists, &[]);
    finish(&mut env);
    Ok(Out { env, data: TraceData { found, multi: roots.len() > 1, hidden } })
}

/// Which indexed repo declares each package, for registry dependencies.
/// Repos this one depends on through its manifests (path dependencies that land in another repo, and registry
/// dependencies on a package another indexed repo declares), opened. With `name`, only those providing a module
/// that a file referencing `name` imports, since only those can resolve its references; module lists come from the
/// workspace summary, so the others are never opened.
pub fn open_dependencies(o: &Opened, name: Option<&str>, no_sync: bool) -> Result<Vec<OpenedDep>> {
    let imported = name.map(|n| query::imports_of_files_referencing(o.store.conn(), n)).transpose()?;
    open_dependencies_importing(o, imported.as_deref(), no_sync)
}

/// [`open_dependencies`], keeping only those providing a module one of `imported` names (every one with `None`).
pub fn open_dependencies_importing(o: &Opened, imported: Option<&[String]>, no_sync: bool) -> Result<Vec<OpenedDep>> {
    let pkgs = manifests::packages(o.store.conn())?;
    let summaries = planned_summaries(o)?;
    let graph = kc_index::workspace::graph(&summaries);
    let me = kc_index::workspace::identity_of(&o.root, &summaries);
    let mut edges = manifests::repo_edges(&pkgs, &o.root.to_string_lossy(), &me, graph.owners());
    kc_index::revision::check_edges(&mut edges, &summaries);
    let mut roots: Vec<PathBuf> = edges.iter().map(|e| PathBuf::from(&e.to_repo)).collect();
    roots.sort();
    roots.dedup();
    roots
        .iter()
        .filter(|r| match (imported, summaries.iter().find(|s| &s.root == *r)) {
            (Some(imp), Some(s)) => imp.iter().any(|i| s.modules.iter().any(|m| query::imports_module(i, m))),
            // Unfiltered, or a path dependency not indexed yet: opening it indexes it.
            _ => true,
        })
        .filter(|r| {
            // An index whose checkout is gone: what the dependency would resolve is missing.
            let there = r.join(".git").exists();
            if !there {
                o.consulted.missing(r);
            }
            there
        })
        .map(|r| {
            let store = open_synced(o, r, no_sync)?;
            // The summary's modules are current unless the sync above just changed the index.
            let modules = match summaries.iter().find(|s| &s.root == r) {
                Some(s) if no_sync => s.modules.clone(),
                _ => query::provided_modules(store.conn())?,
            };
            Ok(OpenedDep { assumptions: kc_index::revision::assumptions_into(&edges, r), root: r.clone(), store, modules })
        })
        .collect()
}

/// A repo's index, synced unless `no_sync`, for a repo consulted on the side of `o` (a dependency, a dependent, a repo
/// crossed): recorded in `o`'s [`Consulted`] for the answer's `provenance`, without running `git status`.
pub(crate) fn open_synced(o: &Opened, root: &Path, no_sync: bool) -> Result<Store> {
    let (store, report) = crate::refresh::open(root, no_sync)?;
    if !no_sync && report.is_none() {
        o.consulted.pending.borrow_mut().push(root.to_path_buf());
    }
    o.consulted.record(root, &store, report.as_ref());
    Ok(store)
}

/// A join needs facts a light scan doesn't contain (HTTP, bus, SQL, Angular or deployment facts). Touch those
/// candidates lazily, under the same query freshness budget, and record every pending or failed sync.
pub(crate) fn full_summaries(o: &Opened) -> Result<std::sync::Arc<Vec<kc_index::workspace::RepoSummary>>> {
    ensure_summaries(o, std::slice::from_ref(&o.root), true)
}

pub(crate) fn planned_summaries(o: &Opened) -> Result<std::sync::Arc<Vec<kc_index::workspace::RepoSummary>>> {
    ensure_summaries(o, std::slice::from_ref(&o.root), false)
}

pub(crate) fn full_workspace(opened: &[Opened], roots: &[PathBuf]) -> Result<std::sync::Arc<Vec<kc_index::workspace::RepoSummary>>> {
    match opened.first() { Some(o) => full_summaries_for(o, roots), None => kc_index::workspace::summaries(roots) }
}

pub(crate) fn full_summaries_for(o: &Opened, focus: &[PathBuf]) -> Result<std::sync::Arc<Vec<kc_index::workspace::RepoSummary>>> {
    ensure_summaries(o, focus, true)
}

fn ensure_summaries(o: &Opened, focus: &[PathBuf], full: bool) -> Result<std::sync::Arc<Vec<kc_index::workspace::RepoSummary>>> {
    let no_sync = o.sync.is_none() && !o.refresh_pending;
    let summaries = kc_index::workspace::summaries(focus)?;
    for s in summaries.iter().filter(|s| s.blocked.whole && !s.gone) {
        o.consulted.withheld.borrow_mut().push(s.root.clone());
    }
    for s in summaries.iter().filter(|s| s.light && !s.blocked.whole && !s.gone && s.root != o.root && (full || !kc_index::light::unchanged(&s.root))) {
        if no_sync {
            o.consulted.partial.borrow_mut().push(Incomplete::new(Reason::Stale, format!("{}: only manifests and names were scanned; graph joins are not indexed", s.root.display()))
                .with_hint("ask again without --no-sync to index the needed checkout automatically"));
        } else {
            // A checkout that can't be read (gone since the summaries were read, a failing `git`) is left out.
            let (store, report) = match crate::refresh::open(&s.root, false) {
                Ok(opened) => opened,
                Err(e) if kc_store::limits::cancelled() => return Err(e),
                Err(e) => {
                    o.consulted.partial.borrow_mut().push(unreadable(&s.root, &e));
                    continue;
                }
            };
            o.consulted.record(&s.root, &store, report.as_ref());
            match report {
                None => o.consulted.pending.borrow_mut().push(s.root.clone()),
                Some(report) if !report.failed.is_empty() => o.consulted.partial.borrow_mut().push(Incomplete::new(Reason::Failed, format!("{}: files could not be read while building its full index", s.root.display()))
                    .with_hint("check this checkout's status, fix unreadable files, and ask again")),
                _ => {},
            }
        }
    }
    kc_index::workspace::summaries(focus)
}

/// A dependency opened to resolve references into: unlike [`Opened`], its HEAD is not read, since answers report
/// only the queried repos'.
pub struct OpenedDep {
    pub root: PathBuf,
    pub store: Store,
    /// Modules it provides, matched against imports.
    pub modules: Vec<String>,
    /// What resolving into it assumes: `revision_unverified` when the registry version asked for isn't known to be
    /// the checkout's.
    pub assumptions: Vec<Assumption>,
}

pub fn dependency_refs(deps: &[OpenedDep]) -> Vec<query::Dependency<'_>> {
    deps.iter()
        .map(|d| query::Dependency { repo: repo_name(&d.root), conn: d.store.conn(), modules: d.modules.clone(), assumptions: d.assumptions.clone() })
        .collect()
}

/// The repos depending on `o` (indexes into `summaries`) whose summary `may` accepts, or that changed since their
/// summary was written (unless `no_sync`): a summary rules a repo out only while it still describes the repo.
fn dependents_to_open(
    o: &Opened,
    summaries: &[kc_index::workspace::RepoSummary],
    graph: &kc_index::workspace::Graph,
    may: impl Fn(&kc_index::workspace::RepoSummary) -> bool,
    no_sync: bool,
) -> Vec<usize> {
    let me = kc_index::workspace::identity_of(&o.root, summaries);
    let (gone, dependents): (Vec<usize>, Vec<usize>) =
        graph.dependents(&me).into_iter().filter(|&i| summaries[i].root != o.root).partition(|&i| summaries[i].gone);
    // A dependent whose checkout is gone, that may use what is asked for: what it would have said is missing.
    for i in gone.into_iter().filter(|&i| may(&summaries[i])) {
        o.consulted.missing(&summaries[i].root);
    }
    let ruled_out: Vec<PathBuf> = dependents.iter().filter(|&&i| !may(&summaries[i])).map(|&i| summaries[i].root.clone()).collect();
    let changed = changed_unless(no_sync, &ruled_out);
    dependents.into_iter().filter(|&i| may(&summaries[i]) || changed.contains(&summaries[i].root)).collect()
}

/// References to `name` in indexed repos that depend on `o`, resolved into it. Each hit's `repo` names the
/// dependent repo, and only hits with a target in `o` are kept. A dependent whose summary shows no reference named
/// `name` isn't opened (unless it changed since: see [`dependents_to_open`]), and in the others only references in
/// files importing `o` are resolved.
pub fn dependent_refs(o: &Opened, name: &str, kinds: &[&str], no_sync: bool) -> Result<Vec<query::RefHit>> {
    let me_name = repo_name(&o.root);
    let summaries = planned_summaries(o)?;
    let graph = kc_index::workspace::graph(&summaries);
    let mut modules = None;
    let mut out = Vec::new();
    for i in dependents_to_open(o, &summaries, &graph, |s| s.may_reference(name.trim()), no_sync) {
        let s = &summaries[i];
        let dependent = open_synced(o, &s.root, no_sync)?;
        let modules = match &modules {
            Some(m) => m,
            None => modules.insert(query::provided_modules(o.store.conn())?),
        };
        // What the dependent asks for of this repo, against the version this checkout declares.
        let mut edges = manifests::repo_edges(&s.packages, &s.root.to_string_lossy(), &s.identity(), graph.owners());
        edges.retain(|e| Path::new(&e.to_repo) == o.root);
        kc_index::revision::check_edges(&mut edges, &summaries);
        let assumptions = kc_index::revision::assumptions_into(&edges, &o.root);
        let dep = query::Dependency { repo: me_name.clone(), conn: o.store.conn(), modules: modules.clone(), assumptions };
        for mut h in query::refs_importing(dependent.conn(), name, kinds, std::slice::from_ref(&dep))? {
            if h.targets.iter().any(|t| t.repo.as_deref() == Some(me_name.as_str())) {
                h.repo = Some(repo_name(&s.root));
                out.push(h);
            }
        }
    }
    Ok(out)
}

/// Types in indexed repos depending on `o` that declare `ty` (a class or interface of `o`) as a supertype, matched by
/// its namespace: a library's interface implemented, or its base class extended, by the code using the library. With
/// `member` (a member of `ty`), their same-named members of its kind instead: the implementations of an interface
/// member. A dependent whose summary names `ty` nowhere isn't opened, unless it changed since.
pub fn dependent_conformers(o: &Opened, ty: &query::SymbolRow, member: Option<&query::SymbolRow>, no_sync: bool) -> Result<Vec<query::Elsewhere>> {
    if !matches!(ty.kind.as_str(), "class" | "struct" | "protocol") {
        return Ok(Vec::new());
    }
    let ns = query::type_namespace(o.store.conn(), ty.id)?;
    let summaries = planned_summaries(o)?;
    let graph = kc_index::workspace::graph(&summaries);
    let mut out = Vec::new();
    for i in dependents_to_open(o, &summaries, &graph, |s| s.may_reference(&ty.name), no_sync) {
        let s = &summaries[i];
        let dependent = open_synced(o, &s.root, no_sync)?;
        for c in query::conformers_named(dependent.conn(), &ty.name, ns.as_deref())? {
            let found = match member {
                None => vec![c],
                Some(m) => query::definitions(dependent.conn(), &format!("{}.{}", c.qualified(), m.name))?
                    .into_iter()
                    .filter(|d| d.kind == m.kind && d.container.as_deref() == Some(c.qualified().as_str()))
                    .collect(),
            };
            out.extend(found.into_iter().map(|symbol| query::Elsewhere { repo: repo_name(&s.root), symbol }));
        }
    }
    Ok(out)
}

/// At most this many of a type's conformers in other repos are listed in its context.
const CONFORMERS_ELSEWHERE: usize = 50;

/// [`dependent_conformers`] as references to `target` (the type or member of `o`, the repo `target_repo`) for
/// `impact`'s `cross_repo`: each conformer's declaration, or the implementing member, as the dependent's use of it.
fn conformer_hits(target: &query::SymbolRow, target_repo: &str, found: Vec<query::Elsewhere>) -> Vec<RefHit> {
    found
        .into_iter()
        .map(|e| {
            let s = e.symbol;
            RefHit {
                code: None,
                precise: false,
                corrected_from: None,
                name: target.name.clone(),
                kind: if query::is_type_or_extension(&s.kind) { "inherits".into() } else { "implements".into() },
                receiver: None,
                receiver_type: None,
                file: s.file.clone(),
                line: s.line,
                col: 0,
                from: Some(query::FromSymbol { id: s.id, name: s.name.clone(), kind: s.kind.clone(), container: s.container.clone() }),
                confidence: "exact",
                targets: vec![query::Target {
                    id: target.id,
                    qualified: target.qualified(),
                    kind: target.kind.clone(),
                    file: target.file.clone(),
                    line: target.line,
                    repo: Some(target_repo.to_string()),
                }],
                repo: Some(e.repo),
                receiver_chain: Vec::new(),
                target_ids: vec![target.id],
                targets_total: None,
                via_di: None,
                via_di_any: Vec::new(),
                usage: None,
                ctors: None,
                cite: None,
                args: None,
                arg_types: None,
                assigns: false,
                factory: false,
                through: None,
                basis: None,
                evidence: Some(Evidence::new(Mechanism::Declaration)),
            }
        })
        .collect()
}

/// References in other repos to a role this repo defines (`name`, its role symbol's name), linked as
/// `kc_index::ansible` links them.
fn role_refs_elsewhere(o: &Opened, name: &str, no_sync: bool) -> Result<Vec<query::RefHit>> {
    let roles: Vec<query::SymbolRow> =
        query::definitions(o.store.conn(), name)?.into_iter().filter(|s| s.kind == "role" && !s.tags.contains("meta")).collect();
    if roles.is_empty() {
        return Ok(Vec::new());
    }
    let summaries = full_summaries(o)?;
    let a = kc_index::ansible::Ansible::new(&summaries);
    let mut out = Vec::new();
    for role in &roles {
        let dir = kc_extract::ansible::place(&role.file).and_then(|p| p.role_dir).unwrap_or_default();
        for (repo, u, confidence) in a.role_users(&o.root, Some(&dir)) {
            if !repo.join(".git").exists() {
                continue;
            }
            let store = open_synced(o, &repo, no_sync)?;
            let from = ansible_symbol_at(store.conn(), &u.file, u.line)?
                .map(|s| query::FromSymbol { id: s.id, name: s.name, kind: s.kind, container: s.container });
            let target = query::Target { id: role.id, qualified: role.qualified(), kind: role.kind.clone(), file: role.file.clone(), line: role.line, repo: None };
            let code = std::fs::read_to_string(repo.join(&u.file))
                .ok()
                .and_then(|t| t.lines().nth(u.line.saturating_sub(1) as usize).map(|l| l.trim().to_string()));
            out.push(query::RefHit {
                code,
                precise: false,
                corrected_from: None,
                name: u.name,
                kind: "role".into(),
                receiver: None,
                receiver_type: None,
                file: u.file,
                line: u.line,
                col: 0,
                from,
                confidence,
                targets: vec![target],
                repo: Some(repo_name(&repo)),
                receiver_chain: Vec::new(),
                target_ids: vec![role.id],
                targets_total: None,
                via_di: None,
                via_di_any: Vec::new(),
                usage: None,
                ctors: None,
                cite: None,
                args: None,
                arg_types: None,
                assigns: false,
                factory: false,
                through: None,
                basis: None,
                evidence: Evidence::by_confidence(confidence, Mechanism::Convention).map(|e| e.assuming(Assumption::DirectoryConvention)),
            });
        }
    }
    Ok(out)
}

/// Links this repo's uses of roles defined elsewhere (`external_likely` or `unresolved` here) to the repos defining
/// them, as `context` does (see [`kc_index::ansible::Ansible::role`]): through the `src` of the `requirements.yml` or
/// meta dependency naming the role (`exact` when an indexed repo's origin is that URL, `unique_name` when one repo's
/// directory is its last segment), else by name or meta `role_name` (`unique_name`, or `ambiguous` across same-named
/// role repos). A use nothing links stays as it was. Returns whether any hit changed.
fn link_role_uses(o: &Opened, hits: &mut [query::RefHit], no_sync: bool) -> Result<bool> {
    let open = |h: &query::RefHit| h.kind == "role" && h.repo.is_none() && matches!(h.confidence, "external_likely" | "unresolved");
    if !hits.iter().any(open) {
        return Ok(false);
    }
    let summaries = full_summaries(o)?;
    let a = kc_index::ansible::Ansible::new(&summaries);
    let mut stores: HashMap<PathBuf, Option<Store>> = HashMap::new();
    let mut changed = false;
    for h in hits.iter_mut().filter(|h| open(h)) {
        let links = a.role(&o.root, &h.name, &h.file);
        if links.is_empty() {
            continue;
        }
        let mut targets = Vec::new();
        for l in &links {
            if !stores.contains_key(&l.repo) {
                let store = if l.repo.join(".git").exists() { Some(open_synced(o, &l.repo, no_sync)?) } else { None };
                stores.insert(l.repo.clone(), store);
            }
            let id: Option<i64> = match stores.get(&l.repo).and_then(Option::as_ref) {
                Some(st) => st
                    .conn()
                    .prepare_cached("SELECT s.id FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.path = ?1 AND s.start_line = ?2 AND s.kind = 'role' LIMIT 1")?
                    .query_row(rusqlite::params![l.role.file, l.role.line], |r| r.get(0))
                    .ok(),
                None => None,
            };
            targets.push(query::Target {
                id: id.unwrap_or(0),
                qualified: l.role.name.clone(),
                kind: "role".into(),
                // `tasks_from`: the role's task file it names.
                file: match &h.receiver {
                    Some(t) if l.role.dir.is_empty() => format!("tasks/{t}"),
                    Some(t) => format!("{}/tasks/{t}", l.role.dir),
                    None => l.role.file.clone(),
                },
                line: if h.receiver.is_some() { 1 } else { l.role.line },
                repo: Some(repo_name(&l.repo)),
            });
        }
        targets.sort_by(|a, b| (&a.repo, &a.file).cmp(&(&b.repo, &b.file)));
        h.confidence = links[0].confidence;
        // Exact through a requirements `src` naming the repo, as an import names a module; else by the role's name.
        h.evidence = Evidence::by_confidence(h.confidence, Mechanism::Import);
        h.target_ids = targets.iter().map(|t| t.id).filter(|id| *id != 0).collect();
        h.targets = targets;
        changed = true;
    }
    Ok(changed)
}

/// The CI job `name` defined in `file`.
fn ci_job_at(conn: &rusqlite::Connection, file: &str, name: &str) -> Result<Option<query::SymbolRow>> {
    let id: rusqlite::Result<i64> = conn
        .prepare_cached("SELECT s.id FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.path = ?1 AND s.name = ?2 AND s.kind = 'job' LIMIT 1")?
        .query_row(rusqlite::params![file, name], |r| r.get(0));
    match id {
        Ok(id) => trace::symbol(conn, id),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The innermost Ansible play, task, role or file symbol holding `line` of `file`: what a reference there comes from.
fn ansible_symbol_at(conn: &rusqlite::Connection, file: &str, line: u32) -> Result<Option<query::SymbolRow>> {
    let id: rusqlite::Result<i64> = conn
        .prepare_cached(
            "SELECT s.id FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.path = ?1 AND s.start_line <= ?2 AND s.end_line >= ?2 AND s.kind IN ('play', 'task', 'handler', 'role', 'tasks', 'playbook')
             ORDER BY s.end_line - s.start_line, s.start_line DESC LIMIT 1",
        )?
        .query_row(rusqlite::params![file, line], |r| r.get(0));
    let id = match id {
        Ok(id) => Some(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    match id {
        Some(id) => trace::symbol(conn, id),
        None => Ok(None),
    }
}

/// Code that depends on a symbol across repos, in one repo: across an HTTP route, the symbols creating the contract
/// the route links to a controller action; across the bus, the consumers of a message the symbol is or publishes.
pub(crate) struct Landing {
    pub(crate) root: PathBuf,
    pub(crate) via: trace::Via,
    pub(crate) creators: Vec<trace::Creator>,
    /// For the bus from a publisher, the publish call (`file:line` in the publisher's repo): where the hop leaves.
    pub(crate) publish: Option<String>,
}

/// Code using a SQL definition (see [`HttpCrossing::sql_uses`]): an Entity Framework table, or a procedure call, in
/// the repo at `root`.
pub(crate) struct SqlUse {
    pub(crate) root: PathBuf,
    pub(crate) confidence: &'static str,
    /// What the link did not check (see [`kc_index::sql::SqlLink::assumptions`]).
    pub(crate) assumptions: Vec<Assumption>,
    pub(crate) table: Option<db::Table>,
    pub(crate) procedure: Option<db::Procedure>,
}

impl SqlUse {
    fn out(self, max_accessors: usize) -> SqlUseOut {
        let repo = repo_name(&self.root);
        match (self.table, self.procedure) {
            (Some(mut t), _) => {
                t.cap_accessors(max_accessors);
                SqlUseOut {
                    repo,
                    how: "entity",
                    symbol: t.entity,
                    context: t.context,
                    name: t.table,
                    file: t.file,
                    line: t.line,
                    confidence: self.confidence,
                    reads: Some(t.reads),
                    writes: Some(t.writes),
                    accessors: t.accessors,
                    accessors_total: t.accessors_total,
                }
            }
            (None, Some(p)) => SqlUseOut {
                repo,
                how: "call",
                symbol: p.symbol,
                context: None,
                name: p.name,
                file: p.file,
                line: p.line,
                confidence: self.confidence,
                reads: None,
                writes: None,
                accessors: Vec::new(),
                accessors_total: None,
            },
            (None, None) => unreachable!("a use is a table or a procedure"),
        }
    }
}

/// Crosses HTTP routes and the message bus for `impact` and `trace`: from a controller action to the code, in any
/// indexed repo, that creates a contract requesting it; from a message type, or code publishing one, to the consumers
/// handling it. The workspace summary is read, and other repos opened, only when a symbol is an action, a message
/// type or a publisher; opened repos stay open for the rest of the walk.
pub(crate) struct HttpCrossing<'a> {
    home: &'a Opened,
    summaries: Option<std::sync::Arc<Vec<kc_index::workspace::RepoSummary>>>,
    /// Every repo's deploy chains, worked out at the first CI job, playbook or role crossed.
    chains: Option<Vec<kc_index::ci::Chain>>,
    stores: Vec<(PathBuf, Store)>,
    no_sync: bool,
    /// The edges `impact` and `trace` follow (an `http`, `bus` or `sql` crossing is an edge too), with their evidence
    /// policy, for the query the walk runs for (see [`crate::graph::Gate`]).
    gate: crate::graph::Gate,
    /// Repos that may be open at once, the home repo included (`path` and `flow` set it).
    max_repos: usize,
    /// A repo was left closed because `max_repos` were open.
    pub(crate) limited: bool,
    /// What the CI limits left out of the pipelines read for deploy chains (`repo: what`), once read.
    pub(crate) ci_cut: Vec<String>,
}

impl<'a> HttpCrossing<'a> {
    pub(crate) fn new(home: &'a Opened, no_sync: bool) -> Self {
        HttpCrossing {
            home,
            summaries: None,
            chains: None,
            stores: Vec::new(),
            no_sync,
            gate: crate::graph::Gate::default(),
            max_repos: usize::MAX,
            limited: false,
            ci_cut: Vec::new(),
        }
    }

    /// Follows only the edges `filter` allows (see [`crate::graph::Gate`]).
    pub(crate) fn set_filter(&mut self, filter: &trace::EdgeFilter) {
        self.gate = crate::graph::Gate::new(filter.clone());
    }

    /// Opens at most `n` repos, the home repo included.
    pub(crate) fn limit(mut self, n: usize) -> Self {
        self.max_repos = n.max(1);
        self
    }

    /// Opens a repo unless it is open already; false when `max_repos` are open, leaving it closed.
    pub(crate) fn open(&mut self, root: &Path) -> Result<bool> {
        if root != self.home.root && !self.stores.iter().any(|(r, _)| r == root) {
            if self.stores.len() + 1 >= self.max_repos {
                self.limited = true;
                return Ok(false);
            }
            let store = open_synced(self.home, root, self.no_sync)?;
            self.stores.push((root.to_path_buf(), store));
        }
        Ok(true)
    }

    /// The index of a repo already opened.
    pub(crate) fn store(&self, root: &Path) -> &Store {
        if root == self.home.root {
            return &self.home.store;
        }
        self.stores.iter().find(|(r, _)| r == root).map(|(_, s)| s).expect("repo opened before use")
    }

    /// Repos open, the home repo included.
    pub(crate) fn opened(&self) -> usize {
        self.stores.len() + 1
    }

    /// The root of the queried repo.
    pub(crate) fn home_root(&self) -> PathBuf {
        self.home.root.clone()
    }

    /// Whether the repo at `root` is open.
    pub(crate) fn opened_root(&self, root: &Path) -> bool {
        root == self.home.root || self.stores.iter().any(|(r, _)| r == root)
    }

    /// The crossings from `qualified` (symbol `id`, defined in `file` of the repo at `root`, which is open) of the edge
    /// kinds the walk follows, each arrival at its confidence across the link (see [`crate::graph::Crossing`]): what
    /// `impact`, `trace` and the frontier check read across repos.
    pub(crate) fn crossings(&mut self, root: &Path, id: i64, qualified: &str, file: &str) -> Result<Vec<crate::graph::Crossing>> {
        let landings = self.landings(root, id, qualified, file)?;
        Ok(landings
            .into_iter()
            .filter(|l| self.gate.allows(l.via.edge()))
            .map(|l| crate::graph::Crossing::new(l.root, l.via, l.publish, l.creators))
            .collect())
    }

    /// The code depending on `qualified` (symbol `id`, defined in `file` of the repo at `root`, which is open) across
    /// repos: requesting it, when it is an action; consuming what it is or publishes, when it is a message type or a
    /// publisher. One landing per repo and hop; nothing for other symbols.
    fn landings(&mut self, root: &Path, id: i64, qualified: &str, file: &str) -> Result<Vec<Landing>> {
        let conn = self.store(root).conn();
        let serves = kc_index::http::serves(conn, id)?;
        let bus = kc_store::bus::involved(conn, id)?;
        let sql = if kc_store::sql::is_sql_path(file) { trace::symbol(conn, id)?.filter(kc_store::sql::is_object) } else { None };
        let role = if kc_extract::ansible::place(file).is_some() { trace::symbol(conn, id)?.filter(|s| s.kind == "role") } else { None };
        // A CI job or template, a playbook or a role: the application jobs deploying with it.
        let yaml = file.ends_with(".yml") || file.ends_with(".yaml");
        let deployer = if yaml {
            trace::symbol(conn, id)?.filter(|s| s.kind == "job" || (matches!(s.kind.as_str(), "playbook" | "role") && !s.tags.contains("meta")))
        } else {
            None
        };
        // A Terraform module's variable or output: the module calls in other repos setting or reading it.
        let module_api = if kc_store::terraform::is_terraform_path(file) {
            trace::symbol(conn, id)?.filter(|s| matches!(s.kind.as_str(), "variable" | "output"))
        } else {
            None
        };
        if !serves && !bus && sql.is_none() && role.is_none() && deployer.is_none() && module_api.is_none() {
            return Ok(Vec::new());
        }
        if self.summaries.is_none() {
            self.summaries = Some(full_summaries(self.home)?);
        }
        let summaries = self.summaries.take().unwrap_or_default();
        let mut all = || -> Result<Vec<Landing>> {
            let line = if serves || bus { trace::symbol(self.store(root).conn(), id)?.map_or(0, |s| s.line) } else { 0 };
            let mut out = if serves { self.landings_with(root, qualified, file, line, &summaries)? } else { Vec::new() };
            if bus {
                out.extend(self.consumers(root, qualified, file, line, &summaries)?);
            }
            if let Some(sym) = &sql {
                out.extend(self.sql_landings(root, sym, &summaries)?);
            }
            if let Some(sym) = &role {
                out.extend(self.role_landings(root, sym, &summaries)?);
            }
            if let Some(sym) = &module_api {
                out.extend(self.module_landings(root, sym, &summaries)?);
            }
            if let Some(sym) = &deployer {
                out.extend(self.deploy_landings(root, sym, &summaries)?);
            }
            Ok(out)
        };
        let out = all();
        self.summaries = Some(summaries);
        out
    }

    /// The plays, tasks and roles in other repos using `sym`, a role of the repo at `root`, through a requirement
    /// naming the repo or by the role's name; one landing per repo.
    fn role_landings(&mut self, root: &Path, sym: &query::SymbolRow, summaries: &[kc_index::workspace::RepoSummary]) -> Result<Vec<Landing>> {
        let dir = kc_extract::ansible::place(&sym.file).and_then(|p| p.role_dir).unwrap_or_default();
        let users = kc_index::ansible::Ansible::new(summaries).role_users(root, Some(&dir));
        let mut out: Vec<Landing> = Vec::new();
        for (repo, u, link) in users {
            if !repo.join(".git").exists() {
                continue;
            }
            self.open(&repo)?;
            let Some(from) = ansible_symbol_at(self.store(&repo).conn(), &u.file, u.line)? else { continue };
            let creator = (from, "exact", Some(format!("{}:{}", u.file, u.line)));
            match out.iter_mut().find(|l| l.root == repo && l.via.link() == link) {
                Some(l) => {
                    if !l.creators.iter().any(|c| c.0.id == creator.0.id) {
                        l.creators.push(creator);
                    }
                }
                None => {
                    let via = trace::Via::Role(trace::RoleVia { role: u.name.clone(), link });
                    out.push(Landing { root: repo.clone(), via, creators: vec![creator], publish: None });
                }
            }
        }
        Ok(out)
    }

    /// The application CI jobs deploying with `sym` (a CI job or template, a playbook or a role of the repo at
    /// `root`): the job of the application's own pipeline starting each chain, one landing per repo, playbook and
    /// application name passed.
    fn deploy_landings(&mut self, root: &Path, sym: &query::SymbolRow, summaries: &[kc_index::workspace::RepoSummary]) -> Result<Vec<Landing>> {
        if self.chains.is_none() {
            let (chains, cut) = kc_index::ci::Ci::new(summaries).all_with_cuts();
            self.chains = Some(chains);
            self.ci_cut = cut;
        }
        let chains = self.chains.take().unwrap_or_default();
        let dir = kc_extract::ansible::place(&sym.file).and_then(|p| p.role_dir).unwrap_or_default();
        let mut out: Vec<Landing> = Vec::new();
        let app_vars = kc_index::config::conventions_now().ci.app_name_variables.clone();
        let mut run = || -> Result<()> {
            for c in &chains {
                let hit = match sym.kind.as_str() {
                    "job" => c.through_job(root, &sym.file, &sym.name),
                    "playbook" => c.runs_playbook(root, &sym.file),
                    _ => c.uses_role(root, &dir),
                };
                if !hit || !c.app.join(".git").exists() || !self.open(&c.app)? {
                    continue;
                }
                let playbook = c.target();
                // The application the deploy installs: the first configured app-name variable the pipeline sets.
                let app = app_vars.iter().find_map(|v| c.var(v)).map(str::to_string);
                let conn = self.store(&c.app).conn();
                if let Some(h) = c.hops.iter().find(|h| h.how == "job" && h.repo == c.app) {
                    let Some(s) = ci_job_at(conn, &h.file, &h.name)? else { continue };
                    let creator = (s, "exact", Some(format!("{}:{}", h.file, h.line)));
                    match out.iter_mut().find(|l| l.root == c.app && l.via.deploy().is_some_and(|d| d.playbook == playbook && d.app == app)) {
                        Some(l) => {
                            if !l.creators.iter().any(|x| x.0.id == creator.0.id) {
                                l.creators.push(creator);
                            }
                        }
                        None => {
                            let via = trace::Via::Deploy(trace::DeployVia { playbook: playbook.clone(), app: app.clone(), link: c.confidence, assumptions: c.assumptions.clone() });
                            out.push(Landing { root: c.app.clone(), via, creators: vec![creator], publish: None });
                        }
                    }
                }
            }
            Ok(())
        };
        let res = run();
        self.chains = Some(chains);
        res.map(|_| out)
    }

    /// The consumers of the message `qualified` is, or of those it publishes: each consumer's `Consume` method (or
    /// the class, without one), one landing per repo and message.
    fn consumers(&mut self, root: &Path, qualified: &str, file: &str, line: u32, summaries: &[kc_index::workspace::RepoSummary]) -> Result<Vec<Landing>> {
        let mut out: Vec<Landing> = Vec::new();
        for (entry, link) in kc_index::bus::consumers(root, qualified, file, line, summaries) {
            if !link.repo.join(".git").exists() {
                continue;
            }
            if !self.open(&link.repo)? {
                continue;
            }
            let conn = self.store(&link.repo).conn();
            let in_file = |q: &str| -> Result<Option<query::SymbolRow>> {
                Ok(query::definitions(conn, q)?.into_iter().find(|d| d.file == link.row.file))
            };
            let Some(handler) = in_file(&link.row.handler())?.or(in_file(&link.row.symbol)?) else { continue };
            let via = trace::BusVia { message: entry.message(), queue: entry.queue(), link: link.confidence, assumptions: link.assumptions.clone() };
            // The `Consume` method's own line; the class's base type, without one.
            let line = if handler.qualified() == link.row.handler() { handler.line } else { link.row.line };
            let creator = (handler, "exact", Some(format!("{}:{line}", link.row.file)));
            match out.iter_mut().find(|l| l.root == link.repo && matches!(&l.via, trace::Via::Bus(b) if b.message == via.message && b.link == via.link && b.assumptions == via.assumptions)) {
                Some(l) => {
                    if !l.creators.iter().any(|c| c.0.id == creator.0.id) {
                        l.creators.push(creator);
                    }
                }
                None => {
                    let publish = (entry.row.kind == "publish").then(|| format!("{}:{}", entry.row.file, entry.row.line));
                    out.push(Landing { root: link.repo.clone(), via: trace::Via::Bus(via), creators: vec![creator], publish })
                }
            }
        }
        Ok(out)
    }

    /// The code using `sym`, a SQL table, view or routine of the repo at `root`, in any indexed repo: Entity Framework
    /// tables mapped to it (with their accessors), and calls running it. Only code whose name links to this definition
    /// (see `kc_index::sql`) counts.
    pub(crate) fn sql_uses(&mut self, root: &Path, sym: &query::SymbolRow, summaries: &[kc_index::workspace::RepoSummary]) -> Result<Vec<SqlUse>> {
        let relation = kc_store::sql::is_relation(sym);
        let kinds = if relation { kc_index::sql::TABLE_KINDS } else { kc_index::sql::ROUTINE_KINDS };
        let sql = kc_index::sql::Sql::new(summaries, Some(&self.home.root));
        let key = |kind: &str, schema: Option<&str>, name: &str| (kind.to_string(), schema.map(str::to_lowercase), name.to_lowercase());
        let me = key(&sym.kind, sym.container.as_deref(), &sym.name);
        let name = sym.name.to_lowercase();
        let mut out = Vec::new();
        for repo in sql.users(&name) {
            if !repo.join(".git").exists() {
                continue;
            }
            if !self.open(&repo)? {
                continue;
            }
            let model = self.store(&repo).db_model()?;
            // A link to this definition, or to another repo's copy of the same `schema.name` (a service repo keeping
            // its own copy of a shared schema links to that copy). Without a schema, only this repo's definition.
            let link = |written: &str, context: Option<&kc_index::sql::DbContextTables>| {
                sql.definitions_in(&repo, written, kinds, context)
                    .into_iter()
                    .find(|l| (l.repo == root || me.1.is_some()) && key(&l.def.kind, l.def.schema.as_deref(), &l.def.name) == me)
                    .map(|l| (l.confidence, l.assumptions()))
            };
            if relation {
                for t in model.tables.iter().filter(|t| kc_store::sql::split_name(&t.table).1 == name) {
                    if let Some((confidence, assumptions)) = link(&t.table, kc_index::sql::DbContextTables::of(&model, t).as_ref()) {
                        out.push(SqlUse { root: repo.clone(), confidence, assumptions, table: Some(t.clone()), procedure: None });
                    }
                }
            } else {
                for p in model.procedures.iter().chain(&model.wrapped).filter(|p| kc_store::sql::split_name(&p.name).1 == name) {
                    if let Some((confidence, assumptions)) = link(&p.name, None) {
                        out.push(SqlUse { root: repo.clone(), confidence, assumptions, table: None, procedure: Some(p.clone()) });
                    }
                }
            }
        }
        Ok(out)
    }

    /// The code depending on `sym`, a SQL table, view or routine: the methods reading or writing an entity mapped to
    /// it, and those calling it. One landing per repo, entity and mode, or per repo for calls.
    fn sql_landings(&mut self, root: &Path, sym: &query::SymbolRow, summaries: &[kc_index::workspace::RepoSummary]) -> Result<Vec<Landing>> {
        let mut out: Vec<Landing> = Vec::new();
        for u in self.sql_uses(root, sym, summaries)? {
            let conn = self.store(&u.root).conn();
            if let Some(t) = &u.table {
                for mode in ["write", "read_write", "read", "access"] {
                    let mut creators = Vec::new();
                    for a in t.accessors.iter().filter(|a| a.mode == mode) {
                        let confidence = match a.confidence.as_str() {
                            "exact" => "exact",
                            "unique_name" | "nearest" => "unique_name",
                            _ => "ambiguous",
                        };
                        if let Some(s) = trace::symbol(conn, a.id)? {
                            creators.push((s, confidence, Some(format!("{}:{}", a.file, a.line))));
                        }
                    }
                    if creators.is_empty() {
                        continue;
                    }
                    let via = trace::SqlVia {
                        object: sym.qualified(),
                        how: "entity",
                        entity: Some(t.entity.clone()),
                        mode: Some(mode.to_string()),
                        link: u.confidence,
                        assumptions: u.assumptions.clone(),
                    };
                    out.push(Landing { root: u.root.clone(), via: trace::Via::Sql(via), creators, publish: None });
                }
            }
            if let Some(p) = &u.procedure
                && let Some(s) = trace::symbol(conn, p.id)?
            {
                let creator = (s, "exact", Some(format!("{}:{}", p.file, p.line)));
                let via = trace::SqlVia { object: sym.qualified(), how: "call", entity: None, mode: None, link: u.confidence, assumptions: u.assumptions.clone() };
                match out.iter_mut().find(|l| l.root == u.root && matches!(&l.via, trace::Via::Sql(v) if v.how == "call" && v.link == u.confidence && v.assumptions == u.assumptions)) {
                    Some(l) => {
                        if !l.creators.iter().any(|c| c.0.id == creator.0.id) {
                            l.creators.push(creator);
                        }
                    }
                    None => out.push(Landing { root: u.root.clone(), via: trace::Via::Sql(via), creators: vec![creator], publish: None }),
                }
            }
        }
        Ok(out)
    }

    /// The code requesting the action `qualified` that starts at `line` of `file` (overloads share the name), over
    /// each route it serves.
    pub(crate) fn landings_with(
        &mut self,
        root: &Path,
        qualified: &str,
        file: &str,
        line: u32,
        summaries: &[kc_index::workspace::RepoSummary],
    ) -> Result<Vec<Landing>> {
        let routes: Vec<&query::RouteRow> = summaries
            .iter()
            .find(|s| s.root == root)
            .map(|s| s.routes.iter().filter(|r| r.side == "server" && r.symbol == qualified && r.file == file && r.start_line == line).collect())
            .unwrap_or_default();
        let mut out = Vec::new();
        for (route, link) in routes.into_iter().flat_map(|r| kc_index::http::callers(r, root, summaries).into_iter().map(move |l| (r, l))) {
            if !link.repo.join(".git").exists() {
                continue;
            }
            if !self.open(&link.repo)? {
                continue;
            }
            let Some((contract_id, creators)) = trace::creators(self.store(&link.repo).conn(), &link.route.symbol, &link.route.file, link.route.start_line)? else { continue };
            let call = creators.iter().any(|(s, ..)| s.id == contract_id);
            // A method requesting the route itself: at the call, not the method's declaration.
            let creators: Vec<trace::Creator> = if call && link.route.line > 0 {
                creators.into_iter().map(|(s, c, _)| (s, c, Some(format!("{}:{}", link.route.file, link.route.line)))).collect()
            } else {
                creators
            };
            let via = trace::Via::Http(trace::HttpVia {
                verb: route.verb.clone(),
                path: route.path.clone(),
                contract: link.route.symbol.clone(),
                call,
                link: link.confidence,
                assumptions: link.assumptions.clone(),
            });
            if !creators.is_empty() {
                out.push(Landing { root: link.repo.clone(), via: via.clone(), creators, publish: None });
            }
            if call {
                continue;
            }
            // A gateway contract ships in a package of the serving repo: the code sending it is in repos that
            // depend on the contract's.
            let short = link.route.symbol.rsplit('.').next().unwrap_or(&link.route.symbol).to_string();
            let contract_repo = repo_name(&link.repo);
            for d in kc_index::workspace::neighbours(&link.repo, summaries).1 {
                if d.gone {
                    // A gone dependent that may create the contract would have been a requester.
                    if d.may_reference(&short) {
                        kc_index::workspace::note_missing(&d.root);
                    }
                    continue;
                }
                if !self.open(&d.root)? {
                    continue;
                }
                let modules = match summaries.iter().find(|s| s.root == link.repo) {
                    Some(s) => s.modules.clone(),
                    None => query::provided_modules(self.store(&link.repo).conn())?,
                };
                let dep = query::Dependency { repo: contract_repo.clone(), conn: self.store(&link.repo).conn(), modules, assumptions: Vec::new() };
                let (hits, _) = query::refs_with(self.store(&d.root).conn(), &short, &["instantiate"], usize::MAX, std::slice::from_ref(&dep))?;
                let mut creators = Vec::new();
                for h in hits {
                    if !h.targets.iter().any(|t| t.id == contract_id && t.repo.as_deref() == Some(contract_repo.as_str())) {
                        continue;
                    }
                    let Some(from) = &h.from else { continue };
                    // Published on the bus (the DTO is a bus message too), not sent over HTTP.
                    if trace::publishes(self.store(&d.root).conn(), from.id, &short)? {
                        continue;
                    }
                    if let Some(s) = trace::symbol(self.store(&d.root).conn(), from.id)?
                        && !creators.iter().any(|(c, ..): &trace::Creator| c.id == s.id)
                    {
                        creators.push((s, h.confidence, Some(format!("{}:{}", h.file, h.line))));
                    }
                }
                if !creators.is_empty() {
                    out.push(Landing { root: d.root.clone(), via: via.clone(), creators, publish: None });
                }
            }
        }
        Ok(out)
    }

    /// Of the symbols a walk stopped at (`stops`, see [`trace::Stop`]), those with neighbours one hop past them that
    /// `listed` says the answer doesn't have: through references, and for dependents, across HTTP routes, the bus,
    /// SQL and deploys too. At most `budget` are checked; also returns how many weren't.
    pub(crate) fn frontier(
        &mut self,
        stops: &[trace::Stop],
        listed: &dyn Fn(Option<&str>, i64) -> bool,
        budget: usize,
    ) -> Result<(Vec<trace::Frontier>, usize)> {
        let mut out: Vec<trace::Frontier> = Vec::new();
        let mut unchecked = 0;
        let mut labels: Vec<Option<String>> = stops.iter().map(|s| s.repo.clone()).collect();
        labels.sort();
        labels.dedup();
        for label in labels {
            let root = match &label {
                None => self.home.root.clone(),
                Some(l) => match self.stores.iter().find(|(r, _)| repo_name(r) == *l) {
                    Some((r, _)) => r.clone(),
                    None => {
                        unchecked += stops.iter().filter(|s| s.repo == label).count();
                        continue;
                    }
                },
            };
            let group: Vec<trace::Stop> = stops.iter().filter(|s| s.repo == label).cloned().collect();
            let room = budget.saturating_sub(out.len());
            let mine = |id: i64| listed(label.as_deref(), id);
            let (found, skipped) = trace::frontier(self.store(&root).conn(), &group, &mine, self.gate.filter(), room)?;
            unchecked += skipped;
            out.extend(found);
            // Dependents across a route, the bus, SQL or a deploy, of a stop that has none through references.
            let without: Vec<&trace::Stop> = group.iter().filter(|st| st.dir == "in" && !out.iter().any(|f| f.id == st.id && f.repo == label)).take(room).collect();
            for st in without {
                let Some(sym) = trace::symbol(self.store(&root).conn(), st.id)? else { continue };
                let mut beyond = HashSet::new();
                for landing in self.crossings(&root, sym.id, &sym.qualified(), &sym.file)? {
                    let there = (landing.root != self.home.root).then(|| repo_name(&landing.root));
                    for a in &landing.arrivals {
                        if !listed(there.as_deref(), a.sym().id) && !self.gate.leaves_out(a.confidence, a.evidence.as_ref()) {
                            beyond.insert((there.clone(), a.sym().id));
                        }
                    }
                }
                if !beyond.is_empty() {
                    out.push(trace::Frontier {
                        qualified: sym.qualified(),
                        kind: sym.kind.clone(),
                        file: sym.file.clone(),
                        line: sym.line,
                        hop: st.hop,
                        reason: st.reason,
                        beyond: beyond.len(),
                        repo: label.clone(),
                        next: String::new(),
                        id: sym.id,
                        dir: st.dir,
                    });
                }
            }
        }
        Ok((out, unchecked))
    }

}

pub struct DepsData {
    pub packages: serde_json::Map<String, Value>,
    pub edges: Vec<(PathBuf, RepoEdge)>,
}

/// Packages of the selected repos and the repo edges that touch them. `with_import_sites` also lists, per edge, the
/// files that import the dependency, which reads each depending repo's imports.
pub fn deps(args: &Scope, with_import_sites: bool) -> Result<Out<DepsData>> {
    let selected = target_repos(&args.repos)?;
    let summaries = kc_index::workspace::summaries(&selected)?;
    // Selected repos are freshness-checked; the others are read from the workspace summary as last synced.
    let mut opened = Vec::new();
    let mut fresh_rows = Vec::new();
    for root in &selected {
        let Some(o) = open_member(&selected, root, args.no_sync)? else { continue };
        fresh_rows.push((manifests::packages(o.store.conn())?, query::provided_modules(o.store.conn())?));
        opened.push(o);
    }
    // An index for a repo that moved or was deleted has no repo to answer for. A selected repo is answered from its
    // fresh manifests, with a stand-in summary while it has none yet.
    let mut members: Vec<kc_index::workspace::RepoSummary> = summaries.iter().filter(|s| s.root.join(".git").exists()).cloned().collect();
    let mut modules: HashMap<String, &[String]> = HashMap::new();
    let mut packages = serde_json::Map::new();
    for (root, (pkgs, mods)) in opened.iter().map(|o| &o.root).zip(&fresh_rows) {
        modules.insert(root.to_string_lossy().into_owned(), mods);
        packages.insert(repo_name(root), serde_json::to_value(pkgs)?);
        match members.iter_mut().find(|s| &s.root == root) {
            Some(s) => s.packages = pkgs.clone(),
            None => members.push(kc_index::workspace::RepoSummary::stand_in(root, pkgs.clone())),
        }
    }
    for s in summaries.iter().filter(|s| s.root.join(".git").exists()) {
        modules.entry(s.root.to_string_lossy().into_owned()).or_insert(&s.modules);
    }
    members.sort_by(|a, b| a.root.cmp(&b.root));
    // Package ownership is by repo identity: another checkout of a repo never takes its packages away. Each repo is
    // answered by the checkout the workspace's policy picks, a selected checkout first.
    let owners = kc_index::workspace::package_owners(&members, &selected, |s| &s.packages);
    let selected_set: HashSet<&Path> = selected.iter().map(PathBuf::as_path).collect();
    let selected_names: HashSet<String> = selected.iter().map(|s| s.to_string_lossy().into_owned()).collect();
    let mut per_repo: Vec<(&Path, Vec<RepoEdge>)> = Vec::new();
    for s in &members {
        let root = s.root.as_path();
        let mut from_here: Vec<RepoEdge> = manifests::repo_edges(&s.packages, &root.to_string_lossy(), &s.identity(), &owners)
            .into_iter()
            .filter(|e| selected_set.contains(root) || selected_names.contains(&e.to_repo))
            .collect();
        kc_index::revision::check_edges(&mut from_here, &summaries);
        if !from_here.is_empty() {
            per_repo.push((root, from_here));
        }
    }
    if with_import_sites {
        // Files that import a module the dependency provides; the repo's imports are read once for all its edges,
        // and grouped by module so each edge is matched against the distinct modules, not every import.
        for (root, edges) in &mut per_repo {
            let imports = match opened.iter().find(|o| o.root == *root) {
                Some(o) => query::imports(o.store.conn())?,
                None => query::imports(kc_index::open(root)?.conn())?,
            };
            let sites = query::ImportSites::new(&imports);
            let targets: Vec<&String> = edges.iter().map(|e| &e.to_repo).collect::<HashSet<_>>().into_iter().collect();
            let mut assigned: Vec<Vec<String>> = vec![Vec::new(); edges.len()];
            for to in targets {
                let Some(mods) = modules.get(to) else { continue };
                let group: Vec<usize> = (0..edges.len()).filter(|&i| edges[i].to_repo == *to).collect();
                for (f, m, l) in sites.matching(mods) {
                    assigned[import_site_edge(edges, &group, f, m)].push(format!("{f}:{l} import {m}"));
                }
            }
            for (e, s) in edges.iter_mut().zip(assigned) {
                e.import_sites = s;
            }
        }
    }
    let edges: Vec<(PathBuf, RepoEdge)> =
        per_repo.into_iter().flat_map(|(root, edges)| edges.into_iter().map(move |e| (root.to_path_buf(), e))).collect();
    let mut items: Vec<_> = edges
        .iter()
        .map(|(from, e)| {
            let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
            v["from_repo"] = json!(from);
            v
        })
        .collect();
    // A dependency on a repo blocked whole by its AI privacy file is named as the depending manifest names it, and
    // said to be blocked: nothing of the repo itself is known or shown.
    let blocked = blocked_repo_names()?;
    let mut held: Vec<PathBuf> = Vec::new();
    for item in items.iter_mut() {
        if let Some(r) = item["to_repo"].as_str().and_then(|t| blocked.iter().find(|(r, _)| r.as_path() == Path::new(t))) {
            item["blocked"] = json!(BLOCKED_NOTE);
            held.push(r.0.clone());
        }
    }
    for pkgs in packages.values_mut().filter_map(Value::as_array_mut) {
        for d in pkgs.iter_mut().filter_map(|p| p["deps"].as_array_mut()).flatten() {
            if let Some((r, _)) = blocked_dependency(&blocked, d["name"].as_str().unwrap_or(""), d["resolved_repo"].as_str()) {
                d["blocked"] = json!(BLOCKED_NOTE);
                held.push(r.clone());
            }
        }
    }
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    env["packages"] = Value::Object(packages.clone());
    held.sort();
    held.dedup();
    for r in &held {
        if let Some(i) = withheld(r) {
            push_incomplete(&mut env, i);
        }
    }
    finish(&mut env);
    Ok(Out { env, data: DepsData { packages, edges } })
}

/// How a dependency on a repo its AI privacy file blocks whole is marked.
pub const BLOCKED_NOTE: &str = "blocked by AI privacy policy";

/// Repos blocked whole, each with the names another repo's manifest may give it: its directory's name and its
/// origin's, lower-cased. Nothing is read from the repo itself.
fn blocked_repo_names() -> Result<Vec<(PathBuf, Vec<String>)>> {
    Ok(kc_index::workspace::all_summaries()?
        .iter()
        .filter(|s| s.blocked.whole)
        .map(|s| {
            let mut names = vec![repo_name(&s.root).to_lowercase()];
            if let Some(o) = &s.origin
                && let Some(last) = o.rsplit('/').next()
            {
                names.push(last.trim_end_matches(".git").to_lowercase());
            }
            names.dedup();
            (s.root.clone(), names)
        })
        .collect())
}

/// The blocked repo a dependency names: by the path it resolved to, or by its package name (whole, or its last
/// `/`-separated part, as npm scopes write it) being the repo's name.
fn blocked_dependency<'a>(blocked: &'a [(PathBuf, Vec<String>)], name: &str, resolved: Option<&str>) -> Option<&'a (PathBuf, Vec<String>)> {
    let name = name.to_lowercase();
    let tail = name.rsplit('/').next().unwrap_or(&name).to_string();
    blocked.iter().find(|(r, names)| resolved.is_some_and(|p| Path::new(p) == r.as_path()) || names.iter().any(|n| *n == name || *n == tail))
}

/// Which of a repo's edges into one dependency repo (`group`, indexes into `edges`, never empty) an import site
/// belongs to, so each site is listed once: the edge whose manifest sits deepest above the importing file, then the
/// one whose dependency name the import names (`@acme/ui-button` for `@acme/ui-button/x`, `Acme.Common.Http` for
/// `Acme.Common.Http.Client`), longest first; else the group's first edge.
fn import_site_edge(edges: &[RepoEdge], group: &[usize], file: &str, module: &str) -> usize {
    let score = |e: &RepoEdge| {
        let dir = Path::new(&e.file).parent().unwrap_or(Path::new(""));
        let near = if Path::new(file).starts_with(dir) { dir.components().count() + 1 } else { 0 };
        let named = if query::imports_module(&module.to_ascii_lowercase(), &e.dependency.to_ascii_lowercase()) { e.dependency.len() } else { 0 };
        (near, named)
    };
    let mut best = group[0];
    for &i in &group[1..] {
        if score(&edges[i]) > score(&edges[best]) {
            best = i;
        }
    }
    best
}

/// Source files too large to extract, for `status` and `overview`.
fn large_files(large: &[(String, u64)]) -> Value {
    json!(large.iter().map(|(p, b)| json!({ "file": p, "bytes": b })).collect::<Vec<_>>())
}

pub fn status(args: &Scope) -> Result<Out<Vec<Value>>> {
    status_with(args, false)
}

/// [`status`]; with `verify`, every file is read and hashed instead of trusting an unchanged mtime and size (see
/// [`SyncOptions::verify`]), so `stale_files` counts edits that kept them.
pub fn status_with(args: &Scope, verify: bool) -> Result<Out<Vec<Value>>> {
    let (mut roots, workspace) = scope_repos_any(&args.repos)?;
    // The workspace, asked for as such or by listing its repos (the MCP server's `workspace`).
    if workspace || (roots.len() > 1 && roots == kc_index::indexed_roots()?) {
        // Repos blocked whole are out of the workspace's answers, but status says they are there and blocked.
        for (r, _) in kc_index::blocked_roots()? {
            if !roots.contains(&r) {
                roots.push(r);
            }
        }
        roots.sort();
    }
    let mut out = Vec::new();
    for root in &roots {
        let privacy = kc_index::privacy::Privacy::load(root);
        let db = kc_index::db_path(root)?;
        if !db.exists() {
            let summaries = kc_index::workspace::summaries(&roots)?;
            // Applying a changed privacy policy may have materialized a blocked marker. Don't create empty
            // indexes for newly discovered checkouts just to display status.
            if !db.exists() {
                let discovery = summaries.iter().find(|s| s.root == *root).map(|s| if s.generation().is_empty() { "pending" } else { "light" }).unwrap_or("unindexed");
                let mut value = json!({ "repo": root, "index": { "path": db, "bytes": 0, "exists": false }, "discovery": discovery,
                    "privacy": privacy_status(&privacy), "knowcode": build_info(), "config": crate::settings::status()? });
                push_incomplete(&mut value, withheld(root).unwrap_or_else(|| Incomplete::new(Reason::Stale, "index: this checkout has no full index; discovery contains only manifests and names")
                    .with_hint("ask a query about this checkout to index it automatically")));
                finish(&mut value);
                out.push(value);
                continue;
            }
        }
        let mut store = kc_index::open(root)?;
        if privacy.whole() {
            // Nothing but that it is blocked, and why.
            let mut value = json!({
                "repo": root,
                "index": { "path": kc_index::db_path(root)?, "bytes": kc_index::index_bytes(root)? },
                "privacy": privacy_status(&privacy),
                "knowcode": build_info(),
                "config": crate::settings::status()?,
            });
            if let Some(i) = withheld(root) {
                push_incomplete(&mut value, i);
            }
            finish(&mut value);
            out.push(value);
            drop(store);
            continue;
        }
        let mut opts = SyncOptions { dry_run: true, ..Default::default() };
        opts.verify |= verify;
        let stale = kc_index::sync(root, &mut store, &opts)?;
        let failed = kc_index::failed_reads(&store)?;
        let stats = query::stats(store.conn())?;
        let errors = query::files_with_errors(store.conn(), 5)?;
        let head = kc_index::head(root, &store);
        let mut value = json!({
            "repo": root,
            "commit": head.commit,
            "dirty": head.dirty,
            "index": { "path": kc_index::db_path(root)?, "bytes": kc_index::index_bytes(root)? },
            "freshness": { "stale_files": stale.parsed, "removed_files": stale.removed, "check_ms": round1(stale.total_ms) },
            "verified": stale.verified,
            "coverage": {
                "files": stats.files,
                "lines": stats.lines,
                "by_lang": stats.files_by_lang,
                "skipped": stale.skipped,
                "skipped_large": large_files(&kc_index::skipped_large(&store)?),
                "skipped_symlinks": kc_index::skipped_symlinks(&store)?,
                "with_parse_errors": stats.files_with_parse_errors,
                "worst_parse_errors": errors.iter().map(|(p, n)| json!({ "file": p, "errors": n })).collect::<Vec<_>>(),
            },
            "symbols": stats.symbols_by_kind,
            "refs": stats.refs_by_kind,
            "relations": stats.relations,
            "imports": stats.imports,
            "tags": stats.tagged,
            "extractor_versions": stats.extractor_versions,
            "content": store.content_stats()?,
            "schema": schema(),
            "knowcode": build_info(),
            "config": crate::settings::status()?,
        });
        if let Some(scip) = scip_status(&store)? {
            value["scip"] = scip;
        }
        // Always shown, so a privacy file that was never found (a wrong `[privacy] file`) is told from one read.
        value["privacy"] = privacy_status(&privacy);
        if let Some(i) = withheld(root) {
            push_incomplete(&mut value, i);
        }
        if !failed.is_empty() {
            value["freshness"]["failed"] = json!(failed);
            value["freshness"]["status"] = json!("incomplete");
        }
        add_cuts(&mut value);
        // A dry run: what the index would pick up, said as the index being behind the working tree.
        if stale.parsed + stale.removed > 0 {
            push_incomplete(
                &mut value,
                Incomplete::new(Reason::Stale, format!("index: {} changed and {} removed since the last sync", stale.parsed, stale.removed))
                    .with_hint("any query syncs it first; or run `knowcode index`"),
            );
        }
        finish(&mut value);
        out.push(value);
    }
    let env = if out.len() == 1 { out[0].clone() } else { json!(out) };
    Ok(Out { env, data: out })
}

/// A repo's AI privacy policy, for `status`: the file's state (with why, when malformed), the file name looked for,
/// its global level, the threshold, how many paths are blocked, and what is blocked (the repo, or the rules' own
/// paths; never a path below them).
fn privacy_status(p: &kc_index::privacy::Privacy) -> Value {
    let mut v = json!(p.state);
    v["name"] = json!(p.file);
    v["global_level"] = json!(p.levels.global);
    v["block_at"] = json!(p.block_at);
    let b = p.blocked();
    v["blocked_paths"] = json!(b.paths.len());
    if b.whole {
        v["blocked"] = json!("repo");
    } else if !b.paths.is_empty() {
        v["blocked"] = json!(b.paths);
    }
    if matches!(p.state, kc_index::privacy::FileState::Malformed { .. }) {
        v["malformed_as"] = json!(p.malformed.as_str());
    }
    v
}

/// The repo's SCIP index, for `status`: what the last read found, and the files it still applies to (those unchanged
/// since). `None` when the repo has never had one.
fn scip_status(store: &kc_store::Store) -> Result<Option<Value>> {
    let Some(report) = kc_index::scip::last_report(store)? else { return Ok(None) };
    let coverage = kc_store::scip::coverage(store.conn())?;
    let mut v = json!(report);
    v["coverage"] = json!(coverage);
    Ok(Some(v))
}

/// Node and edge kinds the index can hold today.
pub fn schema() -> serde_json::Value {
    json!({
        "nodes": ["file", "class", "struct", "enum", "actor", "protocol", "extension", "function", "method", "init",
                  "subscript", "property", "case", "typealias", "template"],
        "edges": {
            "calls": "ref kind `call`, resolved at query time",
            "instantiates": "ref kind `instantiate`",
            "references": "ref kind `type_ref`",
            "inherits / conforms_to": "relations, classified by the target's kind",
            "extends": "extension -> type, by qualified name",
            "imports": "file -> module",
            "contains": "symbol parent links",
            "accesses_table": "Entity Framework: a symbol referencing a DbSet or calling Set<T>(), resolved at query time; SQL text: a symbol running a query naming the table",
        },
        "confidence": ["exact", "unique_name", "ambiguous", "external_likely", "unresolved"],
        "provenance": ["syntactic"],
    })
}

#[derive(Default)]
pub struct ImpactParams {
    pub symbol: Option<String>,
    pub file: Option<String>,
    /// `working` (uncommitted changes) or a git revision to diff the working tree against.
    pub diff: Option<String>,
    pub depth: u8,
    pub limit: usize,
    pub filter: WalkFilter,
    /// How affected symbols are ordered within a hop.
    pub order: ImpactOrder,
    /// The history the `coupled` class reads (default: the 365 days before HEAD).
    pub cochange: vitals::Span,
    /// How many files the `coupled` class lists at most (default and least [`IMPACT_COUPLED_MAX`]): what a cursor
    /// continuing it raises.
    pub coupled_max: Option<usize>,
}

/// The order of `impact`'s affected symbols.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImpactOrder {
    /// By hop, then by the PageRank of their files personalized toward the changed files, so the files most tied to
    /// the change come first within a class.
    #[default]
    Rank,
    /// As the walk found them: by hop, breadth first.
    Hop,
}

impl std::str::FromStr for ImpactOrder {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "rank" => Ok(ImpactOrder::Rank),
            "hop" => Ok(ImpactOrder::Hop),
            other => bail!("order must be rank or hop, not {other}"),
        }
    }
}

/// Lists longer than this are worth ordering by PageRank (which may cost a resolution pass the first time).
pub const LONG_LIST: usize = 5;

/// How rank flows for ordering a change's dependents (see `kc_store::rank::Flow`): both ways. Tuned on replayed
/// commits: against the walk's own order, both ways raised recall@5 most, toward dependents only less, and toward
/// what the changed file uses not at all; damping 0.5 instead of 0.85 did no better.
const IMPACT_FLOW: kc_store::rank::Flow = kc_store::rank::Flow::Both;

/// Orders `affected` by hop, then by the personalized rank of each one's file (seeded at `seeds`, the changed files);
/// symbols in other repos rank last within their hop. A stable sort: equal ranks keep the walk's order.
/// Returns the generation of the graph ranked when it is an earlier one than the index's.
fn order_by_rank(store: &Store, affected: &mut [trace::Affected], seeds: &[&str]) -> Result<Option<String>> {
    if affected.len() <= LONG_LIST {
        return Ok(None);
    }
    let (rank, stale) = store.file_rank_at(seeds, IMPACT_FLOW, true)?;
    let score = |a: &trace::Affected| if a.repo.is_some() { 0.0 } else { rank.score(&a.file) };
    affected.sort_by(|a, b| a.hop.cmp(&b.hop).then_with(|| score(b).total_cmp(&score(a))));
    Ok(stale)
}

pub struct ImpactRepo {
    pub root: PathBuf,
    pub impact: Impact,
    /// References from dependent repos to the changed symbols.
    pub cross_repo: Vec<RefHit>,
    /// Tables of changed entity classes and DbSets, with the code reading and writing them.
    pub tables: Vec<db::Table>,
    pub changes: Vec<kc_index::diff::FileChange>,
    /// Files that historically change with the changed files but that the dependency walk didn't reach.
    pub coupled: Vec<CoupledFile>,
    /// How many such files there are, when more than [`IMPACT_COUPLED_MAX`].
    pub coupled_total: usize,
    /// Changed symbols past [`CROSS_REPO_SCOPE`], whose dependents in other repos weren't looked up.
    pub cross_repo_unchecked: usize,
    /// With a diff: usual partners of a changed file that the diff leaves alone.
    pub missed_partners: Vec<MissedPartner>,
    /// Affected symbols the kind filter left out.
    pub hidden: usize,
    /// Notes on the changed and affected symbols in this repo, as warnings.
    pub notes: Vec<kc_index::notes::NoteView>,
    /// For a symbol given by a bare name with several definitions: the others, `path#Name`, the analysis left out.
    pub others: Vec<String>,
    /// With a diff: the commit it is against (object id), and the definitions it removes (see [`crate::removed`]).
    pub base: Option<String>,
    pub removed: Vec<crate::removed::Removed>,
    /// What the CI limits left out of the pipelines read for deploy hops (`repo: what`).
    pub ci_cut: Vec<String>,
    /// Symbols the walk listed without walking past that have dependents it doesn't list (see [`trace::Frontier`]).
    pub frontier: Vec<trace::Frontier>,
    /// Symbols the walk stopped at past [`FRONTIER_CHECKS`], not checked for dependents.
    pub frontier_unchecked: usize,
    /// DI registrations of the types on the walk's path (see [`registrations_on_path`]).
    pub registrations: Vec<Registered>,
}

/// Symbols an impact or trace walk stopped at that are checked for neighbours it never read (see
/// [`trace::frontier`]); past it the answer says how many weren't.
pub const FRONTIER_CHECKS: usize = 200;

/// Frontier symbols an `incomplete` entry names; the item's `frontier` lists them all.
const FRONTIER_NAMED: usize = 5;

/// A dependency-injection registration of a type on an impact's path: where the container is told to make it (or
/// to hand it out as a service), which the walk's references don't reach.
#[derive(Debug, Clone, Serialize)]
pub struct Registered {
    /// The type on the path: a changed or affected type, or the type declaring a changed or affected member.
    #[serde(rename = "type")]
    pub ty: String,
    /// `implementation` (the container makes it) or `service` (it is handed out as it).
    pub role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation: Option<String>,
    pub lifetime: String,
    pub file: String,
    pub line: u32,
    /// The hop of the path's symbol it is registered for (0 for a changed symbol).
    pub hop: u8,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
}

/// A file history couples to a changed file, in `impact`'s `coupled` class.
#[derive(Debug, Clone, Serialize)]
pub struct CoupledFile {
    pub file: String,
    /// What the file is: an indexed language, or `template`, `style`, `config`, `docs`, `script`, `infra` or `other`
    /// (see [`vitals::partner_kind`]).
    pub kind: String,
    /// The changed file it is coupled to.
    pub with: String,
    pub support: u32,
    pub confidence: f64,
    /// `with`'s commits counted for co-change.
    pub commits: u32,
    pub class: &'static str,
}

/// A usual partner of a changed file that a diff didn't touch.
#[derive(Debug, Clone, Serialize)]
pub struct MissedPartner {
    pub file: String,
    pub with: String,
    pub support: u32,
    pub confidence: f64,
    pub commits: u32,
    pub message: String,
}

/// `impact`'s `coupled` class lists partners sharing at least this many commits with a changed file...
const IMPACT_COUPLED_MIN_SUPPORT: u32 = 2;
/// ...in at least this share of its commits.
const IMPACT_COUPLED_MIN_CONFIDENCE: f64 = 0.3;
/// At most this many.
const IMPACT_COUPLED_MAX: usize = 20;

/// At most this many changed symbols are looked up in the repos depending on the analysed one.
const CROSS_REPO_SCOPE: usize = 20;
/// `impact --diff` warns about a partner the diff leaves alone when it changed with a changed file in at least this
/// many commits...
const MISSED_MIN_SUPPORT: u32 = 3;
/// ...and in at least this share of them. On replayed commits, a partner this strong changed in the same commit most
/// of the time.
const MISSED_MIN_CONFIDENCE: f64 = 0.7;

/// Whether code in `from` names a type `to` declares: the static link the other way from `impact`'s walk (which
/// follows dependents), by name, so cheap and on the generous side.
fn names_types_of(store: &Store, from: &str, to: &str) -> Result<bool> {
    let mut stmt = store.conn().prepare_cached(
        "SELECT 1 FROM refs r JOIN files f ON f.id = r.file_id WHERE f.path = ?1 AND r.name IN (
             SELECT s.name FROM symbols s JOIN files g ON g.id = s.file_id
             WHERE g.path = ?2 AND s.kind IN ('class', 'struct', 'enum', 'actor', 'protocol', 'interface', 'record'))
         LIMIT 1",
    )?;
    Ok(stmt.exists([from, to])?)
}

/// Whether `path` is source code, for `missed_partners`: a language KnowCode reads, or another programming or markup
/// language's extension. Docs (`README.md`), data and extension-less files (`cpuinfo`) are not: changing one rarely
/// calls for changing its usual partners.
fn is_code_file(path: &str) -> bool {
    const CODE: &[&str] = &[
        "js", "jsx", "mjs", "cjs", "vue", "svelte", "css", "scss", "sass", "less", "java", "kt", "kts", "go", "rb", "php",
        "c", "h", "cc", "cpp", "hpp", "m", "mm", "fs", "vb", "cshtml", "razor", "aspx", "ascx", "xaml", "sh", "ps1",
        "psm1", "tf", "proto", "graphql", "gql", "cfm", "cfc", "cfs", "scala", "dart", "lua", "pl", "r",
    ];
    if kc_extract::Lang::from_path(path).is_some() {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && CODE.contains(&ext.to_ascii_lowercase().as_str()))
}

/// The `coupled` class and, with a diff, the usual partners it misses: from the co-change history of `changed`
/// (the scope's files, which may be any tracked files), leaving out files that changed themselves and, from the
/// class, docs and files with a static link: reached by the walk (`reached`), or whose types the changed file names.
fn coupling(
    root: &Path,
    store: &Store,
    changed: &[String],
    reached: &HashSet<&str>,
    diff: bool,
    span: &vitals::Span,
    max: usize,
) -> Result<(Vec<CoupledFile>, usize, Vec<MissedPartner>)> {
    let srcs: Vec<&str> = changed.iter().map(String::as_str).collect();
    // Every partner history keeps: a cut before the filters below would undercount the class, or miss a partner the
    // filters keep behind ones they drop.
    let (partners, _) = vitals::coupled_in(root, store, &srcs, usize::MAX, false, span)?;
    let mut coupled: HashMap<String, CoupledFile> = HashMap::new();
    let mut missed: HashMap<String, MissedPartner> = HashMap::new();
    for (with, list) in &partners {
        for p in list.iter().filter(|p| !srcs.contains(&p.file.as_str())) {
            if diff && p.support >= MISSED_MIN_SUPPORT && p.confidence >= MISSED_MIN_CONFIDENCE && is_code_file(with) && is_code_file(&p.file) {
                let m = MissedPartner {
                    file: p.file.clone(),
                    with: with.clone(),
                    support: p.support,
                    confidence: p.confidence,
                    commits: p.commits,
                    message: format!("you changed {with}; {} changed with it in {} of {} commits", p.file, p.support, p.commits),
                };
                if missed.get(&p.file).is_none_or(|o| (m.confidence, m.support) > (o.confidence, o.support)) {
                    missed.insert(p.file.clone(), m);
                }
            }
            // Docs (a changelog, a README) change with code without anything breaking.
            if reached.contains(p.file.as_str())
                || p.kind == "docs"
                || p.support < IMPACT_COUPLED_MIN_SUPPORT
                || p.confidence < IMPACT_COUPLED_MIN_CONFIDENCE
                || names_types_of(store, with, &p.file)?
            {
                continue;
            }
            let c = CoupledFile { file: p.file.clone(), kind: p.kind.clone(), with: with.clone(), support: p.support, confidence: p.confidence, commits: p.commits, class: "coupled" };
            if coupled.get(&p.file).is_none_or(|o| (c.confidence, c.support) > (o.confidence, o.support)) {
                coupled.insert(p.file.clone(), c);
            }
        }
    }
    let mut coupled: Vec<CoupledFile> = coupled.into_values().collect();
    coupled.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap().then(b.support.cmp(&a.support)).then_with(|| a.file.cmp(&b.file)));
    let total = coupled.len();
    coupled.truncate(max);
    let mut missed: Vec<MissedPartner> = missed.into_values().collect();
    missed.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap().then(b.support.cmp(&a.support)).then_with(|| a.file.cmp(&b.file)));
    Ok((coupled, total, missed))
}

impl ImpactRepo {
    /// The nearest proximity class: a reference from another repo's code (not its tests) is `direct`.
    pub fn nearest(&self) -> &'static str {
        if self.cross_repo.iter().any(|h| !kc_store::is_test_path(&h.file)) { "direct" } else { self.impact.nearest }
    }

    /// The deprecated `worst` (`critical`, `high`, `medium`, `low` or `none`): [`Self::nearest`] in the names it had
    /// before proximity classes. Kept one release for scripts reading it; it was always hop distance.
    pub fn legacy_worst(&self) -> &'static str {
        if self.nearest() == "direct" {
            return "critical";
        }
        let hop = self
            .impact
            .affected
            .iter()
            .filter(|a| a.class != "test")
            .map(|a| a.nearest_ambiguous_hop.unwrap_or(a.hop))
            .min();
        hop.map_or("none", trace::legacy_class)
    }
}

pub fn impact(p: &ImpactParams, args: &Scope) -> Result<Out<Vec<ImpactRepo>>> {
    let given = [p.symbol.is_some(), p.file.is_some(), p.diff.is_some()].iter().filter(|b| **b).count();
    if given != 1 {
        bail!("give exactly one of symbol, file or diff");
    }
    let mut stale_rank: Vec<Incomplete> = Vec::new();
    let mut cite_notes: Vec<Incomplete> = Vec::new();
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    let edges = p.filter.edges()?;
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let conn = o.store.conn();
        let mut changes = Vec::new();
        let (mut old_defs, mut base) = (Vec::new(), None);
        // A file without symbols (a template, say) still has co-change partners.
        let mut bare_file = None;
        let mut others = Vec::new();
        let ids: Vec<i64> = if let Some(s) = &p.symbol {
            let mut defs = query::definitions(conn, s)?;
            // Of same-named types (an Entity Framework entity and a DTO of its name), the entity: a change to a model
            // is most often about the table. The others are named, for `path#Name`.
            if defs.len() > 1 && defs.iter().all(|d| query::is_type_or_extension(&d.kind)) {
                let model = o.store.db_model()?;
                let entity = |d: &query::SymbolRow| model.tables.iter().any(|t| t.entity_id == Some(d.id));
                if !entity(&defs[0])
                    && let Some(i) = defs.iter().position(entity)
                {
                    let picked = defs.remove(i);
                    defs.insert(0, picked);
                }
                others = defs.iter().skip(1).filter(|d| d.kind != "extension").map(|d| format!("{}#{}", d.file, d.name)).collect();
            }
            match defs.first() {
                // A SQL object dumped in several places: every copy, since each serves the routines dumped beside it.
                Some(first) if kc_store::sql::is_object(first) => {
                    let same = |d: &query::SymbolRow| {
                        d.kind == first.kind && d.container.as_deref().map(str::to_lowercase) == first.container.as_deref().map(str::to_lowercase)
                    };
                    defs.iter().filter(|d| same(d)).map(|d| d.id).collect()
                }
                _ => defs.into_iter().take(1).map(|d| d.id).collect(),
            }
        } else if let Some(f) = &p.file {
            let rel = Path::new(f).strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| f.clone());
            let ids = trace::symbols_in_file(conn, &rel)?;
            if ids.is_empty() && is_repo_file(conn, &rel)? {
                bare_file = Some(rel);
            }
            ids
        } else {
            let rev = p.diff.as_deref().filter(|d| *d != "working");
            let d = kc_index::diff::diff(root, rev)?;
            old_defs = kc_index::diff::old_definitions(root, &d)?;
            base = Some(d.base);
            changes = d.changes;
            let mut ids = Vec::new();
            for c in changes.iter().filter(|c| !c.deleted) {
                for id in trace::symbols_in_ranges(conn, &c.file, &c.ranges)? {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
            ids
        };
        if ids.is_empty() && p.diff.is_none() && bare_file.is_none() {
            opened.push(o);
            continue;
        }
        // C# used from Razor markup, which isn't read: dependents there aren't listed.
        if ids.iter().any(|&id| trace::symbol(conn, id).ok().flatten().is_some_and(|s| s.file.ends_with(".cs"))) {
            stale_rank.extend(razor_unread(&o, roots.len() > 1)?);
        }
        // The walk reads every edge through the symbol graph and its gate, as `path` and `flow` do.
        let mut g = crate::walk::Graph::new(&o, &[], args.no_sync, usize::MAX).with_edges(edges.clone());
        let mut impact = crate::tree::impact(&mut g, &ids, p.depth, p.limit)?;
        // The diff's pre-change side: code still naming a definition the change removed.
        let gone = crate::removed::removed(conn, &old_defs, &ids)?;
        let mut removed = Vec::new();
        if !gone.is_empty() {
            let mut seen = trace::Reached::default();
            for s in &impact.scope {
                seen.root(s.id);
            }
            for a in &impact.affected {
                if let Some(h) = a.nearest_ambiguous_hop {
                    seen.offer(a.id, h, "ambiguous");
                }
                seen.offer(a.id, a.hop, a.confidence);
            }
            let room = p.limit.saturating_sub(impact.affected.len());
            let depth = p.depth.clamp(1, trace::MAX_IMPACT_DEPTH);
            let mut onward = |from: &[i64], room: usize, seen: &mut trace::Reached| crate::tree::dependents(&mut g, 0, from, 1, depth, room, seen);
            let (more, list, cut) = crate::removed::dependents(conn, &gone, &mut seen, &edges, depth, room, &mut onward)?;
            // A better way to a symbol listed already replaces its row; each row's class by its nearest way.
            let mut at: HashMap<i64, usize> = impact.affected.iter().enumerate().map(|(i, a)| (a.id, i)).collect();
            for a in more {
                match at.get(&a.id) {
                    Some(&i) => impact.affected[i] = a,
                    None => {
                        at.insert(a.id, impact.affected.len());
                        impact.affected.push(a);
                    }
                }
            }
            for a in &mut impact.affected {
                seen.settle(a);
            }
            impact.stopped.extend(seen.stops(None));
            impact.stopped.retain(|st| !seen.was_grown(st.id));
            impact.truncated |= cut;
            impact.recount();
            removed = list;
        }
        crate::tree::cross_impact(&mut g, &mut impact, p.depth, p.limit)?;
        let crossing = g.crossing();
        // Where the walk stopped short: the symbols it listed without walking past that have dependents it doesn't list.
        let listed: HashSet<(Option<String>, i64)> =
            impact.affected.iter().map(|a| (a.repo.clone(), a.id)).chain(impact.scope.iter().map(|s| (None, s.id))).collect();
        // A walk the symbol limit cut already says so, with a cursor: its stops are counted, not checked (checking costs
        // what one more hop does).
        let budget = if impact.truncated { 0 } else { FRONTIER_CHECKS };
        let (mut frontier, frontier_unchecked) =
            crossing.frontier(&impact.stopped, &|repo, id| listed.contains(&(repo.map(str::to_string), id)), budget)?;
        let ci_cut = std::mem::take(&mut crossing.ci_cut);
        // Each stop's exact call going on from it.
        let mut go_on = p.filter.args(&edges);
        go_on.insert("depth".into(), p.depth.clamp(1, trace::MAX_IMPACT_DEPTH).into());
        crate::tree::continuations(&mut g, &mut frontier, "kc_impact", &go_on, |_, _| {})?;
        let registrations = registrations_on_path(conn, &impact)?;
        if impact.affected.iter().any(|a| kc_store::sql::is_sql_path(&a.file)) {
            impact.collapse_sql_copies();
        }
        let kinds = p.filter.kinds()?;
        let before = impact.affected.len();
        if !kinds.is_empty() {
            impact.affected.retain(|a| WalkFilter::keeps(&kinds, &a.kind));
            impact.recount();
        }
        let hidden = before - impact.affected.len();
        if p.order == ImpactOrder::Rank {
            let mut seeds: Vec<&str> = impact.scope.iter().map(|s| s.file.as_str()).collect();
            seeds.extend(changes.iter().filter(|c| !c.deleted).map(|c| c.file.as_str()));
            seeds.sort();
            seeds.dedup();
            let seeds: Vec<String> = seeds.into_iter().map(str::to_string).collect();
            let seeds: Vec<&str> = seeds.iter().map(String::as_str).collect();
            if let Some(g) = order_by_rank(&o.store, &mut impact.affected, &seeds)? {
                stale_rank.push(kc_store::answer::stale_derived(&format!("order of dependents ({}): file ranking", repo_name(&o.root)), &g, &o.generation));
            }
        }
        // Dependents in other repos: hop 1 (direct) across the repo boundary.
        let mut cross_repo = Vec::new();
        let cross_repo_unchecked = impact.scope.len().saturating_sub(CROSS_REPO_SCOPE);
        for s in impact.scope.iter().take(CROSS_REPO_SCOPE) {
            let (name, kinds): (String, Vec<&str>) = match s.kind.as_str() {
                "class" | "struct" | "enum" | "actor" | "protocol" => {
                    (s.qualified.rsplit('.').next().unwrap_or(&s.qualified).to_string(), vec!["instantiate", "type_ref"])
                }
                // A constructor is reached by creating its type, never by a call named `init` (another type's
                // constructors chaining to theirs).
                "init" => {
                    let ty = s.qualified.strip_suffix(".init").unwrap_or(&s.qualified);
                    (ty.rsplit('.').next().unwrap_or(ty).to_string(), vec!["instantiate"])
                }
                _ => (s.qualified.rsplit('.').next().unwrap_or(&s.qualified).to_string(), vec!["call", "member"]),
            };
            // Implementations and subclasses in dependents, of a type or (as their members) of an interface member.
            if let Some(sym) = trace::symbol(conn, s.id)? {
                let found = if query::is_type_or_extension(&sym.kind) {
                    dependent_conformers(&o, &sym, None, args.no_sync)?
                } else {
                    let owner = sym.container.as_deref().map(|c| query::definitions(conn, c)).transpose()?.unwrap_or_default();
                    match owner.into_iter().find(|d| d.kind == "protocol") {
                        Some(ty) if matches!(sym.kind.as_str(), "method" | "property") => dependent_conformers(&o, &ty, Some(&sym), args.no_sync)?,
                        _ => Vec::new(),
                    }
                };
                let edge = if query::is_type_or_extension(&sym.kind) { "conformer" } else { "witness" };
                if edges.allows(edge) {
                    cross_repo.extend(conformer_hits(&sym, &repo_name(&o.root), found));
                }
            }
            for h in dependent_refs(&o, &name, &kinds, args.no_sync)? {
                let edge = match h.kind.as_str() {
                    "call" => "calls",
                    "instantiate" => "instantiates",
                    _ => "uses",
                };
                // A base list's type use is the conformer listed already.
                let conformer = |c: &RefHit| c.kind == "inherits" && c.repo == h.repo && c.file == h.file && c.line == h.line;
                if h.kind == "type_ref" && cross_repo.iter().any(conformer) {
                    continue;
                }
                if edges.allows(edge)
                    && (h.may_target(s.id) || h.targets.iter().any(|t| t.id == s.id))
                    && edges.judge((&h.repo, &h.file, h.line, s.id, edge), h.confidence, h.evidence.as_ref()) != Verdict::Exclude
                {
                    cross_repo.push(h);
                }
            }
        }
        let model = o.store.db_model()?;
        let tables: Vec<db::Table> = model
            .tables
            .iter()
            .filter(|t| impact.scope.iter().any(|s| t.entity_id == Some(s.id) || t.dbset_id == Some(s.id)))
            .cloned()
            .collect();
        // Co-change: the changed files (a diff's, else the scope's), and the files the walk reached in this repo.
        let mut changed: Vec<String> = if p.diff.is_some() {
            changes.iter().filter(|c| !c.deleted).map(|c| c.file.clone()).collect()
        } else {
            impact.scope.iter().map(|s| s.file.clone()).chain(bare_file).collect()
        };
        changed.sort();
        changed.dedup();
        let reached: HashSet<&str> = impact.affected.iter().filter(|a| a.repo.is_none()).map(|a| a.file.as_str()).collect();
        let coupled_max = p.coupled_max.unwrap_or(IMPACT_COUPLED_MAX).max(IMPACT_COUPLED_MAX);
        let (coupled, coupled_total, missed_partners) = coupling(root, &o.store, &changed, &reached, p.diff.is_some(), &p.cochange, coupled_max)?;
        let notes = if kc_index::notes::any(root)? {
            let names: Vec<String> = impact
                .scope
                .iter()
                .map(|s| s.qualified.clone())
                .chain(impact.affected.iter().filter(|a| a.repo.is_none()).map(|a| a.qualified.clone()))
                .collect();
            kc_index::notes::on(root, &o.store, &names)?
        } else {
            Vec::new()
        };
        let mut citer = crate::cite::Citer::new([]).with_stores(g.stores());
        citer.impact(&o.root, &mut impact);
        citer.refs(&o.root, cross_repo.iter_mut());
        cite_notes.extend(citer.incomplete());
        drop(citer);
        drop(g);
        found.push(ImpactRepo {
            root: o.root.clone(),
            impact,
            cross_repo,
            cross_repo_unchecked,
            tables,
            changes,
            coupled,
            coupled_total,
            missed_partners,
            hidden,
            notes,
            others,
            base,
            removed,
            ci_cut,
            frontier,
            frontier_unchecked,
            registrations,
        });
        opened.push(o);
    }
    let items: Vec<Value> = found
        .iter()
        .map(|r| {
            let mut counts = json!(r.impact.counts);
            counts["cross_repo"] = json!(r.cross_repo.len());
            counts["coupled"] = json!(r.coupled.len());
            let mut item = json!({
                "repo": repo_name(&r.root),
                "scope": r.impact.scope,
                "nearest": r.nearest(),
                // Deprecated (one release): `nearest` in the old severity names. Never a business risk.
                "worst": r.legacy_worst(),
                "counts": counts,
                "low_confidence": r.impact.low_confidence,
                "affected": r.impact.affected,
                "coupled": r.coupled,
                "cross_repo": r.cross_repo,
                "tables": r.tables.iter().map(table_json).collect::<Vec<_>>(),
                "changes": r.changes,
                "truncated": r.impact.truncated,
            });
            if r.impact.truncated {
                // The walk stopped at the limit: the counts are of what it reached, not of everything affected.
                item["counts_cover"] = json!("shown");
            }
            if p.diff.is_some() {
                item["missed_partners"] = json!(r.missed_partners);
                item["base"] = json!(r.base);
                item["removed"] = json!(r.removed);
                item["pre_change"] = json!(crate::removed::PRE_CHANGE);
            }
            if r.hidden > 0 {
                item["hidden"] = json!(r.hidden);
            }
            if r.coupled_total > r.coupled.len() {
                item["coupled_total"] = json!(r.coupled_total);
            }
            if !r.notes.is_empty() {
                item["notes"] = json!(r.notes.iter().map(kc_index::notes::NoteView::line).collect::<Vec<_>>());
            }
            if !r.others.is_empty() {
                item["also_defined"] = json!(r.others);
            }
            // Deploy chains a CI limit cut: a dependent across a deploy may be missing.
            if !r.ci_cut.is_empty() {
                item["ci_cut"] = json!(r.ci_cut);
            }
            if !r.frontier.is_empty() {
                item["frontier"] = json!(r.frontier);
            }
            if !r.registrations.is_empty() {
                item["registrations"] = json!(r.registrations);
            }
            item
        })
        .collect();
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    if let Some(f) = p.filter.describe() {
        env["filter"] = f;
    }
    for i in stale_rank.into_iter().chain(cite_notes) {
        push_incomplete(&mut env, i);
    }
    for r in found.iter().filter(|r| r.cross_repo_unchecked > 0) {
        push_incomplete(
            &mut env,
            Incomplete::new(
                Reason::Capped,
                format!(
                    "items[] cross_repo: dependents in other repos looked up for {CROSS_REPO_SCOPE} of {} changed symbols in {}",
                    CROSS_REPO_SCOPE + r.cross_repo_unchecked,
                    repo_name(&r.root)
                ),
            )
            .with_hint("narrow the change (a symbol, or a smaller diff) to look up the rest"),
        );
    }
    report_policy(&mut env, &edges);
    // A depth past the deepest the walk goes, and the symbols it stopped at with dependents it didn't list.
    let walked = p.depth.clamp(1, trace::MAX_IMPACT_DEPTH);
    if p.depth > walked {
        env["depth"] = json!({ "asked": p.depth, "walked": walked });
    }
    for r in &found {
        let asked = (p.depth > walked).then_some(p.depth);
        for i in frontier_incomplete(&repo_name(&r.root), &r.frontier, r.frontier_unchecked, (walked, trace::MAX_IMPACT_DEPTH, asked, r.impact.truncated), "impact", "dependents") {
            push_incomplete(&mut env, i);
        }
    }
    let coupled_max = p.coupled_max.unwrap_or(IMPACT_COUPLED_MAX).max(IMPACT_COUPLED_MAX);
    let mut lists = Vec::new();
    for (i, r) in found.iter().enumerate() {
        lists.push(cursor::List::new(format!("/items/{i}/affected"), p.limit, r.impact.truncated));
        lists.push(cursor::List::new(format!("/items/{i}/coupled"), coupled_max, r.coupled_total > r.coupled.len()));
        lists.push(cursor::List::new(format!("/items/{i}/cross_repo"), p.limit, false));
    }
    let req = cursor::Request::Impact {
        symbol: p.symbol.clone(),
        file: p.file.clone(),
        diff: p.diff.clone(),
        depth: p.depth,
        limit: p.limit,
        edges: p.filter.edges.clone(),
        kinds: p.filter.kinds.clone(),
        order: if p.order == ImpactOrder::Hop { "hop" } else { "rank" }.to_string(),
        as_of: p.cochange.as_of.clone(),
        since: p.cochange.since.clone(),
        evidence: policy_word(&edges),
    };
    cursor::offer(&mut env, req, args, lists, &[]);
    finish(&mut env);
    Ok(Out { env, data: found })
}

/// The `incomplete` entries saying where a walk (`impact`, `trace`) of `repo` stopped short: at its depth (`walked`,
/// at most `max`, of `asked` when deeper was asked), at ambiguous edges the evidence policy lists without walking past, and stops left unchecked. Each
/// names the first frontier symbols and says how to go on from them; the item's `frontier` lists them all.
pub fn frontier_incomplete(
    repo: &str,
    frontier: &[trace::Frontier],
    unchecked: usize,
    (walked, max, asked, cut): (u8, u8, Option<u8>, bool),
    cmd: &str,
    what: &str,
) -> Vec<Incomplete> {
    let symbols = |n: usize| if n == 1 { "1 symbol".to_string() } else { format!("{n} symbols") };
    // The exact call going on from the first stop of a reason.
    let next = |reason: &str| frontier.iter().find(|f| f.reason == reason && !f.next.is_empty()).map(|f| format!("; continue: {}", f.next)).unwrap_or_default();
    let named = |reason: &str| -> (usize, usize, String) {
        let of: Vec<&trace::Frontier> = frontier.iter().filter(|f| f.reason == reason).collect();
        let beyond = of.iter().map(|f| f.beyond).sum();
        let mut names: Vec<String> = of.iter().take(FRONTIER_NAMED).map(|f| f.qualified.clone()).collect();
        if of.len() > FRONTIER_NAMED {
            names.push(format!("… {} more", of.len() - FRONTIER_NAMED));
        }
        (of.len(), beyond, names.join(", "))
    };
    let mut out = Vec::new();
    let (n, beyond, names) = named("depth");
    if n > 0 {
        let deeper = if walked < max {
            format!("pass depth {} (up to {max}) to walk further, or {cmd} one of them to go on from it", walked + 1)
        } else {
            format!("{cmd} one of them to go on from it")
        };
        // Asked deeper than the walk goes: said, so a shallower answer never passes for the one asked for.
        let reached = match asked {
            Some(a) => format!("depth {walked} reached ({a} asked; {walked} is the deepest {cmd} goes)"),
            None => format!("depth {walked} reached"),
        };
        out.push(
            Incomplete::new(Reason::Depth, format!("{repo}: {reached}: {} there {} {beyond}+ {what} not listed ({names}); see frontier", symbols(n), if n == 1 { "has" } else { "have" }))
                .with_hint(format!("{deeper}{}", next("depth"))),
        );
    }
    let (n, beyond, names) = named("ambiguous");
    if n > 0 {
        out.push(
            Incomplete::new(
                Reason::Policy,
                format!(
                    "{repo}: {} reached only by ambiguous edges, listed but not walked past, {} {beyond}+ {what} not listed ({names}); see frontier",
                    symbols(n),
                    if n == 1 { "has" } else { "have" }
                ),
            )
            .with_hint(format!("pass evidence all to walk past ambiguous edges (marked ambiguous), or {cmd} one of them{}", next("ambiguous"))),
        );
    }
    if unchecked > 0 && cut {
        // The limit cut the walk first: its `capped` entry and cursor say so; what lies past its stops wasn't read.
        out.push(
            Incomplete::new(
                Reason::Depth,
                format!("{repo}: frontier: {} the walk stopped at not checked for {what} past them, the walk being cut at its limit", symbols(unchecked)),
            )
            .with_hint("raise the limit, lower depth, or filter with edges/kinds"),
        );
    } else if unchecked > 0 {
        out.push(
            Incomplete::new(Reason::Deadline, format!("{repo}: frontier: {} the walk stopped at not checked for {what} past them", symbols(unchecked)))
                .with_hint("lower depth, or filter with edges/kinds"),
        );
    }
    out
}

/// The DI registrations of the types on an impact's path: the changed and affected types, and the types declaring a
/// changed or affected member. A registration names a type as a generic argument or in `typeof`, which references
/// reach as type uses, but it is the container that makes the type at run time: these rows say where.
fn registrations_on_path(conn: &rusqlite::Connection, impact: &Impact) -> Result<Vec<Registered>> {
    if !kc_store::di::any(conn)? {
        return Ok(Vec::new());
    }
    // Each type once, by the nearest hop it is on the path at.
    let mut types: Vec<(String, u8)> = Vec::new();
    let mut add = |ty: Option<String>, hop: u8| {
        let Some(ty) = ty.map(|t| t.rsplit('.').next().unwrap_or(&t).to_string()) else { return };
        match types.iter_mut().find(|(t, _)| *t == ty) {
            Some(e) => e.1 = e.1.min(hop),
            None => types.push((ty, hop)),
        }
    };
    let owner = |kind: &str, qualified: &str| -> Option<String> {
        if query::is_type_or_extension(kind) { Some(qualified.to_string()) } else { qualified.rsplit_once('.').map(|(c, _)| c.to_string()) }
    };
    for s in &impact.scope {
        add(owner(&s.kind, &s.qualified), 0);
    }
    for a in impact.affected.iter().filter(|a| a.repo.is_none()) {
        add(owner(&a.kind, &a.qualified), a.hop);
    }
    let mut out: Vec<Registered> = Vec::new();
    for (ty, hop) in types {
        let as_impl = kc_store::di::registered_as(conn, &ty)?.into_iter().map(|r| ("implementation", r));
        let as_service = kc_store::di::registrations_of(conn, &ty)?.into_iter().map(|r| ("service", r));
        for (role, r) in as_impl.chain(as_service) {
            if out.iter().any(|o| o.file == r.file && o.line == r.line && o.ty == ty) {
                continue;
            }
            out.push(Registered {
                ty: ty.clone(),
                role,
                service: r.service,
                implementation: r.implementation,
                lifetime: r.lifetime,
                file: r.file,
                line: r.line,
                hop,
                test: r.test,
            });
        }
    }
    out.sort_by(|a, b| (a.hop, a.test, &a.file, a.line).cmp(&(b.hop, b.test, &b.file, b.line)));
    Ok(out)
}

/// One file's co-change partners in one repo.
pub struct CoupledRepo {
    pub root: PathBuf,
    pub file: String,
    /// The history read, in words: `365 days before HEAD`, `its last 50 commits`, `365 days before abc1234 (…)`.
    pub history: String,
    pub partners: Vec<Partner>,
    /// Partners before `limit`.
    pub total: usize,
    /// Single-commit partners history keeps no record of (past the 50 it keeps besides every partner sharing two or
    /// more commits): not in `total`.
    pub unrecorded: usize,
    /// When history has nothing to rank (no partner sharing [`RANKABLE_SUPPORT`] commits): why, and the files
    /// structure relates to the file instead, a guess (see [`kc_index::guess`]).
    pub fallback: Option<(String, kc_index::guess::Guesses)>,
}

/// The shared commits a partner needs for history to rank it: with every partner at one shared commit, all tie, and
/// one shared commit is often just the commit that created both (test selection asks the same of co-change alone:
/// [`kc_index::test_select::MIN_COCHANGE_ALONE`]). Below it, `coupled` adds structural guesses, labelled as such.
pub const RANKABLE_SUPPORT: u32 = kc_index::test_select::MIN_COCHANGE_ALONE;

#[derive(Debug, Clone, Serialize)]
pub struct Partner {
    /// Always `history`: the partner is co-change evidence (a structural guess is never a partner).
    pub basis: &'static str,
    #[serde(flatten)]
    pub coupled: vitals::Coupled,
    pub test: bool,
    /// References link the two: `impact --file` on the file reaches the partner, or the file names a type the
    /// partner declares. A partner without one is the hidden coupling `impact` lists in its `coupled` class.
    pub static_link: bool,
}

/// Whether `rel` is a file of the repo: indexed, or any other file discovery lists (a Dockerfile, a YAML file, a
/// script), which has co-change history though no symbols.
fn is_repo_file(conn: &rusqlite::Connection, rel: &str) -> Result<bool> {
    Ok(conn.prepare_cached("SELECT 1 FROM files WHERE path = ?1")?.exists([rel])?
        || conn.prepare_cached("SELECT 1 FROM cfiles WHERE path = ?1")?.exists([rel])?)
}

/// Files that historically change in the same commits as `file`, ranked by recency-weighted shared commits. `file`
/// may be any file of the repo, indexed or not: co-change needs only its history.
pub fn coupled(file: &str, limit: usize, span: &vitals::Span, args: &Scope) -> Result<Out<Vec<CoupledRepo>>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let rel = Path::new(file).strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| file.to_string());
        let (mut by_file, mut spans, dropped) = vitals::coupled_in_counted(root, &o.store, &[&rel], usize::MAX, false, span)?;
        let unrecorded = dropped.get(&rel).copied().unwrap_or(0) as usize;
        let mut list = by_file.remove(&rel).unwrap_or_default();
        let history = spans.remove(&rel).unwrap_or_default();
        let total = list.len();
        let rankable = list.iter().any(|c| c.support >= RANKABLE_SUPPORT);
        // A file history lists is never guessed too.
        let in_history: HashSet<String> = list.iter().map(|c| c.file.clone()).collect();
        list.truncate(limit);
        if !list.is_empty() || span.as_of.is_some() || is_repo_file(o.store.conn(), &rel)? {
            // History too thin to rank: what structure relates the file to, as of the same point as the history.
            let fallback = if rankable {
                None
            } else {
                let pin = match &span.as_of {
                    Some(spec) => vitals::pin_commit(root, spec)?.map(Some),
                    None => Some(None),
                };
                match pin {
                    Some(at) => {
                        let g = kc_index::guess::guess(root, &o.store, &rel, at.as_deref(), &in_history, limit)?;
                        let why = if total == 0 {
                            format!("history had nothing: no commit in {history} changed {rel} with another file")
                        } else {
                            format!("history had nothing to rank: no file shared more than one commit with {rel} in {history} (one shared commit is often the commit that created both)")
                        };
                        let at_words = at.as_deref().map(|a| format!(" as of {}", &a[..a.len().min(10)])).unwrap_or_default();
                        let note = if g.total == 0 {
                            format!("{why}; structure{at_words} relates no other file to it either")
                        } else {
                            format!("{why}; `structural` lists files related to it by structure{at_words} instead: a guess (basis structure, confidence low), not co-change evidence")
                        };
                        Some((note, g))
                    }
                    // As of a point before the repo's first commit: no structure either.
                    None => Some((format!("history had nothing: the repo has no commit by {}", span.as_of.as_deref().unwrap_or("")), kc_index::guess::Guesses::default())),
                }
            };
            let ids = trace::symbols_in_file(o.store.conn(), &rel)?;
            let walk = trace::impact(o.store.conn(), &ids, 3, 1000)?;
            let reached: HashSet<&str> = walk.affected.iter().map(|a| a.file.as_str()).collect();
            let mut partners = Vec::new();
            for c in list {
                let static_link = reached.contains(c.file.as_str()) || names_types_of(&o.store, &rel, &c.file)?;
                partners.push(Partner { basis: "history", test: kc_store::is_test_path(&c.file), static_link, coupled: c });
            }
            found.push(CoupledRepo { root: o.root.clone(), file: rel, history, partners, total, unrecorded, fallback });
        }
        opened.push(o);
    }
    let items: Vec<Value> =
        found
            .iter()
            .map(|r| {
                let mut v = json!({ "repo": repo_name(&r.root), "file": r.file, "history": r.history, "partners": r.partners, "partners_total": r.total });
                if r.unrecorded > 0 {
                    v["partners_unrecorded"] = json!(r.unrecorded);
                }
                if let Some((note, g)) = &r.fallback {
                    v["note"] = json!(note);
                    v["structural"] = json!(g.list);
                    v["structural_total"] = json!(g.total);
                }
                v
            })
            .collect();
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    if let Some((note, _)) = found.iter().find_map(|r| r.fallback.as_ref()) {
        env["note"] = json!(note);
    }
    for r in found.iter().filter(|r| r.fallback.as_ref().is_some_and(|(_, g)| g.not_read > 0)) {
        let n = r.fallback.as_ref().map_or(0, |(_, g)| g.not_read);
        push_incomplete(
            &mut env,
            Incomplete::new(Reason::Capped, format!("structural: {n} files changed since the --as-of commit were not read at it, so links through them are not known"))
                .with_hint(format!("at most {} changed files are read at the commit; a later --as-of has fewer", kc_index::guess::MAX_PIN_FILES)),
        );
    }
    for r in found.iter().filter(|r| r.unrecorded > 0) {
        push_incomplete(
            &mut env,
            Incomplete::new(
                Reason::Capped,
                format!(
                    "partners_unrecorded: {} has {} more partners that shared a single commit with it, past the 50 such the history index keeps; `partners_total` counts the {} recorded",
                    r.file, r.unrecorded, r.total
                ),
            )
            .with_hint(format!("git log --format= --name-only -- {} lists every file changed with it", r.file)),
        );
    }
    if found.iter().any(|r| r.total > r.partners.len()) {
        env["truncated"] = json!(true);
    }
    finish(&mut env);
    Ok(Out { env, data: found })
}

pub struct TestsParams {
    /// Files (repo-relative or absolute) or symbols (name or `Type.member`).
    pub targets: Vec<String>,
    /// `working` or a git revision: the changed lines instead of targets.
    pub diff: Option<String>,
    /// Tests to list at most in the tight list, and in `left_out` (at least [`LEFT_OUT_SHOWN`]).
    pub limit: usize,
    /// The evidence policy of the reverse-edge walk (see [`kc_store::answer::EvidencePolicy`]); empty for
    /// [`TESTS_EVIDENCE`].
    pub evidence: String,
}

/// The evidence policy the tests walk takes by default: `all`, ambiguous edges followed too. A test reached only that
/// way ranks low (the model gives an ambiguous edge no weight) but is listed: missing it is what costs.
pub const TESTS_EVIDENCE: &str = "all";

pub struct TestsRepo {
    pub root: PathBuf,
    /// The changed files the selection starts from.
    pub changed: Vec<String>,
    /// Targets that named neither an indexed file nor a symbol.
    pub unknown: Vec<String>,
    pub selection: kc_index::test_select::Selection,
    /// Selected tests cut by `limit`, moved to the head of `left_out`.
    pub over_limit: usize,
}

/// What the tight list is, in every answer: the most likely relevant tests, not the only ones to run.
pub const TESTS_ARE: &str = "most likely relevant: the tests whose signals (reverse edges, naming, co-change) make them most likely to exercise the change, not every test that does; left_out ranks the other candidates, which may also fail, and a test no signal reaches (reflection, configuration, a path past the walk) isn't listed at all";

/// The tests to run for a change: test files ranked by reverse edges, naming and co-change, the most likely relevant
/// first and the rest as left out.
pub fn tests(p: &TestsParams, args: &Scope) -> Result<Out<Vec<TestsRepo>>> {
    if p.targets.is_empty() == p.diff.is_none() {
        bail!("give targets (files or symbols) or a diff, not both");
    }
    let edges = edge_filter(&[], if p.evidence.is_empty() { TESTS_EVIDENCE } else { &p.evidence })?;
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let conn = o.store.conn();
        let mut ids: Vec<i64> = Vec::new();
        let mut changed: Vec<String> = Vec::new();
        let mut unknown = Vec::new();
        if let Some(d) = &p.diff {
            for c in kc_index::diff::changes(root, Some(d.as_str()).filter(|d| *d != "working"))?.into_iter().filter(|c| !c.deleted) {
                ids.extend(trace::symbols_in_ranges(conn, &c.file, &c.ranges)?);
                changed.push(c.file);
            }
        } else {
            let mut is_file = conn.prepare_cached("SELECT 1 FROM files WHERE path = ?1")?;
            for t in &p.targets {
                let rel = Path::new(t).strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| t.clone());
                if is_file.exists([&rel])? {
                    ids.extend(trace::symbols_in_file(conn, &rel)?);
                    changed.push(rel);
                } else if let Some(d) = query::definitions(conn, t)?.into_iter().next() {
                    ids.push(d.id);
                    changed.push(d.file);
                } else {
                    unknown.push(t.clone());
                }
            }
        }
        changed.sort();
        changed.dedup();
        ids.sort_unstable();
        ids.dedup();
        if changed.is_empty() && p.diff.is_none() {
            opened.push(o);
            continue;
        }
        use kc_index::test_select::{DEPTH, MAX_WALK};
        // Under the evidence policy. A diff's removed definitions aren't walked (as `impact --diff` does): code still
        // naming them doesn't build, so it is part of the change itself, and parsing the pre-change side cost more
        // than the rest of the query on a small repo.
        let walk = trace::impact_filtered(conn, &ids, DEPTH, MAX_WALK, &edges)?;
        let mut selection = kc_index::test_select::select_from(root, &o.store, &walk, &changed)?;
        let over_limit = selection.selected.len().saturating_sub(p.limit);
        let cut: Vec<_> = selection.selected.drain(selection.selected.len() - over_limit..).collect();
        selection.left_out.splice(0..0, cut);
        found.push(TestsRepo { root: o.root.clone(), changed, unknown, selection, over_limit });
        opened.push(o);
    }
    let shown = p.limit.max(LEFT_OUT_SHOWN);
    let items: Vec<Value> = found
        .iter()
        .map(|r| {
            let s = &r.selection;
            json!({
                "repo": repo_name(&r.root),
                "changed": r.changed,
                "unknown": r.unknown,
                "tests_are": TESTS_ARE,
                "tests": s.selected,
                // Every candidate: the tight list is a sample of them by design (`sampled`).
                "tests_total": s.selected.len() + s.left_out.len(),
                "left_out": s.left_out.iter().take(shown).collect::<Vec<_>>(),
                "left_out_total": s.left_out.len(),
                "over_limit": r.over_limit,
                "truncated": s.truncated,
            })
        })
        .collect();
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    for r in found.iter().filter(|r| r.over_limit > 0) {
        push_incomplete(
            &mut env,
            Incomplete::new(
                Reason::Capped,
                format!("tests: {} more of the most likely relevant tests in {} are past the limit, at the head of left_out", r.over_limit, repo_name(&r.root)),
            )
            .with_hint("raise the limit (--limit / limit)"),
        );
    }
    report_policy(&mut env, &edges);
    finish(&mut env);
    Ok(Out { env, data: found })
}

/// Left-out tests listed in the JSON answer at least (all are counted; a larger `limit` lists more).
const LEFT_OUT_SHOWN: usize = 20;

pub struct HotspotsData {
    pub repos: Vec<(PathBuf, Vec<FileVitals>)>,
    /// What each repo's history covers, in the same order.
    pub history: Vec<vitals::History>,
    /// Per repo, in the same order: a shallow clone whose only commit is its boundary, so no history counts.
    pub boundary_only: Vec<bool>,
    /// With one file asked for: its biomarkers, with reasons.
    pub findings: Vec<kc_index::health::Finding>,
    pub path: Option<String>,
    pub top: usize,
}

#[derive(Serialize)]
struct RepoHealth {
    repo: String,
    files: usize,
    mean_health: f64,
    worst_file: Option<String>,
    worst_concern: Option<f64>,
}

/// Ranked files of one repo (or one file's vitals and biomarkers with `path`); across several repos, repos ranked
/// by health. Windows end at `as_of` (unix seconds, `YYYY-MM-DD` or `head`), else `KNOWCODE_NOW`, else now.
pub fn hotspots(path: Option<&str>, top: usize, as_of: Option<&str>, args: &Scope) -> Result<Out<HotspotsData>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut repos = Vec::new();
    let mut history = Vec::new();
    let mut boundary_only = Vec::new();
    let mut findings = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let anchor = vitals::anchor(root, as_of)?;
        let mut v = vitals::hotspots_at(root, &o.store, anchor)?;
        if let Some(p) = path {
            let rel = Path::new(p).strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string());
            v.retain(|f| f.file == rel);
            if !v.is_empty() {
                findings = kc_index::health::findings(&o.root, &o.store, Some(&rel))?;
            }
        }
        history.push(vitals::history(root, &o.store, anchor)?);
        boundary_only.push(ownership::only_boundary(root));
        repos.push((o.root.clone(), v));
        opened.push(o);
    }
    let mut env = if let [(_, files)] = repos.as_slice() {
        let total = files.len();
        let shown: Vec<&FileVitals> = files.iter().take(top).collect();
        let mut items = json!(shown);
        if path.is_some()
            && let Some(first) = items.get_mut(0)
        {
            first["findings"] = json!(findings);
        }
        let mut env = envelope(&opened, items, total, shown.len());
        env["history"] = json!(history[0]);
        if boundary_only[0] {
            env["history"]["no_countable_history"] = json!(true);
        }
        env
    } else {
        // In-repo risk is a percentile, so it can't compare repos. Re-score every file against the whole
        // workspace with the same formula, then average health per repo.
        let all: Vec<&FileVitals> = repos.iter().flat_map(|(_, f)| f.iter()).collect();
        let pooled = vitals::concern(&all);
        let mut order: Vec<usize> = (0..all.len()).collect();
        order.sort_by(|a, b| pooled[*b].partial_cmp(&pooled[*a]).unwrap());
        let n_all = all.len();
        let mut health = vec![10.0; n_all];
        for (rank, &i) in order.iter().enumerate() {
            let risk = if n_all <= 1 { 0.0 } else { 100.0 * (n_all - 1 - rank) as f64 / (n_all - 1) as f64 };
            health[i] = 10.0 - (risk * 9.0 / 100.0).floor();
        }
        let mut offset = 0;
        let mut ranked: Vec<RepoHealth> = Vec::new();
        for (root, f) in &repos {
            let h = &health[offset..offset + f.len()];
            offset += f.len();
            if f.is_empty() {
                continue;
            }
            ranked.push(RepoHealth {
                repo: repo_name(root),
                files: f.len(),
                mean_health: round1(h.iter().sum::<f64>() / f.len() as f64),
                worst_file: f.first().map(|x| x.file.clone()),
                worst_concern: f.first().map(|x| x.concern),
            });
        }
        ranked.sort_by(|a, b| a.mean_health.partial_cmp(&b.mean_health).unwrap());
        let n = ranked.len();
        envelope(&opened, json!(ranked), n, n)
    };
    if repos.len() > 1 {
        let dormant: Vec<Value> = repos
            .iter()
            .zip(&history)
            .zip(&boundary_only)
            .filter_map(|(((root, _), h), &none)| {
                let mut v = json!({ "repo": repo_name(root), "dormant_months": h.dormant_months, "last_commit": h.last_commit });
                if none {
                    v["no_countable_history"] = json!(true);
                }
                (none || h.dormant_months.is_some()).then_some(v)
            })
            .collect();
        if !dormant.is_empty() {
            env["dormant"] = json!(dormant);
        }
    }
    finish(&mut env);
    Ok(Out { env, data: HotspotsData { repos, history, boundary_only, findings, path: path.map(str::to_string), top } })
}

pub struct OwnershipParams {
    /// A file or directory (repo-relative or absolute); none for the whole repo.
    pub path: Option<String>,
    /// Authors (names or emails) to treat as gone: the off-boarding simulation.
    pub without: Vec<String>,
    /// Directory depth of the areas listed under a directory or the repo.
    pub depth: usize,
    /// Areas or files listed at most.
    pub top: usize,
    pub as_of: Option<String>,
}

/// The off-boarding simulation: what changes if `authors` leave.
#[derive(Serialize)]
pub struct Offboarding {
    pub authors: Vec<String>,
    /// Names in `authors` that aren't authors of this repo.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
    /// Files (within the path) whose top contributor (by lines added) is one of them, and their current lines.
    pub top_contributor_files: usize,
    pub top_contributor_lines: u64,
    /// The largest of those files.
    pub files: Vec<(String, u64)>,
    /// `contributors_gone` (share of lines added by former authors): before and after they leave.
    pub gone_before: f64,
    pub gone_after: f64,
}

/// Authors of a file's surviving lines listed (`contributors_total` counts them all).
const SURVIVORS_SHOWN: usize = 5;

/// One repo's ownership answer.
pub struct OwnershipRepo {
    pub root: PathBuf,
    pub coverage: ownership::Coverage,
    /// The file, directory or repo asked about.
    pub target: ownership::Knowledge,
    /// For a directory or the repo: its areas, largest first.
    pub areas: Vec<ownership::Knowledge>,
    /// Areas before `top`.
    pub areas_total: usize,
    pub offboarding: Option<Offboarding>,
    /// For a file: its current lines by who last changed them (`git blame`), with the uncommitted and automated
    /// lines counted apart. Directories and repos don't get it (a blame per file).
    pub surviving: Option<(Vec<ownership::Survivor>, u64, u64)>,
}

/// Historical contribution: the top contributor (by lines added), the truck factor and the share of lines added by
/// authors who have left, for a file, a directory or the repo, its areas, and with `without` the off-boarding
/// simulation. For a file, also its surviving lines by author.
pub fn ownership(p: &OwnershipParams, args: &Scope) -> Result<Out<Vec<OwnershipRepo>>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let anchor = vitals::anchor(root, p.as_of.as_deref())?;
        let model = Ownership::load(root, &o.store, anchor, &p.without)?;
        let rel = p.path.as_deref().map(|p| {
            let r = Path::new(p).strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string());
            r.trim_end_matches('/').trim_start_matches("./").to_string()
        });
        let files = model.files();
        let is_file = rel.as_deref().is_some_and(|r| files.binary_search(&r).is_ok());
        let (target, areas) = match rel.as_deref() {
            Some(r) if is_file => (model.file(r), Vec::new()),
            Some(r) if !r.is_empty() && r != "." => {
                let depth = r.split('/').count() + p.depth;
                let areas: Vec<_> = model.areas(depth).into_iter().filter(|k| k.path.starts_with(&format!("{r}/"))).collect();
                let t = model.dir(r);
                if t.files == 0 {
                    opened.push(o);
                    continue;
                }
                (t, areas)
            }
            _ => (model.dir("."), model.areas(p.depth)),
        };
        let areas_total = areas.len();
        let areas: Vec<_> = areas.into_iter().take(p.top).collect();
        let offboarding = if p.without.is_empty() {
            None
        } else {
            let before = Ownership::load(root, &o.store, anchor, &[])?;
            let scope = |f: &str| match rel.as_deref() {
                Some(r) if is_file => f == r,
                Some(r) if !r.is_empty() && r != "." => f.starts_with(&format!("{r}/")),
                _ => true,
            };
            let mut main: Vec<(String, u64)> = p.without.iter().flat_map(|w| model.main_files_of(w)).filter(|(f, _)| scope(f)).collect();
            main.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            main.dedup();
            let gone_before = match rel.as_deref() {
                Some(r) if is_file => before.file(r).contributors_gone,
                Some(r) => before.dir(r).contributors_gone,
                None => before.dir(".").contributors_gone,
            };
            Some(Offboarding {
                authors: p.without.clone(),
                unknown: p.without.iter().filter(|w| !model.knows_author(w)).cloned().collect(),
                top_contributor_files: main.len(),
                top_contributor_lines: main.iter().map(|m| m.1).sum(),
                files: main.into_iter().take(p.top).collect(),
                gone_before,
                gone_after: target.contributors_gone,
            })
        };
        // The code as it is, beside its history: one blame, so for a file only, and only for now (not `as_of`).
        let surviving = if is_file && p.as_of.is_none() { rel.as_deref().and_then(|r| ownership::surviving(root, r)) } else { None };
        found.push(OwnershipRepo { root: o.root.clone(), coverage: model.coverage.clone(), target, areas, areas_total, offboarding, surviving });
        opened.push(o);
    }
    let items: Vec<Value> = found
        .iter()
        .map(|r| {
            let mut v = json!({ "repo": repo_name(&r.root), "coverage": r.coverage, "target": r.target });
            if !r.areas.is_empty() {
                v["areas"] = json!(r.areas);
                v["areas_total"] = json!(r.areas_total);
            }
            if let Some(off) = &r.offboarding {
                v["offboarding"] = json!(off);
            }
            if let Some((authors, uncommitted, automated)) = &r.surviving {
                v["surviving"] = json!({
                    "contributors": authors.iter().take(SURVIVORS_SHOWN).collect::<Vec<_>>(),
                    "contributors_total": authors.len(),
                    "uncommitted": uncommitted,
                    "automated": automated,
                });
            }
            v
        })
        .collect();
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    if found.iter().any(|r| r.areas_total > r.areas.len() || r.offboarding.as_ref().is_some_and(|o| o.top_contributor_files > o.files.len())) {
        env["truncated"] = json!(true);
    }
    finish(&mut env);
    Ok(Out { env, data: found })
}

pub struct ReviewersParams {
    /// Changed files (repo-relative or absolute).
    pub files: Vec<String>,
    /// `working` or a git revision: the changed files instead.
    pub diff: Option<String>,
    pub limit: usize,
    pub as_of: Option<String>,
}

pub struct ReviewersRepo {
    pub root: PathBuf,
    /// The changed files the ranking uses.
    pub files: Vec<String>,
    /// Changed files left out of the ranking: changelogs, lockfiles, generated files.
    pub left_out: Vec<String>,
    /// For a revision diff: the commit the change starts from; ownership is read from the history up to it.
    pub base: Option<String>,
    /// People left out: the authors and committers of the diff's commits (and the repo's `user.email` when the
    /// working tree has changes), or for files and `working`, the repo's `user.email`.
    pub excluded: Vec<String>,
    pub reviewers: Vec<ownership::Reviewer>,
    /// Reviewers before `limit`.
    pub total: usize,
}

/// Who should review a change: authors ranked by recency-weighted lines added to the changed files (changelogs,
/// lockfiles and generated files aside), former authors left out. For files or `working`, the repo's `user.email` is
/// left out. For a revision, the people who made the change are (its commits' authors and committers), and
/// ownership is read from the history up to the diff's base, so the change doesn't vouch for its own authors.
pub fn reviewers(p: &ReviewersParams, args: &Scope) -> Result<Out<Vec<ReviewersRepo>>> {
    if p.files.is_empty() == p.diff.is_none() {
        bail!("give files or a diff, not both");
    }
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let me = ownership::user_email(root);
        let rev = p.diff.as_deref().filter(|d| *d != "working");
        let changed: Vec<String> = match &p.diff {
            Some(_) => kc_index::diff::changes(root, rev)?.into_iter().map(|c| c.file).collect(),
            None => p
                .files
                .iter()
                .map(|f| Path::new(f).strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| f.clone()))
                .collect(),
        };
        let (base, excluded, anchor) = match rev {
            Some(r) => {
                let (base, time) = ownership::diff_base(root, r).ok_or_else(|| anyhow::anyhow!("{r} shares no history with HEAD"))?;
                let mut ex = ownership::change_authors(root, &base);
                if !kc_index::diff::changes(root, None)?.is_empty() {
                    ex.extend(me.clone());
                }
                ex.sort();
                ex.dedup();
                let anchor = match &p.as_of {
                    Some(a) => vitals::anchor(root, Some(a))?,
                    None => vitals::Anchor { time, explicit: true },
                };
                (Some(base), ex, anchor)
            }
            None => (None, me.into_iter().collect(), vitals::anchor(root, p.as_of.as_deref())?),
        };
        let model = Ownership::load_upto(root, &o.store, anchor, &[], base.as_deref())?;
        let (left_out, files): (Vec<String>, Vec<String>) = changed.into_iter().partition(|f| ownership::is_incidental(f));
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let mut list = model.reviewers(&refs, &excluded, usize::MAX);
        let total = list.len();
        list.truncate(p.limit);
        found.push(ReviewersRepo { root: o.root.clone(), files, left_out, base, excluded, reviewers: list, total });
        opened.push(o);
    }
    let items: Vec<Value> = found
        .iter()
        .map(|r| {
            json!({
                "repo": repo_name(&r.root), "files": r.files, "left_out": r.left_out, "base": r.base, "excluded": r.excluded,
                "reviewers": r.reviewers, "reviewers_total": r.total,
            })
        })
        .collect();
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    if found.iter().any(|r| r.total > r.reviewers.len()) {
        env["truncated"] = json!(true);
    }
    finish(&mut env);
    Ok(Out { env, data: found })
}

/// Repo edges grouped by the repo on the other side, for `overview`: one entry per repo, with every manifest line
/// declaring a dependency on it (`sites`, compacted by [`compact_sites`]), the number of dependencies, and the
/// products used.
fn repo_links<'a>(edges: impl Iterator<Item = (String, &'a RepoEdge)>) -> Vec<Value> {
    // Per repo: its name, the manifest sites, the products used.
    type Link<'e> = (String, Vec<(&'e str, u32)>, Vec<&'e str>);
    let mut by_repo: Vec<Link> = Vec::new();
    for (repo, e) in edges {
        let at = match by_repo.iter().position(|(r, ..)| *r == repo) {
            Some(i) => i,
            None => {
                by_repo.push((repo, Vec::new(), Vec::new()));
                by_repo.len() - 1
            }
        };
        by_repo[at].1.push((&e.file, e.line));
        by_repo[at].2.extend(e.products.iter().map(String::as_str));
    }
    by_repo
        .into_iter()
        .map(|(repo, sites, mut products)| {
            products.sort_unstable();
            products.dedup();
            json!({ "repo": repo, "dependencies": sites.len(), "sites": compact_sites(&sites), "products": products })
        })
        .collect()
}

/// Manifest lines as `file:line`, one entry per file, consecutive lines joined into a range in which every line is a
/// site: `package.json:119-145`, `a.csproj:15-16,21`.
pub fn compact_sites(sites: &[(&str, u32)]) -> Vec<String> {
    let mut files: Vec<(&str, Vec<u32>)> = Vec::new();
    for &(f, l) in sites {
        match files.iter_mut().find(|(g, _)| *g == f) {
            Some((_, lines)) => lines.push(l),
            None => files.push((f, vec![l])),
        }
    }
    files
        .into_iter()
        .map(|(f, mut lines)| {
            lines.sort_unstable();
            lines.dedup();
            let mut runs: Vec<String> = Vec::new();
            let mut i = 0;
            while i < lines.len() {
                let mut j = i;
                while j + 1 < lines.len() && lines[j + 1] == lines[j] + 1 {
                    j += 1;
                }
                runs.push(if j == i { lines[i].to_string() } else { format!("{}-{}", lines[i], lines[j]) });
                i = j + 1;
            }
            format!("{f}:{}", runs.join(","))
        })
        .collect()
}

/// Tables listed in an overview at most; `knowcode tables` lists them all.
const OVERVIEW_TABLES: usize = 20;

/// An overview's tables: all of them when they are few, else the most accessed, in the model's order.
fn overview_tables(tables: &[db::Table]) -> Vec<Value> {
    let mut keep: Vec<usize> = (0..tables.len()).collect();
    if keep.len() > OVERVIEW_TABLES {
        keep.sort_by_key(|&i| std::cmp::Reverse(tables[i].reads + tables[i].writes + tables[i].other - tables[i].read_writes));
        keep.truncate(OVERVIEW_TABLES);
        keep.sort_unstable();
    }
    keep.into_iter()
        .map(|i| {
            let t = &tables[i];
            let mut v = json!({
                "context": t.context, "table": t.table, "entity": t.entity, "dbset": t.dbset,
                "reads": t.reads, "writes": t.writes, "other": t.other,
            });
            if let Some(n) = &t.navigation {
                v["navigation"] = json!(n);
            }
            v
        })
        .collect()
}

/// What a repo is: languages, packages, dependencies both ways, entry points, key types and top hotspots. Across
/// several repos, a one-line summary each plus the repo edges.
pub fn overview(args: &Scope, focus: Option<&str>, map_budget: usize) -> Result<Out<()>> {
    let roots = target_repos(&args.repos)?;
    let deps_out = deps(args, false)?;
    let mut opened = Vec::new();
    let mut items = Vec::new();
    let mut stale_rank: Vec<Incomplete> = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let conn = o.store.conn();
        let stats = query::stats(conn)?;
        let name = repo_name(root);
        let me = root.to_string_lossy().into_owned();
        let depends_on =
            repo_links(deps_out.data.edges.iter().filter(|(from, _)| from == root).map(|(_, e)| (repo_name(Path::new(&e.to_repo)), e)));
        let dependents = repo_links(deps_out.data.edges.iter().filter(|(_, e)| e.to_repo == me).map(|(from, e)| (repo_name(from), e)));
        if roots.len() > 1 {
            items.push(json!({
                "repo": name,
                "files": stats.files,
                "lines": stats.lines,
                "symbols": stats.symbols_by_kind.values().sum::<usize>(),
                "depends_on": depends_on,
                "dependents": dependents,
            }));
        } else {
            let pkgs = manifests::packages(conn)?;
            let packages: Vec<Value> = pkgs
                .iter()
                .map(|p| {
                    json!({
                        "kind": p.kind, "name": p.name, "manifest": format!("{}:{}", p.file, p.line),
                        "targets": p.targets.iter().map(|t| format!("{} ({})", t.name, t.kind)).collect::<Vec<_>>(),
                    })
                })
                .collect();
            let (mut entry, _) = query::tagged(conn, "entrypoint", usize::MAX)?;
            let entry_total = entry.len();
            // The most central entry points first, when there are more than fit.
            if entry.len() > 10 {
                let (rank, stale) = o.store.file_rank_at(&[], kc_store::rank::Flow::Uses, true)?;
                if let Some(g) = stale {
                    stale_rank.push(kc_store::answer::stale_derived(&format!("entry points ({name}): the most central, by file ranking"), &g, &o.generation));
                }
                entry.sort_by(|a, b| rank.score(&b.file).total_cmp(&rank.score(&a.file)));
                entry.truncate(10);
            }
            // SwiftUI views are a Swift repo's stat; other repos leave it out.
            let swift = stats.files_by_lang.contains_key("swift");
            let n_views = if swift { query::tagged(conn, "swiftui_view", 0)?.1 } else { 0 };
            let hot = vitals::hotspots(root, &o.store)?;
            let edges = o.store.file_edges()?;
            let mut key_types = o.store.derived("key_types:all", &o.store.generation()?, || query::key_types(conn, &edges, usize::MAX))?;
            let key_types_total = key_types.len();
            key_types.truncate(12);
            let model = o.store.db_model()?;
            let areas = kc_index::areas::areas(root, &o.store)?;
            items.push(json!({
                "repo": name,
                "files": stats.files,
                "lines": stats.lines,
                "languages": stats.files_by_lang,
                "symbols": stats.symbols_by_kind,
                "swiftui_views": n_views,
                "packages": packages,
                "depends_on": depends_on,
                "dependents": dependents,
                "entry_points": entry.iter().map(|s| json!({ "symbol": s.qualified(), "file": s.file, "line": s.line })).collect::<Vec<_>>(),
                "key_types": key_types,
                "areas": overview_areas(&areas),
                "areas_total": areas.areas.len(),
                "map": if map_budget == 0 {
                    Value::Null
                } else if focus.is_some() {
                    json!(kc_store::map::ranked_map(conn, &o.store.file_edges()?, focus, map_budget)?)
                } else {
                    let edges = o.store.file_edges()?;
                    json!(o.store.derived(&format!("map:{}:{map_budget}", kc_store::map::MAP_VERSION), &o.store.generation()?, || {
                        kc_store::map::ranked_map(conn, &edges, None, map_budget)
                    })?)
                },
                "top_hotspots": hot.iter().take(5).map(|f| json!({ "file": f.file, "risk": f.risk, "concern": f.concern })).collect::<Vec<_>>(),
                "top_hotspots_total": hot.len(),
                "entry_points_total": entry_total,
                "key_types_total": key_types_total,
                "tables": overview_tables(&model.tables),
                "tables_total": model.tables.len(),
                "procedures": model.procedures.len(),
            }));
            if !swift
                && let Some(item) = items.last_mut().and_then(Value::as_object_mut)
            {
                item.remove("swiftui_views");
            }
            if model.is_empty()
                && let Some(item) = items.last_mut().and_then(Value::as_object_mut)
            {
                item.remove("tables");
                item.remove("tables_total");
                item.remove("procedures");
            }
            let large = kc_index::skipped_large(&o.store)?;
            if !large.is_empty()
                && let Some(item) = items.last_mut().and_then(Value::as_object_mut)
            {
                item.insert("skipped_large".into(), large_files(&large));
            }
        }
        opened.push(o);
    }
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    for i in stale_rank {
        push_incomplete(&mut env, i);
    }
    Ok(Out { env, data: () })
}

/// A deterministic, facts-only Markdown summary of one repo: the same index gives the same text.
pub fn docs(args: &Scope, map_budget: usize) -> Result<Vec<(PathBuf, String)>> {
    let roots = target_repos(&args.repos)?;
    let mut out = Vec::new();
    for root in &roots {
        let one = Scope { repos: vec![root.clone()], no_sync: args.no_sync };
        let env = overview(&one, None, map_budget)?.env;
        let r = &env["items"][0];
        out.push((root.clone(), docs_markdown(&env, r)));
    }
    Ok(out)
}

fn docs_markdown(env: &Value, r: &Value) -> String {
    use std::fmt::Write;
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let arr = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let mut md = String::new();
    let commit = env["commit"].as_str().map(|c| &c[..c.len().min(12)]).unwrap_or("uncommitted");
    let _ = writeln!(md, "# {}\n", s(&r["repo"]));
    let _ = writeln!(
        md,
        "Generated by `knowcode docs` {} from the index at commit `{commit}`{}. Facts only: every line comes from the \
         code graph, and nothing here is written by a model.\n",
        env!("CARGO_PKG_VERSION"),
        if env["dirty"].as_bool() == Some(true) { " with uncommitted changes" } else { "" },
    );
    let langs: Vec<String> = r["languages"].as_object().map(|m| m.iter().map(|(k, v)| format!("{k} {v}")).collect()).unwrap_or_default();
    let _ = writeln!(md, "## At a glance\n");
    let _ = writeln!(md, "- **Files:** {} ({}), {} lines", r["files"], langs.join(", "), r["lines"]);
    let lang = r["languages"].as_object().map(repo_lang).unwrap_or_default();
    let syms: Vec<String> =
        r["symbols"].as_object().map(|m| m.iter().map(|(k, v)| format!("{v} {}", kind_word(k, &lang))).collect()).unwrap_or_default();
    let _ = writeln!(md, "- **Symbols:** {}", syms.join(", "));
    if r["swiftui_views"].as_u64().unwrap_or(0) > 0 {
        let _ = writeln!(md, "- **SwiftUI views:** {}", r["swiftui_views"]);
    }
    for (label, key) in [("Depends on", "depends_on"), ("Used by", "dependents")] {
        let edges: Vec<String> = arr(&r[key])
            .iter()
            .map(|e| {
                let products = arr(&e["products"]).iter().map(&s).collect::<Vec<_>>().join(", ");
                let sites = arr(&e["sites"]).iter().map(|p| format!("`{}`", s(p))).collect::<Vec<_>>().join(", ");
                format!("{} via {sites}{}", s(&e["repo"]), if products.is_empty() { String::new() } else { format!(" ({products})") })
            })
            .collect();
        if !edges.is_empty() {
            let _ = writeln!(md, "- **{label}:** {}", edges.join("; "));
        }
    }
    let packages = arr(&r["packages"]);
    if !packages.is_empty() {
        let _ = writeln!(md, "\n## Packages and targets\n");
        for p in packages {
            let targets = arr(&p["targets"]).iter().map(|t| format!("`{}`", s(t))).collect::<Vec<_>>().join(", ");
            let _ = writeln!(
                md,
                "- {} **{}** (`{}`){}",
                s(&p["kind"]),
                s(&p["name"]),
                s(&p["manifest"]),
                if targets.is_empty() { String::new() } else { format!(": {targets}") }
            );
        }
    }
    let entries = arr(&r["entry_points"]);
    if !entries.is_empty() {
        let _ = writeln!(md, "\n## Entry points\n");
        for e in entries {
            let _ = writeln!(md, "- `{}` at `{}:{}`", s(&e["symbol"]), s(&e["file"]), e["line"]);
        }
    }
    let types = arr(&r["key_types"]);
    if !types.is_empty() {
        let _ = writeln!(md, "\n## Key types\n\nTypes used by the most other files.\n\n| Type | Kind | Defined at | Used by files |\n|---|---|---|---:|");
        for t in types {
            let (kind, file) = (s(&t["kind"]), s(&t["file"]));
            let _ = writeln!(md, "| `{}` | {} | `{file}:{}` | {} |", s(&t["qualified"]), kind_word(&kind, file_lang(&file)), t["line"], t["used_by_files"]);
        }
    }
    if let Some(files) = r["map"]["files"].as_array().filter(|f| !f.is_empty()) {
        let _ = writeln!(
            md,
            "\n## Map\n\nThe most central files (PageRank over resolved file dependencies), with the types they define and \
             the members other files use.\n"
        );
        for f in files {
            let file = s(&f["file"]);
            let lang = match file.rsplit('.').next() {
                Some("py") => "python",
                Some("rs") => "rust",
                Some("cs") => "csharp",
                Some("ts") => "typescript",
                Some("tsx") => "tsx",
                Some("cfm" | "cfc" | "cfs") => "cfml",
                _ => "swift",
            };
            let _ = writeln!(md, "### `{file}`\n\n```{lang}");
            for sym in arr(&f["symbols"]) {
                let sig: String = s(&sym["signature"]).chars().take(140).collect();
                let _ = writeln!(md, "{sig}  // :{}", sym["line"]);
            }
            let _ = writeln!(md, "```\n");
        }
        let omitted = r["map"]["omitted_files"].as_u64().unwrap_or(0);
        if omitted > 0 {
            let _ = writeln!(md, "{omitted} more files are left out of the map to keep it short.");
        }
    }
    let hot = arr(&r["top_hotspots"]);
    if !hot.is_empty() {
        let _ = writeln!(
            md,
            "\n## Hotspots\n\nFiles ranked by concern (churn over the last year, bug-fix commits, complexity, nesting, function length, coupling); risk is \
             the percentile within this repo.\n\n| File | Risk | Concern |\n|---|---:|---:|"
        );
        for h in hot {
            let _ = writeln!(md, "| `{}` | {} | {} |", s(&h["file"]), h["risk"], h["concern"]);
        }
    }
    md
}

pub struct SearchParams {
    pub opts: search::SearchOptions,
    pub limit: usize,
    pub offset: usize,
    /// Lines of context around each hit.
    pub context: usize,
    /// One item per file instead of per line.
    pub files_only: bool,
}

/// One file in a `files_only` answer.
#[derive(Serialize)]
pub struct FileSummary {
    #[serde(skip)]
    pub repo: usize,
    pub file: String,
    pub matches: usize,
    /// The best-ranked hit's kind and line.
    pub best: &'static str,
    pub line: u32,
}

pub struct SearchData {
    pub roots: Vec<PathBuf>,
    pub hits: Vec<search::PageHit>,
    pub files: Vec<FileSummary>,
    pub total: usize,
    pub total_files: usize,
    pub by_kind: BTreeMap<&'static str, usize>,
    pub next_offset: Option<usize>,
    pub multi: bool,
    pub capped: bool,
}

/// Full-text or regex search over every searchable file, ranked: definitions, then declarations, then code, then
/// tests, comments and prose. Repos are checked for freshness in parallel and candidates read on a shared pool.
pub fn search(p: &SearchParams, args: &Scope) -> Result<Out<SearchData>> {
    let t0 = std::time::Instant::now();
    let roots = target_repos(&args.repos)?;
    let matcher = search::compile(&p.opts)?;
    let threads = SyncOptions::default().threads;
    let per_repo = (threads / roots.len().max(1)).max(1);
    let opened = pool_map(roots.iter().enumerate().collect(), threads, |(i, root)| -> Result<_> {
        let mut o = open_fresh(root, args.no_sync)?;
        let (content, files) = if args.no_sync {
            (None, o.store.cfiles()?)
        } else {
            let (report, files) = content::sync_content_files(root, &mut o.store, per_repo)?;
            (Some(report), files)
        };
        // The files as the sync left them: the list it returns holds their state before.
        let content_gen = cursor::content_gen(root, &o.store.cfiles()?);
        let (files, stats) = search::candidates(&o.store, files, &matcher, i)?;
        Ok((o, files, stats, content, content_gen))
    });
    let mut repos = Vec::new();
    let mut scan_files = Vec::new();
    let mut gens = Vec::new();
    let (mut searchable, mut candidates, mut check_ms) = (0, 0, 0.0f64);
    for (r, root) in opened.into_iter().zip(&roots) {
        let (o, mut files, stats, content, content_gen) = match r {
            Ok(r) => r,
            Err(e) => {
                left_out(&roots, root, e)?;
                continue;
            }
        };
        // Numbered by the repos answering: one left out is not among them.
        files.iter_mut().for_each(|f| f.repo = repos.len());
        gens.push(content_gen);
        searchable += stats.files;
        candidates += stats.candidates;
        check_ms = check_ms.max(content.map_or(0.0, |c| c.total_ms));
        scan_files.extend(files);
        repos.push(o);
    }
    let roots: Vec<PathBuf> = repos.iter().map(|o| o.root.clone()).collect();
    let fresh_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let scanned = search::scan(&roots, &scan_files, &matcher, threads, p.opts.max_lines);
    let scan = scanned.stats.clone();
    let read_ms = t0.elapsed().as_secs_f64() * 1000.0 - fresh_ms;
    // Class hits with each repo's symbols, repos in parallel (each owns its connection).
    let mut by_repo: Vec<Vec<&search::FileHits>> = (0..repos.len()).map(|_| Vec::new()).collect();
    for fh in &scanned.files {
        by_repo[fh.repo].push(fh);
    }
    let classified = pool_map(repos.into_iter().zip(by_repo).collect(), threads, |(o, files)| -> Result<_> {
        let mut out = Vec::with_capacity(files.len());
        let mut scratch = search::Scratch::default();
        for (i, fh) in files.into_iter().enumerate() {
            out.push(search::classify(&o.root, &o.store, &scanned, fh, i as u32, &matcher, &mut scratch)?);
        }
        Ok((o, out))
    });
    // One file table across repos; hit file indexes are renumbered into it.
    let mut repos = Vec::new();
    let mut file_data = Vec::new();
    let mut hits: Vec<search::Hit> = Vec::new();
    for r in classified {
        let (o, per_file) = r?;
        repos.push(o);
        for (fd, fhits) in per_file {
            let idx = file_data.len() as u32;
            file_data.push(fd);
            hits.extend(fhits.into_iter().map(|mut h| {
                h.file = idx;
                h
            }));
        }
    }
    let names: Vec<String> = roots.iter().map(|r| repo_name(r)).collect();
    search::rank(&mut hits, &file_data, &names);
    let scan_ms = t0.elapsed().as_secs_f64() * 1000.0 - fresh_ms;

    let total = hits.len();
    let mut by_kind = BTreeMap::new();
    let mut files: Vec<FileSummary> = Vec::new();
    let mut summary_of: Vec<Option<usize>> = vec![None; file_data.len()];
    for h in &hits {
        *by_kind.entry(h.kind).or_insert(0) += 1;
        match summary_of[h.file as usize] {
            Some(i) => files[i].matches += 1,
            None => {
                summary_of[h.file as usize] = Some(files.len());
                let fd = &file_data[h.file as usize];
                files.push(FileSummary { repo: fd.repo, file: fd.path.clone(), matches: 1, best: h.kind, line: h.line });
            }
        }
    }
    let total_files = files.len();
    let multi = roots.len() > 1;
    let mut page = Vec::new();
    let (count, mut list): (usize, Vec<Value>) = if p.files_only {
        files = files.into_iter().skip(p.offset).take(p.limit).collect();
        (total_files, files.iter().map(|f| tag_repo_at(f, &names[f.repo], multi)).collect())
    } else {
        let end = (p.offset + p.limit).min(hits.len());
        page = search::finish(&roots, &scanned, &file_data, &hits[p.offset.min(end)..end], p.context);
        files.clear();
        (total, page.iter().map(|h| tag_repo_at(h, &names[h.repo], multi)).collect())
    };
    drop(hits);
    drop(file_data);
    drop(scanned);
    let shown = list.len();
    let next_offset = (p.offset + shown < count).then_some(p.offset + shown);
    let mut env = envelope(&repos, Value::Array(std::mem::take(&mut list)), count, shown);
    env["truncated"] = json!(next_offset.is_some());
    env["offset"] = json!(p.offset);
    env["next_offset"] = json!(next_offset);
    env["matches"] = json!(total);
    env["files"] = json!(total_files);
    env["by_kind"] = json!(by_kind);
    if scan.capped {
        env["capped"] = json!(true);
        let more = p.opts.max_lines.saturating_mul(4);
        env["hint"] = json!(format!(
            "the scan stopped at {} matching lines, after {} of {candidates} candidate files in order, so a match past it (a definition too) isn't ranked here: narrow the pattern, paths or lang, or raise the cap (max_lines={more}; CLI --max-lines {more})",
            p.opts.max_lines, scan.files_read
        ));
    }
    search_failed(&mut env, &roots, &scan.failed);
    env["stats"] = json!({
        "searchable_files": searchable,
        "candidates": candidates,
        "bytes_read": scan.bytes_read,
        "content_check_ms": round1(check_ms),
        "open_and_check_ms": round1(fresh_ms),
        "scan_and_rank_ms": round1(scan_ms),
        "read_ms": round1(read_ms),
        "ms": round1(t0.elapsed().as_secs_f64() * 1000.0),
    });
    let case = match p.opts.case {
        search::Case::Sensitive => "sensitive",
        search::Case::Insensitive => "insensitive",
        search::Case::Smart => "smart",
    };
    let req = cursor::Request::Search {
        pattern: p.opts.pattern.clone(),
        regex: p.opts.regex,
        case: case.to_string(),
        word: p.opts.word,
        langs: p.opts.langs.clone(),
        globs: p.opts.globs.clone(),
        max_lines: p.opts.max_lines,
        context: p.context,
        files_only: p.files_only,
        limit: p.limit,
    };
    cursor::offer(&mut env, req, args, vec![cursor::List { start: p.offset, ..cursor::List::new("/items", p.limit, next_offset.is_some()) }], &gens);
    let data = SearchData {
        roots,
        hits: page,
        files,
        total,
        total_files,
        by_kind,
        next_offset,
        multi,
        capped: scan.capped,
    };
    finish(&mut env);
    Ok(Out { env, data })
}

pub struct AstParams {
    pub opts: structural::AstOptions,
    pub limit: usize,
    pub offset: usize,
}

pub struct AstData {
    pub roots: Vec<PathBuf>,
    pub hits: Vec<structural::AstHit>,
    pub total: usize,
    pub files: usize,
    pub next_offset: Option<usize>,
    pub multi: bool,
    pub capped: bool,
}

/// Structural (ast-grep) search over one language's files: candidates from the trigram index, parsed and matched in
/// parallel. Matches in non-test code come first, then tests; each group by repo, path and position.
pub fn grep_ast(p: &AstParams, args: &Scope) -> Result<Out<AstData>> {
    let roots = target_repos(&args.repos)?;
    let matcher = structural::compile(&p.opts)?;
    let threads = SyncOptions::default().threads;
    let per_repo = (threads / roots.len().max(1)).max(1);
    let opened = pool_map(roots.iter().enumerate().collect(), threads, |(i, root)| -> Result<_> {
        let mut o = open_fresh(root, args.no_sync)?;
        let files = if args.no_sync { o.store.cfiles()? } else { content::sync_content_files(root, &mut o.store, per_repo)?.1 };
        // The files as the sync left them: the list it returns holds their state before.
        let content_gen = cursor::content_gen(root, &o.store.cfiles()?);
        let (files, stats) = structural::candidates(&o.store, files, &matcher, i)?;
        Ok((o, files, stats, content_gen))
    });
    let (mut repos, mut scan_files, mut searchable, mut candidates, mut gens) = (Vec::new(), Vec::new(), 0, 0, Vec::new());
    for (r, root) in opened.into_iter().zip(&roots) {
        let (o, mut files, stats, content_gen) = match r {
            Ok(r) => r,
            Err(e) => {
                left_out(&roots, root, e)?;
                continue;
            }
        };
        // Numbered by the repos answering: one left out is not among them.
        files.iter_mut().for_each(|f| f.repo = repos.len());
        gens.push(content_gen);
        searchable += stats.files;
        candidates += stats.candidates;
        scan_files.extend(files);
        repos.push(o);
    }
    let roots: Vec<PathBuf> = repos.iter().map(|o| o.root.clone()).collect();
    let (mut hits, scan) = structural::scan(&roots, &scan_files, &matcher, threads, p.opts.max_matches);
    hits.sort_by_key(|h| search::is_test_path(&h.file));
    let total = hits.len();
    let files = hits.iter().map(|h| (h.repo, h.file.as_str())).collect::<HashSet<_>>().len();
    let names: Vec<String> = roots.iter().map(|r| repo_name(r)).collect();
    let multi = roots.len() > 1;
    let page: Vec<structural::AstHit> = hits.into_iter().skip(p.offset).take(p.limit).collect();
    let list: Vec<Value> = page.iter().map(|h| tag_repo_at(h, &names[h.repo], multi)).collect();
    let shown = list.len();
    let next_offset = (p.offset + shown < total).then_some(p.offset + shown);
    let mut env = envelope(&repos, Value::Array(list), total, shown);
    env["truncated"] = json!(next_offset.is_some());
    env["offset"] = json!(p.offset);
    env["next_offset"] = json!(next_offset);
    env["lang"] = json!(matcher.lang);
    env["files"] = json!(files);
    if scan.capped {
        env["capped"] = json!(true);
        let more = p.opts.max_matches.saturating_mul(4);
        env["hint"] = json!(format!(
            "the scan stopped at {} matches, after {} of {candidates} candidate files in order: narrow the pattern or paths, or raise the cap (CLI --max-matches {more})",
            p.opts.max_matches, scan.files_parsed
        ));
    }
    search_failed(&mut env, &roots, &scan.failed);
    env["stats"] = json!({
        "searchable_files": searchable,
        "candidates": candidates,
        "files_parsed": scan.files_parsed,
        "bytes_read": scan.bytes_read,
    });
    let o = &p.opts;
    let req = cursor::Request::GrepAst {
        pattern: o.pattern.clone(),
        lang: o.lang.clone(),
        selector: o.selector.clone(),
        inside: o.inside.clone(),
        has: o.has.clone(),
        not_inside: o.not_inside.clone(),
        not_has: o.not_has.clone(),
        regex: o.regex.clone(),
        globs: o.globs.clone(),
        max_matches: o.max_matches,
        limit: p.limit,
    };
    cursor::offer(&mut env, req, args, vec![cursor::List { start: p.offset, ..cursor::List::new("/items", p.limit, next_offset.is_some()) }], &gens);
    finish(&mut env);
    Ok(Out { env, data: AstData { roots, hits: page, total, files, next_offset, multi, capped: scan.capped } })
}

/// Says in a search's envelope which files it could not read (repo index, path), whose matches are missing.
fn search_failed(env: &mut Value, roots: &[PathBuf], failed: &[(usize, String)]) {
    let mut by_repo: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    for (r, f) in failed {
        by_repo.entry(*r).or_default().push(f);
    }
    for (r, files) in by_repo {
        let repo = roots.get(r).map(|r| repo_name(r)).unwrap_or_default();
        push_incomplete(
            env,
            Incomplete::new(Reason::Failed, format!("{repo}: could not read {}; their matches are missing", files.join(", ")))
                .with_hint("fix the files' permissions, then search again"),
        );
    }
}

/// Maps `f` over `items` on up to `workers` threads, keeping order.
fn pool_map<T: Send, R: Send>(items: Vec<T>, workers: usize, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    let n = items.len();
    let queue = std::sync::Mutex::new(items.into_iter().enumerate());
    let results: std::sync::Mutex<Vec<Option<R>>> = std::sync::Mutex::new((0..n).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers.clamp(1, n.max(1)) {
            scope.spawn(|| {
                loop {
                    let Some((i, item)) = queue.lock().unwrap().next() else { break };
                    let r = f(item);
                    results.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    results.into_inner().unwrap().into_iter().map(|r| r.expect("every item mapped")).collect()
}

fn tag_repo_at<T: Serialize>(item: &T, name: &str, multi: bool) -> Value {
    let mut v = serde_json::to_value(item).unwrap_or(Value::Null);
    if multi {
        v["repo"] = json!(name);
    }
    v
}

/// Every file on disk the content index covers, with its state: what `search` reads (`indexed`, `pending`) and what it
/// leaves out (`skipped: binary | large | generated`).
pub fn content_files(args: &Scope) -> Result<Vec<(PathBuf, Vec<kc_store::content::CFile>)>> {
    let mut out = Vec::new();
    for root in target_repos(&args.repos)? {
        let mut o = open_fresh(&root, args.no_sync)?;
        let files = if args.no_sync {
            o.store.cfiles()?
        } else {
            content::sync_content_files(&root, &mut o.store, SyncOptions::default().threads)?.1
        };
        out.push((root, files));
    }
    Ok(out)
}

/// Areas listed in an overview at most; `knowcode areas` lists them all.
const OVERVIEW_AREAS: usize = 12;

/// An area as `knowcode areas` lists it; with `members`, every file in it.
fn area_json(a: &kc_index::areas::Area, members: bool) -> Value {
    let mut v = json!({
        "id": a.id,
        "name": a.name,
        "dir": a.dir,
        "files": a.files.len(),
        "lines": a.lines,
        "cohesion": a.cohesion,
        "tokens": a.tokens,
        "key_files": a.key_files,
        "entry_points": a.entry_points,
        "exposed": a.exposed,
        "depends_on": a.depends_on,
    });
    if members {
        v["members"] = json!(a.files);
    }
    v
}

/// An overview's areas: the largest, each with its three most central files and the areas it depends on most.
fn overview_areas(areas: &kc_index::areas::Areas) -> Value {
    json!(areas
        .areas
        .iter()
        .take(OVERVIEW_AREAS)
        .map(|a| json!({
            "id": a.id,
            "name": a.name,
            "files": a.files.len(),
            "cohesion": a.cohesion,
            "key_files": a.key_files.iter().take(3).collect::<Vec<_>>(),
            "key_files_total": a.key_files_total.unwrap_or(a.key_files.len()),
            "depends_on": a.depends_on.iter().take(3).map(|d| &d.name).collect::<Vec<_>>(),
            "depends_on_total": a.depends_on_total.unwrap_or(a.depends_on.len()),
        }))
        .collect::<Vec<_>>())
}

/// One repo's areas for `knowcode areas`, and the one `--area` picked.
pub struct AreasRepo {
    pub root: PathBuf,
    pub areas: kc_index::areas::Areas,
    /// The id of the area asked for.
    pub picked: Option<u32>,
}

/// A repo's functional areas (see `kc_index::areas`), or with `area` (an id, a name or a directory) that one with
/// every file in it.
pub fn areas(area: Option<&str>, args: &Scope) -> Result<Out<Vec<AreasRepo>>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let areas = kc_index::areas::areas(root, &o.store)?;
        let picked = area.and_then(|q| areas.find(q)).map(|a| a.id);
        found.push(AreasRepo { root: o.root.clone(), areas, picked });
        opened.push(o);
    }
    let multi = found.len() > 1;
    let mut items = Vec::new();
    for r in &found {
        let listed: Vec<Value> = match (area, r.picked) {
            (Some(_), Some(id)) => r.areas.areas.iter().filter(|a| a.id == id).map(|a| area_json(a, true)).collect(),
            (Some(_), None) => Vec::new(),
            (None, _) => r.areas.areas.iter().map(|a| area_json(a, false)).collect(),
        };
        if area.is_some() && listed.is_empty() && multi {
            continue;
        }
        items.push(json!({
            "repo": repo_name(&r.root),
            "files": r.areas.files,
            "modularity": r.areas.modularity,
            "cochange_pairs": r.areas.cochange_pairs,
            "areas_total": r.areas.areas.len(),
            "areas": listed,
            "ungrouped": r.areas.ungrouped,
        }));
    }
    let n = items.len();
    let mut env = envelope(&opened, json!(items), n, n);
    if let Some(q) = area
        && found.iter().all(|r| r.picked.is_none())
    {
        let names: Vec<String> = found.iter().flat_map(|r| r.areas.areas.iter().map(|a| format!("#{} {}", a.id, a.name))).collect();
        env["note"] = json!(format!("no area named {q:?} (by id, name or directory); areas: {}", names.join(", ")));
    }
    finish(&mut env);
    Ok(Out { env, data: found })
}

/// What `note add`, `note list` and `note rm` answer: per repo, the notes.
pub struct NotesRepo {
    pub root: PathBuf,
    pub notes: Vec<kc_index::notes::NoteView>,
}

fn notes_env(opened: &[Opened], found: &[NotesRepo]) -> Value {
    let items: Vec<Value> = found
        .iter()
        .flat_map(|r| {
            r.notes.iter().map(|n| {
                let mut v = json!(n);
                v["repo"] = json!(repo_name(&r.root));
                v
            })
        })
        .collect();
    let n = items.len();
    envelope(opened, json!(items), n, n)
}

/// Attaches a note to the symbol `symbol` names, in the first selected repo defining it. Writes `notes.sqlite` in
/// the user data directory (see `kc_index::config::notes_path`).
pub fn note_add(symbol: &str, text: &str, tag: Option<&str>, args: &Scope) -> Result<Out<Vec<NotesRepo>>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        if found.is_empty() && !query::definitions(o.store.conn(), symbol)?.is_empty() {
            let view = kc_index::notes::add(root, &o.store, symbol, text, tag)?;
            found.push(NotesRepo { root: o.root.clone(), notes: vec![view] });
        }
        opened.push(o);
    }
    if found.is_empty() {
        bail!("no definition named {symbol} in the repos searched");
    }
    Ok(Out { env: notes_env(&opened, &found), data: found })
}

/// The notes on what `symbol` names, or every note in the selected repos, each matched to the code as it is now:
/// `current`, `stale` or `orphaned`.
pub fn note_list(symbol: Option<&str>, args: &Scope) -> Result<Out<Vec<NotesRepo>>> {
    let roots = target_repos(&args.repos)?;
    let mut opened = Vec::new();
    let mut found = Vec::new();
    for root in &roots {
        let Some(o) = open_member(&roots, root, args.no_sync)? else { continue };
        let notes = match symbol {
            Some(s) => kc_index::notes::named(root, &o.store, s)?,
            None => kc_index::notes::all(root, &o.store)?,
        };
        if !notes.is_empty() {
            found.push(NotesRepo { root: o.root.clone(), notes });
        }
        opened.push(o);
    }
    let mut env = notes_env(&opened, &found);
    if found.is_empty() {
        env["note"] = json!(match symbol {
            Some(s) => format!("no note on {s}"),
            None => "no notes in the repos searched".to_string(),
        });
    }
    finish(&mut env);
    Ok(Out { env, data: found })
}

/// Removes note `id` from the selected repos. Returns the repos it was removed from.
pub fn note_rm(id: i64, args: &Scope) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for root in target_repos(&args.repos)? {
        if kc_index::notes::remove(&root, id)? {
            removed.push(root);
        }
    }
    Ok(removed)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn every_envelope_says_which_knowcode_answered() {
        let env = envelope(&[], json!([]), 0, 0);
        let k = &env["knowcode"];
        assert_eq!(k["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(k["schema"], kc_store::SCHEMA_VERSION);
        // A git commit (with `-dirty` for uncommitted changes), or `unknown` outside a checkout.
        let build = k["build"].as_str().unwrap();
        assert!(build == "unknown" || build.trim_end_matches("-dirty").chars().all(|c| c.is_ascii_hexdigit()), "{build}");
    }

    /// The cache every test here that indexes uses, held for the whole test (see [`test_cache`]).
    pub(crate) struct TestCache {
        _turn: std::sync::MutexGuard<'static, ()>,
        base: &'static Path,
    }

    /// A test's directory under [`TestCache`]: it borrows the cache, so the test can't let go of its turn while it
    /// still uses the directory (`let base = test_cache().dir("x");` doesn't compile once `base` is used).
    pub(crate) struct Base<'a> {
        dir: PathBuf,
        _cache: std::marker::PhantomData<&'a TestCache>,
    }

    impl std::ops::Deref for Base<'_> {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.dir
        }
    }

    impl AsRef<Path> for Base<'_> {
        fn as_ref(&self) -> &Path {
            &self.dir
        }
    }

    impl TestCache {
        /// `<base>/<name>`, where the test makes its repos.
        pub(crate) fn dir(&self, name: &str) -> Base<'_> {
            Base { dir: self.base.join(name), _cache: std::marker::PhantomData }
        }

        /// The base directory, holding `cache` and `config`.
        pub(crate) fn base(&self) -> &Path {
            self.base
        }
    }

    /// The cache directory and environment are process-wide, so the tests that index take turns, and each starts from
    /// an empty cache and config. Without both, one test's repos would join another's answers: with no workspace
    /// roots, the workspace is every indexed repo, so a repo another test is indexing meanwhile, or indexed and
    /// deleted while its index stays in the cache, is a candidate for HTTP, bus, package and SQL links.
    pub(crate) fn test_cache() -> TestCache {
        static BASE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let base = BASE.get_or_init(|| {
            let base = std::env::temp_dir().join(format!("knowcode-test-ops-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            // SAFETY: set once, before any test in this binary reads them. The config directory holds only the tests'
            // conventions, so the user's own settings (workspace roots, ignore rules) don't reach the tests.
            unsafe {
                std::env::set_var("KNOWCODE_CACHE_DIR", base.join("cache"));
                std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config"));
                std::env::set_var("KNOWCODE_DATA_DIR", base.join("data"));
            }
            base
        });
        // A test that failed holding its turn poisons the lock; the next one starts afresh all the same.
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        for dir in ["cache", "config", "data"] {
            let _ = std::fs::remove_dir_all(base.join(dir));
        }
        // The fixtures are written in a made-up code base's conventions, which the config names.
        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config/config.toml"), kc_index::config::SYNTHETIC_CONVENTIONS).unwrap();
        TestCache { _turn: turn, base }
    }

    /// A git repo at `base/name` holding `files`, indexed.
    pub(crate) fn repo(base: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        assert!(std::process::Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
        for (path, text) in files {
            std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.join(path), text).unwrap();
        }
        let root = kc_index::repo_root(&dir).unwrap();
        kc_index::sync(&root, &mut kc_index::open(&root).unwrap(), &SyncOptions { threads: 1, ..Default::default() }).unwrap();
        root
    }

    /// Commits everything in the repo at `dir` and tags the commit `tag`.
    pub(crate) fn commit_and_tag(dir: &Path, tag: &str) {
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success());
        };
        git(&["add", "-A"]);
        git(&["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&["tag", tag]);
    }

    /// Two queries answered at once on two threads, as two MCP calls in the two lanes are: each envelope names only
    /// the repos its own query left out and the gone checkouts its own links needed, whichever thread finishes first.
    #[test]
    fn invariant_concurrent_queries_never_see_each_others_incomplete_entries() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let query = |name: &'static str| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let _q = kc_store::limits::enter(kc_store::limits::Operation::new());
                let roots = [PathBuf::from(format!("/nowhere/{name}-a")), PathBuf::from(format!("/nowhere/{name}-b"))];
                left_out(&roots, &roots[1], anyhow::anyhow!("unreadable")).unwrap();
                kc_index::workspace::note_missing(Path::new(&format!("/nowhere/{name}-gone")));
                // Both have collected; neither has answered.
                barrier.wait();
                let mut env = json!({ "items": [] });
                if name == "second" {
                    // The other answers first.
                    barrier.wait();
                }
                add_cuts(&mut env);
                if name == "first" {
                    barrier.wait();
                }
                crate::contract::incomplete_of(&env).into_iter().map(|i| i.detail).collect::<Vec<_>>()
            })
        };
        let (first, second) = (query("first"), query("second"));
        for (name, details) in [("first", first.join().unwrap()), ("second", second.join().unwrap())] {
            assert_eq!(details.len(), 2, "{details:?}");
            assert!(details[0].starts_with(&format!("{name}-b: its checkout at /nowhere/{name}-b is gone")), "{details:?}");
            assert!(details[1].starts_with(&format!("{name}-gone: indexed, but its checkout at /nowhere/{name}-gone")), "{details:?}");
        }
    }

    fn impact_of(root: &Path, symbol: &str) -> ImpactRepo {
        let p = ImpactParams { symbol: Some(symbol.to_string()), file: None, diff: None, depth: 3, limit: 100, ..Default::default() };
        let mut out = impact(&p, &Scope { repos: vec![root.to_path_buf()], no_sync: false }).unwrap();
        out.data.remove(0)
    }

    /// `(hop, repo, qualified, edge, confidence, route)` of an affected symbol.
    type Row = (u8, Option<String>, String, &'static str, &'static str, Option<String>);

    fn rows(i: &Impact) -> Vec<Row> {
        i.affected.iter().map(|a| (a.hop, a.repo.clone(), a.qualified.clone(), a.edge, a.confidence, a.http.as_ref().map(|h| h.route()))).collect()
    }

    #[test]
    fn status_says_where_the_config_and_cache_are_and_targets_join_the_workspace_focus() {
        let cache = test_cache();
        let base = cache.dir("status-config");
        let root = repo(&base, "app", &[("src/a.py", "def f():\n    pass\n")]);
        let out = status(&Scope { repos: vec![root.clone()], no_sync: false }).unwrap();
        let c = &out.env["config"];
        assert_eq!(c["file"], json!(cache.base().join("config").join("config.toml")));
        assert_eq!(c["exists"], json!(true), "the tests' config names their conventions");
        assert_eq!(c["cache_dir"], json!({ "path": cache.base().join("cache"), "source": "env" }));
        assert_eq!(c["notes"]["path"], json!(cache.base().join("data").join("notes.sqlite")));
        assert_eq!(c["notes"]["source"], json!("env"));
        assert!(c["notes"]["count"].is_u64());
        assert_eq!(c["workspace_roots"], json!([]));
        assert_eq!(c["ignore_rules"], json!({ "count": 0, "source": "default" }));
        // No roots configured: the workspace is every indexed repo, the target among them.
        assert!(kc_index::indexed_roots().unwrap().contains(&root));
        assert!(kc_index::workspace::summaries(&[]).unwrap().iter().any(|s| s.root == root));
    }

    #[test]
    fn impact_of_a_name_both_an_entity_and_a_dto_have_is_the_entitys() {
        let cache = test_cache();
        let base = cache.dir("entity-names");
        let root = repo(
            &base,
            "shop",
            &[
                ("src/Api/Models/Voucher.cs", "namespace Shop.Api.Models;\npublic class Voucher { public int Id { get; set; } }\n"),
                ("src/Data/Entities/Voucher.cs", "namespace Shop.Data.Entities;\npublic class Voucher { public int Id { get; set; } }\n"),
                (
                    "src/Data/ShopContext.cs",
                    "using Microsoft.EntityFrameworkCore;\nusing Shop.Data.Entities;\nnamespace Shop.Data;\n\
                     public class ShopContext : DbContext { public DbSet<Voucher> Vouchers { get; set; } }\n",
                ),
            ],
        );
        let r = impact_of(&root, "Voucher");
        assert_eq!(r.impact.scope[0].file, "src/Data/Entities/Voucher.cs");
        assert_eq!(r.others, ["src/Api/Models/Voucher.cs#Voucher"]);
    }

    #[test]
    fn impact_and_trace_cross_from_an_action_to_the_frontend_code_requesting_it() {
        let cache = test_cache();
        let base = cache.dir("http");
        let web = repo(
            &base,
            "web",
            &[
                ("src/Api.ts", "export class Api {\n  static readonly filters = 'Filters';\n  static readonly search = 'Search';\n}\n"),
                (
                    "src/contracts.ts",
                    "@RequestContract({ endpoint: Api.filters, httpMethod: HttpMethods.post })\nexport class ApiGetFilters {}\n\
                     @RequestContract({ endpoint: Api.search, httpMethod: HttpMethods.post })\nexport class ApiSearch {}\n",
                ),
                (
                    "src/filter.service.ts",
                    "export class FilterService {\n  constructor(private http: HttpService) {}\n  load(): void {\n    const c: ApiGetFilters = null;\n    this.http.send(new ApiGetFilters());\n  }\n  search(): void {\n    this.http.send(new ApiSearch());\n  }\n}\n",
                ),
                ("src/filter.page.ts", "export class FilterPage {\n  constructor(private filters: FilterService) {}\n  refresh(): void {\n    this.filters.load();\n  }\n  find(): void {\n    this.filters.search();\n  }\n}\n"),
            ],
        );
        let api = repo(
            &base,
            "api",
            &[
                (
                    "src/SearchController.cs",
                    "public class SearchController : Controller\n{\n    private readonly FilterQuery _q;\n    [HttpPost(nameof(Filters))]\n    public Task Filters() => _q.Run();\n\n    [HttpPost(\"Search\")]\n    public Task Search() => null;\n}\n",
                ),
                ("src/FilterQuery.cs", "public class FilterQuery\n{\n    public Task Run() => null;\n}\n"),
            ],
        );
        // Serves `Search` too, but the frontend's backend is `api`: its link is ambiguous.
        let other = repo(&base, "other", &[("src/OtherController.cs", "public class OtherController : Controller\n{\n    [HttpPost(\"search\")]\n    public Task Search() => null;\n}\n")]);

        let r = impact_of(&api, "FilterQuery.Run");
        let web_name = Some("web".to_string());
        let route = Some("POST /Filters".to_string());
        assert_eq!(
            rows(&r.impact),
            [
                (1, None, "SearchController.Filters".to_string(), "calls", r.impact.affected[0].confidence, None),
                (2, web_name.clone(), "FilterService.load".to_string(), "http", "unique_name", route.clone()),
                (3, web_name.clone(), "FilterPage.refresh".to_string(), "calls", r.impact.affected[2].confidence, None),
            ]
        );
        assert_eq!(r.impact.affected[1].http.as_ref().unwrap().contract, "ApiGetFilters");
        assert_eq!(r.impact.affected[1].via, "SearchController.Filters");
        assert_eq!((r.impact.nearest, r.impact.counts.get("far")), ("direct", Some(&1)));
        // Depth counts the HTTP hop like any other.
        let shallow = impact(&ImpactParams { symbol: Some("FilterQuery.Run".into()), file: None, diff: None, depth: 2, limit: 100, ..Default::default() }, &Scope { repos: vec![api.clone()], no_sync: false }).unwrap();
        assert_eq!(shallow.data[0].impact.affected.len(), 2);

        // An ambiguous link is listed, low-confidence, and not followed further.
        let r = impact_of(&other, "OtherController.Search");
        assert_eq!(rows(&r.impact), [(1, web_name.clone(), "FilterService.search".to_string(), "http", "ambiguous", Some("POST /search".to_string()))]);
        assert_eq!(r.impact.low_confidence, 1);

        // Trace inward crosses too, and keeps tracing in the frontend.
        let t = trace("SearchController.Filters", "in", 3, 100, &WalkFilter::default(), &Scope { repos: vec![api.clone()], no_sync: false }).unwrap();
        let nodes: Vec<_> = t.data.found[0].1.nodes.iter().map(|n| (n.depth, n.repo.clone(), n.qualified.clone(), n.edge)).collect();
        assert_eq!(
            nodes,
            [
                (0, None, "SearchController.Filters".to_string(), ""),
                (1, web_name.clone(), "FilterService.load".to_string(), "http"),
                (2, web_name.clone(), "FilterPage.refresh".to_string(), "calls"),
            ]
        );
        assert_eq!(t.env["items"][0]["nodes"][1]["http"]["verb"], "POST");
        // Outward, nothing crosses.
        let t = trace("SearchController.Filters", "out", 3, 100, &WalkFilter::default(), &Scope { repos: vec![api.clone()], no_sync: false }).unwrap();
        assert!(t.data.found[0].1.nodes.iter().all(|n| n.repo.is_none()));
        let _ = web;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A frontend requesting `POST /Orders` and `POST /Items`, and the API serving both.
    fn orders_web_and_api(base: &Path) -> (PathBuf, PathBuf) {
        let web = repo(
            base,
            "web",
            &[
                ("src/Api.ts", "export class Api {\n  static readonly orders = 'Orders';\n  static readonly items = 'Items';\n}\n"),
                (
                    "src/contracts.ts",
                    "@RequestContract({ endpoint: Api.orders, httpMethod: HttpMethods.post })\nexport class ApiPlaceOrder {}\n\
                     @RequestContract({ endpoint: Api.items, httpMethod: HttpMethods.post })\nexport class ApiListItems {}\n",
                ),
                (
                    "src/order.service.ts",
                    "export class OrderService {\n  constructor(private http: HttpService) {}\n  load(): void {\n    this.http.send(new ApiPlaceOrder());\n  }\n  items(): void {\n    this.http.send(new ApiListItems());\n  }\n}\n",
                ),
            ],
        );
        let api = repo(
            base,
            "api",
            &[
                (
                    "src/OrdersController.cs",
                    "public class OrdersController : Controller\n{\n    private readonly OrderQuery _q;\n    [HttpPost(nameof(Orders))]\n    public Task Orders() => _q.Run();\n\n    [HttpPost(\"Items\")]\n    public Task Items() => null;\n}\n",
                ),
                ("src/OrderQuery.cs", "public class OrderQuery\n{\n    public Task Run() => null;\n}\n"),
            ],
        );
        (web, api)
    }

    /// A repo serving every route `web` requests, as `api` does: while its checkout is there, `web`'s links are
    /// ambiguous between the two.
    fn stale_copy(base: &Path) -> PathBuf {
        repo(
            base,
            "extra",
            &[(
                "src/StaleController.cs",
                "public class StaleController : Controller\n{\n    [HttpPost(\"Orders\")]\n    public Task Orders() => null;\n\n    [HttpPost(\"Items\")]\n    public Task Items() => null;\n}\n",
            )],
        )
    }

    /// The `missing_repo` entries of an answer, as `reason: detail`.
    fn missing_notes(env: &Value) -> Vec<String> {
        crate::contract::incomplete_of(env).iter().filter(|i| i.reason == Reason::MissingRepo).map(|i| i.detail.clone()).collect()
    }

    #[test]
    fn a_deleted_checkout_never_makes_an_http_link_ambiguous_and_is_named_missing() {
        let cache = test_cache();
        let base = cache.dir("http-gone");
        let (web, api) = orders_web_and_api(&base);
        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };
        // `web`'s link to the action serving `POST /Orders`, from `web`'s side and from the action's.
        let links = || {
            let c = context("OrderService.load", 4000, Default::default(), &scope(&web)).unwrap();
            let http = &c.env["items"][0]["http"][0];
            let ctx: Vec<(String, String)> = http["links"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| (l["repo"].as_str().unwrap().to_string(), l["confidence"].as_str().unwrap().to_string()))
                .collect();
            let r = impact_of(&api, "OrderQuery.Run");
            let load: Vec<&'static str> = r.impact.affected.iter().filter(|a| a.qualified == "OrderService.load").map(|a| a.confidence).collect();
            (ctx, load, c.env)
        };
        let (ctx, load, env) = links();
        assert_eq!(ctx, [("api".to_string(), "exact".to_string())], "{env}");
        assert_eq!(load, ["unique_name"]);
        assert!(missing_notes(&env).is_empty(), "{env}");

        // Another repo serving the same routes: while it is there, which one serves `web` can't be told.
        let extra = stale_copy(&base);
        let (ctx, load, _) = links();
        assert_eq!(ctx.iter().map(|(r, c)| (r.as_str(), c.as_str())).collect::<Vec<_>>(), [("api", "ambiguous"), ("extra", "ambiguous")]);
        assert_eq!(load, ["ambiguous"]);

        // Its checkout deleted, its index left: the links are as before it came, and the answer says it is missing.
        std::fs::remove_dir_all(&extra).unwrap();
        let (ctx, load, env) = links();
        assert_eq!(ctx, [("api".to_string(), "exact".to_string())], "{env}");
        assert_eq!(load, ["unique_name"]);
        let notes = missing_notes(&env);
        assert!(notes.len() == 1 && notes[0].starts_with("extra: ") && notes[0].contains("is gone"), "{env}");
        assert_eq!(env["complete"], json!(false));
        // The impact answer from the action's side names it too.
        let p = ImpactParams { symbol: Some("OrderQuery.Run".into()), depth: 3, limit: 100, ..Default::default() };
        let env = impact(&p, &scope(&api)).unwrap().env;
        assert!(missing_notes(&env).iter().any(|n| n.starts_with("extra: ")), "{env}");
        // A workspace-wide answer has nothing there to read: the repo is no longer one of the workspace's.
        let all = kc_index::indexed_roots().unwrap();
        assert!(all.contains(&api) && !all.contains(&extra), "{all:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// An answer without what says how complete it is and what it read (`complete`, `incomplete`, `provenance`), nor
    /// what moves between two asks of one question (build, freshness, timings, generations, cursors).
    fn answer_only(v: &Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(
                m.iter()
                    .filter(|(k, _)| {
                        !matches!(k.as_str(), "complete" | "incomplete" | "provenance" | "knowcode" | "freshness" | "generation" | "elapsed" | "ms" | "_resume" | "cursor")
                            && !k.ends_with("_ms")
                    })
                    .map(|(k, v)| (k.clone(), answer_only(v)))
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(answer_only).collect()),
            v => v.clone(),
        }
    }

    const MODELS_PACKAGE: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Shop.Orders.Models</PackageId></PropertyGroup>\n</Project>\n";

    /// Deleting a repo's checkout (its index left behind) never changes what another repo's answers link to, nor how
    /// sure each link is, across every cross-repo linker: HTTP routes, the message bus, SQL tables, package owners,
    /// and the impact, trace and routes answers crossing them. It only adds a `missing_repo` note, to the answers the
    /// repo would have joined, and to no other.
    #[test]
    fn invariant_deleting_a_checkout_never_changes_another_repos_links_only_adds_a_missing_note() {
        let cache = test_cache();
        let base = cache.dir("inv-gone");
        let (web, api) = orders_web_and_api(&base);
        // The API publishes an event and stores orders through Entity Framework; a worker consumes the event; a schema
        // repo defines the table.
        std::fs::write(
            api.join("src/OrderEvents.cs"),
            "namespace Shop.Orders.Events;\n[ShopMessage(ShopHosts.Orders)]\npublic class OrderPlaced { }\n",
        )
        .unwrap();
        std::fs::write(
            api.join("src/OrderStore.cs"),
            "using Shop.Orders.Events;\nnamespace Shop.Orders.Service;\npublic class Order { }\n\
public class ShopContext : DbContext\n{\n    public DbSet<Order> Orders { get; set; }\n}\n\
public class OrderStore\n{\n    private readonly ShopContext _db;\n    private readonly IShopBus _bus;\n\
    public Order Find(int id) => _db.Orders.Find(id);\n\
    public async Task Place()\n    {\n        await _bus.Publish(new OrderPlaced());\n    }\n}\n",
        )
        .unwrap();
        // The event ships in the API's package, which the worker depends on.
        std::fs::write(api.join("src/Shop.Orders.Models.csproj"), MODELS_PACKAGE).unwrap();
        let worker = repo(
            &base,
            "worker",
            &[
                ("src/Worker.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Shop.Orders.Models\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                (
                    "src/OrderPlacedConsumer.cs",
                    "using Shop.Orders.Events;\nnamespace Shop.Worker;\npublic class OrderPlacedConsumer : IConsumer<OrderPlaced>\n{\n    public Task Consume(ConsumeContext<OrderPlaced> context) => null;\n}\n",
                ),
            ],
        );
        let schema = repo(&base, "schema", &[("src/shop.sql", "-- Host: db1    Database: shop\nCREATE TABLE `orders` (`id` int);\n")]);
        // A repo nothing links to, deleted: no answer is about it, so none says it is missing.
        let lonely = repo(&base, "lonely", &[("src/ElsewhereController.cs", "public class ElsewhereController : Controller\n{\n    [HttpGet(\"Elsewhere\")]\n    public Task Get() => null;\n}\n")]);
        std::fs::remove_dir_all(&lonely).unwrap();

        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };
        let questions = || -> Vec<(&'static str, Value)> {
            let ctx = |s: &str, r: &Path| context(s, 4000, Default::default(), &scope(r)).unwrap().env;
            let imp = |s: &str, r: &Path| impact(&ImpactParams { symbol: Some(s.into()), depth: 3, limit: 100, ..Default::default() }, &scope(r)).unwrap().env;
            let tr = |s: &str, dir: &str, r: &Path| trace(s, dir, 3, 100, &WalkFilter::default(), &scope(r)).unwrap().env;
            vec![
                ("context load", ctx("OrderService.load", &web)),
                ("context items", ctx("OrderService.items", &web)),
                ("trace load out", tr("OrderService.load", "out", &web)),
                ("routes web", routes(&scope(&web)).unwrap().env),
                ("impact action", imp("OrderQuery.Run", &api)),
                ("impact publisher", imp("OrderStore.Place", &api)),
                ("context consumer", ctx("OrderPlacedConsumer.Consume", &worker)),
                ("trace consumer in", tr("OrderPlacedConsumer.Consume", "in", &worker)),
                ("tables orders", tables(Some("orders"), &scope(&api)).unwrap().env),
                ("impact table", imp("shop.orders", &schema)),
                ("context event", ctx("OrderPlaced", &api)),
                ("impact event", imp("OrderPlaced", &api)),
            ]
        };
        let before = questions();
        for (q, env) in &before {
            assert!(missing_notes(env).is_empty(), "{q}: a deleted repo nothing links to is named missing: {env}");
        }

        // A repo duplicating every link target: the same routes, a consumer of the event declaring it too, the table
        // in another database. While it is there, some links are less sure.
        let extra = repo(
            &base,
            "extra",
            &[
                (
                    "src/StaleController.cs",
                    "public class StaleController : Controller\n{\n    [HttpPost(\"Orders\")]\n    public Task Orders() => null;\n\n    [HttpPost(\"Items\")]\n    public Task Items() => null;\n}\n",
                ),
                ("src/OrderEvents.cs", "namespace Shop.Orders.Events;\n[ShopMessage(ShopHosts.Orders)]\npublic class OrderPlaced { }\n"),
                ("src/Shop.Orders.Models.csproj", MODELS_PACKAGE),
                (
                    "src/StaleConsumer.cs",
                    "using Shop.Orders.Events;\nnamespace Shop.Stale;\npublic class StaleConsumer : IConsumer<OrderPlaced>\n{\n    public Task Consume(ConsumeContext<OrderPlaced> context) => null;\n}\n",
                ),
                ("src/archive.sql", "-- Host: db1    Database: archive\nCREATE TABLE `orders` (`id` int);\n"),
            ],
        );
        let present = questions();
        let changed: Vec<&str> = before.iter().zip(&present).filter(|(a, b)| answer_only(&a.1) != answer_only(&b.1)).map(|(a, _)| a.0).collect();
        assert!(["context load", "impact publisher", "tables orders", "impact event"].iter().all(|q| changed.contains(q)), "{changed:?}");

        // Its checkout deleted, its index left: every answer is as before it came, plus a note where it mattered.
        std::fs::remove_dir_all(&extra).unwrap();
        let after = questions();
        for ((q, a), (_, b)) in before.iter().zip(&after) {
            assert_eq!(answer_only(a), answer_only(b), "{q}: deleting a checkout changed another repo's answer");
            let mut added = crate::contract::incomplete_of(b);
            added.retain(|i| !crate::contract::incomplete_of(a).contains(i));
            assert!(added.iter().all(|i| i.reason == Reason::MissingRepo && i.detail.starts_with("extra: ")), "{q}: {added:?}");
        }
        for (q, env) in after.iter().filter(|(q, _)| changed.contains(q)) {
            assert!(missing_notes(env).iter().any(|n| n.starts_with("extra: ")), "{q}: the deleted repo mattered but isn't named: {env}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Commits everything in `dir` with a fixed author.
    fn commit(dir: &Path, msg: &str) {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@x", "-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} in {}: {}\n{}{}",
                dir.display(),
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", msg]);
    }

    fn append(dir: &Path, file: &str, text: &str) {
        let path = dir.join(file);
        let old = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, old + text).unwrap();
    }

    /// Commits everything in `dir` as `who` (`who@x`).
    fn commit_as(dir: &Path, who: &str, msg: &str) {
        let email = format!("user.email={who}@x");
        let name = format!("user.name={who}");
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", msg]] {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", name.as_str(), "-c", email.as_str(), "-c", "commit.gpgsign=false"])
                .args(&args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
    }

    #[test]
    fn health_biomarkers_ownership_and_reviewers() {
        let cache = test_cache();
        let base = cache.dir("health");
        let app = repo(
            &base,
            "app",
            &[(
                "src/Orders.cs",
                "public class Orders\n{\n    private int _a;\n    private int _b;\n    public void Ship(int[] xs)\n    {\n        if (_a > 0)\n        {\n            if (_b > 0) { }\n        }\n        foreach (var x in xs)\n        {\n            if (x > 0 && x < 9 || x == 42) { }\n        }\n    }\n    public int A() => _a;\n    public int C() => 3;\n    public int D() => 4;\n}\n",
            )],
        );
        commit_as(&app, "ann", "add orders");
        append(&app, "src/Orders.cs", "// tweak\n");
        commit_as(&app, "bob", "fix: tweak");
        let scope = Scope { repos: vec![app.clone()], no_sync: false };

        // The file's biomarkers, each with its reason.
        let h = hotspots(Some("src/Orders.cs"), 10, None, &scope).unwrap();
        let kinds: Vec<(&str, &str)> = h.data.findings.iter().map(|f| (f.kind.as_str(), f.reason.as_str())).collect();
        assert_eq!(
            kinds,
            [
                ("low_cohesion", "Orders splits into 3 groups of methods sharing no field or call (LCOM4): {Ship, A} {C} {D}"),
                ("bumpy_road", "Orders.Ship has 2 bumps (separate blocks nesting 2+ deep), lines 7–10, 11–14"),
                ("complex_conditional", "Orders.Ship has a condition with 2 && / || operators, line 13"),
            ]
        );
        assert_eq!(h.env["items"][0]["biomarkers"]["bumpy_road"], 1);
        assert_eq!(h.env["items"][0]["fix_commits_365d"], 1);
        assert!(h.env["items"][0]["fix_score"].as_f64().unwrap() > 0.49);
        assert_eq!(h.env["history"]["dormant_months"], Value::Null);
        // Windows anchored a year and a half later: no recent churn, and the repo reads as dormant.
        let later = vitals::format_date(vitals::anchor(&app, Some("head")).unwrap().time + 545 * 86_400);
        let h = hotspots(None, 10, Some(&later), &scope).unwrap();
        assert_eq!(h.env["items"][0]["churn_365d"], 0);
        assert_eq!(h.env["history"]["dormant_months"], 18);

        // `context` on the method carries its own findings, not the class's.
        let c = context("Orders.Ship", 2000, Default::default(), &scope).unwrap();
        let kinds: Vec<&str> = c.data.health[0].iter().map(|f| f.kind.as_str()).collect();
        assert_eq!(kinds, ["bumpy_road", "complex_conditional"]);

        // Ownership: ann wrote the file; bob added a line. Without ann, most lines added are by an author gone.
        let p = OwnershipParams { path: Some("src/Orders.cs".into()), without: vec!["ann@x".into()], depth: 2, top: 10, as_of: None };
        let o = ownership(&p, &scope).unwrap();
        let t = &o.data[0].target;
        assert_eq!((t.top_contributor.as_ref().unwrap().email.as_str(), t.truck_factor), ("ann@x", 1));
        let off = o.data[0].offboarding.as_ref().unwrap();
        assert_eq!((off.top_contributor_files, off.gone_before), (1, 0.0));
        assert!(off.gone_after > 0.5 && off.gone_after < 1.0, "bob's line stays: {}", off.gone_after);
        // Reviewers: ann first by lines written; a former author would be left out.
        let r = reviewers(&ReviewersParams { files: vec!["src/Orders.cs".into()], diff: None, limit: 5, as_of: None }, &scope).unwrap();
        let names: Vec<&str> = r.data[0].reviewers.iter().map(|r| r.email.as_str()).collect();
        assert_eq!(names, ["ann@x", "bob@x"]);

        // A committed change by bob, reviewed by revision: bob made it, so he is left out, and ownership is read from
        // the history before it; the changelog doesn't count.
        append(&app, "src/Orders.cs", "// more\n");
        std::fs::write(app.join("CHANGELOG.md"), "- more\n").unwrap();
        commit_as(&app, "bob", "more");
        let r = reviewers(&ReviewersParams { files: vec![], diff: Some("HEAD~1".into()), limit: 5, as_of: None }, &scope).unwrap();
        let r = &r.data[0];
        assert_eq!((r.files.as_slice(), r.left_out.as_slice()), (&["src/Orders.cs".to_string()][..], &["CHANGELOG.md".to_string()][..]));
        assert!(r.excluded.contains(&"bob@x".to_string()));
        assert_eq!(r.reviewers.iter().map(|r| r.email.as_str()).collect::<Vec<_>>(), ["ann@x"]);

        // A `.mailmap` merges identities: with bob mapped onto ann, ann owns every line.
        std::fs::write(app.join(".mailmap"), "ann <ann@x> <bob@x>\n").unwrap();
        commit_as(&app, "ann", "mailmap");
        let p = OwnershipParams { path: Some("src/Orders.cs".into()), without: vec![], depth: 2, top: 10, as_of: None };
        let o = ownership(&p, &scope).unwrap();
        let t = &o.data[0].target;
        assert_eq!((t.contributors.len(), t.top_contributor.as_ref().unwrap().share), (1, 1.0));
        // The file as it is (blame), apart from its history: every current line is ann's, after the mailmap too.
        let (authors, uncommitted, _) = o.data[0].surviving.as_ref().expect("a file gets its surviving lines");
        let got: Vec<(&str, u64)> = authors.iter().map(|s| (s.email.as_str(), s.lines)).collect();
        assert_eq!((got, *uncommitted), (vec![("ann@x", t.lines)], 0));
        // An uncommitted line is nobody's yet.
        append(&app, "src/Orders.cs", "// draft\n");
        let o = ownership(&p, &scope).unwrap();
        assert_eq!(o.data[0].surviving.as_ref().unwrap().1, 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn coupling_and_test_selection_from_history_and_edges() {
        let cache = test_cache();
        let base = cache.dir("coupling");
        std::fs::create_dir_all(base.join("app/tests/App.Tests")).unwrap();
        let app = repo(
            &base,
            "app",
            &[
                ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/Pricing.cs", "public class Pricing\n{\n    public int Quote() => 1;\n}\n"),
                ("src/Checkout.cs", "public class Checkout\n{\n    private readonly Pricing _p;\n    public int Total() => _p.Quote();\n}\n"),
                ("src/Discounts.cs", "public class Discounts\n{\n    public int Rate() => 5;\n}\n"),
                ("tests/App.Tests/PricingTests.cs", "public class PricingTests\n{\n    private readonly Pricing _p;\n    public void Quotes() => _p.Quote();\n}\n"),
                ("tests/App.Tests/DiscountsTests.cs", "public class DiscountsTests\n{\n    public void Rates() { }\n}\n"),
                // A base class in the test folder, with no tests in it.
                ("tests/App.Tests/TestBase.cs", "public abstract class TestBase\n{\n    protected Pricing P() => new Pricing();\n}\n"),
                ("tests/App.Tests/QuoteTests.cs", "public class QuoteTests : TestBase\n{\n    [Fact]\n    public void Quotes() => P().Quote();\n}\n"),
            ],
        );
        // Created together, then Pricing and Discounts change together three more times; Shipping is added with a
        // change to Pricing once. Checkout and the tests were only ever created with Pricing: not coupled.
        commit(&app, "add the shop");
        for i in 0..3 {
            append(&app, "src/Pricing.cs", &format!("// rule {i}\n"));
            append(&app, "src/Discounts.cs", &format!("// rule {i}\n"));
            commit(&app, &format!("pricing rule {i}"));
        }
        append(&app, "src/Pricing.cs", "// tax\n");
        std::fs::write(app.join("src/Shipping.cs"), "public class Shipping\n{\n    public int Cost() => 3;\n}\n").unwrap();
        commit(&app, "tax");
        let scope = Scope { repos: vec![app.clone()], no_sync: false };

        let c = coupled("src/Pricing.cs", 10, &vitals::Span::default(), &scope).unwrap();
        let partners: Vec<(&str, u32, f64, bool)> =
            c.data[0].partners.iter().map(|p| (p.coupled.file.as_str(), p.coupled.support, p.coupled.confidence, p.static_link)).collect();
        assert_eq!(partners, [("src/Discounts.cs", 4, 0.8, false), ("src/Shipping.cs", 1, 0.2, false)]);

        // `impact` lists Discounts as coupled (no reference links it), never as a proximity class; Shipping's one shared
        // commit is too little.
        let r = impact_of(&app, "Pricing.Quote");
        let coupled: Vec<(&str, &str, u32)> = r.coupled.iter().map(|c| (c.file.as_str(), c.with.as_str(), c.support)).collect();
        assert_eq!(coupled, [("src/Discounts.cs", "src/Pricing.cs", 4)]);
        assert_eq!((r.nearest(), r.legacy_worst()), ("direct", "critical"));
        assert!(r.missed_partners.is_empty(), "only a diff warns");

        // A diff touching Pricing alone is warned about its usual partner.
        append(&app, "src/Pricing.cs", "// again\n");
        let p = ImpactParams { symbol: None, file: None, diff: Some("working".into()), depth: 3, limit: 100, ..Default::default() };
        let d = impact(&p, &scope).unwrap();
        let warned: Vec<&str> = d.data[0].missed_partners.iter().map(|m| m.message.as_str()).collect();
        assert_eq!(warned, ["you changed src/Pricing.cs; src/Discounts.cs changed with it in 4 of 5 commits"]);

        // Tests: PricingTests calls the change and is named for it, QuoteTests calls it through its base class;
        // DiscountsTests has no signal for Pricing.
        let t = tests(&TestsParams { targets: vec!["src/Pricing.cs".into()], diff: None, limit: 10, evidence: String::new() }, &scope).unwrap();
        let sel = &t.data[0].selection;
        assert_eq!(sel.selected.iter().map(|t| t.file.as_str()).collect::<Vec<_>>(), ["tests/App.Tests/PricingTests.cs", "tests/App.Tests/QuoteTests.cs"]);
        let pick = &sel.selected[0];
        assert_eq!(pick.reasons, ["PricingTests.Quotes calls Pricing.Quote at hop 1", "named for src/Pricing.cs"]);
        assert_eq!(pick.methods, ["PricingTests.Quotes"]);
        assert!(sel.left_out.iter().all(|t| t.file != "tests/App.Tests/DiscountsTests.cs"));
        // A helper that changed isn't a test to run; the tests using it are.
        let t = tests(&TestsParams { targets: vec!["tests/App.Tests/TestBase.cs".into()], diff: None, limit: 10, evidence: String::new() }, &scope).unwrap();
        let sel = &t.data[0].selection;
        assert!(sel.selected.iter().all(|t| t.file != "tests/App.Tests/TestBase.cs"), "{:?}", sel.selected);
        let helper = sel.left_out.iter().find(|t| t.file == "tests/App.Tests/TestBase.cs").unwrap();
        assert_eq!(helper.reasons, ["changed itself", "no tests in it (a helper or base class)"]);
        assert_eq!(sel.selected.iter().map(|t| t.file.as_str()).collect::<Vec<_>>(), ["tests/App.Tests/QuoteTests.cs"]);
        // By symbol, and for the Discounts file: named for it, so selected; left-out tests are reported.
        let t = tests(&TestsParams { targets: vec!["Discounts".into()], diff: None, limit: 10, evidence: String::new() }, &scope).unwrap();
        assert_eq!(t.data[0].selection.selected.iter().map(|t| t.file.as_str()).collect::<Vec<_>>(), ["tests/App.Tests/DiscountsTests.cs"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A test that calls the changed code but never changed with it (a regression test written once) is as likely to
    /// fail as the test named for the code: reachability ranks it into the tight list, co-change only adds weight, and
    /// the answer says the list is the most likely relevant tests, not the only ones.
    #[test]
    fn a_test_reaching_the_change_is_selected_without_co_change_history() {
        let cache = test_cache();
        let base = cache.dir("tests-reach");
        let app = repo(
            &base,
            "app",
            &[
                ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/Pricing.cs", "public class Pricing\n{\n    public int Quote() => 1;\n    public int Spare() => 2;\n}\n"),
                ("src/Checkout.cs", "public class Checkout\n{\n    private readonly Pricing _p;\n    public int Total() => _p.Quote();\n}\n"),
                ("tests/App.Tests/PricingTests.cs", "public class PricingTests\n{\n    private readonly Pricing _p;\n    [Fact]\n    public void Quotes() => _p.Quote();\n}\n"),
                ("tests/App.Tests/RefundRegressionTests.cs", "public class RefundRegressionTests\n{\n    private readonly Pricing _p;\n    [Fact]\n    public void KeepsTheQuote() => _p.Quote();\n}\n"),
                ("tests/App.Tests/CheckoutTests.cs", "public class CheckoutTests\n{\n    private readonly Checkout _c;\n    [Fact]\n    public void Totals() => _c.Total();\n}\n"),
                ("tests/App.Tests/SpareTests.cs", "public class SpareTests\n{\n    private readonly Pricing _p;\n    [Fact]\n    public void Spares() => _p.Spare();\n}\n"),
                ("tests/App.Tests/LedgerTests.cs", "public class LedgerTests\n{\n    [Fact]\n    public void Balances() { }\n}\n"),
            ],
        );
        commit(&app, "add the shop");
        // The named test changed with Pricing every time; the regression and checkout tests never did.
        for i in 0..3 {
            append(&app, "src/Pricing.cs", &format!("// rule {i}\n"));
            append(&app, "tests/App.Tests/PricingTests.cs", &format!("// rule {i}\n"));
            append(&app, "tests/App.Tests/LedgerTests.cs", &format!("// rule {i}\n"));
            commit(&app, &format!("pricing rule {i}"));
        }
        let scope = Scope { repos: vec![app.clone()], no_sync: false };
        let text = std::fs::read_to_string(app.join("src/Pricing.cs")).unwrap();
        std::fs::write(app.join("src/Pricing.cs"), text.replace("Quote() => 1", "Quote() => 7")).unwrap();
        let t = tests(&TestsParams { targets: Vec::new(), diff: Some("working".into()), limit: 10, evidence: String::new() }, &scope).unwrap();
        let sel = &t.data[0].selection;
        let mut tight: Vec<&str> = sel.selected.iter().map(|t| t.file.as_str()).collect();
        tight.sort_unstable();
        assert_eq!(
            tight,
            ["tests/App.Tests/CheckoutTests.cs", "tests/App.Tests/PricingTests.cs", "tests/App.Tests/RefundRegressionTests.cs"],
            "left out: {:?}",
            sel.left_out.iter().map(|t| (&t.file, t.confidence)).collect::<Vec<_>>()
        );
        // Co-change ranks the named test first; the direct caller comes before the one two hops away.
        let order: Vec<&str> = sel.selected.iter().map(|t| t.file.as_str()).collect();
        assert_eq!(order[0], "tests/App.Tests/PricingTests.cs");
        assert!(order.iter().position(|f| f.ends_with("RefundRegressionTests.cs")) < order.iter().position(|f| f.ends_with("CheckoutTests.cs")));
        // SpareTests calls only an unchanged member of the changed file, but is named for nothing changed: no signal.
        assert!(sel.selected.iter().chain(&sel.left_out).all(|t| !t.file.ends_with("SpareTests.cs")));
        // The answer says what the list is.
        // LedgerTests only changed with Pricing: a candidate, ranked, listed after the tight list.
        assert_eq!(sel.left_out.iter().map(|t| t.file.as_str()).collect::<Vec<_>>(), ["tests/App.Tests/LedgerTests.cs"]);
        // The answer says what the list is: a sample by design of every candidate, never a list of all that matter.
        let item = &t.env["items"][0];
        assert!(item["tests_are"].as_str().unwrap().starts_with("most likely relevant"), "{item}");
        assert_eq!(item["tests_total"], json!(4), "{item}");
        assert_eq!(t.env["sampled"], json!(["tests"]), "{}", t.env);
        let _ = std::fs::remove_dir_all(&base);
    }

    fn diff_impact(root: &Path) -> ImpactRepo {
        let p = ImpactParams { diff: Some("working".into()), depth: 3, limit: 100, ..Default::default() };
        impact(&p, &Scope { repos: vec![root.to_path_buf()], no_sync: false }).unwrap().data.remove(0)
    }

    /// `(qualified, via, via_removed, confidence)` of the hop-1 affected symbols, sorted.
    fn hop1(r: &ImpactRepo) -> Vec<(String, String, bool, &'static str)> {
        let mut v: Vec<_> =
            r.impact.affected.iter().filter(|a| a.hop == 1).map(|a| (a.qualified.clone(), a.via.clone(), a.via_removed, a.confidence)).collect();
        v.sort();
        v
    }

    const API_PY: &str = "def save():\n    return 1\n\n\ndef load():\n    return 2\n";
    const CALLER_PY: &str = "from api import save, load\n\n\ndef run():\n    return save()\n\n\ndef read():\n    return load()\n";
    const SVC_CS: &str = "public class Store { public void Put() {} public void Keep() {} }\n\
public class User { public void Go(Store s) { s.Put(); s.Keep(); } }\n\
public class Other { public void Put() {} }\n";

    /// Review case `deleted`: a diff deleting a file, or renaming or cutting a definition, still reaches the code
    /// naming what it removed, marked `via_removed` and listed in `removed`; a definition moved to another changed
    /// file is not removed.
    #[test]
    fn diff_impact_reaches_callers_of_removed_definitions() {
        let cache = test_cache();
        let base = cache.dir("diff-removed");
        let app = repo(&base, "app", &[("api.py", API_PY), ("caller.py", CALLER_PY), ("util.py", "def other():\n    return 0\n"), ("src/Svc.cs", SVC_CS)]);
        commit(&app, "init");
        let s = |q: &str, via: &str, c: &'static str| (q.to_string(), via.to_string(), true, c);

        // Deleted with its file.
        std::fs::remove_file(app.join("api.py")).unwrap();
        let r = diff_impact(&app);
        assert_eq!(hop1(&r), [s("read", "load", "unique_name"), s("run", "save", "unique_name")]);
        let removed: Vec<(&str, &str, u32, usize)> = r.removed.iter().map(|g| (g.qualified.as_str(), g.file.as_str(), g.line, g.named_by.len())).collect();
        assert_eq!(removed, [("save", "api.py", 1, 1), ("load", "api.py", 5, 1)]);
        assert_eq!((r.nearest(), r.legacy_worst()), ("direct", "critical"));
        assert!(r.base.as_deref().is_some_and(|b| b.len() == 40));
        let text = crate::render::impact_text(std::slice::from_ref(&r), false);
        assert!(text.contains("removed function save  api.py:1 at "), "{text}");
        assert!(text.contains("(names save, which the change removes)"), "{text}");

        // Renamed: `save` is gone, `store` is new (and in scope).
        std::fs::write(app.join("api.py"), API_PY.replace("def save", "def store")).unwrap();
        let r = diff_impact(&app);
        assert_eq!(hop1(&r), [s("run", "save", "unique_name")]);
        assert!(r.impact.scope.iter().any(|n| n.qualified == "store"));

        // Moved to another changed file: still defined, and in scope, so nothing is removed.
        std::fs::write(app.join("api.py"), "def load():\n    return 2\n").unwrap();
        std::fs::write(app.join("util.py"), "def other():\n    return 0\n\n\ndef save():\n    return 1\n").unwrap();
        let r = diff_impact(&app);
        assert!(r.removed.is_empty(), "{:?}", r.removed);
        assert!(hop1(&r).iter().all(|a| !a.2), "{:?}", hop1(&r));

        // A member cut from its class while another class keeps one of the name: the receiver's type picks it. The
        // caller is reached first through the changed class (its line changed), and still listed as naming `Put`.
        std::fs::write(app.join("api.py"), API_PY).unwrap();
        std::fs::write(app.join("util.py"), "def other():\n    return 0\n").unwrap();
        std::fs::write(app.join("src/Svc.cs"), SVC_CS.replace("public void Put() {} public void Keep", "public void Keep")).unwrap();
        let r = diff_impact(&app);
        assert_eq!(r.removed.iter().map(|g| g.qualified.as_str()).collect::<Vec<_>>(), ["Store.Put"]);
        let named: Vec<(&str, &str, &str)> = r.removed[0].named_by.iter().map(|n| (n.qualified.as_str(), n.site.as_str(), n.confidence)).collect();
        assert_eq!(named, [("User.Go", "src/Svc.cs:2", "unique_name")]);
        assert!(r.impact.affected.iter().any(|a| a.qualified == "User.Go"));
        let text = crate::render::impact_text(std::slice::from_ref(&r), false);
        assert!(text.contains("names it User.Go"), "{text}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Code naming a removed definition is one more way to it at hop 1, weighed with the walk's other ways: the
    /// nearer confident way is the row's evidence, and a nearer ambiguous one sets its class.
    #[test]
    fn a_removed_definitions_caller_is_classed_by_its_nearest_way() {
        let cache = test_cache();
        let base = cache.dir("diff-removed-class");
        let api = "def gone():\n    return 1\n\n\ndef kept():\n    return 2\n";
        let app = repo(
            &base,
            "app",
            &[("api.py", api), ("mid.py", "def mid():\n    return kept()\n"), ("caller.py", "def near():\n    gone()\n    mid()\n")],
        );
        commit(&app, "init");
        // `gone` deleted and `kept` changed: `near` names `gone` (hop 1) and calls `mid`, which calls `kept` (hop 2).
        std::fs::write(app.join("api.py"), "def kept():\n    return 3\n").unwrap();
        let r = diff_impact(&app);
        let near: Vec<_> = r.impact.affected.iter().filter(|a| a.qualified == "near").collect();
        assert_eq!(near.len(), 1, "listed once");
        assert_eq!((near[0].hop, near[0].via_removed, near[0].nearest_ambiguous_hop, near[0].class), (1, true, None, "direct"));
        // Another `gone` is left elsewhere: naming it is ambiguous, so the confident way through `mid` is the evidence,
        // and the nearer ambiguous one keeps the row direct.
        std::fs::write(app.join("api.py"), api).unwrap();
        std::fs::write(app.join("other.py"), "def gone():\n    return 0\n").unwrap();
        commit(&app, "other");
        std::fs::write(app.join("api.py"), "def kept():\n    return 3\n").unwrap();
        let r = diff_impact(&app);
        let near: Vec<_> = r.impact.affected.iter().filter(|a| a.qualified == "near").collect();
        assert_eq!(near.len(), 1, "listed once");
        let n = near[0];
        assert_eq!((n.hop, n.via_removed, n.nearest_ambiguous_hop, n.class, n.via.as_str()), (2, false, Some(1), "direct", "mid"), "{n:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Two uses of a removed member's name on one line are judged each on its own: the typed call is `unique_name`,
    /// the one through a receiver of unknown type `ambiguous`, and a call bound to another type's member isn't one.
    #[test]
    fn removed_definitions_keep_same_line_uses_apart() {
        let cache = test_cache();
        let base = cache.dir("diff-removed-line");
        let svc = "public class Store { public void Put() {} public void Keep() {} }\n\
public class Other { public void Put() {} }\n\
public class User { public void Go(Store s, Other o) { o.Put(); s.Put(); var x = Pick(); x.Put(); } }\n";
        let app = repo(&base, "app", &[("src/Svc.cs", svc)]);
        commit(&app, "init");
        std::fs::write(app.join("src/Svc.cs"), svc.replace("public void Put() {} public void Keep", "public void Keep")).unwrap();
        let r = diff_impact(&app);
        let gone = r.removed.iter().find(|g| g.qualified == "Store.Put").expect("Store.Put removed");
        let line = svc.lines().nth(2).unwrap();
        let col = |needle: &str| (line.find(needle).unwrap() + needle.len() - 2) as u32;
        let named: Vec<(&str, u32, &str)> = gone.named_by.iter().map(|n| (n.site.as_str(), n.col, n.confidence)).collect();
        assert_eq!(named, [("src/Svc.cs:3", col("s.Put"), "unique_name"), ("src/Svc.cs:3", col("x.Put"), "ambiguous")], "{gone:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Invariant: deleting a definition never loses a dependent. Every hop-1 dependent `impact` gave the symbol before
    /// the change is in the diff's impact after deleting it, and listed as naming the removed definition.
    #[test]
    fn invariant_deleting_a_definition_keeps_its_dependents() {
        let cache = test_cache();
        let base = cache.dir("diff-removed-invariant");
        let app = repo(&base, "app", &[("api.py", API_PY), ("caller.py", CALLER_PY), ("src/Svc.cs", SVC_CS)]);
        commit(&app, "init");
        let cases: [(&str, &str, String); 4] = [
            ("save", "api.py", "def load():\n    return 2\n".to_string()),
            ("load", "api.py", "def save():\n    return 1\n".to_string()),
            ("Store.Put", "src/Svc.cs", SVC_CS.replace("public void Put() {} public void Keep", "public void Keep")),
            ("Store.Keep", "src/Svc.cs", SVC_CS.replace(" public void Keep() {}", "")),
        ];
        for (symbol, file, without) in cases {
            let before: Vec<String> = impact_of(&app, symbol).impact.affected.iter().filter(|a| a.hop == 1).map(|a| a.qualified.clone()).collect();
            assert!(!before.is_empty(), "{symbol}");
            let original = std::fs::read_to_string(app.join(file)).unwrap();
            std::fs::write(app.join(file), &without).unwrap();
            let after = diff_impact(&app);
            let gone = after.removed.iter().find(|g| g.qualified == symbol).unwrap_or_else(|| panic!("{symbol} not removed: {:?}", after.removed));
            for q in &before {
                assert!(after.impact.affected.iter().any(|a| &a.qualified == q), "{symbol}: {q} lost; {:?}", hop1(&after));
                assert!(gone.named_by.iter().any(|n| &n.qualified == q), "{symbol}: {q} not marked; {:?}", gone.named_by);
            }
            std::fs::write(app.join(file), original).unwrap();
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Templates and stylesheets are partners with their kind; a moved file keeps its partners; a job's commits
    /// couple nothing.
    #[test]
    fn coupled_lists_templates_follows_moves_and_skips_bots() {
        let cache = test_cache();
        let base = cache.dir("coupled-kinds");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let app = repo(
            &base,
            "web",
            &[
                ("src/app/cart.component.ts", "export class CartComponent {\n  total = 0;\n}\n"),
                ("src/app/cart.component.html", "<p>{{ total }}</p>\n"),
                ("src/app/cart.component.scss", "p { color: red; }\n"),
                ("src/app/cart.store.ts", "export class CartStore {\n  items = [];\n}\n"),
                ("package-lock.json", "{}\n"),
            ],
        );
        commit(&app, "add the cart");
        for i in 0..3 {
            append(&app, "src/app/cart.component.ts", &format!("// {i}\n"));
            append(&app, "src/app/cart.component.html", &format!("<!-- {i} -->\n"));
            append(&app, "src/app/cart.component.scss", &format!("/* {i} */\n"));
            append(&app, "src/app/cart.store.ts", &format!("// {i}\n"));
            append(&app, "package-lock.json", " \n");
            commit(&app, &format!("cart {i}"));
        }
        // The store moves to a state folder, then a job rewrites it with the component every night.
        std::fs::create_dir_all(app.join("src/app/state")).unwrap();
        std::fs::rename(app.join("src/app/cart.store.ts"), app.join("src/app/state/cart.store.ts")).unwrap();
        commit(&app, "move the store");
        for i in 0..4 {
            append(&app, "src/app/cart.component.ts", &format!("// job {i}\n"));
            append(&app, "src/app/state/cart.store.ts", &format!("// job {i}\n"));
            commit_as(&app, "jenkins", "Automated task via jenkins");
        }
        let scope = Scope { repos: vec![app.clone()], no_sync: false };
        let c = coupled("src/app/cart.component.ts", 10, &vitals::Span::default(), &scope).unwrap();
        let partners: Vec<(&str, &str, u32, u32)> =
            c.data[0].partners.iter().map(|p| (p.coupled.file.as_str(), p.coupled.kind.as_str(), p.coupled.support, p.coupled.commits)).collect();
        assert_eq!(
            partners,
            [
                ("src/app/cart.component.html", "template", 4, 4),
                ("src/app/cart.component.scss", "style", 4, 4),
                ("src/app/state/cart.store.ts", "typescript", 4, 4),
            ]
        );
        // Each carries the weight it is ranked by: shared commits, each halved per 90 days of age.
        let w: Vec<f64> = c.data[0].partners.iter().map(|p| p.coupled.weight).collect();
        assert!(w.iter().all(|&x| x > 0.0 && x <= 4.0) && w.windows(2).all(|p| p[0] >= p[1]), "{w:?}");
        assert!(c.env["items"][0]["partners"][0]["weight"].is_number());
        let h = hotspots(None, 10, None, &scope).unwrap();
        assert_eq!(h.env["history"]["bot_commits_365d"], 4);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Co-change needs only history: a repo of Dockerfiles, YAML and Terraform (no file indexed) has partners of
    /// any kind, a changelog changing with everything is left out, and `impact --file` lists them as coupled.
    #[test]
    fn coupled_works_on_any_tracked_file() {
        let cache = test_cache();
        let base = cache.dir("coupled-any");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let app = repo(
            &base,
            "infra",
            &[
                ("Dockerfile", "FROM alpine\n"),
                ("deploy/app.yml", "replicas: 1\n"),
                ("main.tf", "resource \"null_resource\" \"x\" {}\n"),
                ("CHANGELOG.md", "# Changes\n"),
                ("notes/todo.md", "- one\n"),
            ],
        );
        commit(&app, "start");
        for i in 0..3 {
            append(&app, "Dockerfile", &format!("# {i}\n"));
            append(&app, "deploy/app.yml", &format!("# {i}\n"));
            commit(&app, &format!("image {i}"));
        }
        for i in 0..2 {
            append(&app, "Dockerfile", &format!("# tf {i}\n"));
            append(&app, "main.tf", &format!("# {i}\n"));
            commit(&app, &format!("infra {i}"));
        }
        append(&app, "Dockerfile", "# release\n");
        append(&app, "CHANGELOG.md", "- release\n");
        commit(&app, "release");
        for i in 0..11 {
            append(&app, "CHANGELOG.md", &format!("- {i}\n"));
            append(&app, "notes/todo.md", &format!("- {i}\n"));
            commit(&app, &format!("notes {i}"));
        }
        let scope = Scope { repos: vec![app.clone()], no_sync: false };
        let c = coupled("Dockerfile", 10, &vitals::Span::default(), &scope).unwrap();
        let partners: Vec<(&str, &str, u32, u32)> =
            c.data[0].partners.iter().map(|p| (p.coupled.file.as_str(), p.coupled.kind.as_str(), p.coupled.support, p.coupled.commits)).collect();
        // Each was created with it, and changed with it since.
        assert_eq!(partners, [("deploy/app.yml", "config", 4, 7), ("main.tf", "terraform", 3, 7)]);
        // The changelog's own partner, though, is listed: it changed with the notes in 11 of its 12 commits.
        let c = coupled("CHANGELOG.md", 10, &vitals::Span::default(), &scope).unwrap();
        assert_eq!(c.data[0].partners[0].coupled.file, "notes/todo.md");
        let p = ImpactParams { file: Some("Dockerfile".into()), depth: 1, limit: 100, ..Default::default() };
        let r = impact(&p, &scope).unwrap();
        let coupled: Vec<&str> = r.data[0].coupled.iter().map(|c| c.file.as_str()).collect();
        assert_eq!(coupled, ["deploy/app.yml", "main.tf"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A commit dated `days` ago.
    fn commit_days_ago(dir: &Path, msg: &str, days: i64) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - days * 86_400;
        let date = format!("@{t} +0000");
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", msg]] {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@x", "-c", "commit.gpgsign=false"])
                .args(&args)
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }
    }

    /// `--as-of` reads only the history up to a commit, so a later commit's partner isn't seen; a file the last 365
    /// days never changed is read over its own last commits, however old.
    #[test]
    fn coupled_as_of_a_commit_and_for_dormant_files() {
        let cache = test_cache();
        let base = cache.dir("coupled-span");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let app = repo(
            &base,
            "svc",
            &[
                ("src/A.cs", "class A {}\n"),
                ("deploy/a.yml", "x: 1\n"),
                ("src/C.cs", "class C {}\n"),
                ("src/B.cs", "class B {}\n"),
                ("old/Legacy.cs", "class L {}\n"),
                ("old/legacy.sql", "select 1;\n"),
            ],
        );
        commit_days_ago(&app, "start", 900);
        for i in 0..3 {
            append(&app, "old/Legacy.cs", &format!("// {i}\n"));
            append(&app, "old/legacy.sql", &format!("-- {i}\n"));
            commit_days_ago(&app, &format!("legacy {i}"), 800 - i);
        }
        for i in 0..3 {
            append(&app, "src/A.cs", &format!("// {i}\n"));
            append(&app, "deploy/a.yml", &format!("# {i}\n"));
            if i < 2 {
                append(&app, "src/B.cs", &format!("// {i}\n"));
            }
            commit_days_ago(&app, &format!("a {i}"), 30 - i);
        }
        let parent = String::from_utf8(
            std::process::Command::new("git").arg("-C").arg(&app).args(["rev-parse", "HEAD"]).output().unwrap().stdout,
        )
        .unwrap();
        append(&app, "src/A.cs", "// c\n");
        append(&app, "src/C.cs", "// a\n");
        // B goes in the answer commit: it was there at the parent.
        std::fs::remove_file(app.join("src/B.cs")).unwrap();
        commit_days_ago(&app, "a with c", 1);
        let scope = Scope { repos: vec![app.clone()], no_sync: false };
        let files = |c: &Out<Vec<CoupledRepo>>| c.data[0].partners.iter().map(|p| (p.coupled.file.clone(), p.coupled.support)).collect::<Vec<_>>();
        let now = coupled("src/A.cs", 10, &vitals::Span::default(), &scope).unwrap();
        assert!(files(&now).contains(&("src/C.cs".to_string(), 1)));
        assert_eq!(now.data[0].history, "365 days before HEAD");
        let then = coupled("src/A.cs", 10, &vitals::Span { as_of: Some(parent.trim().to_string()), since: None }, &scope).unwrap();
        // The start commit is 900 days old: outside the 365 days before the parent.
        assert_eq!(files(&then), [("deploy/a.yml".to_string(), 3), ("src/B.cs".to_string(), 2)], "the answer commit is not seen");
        // B is gone now, and says so; today's answer doesn't list it.
        assert_eq!(then.data[0].partners.iter().map(|p| p.coupled.exists_now).collect::<Vec<_>>(), [true, false]);
        assert_eq!(then.env["items"][0]["partners"][1]["exists_now"], false);
        assert!(then.env["items"][0]["partners"][0].get("exists_now").is_none());
        assert!(files(&now).iter().all(|(f, _)| f != "src/B.cs"));
        assert!(then.data[0].history.starts_with("365 days before "), "{}", then.data[0].history);
        // The same by date, and a window too short for anything.
        let short = coupled("src/A.cs", 10, &vitals::Span { as_of: None, since: Some("5d".into()) }, &scope).unwrap();
        assert_eq!(files(&short), [("src/C.cs".to_string(), 1)]);
        // Legacy files last changed two years ago still have their partner.
        let old = coupled("old/Legacy.cs", 10, &vitals::Span::default(), &scope).unwrap();
        assert_eq!(files(&old), [("old/legacy.sql".to_string(), 4)]);
        assert_eq!(old.data[0].history, "its last 50 commits");
        // `impact` reads the same span for its coupled class.
        let p = ImpactParams { file: Some("src/A.cs".into()), depth: 1, limit: 100, cochange: vitals::Span { as_of: Some(parent.trim().to_string()), since: None }, ..Default::default() };
        let r = impact(&p, &scope).unwrap();
        assert!(r.data[0].coupled.iter().all(|c| c.file != "src/C.cs"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The invariant of every `coupled` answer: a partner is history (`basis` `history`, with its shared commits), a
    /// structural guess is never presented as one (`basis` `structure`, `confidence` `low`, no co-change numbers,
    /// each relation with its own evidence), guesses come only with a note saying history had nothing to rank, and
    /// no file is both.
    fn guesses_apart_from_history(env: &Value) {
        for item in env["items"].as_array().unwrap() {
            let partners = item["partners"].as_array().unwrap();
            for p in partners {
                assert_eq!(p["basis"], "history", "{p}");
                assert!(p["support"].as_u64().is_some_and(|s| s >= 1) && p["confidence"].is_f64(), "{p}");
            }
            let Some(guesses) = item["structural"].as_array() else {
                assert!(item.get("structural_total").is_none() && item.get("note").is_none(), "{item}");
                continue;
            };
            assert!(item["note"].as_str().is_some_and(|n| n.starts_with("history had nothing")), "{item}");
            assert!(partners.iter().all(|p| p["support"].as_u64() < Some(RANKABLE_SUPPORT as u64)), "{item}");
            for g in guesses {
                assert_eq!((g["basis"].as_str(), g["confidence"].as_str()), (Some("structure"), Some("low")), "{g}");
                for k in ["support", "commits", "weight", "static_link"] {
                    assert!(g.get(k).is_none(), "a guess shows history's {k}: {g}");
                }
                let rels = g["relations"].as_array().unwrap();
                assert!(!rels.is_empty() && rels.iter().all(|r| r["evidence"]["mechanism"].is_string()), "{g}");
                assert!(!partners.iter().any(|p| p["file"] == g["file"]), "{g} is also a history partner");
            }
        }
    }

    /// `coupled` on a file whose history has nothing before the pin (the agent-benefit study's case): structural
    /// guesses as of the pinned commit, labelled as guesses: what it uses and what uses it then, its interface, the
    /// test named for it, its directory; never a link or a file a later commit made. With rankable history there are
    /// none.
    #[test]
    fn coupled_falls_back_to_structure_when_history_has_nothing() {
        let cache = test_cache();
        let base = cache.dir("coupled-fallback");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let app = repo(
            &base,
            "shop",
            &[
                ("src/Orders/IOrderStore.cs", "namespace Shop;\npublic interface IOrderStore\n{\n    void Keep(Order order);\n}\n"),
                ("src/Orders/Order.cs", "namespace Shop;\npublic class Order\n{\n    public decimal Total { get; set; }\n}\n"),
                ("src/Orders/OrderStore.cs", "namespace Shop;\npublic class OrderStore : IOrderStore\n{\n    public void Keep(Order order) { }\n}\n"),
                (
                    "src/Orders/OrderPlacer.cs",
                    "namespace Shop;\npublic class OrderPlacer\n{\n    private readonly IOrderStore store;\n    public void Place(Order order)\n    {\n        var total = PriceRules.Round(order.Total);\n        store.Keep(order);\n    }\n}\n",
                ),
                ("src/Orders/OrdersController.cs", "namespace Shop;\npublic class OrdersController\n{\n    public void Post(OrderPlacer placer, Order order) => placer.Place(order);\n}\n"),
                ("src/Pricing/PriceRules.cs", "namespace Shop;\npublic static class PriceRules\n{\n    public static decimal Round(decimal d) => d;\n}\n"),
                ("src/Billing/InvoiceMaker.cs", "namespace Shop;\npublic class InvoiceMaker\n{\n    public void Make(Order order) { }\n}\n"),
                ("tests/OrderPlacerTests.cs", "namespace Shop.Tests;\npublic class OrderPlacerTests\n{\n    public void Places() => new OrderPlacer();\n}\n"),
            ],
        );
        commit_days_ago(&app, "start", 20);
        let pin = String::from_utf8(std::process::Command::new("git").arg("-C").arg(&app).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap();
        let pin = pin.trim().to_string();
        // After the pin: OrderPlacer starts invoicing, and a refund file uses it. Neither may reach the answer as of
        // the pin.
        std::fs::write(
            app.join("src/Orders/OrderPlacer.cs"),
            "namespace Shop;\npublic class OrderPlacer\n{\n    private readonly IOrderStore store;\n    public void Place(Order order)\n    {\n        var total = PriceRules.Round(order.Total);\n        store.Keep(order);\n        new InvoiceMaker().Make(order);\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(app.join("src/Orders/RefundTaker.cs"), "namespace Shop;\npublic class RefundTaker\n{\n    public void Take(OrderPlacer p) { }\n}\n").unwrap();
        commit_days_ago(&app, "invoice on place", 10);
        let scope = Scope { repos: vec![app.clone()], no_sync: false };
        let as_of = vitals::Span { as_of: Some(pin.clone()), since: None };
        let then = coupled("src/Orders/OrderPlacer.cs", 10, &as_of, &scope).unwrap();
        guesses_apart_from_history(&then.env);
        let item = &then.env["items"][0];
        assert!(item["partners"].as_array().unwrap().is_empty(), "{item:#}");
        assert!(then.env["note"].as_str().unwrap().starts_with("history had nothing: no commit in 365 days before"), "{:#}", then.env);
        let rel = |g: &Value| -> Vec<String> { g["relations"].as_array().unwrap().iter().map(|r| r["how"].as_str().unwrap().to_string()).collect() };
        let got: HashMap<String, Vec<String>> = item["structural"].as_array().unwrap().iter().map(|g| (g["file"].as_str().unwrap().to_string(), rel(g))).collect();
        assert_eq!(got.get("src/Orders/IOrderStore.cs").map(Vec::as_slice), Some(&["uses".to_string(), "same_directory".to_string()][..]), "{got:?}");
        assert_eq!(got.get("src/Pricing/PriceRules.cs").map(Vec::as_slice), Some(&["uses".to_string()][..]), "{got:?}");
        assert_eq!(got.get("src/Orders/OrdersController.cs").map(Vec::as_slice), Some(&["used_by".to_string(), "same_directory".to_string()][..]), "{got:?}");
        assert_eq!(got.get("tests/OrderPlacerTests.cs").map(Vec::as_slice), Some(&["used_by".to_string(), "test_named".to_string()][..]), "{got:?}");
        assert!(got.contains_key("src/Orders/OrderStore.cs"), "{got:?}");
        // Later history doesn't leak in: the invoicing link and the refund file came after the pin.
        assert!(!got.contains_key("src/Billing/InvoiceMaker.cs") && !got.contains_key("src/Orders/RefundTaker.cs"), "{got:?}");
        assert_eq!(item["structural"][0]["relations"][0]["evidence"]["mechanism"], "name_unique");

        // At HEAD the one shared commit can't be ranked: the refund file it brought stays a history partner (and
        // isn't guessed too), and the guesses read today's structure (the invoicing link now).
        let now = coupled("src/Orders/OrderPlacer.cs", 10, &vitals::Span::default(), &scope).unwrap();
        guesses_apart_from_history(&now.env);
        let item = &now.env["items"][0];
        assert!(item["note"].as_str().unwrap().starts_with("history had nothing to rank"), "{item:#}");
        let partners: Vec<&str> = item["partners"].as_array().unwrap().iter().map(|p| p["file"].as_str().unwrap()).collect();
        assert_eq!(partners, ["src/Orders/RefundTaker.cs"], "{item:#}");
        let guessed: Vec<&str> = item["structural"].as_array().unwrap().iter().map(|g| g["file"].as_str().unwrap()).collect();
        assert!(guessed.contains(&"src/Billing/InvoiceMaker.cs") && !guessed.contains(&"src/Orders/RefundTaker.cs"), "{guessed:?}");

        // Rankable history: no guesses.
        for i in 0..2 {
            append(&app, "src/Orders/OrderPlacer.cs", &format!("// {i}\n"));
            append(&app, "src/Orders/OrdersController.cs", &format!("// {i}\n"));
            commit_days_ago(&app, &format!("both {i}"), 5 - i);
        }
        let ranked = coupled("src/Orders/OrderPlacer.cs", 10, &vitals::Span::default(), &scope).unwrap();
        guesses_apart_from_history(&ranked.env);
        assert!(ranked.env["items"][0].get("structural").is_none() && ranked.env.get("note").is_none(), "{:#}", ranked.env);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Terraform templates and Jinja templates are `locate` documents.
    #[test]
    fn locate_reads_templates() {
        let cache = test_cache();
        let base = cache.dir("locate-templates");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let app = repo(
            &base,
            "infra",
            &[
                ("modules/node/user_data.tftpl", "#!/bin/bash\n# Bootstraps the payment worker\nWORKER_QUEUE=${queue}\n"),
                ("templates/site.conf.j2", "{# Storefront proxy #}\nserver_name {{ host }};\n"),
                ("policies/bucket.tpl", "{ \"Effect\": \"Allow\", \"Action\": \"s3:GetObject\" }\n"),
                ("env/prod.hcl", "inputs = { region = \"eu-west-1\" }\n"),
            ],
        );
        commit(&app, "start");
        let scope = Scope { repos: vec![app.clone()], no_sync: false };
        let files = |q: &str| locate(q, 10, &scope).unwrap().data.items.into_iter().map(|(_, h)| h.file).collect::<Vec<_>>();
        assert_eq!(files("payment worker bootstrap"), ["modules/node/user_data.tftpl"]);
        assert_eq!(files("storefront proxy"), ["templates/site.conf.j2"]);
        assert_eq!(files("allow object"), ["policies/bucket.tpl"]);
        assert_eq!(files("region"), ["env/prod.hcl"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `locate` reads config, docs and scripts the index holds no symbols for, the words of past commit subjects,
    /// and ranks an implementation above its tests unless the query asks for tests.
    #[test]
    fn locate_reads_text_files_history_and_prefers_implementations() {
        let cache = test_cache();
        let base = cache.dir("locate-text");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let app = repo(
            &base,
            "ops",
            &[
                ("src/Billing/InvoiceService.cs", "public class InvoiceService\n{\n    public void Send(int id) {}\n}\n"),
                (
                    "tests/Billing.Tests/InvoiceServiceTests.cs",
                    "public class InvoiceServiceTests\n{\n    public void SendsInvoice() => new InvoiceService().Send(1);\n}\n",
                ),
                ("deploy/consul.yml", "# Register the worker with consul\nservice_name: invoice-worker\n"),
                ("docs/runbook.md", "# Failover\nPromote the replica.\n"),
                ("src/Payouts/Ledger.cs", "public class Ledger\n{\n    public decimal Total;\n}\n"),
            ],
        );
        commit(&app, "start");
        for i in 0..2 {
            append(&app, "src/Payouts/Ledger.cs", &format!("// {i}\n"));
            commit(&app, "Settlement rounding for partner payouts");
        }
        let scope = Scope { repos: vec![app.clone()], no_sync: false };
        let files = |q: &str| locate(q, 10, &scope).unwrap().data.items.into_iter().map(|(_, h)| h.file).collect::<Vec<_>>();
        assert_eq!(files("invoice")[..2], ["src/Billing/InvoiceService.cs", "tests/Billing.Tests/InvoiceServiceTests.cs"]);
        assert_eq!(files("register worker in consul")[0], "deploy/consul.yml");
        assert_eq!(files("failover replica"), ["docs/runbook.md"]);
        assert_eq!(files("settlement"), ["src/Payouts/Ledger.cs"]);
        // Asked for tests, the test file keeps its full score.
        let score = |q: &str, f: &str| locate(q, 10, &scope).unwrap().data.items.into_iter().find(|(_, h)| h.file == f).unwrap().1.score;
        let t = "tests/Billing.Tests/InvoiceServiceTests.cs";
        assert!(score("invoice tests", t) > score("invoice", t) * 1.5);
        // A config edit shows in the next query.
        std::fs::write(app.join("deploy/consul.yml"), "service_name: billing-gateway\n").unwrap();
        assert_eq!(files("gateway"), ["deploy/consul.yml"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Plain JavaScript is indexed with the TypeScript machinery; minified and built files are not.
    #[test]
    fn javascript_is_indexed_and_bundles_are_not() {
        let cache = test_cache();
        let base = cache.dir("javascript");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let long = format!("var a={};\n", "1+".repeat(800) + "1");
        let app = repo(
            &base,
            "site",
            &[
                ("src/cart.js", "export function addToCart(item) {\n  return item;\n}\n"),
                ("src/app.mjs", "import { addToCart } from './cart.js';\nexport function buy(x) {\n  return addToCart(x);\n}\n"),
                ("src/view.jsx", "export const Cart = () => <div className=\"cart\" />;\n"),
                ("src/lib/jquery.min.js", "function addToCart(){}\n"),
                ("dist/cart.js", "function addToCart(){}\n"),
                ("src/packed.js", long.as_str()),
            ],
        );
        let scope = Scope { repos: vec![app.clone()], no_sync: false };
        let defs: Vec<String> = find("addToCart", None, 10, &scope).unwrap().data.items.into_iter().map(|(_, h)| h.symbol.file).collect();
        assert_eq!(defs, ["src/cart.js"]);
        assert_eq!(find("Cart", None, 10, &scope).unwrap().data.items.iter().filter(|(_, h)| h.symbol.file == "src/view.jsx").count(), 1);
        let r = refs("addToCart", &[], 10, &scope).unwrap();
        let hit = r.data.items.iter().find(|(_, h)| h.file == "src/app.mjs" && h.kind == "call").unwrap();
        assert_eq!(hit.1.confidence, "exact");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missed_partners_are_code() {
        assert!(is_code_file("src/Pricing.cs"));
        assert!(is_code_file("web/app.component.html"));
        assert!(is_code_file("scripts/build.js"));
        assert!(is_code_file("db/shop.sql"));
        assert!(!is_code_file("AGENTS.md"));
        assert!(!is_code_file("api/cpuinfo"));
        assert!(!is_code_file("docs/notes.txt"));
        assert!(!is_code_file(".gitignore"));
    }

    #[test]
    fn locate_ranks_files_for_a_concept_and_follows_edits() {
        let cache = test_cache();
        let base = cache.dir("locate");
        std::fs::create_dir_all(base.join("shop/src/Refunds")).unwrap();
        std::fs::create_dir_all(base.join("shop/src/Domains")).unwrap();
        let root = repo(
            &base,
            "shop",
            &[
                (
                    "src/Refunds/RefundApprovalService.cs",
                    "public class RefundApprovalService\n{\n    /// <summary>Approves a refund when the order allows it.</summary>\n    public bool Approve(Order order) => order.CanRefund();\n}\n",
                ),
                (
                    "src/Domains/Registry.cs",
                    "public class Registry\n{\n    public void LockForTransfer(Domain domain) => domain.SetTransferLock(true);\n    public void Renew(Domain domain) {}\n}\n",
                ),
                ("src/Orders.cs", "public class Orders\n{\n    public Order Find(int id) => null;\n}\n"),
            ],
        );
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let files = |q: &str| locate(q, 10, &scope).unwrap().data.items.into_iter().map(|(_, h)| h.file).collect::<Vec<_>>();

        let out = locate("refund approvals", 10, &scope).unwrap();
        let top = &out.data.items[0].1;
        assert_eq!(top.file, "src/Refunds/RefundApprovalService.cs");
        assert_eq!(top.terms, ["refund", "approval"]);
        assert_eq!(top.symbols[0].name, "RefundApprovalService");
        assert_eq!(out.env["terms"], json!(["refund", "approval"]));
        assert_eq!(files("where do we lock domains for transfer")[0], "src/Domains/Registry.cs");
        // Only filler words: nothing to look for.
        let none = locate("fix the bug", 10, &scope).unwrap();
        assert!(none.data.items.is_empty() && none.data.terms.is_empty());

        // An edit is seen at the next query; a deleted file drops out.
        std::fs::write(root.join("src/Orders.cs"), "public class Orders\n{\n    public void Lock(Order o) {}\n    public void ScheduleTransfer(Order o) {}\n}\n").unwrap();
        std::fs::remove_file(root.join("src/Refunds/RefundApprovalService.cs")).unwrap();
        assert!(files("lock for transfer").contains(&"src/Orders.cs".to_string()));
        assert!(files("refund approval").is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn impact_classes_test_code_as_test_and_never_nearest() {
        let cache = test_cache();
        let base = cache.dir("tests-class");
        std::fs::create_dir_all(base.join("app/tests/App.Tests")).unwrap();
        let app = repo(
            &base,
            "app",
            &[
                ("src/Pricing.cs", "public class Pricing\n{\n    public int Quote() => 1;\n    public int Debug() => 2;\n}\n"),
                ("src/Checkout.cs", "public class Checkout\n{\n    private readonly Pricing _p;\n    public int Total() => _p.Quote();\n}\n"),
                ("tests/App.Tests/PricingTests.cs", "public class PricingTests\n{\n    private readonly Pricing _p;\n    public void Quotes() => _p.Quote();\n    public void Debugs() => _p.Debug();\n}\n"),
                ("src/pricing.spec.ts", "export function quoteSpec(p: Pricing) {\n  p.Quote();\n}\n"),
            ],
        );
        let classes = |symbol: &str| {
            let r = impact_of(&app, symbol);
            let mut v: Vec<_> = r.impact.affected.iter().map(|a| (a.qualified.clone(), a.class)).collect();
            v.sort();
            (r.nearest(), v)
        };
        let (worst, got) = classes("Pricing.Quote");
        assert_eq!(worst, "direct");
        assert!(got.contains(&("Checkout.Total".to_string(), "direct")), "{got:?}");
        assert!(got.contains(&("PricingTests.Quotes".to_string(), "test")), "{got:?}");
        // Only tests depend on it: listed, but `nearest` stays none.
        let (worst, got) = classes("Pricing.Debug");
        assert_eq!((worst, got), ("none", vec![("PricingTests.Debugs".to_string(), "test")]));
        assert!(kc_store::is_test_path("src/pricing.spec.ts") && kc_store::is_test_path("src/a/b.test.js"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn impact_crosses_from_an_action_to_a_method_requesting_its_url() {
        let cache = test_cache();
        let base = cache.dir("http-call");
        let web = repo(
            &base,
            "shop",
            &[
                ("src/order.service.ts", "export class OrderService {\n  constructor(private http: HttpClient) {}\n  load(id: number) {\n    return this.http.get(`/api/orders/${id}`);\n  }\n}\n"),
                ("src/order.page.ts", "export class OrderPage {\n  constructor(private orders: OrderService) {}\n  open(): void {\n    this.orders.load(1);\n  }\n}\n"),
            ],
        );
        let api = repo(
            &base,
            "orders",
            &[(
                "src/OrdersController.cs",
                "[Route(\"api/orders\")]\npublic class OrdersController : Controller\n{\n    [HttpGet(\"{id}\")]\n    public Task Get(int id) => null;\n}\n",
            )],
        );

        let r = impact_of(&api, "OrdersController.Get");
        let shop = Some("shop".to_string());
        let route = Some("GET /api/orders/{id}".to_string());
        assert_eq!(
            rows(&r.impact),
            [
                (1, shop.clone(), "OrderService.load".to_string(), "http", "exact", route),
                (2, shop.clone(), "OrderPage.open".to_string(), "calls", r.impact.affected[1].confidence, None),
            ]
        );
        assert!(r.impact.affected[0].http.as_ref().unwrap().call);

        let t = trace("OrdersController.Get", "in", 3, 100, &WalkFilter::default(), &Scope { repos: vec![api.clone()], no_sync: false }).unwrap();
        assert_eq!(t.env["items"][0]["nodes"][1]["http"]["call"], true);
        let _ = web;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn context_marks_gateway_dtos_left_over() {
        let cache = test_cache();
        let base = cache.dir("vestige");
        let plan = repo(
            &base,
            "plan",
            &[
                ("src/Ops.PlanApi.Models.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Ops.PlanApi.Models</PackageId></PropertyGroup>\n</Project>\n"),
                (
                    "src/Dtos.cs",
                    "namespace Ops.PlanApi.Models;\n[ShopRequest(\"PlanApi\", nameof(GetPlan), HttpMethods.Get)]\npublic class GetPlan { }\n\
                     [ShopRequest(\"PlanApi\", nameof(ArchivePlan), HttpMethods.Post)]\npublic class ArchivePlan { }\n\
                     [ShopRequest(\"PlanApi\", nameof(RenamePlan), HttpMethods.Post)]\npublic class RenamePlan { }\n",
                ),
                ("src/Startup.cs", "public class Startup(IWebHostEnvironment env) : ShopServiceStartup<S>(\"PlanApi\", 5000, env) { }\n"),
                (
                    "src/PlanController.cs",
                    "public class PlanController : Controller\n{\n    [HttpGet(nameof(Models.GetPlan))]\n    public Task GetPlan() => null;\n\n    //[HttpPost(nameof(Models.ArchivePlan))]\n    //public Task ArchivePlan() => null;\n}\n",
                ),
            ],
        );
        let bff = repo(
            &base,
            "planbff",
            &[
                ("src/Bff.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Ops.PlanApi.Models\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                ("src/PlanService.cs", "using Ops.PlanApi.Models;\npublic class PlanService\n{\n    private readonly IShopGateway _gateway;\n    public Task Load() => _gateway.SendAsync(new GetPlan());\n}\n"),
            ],
        );
        // A frontend requests `RenamePlan`'s route with a contract of its own.
        let web = repo(
            &base,
            "planweb",
            &[("src/contracts.ts", "@RequestContract({ endpoint: 'RenamePlan', httpMethod: HttpMethods.post })\nexport class ApiRenamePlan {}\n")],
        );
        let scope = Scope { repos: vec![plan.clone()], no_sync: false };
        // Nothing creates it, but its route is requested from elsewhere: not unsent.
        let c = context("RenamePlan", 2000, Default::default(), &scope).unwrap();
        assert!(!c.data.found[0].2[0].unsent);
        // Sent from another repo, and served: no marker.
        let c = context("GetPlan", 2000, Default::default(), &scope).unwrap();
        let get = c.data.found.iter().find_map(|(_, ctx, http, ..)| (ctx.symbol.kind == "class").then_some(http)).unwrap();
        assert_eq!(get.len(), 1);
        assert_eq!((get[0].unsent, get[0].commented_out.len(), get[0].links.len()), (false, 0, 1));
        // Never created, and its action commented out.
        let c = context("ArchivePlan", 2000, Default::default(), &scope).unwrap();
        let archive = &c.data.found[0].2;
        assert_eq!(archive.len(), 1);
        assert!(archive[0].unsent && archive[0].links.is_empty());
        let co: Vec<_> = archive[0].commented_out.iter().map(|l| (l.repo.as_str(), l.symbol.as_str(), l.file.as_str(), l.line)).collect();
        assert_eq!(co, [("plan", "PlanController", "src/PlanController.cs", 6)]);
        assert_eq!(c.env["items"][0]["http"][0]["unsent"], true);
        let _ = (bff, web);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn impact_crosses_from_a_service_action_to_the_repo_sending_its_dto() {
        let cache = test_cache();
        let base = cache.dir("gateway");
        let plan = repo(
            &base,
            "mailinglist",
            &[
                ("src/Ops.MailingApi.Models.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Ops.MailingApi.Models</PackageId></PropertyGroup>\n</Project>\n"),
                ("src/ServiceNames.cs", "namespace Ops.MailingApi.Models;\npublic static class ServiceNames { public const string ApiName = \"MailingApi\"; }\n"),
                // A gateway DTO that is a bus message too.
                (
                    "src/DeleteSubscriber.cs",
                    "namespace Ops.MailingApi.Models;\n[ShopMessage(ShopHosts.Mail)]\n[ShopRequest(ServiceNames.ApiName, nameof(DeleteSubscriber), HttpMethods.Delete)]\npublic class DeleteSubscriber { }\n",
                ),
                ("src/Startup.cs", "public class Startup(IWebHostEnvironment env) : ShopServiceStartup<S>(ServiceNames.ApiName, 5000, env) { }\n"),
                ("src/MailingListController.cs", "public class MailingListController : Controller\n{\n    [HttpDelete(nameof(Models.DeleteSubscriber))]\n    public Task DeleteSubscriber() => null;\n}\n"),
            ],
        );
        let bff = repo(
            &base,
            "bff",
            &[
                ("src/Bff.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Ops.MailingApi.Models\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                (
                    "src/MailService.cs",
                    "using EP = Ops.MailingApi.Models;\npublic class MailService\n{\n    private readonly IShopGateway _gateway;\n    public Task Delete() => _gateway.SendAsync(new EP.DeleteSubscriber());\n}\n",
                ),
                ("src/MailController.cs", "public class MailController\n{\n    private readonly MailService _mail;\n    public Task Remove() => _mail.Delete();\n}\n"),
                // Created inside a bus publish: sent on the bus, not over HTTP.
                (
                    "src/MailJob.cs",
                    "using EP = Ops.MailingApi.Models;\npublic class MailJob\n{\n    private readonly IShopBus _bus;\n    public Task Notify() => _bus.Publish(new EP.DeleteSubscriber());\n}\n",
                ),
            ],
        );
        let r = impact_of(&plan, "MailingListController.DeleteSubscriber");
        let got: Vec<_> = rows(&r.impact).into_iter().map(|(hop, repo, q, edge, _, route)| (hop, repo, q, edge, route)).collect();
        let bff_name = Some("bff".to_string());
        assert_eq!(
            got,
            [
                (1, bff_name.clone(), "MailService.Delete".to_string(), "http", Some("DELETE /DeleteSubscriber".to_string())),
                (2, bff_name.clone(), "MailController.Remove".to_string(), "calls", None),
            ]
        );
        assert_eq!(r.impact.affected[0].http.as_ref().unwrap().link, "exact");
        // Nor does a flow from the publisher make an HTTP request.
        let opts = crate::flow::FlowOptions { depth: 5, max_steps: 50, max_repos: 8, edges: Default::default(), children: crate::flow::DEFAULT_CHILDREN };
        let f = crate::flow::flow("MailJob.Notify", &opts, &Scope { repos: vec![bff.clone()], no_sync: false }).unwrap().data;
        assert!(f.iter().flat_map(|f| &f.steps).all(|s| s.step.edge != "http"), "{f:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn impact_on_an_interface_overload_reaches_its_implementation_and_dependents_callers() {
        let cache = test_cache();
        let base = cache.dir("iface-x");
        let lib = repo(
            &base,
            "lib",
            &[
                ("src/Pay.Gateway.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Pay.Gateway</PackageId></PropertyGroup>\n</Project>\n"),
                ("src/IPayClient.cs", "namespace Pay.Gateway;\npublic interface IPayClient\n{\n    int Submit(int amount);\n    int Submit(int amount, int plan);\n}\n"),
                (
                    "src/PayClient.cs",
                    "namespace Pay.Gateway;\npublic class PayClient : IPayClient\n{\n    public int Submit(int amount) => amount;\n    public int Submit(int amount, int plan) => plan;\n}\n",
                ),
            ],
        );
        let app = repo(
            &base,
            "app",
            &[
                ("src/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Pay.Gateway\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                ("src/Checkout.cs", "using Pay.Gateway;\nnamespace App;\npublic class Checkout\n{\n    private readonly IPayClient _pay;\n    public int Run() => _pay.Submit(5);\n    public int Plan() => _pay.Submit(5, 2);\n}\n"),
            ],
        );
        let p = ImpactParams { symbol: Some("src/IPayClient.cs:4".into()), depth: 1, limit: 100, ..Default::default() };
        let out = impact(&p, &Scope { repos: vec![lib.clone()], no_sync: false }).unwrap();
        let r = &out.data[0];
        let implementations: Vec<(String, u32)> = r.impact.affected.iter().map(|a| (a.qualified.clone(), a.line)).collect();
        assert_eq!(implementations, [("PayClient.Submit".to_string(), 4)], "the one-argument overload's implementation only");
        let cross: Vec<(Option<&str>, u32)> = r.cross_repo.iter().map(|h| (h.repo.as_deref(), h.line)).collect();
        assert_eq!(cross, [(Some("app"), 6)], "the dependent's call through the interface, by arity");
        let _ = app;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn bus_messages_link_publishers_to_consumers_across_repos() {
        let cache = test_cache();
        let base = cache.dir("bus");
        let ssl = repo(
            &base,
            "ssl",
            &[
                ("src/Shop.Ssl.Models.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Shop.Ssl.Models</PackageId></PropertyGroup>\n</Project>\n"),
                ("src/PricesChanged.cs", "namespace Shop.Ssl.Models.Events;\n[ShopMessage(ShopHosts.Catalog)]\npublic class PricesChanged { }\n"),
                (
                    "src/SslService.cs",
                    "using Shop.Ssl.Models.Events;\nnamespace Shop.Ssl.Service;\npublic class SslService\n{\n    private readonly IShopBus _serviceBus;\n    public async Task LoadProducts()\n    {\n        await _serviceBus.Publish(new PricesChanged());\n    }\n    public int Count() => 0;\n}\n",
                ),
                ("src/LoadJob.cs", "public class LoadJob\n{\n    private readonly SslService _ssl;\n    public Task Run() => _ssl.LoadProducts();\n}\n"),
            ],
        );
        let catalog = repo(
            &base,
            "catalog",
            &[
                ("src/Catalog.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Shop.Ssl.Models\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                (
                    "src/PricesChangedConsumer.cs",
                    "using Shop.Ssl.Models.Events;\nnamespace Shop.Catalog.Consumers;\npublic class PricesChangedConsumer : ShopConsumer<PricesChanged, PricesChangedConsumer>\n{\n    public override Task Consume(PricesChanged message, ConsumeContext<PricesChanged> context) => null;\n}\n",
                ),
                // No `using`: a global one. The only `PricesChanged` in the repos it depends on.
                ("src/RefreshCacheConsumer.cs", "namespace Shop.Catalog.Consumers;\npublic class RefreshCacheConsumer : IConsumer<PricesChanged>\n{\n    public Task Consume(ConsumeContext<PricesChanged> context) => null;\n}\n"),
            ],
        );
        let catalog_name = Some("catalog".to_string());
        let r = impact_of(&ssl, "SslService.LoadProducts");
        let got: Vec<_> = r.impact.affected.iter().map(|a| (a.hop, a.repo.clone(), a.qualified.clone(), a.edge, a.bus.as_ref().map(|b| b.link))).collect();
        assert_eq!(
            got,
            [
                (1, None, "LoadJob.Run".to_string(), "calls", None),
                (1, catalog_name.clone(), "PricesChangedConsumer.Consume".to_string(), "bus", Some("exact")),
                (1, catalog_name.clone(), "RefreshCacheConsumer.Consume".to_string(), "bus", Some("exact")),
            ]
        );
        let via = r.impact.affected[1].bus.as_ref().unwrap();
        assert_eq!((via.message.as_str(), via.queue.as_deref()), ("Shop.Ssl.Models.Events.PricesChanged", Some("Catalog")));
        // A method that doesn't publish crosses nothing.
        assert!(impact_of(&ssl, "SslService.Count").impact.affected.is_empty());

        // Inward from the message type: its publisher in the repo, and the consumers across the bus.
        let t = trace("PricesChanged", "in", 2, 100, &WalkFilter::default(), &Scope { repos: vec![ssl.clone()], no_sync: false }).unwrap();
        let nodes: Vec<_> = t.data.found[0].1.nodes.iter().map(|n| (n.depth, n.repo.clone(), n.qualified.clone(), n.edge)).collect();
        assert_eq!(
            nodes,
            [
                (0, None, "PricesChanged".to_string(), ""),
                (1, None, "SslService.LoadProducts".to_string(), "instantiates"),
                (2, None, "LoadJob.Run".to_string(), "calls"),
                (1, catalog_name.clone(), "PricesChangedConsumer.Consume".to_string(), "bus"),
                (1, catalog_name.clone(), "RefreshCacheConsumer.Consume".to_string(), "bus"),
            ]
        );
        // From the publishing method, its consumers run after it: out, not among its callers. Each hop is at the
        // publish call, in the publisher's repo; the node is the consumer's `Consume`.
        let both = |dir: &str| {
            let t = trace("SslService.LoadProducts", dir, 1, 100, &WalkFilter::default(), &Scope { repos: vec![ssl.clone()], no_sync: false }).unwrap();
            let bus = t.data.found[0]
                .1
                .nodes
                .iter()
                .filter(|n| n.edge == "bus")
                .map(|n| (n.direction, n.qualified.clone(), format!("{}:{}", n.file, n.line), n.site.clone(), n.site_repo.clone()))
                .collect::<Vec<_>>();
            (bus, crate::render::trace_text(&ssl, &t.data.found[0].1, false))
        };
        assert!(both("in").0.is_empty());
        let (bus, text) = both("out");
        let at = |consumer: &str, line: &str| {
            ("out", consumer.to_string(), line.to_string(), Some("src/SslService.cs:8".to_string()), Some("ssl".to_string()))
        };
        assert_eq!(
            bus,
            [
                at("PricesChangedConsumer.Consume", "src/PricesChangedConsumer.cs:5"),
                at("RefreshCacheConsumer.Consume", "src/RefreshCacheConsumer.cs:4"),
            ]
        );
        assert!(text.contains("PricesChangedConsumer.Consume  catalog/src/PricesChangedConsumer.cs:5  [exact] at ssl/src/SslService.cs:8"), "{text}");
        // From the message type, the consumer's own declaration is where the hop lands.
        let t = trace("PricesChanged", "in", 1, 100, &WalkFilter::default(), &Scope { repos: vec![ssl.clone()], no_sync: false }).unwrap();
        let consumer = t.data.found[0].1.nodes.iter().find(|n| n.edge == "bus").unwrap();
        assert_eq!((consumer.site.as_deref(), consumer.site_repo.as_deref()), (Some("src/PricesChangedConsumer.cs:5"), None));

        // Context, both ways.
        let c = context("PricesChanged", 4000, Default::default(), &Scope { repos: vec![ssl.clone()], no_sync: false }).unwrap();
        let bus = &c.env["items"][0]["bus"][0];
        assert_eq!((bus["role"].as_str(), bus["message"].as_str()), (Some("message"), Some("Shop.Ssl.Models.Events.PricesChanged")));
        let links: Vec<_> = bus["links"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| (l["role"].as_str().unwrap(), l["symbol"].as_str().unwrap(), l["repo"].as_str().unwrap(), l["line"].as_u64().unwrap()))
            .collect();
        // A consumer's line is its `Consume` method's, as in trace and impact; a publisher's is the publish call.
        assert_eq!(
            links,
            [
                ("consumer", "PricesChangedConsumer.Consume", "catalog", 5),
                ("consumer", "RefreshCacheConsumer.Consume", "catalog", 4),
                ("publisher", "SslService.LoadProducts", "ssl", 8),
            ]
        );
        let c = context("PricesChangedConsumer.Consume", 4000, Default::default(), &Scope { repos: vec![catalog.clone()], no_sync: false }).unwrap();
        let bus = &c.env["items"][0]["bus"][0];
        assert_eq!((bus["role"].as_str(), bus["links"][0]["symbol"].as_str()), (Some("consumes"), Some("SslService.LoadProducts")));

        // The listing.
        let e = events(&Scope { repos: vec![catalog.clone()], no_sync: false }).unwrap();
        let rows: Vec<_> = e.data.iter().map(|(_, b)| (b.role, b.symbol.as_str(), b.resolution, b.links.len())).collect();
        assert_eq!(rows, [("consumes", "PricesChangedConsumer", "exact", 1), ("consumes", "RefreshCacheConsumer", "exact", 1)]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn bus_helpers_sagas_and_registered_consumers_link() {
        let cache = test_cache();
        let base = cache.dir("bus-helpers");
        let accounts = repo(
            &base,
            "accounts",
            &[
                ("src/AccountLocked.cs", "namespace Shop.Accounts.Events;\npublic class AccountLocked { }\npublic class WelcomeEmail { }\n"),
                (
                    "src/AccountController.cs",
                    "using Shop.Accounts.Events;\nnamespace Shop.Accounts;\npublic class AccountController\n{\n    private readonly IPublishEndpoint publishEndpoint;\n    public Task Lock() => TryPublish(new AccountLocked(), \"lock\");\n    private async Task TryPublish<T>(T message, string context) where T : class\n    {\n        try { await publishEndpoint.Publish(message); } catch { }\n    }\n    public Task Welcome() => publishEndpoint.Publish(new WelcomeEmail());\n}\n",
                ),
            ],
        );
        let audit = repo(
            &base,
            "audit",
            &[
                (
                    "src/LockStateMachine.cs",
                    "using Shop.Accounts.Events;\nnamespace Shop.Audit;\npublic class LockStateMachine : MassTransitStateMachine<LockState>\n{\n    public Event<AccountLocked> Locked { get; private set; }\n}\n",
                ),
                (
                    "src/Startup.cs",
                    "using Shop.Accounts.Events;\nnamespace Shop.Audit;\npublic class Startup\n{\n    public void Configure(IBusRegistrationConfigurator cfg)\n    {\n        cfg.AddMailConsumer<WelcomeEmail>();\n    }\n}\n",
                ),
            ],
        );
        // The publish is the helper's caller's, linked to the state machine; the helper's own untyped publish is dropped.
        let e = events(&Scope { repos: vec![accounts.clone()], no_sync: false }).unwrap();
        let rows: Vec<_> = e
            .data
            .iter()
            .map(|(_, b)| (b.symbol.as_str(), b.message.as_str(), b.via.as_deref(), b.line, b.links.iter().map(|l| (l.symbol.as_str(), l.line)).collect::<Vec<_>>()))
            .collect();
        assert_eq!(
            rows,
            [
                ("AccountController.Lock", "AccountLocked", Some("TryPublish"), 6, vec![("LockStateMachine", 5)]),
                ("AccountController.Welcome", "WelcomeEmail", Some("Publish"), 11, vec![("Startup.Configure", 7)]),
            ]
        );
        // A registration runs no code of its own: impact and trace stop at the state machine.
        let consumers = |sym: &str| -> Vec<String> {
            impact_of(&accounts, sym).impact.affected.iter().filter(|a| a.edge == "bus").map(|a| a.qualified.clone()).collect()
        };
        assert_eq!(consumers("AccountController.Lock"), ["LockStateMachine"]);
        assert!(consumers("AccountController.Welcome").is_empty());
        let e = events(&Scope { repos: vec![audit.clone()], no_sync: false }).unwrap();
        let rows: Vec<_> = e.data.iter().map(|(_, b)| (b.role, b.symbol.as_str(), b.message.as_str(), b.via.as_deref(), b.line)).collect();
        assert_eq!(
            rows,
            [
                ("consumes", "LockStateMachine", "AccountLocked", Some("MassTransitStateMachine"), 5),
                ("consumes", "Startup.Configure", "WelcomeEmail", Some("AddMailConsumer"), 7),
            ]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn context_lists_callers_through_the_interface_or_base_member() {
        let cache = test_cache();
        let base = cache.dir("through");
        let root = repo(
            &base,
            "shop",
            &[
                ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/IStore.cs", "public interface IStore { void Save(int id); }\n"),
                ("src/SqlStore.cs", "public class SqlStore : IStore\n{\n    public void Save(int id) { }\n}\n"),
                ("src/Orders.cs", "public class Orders\n{\n    private readonly IStore _store;\n    public void Place() => _store.Save(1);\n}\n"),
                (
                    "src/Job.cs",
                    "public abstract class Job\n{\n    public void Run() => Step();\n    protected virtual void Step() { }\n}\npublic class Nightly : Job\n{\n    protected override void Step() { base.Step(); }\n}\n",
                ),
                // Overloads of one arity, told apart by their parameter types.
                (
                    "src/Repo.cs",
                    "public class Order { }\npublic class Customer { }\npublic interface IRepo\n{\n    void Put(Order order);\n    void Put(Customer customer);\n}\npublic class Repo : IRepo\n{\n    public void Put(Order order) { }\n    public void Put(Customer customer) { }\n}\npublic class Service\n{\n    private readonly IRepo _repo;\n    public void PlaceOrder() { var order = new Order(); _repo.Put(order); }\n    public void Register() { var customer = new Customer(); _repo.Put(customer); }\n    public void Both(Order o) { _repo?.Put(o); }\n}\n",
                ),
            ],
        );
        let callers = |sym: &str| -> Vec<(String, u64, String, String)> {
            let c = context(sym, 4000, Default::default(), &Scope { repos: vec![root.clone()], no_sync: false }).unwrap();
            c.env["items"][0]["callers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| {
                    let s = |k: &str| h[k].as_str().unwrap_or_default().to_string();
                    (s("file"), h["line"].as_u64().unwrap(), s("through"), s("confidence"))
                })
                .collect()
        };
        assert_eq!(callers("SqlStore.Save"), [("src/Orders.cs".to_string(), 4, "IStore".to_string(), "exact".to_string())]);
        // The base calls its virtual member; the override's own `base.Step()` isn't a call of the override.
        assert_eq!(callers("Nightly.Step"), [("src/Job.cs".to_string(), 3, "Job".to_string(), "ambiguous".to_string())]);
        // Each overload's implementation gets only the calls passing its parameter's type.
        let at = |line: u64| ("src/Repo.cs".to_string(), line, "IRepo".to_string(), "exact".to_string());
        assert_eq!(callers("src/Repo.cs:10"), [at(16), at(18)]);
        assert_eq!(callers("src/Repo.cs:11"), [at(17)]);
        // A test double beside the one real implementation leaves calls through the interface its own.
        let root2 = repo(
            &base,
            "mail",
            &[
                ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/IMailer.cs", "public interface IMailer { void Send(); }\n"),
                ("src/SmtpMailer.cs", "public class SmtpMailer : IMailer\n{\n    public void Send() { }\n}\n"),
                ("src/Signup.cs", "public class Signup\n{\n    private readonly IMailer _mailer;\n    public void Run() => _mailer.Send();\n}\n"),
                ("tests/App.Tests/FakeMailer.cs", "public class FakeMailer : IMailer\n{\n    public void Send() { }\n}\n"),
            ],
        );
        let c = context("SmtpMailer.Send", 4000, Default::default(), &Scope { repos: vec![root2.clone()], no_sync: false }).unwrap();
        let h = &c.env["items"][0]["callers"][0];
        assert_eq!((h["file"].as_str(), h["through"].as_str(), h["confidence"].as_str()), (Some("src/Signup.cs"), Some("IMailer"), Some("exact")));
        // Outside test paths, a double is one by name only when it names what it stands in for: `MockAddressParser`
        // beside `AddressParser` is one, `StubHillClient` (a ticket client) beside `BoxOfficeClient` is not.
        let root3 = repo(
            &base,
            "tickets",
            &[
                ("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/ITicketClient.cs", "public interface ITicketClient { void Buy(); }\n"),
                ("src/StubHillClient.cs", "public class StubHillClient : ITicketClient\n{\n    public void Buy() { }\n}\n"),
                ("src/BoxOfficeClient.cs", "public class BoxOfficeClient : ITicketClient\n{\n    public void Buy() { }\n}\n"),
                ("src/Checkout.cs", "public class Checkout\n{\n    private readonly ITicketClient _client;\n    public void Run() => _client.Buy();\n}\n"),
                ("src/IParser.cs", "public interface IParser { string Parse(); }\n"),
                ("src/AddressParser.cs", "public class AddressParser : IParser\n{\n    public string Parse() => \"\";\n}\n"),
                ("src/MockAddressParser.cs", "public class MockAddressParser : IParser\n{\n    public string Parse() => \"\";\n}\n"),
                ("src/Gate.cs", "public class Gate\n{\n    private readonly IParser _ip;\n    public string Run() => _ip.Parse();\n}\n"),
            ],
        );
        let through = |sym: &str| {
            let c = context(sym, 4000, Default::default(), &Scope { repos: vec![root3.clone()], no_sync: false }).unwrap();
            let h = &c.env["items"][0]["callers"][0];
            (h["file"].as_str().map(str::to_string), h["confidence"].as_str().map(str::to_string))
        };
        assert_eq!(through("BoxOfficeClient.Buy"), (Some("src/Checkout.cs".into()), Some("ambiguous".into())));
        assert_eq!(through("AddressParser.Parse"), (Some("src/Gate.cs".into()), Some("exact".into())));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_dependencys_base_and_interface_reach_their_overrides_and_implementations_in_other_repos() {
        let cache = test_cache();
        let base = cache.dir("xrepo-impl");
        let lib = repo(
            &base,
            "lib",
            &[
                ("src/Hosting.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><AssemblyName>Acme.Hosting</AssemblyName></PropertyGroup>\n</Project>\n"),
                (
                    "src/StartupBase.cs",
                    "namespace Acme.Hosting;\npublic abstract class StartupBase\n{\n    public void Configure() { RegisterServices(1); }\n    protected abstract void RegisterServices(int n);\n}\npublic abstract class StartupBase<T> : StartupBase { }\n",
                ),
                ("src/IClock.cs", "namespace Acme.Hosting;\npublic interface IClock { int Now(); }\n"),
            ],
        );
        let app = repo(
            &base,
            "app",
            &[
                ("src/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Acme.Hosting\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                (
                    "src/Startup.cs",
                    "using Acme.Hosting;\nnamespace App;\npublic class Settings { }\npublic class Startup : StartupBase<Settings>\n{\n    protected override void RegisterServices(int n) { }\n}\n",
                ),
                ("src/Clock.cs", "using Acme.Hosting;\nnamespace App;\npublic class Clock : IClock\n{\n    public int Now() => 1;\n}\n"),
                // Another interface of the name, of another namespace: not the library's.
                ("src/Other.cs", "namespace App.Other;\npublic interface IClock { }\npublic class OtherClock : IClock { }\n"),
            ],
        );
        let scope = |r: &PathBuf| Scope { repos: vec![r.clone()], no_sync: false };
        // The base class calls its abstract member: a caller of the override, through the base, in the library.
        let c = context("Startup.RegisterServices", 4000, Default::default(), &scope(&app)).unwrap();
        let h = &c.env["items"][0]["callers"][0];
        let got = (h["repo"].as_str(), h["file"].as_str(), h["line"].as_u64(), h["through"].as_str());
        assert_eq!(got, (Some("lib"), Some("src/StartupBase.cs"), Some(4), Some("StartupBase")));
        // The library's interface: its implementation in the repo using it, by namespace.
        let c = context("IClock", 4000, Default::default(), &scope(&lib)).unwrap();
        let found: Vec<(&str, &str)> = c.env["items"][0]["conformers_elsewhere"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["repo"].as_str().unwrap(), e["name"].as_str().unwrap()))
            .collect();
        assert_eq!(found, [("app", "Clock")]);
        // Impact reaches it too, and the implementing member of an interface member.
        let p = ImpactParams { symbol: Some("IClock".into()), depth: 1, limit: 100, ..Default::default() };
        let r = &impact(&p, &scope(&lib)).unwrap().data[0];
        let cross: Vec<(Option<&str>, &str, u32)> = r.cross_repo.iter().map(|h| (h.repo.as_deref(), h.kind.as_str(), h.line)).collect();
        assert_eq!(cross, [(Some("app"), "inherits", 3)]);
        let p = ImpactParams { symbol: Some("IClock.Now".into()), depth: 1, limit: 100, ..Default::default() };
        let r = &impact(&p, &scope(&lib)).unwrap().data[0];
        let cross: Vec<(Option<&str>, &str, u32)> = r.cross_repo.iter().map(|h| (h.repo.as_deref(), h.kind.as_str(), h.line)).collect();
        assert_eq!(cross, [(Some("app"), "implements", 5)]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn sql_tables_and_procedures_link_to_the_code_using_them_across_repos() {
        let cache = test_cache();
        let base = cache.dir("sql");
        let schema = repo(
            &base,
            "schema",
            &[(
                "src/shop.sql",
                "-- Host: db1    Database: shop\n\
CREATE TABLE `orders` (\n  `id` int NOT NULL,\n  `total` decimal(10,2),\n  PRIMARY KEY (`id`)\n);\n\
CREATE TABLE `customers` (`id` int);\n\
DELIMITER ;;\n\
CREATE PROCEDURE `place_order`(p int)\nBEGIN\n  INSERT INTO orders (id) VALUES (p);\n  SELECT * FROM customers;\nEND ;;\n\
CREATE PROCEDURE `nightly`()\nBEGIN\n  CALL place_order(1);\nEND ;;\n\
DELIMITER ;\n\
CREATE VIEW big_orders AS SELECT * FROM orders WHERE total > 100;\n",
            )],
        );
        let api = repo(
            &base,
            "api",
            &[(
                "src/Orders.cs",
                "public class Order { }\npublic class ShopContext : DbContext\n{\n    public DbSet<Order> Orders { get; set; }\n}\n\
public class OrderStore\n{\n    private readonly ShopContext _db;\n    private readonly IDbConnection _conn;\n\
    public void Save(Order o)\n    {\n        _db.Orders.Add(o);\n        _db.SaveChanges();\n    }\n\
    public void Place(int id)\n    {\n        _conn.Execute(\"CALL place_order(@id)\", new { id });\n    }\n}\n\
public class OrderService\n{\n    private readonly OrderStore _store;\n    public void Checkout()\n    {\n        _store.Place(1);\n    }\n}\n",
            )],
        );
        let scope = |r: &PathBuf| Scope { repos: vec![r.clone()], no_sync: false };
        let api_name = Some("api".to_string());

        // Changing the table reaches the routines and views using it, the code reading and writing its entity, and
        // the code calling those routines, in whichever repo.
        let r = impact_of(&schema, "orders");
        assert_eq!(
            rows(&r.impact),
            [
                (1, None, "shop.place_order".to_string(), "writes", "exact", None),
                (1, None, "shop.big_orders".to_string(), "reads", "exact", None),
                // Ranked by hop: the code across the link comes before the next hop's routines. The table is matched by
                // its name across repos: `unique_name` (evidence `sql_name`), as every walk labels such a link.
                (1, api_name.clone(), "OrderStore.Save".to_string(), "sql", "unique_name", None),
                (2, None, "shop.nightly".to_string(), "calls", "exact", None),
                (2, api_name.clone(), "OrderStore.Place".to_string(), "sql", "unique_name", None),
                (3, api_name.clone(), "OrderService.Checkout".to_string(), "calls", r.impact.affected[5].confidence, None),
            ]
        );
        let via = r.impact.affected[2].sql.as_ref().unwrap();
        assert_eq!((via.how, via.entity.as_deref(), via.mode.as_deref(), via.link), ("entity", Some("Order"), Some("write"), "unique_name"));

        // Trace inward from the procedure crosses into the code calling it.
        let t = trace("shop.place_order", "in", 3, 100, &WalkFilter::default(), &scope(&schema)).unwrap();
        let nodes: Vec<_> = t.data.found[0].1.nodes.iter().map(|n| (n.depth, n.repo.clone(), n.qualified.clone(), n.edge)).collect();
        assert_eq!(
            nodes,
            [
                (0, None, "shop.place_order".to_string(), ""),
                (1, None, "shop.nightly".to_string(), "calls"),
                (1, api_name.clone(), "OrderStore.Place".to_string(), "sql"),
                (2, api_name.clone(), "OrderService.Checkout".to_string(), "calls"),
            ]
        );

        // Context on the procedure: what it reads, writes and who calls it; on the table: its columns and entity.
        let c = context("place_order", 4000, Default::default(), &scope(&schema)).unwrap();
        let sql = &c.env["items"][0]["sql"];
        assert_eq!((sql["reads"][0].as_str(), sql["writes"][0].as_str()), (Some("customers"), Some("orders")));
        assert_eq!((sql["used_by"][0]["how"].as_str(), sql["used_by"][0]["symbol"].as_str()), (Some("call"), Some("OrderStore.Place")));
        assert_eq!(c.env["items"][0]["callers"][0]["from"]["name"], "nightly");
        let c = context("orders", 4000, Default::default(), &scope(&schema)).unwrap();
        let sql = &c.env["items"][0]["sql"];
        let columns: Vec<_> = sql["columns"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(columns, ["id", "total"]);
        assert_eq!((sql["used_by"][0]["symbol"].as_str(), sql["used_by"][0]["writes"].as_u64()), (Some("Order"), Some(1)));

        // The code side: tables and procedure calls name their definitions.
        let t = tables(None, &scope(&api)).unwrap();
        let def = &t.env["items"][0]["sql"][0];
        assert_eq!((def["repo"].as_str(), def["name"].as_str(), def["confidence"].as_str()), (Some("schema"), Some("shop.orders"), Some("unique_name")));
        assert_eq!(t.env["procedures"][0]["definition"][0]["name"], "shop.place_order");

        // References to the table, from SQL.
        let r = refs("Orders", &[], 50, &scope(&schema)).unwrap();
        let got: Vec<_> = r.data.items.iter().map(|(_, h)| (h.kind.as_str(), h.from.as_ref().map(|f| f.name.clone()), h.confidence)).collect();
        assert_eq!(got, [("write", Some("place_order".to_string()), "exact"), ("read", Some("big_orders".to_string()), "exact")]);

        // `tables` with a name finds SQL objects defined in .sql files: what a routine's body touches, and the SQL using
        // a table.
        let t = tables(Some("place_order"), &scope(&schema)).unwrap();
        let p = &t.env["sql"][0];
        assert_eq!((p["kind"].as_str(), p["name"].as_str(), p["writes"][0].as_str(), p["reads"][0].as_str()), (Some("procedure"), Some("shop.place_order"), Some("orders"), Some("customers")));
        assert_eq!((p["used_by"][0]["mode"].as_str(), p["used_by"][0]["by"].as_str()), (Some("call"), Some("shop.nightly")));
        let t = tables(Some("orders"), &scope(&schema)).unwrap();
        let used: Vec<_> = t.env["sql"][0]["used_by"].as_array().unwrap().iter().map(|u| (u["mode"].as_str().unwrap(), u["by"].as_str().unwrap())).collect();
        assert_eq!(used, [("write", "shop.place_order"), ("read", "shop.big_orders")]);

        // Context on a procedure the API calls but the schema repo defines: its definition there.
        let c = context("place_order", 4000, Default::default(), &scope(&api)).unwrap();
        let item = &c.env["items"][0];
        assert_eq!((item["repo"].as_str(), item["symbol"]["kind"].as_str(), item["symbol"]["file"].as_str()), (Some("schema"), Some("procedure"), Some("src/shop.sql")));
        assert_eq!(item["sql"]["used_by"][0]["symbol"], "OrderStore.Place");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn same_named_tables_in_several_databases() {
        let cache = test_cache();
        let base = cache.dir("sql-databases");
        let schema = repo(
            &base,
            "dumps",
            &[
                ("books.sql", "-- Host: db1    Database: books\nCREATE TABLE `ledgers` (`id` int);\nCREATE TABLE `payees` (`id` int);\n"),
                ("vault.sql", "-- Host: db1    Database: vault\nCREATE TABLE `ledgers` (`id` int);\n"),
            ],
        );
        let api = repo(
            &base,
            "api",
            &[(
                "src/Ledgers.cs",
                "public class Ledger { }\npublic class Payee { }\n\
public class VaultContext : DbContext\n{\n    public DbSet<Ledger> Ledgers { get; set; }\n    public DbSet<Payee> Payees { get; set; }\n}\n\
public class Startup\n{\n    public void Configure(IServiceCollection s) => s.AddDbContext<VaultContext>(o => o.UseMySql(AppSettings.VaultDbConnection, v));\n}\n\
public class LedgerStore\n{\n    private readonly VaultContext _db;\n    private readonly IDbConnection _conn;\n\
    public Ledger Find(int id) => _db.Ledgers.Find(id);\n\
    public int Live() => _conn.QueryFirst<int>(\"SELECT COUNT(*) FROM books.ledgers WHERE id > @id\", new { id = 1 });\n}\n",
            )],
        );
        let scope = |r: &PathBuf| Scope { repos: vec![r.clone()], no_sync: false };
        // The context's connection names `vault`, though `books` defines more of its tables; the SQL text names `books`.
        let t = tables(Some("ledgers"), &scope(&api)).unwrap();
        let got: Vec<_> = t.env["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                let d = &i["database"];
                (i["table"].as_str().unwrap(), d["name"].as_str(), d["by"].as_str(), d["same_name_in"][0].as_str())
            })
            .collect();
        assert_eq!(got, [("books.ledgers", Some("books"), Some("schema"), Some("vault")), ("Ledgers", Some("vault"), Some("connection"), Some("books"))]);
        // Impact of each definition reaches only the code using that database.
        let users = |db: &str| -> Vec<String> {
            let r = impact_of(&schema, &format!("{db}.ledgers"));
            r.impact.affected.iter().filter(|a| a.repo.is_some()).map(|a| a.qualified.clone()).collect()
        };
        assert_eq!(users("vault"), ["LedgerStore.Find"]);
        assert_eq!(users("books"), ["LedgerStore.Live"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn sql_copies_contexts_and_procedure_wrappers() {
        let cache = test_cache();
        let base = cache.dir("sql-copies");
        for d in ["dumps/src/Warehouse", "dumps/src/main", "svc/src/db"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        let store = |engine: &str| {
            format!(
                "-- Host: db1    Database: store\nCREATE TABLE `purchases` (`id` int NOT NULL) ENGINE={engine};\n\
DELIMITER ;;\nCREATE PROCEDURE `place_purchase`(p int)\nBEGIN\n  INSERT INTO purchases (id) VALUES (p);\nEND ;;\nDELIMITER ;\n"
            )
        };
        let (warehouse, main) = (store("BLACKHOLE"), store("InnoDB"));
        let dumps = repo(
            &base,
            "dumps",
            &[
                // The warehouse replica sorts first by path.
                ("src/Warehouse/store.sql", &warehouse),
                ("src/main/store.sql", &main),
                // A dump's view part, with no header: its schema comes from the file name.
                ("src/main/store.Views.sql", "CREATE OR REPLACE VIEW big_purchases AS SELECT * FROM purchases;\n"),
                ("src/main/invoicing.sql", "-- Host: db1    Database: invoicing\nCREATE TABLE history (id int, action_type int);\n"),
                ("src/main/journal2.sql", "-- Host: db1    Database: journal2\nCREATE TABLE history (id int);\nCREATE TABLE entries (id int);\n"),
            ],
        );
        let api = repo(
            &base,
            "api",
            &[(
                "src/Data.cs",
                "public class History { }\npublic class Entry { }\n\
public class InvoicingContext : DbContext\n{\n    public DbSet<History> Histories { get; set; }\n\
    protected override void OnModelCreating(ModelBuilder b)\n    {\n        b.Entity<History>(e => e.ToTable(\"history\"));\n    }\n}\n\
public class Journal2Context : DbContext\n{\n    public DbSet<Entry> Entries { get; set; }\n    public DbSet<History> Histories { get; set; }\n\
    protected override void OnModelCreating(ModelBuilder b)\n    {\n        b.Entity<Entry>(e => e.ToTable(\"entries\"));\n        b.Entity<History>(e => e.ToTable(\"history\"));\n    }\n}\n\
public class Saver\n{\n    public async Task Save()\n    {\n        await ExecuteStoredProcedureAsync(\"place_purchase\", null);\n        await ExecuteStoredProcedureAsync(\"not_a_proc\", null);\n    }\n\
    private Task ExecuteStoredProcedureAsync(string name, object args) => null;\n}\n",
            )],
        );
        let svc = repo(
            &base,
            "svc",
            &[
                ("src/db/store.sql", "-- Host: db1    Database: store\nCREATE TABLE `purchases` (`id` int NOT NULL);\n"),
                (
                    "src/Orders.cs",
                    "public class Purchase { }\npublic class StoreContext : DbContext\n{\n    public DbSet<Purchase> Purchases { get; set; }\n}\n\
public class PurchaseStore\n{\n    private readonly StoreContext _db;\n    public void Save(Purchase o)\n    {\n        _db.Purchases.Add(o);\n    }\n}\n",
                ),
            ],
        );
        let scope = |r: &PathBuf| Scope { repos: vec![r.clone()], no_sync: false };

        // Each DbContext links to the schema it is named for, though the repo as a whole uses `journal2` more; the
        // representative copy of `store` is the one that stores data; a name passed to a procedure wrapper links when
        // the procedure exists.
        let t = tables(None, &scope(&api)).unwrap();
        let links: Vec<(String, String, String)> = t.env["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
                (s(&i["context"]), s(&i["sql"][0]["name"]), s(&i["sql"][0]["confidence"]))
            })
            .collect();
        assert!(links.contains(&("InvoicingContext".into(), "invoicing.history".into(), "dominant".into())), "{links:?}");
        assert!(links.contains(&("Journal2Context".into(), "journal2.history".into(), "dominant".into())), "{links:?}");
        let procs = t.env["procedures"].as_array().unwrap();
        assert_eq!(procs.len(), 1, "only the defined procedure: {procs:?}");
        assert_eq!((procs[0]["name"].as_str(), procs[0]["definition"][0]["file"].as_str()), (Some("place_purchase"), Some("src/main/store.sql")));
        assert_eq!(procs[0]["definition"][0]["copies"], 1);

        // The repo with its own copy of `store.purchases` links to it.
        let t = tables(None, &scope(&svc)).unwrap();
        let own = &t.env["items"][0]["sql"][0];
        assert_eq!((own["repo"].as_str(), own["confidence"].as_str()), (Some("svc"), Some("exact")));

        // Impact on `store.purchases` lists the procedure once (with its other copy), reads the view (whose schema came
        // from its file name), and reaches code linked to another repo's copy and code calling through a wrapper.
        let r = impact_of(&dumps, "purchases");
        let got: Vec<(u8, Option<String>, String, &str)> = r.impact.affected.iter().map(|a| (a.hop, a.repo.clone(), a.qualified.clone(), a.edge)).collect();
        assert_eq!(got.iter().filter(|g| g.2 == "store.place_purchase").count(), 1, "{got:?}");
        let place = r.impact.affected.iter().find(|a| a.qualified == "store.place_purchase").unwrap();
        assert_eq!((place.file.as_str(), place.copies.clone()), ("src/main/store.sql", vec!["src/Warehouse/store.sql:4".to_string()]));
        assert!(got.contains(&(1, None, "store.big_purchases".to_string(), "reads")), "{got:?}");
        assert!(got.contains(&(1, Some("svc".to_string()), "PurchaseStore.Save".to_string(), "sql")), "{got:?}");
        assert!(got.contains(&(2, Some("api".to_string()), "Saver.Save".to_string(), "sql")), "{got:?}");
        assert_eq!(r.impact.counts.get("direct"), Some(&3), "the collapsed copy isn't counted twice: {got:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn grep_ast_matches_syntax_across_repos_and_sees_edits() {
        let cache = test_cache();
        let base = cache.dir("grep-ast");
        let client = "class Client\n{\n    HttpClient Make() => new HttpClient();\n    // new HttpClient() in a comment\n    string S = \"new HttpClient()\";\n    void Load()\n    {\n        try { Get(); } catch (Exception) { }\n        var r = Fetch(\n            1).Result;\n    }\n}\n";
        let a = repo(&base, "a", &[("src/Client.cs", client), ("src/ClientTests.cs", "class ClientTests\n{\n    HttpClient c = new HttpClient(handler);\n}\n")]);
        let b = repo(&base, "b", &[("src/Other.cs", "class Other\n{\n    object o = new Other();\n}\n"), ("src/app.ts", "const h = new HttpClient();\n")]);
        let scope = Scope { repos: vec![a.clone(), b.clone()], no_sync: false };
        let run = |pattern: &str, selector: Option<&str>| {
            let opts = structural::AstOptions {
                pattern: pattern.into(),
                lang: "csharp".into(),
                selector: selector.map(Into::into),
                max_matches: 1000,
                ..Default::default()
            };
            let out = grep_ast(&AstParams { opts, limit: 50, offset: 0 }, &scope).unwrap();
            let items: Vec<(String, String, u64)> = out.env["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| (i["repo"].as_str().unwrap().to_string(), i["file"].as_str().unwrap().to_string(), i["line"].as_u64().unwrap()))
                .collect();
            (items, out.env)
        };
        // Code only (not comments or strings), C# files only, test files last; the trigram prefilter skips files
        // without `HttpClient`.
        let (items, env) = run("new HttpClient($$$)", None);
        let row = |r: &str, f: &str, l: u64| (r.to_string(), f.to_string(), l);
        assert_eq!(items, [row("a", "src/Client.cs", 3), row("a", "src/ClientTests.cs", 3)]);
        assert_eq!((env["stats"]["searchable_files"].as_u64(), env["stats"]["candidates"].as_u64()), (Some(3), Some(2)));
        // A match spanning lines is reported where it starts.
        let (items, _) = run("$T.Result", None);
        assert_eq!(items, [row("a", "src/Client.cs", 9)]);
        let (items, _) = run("try { $$$ } catch $$$ { }", Some("catch_clause"));
        assert_eq!(items, [row("a", "src/Client.cs", 8)]);

        // An edit is seen without reindexing.
        std::fs::write(b.join("src/Other.cs"), "class Other\n{\n    HttpClient o = new HttpClient();\n}\n").unwrap();
        let (items, _) = run("new HttpClient($$$)", None);
        assert_eq!(items.len(), 3);
        assert!(items.contains(&row("b", "src/Other.cs", 3)));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Terraform end to end: a module repo and a repo of configurations calling it through a registry source and a git
    /// source. `context` on a call says where its source leads; on the module's variable, which calls set it, in the
    /// other repo; `refs` on its output finds the reads there; and `impact` crosses from the module's resource through
    /// its output to the blocks reading it.
    #[test]
    fn terraform_module_calls_link_across_repos() {
        let cache = test_cache();
        let base = cache.dir("terraform");
        let module = repo(
            &base,
            "terraform-aws-cdn",
            &[
                ("variables.tf", "variable \"site_name\" {\n  description = \"The site\"\n}\n"),
                ("main.tf", "resource \"aws_s3_bucket\" \"site\" {\n  bucket = var.site_name\n}\n"),
                ("outputs.tf", "output \"bucket_arn\" {\n  value = aws_s3_bucket.site.arn\n}\n"),
            ],
        );
        assert!(std::process::Command::new("git")
            .args(["remote", "add", "origin", "git@git.example.com:infra/modules/terraform-aws-cdn.git"])
            .current_dir(&module)
            .status()
            .unwrap()
            .success());
        // The callers ask for `?ref=v1`: the checkout is at it.
        commit_and_tag(&module, "v1");
        kc_index::sync(&module, &mut kc_index::open(&module).unwrap(), &SyncOptions { threads: 1, full_discovery: true, ..Default::default() }).unwrap();
        let sites = repo(
            &base,
            "sites",
            &[
                (
                    "prod/main.tf",
                    "module \"cdn\" {\n  source    = \"git.example.com/infra/terraform-aws-cdn/aws\"\n  version   = \"~> 1.0\"\n  site_name = \"shop\"\n}\n\nresource \"aws_iam_policy\" \"read\" {\n  policy = jsonencode({ Resource = module.cdn.bucket_arn })\n}\n",
                ),
                ("staging/main.tf", "module \"cdn\" {\n  source    = \"git::ssh://git@git.example.com/infra/modules/terraform-aws-cdn.git?ref=v1\"\n  site_name = \"shop-staging\"\n}\n"),
            ],
        );
        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };

        let c = context("prod/main.tf#module.cdn", 4000, kc_store::skeleton::SourceMode::None, &scope(&sites)).unwrap();
        let m = &c.env["items"][0]["terraform"]["module"];
        assert_eq!((m["kind"].as_str(), m["repo"].as_str(), m["confidence"].as_str()), (Some("registry"), Some("terraform-aws-cdn"), Some("unique_name")));
        // A registry version is never checked against the checkout.
        assert_eq!(m["assumptions"], json!(["revision_unverified"]));
        let c = context("staging/main.tf#module.cdn", 4000, kc_store::skeleton::SourceMode::None, &scope(&sites)).unwrap();
        let m = &c.env["items"][0]["terraform"]["module"];
        assert_eq!((m["kind"].as_str(), m["confidence"].as_str()), (Some("git"), Some("exact")));
        // `?ref=v1` is the checkout's HEAD.
        assert!(m.get("assumptions").is_none());

        // From the module's side: the calls setting its variable, in the other repo.
        let c = context("var.site_name", 4000, kc_store::skeleton::SourceMode::None, &scope(&module)).unwrap();
        let item = &c.env["items"][0];
        let called: Vec<_> = item["terraform"]["module_called_by"].as_array().unwrap().iter().map(|c| (c["file"].as_str().unwrap(), c["confidence"].as_str().unwrap())).collect();
        assert_eq!(called, [("prod/main.tf", "unique_name"), ("staging/main.tf", "exact")]);
        let mut callers: Vec<_> = item["callers"].as_array().unwrap().iter().map(|h| (h["repo"].as_str(), h["file"].as_str().unwrap(), h["kind"].as_str().unwrap())).collect();
        callers.sort();
        assert_eq!(callers, [(None, "main.tf", "tf_ref"), (Some("sites"), "prod/main.tf", "tf_input"), (Some("sites"), "staging/main.tf", "tf_input")]);

        let r = refs("output.bucket_arn", &[], 10, &scope(&module)).unwrap();
        let got: Vec<_> = r.data.items.iter().map(|(_, h)| (h.repo.as_deref(), h.file.as_str(), h.line, h.from.as_ref().map(|f| f.name.as_str()))).collect();
        assert_eq!(got, [(Some("sites"), "prod/main.tf", 8, Some("aws_iam_policy.read"))]);

        let i = impact_of(&module, "aws_s3_bucket.site");
        let got: Vec<_> = i.impact.affected.iter().map(|a| (a.hop, a.repo.clone(), a.qualified.clone(), a.edge)).collect();
        assert_eq!(
            got,
            [(1, None, "output.bucket_arn".to_string(), "uses"), (2, Some("sites".to_string()), "aws_iam_policy.read".to_string(), "module")]
        );
        assert_eq!(i.impact.affected[1].module.as_ref().map(|m| m.module.as_str()), Some("module.cdn"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Workspace-wide `refs` (no repo named, outside any) on a Terraform module's output and variable: the module repo
    /// is opened though nothing in it uses the name, each caller's site is listed once, linked, under the caller's repo.
    #[test]
    fn workspace_refs_reach_a_module_from_its_callers_once_each() {
        let cache = test_cache();
        let base = cache.dir("tf-workspace");
        let module = repo(
            &base,
            "terraform-aws-wz",
            &[
                ("variables.tf", "variable \"site_wz\" {}\n"),
                ("main.tf", "resource \"aws_s3_bucket\" \"site\" {\n  bucket = var.site_wz\n}\n"),
                ("outputs.tf", "output \"endpoint_wz\" {\n  value = aws_s3_bucket.site.arn\n}\n"),
            ],
        );
        assert!(std::process::Command::new("git")
            .args(["remote", "add", "origin", "git@git.example.com:infra/modules/terraform-aws-wz.git"])
            .current_dir(&module)
            .status()
            .unwrap()
            .success());
        // The callers ask for `?ref=v1`: the checkout is at it.
        commit_and_tag(&module, "v1");
        kc_index::sync(&module, &mut kc_index::open(&module).unwrap(), &SyncOptions { threads: 1, full_discovery: true, ..Default::default() }).unwrap();
        let call = |site: &str| {
            format!(
                "module \"cdn\" {{\n  source  = \"git::ssh://git@git.example.com/infra/modules/terraform-aws-wz.git?ref=v1\"\n  site_wz = \"{site}\"\n}}\n\nresource \"aws_iam_policy\" \"read\" {{\n  policy = module.cdn.endpoint_wz\n}}\n"
            )
        };
        let shop = repo(&base, "shop-wz", &[("main.tf", &call("shop"))]);
        let blog = repo(&base, "blog-wz", &[("main.tf", &call("blog"))]);
        let other = repo(&base, "other-wz", &[("main.tf", "resource \"aws_s3_bucket\" \"x\" {\n  bucket = \"x\"\n}\n")]);
        let mine = [module.clone(), shop.clone(), blog.clone(), other.clone()];
        let all = kc_index::indexed_roots().unwrap();
        let kept: Vec<String> = refs_roots(all.clone(), "output.endpoint_wz", &[], false).unwrap().iter().filter(|r| mine.contains(r)).map(|r| repo_name(r)).collect();
        assert!(kept.contains(&"terraform-aws-wz".to_string()), "{kept:?}");
        assert!(!kept.contains(&"other-wz".to_string()), "{kept:?}");
        for (name, kind, line) in [("output.endpoint_wz", "tf_output", 7), ("var.site_wz", "tf_input", 3)] {
            let ws = refs_in(name, &[], 50, all.clone(), true, true).unwrap();
            let mut got: Vec<_> = ws.env["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|h| h["kind"] == kind)
                .map(|h| (h["repo"].as_str().unwrap().to_string(), h["file"].as_str().unwrap().to_string(), h["line"].as_u64().unwrap(), h["confidence"].as_str().unwrap().to_string()))
                .collect();
            got.sort();
            let want = |r: &str| (r.to_string(), "main.tf".to_string(), line, "exact".to_string());
            assert_eq!(got, [want("blog-wz"), want("shop-wz")], "{name}: {}", ws.env);
            // Nothing else: no unlinked copy of a caller's site from the caller's own repo (the variable is also
            // used once inside the module).
            assert_eq!(ws.env["total"], if kind == "tf_output" { 2 } else { 3 }, "{name}: {}", ws.env);
            // The same as opening every repo (in the order of the roots given).
            let forced = refs_in(name, &[], 50, mine.to_vec(), false, true).unwrap();
            let sorted = |v: &Value| {
                let mut s: Vec<String> = v.as_array().unwrap().iter().map(Value::to_string).collect();
                s.sort();
                s
            };
            assert_eq!((sorted(&ws.env["items"]), &ws.env["total"]), (sorted(&forced.env["items"]), &forced.env["total"]), "{name}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Two role repos share a Galaxy name (a Linux and a Windows `configure_beacon`). `refs` from the playbook repo
    /// links each use through its `requirements.yml` `src` to the right repo, and a use with no `src` is ambiguous
    /// across both.
    #[test]
    fn role_uses_link_through_their_requirements() {
        let cache = test_cache();
        let base = cache.dir("ansible-twins");
        let _ = std::fs::remove_dir_all(&base);
        let meta = "galaxy_info:\n  role_name: configure_beacon\ndependencies: []\n";
        let linux = repo(&base, "Configure.Beacon", &[("tasks/main.yml", "- name: Install\n  apt: name=beacon\n"), ("meta/main.yml", meta)]);
        let windows = repo(&base, "Configure-Beacon.Windows", &[("tasks/main.yml", "- name: Install\n  win_package: path=beacon.msi\n"), ("meta/main.yml", meta)]);
        for (r, url) in [(&linux, "git@git.example.com:Roles/Configure.Beacon.git"), (&windows, "https://git.example.com/Roles/Configure-Beacon.Windows.git")] {
            assert!(std::process::Command::new("git").args(["remote", "add", "origin", url]).current_dir(r).status().unwrap().success());
            kc_index::sync(r, &mut kc_index::open(r).unwrap(), &SyncOptions { threads: 1, full_discovery: true, ..Default::default() }).unwrap();
        }
        let play = |hosts: &str| format!("---\n- name: {hosts}\n  hosts: {hosts}\n  roles:\n    - configure_beacon\n");
        let plays = repo(
            &base,
            "beacon-plays",
            &[
                ("linux/requirements.yml", "- src: http://git.example.com/Roles/Configure.Beacon.git\n  scm: git\n  name: configure_beacon\n"),
                ("linux/site.yml", &play("linux")),
                ("windows/requirements.yml", "- src: git@git.example.com:Roles/Configure-Beacon.Windows.git\n  name: configure_beacon\n"),
                ("windows/site.yml", &play("windows")),
                ("other/site.yml", &play("other")),
                ("linux/extra.yml", "---\n- name: extra\n  hosts: linux\n  tasks:\n    - include_role:\n        name: configure_beacon\n        tasks_from: install.yml\n"),
            ],
        );
        let r = refs("configure_beacon", &["role".to_string()], 10, &Scope { repos: vec![plays.clone()], no_sync: false }).unwrap();
        let got: Vec<(&str, &str, Vec<&str>)> = r
            .data
            .items
            .iter()
            .map(|(_, h)| (h.file.as_str(), h.confidence, h.targets.iter().filter_map(|t| t.repo.as_deref()).collect()))
            .collect();
        assert_eq!(
            got,
            [
                ("linux/extra.yml", "exact", vec!["Configure.Beacon"]),
                ("linux/site.yml", "exact", vec!["Configure.Beacon"]),
                ("other/site.yml", "ambiguous", vec!["Configure-Beacon.Windows", "Configure.Beacon"]),
                ("windows/site.yml", "exact", vec!["Configure-Beacon.Windows"]),
            ]
        );
        // `tasks_from` points at the task file it names, and reads as the role with the file.
        let extra = &r.data.items[0].1;
        assert_eq!((extra.targets[0].file.as_str(), extra.targets[0].qualified.as_str()), ("tasks/install.yml", "Configure.Beacon"));
        assert!(crate::render::ref_line_for_test(extra).contains("role configure_beacon (tasks_from install.yml)"));
        assert!(r.data.items[1..].iter().all(|(_, h)| h.targets.iter().all(|t| t.id != 0 && t.file == "tasks/main.yml")));
        // `context` and `trace` on the name from the playbook repo answer from the role's repo.
        let plays_scope = Scope { repos: vec![plays.clone()], no_sync: false };
        let c = context("configure_beacon", 2000, kc_store::skeleton::SourceMode::None, &plays_scope).unwrap();
        assert_eq!(c.env["items"][0]["repo"], "Configure.Beacon");
        assert_eq!(c.env["items"][0]["symbol"]["name"], "Configure.Beacon");
        let t = trace("configure_beacon", "out", 2, 50, &WalkFilter::default(), &plays_scope).unwrap();
        assert_eq!(t.env["items"][0]["repo"], "Configure.Beacon");
        assert_eq!(t.env["items"][0]["nodes"][0]["qualified"], "Configure.Beacon", "not Configure.Beacon.Configure.Beacon");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Ansible end to end: a role repo, a repo of playbooks using it through a `requirements.yml`, and an application
    /// repo the role deploys. Variables, handlers and templates resolve in the role; the playbook's role reference
    /// links to the role's repo by its origin; impact crosses from a role variable to the play in the other repo; and
    /// `context` on the application repo's name lists the role deploying it. CI and compose files are not Ansible.
    #[test]
    fn ansible_roles_link_across_repos_and_to_what_they_deploy() {
        let cache = test_cache();
        let base = cache.dir("ansible");
        let role = repo(
            &base,
            "Configure.Orders",
            &[
                ("defaults/main.yml", "orders_service: Acme.OrdersApi\norders_port: 8080\n"),
                (
                    "tasks/main.yml",
                    "- name: Render orders config\n  template:\n    src: orders.conf.j2\n    dest: /etc/orders.conf\n  notify: restart orders\n- name: Start orders\n  systemd:\n    name: \"{{ orders_service }}\"\n    state: started\n",
                ),
                ("handlers/main.yml", "- name: restart orders\n  service:\n    name: \"{{ orders_service }}\"\n    state: restarted\n"),
                ("templates/orders.conf.j2", "listen={{ orders_port }}\n"),
                ("meta/main.yml", "galaxy_info:\n  role_name: configure_orders\n  description: Orders service\ndependencies: []\n"),
            ],
        );
        assert!(std::process::Command::new("git")
            .args(["remote", "add", "origin", "git@git.example.com:Roles/Configure.Orders.git"])
            .current_dir(&role)
            .status()
            .unwrap()
            .success());
        let plays = repo(
            &base,
            "plays",
            &[
                ("orders/requirements.yml", "- src: http://git.example.com/Roles/Configure.Orders.git\n  scm: git\n  name: orders\n"),
                ("orders/site.yml", "---\n- name: Orders hosts\n  hosts: orders\n  roles:\n    - orders\n"),
                // Neither is a playbook, whatever they hold.
                (".gitlab-ci.yml", "deploy:\n  script:\n    - ansible-playbook orders/site.yml\n"),
                ("docker-compose.yml", "services:\n  - hosts: x\n"),
            ],
        );
        let app = repo(&base, "Acme.OrdersApi", &[("src/Acme.OrdersApi.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\" />\n")]);
        // Index the role again so its summary has the origin added after the first sync.
        kc_index::sync(&role, &mut kc_index::open(&role).unwrap(), &SyncOptions { threads: 1, full_discovery: true, ..Default::default() }).unwrap();
        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };

        let r = refs("orders_port", &[], 10, &scope(&role)).unwrap();
        let got: Vec<_> = r.data.items.iter().map(|(_, h)| (h.file.as_str(), h.confidence, h.targets[0].file.as_str())).collect();
        assert_eq!(got, [("templates/orders.conf.j2", "exact", "defaults/main.yml")]);
        let r = refs("restart orders", &[], 10, &scope(&role)).unwrap();
        assert_eq!(r.data.items[0].1.targets[0].file, "handlers/main.yml");
        let r = refs("orders.conf.j2", &[], 10, &scope(&role)).unwrap();
        assert_eq!((r.data.items[0].1.confidence, r.data.items[0].1.targets[0].file.as_str()), ("exact", "templates/orders.conf.j2"));

        // The playbook's role, from the role's side: used by the play in the other repo.
        let r = refs("Configure.Orders", &["role".to_string()], 10, &scope(&role)).unwrap();
        let got: Vec<_> = r.data.items.iter().map(|(_, h)| (h.repo.as_deref(), h.file.as_str(), h.confidence)).collect();
        assert_eq!(got, [(Some("plays"), "orders/site.yml", "exact")]);
        let c = context("Configure.Orders", 4000, kc_store::skeleton::SourceMode::Auto, &scope(&role)).unwrap();
        let item = c.env["items"].as_array().unwrap().iter().find(|i| i["symbol"]["kind"] == "role").unwrap();
        assert_eq!(item["ansible"]["used_by"][0]["repo"], "plays");
        let deploys: Vec<_> = item["ansible"]["deploys"].as_array().unwrap().iter().map(|d| (d["resolved"].as_str(), d["links"][0]["repo"].as_str())).collect();
        assert_eq!(deploys, [(Some("Acme.OrdersApi"), Some("Acme.OrdersApi")), (Some("Acme.OrdersApi"), Some("Acme.OrdersApi"))]);

        // From the play's side, and impact from the template's variable across to the play.
        let c = context("Orders hosts", 4000, kc_store::skeleton::SourceMode::Auto, &scope(&plays)).unwrap();
        let roles = &c.env["items"][0]["ansible"]["roles"][0];
        assert_eq!((roles["repo"].as_str(), roles["confidence"].as_str()), (Some("Configure.Orders"), Some("exact")));
        let i = impact_of(&role, "orders_port");
        let got: Vec<_> = i.impact.affected.iter().map(|a| (a.hop, a.repo.clone(), a.qualified.clone(), a.edge)).collect();
        assert_eq!(
            got,
            [
                (1, None, "Configure.Orders.orders.conf.j2".to_string(), "uses"),
                (2, None, "Configure.Orders.Render orders config".to_string(), "templates"),
                (3, None, "Configure.Orders".to_string(), "runs"),
            ]
        );
        let i = impact_of(&role, "Configure.Orders");
        // The plays repo's own CI job runs the playbook using the role: a deploy edge too.
        let plays_edge = |e: &'static str| (Some("plays".to_string()), e);
        assert_eq!(i.impact.affected.iter().map(|a| (a.repo.clone(), a.edge)).collect::<Vec<_>>(), [plays_edge("role"), plays_edge("runs"), plays_edge("deploy")]);

        // The application repo, by name: the role deploys it.
        let c = context("Acme.OrdersApi", 4000, kc_store::skeleton::SourceMode::Auto, &scope(&app)).unwrap();
        let by = &c.env["items"][0]["ansible"]["deployed_by"];
        assert_eq!((by[0]["repo"].as_str(), by[0]["role"].as_str(), by[0]["confidence"].as_str()), (Some("Configure.Orders"), Some("Configure.Orders"), Some("unique_name")));
        assert!(by[0]["role_used_by"][0].as_str().unwrap().starts_with("plays: "));

        // Only Ansible files are Ansible; the CI file is GitLab CI; the compose file is read for its hosts.
        let o = open_fresh(&plays, true).unwrap();
        let langs: Vec<(String, String)> = o
            .store
            .conn()
            .prepare("SELECT path, lang FROM files ORDER BY path")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let lang = |p: &str, l: &str| (p.to_string(), l.to_string());
        assert_eq!(
            langs,
            [lang(".gitlab-ci.yml", "gitlabci"), lang("docker-compose.yml", "hostconfig"), lang("orders/requirements.yml", "ansible"), lang("orders/site.yml", "ansible")]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// GitLab CI end to end: an application repo whose job extends a shared template (in another repo, included by
    /// project path) and starts a child pipeline from its own file, whose job extends the shared deploy template that
    /// clones the deploy repo and runs its playbook. `context` on the application gives the chain with the values
    /// passed and what the playbook does with them; on the playbook and the template, the application deployed;
    /// `impact` on the template reaches the application's job.
    #[test]
    fn ci_pipelines_link_applications_to_the_playbooks_deploying_them() {
        let cache = test_cache();
        let base = cache.dir("gitlabci");
        let add_origin = |root: &Path, url: &str| {
            assert!(std::process::Command::new("git").args(["remote", "add", "origin", url]).current_dir(root).status().unwrap().success());
            kc_index::sync(root, &mut kc_index::open(root).unwrap(), &SyncOptions { threads: 1, full_discovery: true, ..Default::default() }).unwrap();
        };
        let templates = repo(
            &base,
            "pipeline-templates",
            &[
                ("dotnet/main.yml", "include:\n  - local: '/dotnet/pipeline.yml'\n"),
                ("dotnet/pipeline.yml", ".deploy:\n  stage: deploy\n  rules:\n    - if: '$CI_COMMIT_REF_NAME == $CI_DEFAULT_BRANCH'\n"),
                (
                    "dotnet/deploy.yml",
                    "variables:\n  INVENTORY_FILE: \"${APP_DIR}/${ENV_TAG}.inventory\"\n.deploy_vm:\n  before_script:\n    - >\n      [[ -z \"$PACKAGE_NAME\" ]] && pushd \"$APP_DIR\" && PACKAGE_NAME=\"$(basename *.csproj .csproj)\" && popd\n    - '[[ -z \"$SERVICE_NAME\" ]] && SERVICE_NAME=\"${PACKAGE_NAME#Acme.}\"'\n    - git clone --depth 1 https://git.example.com/ops/Deploy.App.git\n  script:\n    - ansible-playbook ./Deploy.App/deploy_app.yml -i $INVENTORY_FILE --extra-vars \"APP_NAME=$PACKAGE_NAME APP_PORT=$APP_PORT SERVICE=$SERVICE_NAME\"\n.stage:\n  variables:\n    ENV_TAG: Staging\n",
                ),
            ],
        );
        add_origin(&templates, "git@git.example.com:Infra/pipeline-templates.git");
        let deploy = repo(
            &base,
            "Deploy.App",
            &[(
                "deploy_app.yml",
                "- name: Install app\n  hosts: all\n  vars:\n    svc: \"{{ SERVICE }}\"\n    app_dir: \"/srv/apps/{{ APP_NAME }}\"\n  tasks:\n    - name: App dir\n      file:\n        path: \"{{ app_dir }}\"\n        state: directory\n    - name: Unit\n      template:\n        src: unit.j2\n        dest: \"/etc/systemd/system/{{ svc }}.service\"\n    - name: Start\n      systemd:\n        name: \"{{ svc }}\"\n        state: started\n",
            )],
        );
        add_origin(&deploy, "git@git.example.com:ops/Deploy.App.git");
        let app = repo(
            &base,
            "Acme.ShippingApi",
            &[
                (".gitlab-ci.yml", "include:\n  - project: 'Infra/pipeline-templates'\n    ref: master\n    file: '/dotnet/main.yml'\n  - local: '**/ci.yml'\n"),
                (
                    "src/Acme.ShippingApi/ci.yml",
                    ".app_vars:\n  variables:\n    APP_DIR: src/Acme.ShippingApi\n    APP_PORT: 5080\ndeploy_api:\n  extends:\n    - .deploy\n    - .app_vars\n  trigger:\n    include: \"${APP_DIR}/deploy.yml\"\n",
                ),
                ("src/Acme.ShippingApi/deploy.yml", "include:\n  - project: 'Infra/pipeline-templates'\n    file: '/dotnet/deploy.yml'\ndeploy_stage:\n  extends:\n    - .deploy_vm\n    - .stage\n"),
                ("src/Acme.ShippingApi/Acme.ShippingApi.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\" />\n"),
            ],
        );
        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };

        let c = context("Acme.ShippingApi", 4000, kc_store::skeleton::SourceMode::Auto, &scope(&app)).unwrap();
        let chains = &c.env["items"][0]["ansible"]["deploy_chains"];
        assert_eq!(chains.as_array().unwrap().len(), 1, "{chains:#}");
        let chain = &chains[0];
        let path: Vec<(&str, &str)> = chain["path"].as_array().unwrap().iter().map(|h| (h["name"].as_str().unwrap(), h["at"].as_str().unwrap())).collect();
        assert_eq!(
            path,
            [
                ("deploy_api", "src/Acme.ShippingApi/ci.yml:5"),
                ("src/Acme.ShippingApi/deploy.yml", "src/Acme.ShippingApi/deploy.yml"),
                ("deploy_stage", "src/Acme.ShippingApi/deploy.yml:4"),
                (".deploy_vm", "pipeline-templates:dotnet/deploy.yml:3"),
            ]
        );
        assert_eq!((chain["playbook"]["name"].as_str(), chain["confidence"].as_str()), (Some("Deploy.App/deploy_app.yml"), Some("exact")));
        assert_eq!(chain["run"], "pipeline-templates:dotnet/deploy.yml:10");
        assert_eq!(chain["inventory"]["value"], "src/Acme.ShippingApi/Staging.inventory");
        let vars: Vec<(&str, &str, &str)> =
            chain["vars"].as_array().unwrap().iter().map(|v| (v["name"].as_str().unwrap(), v["value"].as_str().unwrap(), v["from"].as_str().unwrap())).collect();
        assert_eq!(
            vars,
            [
                ("APP_NAME", "Acme.ShippingApi", "src/Acme.ShippingApi/Acme.ShippingApi.csproj"),
                ("APP_PORT", "5080", "src/Acme.ShippingApi/ci.yml:4"),
                ("SERVICE", "ShippingApi", "pipeline-templates:dotnet/deploy.yml:7"),
            ]
        );
        let deploys: Vec<(&str, &str)> = chain["deploys"].as_array().unwrap().iter().map(|d| (d["what"].as_str().unwrap(), d["resolved"].as_str().unwrap())).collect();
        assert_eq!(deploys, [("path", "/srv/apps/Acme.ShippingApi"), ("service", "ShippingApi")]);

        // From the playbook's side and the template's: the application deployed, with its values.
        let c = context("deploy_app.yml", 4000, kc_store::skeleton::SourceMode::None, &scope(&deploy)).unwrap();
        let apps = &c.env["items"][0]["ansible"]["deploys_apps"];
        assert_eq!((apps["count"].as_u64(), apps["apps"][0]["repo"].as_str(), apps["apps"][0]["job"].as_str()), (Some(1), Some("Acme.ShippingApi"), Some("deploy_api")));
        assert!(apps["apps"][0]["vars"].as_array().unwrap().contains(&json!("APP_NAME=Acme.ShippingApi")));
        let c = context(".deploy_vm", 4000, kc_store::skeleton::SourceMode::None, &scope(&templates)).unwrap();
        assert_eq!(c.env["items"][0]["ansible"]["deploys_apps"]["count"], 1);

        // Impact on the template and on the playbook reaches the application's job.
        for (root, symbol) in [(&templates, ".deploy_vm"), (&deploy, "deploy_app.yml")] {
            let i = impact_of(root, symbol);
            let got: Vec<_> = i.impact.affected.iter().filter(|a| a.edge == "deploy").map(|a| (a.repo.clone(), a.qualified.clone(), a.confidence)).collect();
            assert_eq!(got, [(Some("Acme.ShippingApi".to_string()), "deploy_api".to_string(), "exact")], "{symbol}");
            let via = i.impact.affected.iter().find(|a| a.edge == "deploy").and_then(|a| a.deploy.clone()).unwrap();
            assert_eq!((via.playbook.as_str(), via.app.as_deref()), ("Deploy.App/deploy_app.yml", Some("Acme.ShippingApi")));
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A query naming an indexed repo puts the repo's deploy answer first, ahead of a same-named symbol, and in the
    /// envelope's `deploy` (its `repo` stays the repo's path); a repo nothing deploys still answers, empty.
    /// A construct an extractor recognises but doesn't read makes the answers drawing on it incomplete (`unsupported`),
    /// naming it with file:line, where they used to say `complete: true`: an assembly scan, a typed HTTP client and a
    /// keyed registration (DI), SQL naming its table at run time and dynamic SQL in a procedure, a publish of a message
    /// whose type isn't known (bus), Jinja-named includes and templates (Ansible), a `moved` block (Terraform), an
    /// include in a form not read (CI), a lazy route in a form not read (Angular). What reads none of them stays
    /// complete.
    #[test]
    fn constructs_met_but_not_read_make_the_answers_on_them_incomplete() {
        let cache = test_cache();
        let base = cache.dir("unsupported");
        let shop = repo(
            &base,
            "shop",
            &[
                ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"),
                (
                    "src/Startup.cs",
                    "namespace Shop;\npublic class Startup\n{\n    public void ConfigureContainer(ContainerBuilder builder)\n    {\n        builder.RegisterAssemblyTypes(typeof(Startup).Assembly).Where(t => t.Name.EndsWith(\"Service\")).AsImplementedInterfaces();\n    }\n    public void ConfigureServices(IServiceCollection services)\n    {\n        services.AddHttpClient<CatalogClient>(c => c.Timeout = null);\n        services.AddKeyedScoped<IPricing, Pricing>(\"eu\");\n        services.AddScoped<IAudit, Audit>();\n    }\n}\n",
                ),
                ("src/OrderService.cs", "namespace Shop;\npublic interface IOrderService { }\npublic class OrderService : IOrderService { }\n"),
                ("src/CatalogClient.cs", "namespace Shop;\npublic class CatalogClient { }\n"),
                ("src/Pricing.cs", "namespace Shop;\npublic interface IPricing { }\npublic class Pricing : IPricing { }\n"),
                ("src/Audit.cs", "namespace Shop;\npublic interface IAudit { }\npublic class Audit : IAudit { }\n"),
                ("src/Ledger.cs", "namespace Shop;\npublic class Ledger { }\n"),
                (
                    "src/Reports.cs",
                    "namespace Shop;\npublic class Reports\n{\n    public void Purge(SqlConnection db, string table)\n    {\n        var cmd = new SqlCommand($\"DELETE FROM {table} WHERE id = 1\", db);\n    }\n    public void Count(SqlConnection db)\n    {\n        var cmd = new SqlCommand(\"SELECT COUNT(*) FROM orders\", db);\n    }\n}\n",
                ),
                (
                    "db/procs.sql",
                    "CREATE PROCEDURE dbo.Rebuild\nAS\nBEGIN\n    DECLARE @sql NVARCHAR(MAX) = N'TRUNCATE TABLE orders';\n    EXEC (@sql);\nEND\nGO\nCREATE PROCEDURE dbo.Tidy\nAS\nBEGIN\n    DELETE FROM orders WHERE id = 0;\nEND\nGO\n",
                ),
                ("src/OrderPlaced.cs", "namespace Shop.Events;\n[ShopMessage(ShopHosts.Orders)]\npublic class OrderPlaced { }\n"),
                (
                    "src/Notifier.cs",
                    "using Shop.Events;\nnamespace Shop;\npublic class Notifier\n{\n    private readonly IShopBus _bus;\n    private readonly IMaker _maker;\n    public async Task Notify()\n    {\n        await _bus.Publish(_maker.Make());\n    }\n}\n",
                ),
                // A bus method's wrapper: its callers' calls are publishes of their own.
                (
                    "src/BusWrapper.cs",
                    "using Shop.Events;\nnamespace Shop;\npublic class BusWrapper\n{\n    private readonly IShopBus _bus;\n    public Task Publish<T>(T message) => _bus.Publish(message);\n}\n",
                ),
                ("roles/web/tasks/main.yml", "- name: os tasks\n  include_tasks: \"{{ ansible_os_family }}.yml\"\n- name: site config\n  template:\n    src: \"{{ app }}.conf.j2\"\n    dest: /etc/nginx/conf.d/app.conf\n"),
                ("infra/main.tf", "resource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"logs\"\n}\n\nmoved {\n  from = aws_s3_bucket.old_logs\n  to   = aws_s3_bucket.logs\n}\n"),
                (".gitlab-ci.yml", "include:\n  - local: /ci/build.yml\n  - { file: /shared.yml }\n"),
                ("ci/build.yml", "build:\n  script:\n    - dotnet build\n"),
                ("web/routes.module.ts", "const routes: Routes = [\n  { path: 'admin', loadChildren: () => import('./admin/admin.module') },\n];\n"),
            ],
        );
        let s = Scope { repos: vec![shop.clone()], no_sync: false };
        let unsupported = |sym: &str| -> Vec<String> {
            let env = context(sym, 4000, Default::default(), &s).unwrap().env;
            let list = crate::contract::incomplete_of(&env);
            assert_eq!(env["complete"].as_bool(), Some(list.is_empty()), "{env:#}");
            list.into_iter().filter(|i| i.reason == Reason::Unsupported).map(|i| i.detail).collect()
        };
        let one = |sym: &str, area: &str| -> String {
            let got = unsupported(sym);
            let of: Vec<&String> = got.iter().filter(|d| d.starts_with(&format!("{area}: "))).collect();
            assert_eq!(of.len(), 1, "{sym}: {got:?}");
            of[0].clone()
        };
        // DI: a type the scan's filter lets through, the typed client, the keyed service; a type nothing unread can
        // register, and one registered plainly, are complete.
        assert!(one("OrderService", "di").contains("assembly scan RegisterAssemblyTypes(…) [*Service] (src/Startup.cs:6)"));
        assert!(one("IOrderService", "di").contains("[*Service]"));
        assert!(one("CatalogClient", "di").contains("typed HTTP client AddHttpClient<CatalogClient>(…) (src/Startup.cs:10)"));
        assert!(one("IPricing", "di").contains("keyed registration AddKeyedScoped<IPricing, Pricing>(…) (src/Startup.cs:11)"));
        assert!(unsupported("Ledger").is_empty(), "{:?}", unsupported("Ledger"));
        assert!(!unsupported("Audit").iter().any(|d| d.starts_with("di: ")));
        // SQL: the method building its table's name, the procedure running dynamic SQL, and the table (any of them may
        // touch it); a method and a procedure naming theirs are complete.
        assert!(one("Reports.Purge", "sql").contains("SQL text that writes a table whose name is built at run time (src/Reports.cs:6)"));
        assert!(unsupported("Reports.Count").is_empty());
        assert!(one("dbo.Rebuild", "sql").contains("dynamic SQL run by EXEC (…) (db/procs.sql:5)"));
        assert!(unsupported("dbo.Tidy").is_empty());
        assert!(one("orders", "sql").contains("2 constructs") || one("orders", "sql").contains("constructs KnowCode doesn't read"));
        // Bus: the message's publishers may include the publish of an unknown type in a file it's in scope of.
        let bus = one("OrderPlaced", "bus");
        assert!(bus.starts_with("bus: a construct KnowCode doesn't read: Publish of a message whose type isn't known (src/Notifier.cs:9)"), "{bus}");
        // Ansible: the role's Jinja-named include and template.
        let role = one("web", "ansible");
        assert!(role.contains("include whose file is named by Jinja") && role.contains("template whose source is named by Jinja"), "{role}");
        // Terraform: the resource a `moved` block names.
        assert!(one("aws_s3_bucket.logs", "terraform").contains("moved block"));
        // CI: the include written in a form not read is accounted for, unsupported.
        let deploy = context("shop", 4000, Default::default(), &s).unwrap().env;
        let includes = deploy["items"][0]["ansible"]["ci_includes"].as_array().cloned().unwrap_or_default();
        assert!(includes.iter().any(|i| i["kind"] == "unknown" && i["status"] == "unsupported" && i["target"] == "{file}"), "{deploy:#}");
        assert!(crate::contract::incomplete_of(&deploy).iter().any(|i| i.reason == Reason::Unsupported && i.detail.contains("include {file}")), "{deploy:#}");
        // Angular: the lazy route in a default-export form is stored as met, not read.
        let o = kc_index::open(&shop).unwrap();
        let areas: Vec<String> = kc_store::unsupported::all(o.conn()).unwrap().into_iter().map(|u| u.area).collect();
        assert!(areas.contains(&"angular".to_string()), "{areas:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn repo_answer_comes_before_a_same_named_symbol() {
        let cache = test_cache();
        let base = cache.dir("gitlabci-repo-first");
        let app = repo(
            &base,
            "Payout",
            &[
                (".gitlab-ci.yml", "publish:\n  script:\n    - docker build -t registry/payout .\n    - docker push registry/payout\n"),
                ("Dockerfile", "FROM scratch\n"),
                ("src/Payout.cs", "namespace Billing;\npublic class Payout\n{\n    public decimal Amount { get; set; }\n}\n"),
            ],
        );
        let c = context("Payout", 4000, kc_store::skeleton::SourceMode::None, &Scope { repos: vec![app.clone()], no_sync: false }).unwrap();
        let items = c.env["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "{:#}", c.env);
        assert_eq!(items[0]["repo"], "Payout");
        assert_eq!(items[0]["ansible"]["deploy_chains"][0]["kind"], "docker");
        assert_eq!(c.env["deploy"]["ansible"]["deploy_chains"][0]["kind"], "docker");
        assert_eq!(items[1]["symbol"]["name"], "Payout", "{:#}", items[1]);
        assert!(c.env["repo"].as_str().is_some_and(|r| r.ends_with("Payout")), "{:#}", c.env["repo"]);
        let quiet = repo(&base, "Ledger", &[("src/Ledger.cs", "namespace Billing;\npublic class Entry { }\n")]);
        let c = context("Ledger", 4000, kc_store::skeleton::SourceMode::None, &Scope { repos: vec![quiet], no_sync: false }).unwrap();
        assert_eq!(c.env["items"][0]["repo"], "Ledger", "{:#}", c.env);
        assert!(c.env["deploy"]["ansible"].get("deploy_chains").is_none(), "{:#}", c.env);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A deploy question on a repo whose pipeline includes shared CI from another repo, in every form: followed into
    /// the indexed template repo (its jobs, stages and deploy steps, its own includes, a cycle once), which joins the
    /// answer's provenance; a template project not indexed is named `missing_repo` with its path, file and ref; a
    /// GitLab template this doesn't know is `unsupported`.
    #[test]
    fn deploy_answers_follow_ci_includes_into_template_repos() {
        let cache = test_cache();
        let base = cache.dir("gitlabci-includes");
        let add_origin = |root: &Path, url: &str| {
            assert!(std::process::Command::new("git").args(["remote", "add", "origin", url]).current_dir(root).status().unwrap().success());
            kc_index::sync(root, &mut kc_index::open(root).unwrap(), &SyncOptions { threads: 1, full_discovery: true, ..Default::default() }).unwrap();
        };
        let templates = repo(
            &base,
            "ci-templates",
            &[
                ("deploy/app.yml", "include:\n  - local: /deploy/base.yml\nstages: [build, deploy]\ndeploy_app:\n  extends: .deploy_base\n  stage: deploy\n  script:\n    - helm upgrade --install $CI_PROJECT_NAME ./chart\n"),
                ("deploy/base.yml", "include:\n  - local: /deploy/app.yml\n.deploy_base:\n  image: alpine\n  variables:\n    NS: shop\n"),
                ("remote.yml", "lint_remote:\n  stage: test\n  script:\n    - echo lint\n"),
                ("templates/build.yml", "spec:\n  inputs:\n    stage:\n      default: build\n---\nbuild_image:\n  stage: $[[ inputs.stage ]]\n  script:\n    - docker build -t app .\n"),
            ],
        );
        add_origin(&templates, "git@gitlab.example.test:platform/ci-templates.git");
        let app = repo(
            &base,
            "shop-api",
            &[
                (".gitlab-ci.yml", "include:\n  - project: 'platform/ci-templates'\n    ref: main\n    file: '/deploy/app.yml'\n  - project: 'platform/missing-templates'\n    ref: v2\n    file: '/x.yml'\n  - component: $CI_SERVER_FQDN/platform/ci-templates/build@1.0\n  - remote: https://gitlab.example.test/platform/ci-templates/-/raw/main/remote.yml\n  - template: Jobs/Code-Quality.gitlab-ci.yml\n  - local: /ci/local.yml\n  - project: 'platform/old-templates'\n    ref: v1\n    file: '/old.yml'\n"),
                ("ci/local.yml", "unit:\n  stage: test\n  script:\n    - dotnet test\n"),
            ],
        );
        add_origin(&app, "git@gitlab.example.test:shop/shop-api.git");
        // A template repo indexed once whose checkout is gone since.
        let old = repo(&base, "old-templates", &[("old.yml", "old_job:\n  script:\n    - echo old\n")]);
        add_origin(&old, "git@gitlab.example.test:platform/old-templates.git");
        std::fs::remove_dir_all(&old).unwrap();
        let c = context("shop-api", 4000, kc_store::skeleton::SourceMode::None, &Scope { repos: vec![app.clone()], no_sync: false }).unwrap();
        let includes = c.env["deploy"]["ansible"]["ci_includes"].as_array().cloned().unwrap_or_default();
        let got: Vec<(&str, &str, &str)> = includes.iter().map(|i| (i["kind"].as_str().unwrap(), i["status"].as_str().unwrap(), i["repo"].as_str().unwrap_or("-"))).collect();
        assert_eq!(
            got,
            [
                ("project", "followed", "ci-templates"),
                ("local", "followed", "ci-templates"),
                ("local", "followed", "ci-templates"),
                ("project", "missing", "-"),
                ("component", "followed", "ci-templates"),
                ("remote", "followed", "ci-templates"),
                ("template", "unsupported", "-"),
                ("local", "followed", "-"),
                ("project", "missing", "-"),
            ],
            "{:#}",
            c.env
        );
        // What the template repo brings: its jobs with their stages, and the deploy step.
        assert_eq!(includes[0]["jobs"], json!(["deploy_app (deploy)"]));
        assert_eq!(includes[0]["deploys"], json!(["helm upgrade (deploy_app)"]));
        assert_eq!(includes[4]["jobs"], json!(["build_image (build)"]));
        assert_eq!(includes[5]["jobs"], json!(["lint_remote (test)"]));
        // The template project not indexed is named, with its path, file and ref, as a missing repo.
        assert_eq!((includes[3]["target"].as_str(), includes[3]["ref"].as_str()), (Some("platform/missing-templates"), Some("v2")));
        let incomplete = crate::contract::incomplete_of(&c.env);
        let missing: Vec<&str> = incomplete.iter().filter(|i| i.reason == Reason::MissingRepo).map(|i| i.detail.as_str()).collect();
        // The gone checkout is named once, through the workspace's missing repos, not again as a CI include.
        assert_eq!(missing.len(), 2, "{incomplete:?}");
        assert!(missing.contains(&"deploy chains: project platform/missing-templates, file /x.yml, ref v2 (not indexed)"), "{incomplete:?}");
        assert!(missing.iter().any(|m| m.starts_with("old-templates: indexed, but its checkout at") && m.contains("is gone")), "{incomplete:?}");
        assert!(includes[8]["why"].as_str().unwrap().contains("ref v1 (checkout gone"), "{:#}", includes[8]);
        assert!(incomplete.iter().any(|i| i.reason == Reason::Unsupported && i.detail.contains("Jobs/Code-Quality.gitlab-ci.yml")), "{incomplete:?}");
        assert!(!incomplete.iter().any(|i| i.reason == Reason::Failed), "{incomplete:?}");
        // The template repo the answer read is in its provenance.
        let provenance = crate::contract::provenance_of(&c.env);
        assert!(provenance.iter().any(|p| p.repo == templates.to_string_lossy()), "{provenance:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A playbook's role that two repos define, with no requirement naming either: the chain lists both candidates,
    /// each `ambiguous` with its own files; a Galaxy role no repo holds is listed `external_likely`, without files.
    #[test]
    fn ci_chains_list_ambiguous_and_external_roles() {
        let cache = test_cache();
        let base = cache.dir("gitlabci-roles");
        for name in ["infra-east", "infra-west"] {
            repo(
                &base,
                name,
                &[
                    ("roles/web/tasks/main.yml", "- name: Install nginx\n  package:\n    name: nginx\n"),
                    ("roles/web/handlers/main.yml", "- name: restart nginx\n  service:\n    name: nginx\n    state: restarted\n"),
                ],
            );
        }
        let app = repo(
            &base,
            "storefront",
            &[
                (".gitlab-ci.yml", "deploy:\n  script:\n    - ansible-playbook deploy/site.yml -i inventory\n"),
                ("deploy/site.yml", "- hosts: web\n  roles:\n    - web\n    - geerlingguy.docker\n"),
            ],
        );
        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };
        let c = context("storefront", 4000, kc_store::skeleton::SourceMode::None, &scope(&app)).unwrap();
        let chain = &c.env["items"][0]["ansible"]["deploy_chains"][0];
        let roles: Vec<(&str, Option<&str>, &str, Vec<&str>)> = chain["roles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let tasks = r["tasks"].as_array().map(|t| t.iter().map(|x| x.as_str().unwrap()).collect()).unwrap_or_default();
                (r["name"].as_str().unwrap(), r["repo"].as_str(), r["confidence"].as_str().unwrap(), tasks)
            })
            .collect();
        assert_eq!(
            roles,
            [
                ("web", Some("infra-east"), "ambiguous", vec!["roles/web/tasks/main.yml"]),
                ("web", Some("infra-west"), "ambiguous", vec!["roles/web/tasks/main.yml"]),
                ("geerlingguy.docker", None, "external_likely", vec![]),
            ],
            "{chain:#}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Deploys without Ansible, end to end: a shared template uploading to S3 and invalidating CloudFront (reached
    /// through a `!reference`), a worker deployed by `npm run deploy` with its wrangler config, and Terraform through
    /// GitLab's built-in template with a module from another repo. `context` on the application lists each chain's
    /// steps, files and values; `context` and `impact` on the shared template reach the application's job.
    #[test]
    fn ci_pipelines_link_deploys_without_ansible() {
        let cache = test_cache();
        let base = cache.dir("gitlabci-tools");
        let add_origin = |root: &Path, url: &str| {
            assert!(std::process::Command::new("git").args(["remote", "add", "origin", url]).current_dir(root).status().unwrap().success());
            kc_index::sync(root, &mut kc_index::open(root).unwrap(), &SyncOptions { threads: 1, full_discovery: true, ..Default::default() }).unwrap();
        };
        let templates = repo(
            &base,
            "spa-ci",
            &[(
                "spa/release.yml",
                ".release:\n  script:\n    - !reference [.release_spa, script]\n.release_spa:\n  script:\n    - aws s3 sync dist \"s3://${BUCKET}/${CI_PROJECT_NAME}\"\n    - aws cloudfront create-invalidation --distribution-id \"$CF_ID\" --paths '/*'\n",
            )],
        );
        add_origin(&templates, "git@git.example.com:Infra/spa-ci.git");
        let module = repo(&base, "terraform-aws-site", &[("main.tf", "variable \"name\" {}\n")]);
        add_origin(&module, "git@git.example.com:terraform/terraform-aws-site.git");
        let app = repo(
            &base,
            "web-shop",
            &[
                (
                    ".gitlab-ci.yml",
                    "include:\n  - project: 'Infra/spa-ci'\n    file: '/spa/release.yml'\n  - template: Terraform/Base.latest.gitlab-ci.yml\nvariables:\n  BUCKET: shop-prod\n  CF_ID: E2ABC\n  TF_ROOT: infra\nrelease:\n  extends: .release\nworker:\n  script:\n    - npm run deploy\nterraform:deploy:\n  extends: .terraform:deploy\n",
                ),
                ("package.json", "{\n  \"name\": \"web-shop\",\n  \"scripts\": {\n    \"deploy\": \"wrangler deploy\"\n  }\n}\n"),
                ("wrangler.toml", "name = \"shop-edge\"\nmain = \"worker/index.ts\"\n"),
                ("worker/index.ts", "export default {};\n"),
                ("infra/main.tf", "module \"site\" {\n  source = \"git.example.com/terraform/terraform-aws-site/aws\"\n}\n"),
            ],
        );
        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };

        let c = context("web-shop", 4000, kc_store::skeleton::SourceMode::Auto, &scope(&app)).unwrap();
        let a = &c.env["items"][0]["ansible"];
        let chains = a["deploy_chains"].as_array().unwrap();
        let kinds: Vec<(&str, &str, &str)> =
            chains.iter().map(|c| (c["kind"].as_str().unwrap(), c["path"][0]["name"].as_str().unwrap(), c["confidence"].as_str().unwrap())).collect();
        assert_eq!(kinds, [("aws", "release", "exact"), ("wrangler", "worker", "exact"), ("terraform", "terraform:deploy", "external_likely")], "{a:#}");
        let path: Vec<&str> = chains[0]["path"].as_array().unwrap().iter().map(|h| h["at"].as_str().unwrap()).collect();
        assert_eq!(path, [".gitlab-ci.yml:9", "spa-ci:spa/release.yml:1", "spa-ci:spa/release.yml:4"]);
        let s3 = &chains[0]["steps"][0];
        assert_eq!((s3["action"].as_str(), s3["at"].as_str()), (Some("s3 sync"), Some("spa-ci:spa/release.yml:6")));
        assert_eq!(s3["values"][1]["value"], "s3://shop-prod/web-shop");
        assert_eq!(chains[0]["steps"][1]["values"][0]["value"], "E2ABC");
        let w = &chains[1]["steps"][0];
        assert_eq!((w["at"].as_str(), w["via"].as_str()), (Some("package.json:4"), Some("npm run deploy at .gitlab-ci.yml:13")));
        let files: Vec<(&str, &str)> = w["files"].as_array().unwrap().iter().map(|f| (f["role"].as_str().unwrap(), f["path"].as_str().unwrap())).collect();
        assert_eq!(files, [("package_json", "package.json"), ("config", "wrangler.toml"), ("entry", "worker/index.ts")]);
        assert_eq!(w["values"][0]["value"], "shop-edge");
        let tf = &chains[2]["steps"][0];
        assert_eq!((tf["action"].as_str(), tf["files"][0]["path"].as_str()), (Some("apply"), Some("infra")));
        assert_eq!((tf["modules"][0]["repo"].as_str(), tf["modules"][0]["confidence"].as_str()), (Some("terraform-aws-site"), Some("unique_name")));
        assert!(a["ci_templates"][0].as_str().unwrap().starts_with("Terraform/Base.latest.gitlab-ci.yml (GitLab built-in, not fetched)"));

        // From the shared template's side: the application deployed with it, in `context` and in `impact`.
        let c = context(".release_spa", 4000, kc_store::skeleton::SourceMode::None, &scope(&templates)).unwrap();
        let apps = &c.env["items"][0]["ansible"]["deploys_apps"];
        assert_eq!((apps["count"].as_u64(), apps["apps"][0]["playbook"].as_str()), (Some(1), Some("aws s3 sync, aws cloudfront create-invalidation")));
        let i = impact_of(&templates, ".release_spa");
        let got: Vec<_> = i.impact.affected.iter().filter(|a| a.edge == "deploy").map(|a| (a.repo.clone(), a.qualified.clone())).collect();
        assert_eq!(got, [(Some("web-shop".to_string()), "release".to_string())]);
        let _ = std::fs::remove_dir_all(&base);
    }

    const LAYERS: &str = "public class Repo\n{\n    public void Save() { }\n}\n\
public class Order { }\n\
public class Service\n{\n    private Repo _r;\n    public void Place()\n    {\n        _r.Save();\n        var o = new Order();\n    }\n}\n\
";
    const CONTROLLER: &str = "public class Controller\n{\n    private Service _s;\n    public void Post()\n    {\n        _s.Place();\n    }\n}\n";

    #[test]
    fn walks_keep_only_the_edges_and_kinds_asked_for() {
        let cache = test_cache();
        let base = cache.dir("walk-filter");
        let root = repo(&base, "app", &[("src/Layers.cs", LAYERS), ("src/Controller.cs", CONTROLLER)]);
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let names = |t: &Trace| t.nodes.iter().skip(1).map(|n| n.qualified.clone()).collect::<Vec<_>>();

        let all = trace("Service.Place", "out", 1, 50, &WalkFilter::default(), &scope).unwrap();
        assert_eq!(names(&all.data.found[0].1), ["Repo.Save", "Order"]);
        let calls = WalkFilter { edges: vec!["calls".into()], kinds: Vec::new(), ..Default::default() };
        let t = trace("Service.Place", "out", 1, 50, &calls, &scope).unwrap();
        assert_eq!(names(&t.data.found[0].1), ["Repo.Save"]);
        assert_eq!(t.env["filter"]["edges"], json!(["calls"]));

        // Kinds hide symbols but the walk goes through them: from Repo.Save inward, the controller still hangs off
        // the root once Service.Place is hidden.
        let t = trace("Repo.Save", "in", 2, 50, &WalkFilter::default(), &scope).unwrap();
        assert_eq!(names(&t.data.found[0].1), ["Service.Place", "Controller.Post"]);
        let only = WalkFilter { edges: Vec::new(), kinds: vec!["class".into()], ..Default::default() };
        let t = trace("Repo.Save", "in", 2, 50, &only, &scope).unwrap();
        assert!(names(&t.data.found[0].1).is_empty());
        assert_eq!(t.data.hidden, [2]);

        let i = impact(&ImpactParams { symbol: Some("Repo.Save".into()), depth: 2, limit: 50, ..Default::default() }, &scope).unwrap();
        assert_eq!(i.data[0].impact.affected.len(), 2);
        let none = WalkFilter { edges: vec!["instantiates".into()], kinds: Vec::new(), ..Default::default() };
        let i = impact(&ImpactParams { symbol: Some("Repo.Save".into()), depth: 2, limit: 50, filter: none, ..Default::default() }, &scope).unwrap();
        assert!(i.data[0].impact.affected.is_empty());
        assert!(trace("Repo.Save", "in", 1, 50, &WalkFilter { edges: vec!["callz".into()], kinds: Vec::new(), ..Default::default() }, &scope).is_err());
        assert!(trace("Repo.Save", "in", 1, 50, &WalkFilter { edges: Vec::new(), kinds: vec!["bogus".into()], ..Default::default() }, &scope).is_err());
        // At the node limit, the neighbours left out are counted.
        let t = trace("Repo.Save", "in", 2, 2, &WalkFilter::default(), &scope).unwrap();
        assert!(t.data.found[0].1.truncated && t.data.found[0].1.omitted >= 1);
        assert_eq!(t.env["truncated"], true);

        // The tree reads each relation from its parent.
        let t = trace("Repo.Save", "in", 2, 50, &WalkFilter::default(), &scope).unwrap();
        let text = crate::render::trace_text(&root, &t.data.found[0].1, false);
        assert!(text.contains("\n  called by method Service.Place"), "{text}");
        assert!(text.contains("\n    called by method Controller.Post"), "{text}");
        let i = impact(&ImpactParams { symbol: Some("Repo.Save".into()), depth: 2, limit: 50, ..Default::default() }, &scope).unwrap();
        let text = crate::render::impact_text(&i.data, false);
        assert!(text.contains("\n  called by method Service.Place"), "{text}");
        assert!(text.contains("\n    called by method Controller.Post"), "{text}");

        // The reference graph's pass gives the file edges the overview and hotspots use, unchanged.
        let store = kc_index::open(&root).unwrap();
        let direct = query::file_edges(store.conn()).unwrap();
        assert!(!direct.is_empty());
        assert_eq!(serde_json::to_value(store.file_edges().unwrap()).unwrap(), serde_json::to_value(&direct).unwrap());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn impact_ranks_dependents_within_a_hop_by_their_files() {
        let cache = test_cache();
        let base = cache.dir("impact-rank");
        // Seven callers of Core.Run, one per file; the last by path calls it four times, so it ranks first.
        let mut files: Vec<(String, String)> = vec![("src/Core.cs".into(), "public class Core\n{\n    public void Run() { }\n}\n".into())];
        for c in ["A", "B", "C", "D", "E", "F"] {
            files.push((format!("src/{c}.cs"), format!("public class {c}\n{{\n    private Core _c;\n    public void Go() {{ _c.Run(); }}\n}}\n")));
        }
        files.push((
            "src/Z.cs".into(),
            "public class Z\n{\n    private Core _c;\n    public void Go() { _c.Run(); _c.Run(); }\n    public void Again() { _c.Run(); _c.Run(); }\n}\n".into(),
        ));
        let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let root = repo(&base, "app", &files);
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let order = |o: ImpactOrder| -> Vec<String> {
            let p = ImpactParams { symbol: Some("Core.Run".into()), depth: 1, limit: 50, order: o, ..Default::default() };
            impact(&p, &scope).unwrap().data[0].impact.affected.iter().map(|a| a.file.clone()).collect()
        };
        let hop = order(ImpactOrder::Hop);
        assert_eq!(hop.first().map(String::as_str), Some("src/A.cs"));
        let ranked = order(ImpactOrder::Rank);
        assert_eq!(ranked.first().map(String::as_str), Some("src/Z.cs"), "{ranked:?}");
        let (mut a, mut b) = (hop.clone(), ranked.clone());
        a.sort();
        b.sort();
        assert_eq!(a, b, "ranking reorders, never adds or drops");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Razor files aren't read: a C# answer from a repo holding them says so, naming one, rather than passing its
    /// references off as all of them. An answer about other code doesn't.
    #[test]
    fn a_csharp_answer_says_razor_files_were_not_read() {
        let cache = test_cache();
        let base = cache.dir("razor-unread");
        let root = repo(
            &base,
            "web",
            &[
                ("src/Catalog.cs", "public class Catalog\n{\n    public int Count() => 1;\n}\npublic class Home\n{\n    public int Show(Catalog c) => c.Count();\n}\n"),
                ("src/Pages/Index.razor", "@inject Catalog Catalog\n<p>@Catalog.Count()</p>\n"),
                ("src/Views/Home.cshtml", "<p>@Model.Count()</p>\n"),
                ("src/app.ts", "export function count(): number { return 1; }\nexport function show(): number { return count(); }\n"),
            ],
        );
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let razor = |env: &Value| crate::contract::incomplete_of(env).into_iter().find(|i| i.detail.contains("Razor files"));
        let r = refs("Count", &[], 20, &scope).unwrap();
        let entry = razor(&r.env).expect("said");
        assert_eq!(entry.reason, Reason::Unsupported);
        assert_eq!(
            entry.detail,
            "2 Razor files (.razor, .cshtml) not read, such as src/Pages/Index.razor: uses of C# code in their markup and @code blocks are not listed"
        );
        let i = impact(&ImpactParams { symbol: Some("Catalog.Count".into()), depth: 1, limit: 20, ..Default::default() }, &scope).unwrap();
        assert!(razor(&i.env).is_some(), "{}", i.env);
        // TypeScript: nothing to say about Razor.
        assert!(razor(&refs("count", &[], 20, &scope).unwrap().env).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A name both the code and a test helper define: the code's definition answers, whatever kind each is; the test
    /// helper is listed among the alternatives.
    #[test]
    fn a_test_helper_sharing_a_name_never_answers_for_the_code() {
        let cache = test_cache();
        let base = cache.dir("test-helper-name");
        let root = repo(
            &base,
            "store",
            &[
                ("src/vanilla.ts", "const createStoreImpl = (init: () => number) => ({ get: init });\nexport const createStore = ((init) => (init ? createStoreImpl(init) : createStoreImpl)) as CreateStore;\n"),
                ("tests/helpers.ts", "export function createStore() {\n  return null;\n}\n"),
            ],
        );
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let c = context("createStore", 2000, kc_store::skeleton::SourceMode::Auto, &scope).unwrap();
        let item = &c.env["items"][0];
        assert_eq!(item["symbol"]["file"], "src/vanilla.ts", "{}", c.env);
        assert!(item["alternatives"].as_array().is_some_and(|a| a.iter().any(|d| d["file"] == "tests/helpers.ts")), "{}", c.env);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn context_shows_a_skeleton_with_call_sites_and_says_when_nothing_calls_it() {
        let cache = test_cache();
        let base = cache.dir("skeleton");
        let mut body = String::from("public class Store\n{\n    public int Save() { return 1; }\n}\n\
public class Worker\n{\n    private Store _s;\n    /// Does the work.\n    public int Work(int n)\n    {\n");
        for i in 0..70 {
            body.push_str(&format!("        var v{i} = n * {i};\n"));
        }
        body.push_str("        if (n > 3)\n        {\n            throw new System.Exception(\"big\");\n        }\n        var saved = _s.Save();\n        return saved;\n    }\n}\n\
public class Caller\n{\n    private Worker _w;\n    public void Run() { _w.Work(2); }\n}\n\
public class Orphans\n{\n    public void Unused() { }\n    public string Name() { return \"Unused\"; }\n    public void Other(dynamic d) { d.Unused(); }\n}\n");
        let root = repo(&base, "app", &[("src/Work.cs", &body)]);
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        use kc_store::skeleton::SourceMode;

        let c = context("Worker.Work", 2000, SourceMode::Skeleton, &scope).unwrap();
        let item = &c.env["items"][0];
        assert_eq!(item["source_mode"], "skeleton");
        let src = item["source"].as_str().unwrap();
        assert!(src.starts_with("    public int Work(int n)\n    {\n"), "{src}");
        assert!(src.contains("var saved = _s.Save();") && src.contains("return saved;") && src.contains("throw new System.Exception"), "{src}");
        assert!(!src.contains("var v7 = n * 7;") && src.contains('…'), "{src}");
        assert!(item["source_lines"].as_str().unwrap().starts_with("9-10,"), "{}", item["source_lines"]);
        // Callers quote their line, cited; without cites, as `code`.
        assert_eq!(item["callers"][0]["cite"]["text"], "public void Run() { _w.Work(2); }");
        assert!(item["callers"][0].get("code").is_none(), "{}", item["callers"][0]);
        kc_store::limits::current().set_cite(kc_store::answer::CiteMode::None);
        let c = context("Worker.Work", 2000, SourceMode::Skeleton, &scope).unwrap();
        kc_store::limits::current().set_cite(kc_store::answer::CiteMode::default());
        assert_eq!(c.env["items"][0]["callers"][0]["code"], "public void Run() { _w.Work(2); }");
        assert!(c.env["items"][0]["callers"][0].get("cite").is_none());

        // Auto: the long method is a skeleton, the short one full; none leaves the source out.
        let c = context("Worker.Work", 2000, SourceMode::Auto, &scope).unwrap();
        assert_eq!(c.env["items"][0]["source_mode"], "skeleton");
        let c = context("Store.Save", 2000, SourceMode::Auto, &scope).unwrap();
        assert_eq!(c.env["items"][0]["source_mode"], "full");
        let c = context("Store.Save", 2000, SourceMode::None, &scope).unwrap();
        assert_eq!(c.env["items"][0]["source"], "");

        // No reference resolves to Orphans.Unused: said so.
        let c = context("Orphans.Unused", 2000, SourceMode::Auto, &scope).unwrap();
        let note = c.env["items"][0]["note"].as_str().unwrap().to_string();
        assert!(note.starts_with("no reference resolves to Orphans.Unused"), "{note}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn manifest_sites_compact_into_runs_per_file() {
        let sites = [("package.json", 120), ("package.json", 119), ("a/a.csproj", 21), ("package.json", 121), ("a/a.csproj", 15), ("a/a.csproj", 16)];
        assert_eq!(compact_sites(&sites), ["package.json:119-121", "a/a.csproj:15-16,21"]);
        assert_eq!(compact_sites(&[("x.csproj", 3), ("x.csproj", 3)]), ["x.csproj:3"]);
    }

    #[test]
    fn kinds_take_each_languages_word() {
        assert_eq!(kind_word("protocol", "swift"), "protocol");
        assert_eq!(kind_word("init", "swift"), "init");
        assert_eq!(kind_word("protocol", "typescript"), "interface");
        assert_eq!(kind_word("protocol", "csharp"), "interface");
        assert_eq!(kind_word("protocol", "rust"), "trait");
        assert_eq!(kind_word("init", "csharp"), "constructor");
        assert_eq!(kind_word("typealias", "typescript"), "type");
        assert_eq!(kind_word("class", "csharp"), "class");
        assert_eq!(file_lang("src/app/x.service.ts"), "typescript");
        let langs = |v: Value| repo_lang(v.as_object().unwrap());
        assert_eq!(langs(json!({ "html": 300, "typescript": 200, "npm": 1 })), "typescript");
        assert_eq!(langs(json!({ "csharp": 150, "swift": 1 })), "swift");
        assert_eq!(langs(json!({ "sql": 20 })), "other");
        // The words filter too.
        assert!(query::kind_matches(Some("interface"), "protocol") && query::kind_matches(Some("constructor"), "init"));
    }

    #[test]
    fn the_overview_map_lists_a_records_positional_parameters_once() {
        let cache = test_cache();
        let base = cache.dir("map-record");
        let root = repo(
            &base,
            "app",
            &[
                ("src/Dtos.cs", "namespace App;\npublic record Result(string Name, bool Passed);\n"),
                ("src/A.cs", "namespace App;\npublic class A { public bool F(Result r) => r.Passed; }\n"),
                ("src/B.cs", "namespace App;\npublic class B { public string G(Result r) => r.Name; }\n"),
            ],
        );
        let o = overview(&Scope { repos: vec![root], no_sync: false }, None, 800).unwrap().env;
        let files = o["items"][0]["map"]["files"].as_array().unwrap();
        let dtos = files.iter().find(|f| f["file"] == "src/Dtos.cs").expect("Dtos.cs in the map");
        let lines: Vec<u64> = dtos["symbols"].as_array().unwrap().iter().map(|s| s["line"].as_u64().unwrap()).collect();
        assert_eq!(lines, [2], "{dtos}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn overview_groups_a_dependency_repo_and_deps_lists_each_import_site_once() {
        let cache = test_cache();
        let base = cache.dir("deps-group");
        repo(
            &base,
            "ui",
            &[
                ("packages/button/package.json", "{\n  \"name\": \"@acme/ui-button\",\n  \"version\": \"1.0.0\"\n}\n"),
                ("packages/chip/package.json", "{\n  \"name\": \"@acme/ui-chip\",\n  \"version\": \"1.0.0\"\n}\n"),
                ("packages/button/index.ts", "export class Button {}\n"),
                ("packages/chip/index.ts", "export class Chip {}\n"),
            ],
        );
        let app = repo(
            &base,
            "app",
            &[
                (
                    "package.json",
                    "{\n  \"name\": \"app\",\n  \"dependencies\": {\n    \"@acme/ui-button\": \"1.0.0\",\n    \"@acme/ui-chip\": \"1.0.0\"\n  }\n}\n",
                ),
                ("src/a.ts", "import { Button } from '@acme/ui-button';\nexport interface IState { b: Button }\n"),
                ("src/b.ts", "import { Chip } from '@acme/ui-chip';\nexport class Holder { constructor(public c: Chip) {} }\n"),
            ],
        );
        let scope = Scope { repos: vec![app.clone()], no_sync: false };

        let o = overview(&scope, None, 0).unwrap().env;
        let r = &o["items"][0];
        assert_eq!(r["depends_on"], json!([{ "repo": "ui", "dependencies": 2, "sites": ["package.json:4-5"], "products": [] }]));
        assert!(r.get("swiftui_views").is_none(), "no Swift, no SwiftUI count: {r}");

        let d = deps(&scope, true).unwrap();
        let sites: Vec<(&str, &[String])> = d.data.edges.iter().map(|(_, e)| (e.dependency.as_str(), e.import_sites.as_slice())).collect();
        assert_eq!(
            sites,
            [
                ("@acme/ui-button", &["src/a.ts:1 import @acme/ui-button".to_string()][..]),
                ("@acme/ui-chip", &["src/b.ts:1 import @acme/ui-chip".to_string()][..]),
            ]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn areas_split_a_repo_by_feature_and_context_and_overview_say_which_area_holds_a_symbol() {
        let cache = test_cache();
        let base = cache.dir("areas");
        let feature = |dir: &str, noun: &str| -> Vec<(String, String)> {
            vec![
                (format!("src/{dir}/{noun}.cs"), format!("namespace Shop.{dir};\npublic class {noun} {{ public int Id; }}\n")),
                (
                    format!("src/{dir}/{noun}Repository.cs"),
                    format!("namespace Shop.{dir};\npublic class {noun}Repository {{ public {noun} Get(int id) {{ return new {noun}(); }} }}\n"),
                ),
                (
                    format!("src/{dir}/{noun}Service.cs"),
                    format!(
                        "namespace Shop.{dir};\npublic class {noun}Service {{\n  private readonly {noun}Repository _repo;\n  public {noun} Find(int id) {{ return _repo.Get(id); }}\n}}\n"
                    ),
                ),
                (
                    format!("src/{dir}/{noun}Controller.cs"),
                    format!(
                        "namespace Shop.{dir};\npublic class {noun}Controller {{\n  private readonly {noun}Service _svc;\n  public {noun} Show(int id) {{ return _svc.Find(id); }}\n}}\n"
                    ),
                ),
            ]
        };
        let mut files = feature("Orders", "Order");
        files.extend(feature("Billing", "Invoice"));
        let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let root = repo(&base, "shop", &refs);
        let scope = Scope { repos: vec![root.clone()], no_sync: false };

        let out = areas(None, &scope).unwrap();
        let a = &out.data[0].areas;
        let mut names: Vec<&str> = a.areas.iter().map(|x| x.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["Billing", "Orders"], "{:?}", a.areas);
        let orders = a.areas.iter().find(|x| x.name == "Orders").unwrap();
        assert_eq!(orders.files.len(), 4);
        assert!(orders.files.iter().all(|f| f.starts_with("src/Orders/")));
        // The entity is what the rest of the area uses most.
        assert_eq!(orders.key_files.first().map(String::as_str), Some("src/Orders/Order.cs"), "{:?}", orders.key_files);
        assert!(orders.cohesion > 0.9, "{}", orders.cohesion);

        // --area picks one by name or id, with its files.
        let one = areas(Some("orders"), &scope).unwrap().env;
        assert_eq!(one["items"][0]["areas"][0]["name"], "Orders");
        assert_eq!(one["items"][0]["areas"][0]["members"].as_array().unwrap().len(), 4);
        let none = areas(Some("shipping"), &scope).unwrap().env;
        assert!(none["note"].as_str().unwrap().contains("no area named"), "{none}");

        // context on a symbol, and on a file, names the area.
        let ctx = context("OrderService.Find", 2000, kc_store::skeleton::SourceMode::None, &scope).unwrap().env;
        assert_eq!(ctx["items"][0]["area"]["name"], "Orders", "{ctx}");
        let file = context("src/Billing/Invoice.cs", 2000, kc_store::skeleton::SourceMode::None, &scope).unwrap().env;
        assert_eq!(file["items"][0]["file"], "src/Billing/Invoice.cs");
        assert_eq!(file["items"][0]["area"]["name"], "Billing", "{file}");

        let o = overview(&scope, None, 0).unwrap().env;
        assert_eq!(o["items"][0]["areas_total"], 2);
        assert_eq!(o["items"][0]["areas"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn notes_go_stale_when_the_body_changes_orphan_on_rename_reattach_by_name_and_survive_a_reindex() {
        let cache = test_cache();
        let base = cache.dir("notes");
        let calc = "namespace M;\npublic class Calc {\n  public int Add(int a, int b) {\n    return a + b;\n  }\n  public int Sub(int a, int b) { return a - b; }\n}\n";
        let root = repo(&base, "calc", &[("src/Calc.cs", calc), ("src/Use.cs", "namespace M;\npublic class Use { public int Go(Calc c) { return c.Add(1, 2); } }\n")]);
        commit(&root, "first");
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let status = |sym: Option<&str>| -> Vec<(String, &'static str)> {
            note_list(sym, &scope).unwrap().data.iter().flat_map(|r| r.notes.iter().map(|n| (n.note.symbol.clone(), n.status))).collect()
        };

        let added = note_add("Calc.Add", "overflow is unchecked on purpose", Some("decision"), &scope).unwrap();
        let note = &added.data[0].notes[0];
        assert_eq!((note.note.symbol.as_str(), note.note.kind.as_str(), note.status), ("Calc.Add", "method", "current"));
        assert!(note.note.commit.is_some());
        let id = note.note.id;
        assert!(note_add("Calc.Add", "x", Some("idea"), &scope).is_err(), "unknown tags are refused");
        assert!(note_add("Nope.Nothing", "x", None, &scope).is_err());

        // Moving the method down a line, or trailing whitespace, keeps it current.
        std::fs::write(root.join("src/Calc.cs"), calc.replace("public class Calc {\n", "public class Calc {\n\n").replace("a + b;", "a + b;   ")).unwrap();
        assert_eq!(status(Some("Calc.Add")), [("Calc.Add".to_string(), "current")]);

        // context shows it; editing the body makes it stale, first in the working tree, then since the commit.
        let ctx = context("Calc.Add", 2000, kc_store::skeleton::SourceMode::None, &scope).unwrap().env;
        assert_eq!(ctx["items"][0]["notes"][0]["text"], "overflow is unchecked on purpose", "{ctx}");
        std::fs::write(root.join("src/Calc.cs"), calc.replace("return a + b;", "checked { return a + b; }")).unwrap();
        let listed = note_list(Some("Calc.Add"), &scope).unwrap();
        let v = &listed.data[0].notes[0];
        assert_eq!((v.status, v.stale_since.as_deref(), v.change.as_deref()), ("stale", Some("working tree"), Some("+1 −1 lines")));
        commit(&root, "checked");
        let head = std::process::Command::new("git").arg("-C").arg(&root).args(["log", "-1", "--format=%h"]).output().unwrap().stdout;
        let sha = String::from_utf8(head).unwrap().trim().to_string();
        let listed = note_list(None, &scope).unwrap();
        assert_eq!(listed.data[0].notes[0].stale_since.as_deref(), Some(sha.as_str()));

        // impact on the method warns about the note on it.
        let p = ImpactParams { symbol: Some("Calc.Add".into()), depth: 2, limit: 100, ..Default::default() };
        let env = impact(&p, &scope).unwrap().env;
        let warn = env["items"][0]["notes"][0].as_str().unwrap();
        assert!(warn.starts_with(&format!("note #{id} on Calc.Add (stale since {sha}): [decision] overflow")), "{warn}");

        // Renamed: orphaned. A symbol with the name again, in another file: re-attached there.
        std::fs::write(root.join("src/Calc.cs"), calc.replace("Add(", "Plus(")).unwrap();
        assert_eq!(status(None), [("Calc.Add".to_string(), "orphaned")]);
        assert_eq!(status(Some("Calc.Add")), [("Calc.Add".to_string(), "orphaned")], "an orphan is found by its old name");
        std::fs::write(root.join("src/More.cs"), "namespace M;\npublic partial class Calc {\n  public int Add(int a, int b) {\n    return a + b;\n  }\n}\n").unwrap();
        let listed = note_list(None, &scope).unwrap();
        let v = &listed.data[0].notes[0];
        assert_eq!((v.status, v.moved_from.as_deref(), v.now_at.as_deref()), ("current", Some("src/Calc.cs"), Some("src/More.cs:3")));

        // The index is dropped and rebuilt: the note is kept apart from it.
        assert!(kc_index::forget(&root).unwrap());
        assert_eq!(status(None), [("Calc.Add".to_string(), "current")]);

        assert_eq!(note_rm(id, &scope).unwrap(), [root]);
        assert!(status(None).is_empty());
        assert!(note_rm(id, &scope).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn selectors_name_namespaces_and_modules_and_answers_tell_same_names_apart() {
        let cache = test_cache();
        let base = cache.dir("ns-identity");
        let root = repo(
            &base,
            "app",
            &[
                ("a.cs", "namespace Acme.A;\npublic class Worker { public void Run() {} }\n"),
                ("b.cs", "namespace Acme.B;\npublic class Worker { public void Run() {} }\n"),
                ("src/one/job.ts", "export class Job { run() {} }\n"),
                ("src/two/job.ts", "export class Job { run() {} }\n"),
                ("pkg/alpha.py", "class Task:\n    def go(self):\n        pass\n"),
                ("pkg/beta.py", "class Task:\n    def go(self):\n        pass\n"),
            ],
        );
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let found = |q: &str| -> Vec<(String, Option<String>)> {
            let f = find(q, None, 10, &scope).unwrap();
            f.data.items.iter().map(|(_, h)| (h.symbol.file.clone(), h.symbol.namespace.clone())).collect()
        };
        // A namespace, whole or by its last components; a module by its path or its end; a Python dotted module.
        for (q, file, ns) in [
            ("Acme.B.Worker.Run", "b.cs", "Acme.B"),
            ("B.Worker.Run", "b.cs", "Acme.B"),
            ("A.Worker", "a.cs", "Acme.A"),
            ("src/two/job.Job.run", "src/two/job.ts", "src/two/job"),
            ("two/job.Job.run", "src/two/job.ts", "src/two/job"),
            ("pkg.beta.Task.go", "pkg/beta.py", "pkg.beta"),
        ] {
            assert_eq!(found(q), [(file.to_string(), Some(ns.to_string()))], "{q}");
        }
        assert!(found("C.Worker.Run").is_empty());
        // Unqualified: both, each with its namespace, since their qualified names are the same.
        let both = found("Worker.Run");
        assert_eq!(both, [("a.cs".to_string(), Some("Acme.A".to_string())), ("b.cs".to_string(), Some("Acme.B".to_string()))]);
        // A name only one definition has needs no namespace.
        let one = repo(&base, "one", &[("a.cs", "namespace Acme.A;\npublic class Solo { public void Run() {} }\n")]);
        let f = find("Solo.Run", None, 10, &Scope { repos: vec![one], no_sync: false }).unwrap();
        assert_eq!(f.data.items[0].1.symbol.namespace, None);
        // context picks the named one, and lists the other with its namespace.
        let ctx = context("B.Worker.Run", 2000, kc_store::skeleton::SourceMode::None, &scope).unwrap().env;
        assert_eq!(ctx["items"][0]["symbol"]["file"], "b.cs", "{ctx}");
        let ctx = context("Worker.Run", 2000, kc_store::skeleton::SourceMode::None, &scope).unwrap().env;
        assert_eq!((&ctx["items"][0]["symbol"]["namespace"], &ctx["items"][0]["alternatives"][0]["namespace"]), (&json!("Acme.A"), &json!("Acme.B")), "{ctx}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn notes_keep_their_declaration_identity_and_never_move_to_a_same_named_one() {
        let cache = test_cache();
        let base = cache.dir("note-identity");
        let a = "namespace A;\npublic class Worker { public void Run() {} }\n";
        let b = "namespace B;\npublic class Worker { public void Run() {} }\n";
        let overloads = "namespace C;\npublic class Saver {\n  public void Save(int n) {}\n  public void Save(string s) {}\n}\n";
        let root = repo(&base, "app", &[("a.cs", a), ("b.cs", b), ("c.cs", overloads)]);
        commit(&root, "first");
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let view = |id: i64| -> kc_index::notes::NoteView {
            note_list(None, &scope).unwrap().data.iter().flat_map(|r| r.notes.clone()).find(|n| n.note.id == id).unwrap()
        };
        let id_a = note_add("a.cs#Worker.Run", "A-only invariant", None, &scope).unwrap().data[0].notes[0].note.id;
        assert_eq!(view(id_a).note.namespace.as_deref(), Some("A"));
        // B's context doesn't show A's note.
        let ctx = context("B.Worker.Run", 2000, kc_store::skeleton::SourceMode::None, &scope).unwrap().env;
        assert!(ctx["items"][0].get("notes").is_none_or(|n| n.as_array().is_some_and(|n| n.is_empty())), "{ctx}");

        // A's method goes: B's has the same qualified name and an identical body, yet the note is orphaned, with B's
        // as a candidate.
        std::fs::remove_file(root.join("a.cs")).unwrap();
        let v = view(id_a);
        assert_eq!((v.status, v.now_at.as_deref(), v.candidates.as_slice()), ("orphaned", None, ["b.cs:2 B.Worker.Run".to_string()].as_slice()));
        assert!(note_list(Some("B.Worker.Run"), &scope).unwrap().data.is_empty(), "not a note on B's method");
        // Moved to another file in its namespace, it is the same declaration: re-attached.
        std::fs::write(root.join("moved.cs"), a).unwrap();
        let v = view(id_a);
        assert_eq!((v.status, v.moved_from.as_deref(), v.now_at.as_deref()), ("current", Some("a.cs"), Some("moved.cs:2")));

        // Overloads: a note on one never moves to another.
        let id_int = note_add("c.cs:3", "n is a count", None, &scope).unwrap().data[0].notes[0].note.id;
        std::fs::write(root.join("c.cs"), overloads.replace("  public void Save(int n) {}\n", "")).unwrap();
        assert_eq!(view(id_int).status, "orphaned");

        // A note written before identities were recorded is kept: its identity is recorded from the declaration of
        // its name in its file; one whose declaration is gone says its identity is unknown, and stays put.
        let db = rusqlite::Connection::open(kc_index::notes::db_path().unwrap()).unwrap();
        let legacy = |symbol: &str, file: &str| -> i64 {
            db.execute(
                "INSERT INTO notes(repo, symbol, kind, file, line, body_hash, created, text) VALUES (?1, ?2, 'method', ?3, 2, 'x', 0, 'old')",
                rusqlite::params![root.to_string_lossy(), symbol, file],
            )
            .unwrap();
            db.last_insert_rowid()
        };
        let (kept, lost) = (legacy("Worker.Run", "b.cs"), legacy("Worker.Run", "gone.cs"));
        let v = view(kept);
        assert_eq!((v.status, v.identity, v.note.namespace.as_deref(), v.now_at.as_deref()), ("stale", None, Some("B"), Some("b.cs:2")));
        let v = view(lost);
        assert_eq!((v.status, v.identity), ("orphaned", Some("unknown")));
        assert_eq!(v.candidates.len(), 2, "{:?}", v.candidates);
        let lang: Option<String> = db.query_row("SELECT lang FROM notes WHERE id = ?1", [kept], |r| r.get(0)).unwrap();
        assert_eq!(lang.as_deref(), Some("csharp"), "recorded once established");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn workspace_refs_and_tables_open_only_the_repos_their_name_can_be_in() {
        let cache = test_cache();
        let base = cache.dir("ws-names");
        let lib = repo(
            &base,
            "lib",
            &[
                ("src/Shop.Gizmo.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Shop.Gizmo</PackageId></PropertyGroup>\n</Project>\n"),
                ("src/Gizmo.cs", "namespace Shop.Gizmo;\npublic class GizmoWq\n{\n    public void Spin() { }\n    public void Wobble() { }\n    public void Tilt() { }\n}\n"),
            ],
        );
        let app = repo(
            &base,
            "app",
            &[
                ("src/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Shop.Gizmo\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                ("src/Runner.cs", "using Shop.Gizmo;\nnamespace Shop.App;\npublic class Runner\n{\n    public void Run() { new GizmoWq().Spin(); }\n}\n"),
            ],
        );
        let data = repo(
            &base,
            "data",
            &[(
                "src/Store.cs",
                "[Table(\"gadget_wq\")]\npublic class GadgetWq { }\npublic class StoreContext : DbContext\n{\n    public DbSet<GadgetWq> GadgetsWq { get; set; }\n}\n",
            )],
        );
        let schema = repo(&base, "schema", &[("src/shop.sql", "-- Host: db1    Database: shop\nCREATE TABLE `gadget_wq` (\n  `id` int NOT NULL\n);\n")]);
        let ops_repo = repo(&base, "ops", &[("roles/deploywq/tasks/main.yml", "- name: Copy\n  copy:\n    src: a\n    dest: b\n")]);
        let other = repo(&base, "other", &[("src/Other.cs", "public class Unrelated\n{\n    public void Spin() { }\n}\n")]);
        let mine = [lib.clone(), app.clone(), data.clone(), schema.clone(), ops_repo.clone(), other.clone()];
        let all = kc_index::indexed_roots().unwrap();
        let kept = |roots: Vec<PathBuf>| -> Vec<String> { roots.iter().filter(|r| mine.contains(r)).map(|r| repo_name(r)).collect() };

        // The repo referencing the name, and the one defining it that the first depends on; not the others.
        assert_eq!(kept(refs_roots(all.clone(), "GizmoWq", &[], false).unwrap()), ["app", "lib"]);
        assert_eq!(kept(refs_roots(all.clone(), "GizmoWq.Spin", &[], false).unwrap()), ["app", "lib"]);
        assert!(kept(refs_roots(all.clone(), "NoSuchNameWq", &[], false).unwrap()).is_empty());
        assert_eq!(kept(refs_roots(all.clone(), "deploywq", &[], false).unwrap()), ["ops"]);
        assert!(kept(refs_roots(all.clone(), "deploywq", &["call"], false).unwrap()).is_empty());
        // The same answer as reading every repo.
        let scope = |repos: Vec<PathBuf>| Scope { repos, no_sync: true };
        let only = |keep: Vec<PathBuf>| mine.iter().filter(|r| keep.contains(r)).cloned().collect::<Vec<_>>();
        for name in ["GizmoWq", "GizmoWq.Spin", "Spin"] {
            let every = refs(name, &[], 100, &scope(mine.to_vec())).unwrap();
            let some = refs(name, &[], 100, &scope(only(refs_roots(all.clone(), name, &[], false).unwrap()))).unwrap();
            assert_eq!(every.env["items"], some.env["items"], "{name}");
            assert_eq!((&every.env["total"], &every.env["by_confidence"]), (&some.env["total"], &some.env["by_confidence"]), "{name}");
            assert!(every.data.total > 0, "{name}");
        }
        // The dependent's reference, found from the defining repo, is why `lib` is opened.
        let from_lib = refs("GizmoWq", &[], 100, &scope(vec![lib.clone()])).unwrap();
        assert!(from_lib.data.items.iter().any(|(_, h)| h.repo.as_deref() == Some("app")), "{}", from_lib.env);

        // Tables: by table, entity, DbSet or SQL name, any case, with or without a schema.
        let tables_kept = |n: &str| kept(workspace_roots(all.clone(), false, |s| s.may_name_table(n)).unwrap());
        assert_eq!(tables_kept("gadget_wq"), ["data", "schema"]);
        assert_eq!(tables_kept("Gadget_WQ"), ["data", "schema"]);
        assert_eq!(tables_kept("shop.gadget_wq"), ["data", "schema"]);
        assert_eq!(tables_kept("GadgetWq"), ["data"]);
        assert_eq!(tables_kept("gadgetswq"), ["data"]);
        assert!(tables_kept("NoSuchTableWq").is_empty());
        for n in ["gadget_wq", "GadgetWq", "shop.gadget_wq"] {
            let every = tables(Some(n), &scope(mine.to_vec())).unwrap();
            let some = tables(Some(n), &scope(only(workspace_roots(all.clone(), false, |s| s.may_name_table(n)).unwrap()))).unwrap();
            assert_eq!((&every.env["items"], &every.env["sql"]), (&some.env["items"], &some.env["sql"]), "{n}");
            assert!(!every.data.models.is_empty() || !every.data.sql.is_empty(), "{n}");
        }

        // A summary rules a repo out only while the repo is as it was when the summary was written. A reference added
        // since (an edit in place, no new file) is found by the next query, which syncs the changed repo first.
        assert!(kept(refs_roots(all.clone(), "Wobble", &[], false).unwrap()).is_empty());
        let runner = app.join("src/Runner.cs");
        let text = std::fs::read_to_string(&runner).unwrap();
        std::fs::write(&runner, text.replace("new GizmoWq().Spin();", "new GizmoWq().Spin(); new GizmoWq().Wobble();")).unwrap();
        assert!(kept(refs_roots(all.clone(), "Wobble", &[], true).unwrap()).is_empty(), "--no-sync answers from the summaries as they are");
        assert_eq!(kept(refs_roots(all.clone(), "Wobble", &[], false).unwrap()), ["app", "lib"]);
        let ws = refs_in("Wobble", &[], 100, all.clone(), true, false).unwrap();
        assert!(ws.data.items.iter().any(|(r, h)| *r == app && h.file == "src/Runner.cs"), "{}", ws.env);
        // The same for a dependent, looked up from the defining repo.
        let text = std::fs::read_to_string(&runner).unwrap();
        std::fs::write(&runner, text.replace("new GizmoWq().Wobble();", "new GizmoWq().Wobble(); new GizmoWq().Tilt();")).unwrap();
        let from_lib = refs("GizmoWq.Tilt", &[], 100, &Scope { repos: vec![lib.clone()], no_sync: false }).unwrap();
        assert!(from_lib.data.items.iter().any(|(_, h)| h.repo.as_deref() == Some("app") && h.file == "src/Runner.cs"), "{}", from_lib.env);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A file that can't be read leaves its facts as last indexed: the envelope says which, its status is
    /// `incomplete`, and every item drawn from it is marked `stale`.
    #[test]
    fn an_unreadable_file_makes_the_answer_incomplete() {
        use std::os::unix::fs::PermissionsExt;
        let cache = test_cache();
        let base = cache.dir("unreadable");
        let root = repo(&base, "app", &[("src/a.py", "def old_unread_wq():\n    return 1\n"), ("src/b.py", "def fine_unread_wq():\n    return 1\n")]);
        let file = root.join("src/a.py");
        std::fs::write(&file, "def new_unread_wq():\n    return 1\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let scope = Scope { repos: vec![root.clone()], no_sync: false };
        let out = find("unread_wq", None, 10, &scope).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let env = out.env;
        assert_eq!(env["freshness"]["failed"], json!(["src/a.py"]), "{env}");
        assert_eq!(env["freshness"]["status"], "incomplete");
        let stale: Vec<(&str, bool)> =
            env["items"].as_array().unwrap().iter().map(|i| (i["name"].as_str().unwrap(), i["stale"] == true)).collect();
        assert_eq!(stale, [("old_unread_wq", true), ("fine_unread_wq", false)]);
        assert_eq!(freshness_notes(&env).len(), 1);
        // Readable again: fresh, and nothing is marked.
        let env = find("unread_wq", None, 10, &scope).unwrap().env;
        assert!(env["freshness"].get("failed").is_none() && env["freshness"].get("status").is_none(), "{env}");
        assert!(env["items"].as_array().unwrap().iter().all(|i| i.get("stale").is_none()));
        assert!(freshness_notes(&env).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Showing source checks it against the index: an edit that kept the file's mtime and size is caught there, and
    /// the file is indexed again before answering (with `--no-sync`, the answer says `source_changed`).
    #[test]
    fn context_reindexes_a_file_whose_source_no_longer_matches() {
        let cache = test_cache();
        let base = cache.dir("same-stat");
        let root = repo(&base, "app", &[("src/a.py", "def old_stat_wq():\n    return 1\n")]);
        let file = root.join("src/a.py");
        let keep_stat = |text: &str| {
            let before = std::fs::metadata(&file).unwrap().modified().unwrap();
            std::fs::write(&file, text).unwrap();
            std::fs::File::options().write(true).open(&file).unwrap().set_modified(before).unwrap();
        };
        keep_stat("def new_stat_wq():\n    return 1\n");
        let no_sync = Scope { repos: vec![root.clone()], no_sync: true };
        let synced = Scope { repos: vec![root.clone()], no_sync: false };
        let env = context("old_stat_wq", 2000, kc_store::skeleton::SourceMode::Full, &no_sync).unwrap().env;
        assert_eq!(env["items"][0]["source_changed"], true, "{env}");
        let env = context("old_stat_wq", 2000, kc_store::skeleton::SourceMode::Full, &synced).unwrap().env;
        assert!(env["items"].as_array().is_none_or(Vec::is_empty), "{env}");
        assert_eq!(env["freshness"]["reparsed"], 1);
        let env = context("new_stat_wq", 2000, kc_store::skeleton::SourceMode::Full, &synced).unwrap().env;
        assert!(env["items"][0].get("source_changed").is_none(), "{env}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A deploy hop across a CI pipeline a limit cut (a matrix past its combinations here) says so in impact and trace:
    /// the chains past the limit are missing.
    #[test]
    fn deploy_hops_say_what_the_ci_limits_cut() {
        let cache = test_cache();
        let base = cache.dir("ci-cut-impact");
        let role = repo(
            &base,
            "Configure.Shop",
            &[
                ("defaults/main.yml", "shop_service: ShopApi\n"),
                ("tasks/main.yml", "- name: Start shop\n  systemd:\n    name: \"{{ shop_service }}\"\n    state: started\n"),
                ("meta/main.yml", "galaxy_info:\n  role_name: configure_shop\ndependencies: []\n"),
            ],
        );
        assert!(std::process::Command::new("git")
            .args(["remote", "add", "origin", "git@git.example.com:Roles/Configure.Shop.git"])
            .current_dir(&role)
            .status()
            .unwrap()
            .success());
        let regions: Vec<String> = (0..40).map(|i| format!("r{i}")).collect();
        let ci = format!("deploy:\n  parallel:\n    matrix:\n      - REGION: [{}]\n  script:\n    - ansible-playbook shop/site.yml\n", regions.join(", "));
        let plays = repo(
            &base,
            "plays",
            &[
                ("shop/requirements.yml", "- src: http://git.example.com/Roles/Configure.Shop.git\n  scm: git\n  name: shop\n"),
                ("shop/site.yml", "---\n- name: Shop hosts\n  hosts: shop\n  roles:\n    - shop\n"),
                (".gitlab-ci.yml", &ci),
            ],
        );
        kc_index::sync(&role, &mut kc_index::open(&role).unwrap(), &SyncOptions { threads: 1, full_discovery: true, ..Default::default() }).unwrap();
        let s = Scope { repos: vec![role.clone()], no_sync: false };
        let p = ImpactParams { symbol: Some("Configure.Shop".into()), depth: 3, limit: 100, ..Default::default() };
        let out = impact(&p, &s).unwrap();
        assert!(out.data[0].impact.affected.iter().any(|a| a.edge == "deploy"), "{}", out.env);
        let cut = out.env["items"][0]["ci_cut"].as_array().cloned().unwrap_or_default();
        assert!(cut.iter().any(|c| c.as_str().is_some_and(|c| c.starts_with("plays: matrix of job deploy"))), "{}", out.env);
        let said = crate::contract::incomplete_of(&out.env);
        assert!(said.iter().any(|i| i.reason == Reason::Capped && i.detail.starts_with("deploy chains: plays: matrix")), "{said:?}");
        let _ = plays;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// History's cuts are said: impact's `coupled` class reads every partner before filtering (a partner ranked past
    /// 100 docs is still found), and `coupled` says how many single-commit partners history keeps no record of.
    #[test]
    fn co_change_limits_never_drop_a_partner_silently() {
        let cache = test_cache();
        let base = cache.dir("cochange-limits");
        let mut files: Vec<(String, String)> = vec![
            ("src/A.cs".into(), "namespace N;\npublic class A { public void Run() { } }\n".into()),
            ("src/B.cs".into(), "namespace N;\npublic class B { public void Other() { } }\n".into()),
        ];
        for i in 0..102 {
            files.push((format!("docs/d{i:03}.md"), "x\n".into()));
        }
        let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let root = repo(&base, "app", &files);
        commit_days_ago(&root, "init", 40);
        let _ = std::fs::create_dir_all(root.join("misc"));
        let edit = |path: &str, n: usize| std::fs::write(root.join(path), format!("{}x{n}\n", std::fs::read_to_string(root.join(path)).unwrap_or_default())).unwrap();
        // 24 commits change A with a third of the docs each (8 times per doc); B changes with A in the 8 oldest: its
        // confidence (8 of 24) passes the class's bar, but 102 docs share as many, more recent, commits.
        for n in 0..24 {
            edit("src/A.cs", n);
            for i in (n % 3..102).step_by(3) {
                edit(&format!("docs/d{i:03}.md"), n);
            }
            if n < 8 {
                edit("src/B.cs", n);
            }
            commit_days_ago(&root, &format!("c{n}"), 30 - n as i64);
        }
        let s = Scope { repos: vec![root.clone()], no_sync: false };
        let p = ImpactParams { symbol: Some("A.Run".into()), depth: 1, limit: 50, ..Default::default() };
        let env = impact(&p, &s).unwrap().env;
        let class: Vec<&str> = env["items"][0]["coupled"].as_array().unwrap().iter().filter_map(|c| c["file"].as_str()).collect();
        assert_eq!(class, ["src/B.cs"], "{}", env["items"][0]["coupled"]);

        // Two commits of A with 49 other files each: 98 single-commit partners, 50 of them kept by the history index.
        for round in 0..2 {
            edit("src/A.cs", 100 + round);
            for i in 0..49 {
                edit(&format!("misc/r{round}_{i:02}.txt"), round);
            }
            commit_days_ago(&root, &format!("wide{round}"), 2 - round as i64);
        }
        let env = coupled("src/A.cs", 500, &vitals::Span::default(), &s).unwrap().env;
        let item = &env["items"][0];
        let unrecorded = item["partners_unrecorded"].as_u64().unwrap_or(0);
        assert!(unrecorded > 0, "{item}");
        let i = crate::contract::incomplete_of(&env).into_iter().find(|i| i.detail.starts_with("partners_unrecorded")).expect("said");
        assert_eq!(i.reason, Reason::Capped);
        assert!(i.hint.unwrap().starts_with("git log"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The links of `context`'s HTTP entries: (role, path, linked repo, symbol, confidence, assumptions, host).
    type LinkRow = (String, String, String, String, String, Vec<String>, Option<String>);

    fn http_links(env: &Value) -> Vec<LinkRow> {
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let mut out = Vec::new();
        for item in env["items"].as_array().unwrap() {
            for h in item["http"].as_array().into_iter().flatten() {
                for l in h["links"].as_array().into_iter().flatten() {
                    let assumptions = l["assumptions"].as_array().map(|a| a.iter().map(s).collect()).unwrap_or_default();
                    out.push((s(&h["role"]), s(&h["path"]), s(&l["repo"]), s(&l["symbol"]), s(&l["confidence"]), assumptions, l["host"]["host"].as_str().map(str::to_string)));
                }
            }
        }
        out
    }

    const CATALOG_API: &str = r#"
namespace Shop.Catalog;
public static class CatalogApi
{
    public static IEndpointRouteBuilder MapCatalogApi(this IEndpointRouteBuilder app)
    {
        var api = app.MapGroup("api/catalog");
        api.MapGet("/items/{id:int}", GetItemById);
        return app;
    }
    public static IResult GetItemById(int id) => null;
}
"#;

    const CATALOG_CLIENT: &str = r#"
namespace Shop.Web;
public class CatalogService(HttpClient httpClient)
{
    private readonly string remoteServiceBaseUrl = "api/catalog/";
    public Task<Item> GetCatalogItem(int id)
    {
        var uri = $"{remoteServiceBaseUrl}items/{id}";
        return httpClient.GetFromJsonAsync<Item>(uri);
    }
}
"#;

    /// A minimal API endpoint (dotnet/eShop's shape) links to the typed client in another repo calling it; the client's
    /// base address names a host one repo's compose file serves, which decides between two backends serving the route.
    /// The method group passed to `MapGet` is a reference to the handler.
    #[test]
    fn minimal_api_endpoints_link_to_their_callers_across_repos() {
        let cache = test_cache();
        let base = cache.dir("minimal-api");
        let api = repo(
            &base,
            "catalog-api",
            &[
                ("src/Apis/CatalogApi.cs", CATALOG_API),
                ("src/Program.cs", "var app = builder.Build();\napp.MapCatalogApi();\napp.Run();\n"),
                ("docker-compose.yml", "services:\n  catalog-api:\n    build: .\n"),
            ],
        );
        let decoy = repo(
            &base,
            "legacy-catalog",
            &[("src/ItemsController.cs", "public class ItemsController : Controller\n{\n    [HttpGet(\"api/catalog/items/{id}\")]\n    public Task Get(int id) => null;\n}\n")],
        );
        let web = repo(
            &base,
            "web",
            &[
                ("src/CatalogService.cs", CATALOG_CLIENT),
                ("src/Program.cs", "builder.Services.AddHttpClient<CatalogService>(o => o.BaseAddress = new(\"http://catalog-api\"));\n"),
            ],
        );
        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };
        let ctx = context("CatalogApi.GetItemById", 4000, Default::default(), &scope(&api)).unwrap().env;
        let item = &ctx["items"][0];
        // `MapGet("/items/{id:int}", GetItemById)` refers to the handler.
        assert_eq!(item["callers_summary"]["total"], 1, "{}", item["callers"]);
        assert_eq!(item["callers"][0]["line"], 8);
        let host = Some("catalog-api".to_string());
        assert_eq!(
            http_links(&ctx),
            [(
                "serves".into(),
                "api/catalog/items/{id:int}".into(),
                "web".into(),
                "CatalogService.GetCatalogItem".into(),
                "exact".into(),
                vec!["configured_host".into()],
                host.clone()
            )]
        );
        // From the client: the host decides; the other backend serving the path is not linked.
        let from = context("CatalogService.GetCatalogItem", 4000, Default::default(), &scope(&web)).unwrap().env;
        let links = http_links(&from);
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!((links[0].2.as_str(), links[0].3.as_str(), links[0].6.clone()), ("catalog-api", "CatalogApi.GetItemById", host));
        assert!(ctx["complete"].as_bool().unwrap(), "{}", ctx["incomplete"]);
        let _ = decoy;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A request class declaring its path and verb in an attribute is a client route; the host the client repo's
    /// settings name, which a load balancer routes to a backend one repo's compose file serves, makes that backend
    /// the dominant one of two serving the route.
    #[test]
    fn attribute_contracts_link_through_configured_hosts() {
        let cache = test_cache();
        let base = cache.dir("attribute-contracts");
        let client = repo(
            &base,
            "orders-client",
            &[
                (
                    "src/Contracts/GetOrder.cs",
                    "[Endpoint(\"/orders\", HttpMethod.Get)]\npublic class GetOrder\n{\n    [PathSegment(1)]\n    public string OrderId { get; set; }\n}\n",
                ),
                ("src/Client.cs", "public class Client\n{\n    public Task Load(Sender s) => s.SendAsync(new GetOrder { OrderId = \"1\" });\n}\n"),
                ("src/appsettings.json", "{\n  \"Orders\": { \"BaseUrl\": \"https://shop.example.test/\" }\n}\n"),
            ],
        );
        let controller = "public class OrdersController : Controller\n{\n    [HttpGet(\"orders/{orderId}\")]\n    public Task Get(string orderId) => null;\n}\n";
        repo(&base, "orders-api", &[("src/OrdersController.cs", controller), ("docker-compose.yml", "services:\n  orders-api:\n    build: .\n")]);
        repo(&base, "orders-archive", &[("src/OrdersController.cs", controller)]);
        repo(
            &base,
            "loadbalancer",
            &[("cfgs/prd/haproxy.cfg", "frontend public\n    acl host_shop hdr(host) -i shop.example.test\n    use_backend Shop if host_shop\n\nbackend Shop\n    server s1 orders-api:8080 check\n")],
        );
        let scope = |r: &Path| Scope { repos: vec![r.to_path_buf()], no_sync: false };
        let out = routes(&scope(&client)).unwrap().env;
        let items = out["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!((items[0]["verb"].as_str(), items[0]["path"].as_str(), items[0]["symbol"].as_str()), (Some("GET"), Some("orders/{OrderId}"), Some("GetOrder")));
        let links: Vec<(&str, &str, Option<&str>)> =
            items[0]["links"].as_array().unwrap().iter().map(|l| (l["repo"].as_str().unwrap(), l["confidence"].as_str().unwrap(), l["host"]["server"].as_str())).collect();
        assert_eq!(links.len(), 2, "{links:?}");
        assert_eq!((links[0].0, links[0].1), ("orders-api", "dominant"));
        let via = links[0].2.unwrap();
        assert!(via.starts_with("cfgs/prd/haproxy.cfg:3 (proxy), to orders-api, which orders-api serves"), "{via}");
        assert_eq!((links[1].0, links[1].1, links[1].2), ("orders-archive", "ambiguous", None));
        assert_eq!(items[0]["links"][0]["assumptions"], json!(["configured_host"]));
        // The method sending the contract requests the route too.
        let sends = context("Client.Load", 4000, Default::default(), &scope(&client)).unwrap().env;
        assert_eq!(http_links(&sends).first().map(|l| (l.0.as_str(), l.2.as_str())), Some(("requests", "orders-api")));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A project whose implicit usings can't be read (an SDK not known) makes `context` incomplete only for symbols
    /// it may reference; the test and app host SDKs' usings are known.
    #[test]
    fn unread_implicit_usings_bear_only_on_symbols_the_project_may_reference() {
        let cache = test_cache();
        let base = cache.dir("implicit-usings");
        let proj = |sdk: &str| format!("<Project Sdk=\"{sdk}\">\n  <PropertyGroup>\n    <ImplicitUsings>enable</ImplicitUsings>\n  </PropertyGroup>\n</Project>\n");
        let app = repo(
            &base,
            "shop",
            &[
                ("src/Core/Core.csproj", &proj("Microsoft.NET.Sdk")),
                ("src/Core/Orders.cs", "namespace Shop.Core;\npublic class Orders { public void Place() { } }\npublic class Billing { public void Charge() { } }\n"),
                ("src/Odd/Odd.csproj", &proj("Contoso.Custom.Sdk/1.0.0")),
                ("src/Odd/Uses.cs", "namespace Shop.Odd;\npublic class Uses { public void Run(Shop.Core.Orders o) { o.Place(); } }\n"),
                ("tests/Unit/Unit.csproj", &proj("MSTest.Sdk/4.0.2")),
                ("tests/Unit/Tests.cs", "namespace Shop.Tests;\n[TestClass]\npublic class BillingTests { [TestMethod] public void Charges() => new Shop.Core.Billing().Charge(); }\n"),
                ("src/AppHost/AppHost.csproj", &proj("Aspire.AppHost.Sdk/13.6.0")),
            ],
        );
        let scope = Scope { repos: vec![app], no_sync: false };
        let imports = |s: &str| -> Vec<String> {
            let env = context(s, 4000, Default::default(), &scope).unwrap().env;
            env["incomplete"].as_array().into_iter().flatten().filter_map(|i| i["detail"].as_str()).filter(|d| d.starts_with("imports")).map(str::to_string).collect()
        };
        // The unknown SDK's project calls `Place`: its imports bear on it.
        let place = imports("Orders.Place");
        assert!(place.len() == 1 && place[0].contains("Contoso.Custom.Sdk") && !place[0].contains("MSTest"), "{place:?}");
        // Nothing there names `Charge`; MSTest.Sdk's and Aspire.AppHost.Sdk's usings are known.
        assert_eq!(imports("Billing.Charge"), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Registrations KnowCode recognises but can't place make the answers that depend on them say `unsupported`:
    /// `routes` for the repo, `context` on the handler, and `context` on an action conventional routes may serve (a
    /// template that isn't a literal; a literal one places the actions).
    #[test]
    fn unplaced_endpoint_registrations_are_unsupported_not_complete() {
        let cache = test_cache();
        let base = cache.dir("unsupported-routes");
        let app = repo(
            &base,
            "legacy-web",
            &[
                (
                    "src/Startup.cs",
                    "public class Startup\n{\n    public void Configure(IApplicationBuilder app, string prefix)\n    {\n        app.UseEndpoints(e =>\n        {\n            e.MapControllerRoute(\"default\", prefix + \"/{controller}/{action}\");\n            e.MapGet(prefix + \"/ping\", Ping);\n        });\n    }\n    static string Ping() => \"pong\";\n}\n",
                ),
                ("src/HomeController.cs", "public class HomeController : Controller\n{\n    public IActionResult Index() => View();\n}\n"),
            ],
        );
        let scope = Scope { repos: vec![app.clone()], no_sync: false };
        let unsupported = |env: &Value| -> Vec<String> {
            env["incomplete"].as_array().into_iter().flatten().filter(|i| i["reason"] == "unsupported").map(|i| i["detail"].as_str().unwrap().to_string()).collect()
        };
        let r = routes(&scope).unwrap().env;
        assert_eq!(r["total"], 0);
        assert_eq!(r["complete"], false);
        let why = unsupported(&r);
        assert!(why.len() == 1 && why[0].starts_with("http: "), "{why:?}");
        assert!(why[0].contains("MapControllerRoute: conventional routes") && why[0].contains("(src/Startup.cs:7)"), "{why:?}");
        assert!(why[0].contains("MapGet: its route template is not a constant (src/Startup.cs:8)"), "{why:?}");
        let ping = context("Startup.Ping", 4000, Default::default(), &scope).unwrap().env;
        let why = unsupported(&ping);
        assert!(why.iter().any(|w| w.contains("MapGet: its route template is not a constant (src/Startup.cs:8)")), "{why:?}");
        let index = context("HomeController.Index", 4000, Default::default(), &scope).unwrap().env;
        let why = unsupported(&index);
        assert!(why.iter().any(|w| w.contains("conventional routes") && w.contains("MapControllerRoute")), "{why:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
