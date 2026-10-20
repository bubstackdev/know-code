//! `knowcode history deepen`: fetch more history into a shallow clone, so the history-based answers have something
//! to read, and say how deep the history is before and after. CLI only, like `fetch-deps` (see docs/decisions.md).
//! It runs `git fetch --deepen=N` (or `--shallow-since=DATE`) and nothing else: no ref is written but what that
//! fetch writes. A clone that isn't shallow is left alone.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use clap::Args;
use kc_index::vitals;
use serde::Serialize;
use serde_json::{Value, json};

use crate::ops::{self, repo_name};

/// Commits `history deepen` fetches when neither `--by` nor `--since` is given.
pub const DEFAULT_DEEPEN: u32 = 1000;

/// The window `hotspots` and `coupled` read, ending at HEAD.
const WINDOW_DAYS: i64 = 365;

#[derive(Args)]
pub struct DeepenOptions {
    /// The shallow clone to deepen (default: the repo containing the current directory).
    #[arg(long, short = 'r')]
    pub repo: Option<PathBuf>,
    /// Commits to fetch beyond the current boundary (`git fetch --deepen=N`); default 1000.
    #[arg(long, value_name = "N", conflicts_with = "since")]
    pub by: Option<u32>,
    /// Fetch every commit since DATE instead (`git fetch --shallow-since=DATE`): `2025-01-01`, `1 year ago`.
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    /// Emit the JSON envelope instead of text.
    #[arg(long)]
    pub json: bool,
}

/// How much history a checkout holds.
#[derive(Serialize, Clone, Copy)]
struct Depth {
    shallow: bool,
    /// Commits reachable from HEAD.
    commits: u64,
    /// Of those, the ones history-based answers count: a shallow clone's boundary commits are left out.
    counted: u64,
    /// The oldest counted commit's committer time, and HEAD's.
    #[serde(skip)]
    oldest: Option<i64>,
    #[serde(skip)]
    head: Option<i64>,
}

impl Depth {
    fn days(&self) -> i64 {
        match (self.oldest, self.head) {
            (Some(o), Some(h)) => ((h - o).max(0)) / 86_400,
            _ => 0,
        }
    }

    fn json(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(o) = self.oldest {
            v["since"] = json!(vitals::format_date(o));
            v["days"] = json!(self.days());
        }
        v
    }
}

fn git(root: &Path, args: &[&str], stdin: bool) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(if stdin { Stdio::inherit() } else { Stdio::null() })
        .output()
        .map_err(|e| format!("running git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(crate::fetch::git_error(&String::from_utf8_lossy(&out.stderr)))
    }
}

fn measure(root: &Path) -> Depth {
    let boundary: HashSet<String> = vitals::shallow_commits(root);
    let mut d = Depth { shallow: !boundary.is_empty(), commits: 0, counted: 0, oldest: None, head: None };
    let Ok(log) = git(root, &["log", "--format=%H %ct", "HEAD"], false) else { return d };
    for line in String::from_utf8_lossy(&log).lines() {
        let Some((sha, t)) = line.split_once(' ') else { continue };
        let t: i64 = t.trim().parse().unwrap_or(0);
        d.commits += 1;
        d.head.get_or_insert(t);
        if !boundary.contains(sha) {
            d.counted += 1;
            d.oldest = Some(d.oldest.map_or(t, |o| o.min(t)));
        }
    }
    d
}

/// What each history-based answer has to read at depth `d`: `complete` (all it reads is here), `partial` or `none`.
fn readiness(d: &Depth) -> Vec<Value> {
    [
        ("hotspots", true, "churn and fix commits over the last 365 days"),
        ("coupled", true, "files changed in the same commits over the last 365 days"),
        ("ownership", false, "every commit"),
        ("reviewers", false, "every commit, recent ones weighted most"),
    ]
    .into_iter()
    .map(|(answer, windowed, reads)| {
        let since = d.oldest.map(vitals::format_date).unwrap_or_default();
        let (status, detail) = if !d.shallow {
            ("complete", "the full history is here".to_string())
        } else if d.counted == 0 {
            ("none", "no countable history: only the shallow boundary commit".to_string())
        } else if windowed && d.days() >= WINDOW_DAYS {
            ("complete", format!("the window is covered: history since {since}"))
        } else if windowed {
            ("partial", format!("{} of the window's {WINDOW_DAYS} days: history since {since} ({} commits)", d.days(), d.counted))
        } else {
            ("partial", format!("history since {since} only ({} commits); before that is unknown", d.counted))
        };
        json!({ "answer": answer, "reads": reads, "status": status, "detail": detail })
    })
    .collect()
}

pub fn deepen(o: DeepenOptions) -> Result<()> {
    let start = match &o.repo {
        Some(r) => r.clone(),
        None => std::env::current_dir()?,
    };
    let root = kc_index::repo_root(&start)?;
    let fetch = match (&o.since, o.by) {
        (Some(date), _) => {
            if date.trim().is_empty() || date.chars().any(char::is_control) {
                bail!("--since takes a date git understands: `2025-01-01`, `1 year ago`");
            }
            format!("--shallow-since={}", date.trim())
        }
        (None, by) => {
            let n = by.unwrap_or(DEFAULT_DEEPEN);
            if n == 0 {
                bail!("--by takes a number of commits above 0");
            }
            format!("--deepen={n}")
        }
    };
    let before = measure(&root);
    let mut env = json!({ "repo": root, "shallow": before.shallow, "before": before.json() });
    if !before.shallow {
        env["note"] = json!("not a shallow clone: its full history is here; nothing to deepen");
        env["answers"] = json!(readiness(&before));
    } else {
        git(&root, &["fetch", "--quiet", &fetch], true).map_err(|e| anyhow::anyhow!("git fetch {fetch} failed in {}: {e}", root.display()))?;
        let after = measure(&root);
        env["fetched"] = json!(fetch);
        env["after"] = after.json();
        env["answers"] = json!(readiness(&after));
        if !after.shallow {
            env["note"] = json!("the clone holds its full history now");
        }
    }
    ops::finish(&mut env);
    if o.json {
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }
    let line = |d: &Value| {
        let since = d["since"].as_str().map(|s| format!(", since {s} ({} days)", d["days"])).unwrap_or_default();
        let shallow = if d["shallow"].as_bool() == Some(true) { "shallow" } else { "full history" };
        format!("{} commits, {} counted{since}; {shallow}", d["commits"], d["counted"])
    };
    println!("{}: {}", repo_name(&root), line(&env["before"]));
    if let Some(f) = env["fetched"].as_str() {
        println!("fetched {f}: {}", line(&env["after"]));
    }
    if let Some(n) = env["note"].as_str() {
        println!("{n}");
    }
    for a in env["answers"].as_array().into_iter().flatten() {
        println!("  {:<10} {:<9} {}", a["answer"].as_str().unwrap_or_default(), a["status"].as_str().unwrap_or_default(), a["detail"].as_str().unwrap_or_default());
    }
    Ok(())
}
