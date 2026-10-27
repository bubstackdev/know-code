//! The provenance record: an export for durable records of an answer, asked for with `--provenance` on the command line
//! or `provenance: true` on any MCP tool call. It says which build answered, the call and the answer by their hashes,
//! and for each repo the answer read: the worktree, the full commit, a digest of its uncommitted content, the index
//! generation and the base the index started from, and the privacy policy (see [`kc_index::provenance`]). Compact
//! answers in a conversation don't carry it.
//!
//! Two hashes name the answer. `answer.hash` is of the bytes returned (without the record): it proves what was
//! received. `answer.evidence` is of the answer's content with what names or measures one index or one moment left
//! out (row ids, cursors, the generation, timings, how much a sync reparsed, a search's work counters, how long ago a
//! cached value was computed): the same question asked again of the same code gives the same evidence, from another
//! index, process or worktree.

use std::path::Path;

use kc_index::provenance::digest;
use serde_json::{Map, Value, json};

/// Bumped when the record's fields or the evidence's canonical form change.
pub const FORMAT: u32 = 1;

/// The record of a call: `call` its canonical description, `answer` the bytes returned without the record, `envs` the
/// envelopes the answer was made from (one, or one per input of a batch).
pub fn record(call: &Value, answer: &[u8], envs: &[Value]) -> Value {
    let evidence = match envs {
        [one] => evidence(one),
        many => digest(many.iter().map(evidence).collect::<Vec<_>>().join("\n").as_bytes()),
    };
    json!({
        "format": FORMAT,
        "knowcode": build(),
        "call": { "query": call, "hash": digest(canonical(call).as_bytes()) },
        "answer": { "hash": digest(answer), "bytes": answer.len(), "evidence": evidence },
        "repos": repos(envs),
    })
}

/// The build that answered: version, commit, index schema, resolver, and every extractor's version.
pub fn build() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "build": env!("KNOWCODE_BUILD"),
        "schema": kc_store::SCHEMA_VERSION,
        "resolver": kc_store::RESOLVER_VERSION,
        "extractors": kc_index::provenance::extractors(),
    })
}

/// Each repo the envelopes' `provenance` names, once: what the answer read (commit, generation, dirty, fresh) with the
/// worktree, uncommitted content, base and policy (see [`kc_index::provenance::repo`]).
fn repos(envs: &[Value]) -> Vec<Value> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for entry in envs.iter().filter_map(|e| e["provenance"].as_array()).flatten() {
        let Some(root) = entry["repo"].as_str() else { continue };
        if seen.iter().any(|r| r == root) {
            continue;
        }
        seen.push(root.to_string());
        let mut repo = serde_json::to_value(kc_index::provenance::repo(Path::new(root))).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut repo {
            map.insert("repo".into(), Value::from(root));
            for k in ["generation", "dirty", "fresh"] {
                map.insert(k.into(), entry.get(k).cloned().unwrap_or(Value::Null));
            }
            // The commit the answer read, when its entry says; HEAD now otherwise.
            if let Some(c) = entry.get("commit").filter(|c| c.is_string()) {
                map.insert("commit".into(), c.clone());
            }
        }
        out.push(repo);
    }
    out
}

/// The digest of an answer's evidence (see the module): its content in canonical form, without what names or measures
/// one index or one moment.
pub fn evidence(env: &Value) -> String {
    digest(canonical(&strip(env, "")).as_bytes())
}

/// Whether the field `key` (in the object at `parent`) names or measures one index or one moment.
fn per_index(parent: &str, key: &str) -> bool {
    matches!(key, "id" | "cursor" | "generation" | "knowcode" | "provenance" | "record")
        || key.ends_with("_cursor")
        || key.ends_with("_ms")
        || (parent == "freshness" && matches!(key, "reparsed" | "removed"))
        || (parent == "stats" && matches!(key, "ms" | "candidates" | "bytes_read"))
}

fn strip(v: &Value, parent: &str) -> Value {
    match v {
        Value::Object(map) => {
            Value::Object(map.iter().filter(|(k, _)| !per_index(parent, k)).map(|(k, x)| (k.clone(), strip(x, k))).collect::<Map<_, _>>())
        }
        Value::Array(list) => Value::Array(list.iter().map(|x| strip(x, parent)).collect()),
        Value::String(s) => Value::String(ages(s)),
        other => other.clone(),
    }
}

/// `s` with each "<n> <unit>(s) earlier" (how long ago a cached value was computed) said as "<age> earlier".
fn ages(s: &str) -> String {
    const UNITS: &[&str] = &["seconds", "second", "minutes", "minute", "hours", "hour", "days", "day"];
    if !s.contains(" earlier") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find(" earlier") {
        let (head, tail) = rest.split_at(at);
        let replaced = UNITS.iter().find_map(|u| {
            let before = head.strip_suffix(u)?.strip_suffix(' ')?;
            let digits = before.len() - before.trim_end_matches(|c: char| c.is_ascii_digit()).len();
            (digits > 0).then(|| &before[..before.len() - digits])
        });
        match replaced {
            Some(prefix) => {
                out.push_str(prefix);
                out.push_str("<age>");
            }
            None => out.push_str(head),
        }
        out.push_str(" earlier");
        rest = &tail[" earlier".len()..];
    }
    out.push_str(rest);
    out
}

/// JSON with every object's keys sorted and no whitespace: one text per value.
pub fn canonical(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys.iter().map(|k| format!("{}:{}", Value::from(k.as_str()), canonical(&map[*k]))).collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(list) => format!("[{}]", list.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_leaves_out_what_names_one_index_or_moment() {
        let a = json!({
            "items": [{ "id": 12, "name": "Store", "file": "a.ts", "line": 3 }],
            "cursor": "kc1.abc", "freshness": { "check_ms": 1.5, "reparsed": 2, "failed": [] },
            "stats": { "ms": 3.0, "candidates": 9, "searchable_files": 40 },
            "provenance": [{ "repo": "/w/shop", "generation": "1|r2" }],
            "detail": "areas computed from the index as it was 12 seconds earlier",
        });
        let b = json!({
            "detail": "areas computed from the index as it was 3 minutes earlier",
            "stats": { "searchable_files": 40, "ms": 1.0, "candidates": 4 },
            "freshness": { "failed": [], "reparsed": 0, "check_ms": 9.0 },
            "items": [{ "line": 3, "file": "a.ts", "name": "Store", "id": 99 }],
            "cursor": "kc1.def", "provenance": [{ "repo": "/w/shop", "generation": "7|r2" }],
        });
        assert_eq!(evidence(&a), evidence(&b));
        // What the answer says about the code is evidence.
        let mut c = b.clone();
        c["items"][0]["line"] = json!(4);
        assert_ne!(evidence(&b), evidence(&c));
        let mut d = b.clone();
        d["stats"]["searchable_files"] = json!(41);
        assert_ne!(evidence(&b), evidence(&d));
    }

    #[test]
    fn ages_are_said_without_their_number() {
        assert_eq!(ages("as it was 12 seconds earlier, not recomputed"), "as it was <age> earlier, not recomputed");
        assert_eq!(ages("1 day earlier and 2 hours earlier"), "<age> earlier and <age> earlier");
        assert_eq!(ages("a moment earlier"), "a moment earlier");
    }

    #[test]
    fn canonical_json_sorts_keys() {
        assert_eq!(canonical(&json!({ "b": [1, { "d": 1, "c": "x" }], "a": null })), r#"{"a":null,"b":[1,{"c":"x","d":1}]}"#);
    }
}
