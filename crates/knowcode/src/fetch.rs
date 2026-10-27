//! `knowcode fetch-deps`: clone the git repos a repo's manifests depend on into a directory the user names, so
//! cross-repo answers can reach them. CLI only: the MCP server never clones (see docs/plans/0.1.0-decisions.md). Running the
//! command is the explicit request, so it clones; `--dry-run` prints the plan and clones nothing.
//!
//! What resolves to a git repo: manifest dependencies written as a git URL (SwiftPM, XcodeGen, Cargo `git =`, npm git
//! specs), Terraform module sources from git, GitLab CI includes of another project (on the host of the repo's own
//! origin), and dependencies that resolved to a repo whose checkout is gone (`missing_repo` in cross-repo answers),
//! by that repo's origin. A registry dependency (NuGet, npm) names no repository and is only counted.
//!
//! Safety: DIR must be outside every git repo and every indexed checkout; each dependency is cloned into a new
//! directory of its own under DIR, never into one that exists; only `https://`, `ssh://` and `user@host:path`
//! remotes are cloned; a repo whose AI privacy file blocks it whole is cloned without a checkout, its policy read,
//! and the clone removed again unless `--include-blocked`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use clap::Args;
use kc_index::privacy::Privacy;
use kc_index::workspace::{RepoSummary, normalize_remote};
use kc_store::answer::{Incomplete, Reason};
use serde::Serialize;
use serde_json::{Value, json};

use crate::ops::{self, repo_name};

#[derive(Args)]
pub struct Options {
    /// The directory to clone into, one subdirectory per dependency repo. Required, and never inside a git repo.
    #[arg(long, value_name = "DIR")]
    pub into: PathBuf,
    /// The repo whose dependencies to fetch (default: the repo containing the current directory).
    #[arg(long, short = 'r')]
    pub repo: Option<PathBuf>,
    /// Print what would be cloned from where, and clone nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Clone each repo's full history (default: shallow, its latest commit; `knowcode history deepen` adds more).
    #[arg(long)]
    pub full: bool,
    /// Add DIR as a workspace root when workspace roots are configured and DIR is under none of them.
    #[arg(long)]
    pub add_root: bool,
    /// Also clone a repo whose AI privacy file blocks it whole (KnowCode still indexes and answers nothing from it).
    #[arg(long)]
    pub include_blocked: bool,
    /// Answer from the index as it is, without the freshness check.
    #[arg(long)]
    pub no_sync: bool,
    /// Emit the JSON envelope instead of text.
    #[arg(long)]
    pub json: bool,
}

/// Where a dependency is named.
#[derive(Serialize, Clone, PartialEq)]
struct Site {
    file: String,
    line: u32,
    /// `manifest`, `terraform`, `ci_include` or `missing_repo`.
    via: &'static str,
    /// The dependency's name there.
    dependency: String,
    /// The location as written (a URL, a project path), or the gone checkout's root.
    written: String,
}

/// One dependency repo and what was done with it.
#[derive(Serialize)]
struct Item {
    /// Its directory under DIR.
    name: String,
    /// The remote cloned from (for a refused one, as written).
    url: String,
    sites: Vec<Site>,
    /// `would_clone` (dry run), `cloned`, `present` (a checkout of it is already indexed or in DIR), `blocked`,
    /// `refused` (not an https or ssh git remote), `skipped` (its directory under DIR is taken) or `failed`.
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

pub fn run(o: Options) -> Result<()> {
    let start = match &o.repo {
        Some(r) => r.clone(),
        None => std::env::current_dir()?,
    };
    let root = kc_index::repo_root(&start).map_err(|_| anyhow::anyhow!("{} is not inside a git repo: run fetch-deps in the repo whose dependencies to fetch, or pass --repo", start.display()))?;
    let summaries = kc_index::workspace::all_summaries()?;
    let into = check_into(&o.into, &summaries.iter().map(|s| s.root.clone()).collect::<Vec<_>>())?;
    let deps = ops::deps(&ops::Scope { repos: vec![root.clone()], no_sync: o.no_sync }, false)?;
    let packages: Vec<kc_store::manifests::PackageRow> =
        deps.data.packages.get(&repo_name(&root)).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    // Summaries after the freshness check, so CI includes and module sources are the repo's current ones.
    let summaries = kc_index::workspace::all_summaries()?;
    let own = summaries.iter().find(|s| s.root == root);
    let origin = own_origin(&root);
    let (found, registry_only) = candidates(&root, &packages, own, &summaries, origin.as_deref());

    let live: HashMap<String, &RepoSummary> =
        summaries.iter().filter(|s| !s.gone && s.root.join(".git").exists()).filter_map(|s| Some((s.origin.clone()?, s))).collect();
    let blocked: HashSet<String> = summaries.iter().filter(|s| s.blocked.whole).filter_map(|s| s.origin.clone()).collect();
    let own_identity = own.and_then(|s| s.origin.clone());
    let mut items: Vec<Item> = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut incomplete: Vec<Incomplete> = Vec::new();
    for (url, sites) in found {
        let item = |name: &str, status, path: Option<&Path>, reason: Option<String>| Item {
            name: name.to_string(),
            url: url.clone().unwrap_or_else(|e| e.0.clone()),
            sites: sites.clone(),
            status,
            path: path.map(|p| p.to_string_lossy().into_owned()),
            reason,
        };
        let remote = match &url {
            Ok(r) => r.clone(),
            Err(Refused(written, why)) => {
                incomplete.push(
                    Incomplete::new(Reason::Unsupported, format!("dependency {}: {why}", sites[0].dependency))
                        .with_hint(format!("not cloned: {written}; clone it yourself if you trust it")),
                );
                items.push(item(&sites[0].dependency, "refused", None, Some(why.clone())));
                continue;
            }
        };
        let norm = normalize_remote(&remote).unwrap_or_default();
        if own_identity.as_deref() == Some(norm.as_str()) {
            continue;
        }
        let name = dir_name(&remote);
        if let Some(s) = live.get(&norm) {
            items.push(item(&name, "present", Some(&s.root), Some("a checkout of it is indexed".into())));
            continue;
        }
        if !valid_dir_name(&name) {
            let why = format!("{name:?} can't be a directory name");
            incomplete.push(Incomplete::new(Reason::Unsupported, format!("dependency {}: {why}", sites[0].dependency)));
            items.push(item(&name, "refused", None, Some(why)));
            continue;
        }
        let dest = into.join(&name);
        if dest.exists() || !taken.insert(name.to_lowercase()) {
            if dest.join(".git").exists() && own_origin(&dest).and_then(|o| normalize_remote(&o)).as_deref() == Some(norm.as_str()) {
                items.push(item(&name, "present", Some(&dest), Some("cloned into DIR already".into())));
            } else {
                let why = format!("{} is taken by another repo or file", dest.display());
                incomplete.push(Incomplete::new(Reason::Failed, format!("dependency {}: {why}", sites[0].dependency)).with_hint("move it, or fetch into another --into"));
                items.push(item(&name, "skipped", Some(&dest), Some(why)));
            }
            continue;
        }
        if blocked.contains(&norm) && !o.include_blocked {
            incomplete.push(blocked_entry(&name));
            items.push(item(&name, "blocked", None, Some(ops::BLOCKED_NOTE.into())));
            continue;
        }
        if o.dry_run {
            items.push(item(&name, "would_clone", Some(&dest), None));
            continue;
        }
        std::fs::create_dir_all(&into)?;
        match clone(&remote, &dest, !o.full, o.include_blocked) {
            Ok(Cloned::Done(note)) => items.push(item(&name, "cloned", Some(&dest), note)),
            Ok(Cloned::Blocked) => {
                incomplete.push(blocked_entry(&name));
                items.push(item(&name, "blocked", None, Some(format!("{}; the clone was removed", ops::BLOCKED_NOTE))));
            }
            Err(why) => {
                incomplete.push(Incomplete::new(Reason::Failed, format!("dependency {}: cloning {remote} failed: {why}", sites[0].dependency)).with_hint("check access to the remote with your git credentials, then run fetch-deps again"));
                items.push(item(&name, "failed", None, Some(why)));
            }
        }
    }

    let landed = items.iter().any(|i| matches!(i.status, "cloned" | "would_clone"));
    let workspace = if landed { roots_advice(&into, o.add_root, o.dry_run)? } else { Value::Null };
    let n = items.len();
    let mut env = json!({
        "repo": root,
        "into": into,
        "dry_run": o.dry_run,
        "shallow": !o.full,
        "items": items,
        "total": n,
        "registry_dependencies": registry_only,
    });
    for k in ["commit", "dirty", "freshness", "provenance", "incomplete"] {
        if let Some(v) = deps.env.get(k) {
            env[k] = v.clone();
        }
    }
    if !workspace.is_null() {
        env["workspace"] = workspace;
    }
    if n == 0 {
        env["note"] = json!(format!("no dependency of {} names a git repository: nothing to clone", repo_name(&root)));
    }
    for i in incomplete {
        ops::push_incomplete(&mut env, i);
    }
    ops::finish(&mut env);
    if o.json {
        println!("{}", serde_json::to_string_pretty(&env)?);
    } else {
        render(&env);
    }
    Ok(())
}

fn blocked_entry(name: &str) -> Incomplete {
    Incomplete::new(Reason::Withheld, format!("{name}: its {} blocks the whole repo", kc_index::privacy::file_name()))
        .with_hint("not cloned; --include-blocked clones it, though KnowCode would still answer nothing from it")
}

fn render(env: &Value) {
    let repo = env["repo"].as_str().map(|r| repo_name(Path::new(r))).unwrap_or_default();
    let items = env["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        println!("{}", env["note"].as_str().unwrap_or_default());
    } else {
        let how = if env["shallow"].as_bool() == Some(true) { "shallow" } else { "full history" };
        let dry = if env["dry_run"].as_bool() == Some(true) { ", dry run: nothing cloned" } else { "" };
        println!("{} dependency repo(s) of {repo}, into {} ({how}{dry})", items.len(), env["into"].as_str().unwrap_or_default());
        for i in &items {
            let status = i["status"].as_str().unwrap_or_default().replace('_', " ");
            let site = &i["sites"][0];
            let more = i["sites"].as_array().map_or(0, Vec::len).saturating_sub(1);
            let more = if more > 0 { format!(" (+{more})") } else { String::new() };
            println!("  {status:<12} {:<24} {}", i["name"].as_str().unwrap_or_default(), i["url"].as_str().unwrap_or_default());
            println!("  {:<12} {:<24} {}:{} {}{more}", "", "", site["file"].as_str().unwrap_or_default(), site["line"], site["via"].as_str().unwrap_or_default());
            if let Some(p) = i["path"].as_str() {
                println!("  {:<12} {:<24} -> {p}", "", "");
            }
            if let Some(r) = i["reason"].as_str() {
                println!("  {:<12} {:<24} {r}", "", "");
            }
        }
    }
    if let Some(n) = env["registry_dependencies"].as_u64().filter(|n| *n > 0) {
        println!("{n} registry dependenc{} name no repository and no indexed repo declares them: not fetched", if n == 1 { "y" } else { "ies" });
    }
    if let Some(h) = env["workspace"]["hint"].as_str() {
        println!("{h}");
    }
    for i in env["incomplete"].as_array().into_iter().flatten() {
        if let Ok(i) = serde_json::from_value::<Incomplete>(i.clone()) {
            println!("{}", i.line());
        }
    }
}

/// A location that is not cloned, as written, and why.
#[derive(Clone, Debug)]
struct Refused(String, String);

/// The dependency repos `root` names, by remote (or refusal), each with where it is named, in order of first
/// mention; and how many registry dependencies name no repository.
#[allow(clippy::type_complexity)]
fn candidates(
    root: &Path,
    packages: &[kc_store::manifests::PackageRow],
    own: Option<&RepoSummary>,
    summaries: &[RepoSummary],
    origin: Option<&str>,
) -> (Vec<(Result<String, Refused>, Vec<Site>)>, usize) {
    let mut out: Vec<(Result<String, Refused>, Vec<Site>)> = Vec::new();
    let mut add = |url: Result<String, Refused>, site: Site| {
        let key = |u: &Result<String, Refused>| match u {
            Ok(r) => normalize_remote(r).unwrap_or_else(|| r.clone()),
            Err(Refused(w, _)) => w.clone(),
        };
        let k = key(&url);
        match out.iter_mut().find(|(u, _)| key(u) == k) {
            Some((_, sites)) if !sites.contains(&site) => sites.push(site),
            Some(_) => {}
            None => out.push((url, vec![site])),
        }
    };
    let refuse = |written: &str, why: String| Err(Refused(written.to_string(), why));
    let checked = |written: &str| git_remote(written).or_else(|why| refuse(written, why));
    let owners = kc_index::workspace::package_owners(summaries, &[root.to_path_buf()], |s| &s.packages);
    let own_id = own.map(RepoSummary::identity);
    // A repo every checkout of which is gone: its origin, when it has one.
    let gone_origin = |pick: &dyn Fn(&RepoSummary) -> bool| -> Option<String> {
        let members: Vec<&RepoSummary> = summaries.iter().filter(|s| pick(s)).collect();
        (!members.is_empty() && members.iter().all(|s| s.gone || !s.root.join(".git").exists())).then(|| members[0].origin.clone()).flatten()
    };
    let mut registry_only = 0;
    for p in packages {
        for d in &p.deps {
            let site = |via: &'static str, written: &str| Site { file: p.file.clone(), line: d.line, via, dependency: d.name.clone(), written: written.to_string() };
            match d.source.as_str() {
                "url" => add(checked(&d.location), site("manifest", &d.location)),
                "registry" => match owners.identity(&d.name).filter(|id| Some(*id) != own_id.as_deref()) {
                    Some(id) => {
                        if let Some(o) = gone_origin(&|s: &RepoSummary| s.identity() == id) {
                            add(on_host(origin, &o), site("missing_repo", &o));
                        }
                    }
                    None => registry_only += 1,
                },
                "path" => {
                    if let Some(r) = d.resolved_repo.as_deref().filter(|r| Path::new(r) != root)
                        && let Some(o) = gone_origin(&|s: &RepoSummary| s.root == Path::new(r))
                    {
                        add(on_host(origin, &o), site("missing_repo", r));
                    }
                }
                _ => {}
            }
        }
    }
    let Some(own) = own else { return (out, registry_only) };
    for m in &own.terraform.modules {
        if let Some(written) = terraform_git(&m.source) {
            add(checked(&written), Site { file: m.file.clone(), line: m.line, via: "terraform", dependency: m.name.clone(), written: m.source.clone() });
        }
    }
    let live_paths: HashSet<String> =
        summaries.iter().filter(|s| !s.gone).filter_map(|s| s.origin.as_deref()?.split_once('/').map(|(_, p)| p.to_string())).collect();
    for f in own.ci.facts.iter().filter(|f| matches!(f.kind.as_str(), "ci_include" | "ci_trigger") && f.name == "project") {
        let Some(project) = serde_json::from_str::<Value>(&f.value).ok().and_then(|v| v["project"].as_str().map(str::to_string)) else { continue };
        let path = project.trim().trim_matches('/').trim_end_matches(".git").to_lowercase();
        // A project named by a CI variable can't be fetched; one a checkout answers to is there already.
        if path.is_empty() || path.contains('$') || live_paths.contains(&path) {
            continue;
        }
        let site = Site { file: f.file.clone(), line: f.line, via: "ci_include", dependency: project.clone(), written: project.clone() };
        match origin.and_then(normalize_remote).and_then(|n| n.split_once('/').map(|(h, _)| h.to_string())) {
            Some(host) => add(on_host(origin, &format!("{host}/{path}")), site),
            None => add(refuse(&project, "the repo has no origin remote to find the project's host from".into()), site),
        }
    }
    (out, registry_only)
}

/// The remote for `host/path` (a normalized origin), written the way the repo's own origin is when it is on the
/// same host (so the same credentials apply), else as https.
fn on_host(origin: Option<&str>, normalized: &str) -> Result<String, Refused> {
    let Some((host, path)) = normalized.split_once('/') else { return Err(Refused(normalized.to_string(), "not a host/path remote".into())) };
    if let Some(o) = origin.map(str::trim) {
        let same = normalize_remote(o).is_some_and(|n| n.split_once('/').is_some_and(|(h, _)| h == host));
        if same {
            if let Some((scheme, rest)) = o.split_once("://")
                && matches!(scheme, "https" | "ssh")
            {
                let authority = rest.split('/').next().unwrap_or(host);
                return Ok(format!("{scheme}://{authority}/{path}.git"));
            }
            if let Some((user_host, _)) = o.split_once(':').filter(|(uh, _)| uh.contains('@') && !uh.contains('/')) {
                return Ok(format!("{user_host}:{path}.git"));
            }
        }
    }
    Ok(format!("https://{host}/{path}.git"))
}

/// The git remote a dependency location names, cleaned of what only the manifest reads (`git+`, `git::`, an npm
/// `#committish`, a Terraform `?ref=` and `//subdir`); or why it is not cloned. Only `https://`, `ssh://` and
/// `user@host:path` remotes are: never a local path, `file://`, `http://`, `git://` or a transport helper.
pub fn git_remote(written: &str) -> Result<String, String> {
    let refused = || Err(format!("{written:?} is not an https or ssh git remote"));
    let s = written.trim();
    if s.is_empty() || s.starts_with('-') || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return refused();
    }
    let s = s.strip_prefix("git::").unwrap_or(s);
    let s = s.strip_prefix("git+").unwrap_or(s);
    let s = s.split(['#', '?']).next().unwrap_or_default();
    let s = if let Some(rest) = s.strip_prefix("github:") {
        format!("https://github.com/{rest}")
    } else if s.starts_with("github.com/") || s.starts_with("bitbucket.org/") {
        format!("https://{s}")
    } else {
        s.to_string()
    };
    match s.split_once("://") {
        Some((scheme @ ("https" | "ssh"), rest)) => {
            // A Terraform `//subdir` after the repo's path.
            let rest = rest.find("//").map_or(rest, |i| &rest[..i]).trim_end_matches('/');
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = authority.rsplit('@').next().unwrap_or_default();
            if host.is_empty() || host.starts_with('-') || path.trim_matches('/').is_empty() {
                return refused();
            }
            Ok(format!("{scheme}://{rest}"))
        }
        Some(_) => refused(),
        None => {
            // scp-like: `user@host:path`.
            let Some((user_host, path)) = s.split_once(':') else { return refused() };
            let path = path.find("//").map_or(path, |i| &path[..i]).trim_end_matches('/');
            let Some((user, host)) = user_host.split_once('@') else { return refused() };
            if user.is_empty() || host.is_empty() || user_host.contains('/') || path.trim_matches('/').is_empty() {
                return refused();
            }
            Ok(format!("{user_host}:{path}"))
        }
    }
}

/// A Terraform module source that names a git repo (`git::…`, `github.com/…`, `bitbucket.org/…`, `git@…`), as
/// written; registry and local sources name none.
fn terraform_git(source: &str) -> Option<String> {
    let s = source.trim();
    (s.starts_with("git::") || s.starts_with("github.com/") || s.starts_with("bitbucket.org/") || (s.contains('@') && s.contains(':') && !s.contains("://")))
        .then(|| s.to_string())
}

/// A clone's directory name: the remote's last path segment, without `.git`.
fn dir_name(remote: &str) -> String {
    remote.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or_default().trim_end_matches(".git").to_string()
}

fn valid_dir_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.starts_with('-') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// The `origin` remote of the checkout at `dir`, as configured.
fn own_origin(dir: &Path) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(["config", "--get", "remote.origin.url"]).stderr(Stdio::null()).output().ok()?;
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !url.is_empty()).then_some(url)
}

/// `into` made absolute and canonical (as far as it exists), refused when it is inside a git repo or an indexed
/// checkout, or is a file.
fn check_into(into: &Path, indexed: &[PathBuf]) -> Result<PathBuf> {
    let abs = std::path::absolute(into)?;
    let mut existing = abs.as_path();
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| anyhow::anyhow!("{} has no existing parent", abs.display()))?;
    }
    let rest = abs.strip_prefix(existing).unwrap_or(Path::new(""));
    let dir = std::fs::canonicalize(existing)?.join(rest);
    if dir.exists() && !dir.is_dir() {
        bail!("--into {} is not a directory", dir.display());
    }
    if let Some(repo) = dir.ancestors().find(|p| p.join(".git").exists()) {
        bail!("--into {} is inside the git repo {}: fetch-deps never writes into a repo; name a directory outside every repo", dir.display(), repo.display());
    }
    if let Some(r) = indexed.iter().find(|r| dir.starts_with(r)) {
        bail!("--into {} is inside the indexed checkout {}: name a directory outside every repo", dir.display(), r.display());
    }
    Ok(dir)
}

enum Cloned {
    Done(Option<String>),
    Blocked,
}

fn git(dir: Option<&Path>, args: &[&str]) -> std::result::Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    let out = cmd.args(args).stdin(Stdio::inherit()).output().map_err(|e| format!("running git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(git_error(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// The line of git's standard error that says what failed: its first `fatal:` or `error:` line, else its first line.
pub fn git_error(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines.iter().find(|l| l.starts_with("fatal:") || l.starts_with("error:")).or(lines.first()).copied().unwrap_or("git failed").to_string()
}

/// Clones `remote` into the new directory `dest` without a checkout, reads the AI privacy file from the commit, and
/// checks the files out only when it doesn't block the whole repo (or `include_blocked`); a blocked clone is removed.
fn clone(remote: &str, dest: &Path, shallow: bool, include_blocked: bool) -> std::result::Result<Cloned, String> {
    let d = dest.to_string_lossy();
    let mut args = vec!["clone", "--quiet", "--no-checkout"];
    if shallow {
        args.extend(["--depth", "1"]);
    }
    args.extend(["--", remote, &d]);
    git(None, &args)?;
    let privacy = committed_privacy(dest);
    if privacy.whole() && !include_blocked {
        std::fs::remove_dir_all(dest).map_err(|e| format!("removing the blocked clone: {e}"))?;
        return Ok(Cloned::Blocked);
    }
    let empty = git(Some(dest), &["ls-tree", "--name-only", "HEAD"]).map(|o| o.is_empty()).unwrap_or(true);
    if !empty {
        git(Some(dest), &["checkout", "--quiet", "HEAD", "--", "."])?;
    }
    Ok(Cloned::Done(privacy.whole().then(|| format!("{}; cloned as --include-blocked asked, but KnowCode answers nothing from it", ops::BLOCKED_NOTE))))
}

/// The policy of the AI privacy file in a clone's HEAD commit, as [`Privacy::load`] would read it checked out.
fn committed_privacy(dest: &Path) -> Privacy {
    let (block_at, malformed, _) = kc_index::config::privacy();
    let format = kc_index::config::privacy_format();
    let text = git(Some(dest), &["ls-tree", "HEAD", "--", &format.file]).ok().and_then(|o| {
        let line = String::from_utf8_lossy(&o).lines().next()?.to_string();
        let (meta, _) = line.split_once('\t')?;
        let mut parts = meta.split_whitespace();
        let (mode, kind, sha) = (parts.next()?, parts.next()?, parts.next()?);
        Some(if kind != "blob" || mode == "120000" {
            Err("not a regular file".to_string())
        } else {
            git(Some(dest), &["cat-file", "blob", sha]).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| format!("unreadable: {e}"))
        })
    });
    Privacy::from_text(text, &format, block_at, malformed)
}

/// Whether the clones under `dir` join the workspace, and what to do when they don't: with roots configured and
/// `dir` under none, add it (`add`) or say how; with none configured (the workspace is every indexed repo), say how to
/// index them, since adding a root would narrow the workspace to it.
fn roots_advice(dir: &Path, add: bool, dry: bool) -> Result<Value> {
    let (roots, depth) = kc_index::config::workspace()?;
    let shown = dir.display();
    if roots.is_empty() {
        return Ok(json!({ "status": "no_roots", "hint": format!("no workspace roots are configured, so the workspace is every indexed repo: index the clones with `knowcode index --workspace {shown}`") }));
    }
    let under = roots.iter().filter_map(|r| std::fs::canonicalize(r).ok()).any(|r| dir.strip_prefix(&r).is_ok_and(|rel| rel.components().count() < depth));
    if under {
        return Ok(json!({ "status": "under_root" }));
    }
    if !add {
        return Ok(json!({ "status": "outside_roots", "hint": format!("{shown} is under no workspace root: `knowcode workspace add {shown}` (or --add-root) makes the clones part of the workspace") }));
    }
    if dry {
        return Ok(json!({ "status": "would_add", "hint": format!("would add {shown} as a workspace root") }));
    }
    let (root, _) = kc_index::config::add_root(dir)?;
    kc_index::light::discover_roots()?;
    Ok(json!({ "status": "added", "hint": format!("added {} as a workspace root", root.display()) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_and_ssh_remotes_are_cloned() {
        let ok = |w: &str| git_remote(w).unwrap();
        assert_eq!(ok("https://example.test/group/lib.git"), "https://example.test/group/lib.git");
        assert_eq!(ok("git+https://example.test/group/lib.git#v1.2.0"), "https://example.test/group/lib.git");
        assert_eq!(ok("git+ssh://git@example.test/group/lib.git"), "ssh://git@example.test/group/lib.git");
        assert_eq!(ok("git@example.test:group/lib.git"), "git@example.test:group/lib.git");
        assert_eq!(ok("github:acme/widgets"), "https://github.com/acme/widgets");
        assert_eq!(ok("git::https://example.test/infra/modules.git//vpc?ref=v3"), "https://example.test/infra/modules.git");
        assert_eq!(ok("github.com/acme/tf-mod"), "https://github.com/acme/tf-mod");
        for bad in [
            "http://example.test/group/lib.git",
            "file:///srv/git/lib.git",
            "git://example.test/lib.git",
            "../lib",
            "/srv/git/lib.git",
            "ext::sh -c touch% /tmp/x",
            "-uhttps://example.test/x",
            "https://example.test",
            "https://-oProxyCommand=x/y",
            "example.test:group/lib.git",
            "",
        ] {
            assert!(git_remote(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn a_gone_or_ci_repo_is_fetched_the_way_the_origin_is() {
        let o = |origin: Option<&str>, n: &str| on_host(origin, n).unwrap();
        assert_eq!(o(Some("git@example.test:team/app.git"), "example.test/team/lib"), "git@example.test:team/lib.git");
        assert_eq!(o(Some("https://example.test/team/app"), "example.test/infra/ci"), "https://example.test/infra/ci.git");
        assert_eq!(o(Some("ssh://git@example.test:2222/team/app.git"), "example.test:2222/team/lib"), "ssh://git@example.test:2222/team/lib.git");
        assert_eq!(o(Some("git@other.test:team/app.git"), "example.test/team/lib"), "https://example.test/team/lib.git");
        assert_eq!(o(None, "example.test/team/lib"), "https://example.test/team/lib.git");
        assert_eq!(dir_name("git@example.test:group/lib.git"), "lib");
        assert_eq!(dir_name("https://example.test/group/lib/"), "lib");
        assert!(!valid_dir_name("..") && !valid_dir_name("") && valid_dir_name("lib-2.x"));
    }
}
