//! What an MCP answer in JSON says about its own completeness and size, apart from `complete`: whether the graph it
//! read was whole, whether the source it quotes is, and whether its lists continue (`completeness`); and what was
//! asked and returned, the source mode in effect, and what each continuation would cost (`size`). Said only when
//! something is partial, so a whole answer is as it was, and only in the room the size cap left: what doesn't fit is
//! left out, the continuations first, then `size`, never a row of the answer. Text answers (trees) say it in their
//! header lines already.

use serde_json::{Map, Value, json};

/// The reasons that say the graph an answer read was partial, rather than a list of it cut.
const GRAPH: &[&str] = &["deadline", "stale", "failed", "missing_repo", "unsupported", "policy", "withheld", "depth"];

/// The reasons that say a list was cut: by a limit or to fit the answer.
const LISTS: &[&str] = &["capped", "budget"];

/// `text` (a compact JSON answer capped at `max` characters) with `completeness` and `size` added, when it is partial
/// in any of the three ways and they fit in `max`; `None` when it is whole, isn't a JSON object, or nothing fits.
pub fn add(text: &str, max: usize) -> Option<String> {
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(text) else { return None };
    let entries: Vec<&Value> = ["incomplete", "more"].iter().filter_map(|k| obj.get(*k)?.as_array()).flatten().collect();
    let reason = |e: &&Value| e["reason"].as_str().unwrap_or("").to_string();
    // What was asked: only `incomplete` speaks for the graph (`more` is lists on the side).
    let graph = !obj.get("incomplete").and_then(Value::as_array).into_iter().flatten().any(|e| GRAPH.contains(&reason(&e).as_str()));
    let mut continues = Vec::new();
    continuations(&Value::Object(obj.clone()), "", &mut continues);
    let lists = continues.is_empty() && !entries.iter().any(|e| LISTS.contains(&reason(e).as_str())) && obj.get("truncated") != Some(&Value::Bool(true));
    let modes = source_modes(&Value::Object(obj.clone()));
    let source_cut = entries.iter().any(|e| e["detail"].as_str().is_some_and(|d| d.contains("source")));
    let source = !source_cut && modes.iter().all(|m| m == "full" || m == "none");
    if graph && lists && source {
        return None;
    }
    obj.insert("completeness".into(), json!({ "graph": graph, "source": source, "lists": lists }));
    let mut size = Map::new();
    size.insert("max_chars".into(), Value::from(max));
    if let Some(mode) = modes.iter().find(|m| *m != "full").or(modes.first()) {
        size.insert("source".into(), Value::from(mode.as_str()));
    }
    if !continues.is_empty() {
        size.insert("continue".into(), Value::Array(continues));
    }
    size.insert("chars".into(), Value::from(0));
    // As much as fits: all of it, then without the continuations, then `completeness` alone.
    let mut out = Value::Object(obj);
    for level in 0..3 {
        match level {
            0 => out["size"] = Value::Object(size.clone()),
            1 if size.contains_key("continue") => {
                out["size"].as_object_mut().map(|s| s.remove("continue"));
            }
            1 => continue,
            _ => {
                out.as_object_mut().map(|o| o.remove("size"));
            }
        }
        let text = restate(out.to_string());
        if text.len() <= max {
            return Some(text);
        }
    }
    None
}

/// `text` with its `size.chars` said again after a rewrite changed its length (the `kc` tool's names for calls).
pub fn restate(text: String) -> String {
    const KEY: &str = "\"size\":{\"chars\":";
    let Some(at) = text.find(KEY).map(|i| i + KEY.len()) else { return text };
    let end = at + text[at..].find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    let (head, tail) = (&text[..at], &text[end..]);
    let base = head.len() + tail.len();
    // Its own digits count too.
    let mut n = base;
    while n != base + n.to_string().len() {
        n = base + n.to_string().len();
    }
    format!("{head}{n}{tail}")
}

/// Each list that continues: its path, its cursor, the rows left and about how many characters they would take,
/// at the rate of the rows shown.
fn continuations(v: &Value, path: &str, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            for (k, c) in m {
                let Some(cursor) = c.as_str() else { continue };
                let list = match k.as_str() {
                    "cursor" => "items",
                    k => match k.strip_suffix("_cursor") {
                        Some(l) => l,
                        None => continue,
                    },
                };
                let rows = m.get(list).and_then(Value::as_array);
                let total = if list == "items" { m.get("total") } else { m.get(&format!("{list}_total")).or_else(|| m.get(&format!("{list}_summary")).and_then(|s| s.get("total"))) };
                let mut e = json!({ "list": if path.is_empty() { list.to_string() } else { format!("{path}.{list}") }, "cursor": cursor });
                if let (Some(rows), Some(total)) = (rows, total.and_then(Value::as_u64)) {
                    let rest = total.saturating_sub(rows.len() as u64);
                    e["rest"] = Value::from(rest);
                    if !rows.is_empty() {
                        let per = Value::Array(rows.clone()).to_string().len() as u64 / rows.len() as u64;
                        e["chars"] = Value::from(rest * per);
                    }
                }
                out.push(e);
            }
            for (k, c) in m {
                if matches!(c, Value::Object(_) | Value::Array(_)) && !matches!(k.as_str(), "incomplete" | "more") {
                    continuations(c, &if path.is_empty() { k.clone() } else { format!("{path}.{k}") }, out);
                }
            }
        }
        Value::Array(a) => {
            for (i, c) in a.iter().enumerate() {
                if c.is_object() {
                    continuations(c, &format!("{path}[{i}]"), out);
                }
            }
        }
        _ => {}
    }
}

/// The source modes the answer's items quote in (`full`, `skeleton`, `none`), `cut` for a source cut short.
fn source_modes(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in v["items"].as_array().into_iter().flatten() {
        let mode = if item["source_truncated"] == Value::Bool(true) {
            Some("cut")
        } else {
            item["source_mode"].as_str()
        };
        if let Some(m) = mode
            && !out.iter().any(|o| o == m)
        {
            out.push(m.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_answer_says_nothing_more() {
        let whole = json!({ "complete": true, "items": [{ "a": 1 }], "total": 1 }).to_string();
        assert_eq!(add(&whole, 8000), None);
        let full = json!({ "complete": true, "items": [{ "source_mode": "full" }], "total": 1 }).to_string();
        assert_eq!(add(&full, 8000), None);
    }

    #[test]
    fn a_cut_list_says_how_to_continue_and_what_it_costs() {
        let rows: Vec<Value> = (0..4).map(|i| json!({ "file": "src/a.cs", "line": i })).collect();
        let text = json!({
            "complete": false, "cursor": "kc~1", "items": rows, "total": 10, "truncated": true,
            "incomplete": [{ "reason": "capped", "detail": "items: 4 of 10 shown" }],
        })
        .to_string();
        let v: Value = serde_json::from_str(&add(&text, 8000).unwrap()).unwrap();
        assert_eq!(v["completeness"], json!({ "graph": true, "source": true, "lists": false }));
        let c = &v["size"]["continue"][0];
        assert_eq!((c["list"].as_str(), c["cursor"].as_str(), c["rest"].as_u64()), (Some("items"), Some("kc~1"), Some(6)));
        let per = Value::Array(v["items"].as_array().unwrap().clone()).to_string().len() as u64 / 4;
        assert_eq!(c["chars"].as_u64(), Some(6 * per));
        assert_eq!(v["size"]["max_chars"], 8000);
        assert_eq!(v["size"]["chars"].as_u64(), Some(v.to_string().len() as u64));
    }

    #[test]
    fn the_summary_takes_only_the_room_left() {
        let text = json!({ "complete": false, "cursor": "kc~1", "items": [1], "total": 2 }).to_string();
        // Measured at a cap of as many digits as those below.
        let whole = add(&text, 999).unwrap();
        for (max, has) in [(whole.len(), (true, true)), (whole.len() - 1, (true, false)), (text.len() + 60, (false, false))] {
            let v: Value = serde_json::from_str(&add(&text, max).unwrap()).unwrap();
            assert_eq!((v.get("size").is_some(), v["size"].get("continue").is_some()), has, "{max}: {v}");
            assert!(v.to_string().len() <= max && v["completeness"]["lists"] == false, "{max}: {v}");
        }
        assert_eq!(add(&text, text.len() + 5), None);
    }

    #[test]
    fn a_rewritten_answer_says_its_length_again() {
        let text = add(&json!({ "complete": false, "cursor": "kc~1", "items": [1], "total": 2 }).to_string(), 8000).unwrap();
        let longer = restate(text.replace("kc~1", "a much longer handle"));
        let v: Value = serde_json::from_str(&longer).unwrap();
        assert_eq!(v["size"]["chars"].as_u64(), Some(longer.len() as u64));
    }

    #[test]
    fn graph_source_and_lists_are_said_apart() {
        // A walk stopped at its depth: the graph is partial, its lists whole.
        let depth = json!({ "complete": false, "items": [], "total": 0, "incomplete": [{ "reason": "depth", "detail": "x" }] }).to_string();
        let v: Value = serde_json::from_str(&add(&depth, 8000).unwrap()).unwrap();
        assert_eq!(v["completeness"], json!({ "graph": false, "source": true, "lists": true }));
        // A context bundle quoting a skeleton, its callers continuing: what was asked is complete.
        let ctx = json!({
            "complete": true, "total": 1,
            "items": [{ "source_mode": "skeleton", "callers": [{ "a": 1 }], "callers_total": 3, "callers_cursor": "kc~2" }],
            "more": [{ "reason": "capped", "detail": "items[0].callers: 1 of 3 shown" }],
        })
        .to_string();
        let v: Value = serde_json::from_str(&add(&ctx, 6000).unwrap()).unwrap();
        assert_eq!(v["completeness"], json!({ "graph": true, "source": false, "lists": false }));
        assert_eq!(v["size"]["source"], "skeleton");
        assert_eq!(v["size"]["continue"][0]["list"], "items[0].callers");
        assert_eq!(v["size"]["continue"][0]["rest"], 2);
    }
}
