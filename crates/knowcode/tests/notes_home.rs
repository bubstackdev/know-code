//! Where notes live, through the binary: in the data directory, never the cache, so clearing the cache never loses
//! one; notes from before (in the cache) are moved on first use, never lost; `note export` and `note import` carry
//! them to another machine.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-notes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

/// A git repo with two Python functions, committed.
fn repo(base: &Path) -> PathBuf {
    let root = base.join("app");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.py"), "def charge(n):\n    return n * 2\n\n\ndef refund(n):\n    return -n\n").unwrap();
    for args in [&["init", "-q"][..], &["add", "-A"], &["commit", "-q", "-m", "x"]] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.name=T", "-c", "user.email=t@example.test", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    root
}

/// Runs knowcode with this cache and data directory (and an empty config directory) from `cwd`.
fn kc(base: &Path, data: &Path, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(KC)
        .args(args)
        .current_dir(cwd)
        .env("KNOWCODE_CACHE_DIR", base.join("cache"))
        .env("KNOWCODE_CONFIG_DIR", base.join("config"))
        .env("KNOWCODE_DATA_DIR", data)
        .env("KNOWCODE_NOW", "head")
        // The daily update check writes the cache's `updates`: not what these tests are about.
        .env("KNOWCODE_UPDATE_CHECK", "0")
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn ok(base: &Path, data: &Path, cwd: &Path, args: &[&str]) -> String {
    let (code, out, err) = kc(base, data, cwd, args);
    assert_eq!(code, 0, "{args:?}: {err}");
    out
}

/// `note list --json` without what changes between runs of the same answer (build, timings, freshness).
fn list(base: &Path, data: &Path, root: &Path) -> Value {
    let out = ok(base, data, root, &["note", "list", "--json"]);
    fn strip(v: Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(
                m.into_iter()
                    .filter(|(k, _)| !matches!(k.as_str(), "knowcode" | "freshness" | "elapsed" | "elapsed_ms" | "provenance"))
                    .map(|(k, v)| (k, strip(v)))
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.into_iter().map(strip).collect()),
            v => v,
        }
    }
    strip(serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}")))
}

fn texts(list: &Value) -> Vec<(i64, String)> {
    list["items"].as_array().unwrap().iter().map(|n| (n["id"].as_i64().unwrap(), n["text"].as_str().unwrap().to_string())).collect()
}

fn status_notes(base: &Path, data: &Path, root: &Path) -> Value {
    let out = ok(base, data, root, &["status", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    let c = if v["config"].is_object() { &v["config"] } else { &v["items"][0]["config"] };
    c["notes"].clone()
}

#[test]
fn notes_in_the_cache_directory_move_to_the_data_directory_and_the_old_file_is_kept() {
    let base = scratch("migrate-one");
    let root = repo(&base);
    let (cache, data) = (base.join("cache"), base.join("data"));
    // An older build kept notes in the cache: the data directory pointed at the cache stands in for it.
    ok(&base, &cache, &root, &["note", "add", "charge", "doubles the amount", "--tag", "decision"]);
    let before = list(&base, &cache, &root);
    assert!(cache.join("notes.sqlite").is_file());

    let after = list(&base, &data, &root);
    assert_eq!(after, before, "the same notes, ids and statuses");
    assert!(data.join("notes.sqlite").is_file());
    assert!(!cache.join("notes.sqlite").exists());
    assert!(cache.join("notes.sqlite.migrated").is_file(), "the old file is kept, renamed");
    let s = status_notes(&base, &data, &root);
    assert_eq!((s["path"].as_str(), s["count"].as_u64(), s["source"].as_str()), (Some(&*data.join("notes.sqlite").to_string_lossy()), Some(1), Some("env")));
    let text = ok(&base, &data, &root, &["status"]);
    assert!(text.contains(&format!("notes    {} (1 note; from KNOWCODE_DATA_DIR)", data.join("notes.sqlite").display())), "{text}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn notes_in_both_places_are_merged_without_duplicates() {
    let base = scratch("migrate-both");
    let root = repo(&base);
    let (cache, data) = (base.join("cache"), base.join("data"));
    ok(&base, &data, &root, &["note", "add", "refund", "negates", "--tag", "gotcha"]);
    ok(&base, &cache, &root, &["note", "add", "charge", "doubles"]);
    // Both have a note #1: the one moving in gets a new id; neither is lost.
    assert_eq!(texts(&list(&base, &data, &root)), vec![(1, "negates".to_string()), (2, "doubles".to_string())]);
    assert!(cache.join("notes.sqlite.migrated").is_file());

    // The old file comes back (an older build ran, or a copy was restored): nothing is added twice, and the second
    // backup doesn't overwrite the first.
    std::fs::copy(cache.join("notes.sqlite.migrated"), cache.join("notes.sqlite")).unwrap();
    assert_eq!(texts(&list(&base, &data, &root)), vec![(1, "negates".to_string()), (2, "doubles".to_string())]);
    assert!(cache.join("notes.sqlite.migrated.1").is_file() && !cache.join("notes.sqlite").exists());
    assert_eq!(status_notes(&base, &data, &root)["count"], 2);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn with_no_notes_anywhere_nothing_is_created() {
    let base = scratch("migrate-none");
    let root = repo(&base);
    let data = base.join("data");
    let l = list(&base, &data, &root);
    assert_eq!(l["items"], Value::Array(Vec::new()));
    assert_eq!(status_notes(&base, &data, &root)["count"], 0);
    assert!(!data.join("notes.sqlite").exists() && !base.join("cache/notes.sqlite.migrated").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn an_export_imported_on_a_fresh_machine_gives_the_same_notes_and_importing_twice_adds_nothing() {
    let base = scratch("export");
    let root = repo(&base);
    let (data, fresh) = (base.join("data"), base.join("fresh"));
    ok(&base, &data, &root, &["note", "add", "charge", "doubles the amount", "--tag", "decision"]);
    ok(&base, &data, &root, &["note", "add", "refund", "always negative"]);
    // The body changes: one note is stale, and stays stale on the other side.
    std::fs::write(root.join("a.py"), "def charge(n):\n    return n * 3\n\n\ndef refund(n):\n    return -n\n").unwrap();
    let before = list(&base, &data, &root);
    assert_eq!(before["items"][0]["status"], "stale");

    let export = ok(&base, &data, &root, &["note", "export", "--repo", &root.to_string_lossy()]);
    let parsed: Value = serde_json::from_str(&export).unwrap();
    assert_eq!((parsed["format"].as_str(), parsed["version"].as_u64()), (Some("knowcode-notes"), Some(1)));
    let n = &parsed["notes"][0];
    for key in ["lang", "namespace", "qualified", "kind", "param_types", "body_hash", "repo", "created"] {
        assert!(n.get(key).is_some(), "{key} in {n}");
    }
    let file = base.join("notes.json");
    std::fs::write(&file, &export).unwrap();

    let first = ok(&base, &fresh, &root, &["note", "import", &file.to_string_lossy(), "--json"]);
    assert_eq!(serde_json::from_str::<Value>(&first).unwrap()["added"], 2);
    assert_eq!(list(&base, &fresh, &root), before);
    let again = ok(&base, &fresh, &root, &["note", "import", &file.to_string_lossy(), "--json"]);
    let again: Value = serde_json::from_str(&again).unwrap();
    assert_eq!((again["added"].as_u64(), again["present"].as_u64()), (Some(0), Some(2)));
    assert_eq!(list(&base, &fresh, &root), before);
    // The whole store exports the same notes, in the same stable form.
    assert_eq!(ok(&base, &fresh, &root, &["note", "export"]), export);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn purge_and_forget_never_touch_notes() {
    let base = scratch("purge");
    let root = repo(&base);
    let (cache, data) = (base.join("cache"), base.join("data"));
    ok(&base, &data, &root, &["note", "add", "charge", "doubles", "--tag", "decision"]);
    // And a cache file from an older build, not yet moved.
    ok(&base, &cache, &root, &["note", "add", "refund", "negates"]);
    let (notes, legacy) = (std::fs::read(data.join("notes.sqlite")).unwrap(), std::fs::read(cache.join("notes.sqlite")).unwrap());

    let out = ok(&base, &data, &root, &["purge"]);
    assert!(out.contains("kept") && out.contains("notes.sqlite"), "{out}");
    let mut left: Vec<String> = std::fs::read_dir(&cache).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, vec!["locks".to_string(), "notes.sqlite".to_string()], "index payloads are gone; notes and stable coordination locks stay");
    assert_eq!(std::fs::read(data.join("notes.sqlite")).unwrap(), notes);
    assert_eq!(std::fs::read(cache.join("notes.sqlite")).unwrap(), legacy);

    ok(&base, &data, &root, &["index", &root.to_string_lossy()]);
    ok(&base, &data, &root, &["forget", &root.to_string_lossy()]);
    ok(&base, &data, &root, &["prune"]);
    // Reindexed on the next question: both notes, current, the old one moved in.
    let l = list(&base, &data, &root);
    assert_eq!(texts(&l), vec![(1, "doubles".to_string()), (2, "negates".to_string())]);
    assert!(l["items"].as_array().unwrap().iter().all(|n| n["status"] == "current"), "{l}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn notes_are_never_written_into_an_indexed_repo() {
    let base = scratch("in-repo");
    let root = repo(&base);
    ok(&base, &base.join("data"), &root, &["index", &root.to_string_lossy()]);
    // The data directory pointed into the repo: refused, nothing written there.
    let (code, _, err) = kc(&base, &root, &root, &["note", "add", "charge", "doubles"]);
    assert_ne!(code, 0);
    assert!(err.contains("inside the indexed repo"), "{err}");
    assert!(!root.join("notes.sqlite").exists());
    let _ = std::fs::remove_dir_all(&base);
}
