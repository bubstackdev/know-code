use know_code_core::{
    cursor::GenerationRetention,
    protocol::{envelope::Payload, *},
    query::{QueryRequest, ReadView},
    store::Writer,
};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

fn database() -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.artifacts/core-tests")
        .join(name)
        .join("index.db")
}
fn request(op: &str) -> QueryRequest {
    serde_json::from_str(&format!(r#"{{"op":"{op}","name":"Greeter"}}"#)).unwrap()
}
fn populate(writer: &mut Writer, text: &str) {
    writer
        .stage(Payload::Compilation(Compilation {
            id: "comp".into(),
            language: "typescript".into(),
            project_path: "tsconfig.json".into(),
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Source(SourceArtifact {
            test_classification: None,
            id: "source".into(),
            path: "a.ts".into(),
            utf8: text.as_bytes().into(),
            sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
            generated: false,
            compilation_id: "comp".into(),
        }))
        .unwrap();
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: "comp".into(),
            symbols: vec![Symbol {
                id: "symbol".into(),
                name: "Greeter".into(),
                kind: "class".into(),
                ..Default::default()
            }],
            occurrences: vec![Occurrence {
                symbol_id: "symbol".into(),
                artifact_id: "source".into(),
                start_byte: 0,
                end_byte: 7,
                role: "declaration".into(),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Report(AnalysisReport {
            compilation_id: "comp".into(),
            status: "complete".into(),
            ..Default::default()
        }))
        .unwrap();
}

#[test]
fn published_readers_keep_matching_source_across_replacement() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter old");
    let first = writer.publish().unwrap();
    let pinned = ReadView::open(&path).unwrap();
    let mut replacement = Writer::open(&path).unwrap();
    populate(&mut replacement, "Greeter new");
    let second = replacement.publish().unwrap();
    assert!(second > first);
    assert_eq!(pinned.query(request("symbols")).unwrap().generation, first);
    let source: QueryRequest =
        serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap();
    assert_eq!(
        pinned.query(source).unwrap().source.unwrap().text,
        "Greeter old"
    );
    let new = ReadView::open(&path).unwrap();
    assert_eq!(
        new.query(serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap())
            .unwrap()
            .source
            .unwrap()
            .text,
        "Greeter new"
    );
}

#[test]
fn query_deadline_interrupts_sql_and_releases_a_retained_view() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter");
    writer.publish().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    // An owned fixture amplifies coverage SQL without allocating a huge index.
    connection
        .execute_batch(
            "ALTER TABLE reports RENAME TO fixture_reports;
        CREATE VIEW reports AS WITH RECURSIVE n(i) AS
        (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<100000000)
        SELECT r.* FROM fixture_reports r CROSS JOIN n;",
        )
        .unwrap();
    let view = ReadView::open(&path).unwrap();
    connection
        .execute("UPDATE state SET current_generation=current_generation", [])
        .unwrap();
    let mut timed = request("symbols");
    timed.deadline_ms = Some(30);
    let start = std::time::Instant::now();
    assert!(matches!(
        view.query(timed),
        Err(know_code_core::Error::DeadlineExceeded)
    ));
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    let busy: i32 = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .unwrap();
    assert_eq!(busy, 0, "expired retained view still pinned WAL");
    assert!(matches!(
        view.query(request("symbols")),
        Err(know_code_core::Error::DeadlineExceeded)
    ));
    connection
        .execute_batch("DROP VIEW reports; ALTER TABLE fixture_reports RENAME TO reports;")
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&know_code_core::query_json(
        path.to_str().unwrap(),
        r#"{"op":"symbols","name":"Greeter"}"#,
    ))
    .unwrap();
    assert_eq!(result["result"]["symbols"][0]["name"], "Greeter");
}

#[test]
fn response_budget_limits_the_whole_result_and_keeps_the_view_reusable() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter");
    writer.publish().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("UPDATE symbols SET signature=?", ["\0😀".repeat(40_000)])
        .unwrap();
    let view = ReadView::open(&path).unwrap();
    assert!(matches!(
        view.query(request("symbols")),
        Err(know_code_core::Error::ResponseLimitExceeded)
    ));
    let mut larger = request("symbols");
    larger.response_max_bytes = Some(2_097_152);
    assert!(!view.query(larger).unwrap().symbols.is_empty());
    let response: serde_json::Value = serde_json::from_str(&know_code_core::query_json(
        path.to_str().unwrap(),
        r#"{"op":"symbols","name":"Greeter"}"#,
    ))
    .unwrap();
    assert_eq!(response["error"]["code"], "response_limit_exceeded");
    let json = know_code_core::query_json(
        path.to_str().unwrap(),
        r#"{"op":"symbols","name":"Greeter","response_max_bytes":2097152}"#,
    );
    assert!(json.len() <= 2_097_152);
    let response: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(!response["result"]["symbols"].as_array().unwrap().is_empty());
}

#[test]
fn cancellation_interrupts_active_sql_releases_wal_and_preserves_published_data() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter");
    writer.publish().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE reports RENAME TO fixture_reports;
      CREATE VIEW reports AS WITH RECURSIVE n(i) AS
      (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<100000000)
      SELECT r.* FROM fixture_reports r CROSS JOIN n;",
        )
        .unwrap();
    let view = ReadView::open(&path).unwrap();
    connection
        .execute("UPDATE state SET current_generation=current_generation", [])
        .unwrap();
    let cancellation = know_code_core::execution::QueryCancellation::default();
    let signal = cancellation.clone();
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        signal.cancel();
    });
    assert!(matches!(
        view.query_cancellable(request("symbols"), cancellation),
        Err(know_code_core::Error::QueryCancelled)
    ));
    cancel.join().unwrap();
    let busy: i32 = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .unwrap();
    assert_eq!(busy, 0, "cancelled read retained its WAL transaction");
    assert!(matches!(
        view.query(request("symbols")),
        Err(know_code_core::Error::QueryCancelled)
    ));
    connection
        .execute_batch("DROP VIEW reports; ALTER TABLE fixture_reports RENAME TO reports;")
        .unwrap();
    assert_eq!(
        ReadView::open(&path)
            .unwrap()
            .query(request("symbols"))
            .unwrap()
            .symbols[0]
            .name,
        "Greeter"
    );
}

#[test]
fn retained_generations_reopen_after_publication_without_holding_wal_readers() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter old");
    let first = writer.publish().unwrap();
    let retention = GenerationRetention::activate(&path, [1; 16]).unwrap();
    let pin = retention.pin_current().unwrap();
    let sibling = retention.pin_current().unwrap();
    assert_eq!(pin.generation(), first);
    for text in ["Greeter next", "Greeter latest"] {
        let mut writer = Writer::open(&path).unwrap();
        populate(&mut writer, text);
        assert!(writer.publish().unwrap() > first);
        let view = pin.open_view().unwrap();
        let source = view
            .query(serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap())
            .unwrap();
        assert_eq!(source.generation, first);
        assert_eq!(source.source.unwrap().text, "Greeter old");
    }
    retention.release(&pin).unwrap();
    assert!(matches!(
        pin.open_view(),
        Err(know_code_core::Error::CursorInvalidated)
    ));
    let active = sibling.open_view().unwrap();
    retention.release(&pin).unwrap();
    retention.release(&sibling).unwrap();
    assert!(matches!(
        sibling.open_view(),
        Err(know_code_core::Error::CursorInvalidated)
    ));
    assert_eq!(
        active
            .query(serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap())
            .unwrap()
            .source
            .unwrap()
            .text,
        "Greeter old"
    );
    drop(active);
    let connection = rusqlite::Connection::open(&path).unwrap();
    let contents: u32 = connection
        .query_row("SELECT count(*) FROM contents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(contents, 1, "released old source was not reclaimed");
    let busy: u32 = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
        .unwrap();
    assert_eq!(busy, 0);
    let violations: u32 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn retention_cleanup_preserves_an_empty_active_indexing_stage() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter old");
    writer.publish().unwrap();
    let mut writer = Writer::open(&path).unwrap();
    let staged = writer.generation;
    // Retention may first activate while an existing job has no fact units yet.
    let retention = GenerationRetention::activate(&path, [1; 16]).unwrap();
    let pin = retention.pin_current().unwrap();
    retention.release(&pin).unwrap();
    populate(&mut writer, "Greeter new");
    assert_eq!(writer.publish().unwrap(), staged);
    let connection = rusqlite::Connection::open(&path).unwrap();
    let stages: u32 = connection
        .query_row("SELECT count(*) FROM staging_generations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stages, 0);
}

#[test]
fn new_retention_owner_invalidates_old_pins_and_keeps_current_index() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter old");
    writer.publish().unwrap();
    let old = GenerationRetention::activate(&path, [1; 16]).unwrap();
    let pin = old.pin_current().unwrap();
    GenerationRetention::activate(&path, [1; 16]).unwrap();
    assert!(
        pin.open_view().is_ok(),
        "same owner activation invalidated a pin"
    );
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter new");
    let current = writer.publish().unwrap();
    let next = GenerationRetention::activate(&path, [2; 16]).unwrap();
    assert!(matches!(
        pin.open_view(),
        Err(know_code_core::Error::CursorInvalidated)
    ));
    assert!(matches!(
        old.pin_current(),
        Err(know_code_core::Error::CursorInvalidated)
    ));
    assert_eq!(next.pin_current().unwrap().generation(), current);
    assert_eq!(ReadView::open(&path).unwrap().generation, current);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("UPDATE cursor_retention_state SET version=99", [])
        .unwrap();
    assert!(matches!(
        Writer::open(&path),
        Err(know_code_core::Error::UpdateRequired(_))
    ));
    assert_eq!(ReadView::open(&path).unwrap().generation, current);
}

#[test]
fn incomplete_or_failed_analysis_preserves_published_generation() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter old");
    let generation = writer.publish().unwrap();
    {
        let mut failed = Writer::open(&path).unwrap();
        populate(&mut failed, "Greeter new");
        assert!(
            Writer::open(&path).is_err(),
            "second writer acquired ownership"
        );
        assert!(
            failed
                .stage(Payload::Source(SourceArtifact {
                    id: "bad".into(),
                    utf8: b"bad".to_vec(),
                    sha256: "wrong".into(),
                    ..Default::default()
                }))
                .is_err()
        );
        // Simulate worker failure: drop staging without a completion/publication.
    }
    let view = ReadView::open(&path).unwrap();
    assert_eq!(view.generation, generation);
    assert_eq!(view.query(request("symbols")).unwrap().symbols.len(), 1);
}

#[test]
fn missing_reports_and_truncated_protocol_never_count_as_ready() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    writer
        .stage(Payload::Compilation(Compilation {
            id: "unfinished".into(),
            ..Default::default()
        }))
        .unwrap();
    assert!(writer.publish().is_err());
    assert!(matches!(
        ReadView::open(&path),
        Err(know_code_core::Error::NotReady)
    ));
    let mut truncated = std::io::Cursor::new(vec![10, 0, 0, 0, 1, 2]);
    assert!(read_frame(&mut truncated).is_err());
    let mut oversized = std::io::Cursor::new(((MAX_FRAME + 1) as u32).to_le_bytes());
    assert!(read_frame(&mut oversized).is_err());
}

#[test]
fn incremental_publication_reuses_unchanged_units_and_reclaims_replaced_facts() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "Greeter old");
    writer
        .stage(Payload::Compilation(Compilation {
            id: "other".into(),
            project_path: "other/tsconfig.json".into(),
            target: "alternate".into(),
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: "other".into(),
            symbols: vec![Symbol {
                id: "other-symbol".into(),
                name: "Other".into(),
                signature: "Greeter Other".into(),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Report(AnalysisReport {
            compilation_id: "other".into(),
            status: "complete".into(),
            ..Default::default()
        }))
        .unwrap();
    let first = writer.publish().unwrap();
    let pinned = ReadView::open(&path).unwrap();
    let mut writer = Writer::open_incremental(&path, &["tsconfig.json".into()]).unwrap();
    populate(&mut writer, "Greeter new");
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: "comp".into(),
            symbols: vec![Symbol {
                id: "new-symbol".into(),
                name: "New".into(),
                signature: "Greeter New".into(),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .unwrap();
    let second = writer.publish().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let owner:u64=connection.query_row("SELECT owner_generation FROM generation_units WHERE generation=? AND compilation_id='other'",[second],|r|r.get(0)).unwrap();
    assert_eq!(
        owner, first,
        "unchanged facts were copied instead of reused"
    );
    let symbols: u64 = connection
        .query_row("SELECT count(*) FROM symbols", [], |r| r.get(0))
        .unwrap();
    assert_eq!(symbols, 3, "obsolete replaced facts were retained");
    assert_eq!(
        pinned
            .query(serde_json::from_str(r#"{"op":"source","artifact_id":"source"}"#).unwrap())
            .unwrap()
            .source
            .unwrap()
            .text,
        "Greeter old"
    );
    assert_eq!(
        ReadView::open(&path)
            .unwrap()
            .query(request("symbols"))
            .unwrap()
            .symbols
            .len(),
        1
    );
    let current = ReadView::open(&path).unwrap();
    let search =
        |view: &ReadView, json: &str| view.query(serde_json::from_str(json).unwrap()).unwrap();
    assert_eq!(
        search(&pinned, r#"{"op":"symbols","query":"Greeter"}"#)
            .symbols
            .len(),
        2
    );
    let found = search(&current, r#"{"op":"symbols","query":"Greeter"}"#);
    assert_eq!(
        found
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["Greeter", "New", "Other"]
    );
    let found = search(
        &current,
        r#"{"op":"symbols","name":"Other","query":"Greeter","target":"alternate"}"#,
    );
    assert_eq!(found.symbols.len(), 1);
    assert_eq!(found.symbols[0].compilation_id, "other");
    assert!(
        search(
            &current,
            r#"{"op":"symbols","name":"Other","query":"Greeter","target":""}"#
        )
        .symbols
        .is_empty()
    );
    let limited = search(&current, r#"{"op":"symbols","query":"Greeter","limit":1}"#);
    assert!(limited.truncated);
    assert_eq!(limited.symbols[0].name, "Greeter");
}

fn legacy_database(version: i32) -> PathBuf {
    let path = database();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("../../../tests/fixtures/schema-v2.sql"))
        .unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=WAL;")
        .unwrap();
    let text = "Greeter old";
    let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
    connection.execute_batch("INSERT INTO generations VALUES(1); UPDATE state SET current_generation=1; INSERT INTO compilations(generation,id,language,project_path,target,configuration,inferred,compiler_version,options_json) VALUES(1,'comp','typescript','tsconfig.json','','',0,'test','{}'); INSERT INTO generation_units VALUES(1,'comp',1);").unwrap();
    connection
        .execute(
            "INSERT INTO contents VALUES(?,?)",
            rusqlite::params![hash, text.as_bytes()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO artifacts VALUES(1,'source','a.ts',?,0)",
            [hash],
        )
        .unwrap();
    connection.execute_batch("INSERT INTO unit_artifacts VALUES(1,'comp','source'); INSERT INTO symbols(generation,compilation_id,id,name,kind,signature,external) VALUES(1,'comp','symbol','Greeter','class','Greeter',0); INSERT INTO occurrences VALUES(1,'comp','symbol','source',0,7,'declaration',''); INSERT INTO occurrences VALUES(1,'comp','symbol','source',0,7,'reference',''); INSERT INTO relationships VALUES(1,'comp','symbol','symbol','test'); INSERT INTO reports VALUES(1,'comp','complete','[]');").unwrap();
    if version == 1 {
        connection.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE generation_units; DROP TABLE unit_artifacts; ALTER TABLE compilations DROP COLUMN dependencies_json; ALTER TABLE compilations DROP COLUMN input_paths_json; PRAGMA user_version=1;").unwrap();
    }
    path
}

#[test]
fn legacy_schemas_migrate_without_reanalyzing_or_changing_public_ids() {
    for version in [1, 2] {
        let path = legacy_database(version);
        let compatible = (version == 2).then(|| ReadView::open(&path).unwrap());
        if let Some(view) = &compatible {
            assert_eq!(
                view.query(request("symbols")).unwrap().symbols[0].id,
                "symbol"
            );
        }
        let pinned = rusqlite::Connection::open(&path).unwrap();
        pinned
            .execute_batch("BEGIN; SELECT * FROM occurrences;")
            .unwrap();
        let writer = Writer::open_incremental(&path, &[]).unwrap();
        writer.publish().unwrap();
        let view = ReadView::open(&path).unwrap();
        let symbols = view.query(request("symbols")).unwrap().symbols;
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].id, "symbol");
        assert_eq!(symbols[0].locations[0].artifact_id, "source");
        let references =
            serde_json::from_str(r#"{"op":"references","symbol_id":"symbol"}"#).unwrap();
        assert_eq!(view.query(references).unwrap().occurrences.len(), 1);
        let old_symbol: String = pinned
            .query_row("SELECT symbol_id FROM occurrences LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(old_symbol, "symbol");
        if let Some(view) = &compatible {
            assert_eq!(
                view.query(request("symbols")).unwrap().symbols[0].id,
                "symbol"
            );
        }
        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM relationships", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(
            !connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap()
        );
    }
}

#[test]
fn source_excerpts_are_bounded_unicode_safe_and_hash_guarded() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    let text = "abc😀éxyz";
    populate(&mut writer, text);
    writer.publish().unwrap();
    let view = ReadView::open(&path).unwrap();
    let hash = format!("{:x}", Sha256::digest(text));
    let query = |v: serde_json::Value| view.query(serde_json::from_value(v).unwrap());
    let prefix =
        query(serde_json::json!({"op":"source","artifact_id":"source","max_bytes":4})).unwrap();
    assert!(prefix.truncated);
    let source = prefix.source.unwrap();
    assert_eq!(source.text, "abc");
    assert_eq!(
        (source.start_byte, source.end_byte, source.total_bytes),
        (0, 3, text.len() as u64)
    );
    let range=query(serde_json::json!({"op":"source","artifact_id":"source","content_hash":hash,"start_byte":3,"end_byte":7,"max_bytes":4})).unwrap();
    assert!(!range.truncated);
    assert_eq!(range.source.unwrap().text, "😀");
    for extra in [
        serde_json::json!({"start_byte":3}),
        serde_json::json!({"start_byte":4,"content_hash":hash}),
        serde_json::json!({"end_byte":4,"content_hash":hash}),
        serde_json::json!({"end_byte":99,"content_hash":hash}),
        serde_json::json!({"content_hash":"wrong"}),
        serde_json::json!({"max_bytes":0}),
        serde_json::json!({"max_bytes":262145}),
        serde_json::json!({"start_byte":3,"content_hash":hash,"max_bytes":3}),
    ] {
        let mut value = serde_json::json!({"op":"source","artifact_id":"source"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(query(value).is_err());
    }
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "changed source");
    writer.publish().unwrap();
    assert_eq!(query(serde_json::json!({"op":"source","artifact_id":"source","content_hash":hash,"start_byte":3,"end_byte":7})).unwrap().source.unwrap().text,"😀");
    let current = ReadView::open(&path).unwrap();
    assert!(current.query(serde_json::from_value(serde_json::json!({"op":"source","artifact_id":"source","content_hash":hash,"start_byte":3,"end_byte":7})).unwrap()).is_err());
}

#[test]
fn source_default_budget_does_not_materialize_an_unbounded_rust_result() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    let text = "a".repeat(know_code_core::query::DEFAULT_SOURCE_BYTES as usize + 100);
    populate(&mut writer, &text);
    writer.publish().unwrap();
    let view = ReadView::open(&path).unwrap();
    let result = view
        .query(
            serde_json::from_value(serde_json::json!({"op":"source","artifact_id":"source"}))
                .unwrap(),
        )
        .unwrap();
    assert!(result.truncated);
    assert_eq!(
        result.source.unwrap().text.len(),
        know_code_core::query::DEFAULT_SOURCE_BYTES as usize
    );
}

#[test]
fn legacy_worker_cannot_publish_when_new_policy_is_required() {
    let path = database();
    let mut writer = Writer::open(&path).unwrap();
    populate(&mut writer, "retained source");
    let published = writer.publish().unwrap();
    let response_path = path.with_extension("worker-frames");
    let mut response = std::fs::File::create(&response_path).unwrap();
    for payload in [
        Payload::Provider(ProviderInfo {
            language: "csharp".into(),
            ..Default::default()
        }),
        Payload::Complete(Completion { success: true }),
    ] {
        write_frame(
            &mut response,
            &Envelope {
                protocol_version: VERSION,
                job_id: format!("{}-0", published + 1),
                payload: Some(payload),
            },
        )
        .unwrap();
    }
    let result = know_code_core::update::index(
        &path,
        vec![know_code_core::update::WorkerCommand {
            program: "/bin/cat".into(),
            // Retain stdin until the parent finishes writing its request.
            args: vec![
                response_path.to_string_lossy().into_owned(),
                "/dev/stdin".into(),
            ],
            request: AnalyzeRequest {
                workspace_root: path.parent().unwrap().to_string_lossy().into_owned(),
                analysis_policy: Some(AnalysisPolicy {
                    version: ANALYSIS_POLICY_VERSION,
                    settings: Some(analysis_policy::Settings::Csharp(
                        CSharpBuildContext::default(),
                    )),
                }),
                ..Default::default()
            },
        }],
    );
    assert!(
        matches!(result, Err(know_code_core::Error::UpdateRequired(_))),
        "{result:?}"
    );
    let current = ReadView::open(&path)
        .unwrap()
        .query(request("symbols"))
        .unwrap();
    assert_eq!(current.generation, published);
    assert_eq!(current.symbols.len(), 1);
}
