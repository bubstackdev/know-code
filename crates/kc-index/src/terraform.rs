//! Links Terraform module calls across repos, the way Ansible role requirements are linked (see
//! [`crate::ansible`]).
//!
//! A module call's `source` names where the module's configuration is:
//!
//! - a git source (`git::https://host/group/repo.git//sub?ref=v1`, `git@host:group/repo.git`,
//!   `github.com/org/repo`) links to the indexed repo whose `origin` is that URL (`exact`); failing that, to the one
//!   repo whose directory is named like the URL's last segment (`unique_name`);
//! - a registry source (`host/namespace/name/system`, or `namespace/name/system` on the public registry) links to
//!   the indexed repos on that host, under that namespace, named `name` or `terraform-<system>-<name>` (the registry's
//!   naming convention): `unique_name` for one, `ambiguous` for several. A registry address names a module, not a
//!   repo, so it is never `exact`;
//! - `//subdir` after either is the module's directory in the repo.
//!
//! Anything else (an archive URL, S3, a registry module with no indexed repo) stays unlinked, `external_likely`.
//!
//! The revision asked for (`?ref=v1.2` on a git source, `version` on a registry module call) is kept on the link and
//! checked against the indexed checkout (see [`crate::revision`]): a git `ref` naming the checkout's HEAD matches. One
//! that doesn't, or can't be checked, and any registry version, make the link `revision_unverified`, and never
//! `exact`: the module read in the checkout may not be the one the caller gets.

use std::path::{Path, PathBuf};

use kc_store::answer::Assumption;
use kc_store::terraform::ModuleCall;
use serde::Serialize;

use crate::workspace::{RepoSummary, normalize_remote};

/// A module `source`, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `./modules/x`, `../shared`.
    Local(String),
    /// A git repository: its URL normalized as `host/group/repo`, the module's directory in it, and the revision
    /// its `?ref=` asks for.
    Git { remote: String, subdir: String, rev: Option<String> },
    /// A registry module: host (`None` for the public registry), namespace, name, target system, directory.
    Registry { host: Option<String>, namespace: String, name: String, system: String, subdir: String },
    /// An archive URL, a bucket, Mercurial: never in an indexed repo.
    Other,
}

/// Reads a module `source` as Terraform does.
pub fn parse(source: &str) -> Source {
    let s = source.trim();
    if kc_store::terraform::is_local_source(s) {
        return Source::Local(s.to_string());
    }
    let (forced_git, rest) = match s.strip_prefix("git::") {
        Some(r) => (true, r),
        None => (false, s),
    };
    if !forced_git && s.contains("::") {
        return Source::Other; // `hg::`, `s3::`, `gcs::`, `http::`
    }
    // `?ref=v1.2` and other query arguments; the `//subdir` before them.
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let rev = query.split('&').find_map(|kv| kv.strip_prefix("ref=")).filter(|r| !r.is_empty()).map(str::to_string);
    let scheme_end = rest.find("://").map_or(0, |i| i + 3);
    let (addr, subdir) = match rest[scheme_end..].find("//") {
        Some(i) => (&rest[..scheme_end + i], rest[scheme_end + i + 2..].trim_matches('/')),
        None => (rest, ""),
    };
    let subdir = subdir.to_string();
    let scp = !addr.contains("://") && addr.split_once(':').is_some_and(|(h, _)| h.contains('@'));
    let known_git_host = addr.starts_with("github.com/") || addr.starts_with("bitbucket.org/");
    if forced_git || scp || known_git_host || addr.ends_with(".git") {
        return match normalize_remote(addr) {
            Some(remote) => Source::Git { remote, subdir, rev },
            None => Source::Other,
        };
    }
    if addr.contains("://") {
        return Source::Other;
    }
    let parts: Vec<&str> = addr.split('/').collect();
    let word = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    match parts.as_slice() {
        [ns, name, system] if [ns, name, system].iter().all(|p| word(p)) => Source::Registry {
            host: None,
            namespace: ns.to_lowercase(),
            name: name.to_lowercase(),
            system: system.to_lowercase(),
            subdir,
        },
        [host, ns, name, system] if host.contains('.') && [ns, name, system].iter().all(|p| word(p)) => Source::Registry {
            host: Some(host.to_lowercase()),
            namespace: ns.to_lowercase(),
            name: name.to_lowercase(),
            system: system.to_lowercase(),
            subdir,
        },
        _ => Source::Other,
    }
}

/// A module call linked to the repo holding the module.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleLink {
    pub repo: PathBuf,
    /// The module's directory in the repo (`""` for its root).
    pub dir: String,
    /// `exact`, `unique_name` or `ambiguous` (see the module docs).
    pub confidence: &'static str,
    /// The revision the source asks for: a git `ref`, a registry `version`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// What the link did not check: `revision_unverified` when `rev` isn't the checkout's.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<Assumption>,
}

impl ModuleLink {
    /// The link to the module in `dir` of the checkout at `repo`, found at `confidence`, asking for revision `rev`
    /// (`git`: a git ref, checked against the checkout's HEAD; else a registry version, never checked).
    fn new(repo: PathBuf, dir: String, confidence: &'static str, rev: Option<String>, git: bool) -> ModuleLink {
        let checked = match &rev {
            None => true,
            Some(r) => git && crate::revision::at_head(&repo, r) == Some(true),
        };
        let (confidence, assumptions) =
            if checked { (confidence, Vec::new()) } else { (if confidence == "exact" { "unique_name" } else { confidence }, vec![Assumption::RevisionUnverified]) };
        ModuleLink { repo, dir, confidence, rev, assumptions }
    }
}

fn dir_name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// The workspace's repos, for linking module sources. Build once per answer. A repo whose checkout is gone is no
/// module source; an answer it would have been linked in names it as missing.
pub struct Terraform<'a> {
    repos: Vec<&'a RepoSummary>,
    /// Repos left out because their checkout is gone (see [`crate::workspace::gone`]).
    gone: Vec<&'a RepoSummary>,
}

impl<'a> Terraform<'a> {
    pub fn new(summaries: &'a [RepoSummary]) -> Terraform<'a> {
        Terraform { repos: crate::http::distinct(summaries), gone: crate::workspace::gone(summaries) }
    }

    /// The repos a module `source` links to: none for a local path or an unindexed module.
    pub fn link(&self, source: &str) -> Vec<ModuleLink> {
        self.link_version(source, None)
    }

    /// [`Terraform::link`] for a module call asking for `version` (a registry module's).
    pub fn link_version(&self, source: &str, version: Option<&str>) -> Vec<ModuleLink> {
        let links = self.resolve(source, version);
        // Unless the source names a repo by its origin, a gone repo it may name would have been a candidate.
        let by_origin = || matches!(parse(source), Source::Git { remote, .. } if self.repos.iter().any(|s| s.origin.as_deref() == Some(remote.as_str())));
        if !self.gone.is_empty() && !by_origin() {
            for g in Self::matching(&self.gone, source) {
                crate::workspace::note_missing(&g.root);
            }
        }
        links
    }

    /// [`Terraform::link_version`], saying nothing of gone repos.
    fn resolve(&self, source: &str, version: Option<&str>) -> Vec<ModuleLink> {
        match parse(source) {
            Source::Git { remote, subdir, rev } => {
                if let Some(s) = self.repos.iter().find(|s| s.origin.as_deref() == Some(remote.as_str())) {
                    return vec![ModuleLink::new(s.root.clone(), subdir, "exact", rev, true)];
                }
                let last = remote.rsplit('/').next().unwrap_or("");
                let named: Vec<&&RepoSummary> = self.repos.iter().filter(|s| dir_name(&s.root) == last).collect();
                match named.as_slice() {
                    [s] => vec![ModuleLink::new(s.root.clone(), subdir, "unique_name", rev, true)],
                    _ => Vec::new(),
                }
            }
            Source::Registry { host, namespace, name, system, subdir } => {
                let conventional = format!("terraform-{system}-{name}");
                let found: Vec<&&RepoSummary> = self
                    .repos
                    .iter()
                    .filter(|s| {
                        let Some(o) = s.origin.as_deref() else { return false };
                        let Some((path, last)) = o.rsplit_once('/') else { return false };
                        let in_namespace = match &host {
                            Some(h) => path == format!("{h}/{namespace}") || path.starts_with(&format!("{h}/{namespace}/")),
                            // The public registry publishes from GitHub, `namespace/terraform-<system>-<name>`.
                            None => path == format!("github.com/{namespace}"),
                        };
                        in_namespace && (last == name || last == conventional)
                    })
                    .collect();
                let confidence = if found.len() == 1 { "unique_name" } else { "ambiguous" };
                let rev = version.map(str::to_string);
                found.into_iter().map(|s| ModuleLink::new(s.root.clone(), subdir.clone(), confidence, rev.clone(), false)).collect()
            }
            Source::Local(_) | Source::Other => Vec::new(),
        }
    }

    /// Of `repos`, those a module `source` names, however surely: by origin or directory name for a git source, by
    /// registry namespace and name for a registry one.
    fn matching<'r>(repos: &[&'r RepoSummary], source: &str) -> Vec<&'r RepoSummary> {
        match parse(source) {
            Source::Git { remote, .. } => {
                let last = remote.rsplit('/').next().unwrap_or("").to_string();
                repos.iter().copied().filter(|s| s.origin.as_deref() == Some(remote.as_str()) || dir_name(&s.root) == last).collect()
            }
            Source::Registry { name, system, .. } => {
                let conventional = format!("terraform-{system}-{name}");
                repos
                    .iter()
                    .copied()
                    .filter(|s| s.origin.as_deref().and_then(|o| o.rsplit_once('/')).is_some_and(|(_, last)| last == name || last == conventional))
                    .collect()
            }
            Source::Local(_) | Source::Other => Vec::new(),
        }
    }

    /// The module calls in other repos whose source links to the module in directory `dir` of the repo at `root`,
    /// with their link.
    pub fn callers(&self, root: &Path, dir: &str) -> Vec<(PathBuf, ModuleCall, ModuleLink)> {
        // A gone repo calling a module this repo's name or origin could stand for would have been a caller.
        if !self.gone.is_empty()
            && let Some(me) = self.repos.iter().copied().find(|s| s.root == root)
        {
            crate::workspace::note_missing_if(self.gone.iter().copied(), |g| g.terraform.modules.iter().any(|m| !Self::matching(&[me], &m.source).is_empty()));
        }
        let mut out = Vec::new();
        for s in &self.repos {
            if s.root == root {
                continue;
            }
            for m in &s.terraform.modules {
                for l in self.resolve(&m.source, m.version.as_deref()) {
                    if l.repo == root && l.dir == dir {
                        out.push((s.root.clone(), m.clone(), l));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(remote: &str, subdir: &str) -> Source {
        Source::Git { remote: remote.into(), subdir: subdir.into(), rev: None }
    }

    fn git_at(remote: &str, subdir: &str, rev: &str) -> Source {
        Source::Git { remote: remote.into(), subdir: subdir.into(), rev: Some(rev.into()) }
    }

    #[test]
    fn sources() {
        assert_eq!(parse("./modules/net"), Source::Local("./modules/net".into()));
        assert_eq!(parse("git@git.example.com:infra/modules/net.git"), git("git.example.com/infra/modules/net", ""));
        assert_eq!(parse("git::https://git.example.com/infra/net.git//vpc?ref=v1.2.0"), git_at("git.example.com/infra/net", "vpc", "v1.2.0"));
        assert_eq!(parse("git::ssh://git@git.example.com/infra/net.git?ref=main"), git_at("git.example.com/infra/net", "", "main"));
        assert_eq!(parse("git::ssh://git@git.example.com/infra/net.git?depth=1&ref=v2"), git_at("git.example.com/infra/net", "", "v2"));
        assert_eq!(parse("github.com/acme/terraform-aws-net"), git("github.com/acme/terraform-aws-net", ""));
        assert_eq!(
            parse("git.example.com/infra/terraform-aws-site/aws"),
            Source::Registry { host: Some("git.example.com".into()), namespace: "infra".into(), name: "terraform-aws-site".into(), system: "aws".into(), subdir: String::new() }
        );
        assert_eq!(
            parse("terraform-aws-modules/iam/aws//modules/iam-role"),
            Source::Registry { host: None, namespace: "terraform-aws-modules".into(), name: "iam".into(), system: "aws".into(), subdir: "modules/iam-role".into() }
        );
        assert_eq!(parse("s3::https://bucket.s3.amazonaws.com/m.zip"), Source::Other);
        assert_eq!(parse("https://example.com/m.zip"), Source::Other);
    }

    fn repo(root: &str, origin: &str, modules: serde_json::Value) -> RepoSummary {
        serde_json::from_value(serde_json::json!({
            "root": root, "packages": [], "modules": [], "origin": origin, "terraform": { "modules": modules }, "stamp": "",
        }))
        .unwrap()
    }

    #[test]
    fn module_calls_link_to_the_repo_by_origin_or_registry_namespace() {
        let call = |name: &str, source: &str| serde_json::json!({ "name": name, "source": source, "file": "main.tf", "line": 1 });
        let all = vec![
            repo("/w/terraform-aws-site", "git.example.com/infra/modules/terraform-aws-site", serde_json::json!([])),
            repo("/w/net", "git.example.com/infra/modules/net", serde_json::json!([])),
            repo(
                "/w/site",
                "git.example.com/apps/site",
                serde_json::json!([
                    call("module.spa", "git.example.com/infra/terraform-aws-site/aws"),
                    call("module.net", "git@git.example.com:infra/modules/net.git//vpc"),
                    call("module.vpc", "terraform-aws-modules/vpc/aws"),
                ]),
            ),
        ];
        let tf = Terraform::new(&all);
        let l = tf.link("git.example.com/infra/terraform-aws-site/aws");
        assert_eq!((l[0].repo.to_str(), l[0].confidence), (Some("/w/terraform-aws-site"), "unique_name"));
        let l = tf.link("git::https://git.example.com/infra/modules/net.git//vpc");
        assert_eq!((l[0].repo.to_str(), l[0].dir.as_str(), l[0].confidence), (Some("/w/net"), "vpc", "exact"));
        // A `ref` the checkout can't be checked at (here: none on disk) is kept, unverified, and not exact.
        let l = tf.link("git::https://git.example.com/infra/modules/net.git//vpc?ref=v2");
        assert_eq!((l[0].confidence, l[0].rev.as_deref(), l[0].assumptions.as_slice()), ("unique_name", Some("v2"), &[Assumption::RevisionUnverified][..]));
        assert!(tf.link("terraform-aws-modules/vpc/aws").is_empty());
        // Another namespace never links.
        assert!(tf.link("git.example.com/other/terraform-aws-site/aws").is_empty());
        let callers = tf.callers(Path::new("/w/net"), "vpc");
        assert_eq!(callers.len(), 1);
        assert_eq!((callers[0].1.name.as_str(), callers[0].2.confidence), ("module.net", "exact"));
        assert!(tf.callers(Path::new("/w/net"), "").is_empty());
    }
}
