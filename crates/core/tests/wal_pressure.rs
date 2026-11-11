//! Manual storage observation, not compiler/native throughput or a disk-cap SLO.
use know_code_core::{
    admission::AdmissionScope,
    cursor::GenerationRetention,
    protocol::{
        AnalysisReport, Compilation, FactBatch, Occurrence, SourceArtifact, Symbol,
        envelope::Payload,
    },
    query::{MAX_SOURCE_BYTES, QueryRequest, ReadView},
    store::Writer,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const LARGE: usize = 1024 * 1024;
const WATCHDOG: Duration = Duration::from_secs(180);

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn source(revision: usize, length: usize) -> String {
    let header = format!("/* saved revision {revision:04} π😀\0\n");
    let footer = format!("\n*/\nexport function pressureTarget() {{ return {revision}; }}\n");
    assert!(length > header.len() + footer.len());
    let mut text = header;
    text.extend(std::iter::repeat_n(
        char::from(b'a' + revision as u8),
        length - text.len() - footer.len(),
    ));
    text.push_str(&footer);
    assert_eq!(text.len(), length);
    text
}
fn publish(database: &Path, text: &str, revision: usize) -> u64 {
    let mut writer = Writer::open(database).unwrap();
    writer
        .stage(Payload::Compilation(Compilation {
            id: "fixture".into(),
            language: "typescript".into(),
            project_path: "fixture.ts".into(),
            compiler_version: "owned direct-fact storage probe".into(),
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Source(SourceArtifact {
            id: "source".into(),
            path: "fixture.ts".into(),
            compilation_id: "fixture".into(),
            sha256: hash(text.as_bytes()),
            utf8: text.as_bytes().to_vec(),
            ..Default::default()
        }))
        .unwrap();
    let name = text.find("pressureTarget").unwrap() as u64;
    let extent = text.find("export function").unwrap() as u64;
    writer
        .stage(Payload::Facts(FactBatch {
            compilation_id: "fixture".into(),
            symbols: vec![Symbol {
                id: "target".into(),
                name: "pressureTarget".into(),
                kind: "function".into(),
                signature: format!("owned revision {revision:04}"),
                ..Default::default()
            }],
            occurrences: vec![
                Occurrence {
                    symbol_id: "target".into(),
                    artifact_id: "source".into(),
                    role: "declaration".into(),
                    start_byte: name,
                    end_byte: name + 14,
                    ..Default::default()
                },
                Occurrence {
                    symbol_id: "target".into(),
                    artifact_id: "source".into(),
                    role: "definition".into(),
                    start_byte: extent,
                    end_byte: text.len() as u64,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }))
        .unwrap();
    writer
        .stage(Payload::Report(AnalysisReport {
            compilation_id: "fixture".into(),
            status: "complete".into(),
            ..Default::default()
        }))
        .unwrap();
    writer.publish().unwrap()
}
fn verify(view: &ReadView, generation: u64, revision: usize, text: &str) {
    let lookup: QueryRequest = serde_json::from_value(
        json!({"op":"symbols", "name":"pressureTarget", "deadline_ms":30_000}),
    )
    .unwrap();
    let result = view.query(lookup).unwrap();
    assert_eq!(result.generation, generation);
    assert_eq!(result.symbols.len(), 1);
    assert_eq!(
        result.symbols[0].signature,
        format!("owned revision {revision:04}")
    );
    let digest = hash(text.as_bytes());
    assert!(
        result.symbols[0]
            .locations
            .iter()
            .all(|location| location.content_hash == digest)
    );
    // Each request stays within the actual source cap. Reassembly checks all
    // indexed UTF-8 bytes (including NUL), not only a prefix from the old view.
    let mut collected = Vec::with_capacity(text.len());
    while collected.len() < text.len() {
        let start = collected.len();
        let mut end = (start + MAX_SOURCE_BYTES as usize).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let request = serde_json::from_value(
            json!({"op":"source", "artifact_id":"source", "content_hash":digest,
            "start_byte":start, "end_byte":end, "max_bytes":MAX_SOURCE_BYTES,
            "response_max_bytes":2_097_152, "deadline_ms":30_000}),
        )
        .unwrap();
        let result = view.query(request).unwrap();
        assert_eq!(result.generation, generation);
        let body = result.source.unwrap();
        assert_eq!(body.content_hash, digest);
        assert_eq!(body.total_bytes, text.len() as u64);
        assert_eq!((body.start_byte, body.end_byte), (start as u64, end as u64));
        assert_eq!(body.text.as_bytes(), &text.as_bytes()[start..end]);
        collected.extend_from_slice(body.text.as_bytes());
    }
    assert_eq!(hash(&collected), digest);
    assert_eq!(collected, text.as_bytes());
}
fn file(path: &Path) -> Value {
    match fs::metadata(path) {
        Ok(metadata) => json!({"exists":true, "apparent_bytes":metadata.len(),
            "allocated_bytes":metadata.blocks()*512, "device":metadata.dev(), "inode":metadata.ino()}),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            json!({"exists":false,"apparent_bytes":0,"allocated_bytes":0})
        }
        Err(error) => panic!("{}: {error}", path.display()),
    }
}
fn physical(database: &Path, observer: &Connection) -> Value {
    assert!(
        observer.is_autocommit(),
        "the observer must not become an idle SQL snapshot"
    );
    let number = |pragma: &str| {
        observer
            .query_row(pragma, [], |row| row.get::<_, u64>(0))
            .unwrap()
    };
    json!({"main":file(database), "wal":file(&PathBuf::from(format!("{}-wal",database.display()))),
        "shm":file(&PathBuf::from(format!("{}-shm",database.display()))),
        "page_size":number("PRAGMA page_size"), "page_count":number("PRAGMA page_count"),
        "freelist_count":number("PRAGMA freelist_count")})
}
fn checkpoint(observer: &Connection, truncate: bool) -> Value {
    assert!(observer.is_autocommit());
    let statement = if truncate {
        "PRAGMA wal_checkpoint(TRUNCATE)"
    } else {
        "PRAGMA wal_checkpoint(PASSIVE)"
    };
    let (busy, log, checkpointed) = observer
        .query_row(statement, [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap();
    json!({"mode":if truncate {"truncate"} else {"passive"},"busy":busy,"log_frames":log,"checkpointed_frames":checkpointed})
}
fn write_evidence(path: &Path, record: &Value) {
    fs::write(path, serde_json::to_vec_pretty(record).unwrap()).unwrap();
}
fn watchdog(start: Instant) {
    assert!(
        start.elapsed() < WATCHDOG,
        "manual fixture watchdog elapsed; this is not a latency SLO"
    );
}

fn probe(directory: &Path, record: &mut Value) {
    let started = Instant::now();
    let database = directory.join("a/index.db");
    let sibling = directory.join("b/index.db");
    let scope = AdmissionScope::at(directory.join("query-admission")).unwrap();
    record["admission"] = serde_json::to_value(scope.configure(4).unwrap()).unwrap();
    let original = source(0, LARGE);
    let original_generation = publish(&database, &original, 0);
    let original_sibling_generation = publish(&sibling, &source(0, 8192), 0);
    // Persistent autocommit observers make retained WAL capacity visible;
    // they neither BEGIN nor retain a SQLite read mark between measurements.
    let observer = Connection::open(&database).unwrap();
    let sibling_observer = Connection::open(&sibling).unwrap();
    observer.busy_timeout(Duration::from_millis(100)).unwrap();
    sibling_observer
        .busy_timeout(Duration::from_millis(100))
        .unwrap();
    let retention = GenerationRetention::activate(&database, [19; 16]).unwrap();
    let logical_pin = retention.pin_current().unwrap();
    let automatic: i64 = observer
        .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))
        .unwrap();
    record["sqlite"] = json!({"bundled_library_version":rusqlite::version(),
        "observer_connection_wal_autocheckpoint_pages":automatic,
        "writer_policy_note":"current Writer does not override its connection's SQLite autocheckpoint default; probe does not disable it"});
    record["initial"] = physical(&database, &observer);
    record["source_hashes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"revision":0,"sha256":hash(original.as_bytes()),"bytes":original.len()}));
    let mut raw: Option<(ReadView, u64, String)> = None;
    let mut raw_frame_counts = vec![];
    for revision in 1..=8 {
        let phase = if revision <= 3 {
            "idle_logical_pin"
        } else if revision <= 6 {
            "held_sql_snapshot"
        } else {
            "reuse_after_release"
        };
        if revision == 4 {
            let text = source(3, LARGE);
            let view = ReadView::open_in_scope(&database, &scope).unwrap();
            verify(&view, view.generation, 3, &text);
            raw = Some((view, original_generation + 3, text));
        }
        if revision == 7 {
            drop(raw.take());
            let passive = checkpoint(&observer, false);
            assert_eq!(passive["busy"], 0);
            assert!(
                passive["log_frames"].as_i64().unwrap() <= 0
                    || passive["log_frames"] == passive["checkpointed_frames"]
            );
            // Logical rows are still retained, but they do not prevent physical
            // checkpoint/truncate after the actual SQL read transaction ends.
            let truncate = checkpoint(&observer, true);
            assert_eq!(
                (
                    truncate["busy"].as_i64(),
                    truncate["log_frames"].as_i64(),
                    truncate["checkpointed_frames"].as_i64()
                ),
                (Some(0), Some(0), Some(0))
            );
            verify(
                &logical_pin.open_view_in_scope(&scope).unwrap(),
                original_generation,
                0,
                &original,
            );
            let retained = retention.retained_usage(None, Some(30_000)).unwrap();
            assert!(retained.source_bytes >= LARGE as u64);
            record["after_sql_release"] = json!({"passive":passive,"truncate":truncate,
                "physical":physical(&database,&observer),"logical_retention":retained});
            retention.release(&logical_pin).unwrap();
            assert!(matches!(
                logical_pin.open_view_in_scope(&scope),
                Err(know_code_core::Error::CursorInvalidated)
            ));
            let released = retention.retained_usage(None, Some(30_000)).unwrap();
            assert_eq!(
                (released.units, released.source_bytes, released.pin_bytes),
                (0, 0, 0)
            );
            record["after_logical_release"] =
                json!({"physical":physical(&database,&observer),"logical_retention":released});
        }
        watchdog(started);
        assert!(observer.is_autocommit() && sibling_observer.is_autocommit());
        let text = source(revision, LARGE);
        let tick = Instant::now();
        let generation = publish(&database, &text, revision);
        let publication_ms = tick.elapsed().as_secs_f64() * 1000.;
        assert_eq!(generation, original_generation + revision as u64);
        let sibling_text = source(revision, 8192);
        let sibling_generation = publish(&sibling, &sibling_text, revision);
        assert_eq!(
            sibling_generation,
            original_sibling_generation + revision as u64
        );
        verify(
            &ReadView::open_in_scope(&database, &scope).unwrap(),
            generation,
            revision,
            &text,
        );
        verify(
            &ReadView::open_in_scope(&sibling, &scope).unwrap(),
            sibling_generation,
            revision,
            &sibling_text,
        );
        if let Some((view, generation, text)) = &raw {
            verify(view, *generation, 3, text);
        }
        if revision <= 6 {
            verify(
                &logical_pin.open_view_in_scope(&scope).unwrap(),
                original_generation,
                0,
                &original,
            );
        }
        let before = physical(&database, &observer);
        let passive = checkpoint(&observer, false);
        if revision <= 3 {
            assert_eq!(passive["busy"], 0);
            assert!(
                passive["log_frames"].as_i64().unwrap() <= 0
                    || passive["log_frames"] == passive["checkpointed_frames"]
            );
        } else if revision <= 6 {
            let log = passive["log_frames"].as_i64().unwrap();
            let checkpointed = passive["checkpointed_frames"].as_i64().unwrap();
            assert!(
                log > checkpointed,
                "a real held SQL snapshot should defer complete checkpointing: {passive}"
            );
            raw_frame_counts.push(log);
        }
        let retained = retention.retained_usage(None, Some(30_000)).unwrap();
        let sample = json!({"phase":phase,"revision":revision,"generation":generation,"sibling_generation":sibling_generation,
            "publication_elapsed_ms":publication_ms,"before_probe_checkpoint":before,"probe_checkpoint":passive,
            "after_probe_checkpoint":physical(&database,&observer),"sibling_physical":physical(&sibling,&sibling_observer),
            "logical_retention":retained});
        record["samples"].as_array_mut().unwrap().push(sample);
        record["source_hashes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"revision":revision,"sha256":hash(text.as_bytes()),"bytes":text.len()}));
        write_evidence(&directory.join("evidence.json"), record);
        watchdog(started);
    }
    assert!(
        raw_frame_counts.last() > raw_frame_counts.first(),
        "recorded WAL frames should grow while the same old snapshot survives multiple publications"
    );
    let final_checkpoint = checkpoint(&observer, true);
    assert_eq!(
        (
            final_checkpoint["busy"].as_i64(),
            final_checkpoint["log_frames"].as_i64(),
            final_checkpoint["checkpointed_frames"].as_i64()
        ),
        (Some(0), Some(0), Some(0))
    );
    let contents: u64 = observer
        .query_row("SELECT count(*) FROM contents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        contents, 1,
        "released source facts must not become a history archive"
    );
    let before_pages = record["after_logical_release"]["physical"]["page_count"]
        .as_u64()
        .unwrap();
    let final_physical = physical(&database, &observer);
    let reuse = final_physical["page_count"].as_u64().unwrap() <= before_pages;
    record["final"] = json!({"checkpoint":final_checkpoint,"physical":final_physical,
        "sibling_physical":physical(&sibling,&sibling_observer),"contents":contents,
        "main_page_reuse_without_extension_observed":reuse,
        "elapsed_ms":started.elapsed().as_secs_f64()*1000.});
}

#[test]
#[ignore = "manual finite bundled-SQLite WAL measurement; run explicitly in a reserved validation slot"]
fn pressure_probe() {
    let stamp = format!(
        "{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.artifacts/wal-pressure")
        .join(&stamp[stamp.len().saturating_sub(10)..]);
    fs::create_dir_all(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let mut record = json!({"passed":false,"directory":directory,"samples":[],"source_hashes":[],
        "test_source_sha256":hash(include_bytes!("wal_pressure.rs")),
        "limits":["8 direct-fact revisions, not compiler/native throughput or complete S04 certification",
            "PASSIVE/TRUNCATE calls are explicit bundled-SQLite probe interventions, not evidence of periodic runtime maintenance",
            "physical file size, allocated blocks, checkpoint frames, freelist and logical retention are separate observations",
            "no physical disk ceiling or latency SLO; elapsed watchdog only; no HOME/global controls/live runtime/original corpus"]});
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        probe(&directory, &mut record)
    }));
    record["passed"] = json!(outcome.is_ok());
    if let Err(payload) = &outcome {
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).into()))
            .unwrap_or_else(|| "non-string panic".into());
        record["failure"] = json!(message);
    }
    write_evidence(&directory.join("evidence.json"), &record);
    println!(
        "WAL pressure evidence: {}",
        directory.join("evidence.json").display()
    );
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

/// Interactive owned-fixture reader; no production export or alternate engine.
#[test]
#[ignore = "managed coordinator fixture supplies owned database and query scope"]
fn hold_managed_read_view() {
    use std::io::{BufRead, Write};
    let Some(database) = std::env::var_os("KNOW_CODE_WAL_HOLDER_DATABASE") else {
        return;
    };
    let scope =
        std::env::var_os("KNOW_CODE_QUERY_SCOPE").expect("holder requires an explicit owned scope");
    let scope = AdmissionScope::at(scope).unwrap();
    let mut view = Some(ReadView::open_in_scope(PathBuf::from(database), &scope).unwrap());
    println!(
        "KC_WAL_HOLDER {}",
        json!({"ready":true,"generation":view.as_ref().unwrap().generation})
    );
    std::io::stdout().flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if request["op"] == "release" {
            drop(view.take());
            println!("KC_WAL_HOLDER {}", json!({"released":true}));
            std::io::stdout().flush().unwrap();
            break;
        }
        let result = view
            .as_ref()
            .unwrap()
            .query(serde_json::from_value(request).unwrap());
        let response = match result {
            Ok(result) => json!({"result":result}),
            Err(error) => json!({"error":{"code":error.code(),"message":error.to_string()}}),
        };
        println!("KC_WAL_HOLDER {response}");
        std::io::stdout().flush().unwrap();
    }
}
