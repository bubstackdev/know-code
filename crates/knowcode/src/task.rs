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
    // `locate`, `impact` and `tests` open the same repos: each is checked against its working tree, and for changes
    // since its summary was written, once.
    let _synced = kc_store::limits::current().remember_syncs();
    let _memo = kc_index::memo_changes();
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
    // Each repo the walked files are in, opened once, with one symbol graph: the dependents walk of each file and the
    // tests walk read every edge through it, each under its own evidence policy, so an edge is read once.
    let mut roots: Vec<PathBuf> = Vec::new();
    for (root, _) in &walked {
        if !roots.contains(root) {
            roots.push(root.clone());
        }
    }
    // A repo that doesn't open is left to `impact` and `tests` themselves, which say why as they always have.
    let opened: Vec<Option<ops::Opened>> = roots.iter().map(|r| ops::open_member(std::slice::from_ref(r), r, scope.no_sync).ok().flatten()).collect();
    let mut graphs: Vec<Option<crate::walk::Graph>> = opened.iter().map(|o| o.as_ref().map(|o| crate::walk::Graph::new(o, &[], scope.no_sync, usize::MAX))).collect();
    let at = |root: &PathBuf| roots.iter().position(|r| r == root).unwrap_or(0);
    for (root, h) in &walked {
        let one = Scope { repos: vec![root.clone()], no_sync: scope.no_sync };
        let ip = ops::ImpactParams { file: Some(h.file.clone()), depth: 1, limit: 30, ..Default::default() };
        let i = at(root);
        let got = match (&opened[i], graphs[i].as_mut()) {
            (Some(o), Some(g)) => ops::impact_on(&ip, &one, o, g),
            _ => ops::impact(&ip, &one),
        };
        let out = match got {
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
        let tp = ops::TestsParams { targets: targets.clone(), diff: None, limit: p.tests, evidence: String::new() };
        let i = at(root);
        let got = match (&opened[i], graphs[i].as_mut()) {
            (Some(o), Some(g)) => ops::tests_on(&tp, o, g),
            _ => ops::tests(&tp, &Scope { repos: vec![root.clone()], no_sync: scope.no_sync }),
        };
        match got {
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
    drop(graphs);
    drop(opened);

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

/// The order a task answer's keys are written in: its sections as they are read (the files, the tests to run, what
/// depends on them, what is unsettled), then what the answer says about itself. A JSON object's keys are otherwise
/// written in name order, which put `dependents` and `sections` before `tests`, past the first few thousand bytes a
/// client reads first.
const READING_ORDER: &[&str] = &["goal", "items", "tests", "dependents", "unsettled", "complete", "incomplete", "completeness", "sections"];

/// The compact answer `text` with its top-level keys in [`READING_ORDER`], the rest after them in name order: the same
/// bytes, ordered. Anything but a JSON object is returned as it is.
pub fn reading_order(text: &str) -> String {
    order_keys(text, READING_ORDER)
}

/// The compact answer `text` with its top-level keys in `order`, the rest after them in name order: the same bytes,
/// ordered. Anything but a JSON object is returned as it is.
pub fn order_keys(text: &str, order: &[&str]) -> String {
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(text) else { return text.to_string() };
    let mut parts: Vec<String> = Vec::with_capacity(obj.len());
    let mut put = |k: &str, v: Value| parts.push(format!("{}:{v}", Value::from(k)));
    for k in order {
        if let Some(v) = obj.remove(*k) {
            put(k, v);
        }
    }
    for (k, v) in obj {
        put(&k, v);
    }
    format!("{{{}}}", parts.join(","))
}

/// The answer as text: each section under its heading, then what isn't whole.
pub fn render(env: &Value) {
    let rows = |key: &str| env[key].as_array().cloned().unwrap_or_default();
    for (title, key) in [("files", "items"), ("dependents", "dependents"), ("tests (advisory)", "tests"), ("unsettled", "unsettled")] {
        let list = rows(key);
        if list.is_empty() {
            continue;
        }
        println!("{title}:");
        for r in list {
            println!("  {}", r.as_str().unwrap_or_default());
        }
    }
    for i in incomplete_of(env) {
        println!("{}", i.line());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{repo, test_cache};

    /// A goal in words gets the files that match it, what depends on them, the tests reaching them and the words
    /// nothing matched, each section saying whether it is whole and how to go on; tests are advice, never the only
    /// ones to run.
    #[test]
    fn a_goal_gets_its_files_dependents_tests_and_what_is_unsettled() {
        let cache = test_cache();
        let base = cache.dir("task");
        let root = repo(
            &base,
            "shop",
            &[
                ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/Refunds.cs", "namespace Shop;\npublic class Refunds\n{\n    public void RefundPartialCapture() { Ledger.Post(); }\n}\n"),
                ("src/Ledger.cs", "namespace Shop;\npublic static class Ledger\n{\n    public static void Post() { }\n}\n"),
                ("src/Checkout.cs", "namespace Shop;\npublic class Checkout\n{\n    public void Run() { new Refunds().RefundPartialCapture(); }\n}\n"),
                ("tests/RefundsTests.cs", "namespace Shop.Tests;\npublic class RefundsTests\n{\n    public void Refunds() { new Shop.Refunds().RefundPartialCapture(); }\n}\n"),
            ],
        );
        let s = Scope { repos: vec![root], no_sync: false };
        let p = TaskParams { goal: "refund partial capture zebra".into(), files: 1, dependents: 6, tests: 4 };
        let env = task(&p, &s).unwrap().env;
        let items: Vec<&str> = env["items"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(items[0].starts_with("src/Refunds.cs | Refunds.RefundPartialCapture (method :4)") && items[0].contains(" | 4: public void RefundPartialCapture()"), "{env}");
        // Past the files listed, the rest is a call away.
        assert_eq!(env["sections"]["items"]["complete"], false, "{env}");
        let deps: Vec<&str> = env["dependents"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(deps.iter().any(|d| d.starts_with("src/Checkout.cs:4 Checkout.Run → Refunds.RefundPartialCapture")), "{env}");
        assert!(env["tests"].as_array().unwrap().iter().any(|t| t.as_str().is_some_and(|t| t.starts_with("tests/RefundsTests.cs ("))), "{env}");
        assert_eq!(env["sections"]["tests"]["advisory"], ADVISORY);
        assert!(env["unsettled"].as_array().unwrap().iter().any(|u| u.as_str().is_some_and(|u| u.contains("zebra"))), "{env}");
        // Read in order: the files, then the tests, then what depends on them; the same bytes as the object.
        let compact = env.to_string();
        let ordered = reading_order(&compact);
        assert_eq!(ordered.len(), compact.len());
        assert_eq!(serde_json::from_str::<Value>(&ordered).unwrap(), env);
        let at = |k: &str| ordered.find(&format!("\"{k}\":")).unwrap();
        assert!(at("items") < at("tests") && at("tests") < at("dependents") && at("dependents") < at("sections"), "{ordered}");
        // Each section says whether it is whole and how to continue.
        for key in ["items", "dependents", "tests"] {
            assert!(env["sections"][key]["complete"].is_boolean() && env["sections"][key]["next"].is_string(), "{key}: {env}");
        }
    }

    /// One symbol graph shared by the dependents walk (one hop, the default evidence policy) and the tests walk (five
    /// hops, `all`), in either order, gives each the answer a graph of its own does: the edges cached by one walk carry
    /// nothing of its gate into the other. The policies `new Disk()` names either of two same-named types both imported, and `all` walks past that ambiguous edge
    /// to the test while the default doesn't.
    #[test]
    fn a_shared_graph_gives_each_walk_what_a_graph_of_its_own_does() {
        let cache = test_cache();
        let base = cache.dir("task-shared");
        let root = repo(
            &base,
            "store",
            &[
                ("src/Store.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/Disk.cs", "namespace Store.Local;\npublic class Disk\n{\n    public void Save() { }\n}\n"),
                ("src/Cloud.cs", "namespace Store.Remote;\npublic class Disk\n{\n    public void Save() { }\n}\n"),
                ("src/Saver.cs", "using Store.Local;\nusing Store.Remote;\nnamespace Store;\npublic class Saver\n{\n    public void Run() { new Disk().Save(); }\n}\n"),
                ("tests/SaverTests.cs", "namespace Store.Tests;\npublic class SaverTests\n{\n    public void Runs() { new Store.Saver().Run(); }\n}\n"),
            ],
        );
        let s = Scope { repos: vec![root.clone()], no_sync: false };
        let ip = ops::ImpactParams { file: Some("src/Disk.cs".into()), depth: 1, limit: 30, ..Default::default() };
        let tp = ops::TestsParams { targets: vec!["src/Disk.cs".into()], diff: None, limit: 4, evidence: String::new() };
        // The answers less the time their freshness check took.
        let text = |mut env: Value| {
            env.as_object_mut().unwrap().remove("freshness");
            env.to_string()
        };
        let impact = text(ops::impact(&ip, &s).unwrap().env);
        let tests = text(ops::tests(&tp, &s).unwrap().env);
        assert!(tests.contains("tests/SaverTests.cs"), "{tests}");
        let default = text(ops::tests(&ops::TestsParams { targets: tp.targets.clone(), diff: None, limit: 4, evidence: "default".into() }, &s).unwrap().env);
        assert!(!default.contains("\"tests/SaverTests.cs\""), "{default}");
        for tests_first in [false, true] {
            let o = ops::open_member(std::slice::from_ref(&root), &root, false).unwrap().unwrap();
            let mut g = crate::walk::Graph::new(&o, &[], false, usize::MAX);
            let (mut got_impact, mut got_tests) = (String::new(), String::new());
            for step in if tests_first { [true, false] } else { [false, true] } {
                if step {
                    got_tests = text(ops::tests_on(&tp, &o, &mut g).unwrap().env);
                } else {
                    got_impact = text(ops::impact_on(&ip, &s, &o, &mut g).unwrap().env);
                }
            }
            assert_eq!(got_impact, impact, "impact, tests first: {tests_first}");
            assert_eq!(got_tests, tests, "tests, tests first: {tests_first}");
        }
    }

    /// A walk past the library-crossing budget and a walk within it, on one shared graph in either order, each give
    /// the answer a graph of its own does: the budget is a walk's own, and what the graph caches (the references read
    /// across the boundary) doesn't depend on it. A chain of five library methods, each called from an app depending on
    /// the library: one hop looks up fewer crossings than the budget, five hops more.
    #[test]
    fn the_library_crossing_budget_is_each_walks_own_on_a_shared_graph() {
        let cache = test_cache();
        let base = cache.dir("task-shared-budget");
        let lib = repo(
            &base,
            "ledger",
            &[
                ("src/Ledger.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><PackageId>Ledger</PackageId></PropertyGroup>\n</Project>\n"),
                ("src/A.cs", "namespace Ledger;\npublic static class A\n{\n    public static void Post() { }\n}\n"),
                ("src/B.cs", "namespace Ledger;\npublic static class B\n{\n    public static void Post() { A.Post(); }\n}\n"),
                ("src/C.cs", "namespace Ledger;\npublic static class C\n{\n    public static void Post() { B.Post(); }\n}\n"),
                ("src/D.cs", "namespace Ledger;\npublic static class D\n{\n    public static void Post() { C.Post(); }\n}\n"),
                ("src/E.cs", "namespace Ledger;\npublic static class E\n{\n    public static void Post() { D.Post(); }\n}\n"),
                ("tests/ETests.cs", "namespace Ledger.Tests;\npublic class ETests\n{\n    public void Posts() { Ledger.E.Post(); }\n}\n"),
            ],
        );
        let _app = repo(
            &base,
            "ledger-app",
            &[
                ("src/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup><PackageReference Include=\"Ledger\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                ("src/Books.cs", "using Ledger;\nnamespace App;\npublic class Books\n{\n    public void Close() { A.Post(); B.Post(); C.Post(); D.Post(); E.Post(); }\n}\n"),
            ],
        );
        let s = Scope { repos: vec![lib.clone()], no_sync: false };
        let near = ops::ImpactParams { file: Some("src/A.cs".into()), depth: 1, limit: 30, ..Default::default() };
        let far = ops::ImpactParams { file: Some("src/A.cs".into()), depth: 5, limit: 30, ..Default::default() };
        let tp = ops::TestsParams { targets: vec!["src/A.cs".into()], diff: None, limit: 4, evidence: String::new() };
        let text = |mut env: Value| {
            env.as_object_mut().unwrap().remove("freshness");
            env.to_string()
        };
        let fresh = [text(ops::impact(&near, &s).unwrap().env), text(ops::impact(&far, &s).unwrap().env), text(ops::tests(&tp, &s).unwrap().env)];
        // The budget cuts the five-hop walk and not the one-hop walk: the case the test is for.
        let cut = |t: &str| t.contains("uses across a library boundary looked up for");
        assert!(!cut(&fresh[0]) && cut(&fresh[1]), "{}\n{}", fresh[0], fresh[1]);
        for order in [[0, 1, 2], [1, 0, 2], [2, 1, 0], [2, 0, 1]] {
            let o = ops::open_member(std::slice::from_ref(&lib), &lib, false).unwrap().unwrap();
            let mut g = crate::walk::Graph::new(&o, &[], false, usize::MAX);
            let mut got = [String::new(), String::new(), String::new()];
            for i in order {
                got[i] = match i {
                    0 => text(ops::impact_on(&near, &s, &o, &mut g).unwrap().env),
                    1 => text(ops::impact_on(&far, &s, &o, &mut g).unwrap().env),
                    _ => text(ops::tests_on(&tp, &o, &mut g).unwrap().env),
                };
            }
            assert_eq!(got, fresh, "order {order:?}");
        }
    }

    /// A repo withheld for one walk on a shared graph is said withheld by every later answer on it, though a graph of
    /// its own wouldn't say so: what a walk found withheld is kept for the rest of the call (see `Consulted::reset`), so
    /// an answer may say more than it read, never less. Here the impact walk reads the workspace's summaries, where a
    /// sibling repo is blocked whole; the tests walk reads none.
    #[test]
    fn a_repo_withheld_for_one_walk_is_said_withheld_by_the_next() {
        let cache = test_cache();
        let base = cache.dir("task-shared-withheld");
        let home = repo(
            &base,
            "ledger",
            &[
                ("src/Ledger.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/A.cs", "namespace Ledger;\npublic static class A\n{\n    public static void Post() { }\n}\n"),
                ("tests/ATests.cs", "namespace Ledger.Tests;\npublic class ATests\n{\n    public void Posts() { Ledger.A.Post(); }\n}\n"),
            ],
        );
        let _vault = repo(
            &base,
            "vault",
            &[
                (".knowcode-privacy.yml", "level: 1\n"),
                ("src/Vault.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/Keys.cs", "namespace Vault;\npublic class Keys\n{\n    public void Seal() { Ledger.A.Post(); }\n}\n"),
            ],
        );
        let s = Scope { repos: vec![home.clone()], no_sync: false };
        let ip = ops::ImpactParams { file: Some("src/A.cs".into()), depth: 1, limit: 30, ..Default::default() };
        let tp = ops::TestsParams { targets: vec!["src/A.cs".into()], diff: None, limit: 4, evidence: String::new() };
        let withheld = |env: &Value| incomplete_of(env).iter().any(|i| i.reason == Reason::Withheld);
        assert!(withheld(&ops::impact(&ip, &s).unwrap().env), "the impact walk meets the withheld repo");
        assert!(!withheld(&ops::tests(&tp, &s).unwrap().env), "the tests walk alone doesn't");
        let o = ops::open_member(std::slice::from_ref(&home), &home, false).unwrap().unwrap();
        let mut g = crate::walk::Graph::new(&o, &[], false, usize::MAX);
        assert!(withheld(&ops::impact_on(&ip, &s, &o, &mut g).unwrap().env));
        let after = ops::tests_on(&tp, &o, &mut g).unwrap().env;
        assert!(withheld(&after), "said again after the impact walk: {after}");
        // Said without a path, as every withheld entry is.
        assert!(!after.to_string().contains("Keys.cs"), "{after}");
    }
}
