//! SARIF 2.1.0 logs of KnowCode's findings (`format: sarif`), for code scanning and other SARIF consumers.
//!
//! A log carries locations, rule ids and messages, never source text: a result names the file and lines it is
//! about, and a consumer that shows code reads it from its own checkout. Columns are left out, since KnowCode counts
//! them in bytes and SARIF in UTF-16 code units. What the answer says it missed (`incomplete`, `withheld` among it)
//! becomes a notification of the run, so a log never reads as whole when its answer isn't.

use serde_json::{Value, json};

/// One finding: a rule's match, a contract change's consumer, and so on.
#[derive(Debug, Clone)]
pub struct Finding {
    pub rule: String,
    /// `error`, `warning` or `note`.
    pub level: &'static str,
    pub message: String,
    /// Its file, relative to the repo the log is for.
    pub file: String,
    pub line: u32,
    pub end_line: u32,
}

/// A rule the findings name, with the one-line description a consumer lists it by.
#[derive(Debug, Clone)]
pub struct RuleInfo {
    pub id: String,
    pub description: String,
}

/// The SARIF level of a rule severity (ast-grep's `error`, `warning`, `info`, `hint`).
pub fn level(severity: &str) -> &'static str {
    match severity {
        "error" => "error",
        "warning" => "warning",
        _ => "note",
    }
}

/// A SARIF log of one run: the tool, the rules, the findings, and a notification for each entry of the answer's
/// `incomplete` (from its envelope `env`).
pub fn log(rules: &[RuleInfo], findings: &[Finding], env: &Value) -> Value {
    let index = |id: &str| rules.iter().position(|r| r.id == id);
    let results: Vec<Value> = findings
        .iter()
        .map(|f| {
            let mut r = json!({
                "ruleId": f.rule,
                "level": f.level,
                "message": { "text": f.message },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": f.file, "uriBaseId": "%SRCROOT%" },
                        "region": { "startLine": f.line, "endLine": f.end_line.max(f.line) },
                    }
                }],
            });
            if let Some(i) = index(&f.rule) {
                r["ruleIndex"] = json!(i);
            }
            r
        })
        .collect();
    let notifications: Vec<Value> = env["incomplete"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| {
            let reason = i["reason"].as_str().unwrap_or("incomplete");
            json!({
                "level": "warning",
                "message": { "text": format!("{reason}: {}", i["detail"].as_str().unwrap_or_default()) },
                "descriptor": { "id": reason },
            })
        })
        .collect();
    json!({
        "$schema": "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "KnowCode",
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": "https://github.com/bubstackdev/know-code",
                    "rules": rules.iter().map(|r| json!({
                        "id": r.id,
                        "shortDescription": { "text": r.description },
                    })).collect::<Vec<_>>(),
                }
            },
            "invocations": [{
                "executionSuccessful": true,
                "toolExecutionNotifications": notifications,
            }],
            "results": results,
        }]
    })
}

/// The SARIF log of a `rules` answer: each rule that matched, with its first message as its description, and each
/// match on the page as a result.
pub fn rules(d: &crate::rules::RulesData, env: &Value) -> Value {
    let mut infos: Vec<RuleInfo> = Vec::new();
    for h in &d.hits {
        if !infos.iter().any(|r| r.id == h.rule) {
            infos.push(RuleInfo { id: h.rule.clone(), description: h.message.clone() });
        }
    }
    let findings: Vec<Finding> = d
        .hits
        .iter()
        .map(|h| Finding { rule: h.rule.clone(), level: level(h.severity), message: h.message.clone(), file: h.file.clone(), line: h.line, end_line: h.end_line })
        .collect();
    log(&infos, &findings, env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_has_locations_and_messages_and_says_what_its_answer_missed() {
        let env = json!({ "incomplete": [{ "reason": "withheld", "detail": "shop: its privacy file blocks src/secret" }] });
        let f = Finding { rule: "no-result".into(), level: "warning", message: "blocking on task".into(), file: "src/A.cs".into(), line: 3, end_line: 3 };
        let log = log(&[RuleInfo { id: "no-result".into(), description: "blocking".into() }], &[f], &env);
        let run = &log["runs"][0];
        assert_eq!(run["results"][0]["locations"][0]["physicalLocation"]["region"], json!({ "startLine": 3, "endLine": 3 }));
        assert_eq!(run["results"][0]["ruleIndex"], 0);
        assert_eq!(run["invocations"][0]["toolExecutionNotifications"][0]["descriptor"]["id"], "withheld");
        // No source text anywhere: no snippet, no context region.
        let text = log.to_string();
        assert!(!text.contains("snippet") && !text.contains("contextRegion"), "{text}");
    }
}
