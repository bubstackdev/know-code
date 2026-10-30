//! The `review` answer (structure version 1, see `review.rs`) as markdown and as SARIF 2.1.0: what a CI job posts on a
//! pull or merge request, and what code scanning reads.
//!
//! Both carry locations, ids and messages built from the answer's fields, never source text, so content a privacy
//! file blocks can't reach a comment or a log: the answer never holds it, and neither form quotes a line. Each row's
//! `id` (stable across lines, order and pages) is its SARIF fingerprint and, in markdown, a hidden key at the end of
//! its line, so a job can find a row it posted before.
//!
//! SARIF holds findings only: one result per contract consumer, `error` when the change breaks it and `warning` when it
//! may (its primary location the changed contract's line in the reviewed repo, the consumer a related location in its
//! own repo), and one per rule match, at the rule's severity. Tests, missed partners and dependents are advice and
//! stay in markdown. What the answer says it couldn't settle, and why a section is cut, are run notifications.

use std::fmt::Write as _;

use serde_json::{Value, json};

/// The structure version these renderers read.
pub const VERSION: u64 = 1;

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_default()
}

fn n(v: &Value, k: &str) -> Option<u64> {
    v[k].as_u64()
}

/// Text inside a markdown table cell or list line: no pipes, no line breaks, no markup a name could carry.
fn md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '*' | '_' | '[' | ']' | '`' | '#' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// A code span: a name or a path, with backticks it holds doubled around.
fn code(text: &str) -> String {
    let text = text.replace(['\n', '\r'], " ");
    if text.contains('`') { format!("`` {text} ``") } else { format!("`{text}`") }
}

/// `file:line` (or `file:line-end`), after its repo when it isn't the reviewed one.
fn place(repo: &str, file: &str, line: Option<u64>, end: Option<u64>) -> String {
    let mut p = String::new();
    if !repo.is_empty() {
        p.push_str(repo);
        p.push('/');
    }
    p.push_str(file);
    if let Some(l) = line {
        let _ = write!(p, ":{l}");
        if let Some(e) = end.filter(|e| *e > l) {
            let _ = write!(p, "-{e}");
        }
    }
    code(&p)
}

fn key(row: &Value) -> String {
    format!(" <!-- kc:{} -->", s(row, "id"))
}

/// One section's cut, said under it: how many of how many, and the call that continues it.
fn cut_line(env: &Value, section: &str) -> Option<String> {
    let sec = &env["sections"][section];
    if sec["complete"].as_bool() != Some(false) {
        return None;
    }
    let reasons: Vec<&str> = sec["incomplete"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let mut line = match (n(sec, "shown"), n(sec, "total")) {
        (Some(shown), Some(total)) if total > shown => format!("_{shown} of {total} shown"),
        _ => "_Not whole".to_string(),
    };
    if !reasons.is_empty() {
        let _ = write!(line, " ({})", reasons.join(", "));
    }
    if let Some(next) = sec["next"].as_str().filter(|t| !t.is_empty()) {
        let _ = write!(line, "; more: {}", code(next));
    }
    line.push_str("._");
    Some(line)
}

fn rows<'a>(env: &'a Value, k: &str) -> &'a [Value] {
    env[k].as_array().map(Vec::as_slice).unwrap_or_default()
}

/// The review as markdown: a summary line, then breaking contract changes, rule matches, what changed, what depends
/// on it, the tests to run, the files that usually change with it, and what the index can't settle.
pub fn markdown(env: &Value) -> String {
    let mut out = String::new();
    let head = s(env, "head");
    let head = if head == "working" { "the working tree".to_string() } else { code(&head[..head.len().min(12)]) };
    let base = s(env, "base");
    // The repo by its name: `repo` is its root, a path on the machine that answered, which a comment never shows.
    let repo = s(env, "repo");
    let name = std::path::Path::new(repo).file_name().and_then(|n| n.to_str()).unwrap_or(repo);
    let _ = writeln!(out, "### KnowCode review of {} ({} against {})", code(name), head, code(&base[..base.len().min(12)]));
    let consumers: Vec<(&Value, &Value)> = rows(env, "contracts").iter().flat_map(|c| c["consumers"].as_array().into_iter().flatten().map(move |k| (c, k))).collect();
    let broken = consumers.iter().filter(|(_, k)| s(k, "verdict") == "broken").count();
    let possible = consumers.len() - broken;
    let _ = writeln!(
        out,
        "\nchanged files: {} · consumers broken: {}, possibly broken: {} · rule matches: {} · tests to run: {} · dependent files: {}",
        rows(env, "changes").len(),
        broken,
        possible,
        rows(env, "rules").len(),
        rows(env, "tests").len(),
        rows(env, "dependents").len()
    );
    if env["complete"].as_bool() == Some(false) {
        out.push_str("\n**Not whole**: see the sections marked cut, and what the index can't settle.\n");
    }

    if !rows(env, "contracts").is_empty() || cut_line(env, "contracts").is_some() {
        out.push_str("\n#### Contracts this change breaks\n\n");
        for c in rows(env, "contracts") {
            let detail = c["detail"].as_str().map(|d| format!(": {}", md(d))).unwrap_or_default();
            let _ = writeln!(out, "- {} {} {} at {}{}{}", s(c, "kind"), code(s(c, "contract")), s(c, "change"), place("", s(c, "file"), n(c, "line"), None), detail, key(c));
            for k in c["consumers"].as_array().into_iter().flatten() {
                let verdict = if s(k, "verdict") == "broken" { "**broken**" } else { "possibly broken" };
                let _ = writeln!(out, "  - {verdict}: {} at {} [{}]{}", code(s(k, "symbol")), place(s(k, "repo"), s(k, "file"), n(k, "line"), None), md(s(k, "confidence")), key(k));
            }
        }
        if let Some(l) = cut_line(env, "contracts") {
            let _ = writeln!(out, "\n{l}");
        }
    }

    if !rows(env, "rules").is_empty() {
        out.push_str("\n#### Rule matches\n\n");
        for r in rows(env, "rules") {
            let _ = writeln!(out, "- {} {} at {}: {}{}", s(r, "severity"), code(s(r, "rule")), place("", s(r, "file"), n(r, "line"), n(r, "end")), md(s(r, "message")), key(r));
        }
        if let Some(l) = cut_line(env, "rules") {
            let _ = writeln!(out, "\n{l}");
        }
    }

    out.push_str("\n#### What changed\n\n");
    for c in rows(env, "changes") {
        let from = c["renamed_from"].as_str().map(|f| format!(" from {}", code(f))).unwrap_or_default();
        let syms: Vec<String> = c["symbols"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|y| {
                let removed = if s(y, "change") == "removed" { " (removed)" } else { "" };
                format!("{}{removed}", code(s(y, "symbol")))
            })
            .collect();
        let more = n(c, "symbols_total").filter(|t| *t as usize > syms.len()).map(|t| format!(" and {} more", t as usize - syms.len())).unwrap_or_default();
        let syms = if syms.is_empty() { String::new() } else { format!(": {}{more}", syms.join(", ")) };
        let _ = writeln!(out, "- {} {}{from}{syms}{}", code(s(c, "file")), s(c, "change"), key(c));
    }
    if let Some(l) = cut_line(env, "changes") {
        let _ = writeln!(out, "\n{l}");
    }

    if !rows(env, "dependents").is_empty() {
        out.push_str("\n#### What depends on it\n\n");
        for d in rows(env, "dependents") {
            let _ = writeln!(out, "- {}{}", place(s(d, "repo"), s(d, "file"), None, None), key(d));
            let listed = d["rows"].as_array().map_or(0, Vec::len);
            for r in d["rows"].as_array().into_iter().flatten() {
                let via = r["via"].as_str().map(|v| format!(" → {}", code(v))).unwrap_or_default();
                let _ = writeln!(out, "  - line {}: {}{via} ({}, {} [{}])", r["line"], code(s(r, "symbol")), md(s(r, "edge")), md(s(r, "class")), md(s(r, "confidence")));
            }
            if let Some(t) = n(d, "rows_total").filter(|t| *t as usize > listed) {
                let _ = writeln!(out, "  - and {} more", t as usize - listed);
            }
        }
        if let Some(l) = cut_line(env, "dependents") {
            let _ = writeln!(out, "\n{l}");
        }
    }

    if !rows(env, "tests").is_empty() {
        out.push_str("\n#### Tests to run\n\n");
        if let Some(a) = env["sections"]["tests"]["advisory"].as_str() {
            let _ = writeln!(out, "_{}_\n", md(a));
        }
        for t in rows(env, "tests") {
            let run = t["run"].as_str().map(|r| format!(": {}", code(r))).unwrap_or_default();
            let _ = writeln!(out, "- {} ({}){run}{}", code(s(t, "file")), t["confidence"], key(t));
        }
        if let Some(l) = cut_line(env, "tests") {
            let _ = writeln!(out, "\n{l}");
        }
    }

    if !rows(env, "missed").is_empty() {
        out.push_str("\n#### Files that usually change with these, left alone\n\n");
        for m in rows(env, "missed") {
            let _ = writeln!(out, "- {}: with {} in {} of its commits{}", code(s(m, "file")), code(s(m, "with")), m["commits"], key(m));
        }
        if let Some(l) = cut_line(env, "missed") {
            let _ = writeln!(out, "\n{l}");
        }
    }

    let unsettled: Vec<&str> = rows(env, "unsettled").iter().filter_map(Value::as_str).collect();
    if !unsettled.is_empty() {
        out.push_str("\n#### What the index can't settle\n\n");
        for u in unsettled {
            let _ = writeln!(out, "- {}", md(u));
        }
    }
    out
}

/// The SARIF rule id of a contract consumer's finding.
fn consumer_rule(verdict: &str) -> &'static str {
    if verdict == "broken" { "knowcode/contract-break" } else { "knowcode/contract-possible-break" }
}

fn location(file: &str, line: Option<u64>, end: Option<u64>, base: &str) -> Value {
    let mut region = json!({ "startLine": line.unwrap_or(1).max(1) });
    if let Some(e) = end.filter(|e| Some(*e) > line) {
        region["endLine"] = json!(e);
    }
    json!({ "physicalLocation": { "artifactLocation": { "uri": file, "uriBaseId": base }, "region": region } })
}

/// The review as a SARIF 2.1.0 log (see the module docs for what it holds).
pub fn sarif(env: &Value) -> Value {
    let mut results = Vec::new();
    let mut rule_ids: Vec<(String, String)> = Vec::new();
    let mut use_rule = |id: &str, description: String| {
        if !rule_ids.iter().any(|(r, _)| r == id) {
            rule_ids.push((id.to_string(), description));
        }
        id.to_string()
    };
    // Each other repo a consumer is in gets a base id of its own; its root isn't known to the log's reader.
    let mut bases: Vec<String> = Vec::new();
    for c in rows(env, "contracts") {
        for k in c["consumers"].as_array().into_iter().flatten() {
            let verdict = s(k, "verdict");
            let rule = use_rule(
                consumer_rule(verdict),
                if verdict == "broken" { "A contract change breaks a consumer in another repo".into() } else { "A contract change may break a consumer in another repo".into() },
            );
            let repo = s(k, "repo").to_string();
            let base = format!("REPO_{}", repo.replace(|c: char| !c.is_ascii_alphanumeric(), "_"));
            if !bases.contains(&base) {
                bases.push(base.clone());
            }
            let mut related = location(s(k, "file"), n(k, "line"), None, &base);
            related["id"] = json!(0);
            related["message"] = json!({ "text": format!("{} in {repo}", s(k, "symbol")) });
            results.push(json!({
                "ruleId": rule,
                "level": if verdict == "broken" { "error" } else { "warning" },
                "message": { "text": format!(
                    "{} {} {}: {} {} in {repo} ({}:{}, {})",
                    s(c, "kind"), s(c, "contract"), s(c, "change"),
                    if verdict == "broken" { "breaks" } else { "may break" },
                    s(k, "symbol"), s(k, "file"), k["line"], s(k, "confidence"),
                ) },
                "locations": [location(s(c, "file"), n(c, "line"), None, "%SRCROOT%")],
                "relatedLocations": [related],
                "partialFingerprints": { "knowcodeId/v1": s(k, "id") },
            }));
        }
    }
    for r in rows(env, "rules") {
        let rule = use_rule(s(r, "rule"), s(r, "message").to_string());
        results.push(json!({
            "ruleId": rule,
            "level": crate::sarif::level(s(r, "severity")),
            "message": { "text": s(r, "message") },
            "locations": [location(s(r, "file"), n(r, "line"), n(r, "end"), "%SRCROOT%")],
            "partialFingerprints": { "knowcodeId/v1": s(r, "id") },
        }));
    }
    for r in &mut results {
        let id = r["ruleId"].as_str().unwrap_or_default();
        if let Some(i) = rule_ids.iter().position(|(x, _)| x == id) {
            r["ruleIndex"] = json!(i);
        }
    }
    let mut notes: Vec<Value> = Vec::new();
    let mut note = |id: &str, text: String| notes.push(json!({ "level": "warning", "descriptor": { "id": id }, "message": { "text": text } }));
    for u in rows(env, "unsettled").iter().filter_map(Value::as_str) {
        note("unsettled", u.to_string());
    }
    for section in ["changes", "tests", "missed", "dependents", "contracts", "rules"] {
        let sec = &env["sections"][section];
        if sec["complete"].as_bool() == Some(false) {
            let reasons: Vec<&str> = sec["incomplete"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            note("section-cut", format!("{section}: not whole ({})", reasons.join(", ")));
        }
    }
    for i in env["incomplete"].as_array().into_iter().flatten() {
        let reason = i["reason"].as_str().unwrap_or("incomplete");
        note(reason, format!("{reason}: {}", s(i, "detail")));
    }
    let original: serde_json::Map<String, Value> = bases.iter().map(|b| (b.clone(), json!({ "description": { "text": format!("the root of repo {}", &b[5..]) } }))).collect();
    let mut run = json!({
        "tool": { "driver": {
            "name": "KnowCode",
            "version": env!("CARGO_PKG_VERSION"),
            "informationUri": "https://github.com/bubstackdev/know-code",
            "rules": rule_ids.iter().map(|(id, d)| json!({ "id": id, "shortDescription": { "text": d } })).collect::<Vec<_>>(),
        } },
        "invocations": [{ "executionSuccessful": true, "toolExecutionNotifications": notes }],
        "results": results,
    });
    if !original.is_empty() {
        run["originalUriBaseIds"] = Value::Object(original);
    }
    json!({
        "$schema": "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [run],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A review answer of structure version 1, as `review` answers it, over made-up repos (a shop's API and its web
    /// client), with every section filled and two of them cut. Fields with nothing to report are left out, as the
    /// answer leaves them out: a whole section's `next` and `incomplete`, and a changed symbol's `change`.
    fn answer() -> Value {
        json!({
            "version": 1, "repo": "shop-api", "base": "4b825dc642cb6eb9a060e54bf8d69288fbee4904", "head": "working",
            "complete": false,
            "incomplete": [{ "reason": "withheld", "detail": "shop-api: its privacy file blocks some paths; KnowCode never reads or answers from blocked content" }],
            "changes": [
                { "id": "1111111111111111", "file": "src/BasketController.cs", "change": "modified",
                  "symbols": [{ "symbol": "BasketController.Submit", "kind": "method", "line": 12 },
                              { "symbol": "BasketController.Withdraw", "kind": "method", "line": 30, "change": "removed" }], "symbols_total": 4 },
                { "id": "2222222222222222", "file": "src/Ledger|Book.cs", "change": "renamed", "renamed_from": "src/Ledger.cs", "symbols": [] }
            ],
            "tests": [{ "id": "3333333333333333", "file": "tests/BasketTests.cs", "confidence": 0.82, "run": "dotnet test tests/Shop.Tests.csproj --filter BasketTests" }],
            "missed": [{ "id": "4444444444444444", "file": "src/BasketDto.cs", "with": "src/BasketController.cs", "support": 0.6, "commits": 5 }],
            "dependents": [
                { "id": "5555555555555555", "file": "src/Checkout.cs", "rows": [{ "line": 40, "symbol": "Checkout.Run", "via": "BasketController.Submit", "edge": "calls", "confidence": "exact", "hop": 1, "class": "direct" }], "rows_total": 3 },
                { "id": "6666666666666666", "file": "src/api/basket.ts", "repo": "shop-web", "rows": [{ "line": 8, "symbol": "BasketClient.place", "edge": "http", "confidence": "exact", "hop": 1, "class": "direct" }] }
            ],
            "contracts": [{
                "id": "7777777777777777", "kind": "http", "contract": "DELETE /baskets/{id}", "change": "removed", "file": "src/BasketController.cs", "line": 29,
                "consumers": [
                    { "id": "8888888888888888", "repo": "shop-web", "file": "src/api/basket.ts", "line": 21, "symbol": "BasketClient.cancel", "verdict": "broken", "confidence": "exact", "link": "exact" },
                    { "id": "9999999999999999", "repo": "shop-admin", "file": "src/basket.service.ts", "line": 7, "symbol": "BasketService.drop", "verdict": "possible", "confidence": "unique_name", "link": "unique_name" }
                ]
            }],
            "rules": [{ "id": "aaaaaaaaaaaaaaaa", "rule": "no-result", "severity": "warning", "message": "blocking on <Task>.Result", "file": "src/Checkout.cs", "line": 41, "end": 42 }],
            "walkthrough": null,
            "unsettled": ["policy: which of several implementations calls reach: edges left as ambiguous weren't walked past"],
            "sections": {
                "changes": { "complete": true, "shown": 2, "total": 2 },
                "tests": { "complete": true, "shown": 1, "total": 1, "advisory": "Advice: a test not listed may still fail." },
                "missed": { "complete": true, "shown": 1, "total": 1 },
                "dependents": { "complete": false, "shown": 2, "total": 5, "next": "kc {op: review, diff: working, dependents: 10}", "incomplete": ["capped"] },
                "contracts": { "complete": false, "shown": 1, "total": 1, "incomplete": ["unsupported"] },
                "rules": { "complete": true, "shown": 1, "total": 1 }
            }
        })
    }

    fn golden(name: &str, got: &str) {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots").join(name);
        if std::env::var_os("KNOWCODE_TEST_BLESS").is_some() {
            std::fs::write(&file, got).unwrap();
        }
        let want = std::fs::read_to_string(&file).unwrap_or_default();
        assert!(got == want, "{name} differs from its golden file:\n{got}");
    }

    #[test]
    fn markdown_matches_its_golden_file_and_keys_every_row() {
        let env = answer();
        let md = markdown(&env);
        golden("review.md", &md);
        // Every row's id is a key on its line.
        for k in ["changes", "tests", "missed", "dependents", "contracts", "rules"] {
            for row in rows(&env, k) {
                assert!(md.contains(&format!("<!-- kc:{} -->", s(row, "id"))), "{k}: {row}");
            }
        }
        for c in rows(&env, "contracts")[0]["consumers"].as_array().unwrap() {
            assert!(md.contains(&format!("<!-- kc:{} -->", s(c, "id"))));
        }
        // A pipe in a name can't break a line, and a name's markup is shown, not applied.
        assert!(md.contains("`src/Ledger|Book.cs`") && md.contains("&lt;Task&gt;"), "{md}");
        assert!(md.contains("_2 of 5 shown (capped); more: `kc {op: review, diff: working, dependents: 10}`._"), "{md}");
    }

    #[test]
    fn sarif_matches_its_golden_file_with_findings_only() {
        let env = answer();
        let log = sarif(&env);
        golden("review.sarif", &(serde_json::to_string_pretty(&log).unwrap() + "\n"));
        let results = log["runs"][0]["results"].as_array().unwrap();
        // One result per consumer and per rule match; tests, missed and dependents are advice, not findings.
        assert_eq!(results.len(), 3);
        assert_eq!((s(&results[0], "level"), s(&results[1], "level"), s(&results[2], "level")), ("error", "warning", "warning"));
        assert_eq!(results[0]["locations"][0]["physicalLocation"]["region"]["startLine"], 29);
        assert_eq!(results[0]["relatedLocations"][0]["physicalLocation"]["artifactLocation"]["uriBaseId"], "REPO_shop_web");
        assert_eq!(results[0]["partialFingerprints"]["knowcodeId/v1"], "8888888888888888");
        let notes: Vec<&str> = log["runs"][0]["invocations"][0]["toolExecutionNotifications"].as_array().unwrap().iter().map(|n| s(&n["descriptor"], "id")).collect();
        assert_eq!(notes, ["unsettled", "section-cut", "section-cut", "withheld"]);
        let text = log.to_string();
        assert!(!text.contains("snippet") && !text.contains("contextRegion"), "{text}");
    }
}
