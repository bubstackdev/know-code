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
//! Each section says whether it is whole, which reasons cut it, and the call that continues it (`sections`); a cut
//! section is an `incomplete` entry too. `rules` (W4's rule and monitor matches) and `walkthrough` are reserved: empty
//! and null until they are filled.
//!
//! The structure is version 1 (`version`), frozen for the renderers built on it (markdown, SARIF, the CI action):
//! every row has a stable `id` and a `file`, with 1-based lines where there are any, and `repo` only when it isn't the
//! reviewed repo (a contract's consumer always has it). An `id` is a hash of the row's kind and fields, per kind
//! ([`row_id`]): never its line, order or page, so a comment a CI job posted can be found again. The dependents and the tests are walked on one symbol graph (see
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
/// The answer's structure, for the renderers built on it: a change to it is a new version.
pub const VERSION: u32 = 1;

/// A row's stable id: FNV-1a 64 of its kind and fields joined by `\0`, as 16 hex digits. No line, order or page
/// is part of it, so it doesn't move when a line is added above the row or the rows are paged. Per kind (an empty field
/// stays empty; `repo` is empty for the reviewed repo):
///
/// - `change`, `test`, `missed`: kind, repo, file;
/// - `dependent`: kind, repo, file;
/// - `contract`: kind, repo, file, "", the contract (`GET /api/items/{id}`);
/// - `consumer`: kind, repo, file, symbol, the contract, the link (two consumers in one symbol differ by it);
/// - `rule`: kind, repo, file, the rule's id, the matched text, and the match's index among the same rule's matches of
///   the same text in that file: two matches of one rule in one file differ, and neither moves when the file changes
///   elsewhere.
pub fn row_id(kind: &str, fields: &[&str]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, part) in std::iter::once(kind).chain(fields.iter().copied()).enumerate() {
        if i > 0 {
            h = h.wrapping_mul(0x100_0000_01b3); // the `\0` between fields
        }
        for b in part.bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// [`row_id`] of a rule match: `n` is its index among the matches of rule `rule` with the text `matched` in `file`.
pub fn rule_id(repo: &str, file: &str, rule: &str, matched: &str, n: usize) -> String {
    row_id("rule", &[repo, file, rule, matched, &n.to_string()])
}

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
        let mut row = json!({ "id": row_id("change", &["", &c.file]), "file": c.file, "change": change });
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

    // The tests to run, each with the command running its target when the repo's manifests declare one (the target
    // whose directory holds it, the innermost first): the whole target, the narrowest command known.
    let targets = kc_index::test_targets::list(root, &o.store).unwrap_or_default();
    let run_of = |file: &str| {
        targets
            .iter()
            .filter(|t| t.dir.is_empty() || file.starts_with(&format!("{}/", t.dir.trim_end_matches('/'))))
            .max_by_key(|t| t.dir.len())
            .map(|t| t.command.clone())
    };
    let (mut test_rows, mut tests_total) = (Vec::new(), 0);
    match &tests {
        Ok(out) => {
            for t in out.data.iter() {
                tests_total += t.selection.selected.len() + t.selection.left_out.len();
                for pick in t.selection.selected.iter().take(p.tests) {
                    let mut row = json!({ "id": row_id("test", &["", &pick.file]), "file": pick.file, "confidence": (pick.confidence * 100.0).round() / 100.0 });
                    if let Some(run) = run_of(&pick.file) {
                        row["run"] = json!(run);
                    }
                    test_rows.push(row);
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
            missed.push(json!({ "id": row_id("missed", &["", &m.file]), "file": m.file, "with": m.with, "support": m.support, "commits": m.commits }));
        }
    }
    for c in r.coupled.iter().filter(|c| !changed.contains(&c.file)) {
        if missed_seen.insert(&c.file) {
            missed.push(json!({ "id": row_id("missed", &["", &c.file]), "file": c.file, "with": c.with, "support": c.support, "commits": c.commits }));
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
        let mut row = json!({ "line": a.line, "symbol": a.qualified, "via": a.via, "edge": a.edge, "confidence": a.confidence, "hop": a.hop, "class": a.class });
        if !a.also.is_empty() {
            row["also"] = json!(a.also);
        }
        put(a.repo.clone(), &a.file, row);
    }
    for h in r.cross_repo.iter().filter(|h| !kc_store::is_test_path(&h.file)) {
        let symbol = h.from.as_ref().map(|f| f.container.as_ref().map_or(f.name.clone(), |c| format!("{c}.{}", f.name))).unwrap_or_default();
        put(h.repo.clone(), &h.file, json!({ "line": h.line, "symbol": symbol, "edge": h.kind, "confidence": h.confidence, "hop": 1, "class": "direct" }));
    }
    // This repo's files first, as the walk ranked them; then other repos'.
    dep_files.sort_by_key(|(repo, ..)| repo.is_some());
    let deps_total = dep_files.len();
    let deps_cut = deps_total > p.dependents;
    let dependents: Vec<Value> = dep_files
        .into_iter()
        .take(p.dependents)
        .map(|(repo, file, rows, n)| {
            let mut v = json!({ "id": row_id("dependent", &[repo.as_deref().unwrap_or(""), &file]), "file": file, "rows": rows });
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
    env["version"] = json!(VERSION);
    env["repo"] = json!(ops::repo_name(root));
    env["base"] = json!(r.base);
    env["head"] = json!(if p.diff == "working" { "working".to_string() } else { kc_index::head_commit(root).unwrap_or_default() });
    env["changes"] = json!(changes);
    env["tests"] = json!(test_rows);
    env["missed"] = json!(missed);
    env["dependents"] = json!(dependents);
    // Contract changes are read in a later step: until then the section says so, and the answer isn't complete.
    env["contracts"] = json!([]);
    env["rules"] = json!([]);
    env["walkthrough"] = Value::Null;
    env["unsettled"] = json!(unsettled);
    let shown = |k: &str| env[k].as_array().map_or(0, Vec::len);
    // Each section: whether it is whole, the reasons that cut it, and the call continuing it.
    let section = |shown: usize, total: usize, whole: bool, next: String, reasons: &[&str]| {
        let mut why: Vec<&str> = reasons.to_vec();
        if total > shown && !why.contains(&"capped") {
            why.insert(0, "capped");
        }
        json!({ "complete": whole && total <= shown, "shown": shown, "total": total, "next": next, "incomplete": why })
    };
    let mut sections = json!({
        "changes": section(shown("changes"), changes_total, true, format!("kc_impact {{\"diff\": {diff_arg}}}"), &[]),
        "tests": section(shown("tests"), tests_total, true, format!("kc_tests {{\"diff\": {diff_arg}, \"limit\": 30}}"), &[]),
        "missed": section(shown("missed"), missed_total, true, format!("kc_impact {{\"diff\": {diff_arg}}}"), &[]),
        "dependents": section(shown("dependents"), deps_total, deps_whole && !deps_cut, format!("kc_impact {{\"diff\": {diff_arg}, \"depth\": 2}}"),
                              if deps_whole { &[] } else { &["depth"] }),
        "contracts": section(0, 0, false, String::new(), &["unsupported"]),
        "rules": section(0, 0, true, String::new(), &[]),
    });
    sections["tests"]["advisory"] = json!(ADVISORY);
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
        push_incomplete(&mut env, Incomplete::new(Reason::Depth, "dependents: one hop, the walk cut or stopped at ambiguous edges").with_hint("sections.dependents.next"));
    }
    push_incomplete(&mut env, Incomplete::new(Reason::Unsupported, "contracts: contract changes across repos aren't read yet"));
    finish(&mut env);
    let data = env.clone();
    Ok(Out { env, data })
}

/// The order a review answer's keys are written in: its sections as they are read, then what the answer says about
/// itself (see [`crate::task::reading_order`]).
const READING_ORDER: &[&str] = &[
    "version", "repo", "base", "head", "changes", "tests", "missed", "dependents", "contracts", "rules", "walkthrough", "unsettled", "complete",
    "incomplete", "completeness", "sections",
];

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

    /// Ids are the hash of their kind's fields: the same fields give the same id, and two matches of one rule in one
    /// file, or two consumers in one symbol reached by different links, differ.
    #[test]
    fn row_ids_differ_where_rows_do() {
        assert_eq!(row_id("change", &["", "src/A.cs"]), row_id("change", &["", "src/A.cs"]));
        assert_ne!(row_id("change", &["", "src/A.cs"]), row_id("test", &["", "src/A.cs"]));
        // Fields are kept apart: "ab" + "c" is not "a" + "bc".
        assert_ne!(row_id("dependent", &["ab", "c"]), row_id("dependent", &["a", "bc"]));
        assert_ne!(rule_id("", "src/A.cs", "no-sleep", "Thread.Sleep(1)", 0), rule_id("", "src/A.cs", "no-sleep", "Thread.Sleep(1)", 1));
        let consumer = |link: &str| row_id("consumer", &["web", "src/api.ts", "loadItems", "GET /api/items/{id}", link]);
        assert_ne!(consumer("http_route"), consumer("unique_name"));
        assert_eq!(row_id("change", &["", "src/A.cs"]).len(), 16);
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
                ("tests/Shop.Tests.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup><PackageReference Include=\"xunit\" Version=\"2.9.0\" /></ItemGroup>\n</Project>\n"),
                ("tests/LedgerTests.cs", "namespace Shop.Tests;\npublic class LedgerTests\n{\n    public void Posts() { Shop.Ledger.Post(); }\n}\n"),
            ],
        );
        commit(&root);
        std::fs::write(root.join("src/Ledger.cs"), "namespace Shop;\npublic static class Ledger\n{\n    public static void Post() { System.Console.WriteLine(); }\n}\n").unwrap();
        let s = Scope { repos: vec![root.clone()], no_sync: false };
        let p = ReviewParams { diff: "working".into(), changes: 10, dependents: 10, tests: 4, missed: 4 };
        let env = review(&p, &s).unwrap().env;
        assert_eq!(env["version"], 1, "{env}");
        assert_eq!(env["changes"][0]["file"], "src/Ledger.cs", "{env}");
        // Ids: stable, and the same for the same row whatever its line.
        assert_eq!(env["changes"][0]["id"], row_id("change", &["", "src/Ledger.cs"]), "{env}");
        assert_eq!(env["dependents"][0]["rows"][0]["class"], "direct", "{env}");
        assert!(env["rules"].as_array().is_some_and(Vec::is_empty) && env["walkthrough"].is_null(), "{env}");
        assert_eq!(env["sections"]["contracts"]["incomplete"], json!(["unsupported"]), "{env}");
        assert_eq!(env["changes"][0]["change"], "modified", "{env}");
        assert_eq!(env["changes"][0]["symbols"][0]["symbol"], "Ledger.Post", "{env}");
        assert_eq!(env["dependents"][0]["file"], "src/Refunds.cs", "{env}");
        assert_eq!(env["dependents"][0]["rows"][0]["symbol"], "Refunds.Refund", "{env}");
        assert_eq!(env["tests"][0]["file"], "tests/LedgerTests.cs", "{env}");
        // The command running its target, from the test project declaring it.
        assert!(env["tests"][0]["run"].as_str().is_some_and(|r| r.starts_with("dotnet test")), "{env}");
        assert_eq!(env["sections"]["tests"]["advisory"], ADVISORY);
        for key in ["changes", "tests", "missed", "dependents", "contracts", "rules"] {
            assert!(env["sections"][key]["complete"].is_boolean() && env["sections"][key]["incomplete"].is_array(), "{key}: {env}");
        }
        assert_eq!(env["complete"], false, "{env}");
        // The answer in reading order: the same bytes, the changes first.
        let compact = env.to_string();
        let ordered = reading_order(&compact);
        assert_eq!(ordered.len(), compact.len());
        assert!(ordered.find("\"changes\"").unwrap() < ordered.find("\"tests\"").unwrap(), "{ordered}");
    }
}
