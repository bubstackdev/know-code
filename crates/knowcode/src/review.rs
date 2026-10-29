//! `review`: one bounded answer for a diff, where a client reviewing a change would otherwise join `impact --diff`,
//! `tests` and `coupled` itself, each with its own continuation. Its sections, as they are read:
//!
//! - `changes`: what the diff changes, by file, with the symbols in its changed lines and the definitions it removes;
//! - `tests`: the tests to run, advisory: a test not listed is never said to be safe to skip;
//! - `missed`: files that usually change with the changed files, which the diff leaves alone;
//! - `dependents`: what depends on the changed symbols, one hop, in this repo and in other repos, by file;
//! - `contracts`: the routes, messages and tables the diff changes, with the consumers in other repos it breaks;
//! - `unsettled`: what the index can't settle: edges left as ambiguous, constructs not read, content withheld.
//!
//! Each section says whether it is whole and gives the call that continues it (`sections`); a cut section is an
//! `incomplete` entry too. Rows are objects with a file and a line, so a renderer (markdown, SARIF) places them
//! without parsing text. The dependents and the tests are walked on one symbol graph (see
//! [`crate::walk::Graph::begin_walk`]), as `task` walks them.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use kc_store::answer::{Incomplete, Reason};
use serde_json::{Value, json};

use crate::contract::{finish, incomplete_of, push_incomplete, retain_incomplete};
use crate::ops::{self, Out, Scope};

pub struct ReviewParams {
    /// `working` (uncommitted changes) or a git revision to diff the working tree against.
    pub diff: String,
    /// Changed files listed at most.
    pub changes: usize,
    /// Dependent files listed at most.
    pub dependents: usize,
    /// Tests listed at most.
    pub tests: usize,
    /// Missed partners listed at most.
    pub missed: usize,
}

/// What the tests section is: advice, never a list of the only tests to run.
pub const ADVISORY: &str = crate::task::ADVISORY;

/// Symbols listed per changed file at most; the rest are counted.
const SYMBOLS_PER_FILE: usize = 6;
/// Rows listed per dependent file at most; the rest are counted.
const ROWS_PER_FILE: usize = 3;
/// The dependents walk: one hop, at most this many symbols.
const WALK_LIMIT: usize = 60;

/// The reasons a sub-answer gives that the review can't settle: they go to `unsettled`, said once each.
const UNSETTLED: &[Reason] = &[Reason::Policy, Reason::Unsupported, Reason::Withheld, Reason::MissingRepo, Reason::Failed, Reason::Stale];

/// The review answer for the diff `p.diff` of the one repo `scope` names.
pub fn review(p: &ReviewParams, scope: &Scope) -> Result<Out<Value>> {
    let roots = ops::target_repos(&scope.repos)?;
    let [root] = roots.as_slice() else { bail!("review reads one repo's diff: name the repo") };
    let _synced = kc_store::limits::current().remember_syncs();
    let _memo = kc_index::memo_changes();
    let Some(o) = ops::open_member(&roots, root, scope.no_sync)? else { bail!("{} isn't indexed", root.display()) };
    let mut g = crate::walk::Graph::new(&o, &[], scope.no_sync, usize::MAX);
    let ip = ops::ImpactParams { diff: Some(p.diff.clone()), depth: 1, limit: WALK_LIMIT, ..Default::default() };
    let imp = ops::impact_on(&ip, scope, &o, &mut g)?;
    let tp = ops::TestsParams { targets: Vec::new(), diff: Some(p.diff.clone()), limit: p.tests, evidence: String::new() };
    let tests = ops::tests_on(&tp, &o, &mut g);
    drop(g);

    let mut unsettled: Vec<String> = Vec::new();
    let said = |unsettled: &mut Vec<String>, line: String| {
        if !unsettled.contains(&line) {
            unsettled.push(line);
        }
    };
    let mut deps_whole = true;
    let mut ambiguous = false;
    for i in incomplete_of(&imp.env) {
        if i.reason == Reason::Policy {
            ambiguous = true;
        } else if UNSETTLED.contains(&i.reason) {
            said(&mut unsettled, format!("{}: {}", i.reason.as_str(), i.detail));
        }
        if matches!(i.reason, Reason::Capped | Reason::Budget | Reason::Policy | Reason::Depth) {
            deps_whole = false;
        }
    }
    if ambiguous {
        said(&mut unsettled, "which of several implementations calls reach: edges left as ambiguous weren't walked past (impact with evidence: all follows them)".into());
    }
    let Some(r) = imp.data.first() else { bail!("{}: nothing to review", root.display()) };

    // What the diff changes, by file: its symbols in the changed lines, and the definitions it removes.
    let mut by_file: Vec<(String, Value)> = Vec::new();
    for c in &r.changes {
        let change = if c.deleted {
            "deleted"
        } else if c.renamed_from.is_some() {
            "renamed"
        } else if c.old_blob.is_none() {
            "added"
        } else {
            "modified"
        };
        let mut row = json!({ "file": c.file, "change": change });
        if let Some(from) = &c.renamed_from {
            row["renamed_from"] = json!(from);
        }
        by_file.push((c.file.clone(), row));
    }
    let mut symbols: HashMap<String, Vec<Value>> = HashMap::new();
    for s in &r.impact.scope {
        symbols.entry(s.file.clone()).or_default().push(json!({ "symbol": s.qualified, "kind": s.kind, "line": s.line, "change": "changed" }));
    }
    for d in &r.removed {
        symbols.entry(d.file.clone()).or_default().push(json!({ "symbol": d.qualified, "kind": d.kind, "line": d.line, "change": "removed" }));
    }
    for (file, row) in &mut by_file {
        // A deleted file's removed definitions are at its own path; a renamed file's at the path it had.
        let mut list = symbols.remove(file.as_str()).unwrap_or_default();
        if let Some(from) = row["renamed_from"].as_str() {
            list.extend(symbols.remove(from).unwrap_or_default());
        }
        let total = list.len();
        list.truncate(SYMBOLS_PER_FILE);
        if !list.is_empty() {
            row["symbols"] = json!(list);
        }
        if total > SYMBOLS_PER_FILE {
            row["symbols_total"] = json!(total);
        }
    }
    let changed: HashSet<String> = by_file.iter().map(|(f, _)| f.clone()).collect();
    let changes_total = by_file.len();
    let changes: Vec<Value> = by_file.into_iter().take(p.changes).map(|(_, row)| row).collect();

    // The tests to run.
    let (mut test_rows, mut tests_total) = (Vec::new(), 0);
    match &tests {
        Ok(out) => {
            for t in out.data.iter() {
                tests_total += t.selection.selected.len() + t.selection.left_out.len();
                for pick in t.selection.selected.iter().take(p.tests) {
                    test_rows.push(json!({ "file": pick.file, "confidence": (pick.confidence * 100.0).round() / 100.0 }));
                }
            }
            for i in incomplete_of(&out.env).into_iter().filter(|i| UNSETTLED.contains(&i.reason)) {
                said(&mut unsettled, format!("{}: {}", i.reason.as_str(), i.detail));
            }
        }
        Err(e) => said(&mut unsettled, format!("tests not read ({e:#})")),
    }

    // Usual partners the diff leaves alone: the missed partners first, then files coupled to the changed ones that the
    // walk didn't reach.
    let mut missed: Vec<Value> = Vec::new();
    let mut missed_seen: HashSet<&str> = HashSet::new();
    for m in &r.missed_partners {
        if missed_seen.insert(&m.file) {
            missed.push(json!({ "file": m.file, "with": m.with, "support": m.support, "commits": m.commits }));
        }
    }
    for c in r.coupled.iter().filter(|c| !changed.contains(&c.file)) {
        if missed_seen.insert(&c.file) {
            missed.push(json!({ "file": c.file, "with": c.with, "support": c.support, "commits": c.commits }));
        }
    }
    let missed_total = missed.len();
    missed.truncate(p.missed);

    // What depends on the changed symbols, by file, outside the changed files and the tests (the tests section lists
    // those): in this repo in the walk's order (nearest first, then by file rank), then in other repos.
    let mut dep_files: Vec<(Option<String>, String, Vec<Value>, usize)> = Vec::new();
    let mut at: HashMap<(Option<String>, String), usize> = HashMap::new();
    let mut put = |repo: Option<String>, file: &str, row: Value| {
        let key = (repo.clone(), file.to_string());
        let i = *at.entry(key).or_insert_with(|| {
            dep_files.push((repo, file.to_string(), Vec::new(), 0));
            dep_files.len() - 1
        });
        let entry = &mut dep_files[i];
        entry.3 += 1;
        if entry.2.len() < ROWS_PER_FILE {
            entry.2.push(row);
        }
    };
    for a in r.impact.affected.iter().filter(|a| !kc_store::is_test_path(&a.file)) {
        if a.repo.is_none() && changed.contains(&a.file) {
            continue;
        }
        let mut row = json!({ "line": a.line, "symbol": a.qualified, "via": a.via, "edge": a.edge, "confidence": a.confidence, "hop": a.hop });
        if !a.also.is_empty() {
            row["also"] = json!(a.also);
        }
        put(a.repo.clone(), &a.file, row);
    }
    for h in r.cross_repo.iter().filter(|h| !kc_store::is_test_path(&h.file)) {
        let symbol = h.from.as_ref().map(|f| f.container.as_ref().map_or(f.name.clone(), |c| format!("{c}.{}", f.name))).unwrap_or_default();
        put(h.repo.clone(), &h.file, json!({ "line": h.line, "symbol": symbol, "edge": h.kind, "confidence": h.confidence, "hop": 1 }));
    }
    // This repo's files first, as the walk ranked them; then other repos'.
    dep_files.sort_by_key(|(repo, ..)| repo.is_some());
    let deps_total = dep_files.len();
    let deps_cut = deps_total > p.dependents;
    let dependents: Vec<Value> = dep_files
        .into_iter()
        .take(p.dependents)
        .map(|(repo, file, rows, n)| {
            let mut v = json!({ "file": file, "rows": rows });
            if let Some(repo) = repo {
                v["repo"] = json!(repo);
            }
            if n > ROWS_PER_FILE {
                v["rows_total"] = json!(n);
            }
            v
        })
        .collect();

    // The envelope `impact` made (repos, provenance, its unsettled entries), with the sections in place of its items.
    let mut env = imp.env;
    if let Some(o) = env.as_object_mut() {
        for k in ["items", "hint", "_resume", "cursor", "total", "truncated", "filter", "depth"] {
            o.remove(k);
        }
    }
    retain_incomplete(&mut env, |i| !matches!(i.reason, Reason::Capped | Reason::Budget | Reason::Policy | Reason::Depth));
    let diff_arg = json!(p.diff);
    env["base"] = json!(r.base);
    env["changes"] = json!(changes);
    env["tests"] = json!(test_rows);
    env["missed"] = json!(missed);
    env["dependents"] = json!(dependents);
    // Contract changes are read in a later step: until then the section says so, and the answer isn't complete.
    env["contracts"] = json!([]);
    env["unsettled"] = json!(unsettled);
    let shown = |k: &str| env[k].as_array().map_or(0, Vec::len);
    let sections = json!({
        "changes": { "complete": changes_total <= shown("changes"), "shown": shown("changes"), "total": changes_total,
                     "next": format!("kc_impact {{\"diff\": {diff_arg}}}") },
        "tests": { "complete": tests_total <= shown("tests"), "shown": shown("tests"), "total": tests_total, "advisory": ADVISORY,
                   "next": format!("kc_tests {{\"diff\": {diff_arg}, \"limit\": 30}}") },
        "missed": { "complete": missed_total <= shown("missed"), "shown": shown("missed"), "total": missed_total,
                    "next": format!("kc_impact {{\"diff\": {diff_arg}}}") },
        "dependents": { "complete": deps_whole && !deps_cut, "shown": shown("dependents"), "total": deps_total,
                        "next": format!("kc_impact {{\"diff\": {diff_arg}, \"depth\": 2}}") },
        "contracts": { "complete": false, "shown": 0, "total": 0, "next": "" },
    });
    env["sections"] = sections;
    for (key, what) in [("changes", "changed files"), ("tests", "test candidates"), ("missed", "missed partners"), ("dependents", "dependent files")] {
        let s = &env["sections"][key];
        let (shown, total) = (s["shown"].as_u64().unwrap_or(0), s["total"].as_u64().unwrap_or(0));
        if total > shown {
            let i = Incomplete::new(Reason::Capped, format!("{key}: {shown} of {total} {what} shown")).with_hint(&format!("sections.{key}.next"));
            push_incomplete(&mut env, i);
        }
    }
    if !deps_whole {
        push_incomplete(&mut env, Incomplete::new(Reason::Capped, "dependents: one hop, the walk cut or past ambiguous edges").with_hint("sections.dependents.next"));
    }
    push_incomplete(&mut env, Incomplete::new(Reason::Unsupported, "contracts: contract changes across repos aren't read yet"));
    finish(&mut env);
    let data = env.clone();
    Ok(Out { env, data })
}

/// The order a review answer's keys are written in: its sections as they are read, then what the answer says about
/// itself (see [`crate::task::reading_order`]).
const READING_ORDER: &[&str] = &["base", "changes", "tests", "missed", "dependents", "contracts", "unsettled", "complete", "incomplete", "completeness", "sections"];

/// The compact answer `text` with its top-level keys in [`READING_ORDER`], the rest after them in name order.
pub fn reading_order(text: &str) -> String {
    crate::task::order_keys(text, READING_ORDER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{repo, test_cache};

    /// Commits everything in `dir`, with a fixed author: the base a diff is read against.
    fn commit(dir: &std::path::Path) {
        for args in [&["init", "-q"][..], &["add", "-A"], &["commit", "-q", "-m", "base"]] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@x", "-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }

    /// A diff gets its changed files with their symbols, the tests reaching them, what depends on them by file, and
    /// each section saying whether it is whole and how to go on; tests are advice, and contracts aren't read yet.
    #[test]
    fn a_diff_gets_its_changes_tests_and_dependents_by_file() {
        let cache = test_cache();
        let base = cache.dir("review");
        let root = repo(
            &base,
            "shop",
            &[
                ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/Ledger.cs", "namespace Shop;\npublic static class Ledger\n{\n    public static void Post() { }\n}\n"),
                ("src/Refunds.cs", "namespace Shop;\npublic class Refunds\n{\n    public void Refund() { Ledger.Post(); }\n}\n"),
                ("tests/LedgerTests.cs", "namespace Shop.Tests;\npublic class LedgerTests\n{\n    public void Posts() { Shop.Ledger.Post(); }\n}\n"),
            ],
        );
        commit(&root);
        std::fs::write(root.join("src/Ledger.cs"), "namespace Shop;\npublic static class Ledger\n{\n    public static void Post() { System.Console.WriteLine(); }\n}\n").unwrap();
        let s = Scope { repos: vec![root.clone()], no_sync: false };
        let p = ReviewParams { diff: "working".into(), changes: 10, dependents: 10, tests: 4, missed: 4 };
        let env = review(&p, &s).unwrap().env;
        assert_eq!(env["changes"][0]["file"], "src/Ledger.cs", "{env}");
        assert_eq!(env["changes"][0]["change"], "modified", "{env}");
        assert_eq!(env["changes"][0]["symbols"][0]["symbol"], "Ledger.Post", "{env}");
        assert_eq!(env["dependents"][0]["file"], "src/Refunds.cs", "{env}");
        assert_eq!(env["dependents"][0]["rows"][0]["symbol"], "Refunds.Refund", "{env}");
        assert_eq!(env["tests"][0]["file"], "tests/LedgerTests.cs", "{env}");
        assert_eq!(env["sections"]["tests"]["advisory"], ADVISORY);
        for key in ["changes", "tests", "missed", "dependents", "contracts"] {
            assert!(env["sections"][key]["complete"].is_boolean(), "{key}: {env}");
        }
        assert_eq!(env["complete"], false, "{env}");
        // The answer in reading order: the same bytes, the changes first.
        let compact = env.to_string();
        let ordered = reading_order(&compact);
        assert_eq!(ordered.len(), compact.len());
        assert!(ordered.find("\"changes\"").unwrap() < ordered.find("\"tests\"").unwrap(), "{ordered}");
    }
}
