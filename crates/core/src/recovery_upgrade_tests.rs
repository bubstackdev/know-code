use super::*;
use rusqlite::Connection;

fn fixture(format: i32, generation: Option<u64>) -> (PathBuf, PathBuf) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.artifacts/upgrade-lifecycle-unit")
        .join(nonce().unwrap());
    let logical = root
        .join("workspaces")
        .join("a".repeat(24))
        .join("index.db");
    fs::create_dir_all(logical.parent().unwrap()).unwrap();
    adopt(&logical, 0).unwrap();
    let physical = selected_path(&logical).unwrap();
    let connection = Connection::open(&physical).unwrap();
    connection.execute_batch(&format!("PRAGMA user_version={format}; CREATE TABLE state(singleton INTEGER PRIMARY KEY,current_generation INTEGER); INSERT INTO state VALUES(1,NULL); CREATE TABLE facts(value TEXT); INSERT INTO facts VALUES('saved original');")).unwrap();
    connection
        .execute("UPDATE state SET current_generation=?", [generation])
        .unwrap();
    drop(connection);
    // Deliberately lag the registry/high-water behind the serving SQLite value.
    reserve_generation(&logical, 2).unwrap();
    (root, logical)
}

#[test]
fn guarded_upgrade_records_exact_cause_and_actual_generation_without_bad() {
    for source_format in [1, 2] {
        let (root, logical) = fixture(source_format, Some(12));
        assert_eq!(
            selected_format(&logical).unwrap(),
            Some((source_format, 12))
        );
        let old = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        let candidate = begin_upgrade(&logical, false, 3, source_format).unwrap();
        let reason = RebuildReason::Upgrade {
            source_format,
            target_format: 8,
        };
        assert_eq!(candidate.reason, reason);
        assert_eq!(candidate.minimum_generation, 13);
        assert_eq!(replacement_reason(&logical).unwrap(), Some(reason));
        assert_eq!(failure_reason(&logical).unwrap(), None);
        assert!(!is_bad(&logical).unwrap());
        fail_rebuild(&logical, "bounded worker failure").unwrap();
        assert_eq!(failure_reason(&logical).unwrap(), Some(reason));
        assert_eq!(replacement_reason(&logical).unwrap(), Some(reason));
        assert!(matches!(
            begin_upgrade(&logical, false, 3, source_format),
            Err(Error::IndexRecovering(_))
        ));
        let current = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        assert_eq!(current.incarnation(), old.incarnation());
        assert_eq!(
            Connection::open(current.physical_path())
                .unwrap()
                .query_row("SELECT value FROM facts", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "saved original"
        );
        let retry = begin_upgrade(&logical, true, 3, source_format).unwrap();
        assert_eq!(retry.reason, reason);
        assert_eq!(retry.minimum_generation, 13);
        reconcile(&logical).unwrap();
        assert_eq!(failure_reason(&logical).unwrap(), Some(reason));
        assert!(old.check().is_ok());
        drop((old, current));
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn future_receipts_are_rejected_before_strict_decode_and_v1_causes_are_deliberate() {
    let (root, logical) = fixture(2, Some(4));
    let path = state_path(&logical);
    let original = fs::read(&path).unwrap();
    let future = br#"{"version":999,"new_future_payload":{"opaque":[1,2,3]}}"#;
    fs::write(&path, future).unwrap();
    assert!(matches!(
        selected_format(&logical),
        Err(Error::UpdateRequired(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), future);
    fs::write(&path, &original).unwrap();
    let guard = IndexReadGuard::open_unbudgeted(&logical).unwrap();
    assert!(guard.note_corruption("owned corruption proof").is_err());
    begin_rebuild(&logical, false, 4).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy["version"] = 1.into();
    legacy.as_object_mut().unwrap().remove("failed_reason");
    legacy["pending"].as_object_mut().unwrap().remove("reason");
    let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(&path, &legacy_bytes).unwrap();
    assert_eq!(
        replacement_reason(&logical).unwrap(),
        Some(RebuildReason::Corruption)
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        legacy_bytes,
        "read must not rewrite a legacy receipt"
    );
    reconcile(&logical).unwrap();
    assert_eq!(
        failure_reason(&logical).unwrap(),
        Some(RebuildReason::Corruption)
    );
    let converted: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(converted["version"], 2);
    assert_eq!(converted["failed_reason"], "corruption");
    drop(guard);
    fs::remove_dir_all(root).unwrap();
}
