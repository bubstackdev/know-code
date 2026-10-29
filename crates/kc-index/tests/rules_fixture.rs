//! The rules gate's fixture set: every rule in `eval/rules-v1/rules` over the files in `eval/rules-v1/src` gives
//! exactly the matches the ast-grep command line gave on the same files (`eval/rules-v1/expected.jsonl`, written by
//! `scripts/eval/rules_gate.py reference`): the same (rule, file, start byte, end byte) set, severities and messages.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use kc_index::rules;

fn set_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/rules-v1")
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            files_under(&p, out);
        } else {
            out.push(p);
        }
    }
}

type Match = (String, String, usize, usize, String, String);

#[test]
fn rule_matches_equal_the_ast_grep_command_line() {
    let dir = set_dir();
    let mut rule_files = Vec::new();
    files_under(&dir.join("rules"), &mut rule_files);
    let texts: Vec<(String, String)> =
        rule_files.iter().map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(p).unwrap())).collect();
    let set = rules::load(&texts).unwrap();

    let src = dir.join("src");
    let mut sources = Vec::new();
    files_under(&src, &mut sources);
    let mut actual = BTreeSet::new();
    for p in &sources {
        let rel = p.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
        for h in rules::match_source(&set, 0, &rel, &std::fs::read_to_string(p).unwrap()) {
            actual.insert((h.rule, h.file, h.start, h.end, h.severity.to_string(), h.message));
        }
    }

    let expected: BTreeSet<Match> = std::fs::read_to_string(dir.join("expected.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            let s = |k: &str| v[k].as_str().unwrap().to_string();
            let n = |k: &str| v[k].as_u64().unwrap() as usize;
            (s("rule"), s("file"), n("start"), n("end"), s("severity"), s("message"))
        })
        .collect();
    let missing: Vec<_> = expected.difference(&actual).collect();
    let extra: Vec<_> = actual.difference(&expected).collect();
    assert!(missing.is_empty() && extra.is_empty(), "only ast-grep's: {missing:#?}\nonly KnowCode's: {extra:#?}");
    // Every rule in the set fires somewhere, so none passes by matching nothing on both sides.
    let fired: BTreeSet<&str> = expected.iter().map(|m| m.0.as_str()).collect();
    let unfired: Vec<&str> = set.ids.iter().map(|(id, _)| id.as_str()).filter(|id| !fired.contains(id)).collect();
    assert!(unfired.is_empty(), "rules with no match in the fixture: {unfired:?}");
}

/// The rule files ast-grep refuses (`eval/rules-v1/refused`: a merge key, a custom tag, a number where a pattern
/// goes, a null `not`, a duplicate field, an alias to another document's anchor) are refused here too.
#[test]
fn rule_files_the_ast_grep_command_line_refuses_are_refused() {
    let mut files = Vec::new();
    files_under(&set_dir().join("refused"), &mut files);
    assert!(files.len() >= 6);
    for p in files {
        let text = std::fs::read_to_string(&p).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(rules::load(&[(name.clone(), text)]).is_err(), "{name} loaded");
    }
}
