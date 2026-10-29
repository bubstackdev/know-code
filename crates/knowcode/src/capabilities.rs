//! `capabilities`: every op as data, for a client building its own tool surface (see the `kc_capabilities` help
//! topic). Nothing here restates what an op takes: the input schema is the one help lists and the advertised schema is
//! cut from, the ranges and choices are the tables the ops clamp and refuse by (`mcp::RANGES`, `mcp::CHOICES`), and
//! `q`/`qs` are `mcp::ONE_OPS`. What only this module says (whether an op writes, its answer's shape) is checked
//! against the running server by `tests/capabilities.rs`.

use std::collections::BTreeMap;

use kc_store::answer::Code;
use serde_json::{Value, json};

use crate::mcp;

/// The document's own version: raised when a field changes meaning or goes; new fields don't raise it.
pub const VERSION: u32 = 1;

/// The ops that write. Notes are the only writes.
pub const WRITES: &[&str] = &["kc_note_add"];

/// Parameters standing for another: (tool, alias, the parameter it stands for).
pub const ALIASES: &[(&str, &str, &str)] = &[("kc_refs", "edges", "kinds")];

/// The ops answering several inputs in one call: (tool, the parameter taking them).
const BATCH: &[(&str, &str)] = &[("kc_context", "symbols"), ("kc_refs", "names"), ("kc_locate", "queries")];

/// The evidence policy an op taking `evidence` uses when none is given, where it isn't `default`.
const EVIDENCE_DEFAULT: &[(&str, &str)] = &[("kc_tests", "all")];

/// An answer's shape: its format by default, the formats it takes, the keys a JSON answer always has, and the list
/// it is about.
struct Shape {
    format: &'static str,
    formats: &'static [&'static str],
    keys: &'static [&'static str],
    list: Option<&'static str>,
}

const LIST: Shape = Shape { format: "json", formats: &["json"], keys: &["complete", "items", "total"], list: Some("items") };
const TREE: Shape = Shape { format: "tree", formats: &["tree", "json"], keys: &["complete", "items", "total"], list: Some("items") };

/// Each op's answer, where it isn't [`LIST`].
fn shape(tool: &str) -> Shape {
    match tool {
        "kc_path" | "kc_flow" => Shape { formats: &["tree", "json", "mermaid"], ..TREE },
        "kc_areas" => Shape { formats: &["json", "mermaid"], ..LIST },
        "kc_trace" | "kc_impact" => TREE,
        "kc_status" => Shape { format: "json", formats: &["json"], keys: &["complete", "schema", "coverage", "freshness"], list: None },
        "kc_rename_preview" => Shape { keys: &["complete", "items", "total", "symbol", "new_name", "refused"], ..LIST },
        "kc_task" => Shape { keys: &["complete", "items", "total", "dependents", "tests", "unsettled", "sections"], ..LIST },
        "kc_help" => Shape { format: "text", formats: &["text"], keys: &[], list: None },
        "kc_capabilities" => Shape { format: "json", formats: &["json"], keys: &["capabilities", "server", "protocol", "ops"], list: Some("ops") },
        _ => LIST,
    }
}

/// What a server offers: its tools' full schemas and descriptions, the tools its profile leaves out, and whether it
/// serves the one `kc` tool.
pub struct Surface<'a> {
    pub schemas: &'a BTreeMap<String, rmcp::model::JsonObject>,
    pub descriptions: &'a BTreeMap<String, String>,
    pub hidden: &'a [String],
    pub one: bool,
    /// Restricted mode's scope, when the server runs in it.
    pub restriction: Option<&'a crate::restricted::Restriction>,
}

/// The capabilities document; `of` narrows it to one op.
pub fn document(s: &Surface, of: Option<&str>) -> Result<Value, String> {
    let of = of.map(|o| o.trim().trim_start_matches("kc_")).filter(|o| !o.is_empty());
    if let Some(o) = of
        && !mcp::ONE_OPS.iter().any(|(op, _, _)| *op == o)
    {
        let ops: Vec<&str> = mcp::ONE_OPS.iter().map(|(o, _, _)| *o).collect();
        return Err(format!("no op {o:?}: one of {}", ops.join(", ")));
    }
    let ops: Vec<Value> = mcp::ONE_OPS.iter().filter(|(op, _, _)| of.is_none_or(|o| o == *op)).map(|(op, q, qs)| entry(s, op, q, qs)).collect();
    let advertised: Vec<&String> = s.schemas.keys().filter(|t| !s.hidden.contains(t)).collect();
    let name = |v: Vec<String>| Value::from(v);
    Ok(json!({
        "capabilities": VERSION,
        "server": { "name": "knowcode", "version": concat!(env!("CARGO_PKG_VERSION"), "+", env!("KNOWCODE_BUILD")) },
        "protocol": rmcp::model::ProtocolVersion::default().to_string(),
        "index_schema": kc_store::SCHEMA_VERSION,
        "profile": if s.one { json!("one") } else { json!(advertised) },
        "common": common(),
        "evidence": {
            "presets": ["default", "confirmed", "all"],
            "mechanisms": name(kc_store::answer::MECHANISMS.iter().map(|m| kc_store::answer::name_of(*m)).collect()),
            "confirmed": name(kc_store::answer::CONFIRMED.iter().map(|m| kc_store::answer::name_of(*m)).collect()),
            "assumptions": name(kc_store::answer::ASSUMPTIONS.iter().map(|a| kc_store::answer::name_of(*a)).collect()),
        },
        "reasons": kc_store::answer::REASONS.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        "codes": kc_store::answer::CODES.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "partial": {
            "completeness": ["graph", "source", "lists"],
            "size": ["max_chars", "chars", "source", "continue"],
            "continue": ["list", "cursor", "rest", "chars"],
        },
        "errors": {
            "at": "structuredContent.error",
            "keys": ["code", "message"],
            "privacy": ["repo", "file", "block_at", "blocks"],
        },
        "scope": s.restriction.map_or_else(|| json!({ "restricted": false }), |r| r.scope()),
        "ops": ops,
    }))
}

/// The parameters every op but `more`, `help` and `capabilities` takes.
fn common() -> Value {
    let r = &mcp::ANSWER_CHARS;
    json!({
        "repo": { "type": "string", "description": crate::help::COMMON.iter().find(|(n, _)| *n == "repo").map(|(_, d)| *d).unwrap_or_default() },
        "max_chars": { "type": "integer", "minimum": r.min, "maximum": r.max, "default": r.default, "description": r.line() },
        "provenance": {
            "type": "boolean",
            "default": false,
            "description": "true: the answer is unchanged, and a second content block holds {\"record\": …}, the provenance record for durable records (kc_help contract). Taken by every call; in no tool's schema, so it costs nothing up front.",
        },
    })
}

/// What the server does with a number for a bounded parameter: a count past `max` is clamped to it (up to `type_max`,
/// past which it is refused); one under `min` is clamped up to it, but never a negative number, which the parameter's
/// unsigned type refuses, as it does a value of another type.
pub(crate) fn range_json(r: &mcp::Range) -> Value {
    let refused = Code::InvalidArgument.as_str();
    let mut v = json!({
        "min": r.min,
        "max": r.max,
        "default": r.default,
        "above_max": "clamped",
        "below_min": if r.min > 0 { "clamped" } else { refused },
        "negative": refused,
        "wrong_type": refused,
    });
    if let Some(t) = r.type_max {
        v["type_max"] = json!(t);
        v["above_type_max"] = json!(refused);
    }
    v
}

fn entry(s: &Surface, op: &str, q: &str, qs: &str) -> Value {
    let tool = format!("kc_{op}");
    let input = s.schemas.get(&tool).cloned().unwrap_or_default();
    let props = input.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut ranges = serde_json::Map::new();
    let mut choices = serde_json::Map::new();
    for name in props.keys() {
        if let Some(r) = mcp::range(&tool, name) {
            ranges.insert(name.clone(), range_json(r));
        }
        if let Some(c) = mcp::choices(&tool, name) {
            choices.insert(name.clone(), json!({ "values": c, "default": c[0], "other": "refused", "code": Code::InvalidArgument.as_str() }));
        }
    }
    let aliases: serde_json::Map<String, Value> =
        ALIASES.iter().filter(|(t, _, _)| *t == tool).map(|(_, a, of)| (a.to_string(), Value::from(*of))).collect();
    let topic = crate::help::topic(&tool);
    let shape = shape(&tool);
    let mut e = json!({
        "op": op,
        "tool": tool,
        "summary": topic.map(|t| t.summary).unwrap_or_default(),
        "description": s.descriptions.get(&tool).cloned().unwrap_or_default(),
        "help": format!("knowcode://help/{tool}"),
        "advertised": s.one || !s.hidden.contains(&tool),
        "allowed": s.restriction.is_none_or(|r| r.allows(&tool)),
        "access": if WRITES.contains(&tool.as_str()) { "write" } else { "read" },
        "takes_query": !q.is_empty(),
        "q": if q.is_empty() { Value::Null } else { Value::from(q) },
        "qs": if qs.is_empty() { Value::Null } else { Value::from(qs) },
        "input": input,
        "ranges": ranges,
        "choices": choices,
        "aliases": aliases,
        "output": { "format": shape.format, "formats": shape.formats, "keys": shape.keys, "list": shape.list },
    });
    if let Some((_, param)) = BATCH.iter().find(|(t, _)| *t == tool) {
        e["batch"] = json!({ "param": param, "max": mcp::MAX_BATCH, "keys": ["complete", "results"], "over": Code::InvalidArgument.as_str() });
    }
    if props.contains_key("evidence") {
        let default = EVIDENCE_DEFAULT.iter().find(|(t, _)| *t == tool).map_or("default", |(_, d)| *d);
        e["evidence"] = json!({ "default": default });
    }
    e
}
