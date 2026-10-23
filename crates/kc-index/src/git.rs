//! Git plumbing. Subprocesses cost 10–40 ms each on a busy machine, so the hot path reads `.git` directly and
//! only shells out when it must.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The work-tree root containing `path`: the nearest ancestor with a `.git` directory or file.
pub fn repo_root(path: &Path) -> Result<PathBuf> {
    let start = std::fs::canonicalize(path).with_context(|| format!("resolving {}", path.display()))?;
    start
        .ancestors()
        .find(|p| p.join(".git").exists())
        .map(Path::to_path_buf)
        .with_context(|| format!("{} is not inside a git repo", path.display()))
}

/// Where a work tree keeps its git data. For a linked worktree or submodule, `.git` is a file pointing at the
/// real directory, and refs live in the common directory.
pub struct GitDir {
    pub dir: PathBuf,
    pub common: PathBuf,
}

impl GitDir {
    pub fn find(root: &Path) -> Option<GitDir> {
        let dot = root.join(".git");
        let dir = if dot.is_dir() {
            dot
        } else {
            let text = std::fs::read_to_string(&dot).ok()?;
            let target = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
            if target.is_absolute() { target } else { root.join(target) }
        };
        let common = match std::fs::read_to_string(dir.join("commondir")) {
            Ok(c) => {
                let c = PathBuf::from(c.trim());
                if c.is_absolute() { c } else { dir.join(c) }
            }
            Err(_) => dir.clone(),
        };
        Some(GitDir { dir, common })
    }

    /// The commit HEAD points at, read from `HEAD`, loose refs and `packed-refs`.
    pub fn head_commit(&self) -> Option<String> {
        let head = std::fs::read_to_string(self.dir.join("HEAD")).ok()?;
        let head = head.trim();
        let Some(reference) = head.strip_prefix("ref: ") else {
            return is_sha(head).then(|| head.to_string());
        };
        for base in [&self.dir, &self.common] {
            if let Ok(sha) = std::fs::read_to_string(base.join(reference)) {
                let sha = sha.trim();
                if is_sha(sha) {
                    return Some(sha.to_string());
                }
            }
        }
        let packed = std::fs::read_to_string(self.common.join("packed-refs")).ok()?;
        packed.lines().find_map(|l| {
            let (sha, name) = l.split_once(' ')?;
            (name == reference && is_sha(sha)).then(|| sha.to_string())
        })
    }

    /// `mtime:size` of the git index, which changes on every stage, commit, checkout and merge.
    pub fn index_stamp(&self) -> String {
        stamp(&self.dir.join("index"))
    }
}

/// The user's global git config files, in the order git reads them: `$GIT_CONFIG_GLOBAL` alone when set, else the
/// XDG one and `~/.gitconfig`.
pub fn global_config_files() -> Vec<PathBuf> {
    if let Some(f) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        return vec![PathBuf::from(f)];
    }
    let mut out: Vec<PathBuf> = xdg_git_dir().map(|d| d.join("config")).into_iter().collect();
    out.extend(dirs::home_dir().map(|h| h.join(".gitconfig")));
    out
}

/// `$XDG_CONFIG_HOME/git`, or `~/.config/git`.
fn xdg_git_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(x) => Some(PathBuf::from(x).join("git")),
        None => dirs::home_dir().map(|h| h.join(".config").join("git")),
    }
}

/// The global excludes file git applies: `core.excludesFile` from `configs` (read in order, the last setting
/// winning), else the XDG default `git/ignore`. Read without running git, so `include`d config files aren't
/// followed; a change to any of `configs` still shows in their stamps.
pub fn excludes_files(configs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = None;
    for c in configs {
        if let Ok(text) = std::fs::read_to_string(c)
            && let Some(v) = config_value(&text, "core", "excludesfile")
        {
            found = Some(v);
        }
    }
    let path = match found {
        Some(v) => match v.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().map(|h| h.join(rest)),
            None => Some(PathBuf::from(v)),
        },
        None => xdg_git_dir().map(|d| d.join("ignore")),
    };
    path.into_iter().collect()
}

/// The last value of `section.key` (both matched without case) in a git config file's text.
fn config_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    let mut value = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let name = rest.split(']').next().unwrap_or("").trim();
            in_section = name.eq_ignore_ascii_case(section);
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        if k.trim().eq_ignore_ascii_case(key) {
            value = Some(v.trim().trim_matches('"').to_string());
        }
    }
    value
}

fn is_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `mtime_ns:size` of a file, or `-` if it doesn't exist. A device or pipe (`GIT_CONFIG_GLOBAL=/dev/null`, which
/// sandboxes and CI set) is stamped by its kind alone: its mtime moves on every write to it anywhere on the machine,
/// and would make every freshness check that covers it say "changed".
pub fn stamp(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) if !m.is_file() && !m.is_dir() => "special".to_string(),
        Ok(m) => format!("{}:{}", mtime_ns(&m), m.len()),
        Err(_) => "-".to_string(),
    }
}

pub fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64)
}

/// Runs `git` read-only in `dir` and returns its standard output, within the git deadline (see
/// [`kc_store::limits`]): a command past it, or in a cancelled operation, is killed and fails with a
/// [`kc_store::limits::Cut`], recorded for the answer to say it is incomplete.
pub fn run(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run_inner(dir, &[], args, None)
}

/// Git's memory for a history walk (`log --numstat -M` over a year or more): pack files mapped 16 MiB at a time, 64
/// MiB at most, and a 32 MiB cache of delta bases, where git's defaults (1 GiB windows, 96 MiB) let a walk of a large
/// repo reach 631 MiB resident (bitwarden/clients, a year's history). Same output; measured at 121 MiB resident there
/// for 9% more of git's instructions, 81 rather than 161 MiB on bitwarden/server (same instructions).
const WALK_MEMORY: [&str; 6] =
    ["-c", "core.packedGitWindowSize=16m", "-c", "core.packedGitLimit=64m", "-c", "core.deltaBaseCacheLimit=32m"];

/// [`run`] for a history walk: `git log` over many commits, within [`WALK_MEMORY`].
pub fn run_walk(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run_inner(dir, &WALK_MEMORY, args, None)
}

/// [`run`], writing `input` to git's standard input (`cat-file --batch`).
pub fn run_with_input(dir: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    run_inner(dir, &[], args, Some(input))
}

fn run_inner(dir: &Path, config: &[&str], args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    // Read-only: `--no-optional-locks` keeps `git status` from refreshing and rewriting the repo's `.git/index`.
    cmd.args(config).arg("--no-optional-locks").arg("-C").arg(dir).args(args);
    let what = format!("git {}", args.first().copied().unwrap_or_default());
    let out = kc_store::limits::output(cmd, kc_store::limits::Kind::Git, &what, input).context("running git")?;
    if !out.status.success() {
        bail!("git {} failed in {}: {}", args.join(" "), dir.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// The commit a revision the user gave names (`main`, `HEAD~2`, a sha, `v1.2`), as its full object id. Every
/// revision from outside goes through here before reaching another git command: one starting with `-` is refused,
/// and `--end-of-options` keeps git from reading it as an option, so `--output=…` can't make git write a file.
pub fn resolve_commit(dir: &Path, rev: &str) -> Result<String> {
    if rev.is_empty() || rev.starts_with('-') || rev.contains('\0') {
        bail!("not a git revision: {rev:?}");
    }
    let spec = format!("{rev}^{{commit}}");
    let out = run(dir, &["rev-parse", "--verify", "--quiet", "--end-of-options", &spec])
        .map_err(|_| anyhow::anyhow!("no commit {rev:?} in {}", dir.display()))?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if !is_sha(&sha) {
        bail!("no commit {rev:?} in {}", dir.display());
    }
    Ok(sha)
}

/// HEAD and dirty from `git status`; the slow, authoritative path.
pub fn status(root: &Path) -> Option<(Option<String>, bool)> {
    let out = run(root, &["status", "--porcelain=v2", "--branch", "--untracked-files=no"]).ok()?;
    let out = String::from_utf8_lossy(&out);
    let mut commit = None;
    let mut dirty = false;
    for line in out.lines() {
        match line.strip_prefix("# branch.oid ") {
            Some(oid) if oid != "(initial)" => commit = Some(oid.to_string()),
            Some(_) => {}
            None => dirty |= !line.starts_with('#'),
        }
    }
    Some((commit, dirty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_head_like_git() {
        // Source archives have no development history. Use only this test's own repository.
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("knowcode-head-{}-{unique}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=Fixture Author", "-c", "user.email=fixture@example.invalid"])
                .arg("-C").arg(&root).args(args).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        };
        let agrees = || {
            let gd = GitDir::find(&root).unwrap();
            let ours = gd.head_commit();
            let theirs = status(&root).and_then(|(c, _)| c);
            assert_eq!(ours, theirs);
            ours
        };
        git(&["init", "--quiet"]);
        assert_eq!(repo_root(&root).unwrap(), std::fs::canonicalize(&root).unwrap());
        assert!(agrees().is_none(), "an unborn HEAD is not a commit");
        std::fs::write(root.join("file.txt"), "fixture\n").unwrap();
        git(&["add", "file.txt"]);
        git(&["commit", "--quiet", "-m", "fixture"]);
        let committed = agrees().expect("the first commit");
        git(&["checkout", "--quiet", "--detach", "HEAD"]);
        assert_eq!(agrees().as_deref(), Some(committed.as_str()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_walk_within_its_memory_reads_the_same_log() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("knowcode-walk-{}-{unique}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=Fixture Author", "-c", "user.email=fixture@example.invalid"])
                .arg("-C").arg(&root).args(args).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "--quiet"]);
        let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
        std::fs::write(root.join("old.txt"), &body).unwrap();
        git(&["add", "old.txt"]);
        git(&["commit", "--quiet", "-m", "add"]);
        git(&["mv", "old.txt", "new.txt"]);
        std::fs::write(root.join("new.txt"), format!("{body}one more\n")).unwrap();
        git(&["commit", "--quiet", "-am", "move"]);
        let args = ["log", "--no-merges", "--numstat", "--summary", "-M", "--format=@@%H\t%s"];
        let walk = run_walk(&root, &args).unwrap();
        assert_eq!(walk, run(&root, &args).unwrap());
        assert!(String::from_utf8_lossy(&walk).contains("old.txt => new.txt"), "{}", String::from_utf8_lossy(&walk));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_the_excludes_file_from_config_text() {
        let text = "[user]\n\temail = a@b\n[core]\n\teditor = vi\n\texcludesFile = \"/x/ignore-1\"\n[core]\n\tExcludesfile = /x/ignore-2\n";
        assert_eq!(config_value(text, "core", "excludesfile").as_deref(), Some("/x/ignore-2"));
        assert_eq!(config_value("[core]\n\teditor = vi\n", "core", "excludesfile"), None);
    }
}
