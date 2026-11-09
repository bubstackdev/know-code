use know_code_core::{
    protocol::{envelope::Payload, *},
    query::{QueryRequest, QueryResult, ReadView},
    store::Writer,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.artifacts/context-tests")
        .join(format!(
            "{}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("context"),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("index.db")
}
fn populate(writer: &mut Writer, comp: &str, text: &str, extents: bool) {
    let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
    let artifact = format!("code@{hash}");
    writer
        .stage(Payload::Compilation(Compilation {
            id: comp.into(),
            language: "typescript".into(),
            target: comp.into(),
            options_json: json!({"strict":true,"large": "x".repeat(2000)}).to_string(),
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Source(SourceArtifact {
            id: artifact.clone(),
            path: "code.ts".into(),
            utf8: text.as_bytes().to_vec(),
            sha256: hash,
            compilation_id: comp.into(),
            generated: true,
        }))
        .unwrap();
    let start = text.find("work").unwrap() as u64;
    let mut occurrences = vec![Occurrence {
        symbol_id: "f".into(),
        artifact_id: artifact.clone(),
        start_byte: start,
        end_byte: start + 4,
        role: "declaration".into(),
        ..Default::default()
    }];
    if extents {
        occurrences.push(Occurrence {
            symbol_id: "f".into(),
            artifact_id: artifact,
            start_byte: text.find("function").unwrap() as u64,
            end_byte: text.len() as u64,
            role: "definition".into(),
            ..Default::default()
        });
    }
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: comp.into(),
            occurrences,
            symbols: vec![
                Symbol {
                    id: "f".into(),
                    name: "work".into(),
                    signature: "() => string".into(),
                    ..Default::default()
                },
                Symbol {
                    id: "external".into(),
                    name: "outside".into(),
                    external: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Report(AnalysisReport {
            compilation_id: comp.into(),
            status: "partial".into(),
            diagnostics: vec![Diagnostic {
                severity: "error".into(),
                message: "saved compiler error".into(),
                ..Default::default()
            }],
            capabilities: [("calls".into(), "partial".into())].into(),
        }))
        .unwrap();
}
fn query(view: &ReadView, request: serde_json::Value) -> QueryResult {
    view.query(serde_json::from_value(request).unwrap())
        .unwrap()
}

#[test]
fn context_preserves_compilations_and_budgets_exact_indexed_unicode_source() {
    let db = path();
    let mut writer = Writer::open(&db).unwrap();
    let mut prefix = String::from("// π😀\0 retained");
    prefix.extend(std::iter::repeat_n('x', 8191 - prefix.len()));
    let padded = format!("{prefix}\r\n\rfunction work() {{ return '😀'; }}");
    let text = padded.as_str();
    populate(&mut writer, "one", text, true);
    populate(&mut writer, "two", text, true);
    writer.publish().unwrap();
    let old = ReadView::open(&db).unwrap();
    let result = query(
        &old,
        json!({"op":"symbol_context","symbol_id":"f","max_bytes":45}),
    );
    assert_eq!(result.contexts.len(), 2);
    assert!(result.truncated);
    assert_eq!(result.workspace_coverage.statuses["partial"], 2);
    let mut returned = 0;
    for c in &result.contexts {
        let d = &c.declarations[0];
        assert_eq!(d.line, 3);
        assert!(d.location.generated);
        assert!(d.extent.is_some());
        if let Some(s) = &d.source {
            assert_eq!(s.start_line, 3);
            assert_eq!(
                s.text.as_bytes(),
                &text.as_bytes()[s.start_byte as usize..s.end_byte as usize]
            );
            returned += s.text.len();
        }
    }
    assert!(returned <= 45);
    assert!(result.coverage.iter().all(|c| c.analysis_options.is_null()
        && c.diagnostics.is_empty()
        && c.diagnostic_counts["error"] == 1
        && c.details_omitted));
    let selected = query(
        &old,
        json!({"op":"symbol_context","symbol_id":"f","target":"two","include_source":false}),
    );
    assert_eq!(selected.contexts.len(), 1);
    assert_eq!(selected.coverage.len(), 1);
    assert_eq!(selected.contexts[0].symbol.target, "two");
    assert_eq!(
        selected.contexts[0].declarations[0].source_status,
        "omitted"
    );
    let prefix = text.find("function").unwrap();
    let emoji = text[prefix..].find('😀').unwrap();
    let clipped = query(
        &old,
        json!({"op":"symbol_context","symbol_id":"f","target":"one","max_bytes":emoji+1}),
    );
    let source = clipped.contexts[0].declarations[0].source.as_ref().unwrap();
    assert!(clipped.truncated);
    assert_eq!(source.text, &text[prefix..prefix + emoji]);
    let unread = query(
        &old,
        json!({"op":"source","artifact_id":source.artifact_id,"content_hash":source.content_hash,"start_byte":prefix+emoji,"end_byte":prefix+emoji+4,"max_bytes":4}),
    );
    assert_eq!(unread.source.unwrap().text, "😀");
    let no_match: QueryRequest =
        serde_json::from_value(json!({"op":"symbol_context","symbol_id":"f","target":"absent"}))
            .unwrap();
    assert!(old.query(no_match).is_err());
    let empty = query(&old, json!({"op":"symbols","name":"absent"}));
    assert!(empty.coverage.is_empty());
    assert_eq!(empty.workspace_coverage.compilations, 2);
    assert_eq!(empty.workspace_coverage.diagnostic_counts["error"], 2);

    // Publish changed source; a pinned reader keeps exact old text and extents.
    let mut writer = Writer::open(&db).unwrap();
    let new_text = "\nfunction work() { return 'new'; }";
    populate(&mut writer, "one", new_text, true);
    populate(&mut writer, "two", new_text, true);
    writer.publish().unwrap();
    let pinned = query(
        &old,
        json!({"op":"symbol_context","symbol_id":"f","target":"one","generation":old.generation}),
    );
    assert!(
        pinned.contexts[0].declarations[0]
            .source
            .as_ref()
            .unwrap()
            .text
            .contains('😀')
    );
    let new = ReadView::open(&db).unwrap();
    let stale: QueryRequest = serde_json::from_value(
        json!({"op":"symbol_context","symbol_id":"f","generation":old.generation}),
    )
    .unwrap();
    assert!(
        new.query(stale)
            .unwrap_err()
            .to_string()
            .contains("restart lookup")
    );
    let current = query(
        &new,
        json!({"op":"symbol_context","symbol_id":"f","compilation_id":"one"}),
    );
    assert_eq!(current.contexts.len(), 1);
    assert_eq!(current.coverage.len(), 1);
    assert_eq!(current.coverage[0].compilation_id, "one");
    assert_eq!(current.contexts[0].declarations[0].line, 2);
    assert!(
        current.contexts[0].declarations[0]
            .source
            .as_ref()
            .unwrap()
            .text
            .contains("new")
    );
}

#[test]
fn missing_and_ambiguous_extents_and_external_symbols_are_honest() {
    let db = path();
    let mut writer = Writer::open(&db).unwrap();
    let text = "function work() { return '😀'; }";
    populate(&mut writer, "one", text, false);
    writer.publish().unwrap();
    let view = ReadView::open(&db).unwrap();
    let result = query(&view, json!({"op":"symbol_context","symbol_id":"f"}));
    let d = &result.contexts[0].declarations[0];
    assert_eq!(d.source_status, "extent_unavailable");
    assert!(d.source.is_none());
    let external = query(&view, json!({"op":"symbol_context","symbol_id":"external"}));
    assert_eq!(
        external.contexts[0].boundary,
        Some("external_or_other_compilation_body")
    );
    drop(view);
    let mut writer = Writer::open(&db).unwrap();
    populate(&mut writer, "one", text, true);
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: "one".into(),
            occurrences: vec![Occurrence {
                symbol_id: "f".into(),
                artifact_id: format!("code@{:x}", Sha256::digest(text.as_bytes())),
                start_byte: 1,
                end_byte: text.len() as u64,
                role: "definition".into(),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .unwrap();
    writer.publish().unwrap();
    let view = ReadView::open(&db).unwrap();
    let result = query(&view, json!({"op":"symbol_context","symbol_id":"f"}));
    assert_eq!(
        result.contexts[0].declarations[0].source_status,
        "extent_ambiguous"
    );
    assert!(result.contexts[0].declarations[0].source.is_none());
}

#[test]
fn details_require_generation_and_clip_payloads_without_hiding_counts() {
    let db = path();
    let mut writer = Writer::open(&db).unwrap();
    populate(&mut writer, "one", "function work() {}", true);
    writer.publish().unwrap();
    let view = ReadView::open(&db).unwrap();
    let missing: QueryRequest = serde_json::from_value(json!({"op":"coverage"})).unwrap();
    assert!(
        view.query(missing)
            .unwrap_err()
            .to_string()
            .contains("requires generation")
    );
    let result = query(
        &view,
        json!({"op":"coverage","generation":view.generation,"max_bytes":100}),
    );
    assert!(result.truncated);
    assert!(result.coverage[0].details_omitted);
    assert!(result.coverage[0].analysis_options.is_null());
    assert_eq!(result.coverage[0].diagnostic_counts["error"], 1);
    let full = query(&view, json!({"op":"coverage","generation":view.generation}));
    assert!(!full.truncated);
    assert!(!full.coverage[0].details_omitted);
    assert!(
        full.coverage[0].analysis_options["strict"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        full.coverage[0].diagnostics[0].message,
        "saved compiler error"
    );
    let invalid: QueryRequest = serde_json::from_value(
        json!({"op":"coverage","generation":view.generation,"compilation_id":"absent"}),
    )
    .unwrap();
    assert!(view.query(invalid).is_err());
}
