//! Ansible: playbooks, roles, task and handler files, variable files, `requirements.yml` and Jinja templates.
//!
//! Files are recognised by where they sit and what they hold, never by being YAML: a file under a role's `tasks/`,
//! `handlers/`, `defaults/`, `vars/` or `meta/` (in `roles/<name>/`, or at the repo root of a role repo), under
//! `group_vars/` or `host_vars/`, or under another `tasks/`, `handlers/`, `vars/` or `defaults/` directory; a
//! `requirements.yml`; a `.j2` or `.jn2` template; and any other YAML file whose top level is a list of plays
//! (`hosts:` or `import_playbook:`), which [`sniff`] checks. GitLab CI files, compose files and dotted directories
//! (`.github/`) are never read.
//!
//! YAML is read with yaml-rust2's event parser (see [`crate::yaml`]); the events give every node's line. A syntax
//! error keeps what was read before it. Symbols:
//!
//! - `role` (in a role's `tasks/main.yml`, and in its `meta/main.yml` tagged `meta`), named by its directory under
//!   `roles/`, or [`ROOT_ROLE`] for a repo that is one role (the indexer names it after the repo);
//! - `playbook` (the file, named by its file name), `play` (named by `name:`, else `hosts: …`);
//! - `task` (named tasks and blocks), `handler`, and `tasks` for a task file that isn't a role's main one;
//! - `variable`: top-level keys of defaults, vars, group_vars and host_vars files and of a play's `vars:`, `set_fact`
//!   keys and `register` names, each tagged with where it comes from (Ansible's precedence is decided by that);
//! - `template`: a Jinja template file.
//!
//! References: `role` (a play's `roles:`, `include_role`/`import_role` with its `tasks_from` as the receiver, a meta
//! dependency), `include` (named by the
//! included file's name, the path as written in `receiver`: `include_tasks`, `import_tasks`, `include`,
//! `import_playbook`), `notify` (a handler name or `listen` topic), `template` (a `template` task's `src`, by file
//! name, the path as written in `receiver`) and `var` (each variable a Jinja expression, a `when:` condition or a
//! template uses, magic variables and loop variables left out). Facts ([`AnsibleFact`]) record requirements, vars
//! files a play loads, a role's `meta` name, and what a task deploys or configures: services (systemd units among
//! them), images, repos, RabbitMQ vhosts, queues and exchanges, databases, URLs, directories it creates or unpacks
//! into (`path`) and nginx sites, as written. The fact's kind is what the task does to it ([`TASK_ACTIONS`]), read
//! from the module's `state` (or a `uri` call's method), else from the verb opening the task's name: `deploys`,
//! `undeploys` (`state: absent`, "Deregister service in consul"), `restarts` or `stops`.

use crate::yaml::{Node, Val, parse};
use crate::{AnsibleFact, FileFacts, Ref, RefKind, Symbol, SymbolKind, Unsupported};

pub const VERSION: u32 = 3;

/// The kinds of fact a task acting on something deployable records, by what it does: puts it in place (or starts
/// it), takes it away (`state: absent`, deregistering, uninstalling), restarts or reloads it, or stops it.
pub const TASK_ACTIONS: [&str; 4] = ["deploys", "undeploys", "restarts", "stops"];

/// The name a repo-root role's symbols carry until the indexer replaces it with the repo's directory name.
pub const ROOT_ROLE: &str = "<root role>";

const SIGNATURE_CAP: usize = 160;

/// What an Ansible file is, from its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Tasks,
    Handlers,
    Defaults,
    Vars,
    Meta,
    GroupVars,
    HostVars,
    Requirements,
    Template,
    /// A YAML file whose path says nothing: a playbook (or a task list) by its content.
    Other,
}

/// Where an Ansible file sits: its kind, and the role it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub kind: FileKind,
    /// The role's name: its directory under `roles/`, or [`ROOT_ROLE`].
    pub role: Option<String>,
    /// The role's directory (`roles/x`, or `""` for a repo-root role).
    pub role_dir: Option<String>,
}

fn is_yaml(path: &str) -> bool {
    path.ends_with(".yml") || path.ends_with(".yaml")
}

/// A template Ansible renders: `.j2`, or `.jn2` as some repos name them.
pub fn is_template_path(path: &str) -> bool {
    path.ends_with(".j2") || path.ends_with(".jn2")
}

/// YAML that is never Ansible: CI pipelines, compose files, anything in a dotted directory.
fn excluded(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.starts_with(".gitlab-ci")
        || name.starts_with("docker-compose")
        || name.starts_with("compose.")
        || matches!(name, ".travis.yml" | "azure-pipelines.yml" | "bitbucket-pipelines.yml" | "mkdocs.yml" | "pnpm-lock.yaml")
        || path.split('/').any(|c| c.starts_with('.') && c.len() > 1)
}

const ROLE_DIRS: &[&str] = &["tasks", "handlers", "defaults", "vars", "meta"];

fn dir_kind(dir: &str) -> FileKind {
    match dir {
        "tasks" => FileKind::Tasks,
        "handlers" => FileKind::Handlers,
        "defaults" => FileKind::Defaults,
        "vars" => FileKind::Vars,
        _ => FileKind::Meta,
    }
}

/// The Ansible place of a repo-relative path known from the path alone; `None` for other paths, including YAML
/// files that may be playbooks (see [`candidate`]).
pub fn place(path: &str) -> Option<Place> {
    if excluded(path) {
        return None;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let n = parts.len();
    let template = is_template_path(path);
    if !template && !is_yaml(path) {
        return None;
    }
    // `roles/<name>/<dir>/…`, or a role's templates.
    for i in 0..n.saturating_sub(3) {
        if parts[i] == "roles" && (ROLE_DIRS.contains(&parts[i + 2]) || (template && parts[i + 2] == "templates")) {
            let kind = if template { FileKind::Template } else { dir_kind(parts[i + 2]) };
            return Some(Place { kind, role: Some(parts[i + 1].to_string()), role_dir: Some(parts[..i + 2].join("/")) });
        }
    }
    if template {
        let root_role = n >= 2 && parts[0] == "templates";
        return Some(Place {
            kind: FileKind::Template,
            role: root_role.then(|| ROOT_ROLE.to_string()),
            role_dir: root_role.then(String::new),
        });
    }
    if n >= 2 && ROLE_DIRS.contains(&parts[0]) {
        return Some(Place { kind: dir_kind(parts[0]), role: Some(ROOT_ROLE.to_string()), role_dir: Some(String::new()) });
    }
    if let Some(i) = parts[..n - 1].iter().position(|c| *c == "group_vars" || *c == "host_vars") {
        let kind = if parts[i] == "group_vars" { FileKind::GroupVars } else { FileKind::HostVars };
        return Some(Place { kind, role: None, role_dir: None });
    }
    if n >= 2 && matches!(parts[n - 2], "tasks" | "handlers" | "vars" | "defaults") {
        return Some(Place { kind: dir_kind(parts[n - 2]), role: None, role_dir: None });
    }
    if matches!(parts[n - 1], "requirements.yml" | "requirements.yaml") {
        return Some(Place { kind: FileKind::Requirements, role: None, role_dir: None });
    }
    None
}

/// A YAML file whose path doesn't place it but which may be a playbook: read it and ask [`sniff`].
pub fn candidate(path: &str) -> bool {
    is_yaml(path) && !excluded(path) && place(path).is_none()
}

/// Whether YAML text is a playbook: a top-level list whose first items have `hosts:` or `import_playbook:` among
/// their keys. Reads lines only; GitLab CI and compose files (top-level maps) never match.
pub fn sniff(text: &str) -> bool {
    let mut items = 0;
    let mut in_item = false;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" {
            continue;
        }
        let key_of = |s: &str| -> Option<String> {
            let s = s.trim_start();
            let (k, _) = s.split_once(':')?;
            let k = k.trim().trim_matches(['"', '\'']);
            (!k.is_empty() && !k.contains(' ')).then(|| k.to_string())
        };
        let play_key = |k: &str| matches!(k, "hosts" | "import_playbook" | "ansible.builtin.import_playbook");
        if let Some(rest) = trimmed.strip_prefix("- ") {
            items += 1;
            if items > 5 {
                return false;
            }
            in_item = true;
            if key_of(rest).is_some_and(|k| play_key(&k)) {
                return true;
            }
        } else if trimmed == "-" {
            items += 1;
            in_item = true;
        } else if !line.starts_with(' ') {
            // A top-level map key, or text that isn't a list: not a playbook.
            return false;
        } else if in_item && line.starts_with("  ") && !line.starts_with("   ") && key_of(line).is_some_and(|k| play_key(&k)) {
            return true;
        }
    }
    false
}


// ---------------------------------------------------------------------------------------------------------------
// Jinja.

/// Names that are never a variable a play defines: Ansible's magic variables, Jinja's literals and globals.
const MAGIC: &[&str] = &[
    "item", "hostvars", "groups", "group_names", "inventory_hostname", "inventory_hostname_short", "inventory_dir",
    "inventory_file", "play_hosts", "playbook_dir", "role_path", "role_name", "role_names", "omit", "environment",
    "lookup", "query", "q", "range", "dict", "lipsum", "now", "undef", "loop", "vars", "true", "false", "none", "True",
    "False", "None", "and", "or", "not", "in", "is", "if", "else", "elif", "endif", "for", "endfor", "set", "recursive",
    "defined", "undefined", "self", "super", "varargs", "kwargs", "caller", "namespace", "cycler", "joiner",
    "ansible_check_mode", "ansible_play_batch", "ansible_loop", "ansible_index_var", "zip", "zip_longest",
];

fn magic(name: &str) -> bool {
    name.starts_with("ansible_") || MAGIC.contains(&name)
}

/// The variables a Jinja expression (the inside of `{{ }}`, or a bare `when:` condition) uses: identifiers that are
/// not attributes (`.x`), filters (`| x`), tests (`is x`), calls (`x(`), keyword arguments (`x=`), string contents
/// or magic names.
pub fn expr_vars(expr: &str) -> Vec<&str> {
    let b = expr.as_bytes();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    let mut prev: u8 = b' ';
    // After `is` (and `is not`), the next word is a test.
    let mut after_is = false;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' || c == b'"' {
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            prev = b'"';
            after_is = false;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &expr[start..i];
            let next = b[i..].iter().find(|c| !c.is_ascii_whitespace()).copied();
            let attribute = prev == b'.';
            let filter = prev == b'|';
            let test = after_is && word != "not";
            let call = next == Some(b'(');
            let kwarg = next == Some(b'=') && b.get(i + 1..).and_then(|r| r.iter().find(|c| !c.is_ascii_whitespace())) != Some(&b'=');
            if !(attribute || filter || test || call || kwarg || magic(word)) && !out.contains(&word) {
                out.push(word);
            }
            prev = b'a';
            after_is = word == "is" || (after_is && word == "not");
            continue;
        }
        if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                i += 1;
            }
            prev = b'0';
            after_is = false;
            continue;
        }
        if !c.is_ascii_whitespace() {
            prev = c;
            after_is = false;
        }
        i += 1;
    }
    out
}

/// The variables a text with Jinja uses (`{{ x }}`, `{% if x %}`, `{% for a in b %}`), with each one's line offset
/// within the text. Names a `{% for %}` or `{% set %}` binds are left out; `{# #}` comments and `{% raw %}` blocks are
/// skipped.
pub fn template_vars(text: &str) -> Vec<(&str, u32)> {
    let mut out: Vec<(&str, u32)> = Vec::new();
    let mut locals: Vec<&str> = Vec::new();
    let mut i = 0;
    let line_at = |at: usize| text[..at].matches('\n').count() as u32;
    while let Some(rel) = text[i..].find(['{']) {
        let at = i + rel;
        let rest = &text[at..];
        let (close, statement) = if rest.starts_with("{{") {
            ("}}", false)
        } else if rest.starts_with("{%") {
            ("%}", true)
        } else if rest.starts_with("{#") {
            i = at + rest.find("#}").map_or(rest.len(), |e| e + 2);
            continue;
        } else {
            i = at + 1;
            continue;
        };
        let Some(end) = rest[2..].find(close) else { break };
        let inner = rest[2..2 + end].trim_matches(['-', '+', ' ', '\t', '\n', '\r']);
        let line = line_at(at);
        i = at + 2 + end + 2;
        let mut vars = if statement {
            let (word, body) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
            match word {
                "raw" => {
                    i = text[i..].find("endraw").map_or(text.len(), |e| i + e);
                    continue;
                }
                "for" => match body.split_once(" in ") {
                    Some((names, iter)) => {
                        locals.extend(names.split(',').map(str::trim).filter(|n| !n.is_empty()));
                        expr_vars(iter)
                    }
                    None => Vec::new(),
                },
                "set" => match body.split_once('=') {
                    Some((name, value)) => {
                        locals.push(name.trim());
                        expr_vars(value)
                    }
                    None => Vec::new(),
                },
                "if" | "elif" => expr_vars(body),
                _ => Vec::new(),
            }
        } else {
            expr_vars(inner)
        };
        vars.retain(|v| !locals.contains(v));
        for v in vars {
            if !out.iter().any(|(o, l)| *o == v && *l == line) {
                out.push((v, line));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Extraction.

/// Task keywords: any other key of a task is its module.
const TASK_KEYWORDS: &[&str] = &[
    "name", "action", "args", "async", "become", "become_exe", "become_flags", "become_method", "become_user",
    "changed_when", "check_mode", "collections", "connection", "debugger", "delay", "delegate_facts", "delegate_to",
    "diff", "environment", "failed_when", "ignore_errors", "ignore_unreachable", "local_action", "loop",
    "loop_control", "module_defaults", "no_log", "notify", "poll", "port", "register", "remote_user", "retries",
    "run_once", "tags", "throttle", "timeout", "until", "vars", "when", "block", "rescue", "always", "listen",
    "any_errors_fatal", "vars_files",
];

/// Keys holding a bare Jinja condition (no braces).
const CONDITION_KEYS: &[&str] = &["when", "changed_when", "failed_when", "until"];

/// The module's short name: `ansible.builtin.template` → `template`.
fn short_module(m: &str) -> &str {
    if m.matches('.').count() >= 2 { m.rsplit('.').next().unwrap_or(m) } else { m }
}

fn module_of(task: &Node) -> Option<(&str, &Node)> {
    for (k, _, v) in task.map() {
        if TASK_KEYWORDS.contains(&k.as_str()) || k.starts_with("with_") {
            continue;
        }
        return Some((k.as_str(), v));
    }
    // `action: module args` / `local_action: module args`.
    let a = task.get("action").or_else(|| task.get("local_action"))?;
    match &a.v {
        Val::Str(s) => Some((s.split_whitespace().next()?, a)),
        Val::Map(_) => Some((a.get("module")?.str()?, a)),
        _ => None,
    }
}

/// A module argument: from a map (or `args:`), or from a free-form `k=v` string.
fn arg(task: &Node, module: &Node, key: &str) -> Option<(String, u32)> {
    if let Some(n) = module.get(key).or_else(|| task.get("args").and_then(|a| a.get(key)))
        && let Some(s) = n.str()
    {
        return Some((s.to_string(), n.line));
    }
    let s = module.str()?;
    let mut rest = s;
    while let Some(at) = rest.find(&format!("{key}=")) {
        let before_ok = at == 0 || rest.as_bytes()[at - 1].is_ascii_whitespace();
        let after = &rest[at + key.len() + 1..];
        if before_ok {
            let value = if let Some(q) = after.strip_prefix('"') {
                q.split('"').next().unwrap_or("")
            } else if let Some(q) = after.strip_prefix('\'') {
                q.split('\'').next().unwrap_or("")
            } else {
                after.split_whitespace().next().unwrap_or("")
            };
            return Some((value.to_string(), module.line));
        }
        rest = after;
    }
    None
}

/// The first word of a free-form module string (`include_tasks: foo.yml`), or the `file`/`name` argument.
fn free_or(task: &Node, module: &Node, key: &str) -> Option<(String, u32)> {
    match &module.v {
        Val::Str(s) if !s.contains('=') => Some((s.split_whitespace().next()?.to_string(), module.line)),
        _ => arg(task, module, key),
    }
}

/// What a task does to what it acts on (one of [`TASK_ACTIONS`]): from the module's literal `state` (a `uri` call's
/// `method`), else from the verb opening the task's name, else `deploys`. The playbook or role the task sits in says
/// nothing: a deploy role also stops, deregisters and restarts things.
fn task_action(task: &Node, module: &str, args: &Node) -> &'static str {
    let state = arg(task, args, "state").map(|(s, _)| s.to_ascii_lowercase());
    match state.as_deref() {
        Some("absent" | "removed" | "deregistered" | "unregistered" | "uninstalled" | "disabled") => return "undeploys",
        Some("stopped" | "killed") => return "stops",
        Some("restarted" | "reloaded") => return "restarts",
        Some(s) if !s.contains("{{") => return "deploys",
        _ => {}
    }
    if matches!(module, "uri" | "win_uri")
        && arg(task, args, "method").is_some_and(|(m, _)| m.eq_ignore_ascii_case("delete"))
    {
        return "undeploys";
    }
    let name = task.get("name").and_then(Node::str).unwrap_or("").to_ascii_lowercase();
    let verb = name.split(|c: char| !c.is_ascii_alphanumeric()).find(|w| !w.is_empty()).unwrap_or("");
    match verb {
        "deregister" | "unregister" | "remove" | "delete" | "uninstall" | "undeploy" | "destroy" | "purge" | "drop"
        | "decommission" | "disable" | "cleanup" => "undeploys",
        "stop" | "kill" | "halt" | "shutdown" => "stops",
        "restart" | "reload" | "bounce" | "recycle" => "restarts",
        _ => "deploys",
    }
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

/// What a value looks like in a signature: the scalar, or `{…}` / `[…]`.
fn shown(n: &Node) -> String {
    match &n.v {
        Val::Str(s) => collapse(s, SIGNATURE_CAP),
        Val::Map(_) => "{…}".into(),
        Val::Seq(_) => "[…]".into(),
        Val::Null => String::new(),
    }
}

struct Ex<'a> {
    src: &'a str,
    line_starts: Vec<usize>,
    f: FileFacts,
    /// The container of everything this file defines: the role, the playbook's file name, a directory.
    container: Option<String>,
    /// Loop variables in scope (`loop_control.loop_var`).
    loop_vars: Vec<String>,
}

impl<'a> Ex<'a> {
    fn new(src: &'a str, container: Option<String>) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
        Ex { src, line_starts, f: FileFacts::default(), container, loop_vars: Vec::new() }
    }

    fn byte_of_line(&self, line: u32) -> u32 {
        self.line_starts.get(line.saturating_sub(1) as usize).copied().unwrap_or(self.src.len()) as u32
    }

    fn end_byte_of_line(&self, line: u32) -> u32 {
        self.line_starts.get(line as usize).map_or(self.src.len(), |&b| b.saturating_sub(1)) as u32
    }

    fn last_line(&self) -> u32 {
        let n = self.line_starts.len() as u32;
        if self.src.ends_with('\n') { n - 1 } else { n }.max(1)
    }

    #[allow(clippy::too_many_arguments)]
    fn symbol(
        &mut self,
        parent: Option<usize>,
        name: &str,
        kind: SymbolKind,
        signature: String,
        tags: Vec<&'static str>,
        start: u32,
        end: u32,
    ) -> usize {
        let end = end.max(start);
        self.f.symbols.push(Symbol {
            parent,
            name: name.to_string(),
            kind,
            container: self.container.clone(),
            signature,
            doc: None,
            attrs: Vec::new(),
            tags,
            start_line: start,
            end_line: end,
            start_byte: self.byte_of_line(start),
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
        self.f.symbols.len() - 1
    }

    fn reference(&mut self, from: Option<usize>, name: &str, receiver: Option<String>, kind: RefKind, line: u32) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.f.refs.push(Ref { from, name: name.to_string(), receiver, receiver_type: None, kind, line, col: 0, args: None });
    }

    fn fact(&mut self, kind: &'static str, sym: Option<usize>, name: &str, value: &str, line: u32) {
        self.f.ansible.push(AnsibleFact { kind, sym, name: name.to_string(), value: value.to_string(), line });
    }

    /// `var` references for a string: Jinja inside it, or the whole of it as a bare expression.
    fn vars_in(&mut self, from: Option<usize>, s: &str, line: u32, bare: bool) {
        let found: Vec<(String, u32)> = if bare {
            let mut v: Vec<(String, u32)> = expr_vars(s).into_iter().map(|n| (n.to_string(), 0)).collect();
            v.extend(template_vars(s).into_iter().map(|(n, l)| (n.to_string(), l)));
            v
        } else if s.contains("{{") || s.contains("{%") {
            template_vars(s).into_iter().map(|(n, l)| (n.to_string(), l)).collect()
        } else {
            Vec::new()
        };
        for (name, offset) in found {
            if self.loop_vars.contains(&name) {
                continue;
            }
            // A block scalar's text starts on the line after its indicator.
            self.reference(from, &name, None, RefKind::Var, line + offset);
        }
    }

    /// Variable references in every string below `node`; condition keys are bare expressions.
    fn vars_below(&mut self, from: Option<usize>, node: &Node) {
        let mut strings = Vec::new();
        node.walk_strings(&mut strings);
        let owned: Vec<(String, u32)> = strings.into_iter().map(|(s, l)| (s.to_string(), l)).collect();
        for (s, l) in owned {
            self.vars_in(from, &s, l, false);
        }
    }

    fn variable(&mut self, parent: Option<usize>, key: &str, line: u32, value: &Node, tag: &'static str) -> Option<usize> {
        if key.is_empty() || key.contains(' ') || key.contains('{') {
            return None;
        }
        let sig = format!("{key}: {}", shown(value));
        let id = self.symbol(parent, key, SymbolKind::Variable, sig, vec![tag], line, value.end.max(line));
        // A value naming other variables.
        self.vars_below(Some(id), value);
        Some(id)
    }

    fn vars_map(&mut self, parent: Option<usize>, map: &Node, tag: &'static str) {
        for (k, line, v) in map.map().to_vec() {
            self.variable(parent, &k, line, &v, tag);
        }
    }

    /// The tasks of a list, under `parent` (a play, a role, a task file, a block). `handlers` makes them handlers.
    fn tasks(&mut self, parent: Option<usize>, list: &Node, handlers: bool) {
        for t in list.seq() {
            if matches!(t.v, Val::Map(_)) {
                self.task(parent, t, handlers);
            }
        }
    }

    fn task(&mut self, parent: Option<usize>, t: &Node, handler: bool) {
        let name = t.get("name").and_then(Node::str).map(|s| collapse(s, SIGNATURE_CAP));
        let module = module_of(t);
        let block = t.get("block");
        let kind = if handler { SymbolKind::Handler } else { SymbolKind::Task };
        let me = match &name {
            Some(n) => {
                let sig = match (module, block) {
                    (_, Some(_)) => format!("block: {n}"),
                    (Some((m, _)), _) => format!("{m}: {n}"),
                    _ => n.clone(),
                };
                let id = self.symbol(parent, n, kind, sig, Vec::new(), t.line, t.end);
                if handler && let Some(topics) = t.get("listen") {
                    let topics: Vec<String> = topics.strings().into_iter().map(|(s, _)| s.to_string()).collect();
                    self.f.symbols[id].attrs = topics;
                }
                Some(id)
            }
            None => None,
        };
        let from = me.or(parent);
        let pushed = t.get("loop_control").and_then(|c| c.get("loop_var")).and_then(Node::str).map(str::to_string);
        if let Some(v) = &pushed {
            self.loop_vars.push(v.clone());
        }

        if let Some(b) = block {
            self.tasks(from, b, handler);
            for k in ["rescue", "always"] {
                if let Some(l) = t.get(k) {
                    self.tasks(from, l, handler);
                }
            }
        }
        if let Some(n) = t.get("notify") {
            for (h, line) in n.strings() {
                if h.contains("{{") {
                    self.unread(format!("handler named by Jinja: notify {h}"), line);
                } else {
                    self.reference(from, h, None, RefKind::Notify, line);
                }
            }
        }
        if let Some(r) = t.get("register").and_then(|r| r.str().map(|s| (s.to_string(), r.line))) {
            let node = Node { v: Val::Null, line: r.1, end: r.1 };
            self.variable(from, &r.0, r.1, &node, "register");
        }
        for (k, _, v) in t.map().to_vec() {
            if CONDITION_KEYS.contains(&k.as_str()) {
                for (s, line) in v.strings() {
                    let s = s.to_string();
                    self.vars_in(from, &s, line, true);
                }
            } else if k.starts_with("with_") {
                // `with_items: list_var`, the old bare form.
                if let Some(s) = v.str()
                    && !s.contains("{{")
                    && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                    && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                {
                    let s = s.split('.').next().unwrap_or(s).to_string();
                    self.vars_in(from, &s, v.line, true);
                } else {
                    self.vars_below(from, &v);
                }
            } else if !matches!(k.as_str(), "block" | "rescue" | "always" | "name" | "register" | "notify" | "listen") {
                if module.is_some_and(|(m, _)| m == k) {
                    continue; // read below, with what the module means
                }
                self.vars_below(from, &v);
            }
        }
        if let Some((m, args)) = module {
            let m = m.to_string();
            let args = args.clone();
            self.module(from, t, &m, &args);
        }
        if name.is_some() && let Some(n) = t.get("name").and_then(Node::str) {
            let n = n.to_string();
            self.vars_in(from, &n, t.line, false);
        }
        if pushed.is_some() {
            self.loop_vars.pop();
        }
    }

    /// What a task's module refers to and deploys.
    fn module(&mut self, from: Option<usize>, t: &Node, module: &str, args: &Node) {
        let m = short_module(module);
        match m {
            "debug" => {
                if let Some((v, line)) = arg(t, args, "var") {
                    self.vars_in(from, &v, line, true);
                }
                self.vars_below(from, args);
            }
            "assert" => {
                if let Some(that) = args.get("that") {
                    for (s, line) in that.strings() {
                        let s = s.to_string();
                        self.vars_in(from, &s, line, true);
                    }
                }
            }
            "set_fact" => {
                for (k, line, v) in args.map().to_vec() {
                    if k != "cacheable" {
                        self.variable(from, &k, line, &v, "set_fact");
                    }
                }
            }
            "include_role" | "import_role" => {
                // `tasks_from` names the role's task file to run instead of its main one: the receiver.
                let tasks_from = arg(t, args, "tasks_from").map(|(f, _)| f).filter(|f| !f.contains("{{"));
                if let Some((r, line)) = arg(t, args, "name") {
                    self.reference(from, &r, tasks_from, RefKind::Role, line);
                }
                self.vars_below(from, args);
            }
            "include_tasks" | "import_tasks" | "include" | "import_playbook" => {
                if let Some((p, line)) = free_or(t, args, "file") {
                    self.include(from, &p, line);
                }
                self.vars_below(from, args);
            }
            "include_vars" => {
                if let Some((p, line)) = free_or(t, args, "file") {
                    self.fact("vars_file", from, &p, "", line);
                }
                self.vars_below(from, args);
            }
            "template" | "win_template" => {
                if let Some((src, line)) = arg(t, args, "src") {
                    self.template_ref(from, &src, line);
                }
                if let Some((dest, line)) = arg(t, args, "dest") {
                    self.unit_file(from, "deploys", &dest, line);
                }
                self.vars_below(from, args);
            }
            _ => {
                self.vars_below(from, args);
                self.deploys(from, t, m, args);
            }
        }
    }

    /// A construct met but not read: said (see `Unsupported`).
    fn unread(&mut self, construct: String, line: u32) {
        self.f.unsupported.push(Unsupported { area: "ansible", construct, line });
    }

    fn include(&mut self, from: Option<usize>, path: &str, line: u32) {
        let file = path.rsplit('/').next().unwrap_or(path);
        if file.contains("{{") {
            self.unread(format!("include whose file is named by Jinja: {path}"), line);
            return;
        }
        self.reference(from, file, Some(path.to_string()), RefKind::Include, line);
    }

    fn template_ref(&mut self, from: Option<usize>, src: &str, line: u32) {
        let file = src.rsplit(['/', '\\']).next().unwrap_or(src);
        if file.contains("{{") {
            self.unread(format!("template whose source is named by Jinja: {src}"), line);
            return;
        }
        self.reference(from, file, Some(src.to_string()), RefKind::Template, line);
    }

    /// A systemd unit written by `template` or `copy`: the service it defines. An nginx site's configuration: the
    /// site, named by its file without `.conf`.
    fn unit_file(&mut self, from: Option<usize>, action: &'static str, dest: &str, line: u32) {
        let file = dest.rsplit('/').next().unwrap_or(dest);
        if dest.contains("/systemd/") && let Some(unit) = file.strip_suffix(".service") {
            self.fact(action, from, "service", unit, line);
        } else if dest.contains("/nginx/") && (dest.contains("/sites-") || dest.contains("/conf.d/")) && !file.is_empty() {
            self.fact(action, from, "site", file.strip_suffix(".conf").unwrap_or(file), line);
        }
    }

    fn deploys(&mut self, from: Option<usize>, t: &Node, m: &str, args: &Node) {
        let action = task_action(t, m, args);
        let put = |this: &mut Self, what: &'static str, key: &str| {
            if let Some((v, line)) = arg(t, args, key) {
                this.fact(action, from, what, &v, line);
            } else if let Some(list) = args.get(key) {
                for (v, line) in list.strings() {
                    let v = v.to_string();
                    this.fact(action, from, what, &v, line);
                }
            }
        };
        match m {
            "service" | "systemd" | "systemd_service" | "sysvinit" | "win_service" | "nssm" | "win_nssm" | "win_iis_website"
            | "win_iis_webapppool" => put(self, "service", "name"),
            "consul" => put(self, "service", "service_name"),
            "docker_container" => put(self, "image", "image"),
            "docker_image" | "docker_image_pull" => put(self, "image", "name"),
            "git" | "win_git" => put(self, "repo", "repo"),
            "rabbitmq_vhost" => put(self, "vhost", "name"),
            "rabbitmq_queue" => {
                put(self, "queue", "name");
                put(self, "vhost", "vhost");
            }
            "rabbitmq_exchange" => {
                put(self, "exchange", "name");
                put(self, "vhost", "vhost");
            }
            "rabbitmq_binding" => {
                put(self, "exchange", "name");
                put(self, "vhost", "vhost");
            }
            "rabbitmq_user" | "rabbitmq_policy" | "rabbitmq_parameter" => {
                put(self, "vhost", "vhost");
                if let Some(p) = args.get("permissions") {
                    for perm in p.seq() {
                        if let Some(v) = perm.get("vhost").and_then(|v| v.str().map(|s| (s.to_string(), v.line))) {
                            self.fact(action, from, "vhost", &v.0, v.1);
                        }
                    }
                }
            }
            "mysql_db" | "postgresql_db" | "mssql_db" | "mongodb_database" => put(self, "database", "name"),
            "mysql_user" | "postgresql_user" => {
                if let Some((priv_, line)) = arg(t, args, "priv") {
                    for part in priv_.split('/') {
                        let db = part.split('.').next().unwrap_or("").trim_matches(['`', '\'', '"']);
                        if !db.is_empty() && db != "*" && !part.starts_with("*.") {
                            self.fact(action, from, "database", db, line);
                        }
                    }
                }
            }
            "uri" | "get_url" | "win_get_url" | "win_uri" => put(self, "url", "url"),
            "file" | "win_file" => {
                // A directory made for what is deployed; files and removals are left out.
                if arg(t, args, "state").is_some_and(|(s, _)| s == "directory")
                    && let Some((p, line)) = arg(t, args, "path").or_else(|| arg(t, args, "dest")).or_else(|| arg(t, args, "name"))
                {
                    self.fact("deploys", from, "path", &p, line);
                }
            }
            "unarchive" | "win_unzip" => put(self, "path", "dest"),
            "copy" | "win_copy" => {
                if let Some((dest, line)) = arg(t, args, "dest") {
                    self.unit_file(from, action, &dest, line);
                }
            }
            _ => {}
        }
    }

    fn play(&mut self, playbook: usize, p: &Node) {
        if let Some(path) = p.get("import_playbook").or_else(|| p.get("ansible.builtin.import_playbook")).and_then(Node::str) {
            let path = path.to_string();
            self.include(Some(playbook), &path, p.line);
            return;
        }
        let hosts = p.get("hosts").map(shown).unwrap_or_default();
        let name = match p.get("name").and_then(Node::str) {
            Some(n) => collapse(n, SIGNATURE_CAP),
            None => format!("hosts: {hosts}"),
        };
        let id = self.symbol(Some(playbook), &name, SymbolKind::Play, format!("hosts: {hosts}"), Vec::new(), p.line, p.end);
        if let Some(v) = p.get("vars") {
            self.vars_map(Some(id), v, "play_vars");
        }
        if let Some(files) = p.get("vars_files") {
            for (f, line) in files.strings() {
                let f = f.to_string();
                self.fact("vars_file", Some(id), &f, "", line);
            }
        }
        if let Some(roles) = p.get("roles") {
            for r in roles.seq() {
                let named = match &r.v {
                    Val::Str(s) => Some((s.clone(), r.line)),
                    Val::Map(_) => r.get("role").or_else(|| r.get("name")).and_then(|n| n.str().map(|s| (s.to_string(), n.line))),
                    _ => None,
                };
                if let Some((role, line)) = named {
                    self.reference(Some(id), &role, None, RefKind::Role, line);
                }
                if let Val::Map(_) = r.v {
                    for (k, _, v) in r.map().to_vec() {
                        if !matches!(k.as_str(), "role" | "name") {
                            self.vars_below(Some(id), &v);
                        }
                    }
                }
            }
        }
        for (k, _, v) in p.map().to_vec() {
            match k.as_str() {
                "pre_tasks" | "tasks" | "post_tasks" => self.tasks(Some(id), &v, false),
                "handlers" => self.tasks(Some(id), &v, true),
                "hosts" | "serial" | "remote_user" | "become_user" | "environment" => self.vars_below(Some(id), &v),
                "when" => {
                    for (s, line) in v.strings() {
                        let s = s.to_string();
                        self.vars_in(Some(id), &s, line, true);
                    }
                }
                _ => {}
            }
        }
    }

    fn requirements(&mut self, doc: &Node) {
        let list = match &doc.v {
            Val::Seq(_) => doc,
            Val::Map(_) => match doc.get("roles") {
                Some(r) => r,
                None => return,
            },
            _ => return,
        };
        for r in list.seq().to_vec() {
            self.requirement(None, &r);
        }
    }

    /// A requirement (`src`, `name`), or a meta dependency (which also refers to the role).
    fn requirement(&mut self, from: Option<usize>, r: &Node) -> Option<(String, u32)> {
        let src = r.get("src").and_then(|n| n.str().map(|s| (s.to_string(), n.line)));
        let name = r.get("name").or_else(|| r.get("role")).and_then(|n| n.str().map(|s| (s.to_string(), n.line)));
        let name = match (&name, &src) {
            (Some(n), _) => n.clone(),
            (None, Some((s, line))) => {
                let last = s.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or(s);
                (last.trim_end_matches(".git").to_string(), *line)
            }
            (None, None) => return r.str().map(|s| (s.to_string(), r.line)),
        };
        if let Some((s, line)) = &src {
            self.fact("requirement", from, &name.0, s, *line);
        }
        Some(name)
    }
}

/// The container of a playbook or task file that isn't in a role: its file name for a playbook, else the directory
/// holding its `tasks/`/`vars/` directory (or its own directory).
fn dir_container(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let n = parts.len();
    let dir = if n >= 2 && matches!(parts[n - 2], "tasks" | "handlers" | "vars" | "defaults") { n.checked_sub(3) } else { n.checked_sub(2) };
    dir.and_then(|i| parts.get(i)).map(|s| s.to_string())
}

/// Facts of an Ansible file at `path` (repo-relative).
pub fn extract(path: &str, src: &str) -> FileFacts {
    let file = path.rsplit('/').next().unwrap_or(path);
    let place = place(path).unwrap_or(Place { kind: FileKind::Other, role: None, role_dir: None });
    if place.kind == FileKind::Template {
        let mut ex = Ex::new(src, place.role.clone());
        let last = ex.last_line();
        let id = ex.symbol(None, file, SymbolKind::Template, file.to_string(), Vec::new(), 1, last);
        for (v, line) in template_vars(src) {
            ex.reference(Some(id), v, None, RefKind::Var, line + 1);
        }
        return ex.f;
    }
    let (docs, failed) = parse(src);
    let container = match &place.role {
        Some(r) => Some(r.clone()),
        None => match place.kind {
            FileKind::GroupVars | FileKind::HostVars => {
                Some(file.trim_end_matches(".yml").trim_end_matches(".yaml").to_string()).filter(|f| f != "main")
                    .or_else(|| dir_container(path))
            }
            FileKind::Other => Some(file.to_string()),
            _ => dir_container(path),
        },
    };
    let mut ex = Ex::new(src, container);
    ex.f.parse_errors = failed as u32;
    let last = ex.last_line();
    let role_main = place.role.is_some() && file.starts_with("main.");
    for doc in &docs {
        match place.kind {
            FileKind::Defaults | FileKind::Vars | FileKind::GroupVars | FileKind::HostVars => {
                let tag = match (place.kind, place.role.is_some()) {
                    (FileKind::Defaults, true) => "role_defaults",
                    (FileKind::Vars, true) => "role_vars",
                    (FileKind::GroupVars, _) => "group_vars",
                    (FileKind::HostVars, _) => "host_vars",
                    _ => "vars_file",
                };
                ex.vars_map(None, doc, tag);
            }
            FileKind::Meta => {
                let Some(role) = place.role.clone() else { continue };
                let galaxy = doc.get("galaxy_info");
                let desc = galaxy.and_then(|g| g.get("description")).and_then(Node::str).map(|s| collapse(s, SIGNATURE_CAP));
                let id = ex.symbol(None, &role, SymbolKind::Role, format!("role {role}"), vec!["meta"], 1, last);
                ex.f.symbols[id].doc = desc;
                if let Some(alias) = galaxy.and_then(|g| g.get("role_name")).and_then(Node::str) {
                    let alias = alias.to_string();
                    ex.fact("role_name", Some(id), &alias, "", 1);
                }
                if let Some(deps) = doc.get("dependencies") {
                    for d in deps.seq().to_vec() {
                        if let Some((name, line)) = ex.requirement(Some(id), &d) {
                            ex.reference(Some(id), &name, None, RefKind::Role, line);
                        }
                        if let Val::Map(_) = d.v {
                            for (k, _, v) in d.map().to_vec() {
                                if !matches!(k.as_str(), "role" | "name" | "src" | "scm" | "version") {
                                    ex.vars_below(Some(id), &v);
                                }
                            }
                        }
                    }
                }
            }
            FileKind::Requirements => ex.requirements(doc),
            FileKind::Tasks | FileKind::Handlers | FileKind::Other => {
                let plays = doc.seq().iter().any(|p| p.get("hosts").is_some() || p.get("import_playbook").is_some() || p.get("ansible.builtin.import_playbook").is_some());
                if plays && place.kind == FileKind::Other {
                    let id = ex.symbol(None, file, SymbolKind::Playbook, file.to_string(), Vec::new(), 1, last);
                    for p in doc.seq() {
                        ex.play(id, p);
                    }
                    continue;
                }
                if !matches!(doc.v, Val::Seq(_)) {
                    continue;
                }
                let handlers = place.kind == FileKind::Handlers;
                let top = if role_main && place.kind == FileKind::Tasks {
                    let role = place.role.clone().unwrap_or_default();
                    Some(ex.symbol(None, &role, SymbolKind::Role, format!("role {role}"), Vec::new(), 1, last))
                } else if handlers {
                    None
                } else {
                    Some(ex.symbol(None, file, SymbolKind::Tasks, file.to_string(), Vec::new(), 1, last))
                };
                ex.tasks(top, doc, handlers);
            }
            FileKind::Template => {}
        }
    }
    ex.f
}

/// Gives a repo-root role's symbols and facts their name: the repo's directory name.
pub fn name_root_role(facts: &mut FileFacts, name: &str) {
    for s in &mut facts.symbols {
        if s.name == ROOT_ROLE {
            s.name = name.to_string();
            s.signature = format!("role {name}");
        }
        if s.container.as_deref() == Some(ROOT_ROLE) {
            s.container = Some(name.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn syms(f: &FileFacts) -> Vec<(String, &'static str, Option<String>, u32)> {
        f.symbols.iter().map(|s| (s.name.clone(), s.kind.as_str(), s.container.clone(), s.start_line)).collect()
    }

    fn refs(f: &FileFacts) -> Vec<(String, &'static str, Option<String>, u32)> {
        f.refs.iter().map(|r| (r.name.clone(), r.kind.as_str(), r.from.map(|i| f.symbols[i].name.clone()), r.line)).collect()
    }

    #[test]
    fn places_by_path() {
        let p = |path: &str| place(path).map(|p| (p.kind, p.role));
        assert_eq!(p("roles/web/tasks/main.yml"), Some((FileKind::Tasks, Some("web".into()))));
        assert_eq!(p("deploy/roles/web/handlers/main.yml"), Some((FileKind::Handlers, Some("web".into()))));
        assert_eq!(p("roles/web/templates/app.conf.j2"), Some((FileKind::Template, Some("web".into()))));
        assert_eq!(p("defaults/main.yml"), Some((FileKind::Defaults, Some(ROOT_ROLE.into()))));
        assert_eq!(p("templates/x.conf.jn2"), Some((FileKind::Template, Some(ROOT_ROLE.into()))));
        assert_eq!(p("inventory/group_vars/web.yml"), Some((FileKind::GroupVars, None)));
        assert_eq!(p("plays/backup/vars/aws.yml"), Some((FileKind::Vars, None)));
        assert_eq!(p("requirements.yml"), Some((FileKind::Requirements, None)));
        assert_eq!(p("deploy.yml"), None);
        assert_eq!(p(".gitlab-ci.yml"), None);
        assert_eq!(p("ci/.gitlab-ci-build.yml"), None);
        assert_eq!(p(".github/workflows/tasks/x.yml"), None);
        assert_eq!(p("docker-compose.yml"), None);
        assert!(candidate("deploy.yml"));
        assert!(!candidate("docker-compose.override.yml"));
        assert!(!candidate("src/app.cs"));
    }

    #[test]
    fn sniffs_playbooks_only() {
        assert!(sniff("---\n# deploy\n- name: Deploy\n  hosts: web\n  tasks: []\n"));
        assert!(sniff("- hosts: all\n  roles: [x]\n"));
        assert!(sniff("- import_playbook: other.yml\n"));
        assert!(!sniff("stages:\n  - build\nbuild:\n  script: make\n"));
        assert!(!sniff("version: '3'\nservices:\n  web:\n    image: x\n"));
        assert!(!sniff("- name: a task\n  shell: echo\n"));
        assert!(!sniff("- one\n- two\n"));
    }

    #[test]
    fn jinja_variables() {
        assert_eq!(expr_vars("app_port | default(other_port) | int"), ["app_port", "other_port"]);
        assert_eq!(expr_vars("result.stdout is defined and 'x' in names"), ["result", "names"]);
        assert_eq!(expr_vars("lookup('env', 'HOME') ~ item.name ~ ansible_hostname"), Vec::<&str>::new());
        assert_eq!(expr_vars("hostvars[target]['ip']"), ["target"]);
        assert_eq!(expr_vars("x | regex_replace('a', b) | to_json(indent=2)"), ["x", "b"]);
        let t = "port={{ port }}\n{% for u in users %}{{ u.name }}{{ suffix }}{% endfor %}\n{# {{ hidden }} #}\n{% if enabled %}on{% endif %}";
        assert_eq!(template_vars(t), [("port", 0), ("users", 1), ("suffix", 1), ("enabled", 3)]);
    }

    const PLAYBOOK: &str = "---
- name: Deploy app
  hosts: \"{{ TARGET }}\"
  vars:
    app_dir: /srv/{{ app_name }}
  vars_files:
    - vars/prod.yml
  roles:
    - common
    - role: web
      web_port: \"{{ port }}\"
  tasks:
    - name: Render config
      template:
        src: templates/app.conf.j2
        dest: /etc/app.conf
      notify: restart app
    - include_tasks: tasks/extra.yml
    - name: Start service
      systemd: name=myapp state=started
      when: start_now and not (skip | bool)
      register: started
    - block:
        - name: Show
          debug:
            var: started
      when: started is changed
  handlers:
    - name: restart app
      service:
        name: myapp
        state: restarted
- import_playbook: other.yml
";

    #[test]
    fn playbook_symbols_and_references() {
        let f = extract("deploy.yml", PLAYBOOK);
        assert_eq!(f.parse_errors, 0);
        let s = syms(&f);
        let c = Some("deploy.yml".to_string());
        assert_eq!(s[0], ("deploy.yml".into(), "playbook", c.clone(), 1));
        assert!(s.contains(&("Deploy app".into(), "play", c.clone(), 2)));
        assert!(s.contains(&("app_dir".into(), "variable", c.clone(), 5)));
        assert!(s.contains(&("Render config".into(), "task", c.clone(), 13)));
        assert!(s.contains(&("started".into(), "variable", c.clone(), 22)));
        assert!(s.contains(&("Show".into(), "task", c.clone(), 24)));
        assert!(s.contains(&("restart app".into(), "handler", c.clone(), 29)));
        let r = refs(&f);
        let has = |name: &str, kind: &str, from: &str| r.iter().any(|x| x.0 == name && x.1 == kind && x.2.as_deref() == Some(from));
        assert!(has("TARGET", "var", "Deploy app"));
        assert!(has("app_name", "var", "app_dir"));
        assert!(has("common", "role", "Deploy app"));
        assert!(has("web", "role", "Deploy app"));
        assert!(has("port", "var", "Deploy app"));
        assert!(has("app.conf.j2", "template", "Render config"));
        assert!(has("restart app", "notify", "Render config"));
        assert!(has("extra.yml", "include", "Deploy app"));
        assert!(has("start_now", "var", "Start service"));
        assert!(has("skip", "var", "Start service"));
        assert!(has("started", "var", "Show"));
        assert!(has("other.yml", "include", "deploy.yml"));
        let tpl = f.refs.iter().find(|x| x.kind == RefKind::Template).unwrap();
        assert_eq!(tpl.receiver.as_deref(), Some("templates/app.conf.j2"));
        let deploys: Vec<(&str, &str, &str)> =
            f.ansible.iter().map(|a| (a.kind, a.name.as_str(), a.value.as_str())).collect();
        assert!(deploys.contains(&("vars_file", "vars/prod.yml", "")));
        assert!(deploys.contains(&("deploys", "service", "myapp")));
    }

    #[test]
    fn role_files() {
        let tasks = "- name: Install\n  apt: name=nginx\n- name: Add vhost\n  rabbitmq_vhost:\n    name: \"{{ rabbit_vhost }}\"\n- include_role:\n    name: common\n";
        let f = extract("roles/web/tasks/main.yml", tasks);
        assert_eq!(syms(&f)[0], ("web".into(), "role", Some("web".into()), 1));
        assert!(refs(&f).contains(&("common".into(), "role", Some("web".into()), 7)));
        assert!(refs(&f).contains(&("rabbit_vhost".into(), "var", Some("Add vhost".into()), 5)));
        assert!(f.ansible.iter().any(|a| a.kind == "deploys" && a.name == "vhost" && a.value == "{{ rabbit_vhost }}"));
        let f = extract("roles/web/tasks/install.yml", "- name: x\n  debug: msg=hi\n");
        assert_eq!(syms(&f)[0], ("install.yml".into(), "tasks", Some("web".into()), 1));

        let defaults = "---\nrabbit_vhost: Orders\nweb_port: 8080\nurl: \"http://{{ host }}:{{ web_port }}\"\n";
        let f = extract("defaults/main.yml", defaults);
        let mut f2 = f.clone();
        name_root_role(&mut f2, "Configure.Web");
        let v: Vec<_> = f2.symbols.iter().map(|s| (s.name.as_str(), s.container.as_deref(), s.tags.clone(), s.signature.as_str())).collect();
        assert_eq!(v[0], ("rabbit_vhost", Some("Configure.Web"), vec!["role_defaults"], "rabbit_vhost: Orders"));
        assert!(refs(&f).contains(&("web_port".into(), "var", Some("url".into()), 4)));

        let meta = "galaxy_info:\n  role_name: web_role\n  description: Web server\ndependencies:\n  - src: http://git.example.com/Roles/Common.Base.git\n    scm: git\n    name: base\n  - role: tls\n";
        let f = extract("meta/main.yml", meta);
        assert_eq!(f.symbols[0].tags, ["meta"]);
        assert_eq!(f.symbols[0].doc.as_deref(), Some("Web server"));
        assert!(refs(&f).iter().any(|r| r.0 == "base" && r.1 == "role"));
        assert!(refs(&f).iter().any(|r| r.0 == "tls" && r.1 == "role"));
        let facts: Vec<(&str, &str, &str)> = f.ansible.iter().map(|a| (a.kind, a.name.as_str(), a.value.as_str())).collect();
        assert!(facts.contains(&("role_name", "web_role", "")));
        assert!(facts.contains(&("requirement", "base", "http://git.example.com/Roles/Common.Base.git")));
    }

    #[test]
    fn requirements_and_templates() {
        let req = "- src: http://git.example.com/Roles/Database.Install.git\n  scm: git\n  name: install_db\n- src: https://git.example.com/Roles/Common.Dns.git\n";
        let f = extract("plays/db/requirements.yml", req);
        let facts: Vec<(&str, &str)> = f.ansible.iter().map(|a| (a.name.as_str(), a.value.as_str())).collect();
        assert_eq!(facts, [("install_db", "http://git.example.com/Roles/Database.Install.git"), ("Common.Dns", "https://git.example.com/Roles/Common.Dns.git")]);

        let f = extract("roles/web/templates/app.conf.j2", "listen {{ web_port }}\nvhost={{ rabbit_vhost }}\n");
        assert_eq!(syms(&f), [("app.conf.j2".into(), "template", Some("web".into()), 1)]);
        assert_eq!(refs(&f), [("web_port".into(), "var", Some("app.conf.j2".into()), 1), ("rabbit_vhost".into(), "var", Some("app.conf.j2".into()), 2)]);
    }

    #[test]
    fn directories_and_nginx_sites_are_deployed() {
        let tasks = "- name: App dir\n  file:\n    path: \"/srv/{{ app }}\"\n    state: directory\n- name: Old file\n  file: path=/tmp/x state=absent\n- name: Unpack\n  unarchive:\n    src: a.zip\n    dest: /srv/app\n- name: Site\n  template:\n    src: site.j2\n    dest: /etc/nginx/sites-available/shop.conf\n";
        let f = extract("roles/web/tasks/main.yml", tasks);
        let deploys: Vec<(&str, &str)> = f.ansible.iter().filter(|a| a.kind == "deploys").map(|a| (a.name.as_str(), a.value.as_str())).collect();
        assert_eq!(deploys, [("path", "/srv/{{ app }}"), ("path", "/srv/app"), ("site", "shop")]);
    }

    #[test]
    fn task_kinds_come_from_state_and_verb() {
        let tasks = "- name: Deregister service in consul\n  uri:\n    url: \"http://localhost:8500/v1/agent/service/deregister/{{ svc }}\"\n    method: PUT\n- name: Remove old registration\n  consul:\n    service_name: orders\n    state: absent\n- name: Register service\n  consul:\n    service_name: orders\n- name: Stop app\n  systemd:\n    name: orders\n    state: \"{{ wanted }}\"\n- name: Ensure app\n  service: name=orders state=restarted\n- name: Remove queue\n  rabbitmq_queue:\n    name: q1\n    state: present\n- name: Delete hook\n  uri:\n    url: http://h/hooks/1\n    method: DELETE\n";
        let f = extract("roles/deploy/tasks/main.yml", tasks);
        let facts: Vec<(&str, &str, &str)> = f.ansible.iter().map(|a| (a.kind, a.name.as_str(), a.value.as_str())).collect();
        assert_eq!(
            facts,
            [
                ("undeploys", "url", "http://localhost:8500/v1/agent/service/deregister/{{ svc }}"),
                ("undeploys", "service", "orders"),
                ("deploys", "service", "orders"),
                ("stops", "service", "orders"),
                ("restarts", "service", "orders"),
                ("deploys", "queue", "q1"),
                ("undeploys", "url", "http://h/hooks/1"),
            ]
        );
    }

    #[test]
    fn syntax_errors_keep_what_came_before() {
        let src = "- name: first\n  shell: echo\n- name: second\n   bad: indent\n  - x\n";
        let f = extract("tasks/main.yml", src);
        assert_eq!(f.parse_errors, 1);
        assert!(f.symbols.iter().any(|s| s.name == "first"));
    }

    #[test]
    fn duplicate_keys_and_vault_values_are_read() {
        let src = "db_password: !vault |\n  $ANSIBLE_VAULT;1.1;AES256\n  6162\nport: 1\nport: 2\n";
        let f = extract("vars/main.yml", src);
        assert_eq!(f.parse_errors, 0);
        let v: Vec<&str> = f.symbols.iter().map(|s| s.signature.as_str()).collect();
        assert_eq!(v, ["db_password: <vault>", "port: 1", "port: 2"]);
    }
}
