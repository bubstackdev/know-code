//! `knowcode hook claude-code`: a Claude Code `PreToolUse` hook for the Grep and Glob tools. When the search target
//! is a name the index knows as a symbol, it adds a short note for the agent (where the symbol is defined, how many
//! references resolve to it and with what confidence, its top callers) and points at `kc_context` and `kc_refs`.
//!
//! It never stands in the tool's way. It answers from the index as it is (no sync, and no index is ever created),
//! checks that the definition is still where the index says, and on any error, panic or a slow answer it prints
//! nothing and exits 0, so the tool runs as if there were no hook.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kc_store::query::{self, RefHit, SymbolRow};
use serde_json::{Value, json};

use crate::ops::{file_lang, kind_word};

/// How long the hook may take before it gives up and says nothing (`KNOWCODE_HOOK_MS` overrides it).
const DEFAULT_BUDGET_MS: u64 = 1500;

/// Runs the hook: reads the PreToolUse JSON on stdin and prints the hook's JSON answer, or nothing. Always exits 0.
pub fn run() -> ! {
    let budget = std::env::var("KNOWCODE_HOOK_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_BUDGET_MS);
    // Fail open on a slow answer: the watchdog ends the process, silently, when the budget runs out.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(budget));
        std::process::exit(0);
    });
    std::panic::set_hook(Box::new(|_| {}));
    let answer = std::panic::catch_unwind(|| {
        let mut input = String::new();
        std::io::stdin().take(1 << 20).read_to_string(&mut input).ok()?;
        answer(&serde_json::from_str(&input).ok()?)
    });
    if let Ok(Some(out)) = answer {
        println!("{out}");
    }
    std::process::exit(0)
}

/// The hook's output for one PreToolUse event, or `None` to add nothing.
pub fn answer(event: &Value) -> Option<Value> {
    let tool = event["tool_name"].as_str()?;
    let input = &event["tool_input"];
    let name = target_name(tool, input["pattern"].as_str()?)?;
    let cwd = event["cwd"].as_str().map(PathBuf::from).or_else(|| std::env::current_dir().ok())?;
    let start = match input["path"].as_str() {
        Some(p) if cwd.join(p).exists() => cwd.join(p),
        _ => cwd,
    };
    let root = kc_index::repo_root(&start).ok()?;
    let note = note(&root, &name)?;
    Some(json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "additionalContext": note } }))
}

/// The symbol name a Grep or Glob call is looking for, when it looks like one: `UserService`, `\bget_user\b`,
/// `class UserService`, `Store.load\(`, or a Glob's file stem (`**/UserService*.cs`). Regexes, plain words and
/// paths aren't names.
pub fn target_name(tool: &str, pattern: &str) -> Option<String> {
    let mut s = pattern.trim();
    match tool {
        "Grep" => {
            s = s.trim_start_matches("\\b").trim_end_matches("\\b");
            for kw in ["class", "interface", "struct", "enum", "record", "trait", "protocol", "def", "fn", "func", "function", "type"] {
                for sep in [" ", "\\s+", "\\s"] {
                    if let Some(rest) = s.strip_prefix(kw).and_then(|r| r.strip_prefix(sep)) {
                        s = rest;
                    }
                }
            }
            for tail in ["\\s*\\(", "\\(", "(", "\\b"] {
                s = s.strip_suffix(tail).unwrap_or(s);
            }
            s = s.trim_start_matches("\\b");
            // `Store\.load`: an escaped dot is a member access.
            let unescaped = s.replace("\\.", ".");
            looks_like_symbol(&unescaped).then_some(unescaped)
        }
        "Glob" => {
            let file = s.rsplit('/').next()?;
            let stem = file.split('.').next()?.trim_matches('*');
            looks_like_symbol(stem).then(|| stem.to_string())
        }
        _ => None,
    }
}

/// An identifier (or `Type.member`) written like code: camelCase or PascalCase, or with an underscore, and at least
/// three characters. All-lower and all-upper words (`error`, `TODO`) are left to plain search.
fn looks_like_symbol(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    let ident = |p: &str| {
        let mut c = p.chars();
        c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    };
    if s.len() < 3 || parts.len() > 2 || !parts.iter().all(|p| ident(p)) {
        return false;
    }
    let (upper, lower) = (s.chars().any(|c| c.is_ascii_uppercase()), s.chars().any(|c| c.is_ascii_lowercase()));
    (upper && lower) || (s.contains('_') && s.chars().any(|c| c.is_ascii_alphabetic())) || parts.len() == 2
}

/// The note about `name` in the repo at `root`, from its index; `None` when the repo has no index, the name defines
/// nothing, or the index is out of date where it matters (the definition's line no longer names it).
pub fn note(root: &Path, name: &str) -> Option<String> {
    if !kc_index::db_path(root).ok()?.exists() {
        return None;
    }
    let store = kc_index::open(root).ok()?;
    let conn = store.conn();
    let mut defs: Vec<SymbolRow> = query::definitions(conn, name).ok()?.into_iter().filter(|d| still_there(root, d)).collect();
    if defs.is_empty() {
        return None;
    }
    // Code before tests, types before members.
    defs.sort_by_key(|d| (kc_store::is_test_path(&d.file), !matches!(d.kind.as_str(), "class" | "struct" | "enum" | "actor" | "protocol"), d.file.clone(), d.line));
    let (hits, _) = query::refs(conn, name, &[], usize::MAX).ok()?;
    let ids: Vec<i64> = defs.iter().map(|d| d.id).collect();
    let to_it: Vec<&RefHit> = hits.iter().filter(|h| ids.iter().any(|&id| h.may_target(id))).collect();

    let d = &defs[0];
    let kind = kind_word(&d.kind, file_lang(&d.file));
    let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
    let mut out = format!("KnowCode index: `{name}` is {article} {kind} defined at {}:{}", d.file, d.line);
    if defs.len() > 1 {
        out.push_str(&format!(" (+{} more definition{})", defs.len() - 1, if defs.len() > 2 { "s" } else { "" }));
    }
    if to_it.is_empty() {
        out.push_str(". No reference in the index resolves to it");
    } else {
        let mut by_conf: Vec<(&str, usize)> = Vec::new();
        for h in &to_it {
            match by_conf.iter_mut().find(|(c, _)| *c == h.confidence) {
                Some((_, n)) => *n += 1,
                None => by_conf.push((h.confidence, 1)),
            }
        }
        by_conf.sort_by_key(|(c, _)| confidence_rank(c));
        let conf: Vec<String> = by_conf.iter().map(|(c, n)| format!("{n} {c}")).collect();
        let refs = if to_it.len() == 1 { "reference resolves" } else { "references resolve" };
        out.push_str(&format!(". {} {refs} to it ({})", to_it.len(), conf.join(", ")));
        let callers = top_callers(&to_it, 3);
        if !callers.is_empty() {
            out.push_str(&format!("; top callers: {}", callers.join("; ")));
        }
    }
    out.push_str(&format!(
        ". For callers, members and source in one call use kc_context(symbol: \"{name}\"); every reference: kc_refs(name: \"{name}\")."
    ));
    Some(out)
}

/// Whether the definition's first lines on disk still name the symbol (a declaration may start with decorators or
/// attributes): a cheap check that the index isn't stale there. Any encoding: legacy files aren't always UTF-8.
fn still_there(root: &Path, d: &SymbolRow) -> bool {
    let Ok(bytes) = std::fs::read(root.join(&d.file)) else { return false };
    let text = String::from_utf8_lossy(&bytes);
    let first = d.line.saturating_sub(1) as usize;
    let span = (d.end_line.max(d.line) - d.line) as usize + 1;
    text.lines().skip(first).take(span.min(40)).any(|l| l.contains(d.name.as_str()))
}

fn confidence_rank(c: &str) -> u8 {
    match c {
        "exact" => 0,
        "unique_name" => 1,
        "ambiguous" => 2,
        "external_likely" => 3,
        _ => 4,
    }
}

/// The symbols referencing it most, outside tests first: `Caller.method (file:line, confidence)`.
fn top_callers(hits: &[&RefHit], n: usize) -> Vec<String> {
    let mut by_from: HashMap<String, (usize, &RefHit)> = HashMap::new();
    for h in hits {
        let Some(f) = &h.from else { continue };
        // A constructor by its language's word: `CartComponent.constructor`, not `.init`.
        let member = if f.kind == "init" { kind_word("init", file_lang(&h.file)) } else { f.name.as_str() };
        let who = f.container.as_ref().map_or_else(|| member.to_string(), |c| format!("{c}.{member}"));
        let e = by_from.entry(who).or_insert((0, h));
        e.0 += 1;
        if confidence_rank(h.confidence) < confidence_rank(e.1.confidence) {
            e.1 = h;
        }
    }
    let mut list: Vec<(String, (usize, &RefHit))> = by_from.into_iter().collect();
    list.sort_by(|a, b| {
        (kc_store::is_test_path(&a.1.1.file), std::cmp::Reverse(a.1.0), &a.0).cmp(&(kc_store::is_test_path(&b.1.1.file), std::cmp::Reverse(b.1.0), &b.0))
    });
    list.into_iter().take(n).map(|(who, (_, h))| format!("{who} ({}:{}, {})", h.file, h.line, h.confidence)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::{repo, test_cache};

    #[test]
    fn names_from_grep_and_glob_patterns() {
        for (tool, pattern, want) in [
            ("Grep", "UserService", Some("UserService")),
            ("Grep", "\\bget_user\\b", Some("get_user")),
            ("Grep", "class UserService", Some("UserService")),
            ("Grep", "def load_items\\(", Some("load_items")),
            ("Grep", "Store\\.load", Some("Store.load")),
            ("Grep", "getUser(", Some("getUser")),
            ("Grep", "error", None),
            ("Grep", "TODO", None),
            ("Grep", "foo.*bar", None),
            ("Grep", "(Get|Set)User", None),
            ("Grep", "user service", None),
            ("Grep", "ab", None),
            ("Glob", "**/UserService*.cs", Some("UserService")),
            ("Glob", "src/**/*.ts", None),
            ("Glob", "**/user-service.ts", None),
            ("Read", "UserService", None),
        ] {
            assert_eq!(target_name(tool, pattern).as_deref(), want, "{tool} {pattern}");
        }
    }

    #[test]
    fn adds_context_for_a_known_symbol_only() {
        let cache = test_cache();
        let base = cache.dir("hook");
        let root = repo(
            &base,
            "app",
            &[
                ("src/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
                ("src/Store.cs", "namespace App;\npublic class CartStore\n{\n    public int Save() { return 1; }\n}\n"),
                ("src/Worker.cs", "namespace App;\npublic class Worker\n{\n    private CartStore _s;\n    public int Run() { return _s.Save(); }\n    public int Again() { return _s.Save(); }\n}\n"),
            ],
        );
        let event = |tool: &str, pattern: &str| {
            json!({ "hook_event_name": "PreToolUse", "cwd": root, "tool_name": tool, "tool_input": { "pattern": pattern } })
        };
        let out = answer(&event("Grep", "CartStore")).expect("a known type gets a note");
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        let note = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(note.starts_with("KnowCode index: `CartStore` is a class defined at src/Store.cs:2"), "{note}");
        assert!(note.contains("kc_context(symbol: \"CartStore\")"), "{note}");
        let note = answer(&event("Grep", "CartStore.Save")).unwrap()["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string();
        assert!(note.contains("2 references resolve to it (2 exact)") && note.contains("top callers: Worker.Again"), "{note}");

        // Nothing for a name the index doesn't define, a regex, another tool, or a repo with no index.
        assert!(answer(&event("Grep", "NoSuchThing")).is_none());
        assert!(answer(&event("Grep", "Cart.*Store")).is_none());
        assert!(answer(&event("Read", "CartStore")).is_none());
        let bare = base.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(std::process::Command::new("git").arg("init").arg("-q").arg(&bare).status().unwrap().success());
        let mut e = event("Grep", "CartStore");
        e["cwd"] = json!(bare);
        assert!(answer(&e).is_none());
        assert!(!kc_index::db_path(&kc_index::repo_root(&bare).unwrap()).unwrap().exists(), "the hook never creates an index");

        // A stale index (the class was renamed since) says nothing rather than something wrong.
        std::fs::write(root.join("src/Store.cs"), "namespace App;\npublic class BasketStore\n{\n    public int Save() { return 1; }\n}\n").unwrap();
        assert!(answer(&event("Grep", "CartStore")).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }
}
