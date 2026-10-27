//! A query reads one state of the index while another connection (another process, in use) reindexes: its reads
//! never mix two generations, find no dangling symbol ids, and raise no errors.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use kc_index::{SyncOptions, sync};
use kc_store::{Store, query};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-snapshot-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
    dir
}

/// Version `k` of the one source file: `k` filler methods declared ahead of `Target.Run` (so every sync with another
/// `k` gives `Run` another symbol id) and `k` calls of it.
fn version(k: usize) -> String {
    let fillers: String = (0..k).map(|i| format!("        public void F{i}() {{}}\n")).collect();
    let calls: String = (0..k).map(|_| "            t.Run();\n").collect();
    format!(
        "namespace Shop {{\n    public class Filler {{\n{fillers}    }}\n    public class Target {{\n        public void Run() {{}}\n    }}\n    public class Caller {{\n        public void Go(Target t) {{\n{calls}        }}\n    }}\n}}\n"
    )
}

fn write(root: &Path, k: usize) {
    std::fs::write(root.join("src/Shop.cs"), version(k)).unwrap();
}

/// One multi-step query, as resolution makes them: the generation, the definition's ids, each id looked up again, the
/// fillers, then the references resolved to the definition. Returns the `k` its reads agree on, or what disagreed.
fn read_once(store: &Store) -> Result<usize, String> {
    let conn = store.conn();
    let generation = store.generation().map_err(|e| e.to_string())?;
    let defs = query::definitions(conn, "Target.Run").map_err(|e| e.to_string())?;
    if defs.len() != 1 {
        return Err(format!("{} definitions of Target.Run at {generation}", defs.len()));
    }
    // Give a writer room to commit between the steps, as a long resolution does.
    std::thread::yield_now();
    let id = defs[0].id;
    let name: Option<String> = conn
        .query_row("SELECT name FROM symbols WHERE id = ?1", [id], |r| r.get(0))
        .map(Some)
        .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })
        .map_err(|e| e.to_string())?;
    if name.as_deref() != Some("Run") {
        return Err(format!("symbol id {id} of Run reads back as {name:?}"));
    }
    let fillers: usize = conn
        .query_row("SELECT count(*) FROM symbols WHERE name GLOB 'F[0-9]*' AND kind = 'method'", [], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())? as usize;
    std::thread::yield_now();
    let (hits, _) = query::refs(conn, "Target.Run", &[], 1000).map_err(|e| e.to_string())?;
    let calls = hits.iter().filter(|h| h.targets.iter().any(|t| t.id == id)).count();
    if calls != fillers || hits.len() != calls {
        return Err(format!("{fillers} fillers but {calls} of {} references resolved to Run #{id} at {generation}", hits.len()));
    }
    if store.generation().map_err(|e| e.to_string())? != generation {
        return Err("the generation moved within one query".into());
    }
    Ok(fillers)
}

#[test]
fn invariant_a_query_reads_one_generation_while_another_connection_reindexes() {
    let root = scratch("reindex");
    let db = root.join("index.sqlite");
    write(&root, 1);
    let opts = SyncOptions { threads: 1, ..Default::default() };
    sync(&root, &mut Store::open(&db).unwrap(), &opts).unwrap();

    // The reader reads until the writer has reindexed `SYNCS` times since its first read, it has made `READS` queries
    // and seen more than one state, however slowly either runs: counted, not timed (an unloaded run of the timed test
    // made about this many of each). The deadline only stops a hang.
    const SYNCS: usize = 12;
    const READS: usize = 2000;
    let stop = AtomicBool::new(false);
    let syncs = AtomicUsize::new(0);
    let (reads, seen, problems) = std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            let mut store = Store::open(&db).unwrap();
            let mut k = 1;
            while !stop.load(Ordering::Relaxed) {
                k = k % 6 + 1;
                write(&root, k);
                // A new size each time, so the stat alone says the file changed.
                sync(&root, &mut store, &opts).unwrap();
                syncs.fetch_add(1, Ordering::Relaxed);
            }
        });
        let reader = scope.spawn(|| {
            let store = Store::open(&db).unwrap();
            let (mut reads, mut seen, mut problems) = (0, std::collections::BTreeSet::new(), Vec::new());
            let start = syncs.load(Ordering::Relaxed);
            let hang = std::time::Instant::now() + std::time::Duration::from_secs(300);
            while syncs.load(Ordering::Relaxed) < start + SYNCS || reads < READS || seen.len() < 2 {
                assert!(std::time::Instant::now() < hang, "no progress: {} reindexes and {seen:?} over {reads} queries", syncs.load(Ordering::Relaxed) - start);
                store.begin_snapshot().unwrap();
                match read_once(&store) {
                    Ok(k) => {
                        seen.insert(k);
                    }
                    Err(e) => problems.push(e),
                }
                store.end_snapshot().unwrap();
                reads += 1;
            }
            (reads, seen, problems)
        });
        let out = reader.join();
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        out.unwrap()
    });
    assert!(problems.is_empty(), "{} of {reads} queries saw a mixed state, first: {:?}", problems.len(), &problems[..problems.len().min(5)]);
    // Every query ran while the writer reindexed, and the reader saw its changes, not just one state.
    assert!(seen.len() > 1 && reads >= READS, "only saw {seen:?} over {reads} queries");
    let _ = std::fs::remove_dir_all(&root);
}

/// The snapshot ends at a write batch of the same connection and starts again after it, at the new state: a query
/// that reindexes a file it read (a hash mismatch) sees its own reindex.
#[test]
fn invariant_a_write_batch_moves_the_snapshot_to_what_it_committed() {
    let root = scratch("batch");
    let db = root.join("index.sqlite");
    write(&root, 2);
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let mut store = Store::open(&db).unwrap();
    sync(&root, &mut store, &opts).unwrap();
    store.begin_snapshot().unwrap();
    assert_eq!(read_once(&store), Ok(2));
    // Another connection reindexes: this one keeps reading its snapshot.
    write(&root, 4);
    sync(&root, &mut Store::open(&db).unwrap(), &opts).unwrap();
    assert_eq!(read_once(&store), Ok(2));
    // Its own sync (a batch) releases the snapshot and takes one again at the state it committed.
    write(&root, 5);
    sync(&root, &mut store, &opts).unwrap();
    assert!(store.in_snapshot());
    assert_eq!(read_once(&store), Ok(5));
    // Cache writes inside the snapshot go through the side connection: no write lock is held by the query, and
    // another connection can still sync.
    let edges = store.file_edges().unwrap();
    assert!(store.in_snapshot());
    write(&root, 3);
    sync(&root, &mut Store::open(&db).unwrap(), &opts).unwrap();
    assert_eq!(read_once(&store), Ok(5));
    assert_eq!(store.file_edges().unwrap().len(), edges.len());
    store.end_snapshot().unwrap();
    assert_eq!(read_once(&store), Ok(3));
    let _ = std::fs::remove_dir_all(&root);
}
