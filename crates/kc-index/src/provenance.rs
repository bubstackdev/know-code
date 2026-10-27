//! What a durable record of an answer says about each repo it read (the provenance record, `knowcode`'s
//! `--provenance`): which worktree, its full commit, a digest of its uncommitted content, the base its index started
//! from, and the privacy policy it was read under. With the index generation and the build, the same evidence can be
//! asked for again and compared.

use std::collections::BTreeMap;
use std::path::Path;

use kc_extract::Lang;
use serde::Serialize;

use crate::{base, git, privacy};

/// `blake3:<hex>` of `bytes`: every digest a record carries.
pub fn digest(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

/// Every extractor's version, by language, as this build and the configured conventions make them.
pub fn extractors() -> BTreeMap<String, u32> {
    let _ = crate::config::load(); // the configured conventions are part of the extractor versions
    Lang::ALL.iter().map(|l| (l.as_str().to_string(), l.extractor_version())).collect()
}

/// The checkout a repo was read from.
#[derive(Debug, Clone, Serialize)]
pub struct Worktree {
    /// Its git directory (a linked worktree's own) and the common directory every worktree of it shares.
    pub git_dir: String,
    pub common_dir: String,
    /// The repository it is a checkout of: its normalized origin, else its common directory (see [`base::Identity`]).
    pub identity: Option<String>,
}

/// The content that differs from the commit: tracked files changed or deleted, untracked files (each by its path, its
/// status and its content's hash). Paths the privacy policy blocks are left out by git itself, as from every history
/// walk: a change to them never leaves git.
#[derive(Debug, Clone, Serialize)]
pub struct Uncommitted {
    pub files: usize,
    /// Of the sorted `<status>\t<path>\t<hash or "deleted">` lines; the digest of no lines when `files` is 0.
    pub digest: String,
}

/// The privacy policy a repo was read under: the digest of its key (what it blocks; absent when it blocks nothing),
/// of the privacy file's bytes (absent without one), and the level at or below which content is blocked.
#[derive(Debug, Clone, Serialize)]
pub struct Policy {
    pub key: Option<String>,
    pub file: Option<String>,
    pub block_at: u8,
}

/// What a record says about one repo, besides what the answer's own `provenance` entry says (commit, generation,
/// dirty, fresh).
#[derive(Debug, Clone, Serialize)]
pub struct Repo {
    pub worktree: Option<Worktree>,
    /// HEAD, in full.
    pub commit: Option<String>,
    /// `None` for a repo its privacy policy blocks whole.
    pub uncommitted: Option<Uncommitted>,
    /// The shared base the checkout's index was cloned from (see [`base`]): its file name, its commit, and whether any
    /// file differs from it yet.
    pub base: Option<base::Cloned>,
    pub privacy: Policy,
}

/// The record of the checkout at `root`.
pub fn repo(root: &Path) -> Repo {
    let policy = privacy::Privacy::load(root);
    let key = policy.key();
    let file = std::fs::read(root.join(privacy::file_name())).ok().map(|b| digest(&b));
    let worktree = git::GitDir::find(root).map(|g| Worktree {
        git_dir: canonical(&g.dir),
        common_dir: canonical(&g.common),
        identity: base::Identity::of(root).map(|i| i.key),
    });
    Repo {
        worktree,
        commit: crate::head_commit(root),
        uncommitted: if policy.whole() { None } else { uncommitted(root, &policy) },
        base: base::cloned_from(root),
        privacy: Policy { key: (!key.is_empty()).then(|| digest(key.as_bytes())), file, block_at: policy.block_at },
    }
}

fn canonical(p: &Path) -> String {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned()
}

/// [`Uncommitted`] for the checkout at `root`, from `git status`; `None` when git can't say.
fn uncommitted(root: &Path, policy: &privacy::Privacy) -> Option<Uncommitted> {
    let excludes = policy.exclude_pathspecs();
    let mut args = vec!["status", "--porcelain=v1", "-z", "--untracked-files=all", "--no-renames"];
    if !excludes.is_empty() {
        args.push("--");
        args.push(".");
        args.extend(excludes.iter().map(String::as_str));
    }
    let out = git::run(root, &args).ok()?;
    let mut lines = Vec::new();
    for entry in out.split(|b| *b == 0).filter(|e| e.len() > 3) {
        let entry = String::from_utf8_lossy(entry);
        let (status, path) = entry.split_at(2);
        let path = path.strip_prefix(' ').unwrap_or(path);
        if policy.blocks(path) {
            continue;
        }
        let hash = match std::fs::symlink_metadata(root.join(path)) {
            Ok(m) if m.is_file() => std::fs::read(root.join(path)).map(|b| digest(&b)).unwrap_or_else(|_| "unreadable".into()),
            Ok(m) if m.is_symlink() => std::fs::read_link(root.join(path)).map(|t| digest(t.to_string_lossy().as_bytes())).unwrap_or_else(|_| "unreadable".into()),
            Ok(_) => "directory".into(),
            Err(_) => "deleted".into(),
        };
        lines.push(format!("{status}\t{path}\t{hash}\n"));
    }
    lines.sort();
    Some(Uncommitted { files: lines.len(), digest: digest(lines.concat().as_bytes()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_name_their_hash() {
        assert_eq!(digest(b""), "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
    }
}
