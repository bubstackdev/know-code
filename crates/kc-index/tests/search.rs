//! Content search end to end on a scratch git repo: index, search, edit, and check that searches see the edit.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::content::{PENDING_FILES, sync_content_files};
use kc_index::search::{self, Case, SearchOptions};
use kc_index::{SyncOptions, sync};
use kc_store::Store;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-search-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Sources")).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
    dir
}

fn write(root: &Path, path: &str, text: &[u8]) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Matching (file, line, kind) triples, ranked.
fn find(root: &Path, store: &mut Store, pattern: &str, regex: bool, case: Case) -> Vec<(String, u32, &'static str)> {
    sync(root, store, &SyncOptions { threads: 2, full_discovery: true, ..Default::default() }).unwrap();
    let (_, files) = sync_content_files(root, store, 2).unwrap();
    let opts = SearchOptions {
        pattern: pattern.into(),
        regex,
        case,
        word: false,
        langs: Vec::new(),
        globs: Vec::new(),
        max_lines: 10_000,
    };
    let m = search::compile(&opts).unwrap();
    let (cands, _) = search::candidates(store, files, &m, 0).unwrap();
    let scanned = search::scan(&[root.to_path_buf()], &cands, &m, 2, 10_000);
    let mut files = Vec::new();
    let mut hits = Vec::new();
    let mut scratch = search::Scratch::default();
    for (i, fh) in scanned.files.iter().enumerate() {
        let (fd, h) = search::classify(root, store, &scanned, fh, i as u32, &m, &mut scratch).unwrap();
        files.push(fd);
        hits.extend(h);
    }
    search::rank(&mut hits, &files, &["r".to_string()]);
    hits.into_iter().map(|h| (files[h.file as usize].path.clone(), h.line, h.kind)).collect()
}

const STORE: &str = "/// Keeps settings.\nstruct SettingsStore {\n    let path: String\n    func load() -> SettingsStore { SettingsStore(path: path) }\n}\n";
const MODEL: &str = "final class AppModel {\n    // the SettingsStore we read at launch\n    let settings = SettingsStore(path: \"x\")\n}\nprint(SettingsStore.self)\n";

#[test]
fn ranks_definitions_first_and_stays_fresh() {
    let root = scratch("rank");
    write(&root, "Sources/Store.swift", STORE.as_bytes());
    write(&root, "Sources/Model.swift", MODEL.as_bytes());
    write(&root, "Tests/StoreTests.swift", b"func testLoad() {\n    _ = SettingsStore(path: \"t\")\n}\n");
    write(&root, "README.md", b"Use SettingsStore to load settings.\n");
    write(&root, "icon.png", b"\x89PNG\r\n\x1a\n\0\0\0SettingsStore");
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();

    let hits = find(&root, &mut store, "SettingsStore", false, Case::Sensitive);
    let kinds: Vec<_> = hits.iter().map(|h| h.2).collect();
    assert_eq!(hits[0], ("Sources/Store.swift".into(), 2, "def"));
    assert_eq!(kinds, vec!["def", "decl", "decl", "code", "test", "comment", "doc"], "{hits:?}");
    assert!(!hits.iter().any(|h| h.0 == "icon.png"), "binary files are skipped");

    // An edit is visible on the next search, before any merge.
    write(&root, "Sources/Model.swift", b"final class AppModel {}\n");
    write(&root, "Sources/New.swift", b"let s = SettingsStore(path: \"n\")\n");
    let hits = find(&root, &mut store, "SettingsStore", false, Case::Sensitive);
    assert!(!hits.iter().any(|h| h.0 == "Sources/Model.swift"));
    assert!(hits.iter().any(|h| h.0 == "Sources/New.swift"));

    // Enough changed files merge into the trigram lists; results stay the same.
    for i in 0..PENDING_FILES + 4 {
        write(&root, &format!("Sources/Gen{i}.swift"), format!("let v{i} = 1\n").as_bytes());
    }
    let before = find(&root, &mut store, "SettingsStore", false, Case::Sensitive);
    let (report, _) = sync_content_files(&root, &mut store, 2).unwrap();
    assert_eq!(report.pending, 0, "{report:?}");
    assert_eq!(find(&root, &mut store, "SettingsStore", false, Case::Sensitive), before);
    assert_eq!(find(&root, &mut store, "let v7 =", false, Case::Sensitive).len(), 1);

    // Deleting a file drops its hits.
    std::fs::remove_file(root.join("Sources/New.swift")).unwrap();
    let hits = find(&root, &mut store, "SettingsStore", false, Case::Sensitive);
    assert!(!hits.iter().any(|h| h.0 == "Sources/New.swift"));

    // Regex and case options.
    let hits = find(&root, &mut store, r"settings(store)?\b", true, Case::Insensitive);
    assert!(hits.len() >= 5, "{hits:?}");
    let hits = find(&root, &mut store, r"^struct \w+Store", true, Case::Sensitive);
    assert_eq!(hits, vec![("Sources/Store.swift".into(), 2, "def")]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A capped search is the same whatever the thread count and timing: the scan stops at the file where the count
/// passes the cap, in file order, not wherever the workers happened to be. Unreadable files are reported, never
/// skipped silently.
#[test]
fn invariant_a_capped_scan_is_the_same_on_every_run_and_thread_count() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch("determinism");
    for i in 0..120 {
        let body: String = (0..(i % 7 + 1)).map(|j| format!("let token{j} = Needle()\n")).collect();
        write(&root, &format!("Sources/F{i:03}.swift"), body.as_bytes());
    }
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 2, full_discovery: true, ..Default::default() }).unwrap();
    let (_, files) = sync_content_files(&root, &mut store, 2).unwrap();
    let opts = SearchOptions { pattern: "Needle".into(), regex: false, case: Case::Sensitive, word: false, langs: Vec::new(), globs: Vec::new(), max_lines: 40 };
    let m = search::compile(&opts).unwrap();
    let (cands, _) = search::candidates(&store, files, &m, 0).unwrap();
    // One file can't be read: it is reported, at its place in the order (unless running as root, who reads it).
    let locked = root.join("Sources/F002.swift");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = std::fs::read(&locked).is_err();
    let roots = [root.clone()];
    type Run = (Vec<(String, Vec<u32>)>, usize, Vec<(usize, String)>);
    let mut first: Option<Run> = None;
    for run in 0..20 {
        let threads = run % 8 + 1;
        let s = search::scan(&roots, &cands, &m, threads, opts.max_lines);
        assert!(s.stats.capped, "the cap is reached");
        let got: Run = (s.files.iter().map(|f| (f.path.clone(), f.lines.iter().map(|l| l.line).collect())).collect(), s.stats.files_read, s.stats.failed.clone());
        match &first {
            None => first = Some(got),
            Some(f) => assert_eq!(f, &got, "run {run} with {threads} threads"),
        }
    }
    let (kept, read, failed) = first.unwrap();
    // The files kept are a prefix of the candidates, ending at the one passing the cap.
    let lines: usize = kept.iter().map(|(_, l)| l.len()).sum();
    assert!(lines > 40 && lines - kept.last().unwrap().1.len() <= 40, "{lines}");
    assert_eq!(read, cands.iter().position(|c| c.path == kept.last().unwrap().0).unwrap() + 1);
    if unreadable {
        assert_eq!(failed, [(0, "Sources/F002.swift".to_string())]);
    }
    // Structural search too.
    let ast = kc_index::structural::compile(&kc_index::structural::AstOptions {
        pattern: "Needle()".into(),
        lang: "swift".into(),
        max_matches: 40,
        ..Default::default()
    })
    .unwrap();
    let (acands, _) = kc_index::structural::candidates(&store, store.cfiles().unwrap(), &ast, 0).unwrap();
    let mut first: Option<(Vec<(String, u32)>, usize)> = None;
    for run in 0..20 {
        let (hits, stats) = kc_index::structural::scan(&roots, &acands, &ast, run % 8 + 1, 40);
        assert!(stats.capped);
        let got = (hits.iter().map(|h| (h.file.clone(), h.line)).collect(), stats.files_parsed);
        match &first {
            None => first = Some(got),
            Some(f) => assert_eq!(f, &got, "grep-ast run {run}"),
        }
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
