//! The MCP server's compact answer: the same envelope, said in fewer characters so that rows, not bookkeeping, fill
//! a small `max_chars`. The CLI's `--json` stays full; only what an agent can do without is shortened, never what
//! the answer contract says (`complete`, `incomplete`, every `provenance` entry that differs from the answer's own
//! repo, every cursor):
//!
//! - a repo is named by its short name, the key rows use in `repo` and that the tools' `repo` parameter takes; its
//!   full path only when another indexed repo has the same name. Commits are cut to 12 hex digits;
//! - a `provenance` entry identical to the queried repo (same repo, commit and dirty state, fresh) is dropped: the
//!   envelope's `repo`, `commit` and `dirty` already say it; the others are one string each, as the text rendering
//!   says them (`billing @ 0123456789ab (dirty), from its last sync`): an answer crossing into 40 repos would
//!   otherwise spend most of a small `max_chars` on them;
//! - `omitted` (what the size cap cut, by list) is left out: each cut list has its `incomplete` entry;
//! - `evidence` is a string, `"receiver_type"`, with its assumptions in brackets when it has any:
//!   `"receiver_type [project_unverified]"`;
//! - a cursor is a short handle (`kc~1f`) the server maps back to the full cursor for `kc_more`; it lasts as long as
//!   the server process, for the last [`KEPT`] cursors;
//! - internal row ids, the build stamp (`knowcode`, also in the server's info), zero freshness counters and timings
//!   (`ms` and `*_ms` anywhere, a search's `stats` among them) are left out; a continuation hint names the `kc_more`
//!   call without the CLI form;
//! - a repo's deploy answer is said once, at `items[0]` (the full form repeats it as `deploy`; see [`once`]);
//! - a reference row's `from` is the enclosing symbol's qualified name, and its column is left out; a list whose rows
//!   all have the same `targets`, `from` or `name` says it once, beside the list (see [`lean_row`],
//!   [`shared_targets`]); a list of links says once what all its rows have alike (`repo`, `confidence`,
//!   `assumptions`), each row then a text row, `repo/file:line kind symbol` (see [`shared_link_fields`]);
//! - a test file's ranking `signals` are left out: its `confidence` and `reasons` say them;
//! - a clean, current answer says nothing about its repos: `repos` and `provenance` are left out when no repo is dirty,
//!   every repo was checked against its working tree, and nothing is `stale`, `failed` or `missing_repo` (see
//!   [`clean`]); otherwise `provenance` stays, and `repos` keeps the entries whose `freshness` says something;
//! - what goes without saying is left out of the envelope: `false` flags (`dirty`, `truncated`, `capped`), nulls,
//!   `offset: 0`, a search's `stats` and its `matches` when `total` says it. `complete` stays, true or false, so a
//!   complete answer says it is;
//! - `stale` entries are said in one: the repos named by their short name, entries saying the same thing joined (see
//!   [`one_stale`]);
//! - a search's or a structural search's hits, in its answer and in a page of them, are text rows (see [`hit_row`]):
//!   `repo/file:line kind symbol | text`, the same row for every hit instead of an object repeating its keys.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

use serde_json::{Map, Value};

/// Cursors the server keeps: an older handle is refused, saying to ask again.
const KEPT: usize = 4096;

/// What every handle starts with. A full cursor starts with `kc1.`.
const HANDLE: &str = "kc~";

/// The size a handle is measured at while an answer is cut to fit: longer than any handle this process makes.
const PLACEHOLDER: &str = "kc~ffffffff";

struct Handles {
    next: u64,
    kept: VecDeque<(u64, String)>,
}

static HANDLES: Mutex<Handles> = Mutex::new(Handles { next: 1, kept: VecDeque::new() });

/// A handle for `cursor`, the same one for a cursor handed out before.
fn handle(cursor: &str) -> String {
    let mut h = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((n, _)) = h.kept.iter().find(|(_, c)| c == cursor) {
        return format!("{HANDLE}{n:x}");
    }
    let n = h.next;
    h.next += 1;
    h.kept.push_back((n, cursor.to_string()));
    if h.kept.len() > KEPT {
        h.kept.pop_front();
    }
    format!("{HANDLE}{n:x}")
}

/// The full cursor `cursor` stands for: itself when it is one, the cursor a handle was made for, or an error saying
/// why it can't be continued.
pub fn resolve(cursor: &str) -> Result<String, String> {
    let c = cursor.trim();
    let Some(hex) = c.strip_prefix(HANDLE) else { return Ok(c.to_string()) };
    let n = u64::from_str_radix(hex, 16).map_err(|_| format!("{c} is not a cursor this server made"))?;
    let h = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    h.kept
        .iter()
        .find(|(k, _)| *k == n)
        .map(|(_, full)| full.clone())
        .ok_or_else(|| format!("cursor {c} is no longer held by this server (it keeps the last {KEPT}, until it restarts): ask the original question again"))
}

/// Repo names more than one indexed repo has: those keep their full paths.
#[derive(Default)]
pub struct Names(HashSet<String>);

impl Names {
    /// The names shared by indexed repos now.
    pub fn load() -> Names {
        let mut seen = HashSet::new();
        let mut shared = HashSet::new();
        for r in kc_index::all_indexed_roots().unwrap_or_default() {
            let n = crate::ops::repo_name(&r);
            if !seen.insert(n.clone()) {
                shared.insert(n);
            }
        }
        Names(shared)
    }

    /// How the compact form names the repo at `path`.
    fn key(&self, path: &str) -> String {
        let name = std::path::Path::new(path).file_name().map_or(path.to_string(), |n| n.to_string_lossy().into_owned());
        if self.0.contains(&name) { path.to_string() } else { name }
    }
}

/// The compact form of `env` as it would be output, cursors measured at the longest handle (for cutting to fit).
pub fn measure(env: &Value, names: &Names) -> String {
    let mut v = env.clone();
    compact(&mut v, names, &mut |_| PLACEHOLDER.to_string());
    v.to_string()
}

/// The compact form of `env`, its cursors registered as handles.
pub fn output(env: &Value, names: &Names) -> String {
    let mut v = env.clone();
    compact(&mut v, names, &mut handle);
    v.to_string()
}

/// Drops what the envelope says twice: `deploy`, a repo's deploy answer, when it is the first item as well (the full
/// form keeps both, `deploy` as a stable key for readers; the compact form says it once, at `items[0]`, where its
/// lists' cursors are).
pub fn once(env: &mut Value) {
    let Some(obj) = env.as_object_mut() else { return };
    // The first item carries its lists' cursors besides: the same answer when it is the same repo's.
    let first = obj.get("items").and_then(|i| i.get(0));
    let same = |d: &Value| first.is_some_and(|f| f.get("ansible").is_some() && f.get("repo") == d.get("repo"));
    if obj.get("deploy").is_some_and(same) {
        obj.remove("deploy");
    }
}

/// Rewrites `env` in its compact form, giving each cursor the value `cursor` makes of it.
fn compact(env: &mut Value, names: &Names, cursor: &mut dyn FnMut(&str) -> String) {
    once(env);
    let clean = clean(env);
    let Some(obj) = env.as_object_mut() else {
        rows(env, cursor);
        return;
    };
    obj.remove("knowcode");
    obj.remove("omitted");
    freshness(obj);
    provenance(obj);
    repo(obj, names);
    if let Some(Value::Array(repos)) = obj.get_mut("repos") {
        for r in repos.iter_mut().filter_map(Value::as_object_mut) {
            freshness(r);
            repo(r, names);
            if r.get("dirty") == Some(&Value::Bool(false)) {
                r.remove("dirty");
            }
        }
        // A repo's commit and dirty state are in `provenance`; only what its freshness says is its own.
        repos.retain(|r| r.get("freshness").is_some_and(|f| f.get("failed").is_some() || f.get("status").is_some()));
        if repos.is_empty() {
            obj.remove("repos");
        }
    }
    if clean {
        obj.remove("repos");
        obj.remove("provenance");
    }
    if let Some(Value::Array(list)) = obj.get_mut("provenance") {
        for p in list.iter_mut() {
            *p = Value::from(provenance_line(p, names));
        }
    }
    for key in ["incomplete", "more"] {
        if let Some(Value::Array(list)) = obj.get_mut(key) {
            for i in list.iter_mut() {
                if let Some(Value::String(h)) = i.get_mut("hint") {
                    *h = short_hint(h, None);
                }
            }
            one_stale(list, names);
        }
    }
    // The size cap's own hint: each cut list's `incomplete` or `more` entry says how to get the rest.
    if obj.get("hint").and_then(Value::as_str).is_some_and(|h| h.contains(" list items cut to fit max_chars="))
        && (obj.contains_key("incomplete") || obj.contains_key("more"))
    {
        obj.remove("hint");
    }
    // What the rows cut are: said in the entry for the cut list.
    obj.remove("rest");
    defaults(obj);
    rows(env, cursor);
}

/// Whether the answer is clean and current, so that its repos go without saying: no repo it read is dirty or was
/// answered from its last sync (not checked against its working tree), none had files it could not read, and nothing
/// in it is `stale`, `failed` or `missing_repo`. Read from the full envelope.
pub fn clean(env: &Value) -> bool {
    let dirty = |v: &Value| v["dirty"] == Value::Bool(true);
    let unread = |v: &Value| v["freshness"]["failed"].as_array().is_some_and(|f| !f.is_empty()) || v["freshness"]["status"].is_string();
    let list = |key: &str| env[key].as_array().cloned().unwrap_or_default();
    let matters = list("incomplete").iter().any(|i| matches!(i["reason"].as_str(), Some("stale" | "failed" | "missing_repo")));
    !matters
        && !dirty(env)
        && !unread(env)
        && list("repos").iter().all(|r| !dirty(r) && !unread(r))
        && list("provenance").iter().all(|p| !dirty(p) && p["fresh"] != Value::Bool(false))
}

/// The envelope without what goes without saying (see the module): false flags, nulls, a first page's `offset`, a
/// search's `stats`, and `matches` equal to `total`. `complete` stays.
fn defaults(obj: &mut Map<String, Value>) {
    obj.retain(|k, v| match (k.as_str(), &*v) {
        (_, Value::Null) => false,
        ("dirty" | "truncated" | "capped", Value::Bool(false)) => false,
        ("offset", n) => n.as_u64() != Some(0),
        ("stats", _) => false,
        _ => true,
    });
    if obj.contains_key("total") && obj.get("matches") == obj.get("total") {
        obj.remove("matches");
    }
}

/// The `stale` entries of `incomplete` said as one: the repos they name by their short name (not an absolute path),
/// and entries saying the same thing of different repos joined (`a, b: indexing is queued or running; …`), each hint
/// once. One entry per reason, as the contract has it.
fn one_stale(list: &mut Vec<Value>, names: &Names) {
    let at: Vec<usize> = (0..list.len()).filter(|&i| list[i]["reason"] == "stale").collect();
    let Some(&first) = at.first() else { return };
    // What each says, with the subjects saying it, in the order first said.
    let mut said: Vec<(String, Vec<String>)> = Vec::new();
    let mut hints: Vec<String> = Vec::new();
    for &i in &at {
        let detail = short_paths(list[i]["detail"].as_str().unwrap_or(""), names);
        let (subject, what) = match detail.split_once(": ") {
            Some((s, w)) => (s.to_string(), w.to_string()),
            None => (String::new(), detail.clone()),
        };
        match said.iter_mut().find(|(w, _)| *w == what) {
            Some((_, subjects)) => subjects.push(subject),
            None => said.push((what, vec![subject])),
        }
        if let Some(h) = list[i]["hint"].as_str()
            && !hints.iter().any(|x| x == h)
        {
            hints.push(h.to_string());
        }
    }
    let detail: Vec<String> = said
        .into_iter()
        .map(|(what, subjects)| {
            let subjects: Vec<String> = subjects.into_iter().filter(|s| !s.is_empty()).collect();
            if subjects.is_empty() { what } else { format!("{}: {what}", subjects.join(", ")) }
        })
        .collect();
    let mut one = Map::new();
    one.insert("reason".into(), Value::from("stale"));
    one.insert("detail".into(), Value::from(detail.join("; ")));
    if !hints.is_empty() {
        one.insert("hint".into(), Value::from(hints.join("; ")));
    }
    list[first] = Value::Object(one);
    for &i in at.iter().skip(1).rev() {
        list.remove(i);
    }
}

/// For an answer said as text (a tree): its `stale` entries said as one, as the compact JSON says them.
pub fn text_stale(env: &mut Value) {
    if let Some(Value::Array(list)) = env.get_mut("incomplete") {
        one_stale(list, &Names::load());
    }
}

/// `text` with every absolute path in it (a word starting with `/`, up to a space or one of `,;:)]`) said as
/// [`Names::key`] says a repo.
fn short_paths(text: &str, names: &Names) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('/') {
        let starts_word = i == 0 || rest[..i].ends_with([' ', '(', '[']);
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find(|c: char| c.is_whitespace() || ",;:)]".contains(c)).unwrap_or(tail.len());
        let word = &tail[..end];
        if starts_word && word.len() > 1 {
            out.push_str(&names.key(word));
        } else {
            out.push_str(word);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Freshness without its timing and zero counters; gone when nothing is left.
fn freshness(obj: &mut Map<String, Value>) {
    let Some(Value::Object(f)) = obj.get_mut("freshness") else { return };
    f.remove("check_ms");
    for k in ["reparsed", "removed"] {
        if f.get(k).and_then(Value::as_u64) == Some(0) {
            f.remove(k);
        }
    }
    if f.is_empty() {
        obj.remove("freshness");
    }
}

/// An object's `repo` (a path) by its key, and its `commit` cut to 12 digits.
fn repo(obj: &mut Map<String, Value>, names: &Names) {
    if let Some(Value::String(r)) = obj.get_mut("repo")
        && r.starts_with('/')
    {
        *r = names.key(r);
    }
    if let Some(Value::String(c)) = obj.get_mut("commit")
        && c.len() > 12
        && c.bytes().all(|b| b.is_ascii_hexdigit())
    {
        c.truncate(12);
    }
}

/// A provenance entry as one string: `billing @ 0123456789ab`, then ` (dirty)` and `, from its last sync` (not checked
/// against its working tree for this answer) when they apply, as the text rendering says it. The index generation is
/// left out (`--json` has it): the cursors carry it, and an agent has no use for it.
fn provenance_line(p: &Value, names: &Names) -> String {
    let repo = p["repo"].as_str().map(|r| names.key(r)).unwrap_or_default();
    let commit = p["commit"].as_str().map(|c| format!(" @ {}", &c[..c.len().min(12)])).unwrap_or_default();
    let dirty = if p["dirty"] == Value::Bool(true) { " (dirty)" } else { "" };
    let fresh = if p["fresh"] == Value::Bool(true) { "" } else { ", from its last sync" };
    format!("{repo}{commit}{dirty}{fresh}")
}

/// Drops the provenance entries that repeat the envelope's own repo (see the module).
fn provenance(obj: &mut Map<String, Value>) {
    let own = (obj.get("repo").cloned(), obj.get("commit").cloned(), obj.get("dirty").cloned());
    let Some(Value::Array(list)) = obj.get_mut("provenance") else { return };
    list.retain(|p| {
        let same = Some(&p["repo"]) == own.0.as_ref()
            && Some(&p["commit"]) == own.1.as_ref()
            && (p["dirty"] == Value::Null || Some(&p["dirty"]) == own.2.as_ref())
            && p["fresh"] == Value::Bool(true);
        !same
    });
    if list.is_empty() {
        obj.remove("provenance");
    }
}

/// Evidence as a string, cursors as handles, and no internal row ids, anywhere in `v`.
fn rows(v: &mut Value, cursor: &mut dyn FnMut(&str) -> String) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(|x| rows(x, cursor)),
        Value::Object(m) => {
            // A search's hits (its answer, or a page of it): one text row each.
            if let Some(Value::Array(items)) = m.get_mut("items")
                && let Some(lines) = items.iter().map(hit_row).collect::<Option<Vec<String>>>()
            {
                *items = lines.into_iter().map(Value::from).collect();
            }
            if let Some(e) = m.get("evidence").filter(|e| e.get("mechanism").is_some()) {
                let s = evidence(e);
                m.insert("evidence".into(), Value::from(s));
            }
            // A symbol's row id means nothing outside its index generation, and no tool takes one. A note's id is
            // how it is removed: kept.
            let symbol = m.contains_key("qualified") || (m.contains_key("kind") && m.contains_key("name"));
            if symbol && !m.contains_key("text") && m.get("id").is_some_and(Value::is_i64) {
                m.remove("id");
            }
            // Timings (a search's `stats.ms`, `stats.read_ms`, …), wherever they are: they change between two runs
            // of the same answer and no agent acts on them.
            m.retain(|k, x| !(x.is_number() && (k == "ms" || k.ends_with("_ms"))));
            lean_row(m);
            shared_targets(m);
            shared_link_fields(m);
            for (k, x) in m.iter_mut() {
                match x {
                    Value::String(s) if (k == "cursor" || k.ends_with("_cursor")) && s.starts_with("kc1.") => *s = cursor(s),
                    _ => rows(x, cursor),
                }
            }
        }
        _ => {}
    }
}

/// A row said in fewer characters: a reference's enclosing symbol (`from`) as the name kc_context takes
/// (`Checkout.Run`), without its kind; a reference's column left out (its line is the citation); a test file's
/// ranking `signals` left out (its `confidence` and `reasons` say them in words). `--json` keeps all three.
fn lean_row(m: &mut Map<String, Value>) {
    if let Some(q) = m.get("from").and_then(qualified) {
        m.insert("from".into(), q);
    }
    // A reference: its targets, or (they being said beside its list) its confidence and evidence.
    let reference = m.contains_key("targets") || (m.contains_key("confidence") && m.contains_key("evidence"));
    if reference && m.contains_key("line") {
        m.remove("col");
    }
    if m.contains_key("reasons") && m.contains_key("confidence") {
        m.remove("signals");
    }
    // A cite at the place the row names already (its site, else its own line) doesn't say it again; one left with
    // nothing to say goes.
    let place = match (m.get("file").and_then(Value::as_str), m.get("line").and_then(Value::as_u64)) {
        (Some(f), Some(l)) => Some(format!("{f}:{l}")),
        _ => None,
    };
    let site = m.get("site").and_then(Value::as_str).map(str::to_string).or(place);
    if let Some(Value::Object(c)) = m.get_mut("cite") {
        if c.get("at").and_then(Value::as_str).is_some_and(|at| Some(at) == site.as_deref()) {
            c.remove("at");
        }
        if c.is_empty() {
            m.remove("cite");
        }
    }
}

/// A search hit as one text row, `None` for any other row. The location first, `repo/` before the file when the
/// answer spans repos, and the line's text last, after the first ` | `:
///
/// - `kc_search`: `repo/file:line kind symbol | text`, the symbol a `def` or `decl` declares, or `in` the one
///   containing the line, with its parameter names on the first code hit inside it (`in Orders.Place(o)`); `[stale]`
///   for a file that could not be read (as last indexed); the lines around it (`context`, or the rest of its
///   statement) follow, one per line, `  line- text`;
/// - `kc_search` with `files_only`: `repo/file:line best (n matches)`, the line and kind of its best-ranked hit;
/// - `kc_grep_ast`: `repo/file:line:column[-end_line] [$X=captured, …] | text`.
pub fn hit_row(v: &Value) -> Option<String> {
    let m = v.as_object()?;
    let file = m.get("file")?.as_str()?;
    let line = m.get("line")?.as_u64()?;
    let repo = match m.get("repo") {
        Some(r) => format!("{}/", r.as_str()?),
        None => String::new(),
    };
    let stale = if m.get("stale") == Some(&Value::Bool(true)) { " [stale]" } else { "" };
    let only = |keys: &[&str]| m.keys().all(|k| keys.contains(&k.as_str()) || k == "repo" || k == "stale");
    let str_of = |key: &str| m.get(key).and_then(Value::as_str);
    // A search hit.
    if let (Some(kind), Some(text)) = (str_of("kind"), str_of("text"))
        && matches!(kind, "def" | "decl" | "code" | "test" | "comment" | "doc")
        && only(&["file", "line", "kind", "text", "symbol", "in", "in_params", "before", "after"])
    {
        let at = match (str_of("symbol"), str_of("in")) {
            (Some(s), _) => format!(" {s}"),
            (None, Some(s)) => format!(" in {s}{}", str_of("in_params").unwrap_or_default()),
            _ => String::new(),
        };
        let mut row = format!("{repo}{file}:{line} {kind}{at}{stale} | {text}");
        let lines = |key: &str| m.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>()).unwrap_or_default();
        let (before, after) = (lines("before"), lines("after"));
        let first = line.saturating_sub(before.len() as u64);
        let around = before.iter().enumerate().map(|(i, l)| (first + i as u64, l)).chain(after.iter().enumerate().map(|(i, l)| (line + 1 + i as u64, l)));
        for (n, l) in around {
            row.push_str(&format!("\n  {n}- {l}"));
        }
        return Some(row);
    }
    // A file of a `files_only` search.
    if let (Some(best), Some(n)) = (str_of("best"), m.get("matches").and_then(Value::as_u64))
        && only(&["file", "line", "best", "matches"])
    {
        return Some(format!("{repo}{file}:{line} {best}{stale} ({n} {})", if n == 1 { "match" } else { "matches" }));
    }
    // A structural match.
    if let (Some(col), Some(end), Some(text)) = (m.get("column").and_then(Value::as_u64), m.get("end_line").and_then(Value::as_u64), str_of("text"))
        && only(&["file", "line", "end_line", "column", "text", "vars"])
    {
        let end = if end > line { format!("-{end}") } else { String::new() };
        let vars: Vec<String> =
            m.get("vars").and_then(Value::as_object).map(|vs| vs.iter().map(|(k, x)| format!("${k}={}", x.as_str().unwrap_or_default())).collect()).unwrap_or_default();
        let vars = if vars.is_empty() { String::new() } else { format!(" [{}]", vars.join(", ")) };
        return Some(format!("{repo}{file}:{line}:{col}{end}{vars}{stale} | {text}"));
    }
    None
}

/// An enclosing symbol `{container, kind, name}` as its qualified name, `container.name`.
fn qualified(from: &Value) -> Option<Value> {
    let from = from.as_object()?;
    let name = from.get("name").and_then(Value::as_str)?;
    if !from.keys().all(|k| matches!(k.as_str(), "name" | "container" | "kind" | "id")) {
        return None;
    }
    Some(Value::from(match from.get("container").and_then(Value::as_str) {
        Some(c) if !c.is_empty() => format!("{c}.{name}"),
        _ => name.to_string(),
    }))
}

/// What every row of a list of references says alike, said once beside the list instead: the same `targets` (every
/// use of one name), the same `from` (a symbol's callees), the same `name`. The list's key names it:
/// `<list>_targets`, `<list>_from`, `<list>_name` (for `items`, plain `targets`, `from`, `name`).
fn shared_targets(m: &mut Map<String, Value>) {
    let refs = |x: &Value| x.as_array().is_some_and(|a| a.len() >= 2 && a.iter().all(|r| r.get("targets").is_some_and(Value::is_array)));
    let lists: Vec<String> = m.iter().filter(|(_, x)| refs(x)).map(|(k, _)| k.clone()).collect();
    for k in lists {
        for field in ["targets", "from", "name"] {
            let key = if k == "items" { field.to_string() } else { format!("{k}_{field}") };
            if m.contains_key(&key) {
                continue;
            }
            let Some(Value::Array(rows)) = m.get_mut(&k) else { continue };
            let Some(first) = rows[0].get(field).cloned() else { continue };
            if !rows.iter().all(|r| r.get(field) == Some(&first)) {
                continue;
            }
            for r in rows.iter_mut().filter_map(Value::as_object_mut) {
                r.remove(field);
            }
            let first = if field == "from" { qualified(&first).unwrap_or(first) } else { first };
            m.insert(key, first);
        }
    }
}

/// What every row of a list of links (`links`: the code reaching an endpoint over HTTP, the bus or SQL) says alike,
/// said once beside it: the same `repo`, `confidence` or `assumptions`, as `links_repo`, … A row naming only its
/// place and symbol is then a text row, `repo/file:line kind symbol [confidence; assumption, …]` (what isn't said
/// beside the list). Forty callers of one endpoint then fit where ten did.
fn shared_link_fields(m: &mut Map<String, Value>) {
    let Some(Value::Array(rows)) = m.get_mut("links") else { return };
    if rows.len() < 2 || !rows.iter().all(Value::is_object) {
        return;
    }
    let mut shared = Vec::new();
    for field in ["repo", "confidence", "assumptions"] {
        let Some(first) = rows[0].get(field).cloned() else { continue };
        if rows.iter().all(|r| r.get(field) == Some(&first)) {
            rows.iter_mut().filter_map(Value::as_object_mut).for_each(|r| {
                r.remove(field);
            });
            shared.push((format!("links_{field}"), first));
        }
    }
    if let Some(lines) = rows.iter().map(link_row).collect::<Option<Vec<String>>>() {
        *rows = lines.into_iter().map(Value::from).collect();
    }
    for (k, v) in shared {
        m.entry(k).or_insert(v);
    }
}

/// A link row naming only its place and symbol as one text row (see [`shared_link_fields`]); `None` for any other.
fn link_row(v: &Value) -> Option<String> {
    let m = v.as_object()?;
    if !m.keys().all(|k| matches!(k.as_str(), "repo" | "file" | "line" | "kind" | "symbol" | "confidence" | "assumptions" | "local")) {
        return None;
    }
    let s = |k: &str| m.get(k).and_then(Value::as_str);
    let repo = s("repo").map(|r| format!("{r}/")).unwrap_or_default();
    let mut row = format!("{repo}{}:{}", s("file")?, m.get("line")?.as_u64()?);
    for k in ["kind", "symbol"] {
        if let Some(x) = s(k) {
            row.push(' ');
            row.push_str(x);
        }
    }
    let assumptions: Vec<&str> = m.get("assumptions").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let said: Vec<String> = s("confidence")
        .map(str::to_string)
        .into_iter()
        .chain((!assumptions.is_empty()).then(|| assumptions.join(", ")))
        .chain(s("local").map(|l| format!("URL from {l}")))
        .collect();
    if !said.is_empty() {
        row.push_str(&format!(" [{}]", said.join("; ")));
    }
    Some(row)
}

/// `mechanism`, or `mechanism [assumption, …]`.
fn evidence(e: &Value) -> String {
    let m = e["mechanism"].as_str().unwrap_or("");
    let a: Vec<&str> = e["assumptions"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    if a.is_empty() { m.to_string() } else { format!("{m} [{}]", a.join(", ")) }
}

/// A continuation hint without its CLI form; with `value`, naming the cursor itself rather than its field.
fn short_hint(h: &str, value: Option<&str>) -> String {
    let Some(start) = h.find("continue: kc_more {cursor: <`") else { return h.to_string() };
    let rest = &h[start + "continue: kc_more {cursor: <`".len()..];
    let Some(end) = rest.find("`>}") else { return h.to_string() };
    let field = &rest[..end];
    let mut tail = &rest[end + "`>}".len()..];
    let cli = format!("; CLI: knowcode more <`{field}`>");
    if let Some(t) = tail.strip_prefix(cli.as_str()) {
        tail = t;
    }
    let call = match value {
        Some(v) => format!("kc_more {{cursor: \"{v}\"}}"),
        None => format!("kc_more {{cursor: <`{field}`>}}"),
    };
    format!("{}{call}{tail}", &h[..start])
}

/// The value at a readable field (`items[0].affected_cursor`).
fn at<'a>(env: &'a Value, field: &str) -> Option<&'a Value> {
    let mut v = env;
    for part in field.split('.') {
        let (key, idx) = match part.find('[') {
            Some(i) => (&part[..i], Some(&part[i..])),
            None => (part, None),
        };
        if !key.is_empty() {
            v = v.get(key)?;
        }
        if let Some(idx) = idx {
            for n in idx.split(['[', ']']).filter(|s| !s.is_empty()) {
                v = v.get(n.parse::<usize>().ok()?)?;
            }
        }
    }
    Some(v)
}

/// For an answer said as text (a tree), which shows no cursor fields: its `incomplete` hints name each cursor's
/// handle, so the text alone can be continued.
pub fn text_hints(env: &mut Value) {
    let snapshot = env.clone();
    for key in ["incomplete", "more"] {
        let Some(Value::Array(list)) = env.get_mut(key) else { continue };
        for i in list.iter_mut() {
            let Some(Value::String(h)) = i.get_mut("hint") else { continue };
            let field = h.split("<`").nth(1).and_then(|r| r.split("`>").next()).map(str::to_string);
            let value = field.as_deref().and_then(|f| at(&snapshot, f)).and_then(Value::as_str).filter(|c| c.starts_with("kc1."));
            *h = short_hint(h, value.map(handle).as_deref());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_compact_form_keeps_the_contract_and_drops_repetition() {
        let env = json!({
            "repo": "/long/path/to/shop", "commit": "abc", "dirty": false,
            "freshness": { "check_ms": 1.2, "reparsed": 0, "removed": 0 },
            "knowcode": { "build": "x", "schema": 1, "version": "0" },
            "provenance": [
                { "repo": "/long/path/to/shop", "commit": "abc", "dirty": false, "fresh": true, "generation": "g" },
                { "repo": "/long/path/to/billing", "commit": "def", "dirty": null, "fresh": false, "generation": "h" },
            ],
            "complete": false,
            "incomplete": [{ "reason": "budget", "detail": "/items: 3 items cut to fit max_chars",
                             "hint": "continue: kc_more {cursor: <`cursor`>}; CLI: knowcode more <`cursor`>" }],
            "hint": "3 list items cut to fit max_chars=1000; pass max_chars (up to 40000) or a smaller limit",
            "cursor": "kc1.abcdef",
            "stats": { "candidates": 4, "ms": 3.1, "read_ms": 0.4 },
            "deploy": { "repo": "shop", "ansible": { "own": ["playbook deploy.yml"] } },
            "items": [{ "id": 7, "qualified": "A.b", "kind": "method", "file": "a.cs", "line": 1,
                        "evidence": { "mechanism": "receiver_type", "assumptions": ["project_unverified"] },
                        "from": { "id": 3, "kind": "method", "name": "Run", "container": "C" },
                        "targets": [{ "id": 2, "qualified": "X.y", "evidence": { "mechanism": "scip" } }] },
                      { "id": 9, "symbol": "A.b", "kind": "method", "text": "a note keeps its id" },
                      { "id": 1, "name": "orders", "dir": "src/orders" }],
        });
        let names = Names::default();
        let v: Value = serde_json::from_str(&output(&env, &names)).unwrap();
        // The contract: unchanged.
        assert_eq!((v["complete"].clone(), v["incomplete"][0]["reason"].clone()), (json!(false), json!("budget")));
        assert_eq!(v["incomplete"][0]["hint"], "kc_more {cursor: <`cursor`>}");
        // The queried repo's own entry is dropped; another keeps everything, its repo named by its key.
        assert_eq!(v["provenance"], json!(["billing @ def, from its last sync"]));
        assert_eq!(v["repo"], "shop");
        assert!(v.get("knowcode").is_none() && v.get("freshness").is_none() && v.get("hint").is_none(), "{v}");
        // A search's stats are dropped; `deploy` is kept unless it repeats the first item.
        assert!(v.get("stats").is_none(), "{v}");
        assert_eq!(v["deploy"]["repo"], "shop");
        let mut twice = json!({ "deploy": { "repo": "shop", "ansible": {} }, "items": [{ "repo": "shop", "ansible": {}, "own_cursor": "kc1.x" }] });
        once(&mut twice);
        assert!(twice.get("deploy").is_none() && twice["items"][0]["own_cursor"] == "kc1.x", "{twice}");
        // A name two indexed repos share keeps its path; a long commit is cut to 12 digits.
        let shared = Names(["shop".to_string()].into());
        let mut env2 = env.clone();
        env2["commit"] = json!("0123456789abcdef0123456789abcdef01234567");
        let v2: Value = serde_json::from_str(&output(&env2, &shared)).unwrap();
        assert_eq!((v2["repo"].as_str(), v2["commit"].as_str()), (Some("/long/path/to/shop"), Some("0123456789ab")));
        // Rows: evidence as a string, no symbol ids; a note's and an area's ids stay.
        assert_eq!(v["items"][0]["evidence"], "receiver_type [project_unverified]");
        assert_eq!(v["items"][0]["targets"][0]["evidence"], "scip");
        assert!(v["items"][0].get("id").is_none() && v["items"][0]["from"].get("id").is_none());
        assert_eq!((v["items"][1]["id"].clone(), v["items"][2]["id"].clone()), (json!(9), json!(1)));
        // The cursor is a handle that resolves to the full cursor; a full cursor resolves to itself.
        let h = v["cursor"].as_str().unwrap();
        assert!(h.starts_with(HANDLE) && h.len() <= PLACEHOLDER.len(), "{h}");
        assert_eq!(resolve(h).unwrap(), "kc1.abcdef");
        assert_eq!(resolve("kc1.xyz").unwrap(), "kc1.xyz");
        assert!(resolve("kc~ffffffff").unwrap_err().contains("ask the original question again"));
        // Measured never shorter than output: a cut to fit stays within the cap.
        assert!(measure(&env, &names).len() >= output(&env, &names).len());
    }

    /// Lean rows lose nothing an agent acts on: `from` is the name kc_context takes, the column and ranking signals go,
    /// and what every row of a reference list says alike is said once beside the list, never over a key the answer
    /// already has.
    #[test]
    fn reference_rows_say_what_they_share_once() {
        let row = |file: &str, line: u32, from: &str| {
            json!({ "file": file, "line": line, "col": 9, "kind": "call", "name": "Log", "confidence": "exact",
                    "evidence": { "mechanism": "receiver_type" },
                    "from": { "id": 4, "container": from, "kind": "method", "name": "Run" },
                    "targets": [{ "id": 1, "qualified": "Audit.Log", "file": "a.cs", "line": 3 }] })
        };
        let env = json!({ "repo": "shop", "items": [row("b.cs", 5, "B"), row("c.cs", 7, "C")], "total": 2, "truncated": false });
        let v: Value = serde_json::from_str(&output(&env, &Names::default())).unwrap();
        assert_eq!(v["targets"], json!([{ "qualified": "Audit.Log", "file": "a.cs", "line": 3 }]), "{v}");
        assert_eq!(v["name"], "Log");
        assert!(v.get("from").is_none(), "rows differ in `from`: kept in each: {v}");
        for (r, from) in v["items"].as_array().unwrap().iter().zip(["B.Run", "C.Run"]) {
            assert_eq!(r["from"], from);
            assert!(r.get("targets").is_none() && r.get("name").is_none() && r.get("col").is_none(), "{r}");
            assert_eq!((r["line"].is_u64(), r["confidence"].as_str(), r["evidence"].as_str()), (true, Some("exact"), Some("receiver_type")));
        }
        // A symbol's callees: one `from`, said beside the list; targets that differ stay in each row.
        let mut callees = vec![row("b.cs", 5, "B"), row("b.cs", 6, "B")];
        callees[1]["targets"][0]["qualified"] = json!("Audit.Flush");
        let v: Value = serde_json::from_str(&output(&json!({ "items": [{ "callees": callees }] }), &Names::default())).unwrap();
        let item = &v["items"][0];
        assert_eq!(item["callees_from"], "B.Run", "{v}");
        assert!(item.get("callees_targets").is_none() && item["callees"][1]["targets"][0]["qualified"] == "Audit.Flush", "{v}");
        // A key the answer has is never overwritten: the rows keep theirs.
        let env = json!({ "name": "query", "items": [row("b.cs", 5, "B"), row("c.cs", 7, "C")] });
        let v: Value = serde_json::from_str(&output(&env, &Names::default())).unwrap();
        assert_eq!((v["name"].as_str(), v["items"][0]["name"].as_str()), (Some("query"), Some("Log")), "{v}");
        // One row: nothing to share. Rows that aren't references (no targets) are left as they are.
        let v: Value = serde_json::from_str(&output(&json!({ "items": [row("b.cs", 5, "B")] }), &Names::default())).unwrap();
        assert!(v["items"][0]["targets"].is_array() && v.get("targets").is_none(), "{v}");
        let defs = json!({ "items": [{ "name": "Log", "file": "a.cs", "line": 3, "col": 5 }, { "name": "Log", "file": "b.cs", "line": 4, "col": 5 }] });
        let v: Value = serde_json::from_str(&output(&defs, &Names::default())).unwrap();
        assert_eq!(v["items"][0]["col"], 5, "{v}");
        assert!(v.get("name").is_none(), "{v}");
        // A test file's ranking signals go; its confidence and reasons stay.
        let tests = json!({ "items": [{ "tests": [{ "file": "t.cs", "confidence": 0.9, "reasons": ["calls Audit.Log at hop 1"], "signals": { "hop": 1 } }] }] });
        let v: Value = serde_json::from_str(&output(&tests, &Names::default())).unwrap();
        let t = &v["items"][0]["tests"][0];
        assert!(t.get("signals").is_none() && t["confidence"] == 0.9 && t["reasons"][0] == "calls Audit.Log at hop 1", "{v}");
        // Measured never shorter than output.
        assert!(measure(&env, &Names::default()).len() >= output(&env, &Names::default()).len());
    }

    /// A workspace answer's bookkeeping, as the server builds it: three repos, each with its provenance entry.
    fn workspace(dirty: &str, fresh: bool) -> Value {
        let repos = ["/w/audit", "/w/billing", "/w/shop"];
        json!({
            "repo": "workspace", "commit": null, "dirty": repos.contains(&dirty),
            "repos": repos.iter().map(|r| json!({ "repo": r, "commit": "0123456789abcdef", "dirty": *r == dirty, "freshness": { "check_ms": 0.4, "reparsed": 0, "removed": 0 } })).collect::<Vec<_>>(),
            "provenance": repos.iter().map(|r| json!({ "repo": r, "commit": "0123456789abcdef", "generation": "g", "dirty": *r == dirty, "fresh": fresh })).collect::<Vec<_>>(),
            "items": [], "total": 0, "truncated": false, "offset": 0, "next_offset": null, "complete": true,
            "matches": 0, "stats": { "searchable_files": 9, "candidates": 0 },
        })
    }

    /// A clean, current answer says nothing about its repos or the defaults, only that it is complete; one with a dirty or unchecked repo, or a
    /// `stale`, `failed` or `missing_repo` entry, keeps its provenance. Never what the contract says when it is partial.
    #[test]
    fn a_clean_answer_leaves_out_its_repos_and_defaults_and_a_dirty_one_keeps_them() {
        let names = Names::default();
        let v: Value = serde_json::from_str(&output(&workspace("", true), &names)).unwrap();
        assert_eq!(v, json!({ "repo": "workspace", "items": [], "total": 0, "complete": true }));
        // Dirty: provenance says which, as strings; `repos` adds nothing to it.
        let v: Value = serde_json::from_str(&output(&workspace("/w/shop", true), &names)).unwrap();
        assert_eq!(v["provenance"], json!(["audit @ 0123456789ab", "billing @ 0123456789ab", "shop @ 0123456789ab (dirty)"]), "{v}");
        assert_eq!((v["dirty"].as_bool(), v.get("repos")), (Some(true), None), "{v}");
        // Unchecked against the working tree: kept.
        let v: Value = serde_json::from_str(&output(&workspace("", false), &names)).unwrap();
        assert!(v["provenance"][0].as_str().unwrap().ends_with("from its last sync"), "{v}");
        // A repo's unread files: its `repos` entry stays, with them; `complete: false` and `incomplete` always stay.
        let mut env = workspace("", true);
        env["repos"][1]["freshness"] = json!({ "failed": ["a.cs"], "status": "incomplete" });
        env["complete"] = json!(false);
        env["incomplete"] = json!([{ "reason": "failed", "detail": "billing: could not read a.cs" }]);
        let v: Value = serde_json::from_str(&output(&env, &names)).unwrap();
        assert_eq!(v["repos"], json!([{ "repo": "billing", "commit": "0123456789ab", "freshness": { "failed": ["a.cs"], "status": "incomplete" } }]), "{v}");
        assert_eq!((v["complete"].as_bool(), v["provenance"].as_array().map(Vec::len)), (Some(false), Some(3)), "{v}");
        // A list cut to fit doesn't make the repos matter; a missing repo does.
        let mut env = workspace("", true);
        env["complete"] = json!(false);
        env["incomplete"] = json!([{ "reason": "capped", "detail": "items: 3 of 9 shown" }]);
        let v: Value = serde_json::from_str(&output(&env, &names)).unwrap();
        assert!(v.get("provenance").is_none() && v["complete"] == false, "{v}");
        env["incomplete"] = json!([{ "reason": "missing_repo", "detail": "gone: indexed, but its checkout is gone" }]);
        let v: Value = serde_json::from_str(&output(&env, &names)).unwrap();
        assert!(v["provenance"].is_array(), "{v}");
        // Matches that aren't the total, and a later page's offset, are said; so is a true flag.
        let mut env = workspace("", true);
        env["matches"] = json!(40);
        env["offset"] = json!(20);
        env["truncated"] = json!(true);
        let v: Value = serde_json::from_str(&output(&env, &names)).unwrap();
        assert_eq!((v["matches"].as_u64(), v["offset"].as_u64(), v["truncated"].as_bool()), (Some(40), Some(20), Some(true)), "{v}");
    }

    /// Staleness is one entry: repos by name, not path, entries saying the same thing of several repos joined, each
    /// hint once; an answer with one stale entry keeps it, its paths shortened.
    #[test]
    fn stale_entries_are_said_once_by_repo_name() {
        let pending = |r: &str| {
            json!({ "reason": "stale", "detail": format!("/Users/me/src/{r}: indexing is queued or running; this answer uses the available committed snapshot"),
                    "hint": "indexing continues in the background; ask again after it finishes" })
        };
        let env = json!({
            "repo": "workspace", "complete": false, "items": [],
            "incomplete": [pending("shop"), { "reason": "capped", "detail": "items: 3 of 9 shown" }, pending("billing"),
                           { "reason": "stale", "detail": "workspace summary: audit changed since last synced", "hint": "index them" }],
        });
        let v: Value = serde_json::from_str(&output(&env, &Names::default())).unwrap();
        let list = v["incomplete"].as_array().unwrap();
        assert_eq!(list.len(), 2, "{v}");
        assert_eq!(list[0]["reason"], "stale");
        assert_eq!(
            list[0]["detail"],
            "shop, billing: indexing is queued or running; this answer uses the available committed snapshot; workspace summary: audit changed since last synced"
        );
        assert_eq!(list[0]["hint"], "indexing continues in the background; ask again after it finishes; index them");
        assert_eq!(list[1]["reason"], "capped");
        // One entry: as it was, without the path. A name two repos share keeps its path.
        let one = json!({ "incomplete": [pending("shop")], "complete": false });
        let v: Value = serde_json::from_str(&output(&one, &Names::default())).unwrap();
        assert!(v["incomplete"][0]["detail"].as_str().unwrap().starts_with("shop: indexing is queued"), "{v}");
        let v: Value = serde_json::from_str(&output(&one, &Names(["shop".to_string()].into()))).unwrap();
        assert!(v["incomplete"][0]["detail"].as_str().unwrap().starts_with("/Users/me/src/shop: "), "{v}");
        assert_eq!(short_paths("a/b and (/x/y), c:/z", &Names::default()), "a/b and (y), c:/z");
    }

    /// Search hits are text rows: location, kind, symbol, then the line after ` | `; the lines around a hit follow it;
    /// a files-only search and a structural search have rows of their own. Any other row stays an object, and so does
    /// a list mixing hits with other rows.
    #[test]
    fn search_hits_are_text_rows() {
        let env = json!({
            "repo": "workspace", "total": 4, "truncated": true, "cursor": "kc1.page",
            "items": [
                { "file": "src/Audit.cs", "line": 3, "kind": "def", "repo": "audit", "symbol": "Audit.Log", "text": "public void Log(string m) { }" },
                { "file": "src/Orders.cs", "line": 6, "kind": "code", "repo": "shop", "in": "Orders.Place", "in_params": "(o)",
                  "text": "_audit.Log(\"placed | ok\");", "before": ["// Log it"], "after": ["Validate(o);", "}"] },
                { "file": "README.md", "line": 2, "kind": "doc", "repo": "billing", "text": "Logged through Audit.Log.", "stale": true },
                { "file": "a.cs", "line": 1, "kind": "comment", "text": "// Log" },
            ],
        });
        let v: Value = serde_json::from_str(&output(&env, &Names::default())).unwrap();
        assert_eq!(
            v["items"],
            json!([
                "audit/src/Audit.cs:3 def Audit.Log | public void Log(string m) { }",
                "shop/src/Orders.cs:6 code in Orders.Place(o) | _audit.Log(\"placed | ok\");\n  5- // Log it\n  7- Validate(o);\n  8- }",
                "billing/README.md:2 doc [stale] | Logged through Audit.Log.",
                "a.cs:1 comment | // Log",
            ]),
            "{v}"
        );
        assert!(v["cursor"].as_str().unwrap().starts_with(HANDLE), "{v}");
        // A page of them, as kc_more answers it.
        let page = json!({ "list": "items", "offset": 20, "total": 21, "items": [{ "file": "a.cs", "line": 9, "kind": "test", "text": "Log();" }] });
        let v: Value = serde_json::from_str(&output(&page, &Names::default())).unwrap();
        assert_eq!((v["items"][0].as_str(), v["offset"].as_u64()), (Some("a.cs:9 test | Log();"), Some(20)), "{v}");
        // Files only, and structural matches.
        let files = json!({ "items": [{ "file": "a.cs", "line": 4, "best": "def", "matches": 2 }, { "file": "b.cs", "line": 1, "best": "doc", "matches": 1, "repo": "r" }] });
        let v: Value = serde_json::from_str(&output(&files, &Names::default())).unwrap();
        assert_eq!(v["items"], json!(["a.cs:4 def (2 matches)", "r/b.cs:1 doc (1 match)"]), "{v}");
        let ast = json!({ "items": [
            { "file": "a.cs", "line": 3, "end_line": 3, "column": 26, "text": "new Audit().Log(\"x\")", "vars": { "A": "new Audit()", "B": "\"x\"" } },
            { "file": "b.cs", "line": 5, "end_line": 7, "column": 9, "text": "Run(", "repo": "shop" },
        ] });
        let v: Value = serde_json::from_str(&output(&ast, &Names::default())).unwrap();
        assert_eq!(v["items"], json!(["a.cs:3:26 [$A=new Audit(), $B=\"x\"] | new Audit().Log(\"x\")", "shop/b.cs:5:9-7 | Run("]), "{v}");
        // Not hits: left as they are.
        let find = json!({ "items": [{ "file": "a.cs", "line": 3, "kind": "method", "name": "Log", "text": "x" }] });
        let v: Value = serde_json::from_str(&output(&find, &Names::default())).unwrap();
        assert!(v["items"][0].is_object(), "{v}");
        let mixed = json!({ "items": [{ "file": "a.cs", "line": 1, "kind": "code", "text": "x" }, { "file": "a.cs", "line": 2, "kind": "code", "text": "y", "id": 4 }] });
        let v: Value = serde_json::from_str(&output(&mixed, &Names::default())).unwrap();
        assert!(v["items"][0].is_object() && v["items"][1].is_object(), "{v}");
        // Measured never shorter than output.
        assert!(measure(&env, &Names::default()).len() >= output(&env, &Names::default()).len());
    }

    #[test]
    fn a_text_answer_names_its_cursors_by_handle() {
        let mut env = json!({
            "items": [{ "affected_cursor": "kc1.zzz" }],
            "incomplete": [{ "reason": "budget", "detail": "/items/0/affected: 5 items cut",
                             "hint": "continue: kc_more {cursor: <`items[0].affected_cursor`>}; CLI: knowcode more <`items[0].affected_cursor`>" }],
        });
        text_hints(&mut env);
        let h = env["incomplete"][0]["hint"].as_str().unwrap();
        let handle = h.strip_prefix("kc_more {cursor: \"").and_then(|r| r.strip_suffix("\"}")).unwrap();
        assert_eq!(resolve(handle).unwrap(), "kc1.zzz");
    }
}
