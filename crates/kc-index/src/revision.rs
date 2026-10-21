//! Whether the checkout a dependency links to is the revision the dependency asks for: a Terraform module source's
//! `?ref=`, a GitLab CI include's `ref:`, a registry package's version requirement. A link names a repo; the indexed
//! checkout of it may be at another revision, and then what the answer reads there isn't what the dependent gets.
//!
//! A git revision is checked against the checkout's HEAD: it matches when it names that commit (a tag on it, the
//! branch checked out, its sha). A package version is checked against the version the checkout's own manifest
//! declares. Anything that can't be checked (no checkout, a revision the checkout doesn't have, a version computed at
//! build time) counts as not matching: the link then says `revision_unverified`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::git::{GitDir, resolve_commit};

/// A revision resolved in a checkout: the checkout, the revision, and HEAD when it was resolved.
type Checked = (PathBuf, String, String);

/// Revision checks are inexpensive to repeat, so old checkout states must not accumulate for the server's lifetime.
const CACHE_ENTRIES: usize = 1024;

#[derive(Default)]
struct Cache<K> {
    values: HashMap<K, Option<String>>,
    order: VecDeque<K>,
}

impl<K: std::hash::Hash + Eq + Clone> Cache<K> {
    fn get(&self, key: &K) -> Option<&Option<String>> {
        self.values.get(key)
    }

    fn insert(&mut self, key: K, value: Option<String>) {
        // Two callers may finish the same lookup concurrently; replacing it must not consume another slot.
        if let Some(old) = self.values.get_mut(&key) {
            *old = value;
            return;
        }
        if self.values.len() == CACHE_ENTRIES && let Some(oldest) = self.order.pop_front() {
            self.values.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.values.insert(key, value);
    }
}

/// Whether the git revision `rev` (a branch, a tag, a sha) is the commit the checkout at `root` has checked out:
/// `None` when that can't be told (no checkout, no such revision there).
pub fn at_head(root: &Path, rev: &str) -> Option<bool> {
    let head = GitDir::find(root)?.head_commit()?;
    // Cached per checkout, revision and HEAD: one answer resolves the same `ref` for every module call using it.
    static SEEN: OnceLock<Mutex<Cache<Checked>>> = OnceLock::new();
    let key = (root.to_path_buf(), rev.to_string(), head.clone());
    let cache = SEEN.get_or_init(Default::default);
    if let Some(found) = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key) {
        return found.as_ref().map(|c| *c == head);
    }
    // A branch the checkout only tracks (`main` cloned on another branch) is its remote's.
    let found = resolve_commit(root, rev).or_else(|_| resolve_commit(root, &format!("refs/remotes/origin/{rev}"))).ok();
    cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, found.clone());
    found.map(|c| c == head)
}

/// Whether the version a checkout's manifest declares (`have`) satisfies the requirement `want` a dependent writes:
/// npm ranges (`^1.2.0`, `~1.2`, `>=1 <2`, `1.x`, `a || b`) and NuGet versions (`1.2.3`, which NuGet resolves to that
/// version when the feed has it, and ranges `[1.0,2.0)`). `None` when either isn't a version this can compare (a
/// property, a tag, `latest`, a git or file URL).
pub fn satisfies(want: &str, have: &str) -> Option<bool> {
    let have = Version::parse(have)?;
    let want = want.trim();
    if want.is_empty() || want == "*" || want.eq_ignore_ascii_case("x") {
        return Some(true);
    }
    // NuGet ranges: `[1.0,2.0)`, `(,2.0]`, `[1.2.3]`.
    if let Some(first) = want.chars().next().filter(|c| matches!(c, '[' | '(')) {
        let last = want.chars().last()?;
        let inner = &want[1..want.len() - 1];
        let (lo, hi) = match inner.split_once(',') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (inner.trim(), inner.trim()),
        };
        let ok_lo = lo.is_empty() || {
            let v = Version::parse(lo)?;
            if first == '[' { have >= v } else { have > v }
        };
        let ok_hi = hi.is_empty() || {
            let v = Version::parse(hi)?;
            if last == ']' { have <= v } else { have < v }
        };
        return Some(ok_lo && ok_hi);
    }
    let mut any_parsed = false;
    for alt in want.split("||") {
        let mut all = true;
        let comparators: Vec<&str> = alt.split_whitespace().collect();
        if comparators.is_empty() {
            continue;
        }
        for c in comparators {
            all &= comparator(c, &have)?;
            any_parsed = true;
        }
        if all {
            return Some(true);
        }
    }
    any_parsed.then_some(false)
}

/// One npm comparator (`^1.2`, `>=1.0.0`, `1.x`, `1.2.3`) against `have`.
fn comparator(c: &str, have: &Version) -> Option<bool> {
    let (op, v) = ["<=", ">=", "<", ">", "=", "^", "~", "v"]
        .iter()
        .find_map(|op| c.strip_prefix(op).map(|rest| (*op, rest)))
        .unwrap_or(("", c));
    let parts: Vec<&str> = v.split(['-', '+']).next()?.split('.').collect();
    let wild = |p: &str| matches!(p, "x" | "X" | "*");
    let given = parts.iter().take_while(|p| !wild(p)).count().min(3);
    let lo = Version::parse(&parts[..given].join("."))?;
    Some(match op {
        "<=" => *have <= lo,
        ">=" => *have >= lo,
        "<" => *have < lo,
        ">" => *have > lo,
        "^" => *have >= lo && *have < lo.caret_bound(given),
        "~" => *have >= lo && *have < lo.bump(if given >= 2 { 1 } else { 0 }),
        // `1.2.3`, `=1.2.3`, `v1.2.3`; a partial or wildcard version is a range over what it leaves out.
        _ if given == 3 => *have == lo,
        _ if given == 0 => true,
        _ => *have >= lo && *have < lo.bump(given - 1),
    })
}

/// A version's numbers, compared numerically; a pre-release or build suffix is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version([u64; 3]);

impl Version {
    fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches(['v', 'V', '=']);
        let core = s.split(['-', '+']).next()?;
        let mut out = [0u64; 3];
        let mut n = 0;
        for (i, p) in core.split('.').enumerate() {
            if i >= 4 || p.is_empty() {
                return None;
            }
            // NuGet's fourth part (`1.2.3.0`) counts only when it isn't zero.
            if i == 3 {
                if p != "0" {
                    return None;
                }
                continue;
            }
            out[i] = p.parse().ok()?;
            n += 1;
        }
        (n > 0).then_some(Version(out))
    }

    /// The version past the range `level` (0 major, 1 minor, 2 patch) spans: `1.2.3` bumped at 1 is `1.3.0`.
    fn bump(self, level: usize) -> Version {
        let mut v = self.0;
        v[level] += 1;
        for x in v.iter_mut().skip(level + 1) {
            *x = 0;
        }
        Version(v)
    }

    /// The upper bound of `^` over this version, `given` parts written: the first non-zero part bumped.
    fn caret_bound(self, given: usize) -> Version {
        let level = self.0.iter().take(given.max(1)).position(|&x| x != 0).unwrap_or(given.saturating_sub(1).min(2));
        self.bump(level)
    }
}

/// The version a package manifest declares for itself: `"version"` in a package.json, `<Version>`,
/// `<PackageVersion>` or `<VersionPrefix>` in a .csproj (or the `Directory.Build.props` beside it), when written as
/// a literal (not `$(Property)`).
pub fn declared_version(root: &Path, manifest: &str) -> Option<String> {
    let path = root.join(manifest);
    // Cached per file as it is (size and mtime): one answer checks a package once for all its dependents.
    static SEEN: OnceLock<Mutex<Cache<Stamped>>> = OnceLock::new();
    let meta = std::fs::metadata(&path).ok()?;
    let key = (path.clone(), meta.len(), crate::git::mtime_ns(&meta));
    let cache = SEEN.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key) {
        return v.clone();
    }
    let v = read_version(&path, manifest, root);
    cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, v.clone());
    v
}

/// A file as it is: its path, size and mtime.
type Stamped = (PathBuf, u64, i64);

/// [`declared_version`], read.
fn read_version(path: &Path, manifest: &str, root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    if manifest.ends_with("package.json") {
        let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
        return doc.get("version").and_then(|v| v.as_str()).map(str::to_string);
    }
    let tag = |text: &str| -> Option<String> {
        ["Version", "PackageVersion", "VersionPrefix"].iter().find_map(|t| {
            let open = format!("<{t}>");
            let start = text.find(&open)? + open.len();
            let end = text[start..].find("</")? + start;
            let v = text[start..end].trim();
            (!v.is_empty() && !v.contains("$(")).then(|| v.to_string())
        })
    };
    if let Some(v) = tag(&text) {
        return Some(v);
    }
    // MSBuild reads the nearest Directory.Build.props above the project.
    let mut dir = path.parent();
    while let Some(d) = dir {
        if let Ok(props) = std::fs::read_to_string(d.join("Directory.Build.props")) {
            return tag(&props);
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }
    None
}

/// Checks each registry dependency of `edges` against the version its package's manifest declares in the checkout it
/// links to (from `summaries`): sets the edge's `version`, and `revision_unverified` unless the version satisfies the
/// requirement.
pub fn check_edges(edges: &mut [kc_store::manifests::RepoEdge], summaries: &[crate::workspace::RepoSummary]) {
    for e in edges.iter_mut().filter(|e| e.registry) {
        let to = Path::new(&e.to_repo);
        let manifest = summaries
            .iter()
            .find(|s| s.root == to)
            .and_then(|s| s.packages.iter().find(|p| p.name.eq_ignore_ascii_case(&e.dependency)))
            .map(|p| p.file.clone());
        e.version = manifest.and_then(|m| declared_version(to, &m));
        let ok = match (&e.requirement, &e.version) {
            (Some(want), Some(have)) => satisfies(want, have) == Some(true),
            _ => false,
        };
        e.assumptions = if ok { Vec::new() } else { vec![kc_store::answer::Assumption::RevisionUnverified] };
    }
}

/// What code resolving into the checkout at `to` through `edges` (one repo's, checked by [`check_edges`]) assumes:
/// `revision_unverified` when a registry dependency on it is.
pub fn assumptions_into(edges: &[kc_store::manifests::RepoEdge], to: &Path) -> Vec<kc_store::answer::Assumption> {
    let mut out: Vec<kc_store::answer::Assumption> = edges.iter().filter(|e| Path::new(&e.to_repo) == to).flat_map(|e| e.assumptions.iter().copied()).collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_cache_bounds_old_states_and_replaces_duplicate_lookups() {
        let mut cache = Cache::default();
        cache.insert(0, None);
        cache.insert(0, Some("resolved".to_string()));
        assert_eq!(cache.order.len(), 1);
        assert_eq!(cache.get(&0), Some(&Some("resolved".to_string())));
        for i in 1..CACHE_ENTRIES {
            cache.insert(i, Some(i.to_string()));
        }
        assert!(cache.get(&0).is_some());
        for i in CACHE_ENTRIES..CACHE_ENTRIES * 3 {
            cache.insert(i, None);
            assert_eq!(cache.values.len(), CACHE_ENTRIES);
            assert_eq!(cache.order.len(), CACHE_ENTRIES);
        }
        assert!(cache.get(&0).is_none());
        assert_eq!(cache.get(&(CACHE_ENTRIES * 3 - 1)), Some(&None));
        // An evicted lookup can be computed and cached again, including a negative result.
        cache.insert(0, None);
        assert_eq!(cache.get(&0), Some(&None));
        assert_eq!(cache.values.len(), CACHE_ENTRIES);
    }

    #[test]
    fn npm_and_nuget_requirements() {
        let s = |w: &str, h: &str| satisfies(w, h);
        assert_eq!(s("^1.2.0", "1.4.1"), Some(true));
        assert_eq!(s("^1.2.0", "2.0.0"), Some(false));
        assert_eq!(s("^0.2.3", "0.3.0"), Some(false));
        assert_eq!(s("~1.2.0", "1.2.9"), Some(true));
        assert_eq!(s("~1.2.0", "1.3.0"), Some(false));
        assert_eq!(s("1.x", "1.9.0"), Some(true));
        assert_eq!(s(">=1.0.0 <2.0.0", "1.5.0"), Some(true));
        assert_eq!(s("<1.0.0 || >=3.0.0", "2.0.0"), Some(false));
        assert_eq!(s("1.2.3", "1.2.3"), Some(true));
        assert_eq!(s("1.2.3", "1.2.4"), Some(false));
        assert_eq!(s("[1.0,2.0)", "1.9.9"), Some(true));
        assert_eq!(s("[1.0,2.0)", "2.0.0"), Some(false));
        assert_eq!(s("$(OrdersVersion)", "1.0.0"), None);
        assert_eq!(s("^1.0.0", "$(Version)"), None);
        assert_eq!(s("latest", "1.0.0"), None);
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A revision matches when it names the commit checked out: a tag on it, its branch, its sha; an older tag, or one
    /// the checkout doesn't have, doesn't.
    #[test]
    fn revisions_match_the_checkout_only_at_its_head() {
        let root = std::env::temp_dir().join(format!("kc-revision-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&root, &["tag", "v1"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "two"]);
        git(&root, &["tag", "v2"]);
        assert_eq!(at_head(&root, "v2"), Some(true));
        assert_eq!(at_head(&root, "main"), Some(true));
        assert_eq!(at_head(&root, "v1"), Some(false));
        assert_eq!(at_head(&root, "v9"), None);
        // Never read as an option.
        assert_eq!(at_head(&root, "--output=x"), None);
        assert!(!root.join("x").exists());
        assert_eq!(at_head(Path::new("/nonexistent/kc"), "v1"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A registry dependency is checked against the version the linked checkout's manifest declares.
    #[test]
    fn registry_dependencies_check_the_version_the_checkout_declares() {
        let root = std::env::temp_dir().join(format!("kc-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("package.json"), r#"{ "name": "@acme/ui", "version": "1.4.0" }"#).unwrap();
        std::fs::write(root.join("lib/Acme.Data.csproj"), "<Project><PropertyGroup><Version>$(DataVersion)</Version></PropertyGroup></Project>").unwrap();
        let summary: crate::workspace::RepoSummary = serde_json::from_value(serde_json::json!({
            "root": root, "modules": [], "stamp": "",
            "packages": [
                { "kind": "npm", "name": "@acme/ui", "file": "package.json", "line": 1, "targets": [], "deps": [] },
                { "kind": "csproj", "name": "Acme.Data", "file": "lib/Acme.Data.csproj", "line": 1, "targets": [], "deps": [] },
            ],
        }))
        .unwrap();
        let edge = |dep: &str, want: Option<&str>| kc_store::manifests::RepoEdge {
            file: "package.json".into(),
            line: 3,
            package: "app".into(),
            dependency: dep.into(),
            to_repo: root.to_string_lossy().into_owned(),
            other_checkouts: Vec::new(),
            products: Vec::new(),
            import_sites: Vec::new(),
            requirement: want.map(str::to_string),
            version: None,
            assumptions: Vec::new(),
            registry: true,
        };
        let mut edges = vec![edge("@acme/ui", Some("^1.2.0")), edge("@acme/ui", Some("~1.3.0")), edge("Acme.Data", Some("2.0.0")), edge("@acme/ui", None)];
        check_edges(&mut edges, std::slice::from_ref(&summary));
        let got: Vec<(Option<&str>, bool)> = edges.iter().map(|e| (e.version.as_deref(), e.assumptions.is_empty())).collect();
        // Satisfied; another range; a version computed at build time; no requirement written.
        assert_eq!(got, [(Some("1.4.0"), true), (Some("1.4.0"), false), (None, false), (Some("1.4.0"), false)]);
        assert_eq!(assumptions_into(&edges[..1], &root), Vec::new());
        assert_eq!(assumptions_into(&edges, &root), [kc_store::answer::Assumption::RevisionUnverified]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
