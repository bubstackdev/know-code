mod common;
use know_code_core::{
    protocol::{envelope::Payload, *},
    query::{QueryRequest, ReadView},
    store::Writer,
};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn database(label: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.artifacts/graph-tests")
        .join(format!(
            "{}-{label}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("index.db")
}
fn populate(writer: &mut Writer, comp: &str, ids: &[&str], edges: Vec<Relationship>) {
    writer
        .stage(Payload::Compilation(Compilation {
            id: comp.into(),
            project_path: comp.into(),
            target: comp.into(),
            ..Default::default()
        }))
        .unwrap();
    let text = b"0123456789 source evidence";
    writer
        .stage(Payload::Source(SourceArtifact {
            id: "source".into(),
            path: "a.ts".into(),
            utf8: text.to_vec(),
            sha256: format!("{:x}", Sha256::digest(text)),
            compilation_id: comp.into(),
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: comp.into(),
            symbols: ids
                .iter()
                .map(|id| Symbol {
                    id: (*id).into(),
                    name: (*id).into(),
                    kind: "function".into(),
                    ..Default::default()
                })
                .collect(),
            relationships: edges,
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Report(AnalysisReport {
            compilation_id: comp.into(),
            status: "complete".into(),
            capabilities: [
                ("calls".into(), "complete".into()),
                ("dependencies".into(), "complete".into()),
            ]
            .into(),
            ..Default::default()
        }))
        .unwrap();
}
fn edge(from: &str, to: &str, position: u64, resolution: &str) -> Relationship {
    Relationship {
        source_id: from.into(),
        target_id: to.into(),
        kind: "call".into(),
        resolution: resolution.into(),
        artifact_id: "source".into(),
        start_byte: position,
        end_byte: position + 1,
        dispatch: "direct".into(),
        dispatch_key: "$".into(),
        ..Default::default()
    }
}
fn query(view: &ReadView, json: serde_json::Value) -> know_code_core::query::QueryResult {
    view.query(serde_json::from_value(json).unwrap()).unwrap()
}

#[test]
fn strong_paths_win_over_shorter_possible_paths_and_cycles_are_bounded() {
    let db = database("paths");
    let mut writer = Writer::open(&db).unwrap();
    populate(
        &mut writer,
        "one",
        &["changed", "middle", "caller"],
        vec![
            edge("middle", "changed", 0, "compiler_bound"),
            edge("caller", "middle", 1, "compiler_bound"),
            edge("caller", "changed", 2, "possible_dispatch"),
            edge("middle", "caller", 3, "compiler_bound"),
        ],
    );
    // Same opaque ID in another context must never fabricate a joined path.
    populate(
        &mut writer,
        "two",
        &["middle", "unrelated"],
        vec![edge("unrelated", "middle", 4, "compiler_bound")],
    );
    writer.publish().unwrap();
    let view = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let r = query(
        &view,
        serde_json::json!({"op":"impact","symbol_id":"changed"}),
    );
    let graph = r.graph.unwrap();
    assert_eq!(graph.findings.len(), 2);
    let caller = graph
        .findings
        .iter()
        .find(|f| f.symbol.id == "caller")
        .unwrap();
    assert_eq!(caller.paths.len(), 1);
    assert_eq!(caller.paths[0].len(), 2);
    assert!(
        caller.paths[0]
            .iter()
            .all(|e| e.resolution == "compiler_bound" && e.compilation_id == "one")
    );
    let graph = query(
        &view,
        serde_json::json!({"op":"impact","symbol_id":"changed","paths_per_symbol":2}),
    )
    .graph
    .unwrap();
    let caller = graph
        .findings
        .iter()
        .find(|f| f.symbol.id == "caller")
        .unwrap();
    assert_eq!(caller.paths.len(), 2);
    assert_eq!(caller.paths[1].len(), 1);
    assert_eq!(caller.paths[1][0].resolution, "possible_dispatch");
    let limited = query(
        &view,
        serde_json::json!({"op":"impact","symbol_id":"changed","max_edges":1}),
    );
    assert!(limited.truncated);
    let limited = limited.graph.unwrap();
    assert!(limited.limits.iter().any(|l| l == "edge_budget"));
    assert!(
        !limited.findings.is_empty(),
        "budget discarded discovered valid paths"
    );
}

#[test]
fn preferred_path_does_not_hide_shorter_possible_reachability_at_depth_limit() {
    let db = database("depth-preference");
    let mut writer = Writer::open(&db).unwrap();
    populate(
        &mut writer,
        "one",
        &["changed", "a", "b", "c", "d", "x", "y"],
        vec![
            edge("a", "changed", 0, "compiler_bound"),
            edge("b", "a", 1, "compiler_bound"),
            edge("c", "b", 2, "compiler_bound"),
            edge("d", "c", 3, "compiler_bound"),
            edge("x", "d", 4, "compiler_bound"),
            edge("x", "changed", 5, "possible_dispatch"),
            edge("y", "x", 6, "compiler_bound"),
        ],
    );
    writer.publish().unwrap();
    let view = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let graph = query(
        &view,
        serde_json::json!({"op":"impact","symbol_id":"changed","max_depth":5}),
    )
    .graph
    .unwrap();
    let x = graph.findings.iter().find(|f| f.symbol.id == "x").unwrap();
    assert_eq!(x.paths.len(), 1);
    assert_eq!(x.paths[0].len(), 5);
    let y = graph
        .findings
        .iter()
        .find(|f| f.symbol.id == "y")
        .expect("preferred route hid a reachable affected symbol");
    assert_eq!(y.paths.len(), 1);
    assert_eq!(y.paths[0].len(), 2);
    assert!(
        y.paths[0]
            .iter()
            .any(|e| e.resolution == "possible_dispatch")
    );
}

#[test]
fn callsites_unresolved_targets_implicit_filters_and_dispatch_proofs_survive() {
    let db = database("evidence");
    let mut writer = Writer::open(&db).unwrap();
    let mut property = edge("caller", "getter", 3, "compiler_bound");
    property.implicit = true;
    let mut virtual_call = edge("caller", "interface", 6, "compiler_bound");
    virtual_call.dispatch = "interface".into();
    let mut implementation = edge("implementation", "interface", 7, "compiler_bound");
    implementation.kind = "implements".into();
    implementation.dispatch.clear();
    populate(
        &mut writer,
        "one",
        &["caller", "callee", "getter", "interface", "implementation"],
        vec![
            edge("caller", "callee", 0, "compiler_bound"),
            edge("caller", "callee", 0, "compiler_bound"),
            edge("caller", "callee", 1, "compiler_bound"),
            edge("caller", "", 2, "unresolved"),
            property,
            virtual_call,
            implementation,
        ],
    );
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: "one".into(),
            occurrences: (0..20)
                .map(|start| Occurrence {
                    symbol_id: "callee".into(),
                    artifact_id: "source".into(),
                    start_byte: start,
                    end_byte: start + 1,
                    role: "declaration".into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }))
        .unwrap();
    writer.publish().unwrap();
    let pinned = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let graph = query(
        &pinned,
        serde_json::json!({"op":"callees","symbol_id":"caller","paths_per_symbol":2}),
    )
    .graph
    .unwrap();
    assert_eq!(
        graph
            .findings
            .iter()
            .find(|f| f.symbol.id == "callee")
            .unwrap()
            .paths
            .len(),
        2
    );
    assert!(
        graph
            .boundaries
            .iter()
            .any(|b| b.reason == "declaration_locations_limited")
    );
    assert!(graph.boundaries.iter().any(
        |b| b.reason == "unresolved_target" && b.evidence.as_ref().unwrap().target_id.is_none()
    ));
    let possible = &graph
        .findings
        .iter()
        .find(|f| f.symbol.id == "implementation")
        .unwrap()
        .paths[0][0];
    assert_eq!(possible.resolution, "possible_dispatch");
    assert_eq!(possible.location.start_byte, 6);
    assert_eq!(possible.dispatch_evidence.as_ref().unwrap().start_byte, 7);
    let graph=query(&pinned,serde_json::json!({"op":"callees","symbol_id":"caller","include_implicit":false,"exact_only":true})).graph.unwrap();
    assert!(
        !graph
            .findings
            .iter()
            .any(|f| ["getter", "implementation"].contains(&f.symbol.id.as_str()))
    );
    let incoming = query(
        &pinned,
        serde_json::json!({"op":"callers","symbol_id":"implementation"}),
    )
    .graph
    .unwrap();
    assert_eq!(incoming.findings[0].symbol.id, "caller");
    assert_eq!(
        incoming.findings[0].paths[0][0].resolution,
        "possible_dispatch"
    );
    let mut replacement = Writer::open(&db).unwrap();
    populate(
        &mut replacement,
        "one",
        &["caller", "callee"],
        vec![edge("caller", "callee", 4, "compiler_bound")],
    );
    replacement.publish().unwrap();
    let old = query(
        &pinned,
        serde_json::json!({"op":"callees","symbol_id":"caller","paths_per_symbol":2}),
    );
    assert_eq!(
        old.graph
            .unwrap()
            .findings
            .iter()
            .find(|f| f.symbol.id == "callee")
            .unwrap()
            .paths[0][0]
            .location
            .start_byte,
        0
    );
    let now = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    assert_eq!(
        query(
            &now,
            serde_json::json!({"op":"callees","symbol_id":"caller"})
        )
        .graph
        .unwrap()
        .findings[0]
            .paths[0][0]
            .location
            .start_byte,
        4
    );
}

#[test]
fn invalid_evidence_aborts_without_replacing_a_published_graph() {
    let db = database("invalid");
    let mut w = Writer::open(&db).unwrap();
    populate(
        &mut w,
        "one",
        &["a", "b"],
        vec![edge("a", "b", 0, "compiler_bound")],
    );
    let generation = w.publish().unwrap();
    {
        let mut invalid = Writer::open(&db).unwrap();
        populate(&mut invalid, "one", &["a", "b"], vec![]);
        assert!(
            invalid
                .stage(Payload::Facts(FactBatch {
                    compilation_id: "one".into(),
                    symbols: vec![Symbol {
                        id: String::new(),
                        name: "not-an-unresolved-target".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }))
                .is_err()
        );
        assert_eq!(
            ReadView::open_in_scope(&db, &common::scope(&db))
                .unwrap()
                .generation,
            generation
        );
    }
    for bad in [
        edge("a", "b", 100, "compiler_bound"),
        edge("a", "", 0, "compiler_bound"),
        edge("a", "b", 0, "unresolved"),
        edge("a", "missing", 0, "compiler_bound"),
    ] {
        let mut w = Writer::open(&db).unwrap();
        populate(&mut w, "one", &["a", "b"], vec![]);
        assert!(
            w.stage(Payload::Facts(FactBatch {
                compilation_id: "one".into(),
                relationships: vec![bad],
                ..Default::default()
            }))
            .is_err()
        );
        assert_eq!(
            ReadView::open_in_scope(&db, &common::scope(&db))
                .unwrap()
                .generation,
            generation
        );
    }
    let invalid: QueryRequest =
        serde_json::from_value(serde_json::json!({"op":"impact","symbol_id":"a","max_edges":0}))
            .unwrap();
    assert!(
        ReadView::open_in_scope(&db, &common::scope(&db))
            .unwrap()
            .query(invalid)
            .is_err()
    );
}

#[test]
fn v3_migration_preserves_pinned_readers_and_does_not_invent_call_evidence() {
    let db = database("migration");
    let mut w = Writer::open(&db).unwrap();
    populate(
        &mut w,
        "one",
        &["a", "b"],
        vec![edge("a", "b", 0, "compiler_bound")],
    );
    w.publish().unwrap();
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch("CREATE TABLE old_relationships(source_key INTEGER REFERENCES symbols(rowid) ON DELETE CASCADE,target_key INTEGER REFERENCES symbols(rowid) ON DELETE CASCADE,kind TEXT,PRIMARY KEY(source_key,target_key,kind)) WITHOUT ROWID;
      INSERT INTO old_relationships SELECT source_key,target_key,kind FROM relationships; DROP TABLE relationships; ALTER TABLE old_relationships RENAME TO relationships;
      ALTER TABLE reports DROP COLUMN capabilities_json;
      DROP INDEX symbol_definition;
      ALTER TABLE compilations DROP COLUMN references_json;
      ALTER TABLE compilations DROP COLUMN input_fingerprint;
      ALTER TABLE symbols DROP COLUMN definition_compilation_id;
      ALTER TABLE symbols DROP COLUMN definition_symbol_id;
      ALTER TABLE symbols DROP COLUMN definition_fingerprint; DROP INDEX symbol_test_mask; ALTER TABLE symbols DROP COLUMN test_mask; ALTER TABLE compilations DROP COLUMN test_classification_json; ALTER TABLE unit_artifacts DROP COLUMN test_classification_json; DROP INDEX symbol_grouping; ALTER TABLE symbols DROP COLUMN grouping_key; ALTER TABLE symbols DROP COLUMN declaration_key; ALTER TABLE compilations DROP COLUMN target_family; ALTER TABLE compilations DROP COLUMN target_equivalence_policy; PRAGMA user_version=3;").unwrap();
    drop(c);
    let old = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let graph = query(&old, serde_json::json!({"op":"callees","symbol_id":"a"}))
        .graph
        .unwrap();
    assert!(graph.findings.is_empty());
    assert!(!graph.boundaries.is_empty());
    Writer::open_incremental(&db, &[])
        .unwrap()
        .publish()
        .unwrap();
    assert!(
        query(&old, serde_json::json!({"op":"symbols","name":"a"}))
            .symbols
            .len()
            == 1
    );
    let new = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let graph = query(&new, serde_json::json!({"op":"callees","symbol_id":"a"}))
        .graph
        .unwrap();
    assert!(graph.findings.is_empty());
    assert!(!graph.boundaries.is_empty());
    let c = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(
        c.query_row("SELECT resolution FROM relationships", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "legacy_unknown"
    );
    assert!(
        !c.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
}

fn linked_unit(
    writer: &mut Writer,
    id: &str,
    project: &str,
    fingerprint: &str,
    refs: &[(&str, &str)],
    symbols: Vec<Symbol>,
    edges: Vec<Relationship>,
) {
    writer
        .stage(Payload::Compilation(Compilation {
            id: id.into(),
            project_path: project.into(),
            target: id.into(),
            input_fingerprint: fingerprint.into(),
            references: refs
                .iter()
                .map(|(id, path)| ProjectReferenceContext {
                    compilation_id: (*id).into(),
                    project_path: (*path).into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }))
        .unwrap();
    linked_facts(writer, id, symbols, edges);
}
fn linked_facts(writer: &mut Writer, id: &str, symbols: Vec<Symbol>, edges: Vec<Relationship>) {
    let text = b"0123456789 source evidence";
    writer
        .stage(Payload::Source(SourceArtifact {
            id: "source".into(),
            path: "code".into(),
            utf8: text.to_vec(),
            sha256: format!("{:x}", Sha256::digest(text)),
            compilation_id: id.into(),
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: id.into(),
            symbols,
            relationships: edges,
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Report(AnalysisReport {
            compilation_id: id.into(),
            status: "complete".into(),
            capabilities: [
                ("calls".into(), "complete".into()),
                ("dependencies".into(), "complete".into()),
                ("project_links".into(), "complete".into()),
            ]
            .into(),
            ..Default::default()
        }))
        .unwrap();
}
fn node(id: &str) -> Symbol {
    Symbol {
        id: id.into(),
        name: id.into(),
        ..Default::default()
    }
}
fn proxy(id: &str, unit: &str, definition: &str, fingerprint: &str) -> Symbol {
    Symbol {
        external: true,
        definition_compilation_id: unit.into(),
        definition_symbol_id: definition.into(),
        definition_fingerprint: fingerprint.into(),
        ..node(id)
    }
}
#[test]
fn project_links_are_exact_snapshot_bound_and_keep_pinned_readers() {
    let db = database("project-links");
    let mut w = Writer::open(&db).unwrap();
    linked_unit(
        &mut w,
        "lib8",
        "lib",
        "v1",
        &[],
        vec![node("body"), node("leaf")],
        vec![edge("body", "leaf", 0, "compiler_bound")],
    );
    linked_unit(
        &mut w,
        "libstd",
        "lib",
        "v1",
        &[],
        vec![node("wrong-body")],
        vec![],
    );
    linked_unit(
        &mut w,
        "app",
        "app",
        "app1",
        &[("lib8", "lib")],
        vec![node("caller"), proxy("reference", "lib8", "body", "v1")],
        vec![edge("caller", "reference", 1, "compiler_bound")],
    );
    w.publish().unwrap();
    let pinned = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let request =
        serde_json::json!({"op":"callees","symbol_id":"caller","target":"app","max_depth":3});
    let graph = query(&pinned, request.clone()).graph.unwrap();
    assert_eq!(
        graph
            .findings
            .iter()
            .map(|f| f.symbol.id.as_str())
            .collect::<Vec<_>>(),
        vec!["body", "leaf"]
    );
    let link = graph.findings[0].paths[0][0]
        .definition_link
        .as_ref()
        .unwrap();
    assert_eq!(link.definition_compilation_id, "lib8");
    let incoming = query(
        &pinned,
        serde_json::json!({"op":"impact","symbol_id":"leaf"}),
    )
    .graph
    .unwrap();
    assert!(incoming.findings.iter().any(|f| f.symbol.id == "caller"));
    let alias = query(
        &pinned,
        serde_json::json!({"op":"callees","symbol_id":"reference"}),
    )
    .graph
    .unwrap();
    assert_eq!(alias.root_definitions.len(), 1);
    assert_eq!(alias.findings[0].symbol.id, "leaf");
    let mut replacement = Writer::open_incremental(&db, &["lib8".into()]).unwrap();
    linked_unit(
        &mut replacement,
        "lib8",
        "lib",
        "v2",
        &[],
        vec![node("body"), node("leaf")],
        vec![edge("body", "leaf", 0, "compiler_bound")],
    );
    replacement.publish().unwrap();
    assert_eq!(
        query(&pinned, request.clone())
            .graph
            .unwrap()
            .findings
            .len(),
        2
    );
    let current = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let g = query(&current, request).graph.unwrap();
    assert_eq!(g.findings.len(), 1);
    assert_eq!(g.findings[0].symbol.id, "reference");
    assert!(!g.boundaries.is_empty());
}
#[test]
fn project_paths_cannot_select_conflicting_compilations_along_a_path() {
    let db = database("context-conflict");
    let mut w = Writer::open(&db).unwrap();
    linked_unit(&mut w, "b8", "b", "b8fp", &[], vec![node("b8body")], vec![]);
    linked_unit(
        &mut w,
        "bstd",
        "b",
        "bstdfp",
        &[],
        vec![node("bstdbody")],
        vec![],
    );
    linked_unit(
        &mut w,
        "mid",
        "mid",
        "midfp",
        &[("bstd", "b")],
        vec![node("middle"), proxy("bref", "bstd", "bstdbody", "bstdfp")],
        vec![edge("middle", "bref", 0, "compiler_bound")],
    );
    linked_unit(
        &mut w,
        "app",
        "app",
        "appfp",
        &[("mid", "mid"), ("b8", "b")],
        vec![node("caller"), proxy("midref", "mid", "middle", "midfp")],
        vec![edge("caller", "midref", 1, "compiler_bound")],
    );
    w.publish().unwrap();
    let view = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let g = query(
        &view,
        serde_json::json!({"op":"callees","symbol_id":"caller","max_depth":5}),
    )
    .graph
    .unwrap();
    assert!(g.findings.is_empty());
    assert_eq!(g.boundaries[0].reason, "incompatible_compilation_context");
    let conflict = g.boundaries[0].context_conflict.as_ref().unwrap();
    assert_eq!(conflict.project_path, "b");
    assert_eq!(conflict.selected_compilation_id, "b8");
    assert_eq!(conflict.required_compilation_id, "bstd");
}

#[test]
fn converging_paths_keep_distinct_compilation_selections() {
    let db = database("context-states");
    let mut w = Writer::open(&db).unwrap();
    linked_unit(&mut w, "b8", "b", "8", &[], vec![node("b8")], vec![]);
    linked_unit(&mut w, "bstd", "b", "std", &[], vec![node("bstd")], vec![]);
    linked_unit(
        &mut w,
        "z",
        "z",
        "z",
        &[("bstd", "b")],
        vec![node("zbody")],
        vec![],
    );
    linked_unit(
        &mut w,
        "mid",
        "mid",
        "m",
        &[("z", "z")],
        vec![node("middle"), proxy("zr", "z", "zbody", "z")],
        vec![edge("middle", "zr", 0, "compiler_bound")],
    );
    for (id, b) in [("x", "b8"), ("y", "bstd")] {
        linked_unit(
            &mut w,
            id,
            id,
            id,
            &[(b, "b"), ("mid", "mid")],
            vec![node(id), proxy(&format!("{id}mr"), "mid", "middle", "m")],
            vec![edge(id, &format!("{id}mr"), 0, "compiler_bound")],
        );
    }
    linked_unit(
        &mut w,
        "app",
        "app",
        "a",
        &[("x", "x"), ("y", "y")],
        vec![
            node("root"),
            proxy("xr", "x", "x", "x"),
            proxy("yr", "y", "y", "y"),
        ],
        vec![
            edge("root", "xr", 0, "compiler_bound"),
            edge("root", "yr", 1, "compiler_bound"),
        ],
    );
    w.publish().unwrap();
    let view = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let g = query(
        &view,
        serde_json::json!({"op":"callees","symbol_id":"root","max_depth":5}),
    )
    .graph
    .unwrap();
    let z = g
        .findings
        .iter()
        .find(|f| f.symbol.id == "zbody")
        .expect("compatible converging path was pruned");
    assert_eq!(z.paths[0][0].target_id.as_deref(), Some("y"));
    assert!(g.boundaries.iter().any(|b| b.context_conflict.is_some()));
}
#[test]
fn v4_migration_keeps_local_graphs_without_inventing_project_links() {
    let db = database("v4-links");
    let mut w = Writer::open(&db).unwrap();
    populate(
        &mut w,
        "one",
        &["a", "b"],
        vec![edge("a", "b", 0, "compiler_bound")],
    );
    w.publish().unwrap();
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch("DROP INDEX symbol_definition; ALTER TABLE compilations DROP COLUMN references_json; ALTER TABLE compilations DROP COLUMN input_fingerprint; ALTER TABLE symbols DROP COLUMN definition_compilation_id; ALTER TABLE symbols DROP COLUMN definition_symbol_id; ALTER TABLE symbols DROP COLUMN definition_fingerprint; DROP INDEX relationship_evidence; ALTER TABLE relationships DROP COLUMN dispatch_key; DROP INDEX symbol_test_mask; ALTER TABLE symbols DROP COLUMN test_mask; ALTER TABLE compilations DROP COLUMN test_classification_json; ALTER TABLE unit_artifacts DROP COLUMN test_classification_json; DROP INDEX symbol_grouping; ALTER TABLE symbols DROP COLUMN grouping_key; ALTER TABLE symbols DROP COLUMN declaration_key; ALTER TABLE compilations DROP COLUMN target_family; ALTER TABLE compilations DROP COLUMN target_equivalence_policy; PRAGMA user_version=4;").unwrap();
    drop(c);
    let old = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    Writer::open_incremental(&db, &[])
        .unwrap()
        .publish()
        .unwrap();
    for view in [
        old,
        ReadView::open_in_scope(&db, &common::scope(&db)).unwrap(),
    ] {
        let g = query(&view, serde_json::json!({"op":"callees","symbol_id":"a"}))
            .graph
            .unwrap();
        assert_eq!(g.findings.len(), 1);
        assert!(g.findings[0].paths[0][0].definition_link.is_none());
    }
}

fn selected(id: &str, project: &str) -> ProjectReferenceContext {
    ProjectReferenceContext {
        compilation_id: id.into(),
        project_path: project.into(),
        input_fingerprint: id.into(),
    }
}
fn dispatch_unit(
    writer: &mut Writer,
    id: &str,
    project: &str,
    refs: Vec<ProjectReferenceContext>,
    symbols: Vec<Symbol>,
    edges: Vec<Relationship>,
) {
    writer
        .stage(Payload::Compilation(Compilation {
            id: id.into(),
            project_path: project.into(),
            target: id.into(),
            input_fingerprint: id.into(),
            references: refs,
            ..Default::default()
        }))
        .unwrap();
    linked_facts(writer, id, symbols, edges);
}
#[test]
fn dispatch_uses_specializations_and_snapshot_bound_composition_witnesses() {
    let db = database("dispatch-witness");
    let mut w = Writer::open(&db).unwrap();
    dispatch_unit(&mut w, "b", "b", vec![], vec![node("slot")], vec![]);
    let mut call = edge("caller", "callslot", 0, "compiler_bound");
    call.dispatch = "interface".into();
    call.dispatch_key = "number".into();
    dispatch_unit(
        &mut w,
        "a",
        "a",
        vec![selected("b", "b")],
        vec![node("caller"), proxy("callslot", "b", "slot", "b")],
        vec![call],
    );
    let mut proof = edge("number", "proofslot", 1, "compiler_bound");
    proof.kind = "implements".into();
    proof.dispatch.clear();
    proof.dispatch_key = "number".into();
    let mut alternate = proof.clone();
    alternate.dispatch_key = "string".into();
    let mut different = alternate.clone();
    different.source_id = "string".into();
    dispatch_unit(
        &mut w,
        "c",
        "c",
        vec![selected("b", "b")],
        vec![
            node("number"),
            node("string"),
            proxy("proofslot", "b", "slot", "b"),
        ],
        vec![proof, alternate, different],
    );
    dispatch_unit(
        &mut w,
        "d",
        "d",
        vec![selected("a", "a"), selected("c", "c"), selected("b", "b")],
        vec![node("composition")],
        vec![],
    );
    w.publish().unwrap();
    let pinned = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let request = serde_json::json!({"op":"callees","symbol_id":"caller"});
    let graph = query(&pinned, request.clone()).graph.unwrap();
    assert_eq!(graph.findings.len(), 2);
    let candidate = graph
        .findings
        .iter()
        .find(|f| f.symbol.id == "number")
        .unwrap();
    let edge = &candidate.paths[0][0];
    assert_eq!(edge.dispatch_context_id.as_deref(), Some("d"));
    assert_eq!(edge.dispatch_context_fingerprint.as_deref(), Some("d"));
    assert_eq!(edge.dispatch_key, "number");
    assert!(
        edge.dispatch_proof
            .as_ref()
            .unwrap()
            .target_definition_link
            .is_some()
    );
    assert!(
        query(
            &pinned,
            serde_json::json!({"op":"callers","symbol_id":"number"})
        )
        .graph
        .unwrap()
        .findings
        .iter()
        .any(|f| f.symbol.id == "caller")
    );
    let limited = query(
        &pinned,
        serde_json::json!({"op":"callees","symbol_id":"caller","max_edges":1}),
    );
    assert!(limited.truncated);
    assert_eq!(limited.graph.unwrap().findings[0].symbol.id, "slot");
    let mut w = Writer::open_incremental(&db, &["d".into()]).unwrap();
    let mut stale = selected("a", "a");
    stale.input_fingerprint = "old-a".into();
    dispatch_unit(
        &mut w,
        "d",
        "d",
        vec![stale, selected("c", "c"), selected("b", "b")],
        vec![node("composition")],
        vec![],
    );
    w.publish().unwrap();
    assert_eq!(
        query(&pinned, request.clone())
            .graph
            .unwrap()
            .findings
            .len(),
        2
    );
    let current = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    let g = query(&current, request).graph.unwrap();
    assert_eq!(g.findings.len(), 1);
    assert!(
        g.boundaries
            .iter()
            .any(|b| b.reason == "dispatch_context_unproven")
    );
}
#[test]
fn v5_dispatch_key_migration_preserves_pinned_body_queries() {
    let db = database("v5-dispatch");
    let mut w = Writer::open(&db).unwrap();
    populate(
        &mut w,
        "one",
        &["a", "b"],
        vec![edge("a", "b", 0, "compiler_bound")],
    );
    w.publish().unwrap();
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch("DROP INDEX relationship_evidence; ALTER TABLE relationships DROP COLUMN dispatch_key; DROP INDEX symbol_test_mask; ALTER TABLE symbols DROP COLUMN test_mask; ALTER TABLE compilations DROP COLUMN test_classification_json; ALTER TABLE unit_artifacts DROP COLUMN test_classification_json; DROP INDEX symbol_grouping; ALTER TABLE symbols DROP COLUMN grouping_key; ALTER TABLE symbols DROP COLUMN declaration_key; ALTER TABLE compilations DROP COLUMN target_family; ALTER TABLE compilations DROP COLUMN target_equivalence_policy; PRAGMA user_version=5;").unwrap();
    drop(c);
    let old = ReadView::open_in_scope(&db, &common::scope(&db)).unwrap();
    Writer::open_incremental(&db, &[])
        .unwrap()
        .publish()
        .unwrap();
    for view in [
        old,
        ReadView::open_in_scope(&db, &common::scope(&db)).unwrap(),
    ] {
        let g = query(&view, serde_json::json!({"op":"callees","symbol_id":"a"}))
            .graph
            .unwrap();
        assert_eq!(g.findings[0].symbol.id, "b");
        assert!(g.findings[0].paths[0][0].dispatch_key.is_empty());
    }
}
