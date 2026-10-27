//! Workspace roots: directories whose git repos make up the workspace that cross-repo answers draw on.
//!
//! A root is walked down to `workspace.max_depth` directories. A directory holding `.git` (a clone, or a worktree
//! whose `.git` is a file) is a repo, and the walk doesn't go into it; the repo's own linked worktrees (those git
//! lists under `.git/worktrees`) that lie under the root are repos too, however deep, which finds worktrees nested
//! inside a checkout (`repo/.claude/worktrees/x`). Hidden directories are not entered, nor are dependency and build
//! directories (`node_modules`, `vendor`, `target`, …) unless they are repos themselves.
//!
//! [`discover`] walks a root to list its repos (`workspace list`). Which indexed repos are in the workspace is decided
//! by [`Roots::contains`] instead, from each repo's path and a few stats along it: the same answer for a repo that
//! exists, without walking every root on every query.

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use anyhow::Result;

/// Directories the walk doesn't enter unless they are repos: dependencies, build output, virtualenvs, vendored trees.
const SKIP_DIRS: &[&str] = &[
    "node_modules", "Pods", "Carthage", "DerivedData", "vendor", "third_party", "venv", "target", "build", "dist", "obj", "bin",
    "out", "packages", "bower_components", "__pycache__",
];

/// Every git repo under `root` (see the module docs), as canonical paths, sorted. A root that doesn't exist has none.
pub fn discover(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    inventory(root, max_depth).0
}

/// Directories whose entries decided the checkout inventory, for detecting new repos without repeating the walk.
pub(crate) fn inventory(root: &Path, max_depth: usize) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let Ok(root) = std::fs::canonicalize(root) else { return (Vec::new(), vec![root.to_path_buf()]) };
    let mut found = BTreeSet::new();
    let mut visited = Vec::new();
    let mut stack = vec![(root.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let dot = dir.join(".git");
        if dot.exists() {
            if dot.is_dir() {
                visited.extend([dot.clone(), dot.join("worktrees")]);
                found.extend(linked_worktrees(&dot).into_iter().filter(|w| w.starts_with(&root)));
            }
            found.insert(dir);
            continue;
        }
        visited.push(dir.clone());
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Symlinks aren't followed: they lead out of the root, or to a repo found under its own path.
            if name.starts_with('.') || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            // A skipped name is skipped only on the way to repos: a repo of that name is still one.
            if SKIP_DIRS.contains(&name.as_ref()) && !entry.path().join(".git").exists() {
                continue;
            }
            stack.push((entry.path(), depth + 1));
        }
    }
    (found.into_iter().collect(), visited)
}

/// The checkouts of a repo's linked worktrees that still exist: each `.git/worktrees/<name>/gitdir` names the
/// checkout's `.git` file.
fn linked_worktrees(git_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(git_dir.join("worktrees")) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("gitdir")).ok())
        .filter_map(|t| Path::new(t.trim()).parent().map(Path::to_path_buf))
        .filter(|w| w.join(".git").is_file())
        .filter_map(|w| std::fs::canonicalize(w).ok())
        .collect()
}

/// The configured roots, each with the repos under it.
pub fn configured() -> Result<Vec<(PathBuf, Vec<PathBuf>)>> {
    let (roots, depth) = crate::config::workspace()?;
    Ok(roots.into_iter().map(|r| {
        let repos = discover(&r, depth);
        (r, repos)
    }).collect())
}

/// The configured roots, resolved, to test repos against.
pub struct Roots {
    roots: Vec<PathBuf>,
    depth: usize,
    /// Whether a directory holds `.git`, for the directories tested so far.
    repo_dirs: HashMap<PathBuf, bool>,
}

impl Roots {
    /// The configured roots that exist, or `None` when no root is configured (the workspace is then every indexed
    /// repo).
    pub fn configured() -> Result<Option<Roots>> {
        let (roots, depth) = crate::config::workspace()?;
        if roots.is_empty() {
            return Ok(None);
        }
        Ok(Some(Roots::new(&roots, depth)))
    }

    pub fn new(roots: &[PathBuf], depth: usize) -> Roots {
        Roots { roots: roots.iter().filter_map(|r| std::fs::canonicalize(r).ok()).collect(), depth, repo_dirs: HashMap::new() }
    }

    /// Whether [`discover`] finds the checkout at `repo` (a canonical path) under some root. A checkout deleted since
    /// it was indexed counts while it has an index, as without roots; `knowcode prune` drops it.
    pub fn contains(&mut self, repo: &Path) -> bool {
        for i in 0..self.roots.len() {
            let root = self.roots[i].clone();
            if self.walked_to(&root, repo) || self.linked_from_member(&root, repo) {
                return true;
            }
        }
        false
    }

    fn is_repo(&mut self, dir: &Path) -> bool {
        if let Some(&r) = self.repo_dirs.get(dir) {
            return r;
        }
        let r = dir.join(".git").exists();
        self.repo_dirs.insert(dir.to_path_buf(), r);
        r
    }

    /// Whether the walk from `root` reaches `repo`: within the depth, through no hidden directory, no skipped one and
    /// no other repo.
    fn walked_to(&mut self, root: &Path, repo: &Path) -> bool {
        let Ok(rel) = repo.strip_prefix(root) else { return false };
        let names: Vec<&std::ffi::OsStr> = rel.components().filter_map(|c| if let Component::Normal(n) = c { Some(n) } else { None }).collect();
        if names.len() > self.depth {
            return false;
        }
        let mut dir = root.to_path_buf();
        for (i, name) in names.iter().enumerate() {
            // The walk stops at a repo: `dir` (the root or a directory on the way) must not be one.
            if self.is_repo(&dir) {
                return false;
            }
            let name = name.to_string_lossy();
            let last = i + 1 == names.len();
            if name.starts_with('.') || (!last && SKIP_DIRS.contains(&name.as_ref())) {
                return false;
            }
            dir.push(name.as_ref());
        }
        true
    }

    /// Whether `repo` is a linked worktree, under `root`, of a repo the walk from `root` reaches.
    fn linked_from_member(&mut self, root: &Path, repo: &Path) -> bool {
        if !repo.starts_with(root) {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(repo.join(".git")) else { return false };
        let Some(gitdir) = text.trim().strip_prefix("gitdir:").map(|g| PathBuf::from(g.trim())) else { return false };
        // `<main>/.git/worktrees/<name>`.
        let Some(main) = gitdir.parent().filter(|w| w.ends_with("worktrees")).and_then(Path::parent).filter(|g| g.ends_with(".git")).and_then(Path::parent) else {
            return false;
        };
        let main = std::fs::canonicalize(main).unwrap_or_else(|_| main.to_path_buf());
        main != repo && self.walked_to(root, &main)
    }
}
