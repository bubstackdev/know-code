//! AI privacy files: a privacy file at a repo's root (`.knowcode-privacy.yml` unless configured) says how much of the
//! repo AI tools may see.
//!
//! ```yaml
//! level: 3                         # the whole repo's level; 3 when absent
//! rules:                           # optional: folders (trailing /) or files, repo-relative, leading / optional
//!   - path: /payments/keys/
//!     level: 1
//! ```
//!
//! The file's name and its four keys are configurable (`[privacy] file`, `global_key`, `rules_key`, `path_key`,
//! `level_key`; see [`Format`] and [`crate::config::privacy_format`]), so a code base that already keeps such files
//! under its own name and keys is read as it is.
//!
//! Levels: 1 restricted (no AI access), 2 confidential, 3 internal, 4 public. A file's level is the lowest of the
//! global level and the level of every rule naming it or a folder above it, so a path can be made more restrictive
//! than its parent but never less. Content at or below `[privacy] block_at` (1 unless configured; see
//! [`crate::config::privacy`]) is blocked: never read, extracted, stored, searched or answered from. A repo blocked
//! whole is not indexed at all; its index holds only that it is blocked. Paths match without case (a rule written
//! `Keys/` blocks `keys/`), and a rule without a trailing `/` blocks a file of that name and, should it be a
//! directory, everything under it: both are the reading that blocks more.
//!
//! An empty file (or one of comments only) is the default, level 3. A file that can't be read as this format (bad
//! YAML, an unknown key, a level outside 1–4, a rule without a path) is warned about and taken as level 1, the whole
//! repo blocked, until it is fixed: its author meant to restrict something, and guessing what would risk exactly the
//! exposure the file exists to prevent, while blocking too much only costs answers, and `status` says why.
//! `[privacy] malformed = "default"` takes such a file as absent instead.

use std::path::Path;

use serde::{Deserialize, Serialize};
use yaml_rust2::{Yaml, YamlLoader};

/// The privacy file's name, at a repo's root, unless `[privacy] file` says otherwise.
pub const DEFAULT_FILE_NAME: &str = ".knowcode-privacy.yml";

/// The privacy file's name and keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    /// The file's name, at a repo's root.
    pub file: String,
    /// The top-level key holding the whole repo's level.
    pub global_key: String,
    /// The top-level key holding the list of path rules.
    pub rules_key: String,
    /// A rule's key holding its path.
    pub path_key: String,
    /// A rule's key holding its level.
    pub level_key: String,
}

impl Default for Format {
    fn default() -> Format {
        Format { file: DEFAULT_FILE_NAME.into(), global_key: "level".into(), rules_key: "rules".into(), path_key: "path".into(), level_key: "level".into() }
    }
}

/// The privacy file's name as configured.
pub fn file_name() -> String {
    crate::config::privacy_format().file
}

/// A repo's level when its file doesn't set one (or there is no file).
pub const DEFAULT_LEVEL: u8 = 3;

/// The level at or below which content is blocked, unless `[privacy] block_at` says otherwise.
pub const DEFAULT_BLOCK_AT: u8 = 1;

/// The index's `meta` key holding [`Privacy::key`] as of the index's last open: what the index was purged for.
pub const META_KEY: &str = "privacy.key";

/// The index's `meta` key holding [`Blocked`] as JSON, for `status` and the workspace summary.
pub const META_BLOCKED: &str = "privacy.blocked";

/// Bumped when what [`Privacy::key`] covers changes, so every index is checked against its policy again.
const KEY_FORMAT: u32 = 1;

/// How a privacy file that can't be read is taken.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Malformed {
    /// As level 1: the whole repo is blocked until the file is fixed.
    #[default]
    Restricted,
    /// As if there were no file: the default level.
    Default,
}

impl Malformed {
    pub fn parse(s: &str) -> Option<Malformed> {
        match s.trim() {
            "restricted" => Some(Malformed::Restricted),
            "default" => Some(Malformed::Default),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Malformed::Restricted => "restricted",
            Malformed::Default => "default",
        }
    }
}

/// One path rule: a repo-relative path (no leading `/`; a folder ends with `/`, `""` is the whole repo)
/// and its level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub path: String,
    pub level: u8,
}

/// What a privacy file says.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Levels {
    pub global: u8,
    pub rules: Vec<Rule>,
}

/// The state of a repo's privacy file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "file")]
pub enum FileState {
    /// No file: the default level.
    Absent,
    /// A file with nothing in it (or only comments): the default level.
    Empty,
    /// A file read as written.
    Read,
    /// A file that can't be read as the format, with why.
    Malformed { error: String },
}

/// Parses a privacy file's text, written in `format`'s keys. `Ok(None)` for an empty file; an error says why the text
/// isn't the format.
pub fn parse(text: &str, format: &Format) -> Result<Option<Levels>, String> {
    let Format { global_key: global, rules_key: rules, path_key: path_k, level_key: level_k, .. } = format;
    let docs = YamlLoader::load_from_str(text).map_err(|e| format!("not valid YAML: {e}"))?;
    let doc = match docs.as_slice() {
        [] | [Yaml::Null] => return Ok(None),
        [one] => one,
        _ => return Err("more than one YAML document".into()),
    };
    let Yaml::Hash(map) = doc else { return Err("the top level must be a mapping".into()) };
    let level = |v: &Yaml, key: &str| -> Result<u8, String> {
        let n = match v {
            Yaml::Integer(n) => Some(*n),
            Yaml::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        n.filter(|n| (1..=4).contains(n)).map(|n| n as u8).ok_or_else(|| format!("`{key}` must be a level from 1 to 4"))
    };
    let mut out = Levels { global: DEFAULT_LEVEL, rules: Vec::new() };
    for (k, v) in map {
        match k.as_str() {
            Some(k) if k == global => out.global = level(v, global)?,
            Some(k) if k == rules => {
                let list = match v {
                    Yaml::Null => continue,
                    Yaml::Array(a) => a,
                    _ => return Err(format!("`{rules}` must be a list")),
                };
                for (i, item) in list.iter().enumerate() {
                    let Yaml::Hash(entry) = item else { return Err(format!("`{rules}[{i}]` must be a mapping with `{path_k}` and `{level_k}`")) };
                    let (mut path, mut lvl) = (None, None);
                    for (ek, ev) in entry {
                        match ek.as_str() {
                            Some(k) if k == path_k => path = Some(ev.as_str().ok_or_else(|| format!("`{rules}[{i}].{path_k}` must be a string"))?.to_string()),
                            Some(k) if k == level_k => lvl = Some(level(ev, &format!("{rules}[{i}].{level_k}"))?),
                            _ => return Err(format!("`{rules}[{i}]` has an unknown key")),
                        }
                    }
                    let path = path.ok_or_else(|| format!("`{rules}[{i}]` has no `{path_k}`"))?;
                    let level = lvl.ok_or_else(|| format!("`{rules}[{i}]` has no `{level_k}`"))?;
                    out.rules.push(Rule { path: normalize(&path).ok_or_else(|| format!("`{rules}[{i}].{path_k}` is not a path inside the repo"))?, level });
                }
            }
            _ => return Err(format!("unknown key at the top level (the keys are `{global}` and `{rules}`)")),
        }
    }
    Ok(Some(out))
}

/// A rule's path as matched: `/` separated, no leading `/` or `./`, a folder ending with `/`, `""` for the repo.
/// `None` for a path leaving the repo.
fn normalize(raw: &str) -> Option<String> {
    let s = raw.trim().replace('\\', "/");
    let folder = s.ends_with('/');
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.contains(&"..") {
        return None;
    }
    if parts.is_empty() {
        return Some(String::new());
    }
    let joined = parts.join("/");
    Some(if folder { format!("{joined}/") } else { joined })
}

/// What is blocked in a repo, as stored in its index (`meta`) and its workspace summary, and shown by `status`. Never
/// holds a path below a blocked area: only the rules' own paths, which the privacy file names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocked {
    /// The whole repo.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub whole: bool,
    /// The folders and files blocked, as the rules name them (repo-relative, `/` separated).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// The level at or below which content is blocked.
    pub block_at: u8,
}

impl Blocked {
    pub fn any(&self) -> bool {
        self.whole || !self.paths.is_empty()
    }
}

/// A repo's privacy policy, as its file and the config give it.
#[derive(Debug, Clone)]
pub struct Privacy {
    pub state: FileState,
    pub levels: Levels,
    pub block_at: u8,
    pub malformed: Malformed,
    /// Lower-cased blocked rule paths.
    blocked: Vec<String>,
    whole: bool,
}

impl Privacy {
    /// The policy of the repo at `root`: its privacy file (never followed if a link) under the configured threshold.
    /// A malformed file is warned about on stderr.
    pub fn load(root: &Path) -> Privacy {
        let (block_at, malformed, _) = crate::config::privacy();
        let format = crate::config::privacy_format();
        let path = root.join(&format.file);
        let text = match std::fs::symlink_metadata(&path) {
            Err(_) => None,
            Ok(m) if m.is_symlink() || !m.is_file() => Some(Err("not a regular file".to_string())),
            Ok(_) => Some(std::fs::read(&path).map_err(|e| format!("unreadable: {e}")).map(|b| String::from_utf8_lossy(&b).into_owned())),
        };
        let p = Privacy::from_text(text, &format, block_at, malformed);
        if let FileState::Malformed { error } = &p.state {
            warn_once(&path, error, malformed);
        }
        p
    }

    /// The policy for a file's text (`None`: no file; `Some(Err)`: a file that can't be read), written in `format`.
    pub fn from_text(text: Option<Result<String, String>>, format: &Format, block_at: u8, malformed: Malformed) -> Privacy {
        let (state, levels) = match text {
            None => (FileState::Absent, Levels { global: DEFAULT_LEVEL, rules: Vec::new() }),
            Some(Err(error)) => (FileState::Malformed { error }, Levels::default()),
            Some(Ok(text)) => match parse(&text, format) {
                Ok(None) => (FileState::Empty, Levels { global: DEFAULT_LEVEL, rules: Vec::new() }),
                Ok(Some(l)) => (FileState::Read, l),
                Err(error) => (FileState::Malformed { error }, Levels::default()),
            },
        };
        let levels = match (&state, malformed) {
            (FileState::Malformed { .. }, Malformed::Restricted) => Levels { global: 1, rules: Vec::new() },
            (FileState::Malformed { .. }, Malformed::Default) => Levels { global: DEFAULT_LEVEL, rules: Vec::new() },
            _ => levels,
        };
        let whole = levels.global <= block_at || levels.rules.iter().any(|r| r.path.is_empty() && r.level <= block_at);
        let mut blocked: Vec<String> = if whole {
            Vec::new()
        } else {
            levels.rules.iter().filter(|r| r.level <= block_at).map(|r| r.path.to_lowercase()).collect()
        };
        blocked.sort();
        blocked.dedup();
        Privacy { state, levels, block_at, malformed, blocked, whole }
    }

    /// Whether the whole repo is blocked.
    pub fn whole(&self) -> bool {
        self.whole
    }

    /// Whether anything in the repo is blocked.
    pub fn blocks_anything(&self) -> bool {
        self.whole || !self.blocked.is_empty()
    }

    /// Whether the repo-relative `path` (a file or a directory) is blocked.
    pub fn blocks(&self, path: &str) -> bool {
        if self.whole {
            return true;
        }
        if self.blocked.is_empty() {
            return false;
        }
        let p = path.trim_start_matches("./").trim_start_matches('/').to_lowercase();
        self.blocked.iter().any(|b| match b.strip_suffix('/') {
            Some(dir) => p == dir || p.starts_with(b.as_str()),
            None => p == *b || p.strip_prefix(b.as_str()).is_some_and(|rest| rest.starts_with('/')),
        })
    }

    /// The level of the repo-relative `path`: the lowest of the global level and every rule naming it or a folder
    /// above it.
    pub fn level_of(&self, path: &str) -> u8 {
        let p = path.trim_start_matches('/').to_lowercase();
        self.levels
            .rules
            .iter()
            .filter(|r| {
                let b = r.path.to_lowercase();
                b.is_empty()
                    || match b.strip_suffix('/') {
                        Some(dir) => p == dir || p.starts_with(b.as_str()),
                        None => p == b || p.strip_prefix(b.as_str()).is_some_and(|rest| rest.starts_with('/')),
                    }
            })
            .map(|r| r.level)
            .fold(self.levels.global, u8::min)
    }

    /// What is blocked, to store and show.
    pub fn blocked(&self) -> Blocked {
        let paths = if self.whole {
            Vec::new()
        } else {
            let mut p: Vec<String> = self.levels.rules.iter().filter(|r| r.level <= self.block_at).map(|r| r.path.clone()).collect();
            p.sort();
            p.dedup();
            p
        };
        Blocked { whole: self.whole, paths, block_at: self.block_at }
    }

    /// What the index's contents depend on: empty when nothing is blocked (so an index from before privacy files,
    /// or a file that blocks nothing, needs no work), else the blocked set. An index recorded under another key may
    /// hold what is blocked now, and is rebuilt (see `crate::open`).
    pub fn key(&self) -> String {
        if !self.blocks_anything() {
            return String::new();
        }
        if self.whole {
            return format!("{KEY_FORMAT}|whole");
        }
        format!("{KEY_FORMAT}|{}", self.blocked.join("\u{1f}"))
    }

    /// Pathspecs for `git log` and `git diff` leaving the blocked paths out, so their changes never leave git.
    pub fn exclude_pathspecs(&self) -> Vec<String> {
        self.blocked.iter().map(|b| format!(":(exclude,icase,literal){}", b.trim_end_matches('/'))).collect()
    }

    /// The `incomplete` detail naming the repo whose content an answer had to leave out.
    pub fn detail(&self, repo: &str) -> String {
        withheld_detail(repo, self.block_at)
    }
}

/// `AI privacy level 1 in <repo>` (`level 2 or lower` when the threshold is 2, and so on).
pub fn withheld_detail(repo: &str, block_at: u8) -> String {
    if block_at <= 1 { format!("AI privacy level 1 in {repo}") } else { format!("AI privacy level {block_at} or lower in {repo}") }
}

/// Whether the repo-relative `path` of the repo at `root` is blocked by its privacy file (for code reading files
/// at query time by a path no index listed: CI tool configs, Terraform directories).
pub fn blocks(root: &Path, path: &str) -> bool {
    Privacy::load(root).blocks(path)
}

/// Warns about a malformed privacy file (at `file`) once per process and file.
fn warn_once(file: &Path, error: &str, malformed: Malformed) {
    static WARNED: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);
    let key = format!("{}|{error}", file.display());
    let mut w = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if w.get_or_insert_with(Default::default).insert(key) {
        let taken = match malformed {
            Malformed::Restricted => "the whole repo is blocked (level 1) until it is fixed",
            Malformed::Default => "taken as absent ([privacy] malformed = \"default\")",
        };
        eprintln!("knowcode: {}: {error}; {taken}", file.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(text: &str, block_at: u8) -> Privacy {
        Privacy::from_text(Some(Ok(text.to_string())), &Format::default(), block_at, Malformed::Restricted)
    }

    fn parse(text: &str) -> Result<Option<Levels>, String> {
        super::parse(text, &Format::default())
    }

    #[test]
    fn a_configured_format_reads_its_own_keys_and_no_others() {
        let f = Format { file: ".policy.yml".into(), global_key: "repo_tier".into(), rules_key: "tiers".into(), path_key: "where".into(), level_key: "tier".into() };
        let l = super::parse("repo_tier: 2\ntiers:\n  - where: keys/\n    tier: 1\n", &f).unwrap().unwrap();
        assert_eq!((l.global, l.rules), (2, vec![Rule { path: "keys/".into(), level: 1 }]));
        let err = super::parse("level: 2\n", &f).unwrap_err();
        assert!(err.contains("`repo_tier` and `tiers`"), "{err}");
        assert!(super::parse("tiers:\n  - path: keys/\n    tier: 1\n", &f).unwrap_err().contains("unknown key"));
        assert!(Privacy::from_text(Some(Ok("tiers:\n  - where: /\n    tier: 1\n".into())), &f, 1, Malformed::Restricted).whole());
    }

    #[test]
    fn the_format_parses_with_paths_normalized() {
        let l = parse("level: 3\nrules:\n  - path: /my/folder/\n    level: 2\n  - path: lib/keys.cs\n    level: 1\n")
            .unwrap()
            .unwrap();
        assert_eq!(l.global, 3);
        assert_eq!(l.rules, vec![Rule { path: "my/folder/".into(), level: 2 }, Rule { path: "lib/keys.cs".into(), level: 1 }]);
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("# nothing here\n").unwrap(), None);
        assert_eq!(parse("rules:\n").unwrap().unwrap().global, DEFAULT_LEVEL);
    }

    #[test]
    fn a_malformed_file_blocks_the_repo_unless_configured_otherwise() {
        for bad in [
            "level: [1\n",
            "level: 0\n",
            "level: 5\n",
            "privacy: 1\n",
            "rules:\n  - level: 1\n",
            "rules:\n  - path: a/\n",
            "rules:\n  - path: ../other/\n    level: 1\n",
            "rules: a/\n",
            "- 1\n",
            "level: 3\n---\nlevel: 1\n",
        ] {
            let p = policy(bad, 1);
            assert!(matches!(p.state, FileState::Malformed { .. }), "{bad:?}");
            assert!(p.whole() && p.blocks("anything.cs"), "{bad:?}");
            let lenient = Privacy::from_text(Some(Ok(bad.to_string())), &Format::default(), 1, Malformed::Default);
            assert!(!lenient.blocks_anything(), "{bad:?}");
        }
        assert!(Privacy::from_text(Some(Err("unreadable".into())), &Format::default(), 1, Malformed::Restricted).whole());
    }

    #[test]
    fn an_empty_or_absent_file_is_the_default_level() {
        for p in [policy("", 1), policy("\n# c\n", 1), Privacy::from_text(None, &Format::default(), 1, Malformed::Restricted)] {
            assert!(!p.blocks_anything());
            assert_eq!(p.level_of("a/b.cs"), DEFAULT_LEVEL);
            assert_eq!(p.key(), "");
        }
    }

    #[test]
    fn invariant_a_path_is_never_more_permissive_than_its_parent() {
        let text = "level: 3\nrules:\n  - path: /vault/\n    level: 1\n  - path: /vault/public/\n    level: 4\n  - path: docs/\n    level: 2\n  - path: docs/secret.md\n    level: 1\n";
        let p = policy(text, 1);
        let paths = ["vault/a.cs", "vault/public/b.cs", "Vault/Public/c.cs", "docs/x.md", "docs/secret.md", "docs/secret.md/inner", "src/z.cs", "vaults/a.cs"];
        for path in paths {
            // Every ancestor's level bounds the path's.
            let mut parent = String::new();
            for part in path.split('/') {
                parent.push_str(part);
                assert!(p.level_of(path) <= p.level_of(&parent), "{path} vs {parent}");
                parent.push('/');
            }
            assert_eq!(p.blocks(path), p.level_of(path) <= 1, "{path}");
        }
        assert!(p.blocks("vault/public/b.cs"), "a child rule can't loosen its parent's");
        assert!(p.blocks("vault"), "the folder itself");
        assert!(!p.blocks("vaults/a.cs"));
        assert!(!p.blocks("docs/x.md"));
        assert!(p.blocks("docs/secret.md"));
        assert!(!p.blocks("src/z.cs"));
        assert_eq!(p.blocked().paths, vec!["docs/secret.md".to_string(), "vault/".to_string()]);
    }

    #[test]
    fn the_threshold_decides_what_is_blocked() {
        let text = "rules:\n  - path: conf/\n    level: 2\n  - path: keys/\n    level: 1\n";
        let at = |n: u8| policy(text, n);
        assert!(at(1).blocks("keys/a") && !at(1).blocks("conf/a"));
        assert!(at(2).blocks("keys/a") && at(2).blocks("conf/a"));
        assert!(!at(0).blocks_anything(), "0 blocks nothing");
        assert!(at(3).whole(), "the default level 3 is at or below 3");
        assert_ne!(at(1).key(), at(2).key());
        assert!(policy("level: 1\n", 1).whole());
        assert!(policy("rules:\n  - path: /\n    level: 1\n", 1).whole());
        assert_eq!(withheld_detail("r", 1), "AI privacy level 1 in r");
    }
}
