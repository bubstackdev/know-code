//! GitLab CI: `.gitlab-ci.yml` and the files pipelines include.
//!
//! A file is GitLab CI by its name (`.gitlab-ci.yml`, `*.gitlab-ci.yml`, `.gitlab-ci-*.yml`), or, for other YAML that
//! isn't Ansible, by its content: a top-level map with a job (a key whose value has `script`, `extends`, `trigger`,
//! `stage`, …), a hidden job (`.x`) holding `variables`, or an `include` of `local`/`project`/`remote` files, which
//! [`sniff`] checks. Shared template repos and per-project `ci.yml` files are found so.
//!
//! YAML is read as the Ansible extractor reads it (see [`crate::yaml`]). Each job, hidden ones (`.x`, tagged
//! `hidden`) included, is a `job` symbol. Everything a pipeline is resolved from is a fact ([`AnsibleFact`], stored
//! with Ansible's), with the job as its symbol (none for the file's top level):
//!
//! - `ci_include` (a top-level `include:` entry) and `ci_trigger` (a job's child pipeline, `trigger: include:`):
//!   `name` is `local`, `project`, `remote`, `template` or `component`, `value` JSON with `file`, `project`, `ref` and
//!   `rules` (each `if:` condition, `never:` before one whose rule says `when: never`); a multi-project trigger
//!   (`trigger: project:`) is `name` `project` with no `file`;
//! - `ci_extends`: `name` a template the job extends, in order;
//! - `ci_var`: a variable of the job, or of the file (`variables:` at the top level), `name` and `value` as written;
//! - `ci_stage`: `name` the job's stage;
//! - `ci_matrix`: an entry of the job's `parallel: matrix:`, `value` JSON (each variable's value or values);
//! - `ci_input`: an input a file's `spec:` header declares (a component), `value` its default as JSON;
//! - `ci_rules`: the job has `rules:` (`name` empty), then `ci_rule` for each, in order: `name` its `when` (empty
//!   for the default), `value` its `if:` condition (empty for none); a `!reference` rule is `name` `reference`,
//!   `value` the job and key it names;
//! - `ci_only` and `ci_except`: `name` a ref pattern of the job's legacy `only:`/`except:` (or their `refs:`);
//! - `ci_ruledef`: a rule kept under a key of a (hidden) job for `!reference` (`.rules: {on_default: {if: …}}`):
//!   `name` the key, `value` its `when` and `if`, tab-separated;
//! - `ci_script`: `name` the section (`before_script`, `script`, `after_script`; at the top level, or in `default:`,
//!   the default for jobs without one), `value` one step of it, in order: the section itself (empty), `set NAME expr`,
//!   `default NAME expr` and `bset NAME expr` (a shell assignment; `default` when guarded by `[[ -z "$NAME" ]]`,
//!   `bset` inside a branch of another `if`), `cd dir` and `popd`,
//!   `clone url dir` (`git clone`), `reference job section` (`!reference`), and `playbook path inventory vars` (an
//!   `ansible-playbook` command: the playbook as written, its `-i`, and its `--extra-vars`/`-e` pairs as `K=V`
//!   lines), `tool name args…` (another deploy command: `wrangler deploy`, `aws s3 sync`, `terraform apply`,
//!   `docker build`, `kubectl apply`, `helm upgrade`, `sam deploy`, `cdk deploy`, …; see [`deploy_op`]) and `run pm
//!   script args…` (`npm run script`, `yarn run script`, `pnpm run script`, whose body is read at query time), fields
//!   separated by tabs. Values keep `$VAR` as written; the pipeline is resolved at query time (`kc_index::ci`).

use crate::yaml::{Node, Val, parse};
use crate::{AnsibleFact, FileFacts, Symbol, SymbolKind};

pub const VERSION: u32 = 5;

const SIGNATURE_CAP: usize = 160;

/// Keys at a CI file's top level that are not jobs.
const RESERVED: &[&str] =
    &["image", "services", "cache", "stages", "types", "before_script", "after_script", "variables", "include", "default", "workflow", "spec"];

/// Keys that make a map a job.
const JOB_KEYS: &[&str] = &["script", "extends", "trigger", "stage", "before_script", "after_script", "needs", "run"];

fn is_yaml(path: &str) -> bool {
    path.ends_with(".yml") || path.ends_with(".yaml")
}

/// A file GitLab CI by its name.
pub fn is_ci_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    is_yaml(name) && (name.starts_with(".gitlab-ci") || name.contains(".gitlab-ci.") || name.starts_with("gitlab-ci."))
}

/// A YAML file the path doesn't place, which may be GitLab CI: read it and ask [`sniff`]. Ansible files, compose
/// files, other CI systems' files and dotted directories other than `.gitlab/` are never.
pub fn candidate(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    is_yaml(path)
        && !is_ci_path(path)
        && crate::ansible::place(path).is_none()
        && !name.starts_with("docker-compose")
        && !name.starts_with("compose.")
        && !matches!(name, ".travis.yml" | "azure-pipelines.yml" | "bitbucket-pipelines.yml" | "mkdocs.yml" | "pnpm-lock.yaml")
        && !path.split('/').any(|c| c.starts_with('.') && c.len() > 1 && c != ".gitlab")
}

/// Whether YAML text is GitLab CI: a top-level map with a job, a hidden job holding variables, or an `include` of
/// CI files. Reads lines only.
pub fn sniff(text: &str) -> bool {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut top: Option<String> = None;
    let mut child_indent: Option<usize> = None;
    for line in text.lines() {
        let trimmed = line.trim_end();
        let body = trimmed.trim_start();
        if body.is_empty() || body.starts_with('#') || trimmed == "---" {
            continue;
        }
        let indent = trimmed.len() - body.len();
        if indent == 0 {
            if body.starts_with('-') || body.starts_with('[') || body.starts_with('{') {
                return false; // a list: Ansible, or data
            }
            let Some((k, _)) = body.split_once(':') else { return false };
            top = Some(k.trim().trim_matches(['"', '\'']).to_string());
            child_indent = None;
            continue;
        }
        let Some(t) = &top else { continue };
        let item = body.strip_prefix("- ").unwrap_or(body);
        let key = item.split_once(':').map(|(k, _)| k.trim().trim_matches(['"', '\''])).unwrap_or("");
        // A CI component or a template taking inputs: a `spec: inputs:` header document.
        if t == "spec" && key == "inputs" && text.lines().any(|l| l.trim_end() == "---") {
            return true;
        }
        if t == "include" {
            if matches!(key, "local" | "project" | "remote" | "template" | "component") {
                return true;
            }
            continue;
        }
        if RESERVED.contains(&t.as_str()) {
            continue;
        }
        let ci = *child_indent.get_or_insert(indent);
        if indent != ci || body.starts_with('-') {
            continue;
        }
        if JOB_KEYS.contains(&key) || (t.starts_with('.') && matches!(key, "variables" | "rules" | "image" | "tags")) {
            return true;
        }
    }
    false
}

fn collapse(s: &str, cap: usize) -> String {
    let mut out = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.len() > cap {
        let mut cut = cap;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

struct Ex<'a> {
    src: &'a str,
    line_starts: Vec<usize>,
    f: FileFacts,
}

impl Ex<'_> {
    fn byte_of_line(&self, line: u32) -> u32 {
        self.line_starts.get(line.saturating_sub(1) as usize).copied().unwrap_or(self.src.len()) as u32
    }

    fn end_byte_of_line(&self, line: u32) -> u32 {
        self.line_starts.get(line as usize).map_or(self.src.len(), |&b| b.saturating_sub(1)) as u32
    }

    fn fact(&mut self, kind: &'static str, sym: Option<usize>, name: &str, value: &str, line: u32) {
        self.f.ansible.push(AnsibleFact { kind, sym, name: name.to_string(), value: value.to_string(), line });
    }

    fn job(&mut self, name: &str, line: u32, v: &Node) {
        let extends: Vec<(String, u32)> = v.get("extends").map(|e| e.strings().into_iter().map(|(s, l)| (s.to_string(), l)).collect()).unwrap_or_default();
        let stage = v.get("stage").and_then(Node::str);
        let mut sig = format!("job {name}");
        if let Some(s) = stage {
            sig.push_str(&format!(" (stage {s})"));
        }
        if !extends.is_empty() {
            let names: Vec<&str> = extends.iter().map(|(n, _)| n.as_str()).collect();
            sig.push_str(&format!(": extends {}", names.join(", ")));
        }
        let end = v.end.max(line);
        self.f.symbols.push(Symbol {
            parent: None,
            name: name.to_string(),
            kind: SymbolKind::Job,
            container: None,
            signature: collapse(&sig, SIGNATURE_CAP),
            doc: None,
            attrs: Vec::new(),
            tags: if name.starts_with('.') { vec!["hidden"] } else { Vec::new() },
            start_line: line,
            end_line: end,
            start_byte: self.byte_of_line(line),
            end_byte: self.end_byte_of_line(end),
            complexity: 0,
            nesting: 0,
            bumps: Vec::new(),
            cond_ops: 0,
            cond_line: 0,
            type_name: None,
            arity: None,
            lambda_types: None,
        });
        let id = Some(self.f.symbols.len() - 1);
        for (e, l) in &extends {
            self.fact("ci_extends", id, e, "", *l);
        }
        if let Some(s) = v.get("stage").and_then(|s| s.str().map(|t| (t.to_string(), s.line))) {
            self.fact("ci_stage", id, &s.0, "", s.1);
        }
        if let Some(vars) = v.get("variables") {
            self.variables(id, vars);
        }
        if let Some(rules) = v.get("rules") {
            self.fact("ci_rules", id, "", "", rules.line);
            for r in rules.seq() {
                match &r.v {
                    Val::Str(s) if s.starts_with("!reference ") => self.fact("ci_rule", id, "reference", &s["!reference ".len()..], r.line),
                    Val::Map(_) => {
                        let when = r.get("when").and_then(Node::str).unwrap_or("");
                        let cond = r.get("if").and_then(Node::str).unwrap_or("");
                        self.fact("ci_rule", id, when, cond, r.line);
                    }
                    _ => {}
                }
            }
        }
        for (key, kind) in [("only", "ci_only"), ("except", "ci_except")] {
            if let Some(o) = v.get(key) {
                let refs = if matches!(o.v, Val::Map(_)) { o.get("refs").map(Node::strings).unwrap_or_default() } else { o.strings() };
                for (r, line) in refs {
                    self.fact(kind, id, r, "", line);
                }
            }
        }
        for (k, kline, n) in v.map() {
            if matches!(k.as_str(), "before_script" | "script" | "after_script") {
                self.script(id, k, *kline, n);
            } else if matches!(n.v, Val::Map(_)) && (n.get("if").is_some() || n.get("when").is_some()) && !JOB_KEYS.contains(&k.as_str()) {
                let when = n.get("when").and_then(Node::str).unwrap_or("");
                let cond = n.get("if").and_then(Node::str).unwrap_or("");
                self.fact("ci_ruledef", id, k, &format!("{when}\t{cond}"), *kline);
            }
        }
        // `parallel: matrix:`: each entry (variable → value or values) runs the job once per combination.
        if let Some(m) = v.get("parallel").and_then(|p| p.get("matrix")) {
            for entry in m.seq().iter().filter(|e| matches!(e.v, Val::Map(_))) {
                self.fact("ci_matrix", id, "", &to_json(entry).to_string(), entry.line);
            }
        }
        if let Some(t) = v.get("trigger") {
            match &t.v {
                Val::Str(p) => self.include_fact("ci_trigger", id, "project", Some(p), None, None, &[], t.line),
                Val::Map(_) => {
                    if let Some(inc) = t.get("include") {
                        self.includes("ci_trigger", id, inc);
                    } else if let Some(p) = t.get("project").and_then(Node::str) {
                        let r = t.get("branch").and_then(Node::str);
                        self.include_fact("ci_trigger", id, "project", Some(p), r, None, &[], t.line);
                    } else {
                        self.unknown_include("ci_trigger", id, t);
                    }
                }
                _ => self.unknown_include("ci_trigger", id, t),
            }
        }
    }

    /// An include or trigger in a form this doesn't read (a map with none of `project`, `local`, `remote`,
    /// `template`, `component` or `artifact` as a string; a nested list): an include of kind `unknown`, its keys as
    /// its target, so the pipeline accounts for it (`unsupported`) rather than dropping it.
    fn unknown_include(&mut self, kind: &'static str, sym: Option<usize>, i: &Node) {
        let what = match &i.v {
            Val::Map(_) => format!("{{{}}}", i.map().iter().map(|(k, ..)| k.as_str()).collect::<Vec<_>>().join(", ")),
            Val::Seq(_) => "[…]".to_string(),
            _ => "…".to_string(),
        };
        self.include_fact(kind, sym, "unknown", None, None, Some(&what), &[], i.line);
    }

    fn variables(&mut self, sym: Option<usize>, vars: &Node) {
        for (k, line, v) in vars.map() {
            // `NAME: value`, or `NAME: {value: …, description: …}`.
            let value = match &v.v {
                Val::Str(s) => Some(s.clone()),
                Val::Map(_) => v.get("value").and_then(Node::str).map(str::to_string),
                Val::Null => Some(String::new()),
                Val::Seq(_) => None,
            };
            if let Some(value) = value {
                self.fact("ci_var", sym, k, &value, *line);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn include_fact(&mut self, kind: &'static str, sym: Option<usize>, how: &str, project: Option<&str>, git_ref: Option<&str>, file: Option<&str>, rules: &[String], line: u32) {
        let mut v = serde_json::Map::new();
        if let Some(p) = project {
            v.insert("project".into(), p.into());
        }
        if let Some(r) = git_ref {
            v.insert("ref".into(), r.into());
        }
        if let Some(f) = file {
            v.insert("file".into(), f.into());
        }
        if !rules.is_empty() {
            v.insert("rules".into(), rules.into());
        }
        self.fact(kind, sym, how, &serde_json::Value::Object(v).to_string(), line);
    }

    /// The entries of an `include:` (a string, a map, or a list of either).
    fn includes(&mut self, kind: &'static str, sym: Option<usize>, inc: &Node) {
        let items: Vec<&Node> = match &inc.v {
            Val::Seq(items) => items.iter().collect(),
            _ => vec![inc],
        };
        for i in items {
            match &i.v {
                Val::Str(s) => {
                    let how = if s.starts_with("http://") || s.starts_with("https://") { "remote" } else { "local" };
                    self.include_fact(kind, sym, how, None, None, Some(s), &[], i.line);
                }
                Val::Map(_) => {
                    let before = self.f.ansible.len();
                    self.include_map(kind, sym, i);
                    if self.f.ansible.len() == before {
                        self.unknown_include(kind, sym, i);
                    }
                    // A component's or template's `inputs:`, for interpolating `$[[ inputs.x ]]` in the file included.
                    if let Some(inputs) = i.get("inputs").map(to_json).filter(|v| v.is_object()) {
                        for f in &mut self.f.ansible[before..] {
                            if let Ok(serde_json::Value::Object(mut m)) = serde_json::from_str::<serde_json::Value>(&f.value) {
                                m.insert("inputs".into(), inputs.clone());
                                f.value = serde_json::Value::Object(m).to_string();
                            }
                        }
                    }
                }
                _ => self.unknown_include(kind, sym, i),
            }
        }
    }

    /// One `include:` entry written as a map.
    fn include_map(&mut self, kind: &'static str, sym: Option<usize>, i: &Node) {
        let rules: Vec<String> = i
            .get("rules")
            .map(|r| {
                r.seq()
                    .iter()
                    .map(|rule| {
                        let never = rule.get("when").and_then(Node::str) == Some("never");
                        let cond = rule.get("if").and_then(Node::str);
                        let exists = rule.get("exists").map(|e| e.strings().into_iter().map(|(s, _)| s).collect::<Vec<_>>().join(","));
                        match (cond, exists) {
                            (Some(c), _) => format!("{}{c}", if never { "never:" } else { "if:" }),
                            (None, Some(e)) => format!("exists:{e}"),
                            (None, None) => if never { "never:".into() } else { "if:".into() },
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let project = i.get("project").and_then(Node::str);
        let git_ref = i.get("ref").and_then(Node::str);
        if let Some(p) = project {
            let files: Vec<(&str, u32)> = i.get("file").map(|f| f.strings()).unwrap_or_default();
            if files.is_empty() {
                self.include_fact(kind, sym, "project", Some(p), git_ref, None, &rules, i.line);
            }
            for (f, line) in files {
                self.include_fact(kind, sym, "project", Some(p), git_ref, Some(f), &rules, line);
            }
            return;
        }
        for how in ["local", "remote", "template", "component"] {
            if let Some(n) = i.get(how)
                && let Some(f) = n.str()
            {
                self.include_fact(kind, sym, how, None, None, Some(f), &rules, n.line);
            }
        }
        // `artifact:` (a generated child pipeline) can't be read.
        if let Some(a) = i.get("artifact").and_then(Node::str) {
            self.include_fact(kind, sym, "artifact", None, None, Some(a), &rules, i.line);
        }
    }

    /// A script section: its steps, in order (see the module doc).
    fn script(&mut self, sym: Option<usize>, section: &str, key_line: u32, n: &Node) {
        self.fact("ci_script", sym, section, "", key_line);
        let mut items: Vec<(&str, u32)> = Vec::new();
        flatten(n, &mut items);
        for (text, line) in items {
            if let Some(rest) = text.strip_prefix("!reference ") {
                let mut parts = rest.split_whitespace();
                let job = parts.next().unwrap_or("");
                let key = parts.next().unwrap_or("");
                self.fact("ci_script", sym, section, &format!("reference\t{job}\t{key}"), line);
                continue;
            }
            // A plain or quoted scalar's line breaks fold into spaces, so offsets in its text undercount: each step
            // is placed on the first source line from there on that holds its command.
            let mut at = line;
            for (op, offset) in steps(text) {
                at = self.line_of(&op, (line + offset).max(at));
                self.fact("ci_script", sym, section, &op, at);
            }
        }
    }

    /// The first line from `from` on (within a few hundred) holding the text a step comes from, else `from`.
    fn line_of(&self, op: &str, from: u32) -> u32 {
        let mut f = op.split('\t');
        let needle = match f.next().unwrap_or("") {
            "tool" | "run" => {
                // The command word, then its first argument on the same line (`aws s3`, `npm run deploy`).
                let word = f.next().unwrap_or("");
                let first = f.next().unwrap_or("");
                let start = from.saturating_sub(1) as usize;
                let hit = self.src.lines().skip(start).take(400).position(|l| l.find(word).is_some_and(|i| l[i + word.len()..].contains(first)));
                return hit.map_or(from, |i| (start + i + 1) as u32);
            }
            "playbook" => "ansible-playbook".to_string(),
            "clone" => f.next().unwrap_or("clone").to_string(),
            "set" | "default" | "bset" => format!("{}=", f.next().unwrap_or("")),
            "cd" => f.next().unwrap_or("cd").to_string(),
            other => other.to_string(),
        };
        let start = from.saturating_sub(1) as usize;
        self.src.lines().skip(start).take(400).position(|l| l.contains(&needle)).map_or(from, |i| (start + i + 1) as u32)
    }
}

/// The strings of a script: a scalar, or a list of them (nested lists, as `!reference` makes, flattened), each with
/// the line its text starts on (a block scalar's first line, after its indicator).
fn flatten<'a>(n: &'a Node, out: &mut Vec<(&'a str, u32)>) {
    match &n.v {
        Val::Str(s) => out.push((s, n.line)),
        Val::Seq(items) => {
            for i in items {
                flatten(i, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Shell.

/// A word of a shell statement: quotes removed, `$…` expansions kept as written.
#[derive(Debug, Clone, PartialEq)]
struct Word {
    text: String,
    /// Some of it was single-quoted (no expansion there).
    single: bool,
}

/// Statements of a shell text, each with its words and the line offset it starts at. Statements are split at unquoted
/// newlines (not after `\`), `;`, `&&`, `||`, `|` and `&`; `then`, `do`, `else` and `elif` opening a statement are
/// dropped, `fi`, `done` and `esac` are statements of their own.
fn statements(text: &str) -> Vec<(Vec<Word>, u32)> {
    let b = text.as_bytes();
    let mut out: Vec<(Vec<Word>, u32)> = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut single = false;
    let mut in_word = false;
    let mut line = 0u32;
    let mut start_line = 0u32;
    let mut i = 0;
    let end_word = |cur: &mut Vec<u8>, single: &mut bool, in_word: &mut bool, words: &mut Vec<Word>| {
        if *in_word {
            words.push(Word { text: String::from_utf8_lossy(&std::mem::take(cur)).into_owned(), single: *single });
            *single = false;
            *in_word = false;
        }
    };
    let end_stmt = |words: &mut Vec<Word>, out: &mut Vec<(Vec<Word>, u32)>, start: u32| {
        while words.first().is_some_and(|w| matches!(w.text.as_str(), "then" | "do" | "else" | "elif" | "{" | "(")) {
            words.remove(0);
        }
        if !words.is_empty() {
            out.push((std::mem::take(words), start));
        }
    };
    while i < b.len() {
        let c = b[i];
        match c {
            b'\\' if b.get(i + 1) == Some(&b'\n') => {
                line += 1;
                i += 2;
                continue;
            }
            b'\\' => {
                if let Some(&n) = b.get(i + 1) {
                    cur.push(n);
                    in_word = true;
                }
                i += 2;
                continue;
            }
            b'\'' => {
                let close = text[i + 1..].find('\'').map_or(b.len(), |e| i + 1 + e);
                line += text[i..close.min(b.len())].matches('\n').count() as u32;
                cur.extend_from_slice(&b[i + 1..close.min(b.len())]);
                single = true;
                in_word = true;
                i = close + 1;
                continue;
            }
            b'"' => {
                // A double-quoted string: `$(…)` inside it may hold quotes of its own.
                let mut j = i + 1;
                let mut depth = 0;
                while j < b.len() {
                    match b[j] {
                        b'\\' => j += 1,
                        b'(' if j > 0 && b[j - 1] == b'$' => depth += 1,
                        b')' if depth > 0 => depth -= 1,
                        b'"' if depth == 0 => break,
                        _ => {}
                    }
                    j += 1;
                }
                let inner = &b[i + 1..j.min(b.len())];
                line += inner.iter().filter(|&&x| x == b'\n').count() as u32;
                cur.extend_from_slice(inner);
                in_word = true;
                i = j + 1;
                continue;
            }
            b'$' if b.get(i + 1) == Some(&b'(') => {
                // `$(…)` as one piece of the word.
                let mut j = i + 2;
                let mut depth = 1;
                while j < b.len() && depth > 0 {
                    match b[j] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    j += 1;
                }
                cur.extend_from_slice(&b[i..j.min(b.len())]);
                in_word = true;
                i = j;
                continue;
            }
            b'#' if !in_word => {
                // A comment, to the end of the line.
                i = text[i..].find('\n').map_or(b.len(), |e| i + e);
                continue;
            }
            b'\n' | b';' | b'&' | b'|' => {
                end_word(&mut cur, &mut single, &mut in_word, &mut words);
                end_stmt(&mut words, &mut out, start_line);
                if c == b'\n' {
                    line += 1;
                }
                if (c == b'&' || c == b'|') && b.get(i + 1) == Some(&c) {
                    i += 1;
                }
                i += 1;
                start_line = line;
                continue;
            }
            c if c.is_ascii_whitespace() => {
                end_word(&mut cur, &mut single, &mut in_word, &mut words);
                if words.is_empty() {
                    start_line = line;
                }
            }
            _ => {
                if words.is_empty() && !in_word {
                    start_line = line;
                }
                cur.push(c);
                in_word = true;
            }
        }
        i += 1;
    }
    end_word(&mut cur, &mut single, &mut in_word, &mut words);
    end_stmt(&mut words, &mut out, start_line);
    out
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A shell assignment word: `NAME=value`.
fn assignment(w: &Word) -> Option<(&str, &str)> {
    let (name, value) = w.text.split_once('=')?;
    is_name(name).then_some((name, value))
}

/// The variable a test checks for emptiness: `[[ -z "$X" ]]`, `[ -z $X ]`, `test -z "${X}"`.
fn empty_test(words: &[Word]) -> Option<String> {
    let w: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let at = w.iter().position(|x| *x == "-z")?;
    if !w[..at].iter().any(|x| matches!(*x, "[[" | "[" | "test" | "if" | "!")) {
        return None;
    }
    let v = w.get(at + 1)?.trim_start_matches('$').trim_start_matches('{').trim_end_matches('}');
    is_name(v).then(|| v.to_string())
}

/// Options of `ansible-playbook` that take a value.
const PLAYBOOK_OPTS: &[&str] = &[
    "-i", "--inventory", "--inventory-file", "-e", "--extra-vars", "-l", "--limit", "-u", "--user", "-t", "--tags",
    "--skip-tags", "--vault-password-file", "--vault-id", "-c", "--connection", "-f", "--forks", "-M", "--module-path",
    "--private-key", "--key-file", "-T", "--timeout", "--become-user", "--become-method", "--start-at-task",
    "--ssh-common-args", "--ssh-extra-args", "--scp-extra-args", "--sftp-extra-args",
];

/// An `ansible-playbook` command's playbook, inventory and extra vars (`K=V` each; a JSON object's entries; `@file`
/// as `@=file`).
fn playbook(args: &[Word]) -> Option<(String, String, Vec<String>)> {
    let mut book: Option<String> = None;
    let mut inventory = String::new();
    let mut vars = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].text.as_str();
        let (opt, inline) = match a.split_once('=') {
            Some((o, v)) if o.starts_with("--") => (o, Some(v.to_string())),
            _ => (a, None),
        };
        if PLAYBOOK_OPTS.contains(&opt) {
            let value = match inline {
                Some(v) => v,
                None => {
                    i += 1;
                    args.get(i).map(|w| w.text.clone()).unwrap_or_default()
                }
            };
            match opt {
                "-i" | "--inventory" | "--inventory-file" => inventory = value,
                "-e" | "--extra-vars" => vars.extend(extra_vars(&value)),
                _ => {}
            }
        } else if !a.starts_with('-') && book.as_ref().is_none_or(|b| !(b.ends_with(".yml") || b.ends_with(".yaml"))) {
            // The first positional naming a YAML file (a `$VERBOSITY` before it doesn't count).
            if book.is_none() || a.ends_with(".yml") || a.ends_with(".yaml") {
                book = Some(a.to_string());
            }
        }
        i += 1;
    }
    let book = book?;
    Some((book, inventory, vars))
}

fn extra_vars(value: &str) -> Vec<String> {
    let v = value.trim();
    if let Some(file) = v.strip_prefix('@') {
        return vec![format!("@={file}")];
    }
    if v.starts_with('{') {
        return match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(v) {
            Ok(m) => m
                .into_iter()
                .map(|(k, x)| match x {
                    serde_json::Value::String(s) => format!("{k}={s}"),
                    other => format!("{k}={other}"),
                })
                .collect(),
            Err(_) => Vec::new(),
        };
    }
    v.split_whitespace().filter(|p| p.split_once('=').is_some_and(|(k, _)| is_name(k))).map(|p| p.trim_matches(['"', '\'']).to_string()).collect()
}

/// The steps of one script item (see the module doc), each with its line offset in the item.
fn steps(text: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut guard: Option<String> = None;
    // Open `if`s: whether each is an emptiness guard (`if [[ -z "$X" ]]`) rather than a branch.
    let mut ifs: Vec<bool> = Vec::new();
    for (words, offset) in statements(text) {
        let first = words[0].text.as_str();
        if matches!(first, "fi" | "done" | "esac") {
            guard = None;
            if first == "fi" {
                ifs.pop();
            }
            continue;
        }
        let test = empty_test(&words);
        if first == "if" {
            ifs.push(test.is_some());
        }
        if let Some(v) = test {
            guard = Some(v);
            continue;
        }
        // Leading assignments: `A=1 B=2` alone sets them; before a command, they only set its environment.
        let assigns = words.iter().take_while(|w| assignment(w).is_some()).count();
        let (cmd, rest) = if words.first().is_some_and(|w| w.text == "export") { (words.get(1..), true) } else { (Some(&words[..]), false) };
        if rest || assigns == words.len() {
            for w in cmd.unwrap_or(&[]) {
                if let Some((name, value)) = assignment(w) {
                    let kind = match () {
                        _ if guard.as_deref() == Some(name) => "default",
                        // Set in one branch of an `if`: the value depends on which runs.
                        _ if ifs.iter().any(|g| !g) => "bset",
                        _ => "set",
                    };
                    out.push((format!("{kind}\t{name}\t{value}"), offset));
                    // `V=$(aws ec2 create-launch-template-version …)`: the command runs too.
                    for inner in substitutions(value) {
                        for (words, _) in statements(inner) {
                            if let Some(op) = deploy_op(&words) {
                                out.push((op, offset));
                            }
                        }
                    }
                }
            }
            continue;
        }
        let words = &words[assigns..];
        let cmd = words[0].text.as_str();
        let base = cmd.rsplit('/').next().unwrap_or(cmd);
        match base {
            "cd" | "pushd" => {
                if let Some(d) = words.get(1) {
                    out.push((format!("cd\t{}", d.text), offset));
                }
            }
            "popd" => out.push(("popd".into(), offset)),
            "git" if words.get(1).is_some_and(|w| w.text == "clone") => {
                // `-b branch` and `--depth n` take values: skip what follows them.
                let mut skip = false;
                let mut args = Vec::new();
                for w in &words[2..] {
                    if skip {
                        skip = false;
                        continue;
                    }
                    if matches!(w.text.as_str(), "-b" | "--branch" | "--depth" | "-o" | "--origin" | "-c" | "--config" | "--reference") {
                        skip = true;
                        continue;
                    }
                    if !w.text.starts_with('-') {
                        args.push(w.text.as_str());
                    }
                }
                if let Some(url) = args.first() {
                    out.push((format!("clone\t{url}\t{}", args.get(1).copied().unwrap_or("")), offset));
                }
            }
            "ansible-playbook" => {
                if let Some((book, inv, vars)) = playbook(&words[1..]) {
                    out.push((format!("playbook\t{book}\t{inv}\t{}", vars.join("\n")), offset));
                }
            }
            _ => {
                if let Some(op) = deploy_op(words) {
                    out.push((op, offset));
                }
            }
        }
    }
    out
}

/// The commands of the `$(…)` substitutions in a word, outermost only.
fn substitutions(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'$' && b[i + 1] == b'(' {
            let mut j = i + 2;
            let mut depth = 1;
            while j < b.len() && depth > 0 {
                match b[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            out.push(&s[i + 2..j.saturating_sub(1).max(i + 2)]);
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// The steps of a shell text, as a CI script's are read (see the module doc): for a package.json script's body.
pub fn shell_steps(text: &str) -> Vec<String> {
    steps(text).into_iter().map(|(op, _)| op).collect()
}

/// Subcommands of yarn and pnpm that aren't scripts: `yarn x` and `pnpm x` otherwise run the package script `x`.
const PM_BUILTINS: &[&str] = &[
    "install", "i", "add", "remove", "rm", "ci", "test", "t", "exec", "dlx", "x", "config", "audit", "outdated", "why",
    "list", "ls", "link", "unlink", "store", "init", "create", "publish", "pack", "version", "cache", "info", "run",
    "upgrade", "update", "up", "rebuild", "prune", "dedupe", "import", "licenses", "root", "bin", "setup", "env",
    "patch", "fetch", "deploy", "workspace", "workspaces", "global", "login", "logout", "set", "node", "plugin",
];

/// Options of the aws CLI before the service that take a value.
const AWS_GLOBAL_OPTS: &[&str] = &["--region", "--profile", "--output", "--endpoint-url", "--query", "--color", "--ca-bundle", "--cli-read-timeout", "--cli-connect-timeout"];

/// Options of kubectl and helm before the subcommand that take a value.
const KUBE_GLOBAL_OPTS: &[&str] = &["-n", "--namespace", "--context", "--kubeconfig", "--cluster", "--user", "-s", "--server", "--kube-context"];

/// A deploy command (`tool\tNAME\targs…`), or a package script run (`run\tPM\tSCRIPT\targs…`), with the wrappers
/// in front of it read through (`npx`, `pnpm exec`, `yarn <bin>`, `sudo`, `env A=1`). Tools and the subcommands
/// kept: `wrangler` deploy, publish, versions, pages; `aws` s3 sync/cp, cloudfront create-invalidation and
/// update-distribution, ecs update-service, lambda update-function-code; `sam deploy`; `cdk deploy`; `terraform`,
/// `tofu` and `gitlab-terraform` init, validate, plan, apply, destroy; `docker` build, buildx build, push;
/// `kubectl` apply, set, rollout, create, replace; `helm` upgrade, install; and releases: `npm`/`yarn`/`pnpm`
/// publish, `upack push`, `release-cli create`. Arguments are kept as written.
fn deploy_op(words: &[Word]) -> Option<String> {
    // Redirections (`2>`, `> file`) aren't arguments.
    let mut w: Vec<&str> = Vec::new();
    let mut target = false;
    for x in words.iter().map(|w| w.text.as_str()) {
        if std::mem::take(&mut target) {
            continue;
        }
        let r = x.trim_start_matches(|c: char| c.is_ascii_digit());
        if r.starts_with(['>', '<']) {
            // A bare operator (`>`, `2>>`) takes the next word as its target.
            target = r.trim_start_matches(['>', '<', '&']).is_empty();
            continue;
        }
        w.push(x);
    }
    let skip_options = |w: &mut Vec<&str>| {
        while w.first().is_some_and(|x| x.starts_with('-')) {
            let dashdash = w[0] == "--";
            w.remove(0);
            if dashdash {
                break;
            }
        }
    };
    loop {
        let base = w.first()?.rsplit('/').next().unwrap_or("");
        match base {
            "sudo" | "time" | "exec" | "command" | "nohup" => {
                w.remove(0);
                skip_options(&mut w);
            }
            "env" => {
                w.remove(0);
                while w.first().is_some_and(|x| x.contains('=') || x.starts_with('-')) {
                    w.remove(0);
                }
            }
            "npx" | "bunx" => {
                w.remove(0);
                skip_options(&mut w);
            }
            "npm" | "pnpm" | "yarn" | "bun" if matches!(w.get(1).copied(), Some("exec" | "dlx" | "x")) => {
                w.drain(..2);
                skip_options(&mut w);
            }
            // `yarn wrangler deploy`: yarn running a package's binary.
            "yarn" | "pnpm" if w.get(1).is_some_and(|x| TOOLS.contains(x)) => {
                w.remove(0);
            }
            _ => break,
        }
    }
    // `wrangler@4`, `@cloudflare/wrangler`: the package's name.
    let tool = w.first()?.rsplit('/').next().unwrap_or("").split('@').next().unwrap_or("");
    let args = &w[1..];
    let positionals = |opts: &[&str]| -> Vec<&str> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            if a.starts_with('-') {
                if opts.contains(&a) {
                    i += 1;
                }
            } else {
                out.push(a);
            }
            i += 1;
        }
        out
    };
    let keep = match tool {
        "wrangler" => matches!(positionals(&["--env", "-e", "--config", "-c", "--name"]).first().copied(), Some("deploy" | "publish" | "versions" | "pages")),
        "aws" => {
            let p = positionals(AWS_GLOBAL_OPTS);
            matches!(
                (p.first().copied(), p.get(1).copied()),
                (Some("s3"), Some("sync" | "cp"))
                    | (Some("cloudfront"), Some("create-invalidation" | "update-distribution"))
                    | (Some("ecs"), Some("update-service"))
                    | (Some("lambda"), Some("update-function-code"))
                    | (Some("ec2"), Some("create-launch-template-version" | "modify-launch-template"))
                    | (Some("autoscaling"), Some("start-instance-refresh" | "set-desired-capacity" | "update-auto-scaling-group"))
                    | (Some("ssm"), Some("send-command"))
                    | (Some("cloudformation"), Some("deploy"))
                    | (Some("elasticbeanstalk"), Some("update-environment" | "create-application-version"))
                    | (Some("codeartifact"), Some("publish-package-version"))
            )
        }
        "sam" | "cdk" => positionals(&[]).first() == Some(&"deploy"),
        "terraform" | "tofu" | "gitlab-terraform" | "gitlab-tofu" => matches!(positionals(&[]).first().copied(), Some("init" | "validate" | "plan" | "apply" | "destroy")),
        "docker" => matches!(positionals(&["-H", "--host", "--context", "-c", "--config", "-l", "--log-level"]).as_slice(), ["build" | "push", ..] | ["buildx", "build", ..] | ["image", "push", ..]),
        "kubectl" => matches!(positionals(KUBE_GLOBAL_OPTS).first().copied(), Some("apply" | "set" | "rollout" | "create" | "replace")),
        "helm" => matches!(positionals(KUBE_GLOBAL_OPTS).first().copied(), Some("upgrade" | "install")),
        // A release: a package published (`npm publish`, `upack push`) or a GitLab release made.
        "npm" | "yarn" | "pnpm" if args.first() == Some(&"publish") => true,
        "upack" => args.first() == Some(&"push"),
        "release-cli" => args.first() == Some(&"create"),
        "npm" | "yarn" | "pnpm" => {
            let script = match args {
                ["run" | "run-script", rest @ ..] => rest.iter().find(|a| !a.starts_with('-')).copied(),
                // pnpm's `deploy` is its own command; yarn has none.
                [s, ..] if tool != "npm" && !s.starts_with('-') && (!PM_BUILTINS.contains(s) || (tool == "yarn" && *s == "deploy")) => Some(*s),
                _ => None,
            };
            let script = script?;
            let rest: Vec<&str> = args.iter().skip_while(|a| **a != script).skip(1).copied().collect();
            let mut op = format!("run\t{tool}\t{}", clean(script));
            for a in rest {
                op.push('\t');
                op.push_str(&clean(a));
            }
            return Some(op);
        }
        _ => false,
    };
    if !keep {
        return None;
    }
    let mut op = format!("tool\t{tool}");
    for a in args {
        op.push('\t');
        op.push_str(&clean(a));
    }
    Some(op)
}

/// The deploy tools [`deploy_op`] reads.
const TOOLS: &[&str] = &["wrangler", "aws", "sam", "cdk", "terraform", "tofu", "gitlab-terraform", "gitlab-tofu", "docker", "kubectl", "helm", "upack", "release-cli"];

/// A word as one tab-separated field.
fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Facts of a GitLab CI file.
pub fn extract(_path: &str, src: &str) -> FileFacts {
    let mut line_starts = vec![0];
    line_starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
    let mut ex = Ex { src, line_starts, f: FileFacts::default() };
    // A byte order mark would read as part of the first key; the text keeps it, so offsets stay the file's.
    let body = src.strip_prefix('\u{feff}').unwrap_or(src);
    let (docs, failed) = parse(body);
    ex.f.parse_errors = failed as u32;
    let mut maps = docs.iter().filter(|d| matches!(d.v, Val::Map(_)));
    let Some(mut doc) = maps.next() else { return ex.f };
    // A component or a template with inputs: a `spec:` header document, then the configuration. Each input's
    // default is a `ci_input` fact (JSON), for interpolating `$[[ inputs.x ]]` where the file is included.
    if doc.map().len() == 1
        && let Some(spec) = doc.get("spec")
        && let Some(next) = maps.next()
    {
        for (name, line, def) in spec.get("inputs").map(Node::map).unwrap_or(&[]) {
            let default = def.get("default").map_or(serde_json::Value::Null, to_json);
            ex.fact("ci_input", None, name, &default.to_string(), *line);
        }
        doc = next;
    }
    for (k, line, v) in doc.map() {
        match k.as_str() {
            "include" => ex.includes("ci_include", None, v),
            "variables" => ex.variables(None, v),
            "before_script" | "after_script" | "script" => ex.script(None, k, *line, v),
            "default" => {
                for (dk, dline, dv) in v.map() {
                    if matches!(dk.as_str(), "before_script" | "after_script" | "script") {
                        ex.script(None, dk, *dline, dv);
                    }
                }
            }
            k if RESERVED.contains(&k) => {}
            _ if matches!(v.v, Val::Map(_)) => {
                // `<<: *anchor` merges the anchored map's keys under the job's own.
                let merged = merge_keys(v);
                ex.job(k, *line, &merged);
            }
            _ => {}
        }
    }
    ex.f
}

/// A YAML node as JSON: scalars as strings (YAML's types aren't kept), maps and lists as such.
fn to_json(n: &Node) -> serde_json::Value {
    match &n.v {
        Val::Str(s) => serde_json::Value::String(s.clone()),
        Val::Map(m) => serde_json::Value::Object(m.iter().map(|(k, _, v)| (k.clone(), to_json(v))).collect()),
        Val::Seq(items) => serde_json::Value::Array(items.iter().map(to_json).collect()),
        Val::Null => serde_json::Value::Null,
    }
}

/// A map with its `<<` merge keys read: the merged maps' entries first, the map's own after (so they win).
fn merge_keys(v: &Node) -> Node {
    let Some(m) = v.get("<<") else { return v.clone() };
    let mut items: Vec<(String, u32, Node)> = Vec::new();
    let sources: Vec<&Node> = match &m.v {
        Val::Seq(s) => s.iter().collect(),
        _ => vec![m],
    };
    for s in sources {
        items.extend(s.map().iter().cloned());
    }
    items.extend(v.map().iter().filter(|(k, ..)| k != "<<").cloned());
    Node { v: Val::Map(items), line: v.line, end: v.end }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(f: &FileFacts) -> Vec<(&'static str, Option<String>, String, String, u32)> {
        f.ansible.iter().map(|a| (a.kind, a.sym.map(|i| f.symbols[i].name.clone()), a.name.clone(), a.value.clone(), a.line)).collect()
    }

    #[test]
    fn paths_and_sniffing() {
        assert!(is_ci_path(".gitlab-ci.yml"));
        assert!(is_ci_path("ci/.gitlab-ci-library.yml"));
        assert!(is_ci_path("ansible/.Deploy.gitlab-ci.yml"));
        assert!(!is_ci_path("deploy.yml"));
        assert!(candidate("src/App/ci.yml"));
        assert!(candidate(".gitlab/ci/build.yml"));
        assert!(!candidate(".github/workflows/x.yml"));
        assert!(!candidate("roles/web/tasks/main.yml"));
        assert!(!candidate("docker-compose.yml"));
        assert!(sniff("include:\n  - project: 'infra/ci'\n    file: '/build.yml'\n"));
        assert!(sniff(".app_vars:\n  variables:\n    APP_DIR: src/App\n"));
        assert!(sniff("variables:\n  A: 1\n\ndeploy:\n  extends: .deploy\n  trigger:\n    include: x.yml\n"));
        assert!(!sniff("- hosts: all\n  roles: [x]\n"));
        assert!(!sniff("version: '3'\nservices:\n  web:\n    image: x\n"));
        assert!(!sniff("Logging:\n  LogLevel:\n    Default: Information\n"));
        assert!(!sniff("openapi: 3.0.0\npaths:\n  /x:\n    get:\n      summary: y\n"));
        assert!(sniff("\u{feff}include:\n  - local: '/a.yml'\n"));
        assert!(sniff("spec:\n  inputs:\n    stage:\n      default: test\n---\n\"$[[ inputs.as ]]\":\n  rules:\n    - when: always\n"));
        assert!(!sniff("apiVersion: v1\nspec:\n  containers: []\n"));
        assert_eq!(extract("x.yml", "\u{feff}include:\n  - local: '/a.yml'\n").ansible.len(), 1);
    }

    #[test]
    fn shell_steps() {
        let s = steps("[[ -z \"$PACKAGE_NAME\" ]] && PACKAGE_NAME=\"$(basename *.csproj .csproj)\"");
        assert_eq!(s, [("default\tPACKAGE_NAME\t$(basename *.csproj .csproj)".to_string(), 0)]);
        let s = steps("if [[ -z \"$P\" ]]; then\n  pushd \"$APP_DIR\"\n  P=\"$(basename *.csproj .csproj)\"\n  popd\nfi\nQ=${P#Acme.}\n");
        let ops: Vec<&str> = s.iter().map(|(o, _)| o.as_str()).collect();
        assert_eq!(ops, ["cd\t$APP_DIR", "default\tP\t$(basename *.csproj .csproj)", "popd", "set\tQ\t${P#Acme.}"]);
        let s = steps("D=dist\nif [ -d \"dist/$N\" ]; then\n  D=\"dist/$N\"\nelif [ -d x ]; then\n  D=x\nfi\nU=$D");
        let ops: Vec<&str> = s.iter().map(|(o, _)| o.as_str()).collect();
        assert_eq!(ops, ["set\tD\tdist", "bset\tD\tdist/$N", "bset\tD\tx", "set\tU\t$D"]);
        assert_eq!(s[1].1, 2);
        let s = steps("git clone -b $BRANCH --depth 1 https://git.example.com/ops/Deploy.App.git\nexport X=1");
        let ops: Vec<&str> = s.iter().map(|(o, _)| o.as_str()).collect();
        assert_eq!(ops, ["clone\thttps://git.example.com/ops/Deploy.App.git\t", "set\tX\t1"]);
        let s = steps(
            "ansible-playbook ./Deploy.App/deploy.yml -i $INVENTORY_FILE --vault-password-file \"$KEY\" --extra-vars \"TARGET=servers APP_NAME=$PACKAGE_NAME PORT=${PORT}\" -e '{\"svc\": \"x\"}'",
        );
        assert_eq!(s[0].0, "playbook\t./Deploy.App/deploy.yml\t$INVENTORY_FILE\tTARGET=servers\nAPP_NAME=$PACKAGE_NAME\nPORT=${PORT}\nsvc=x");
        let s = steps("ansible-playbook ${VERBOSITY} ./main.yml \\\n  -i ./inventory.yml");
        assert_eq!(s[0].0, "playbook\t./main.yml\t./inventory.yml\t");
        // A command's environment isn't a variable of the script.
        assert!(steps("FOO=1 make build").is_empty());
    }

    #[test]
    fn deploy_commands() {
        let ops = |s: &str| super::shell_steps(s);
        assert_eq!(ops("pnpm exec wrangler deploy --env staging 2>&1 | tee x.log"), ["tool\twrangler\tdeploy\t--env\tstaging"]);
        assert_eq!(ops("npx --yes wrangler@4 publish > out.txt"), ["tool\twrangler\tpublish"]);
        assert_eq!(ops("wrangler dev"), Vec::<String>::new());
        assert_eq!(
            ops("for B in $BUCKETS; do\n  aws s3 cp --recursive ${UPLOAD_PATH} \"s3://${B}/${V}\";\ndone"),
            ["tool\taws\ts3\tcp\t--recursive\t${UPLOAD_PATH}\ts3://${B}/${V}"]
        );
        assert_eq!(ops("aws s3api list-buckets --query x"), Vec::<String>::new());
        assert_eq!(ops("/usr/local/bin/aws --region us-east-1 ecs update-service --cluster k --service s"), ["tool\taws\t--region\tus-east-1\tecs\tupdate-service\t--cluster\tk\t--service\ts"]);
        assert_eq!(ops("aws cloudfront create-invalidation --distribution-id $CF --paths '/*'"), ["tool\taws\tcloudfront\tcreate-invalidation\t--distribution-id\t$CF\t--paths\t/*"]);
        assert_eq!(ops("terraform -chdir=infra apply -auto-approve"), ["tool\tterraform\t-chdir=infra\tapply\t-auto-approve"]);
        assert_eq!(ops("gitlab-terraform plan-json"), Vec::<String>::new());
        assert_eq!(ops("terraform workspace select prod"), Vec::<String>::new());
        assert_eq!(ops("docker build -f \"$DOCKERFILE\" -t \"$IMG\" ."), ["tool\tdocker\tbuild\t-f\t$DOCKERFILE\t-t\t$IMG\t."]);
        assert_eq!(ops("docker login -u x"), Vec::<String>::new());
        assert_eq!(ops("sudo kubectl -n web apply -f k8s/"), ["tool\tkubectl\t-n\tweb\tapply\t-f\tk8s/"]);
        assert_eq!(ops("helm upgrade --install api ./chart"), ["tool\thelm\tupgrade\t--install\tapi\t./chart"]);
        assert_eq!(ops("sam deploy --stack-name s"), ["tool\tsam\tdeploy\t--stack-name\ts"]);
        assert_eq!(ops("npm run deploy -- --env prod"), ["run\tnpm\tdeploy\t--\t--env\tprod"]);
        assert_eq!(ops("yarn deploy"), ["run\tyarn\tdeploy"]);
        assert_eq!(ops("pnpm deploy out"), Vec::<String>::new(), "pnpm's own deploy command");
        assert_eq!(ops("pnpm install --frozen-lockfile"), Vec::<String>::new());
        assert_eq!(ops("yarn wrangler deploy"), ["tool\twrangler\tdeploy"]);
        assert_eq!(
            ops("NEW_V=$(aws ec2 create-launch-template-version --launch-template-id $LT --source-version 1 | jq '.V')"),
            ["set\tNEW_V\t$(aws ec2 create-launch-template-version --launch-template-id $LT --source-version 1 | jq '.V')", "tool\taws\tec2\tcreate-launch-template-version\t--launch-template-id\t$LT\t--source-version\t1"]
        );
        assert_eq!(ops("aws autoscaling start-instance-refresh --auto-scaling-group-name \"$ASG\""), ["tool\taws\tautoscaling\tstart-instance-refresh\t--auto-scaling-group-name\t$ASG"]);
        assert_eq!(ops("aws ssm send-command --document-name AWS-RunShellScript --targets Key=tag:App,Values=web"), ["tool\taws\tssm\tsend-command\t--document-name\tAWS-RunShellScript\t--targets\tKey=tag:App,Values=web"]);
        assert_eq!(ops("aws ec2 describe-launch-templates --launch-template-ids $LT"), Vec::<String>::new());
        assert_eq!(ops("gitlab-tofu apply"), ["tool\tgitlab-tofu\tapply"]);
        assert_eq!(ops("npm publish --tag \"${T:-next}\" \"${F}\""), ["tool\tnpm\tpublish\t--tag\t${T:-next}\t${F}"]);
        assert_eq!(ops("./upack push p.upack https://feed/ --user=x"), ["tool\tupack\tpush\tp.upack\thttps://feed/\t--user=x"]);
    }

    #[test]
    fn components_and_inputs() {
        let f = extract(
            "templates/apply.yml",
            "spec:\n  inputs:\n    as:\n      default: apply\n    root_dir:\n      default: '.'\n    var_files:\n      default: []\n---\n'$[[ inputs.as ]]':\n  variables:\n    ROOT: $[[ inputs.root_dir ]]\n  script:\n    - gitlab-tofu apply\n",
        );
        let fs = facts(&f);
        assert!(fs.contains(&("ci_input", None, "as".into(), "\"apply\"".into(), 3)));
        assert!(fs.contains(&("ci_input", None, "var_files".into(), "[]".into(), 7)));
        assert_eq!(f.symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["$[[ inputs.as ]]"]);
        let f = extract(".gitlab-ci.yml", "include:\n  - component: $CI_SERVER_FQDN/components/opentofu/apply@1.0\n    inputs:\n      as: stg_apply\n      var_files: [a.tfvars]\n");
        assert_eq!(f.ansible[0].value, r#"{"file":"$CI_SERVER_FQDN/components/opentofu/apply@1.0","inputs":{"as":"stg_apply","var_files":["a.tfvars"]}}"#);
    }

    const CI: &str = "include:
  - project: 'Infra/pipeline-templates'
    ref: master
    file:
      - '/dotnet/build.yml'
  - local: '**/ci.yml'
    rules:
      - if: $CI_PIPELINE_SOURCE == \"web\"
        when: never
variables:
  GIT_DEPTH: 1
.vars: &vars
  variables:
    APP_DIR: src/App
    PORT: 5080
deploy_app:
  <<: *vars
  extends:
    - .deploy
  stage: deploy
  trigger:
    include: \"${APP_DIR}/deploy.yml\"
    strategy: depend
.deploy_vm:
  before_script:
    - !reference [.setup, before_script]
    - >
      [[ -z \"$PACKAGE_NAME\" ]] && PACKAGE_NAME=\"$(basename *.csproj .csproj)\"
    - |
      git clone --depth 1 http://git.example.com/ops/Deploy.App.git
  script:
    - ansible-playbook ./Deploy.App/deploy.yml -i $INV --extra-vars \"APP=$PACKAGE_NAME\"
";

    #[test]
    fn jobs_includes_variables_and_scripts() {
        let f = extract(".gitlab-ci.yml", CI);
        assert_eq!(f.parse_errors, 0);
        let syms: Vec<(&str, &str, Vec<&str>, u32)> = f.symbols.iter().map(|s| (s.name.as_str(), s.kind.as_str(), s.tags.clone(), s.start_line)).collect();
        assert_eq!(syms, [(".vars", "job", vec!["hidden"], 12), ("deploy_app", "job", vec![], 16), (".deploy_vm", "job", vec!["hidden"], 24)]);
        assert_eq!(f.symbols[1].signature, "job deploy_app (stage deploy): extends .deploy");
        let fs = facts(&f);
        let n = |s: &str| Some(s.to_string());
        assert!(fs.contains(&("ci_include", None, "project".into(), r#"{"file":"/dotnet/build.yml","project":"Infra/pipeline-templates","ref":"master"}"#.into(), 5)));
        assert!(fs.contains(&("ci_include", None, "local".into(), r#"{"file":"**/ci.yml","rules":["never:$CI_PIPELINE_SOURCE == \"web\""]}"#.into(), 6)));
        assert!(fs.contains(&("ci_var", None, "GIT_DEPTH".into(), "1".into(), 11)));
        assert!(fs.contains(&("ci_var", n("deploy_app"), "PORT".into(), "5080".into(), 15)));
        assert!(fs.contains(&("ci_extends", n("deploy_app"), ".deploy".into(), String::new(), 19)));
        assert!(fs.contains(&("ci_trigger", n("deploy_app"), "local".into(), r#"{"file":"${APP_DIR}/deploy.yml"}"#.into(), 22)));
        let script: Vec<(String, String, u32)> =
            fs.iter().filter(|x| x.0 == "ci_script").map(|x| (x.2.clone(), x.3.clone(), x.4)).collect();
        assert_eq!(
            script,
            [
                ("before_script".into(), String::new(), 25),
                ("before_script".into(), "reference\t.setup\tbefore_script".into(), 26),
                ("before_script".into(), "default\tPACKAGE_NAME\t$(basename *.csproj .csproj)".into(), 28),
                ("before_script".into(), "clone\thttp://git.example.com/ops/Deploy.App.git\t".into(), 30),
                ("script".into(), String::new(), 31),
                ("script".into(), "playbook\t./Deploy.App/deploy.yml\t$INV\tAPP=$PACKAGE_NAME".into(), 32),
            ]
        );
    }
}
