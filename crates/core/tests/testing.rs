use know_code_core::{
    protocol::{envelope::Payload, *},
    query::{QueryRequest, ReadView},
    store::Writer,
};
use sha2::{Digest, Sha256};
fn path() -> std::path::PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.artifacts/test-classification/core")
        .join(format!(
            "{}-{}-{}",
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("index.db")
}
fn class(kind: &str) -> TestClassification {
    TestClassification {
        policy_version: 1,
        kind: kind.into(),
        evidence: vec![TestEvidence {
            basis: "fixture".into(),
            value: kind.into(),
            path: "project".into(),
            inferred: false,
        }],
    }
}
fn query(view: &ReadView, value: serde_json::Value) -> know_code_core::query::QueryResult {
    view.query(serde_json::from_value::<QueryRequest>(value).unwrap())
        .unwrap()
}
fn source(writer: &mut Writer, comp: &str, id: &str, label: Option<&str>) {
    writer
        .stage(Payload::Source(SourceArtifact {
            id: id.into(),
            path: format!("{id}.ts"),
            utf8: b"abcdef".to_vec(),
            sha256: format!("{:x}", Sha256::digest(b"abcdef")),
            compilation_id: comp.into(),
            test_classification: label.map(class),
            ..Default::default()
        }))
        .unwrap();
}
fn symbol(id: &str) -> Symbol {
    Symbol {
        id: id.into(),
        name: id.into(),
        kind: "function".into(),
        ..Default::default()
    }
}
fn occurrence(id: &str, artifact: &str, role: &str) -> Occurrence {
    Occurrence {
        symbol_id: id.into(),
        artifact_id: artifact.into(),
        role: role.into(),
        end_byte: 1,
        ..Default::default()
    }
}
fn report(writer: &mut Writer, comp: &str) {
    writer
        .stage(Payload::Report(AnalysisReport {
            compilation_id: comp.into(),
            status: "partial".into(),
            capabilities: [
                ("test_classification".into(), "partial".into()),
                ("calls".into(), "complete".into()),
                ("dependencies".into(), "complete".into()),
            ]
            .into(),
            ..Default::default()
        }))
        .unwrap();
}
fn fixture() -> std::path::PathBuf {
    let path = path();
    let mut w = Writer::open(&path).unwrap();
    w.stage(Payload::Compilation(Compilation {
        id: "comp".into(),
        language: "typescript".into(),
        test_classification: Some(class("unknown")),
        ..Default::default()
    }))
    .unwrap();
    source(&mut w, "comp", "a", None);
    source(&mut w, "comp", "test", Some("test"));
    source(&mut w, "comp", "prod", Some("non_test"));
    let mut external = symbol("external");
    external.external = true;
    w.stage(Payload::Facts(FactBatch {
        compilation_id: "comp".into(),
        symbols: vec![
            symbol("a_prod"),
            symbol("helper"),
            symbol("z_test"),
            symbol("mixed"),
            symbol("unknown"),
            external,
        ],
        occurrences: vec![
            occurrence("a_prod", "prod", "declaration"),
            occurrence("helper", "a", "declaration"),
            occurrence("z_test", "test", "declaration"),
            occurrence("mixed", "test", "declaration"),
            occurrence("mixed", "a", "declaration"),
            occurrence("unknown", "a", "declaration"),
            occurrence("external", "test", "declaration"),
            occurrence("a_prod", "test", "reference"),
            occurrence("a_prod", "a", "reference"),
        ],
        relationships: vec![
            Relationship {
                source_id: "helper".into(),
                target_id: "a_prod".into(),
                kind: "call".into(),
                resolution: "compiler_bound".into(),
                artifact_id: "a".into(),
                end_byte: 1,
                ..Default::default()
            },
            Relationship {
                source_id: "z_test".into(),
                target_id: "helper".into(),
                kind: "call".into(),
                resolution: "compiler_bound".into(),
                artifact_id: "test".into(),
                end_byte: 1,
                ..Default::default()
            },
        ],
    }))
    .unwrap();
    report(&mut w, "comp");
    w.publish().unwrap();
    path
}
#[test]
fn positive_mixed_unknown_external_and_prelimit_filters() {
    let p = fixture();
    let v = ReadView::open(&p).unwrap();
    let only = query(
        &v,
        serde_json::json!({"op":"symbols","tests":"only","limit":1}),
    );
    assert_eq!(only.symbols[0].id, "mixed");
    assert!(only.truncated);
    assert_eq!(only.symbols[0].test_classification.kind, "mixed");
    assert_eq!(only.symbols[0].test_classification.evidence.len(), 2);
    let exclude = query(&v, serde_json::json!({"op":"symbols","tests":"exclude"}));
    let ids = exclude
        .symbols
        .iter()
        .map(|s| s.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec!["a_prod", "external", "helper", "mixed", "unknown"]
    );
    assert_eq!(exclude.symbols[1].test_classification.kind, "unknown");
    let fts = query(
        &v,
        serde_json::json!({"op":"symbols","tests":"only","query":"z_test","limit":1}),
    );
    assert_eq!(fts.symbols[0].id, "z_test");
    assert_eq!(
        only.workspace_coverage.test_classification.get("partial"),
        Some(&1)
    );
    assert!(!v.needs_test_classification().unwrap());
}
#[test]
fn reference_sites_and_context_filter_before_limit() {
    let p = fixture();
    let v = ReadView::open(&p).unwrap();
    for (filter, expected) in [("only", "test.ts"), ("exclude", "a.ts")] {
        let r = query(
            &v,
            serde_json::json!({"op":"references","symbol_id":"a_prod","tests":filter,"limit":1}),
        );
        assert_eq!(r.occurrences.len(), 1);
        assert_eq!(r.occurrences[0].location.path, expected);
        assert!(!r.truncated);
    }
    assert!(query(&v,serde_json::json!({"op":"symbol_context","symbol_id":"a_prod","tests":"only","include_source":false})).contexts.is_empty());
    assert_eq!(query(&v,serde_json::json!({"op":"symbol_context","symbol_id":"mixed","tests":"exclude","include_source":false})).contexts[0].symbol.test_classification.kind,"mixed");
    for op in ["source", "coverage"] {
        assert!(matches!(
            v.query(
                serde_json::from_value(
                    serde_json::json!({"op":op,"tests":"only","artifact_id":"a"})
                )
                .unwrap()
            ),
            Err(know_code_core::Error::InvalidRequest(_))
        ));
    }
}
#[test]
fn graph_filters_findings_without_pruning_production_helpers() {
    let p = fixture();
    let v = ReadView::open(&p).unwrap();
    let r = query(
        &v,
        serde_json::json!({"op":"impact","symbol_id":"a_prod","tests":"only","limit":1}),
    );
    let g = r.graph.unwrap();
    assert_eq!(g.findings.len(), 1);
    assert_eq!(g.findings[0].symbol.id, "z_test");
    assert_eq!(g.findings[0].paths[0].len(), 2);
    assert!(g.visited_nodes >= 3);
    assert_eq!(g.findings[0].symbol.test_classification.kind, "test");
}
#[test]
fn shared_source_inherits_distinct_contexts_and_explicit_unknown() {
    let p = path();
    let mut w = Writer::open(&p).unwrap();
    for (comp, kind) in [("tests", "test"), ("production", "non_test")] {
        w.stage(Payload::Compilation(Compilation {
            id: comp.into(),
            language: "csharp".into(),
            test_classification: Some(class(kind)),
            ..Default::default()
        }))
        .unwrap();
        source(&mut w, comp, "linked", None);
        source(&mut w, comp, "explicit", Some("unknown"));
        w.stage(Payload::Facts(FactBatch {
            compilation_id: comp.into(),
            symbols: vec![symbol("linked"), symbol("explicit")],
            occurrences: vec![
                occurrence("linked", "linked", "declaration"),
                occurrence("explicit", "explicit", "declaration"),
            ],
            ..Default::default()
        }))
        .unwrap();
        report(&mut w, comp);
    }
    w.publish().unwrap();
    let v = ReadView::open(&p).unwrap();
    let r = query(&v, serde_json::json!({"op":"symbols","tests":"only"}));
    assert_eq!(r.symbols.len(), 1);
    assert_eq!(r.symbols[0].compilation_id, "tests");
    let c = rusqlite::Connection::open(&p).unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM contents", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        query(
            &v,
            serde_json::json!({"op":"symbols","name":"explicit","tests":"exclude"})
        )
        .symbols
        .len(),
        2
    );
}
#[test]
fn format6_migration_keeps_old_readers_unknown_until_backfill() {
    let p = fixture();
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch("DROP INDEX symbol_test_mask; ALTER TABLE symbols DROP COLUMN test_mask; ALTER TABLE compilations DROP COLUMN test_classification_json; ALTER TABLE unit_artifacts DROP COLUMN test_classification_json; UPDATE reports SET capabilities_json='{}'; PRAGMA user_version=6;").unwrap();
    let old = ReadView::open(&p).unwrap();
    assert!(old.needs_test_classification().unwrap());
    assert!(
        query(&old, serde_json::json!({"op":"symbols","tests":"only"}))
            .symbols
            .is_empty()
    );
    let w = Writer::open_incremental(&p, &[]).unwrap();
    w.publish().unwrap();
    assert_eq!(
        query(&old, serde_json::json!({"op":"symbols","tests":"exclude"}))
            .symbols
            .len(),
        6
    );
    let new = ReadView::open(&p).unwrap();
    assert!(new.needs_test_classification().unwrap());
    assert!(
        query(&new, serde_json::json!({"op":"symbols","tests":"only"}))
            .symbols
            .is_empty()
    );
    assert_eq!(
        query(&new, serde_json::json!({"op":"symbols"}))
            .workspace_coverage
            .test_classification
            .get("unknown"),
        Some(&1)
    );
}
#[test]
fn invalid_evidence_is_rejected_and_late_facts_are_finalized() {
    let p = path();
    let mut w = Writer::open(&p).unwrap();
    let mut c = class("mixed");
    assert!(
        w.stage(Payload::Compilation(Compilation {
            id: "bad".into(),
            test_classification: Some(c.clone()),
            ..Default::default()
        }))
        .is_err()
    );
    c.kind = "test".into();
    let mut missing = c.clone();
    missing.evidence.clear();
    assert!(
        w.stage(Payload::Compilation(Compilation {
            id: "bad".into(),
            test_classification: Some(missing),
            ..Default::default()
        }))
        .is_err()
    );
    c.evidence[0].value = "é".repeat(257);
    assert!(
        w.stage(Payload::Compilation(Compilation {
            id: "bad".into(),
            test_classification: Some(c),
            ..Default::default()
        }))
        .is_err()
    );
    w.stage(Payload::Compilation(Compilation {
        id: "ok".into(),
        ..Default::default()
    }))
    .unwrap();
    report(&mut w, "ok");
    w.stage(Payload::Facts(FactBatch {
        compilation_id: "ok".into(),
        symbols: vec![symbol("late")],
        ..Default::default()
    }))
    .unwrap();
    w.publish().unwrap();
    let view = ReadView::open(&p).unwrap();
    assert_eq!(
        query(&view, serde_json::json!({"op":"symbols","tests":"exclude"})).symbols[0].id,
        "late"
    );
}

#[test]
fn bounded_mixed_evidence_keeps_positive_witness_and_unanchored_is_unknown() {
    let p = path();
    let mut w = Writer::open(&p).unwrap();
    w.stage(Payload::Compilation(Compilation {
        id: "c".into(),
        test_classification: Some(class("test")),
        ..Default::default()
    }))
    .unwrap();
    let mut occurrences = vec![];
    for i in 0..10 {
        let id = format!("a{i}");
        source(&mut w, "c", &id, Some("unknown"));
        occurrences.push(occurrence("mixed", &id, "declaration"));
    }
    source(&mut w, "c", "zz_test", Some("test"));
    occurrences.push(occurrence("mixed", "zz_test", "declaration"));
    w.stage(Payload::Facts(FactBatch {
        compilation_id: "c".into(),
        symbols: vec![symbol("mixed"), symbol("unanchored")],
        occurrences,
        ..Default::default()
    }))
    .unwrap();
    report(&mut w, "c");
    w.publish().unwrap();
    let v = ReadView::open(&p).unwrap();
    let r = query(&v, serde_json::json!({"op":"symbols","tests":"only"}));
    assert_eq!(r.symbols.len(), 1);
    let c = &r.symbols[0].test_classification;
    assert!(c.evidence_limited);
    assert_eq!(c.evidence.len(), 8);
    assert!(c.evidence.iter().any(|e| e.kind == "test"));
    assert_eq!(
        query(&v, serde_json::json!({"op":"symbols","name":"unanchored"})).symbols[0]
            .test_classification
            .kind,
        "unknown"
    );
}
