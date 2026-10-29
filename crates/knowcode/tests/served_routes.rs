//! routes-v1's reader agrees with the answers: the routes a repo serves, as the workspace summary holds them
//! (crates/kc-index/examples/served_routes.rs reads them the same way), are exactly the "serves" rows `context` gives
//! for their owners (eval/routes-v1/README.md, Scoring). One fixture per extractor the routes gate measures; Go's and
//! Java's are added with their route extraction.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn base() -> PathBuf {
    let base = std::env::temp_dir().join(format!("knowcode-test-served-routes-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let base = fs::canonicalize(&base).unwrap();
    // SAFETY: one test in this binary; set before anything reads them, for the in-process summary and the binary alike.
    unsafe {
        std::env::set_var("KNOWCODE_CACHE_DIR", base.join("cache"));
        std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config"));
        std::env::set_var("KNOWCODE_DATA_DIR", base.join("data"));
        std::env::set_var("KNOWCODE_UPDATE_CHECK", "0");
    }
    base
}

fn repo(base: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = base.join(name);
    fs::create_dir_all(&dir).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
    for (path, text) in files {
        let p = dir.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    let out = Command::new(KC).arg("index").current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    dir
}

/// The served routes as (verb, path, owner file, owner line), from the summary.
fn summary_routes(root: &Path) -> Vec<(String, String, String, u32)> {
    let root = kc_index::repo_root(root).unwrap();
    let summaries = kc_index::workspace::summaries(std::slice::from_ref(&root)).unwrap();
    let s = summaries.iter().find(|s| s.root == root).expect("indexed");
    s.routes
        .iter()
        .filter(|r| r.side == "server")
        .map(|r| (r.verb.clone(), r.path.clone().unwrap_or_default(), r.file.clone(), r.start_line))
        .collect()
}

/// The "serves" rows `context` gives for the declaration at `file:line`, as (verb, path).
fn context_serves(root: &Path, file: &str, line: u32) -> BTreeSet<(String, String)> {
    let out = Command::new(KC).args(["context", &format!("{file}:{line}"), "--json"]).current_dir(root).output().unwrap();
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let mut rows = BTreeSet::new();
    for item in answer["items"].as_array().into_iter().flatten() {
        for h in item["http"].as_array().into_iter().flatten().filter(|h| h["role"] == "serves") {
            rows.insert((h["verb"].as_str().unwrap_or("").to_string(), h["path"].as_str().unwrap_or("").to_string()));
        }
    }
    rows
}

fn agree(root: &Path) -> usize {
    let routes = summary_routes(root);
    assert!(!routes.is_empty(), "the fixture serves routes");
    for (verb, path, file, line) in &routes {
        let rows = context_serves(root, file, *line);
        assert!(rows.contains(&(verb.clone(), path.clone())), "{verb} {path} at {file}:{line}: context says {rows:?}");
    }
    routes.len()
}

#[test]
fn the_summarys_served_routes_are_contexts() {
    let base = base();
    let cs = repo(
        &base,
        "crates-api",
        &[
            ("Api/Api.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"),
            (
                "Api/CratesController.cs",
                "using Microsoft.AspNetCore.Mvc;\n\n[Route(\"api/crates\")]\npublic class CratesController : Controller\n{\n    [HttpGet(\"{id}\")]\n    public object Get(int id) => null;\n\n    [HttpPost]\n    public object Create() => null;\n}\n",
            ),
            // A minimal API registered in a method. One registered in top-level statements is in the summary but not in
            // any `context` answer: a known gap of C#'s, reported, and the reason the gate also samples `context`.
            (
                "Api/Endpoints.cs",
                "public static class Endpoints\n{\n    public static void Map(WebApplication app)\n    {\n        app.MapGet(\"/health\", Health);\n    }\n\n    static string Health() => \"ok\";\n}\n",
            ),
        ],
    );
    assert_eq!(agree(&cs), 3);
    let _ = fs::remove_dir_all(&base);
}
