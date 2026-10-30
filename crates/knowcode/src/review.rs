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
//! ([`row_id`]): never its line, order or page, so a comment a CI job posted can be found again.
//!
//! A field with nothing to report is left out, as everywhere in an answer: in `sections`, a missing `next` means no call
//! continues the section (it is whole) and a missing `incomplete` means no reason cut it; in a changed file's
//! `symbols`, a missing `change` means `changed` (`removed` is written out). The dependents and the tests are walked on one symbol graph (see
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

impl ReviewParams {
    /// The limits a review answers with by default: every changed file of a diff the size of most pull requests (the
    /// review-v1 set takes introducing commits of at most 30 files), and the first few of each other section.
    pub fn new(diff: String) -> Self {
        ReviewParams { diff, changes: 30, dependents: 8, tests: 6, missed: 6 }
    }
}

/// What the tests section is: advice, never a list of the only tests to run.
pub const ADVISORY: &str = crate::task::ADVISORY;

/// Symbols listed per changed file at most; the rest are counted. The file is what a reviewer opens; three of its
/// symbols say where to look in it.
const SYMBOLS_PER_FILE: usize = 3;
/// Rows listed per dependent file at most; the rest are counted.
const ROWS_PER_FILE: usize = 3;
/// The bytes the dependents and the tests take at most, as compact JSON: past them, rows are counted rather than
/// listed. The changed files come first and are what a review most needs; the size cap of an MCP answer would cut the
/// longest list, which is often theirs, so the other sections keep within these and leave the room to them.
const DEPENDENTS_BYTES: usize = 1600;
const TESTS_BYTES: usize = 800;

/// Rule matches looked for at most: past them the rules section says it is cut.
const RULE_MATCHES: usize = 10_000;

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
    // The contracts first: reading the workspace's summaries records the repos withheld or missing, which every
    // section's answer then carries.
    let rev = Some(p.diff.as_str()).filter(|d| *d != "working");
    let contracts = ops::full_summaries(&o).and_then(|s| kc_index::contracts::http(root, &o.store, &kc_index::diff::diff(root, rev)?, &s));
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
        symbols.entry(s.file.clone()).or_default().push(symbol_row(&s.qualified, &s.kind, s.line, false));
    }
    for d in &r.removed {
        symbols.entry(d.file.clone()).or_default().push(symbol_row(&d.qualified, d.kind, d.line, true));
    }
    for (file, row) in &mut by_file {
        // A deleted file's removed definitions are at its own path; a renamed file's at the path it had.
        let mut list = symbols.remove(file.as_str()).unwrap_or_default();
        if let Some(from) = row["renamed_from"].as_str() {
            list.extend(symbols.remove(from).unwrap_or_default());
        }
        let total = list.len();
        // An added file's symbols are all new: its count says as much, and a large diff of new files would otherwise
        // spend the answer on them before the files it changed.
        let room = if row["change"] == "added" { 0 } else { SYMBOLS_PER_FILE };
        list.truncate(room);
        if !list.is_empty() {
            row["symbols"] = json!(list);
        }
        if total > room {
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
                let mut used = 0;
                for pick in t.selection.selected.iter().take(p.tests) {
                    let mut row = json!({ "id": row_id("test", &["", &pick.file]), "file": pick.file, "confidence": (pick.confidence * 100.0).round() / 100.0 });
                    if let Some(run) = run_of(&pick.file) {
                        row["run"] = json!(run);
                    }
                    used += row.to_string().len();
                    if used > TESTS_BYTES && !test_rows.is_empty() {
                        break;
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
    let mut used = 0;
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
        .take_while(|v| {
            used += v.to_string().len();
            used <= DEPENDENTS_BYTES
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
    env["base"] = json!(r.base);
    env["head"] = json!(if p.diff == "working" { "working".to_string() } else { kc_index::head_commit(root).unwrap_or_default() });
    // The envelope's `commit` is HEAD; against a revision `head` says the same, once.
    if env["commit"] == env["head"]
        && let Some(o) = env.as_object_mut()
    {
        o.remove("commit");
    }
    env["changes"] = json!(changes);
    env["tests"] = json!(test_rows);
    env["missed"] = json!(missed);
    env["dependents"] = json!(dependents);
    // The routes the diff removes or changes, each with the consumers in other repos it breaks.
    let mut contract_rows: Vec<Value> = Vec::new();
    match &contracts {
        Ok(c) => {
            for ch in &c.changes {
                let consumers: Vec<Value> = ch
                    .consumers
                    .iter()
                    .map(|u| {
                        let repo = ops::repo_name(std::path::Path::new(&u.repo));
                        json!({ "id": row_id("consumer", &[&repo, &u.file, &u.symbol, &ch.contract, &u.link]), "repo": repo, "file": u.file,
                                "line": u.line, "symbol": u.symbol, "verdict": u.verdict, "confidence": u.confidence, "link": u.link })
                    })
                    .collect();
                let mut row = json!({ "id": row_id("contract", &["", &ch.file, "", &ch.contract]), "kind": ch.kind, "contract": ch.contract,
                                      "change": ch.change, "file": ch.file, "line": ch.line, "consumers": consumers });
                if let Some(d) = &ch.detail {
                    row["detail"] = json!(d);
                }
                contract_rows.push(row);
            }
            for u in &c.unsettled {
                said(&mut unsettled, format!("contracts: {u}"));
            }
        }
        Err(e) => said(&mut unsettled, format!("contracts not read ({e:#})")),
    }
    let contracts_whole = contracts.as_ref().is_ok_and(|c| c.unsettled.is_empty());
    let contracts_total = contract_rows.len();
    env["contracts"] = json!(contract_rows);
    // The repo's own rules (`[rules] paths`) as a monitor against the base: only the matches the change brings. A rule
    // file refused (a `fix`, a blocked path) is said in `unsettled`; the review goes on without it.
    let (mut rule_rows, mut rules_whole, mut rules_why) = (Vec::new(), true, Vec::new());
    match r.base.as_deref().map(|b| crate::rules::for_review(root, b, scope.no_sync, RULE_MATCHES)) {
        Some(Ok(found)) => {
            for m in &found.matches {
                rule_rows.push(json!({ "id": rule_id("", &m.file, &m.rule, &m.matched, m.n), "rule": m.rule, "severity": m.severity,
                                       "message": m.message, "file": m.file, "line": m.line, "end": m.end }));
            }
            rules_whole = found.complete;
            rules_why = found.incomplete.clone();
        }
        Some(Err(e)) => {
            said(&mut unsettled, format!("rules not run ({e:#})"));
            rules_whole = false;
            rules_why.push("failed");
        }
        None => {}
    }
    let rules_total = rule_rows.len();
    env["rules"] = json!(rule_rows);
    env["walkthrough"] = Value::Null;
    env["unsettled"] = json!(unsettled);
    let shown = |k: &str| env[k].as_array().map_or(0, Vec::len);
    // Each section: whether it is whole, the reasons that cut it, and the call continuing it.
    let mut sections = json!({
        "changes": section_json(shown("changes"), changes_total, true, format!("kc_impact {{\"diff\": {diff_arg}}}"), &[]),
        "tests": section_json(shown("tests"), tests_total, true, format!("kc_tests {{\"diff\": {diff_arg}, \"limit\": 30}}"), &[]),
        "missed": section_json(shown("missed"), missed_total, true, format!("kc_impact {{\"diff\": {diff_arg}}}"), &[]),
        "dependents": section_json(shown("dependents"), deps_total, deps_whole, format!("kc_impact {{\"diff\": {diff_arg}, \"depth\": 2}}"),
                              if deps_whole { &[] } else { &["depth"] }),
        // HTTP routes are read; bus messages and table columns aren't yet, so the section is never whole.
        "contracts": section_json(contracts_total, contracts_total, false, String::new(), if contracts_whole { &["unsupported"] } else { &["unsupported", "failed"] }),
        "rules": section_json(rules_total, rules_total, rules_whole, String::new(), &rules_why),
    });
    sections["tests"]["advisory"] = json!(ADVISORY);
    env["sections"] = sections;
    for (key, what) in [("changes", "changed files"), ("tests", "test candidates"), ("missed", "missed partners"), ("dependents", "dependent files")] {
        let s = &env["sections"][key];
        let (shown, total) = (s["shown"].as_u64().unwrap_or(0), s["total"].as_u64().unwrap_or(0));
        if total > shown {
            let i = Incomplete::new(Reason::Capped, format!("{key}: {shown} of {total} {what}")).with_hint(format!("sections.{key}.next"));
            push_incomplete(&mut env, i);
        }
    }
    if !deps_whole {
        push_incomplete(&mut env, Incomplete::new(Reason::Depth, "dependents: one hop").with_hint("sections.dependents.next"));
    }
    push_incomplete(&mut env, Incomplete::new(Reason::Unsupported, "contracts: bus messages and table columns not read yet"));
    finish(&mut env);
    let data = env.clone();
    Ok(Out { env, data })
}

/// A section's entry in `sections`: whether it is whole, how many rows it shows of how many, and, for a cut one, the
/// call continuing it and the reasons that cut it. A field with nothing to say is left out, as everywhere in an answer:
/// a missing `next` means there is no call to continue (the section is whole), a missing `incomplete` means no reason
/// cut it.
fn section_json(shown: usize, total: usize, whole: bool, next: String, reasons: &[&str]) -> Value {
    let mut why: Vec<&str> = reasons.to_vec();
    if total > shown && !why.contains(&"capped") {
        why.insert(0, "capped");
    }
    let complete = whole && total <= shown;
    let mut v = json!({ "complete": complete, "shown": shown, "total": total });
    if !complete && !next.is_empty() {
        v["next"] = json!(next);
    }
    if !why.is_empty() {
        v["incomplete"] = json!(why);
    }
    v
}

/// A changed file's symbol: its name, kind and line, and `change: removed` for a definition the diff removes (its line
/// at the base). A missing `change` means `changed`: the symbol is in the diff's changed lines.
fn symbol_row(symbol: &str, kind: &str, line: u32, removed: bool) -> Value {
    let mut v = json!({ "symbol": symbol, "kind": kind, "line": line });
    if removed {
        v["change"] = json!("removed");
    }
    v
}

/// The order a review answer's keys are written in: its sections as they are read, then what the answer says about
/// itself (see [`crate::task::reading_order`]).
const READING_ORDER: &[&str] = &[
    "version", "repo", "base", "head", "changes", "tests", "missed", "dependents", "contracts", "rules", "walkthrough", "unsettled", "complete",
    "incomplete", "completeness", "sections",
];

/// The compact answer `text` with its top-level keys in [`READING_ORDER`], the rest after them in name order.
pub fn reading_order(text: &str) -> String {
    crate::task::order_keys(&budgeted(text), READING_ORDER)
}

/// The answer `text` with each section's count and completeness made true of the list the answer holds: the MCP
/// server's size cap may have cut a list after the sections were written, and a section must never say whole of a list
/// cut short (the cut is the `budget` entry the cap added). Anything but a JSON object is returned as it is.
fn budgeted(text: &str) -> String {
    let Ok(mut env) = serde_json::from_str::<Value>(text) else { return text.to_string() };
    let mut changed = false;
    for key in ["changes", "tests", "missed", "dependents", "contracts", "rules"] {
        let held = env[key].as_array().map_or(0, Vec::len) as u64;
        let s = &mut env["sections"][key];
        if s["shown"].as_u64().is_some_and(|shown| held < shown) {
            s["shown"] = json!(held);
            s["complete"] = json!(false);
            match s["incomplete"].as_array_mut() {
                Some(why) if why.iter().any(|w| w == "budget") => {}
                Some(why) => why.push(json!("budget")),
                None => s["incomplete"] = json!(["budget"]),
            }
            changed = true;
        }
    }
    if changed { env.to_string() } else { text.to_string() }
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

    /// What a field left out means is pinned byte for byte: a whole, empty section is its counts alone; a cut one has
    /// its call and reasons; a changed symbol has no `change`, a removed one says so.
    #[test]
    fn fields_with_nothing_to_report_are_left_out() {
        assert_eq!(section_json(0, 0, true, "kc_tests {}".into(), &[]).to_string(), r#"{"complete":true,"shown":0,"total":0}"#);
        assert_eq!(
            section_json(2, 5, true, "kc_tests {}".into(), &[]).to_string(),
            r#"{"complete":false,"incomplete":["capped"],"next":"kc_tests {}","shown":2,"total":5}"#
        );
        assert_eq!(symbol_row("Ledger.Post", "method", 4, false).to_string(), r#"{"kind":"method","line":4,"symbol":"Ledger.Post"}"#);
        assert_eq!(symbol_row("Ledger.Void", "method", 9, true).to_string(), r#"{"change":"removed","kind":"method","line":9,"symbol":"Ledger.Void"}"#);
    }

    /// A list the size cap cut after the sections were written makes its section say so.
    #[test]
    fn a_section_the_size_cap_cut_says_it_is_cut() {
        let env = json!({"changes": [{"file": "a"}], "sections": {"changes": {"complete": true, "shown": 3, "total": 3}}});
        let out: Value = serde_json::from_str(&reading_order(&env.to_string())).unwrap();
        assert_eq!(out["sections"]["changes"], json!({"complete": false, "shown": 1, "total": 3, "incomplete": ["budget"]}));
    }

    /// A route the diff removes, with the request in another repo it served and nothing serves now: broken.
    #[test]
    fn a_removed_route_lists_the_request_it_breaks_in_another_repo() {
        let cache = test_cache();
        let base = cache.dir("review-contract");
        let api = repo(
            &base,
            "shop-api",
            &[
                ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"),
                (
                    "src/ItemsController.cs",
                    "using Microsoft.AspNetCore.Mvc;\nnamespace Shop;\n[ApiController]\n[Route(\"api/items\")]\npublic class ItemsController : ControllerBase\n{\n    [HttpGet(\"{id}\")]\n    public IActionResult Get(int id) => Ok();\n    [HttpPost]\n    public IActionResult Add() => Ok();\n}\n",
                ),
            ],
        );
        let _web = repo(
            &base,
            "shop-web",
            &[
                ("package.json", "{ \"name\": \"shop-web\" }\n"),
                ("src/api.ts", "export function load(id: number) {\n  return fetch(`/api/items/${id}`);\n}\n"),
            ],
        );
        commit(&api);
        std::fs::write(
            api.join("src/ItemsController.cs"),
            "using Microsoft.AspNetCore.Mvc;\nnamespace Shop;\n[ApiController]\n[Route(\"api/items\")]\npublic class ItemsController : ControllerBase\n{\n    [HttpPost]\n    public IActionResult Add() => Ok();\n}\n",
        )
        .unwrap();
        let env = review(&ReviewParams::new("working".into()), &Scope { repos: vec![api.clone()], no_sync: false }).unwrap().env;
        let c = &env["contracts"];
        assert_eq!(c.as_array().map(Vec::len), Some(1), "{env}");
        assert_eq!(c[0]["contract"], "GET api/items/{id}", "{env}");
        assert_eq!(c[0]["change"], "removed", "{env}");
        assert_eq!(c[0]["consumers"][0]["repo"], "shop-web", "{env}");
        assert_eq!(c[0]["consumers"][0]["file"], "src/api.ts", "{env}");
        assert_eq!(c[0]["consumers"][0]["verdict"], "broken", "{env}");
        // The route still served isn't a change.
        assert!(!env.to_string().contains("POST api/items\""), "{env}");
    }

    /// A route whose path changes breaks the request for its old path, unless another route the repo serves now still
    /// answers it; under a base class's prefix (`{^Action}`), the old path is placed by a route the class still serves.
    #[test]
    fn a_changed_route_breaks_only_requests_nothing_serves_now() {
        let cache = test_cache();
        let base = cache.dir("review-contract-changed");
        let controller = |get: &str, extra: &str| {
            format!(
                "using Microsoft.AspNetCore.Mvc;\nnamespace Shop;\n[Route(\"api/[controller]\")]\npublic class ApiBase : ControllerBase {{ }}\npublic class OrdersController : ApiBase\n{{\n    [HttpGet(\"{get}\")]\n    public IActionResult Get(int id) => Ok();\n    [HttpGet(\"open\")]\n    public IActionResult Open() => Ok();\n{extra}}}\n"
            )
        };
        let api = repo(
            &base,
            "shop-api",
            &[("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"), ("src/OrdersController.cs", &controller("{id}", ""))],
        );
        let _web = repo(
            &base,
            "shop-web",
            &[
                ("package.json", "{ \"name\": \"shop-web\" }\n"),
                ("src/orders.ts", "export function order(id: number) {\n  return fetch(`/api/Orders/${id}`);\n}\nexport function open() {\n  return fetch(`/api/Orders/open`);\n}\n"),
            ],
        );
        commit(&api);
        // `{id}` moves to `byId/{id}`: the request for the old path breaks.
        std::fs::write(api.join("src/OrdersController.cs"), controller("byId/{id}", "")).unwrap();
        let env = review(&ReviewParams::new("working".into()), &Scope { repos: vec![api.clone()], no_sync: false }).unwrap().env;
        let c = &env["contracts"];
        assert_eq!(c.as_array().map(Vec::len), Some(1), "{env}");
        assert_eq!(c[0]["contract"], "GET api/Orders/{id}", "{env}");
        assert_eq!(c[0]["change"], "changed", "{env}");
        assert_eq!(c[0]["consumers"][0]["symbol"], "order", "{env}");
        // Moved, with another action now serving the old path: nothing is broken.
        std::fs::write(
            api.join("src/OrdersController.cs"),
            controller("byId/{id}", "    [HttpGet(\"{id}\")]\n    public IActionResult Legacy(int id) => Ok();\n"),
        )
        .unwrap();
        let env = review(&ReviewParams::new("working".into()), &Scope { repos: vec![api.clone()], no_sync: false }).unwrap().env;
        assert!(env["contracts"].as_array().is_some_and(|c| c.iter().all(|r| r["consumers"].as_array().is_some_and(Vec::is_empty))), "{env}");
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
            assert!(env["sections"][key]["complete"].is_boolean() && env["sections"][key]["total"].is_u64(), "{key}: {env}");
        }
        assert_eq!(env["complete"], false, "{env}");
        // The answer in reading order: the same bytes, the changes first.
        let compact = env.to_string();
        let ordered = reading_order(&compact);
        assert_eq!(ordered.len(), compact.len());
        assert!(ordered.find("\"changes\"").unwrap() < ordered.find("\"tests\"").unwrap(), "{ordered}");
    }
}
