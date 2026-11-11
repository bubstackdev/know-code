use know_code_core::{
    Error,
    protocol::{envelope::Payload, *},
    query::{QueryRequest, ReadView},
    recovery::{self, IndexReadGuard},
    resource::Control,
    store::Writer,
};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
fn database() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.artifacts/index-read-guards")
        .join(format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .join("workspaces")
        .join("a".repeat(24))
        .join("index.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}
fn populate(writer: &mut Writer, text: &str) {
    writer
        .stage(Payload::Compilation(Compilation {
            id: "c".into(),
            language: "typescript".into(),
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Source(SourceArtifact {
            id: "a".into(),
            path: "a.ts".into(),
            utf8: text.as_bytes().to_vec(),
            sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
            compilation_id: "c".into(),
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: "c".into(),
            symbols: vec![Symbol {
                id: "s".into(),
                name: "needle".into(),
                signature: text.into(),
                ..Default::default()
            }],
            occurrences: vec![
                Occurrence {
                    symbol_id: "s".into(),
                    artifact_id: "a".into(),
                    role: "declaration".into(),
                    end_byte: 6,
                    ..Default::default()
                },
                Occurrence {
                    symbol_id: "s".into(),
                    artifact_id: "a".into(),
                    role: "definition".into(),
                    end_byte: text.len() as u64,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Report(AnalysisReport {
            compilation_id: "c".into(),
            status: "complete".into(),
            ..Default::default()
        }))
        .unwrap();
}
fn request(value: serde_json::Value) -> QueryRequest {
    serde_json::from_value(value).unwrap()
}
fn lookup(view: &ReadView) -> know_code_core::query::QueryResult {
    view.query(request(serde_json::json!({"op":"symbols","name":"needle"})))
        .unwrap()
}
#[test]
fn snapshots_bind_physical_incarnation_even_when_legacy_generations_repeat() {
    let logical = database();
    let mut first = Writer::open(&logical).unwrap();
    populate(&mut first, "needle original 😀");
    let generation = first.publish().unwrap();
    // Legacy publication was never recorded by the registry/lifecycle counter.
    recovery::adopt(&logical, 0).unwrap();
    let held = ReadView::open(&logical).unwrap();
    let original = lookup(&held);
    let token = original.snapshot.unwrap();
    let canonical_handle = logical
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join("index.db");
    assert_eq!(
        lookup(&ReadView::open(canonical_handle).unwrap())
            .snapshot
            .as_ref(),
        Some(&token)
    );
    assert_eq!(generation, original.generation);
    let scoped = request(
        serde_json::json!({"op":"symbol_context","symbol_id":"s","snapshot":token,"include_source":false}),
    );
    assert_eq!(
        held.query(scoped).unwrap().contexts[0].symbol.signature,
        "needle original 😀"
    );
    assert!(matches!(
        held.query(request(
            serde_json::json!({"op":"symbols","generation":generation})
        )),
        Err(Error::InvalidRequest(_))
    ));
    let guard = IndexReadGuard::open_unbudgeted(&logical).unwrap();
    std::fs::write(guard.physical_path(), b"owned corrupt header").unwrap();
    let db = rusqlite::Connection::open(guard.physical_path()).unwrap();
    let error = db
        .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
        .unwrap_err();
    assert!(matches!(
        guard.map_sqlite(error),
        Error::IndexCorrupt { .. }
    ));
    drop(db);
    assert!(matches!(
        held.query(request(serde_json::json!({"op":"symbols"}))),
        Err(Error::IndexCorrupt { .. })
    ));
    let envelope: serde_json::Value = serde_json::from_str(&know_code_core::query_json(
        logical.to_str().unwrap(),
        r#"{"op":"symbols","freshness_wait_ms":0}"#,
    ))
    .unwrap();
    assert_eq!(envelope["error"]["code"], "index_corrupt");
    let candidate = recovery::begin_rebuild(&logical, false, 0).unwrap();
    let mut replacement = Writer::open_destination(
        &logical,
        &candidate.database,
        None,
        candidate.minimum_generation,
        &Control::default(),
    )
    .unwrap();
    populate(&mut replacement, "needle replacement 😀");
    assert_eq!(
        replacement.publish().unwrap(),
        generation,
        "fixture must exercise numeric reuse"
    );
    recovery::finish_rebuild(&candidate, &Control::default(), || Ok(())).unwrap();
    let current = ReadView::open(&logical).unwrap();
    let refreshed = lookup(&current);
    assert_eq!(refreshed.generation, generation);
    assert_ne!(refreshed.snapshot.as_ref(), Some(&token));
    assert!(matches!(current.query(request(serde_json::json!({"op":"symbol_context","symbol_id":"s","generation":generation,"snapshot":token}))),Err(Error::SnapshotInvalidated)));
    assert!(matches!(
        held.query(request(serde_json::json!({"op":"symbols"}))),
        Err(Error::IndexCorrupt { .. })
    ));
    let context=current.query(request(serde_json::json!({"op":"symbol_context","symbol_id":"s","snapshot":refreshed.snapshot}))).unwrap();
    assert_eq!(
        context.contexts[0].declarations[0]
            .source
            .as_ref()
            .unwrap()
            .text,
        "needle replacement 😀"
    );
}
#[test]
fn ordinary_publication_rejects_old_tokens_but_held_coherent_views_still_work() {
    let logical = database();
    recovery::adopt(&logical, 0).unwrap();
    let mut writer = Writer::open(&logical).unwrap();
    populate(&mut writer, "needle before");
    writer.publish().unwrap();
    assert!(
        !logical.exists(),
        "new managed indexes should use physical incarnations"
    );
    let held = ReadView::open(&logical).unwrap();
    let previous = lookup(&held);
    let token = previous.snapshot.unwrap();
    let mut writer = Writer::open(&logical).unwrap();
    populate(&mut writer, "needle after");
    let next = writer.publish().unwrap();
    assert!(next > previous.generation);
    let current = ReadView::open(&logical).unwrap();
    assert!(matches!(
        current.query(request(
            serde_json::json!({"op":"symbols","snapshot":token})
        )),
        Err(Error::SnapshotInvalidated)
    ));
    let source = held
        .query(request(
            serde_json::json!({"op":"source","artifact_id":"a","snapshot":token}),
        ))
        .unwrap()
        .source
        .unwrap();
    assert_eq!(source.text, "needle before");
}
#[test]
fn a_bad_latch_interrupts_active_sql_without_waiting_for_the_query_deadline() {
    let logical = database();
    recovery::adopt(&logical, 0).unwrap();
    let mut writer = Writer::open(&logical).unwrap();
    populate(&mut writer, "needle current");
    writer.publish().unwrap();
    let token = lookup(&ReadView::open(&logical).unwrap()).snapshot.unwrap();
    let guard = IndexReadGuard::open_unbudgeted(&logical).unwrap();
    let db = rusqlite::Connection::open(guard.physical_path()).unwrap();
    db.execute_batch("ALTER TABLE reports RENAME TO fixture_reports; CREATE VIEW reports AS WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<100000000) SELECT r.* FROM fixture_reports r CROSS JOIN n;").unwrap();
    drop(db);
    let view = ReadView::open(&logical).unwrap();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(matches!(
            guard.note_corruption("owned concurrent fixture proof"),
            Err(Error::IndexCorrupt { .. })
        ));
    });
    let started = std::time::Instant::now();
    assert!(matches!(
        view.query(request(
            serde_json::json!({"op":"coverage","snapshot":token,"deadline_ms":5000})
        )),
        Err(Error::IndexCorrupt { .. })
    ));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "BAD latch waited for the overall deadline"
    );
    thread.join().unwrap();
}

#[test]
fn expired_low_level_view_releases_sql_but_keeps_physical_lease_until_drop() {
    let logical = database();
    recovery::adopt(&logical, 0).unwrap();
    let mut writer = Writer::open(&logical).unwrap();
    populate(&mut writer, "needle current");
    writer.publish().unwrap();
    let physical = recovery::selected_path(&logical).unwrap();
    let view = ReadView::open(&logical).unwrap();
    assert!(matches!(
        view.query(request(serde_json::json!({"op":"symbols","deadline_ms":0}))),
        Err(Error::DeadlineExceeded)
    ));
    let probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(physical.parent().unwrap().join("readers.lock"))
        .unwrap();
    assert!(matches!(
        probe.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(view);
    probe.try_lock().unwrap();
}
