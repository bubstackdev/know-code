//! The git history of a range of lines: one definition's, read over the lines the index says it spans. The commits
//! that changed them (`git log -L`, which follows them through edits and renames of the file), and the files changed
//! in the same commits. Every walk is bounded in commits and, when the caller gives one, in time.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;

use crate::{git, vitals};

/// Characters of a commit subject kept.
const SUBJECT_CHARS: usize = 100;

/// One commit that changed the lines.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Change {
    /// The commit's full id.
    #[serde(skip)]
    pub id: String,
    /// Its first 12 characters.
    pub sha: String,
    pub date: String,
    pub author: String,
    pub subject: String,
}

/// A file changed in the same commits as the lines.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ChangedWith {
    pub file: String,
    /// Commits (of at most [`vitals::COCHANGE_MAX_FILES`] files) that changed both.
    pub shared: u32,
}

/// What a walk read.
#[derive(Default, Debug)]
pub struct Log {
    /// Newest first.
    pub changes: Vec<Change>,
    /// The walk stopped at its commit cap: older changes aren't known.
    pub capped: bool,
    /// The walk stopped at its time limit, keeping what it read.
    pub cut: bool,
}

/// Lines `start..=end` of the working tree's `file`, as lines of HEAD's version: `None` when its first line (a
/// definition's declaration) isn't committed as it is. Uncommitted edits before or inside the range move or shorten it.
pub fn head_span(root: &Path, file: &str, start: u32, end: u32) -> Result<Option<(u32, u32)>> {
    let spec = format!("HEAD:{file}");
    if git::run(root, &["cat-file", "-e", &spec]).is_err() {
        return Ok(None);
    }
    let diff = git::run(root, &["diff", "-U0", "--no-color", "--no-ext-diff", "HEAD", "--", file])?;
    Ok(map_lines(&String::from_utf8_lossy(&diff), start, end))
}

/// [`head_span`] over a `git diff -U0 HEAD` of the file: each working line outside the added ranges, moved by the
/// lines the hunks before it added and removed.
fn map_lines(diff: &str, start: u32, end: u32) -> Option<(u32, u32)> {
    let count = |s: &str| -> Option<(i64, i64)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    // (new start, new count, old count) per hunk.
    let hunks: Vec<(i64, i64, i64)> = diff
        .lines()
        .filter_map(|l| l.strip_prefix("@@ -"))
        .filter_map(|h| {
            let (old, rest) = h.split_once(" +")?;
            let new = rest.split_once(" @@").map_or(rest, |(n, _)| n);
            let (_, oc) = count(old)?;
            let (ns, nc) = count(new)?;
            Some((ns, nc, oc))
        })
        .collect();
    let map = |w: i64| -> Option<i64> {
        let mut offset = 0;
        for &(ns, nc, oc) in &hunks {
            // A hunk adding nothing (`+ns,0`) sits after line `ns`.
            let last = if nc == 0 { ns } else { ns + nc - 1 };
            if nc > 0 && (ns..=last).contains(&w) {
                return None;
            }
            if last < w {
                offset += oc - nc;
            }
        }
        Some(w + offset)
    };
    // Its first line (the declaration) uncommitted, the range isn't committed as it is: lines a diff happens to
    // match around new code (a brace, a blank line) would lend it another definition's history.
    let first = map(start as i64)?;
    let last = (start as i64..=end as i64).rev().find_map(map)?;
    Some((first as u32, last as u32))
}

/// Parses `git log --format=@@%H%x09%at%x09%aN%x09%s` output, newest first.
fn parse_changes(log: &str) -> Vec<Change> {
    log.lines()
        .filter_map(|l| l.strip_prefix("@@"))
        .filter_map(|l| {
            let mut f = l.splitn(4, '\t');
            let (sha, t, author, subject) = (f.next()?, f.next()?, f.next()?, f.next().unwrap_or_default());
            let subject: String = if subject.chars().count() > SUBJECT_CHARS {
                subject.chars().take(SUBJECT_CHARS - 1).chain(['…']).collect()
            } else {
                subject.to_string()
            };
            Some(Change { id: sha.to_string(), sha: sha[..sha.len().min(12)].to_string(), date: vitals::format_date(t.parse().ok()?), author: author.to_string(), subject })
        })
        .collect()
}

/// The commits that changed lines `a..=b` of HEAD's `file` (`git log -L`), newest first: at most `max`, only those
/// since `since` (a date git understands), and stopping at `until`, keeping what was read. Authors after `.mailmap`.
pub fn log(root: &Path, file: &str, (a, b): (u32, u32), since: Option<&str>, max: usize, until: Option<Instant>) -> Result<Log> {
    let range = format!("-L{a},{b}:{file}");
    let n = format!("-n{}", max + 1);
    let since = since.map(|s| format!("--since={}", s.trim()));
    let mut args = vec!["log", range.as_str(), "--no-patch", "--format=@@%H%x09%at%x09%aN%x09%s", n.as_str()];
    args.extend(since.as_deref());
    let (out, cut) = git::run_walk_until(root, &args, until)?;
    let mut changes = parse_changes(&String::from_utf8_lossy(&out));
    let capped = changes.len() > max;
    changes.truncate(max);
    Ok(Log { changes, capped, cut: cut.is_some() })
}

/// The files changed in `shas` other than `own`, existing now and not blocked by the repo's AI privacy file, by
/// shared commits, most first; and how many commits were left out for changing more than
/// [`vitals::COCHANGE_MAX_FILES`] files (sweeps say nothing about what belongs together).
pub fn changed_with(root: &Path, shas: &[String], own: &str) -> Result<(Vec<ChangedWith>, u32)> {
    if shas.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let input = shas.join("\n") + "\n";
    let out = git::run_with_input(root, &["diff-tree", "--stdin", "-r", "--name-only", "--root", "-m", "--first-parent", "--no-renames"], input.as_bytes())?;
    let now: HashSet<String> = git::run(root, &["ls-files", "-z"])
        .map(|o| o.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect())
        .unwrap_or_default();
    let privacy = crate::privacy::Privacy::load(root);
    let text = String::from_utf8_lossy(&out);
    // `--stdin` prints each commit's id, then its files.
    let given: HashSet<&str> = shas.iter().map(String::as_str).collect();
    let mut commits: Vec<Vec<&str>> = Vec::new();
    for l in text.lines().filter(|l| !l.is_empty()) {
        if given.contains(l) {
            commits.push(Vec::new());
        } else if let Some(c) = commits.last_mut() {
            c.push(l);
        }
    }
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut sweeps = 0;
    for files in commits {
        let files: HashSet<&str> = files.into_iter().collect();
        if files.len() > vitals::COCHANGE_MAX_FILES {
            sweeps += 1;
            continue;
        }
        for f in files {
            if f != own && now.contains(f) && !privacy.blocks(f) {
                *counts.entry(f.to_string()).or_default() += 1;
            }
        }
    }
    let mut list: Vec<ChangedWith> = counts.into_iter().map(|(file, shared)| ChangedWith { file, shared }).collect();
    list.sort_by(|a, b| b.shared.cmp(&a.shared).then_with(|| a.file.cmp(&b.file)));
    Ok((list, sweeps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_lines_map_to_head_past_uncommitted_edits() {
        // Two lines added after line 3 (working 4-5), and HEAD's line 10 removed (after working line 11).
        let diff = "@@ -3,0 +4,2 @@\n+a\n+b\n@@ -10 +11,0 @@\n-c\n";
        assert_eq!(map_lines(diff, 1, 3), Some((1, 3)));
        assert_eq!(map_lines(diff, 6, 9), Some((4, 7)));
        // A range whose first line is uncommitted isn't committed as it is; one ending in added lines ends before them.
        assert_eq!(map_lines(diff, 4, 7), None);
        assert_eq!(map_lines(diff, 2, 5), Some((2, 3)));
        // Past the removed line: two added and one removed before it.
        assert_eq!(map_lines(diff, 12, 12), Some((11, 11)));
        // A changed line (`-7 +7`) isn't committed as it is.
        let changed = "@@ -7 +7 @@\n-x\n+y\n";
        assert_eq!(map_lines(changed, 6, 8), Some((6, 8)));
        assert_eq!(map_lines(changed, 7, 7), None);
        assert_eq!(map_lines("", 5, 9), Some((5, 9)));
    }

    #[test]
    fn changes_parse_and_long_subjects_are_shortened() {
        let long = "x".repeat(150);
        let log = format!("@@{}\t1700000000\tAnn\tFix the cart\n\n@@{}\t1600000000\tBob\t{long}\n", "a".repeat(40), "b".repeat(40));
        let c = parse_changes(&log);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].sha.as_str(), c[0].author.as_str(), c[0].subject.as_str()), ("aaaaaaaaaaaa", "Ann", "Fix the cart"));
        assert_eq!(c[1].subject.chars().count(), SUBJECT_CHARS);
    }
}
