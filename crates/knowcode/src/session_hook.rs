//! `knowcode hook session-start`: a Claude Code `SessionStart` hook. At the start of a session (and after `/clear` or
//! a compaction, which leave the context without it) it adds a line for the agent saying what KnowCode has here (this
//! repo indexed or about to be, or how many repos of a workspace root are) and which tools answer callers, impact,
//! HTTP and definition questions.
//!
//! It says nothing where KnowCode has nothing to offer: outside a git repo (unless in a workspace root), in a repo
//! with no index that isn't under a workspace root, in a repo whose privacy file blocks it whole, when there is no
//! index at all, when `hooks.session_start` is off, and when a session is resumed or forked (its transcript already
//! holds the line). Every turn after it pays for the line, so it is kept to about 40 tokens.
//!
//! It runs at every session start, so it only reads metadata: the config file, the privacy file, and the cache's
//! directory listing. It never opens or creates an index, makes no network call, and fails open like the
//! PreToolUse hook (see [`crate::hook`]).

use std::path::Path;

use kc_index::privacy::Privacy;
use serde_json::{Value, json};

/// The sources it speaks for: a new session, `/clear`, and a compaction. A resumed or forked session carries the
/// line it had.
const SOURCES: &[&str] = &["startup", "clear", "compact"];

/// The hook's output for one SessionStart event, or `None` to add nothing.
pub fn answer(event: &Value) -> Option<Value> {
    if let Some(source) = event["source"].as_str()
        && !SOURCES.contains(&source)
    {
        return None;
    }
    let cwd = event["cwd"].as_str().map(std::path::PathBuf::from).or_else(|| std::env::current_dir().ok())?;
    let note = note(&cwd)?;
    Some(json!({ "hookSpecificOutput": { "hookEventName": "SessionStart", "additionalContext": note } }))
}

/// What KnowCode has for a session in `cwd`, or `None`.
pub fn note(cwd: &Path) -> Option<String> {
    if !kc_index::config::session_hook().ok()?.0 {
        return None;
    }
    let indexes = index_count()?;
    let here = match kc_index::repo_root(cwd) {
        Ok(root) => {
            if Privacy::load(&root).whole() {
                return None;
            }
            let indexed = kc_index::db_path(&root).ok()?.exists();
            if !indexed && !kc_index::roots::Roots::configured().ok().flatten().is_some_and(|mut r| r.contains(&root)) {
                return None;
            }
            Here::Repo { name: root.file_name()?.to_string_lossy().into_owned(), indexed }
        }
        // A workspace root, or a directory under one, that isn't a repo itself.
        Err(_) => {
            let cwd = std::fs::canonicalize(cwd).ok()?;
            let (roots, _) = kc_index::config::workspace().ok()?;
            if !roots.iter().filter_map(|r| std::fs::canonicalize(r).ok()).any(|r| cwd.starts_with(r)) {
                return None;
            }
            Here::Workspace
        }
    };
    text(&here, indexes, crate::mcp::is_one(&crate::mcp::profile_spec()))
}

/// Where the session is, for [`text`].
#[derive(Debug, Clone, PartialEq)]
pub enum Here {
    /// In a repo, by its directory's name, indexed or not yet.
    Repo { name: String, indexed: bool },
    /// In a workspace root that isn't a repo.
    Workspace,
}

/// The line itself, from where the session is and how many indexes the cache holds. Every later turn pays for it
/// (about 40 tokens), so it says only what the tool list doesn't: what is indexed here, and which tool answers the
/// questions agents most often took to grep, by the names the plugin's server profile gives them (`one`: ops of `kc`).
pub fn text(here: &Here, indexes: usize, one: bool) -> Option<String> {
    let first = match here {
        Here::Repo { name, indexed: true } => format!("KnowCode indexes {name}."),
        Here::Repo { name, indexed: false } => format!("KnowCode indexes {name} on first use."),
        Here::Workspace if indexes == 0 => return None,
        Here::Workspace => format!("KnowCode indexes {indexes} repo{} here.", if indexes == 1 { "" } else { "s" }),
    };
    // Under `one`, the ops of `kc`: 44 tokens for a workspace of 12 repos (count_tokens), against 45 allowed.
    Some(if one {
        format!("{first} Callers, impact, HTTP: kc {{op: impact}}; definitions: op find.")
    } else {
        format!("{first} Callers, impact, HTTP: kc_impact; definitions: kc_find.")
    })
}

/// How many repo indexes the cache holds, from its directory listing alone; `None` when it holds none.
fn index_count() -> Option<usize> {
    let dir = kc_index::cache_dir().ok()?.join("repos");
    let n = std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "sqlite")).count();
    (n > 0).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_says_what_is_indexed_and_names_the_tools() {
        let repo = |indexed| Here::Repo { name: "shop".into(), indexed };
        let t = |here: Here, n| text(&here, n, false).unwrap();
        assert_eq!(t(repo(true), 1), "KnowCode indexes shop. Callers, impact, HTTP: kc_impact; definitions: kc_find.");
        assert_eq!(t(repo(true), 13), t(repo(true), 1));
        assert_eq!(t(repo(false), 3), "KnowCode indexes shop on first use. Callers, impact, HTTP: kc_impact; definitions: kc_find.");
        assert_eq!(t(Here::Workspace, 4), "KnowCode indexes 4 repos here. Callers, impact, HTTP: kc_impact; definitions: kc_find.");
        assert_eq!(t(Here::Workspace, 1), "KnowCode indexes 1 repo here. Callers, impact, HTTP: kc_impact; definitions: kc_find.");
        assert_eq!(text(&Here::Workspace, 0, false), None);
        // Under `one`, the default: the same tools, as ops of `kc`.
        assert_eq!(text(&repo(true), 1, true).unwrap(), "KnowCode indexes shop. Callers, impact, HTTP: kc {op: impact}; definitions: op find.");
        assert_eq!(text(&Here::Workspace, 0, true), None);
    }

    #[test]
    fn resumed_and_forked_sessions_get_nothing_more() {
        for source in ["resume", "fork", "something-new"] {
            assert!(answer(&json!({ "hook_event_name": "SessionStart", "source": source, "cwd": "/" })).is_none(), "{source}");
        }
    }
}
