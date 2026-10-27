//! `task`: one bounded answer for a goal written in words ("refunds fail for partial captures"), where a client would
//! otherwise join `locate`, `context`, `impact` and `tests` itself, each with its own continuation. Four sections:
//!
//! - `items`: the files most likely involved (`locate`'s ranking), each with the symbols matching the goal and, for the
//!   first few, the line declaring the best of them: the source that matters;
//! - `dependents`: what depends on the first few files (`impact` by file, one hop) and the files that usually change
//!   with them (co-change), the files a change there most often reaches;
//! - `tests`: the tests to run (`tests` on those files), advisory: a test not listed is never said to be safe to skip;
//! - `unsettled`: what the index can't settle: goal words no listed file matches, edges left out as ambiguous,
//!   constructs not read, content withheld.
//!
//! Each section says whether it is whole and gives the call that continues it (`sections`); a cut section is an
//! `incomplete` entry too. Rows are short text, so the answer stays within what `locate` alone returns.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use kc_store::answer::{Incomplete, Reason};
use serde_json::{Value, json};

use crate::contract::{finish, incomplete_of, push_incomplete};
use crate::ops::{self, Out, Scope};

pub struct TaskParams {
    pub goal: String,
    /// Files listed at most.
    pub files: usize,
    /// Dependents listed at most.
    pub dependents: usize,
    /// Tests listed at most.
    pub tests: usize,
}

/// The files whose dependents and tests the answer reads: the first few, where most of a change lands.
const WALKED: usize = 3;
/// The files whose declaring line is quoted.
const QUOTED: usize = 2;
/// What the tests section is: advice, never a list of the only tests to run.
pub const ADVISORY: &str = "the most likely tests, not all: a test not listed may still fail";

/// Characters of a quoted line kept.
const QUOTE_CHARS: usize = 120;

/// The reasons a sub-answer gives that the task can't settle: they go to `unsettled`, said once each.
const UNSETTLED: &[Reason] = &[Reason::Policy, Reason::Unsupported, Reason::Withheld, Reason::MissingRepo, Reason::Failed, Reason::Stale];

/// The task answer for `p.goal` over `scope`'s repos.
pub fn task(p: &TaskParams, scope: &Scope) -> Result<Out<Value>> {
    let goal = p.goal.trim();
    let loc = ops::locate(goal, p.files, scope)?;
    let multi = loc.data.multi;
    let place = |root: &Path, file: &str| if multi { format!("{}/{file}", ops::repo_name(root)) } else { file.to_string() };
    let listed: HashSet<(PathBuf, String)> = loc.data.items.iter().map(|(r, h)| (r.clone(), h.file.clone())).collect();
    let mut unsettled: Vec<String> = Vec::new();
    let said = |unsettled: &mut Vec<String>, line: String| {
        if !unsettled.contains(&line) {
            unsettled.push(line);
        }
    };

    // The files, with the symbols matching the goal; the first few with their best symbol's declaring line.
    let files: Vec<Value> = loc
        .data
        .items
        .iter()
        .enumerate()
        .map(|(i, (root, h))| {
            let syms: Vec<String> = h
                .symbols
                .iter()
                .map(|s| {
                    let name = s.container.as_ref().map_or(s.name.clone(), |c| format!("{c}.{}", s.name));
                    format!("{name} ({} :{})", s.kind, s.line)
                })
                .collect();
            let mut row = place(root, &h.file);
            if !syms.is_empty() {
                row.push_str(&format!(" | {}", syms.join(", ")));
            }
            if i < QUOTED
                && let Some(s) = h.symbols.first()
                && let Some(line) = quote(root, &h.file, s.line)
            {
                row.push_str(&format!(" | {}: {line}", s.line));
            }
            Value::from(row)
        })
        .collect();

    // Goal words no listed file matches: what the ranking couldn't place.
    let matched: HashSet<&str> = loc.data.items.iter().flat_map(|(_, h)| h.terms.iter().map(String::as_str)).collect();
    let unmatched: Vec<&str> = loc.data.terms.iter().map(String::as_str).filter(|t| !matched.contains(t)).collect();
    if !unmatched.is_empty() {
        said(&mut unsettled, format!("no listed file matches {}: search them as text (kc_search) or name the code", unmatched.join(", ")));
    }

    // What depends on the first files, and what changes with them.
    let walked: Vec<&(PathBuf, kc_store::locate::LocateHit)> = loc.data.items.iter().take(WALKED).collect();
    // Each with its rank: confident edges first, then co-change, then ambiguous edges.
    let mut dependents: Vec<(u8, String)> = Vec::new();
    let mut dep_seen: HashSet<(PathBuf, String)> = HashSet::new();
    let mut deps_total = 0;
    let mut deps_whole = true;
    let mut ambiguous = false;
    for (root, h) in &walked {
        let one = Scope { repos: vec![root.clone()], no_sync: scope.no_sync };
        let ip = ops::ImpactParams { file: Some(h.file.clone()), depth: 1, limit: 30, ..Default::default() };
        let out = match ops::impact(&ip, &one) {
            Ok(out) => out,
            Err(e) => {
                said(&mut unsettled, format!("{}: dependents not read ({e:#})", place(root, &h.file)));
                deps_whole = false;
                continue;
            }
        };
        for i in incomplete_of(&out.env) {
            if i.reason == Reason::Policy {
                // Said once for the walk: the names past the frontier are an `impact` call away.
                ambiguous = true;
            } else if UNSETTLED.contains(&i.reason) {
                said(&mut unsettled, format!("{}: {}", i.reason.as_str(), i.detail));
            }
            if matches!(i.reason, Reason::Capped | Reason::Budget | Reason::Policy | Reason::Depth) {
                deps_whole = false;
            }
        }
        for r in &out.data {
            for a in r.impact.affected.iter().filter(|a| !kc_store::is_test_path(&a.file) && a.repo.is_none()) {
                let key = (root.clone(), a.file.clone());
                if listed.contains(&key) || !dep_seen.insert(key) {
                    continue;
                }
                deps_total += 1;
                let rank = match a.confidence { "exact" => 0, "unique_name" => 1, _ => 3 };
                dependents.push((rank, format!("{}:{} {} → {} [{}]", place(root, &a.file), a.line, a.qualified, a.via, a.confidence)));
            }
            for c in r.coupled.iter().filter(|c| !kc_store::is_test_path(&c.file)) {
                let key = (root.clone(), c.file.clone());
                if listed.contains(&key) || !dep_seen.insert(key) {
                    continue;
                }
                deps_total += 1;
                dependents.push((2, format!("{} (changes with {}: {} of {} commits)", place(root, &c.file), c.with, c.support, c.commits)));
            }
        }
    }
    if ambiguous {
        said(&mut unsettled, "which of several implementations calls reach: edges left as ambiguous weren't walked past (impact with evidence: all follows them)".into());
    }
    dependents.sort_by_key(|(rank, _)| *rank);
    let deps_cut = dependents.len() > p.dependents;
    let dependents: Vec<String> = dependents.into_iter().take(p.dependents).map(|(_, row)| row).collect();

    // The tests to run for those files, per repo.
    let mut tests: Vec<String> = Vec::new();
    let mut tests_total = 0;
    let mut targets_by_root: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for (root, h) in &walked {
        match targets_by_root.iter_mut().find(|(r, _)| r == root) {
            Some((_, t)) => t.push(h.file.clone()),
            None => targets_by_root.push((root.clone(), vec![h.file.clone()])),
        }
    }
    for (root, targets) in &targets_by_root {
        let one = Scope { repos: vec![root.clone()], no_sync: scope.no_sync };
        let tp = ops::TestsParams { targets: targets.clone(), diff: None, limit: p.tests, evidence: String::new() };
        match ops::tests(&tp, &one) {
            Ok(out) => {
                for r in &out.data {
                    tests_total += r.selection.selected.len() + r.selection.left_out.len();
                    // The file and its likelihood: the reasons are a `tests` call away.
                    for t in &r.selection.selected {
                        tests.push(format!("{} ({:.2})", place(root, &t.file), t.confidence));
                    }
                }
            }
            Err(e) => said(&mut unsettled, format!("tests not read ({e:#})")),
        }
    }
    tests.truncate(p.tests);

    // The envelope `locate` made (repos, provenance, its own entries), with the sections.
    // The calls continuing each section, on the files listed first (said once, not a call per file).
    let deps_next = format!("kc_impact {{\"file\": <each of the first {} items>, \"depth\": 2}}", walked.len());
    let tests_next = format!("kc_tests {{\"targets\": <the first {} items>, \"limit\": 30}}", walked.len());
    let mut env = loc.env;
    // Said by the sections instead.
    if let Some(o) = env.as_object_mut() {
        o.remove("hint");
    }
    crate::contract::retain_incomplete(&mut env, |i| !matches!(i.reason, Reason::Capped | Reason::Budget));
    let files_total = loc.data.total;
    let files_cut = files_total > files.len();
    env["goal"] = Value::from(goal);
    env["items"] = Value::Array(files);
    env["dependents"] = json!(dependents);
    env["dependents_total"] = json!(deps_total);
    env["tests"] = json!(tests);
    env["tests_total"] = json!(tests_total);
    env["unsettled"] = json!(unsettled);
    let files_next = format!("kc_locate {{\"query\": {}, \"limit\": {}}}", json!(goal), (p.files * 2).max(20));
    env["sections"] = json!({
        "items": { "complete": !files_cut, "shown": env["items"].as_array().map_or(0, Vec::len), "total": files_total, "next": files_next },
        "dependents": { "complete": deps_whole && !deps_cut, "of": walked.len(), "next": deps_next },
        "tests": { "complete": tests_total <= tests.len(), "advisory": ADVISORY, "next": tests_next },
    });
    if files_cut {
        let shown = env["items"].as_array().map_or(0, Vec::len);
        push_incomplete(&mut env, Incomplete::capped("items", shown, files_total).with_hint("sections.items.next"));
    }
    if deps_cut || !deps_whole {
        let detail = format!("dependents: {} of {deps_total} shown, of the first {} files, one hop", dependents.len(), walked.len());
        push_incomplete(&mut env, Incomplete::new(Reason::Capped, detail).with_hint("sections.dependents.next"));
    }
    if tests_total > tests.len() {
        let detail = format!("tests: {} of {tests_total} candidates shown, the most likely first", tests.len());
        push_incomplete(&mut env, Incomplete::new(Reason::Capped, detail).with_hint("sections.tests.next"));
    }
    finish(&mut env);
    let data = env.clone();
    Ok(Out { env, data })
}

/// Line `line` of `file` in the repo at `root`, trimmed and cut to [`QUOTE_CHARS`]; none when the privacy file blocks
/// it or it can't be read.
fn quote(root: &Path, file: &str, line: u32) -> Option<String> {
    if kc_index::privacy::blocks(root, file) {
        return None;
    }
    let text = std::fs::read_to_string(root.join(file)).ok()?;
    let l = text.lines().nth(line.checked_sub(1)? as usize)?.trim();
    Some(l.chars().take(QUOTE_CHARS).collect())
}
