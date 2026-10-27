//! The answer contract on the JSON envelope (types in [`kc_store::answer`], documented in docs/answers.md, "Answer
//! contract"): `complete`, `incomplete`, `more` and `provenance`, the same for every command and tool.
//!
//! Operations say what they cut or couldn't read either with the fields they always had (`truncated`, `capped`,
//! `freshness.failed`, a list's `*_total`, the MCP server's `omitted` and `left_out`), which [`finish`] reads, or by
//! [`push_incomplete`] for what no field says. Each entry goes to `incomplete` when it is about what was asked, and to
//! `more` when it is a list on the side that a limit cut (see [`side`]): `complete` speaks for what was asked.
//! [`finish`] is idempotent: it runs when an operation's envelope is made, again after anything cuts it further (the
//! MCP size cap), and its result always has `complete == incomplete.is_empty()`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use kc_store::answer::{Incomplete, PolicyReport, Provenance, Reason};
use serde_json::{Value, json};

/// Adds `i` to the envelope's `incomplete` (once) and marks it not `complete`; or to `more`, when it is a list on the
/// side that a limit cut (see [`side`]).
pub fn push_incomplete(env: &mut Value, i: Incomplete) {
    let mut list = entries_of(env);
    if !list.contains(&i) {
        list.push(i);
    }
    set(env, list);
}

/// Keeps the envelope's entries (`incomplete` and `more`) that `keep` accepts.
pub fn retain_incomplete(env: &mut Value, keep: impl Fn(&Incomplete) -> bool) {
    let list = entries_of(env).into_iter().filter(|i| keep(i)).collect();
    set(env, list);
}

/// The envelope's `incomplete` entries.
pub fn incomplete_of(env: &Value) -> Vec<Incomplete> {
    env.get("incomplete").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}

/// The envelope's `more` entries: lists on the side of what was asked that a limit cut, each saying how to get the rest.
pub fn more_of(env: &Value) -> Vec<Incomplete> {
    env.get("more").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}

/// Every entry, `incomplete` and `more`.
fn entries_of(env: &Value) -> Vec<Incomplete> {
    let mut list = incomplete_of(env);
    list.extend(more_of(env));
    list
}

/// Lists around what an answer is about rather than the answer: a symbol's callers, callees and members, the tables its
/// calls reach, the candidate targets of one reference, its source, the repos read, the files changed with a symbol
/// (`history`), and strings the size cap shortened (`…`). Cut, they leave the answer whole and the rest is a call away
/// (`more`). The lists an answer is about (its `items`, implementations, DI registrations, links, a walk's nodes) are
/// cut last by the size cap (`mcp::answer_list`), and cut, they make it incomplete.
const SIDE_LISTS: &[&str] =
    &["callers", "callees", "members", "touches_tables", "touches", "targets", "source", "repos", "changed_with", "…"];

/// Whether `i` says a list on the side of what was asked was cut, by a limit or to fit the size budget: one of
/// [`SIDE_LISTS`], cites past the first few quoting only their place, or the answer's own `items` cut by their limit
/// where `rest` says the rows cut are past what was asked (`find`: none has the name asked; `search`: every file with a
/// match is named).
fn side(env: &Value, i: &Incomplete) -> bool {
    if !matches!(i.reason, Reason::Capped | Reason::Budget) {
        return false;
    }
    if i.detail.starts_with("cite text: ") {
        return true;
    }
    let (_, place) = subject(i);
    let list = place.rsplit(['/', '.', '[']).map(|k| k.trim_end_matches(']')).find(|k| !k.is_empty() && !k.bytes().all(|b| b.is_ascii_digit()));
    match list {
        Some("items") => i.reason == Reason::Capped && place == "items" && env["rest"].is_string(),
        Some(k) => SIDE_LISTS.contains(&k),
        None => false,
    }
}

fn set(env: &mut Value, list: Vec<Incomplete>) {
    let (more, list): (Vec<Incomplete>, Vec<Incomplete>) = list.into_iter().partition(|i| side(env, i));
    let Some(obj) = env.as_object_mut() else { return };
    obj.insert("complete".into(), Value::Bool(list.is_empty()));
    for (key, list) in [("incomplete", list), ("more", more)] {
        if list.is_empty() {
            obj.remove(key);
        } else {
            obj.insert(key.into(), json!(list));
        }
    }
}

/// What an entry is about, for replacing an entry derived from a field when the field changes: its reason and the
/// list it names (the detail up to its first `:`).
fn subject(i: &Incomplete) -> (Reason, String) {
    (i.reason, i.detail.split(':').next().unwrap_or("").to_string())
}

/// Brings `complete` and `incomplete` in line with what the envelope says: every entry pushed, plus one for each
/// partiality field (recomputed, so a list cut again is counted as it is now). Drops the dummy `cursor`.
pub fn finish(env: &mut Value) {
    if !env.is_object() {
        return;
    }
    if env.get("provenance").is_some() {
        summary_provenance(env);
    }
    let mut sampled = Vec::new();
    let derived = derive(env, &mut sampled);
    if let Some(obj) = env.as_object_mut() {
        if sampled.is_empty() {
            obj.remove("sampled");
        } else {
            obj.insert("sampled".into(), json!(sampled));
        }
    }
    let subjects: HashSet<(Reason, String)> = derived.iter().map(subject).collect();
    let mut list: Vec<Incomplete> = entries_of(env).into_iter().filter(|i| !subjects.contains(&subject(i))).collect();
    for d in derived {
        if !list.contains(&d) {
            list.push(d);
        }
    }
    set(env, list);
    if let Some(obj) = env.as_object_mut()
        && obj.get("cursor").is_some_and(Value::is_null)
    {
        obj.remove("cursor");
    }
}

/// Entries for the partiality fields `env` carries.
fn derive(env: &Value, sampled: &mut Vec<String>) -> Vec<Incomplete> {
    let mut out = Vec::new();
    let hint = env["hint"].as_str();
    let with_hint = |i: Incomplete| match hint {
        Some(h) => i.with_hint(h),
        None => i,
    };
    // Files that could not be read: their facts are as last indexed.
    let mut failed = |v: &Value| {
        let files: Vec<&str> = v["freshness"]["failed"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if !files.is_empty() {
            let repo = v["repo"].as_str().map(name).unwrap_or_default();
            out.push(
                Incomplete::new(Reason::Failed, format!("{repo}: could not read {}; what comes from them is as last indexed (marked stale)", files.join(", ")))
                    .with_hint("fix the files' permissions or encoding, then ask again"),
            );
        }
    };
    match env["repos"].as_array() {
        Some(list) => list.iter().for_each(&mut failed),
        None => failed(env),
    }
    // Edges the evidence policy asked for left out.
    if let Some(i) = env.get("policy").and_then(|p| serde_json::from_value::<PolicyReport>(p.clone()).ok()).and_then(|p| p.incomplete()) {
        out.push(i);
    }
    if env["capped"].as_bool() == Some(true) {
        out.push(with_hint(Incomplete::new(Reason::Capped, "candidates: the candidate cap was reached, so `total` is a lower bound and a match may be missing")));
    }
    // The size cap of the MCP server.
    if let Some(lost) = env["omitted"].as_object() {
        for (path, n) in lost {
            let i = Incomplete::new(Reason::Budget, format!("{}: {} cut to fit max_chars", if path.is_empty() { "/" } else { path.as_str() }, plural(n.as_u64().unwrap_or(0), "item")));
            // A list that continues: its cursor gets the rest.
            out.push(match crate::cursor::cursor_at(env, path) {
                Some(_) => i.with_hint(crate::cursor::call(&crate::cursor::field_of(path))),
                None => with_hint(i),
            });
        }
    }
    if let Some(left) = env["left_out"].as_array() {
        let n = left.len() as u64 + env["left_out_more"].as_u64().unwrap_or(0);
        out.push(with_hint(Incomplete::new(Reason::Budget, format!("inputs: {} not run to fit max_chars", plural(n, "input")))));
    }
    // Lists anywhere in the answer that carry their own totals or flags, aggregated by where they are. `deploy` is
    // the first item again (a repo's deploy answer): its cuts are the item's.
    let mut cuts = Cuts::default();
    if let Some(obj) = env.as_object() {
        for (k, v) in obj.iter().filter(|(k, _)| !matches!(k.as_str(), "freshness" | "repos" | "knowcode" | "incomplete" | "more" | "provenance" | "omitted" | "sampled" | "policy" | "deploy")) {
            cuts.walk(v, k);
        }
    }
    *sampled = cuts.1.iter().cloned().collect();
    out.extend(cuts.entries(env));
    // A deploy answer's CI pipeline: includes it couldn't read, and what its limits left out (anywhere in the answer:
    // the same pipeline shows under `deploy` and the first item).
    let mut ci: Vec<(Reason, String)> = Vec::new();
    ci_notes(env, &mut ci);
    for (reason, why) in ci {
        let i = Incomplete::new(reason, format!("deploy chains: {why}"));
        out.push(match reason {
            Reason::Capped => i.with_hint("the chains past the limit aren't listed; ask about the job or playbook directly"),
            Reason::MissingRepo => i
                .with_code(kc_store::answer::Code::NotIndexed)
                .with_hint("clone and index the repo holding it (its project path and ref are named); chains through it aren't listed"),
            Reason::Unsupported => i.with_hint("GitLab's own template or a generated file: read it on the GitLab instance"),
            _ => i.with_hint("set the variable its path needs, or check the file it names is there"),
        });
    }
    // The answer's own list: cut by its limit (fewer shown than its total), or said cut with nothing above saying why.
    let shown = env["items"].as_array().map_or(0, Vec::len) as u64;
    let limited = env["total"].as_u64().is_some_and(|t| t > shown);
    // A sample flagged by a hint (the MCP server's `truncated` for a summary's areas) explains it too.
    let explained = out.iter().any(|i| matches!(i.reason, Reason::Capped | Reason::Budget | Reason::Deadline)) || !sampled.is_empty();
    if env["truncated"].as_bool() == Some(true) && (limited || !explained) {
        let mut detail = match env["total"].as_u64() {
            Some(t) if t > shown => format!("items: {shown} of {t} shown"),
            _ => "items: the list was cut short".to_string(),
        };
        // What the rows cut are, when they are past what was asked (see [`side`]).
        if let Some(rest) = env["rest"].as_str() {
            detail = format!("{detail}; {rest}");
        }
        let paged = env["next_offset"].as_u64().map(|n| format!("page with offset={n}"));
        let resume = env["cursor"].as_str().map(|_| crate::cursor::call("cursor"));
        out.push(match (hint, paged) {
            (_, Some(p)) if resume.is_some() => Incomplete::new(Reason::Capped, detail).with_hint(format!("{}; or {p}", resume.unwrap_or_default())),
            _ if resume.is_some() => Incomplete::new(Reason::Capped, detail).with_hint(resume.unwrap_or_default()),
            (Some(h), _) => Incomplete::new(Reason::Capped, detail).with_hint(h),
            (None, Some(p)) => Incomplete::new(Reason::Capped, detail).with_hint(p),
            (None, None) => Incomplete::new(Reason::Capped, detail).with_hint("raise the limit"),
        });
    }
    out
}

/// Lists cut inside an answer, by their place with indexes dropped (`items[].callers`): how many were cut, and their
/// shown and total lengths (`None` when only a flag says so).
#[derive(Default)]
struct Cuts(std::collections::BTreeMap<String, Cut>, std::collections::BTreeSet<String>);

/// The lists cut at one place: how many, their shown and total lengths (`None` when only a flag says so), and the
/// cursors continuing them.
struct Cut(u64, u64, Option<u64>, Vec<String>);

/// How a list an answer shortened counts.
#[derive(PartialEq)]
enum Shortened {
    /// It answers the question: cut, the answer is incomplete.
    Cut,
    /// A sample by design (a summary's top few): its total is given, the answer isn't incomplete for it.
    Sampled,
    /// Not shortened at all: filtered to what was asked (`areas --area`).
    Filtered,
}

/// Whether `list` of the object `obj` (at `path`) is a sample: overview's entry points, key types, hotspots, areas and
/// their key files and dependencies, its tables; areas' key files, entry points, exposed files and dependencies; locate's
/// symbols per file; contributors (ownership); the ranked map and its symbols per file; the tight list of tests to run;
/// `coupled`'s structural guesses.
fn shortened(path: &str, list: &str, obj: &serde_json::Map<String, Value>) -> Shortened {
    let overview = obj.contains_key("key_types") || obj.contains_key("top_hotspots");
    match list {
        "entry_points" | "key_types" | "top_hotspots" | "key_files" | "exposed" | "contributors" | "map" => Shortened::Sampled,
        "depends_on" if path.contains(".areas[]") => Shortened::Sampled,
        // A file's symbols in the map, or in a located file.
        "symbols" if path.ends_with(".map.files[]") || obj.contains_key("score") => Shortened::Sampled,
        "areas" | "tables" if overview => Shortened::Sampled,
        // The tests to run: the most likely relevant of every candidate, by design (the rest are `left_out`).
        "tests" if obj.contains_key("tests_are") => Shortened::Sampled,
        // Structural guesses at co-change partners when history had nothing: the strongest few, by design.
        "structural" => Shortened::Sampled,
        // `areas` lists every area, or the one asked for.
        "areas" if obj.contains_key("modularity") => Shortened::Filtered,
        _ => Shortened::Cut,
    }
}

impl Cuts {
    /// Records `list` of `obj` (at `path`) shortened to `shown` of `total`: cut, sampled or neither.
    fn note(&mut self, path: &str, list: &str, obj: &serde_json::Map<String, Value>, shown: u64, total: Option<u64>) {
        match shortened(path, list, obj) {
            Shortened::Cut => self.add(format!("{path}.{list}"), shown, total, obj.get(&format!("{list}_cursor")).and_then(Value::as_str)),
            Shortened::Sampled => {
                self.1.insert(list.to_string());
            }
            Shortened::Filtered => {}
        }
    }

    fn add(&mut self, path: String, shown: u64, total: Option<u64>, cursor: Option<&str>) {
        let e = self.0.entry(path).or_insert(Cut(0, 0, Some(0), Vec::new()));
        e.0 += 1;
        e.1 += shown;
        e.2 = e.2.zip(total).map(|(a, b)| a + b);
        e.3.extend(cursor.map(str::to_string));
    }

    /// Records the cuts `v` (at `path`) says of itself: `<list>_total` beyond its `<list>` (`callers_summary.total`
    /// for `callers`), `<list>_truncated` (`source_truncated`), and a nested `truncated` with the count `omitted` (a
    /// walk at its node limit).
    fn walk(&mut self, v: &Value, path: &str) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| self.walk(x, &format!("{path}[]"))),
            Value::Object(obj) => {
                let len = |k: &str| obj.get(k).and_then(Value::as_array).map(|a| a.len() as u64);
                for (key, x) in obj {
                    if let Some(list) = key.strip_suffix("_total")
                        && let (Some(total), Some(shown)) = (x.as_u64(), len(list))
                        && total > shown
                    {
                        self.note(path, list, obj, shown, Some(total));
                    }
                    if let Some(list) = key.strip_suffix("_truncated")
                        && x.as_bool() == Some(true)
                    {
                        self.note(path, list, obj, len(list).unwrap_or(0), None);
                    }
                }
                if let (Some(total), Some(shown)) = (obj.get("callers_summary").and_then(|c| c["total"].as_u64()), len("callers"))
                    && total > shown
                {
                    self.add(format!("{path}.callers"), shown, Some(total), obj.get("callers_cursor").and_then(Value::as_str));
                }
                // A flow or a path search says its own cut, with the call going on (`flow::stop_incomplete`, `walk::path`).
                let says_own = (obj.contains_key("entry") && obj.contains_key("steps")) || obj.contains_key("explored_forward");
                if obj.get("truncated").and_then(Value::as_bool) == Some(true) && !says_own {
                    let shown = len("nodes").unwrap_or(0);
                    let omitted = obj.get("omitted").and_then(Value::as_u64);
                    // The ranked map within its budget is a sample; a walk at its node limit is cut.
                    if path.ends_with(".map") {
                        self.1.insert("map".to_string());
                    } else {
                        // A walk's own list (`nodes`, `affected`) continues by its cursor.
                        let cursor = ["nodes_cursor", "affected_cursor"].iter().find_map(|k| obj.get(*k).and_then(Value::as_str));
                        self.add(path.to_string(), shown, omitted.map(|n| n + shown), cursor);
                    }
                }
                for (k, x) in obj {
                    self.walk(x, &format!("{path}.{k}"));
                }
            }
            _ => {}
        }
    }

    fn entries(self, env: &Value) -> Vec<Incomplete> {
        self.0
            .into_iter()
            .map(|(path, Cut(n, shown, total, cursors))| {
                let list = path.rsplit(['.', '[']).next().unwrap_or("").trim_end_matches(']').to_string();
                let lists = if n > 1 { format!("{n} lists cut, ") } else { String::new() };
                let detail = match total {
                    Some(t) => format!("{path}: {lists}{shown} of {t} shown"),
                    None if n > 1 => format!("{path}: {n} cut short"),
                    None => format!("{path}: cut short"),
                };
                let hint = if cursors.is_empty() {
                    cut_hint(&list).to_string()
                } else {
                    cursors.iter().map(|c| crate::cursor::call(&crate::cursor::find(env, c).unwrap_or_else(|| format!("{list}_cursor")))).collect::<Vec<_>>().join("; ")
                };
                Incomplete::new(Reason::Capped, detail).with_hint(hint)
            })
            .collect()
    }
}

fn cut_hint(list: &str) -> &'static str {
    match list {
        "callers" => "refs lists every caller",
        "targets" => "every candidate: refs or context on the target's qualified name",
        "source" => "raise the budget, or ask for the skeleton",
        "" | "items" => "raise the node limit (--max-nodes / max_nodes), lower the depth, or filter edges",
        _ => "raise the limit or the budget",
    }
}

/// CI includes not read (`ci_unread`) and limits' cuts (`ci_cut`) anywhere in `v`, once each.
fn ci_notes(v: &Value, out: &mut Vec<(Reason, String)>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| ci_notes(x, out)),
        Value::Object(m) => {
            for (k, x) in m {
                let reason = |s: &str| match k.as_str() {
                    "ci_cut" => Some(Reason::Capped),
                    // A gone checkout is named missing with its repo (`ops::add_missing`), once.
                    "ci_unread" if s.contains("(checkout gone") => None,
                    "ci_unread" if s.contains("(not indexed") => Some(Reason::MissingRepo),
                    "ci_unread" if s.contains("(unsupported:") => Some(Reason::Unsupported),
                    "ci_unread" => Some(Reason::Failed),
                    _ => None,
                };
                match x {
                    Value::Array(list) if matches!(k.as_str(), "ci_cut" | "ci_unread") => {
                        for s in list.iter().filter_map(Value::as_str) {
                            if let Some(r) = reason(s)
                                && !out.iter().any(|(_, w)| w == s)
                            {
                                out.push((r, s.to_string()));
                            }
                        }
                    }
                    _ => ci_notes(x, out),
                }
            }
        }
        _ => {}
    }
}

fn plural(n: u64, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

fn name(path: &str) -> String {
    Path::new(path).file_name().map_or(path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// What the envelope says beyond the answer itself, as lines for a text rendering: each `incomplete` and `more` entry,
/// and the repos consulted besides the ones answered for.
pub fn notes(env: &Value) -> Vec<String> {
    let mut out: Vec<String> = incomplete_of(env).iter().map(Incomplete::line).collect();
    out.extend(more_of(env).iter().map(more_line));
    let answered: HashSet<&str> = match env["repos"].as_array() {
        Some(list) => list.iter().filter_map(|r| r["repo"].as_str()).collect(),
        None => env["repo"].as_str().into_iter().collect(),
    };
    let others: Vec<String> = provenance_of(env).iter().filter(|p| !answered.contains(p.repo.as_str())).map(Provenance::line).collect();
    if !others.is_empty() {
        out.push(format!("also consulted: {}", others.join(", ")));
    }
    out
}

/// The text rendering of a `more` entry: `more (budget): items[0].callers: 8 items cut to fit max_chars; kc_more …`.
pub fn more_line(i: &Incomplete) -> String {
    match &i.hint {
        Some(h) => format!("more ({}): {}; {h}", i.reason.as_str(), i.detail),
        None => format!("more ({}): {}", i.reason.as_str(), i.detail),
    }
}

/// The envelope's `provenance` entries.
pub fn provenance_of(env: &Value) -> Vec<Provenance> {
    env.get("provenance").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}

/// Repo names (`"repo": "…"` anywhere in `v`, bare names or paths): what the answer drew from which repo.
fn named_repos(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| named_repos(x, out)),
        Value::Object(m) => {
            for (k, x) in m {
                match (k.as_str(), x) {
                    ("repo" | "site_repo" | "to_repo" | "from_repo", Value::String(s)) => {
                        if !out.contains(s) {
                            out.push(s.clone());
                        }
                    }
                    _ => named_repos(x, out),
                }
            }
        }
        _ => {}
    }
}

/// Adds to `provenance` the repos the answer names but didn't open: those it drew from their workspace summary (an
/// HTTP route's handler, a message's consumer, a SQL table's definition), as of their last sync. A summary still
/// holds while its repo is unchanged since (checked by `stat`, as the workspace does); when the query checked
/// freshness and a summary no longer holds, the answer is `stale` there. Under `--no-sync` nothing was checked:
/// `fresh` is false and nothing more is said.
fn summary_provenance(env: &mut Value) {
    let mut list = provenance_of(env);
    let mut names = Vec::new();
    if let Some(obj) = env.as_object() {
        for (_, v) in obj.iter().filter(|(k, _)| !matches!(k.as_str(), "repo" | "repos" | "freshness" | "knowcode" | "incomplete" | "more" | "provenance")) {
            named_repos(v, &mut names);
        }
    }
    names.retain(|n| n != "workspace");
    if names.is_empty() {
        return;
    }
    let answered: Vec<PathBuf> = list.iter().map(|p| PathBuf::from(&p.repo)).collect();
    let checked = list.iter().any(|p| p.fresh);
    let Ok(summaries) = kc_index::workspace::summaries(&answered) else { return };
    let cached_reads = kc_index::workspace::cached_reads();
    let mut stale = Vec::new();
    for n in names {
        let checkouts: Vec<&kc_index::workspace::RepoSummary> =
            summaries.iter().filter(|s| s.root.as_os_str() == n.as_str() || s.root.file_name().is_some_and(|f| f == n.as_str())).collect();
        let Some(s) = kc_index::workspace::choose(&checkouts, &answered) else { continue };
        let known = list.iter().any(|p| p.repo == n || Path::new(&p.repo).file_name().is_some_and(|f| f == n.as_str()));
        let cached_generation = cached_reads.get(&s.root);
        if known && cached_generation.is_none() { continue; }
        // Checked here, not through the query's memo of changes: a memo held by another query in the process (a
        // server's, a test's) may predate the change.
        let changed = checked && (cached_generation.is_some() || kc_index::db_path(&s.root).is_ok_and(|db| kc_index::changed_since_sync(&s.root, &db)));
        if changed {
            let detail = match cached_generation {
                Some(generation) => format!("{} (summary generation {generation})", name(&s.root.to_string_lossy())),
                None => name(&s.root.to_string_lossy()),
            };
            if !stale.contains(&detail) { stale.push(detail); }
            if s.cached_only() && let Err(e) = crate::refresh::restore_cached(&s.root) {
                eprintln!("knowcode: restoring cached index {}: {e:#}", s.root.display());
            }
        }
        if !known { list.push(Provenance {
            repo: s.root.to_string_lossy().into_owned(),
            commit: if cached_generation.is_some() { None } else { kc_index::head_commit(&s.root) },
            // The summary's own generation: the index's isn't read for a repo answered from its summary.
            generation: Some(cached_generation.map(String::as_str).unwrap_or_else(|| s.generation())).filter(|g| !g.is_empty()).map(|g| format!("summary:{g}")),
            dirty: None,
            fresh: checked && !changed,
        }); }
    }
    env["provenance"] = json!(list);
    if !stale.is_empty() {
        push_incomplete(
            env,
            Incomplete::new(Reason::Stale, format!("workspace summary: {} changed since last synced; what comes from them is as of that sync", stale.join(", ")))
                .with_hint("ask with --repo naming them, or `knowcode index` them, to bring them up to date"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{repo, test_cache};
    use crate::ops::{self, ImpactParams, Scope, WalkFilter};

    fn scope(r: &Path) -> Scope {
        Scope { repos: vec![r.to_path_buf()], no_sync: false }
    }

    /// The provenance entry for the repo named `name`.
    fn consulted<'a>(env: &'a Value, name: &str) -> Option<&'a Value> {
        env["provenance"].as_array()?.iter().find(|p| p["repo"].as_str().is_some_and(|r| r.ends_with(&format!("/{name}"))))
    }

    /// Every entry, `incomplete` and `more`.
    fn reasons(env: &Value) -> Vec<String> {
        entries_of(env).iter().map(|i| format!("{}: {}", i.reason.as_str(), i.detail)).collect()
    }

    /// The `more` entries.
    fn more(env: &Value) -> Vec<String> {
        more_of(env).iter().map(|i| format!("{}: {}", i.reason.as_str(), i.detail)).collect()
    }

    /// A library with an interface and a base class, and an app depending on it by package.
    fn lib_and_app(base: &Path) -> (PathBuf, PathBuf) {
        let lib = repo(
            base,
            "lib",
            &[
                ("src/Hosting.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><AssemblyName>Contoso.Timing</AssemblyName></PropertyGroup>\n</Project>\n"),
                (
                    "src/StartupBase.cs",
                    "namespace Contoso.Timing;\npublic abstract class StartupBase\n{\n    public void Configure() { RegisterServices(1); }\n    protected abstract void RegisterServices(int n);\n}\n",
                ),
                ("src/IClock.cs", "namespace Contoso.Timing;\npublic interface IClock { int Now(); }\n"),
            ],
        );
        let app = repo(
            base,
            "app",
            &[
                ("src/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <ItemGroup><PackageReference Include=\"Contoso.Timing\" Version=\"1.*\" /></ItemGroup>\n</Project>\n"),
                ("src/Startup.cs", "using Contoso.Timing;\nnamespace App;\npublic class Startup : StartupBase\n{\n    protected override void RegisterServices(int n) { }\n}\n"),
                ("src/Clock.cs", "using Contoso.Timing;\nnamespace App;\npublic class Clock : IClock\n{\n    public int Now() => 1;\n}\n"),
            ],
        );
        (lib, app)
    }

    #[test]
    fn provenance_lists_the_dependencies_and_dependents_an_answer_opened() {
        let cache = test_cache();
        let base = cache.dir("c-provenance");
        let (lib, app) = lib_and_app(&base);
        // The library's interface: its implementations are in the app, which the answer opens.
        let env = ops::context("IClock", 4000, Default::default(), &scope(&lib)).unwrap().env;
        let (l, a) = (consulted(&env, "lib").expect("the queried repo"), consulted(&env, "app").expect("the dependent opened"));
        assert_eq!((l["fresh"].as_bool(), a["fresh"].as_bool()), (Some(true), Some(true)), "{env}");
        assert!(l["generation"].is_string() && a["generation"].is_string(), "{env}");
        // The app's override: its caller is the library's base, a dependency the answer opens.
        let env = ops::context("Startup.RegisterServices", 4000, Default::default(), &scope(&app)).unwrap().env;
        assert!(consulted(&env, "lib").is_some(), "{}", env["provenance"]);
        assert_eq!(env["complete"], json!(true), "{env}");
        // Impact across the repo boundary names the dependent too.
        let p = ImpactParams { symbol: Some("IClock".into()), depth: 1, limit: 100, ..Default::default() };
        let env = ops::impact(&p, &scope(&lib)).unwrap().env;
        assert!(consulted(&env, "app").is_some(), "{}", env["provenance"]);
        // Without a freshness check, nothing is fresh.
        let env = ops::context("IClock", 4000, Default::default(), &Scope { repos: vec![lib.clone()], no_sync: true }).unwrap().env;
        assert!(env["provenance"].as_array().unwrap().iter().all(|p| p["fresh"] == json!(false)), "{}", env["provenance"]);
        // The library's checkout is gone, its index left: the answer says what it couldn't resolve into.
        std::fs::rename(lib.join(".git"), lib.join("git-moved")).unwrap();
        let env = ops::context("Startup.RegisterServices", 4000, Default::default(), &scope(&app)).unwrap().env;
        std::fs::rename(lib.join("git-moved"), lib.join(".git")).unwrap();
        assert!(reasons(&env).iter().any(|r| r.starts_with("missing_repo: lib")), "{env}");
        assert_eq!(env["complete"], json!(false));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A frontend requesting a route, and the API serving it.
    fn web_and_api(base: &Path) -> (PathBuf, PathBuf) {
        let web = repo(
            base,
            "web",
            &[
                ("src/Api.ts", "export class Api {\n  static readonly filters = 'Filters';\n}\n"),
                ("src/contracts.ts", "@RequestContract({ endpoint: Api.filters, httpMethod: HttpMethods.post })\nexport class ApiGetFilters {}\n"),
                (
                    "src/filter.service.ts",
                    "export class FilterService {\n  constructor(private http: HttpService) {}\n  load(): void {\n    this.http.send(new ApiGetFilters());\n  }\n}\n",
                ),
            ],
        );
        let api = repo(
            base,
            "api",
            &[
                ("src/SearchController.cs", "public class SearchController : Controller\n{\n    private readonly FilterQuery _q;\n    [HttpPost(nameof(Filters))]\n    public Task Filters() => _q.Run();\n}\n"),
                ("src/FilterQuery.cs", "public class FilterQuery\n{\n    public Task Run() => null;\n}\n"),
            ],
        );
        (web, api)
    }

    #[test]
    fn provenance_names_repos_crossed_by_http_and_says_when_a_summary_no_longer_holds() {
        let cache = test_cache();
        let base = cache.dir("c-provenance-http");
        let (web, api) = web_and_api(&base);
        // Impact crosses into the frontend, which it opens.
        let p = ImpactParams { symbol: Some("FilterQuery.Run".into()), depth: 3, limit: 100, ..Default::default() };
        let env = ops::impact(&p, &scope(&api)).unwrap().env;
        assert_eq!(consulted(&env, "web").map(|w| &w["fresh"]), Some(&json!(true)), "{}", env["provenance"]);
        // The routes answer reads the other side from its workspace summary: current while the repo is unchanged.
        let env = ops::routes(&scope(&web)).unwrap().env;
        assert_eq!(consulted(&env, "api").map(|a| &a["fresh"]), Some(&json!(true)), "{env}");
        assert_eq!(env["complete"], json!(true), "{env}");
        // Changed since: the summary is as of the last sync, and the answer says so.
        std::fs::write(api.join("src/FilterQuery.cs"), "public class FilterQuery\n{\n    public Task Run() => null;\n    public int Count() => 0;\n}\n").unwrap();
        let env = ops::routes(&scope(&web)).unwrap().env;
        assert_eq!(consulted(&env, "api").map(|a| &a["fresh"]), Some(&json!(false)), "{env}");
        assert!(reasons(&env).iter().any(|r| r.starts_with("stale: workspace summary: api")), "{env}");
        assert_eq!(env["complete"], json!(false));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Every answer's `complete` is `incomplete` being empty, and any list an answer says it cut (a total beyond
    /// the list, `truncated`, `capped`) is said: in `incomplete` when it is what was asked, in `more` when it is on the
    /// side.
    fn check(env: &Value) {
        assert_eq!(env["complete"].as_bool(), Some(incomplete_of(env).is_empty()), "{env}");
        assert!(env.get("cursor").is_none_or(Value::is_string), "a dummy cursor: {env}");
        if env["truncated"] == json!(true) || env["capped"] == json!(true) || !Cuts::of(env).0.is_empty() {
            assert!(!entries_of(env).is_empty(), "a cut list not said: {env}");
        }
        if env["truncated"] == json!(true) && env.get("rest").is_none() || env["capped"] == json!(true) {
            assert_eq!(env["complete"], json!(false), "the answer's own list cut but complete: {env}");
        }
        assert!(more_of(env).iter().all(|i| matches!(i.reason, Reason::Capped | Reason::Budget)), "{env}");
    }

    impl Cuts {
        fn of(env: &Value) -> Cuts {
            let mut c = Cuts::default();
            c.walk(&env["items"], "items");
            c
        }
    }

    #[test]
    fn invariant_every_list_a_limit_cuts_is_said() {
        let cache = test_cache();
        let base = cache.dir("c-limits");
        let mut files: Vec<(String, String)> = vec![("src/IThing.cs".into(), "namespace N;\npublic interface IThing { void Go(); }\n".into())];
        // More implementations than a context lists, and more same-named methods than a reference lists.
        for i in 0..55 {
            files.push((format!("src/T{i}.cs"), format!("namespace N;\npublic class T{i} : IThing {{ public void Go() {{ }} }}\n")));
        }
        let py: String = (0..55).map(|i| format!("class P{i}:\n    def go(self):\n        pass\n")).collect();
        files.push(("src/many.py".into(), py));
        files.push(("src/caller.py".into(), "def run(x):\n    x.go()\n    x.go()\n".into()));
        let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let root = repo(&base, "app", &files);
        let s = scope(&root);

        let env = ops::context("IThing", 2000, Default::default(), &s).unwrap().env;
        check(&env);
        assert_eq!(env["items"][0]["conformers_total"], json!(55), "{env}");
        assert!(reasons(&env).iter().any(|r| r.starts_with("capped: items[].conformers: 50 of 55 shown")), "{:?}", reasons(&env));
        // The implementations are what a type's context is about: cut, the answer is incomplete.
        assert_eq!(env["complete"], json!(false), "{env}");

        // A reference to one of many same-named methods lists the first few candidates, and says how many.
        let env = ops::refs("go", &[], 1, &s).unwrap().env;
        check(&env);
        assert_eq!(env["items"][0]["targets_total"], json!(55), "{env}");
        let r = reasons(&env);
        // One reference's candidates are on the side: more to read, not missing.
        assert!(more(&env).iter().any(|r| r.starts_with("capped: items[].targets: ")), "{r:?}");
        // `-n 1` of two references: the list itself is cut.
        assert!(incomplete_of(&env).iter().any(|i| i.detail.starts_with("items: 1 of")), "{r:?}");

        for env in [
            ops::find("T", None, 2, &s).unwrap().env,
            ops::locate("thing go", 1, &s).unwrap().env,
            ops::trace("IThing.Go", "in", 2, 3, &WalkFilter::default(), &s).unwrap().env,
            ops::impact(&ImpactParams { symbol: Some("IThing.Go".into()), depth: 2, limit: 3, ..Default::default() }, &s).unwrap().env,
            ops::hotspots(None, 1, None, &s).unwrap().env,
            ops::overview(&s, None, 50).unwrap().env,
            ops::areas(None, &s).unwrap().env,
        ] {
            check(&env);
        }
        // A summary's samples by design: their totals given, named in `sampled`, the answer still complete.
        let env = ops::overview(&s, None, 0).unwrap().env;
        assert_eq!(env["items"][0]["top_hotspots_total"], json!(58), "{env}");
        assert_eq!(env["sampled"], json!(["top_hotspots"]), "{env}");
        assert_eq!(env["complete"], json!(true), "{env}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn invariant_finish_is_idempotent_and_recounts_a_list_cut_again() {
        let mut env = json!({ "items": [{ "symbol": { "name": "X" }, "callers": [1, 2, 3], "callers_summary": { "total": 9 } }], "total": 1, "truncated": false });
        finish(&mut env);
        let once = env.clone();
        finish(&mut env);
        assert_eq!(env, once);
        // A symbol's callers are on the side of what it is: their cut is more to read, and the answer is complete.
        assert_eq!(more(&env), ["capped: items[].callers: 3 of 9 shown"]);
        assert_eq!((env["complete"].clone(), env.get("incomplete")), (json!(true), None));
        // Cut further (the MCP size cap): the entry is the list as it is now, not a second one.
        env["items"][0]["callers"] = json!([1]);
        env["omitted"] = json!({ "/items/0/callers": 2 });
        finish(&mut env);
        assert_eq!(more(&env), ["budget: /items/0/callers: 2 items cut to fit max_chars", "capped: items[].callers: 1 of 9 shown"]);
        assert_eq!(env["complete"], json!(true));
        // Its implementations are what it is about: cut, the answer is incomplete, the callers still `more`.
        env["items"][0]["conformers"] = json!([1]);
        env["items"][0]["conformers_total"] = json!(4);
        finish(&mut env);
        assert_eq!(incomplete_of(&env).iter().map(|i| i.detail.as_str()).collect::<Vec<_>>(), ["items[].conformers: 1 of 4 shown"]);
        assert_eq!((env["complete"].clone(), more(&env).len()), (json!(false), 2));
        // The answer's own list cut where the rest is past what was asked (`rest`): more, with what the rest is.
        let mut env = json!({ "items": [1, 2], "total": 5, "truncated": true, "rest": "the rest match by prefix" });
        finish(&mut env);
        assert_eq!(more(&env), ["capped: items: 2 of 5 shown; the rest match by prefix"]);
        assert_eq!(env["complete"], json!(true));
        // Cut further to fit, it is incomplete.
        env["items"] = json!([1]);
        env["omitted"] = json!({ "/items": 1 });
        finish(&mut env);
        assert_eq!(reasons(&env), ["budget: /items: 1 item cut to fit max_chars", "capped: items: 1 of 5 shown; the rest match by prefix"]);
        assert_eq!(env["complete"], json!(false));
        // Nothing to report: complete, and no `incomplete` field.
        let mut env = json!({ "items": [], "total": 0, "truncated": false, "cursor": null });
        finish(&mut env);
        assert_eq!(env, json!({ "items": [], "total": 0, "truncated": false, "complete": true }));
    }

    #[test]
    fn context_says_when_its_area_comes_from_an_earlier_index() {
        let cache = test_cache();
        let base = cache.dir("c-stale-areas");
        let root = repo(&base, "app", &[("src/a.py", "def a():\n    b()\n"), ("src/b.py", "def b():\n    pass\n")]);
        let s = scope(&root);
        ops::areas(None, &s).unwrap();
        let env = ops::context("a", 2000, Default::default(), &s).unwrap().env;
        assert_eq!(env["complete"], json!(true), "{env}");
        // An edit moves the index on; the areas are as computed before it, and context doesn't recompute them.
        std::fs::write(root.join("src/b.py"), "def b():\n    return 1\n").unwrap();
        let env = ops::context("a", 2000, Default::default(), &s).unwrap().env;
        let r = reasons(&env);
        assert!(r.iter().any(|r| r.starts_with("stale: context area (app): computed from the index as it was")), "{r:?}");
        // Areas brings them up to date; then context is current again.
        ops::areas(None, &s).unwrap();
        assert_eq!(ops::context("a", 2000, Default::default(), &s).unwrap().env["complete"], json!(true));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn every_resolved_reference_says_what_it_rests_on() {
        let cache = test_cache();
        let base = cache.dir("c-evidence");
        let root = repo(
            &base,
            "app",
            &[
                ("src/m.py", "def helper():\n    pass\n"),
                ("src/use.py", "from m import helper\ndef run():\n    helper()\n"),
                ("src/Shop.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/Store.cs", "namespace Shop;\npublic class Store { public void Save() { } }\npublic class Other { public void Save() { } }\n"),
                ("src/Cart.cs", "namespace Shop;\npublic class Cart { private Store _s; public void Buy() { _s.Save(); } }\n"),
            ],
        );
        let mech = |name: &str| -> Vec<(String, String)> {
            let env = ops::refs(name, &[], 50, &scope(&root)).unwrap().env;
            env["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|h| matches!(h["confidence"].as_str(), Some("exact" | "unique_name" | "ambiguous")))
                .map(|h| (h["confidence"].as_str().unwrap().to_string(), h["evidence"]["mechanism"].as_str().unwrap_or("NONE").to_string()))
                .collect()
        };
        assert_eq!(mech("helper"), [("exact".to_string(), "import".to_string())]);
        assert_eq!(mech("Save"), [("exact".to_string(), "receiver_type".to_string())]);
        // A trace's edges carry the reference's evidence.
        let env = ops::trace("Cart.Buy", "out", 1, 50, &WalkFilter::default(), &scope(&root)).unwrap().env;
        assert_eq!(env["items"][0]["nodes"][1]["evidence"]["mechanism"], json!("receiver_type"), "{env}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
