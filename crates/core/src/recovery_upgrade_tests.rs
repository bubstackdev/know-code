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
    assert_eq!(lifecycle_version(&logical).unwrap(), Some(1));
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
    assert_eq!(lifecycle_version(&logical).unwrap(), Some(2));
    assert_eq!(converted["failed_reason"], "corruption");
    drop(guard);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn disk_sampling_observes_other_healthy_retired_wal_growth_and_cleanup() {
    let (root, logical) = fixture(2, Some(12));
    let old = IndexReadGuard::open_unbudgeted(&logical).unwrap();
    let upgrade = begin_upgrade(&logical, false, 2, 2).unwrap();
    let replacement = Connection::open(&upgrade.database).unwrap();
    replacement.execute_batch("PRAGMA user_version=8; PRAGMA journal_mode=WAL; CREATE TABLE state(current_generation INTEGER); INSERT INTO state VALUES(13);").unwrap();
    drop(replacement);
    reserve_generation(&logical, 13).unwrap();
    finish_rebuild(&upgrade, &Control::default(), || Ok(())).unwrap();
    let sibling = root
        .join("workspaces")
        .join("b".repeat(24))
        .join("index.db");
    fs::create_dir_all(sibling.parent().unwrap()).unwrap();
    adopt(&sibling, 0).unwrap();
    let budget = DiskBudget::new(&sibling, &root).unwrap();
    let limits = Limits {
        rebuild_disk_mib: 1,
        min_free_disk_mib: 0,
        ..Default::default()
    };
    budget.check(&limits, false).unwrap();
    let bookkeeping = Connection::open(old.physical_path()).unwrap();
    bookkeeping.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE cursor_growth(payload BLOB); INSERT INTO cursor_growth VALUES(zeroblob(2097152));").unwrap();
    assert!(
        family_path(old.physical_path(), "-wal")
            .metadata()
            .unwrap()
            .len()
            > 1024 * 1024
    );
    assert!(
        matches!(budget.check(&limits, false), Err(Error::ResourceLimited(_))),
        "retired family growth after budget construction must be observed"
    );
    assert!(
        old.check().is_ok(),
        "upgrade never poisons the old healthy reader"
    );
    drop(bookkeeping);
    let retired = old.physical_path().to_owned();
    cleanup(&logical, 13).unwrap();
    assert!(retired.exists());
    drop(old);
    cleanup(&logical, 13).unwrap();
    assert!(!retired.exists());
    budget.check(&limits, false).unwrap();
    fs::remove_dir_all(root).unwrap();
}

fn candidate_database(candidate: &Candidate, generation: u64) {
    let connection = Connection::open(&candidate.database).unwrap();
    connection.execute_batch("PRAGMA user_version=8; PRAGMA journal_mode=WAL; CREATE TABLE state(current_generation INTEGER);").unwrap();
    connection
        .execute("INSERT INTO state VALUES(?)", [generation])
        .unwrap();
}

#[test]
fn known_unpublished_old_formats_keep_the_rebuild_decision() {
    for source_format in [1, 2] {
        let (root, logical) = fixture(source_format, None);
        assert_eq!(selected_format(&logical).unwrap(), Some((source_format, 0)));
        assert_eq!(
            begin_upgrade(&logical, false, 0, source_format)
                .unwrap()
                .reason,
            RebuildReason::Upgrade {
                source_format,
                target_format: 8
            }
        );
        fs::remove_dir_all(root).unwrap();
    }
    let (root, logical) = fixture(8, None);
    assert_eq!(selected_format(&logical).unwrap(), None);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prepared_upgrade_seal_rejects_changed_receipts_or_physical_identity() {
    for changed in ["reason", "floor", "ready_generation", "identity"] {
        let (root, logical) = fixture(2, Some(12));
        let old = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        let candidate = begin_upgrade(&logical, false, 2, 2).unwrap();
        candidate_database(&candidate, 13);
        let prepared = prepare_rebuild(&candidate, &Control::default()).unwrap();
        assert_eq!(prepared.generation(), 13);
        let mut state = load(&logical).unwrap().unwrap();
        let pending = state.pending.as_mut().unwrap();
        match changed {
            "reason" => pending.reason = RebuildReason::Corruption,
            "floor" => pending.minimum_generation += 1,
            "ready_generation" => pending.ready_generation = Some(14),
            "identity" => {
                let held = candidate.database.with_extension("held");
                fs::rename(&candidate.database, &held).unwrap();
                fs::copy(&held, &candidate.database).unwrap();
            }
            _ => unreachable!(),
        }
        save(&logical, &state).unwrap();
        assert!(
            matches!(
                finish_prepared_rebuild(&prepared, &Control::default(), || Ok(())),
                Err(Error::InvalidRequest(_))
            ),
            "{changed} must invalidate the prepared seal"
        );
        assert_eq!(
            selected_incarnation(&logical).unwrap().as_deref(),
            Some(old.incarnation())
        );
        assert!(old.check().is_ok());
        drop(old);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn ready_upgrade_waits_for_publication_and_interruption_consumes_its_attempt() {
    let (root, logical) = fixture(2, Some(12));
    let old = IndexReadGuard::open_unbudgeted(&logical).unwrap();
    let candidate = begin_upgrade(&logical, false, 2, 2).unwrap();
    candidate_database(&candidate, 13);
    let prepared = prepare_rebuild(&candidate, &Control::default()).unwrap();
    assert_eq!(
        selected_incarnation(&logical).unwrap().as_deref(),
        Some(old.incarnation())
    );
    reconcile(&logical).unwrap();
    assert_eq!(failure_reason(&logical).unwrap(), Some(candidate.reason));
    assert!(finish_prepared_rebuild(&prepared, &Control::default(), || Ok(())).is_err());
    assert!(matches!(
        begin_upgrade(&logical, false, 2, 2),
        Err(Error::IndexRecovering(_))
    ));
    let retry = begin_upgrade(&logical, true, 2, 2).unwrap();
    assert_eq!(retry.minimum_generation, 14);
    candidate_database(&retry, 14);
    let prepared = prepare_rebuild(&retry, &Control::default()).unwrap();
    // A writer lock does not change the verified candidate. The short selector
    // phase must not reopen SQL to scan/checkpoint while cursor admission holds.
    let writer = Connection::open(&retry.database).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE;").unwrap();
    finish_prepared_rebuild(&prepared, &Control::default(), || Ok(())).unwrap();
    writer.execute_batch("ROLLBACK;").unwrap();
    drop(writer);
    assert_eq!(selected_replacement_generation(&logical).unwrap(), Some(14));
    assert!(!is_bad(&logical).unwrap());
    assert!(old.check().is_ok());
    drop(old);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_start_settles_its_exact_receipt_and_allows_explicit_retry_without_restart() {
    use std::os::unix::fs::PermissionsExt;
    for failure in ["directory_sync", "mkdir"] {
        let (root, logical) = fixture(2, Some(12));
        let old = IndexReadGuard::open_unbudgeted(&logical).unwrap();
        let indexes = logical.parent().unwrap().join("indexes");
        if failure == "directory_sync" {
            FAIL_NEXT_DIRECTORY_SYNC.with(|flag| flag.set(true));
        } else {
            fs::set_permissions(&indexes, fs::Permissions::from_mode(0o500)).unwrap();
        }
        let result = begin_upgrade(&logical, false, 2, 2);
        fs::set_permissions(&indexes, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(result, Err(Error::Io(_))), "{failure}: {result:?}");
        let reason = RebuildReason::Upgrade {
            source_format: 2,
            target_format: 8,
        };
        assert_eq!(failure_reason(&logical).unwrap(), Some(reason));
        assert!(load(&logical).unwrap().unwrap().pending.is_none());
        assert!(matches!(
            begin_upgrade(&logical, false, 2, 2),
            Err(Error::IndexRecovering(_))
        ));
        assert!(old.check().is_ok());
        let retry = begin_upgrade(&logical, true, 2, 2).unwrap();
        assert_eq!(retry.reason, reason);
        assert_eq!(retry.minimum_generation, 13);
        drop(old);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn unsupported_or_unmanaged_sources_never_authorize_replacement_or_rewrite_receipts() {
    for format in [-1, 0, 3, 7, 8, 999] {
        let (root, logical) = fixture(format, Some(12));
        let physical = selected_path(&logical).unwrap();
        if ![3, 7, 8].contains(&format) {
            // The probe may inspect SQLite's stable version header, but must
            // not interpret an unknown source's tables to find a generation.
            Connection::open(&physical)
                .unwrap()
                .execute_batch("DROP TABLE state;")
                .unwrap();
            assert_eq!(selected_format(&logical).unwrap(), Some((format, 0)));
        }
        let receipt = fs::read(state_path(&logical)).unwrap();
        let bytes = fs::read(&physical).unwrap();
        for explicit in [false, true] {
            assert!(matches!(
                begin_upgrade(&logical, explicit, 2, format),
                Err(Error::UpdateRequired(_))
            ));
            assert!(matches!(
                begin_upgrade(&logical, explicit, 2, 2),
                Err(Error::UpdateRequired(_))
            ));
            assert_eq!(fs::read(state_path(&logical)).unwrap(), receipt);
            assert_eq!(fs::read(&physical).unwrap(), bytes);
        }
        assert!(!is_bad(&logical).unwrap());
        assert_eq!(failure_reason(&logical).unwrap(), None);
        assert_eq!(
            fs::read_dir(logical.parent().unwrap().join("indexes"))
                .unwrap()
                .count(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }
    let (root, logical) = fixture(2, Some(12));
    fs::remove_file(state_path(&logical)).unwrap();
    assert!(matches!(
        selected_format(&logical),
        Err(Error::InvalidRequest(_))
    ));
    assert!(matches!(
        begin_upgrade(&logical, true, 0, 2),
        Err(Error::InvalidRequest(_))
    ));
    assert!(!state_path(&logical).exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_target_or_mutated_candidate_cannot_checkpoint_unowned_paths_or_publish() {
    let (root, logical) = fixture(2, Some(12));
    let old = IndexReadGuard::open_unbudgeted(&logical).unwrap();
    let candidate = begin_upgrade(&logical, false, 2, 2).unwrap();
    candidate_database(&candidate, 13);
    let mut different = candidate.clone();
    different.database = old.physical_path().into();
    assert!(matches!(
        prepare_rebuild(&different, &Control::default()),
        Err(Error::InvalidRequest(_))
    ));
    let different = Connection::open(&candidate.database).unwrap();
    different.execute_batch("PRAGMA user_version=999;").unwrap();
    drop(different);
    assert!(matches!(
        prepare_rebuild(&candidate, &Control::default()),
        Err(Error::UpdateRequired(_))
    ));
    assert!(
        load(&logical)
            .unwrap()
            .unwrap()
            .pending
            .unwrap()
            .ready_generation
            .is_none()
    );
    assert!(old.check().is_ok());
    assert_eq!(selected_format(&logical).unwrap(), Some((2, 12)));
    drop(old);
    fs::remove_dir_all(root).unwrap();
}
