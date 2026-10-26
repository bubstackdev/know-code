//! `knowcode hook claude-code`: a Claude Code `PreToolUse` hook for the Grep, Glob and Bash tools. When the search
//! target is a name the index knows as a symbol, it adds a short note for the agent (where the symbol is defined, how
//! many references resolve to it and with what confidence, its top callers) and points at `kc_context` and `kc_refs`.
//! For Bash it acts only on code searches (`grep`, `rg`, `git grep`, `find -name` and the like, also inside pipelines
//! and `cd x && ...` chains), decided from the command string alone; any other command adds nothing.
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

/// At most this many names from one Bash search get a note.
const MAX_NAMES: usize = 3;

/// Runs the hook: reads the PreToolUse JSON on stdin and prints the hook's JSON answer, or nothing. Always exits 0.
pub fn run() -> ! {
    run_with(answer)
}

/// Runs a hook: reads its event's JSON on stdin and prints `answer`'s JSON, or nothing, within the time budget.
/// Always exits 0.
pub fn run_with(answer: fn(&Value) -> Option<Value>) -> ! {
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
    // What is searched for, and where, from the tool input alone: nothing touches the disk before this.
    let (names, dir, path) = match tool {
        "Bash" => {
            let s = bash_search(input["command"].as_str()?)?;
            (s.names, s.dir, s.path)
        }
        "Grep" | "Glob" => (vec![target_name(tool, input["pattern"].as_str()?)?], None, input["path"].as_str().map(PathBuf::from)),
        _ => return None,
    };
    let cwd = event["cwd"].as_str().map(PathBuf::from).or_else(|| std::env::current_dir().ok())?;
    let base = dir.map(|d| cwd.join(d)).filter(|d| d.is_dir()).unwrap_or(cwd);
    let start = match path {
        Some(p) if base.join(&p).exists() => base.join(p),
        _ => base,
    };
    let root = kc_index::repo_root(&start).ok()?;
    let note = notes(&root, &names)?;
    Some(json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "additionalContext": note } }))
}

/// A code search found in a Bash command: the symbol-like names it looks for, the directory a `cd` before it moved
/// to, and the first path it searches.
#[derive(Debug, Default, PartialEq)]
pub struct BashSearch {
    pub names: Vec<String>,
    pub dir: Option<PathBuf>,
    pub path: Option<PathBuf>,
}

/// The code search in a Bash command, when it has one that looks for a symbol: `grep`/`egrep`/`fgrep`, `rg`, `ag`,
/// `ack`, `git grep`, or `find -name`, alone or in a pipeline or a `&&`/`;` chain. Every other command is `None`,
/// decided from the string alone.
pub fn bash_search(command: &str) -> Option<BashSearch> {
    // Cheap first: a command that names no search tool at all is not split.
    if !["grep", "rg", "ag", "ack", "find"].iter().any(|w| command.contains(w)) {
        return None;
    }
    let mut found = BashSearch::default();
    let mut dir: Option<PathBuf> = None;
    for words in shell_commands(command) {
        let words = strip_wrappers(&words);
        let Some(first) = words.first() else { continue };
        let program = first.rsplit('/').next().unwrap_or(first);
        let args = &words[1..];
        let (patterns, operands, files) = match program {
            "cd" | "pushd" => {
                if let Some(d) = args.iter().find(|a| !a.starts_with('-')) {
                    let d = expand_home(d);
                    dir = Some(match dir.take() {
                        Some(prev) if d.is_relative() => prev.join(d),
                        _ => d,
                    });
                }
                continue;
            }
            "grep" | "egrep" | "fgrep" => grep_like(args, &GREP),
            "rg" => grep_like(args, &RG),
            "ag" => grep_like(args, &AG),
            "ack" | "ack-grep" => grep_like(args, &ACK),
            "git" => match git_subcommand(args) {
                Some(i) if args[i] == "grep" => grep_like(&args[i + 1..], &GIT_GREP),
                _ => continue,
            },
            "find" => find_names(args),
            _ => continue,
        };
        let before = found.names.len();
        for p in &patterns {
            for alt in alternatives(p) {
                push_name(&mut found.names, target_name("Grep", &alt));
            }
        }
        for f in &files {
            push_name(&mut found.names, target_name("Glob", f));
        }
        // The first search that names something says where to look.
        if before == 0 && !found.names.is_empty() {
            found.dir.clone_from(&dir);
            found.path = operands.first().map(|o| expand_home(o));
        }
    }
    found.names.truncate(MAX_NAMES);
    (!found.names.is_empty()).then_some(found)
}

fn push_name(names: &mut Vec<String>, name: Option<String>) {
    if let Some(n) = name.filter(|n| !names.contains(n)) {
        names.push(n);
    }
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => PathBuf::from(home).join(rest.trim_start_matches('/')),
        _ => PathBuf::from(p),
    }
}

/// The simple commands of a shell command line, as words with quotes and escapes resolved: split at `|`, `&`, `;`,
/// newlines, parentheses and backquotes outside quotes, with redirections and their targets dropped. Not a shell,
/// only enough of one to find the words of a search.
fn shell_commands(line: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    let end_word = |words: &mut Vec<String>, word: &mut String, in_word: &mut bool| {
        if std::mem::take(in_word) {
            words.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => match chars.peek().copied() {
                            Some(n @ ('"' | '\\' | '$' | '`')) => {
                                word.push(n);
                                chars.next();
                            }
                            Some('\n') => {
                                chars.next();
                            }
                            _ => word.push('\\'),
                        },
                        _ => word.push(q),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(n) => {
                    in_word = true;
                    word.push(n);
                }
            },
            ' ' | '\t' => end_word(&mut words, &mut word, &mut in_word),
            '>' | '<' => {
                // `2>` and `&>`: the digits before belong to the redirection.
                if in_word && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                end_word(&mut words, &mut word, &mut in_word);
                let dup = chars.peek() == Some(&'&');
                while matches!(chars.peek(), Some('>' | '<' | '&')) {
                    chars.next();
                }
                // `2>&1` and `>&-` name a descriptor, not a file word that follows.
                if dup {
                    while chars.peek().is_some_and(|d| d.is_ascii_digit() || *d == '-') {
                        chars.next();
                    }
                } else {
                    while matches!(chars.peek(), Some(' ' | '\t')) {
                        chars.next();
                    }
                    redirect_next(&mut chars);
                }
            }
            '|' | '&' | ';' | '\n' | '(' | ')' | '`' => {
                if c == '(' && word == "$" {
                    word.clear();
                    in_word = false;
                }
                end_word(&mut words, &mut word, &mut in_word);
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            _ => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut words, &mut word, &mut in_word);
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

/// Skips a redirection's target word (`> out.txt`, `2>/dev/null`), quoted or not.
fn redirect_next(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '|' | '&' | ';' | '(' | ')' | '<' | '>' | '`' => return,
            '\'' | '"' => {
                chars.next();
                for q in chars.by_ref() {
                    if q == c {
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
}

/// The command after leading `NAME=value` assignments and wrappers such as `sudo`, `time`, `env` or `xargs`.
fn strip_wrappers(words: &[String]) -> &[String] {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        let assignment = w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        match w {
            _ if assignment => i += 1,
            "sudo" | "command" | "time" | "nice" | "nohup" | "env" | "builtin" | "exec" => {
                i += 1;
                while i < words.len() && words[i].starts_with('-') {
                    i += 1;
                }
            }
            "xargs" => {
                i += 1;
                while i < words.len() && words[i].starts_with('-') {
                    let takes = matches!(words[i].as_str(), "-I" | "-n" | "-P" | "-L" | "-d" | "-s" | "-E" | "-J" | "-R" | "-S");
                    i += if takes { 2 } else { 1 };
                }
            }
            _ => break,
        }
    }
    &words[i.min(words.len())..]
}

/// Where `git`'s subcommand is, past global options such as `-C dir` or `--no-pager`.
fn git_subcommand(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" => i += 2,
            a if a.starts_with('-') => i += 1,
            _ => return Some(i),
        }
    }
    None
}

/// How one grep-like tool reads its options.
struct Grammar {
    /// Short options that take a value (`-A 3`, `-A3`).
    short_value: &'static str,
    /// Long options that take a value when it isn't given with `=`.
    long_value: &'static [&'static str],
    /// Options whose value is a search pattern (`-e`, `--regexp`); the first operand is then a path.
    short_pattern: &'static str,
    long_pattern: &'static [&'static str],
    /// Options whose value is a file name pattern (`ag -g`, `ack -g`).
    short_file: &'static str,
}

const GREP: Grammar = Grammar {
    short_value: "fmABCdD",
    long_value: &[
        "file", "max-count", "after-context", "before-context", "context", "include", "exclude", "exclude-dir", "exclude-from", "label",
        "binary-files", "devices", "directories", "group-separator",
    ],
    short_pattern: "e",
    long_pattern: &["regexp"],
    short_file: "",
};
const GIT_GREP: Grammar = Grammar {
    short_value: "fmABC",
    long_value: &["file", "max-count", "after-context", "before-context", "context", "max-depth", "threads"],
    short_pattern: "e",
    long_pattern: &["regexp"],
    short_file: "",
};
const RG: Grammar = Grammar {
    short_value: "fgtTmABCjMrEd",
    long_value: &[
        "file", "glob", "iglob", "type", "type-not", "type-add", "type-clear", "max-count", "after-context", "before-context", "context",
        "context-separator", "replace", "encoding", "max-depth", "threads", "max-columns", "max-filesize", "sort", "sortr", "color", "colors",
        "path-separator", "pre", "pre-glob", "engine", "ignore-file", "field-context-separator", "field-match-separator", "dfa-size-limit",
        "regex-size-limit", "hostname-bin", "hyperlink-format", "generate",
    ],
    short_pattern: "e",
    long_pattern: &["regexp"],
    short_file: "",
};
const AG: Grammar = Grammar {
    short_value: "ABCmGp",
    long_value: &["ignore", "ignore-dir", "depth", "file-search-regex", "after", "before", "context", "max-count", "path-to-ignore", "pager", "workers"],
    short_pattern: "",
    long_pattern: &[],
    short_file: "g",
};
const ACK: Grammar = Grammar {
    short_value: "ABCm",
    long_value: &["type", "ignore-dir", "noignore-dir", "ignore-file", "after-context", "before-context", "context", "max-count", "output"],
    short_pattern: "",
    long_pattern: &["match"],
    short_file: "g",
};

/// A grep-like command's search patterns, its operands after the pattern (paths), and its file name patterns.
fn grep_like(args: &[String], g: &Grammar) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut patterns, mut operands, mut files) = (Vec::new(), Vec::new(), Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            operands.extend(it.by_ref().cloned());
            break;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            let pattern = g.long_pattern.contains(&name);
            if pattern || g.long_value.contains(&name) {
                let value = inline.or_else(|| it.next().cloned());
                if let (true, Some(v)) = (pattern, value) {
                    patterns.push(v);
                }
            }
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            for (at, c) in a.char_indices().skip(1) {
                let (pattern, file) = (g.short_pattern.contains(c), g.short_file.contains(c));
                if pattern || file || g.short_value.contains(c) {
                    let rest = &a[at + c.len_utf8()..];
                    let value = if rest.is_empty() { it.next().cloned() } else { Some(rest.to_string()) };
                    match value {
                        Some(v) if pattern => patterns.push(v),
                        Some(v) if file => files.push(v),
                        _ => {}
                    }
                    break;
                }
            }
            continue;
        }
        operands.push(a.clone());
    }
    if patterns.is_empty() && files.is_empty() && !operands.is_empty() {
        patterns.push(operands.remove(0));
    }
    (patterns, operands, files)
}

/// `find`'s start paths and the file names it looks for (`-name`, `-iname`, `-path`, ...).
fn find_names(args: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let operands = args.iter().take_while(|a| !a.starts_with('-') && *a != "(" && *a != "!").cloned().collect();
    let files = args
        .windows(2)
        .filter(|w| matches!(w[0].as_str(), "-name" | "-iname" | "-path" | "-ipath" | "-wholename" | "-iwholename"))
        .map(|w| w[1].clone())
        .collect();
    (Vec::new(), operands, files)
}

/// The alternatives of a search pattern at its top level: `A|B`, `A\|B` (a basic regex), `(A|B)`, `\(A\|B\)`.
/// Alternatives inside a group stay whole, so `(Get|Set)User` is one alternative, and not a name.
fn alternatives(pattern: &str) -> Vec<String> {
    let mut p = pattern.trim();
    for (open, close) in [("\\(", "\\)"), ("(", ")")] {
        if let Some(inner) = p.strip_prefix(open).and_then(|r| r.strip_suffix(close))
            && !inner.contains(open)
        {
            p = inner;
        }
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('|') if depth <= 0 => out.push(std::mem::take(&mut cur)),
                Some(n) => {
                    depth += i32::from(n == '(') - i32::from(n == ')');
                    cur.push('\\');
                    cur.push(n);
                }
                None => cur.push('\\'),
            },
            '|' if depth <= 0 => out.push(std::mem::take(&mut cur)),
            _ => {
                depth += i32::from(c == '(') - i32::from(c == ')');
                cur.push(c);
            }
        }
    }
    out.push(cur);
    out
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

/// The notes about `names` in the repo at `root`, from its index, one line each, then the tools to call by the names
/// Claude sees them under; `None` when the repo has no index or none of the names has a note.
pub fn notes(root: &Path, names: &[String]) -> Option<String> {
    if !kc_index::db_path(root).ok()?.exists() {
        return None;
    }
    let store = kc_index::open(root).ok()?;
    let lines: Vec<String> = names.iter().filter_map(|n| note(store.conn(), root, n)).collect();
    if lines.is_empty() {
        return None;
    }
    let prefix = plugin_tool_prefix();
    let tools: Vec<String> = crate::mcp::ALWAYS_LOADED.iter().map(|t| format!("{t} ({prefix}{t})")).collect();
    Some(format!(
        "{}\nAlready loaded through the KnowCode plugin: {}. If one isn't listed yet, load it by that name with tool search.",
        lines.join("\n"),
        tools.join(", ")
    ))
}

/// The prefix Claude Code gives the bundled plugin's MCP tools, `mcp__plugin_<plugin>_<server>__`, from the plugin
/// manifest and server configuration that `knowcode setup claude` installs.
pub fn plugin_tool_prefix() -> String {
    let manifest: Value = serde_json::from_str(include_str!("../../../integrations/claude-code/.claude-plugin/plugin.json")).unwrap_or_default();
    let servers: Value = serde_json::from_str(include_str!("../../../integrations/claude-code/.mcp.json")).unwrap_or_default();
    let plugin = manifest["name"].as_str().unwrap_or("knowcode");
    let server = servers["mcpServers"].as_object().and_then(|m| m.keys().next().cloned()).unwrap_or_else(|| "knowcode".into());
    // Claude Code keeps letters, digits, `_` and `-` in MCP names and turns anything else into `_`.
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect::<String>();
    format!("mcp__plugin_{}_{}__", clean(plugin), clean(&server))
}

/// The note about `name`; `None` when the name defines nothing, or the index is out of date where it matters (the
/// definition's line no longer names it).
fn note(conn: &rusqlite::Connection, root: &Path, name: &str) -> Option<String> {
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
    fn names_from_bash_searches() {
        let names = |command: &str| bash_search(command).map(|s| s.names);
        let v = |n: &[&str]| Some(n.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        for (command, want) in [
            // The commands an agent ran on eShop instead of Grep.
            ("grep -rn \"GetItemById\" --include=*.cs .", v(&["GetItemById"])),
            ("grep -rn \"GetItemById\\|CatalogApi\" --include=\"*.cs\" src/", v(&["GetItemById", "CatalogApi"])),
            ("grep -rnE 'GetItemById|MapCatalogApi' src | head -20", v(&["GetItemById", "MapCatalogApi"])),
            ("cd eShop && grep -rn \"GetItemById\" .", v(&["GetItemById"])),
            ("cd src; grep -rn GetItemById . 2>/dev/null", v(&["GetItemById"])),
            ("grep -e CatalogApi -e GetItemById -r src", v(&["CatalogApi", "GetItemById"])),
            ("grep -rn -A3 -F 'CatalogApi.GetItemById' .", v(&["CatalogApi.GetItemById"])),
            ("grep -P '\\bGetItemById\\b' -r .", v(&["GetItemById"])),
            ("egrep -r '(GetItemById|GetItemsByIds)' src", v(&["GetItemById", "GetItemsByIds"])),
            ("rg -t cs -n 'GetItemById' src", v(&["GetItemById"])),
            ("rg --type cs -g '*.cs' -e GetItemById", v(&["GetItemById"])),
            ("ag GetItemById src", v(&["GetItemById"])),
            ("ack --type=csharp GetItemById", v(&["GetItemById"])),
            ("git grep -n 'GetItemById' -- '*.cs'", v(&["GetItemById"])),
            ("git -C repo --no-pager grep -n GetItemById", v(&["GetItemById"])),
            ("find . -name 'CatalogApi*.cs' -not -path '*/bin/*'", v(&["CatalogApi"])),
            ("find src -type f -iname \"*CatalogApi.cs\" | xargs grep -n GetItemById", v(&["CatalogApi", "GetItemById"])),
            ("LC_ALL=C grep -rn 'class CatalogApi' .", v(&["CatalogApi"])),
            ("echo $(grep -rln GetItemById src)", v(&["GetItemById"])),
            // Not searches, or not for a symbol.
            ("cargo build", None),
            ("git status", None),
            ("git log --grep=CatalogApi", None),
            ("sed -n 30,45p src/Catalog.API/Apis/CatalogApi.cs", None),
            ("cat x | grep -c foo", None),
            ("ls -la src", None),
            ("grep -rn 'foo.*bar' .", None),
            ("grep -rn '(Get|Set)User' .", None),
            ("find . -name '*.cs'", None),
            ("echo grep CatalogApi", None),
            ("grep -rn TODO src", None),
        ] {
            assert_eq!(names(command), want, "{command}");
        }
        let s = bash_search("cd app && grep -rn \"CartStore\" src/Store.cs").unwrap();
        assert_eq!((s.dir, s.path), (Some(PathBuf::from("app")), Some(PathBuf::from("src/Store.cs"))));
        let s = bash_search("grep -rn CartStore --include=*.cs > out.txt").unwrap();
        assert_eq!(s.path, None, "a redirection's target isn't a searched path");
        assert_eq!(plugin_tool_prefix(), "mcp__plugin_knowcode_knowcode__");
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

        // A search through Bash gets the same note, from a `cd` chain too, naming the tools as the plugin loads them.
        let bash = |cwd: &Path, command: &str| {
            let e = json!({ "hook_event_name": "PreToolUse", "cwd": cwd, "tool_name": "Bash", "tool_input": { "command": command, "description": "Search" } });
            answer(&e).map(|o| o["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string())
        };
        let note = bash(&root, "grep -rn \"CartStore\" --include=*.cs .").expect("a grep for a known type gets a note");
        assert!(note.starts_with("KnowCode index: `CartStore` is a class defined at src/Store.cs:2"), "{note}");
        for t in ["kc_find", "kc_context", "kc_refs", "kc_impact", "kc_search"] {
            assert!(note.contains(&format!("{t} (mcp__plugin_knowcode_knowcode__{t})")), "{note}");
        }
        let note = bash(&base, "cd app && grep -rn \"CartStore\" .").expect("the search runs where `cd` went");
        assert!(note.contains("src/Store.cs:2"), "{note}");
        let note = bash(&root, "rg -n 'CartStore.Save|Worker' src").expect("both alternatives");
        assert!(note.contains("`CartStore.Save`") && note.contains("`Worker`"), "{note}");
        assert_eq!(note.matches("Already loaded through the KnowCode plugin").count(), 1, "{note}");
        assert!(bash(&root, "find . -name 'CartStore*.cs'").is_some());
        for command in ["cargo build", "git status", "sed -n 1,20p src/Store.cs", "cat src/Store.cs | grep -c foo", "grep -rn NoSuchThing ."] {
            assert!(bash(&root, command).is_none(), "{command}");
        }

        // A stale index (the class was renamed since) says nothing rather than something wrong.
        std::fs::write(root.join("src/Store.cs"), "namespace App;\npublic class BasketStore\n{\n    public int Save() { return 1; }\n}\n").unwrap();
        assert!(answer(&event("Grep", "CartStore")).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }
}
