use know_code_core::{
    protocol::{envelope::Payload, *},
    query::{QueryRequest, ReadView},
    store::Writer,
};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

fn database() -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.artifacts/target-grouping-tests")
        .join(format!(
            "{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
        .join("index.db")
}
fn classification(kind: &str, target: &str) -> TestClassification {
    TestClassification {
        policy_version: 1,
        kind: kind.into(),
        evidence: vec![TestEvidence {
            basis: "fixture".into(),
            value: format!("evaluated {kind}; target={target}"),
            path: "p.csproj".into(),
            inferred: false,
        }],
    }
}
fn source(writer: &mut Writer, comp: &str, artifact: &str, class: Option<&str>, generated: bool) {
    let text = b"0123456789abcdef0123456789abcdef";
    writer
        .stage(Payload::Source(SourceArtifact {
            id: artifact.into(),
            path: format!("{artifact}.cs"),
            utf8: text.to_vec(),
            sha256: format!("{:x}", Sha256::digest(text)),
            compilation_id: comp.into(),
            generated,
            test_classification: class.map(|kind| classification(kind, comp)),
        }))
        .unwrap();
}
fn occurrence(id: &str, artifact: &str, at: u64, role: &str) -> Occurrence {
    Occurrence {
        symbol_id: id.into(),
        artifact_id: artifact.into(),
        start_byte: at,
        end_byte: at + 1,
        role: role.into(),
        ..Default::default()
    }
}
fn unit(writer: &mut Writer, comp: &str, target: &str, policy: bool, variant: bool) {
    writer
        .stage(Payload::Compilation(Compilation {
            id: comp.into(),
            language: "csharp".into(),
            project_path: "p.csproj".into(),
            target: target.into(),
            configuration: "Debug".into(),
            target_family: if policy {
                "compiler family".into()
            } else {
                String::new()
            },
            target_equivalence_policy: u32::from(policy),
            test_classification: Some(classification("non_test", target)),
            ..Default::default()
        }))
        .unwrap();
    source(writer, comp, "code", None, false);
    source(writer, comp, "test", Some("test"), false);
    source(writer, comp, "generated", None, true);
    let names = [
        "Root",
        "Helper",
        "Caller",
        "Generated",
        "Unproven",
        "External",
    ];
    let symbols = names
        .iter()
        .map(|name| Symbol {
            id: format!("{comp}:{name}"),
            name: (*name).into(),
            kind: "method".into(),
            signature: format!(
                "{} {name}()",
                if variant && *name == "Root" {
                    "string"
                } else {
                    "int"
                }
            ),
            external: *name == "External",
            declaration_key: if policy && !["Unproven", "External"].contains(name) {
                format!("compiler:{name}")
            } else {
                String::new()
            },
            ..Default::default()
        })
        .collect();
    let mut occurrences = vec![];
    for (i, name) in names.iter().enumerate() {
        if *name != "External" {
            let artifact = if *name == "Generated" {
                "generated"
            } else {
                "code"
            };
            occurrences.push(occurrence(
                &format!("{comp}:{name}"),
                artifact,
                i as u64,
                "declaration",
            ));
            let mut definition =
                occurrence(&format!("{comp}:{name}"), artifact, i as u64, "definition");
            definition.end_byte += 1;
            occurrences.push(definition);
        }
    }
    occurrences.push(occurrence(&format!("{comp}:Root"), "code", 8, "reference"));
    occurrences.push(occurrence(&format!("{comp}:Root"), "test", 10, "reference"));
    let relationships = if variant {
        vec![]
    } else {
        [("Helper", "Root", 8), ("Caller", "Helper", 9)]
            .into_iter()
            .map(|(from, to, at)| Relationship {
                source_id: format!("{comp}:{from}"),
                target_id: format!("{comp}:{to}"),
                kind: "call".into(),
                resolution: "compiler_bound".into(),
                artifact_id: "code".into(),
                start_byte: at,
                end_byte: at + 1,
                dispatch: "direct".into(),
                ..Default::default()
            })
            .collect()
    };
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: comp.into(),
            symbols,
            occurrences,
            relationships,
        }))
        .unwrap();
    writer
        .stage(Payload::Report(AnalysisReport {
            compilation_id: comp.into(),
            status: "complete".into(),
            capabilities: [
                ("calls".into(), "complete".into()),
                ("dependencies".into(), "complete".into()),
                (
                    "target_equivalence".into(),
                    if policy { "partial" } else { "unknown" }.into(),
                ),
            ]
            .into(),
            ..Default::default()
        }))
        .unwrap();
}
fn fixture() -> PathBuf {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    unit(&mut writer, "a", "net10.0", true, false);
    unit(&mut writer, "b", "netstandard2.1", true, false);
    unit(&mut writer, "c", "variant", true, true);
    writer.publish().unwrap();
    path
}
fn query(view: &ReadView, json: serde_json::Value) -> know_code_core::query::QueryResult {
    view.query(serde_json::from_value::<QueryRequest>(json).unwrap())
        .unwrap()
}
#[test]
fn logical_search_limits_preserve_all_exact_support_and_conditional_variants() {
    let path = fixture();
    let view = ReadView::open(&path).unwrap();
    let result = query(
        &view,
        serde_json::json!({"op":"symbols","name":"Root","limit":1}),
    );
    assert!(result.truncated);
    assert_eq!(result.symbols.len(), 1);
    let first = &result.symbols[0];
    assert_eq!(first.id, "a:Root");
    assert_eq!(first.supporting_targets.len(), 2);
    assert_eq!(
        first
            .supporting_targets
            .iter()
            .map(|s| s.symbol_id.as_str())
            .collect::<Vec<_>>(),
        ["a:Root", "b:Root"]
    );
    assert!(
        first
            .supporting_targets
            .iter()
            .all(|s| s.locations.len() == 1 && s.test_classification.kind == "non_test")
    );
    assert_ne!(
        first.supporting_targets[0].test_classification.evidence[0].evidence[0].value,
        first.supporting_targets[1].test_classification.evidence[0].evidence[0].value
    );
    let all = query(&view, serde_json::json!({"op":"symbols","name":"Root"}));
    assert_eq!(all.symbols.len(), 2);
    assert!(
        all.symbols
            .iter()
            .any(|s| s.id == "c:Root" && s.supporting_targets.is_empty())
    );
    let target = query(
        &view,
        serde_json::json!({"op":"symbols","name":"Root","target":"netstandard2.1"}),
    );
    assert_eq!(target.symbols[0].id, "b:Root");
    assert!(target.symbols[0].supporting_targets.is_empty());
    let fts = query(
        &view,
        serde_json::json!({"op":"symbols","query":"Root","compilation_id":"b","limit":1}),
    );
    assert_eq!(fts.symbols[0].id, "b:Root");
    assert!(!fts.truncated);
    assert!(!view.needs_target_equivalence().unwrap());
}
#[test]
fn followup_references_group_use_sites_before_limit_and_honor_scope_and_tests() {
    let path = fixture();
    let view = ReadView::open(&path).unwrap();
    let all = query(
        &view,
        serde_json::json!({"op":"references","symbol_id":"a:Root","limit":1}),
    );
    assert!(all.truncated);
    assert_eq!(all.occurrences.len(), 1);
    assert_eq!(all.occurrences[0].supporting_targets.len(), 2);
    let only = query(
        &view,
        serde_json::json!({"op":"references","symbol_id":"a:Root","tests":"only","limit":1}),
    );
    assert!(!only.truncated);
    assert_eq!(only.occurrences[0].location.path, "test.cs");
    assert_eq!(only.occurrences[0].supporting_targets.len(), 2);
    assert!(
        only.occurrences[0]
            .supporting_targets
            .iter()
            .all(|s| s.test_classification.kind == "test")
    );
    let scoped = query(
        &view,
        serde_json::json!({"op":"references","symbol_id":"a:Root","compilation_id":"b"}),
    );
    assert_eq!(scoped.occurrences.len(), 2);
    assert!(
        scoped
            .occurrences
            .iter()
            .all(|s| s.compilation_id == "b" && s.supporting_targets.is_empty())
    );
    let by_target = query(
        &view,
        serde_json::json!({"op":"references","symbol_id":"a:Root","target":"netstandard2.1"}),
    );
    assert!(
        by_target
            .occurrences
            .iter()
            .all(|s| s.compilation_id == "b")
    );
    assert!(matches!(
        view.query(
            serde_json::from_value(serde_json::json!({"op":"references","symbol_id":"missing"}))
                .unwrap()
        ),
        Err(know_code_core::Error::InvalidRequest(_))
    ));
}
#[test]
fn context_support_keeps_each_exact_declaration_and_source() {
    let path = fixture();
    let view = ReadView::open(&path).unwrap();
    let result = query(
        &view,
        serde_json::json!({"op":"symbol_context","symbol_id":"a:Root","limit":1}),
    );
    assert_eq!(result.contexts.len(), 1);
    assert!(!result.truncated);
    let context = &result.contexts[0];
    assert_eq!(context.supporting_targets.len(), 2);
    for member in &context.supporting_targets {
        assert_eq!(member.declarations.len(), 1);
        let d = &member.declarations[0];
        assert_eq!(d.source.as_ref().unwrap().text, "01");
        assert_eq!(d.location.path, "code.cs");
    }
    let scoped = query(
        &view,
        serde_json::json!({"op":"symbol_context","symbol_id":"a:Root","compilation_id":"b"}),
    );
    assert_eq!(scoped.contexts[0].symbol.id, "b:Root");
    assert!(scoped.contexts[0].supporting_targets.is_empty());
}
#[test]
fn context_limit_bounds_declaration_slots_and_preserves_equivalent_support() {
    for members in 1..=2 {
        let path = database();
        let mut writer = Writer::open(&path).unwrap();
        for (comp, target) in [("a", "net10.0"), ("b", "netstandard2.1")]
            .into_iter()
            .take(members)
        {
            unit(&mut writer, comp, target, true, false);
            let mut occurrences = vec![];
            for at in [14, 16] {
                occurrences.push(occurrence(
                    &format!("{comp}:Root"),
                    "code",
                    at,
                    "declaration",
                ));
                let mut definition = occurrence(&format!("{comp}:Root"), "code", at, "definition");
                definition.end_byte += 1;
                occurrences.push(definition);
            }
            writer
                .stage(Payload::Facts(FactBatch {
                    compilation_id: comp.into(),
                    occurrences,
                    ..Default::default()
                }))
                .unwrap();
        }
        writer.publish().unwrap();
        let view = ReadView::open(&path).unwrap();
        for limit in 1..=3 {
            let result = query(
                &view,
                serde_json::json!({
                    "op":"symbol_context", "symbol_id":"a:Root", "limit":limit,
                    "include_source":false
                }),
            );
            assert_eq!(result.contexts.len(), 1);
            assert_eq!(result.truncated, limit < 3);
            let context = &result.contexts[0];
            assert_eq!(context.declarations.len(), limit);
            assert_eq!(
                context.supporting_targets.len(),
                if members == 1 { 0 } else { 2 }
            );
            let offsets = context
                .declarations
                .iter()
                .map(|d| d.location.start_byte)
                .collect::<Vec<_>>();
            assert_eq!(offsets, [0, 14, 16][..limit]);
            for support in &context.supporting_targets {
                assert_eq!(support.declarations.len(), limit);
                assert_eq!(support.symbol.locations.len(), limit);
                assert_eq!(
                    support
                        .declarations
                        .iter()
                        .map(|d| d.location.start_byte)
                        .collect::<Vec<_>>(),
                    offsets
                );
                assert!(
                    support
                        .declarations
                        .iter()
                        .all(|d| d.source.is_none() && d.source_status == "omitted")
                );
            }
        }
        let bounded = query(
            &view,
            serde_json::json!({
                "op":"symbol_context", "symbol_id":"a:Root", "limit":3, "max_bytes":5
            }),
        );
        assert!(bounded.truncated);
        let context = &bounded.contexts[0];
        let declarations: Vec<_> = context
            .declarations
            .iter()
            .chain(
                context
                    .supporting_targets
                    .iter()
                    .flat_map(|s| &s.declarations),
            )
            .collect();
        assert_eq!(
            declarations
                .iter()
                .filter_map(|d| d.source.as_ref())
                .map(|s| s.text.len())
                .sum::<usize>(),
            5
        );
    }
}

#[test]
fn grouped_context_source_budget_counts_every_wire_copy_at_unicode_boundaries() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    let text = "f😀π";
    for (comp, target) in [("a", "net10.0"), ("b", "netstandard2.1")] {
        unit(&mut writer, comp, target, true, false);
        writer
            .stage(Payload::Source(SourceArtifact {
                id: "unicode".into(),
                path: "unicode.cs".into(),
                utf8: text.as_bytes().to_vec(),
                sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
                compilation_id: comp.into(),
                ..Default::default()
            }))
            .unwrap();
        let id = format!("{comp}:Unicode");
        let mut definition = occurrence(&id, "unicode", 0, "definition");
        definition.end_byte = text.len() as u64;
        writer
            .stage(Payload::Facts(FactBatch {
                compilation_id: comp.into(),
                symbols: vec![Symbol {
                    id: id.clone(),
                    name: "f".into(),
                    kind: "method".into(),
                    signature: "void f()".into(),
                    declaration_key: "compiler:Unicode".into(),
                    ..Default::default()
                }],
                occurrences: vec![occurrence(&id, "unicode", 0, "declaration"), definition],
                ..Default::default()
            }))
            .unwrap();
    }
    writer.publish().unwrap();
    let view = ReadView::open(&path).unwrap();
    for budget in 1..=22 {
        let result = query(
            &view,
            serde_json::json!({"op":"symbol_context", "symbol_id":"a:Unicode", "max_bytes":budget}),
        );
        assert_eq!(result.contexts.len(), 1);
        let context = &result.contexts[0];
        assert_eq!(context.supporting_targets.len(), 2);
        assert_eq!(context.declarations.len(), 1);
        assert!(
            context
                .supporting_targets
                .iter()
                .all(|s| s.declarations.len() == 1)
        );
        let declarations: Vec<_> = context
            .declarations
            .iter()
            .chain(
                context
                    .supporting_targets
                    .iter()
                    .flat_map(|s| &s.declarations),
            )
            .collect();
        let wire_bytes: usize = declarations
            .iter()
            .filter_map(|d| d.source.as_ref())
            .map(|s| s.text.len())
            .sum();
        assert!(
            wire_bytes <= budget,
            "budget={budget}, wire_bytes={wire_bytes}"
        );
        for declaration in &declarations {
            if let Some(source) = &declaration.source {
                assert_eq!(
                    source.text.as_bytes(),
                    &text.as_bytes()[source.start_byte as usize..source.end_byte as usize]
                );
                assert!(
                    text.is_char_boundary(source.start_byte as usize)
                        && text.is_char_boundary(source.end_byte as usize)
                );
            }
        }
        assert_eq!(result.truncated, budget < text.len() * 3);
        if budget == 1 {
            assert!(context.declarations[0].source.is_none());
            assert_eq!(context.declarations[0].source_status, "budget_exhausted");
        }
        if budget >= text.len() * 3 {
            assert_eq!(wire_bytes, text.len() * 3);
            assert!(declarations.iter().all(|d| d.source_status == "included"));
        }
    }
}

#[test]
fn context_declaration_projection_cap_retains_all_exact_support() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    for (comp, target) in [("a", "net10.0"), ("b", "netstandard2.1")] {
        unit(&mut writer, comp, target, true, false);
        writer
            .stage(Payload::Facts(FactBatch {
                compilation_id: comp.into(),
                occurrences: (1..=16)
                    .map(|at| occurrence(&format!("{comp}:Root"), "code", at, "declaration"))
                    .collect(),
                ..Default::default()
            }))
            .unwrap();
    }
    writer.publish().unwrap();
    let result = query(
        &ReadView::open(&path).unwrap(),
        serde_json::json!({
            "op":"symbol_context", "symbol_id":"a:Root", "limit":1000, "include_source":false
        }),
    );
    assert!(result.truncated);
    assert_eq!(result.contexts.len(), 1);
    let context = &result.contexts[0];
    assert_eq!(context.declarations.len(), 16);
    assert_eq!(context.supporting_targets.len(), 2);
    for support in &context.supporting_targets {
        assert_eq!(support.declarations.len(), 16);
        assert_eq!(support.symbol.locations.len(), 16);
        assert_eq!(
            support
                .declarations
                .iter()
                .map(|d| d.location.start_byte)
                .collect::<Vec<_>>(),
            (0..16).collect::<Vec<_>>()
        );
    }
}

#[test]
fn graph_groups_only_projection_and_keeps_full_concrete_paths_and_shared_budgets() {
    let path = fixture();
    let view = ReadView::open(&path).unwrap();
    let graph = query(
        &view,
        serde_json::json!({"op":"impact","symbol_id":"a:Root"}),
    )
    .graph
    .unwrap();
    assert_eq!(graph.findings.len(), 2);
    for finding in &graph.findings {
        assert_eq!(finding.supporting_targets.len(), 2);
        for member in &finding.supporting_targets {
            assert!(member.paths.iter().all(|p| {
                p.iter()
                    .all(|e| e.compilation_id == member.symbol.compilation_id)
            }));
            assert_eq!(
                member.paths[0].len(),
                if member.symbol.symbol_id.ends_with(":Caller") {
                    2
                } else {
                    1
                }
            );
        }
    }
    let limited = query(
        &view,
        serde_json::json!({"op":"impact","symbol_id":"a:Root","limit":1}),
    );
    assert!(limited.truncated);
    let g = limited.graph.unwrap();
    assert_eq!(g.findings.len(), 1);
    assert_eq!(g.findings[0].supporting_targets.len(), 2);
    assert!(g.limits.iter().any(|s| s == "result_limit"));
    let scoped = query(
        &view,
        serde_json::json!({"op":"callers","symbol_id":"a:Root","compilation_id":"b"}),
    )
    .graph
    .unwrap();
    assert!(
        scoped
            .findings
            .iter()
            .all(|f| f.symbol.compilation_id == "b" && f.supporting_targets.is_empty())
    );
}
#[test]
fn unproven_external_and_generated_symbols_remain_singletons() {
    let path = fixture();
    let view = ReadView::open(&path).unwrap();
    for name in ["Unproven", "External", "Generated"] {
        let result = query(&view, serde_json::json!({"op":"symbols","name":name}));
        assert_eq!(result.symbols.len(), 3, "{name}");
        assert!(
            result
                .symbols
                .iter()
                .all(|s| s.supporting_targets.is_empty())
        );
    }
    let oversized = view.query(
        serde_json::from_value(serde_json::json!({"op":"symbols","response_max_bytes":1024}))
            .unwrap(),
    );
    assert!(matches!(
        oversized,
        Err(know_code_core::Error::ResponseLimitExceeded)
    ));
}
#[test]
fn legacy_format7_migration_preserves_pinned_singletons_without_invented_keys() {
    let path = fixture();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("DROP INDEX symbol_grouping; ALTER TABLE symbols DROP COLUMN grouping_key; ALTER TABLE symbols DROP COLUMN declaration_key; ALTER TABLE compilations DROP COLUMN target_family; ALTER TABLE compilations DROP COLUMN target_equivalence_policy; UPDATE reports SET capabilities_json=json_remove(capabilities_json,'$.target_equivalence'); PRAGMA user_version=7;").unwrap();
    let old = ReadView::open(&path).unwrap();
    assert!(old.needs_target_equivalence().unwrap());
    let writer = Writer::open_incremental(&path, &[]).unwrap();
    writer.publish().unwrap();
    for view in [old, ReadView::open(&path).unwrap()] {
        let result = query(&view, serde_json::json!({"op":"symbols","name":"Root"}));
        assert_eq!(result.symbols.len(), 3);
        assert!(
            result
                .symbols
                .iter()
                .all(|s| s.supporting_targets.is_empty())
        );
        assert!(view.needs_target_equivalence().unwrap());
    }
}

#[test]
fn expanded_graph_seeds_consume_the_same_node_budget() {
    let path = fixture();
    let view = ReadView::open(&path).unwrap();
    let result = query(
        &view,
        serde_json::json!({"op":"impact","symbol_id":"a:Root","max_nodes":1}),
    );
    assert!(result.truncated);
    let graph = result.graph.unwrap();
    assert!(graph.visited_nodes <= 1);
    assert!(graph.limits.iter().any(|limit| limit == "node_budget"));
    assert!(
        graph
            .findings
            .iter()
            .all(|finding| finding.supporting_targets.len() <= 1)
    );
}
