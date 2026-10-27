//! `index` of several repos runs them at once (`--jobs`, by default from the machine), and answers as if one at a
//! time: every repo indexed, rows in the order the repos were named.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-jobs-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

fn repo(dir: &Path, i: usize) {
    fs::create_dir_all(dir).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).arg(dir).status().unwrap().success());
    for f in 0..=i {
        fs::write(dir.join(format!("m{f}.py")), format!("def handler_{i}_{f}():\n    return {f}\n")).unwrap();
    }
}

fn index(base: &Path, args: &[&str]) -> Value {
    let out = Command::new(KC)
        .arg("index")
        .args(args)
        .arg("--json")
        .current_dir(base)
        .env("KNOWCODE_CACHE_DIR", base.join("cache"))
        .env("KNOWCODE_CONFIG_DIR", base.join("config"))
        .env("KNOWCODE_DATA_DIR", base.join("data"))
        .env("KNOWCODE_UPDATE_CHECK", "0")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn several_repos_at_once_index_every_one_in_order() {
    let base = scratch("ws");
    let ws = base.join("ws");
    let names: Vec<String> = (0..6).map(|i| format!("repo-{i}")).collect();
    for (i, n) in names.iter().enumerate() {
        repo(&ws.join(n), i);
    }
    for jobs in [None, Some("3"), Some("1")] {
        let _ = fs::remove_dir_all(base.join("cache"));
        let mut args = vec!["--workspace", ws.to_str().unwrap()];
        if let Some(j) = jobs {
            args.extend(["--jobs", j]);
        }
        let v = index(&base, &args);
        let rows = v["repos"].as_array().unwrap();
        let got: Vec<&str> = rows.iter().map(|r| Path::new(r["repo"].as_str().unwrap()).file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(got, names, "jobs {jobs:?}");
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r["report"]["parsed"], i + 1, "jobs {jobs:?}: {r}");
        }
    }
    let _ = fs::remove_dir_all(&base);
}
