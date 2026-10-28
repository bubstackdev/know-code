//! An awaited Swift call is made in an async context, where Swift prefers an `async` overload: `try await
//! channel.option(x)` is the protocol extension's `async` member, not the class's own synchronous one.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn an_awaited_call_picks_the_async_overload() {
    let root = scratch("swift-async-pick");
    write(
        &root,
        "Sources/Pipes/Pipe.swift",
        "protocol Pipe {}\n\nextension Pipe {\n    func option(_ k: Int) async throws -> Int { 0 }\n}\n\nfinal class TestPipe: Pipe {\n    func option(_ k: Int) -> Int { k }\n    func run(_ body: () async -> Void) {}\n}\n",
    );
    write(
        &root,
        "Sources/Pipes/Use.swift",
        "func check(_ p: TestPipe) async throws {\n    let a = try await p.option(1)\n    let b = p.option(2)\n    _ = (a, b)\n}\n",
    );
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let (hits, _) = query::refs(store.conn(), "option", &["call"], 50).unwrap();
    let got: Vec<(u32, &str, Vec<u32>)> = hits.iter().map(|h| (h.line, h.confidence, h.targets.iter().map(|t| t.line).collect())).collect();
    // Awaited: the extension's `async` member (line 4); not awaited: the class's own (line 8).
    assert!(got.iter().any(|g| g.0 == 2 && g.2 == [4]), "{got:?}");
    assert!(got.iter().any(|g| g.0 == 3 && g.1 == "exact" && g.2 == [8]), "{got:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_actors_own_member_stays_when_awaited() {
    let root = scratch("swift-async-actor");
    write(
        &root,
        "Sources/Stores/Store.swift",
        "protocol Store {}\n\nextension Store {\n    func get(_ k: Int) async throws -> Int { 0 }\n}\n\nactor Box: Store {\n    func get(_ k: Int) -> Int { k }\n}\n",
    );
    write(&root, "Sources/Stores/Use.swift", "func check(_ b: Box) async {\n    let a = await b.get(1)\n    _ = a\n}\n");
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let (hits, _) = query::refs(store.conn(), "get", &["call"], 50).unwrap();
    let got: Vec<(u32, &str, Vec<u32>)> = hits.iter().map(|h| (h.line, h.confidence, h.targets.iter().map(|t| t.line).collect())).collect();
    // Awaited because `Box` is an actor: its own synchronous `get` (line 8).
    assert!(got.iter().any(|g| g.0 == 2 && g.1 == "exact" && g.2 == [8]), "{got:?}");
    let _ = std::fs::remove_dir_all(&root);
}
