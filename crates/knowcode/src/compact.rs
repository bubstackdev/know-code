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
//! - a repo's deploy answer is said once, at `items[0]` (the full form repeats it as `deploy`; see [`once`]).

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
        }
    }
    if let Some(Value::Array(list)) = obj.get_mut("provenance") {
        for p in list.iter_mut() {
            *p = Value::from(provenance_line(p, names));
        }
    }
    if let Some(Value::Array(list)) = obj.get_mut("incomplete") {
        for i in list.iter_mut() {
            if let Some(Value::String(h)) = i.get_mut("hint") {
                *h = short_hint(h, None);
            }
        }
    }
    // The size cap's own hint: each cut list's `incomplete` entry says how to get the rest.
    if obj.get("hint").and_then(Value::as_str).is_some_and(|h| h.contains(" list items cut to fit max_chars=")) && obj.contains_key("incomplete") {
        obj.remove("hint");
    }
    rows(env, cursor);
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
    let Some(Value::Array(list)) = env.get_mut("incomplete") else { return };
    for i in list.iter_mut() {
        let Some(Value::String(h)) = i.get_mut("hint") else { continue };
        let field = h.split("<`").nth(1).and_then(|r| r.split("`>").next()).map(str::to_string);
        let value = field.as_deref().and_then(|f| at(&snapshot, f)).and_then(Value::as_str).filter(|c| c.starts_with("kc1."));
        *h = short_hint(h, value.map(handle).as_deref());
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
        // Timings anywhere are dropped, counts kept; `deploy` is kept unless it repeats the first item.
        assert_eq!(v["stats"], json!({ "candidates": 4 }));
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
