//! Git invocations with user input: a revision never becomes an option, and diff paths are read exactly.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{diff, ownership, vitals};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("repo")).unwrap();
    let repo = dir.join("repo");
    assert!(Command::new("git").arg("init").arg("-q").arg(&repo).status().unwrap().success());
    dir
}

fn git_in(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@x", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn commit_all(dir: &Path) {
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-qm", "c"]);
}

/// Every file under `dir`, recursively, outside `.git`.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Strings a user could pass where a revision goes, each of which git would read as an option if it reached git
/// unresolved; several would make git write a file.
fn hostile(dir: &Path) -> Vec<String> {
    let out = dir.join("written");
    vec![
        format!("--output={}", out.display()),
        format!("--output={}^{{commit}}", out.display()),
        "-o".to_string(),
        "--no-index".to_string(),
        format!("-c core.pager=touch {}", out.display()),
        "--exec-path".to_string(),
        "-".to_string(),
        String::new(),
        "\0HEAD".to_string(),
    ]
}

#[test]
fn invariant_git_revision_option_never_reaches_git_as_an_option() {
    let dir = scratch("git-revision-option");
    let repo = dir.join("repo");
    std::fs::write(repo.join("a.py"), "def run(): return 1\n").unwrap();
    commit_all(&repo);
    std::fs::write(repo.join("a.py"), "def run(): return 2\n").unwrap();
    let before = files_under(&dir);
    let store = kc_index::open(&repo).unwrap();
    for rev in hostile(&dir) {
        assert!(diff::changes(&repo, Some(&rev)).is_err(), "{rev:?} diffed");
        assert!(ownership::diff_base(&repo, &rev).is_none(), "{rev:?} had a base");
        let span = vitals::Span { as_of: Some(rev.clone()), since: None };
        assert!(vitals::coupled_in(&repo, &store, &["a.py"], 10, false, &span).is_err(), "{rev:?} walked");
        assert_eq!(files_under(&dir), before, "{rev:?} wrote a file");
    }
    // An ordinary revision still works, and a commit names itself by id.
    let head = diff::diff(&repo, Some("HEAD")).unwrap().base;
    assert_eq!(head.len(), 40);
    assert_eq!(diff::changes(&repo, Some(&head[..8])).unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `(file, renamed_from, ranges, deleted)` of a change.
type Change = (String, Option<String>, Vec<(u32, u32)>, bool);

#[test]
fn quoted_path_names_are_read_exactly() {
    let dir = scratch("quoted-path");
    let repo = dir.join("repo");
    let names = ["a space.py", "tab\there.py", "ünïcödé.py", "quote\"d.py", "back\\slash.py", "plain.py", "old dir/moved.py", "gone file.py"];
    for n in names {
        let p = repo.join(n);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "def one():\n    return 1\n\n\ndef two():\n    return 2\n").unwrap();
    }
    commit_all(&repo);
    for n in &names[..6] {
        std::fs::write(repo.join(n), "def one():\n    return 1\n\n\ndef two():\n    return 3\n").unwrap();
    }
    // Moved (staged, so git sees the rename) and edited; deleted.
    std::fs::create_dir_all(repo.join("new dir")).unwrap();
    git_in(&repo, &["mv", "old dir/moved.py", "new dir/moved.py"]);
    std::fs::write(repo.join("new dir/moved.py"), "def one():\n    return 1\n\n\ndef two():\n    return 2\n\ndef three():\n    return 3\n").unwrap();
    std::fs::remove_file(repo.join("gone file.py")).unwrap();
    let mut got: Vec<Change> =
        diff::changes(&repo, None).unwrap().into_iter().map(|c| (c.file, c.renamed_from, c.ranges, c.deleted)).collect();
    got.sort();
    let mut want: Vec<Change> =
        names[..6].iter().map(|n| (n.to_string(), None, vec![(6, 6)], false)).collect();
    want.push(("gone file.py".into(), None, vec![(1, 1)], true));
    want.push(("new dir/moved.py".into(), Some("old dir/moved.py".into()), vec![(7, 9)], false));
    want.sort();
    assert_eq!(got, want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn old_definitions_come_from_the_base_commit() {
    let dir = scratch("old-definitions");
    let repo = dir.join("repo");
    std::fs::write(repo.join("api.py"), "def save():\n    return 1\n\n\ndef load():\n    return 2\n").unwrap();
    std::fs::write(repo.join("other.py"), "def keep():\n    return 1\n").unwrap();
    std::fs::write(repo.join("new.py"), "def fresh():\n    return 1\n").unwrap();
    commit_all(&repo);
    std::fs::remove_file(repo.join("api.py")).unwrap();
    // Lines added only: nothing of the old version can be gone, so it isn't read.
    std::fs::write(repo.join("new.py"), "def fresh():\n    return 1\n\n\ndef more():\n    return 2\n").unwrap();
    std::fs::write(repo.join("other.py"), "def kept():\n    return 1\n").unwrap();
    let d = diff::diff(&repo, None).unwrap();
    let mut defs: Vec<(String, String, u32, Option<String>)> =
        diff::old_definitions(&repo, &d).unwrap().into_iter().map(|o| (o.name, o.file, o.line, o.now)).collect();
    defs.sort();
    assert_eq!(
        defs,
        [
            ("keep".to_string(), "other.py".to_string(), 1, Some("other.py".to_string())),
            ("load".to_string(), "api.py".to_string(), 5, None),
            ("save".to_string(), "api.py".to_string(), 1, None),
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
