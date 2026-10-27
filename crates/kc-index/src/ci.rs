//! Links application repos to how they are deployed: GitLab CI pipeline → shared CI templates → Ansible playbook →
//! roles, with the variables the pipeline passes, read from the facts `kc_extract::gitlabci` keeps.
//!
//! A repo's pipeline starts at its `.gitlab-ci.yml`. Includes are read as GitLab merges them (an included file
//! first, the including file's own content over it; later includes over earlier ones): `local` files of the same
//! project (globs included), `project` files of the repo whose `origin` is that project path (`exact`; a repo named
//! like the path's last segment is `unique_name`), `remote` raw URLs of such a project, and `component`s
//! (`templates/<name>.yml` of such a project, read with `$[[ inputs.x ]]` replaced by the include's inputs over the
//! file's `spec:` defaults, once per set of inputs). Include and job `rules`
//! are read for a push to the default branch (`CI_PIPELINE_SOURCE` is `push`, or `parent_pipeline` in a child
//! pipeline; no merge request, no tag): a rule that is false is skipped, one that can't be decided (it names a
//! variable nothing sets) counts as matching, and the chain records its condition.
//!
//! A job's `variables`, scripts, `trigger` and `rules` come from its own definition, else from the templates it
//! `extends` (later ones first, recursively; variables merge, the job's own winning), else from `default:`.
//! Variables in scope, highest first: those a trigger job passes to its child pipeline, the job's own, the
//! pipeline's global ones (included files' too), then GitLab's predefined ones this can know (`CI_PROJECT_NAME`,
//! `CI_JOB_NAME`, `CI_JOB_STAGE`, …). The job's `before_script` and `script` are then run in order: `X=…` sets a
//! variable, `[[ -z "$X" ]] && X=…` defaults it, `pushd`/`cd` change directory, and `$(basename DIR/*.csproj
//! .csproj)` reads the one project file in that directory of the application repo. `$X`, `${X}`, `${X#p}`, `${X%p}`
//! and `${X:-d}` expand.
//!
//! An `ansible-playbook` command gives a chain: its playbook (a path under a directory a `git clone` in the job
//! made links to the repo with that clone URL as `origin`, `exact`; a path in the application repo, `exact`; a
//! path under a directory named like a repo, `unique_name`), its inventory and its extra vars, each with its value
//! and where that comes from, or the variables nothing in the files sets (project or group CI settings, protected
//! variables, values computed by earlier jobs). Then the playbook's roles (in its repo, or linked across repos as
//! `kc_index::ansible` links them), and what the playbook and roles deploy with those values: systemd units,
//! services, consul names, paths, nginx sites, read through the play's and roles' variables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kc_store::ansible::FactRow;
use serde::Serialize;

use crate::ansible::{Ansible, Deploy};
use crate::ci_tools::{DeployStep, FileRef, ToolCtx, Via};
use crate::workspace::{RepoSummary, normalize_remote};

/// The most files one pipeline reads, the deepest include or `extends` nesting, and child pipelines per chain.
const MAX_FILES: usize = 200;
const MAX_DEPTH: usize = 12;
const MAX_CHILDREN: usize = 3;
/// The most `parallel: matrix:` combinations of one job read.
const MAX_MATRIX: usize = 32;

/// One `parallel: matrix:` combination: each variable and value, and the entry (fact, repo) it comes from.
type Combination<'a> = (Vec<(String, String)>, &'a FactRow, &'a RepoSummary);

/// One step of a deploy chain.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Hop {
    /// `job` (a job of the pipeline), `template` (the job it extends that supplies the step), `trigger` (the child
    /// pipeline a job starts, by file).
    pub how: &'static str,
    /// The job, or the child pipeline's file.
    pub name: String,
    pub repo: PathBuf,
    pub file: String,
    pub line: u32,
    /// How the pipeline reaches the file, when it is included: each including file, outermost first (`file`, or
    /// `repo:file` in another repo).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub included: Vec<String>,
}

/// A value the pipeline passes, as written and as resolved.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Passed {
    pub name: String,
    pub written: String,
    /// The value, when every variable in it is set in the files read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Otherwise the value with what is known substituted and the rest as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<String>,
    /// Where the value is set: `file:line` (`repo:file:line` outside the application repo), or a project file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Variables nothing in the files sets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

/// The playbook an `ansible-playbook` command runs.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlaybookLink {
    pub written: String,
    /// The repo holding it, when one is indexed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    /// Its path in that repo.
    pub file: String,
    /// `exact`, `unique_name`, or `unlinked`.
    pub confidence: &'static str,
    /// The `git clone` URL the path's directory comes from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clone: Option<String>,
    /// The repo has a playbook at that path.
    pub found: bool,
    /// Files the playbook itself reads, by path in its repo, from the checkout: templates its `template` tasks
    /// render, task files and playbooks it includes, variable files (`vars_files`, `include_vars`), and the
    /// `requirements.yml` beside it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// How many files it reads, when more than [`MAX_ROLE_FILES`] are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_total: Option<usize>,
}

/// The files a playbook reads (see [`PlaybookLink::files`]), at most [`MAX_ROLE_FILES`], and how many there are.
fn playbook_files(root: &Path, playbook: &str) -> (Vec<String>, usize) {
    static REFS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let refs = REFS.get_or_init(|| {
        regex::Regex::new(r#"(?m)(?:\bsrc[:=]\s*|(?:include_tasks|import_tasks|import_playbook|include|include_vars):\s*(?:file:\s*)?)["']?([^"'\s#{}]+)"#).unwrap()
    });
    let Some(text) = crate::ci_tools::read(root, playbook) else { return (Vec::new(), 0) };
    // Commented-out tasks (`# include_vars: …`) don't count.
    let text: String = text.lines().filter(|l| !l.trim_start().starts_with('#')).map(|l| format!("{l}\n")).collect();
    let dir = playbook.rsplit_once('/').map_or("", |(d, _)| d);
    let at = |rel: &str| if dir.is_empty() { rel.to_string() } else { format!("{dir}/{rel}") };
    let mut out: Vec<String> = Vec::new();
    // `vars_files:` lists one file per item.
    let mut vars_files = Vec::new();
    let mut in_list: Option<usize> = None;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if let Some(i) = in_list {
            match t.strip_prefix("- ") {
                Some(item) if indent >= i => {
                    vars_files.push(item.trim().trim_matches(['"', '\'']).to_string());
                    continue;
                }
                _ if t.is_empty() || t.starts_with('#') => continue,
                _ => in_list = None,
            }
        }
        if t == "vars_files:" {
            in_list = Some(indent);
        }
    }
    let refs: Vec<String> = refs.captures_iter(&text).map(|c| c[1].to_string()).chain(vars_files).collect();
    for rel in &refs {
        if rel.contains("{{") {
            continue;
        }
        for cand in [at(&format!("templates/{rel}")), at(rel), at(&format!("vars/{rel}"))] {
            if root.join(&cand).is_file() && !out.contains(&cand) && cand != playbook {
                out.push(cand);
                break;
            }
        }
    }
    for req in ["requirements.yml", "roles/requirements.yml"] {
        let cand = at(req);
        if root.join(&cand).is_file() && !out.contains(&cand) {
            out.push(cand);
        }
    }
    let total = out.len();
    out.truncate(MAX_ROLE_FILES);
    (out, total)
}

/// A role the playbook (or a role it uses) runs.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RoleHop {
    pub name: String,
    /// The repo holding it; empty for a role no indexed repo holds (`external_likely`).
    pub repo: PathBuf,
    /// The role's directory in that repo (`""` for a repo that is one role).
    pub dir: String,
    pub confidence: &'static str,
    /// How many roles away from the playbook: 1 for a role the playbook runs, 2 for one that role runs, ….
    pub depth: u32,
    /// Its task files (`tasks/main.yml` first), handler files and templates, by path in its repo, read from the
    /// checkout; at most [`MAX_ROLE_FILES`] in all.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub handlers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<String>,
    /// `meta/main.yml`, where its role dependencies are.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub meta: Vec<String>,
    /// Files left out by the cap.
    #[serde(skip_serializing_if = "is_zero")]
    pub more_files: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The most roles a chain lists, how deep into roles running roles it goes, and the most files listed per role.
const MAX_ROLES: usize = 24;
const MAX_ROLE_DEPTH: u32 = 4;
pub const MAX_ROLE_FILES: usize = 40;

impl RoleHop {
    /// A role no indexed repo holds: no repo (an empty path), `external_likely`.
    fn external(name: String, depth: u32) -> RoleHop {
        RoleHop { name, repo: PathBuf::new(), dir: String::new(), confidence: "external_likely", depth, tasks: Vec::new(), handlers: Vec::new(), templates: Vec::new(), meta: Vec::new(), more_files: 0 }
    }

    fn new(name: String, repo: PathBuf, dir: String, confidence: &'static str, depth: u32) -> RoleHop {
        let mut r = RoleHop { name, repo, dir, confidence, depth, tasks: Vec::new(), handlers: Vec::new(), templates: Vec::new(), meta: Vec::new(), more_files: 0 };
        let mut left = MAX_ROLE_FILES;
        for (sub, list) in [("tasks", &mut r.tasks), ("handlers", &mut r.handlers), ("meta", &mut r.meta), ("templates", &mut r.templates)] {
            let base = if r.dir.is_empty() { sub.to_string() } else { format!("{}/{sub}", r.dir) };
            let mut files = Vec::new();
            walk_files(&r.repo, &base, &mut files, 0);
            // Never a file the role repo's AI privacy file blocks.
            if !files.is_empty() {
                let privacy = crate::privacy::Privacy::load(&r.repo);
                files.retain(|f| !privacy.blocks(f));
            }
            files.sort_by_key(|f| (!(f.ends_with("/main.yml") || f.ends_with("/main.yaml")), f.clone()));
            let keep = files.len().min(left);
            r.more_files += files.len() - keep;
            left -= keep;
            list.extend(files.into_iter().take(keep));
        }
        r
    }
}

/// Files under `dir` of the checkout at `root`, by path from `root`, a few levels deep.
fn walk_files(root: &Path, dir: &str, out: &mut Vec<String>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{dir}/{name}");
        match e.file_type() {
            Ok(t) if t.is_dir() && depth < 4 => walk_files(root, &path, out, depth + 1),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
        if out.len() > 4 * MAX_ROLE_FILES {
            return;
        }
    }
}

/// How an application repo is deployed: one `ansible-playbook` command a job of its pipeline runs, or a job's other
/// deploy commands (see [`crate::ci_tools`]).
#[derive(Debug, Clone, Serialize)]
pub struct Chain {
    pub app: PathBuf,
    /// From the application's job to the job running the command.
    pub hops: Vec<Hop>,
    /// The command (the first one, for a job's other deploy commands).
    pub run: Hop,
    /// `ansible`; else the tool of the job's first deploy command (`wrangler`, `aws`, `terraform`, `docker`,
    /// `kubectl`, `helm`, `sam`, `cdk`), or `release` when it only publishes (`npm publish`, `upack push`,
    /// `release-cli create`).
    pub kind: &'static str,
    /// The playbook, for `ansible`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playbook: Option<PlaybookLink>,
    /// The deploy commands, for the other kinds, in the order they run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<crate::ci_tools::DeployStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Passed>,
    pub vars: Vec<Passed>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RoleHop>,
    /// What the playbook and its roles deploy with the values passed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deploys: Vec<Deploy>,
    /// Rule conditions on the way that couldn't be decided (the chain runs only when they hold).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<String>,
    /// The `parallel: matrix:` values of the job's run giving the chain (`NAME=value`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matrix: Vec<String>,
    /// The CI files the chain goes through, the application's job first: each job's and template's file, and the files
    /// including them on the way (`file`, or `repo:file` in another repo).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ci_files: Vec<String>,
    /// The weakest link: `exact`, `unique_name`, `external_likely` (through a GitLab built-in template) or
    /// `ambiguous` (through an include whose rule can't be decided).
    pub confidence: &'static str,
    /// What the chain did not check: `revision_unverified` when an include on the way asks for a `ref` that isn't
    /// the indexed checkout's HEAD (or can't be checked); such an include is never `exact`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<kc_store::answer::Assumption>,
    /// Every job and template the chain goes through (repo, file, name), for reverse lookups.
    #[serde(skip)]
    pub jobs: Vec<(PathBuf, String, String)>,
}

impl Chain {
    /// The chain goes through job (or template) `name` of `file` in the repo at `repo`.
    pub fn through_job(&self, repo: &Path, file: &str, name: &str) -> bool {
        self.jobs.iter().any(|(r, f, n)| r == repo && f == file && n == name)
    }

    /// The chain runs the playbook `file` of the repo at `repo`.
    pub fn runs_playbook(&self, repo: &Path, file: &str) -> bool {
        self.playbook.as_ref().is_some_and(|p| p.repo.as_deref() == Some(repo) && p.file == file)
    }

    /// What the chain runs, for a list: the playbook (`repo/file`, or as written), else its commands (`aws s3 cp,
    /// aws cloudfront update-distribution`).
    pub fn target(&self) -> String {
        match &self.playbook {
            Some(p) => match &p.repo {
                Some(r) => format!("{}/{}", dir_name(r), p.file),
                None => p.written.clone(),
            },
            None => {
                let mut names: Vec<String> = Vec::new();
                for s in &self.steps {
                    let n = format!("{} {}", s.tool, s.action);
                    if !names.contains(&n) {
                        names.push(n);
                    }
                }
                names.join(", ")
            }
        }
    }

    /// The chain's playbook runs the role in `dir` of the repo at `repo`.
    pub fn uses_role(&self, repo: &Path, dir: &str) -> bool {
        self.roles.iter().any(|r| r.repo == repo && r.dir == dir)
    }

    /// The value passed as `name`, resolved or partly.
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars.iter().find(|v| v.name == name).and_then(|v| v.value.as_deref().or(v.partial.as_deref()))
    }
}

fn rank(c: &str) -> u8 {
    match c {
        "exact" => 4,
        "unique_name" => 3,
        "external_likely" => 2,
        "ambiguous" => 1,
        _ => 0,
    }
}

/// The revision an include of kind `how` asks for: `None` when it asks for none (the default branch); `Some(None)` when
/// it names one that can't be read here (a variable this pipeline doesn't set).
fn include_rev(how: &str, spec: &serde_json::Value, env: &Env, ctx: &Ctx) -> Option<Option<String>> {
    let written = match how {
        "project" => spec.get("ref").and_then(|v| v.as_str())?.to_string(),
        "component" => spec.get("file").and_then(|v| v.as_str())?.rsplit_once('@')?.1.to_string(),
        _ => return None,
    };
    // `~latest` is the newest release, which the checkout may or may not be.
    if written.starts_with('~') {
        return Some(None);
    }
    let r = env.expand(&written, None, ctx, 0);
    Some(r.unresolved.is_empty().then_some(r.text).filter(|t| !t.is_empty()))
}

fn weakest(a: &'static str, b: &'static str) -> &'static str {
    if rank(b) < rank(a) { b } else { a }
}

fn dir_name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if dir.is_empty() || rel.starts_with('/') { Vec::new() } else { dir.split('/').collect() };
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// GitLab's glob for `include:local`: `*` within a path segment, `**` across them.
fn glob(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            [b'*', b'*', b'/', rest @ ..] => (0..=s.len()).any(|i| (i == 0 || s[i - 1] == b'/') && go(rest, &s[i..])),
            [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| go(rest, &s[i..])),
            [b'*', rest @ ..] => (0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != b'/').any(|i| go(rest, &s[i..])),
            [c, rest @ ..] => s.first().is_some_and(|x| x.eq_ignore_ascii_case(c)) && go(rest, &s[1..]),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

// ---------------------------------------------------------------------------------------------------------------
// Variables.

/// Where a value is set.
#[derive(Debug, Clone)]
struct Origin<'a> {
    repo: &'a Path,
    file: String,
    line: u32,
}

/// One definition of a variable.
#[derive(Debug, Clone)]
struct Def<'a> {
    value: String,
    /// The value is already expanded (a script assignment), not a CI variable to expand on use.
    expanded: bool,
    /// Variables the value couldn't read.
    unresolved: Vec<String>,
    at: Option<Origin<'a>>,
    /// A project file the value comes from (`$(basename *.csproj)`).
    source: Option<String>,
    /// Passed down by a trigger job: above every variable the child pipeline's files set.
    passed: bool,
    /// Variables set in one branch of a shell `if` that the value reads (itself, when it is set so).
    branch: Vec<String>,
}

impl<'a> Def<'a> {
    fn yaml(f: &FactRow, s: &'a RepoSummary) -> Def<'a> {
        Def { value: f.value.clone(), expanded: false, unresolved: Vec::new(), at: Some(Origin { repo: &s.root, file: f.file.clone(), line: f.line }), source: None, passed: false, branch: Vec::new() }
    }
}

/// Variables in scope, each with its definitions highest precedence first.
#[derive(Debug, Clone, Default)]
struct Env<'a> {
    vars: HashMap<String, Vec<Def<'a>>>,
}

/// An expanded text.
#[derive(Debug, Default)]
struct Expanded {
    text: String,
    unresolved: Vec<String>,
    /// Where the first variable it reads is set.
    from: Option<String>,
    /// Variables it reads that a shell `if` sets in one branch.
    branch: Vec<String>,
}

impl<'a> Env<'a> {
    fn add_low(&mut self, name: &str, d: Def<'a>) {
        self.vars.entry(name.to_string()).or_default().push(d);
    }

    /// A variable a CI file sets: below passed ones, above those set before it.
    fn put_yaml(&mut self, name: &str, d: Def<'a>) {
        let defs = self.vars.entry(name.to_string()).or_default();
        let at = defs.iter().take_while(|x| x.passed).count();
        defs.insert(at, d);
    }

    fn set(&mut self, name: &str, d: Def<'a>) {
        self.vars.entry(name.to_string()).or_default().insert(0, d);
    }

    /// Whether `[[ -z "$name" ]]` is false: the variable is assigned a value that isn't empty. A value holding
    /// run-time variables (`img:0.0.${CI_PIPELINE_IID}`) counts when it has text of its own besides them; one that is
    /// only variables nothing sets (`$BUILD_VERSION`) may be empty, and a guarded default still applies.
    fn is_set(&self, name: &str, ctx: &Ctx) -> bool {
        let e = self.lookup(name, 0, ctx, 0);
        if e.unresolved.is_empty() {
            return !e.text.is_empty();
        }
        static VARS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let vars = VARS.get_or_init(|| regex::Regex::new(r"\$\{[^}]*\}|\$\([^)]*\)|\$[A-Za-z_][A-Za-z0-9_]*").unwrap());
        !vars.replace_all(&e.text, "").trim().is_empty()
    }

    /// The value of definition `level` of `name` and below, expanded.
    fn lookup(&self, name: &str, level: usize, ctx: &Ctx, depth: usize) -> Expanded {
        let Some(defs) = self.vars.get(name) else {
            return Expanded { text: format!("${name}"), unresolved: vec![name.to_string()], from: None, branch: Vec::new() };
        };
        let Some(d) = defs.get(level) else {
            return Expanded { text: format!("${name}"), unresolved: vec![name.to_string()], from: None, branch: Vec::new() };
        };
        let from = ctx.origin(d);
        if d.expanded || depth > MAX_DEPTH {
            return Expanded { text: d.value.clone(), unresolved: d.unresolved.clone(), from, branch: d.branch.clone() };
        }
        let mut e = self.expand(&d.value, Some((name, level)), ctx, depth + 1);
        e.from = from;
        e.branch.extend(d.branch.iter().cloned());
        e
    }

    /// Expands `$X`, `${X}`, `${X#p}`, `${X##p}`, `${X%p}`, `${X%%p}`, `${X:-d}`, `${X-d}` and `$(basename …)` in `s`.
    /// Inside the value of definition `level` of `own`, `$own` reads the definitions below it.
    fn expand(&self, s: &str, own: Option<(&str, usize)>, ctx: &Ctx, depth: usize) -> Expanded {
        let mut out = Expanded::default();
        let b = s.as_bytes();
        let mut i = 0;
        let get = |name: &str, out: &mut Expanded| -> Expanded {
            let e = match own {
                Some((o, level)) if o == name => self.lookup(name, level + 1, ctx, depth),
                _ => self.lookup(name, 0, ctx, depth),
            };
            if out.from.is_none() {
                out.from.clone_from(&e.from);
            }
            for b in &e.branch {
                if !out.branch.contains(b) {
                    out.branch.push(b.clone());
                }
            }
            e
        };
        while i < b.len() {
            if b[i] != b'$' {
                let next = s[i..].find('$').map_or(s.len(), |n| i + n);
                out.text.push_str(&s[i..next]);
                i = next;
                continue;
            }
            match b.get(i + 1) {
                Some(b'{') => {
                    let Some(close) = s[i..].find('}').map(|c| i + c) else {
                        out.text.push_str(&s[i..]);
                        break;
                    };
                    let inner = &s[i + 2..close];
                    let name_end = inner.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(inner.len());
                    let (name, op) = inner.split_at(name_end);
                    let e = get(name, &mut out);
                    let known = e.unresolved.is_empty();
                    let v = match op {
                        "" => Some(e.text.clone()),
                        _ if op.starts_with(":-") || op.starts_with('-') => {
                            let d = op.trim_start_matches(':').trim_start_matches('-');
                            if known && !e.text.is_empty() { Some(e.text.clone()) } else { Some(self.expand(d, own, ctx, depth + 1).text) }
                        }
                        _ if known && (op.starts_with('#') || op.starts_with('%')) => Some(strip(&e.text, op)),
                        _ => None,
                    };
                    match v {
                        Some(v) if known || op.starts_with(":-") || op.starts_with('-') => out.text.push_str(&v),
                        _ => {
                            out.text.push_str(&s[i..=close]);
                            out.unresolved.extend(e.unresolved);
                        }
                    }
                    i = close + 1;
                }
                Some(b'(') => {
                    let mut j = i + 2;
                    let mut nest = 1;
                    while j < b.len() && nest > 0 {
                        match b[j] {
                            b'(' => nest += 1,
                            b')' => nest -= 1,
                            _ => {}
                        }
                        j += 1;
                    }
                    let inner = s[i + 2..j.saturating_sub(1).max(i + 2)].trim();
                    match self.command(inner, own, ctx, depth) {
                        Some((v, source)) => {
                            out.text.push_str(&v);
                            if out.from.is_none() {
                                out.from = Some(source);
                            }
                        }
                        None => {
                            out.text.push_str(&s[i..j]);
                            out.unresolved.push(format!("$({inner})"));
                        }
                    }
                    i = j;
                }
                Some(c) if c.is_ascii_alphabetic() || *c == b'_' => {
                    let end = s[i + 1..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(s.len(), |e| i + 1 + e);
                    let e = get(&s[i + 1..end], &mut out);
                    out.text.push_str(&e.text);
                    out.unresolved.extend(e.unresolved);
                    i = end;
                }
                _ => {
                    out.text.push('$');
                    i += 1;
                }
            }
        }
        out.unresolved.dedup();
        out
    }

    /// The output of a command substitution this can know: `basename DIR/*.csproj .csproj` (the one project file
    /// there), `basename PATH [SUFFIX]`; with the project file it read.
    fn command(&self, cmd: &str, own: Option<(&str, usize)>, ctx: &Ctx, depth: usize) -> Option<(String, String)> {
        let mut words = cmd.split_whitespace();
        if words.next()? != "basename" {
            return None;
        }
        let pattern = self.expand(words.next()?.trim_matches(['"', '\'']), own, ctx, depth + 1);
        if !pattern.unresolved.is_empty() {
            return None;
        }
        let suffix = words.next().unwrap_or("").trim_matches(['"', '\'']);
        let path = join(&ctx.cwd(), &pattern.text);
        let file = if path.contains('*') {
            let found: Vec<&str> = ctx.project_files.iter().map(String::as_str).filter(|f| glob(&path, f)).collect();
            let [one] = found.as_slice() else { return None };
            one.to_string()
        } else {
            path
        };
        let base = file.rsplit('/').next().unwrap_or(&file);
        Some((base.strip_suffix(suffix).filter(|_| !suffix.is_empty()).unwrap_or(base).to_string(), file.clone()))
    }
}

/// A value as written (`name` is what it is called), expanded with the variables in `env`. When `strict`, a value
/// reading a variable a shell `if` sets in one branch isn't known (which branch runs is decided at run time).
fn passed(env: &Env, ctx: &Ctx, name: &str, written: &str, strict: bool) -> Passed {
    let mut e = env.expand(written, None, ctx, 0);
    if strict {
        e.unresolved.extend(e.branch.iter().map(|b| format!("{b} (set in an if/else branch)")));
    }
    let known = e.unresolved.is_empty();
    Passed {
        name: name.to_string(),
        written: written.to_string(),
        value: known.then(|| e.text.clone()),
        partial: (!known).then(|| e.text.clone()),
        from: e.from,
        unresolved: e.unresolved,
    }
}

/// `${X#p}` and friends on a value: `p` may hold `*` and `?`.
fn strip(v: &str, op: &str) -> String {
    let (longest, from_start, pat) = if let Some(p) = op.strip_prefix("##") {
        (true, true, p)
    } else if let Some(p) = op.strip_prefix('#') {
        (false, true, p)
    } else if let Some(p) = op.strip_prefix("%%") {
        (true, false, p)
    } else {
        (false, false, op.trim_start_matches('%'))
    };
    let m = |s: &str| shell_match(pat.as_bytes(), s.as_bytes());
    let mut cuts: Vec<usize> = (0..=v.len()).filter(|&i| v.is_char_boundary(i)).collect();
    // The shortest match is tried first: from the start, the nearest cut; from the end, the farthest.
    if from_start == longest {
        cuts.reverse();
    }
    let at = cuts.into_iter().find(|&i| if from_start { m(&v[..i]) } else { m(&v[i..]) });
    match at {
        Some(i) if from_start => v[i..].to_string(),
        Some(i) => v[..i].to_string(),
        None => v.to_string(),
    }
}

fn shell_match(p: &[u8], s: &[u8]) -> bool {
    match p {
        [] => s.is_empty(),
        [b'*', rest @ ..] => (0..=s.len()).any(|i| shell_match(rest, &s[i..])),
        [b'?', rest @ ..] => !s.is_empty() && shell_match(rest, &s[1..]),
        [c, rest @ ..] => s.first() == Some(c) && shell_match(rest, &s[1..]),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Rules.

/// A value in a rule condition: a string, nothing (an unset variable or `null`), or unknown.
#[derive(Debug, Clone, PartialEq)]
enum RVal {
    Str(String),
    Null,
    Unknown,
}

/// Three-valued logic: `Some(true)`, `Some(false)`, or `None` when it depends on what isn't known.
type Tri = Option<bool>;

struct RuleParser<'s, 'e, 'a> {
    toks: Vec<&'s str>,
    at: usize,
    env: &'e Env<'a>,
    ctx: &'e Ctx<'e>,
}

fn rule_tokens(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c == b'"' || c == b'\'' || c == b'/' {
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            // Regex flags.
            while c == b'/' && i < b.len() && b[i].is_ascii_alphabetic() {
                i += 1;
            }
        } else if matches!(c, b'(' | b')') {
            i += 1;
        } else if matches!(c, b'=' | b'!' | b'&' | b'|') {
            i += 1;
            while i < b.len() && matches!(b[i], b'=' | b'~' | b'&' | b'|') {
                i += 1;
            }
        } else {
            while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'(' | b')' | b'=' | b'!' | b'&' | b'|') {
                i += 1;
            }
        }
        out.push(&s[start..i.min(b.len())]);
    }
    out
}

impl<'s> RuleParser<'s, '_, '_> {
    fn peek(&self) -> Option<&'s str> {
        self.toks.get(self.at).copied()
    }

    fn or(&mut self) -> Tri {
        let mut v = self.and();
        while self.peek() == Some("||") {
            self.at += 1;
            let r = self.and();
            v = match (v, r) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            };
        }
        v
    }

    fn and(&mut self) -> Tri {
        let mut v = self.cmp();
        while self.peek() == Some("&&") {
            self.at += 1;
            let r = self.cmp();
            v = match (v, r) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            };
        }
        v
    }

    fn cmp(&mut self) -> Tri {
        if self.peek() == Some("(") {
            self.at += 1;
            let v = self.or();
            if self.peek() == Some(")") {
                self.at += 1;
            }
            return v;
        }
        let left = self.value();
        let op = match self.peek() {
            Some(op @ ("==" | "!=" | "=~" | "!~")) => op.to_string(),
            _ => {
                return match left {
                    RVal::Str(s) => Some(!s.is_empty()),
                    RVal::Null => Some(false),
                    RVal::Unknown => None,
                };
            }
        };
        self.at += 1;
        let raw = self.peek().unwrap_or("").to_string();
        let right = if op.ends_with('~') {
            self.at += 1;
            RVal::Str(raw)
        } else {
            self.value()
        };
        match op.as_str() {
            "==" | "!=" => {
                let eq = match (&left, &right) {
                    (RVal::Unknown, _) | (_, RVal::Unknown) => return None,
                    (a, b) => a == b,
                };
                Some(if op == "==" { eq } else { !eq })
            }
            _ => {
                let RVal::Str(s) = left else { return if left == RVal::Null { Some(op == "!~") } else { None } };
                let RVal::Str(re) = right else { return None };
                let body = re.trim_start_matches('/');
                let (pat, flags) = body.rsplit_once('/').unwrap_or((body, ""));
                let pat = if flags.contains('i') { format!("(?i){pat}") } else { pat.to_string() };
                let m = regex::Regex::new(&pat).ok()?.is_match(&s);
                Some(if op == "=~" { m } else { !m })
            }
        }
    }

    fn value(&mut self) -> RVal {
        let Some(t) = self.peek() else { return RVal::Unknown };
        self.at += 1;
        if t == "null" {
            return RVal::Null;
        }
        if let Some(name) = t.strip_prefix('$') {
            let name = name.trim_start_matches('{').trim_end_matches('}');
            if !self.env.vars.contains_key(name) {
                return if self.ctx.unset.contains(&name) { RVal::Null } else { RVal::Unknown };
            }
            let e = self.env.lookup(name, 0, self.ctx, 0);
            return if e.unresolved.is_empty() { RVal::Str(e.text) } else { RVal::Unknown };
        }
        if (t.starts_with('"') && t.ends_with('"') || t.starts_with('\'') && t.ends_with('\'')) && t.len() >= 2 {
            return RVal::Str(t[1..t.len() - 1].to_string());
        }
        // A bare word, as an interpolated input leaves (`'$[[ inputs.run ]] == "true"'` reads `true == "true"`).
        if t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
            return RVal::Str(t.to_string());
        }
        RVal::Unknown
    }
}

/// Whether a rule condition holds.
fn holds(cond: &str, env: &Env, ctx: &Ctx) -> Tri {
    let mut p = RuleParser { toks: rule_tokens(cond), at: 0, env, ctx };
    p.or()
}

/// A condition's negation, as GitLab would write it: `A == B` for `A != B` and the reverse, else `not (…)`.
fn negate(cond: &str) -> String {
    let toks = rule_tokens(cond);
    match toks.as_slice() {
        [a, "!=", b] => format!("{a} == {b}"),
        [a, "==", b] => format!("{a} != {b}"),
        [a, "=~", b] => format!("{a} !~ {b}"),
        [a, "!~", b] => format!("{a} =~ {b}"),
        _ => format!("not ({cond})"),
    }
}

/// Whether rules (`(when, if)` in order) let a job or an include in, and the undecided condition that did.
fn rules_admit(rules: &[(&str, &str)], env: &Env, ctx: &Ctx) -> (bool, Option<String>) {
    for (when, cond) in rules {
        if *when == "reference" {
            return (true, Some(format!("!reference {cond}")));
        }
        let t = if cond.is_empty() { Some(true) } else { holds(cond, env, ctx) };
        match t {
            Some(true) => return (*when != "never", None),
            Some(false) => continue,
            None => return (true, Some(if *when == "never" { negate(cond) } else { cond.to_string() })),
        }
    }
    (false, None)
}

// ---------------------------------------------------------------------------------------------------------------
// Pipelines.

/// What a resolution knows besides variables: the application repo's project files and the current directory.
struct Ctx<'a> {
    app: &'a RepoSummary,
    project_files: Vec<String>,
    cwd: std::cell::RefCell<Vec<String>>,
    /// Predefined variables this models as unset (merge request and tag ones).
    unset: Vec<&'static str>,
    /// A child pipeline (`CI_PIPELINE_SOURCE` is `parent_pipeline` for its includes).
    child: bool,
}

impl Ctx<'_> {
    fn cwd(&self) -> String {
        self.cwd.borrow().last().cloned().unwrap_or_default()
    }

    fn origin(&self, d: &Def) -> Option<String> {
        if let Some(s) = &d.source {
            return Some(s.clone());
        }
        let at = d.at.as_ref()?;
        Some(if at.repo == self.app.root { format!("{}:{}", at.file, at.line) } else { format!("{}:{}:{}", dir_name(at.repo), at.file, at.line) })
    }
}

/// A job's definition in one file.
#[derive(Clone)]
struct JobDef<'a> {
    s: &'a RepoSummary,
    file: &'a str,
    line: u32,
    facts: Vec<&'a FactRow>,
}

impl<'a> JobDef<'a> {
    fn has(&self, kind: &str, name: Option<&str>) -> bool {
        self.facts.iter().any(|f| f.kind == kind && name.is_none_or(|n| f.name == n))
    }

    fn of(&self, kind: &str) -> impl Iterator<Item = &&'a FactRow> {
        let kind = kind.to_string();
        self.facts.iter().filter(move |f| f.kind == kind)
    }
}

/// A pipeline's merged configuration.
struct Pipeline<'a> {
    /// Each job's definitions, in merge order (later ones win key by key).
    jobs: HashMap<&'a str, Vec<JobDef<'a>>>,
    order: Vec<&'a str>,
    globals: Vec<(&'a FactRow, &'a RepoSummary)>,
    /// `default:` script sections, in merge order.
    defaults: Vec<(&'a FactRow, &'a RepoSummary)>,
    /// Each file read (repo, file) and the files including it.
    files: HashMap<(PathBuf, String), Vec<String>>,
    /// The weakest include link to each file.
    links: HashMap<(PathBuf, String), &'static str>,
    /// Include rule conditions on the way to each file that couldn't be decided.
    conds: HashMap<(PathBuf, String), Vec<String>>,
    /// Every way each file is reached (the maps above keep the first): a file two workflows include is read once,
    /// and each job takes the way that brought in its own definition (see [`Pipeline::way`]).
    ways: HashMap<(PathBuf, String), Vec<Way>>,
    /// Each file read, as trails name it (`file`, or `repo:file`).
    names: HashMap<(PathBuf, String), String>,
    /// Files reached through an include whose `ref` isn't the indexed checkout's (any way to them).
    unverified: std::collections::HashSet<(PathBuf, String)>,
}

/// One way a pipeline reaches a file: the files including it, outermost first, the weakest link on the way, and
/// the include conditions that couldn't be decided.
#[derive(Clone)]
struct Way {
    trail: Vec<String>,
    link: &'static str,
    conds: Vec<String>,
}

impl Pipeline<'_> {
    /// The way to `key` for the job defined in `job`: when the file is reached several ways, the one going through
    /// the job's own file, else the one sharing most of the job's own way in; the first otherwise.
    fn way(&self, key: &(PathBuf, String), job: Option<&(PathBuf, String)>) -> Option<&Way> {
        let ways = self.ways.get(key)?;
        let (Some(job), [_, _, ..]) = (job, ways.as_slice()) else { return ways.first() };
        let Some(name) = self.names.get(job) else { return ways.first() };
        let mut own: Vec<&String> = self.ways.get(job).and_then(|w| w.first()).map(|w| w.trail.iter().collect()).unwrap_or_default();
        own.push(name);
        let score = |w: &Way| (w.trail.contains(name), w.trail.iter().zip(&own).take_while(|(a, b)| a == *b).count());
        let mut best = &ways[0];
        for w in &ways[1..] {
            if score(w) > score(best) {
                best = w;
            }
        }
        Some(best)
    }
}

/// A step of a job's script, with where it is written.
#[derive(Clone)]
struct Step<'a> {
    op: &'a str,
    s: &'a RepoSummary,
    file: &'a str,
    line: u32,
    /// The template supplying it (the job itself when its own).
    from_job: &'a str,
    /// The jobs whose `!reference`s spliced it in, outermost first.
    through: Vec<&'a str>,
}

/// The workspace's CI pipelines. Build once per answer.
pub struct Ci<'a> {
    repos: Vec<&'a RepoSummary>,
    /// Repos by origin path (`group/project`, lower case), and by directory name (lower case).
    by_path: HashMap<String, Vec<&'a RepoSummary>>,
    by_name: HashMap<String, Vec<&'a RepoSummary>>,
    by_origin: HashMap<String, &'a RepoSummary>,
    /// Each repo's CI facts by file.
    facts: HashMap<(&'a Path, &'a str), Vec<&'a FactRow>>,
    ansible: Ansible<'a>,
    /// Links Terraform module sources to repos (the same reading as `context` on a module call).
    terraform: crate::terraform::Terraform<'a>,
    /// GitLab's built-in templates this knows, as a repo of their own (see [`builtin`]).
    builtin: &'a RepoSummary,
    /// The built-in templates the pipeline being resolved includes (see [`Pipelines::templates`]).
    templates: std::cell::RefCell<Vec<String>>,
    /// What a limit left out of the pipeline being resolved (see [`Pipelines::cut`]).
    cut: std::cell::RefCell<Vec<String>>,
    /// Repos left out because their checkout is gone (see [`crate::workspace::gone`]): a project, clone or directory
    /// naming one is unread, and the repo noted as missing.
    gone: Vec<&'a RepoSummary>,
    /// The includes of the pipeline being resolved (see [`Pipelines::includes`]).
    includes: std::cell::RefCell<Vec<Include>>,
}

/// The deploy chains of a repo's pipeline, with what couldn't be read.
#[derive(Debug, Default)]
pub struct Pipelines {
    pub chains: Vec<Chain>,
    /// Includes and child pipelines that couldn't be read (another project's file not indexed, a template this
    /// doesn't know, a path holding a variable nothing sets).
    pub unread: Vec<String>,
    /// GitLab built-in templates included (not fetched; this knows their jobs), with the jobs each provides.
    pub templates: Vec<String>,
    /// What a limit left out: includes past [`MAX_FILES`] files or [`MAX_DEPTH`] levels, child pipelines past
    /// [`MAX_CHILDREN`], matrix combinations past [`MAX_MATRIX`], roles past [`MAX_ROLES`].
    pub cut: Vec<String>,
    /// Every `include:` entry of every file the pipeline read, in the order read, and what came of it: followed (with
    /// the files, jobs and deploy steps it brings), or why not. None is left out.
    pub includes: Vec<Include>,
}

/// The most jobs an [`Include`] lists.
const MAX_INCLUDE_JOBS: usize = 20;

/// One `include:` entry of a file a pipeline read, and what came of it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Include {
    /// Where it is written: `file:line` (`repo:file:line` outside the application repo).
    pub at: String,
    /// `local`, `project`, `remote`, `template`, `component` or `artifact`.
    pub kind: String,
    /// What it names, as written: the project path, the URL, the template's name, the component (without its
    /// `@ref`), or the local path.
    pub target: String,
    /// A `project` include's file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The revision it asks for: a `project` include's `ref:`, a component's `@ref`.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// `followed`; `missing` (the repo holding it isn't indexed: the answer is `missing_repo` for it); `unread` (a
    /// path holding a variable nothing sets, a file not there); `unsupported` (a GitLab built-in template this doesn't
    /// know, a job's artifact); `skipped` (its `rules` leave it out of a default-branch push pipeline); `cut` (past
    /// a limit).
    pub status: &'static str,
    /// The repo it was read from, when not the application's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The files read through it (`file`, or `repo:file`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// The jobs those files define (hidden templates left out), with their stage: `deploy_app (deploy)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<String>,
    /// Jobs past [`MAX_INCLUDE_JOBS`], when there are.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jobs_total: Option<usize>,
    /// The deploy steps the answer's chains run from those files, with the job: `helm upgrade (deploy_app)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deploys: Vec<String>,
    /// Why it wasn't followed; for one followed, that its files were already read (a cycle, or a file two
    /// includes name) or that it is a GitLab built-in template (known here, not fetched).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The files read through it (repo root, file).
    #[serde(skip)]
    pub targets: Vec<(PathBuf, String)>,
}

impl Include {
    /// What the include names, in words: `project platform/ci, file /deploy.yml, ref main`.
    pub fn named(&self) -> String {
        let mut s = format!("{} {}", self.kind, self.target);
        if let Some(f) = &self.file {
            s.push_str(&format!(", file {f}"));
        }
        if let Some(r) = &self.git_ref {
            s.push_str(&format!(", ref {r}"));
        }
        s
    }
}

/// Where GitLab's built-in templates are placed: not a checkout.
pub const BUILTIN_ROOT: &str = "/gitlab-builtin-templates";

/// The jobs of the GitLab built-in templates this knows, as the templates define them (GitLab 17), reduced to what a
/// pipeline is resolved from. `gitlab-terraform` runs `terraform init` itself before `plan` and `apply`.
const TEMPLATES: &[(&str, &str)] = &[
    ("Terraform/Base.latest.gitlab-ci.yml", TERRAFORM_BASE),
    ("Terraform/Base.gitlab-ci.yml", TERRAFORM_BASE),
    ("Terraform.latest.gitlab-ci.yml", "include:\n  - template: Terraform/Base.latest.gitlab-ci.yml\n"),
    ("Terraform.gitlab-ci.yml", "include:\n  - template: Terraform/Base.gitlab-ci.yml\n"),
];

const TERRAFORM_BASE: &str = "variables:
  TF_ROOT: ${CI_PROJECT_DIR}
  TF_STATE_NAME: default
.terraform:fmt:
  stage: validate
  script:
    - gitlab-terraform fmt
.terraform:validate:
  stage: validate
  script:
    - gitlab-terraform validate
.terraform:build:
  stage: build
  script:
    - gitlab-terraform plan
    - gitlab-terraform plan-json
.terraform:deploy:
  stage: deploy
  script:
    - gitlab-terraform apply
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && $TF_AUTO_DEPLOY == \"true\"
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH
      when: manual
.terraform:destroy:
  stage: cleanup
  script:
    - gitlab-terraform destroy
  when: manual
";

/// The jobs `Terraform.latest.gitlab-ci.yml` and `Terraform.gitlab-ci.yml` add over their base.
const TERRAFORM_JOBS: &str = "fmt:\n  extends: .terraform:fmt\nvalidate:\n  extends: .terraform:validate\nbuild:\n  extends: .terraform:build\ndeploy:\n  extends: .terraform:deploy\ncleanup:\n  extends: .terraform:destroy\n";

/// GitLab's built-in templates this knows, as one repo at [`BUILTIN_ROOT`]: their files, jobs and facts, every
/// line 0 (the files aren't read).
pub fn builtin() -> &'static RepoSummary {
    static B: std::sync::OnceLock<RepoSummary> = std::sync::OnceLock::new();
    B.get_or_init(|| {
        let mut files = Vec::new();
        let mut jobs = Vec::new();
        let mut facts = Vec::new();
        for (name, text) in TEMPLATES {
            let text = if name.starts_with("Terraform/") { text.to_string() } else { format!("{text}{TERRAFORM_JOBS}") };
            let f = kc_extract::gitlabci::extract(name, &text);
            files.push(name.to_string());
            for s in &f.symbols {
                jobs.push(serde_json::json!({"name": s.name, "file": name, "line": 0}));
            }
            for a in &f.ansible {
                facts.push(serde_json::json!({"kind": a.kind, "name": a.name, "value": a.value, "file": name, "line": 0, "symbol": a.sym.map(|i| f.symbols[i].name.as_str())}));
            }
        }
        serde_json::from_value(serde_json::json!({
            "root": BUILTIN_ROOT, "packages": [], "modules": [], "ci": {"files": files, "jobs": jobs, "facts": facts}, "stamp": "",
        }))
        .expect("built-in templates summary")
    })
}

/// An include as written at `at` (`file:line`), of kind `how`, its outcome not yet known.
fn include_entry(at: &str, how: &str, spec: &serde_json::Value) -> Include {
    let get = |k: &str| spec.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let (target, file, git_ref) = match how {
        "project" => (get("project").unwrap_or_default(), get("file"), get("ref")),
        "component" => {
            let raw = get("file").unwrap_or_default();
            match raw.rsplit_once('@') {
                Some((path, r)) => (path.to_string(), None, Some(r.to_string())),
                None => (raw, None, None),
            }
        }
        _ => (get("file").unwrap_or_default(), None, None),
    };
    Include {
        at: at.to_string(),
        kind: how.to_string(),
        target,
        file,
        git_ref,
        status: "unread",
        repo: None,
        files: Vec::new(),
        jobs: Vec::new(),
        jobs_total: None,
        deploys: Vec::new(),
        why: None,
        targets: Vec::new(),
    }
}

impl<'a> Ci<'a> {
    /// Records what a limit left out, once.
    fn note_cut(&self, why: String) {
        let mut cut = self.cut.borrow_mut();
        if !cut.contains(&why) {
            cut.push(why);
        }
    }

    /// Records an include of the pipeline being resolved, once (a file read for two pipelines lists its includes once).
    fn note_include(&self, e: Include) {
        let mut all = self.includes.borrow_mut();
        if !all.iter().any(|x| x.at == e.at && x.kind == e.kind && x.target == e.target && x.file == e.file) {
            all.push(e);
        }
    }

    /// Fills in an include whose targets are `ts`, read `depth` includes deep: followed, unless nothing matched or a
    /// limit stops it; with the repo and files it reads.
    fn followed(&self, e: &mut Include, p: &Pipeline<'a>, ts: &[(&'a RepoSummary, String, &'static str)], depth: usize, ctx: &Ctx) {
        let seen = !ts.is_empty() && ts.iter().all(|(r, t, _)| p.files.contains_key(&(r.root.clone(), t.clone())));
        let over = !seen && (p.files.len() >= MAX_FILES || depth > MAX_DEPTH);
        (e.status, e.why) = if ts.is_empty() {
            ("unread", Some("no CI file matches it".into()))
        } else if over {
            ("cut", Some(format!("past {MAX_FILES} files or {MAX_DEPTH} levels deep")))
        } else if seen {
            ("followed", Some("its files were already read (an include cycle, or files two includes name)".into()))
        } else if ts.iter().all(|(r, ..)| std::ptr::eq(*r, self.builtin)) {
            ("followed", Some("a GitLab built-in template: known here, not fetched".into()))
        } else {
            ("followed", None)
        };
        if let Some((r, ..)) = ts.first().filter(|(r, ..)| r.root != ctx.app.root && !std::ptr::eq(*r, self.builtin)) {
            e.repo = Some(dir_name(&r.root));
        }
        for (r, t, _) in ts {
            e.files.push(if r.root == ctx.app.root { t.clone() } else { format!("{}:{t}", dir_name(&r.root)) });
            e.targets.push((r.root.clone(), t.clone()));
        }
    }

    /// The jobs each followed include's files define (with their stage, as the pipeline `p` merged them, else as
    /// written) and the deploy steps `chains` run from them.
    fn describe_includes(&self, includes: &mut [Include], p: &Pipeline<'a>, chains: &[Chain]) {
        // An include of files already read brings nothing new: the include that read them lists their jobs.
        for e in includes.iter_mut().filter(|e| e.status == "followed" && !e.why.as_deref().is_some_and(|w| w.starts_with("its files were already read"))) {
            let mut jobs: Vec<String> = Vec::new();
            for (root, file) in &e.targets {
                for name in p.order.iter().filter(|n| !n.starts_with('.')) {
                    let Some(defs) = p.jobs.get(name) else { continue };
                    if !defs.iter().any(|d| d.s.root == *root && d.file == file.as_str()) {
                        continue;
                    }
                    let stage = self.supplier(p, name, "ci_stage", None, 0).and_then(|(d, _)| d.of("ci_stage").last().map(|f| f.name.clone()));
                    let j = format!("{name} ({})", stage.as_deref().unwrap_or("test"));
                    if !jobs.contains(&j) {
                        jobs.push(j);
                    }
                }
                for c in chains.iter().filter(|c| c.run.repo == *root && c.run.file == *file) {
                    let job = c.hops.last().map_or("", |h| h.name.as_str());
                    let d = format!("{} ({job})", c.run.name);
                    if !e.deploys.contains(&d) {
                        e.deploys.push(d);
                    }
                }
            }
            if jobs.len() > MAX_INCLUDE_JOBS {
                e.jobs_total = Some(jobs.len());
                jobs.truncate(MAX_INCLUDE_JOBS);
            }
            e.jobs = jobs;
        }
    }

    pub fn new(summaries: &'a [RepoSummary]) -> Ci<'a> {
        let repos = crate::http::distinct(summaries);
        let mut by_path: HashMap<String, Vec<&RepoSummary>> = HashMap::new();
        let mut by_name: HashMap<String, Vec<&RepoSummary>> = HashMap::new();
        let mut by_origin = HashMap::new();
        let mut facts: HashMap<(&Path, &str), Vec<&FactRow>> = HashMap::new();
        for s in &repos {
            if let Some(o) = &s.origin {
                by_origin.insert(o.clone(), *s);
                if let Some((_, path)) = o.split_once('/') {
                    by_path.entry(path.to_string()).or_default().push(*s);
                }
            }
            by_name.entry(dir_name(&s.root).to_lowercase()).or_default().push(*s);
            for f in &s.ci.facts {
                facts.entry((s.root.as_path(), f.file.as_str())).or_default().push(f);
            }
        }
        let builtin = builtin();
        for f in &builtin.ci.facts {
            facts.entry((builtin.root.as_path(), f.file.as_str())).or_default().push(f);
        }
        Ci {
            repos: repos.clone(),
            by_path,
            by_name,
            by_origin,
            facts,
            ansible: Ansible::new(summaries),
            terraform: crate::terraform::Terraform::new(summaries),
            builtin,
            templates: Default::default(),
            cut: Default::default(),
            gone: crate::workspace::gone(summaries),
            includes: Default::default(),
        }
    }

    /// The repo a Terraform module source names, as [`crate::terraform::Terraform::link`] links it: a git origin is
    /// `exact`, a registry address `unique_name` (`ambiguous` for several repos), anything unlinked `external_likely`.
    /// `None` for a local path.
    fn module_repo(&self, source: &str) -> Option<(Option<PathBuf>, &'static str)> {
        if matches!(crate::terraform::parse(source), crate::terraform::Source::Local(_)) {
            return None;
        }
        match self.terraform.link(source).as_slice() {
            [] => Some((None, "external_likely")),
            [one] => Some((Some(one.repo.clone()), one.confidence)),
            _ => Some((None, "ambiguous")),
        }
    }

    /// The repo a project path (`group/project`) names, and how sure that is.
    fn project(&self, path: &str) -> Option<(&'a RepoSummary, &'static str)> {
        let p = path.trim().trim_matches('/').trim_end_matches(".git").to_lowercase();
        if let Some([one]) = self.by_path.get(&p).map(Vec::as_slice) {
            return Some((one, "exact"));
        }
        let last = p.rsplit('/').next().unwrap_or(&p);
        // Not named by origin: a gone repo of that path or name would have been a candidate.
        crate::workspace::note_missing_if(self.gone.iter().copied(), |g| {
            g.origin.as_deref().and_then(|o| o.split_once('/')).is_some_and(|(_, gp)| gp == p) || dir_name(&g.root).to_lowercase() == last
        });
        match self.by_name.get(last).map(Vec::as_slice) {
            Some([one]) => Some((one, "unique_name")),
            _ => None,
        }
    }

    /// Whether a repo whose checkout is gone answers to the project path `path` (by origin, or by its last segment):
    /// the include naming it is then reported through the workspace's missing repos (see
    /// [`crate::workspace::note_missing`], which [`Ci::project`] has noted), not as a repo never indexed.
    fn gone_project(&self, path: &str) -> bool {
        let p = path.trim().trim_matches('/').trim_end_matches(".git").to_lowercase();
        let last = p.rsplit('/').next().unwrap_or(&p).to_string();
        self.gone.iter().any(|g| g.origin.as_deref().and_then(|o| o.split_once('/')).is_some_and(|(_, gp)| gp == p) || dir_name(&g.root).to_lowercase() == last)
    }

    /// Why an include of a project no checkout answers to isn't read: its checkout is gone (reported as missing
    /// with the repo), or it was never indexed.
    fn absent(&self, project: &str) -> &'static str {
        if self.gone_project(project) { "(checkout gone: missing_repo names it)" } else { "(not indexed)" }
    }

    /// The files an include names, in the repo it names, with the link's confidence; or why none.
    fn targets(&self, from: &'a RepoSummary, how: &str, spec: &serde_json::Value, env: &Env, ctx: &Ctx) -> Result<Vec<(&'a RepoSummary, String, &'static str)>, String> {
        let get = |k: &str| spec.get(k).and_then(|v| v.as_str()).map(|v| env.expand(v, None, ctx, 0));
        let file = get("file");
        // A component's `@ref` is often a run-time value (`$CI_COMMIT_SHA`); its path is read below.
        if how != "component" && file.as_ref().is_some_and(|f| !f.unresolved.is_empty()) {
            return Err(format!("{how} {}", spec.get("file").and_then(|v| v.as_str()).unwrap_or("")));
        }
        let file = file.map(|f| f.text.trim_start_matches('/').to_string());
        let (repo, file, link) = match how {
            "local" => (from, file.unwrap_or_default(), "exact"),
            "project" => {
                let p = get("project").ok_or("project")?;
                // Named with its file and ref, so the answer says exactly what is missing.
                let named = || {
                    let mut s = format!("project {}", spec.get("project").and_then(|v| v.as_str()).unwrap_or(""));
                    if let Some(f) = spec.get("file").and_then(|v| v.as_str()) {
                        s.push_str(&format!(", file {f}"));
                    }
                    if let Some(r) = spec.get("ref").and_then(|v| v.as_str()) {
                        s.push_str(&format!(", ref {r}"));
                    }
                    s
                };
                if !p.unresolved.is_empty() {
                    return Err(format!("{} (its path holds a variable nothing sets: {})", named(), p.unresolved.join(", ")));
                }
                let (repo, link) = self.project(&p.text).ok_or_else(|| format!("{} {}", named(), self.absent(&p.text)))?;
                (repo, file.ok_or("project without file")?, link)
            }
            "remote" => {
                let url = file.unwrap_or_default();
                let (project, file_path) =
                    url.split_once("/-/raw/").or_else(|| url.split_once("/raw/")).ok_or_else(|| format!("remote {url} (not indexed: not a project's raw file URL)"))?;
                let project = project.split_once("://").map_or(project, |(_, r)| r);
                let project = project.split_once('/').map_or("", |(_, p)| p);
                let file_path = file_path.split_once('/').map_or("", |(_, p)| p);
                let (repo, link) = self.project(project).ok_or_else(|| format!("remote {url} {}", self.absent(project)))?;
                (repo, file_path.to_string(), link)
            }
            // `$CI_SERVER_FQDN/group/project/name@ref`: `templates/name.yml` or `templates/name/template.yml` of the
            // repo whose origin is that project.
            "component" => {
                let raw = spec.get("file").and_then(|v| v.as_str()).unwrap_or("");
                let path = raw.rsplit_once('@').map_or(raw, |(p, _)| p);
                let path = env.expand(path, None, ctx, 0);
                let unread = || format!("component {raw} (not indexed: external_likely)");
                if !path.unresolved.is_empty() {
                    return Err(format!("component {raw} (its path holds a variable nothing sets: {})", path.unresolved.join(", ")));
                }
                // The server's host first.
                let path = path.text.split_once('/').map_or("", |(_, p)| p).trim_matches('/');
                let (project, name) = path.rsplit_once('/').ok_or_else(unread)?;
                let (repo, link) = match self.project(project) {
                    Some(found) => found,
                    None if self.gone_project(project) => return Err(format!("component {raw} {}", self.absent(project))),
                    None => return Err(unread()),
                };
                let file = [format!("templates/{name}.yml"), format!("templates/{name}/template.yml")].into_iter().find(|f| repo.ci.files.contains(f)).ok_or_else(unread)?;
                return Ok(vec![(repo, file, link)]);
            }
            // GitLab's own templates: those this knows, never fetched.
            "template" if file.as_ref().is_some_and(|f| self.builtin.ci.files.contains(f)) => (self.builtin, file.unwrap_or_default(), "external_likely"),
            "template" => return Err(format!("template {} (unsupported: a GitLab built-in template this doesn't know; its jobs aren't read)", file.unwrap_or_default())),
            "artifact" => return Err(format!("artifact {} (unsupported: a file a job generates, in no repo)", file.unwrap_or_default())),
            "unknown" => return Err(format!("include {} (unsupported: a form of include this doesn't read)", file.unwrap_or_default())),
            other => return Err(format!("{other} {}", file.unwrap_or_default())),
        };
        if file.contains('*') {
            let hits: Vec<_> = repo.ci.files.iter().filter(|f| glob(&file, f)).map(|f| (repo, f.clone(), link)).collect();
            return Ok(hits);
        }
        if !repo.ci.files.contains(&file) {
            return Err(format!("{}:{file} (not a CI file there)", dir_name(&repo.root)));
        }
        Ok(vec![(repo, file, link)])
    }

    /// Reads `file` of `s` into `p`: its includes first, then its own content.
    #[allow(clippy::too_many_arguments)]
    fn load(
        &self,
        p: &mut Pipeline<'a>,
        s: &'a RepoSummary,
        file: &str,
        inputs: Option<&serde_json::Value>,
        trail: Vec<String>,
        link: &'static str,
        conds: Vec<String>,
        env: &mut Env<'a>,
        ctx: &Ctx,
        missing: &mut Vec<String>,
    ) {
        let Some(base) = self.facts.get(&(s.root.as_path(), file)) else {
            p.files.entry((s.root.clone(), file.to_string())).or_insert(trail);
            return;
        };
        // A file with `spec: inputs:` (a component) is read with `$[[ inputs.x ]]` replaced: its defaults, under the
        // include's `inputs:`. Each set of inputs is a file of its own.
        let instance = (inputs.is_some() || base.iter().any(|f| f.kind == "ci_input")).then(|| instance(s, file, base, inputs));
        let key = match &instance {
            Some(i) => (s.root.clone(), format!("{file}\u{1}{}", i.key)),
            None => (s.root.clone(), file.to_string()),
        };
        let way = Way { trail: trail.clone(), link, conds: conds.clone() };
        let here = if s.root == ctx.app.root { file.to_string() } else { format!("{}:{file}", dir_name(&s.root)) };
        for k in [key.clone(), (s.root.clone(), file.to_string())] {
            let ways = p.ways.entry(k.clone()).or_default();
            if !ways.iter().any(|w| w.trail == way.trail) {
                ways.push(way.clone());
            }
            p.names.entry(k).or_insert_with(|| here.clone());
        }
        if p.files.contains_key(&key) {
            return;
        }
        if p.files.len() >= MAX_FILES || trail.len() > MAX_DEPTH {
            self.note_cut(format!("CI includes past {MAX_FILES} files or {MAX_DEPTH} levels deep not read ({file} in {})", s.root.display()));
            return;
        }
        for k in [key.clone(), (s.root.clone(), file.to_string())] {
            p.files.entry(k.clone()).or_insert_with(|| trail.clone());
            p.links.entry(k.clone()).or_insert(link);
            if !conds.is_empty() {
                p.conds.entry(k).or_insert_with(|| conds.clone());
            }
        }
        let facts: Vec<&'a FactRow> = match instance {
            Some(i) => i.facts.iter().collect(),
            None => base.clone(),
        };
        let jobs: Vec<&'a kc_store::ci::JobRow> = match instance {
            Some(i) => i.jobs.iter().collect(),
            None => s.ci.jobs.iter().filter(|j| j.file == file).collect(),
        };
        if std::ptr::eq(s, self.builtin) {
            let t = self.template_jobs(file);
            let mut seen = self.templates.borrow_mut();
            if !seen.contains(&t) {
                seen.push(t);
            }
        }
        let mut next = trail.clone();
        next.push(here.clone());
        // The includes of GitLab's own templates are GitLab's: not the pipeline's to account for.
        let builtin = std::ptr::eq(s, self.builtin);
        for f in facts.iter().filter(|f| f.kind == "ci_include") {
            let spec: serde_json::Value = serde_json::from_str(&f.value).unwrap_or_default();
            let mut entry = (!builtin).then(|| include_entry(&format!("{here}:{}", f.line), &f.name, &spec));
            let rules: Vec<(&str, &str)> = spec
                .get("rules")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|x| x.as_str()).filter_map(|x| x.split_once(':')).map(|(w, c)| (if w == "if" { "" } else { w }, c)).collect())
                .unwrap_or_default();
            let rules: Vec<(&str, &str)> = rules.into_iter().map(|(w, c)| if w == "exists" { ("reference", c) } else { (w, c) }).collect();
            let (admitted, cond) = if rules.is_empty() { (true, None) } else { rules_admit(&rules, env, ctx) };
            if !admitted {
                if let Some(mut e) = entry {
                    e.status = "skipped";
                    e.why = Some("its rules leave it out of a pipeline for a push to the default branch".into());
                    self.note_include(e);
                }
                continue;
            }
            // An include whose `if:` can't be decided may or may not be in the pipeline: what comes through it is
            // ambiguous, under its condition. (`exists:` counts as matching, as before.)
            let mut conds = conds.clone();
            let mut link = link;
            if let Some(c) = cond.filter(|c| !c.starts_with("!reference")) {
                conds.push(c);
                link = weakest(link, "ambiguous");
            }
            // The revision the include asks for (`ref:`, a component's `@ref`): the file read is the checkout's, which
            // is that revision only when it is at it.
            let parent_unverified = p.unverified.contains(&(s.root.clone(), file.to_string()));
            let rev = include_rev(&f.name, &spec, env, ctx);
            match self.targets(s, &f.name, &spec, env, ctx) {
                Ok(ts) => {
                    if let Some(mut e) = entry.take() {
                        self.followed(&mut e, p, &ts, next.len(), ctx);
                        self.note_include(e);
                    }
                    for (repo, t, l) in ts {
                        let unverified = parent_unverified
                            || match &rev {
                                None => false,
                                Some(Some(r)) => crate::revision::at_head(&repo.root, r) != Some(true),
                                Some(None) => true,
                            };
                        let l = if unverified {
                            p.unverified.insert((repo.root.clone(), t.clone()));
                            weakest(l, "unique_name")
                        } else {
                            l
                        };
                        self.load(p, repo, &t, spec.get("inputs"), next.clone(), weakest(link, l), conds.clone(), env, ctx, missing);
                    }
                }
                Err(why) => {
                    if let Some(mut e) = entry {
                        e.status = if why.contains("(not indexed") || why.contains("(checkout gone") {
                            "missing"
                        } else if why.contains("(unsupported:") {
                            "unsupported"
                        } else {
                            "unread"
                        };
                        e.why = Some(why.clone());
                        self.note_include(e);
                    }
                    if !missing.contains(&why) {
                        missing.push(why);
                    }
                }
            }
        }
        for f in facts.iter().filter(|f| f.symbol.is_none()) {
            match f.kind.as_str() {
                "ci_var" => {
                    p.globals.push((f, s));
                    env.put_yaml(&f.name, Def::yaml(f, s));
                }
                "ci_script" => p.defaults.push((f, s)),
                _ => {}
            }
        }
        for j in jobs {
            let facts: Vec<&FactRow> = facts.iter().copied().filter(|f| f.symbol.as_deref() == Some(j.name.as_str())).collect();
            let defs = p.jobs.entry(j.name.as_str()).or_default();
            if defs.is_empty() {
                p.order.push(j.name.as_str());
            }
            defs.push(JobDef { s, file: j.file.as_str(), line: j.line, facts });
        }
    }

    /// The pipeline starting at `file` of `s`, with `passed` variables (a child pipeline's), for the application `ctx`.
    fn pipeline(&self, files: &[(&'a RepoSummary, String, &'static str, Option<serde_json::Value>)], env: &mut Env<'a>, ctx: &Ctx, missing: &mut Vec<String>) -> Pipeline<'a> {
        let mut p = Pipeline {
            jobs: HashMap::new(),
            order: Vec::new(),
            globals: Vec::new(),
            defaults: Vec::new(),
            files: HashMap::new(),
            links: HashMap::new(),
            conds: HashMap::new(),
            ways: HashMap::new(),
            names: HashMap::new(),
            unverified: Default::default(),
        };
        // Include rules read the pipeline's predefined variables; jobs get their own (see `run`).
        predefined(env, ctx, None, None, if ctx.child { "parent_pipeline" } else { "push" });
        for (repo, f, link, inputs) in files {
            self.load(&mut p, repo, f, inputs.as_ref(), Vec::new(), link, Vec::new(), env, ctx, missing);
        }
        for defs in env.vars.values_mut() {
            defs.retain(|d| d.at.is_some() || d.source.is_some() || d.passed);
        }
        env.vars.retain(|_, defs| !defs.is_empty());
        p
    }

    /// A built-in template and the jobs it provides, with what each runs: `Terraform/Base.latest.gitlab-ci.yml
    /// (GitLab built-in, not fetched): .terraform:validate validate, .terraform:build plan, …`.
    fn template_jobs(&self, file: &str) -> String {
        let b = self.builtin;
        let jobs: Vec<String> = b
            .ci
            .jobs
            .iter()
            .filter(|j| j.file == file)
            .map(|j| {
                let mine = b.ci.facts.iter().filter(|f| f.file == file && f.symbol.as_deref() == Some(j.name.as_str()));
                let runs: Vec<&str> = mine.clone().filter(|f| f.kind == "ci_script").filter_map(|f| f.value.strip_prefix("tool\t")).filter_map(|v| v.split('\t').nth(1)).collect();
                let extends: Vec<&str> = mine.filter(|f| f.kind == "ci_extends").map(|f| f.name.as_str()).collect();
                match (runs.is_empty(), extends.is_empty()) {
                    (false, _) => format!("{} {}", j.name, runs.join("+")),
                    (true, false) => format!("{} (extends {})", j.name, extends.join(", ")),
                    (true, true) => j.name.clone(),
                }
            })
            .collect();
        format!("{file} (GitLab built-in, not fetched): {}", jobs.join(", "))
    }

    /// The definition of `job` supplying `kind` facts (with `name`, when given): its own last definition holding
    /// them, else the templates it extends, later ones first. With the job or template it comes from.
    fn supplier<'p>(&self, p: &'p Pipeline<'a>, job: &'a str, kind: &str, name: Option<&str>, depth: usize) -> Option<(&'p JobDef<'a>, &'a str)> {
        let defs = p.jobs.get(job)?;
        if let Some(d) = defs.iter().rev().find(|d| d.has(kind, name)) {
            return Some((d, job));
        }
        if depth >= MAX_DEPTH {
            return None;
        }
        for parent in self.extends(p, job).into_iter().rev() {
            if let Some(found) = self.supplier(p, parent, kind, name, depth + 1) {
                return Some(found);
            }
        }
        None
    }

    /// The templates a job extends, from its last definition naming any.
    fn extends<'p>(&self, p: &'p Pipeline<'a>, job: &str) -> Vec<&'a str> {
        let Some(defs) = p.jobs.get(job) else { return Vec::new() };
        let Some(d) = defs.iter().rev().find(|d| d.has("ci_extends", None)) else { return Vec::new() };
        d.of("ci_extends").map(|f| f.name.as_str()).collect()
    }

    /// Every job and template `job` is made of.
    fn closure(&self, p: &Pipeline<'a>, job: &'a str, out: &mut Vec<(PathBuf, String, String)>, depth: usize) {
        let Some(defs) = p.jobs.get(job) else { return };
        for d in defs {
            let k = (d.s.root.clone(), d.file.to_string(), job.to_string());
            if !out.contains(&k) {
                out.push(k);
            }
        }
        if depth < MAX_DEPTH {
            for e in self.extends(p, job) {
                self.closure(p, e, out, depth + 1);
            }
        }
    }

    /// A job's variables, lowest precedence first: its templates' (in `extends` order), then its own definitions'.
    fn job_vars(&self, p: &Pipeline<'a>, job: &'a str, depth: usize, out: &mut Vec<(&'a FactRow, &'a RepoSummary)>) {
        if depth < MAX_DEPTH {
            for e in self.extends(p, job) {
                self.job_vars(p, e, depth + 1, out);
            }
        }
        for d in p.jobs.get(job).into_iter().flatten() {
            out.extend(d.of("ci_var").map(|f| (*f, d.s)));
        }
    }

    /// A section's steps: the job's, its templates', or the default; `!reference`s spliced in.
    fn steps(&self, p: &Pipeline<'a>, job: &'a str, section: &str, depth: usize) -> Vec<Step<'a>> {
        let (facts, s, from_job): (Vec<&'a FactRow>, &'a RepoSummary, &'a str) = match self.supplier(p, job, "ci_script", Some(section), 0) {
            Some((d, from)) => (d.facts.iter().copied().filter(|f| f.kind == "ci_script" && f.name == section).collect(), d.s, from),
            None => match p.defaults.iter().rev().find(|(f, _)| f.name == section) {
                Some((f0, s)) => {
                    let file = f0.file.as_str();
                    (p.defaults.iter().filter(|(f, r)| f.name == section && f.file == file && r.root == s.root).map(|(f, _)| *f).collect(), *s, "default")
                }
                None => return Vec::new(),
            },
        };
        let mut out = Vec::new();
        for f in facts {
            if f.value.is_empty() {
                continue;
            }
            if let Some(rest) = f.value.strip_prefix("reference\t") {
                let (rjob, rkey) = rest.split_once('\t').unwrap_or((rest, ""));
                if depth < MAX_DEPTH
                    && let Some((name, _)) = p.jobs.get_key_value(rjob)
                {
                    out.extend(self.steps(p, name, rkey, depth + 1).into_iter().map(|mut st| {
                        st.through.insert(0, from_job);
                        st
                    }));
                }
                continue;
            }
            out.push(Step { op: f.value.as_str(), s, file: f.file.as_str(), line: f.line, from_job, through: Vec::new() });
        }
        out
    }

    /// A job's rules, from its own definitions or its templates'; a `!reference` to a rule kept under a job's key
    /// is read as that rule.
    fn rules(&self, p: &Pipeline<'a>, job: &'a str) -> Vec<(&'a str, &'a str)> {
        let Some((d, _)) = self.supplier(p, job, "ci_rules", None, 0) else { return Vec::new() };
        d.of("ci_rule")
            .map(|f| {
                if f.name == "reference"
                    && let Some((rjob, key)) = f.value.split_once(' ')
                    && let Some(def) = p.jobs.get(rjob).into_iter().flatten().rev().flat_map(|d| d.of("ci_ruledef")).find(|r| r.name == key)
                {
                    let (when, cond) = def.value.split_once('\t').unwrap_or(("", ""));
                    return (when, cond);
                }
                (f.name.as_str(), f.value.as_str())
            })
            .collect()
    }

    /// Whether a job's legacy `only:`/`except:` refs let it run on a push to the default branch (`main` or
    /// `master`).
    fn refs_admit(&self, p: &Pipeline<'a>, job: &'a str) -> bool {
        let refs = |kind: &str| -> Vec<&'a str> {
            self.supplier(p, job, kind, None, 0).map(|(d, _)| d.of(kind).map(|f| f.name.as_str()).collect()).unwrap_or_default()
        };
        let matches = |pat: &str| -> bool {
            match pat {
                "branches" | "pushes" => true,
                "tags" | "merge_requests" | "schedules" | "api" | "web" | "triggers" | "pipelines" | "external" | "external_pull_requests" | "chat" => false,
                _ if pat.starts_with('/') => {
                    let body = pat.trim_start_matches('/');
                    let (re, flags) = body.rsplit_once('/').unwrap_or((body, ""));
                    let re = if flags.contains('i') { format!("(?i){re}") } else { re.to_string() };
                    regex::Regex::new(&re).is_ok_and(|r| r.is_match("main") || r.is_match("master"))
                }
                _ => pat == "main" || pat == "master",
            }
        };
        let only = refs("ci_only");
        if !only.is_empty() && !only.iter().any(|r| matches(r)) {
            return false;
        }
        !refs("ci_except").iter().any(|r| matches(r))
    }

    fn hop(&self, how: &'static str, name: &str, d: &JobDef, p: &Pipeline, job: Option<&(PathBuf, String)>) -> Hop {
        let key = (d.s.root.clone(), d.file.to_string());
        let included = p.way(&key, job).map(|w| w.trail.clone()).unwrap_or_default();
        Hop { how, name: name.to_string(), repo: d.s.root.clone(), file: d.file.to_string(), line: d.line, included }
    }

    /// The jobs of pipeline `p` and the chains each gives, with `prefix` hops before them (a parent pipeline's).
    #[allow(clippy::too_many_arguments)]
    fn run(&self, p: &Pipeline<'a>, env: &Env<'a>, ctx: &Ctx, prefix: &[Hop], pre_jobs: &[(PathBuf, String, String)], conditions: &[String], children: usize, out: &mut Vec<Chain>, missing: &mut Vec<String>) {
        for &job in &p.order {
            if job.starts_with('.') {
                continue;
            }
            let start = out.len();
            // `parallel: matrix:` runs the job once per combination of its values.
            let combos = self.matrix(p, job);
            if combos.is_empty() {
                self.run_job(p, env, ctx, job, prefix, pre_jobs, conditions, children, out, missing);
            }
            for (values, f, s) in &combos {
                let mut env = env.clone();
                for (name, value) in values {
                    let at = Some(Origin { repo: &s.root, file: f.file.clone(), line: f.line });
                    env.set(name, Def { value: value.clone(), expanded: true, unresolved: Vec::new(), at, source: None, passed: true, branch: Vec::new() });
                }
                let from = out.len();
                self.run_job(p, &env, ctx, job, prefix, pre_jobs, conditions, children, out, missing);
                for c in &mut out[from..] {
                    c.matrix.extend(values.iter().map(|(n, v)| format!("{n}={v}")));
                }
            }
            // The files of this pipeline the new chains go through, before those of any child pipeline.
            for c in &mut out[start..] {
                let mut keys: Vec<(PathBuf, String)> = Vec::new();
                let mut closure = Vec::new();
                self.closure(p, job, &mut closure, 0);
                keys.extend(closure.into_iter().map(|(r, f, _)| (r, f)));
                // Where its script steps are written, `!reference`d ones included.
                for section in ["before_script", "script", "after_script"] {
                    keys.extend(self.steps(p, job, section, 0).into_iter().map(|st| (st.s.root.clone(), st.file.to_string())));
                    // A `!reference` to steps this doesn't keep (`npm config set …`) still names a file the job runs.
                    if let Some((d, _)) = self.supplier(p, job, "ci_script", Some(section), 0) {
                        for f in d.of("ci_script").filter(|f| f.name == section) {
                            let Some(rest) = f.value.strip_prefix("reference\t") else { continue };
                            let (rjob, rkey) = rest.split_once('\t').unwrap_or((rest, ""));
                            if let Some(rd) = p.jobs.get(rjob).and_then(|defs| defs.iter().rev().find(|x| x.has("ci_script", Some(rkey))).or(defs.last())) {
                                keys.push((rd.s.root.clone(), rd.file.to_string()));
                            }
                        }
                    }
                }
                keys.extend(c.hops.iter().chain([&c.run]).map(|h| (h.repo.clone(), h.file.clone())));
                for st in &c.steps {
                    keys.push(match &st.via {
                        Some(v) => (v.repo.clone(), v.file.clone()),
                        None => (st.repo.clone(), st.file.clone()),
                    });
                }
                let own_key = p.jobs.get(job).and_then(|d| d.last()).map(|d| (d.s.root.clone(), d.file.to_string()));
                let mut files = Vec::new();
                for k in &keys {
                    if !p.files.contains_key(k) {
                        continue;
                    }
                    let trail = p.way(k, own_key.as_ref()).map(|w| w.trail.clone()).unwrap_or_default();
                    let here = if k.0 == ctx.app.root { k.1.clone() } else { format!("{}:{}", dir_name(&k.0), k.1) };
                    for f in trail.iter().chain([&here]) {
                        if !files.contains(f) {
                            files.push(f.clone());
                        }
                    }
                }
                for f in std::mem::take(&mut c.ci_files) {
                    if !files.contains(&f) {
                        files.push(f);
                    }
                }
                c.ci_files = files;
            }
        }
    }

    /// The combinations of a job's `parallel: matrix:` (each variable and value, with the entry they come from), at
    /// most [`MAX_MATRIX`]; none without a matrix.
    fn matrix(&self, p: &Pipeline<'a>, job: &'a str) -> Vec<Combination<'a>> {
        let Some((d, _)) = self.supplier(p, job, "ci_matrix", None, 0) else { return Vec::new() };
        let mut out = Vec::new();
        for f in d.of("ci_matrix") {
            let Ok(serde_json::Value::Object(entry)) = serde_json::from_str::<serde_json::Value>(&f.value) else { continue };
            let mut combos: Vec<Vec<(String, String)>> = vec![Vec::new()];
            let mut all = 1usize;
            for (name, v) in &entry {
                let values: Vec<String> = match v {
                    serde_json::Value::Array(a) => a.iter().map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string)).collect(),
                    serde_json::Value::String(s) => vec![s.clone()],
                    other => vec![other.to_string()],
                };
                all = all.saturating_mul(values.len().max(1));
                combos = combos.into_iter().flat_map(|c| values.iter().map(move |v| [c.clone(), vec![(name.clone(), v.clone())]].concat())).take(MAX_MATRIX).collect();
            }
            let room = MAX_MATRIX.saturating_sub(out.len());
            if all > room {
                self.note_cut(format!("matrix of job {job}: {MAX_MATRIX} combinations followed, {} left out", all - room.min(combos.len())));
            }
            out.extend(combos.into_iter().map(|c| (c, *f, d.s)));
            if out.len() >= MAX_MATRIX {
                out.truncate(MAX_MATRIX);
                break;
            }
        }
        out
    }

    /// The chains of one job of pipeline `p` (see [`Ci::run`]).
    #[allow(clippy::too_many_arguments)]
    fn run_job(&self, p: &Pipeline<'a>, env: &Env<'a>, ctx: &Ctx, job: &'a str, prefix: &[Hop], pre_jobs: &[(PathBuf, String, String)], conditions: &[String], children: usize, out: &mut Vec<Chain>, missing: &mut Vec<String>) {
        let Some(own) = p.jobs.get(job).and_then(|d| d.last()) else { return };
        // Variables: passed (already in `env`) over the job's over the pipeline's (also in `env`, below).
        let mut env = env.clone();
        let mut jv = Vec::new();
        self.job_vars(p, job, 0, &mut jv);
        for (f, s) in jv {
            env.put_yaml(&f.name, Def::yaml(f, s));
        }
        let stage = self.supplier(p, job, "ci_stage", None, 0).and_then(|(d, _)| d.of("ci_stage").next().map(|f| f.name.clone()));
        predefined(&mut env, ctx, Some(job), stage.as_deref(), "push");
        if !self.refs_admit(p, job) {
            return;
        }
        let rules = self.rules(p, job);
        let (admitted, cond) = rules_admit(&rules, &env, ctx);
        if !admitted && !rules.is_empty() {
            return;
        }
        let mut conditions = conditions.to_vec();
        conditions.extend(cond);
        let mut jobs = pre_jobs.to_vec();
        self.closure(p, job, &mut jobs, 0);
        let mut hops = prefix.to_vec();
        let own_key = (own.s.root.clone(), own.file.to_string());
        hops.push(self.hop("job", job, own, p, None));
        // A child pipeline.
        if let Some((tdef, tfrom)) = self.supplier(p, job, "ci_trigger", None, 0) {
            if children >= MAX_CHILDREN {
                self.note_cut(format!("child pipelines more than {MAX_CHILDREN} levels deep not followed (job {job})"));
                return;
            }
            let mut thops = hops.clone();
            if tfrom != job {
                thops.push(self.hop("template", tfrom, tdef, p, Some(&own_key)));
            }
            let mut child_env = Env::default();
            // Everything the trigger job sees is passed down, above the child's own variables.
            for (name, defs) in &env.vars {
                if let Some(top) = defs.first().filter(|d| d.at.is_some()) {
                    let e = env.lookup(name, 0, ctx, 0);
                    let mut d = top.clone();
                    d.value = e.text;
                    d.unresolved = e.unresolved;
                    d.expanded = true;
                    d.passed = true;
                    child_env.add_low(name, d);
                }
            }
            let mut files = Vec::new();
            let cctx = child_ctx(ctx);
            for f in tdef.of("ci_trigger") {
                let spec: serde_json::Value = serde_json::from_str(&f.value).unwrap_or_default();
                // The child's `inputs:`, with the variables the trigger job sees (its matrix values too) expanded.
                let inputs = spec.get("inputs").map(|i| expand_json(i, &child_env, &cctx));
                match self.targets(ctx.app, &f.name, &spec, &child_env, &cctx) {
                    Ok(ts) => files.extend(ts.into_iter().map(|(r, f, l)| (r, f, l, inputs.clone()))),
                    Err(why) if !missing.contains(&why) => missing.push(why),
                    Err(_) => {}
                }
            }
            for (repo, file, _, _) in &files {
                thops.push(Hop { how: "trigger", name: file.clone(), repo: repo.root.clone(), file: file.clone(), line: 1, included: Vec::new() });
            }
            let child = self.pipeline(&files, &mut child_env, &cctx, missing);
            self.run(&child, &child_env, &cctx, &thops, &jobs, &conditions, children + 1, out, missing);
            return;
        }
        // The job's script: `after_script` runs in a shell of its own, from the project directory.
        let mut steps: Vec<(bool, Step)> = Vec::new();
        for section in ["before_script", "script", "after_script"] {
            steps.extend(self.steps(p, job, section, 0).into_iter().map(|s| (section == "after_script", s)));
        }
        ctx.cwd.borrow_mut().clear();
        let mut clones: Vec<(String, String)> = Vec::new();
        let mut tools: Vec<DeployStep> = Vec::new();
        let mut in_after = false;
        for (after, st) in &steps {
            if *after && !in_after {
                in_after = true;
                ctx.cwd.borrow_mut().clear();
            }
            let mut f = st.op.split('\t');
            let op = f.next().unwrap_or("");
            match op {
                "set" | "default" | "bset" => {
                    let name = f.next().unwrap_or("");
                    let expr = f.next().unwrap_or("");
                    if op == "default" && env.is_set(name, ctx) {
                        continue;
                    }
                    let e = env.expand(expr, Some((name, 0)), ctx, 0);
                    let source = e.from.clone().filter(|_| expr.contains("$(basename"));
                    let mut branch = e.branch;
                    if op == "bset" && !branch.iter().any(|b| b == name) {
                        branch.push(name.to_string());
                    }
                    env.set(name, Def { value: e.text, expanded: true, unresolved: e.unresolved, at: Some(Origin { repo: &st.s.root, file: st.file.to_string(), line: st.line }), source, passed: false, branch });
                }
                "cd" => {
                    let d = env.expand(f.next().unwrap_or(""), None, ctx, 0).text;
                    let cur = ctx.cwd();
                    ctx.cwd.borrow_mut().push(join(&cur, &d));
                }
                "popd" => {
                    ctx.cwd.borrow_mut().pop();
                }
                "clone" => {
                    let url = env.expand(f.next().unwrap_or(""), None, ctx, 0).text;
                    let dir = f.next().filter(|d| !d.is_empty()).map(str::to_string).unwrap_or_else(|| {
                        url.trim_end_matches('/').rsplit('/').next().unwrap_or("").trim_end_matches(".git").to_string()
                    });
                    clones.push((dir, url));
                }
                "playbook" if !in_after => {
                    let book = f.next().unwrap_or("");
                    let inv = f.next().unwrap_or("");
                    let vars = f.next().unwrap_or("");
                    let mut hops = hops.clone();
                    if st.from_job != job
                        && let Some(d) = p.jobs.get(st.from_job).and_then(|d| d.iter().rev().find(|d| d.file == st.file && d.s.root == st.s.root).or(d.last()))
                    {
                        hops.push(self.hop("template", st.from_job, d, p, Some(&own_key)));
                    }
                    let run_key = (st.s.root.clone(), st.file.to_string());
                    let run = Hop { how: "run", name: "ansible-playbook".into(), repo: st.s.root.clone(), file: st.file.to_string(), line: st.line, included: p.files.get(&run_key).cloned().unwrap_or_default() };
                    let (link, conds, assumptions) = self.links_of(p, hops.iter().chain([&run]).map(|h| (&h.repo, h.file.as_str())), &conditions, Some(&own_key));
                    let mut chain = self.chain(ctx, &env, book, inv, vars, &clones, hops, run, &jobs, &conds, link);
                    chain.assumptions = assumptions;
                    out.push(chain);
                }
                "tool" => {
                    let fields: Vec<&str> = f.collect();
                    if let Some((tool, args)) = fields.split_first()
                        && let Some(mut d) = self.tool_step(ctx, &env, &ctx.cwd(), tool, args)
                    {
                        d.repo = st.s.root.clone();
                        d.file = st.file.to_string();
                        d.line = st.line;
                        d.from_job = st.from_job.to_string();
                        d.through = st.through.iter().map(|j| j.to_string()).collect();
                        tools.push(d);
                    }
                }
                "run" => {
                    let fields: Vec<&str> = f.collect();
                    if let [pm, script, ..] = fields.as_slice() {
                        tools.extend(self.script_steps(ctx, &env, pm, script, st));
                    }
                }
                _ => {}
            }
        }
        if !tools.is_empty() {
            out.push(self.tool_chain(ctx, p, job, &hops, tools, &jobs, &conditions));
        }
    }

    /// The weakest include link to the files `sites` are in, `conditions` with the include conditions on the way
    /// to them, and what the way assumed (`revision_unverified`: an include whose `ref` isn't the checkout's).
    fn links_of<'h>(
        &self,
        p: &Pipeline<'a>,
        sites: impl Iterator<Item = (&'h PathBuf, &'h str)>,
        conditions: &[String],
        job: Option<&(PathBuf, String)>,
    ) -> (&'static str, Vec<String>, Vec<kc_store::answer::Assumption>) {
        let mut link = "exact";
        let mut conds = conditions.to_vec();
        let mut assumptions = Vec::new();
        for (repo, file) in sites {
            let key = (repo.clone(), file.to_string());
            if p.unverified.contains(&key) && assumptions.is_empty() {
                assumptions.push(kc_store::answer::Assumption::RevisionUnverified);
            }
            let Some(way) = p.way(&key, job) else { continue };
            link = weakest(link, way.link);
            for c in &way.conds {
                if !conds.contains(c) {
                    conds.push(c.clone());
                }
            }
        }
        (link, conds, assumptions)
    }

    /// The step a deploy command gives, run in `cwd` of the application repo with the variables of `env`.
    fn tool_step(&self, ctx: &Ctx, env: &Env<'a>, cwd: &str, tool: &str, args: &[&str]) -> Option<DeployStep> {
        let expand = |name: &str, written: &str| passed(env, ctx, name, written, true);
        let has = |name: &str| env.vars.contains_key(name);
        let module = |source: &str| self.module_repo(source);
        let cx = ToolCtx { root: &ctx.app.root, cwd: crate::ci_tools::join(cwd, "."), expand: &expand, has: &has, module: &module };
        crate::ci_tools::step(tool, args, &cx)
    }

    /// The deploy commands of a package script a job runs (`npm run deploy`), read one level deep from the
    /// package.json in the working directory.
    fn script_steps(&self, ctx: &Ctx, env: &Env<'a>, pm: &str, script: &str, st: &Step<'a>) -> Vec<DeployStep> {
        let cwd = ctx.cwd();
        let file = crate::ci_tools::join(&cwd, "package.json");
        let Some(text) = crate::ci_tools::read(&ctx.app.root, &file) else { return Vec::new() };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
        let Some(body) = doc.get("scripts").and_then(|s| s.get(script)).and_then(|b| b.as_str()) else { return Vec::new() };
        // The script's line: its key after `"scripts"`.
        let at = text.find("\"scripts\"").unwrap_or(0);
        let key = format!("\"{script}\"");
        let line = text[at..].find(&key).map_or(1, |i| text[..at + i].matches('\n').count() as u32 + 1);
        let via = Via { command: format!("{pm} run {script}"), repo: st.s.root.clone(), file: st.file.to_string(), line: st.line };
        let mut dir = cwd.clone();
        let mut out = Vec::new();
        for op in kc_extract::gitlabci::shell_steps(body) {
            let mut f = op.split('\t');
            match f.next().unwrap_or("") {
                "cd" => dir = join(&dir, &env.expand(f.next().unwrap_or(""), None, ctx, 0).text),
                "tool" => {
                    let fields: Vec<&str> = f.collect();
                    if let Some((tool, args)) = fields.split_first()
                        && let Some(mut d) = self.tool_step(ctx, env, &dir, tool, args)
                    {
                        d.repo = ctx.app.root.clone();
                        d.file = if file == "." { "package.json".into() } else { file.clone() };
                        d.line = line;
                        d.from_job = st.from_job.to_string();
                        d.through = st.through.iter().map(|j| j.to_string()).collect();
                        d.via = Some(via.clone());
                        d.files.insert(0, FileRef { role: "package_json", path: d.file.clone(), found: true });
                        out.push(d);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The chain of a job running deploy commands other than `ansible-playbook`.
    #[allow(clippy::too_many_arguments)]
    fn tool_chain(&self, ctx: &Ctx, p: &Pipeline<'a>, job: &'a str, hops: &[Hop], steps: Vec<DeployStep>, jobs: &[(PathBuf, String, String)], conditions: &[String]) -> Chain {
        // Where a step is written in CI: its own place, or where the package script it is in is run.
        let site = |s: &DeployStep| s.via.as_ref().map_or((s.repo.clone(), s.file.clone(), s.line), |v| (v.repo.clone(), v.file.clone(), v.line));
        let own_key = p.jobs.get(job).and_then(|d| d.last()).map(|d| (d.s.root.clone(), d.file.to_string()));
        let mut hops = hops.to_vec();
        for s in &steps {
            let (repo, file, _) = site(s);
            // The templates whose `!reference`s reach the step, then the one it is written in.
            for (name, own) in s.through.iter().map(|t| (t, false)).chain([(&s.from_job, true)]) {
                if name == job || hops.iter().any(|h| h.how == "template" && h.name == *name) {
                    continue;
                }
                let defs = p.jobs.get(name.as_str());
                let d = if own { defs.and_then(|d| d.iter().rev().find(|d| d.file == file && d.s.root == repo).or(d.last())) } else { defs.and_then(|d| d.last()) };
                if let Some(d) = d {
                    hops.push(self.hop("template", name, d, p, own_key.as_ref()));
                }
            }
        }
        let (repo, file, line) = site(&steps[0]);
        let run = Hop { how: "run", name: format!("{} {}", steps[0].tool, steps[0].action), included: p.files.get(&(repo.clone(), file.clone())).cloned().unwrap_or_default(), repo, file, line };
        let sites: Vec<(PathBuf, String)> = steps.iter().map(site).map(|(r, f, _)| (r, f)).chain(hops.iter().map(|h| (h.repo.clone(), h.file.clone()))).collect();
        let (confidence, conds, assumptions) = self.links_of(p, sites.iter().map(|(r, f)| (r, f.as_str())), conditions, own_key.as_ref());
        let kind = steps.iter().find(|s| !crate::ci_tools::is_release(s)).map_or("release", |s| s.tool);
        let mut jobs = jobs.to_vec();
        for h in &hops {
            let k = (h.repo.clone(), h.file.clone(), h.name.clone());
            if h.how != "trigger" && !jobs.contains(&k) {
                jobs.push(k);
            }
        }
        Chain {
            app: ctx.app.root.clone(),
            hops,
            run,
            kind,
            playbook: None,
            steps,
            inventory: None,
            vars: Vec::new(),
            roles: Vec::new(),
            deploys: Vec::new(),
            conditions: conds,
            ci_files: Vec::new(),
            matrix: Vec::new(),
            confidence,
            assumptions,
            jobs,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn chain(&self, ctx: &Ctx, env: &Env<'a>, book: &str, inv: &str, vars: &str, clones: &[(String, String)], hops: Vec<Hop>, run: Hop, jobs: &[(PathBuf, String, String)], conditions: &[String], link: &'static str) -> Chain {
        let vars: Vec<Passed> = vars.split('\n').filter_map(|kv| kv.split_once('=')).map(|(k, v)| passed(env, ctx, k, v, false)).collect();
        let inventory = (!inv.is_empty()).then(|| passed(env, ctx, "inventory", inv, false));
        let path = env.expand(book, None, ctx, 0).text;
        let path = join(&ctx.cwd(), &path);
        let mut playbook = self.playbook(ctx, book, &path, clones);
        if let Some(r) = playbook.repo.clone().filter(|_| playbook.found) {
            let (files, total) = playbook_files(&r, &playbook.file);
            playbook.files_total = (total > files.len()).then_some(total);
            playbook.files = files;
        }
        let extra: HashMap<&str, &str> = vars.iter().filter_map(|v| v.value.as_deref().map(|x| (v.name.as_str(), x))).collect();
        let (roles, deploys) = match &playbook.repo {
            Some(r) => self.ansible_side(r, &playbook.file, &extra),
            None => (Vec::new(), Vec::new()),
        };
        let confidence = weakest(link, if playbook.confidence == "unlinked" { "ambiguous" } else { playbook.confidence });
        let mut jobs = jobs.to_vec();
        for h in &hops {
            let k = (h.repo.clone(), h.file.clone(), h.name.clone());
            if h.how != "trigger" && !jobs.contains(&k) {
                jobs.push(k);
            }
        }
        Chain {
            app: ctx.app.root.clone(),
            hops,
            run,
            kind: "ansible",
            playbook: Some(playbook),
            steps: Vec::new(),
            inventory,
            vars,
            roles,
            deploys,
            conditions: conditions.to_vec(),
            ci_files: Vec::new(),
            matrix: Vec::new(),
            confidence,
            assumptions: Vec::new(),
            jobs,
        }
    }

    /// The playbook a path names: under a cloned directory, in the application repo, or under a directory named like
    /// a repo.
    fn playbook(&self, ctx: &Ctx, written: &str, path: &str, clones: &[(String, String)]) -> PlaybookLink {
        let (first, rest) = path.split_once('/').unwrap_or(("", path));
        if let Some((_, url)) = clones.iter().rev().find(|(d, _)| !first.is_empty() && d == first) {
            let origin = normalize_remote(url);
            let repo = origin.as_ref().and_then(|o| self.by_origin.get(o).copied());
            if repo.is_none() {
                crate::workspace::note_missing_if(self.gone.iter().copied(), |g| g.origin.is_some() && g.origin == origin);
            }
            return PlaybookLink {
                written: written.to_string(),
                found: repo.is_some_and(|r| r.ansible.playbooks.iter().any(|p| p == rest)),
                files: Vec::new(),
                files_total: None,
                repo: repo.map(|r| r.root.clone()),
                file: rest.to_string(),
                confidence: if repo.is_some() { "exact" } else { "unlinked" },
                clone: Some(url.clone()),
            };
        }
        if ctx.app.ansible.playbooks.iter().any(|p| p == path) {
            return PlaybookLink { written: written.to_string(), repo: Some(ctx.app.root.clone()), file: path.to_string(), confidence: "exact", clone: None, found: true, files: Vec::new(), files_total: None };
        }
        if !first.is_empty() {
            // A gone repo named like the directory would have been a candidate.
            crate::workspace::note_missing_if(self.gone.iter().copied(), |g| dir_name(&g.root).eq_ignore_ascii_case(first));
        }
        if !first.is_empty()
            && let Some([one]) = self.by_name.get(&first.to_lowercase()).map(Vec::as_slice)
        {
            return PlaybookLink {
                written: written.to_string(),
                repo: Some(one.root.clone()),
                file: rest.to_string(),
                confidence: "unique_name",
                clone: None,
                found: one.ansible.playbooks.iter().any(|p| p == rest),
                files: Vec::new(),
                files_total: None,
            };
        }
        PlaybookLink { written: written.to_string(), repo: None, file: path.to_string(), confidence: "unlinked", clone: None, found: false, files: Vec::new(), files_total: None }
    }

    /// The roles a playbook runs (in its repo, or another), and what it and they deploy with the extra vars.
    fn ansible_side(&self, root: &Path, playbook: &str, extra: &HashMap<&str, &str>) -> (Vec<RoleHop>, Vec<Deploy>) {
        let Some(s) = self.repos.iter().copied().find(|s| s.root == root) else { return (Vec::new(), Vec::new()) };
        let mut roles: Vec<RoleHop> = Vec::new();
        // (repo, file prefix or file, a role's directory, depth) whose deploys count: the playbook, then each role.
        let mut scopes: Vec<(&RepoSummary, String, bool, u32)> = vec![(s, playbook.to_string(), false, 0)];
        let mut i = 0;
        while i < scopes.len() {
            if i >= 32 {
                self.note_cut(format!("playbook {playbook}: roles past the first 32 scopes not followed"));
                break;
            }
            let (r, scope, dir, depth) = scopes[i].clone();
            i += 1;
            if depth >= MAX_ROLE_DEPTH {
                continue;
            }
            let inside = |f: &str| if dir { scope.is_empty() || f.starts_with(&format!("{scope}/")) } else { f == scope };
            for (f, d) in &r.ansible.local_roles {
                if inside(f) && roles.len() >= MAX_ROLES && !roles.iter().any(|x| x.repo == r.root && x.dir == *d) {
                    self.note_cut(format!("playbook {playbook}: more than {MAX_ROLES} roles; the rest not followed"));
                }
                if inside(f) && roles.len() < MAX_ROLES && !roles.iter().any(|x| x.repo == r.root && x.dir == *d) {
                    roles.push(RoleHop::new(d.rsplit('/').next().unwrap_or(d).to_string(), r.root.clone(), d.clone(), "exact", depth + 1));
                    scopes.push((r, d.clone(), true, depth + 1));
                }
            }
            for u in r.ansible.uses.iter().filter(|u| inside(&u.file)) {
                let links = self.ansible.role(&r.root, &u.name, &u.file);
                // A role no indexed repo holds (a Galaxy role, `geerlingguy.docker`): listed, without files.
                if links.is_empty() && !u.name.contains("{{") && roles.len() < MAX_ROLES && !roles.iter().any(|x| x.repo.as_os_str().is_empty() && x.name == u.name) {
                    roles.push(RoleHop::external(u.name.clone(), depth + 1));
                }
                for l in links {
                    if roles.iter().any(|x| x.repo == l.repo && x.dir == l.role.dir) {
                        continue;
                    }
                    if roles.len() >= MAX_ROLES {
                        self.note_cut(format!("playbook {playbook}: more than {MAX_ROLES} roles; the rest not followed"));
                        continue;
                    }
                    roles.push(RoleHop::new(u.name.clone(), l.repo.clone(), l.role.dir.clone(), l.confidence, depth + 1));
                    if let Some(rs) = self.repos.iter().copied().find(|x| x.root == l.repo) {
                        scopes.push((rs, l.role.dir.clone(), true, depth + 1));
                    }
                }
            }
        }
        let mut deploys = Vec::new();
        for (r, scope, dir, _) in &scopes {
            let inside = |f: &str| if *dir { scope.is_empty() || f.starts_with(&format!("{scope}/")) } else { f == scope };
            for f in r.ansible.facts.iter().filter(|f| crate::ansible::is_task_action(&f.kind) && f.name != "url" && inside(&f.file)) {
                let mut used = false;
                let v = jinja(&f.value, r, &f.file, scope, *dir, extra, &mut used, 0);
                if !used || deploys.iter().any(|d: &Deploy| d.action == f.kind && d.what == f.name && d.resolved.as_deref() == Some(v.as_str())) {
                    continue;
                }
                let links = if v.contains("{{") { Vec::new() } else { self.ansible.links_of(&r.root, &f.name, &v) };
                deploys.push(Deploy {
                    action: f.kind.clone(),
                    what: f.name.clone(),
                    value: f.value.clone(),
                    resolved: Some(v),
                    file: f.file.clone(),
                    line: f.line,
                    symbol: f.symbol.clone(),
                    role: f.role.clone(),
                    links,
                });
            }
        }
        (roles, deploys)
    }

    /// The deploy chains of the repo at `root`: what its own pipeline (`.gitlab-ci.yml`) runs; with the includes
    /// and child pipelines that couldn't be read (another project's file not indexed, a template, a path holding a
    /// variable nothing sets).
    pub fn chains(&self, root: &Path) -> Pipelines {
        let Some(s) = self.repos.iter().copied().find(|s| s.root == root) else { return Pipelines::default() };
        self.chains_of(s)
    }

    fn chains_of(&self, s: &'a RepoSummary) -> Pipelines {
        if !s.ci.files.iter().any(|f| f == ".gitlab-ci.yml") {
            return Pipelines::default();
        }
        self.templates.borrow_mut().clear();
        self.includes.borrow_mut().clear();
        let project_files: Vec<String> = s.packages.iter().filter(|p| p.kind == "csproj").map(|p| p.file.clone()).collect();
        let ctx = Ctx { app: s, project_files, cwd: std::cell::RefCell::new(Vec::new()), unset: UNSET.to_vec(), child: false };
        let mut env = Env::default();
        let mut missing = Vec::new();
        let p = self.pipeline(&[(s, ".gitlab-ci.yml".to_string(), "exact", None)], &mut env, &ctx, &mut missing);
        let mut out = Vec::new();
        self.run(&p, &env, &ctx, &[], &[], &[], 0, &mut out, &mut missing);
        let mut includes = self.includes.take();
        self.describe_includes(&mut includes, &p, &out);
        Pipelines { chains: out, unread: missing, templates: self.templates.take(), cut: self.cut.take(), includes }
    }

    /// Every repo's deploy chains.
    pub fn all(&self) -> Vec<Chain> {
        self.all_with_cuts().0
    }

    /// Every repo's deploy chains, and what the limits left out of each repo's pipeline (`repo: what`): a chain they
    /// cut is missing.
    pub fn all_with_cuts(&self) -> (Vec<Chain>, Vec<String>) {
        // Every repo's pipeline is read, most about other repos than the answer's: a gone repo one of them names is
        // not said to be missing from an answer that may not need it.
        crate::workspace::quietly(|| self.all_chains())
    }

    fn all_chains(&self) -> (Vec<Chain>, Vec<String>) {
        let (mut chains, mut cut) = (Vec::new(), Vec::new());
        for s in self.repos.iter().filter(|s| !s.ci.files.is_empty()) {
            let p = self.chains_of(s);
            let name = s.root.file_name().map_or_else(|| s.root.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
            cut.extend(p.cut.into_iter().map(|why| format!("{name}: {why}")));
            chains.extend(p.chains);
        }
        (chains, cut)
    }
}

/// A file read with its inputs: facts and jobs with `$[[ inputs.x ]]` replaced.
struct Instance {
    /// Names the inputs, so each set is a file of its own.
    key: String,
    facts: Vec<FactRow>,
    jobs: Vec<kc_store::ci::JobRow>,
}

/// The instance of `file` of `s` for the include's `inputs` over the file's defaults. Instances are kept for the
/// process (a query reads the same files many times, and the resolver holds references to them): one per file
/// content and inputs.
fn instance(s: &RepoSummary, file: &str, base: &[&FactRow], inputs: Option<&serde_json::Value>) -> &'static Instance {
    static INSTANCES: std::sync::Mutex<Option<HashMap<String, &'static Instance>>> = std::sync::Mutex::new(None);
    let mut values = serde_json::Map::new();
    for f in base.iter().filter(|f| f.kind == "ci_input") {
        values.insert(f.name.clone(), serde_json::from_str(&f.value).unwrap_or(serde_json::Value::Null));
    }
    if let Some(serde_json::Value::Object(given)) = inputs {
        values.extend(given.clone());
    }
    let inputs_key = serde_json::Value::Object(values.clone()).to_string();
    let mut h = blake3::Hasher::new();
    for f in base {
        for part in [f.kind.as_str(), &f.name, &f.value, f.symbol.as_deref().unwrap_or(""), &f.line.to_string()] {
            h.update(part.as_bytes());
            h.update(&[0]);
        }
    }
    h.update(inputs_key.as_bytes());
    let key = format!("{}\0{file}\0{}", s.root.display(), h.finalize().to_hex());
    let mut guard = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(i) = map.get(&key) {
        return i;
    }
    let sub = |t: &str| interpolate(t, &values);
    let facts = base
        .iter()
        .filter(|f| f.kind != "ci_input")
        .map(|f| {
            let value = if f.kind == "ci_include" || f.kind == "ci_trigger" {
                // JSON: replace in its strings, so values holding quotes stay JSON.
                serde_json::from_str::<serde_json::Value>(&f.value).map(|v| interpolate_json(v, &values).to_string()).unwrap_or_else(|_| f.value.clone())
            } else {
                sub(&f.value)
            };
            FactRow { kind: f.kind.clone(), name: sub(&f.name), value, file: f.file.clone(), line: f.line, symbol: f.symbol.as_deref().map(sub), role: f.role.clone() }
        })
        .collect();
    let jobs = s.ci.jobs.iter().filter(|j| j.file == file).map(|j| kc_store::ci::JobRow { name: sub(&j.name), file: j.file.clone(), line: j.line }).collect();
    let short: String = h.finalize().to_hex().chars().take(12).collect();
    let i: &'static Instance = Box::leak(Box::new(Instance { key: short, facts, jobs }));
    map.insert(key, i);
    i
}

/// A JSON value with `$VAR`s in its strings expanded where `env` knows them (left as written otherwise).
fn expand_json(v: &serde_json::Value, env: &Env, ctx: &Ctx) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) if s.contains('$') => {
            let e = env.expand(s, None, ctx, 0);
            serde_json::Value::String(if e.unresolved.is_empty() { e.text } else { s.clone() })
        }
        serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(|x| expand_json(x, env, ctx)).collect()),
        serde_json::Value::Object(m) => serde_json::Value::Object(m.iter().map(|(k, x)| (k.clone(), expand_json(x, env, ctx))).collect()),
        other => other.clone(),
    }
}

/// `$[[ inputs.x ]]` (also `inputs.x[0]`, and with a `| function` after it, not applied) replaced by the input's
/// value: a string as it is, a list or map as JSON. An input that isn't given is left as written.
fn interpolate(text: &str, values: &serde_json::Map<String, serde_json::Value>) -> String {
    if !text.contains("$[[") {
        return text.to_string();
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("$[[") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("]]").map(|e| start + e) else {
            out.push_str(&rest[start..]);
            return out;
        };
        let expr = rest[start + 3..end].split('|').next().unwrap_or("").trim();
        let found = expr.strip_prefix("inputs.").and_then(|path| {
            let (name, index) = match path.split_once('[') {
                Some((n, i)) => (n, i.trim_end_matches(']').parse::<usize>().ok()),
                None => (path, None),
            };
            let v = values.get(name)?;
            match index {
                Some(i) => v.get(i).cloned(),
                None => Some(v.clone()),
            }
        });
        match found {
            Some(serde_json::Value::String(s)) => out.push_str(&s),
            Some(serde_json::Value::Null) => {}
            Some(v) => out.push_str(&v.to_string()),
            None => out.push_str(&rest[start..end + 2]),
        }
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out
}

fn interpolate_json(v: serde_json::Value, values: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => serde_json::Value::String(interpolate(&s, values)),
        serde_json::Value::Array(a) => serde_json::Value::Array(a.into_iter().map(|x| interpolate_json(x, values)).collect()),
        serde_json::Value::Object(m) => serde_json::Value::Object(m.into_iter().map(|(k, x)| (k, interpolate_json(x, values))).collect()),
        other => other,
    }
}

/// Predefined variables this models as unset: merge request and tag pipelines don't deploy.
const UNSET: &[&str] = &["CI_MERGE_REQUEST_IID", "CI_MERGE_REQUEST_EVENT_TYPE", "CI_MERGE_REQUEST_TITLE", "CI_COMMIT_TAG", "CI_EXTERNAL_PULL_REQUEST_IID"];

/// The context of a child pipeline: the same project, started by its parent.
fn child_ctx<'c>(ctx: &Ctx<'c>) -> Ctx<'c> {
    Ctx { app: ctx.app, project_files: ctx.project_files.clone(), cwd: std::cell::RefCell::new(Vec::new()), unset: ctx.unset.clone(), child: true }
}

/// GitLab's predefined variables this can know, lowest precedence: the pipeline's, and the job's when given.
fn predefined(env: &mut Env, ctx: &Ctx, job: Option<&str>, stage: Option<&str>, source: &str) {
    let mut put = |name: &str, value: String| {
        env.add_low(name, Def { value, expanded: true, unresolved: Vec::new(), at: None, source: None, passed: false, branch: Vec::new() });
    };
    let origin_path = ctx.app.origin.as_deref().and_then(|o| o.split_once('/')).map(|(_, p)| p.to_string());
    let name = origin_path.as_deref().and_then(|p| p.rsplit('/').next()).map_or_else(|| dir_name(&ctx.app.root), str::to_string);
    put("CI_PROJECT_NAME", name);
    if let Some(p) = origin_path {
        put("CI_PROJECT_PATH", p);
    }
    put("CI_PROJECT_DIR", ".".into());
    if let Some((host, _)) = ctx.app.origin.as_deref().and_then(|o| o.split_once('/')) {
        put("CI_SERVER_FQDN", host.to_string());
        put("CI_SERVER_HOST", host.to_string());
    }
    if let Some(job) = job {
        put("CI_JOB_NAME", job.to_string());
    }
    if let Some(s) = stage {
        put("CI_JOB_STAGE", s.to_string());
    }
    // A push to the default branch.
    put("CI_COMMIT_BRANCH", "main".into());
    put("CI_COMMIT_REF_NAME", "main".into());
    put("CI_DEFAULT_BRANCH", "main".into());
    put("CI_COMMIT_REF_PROTECTED", "true".into());
    put("CI_PIPELINE_SOURCE", source.into());
}

/// A deployed value with its Jinja read: extra vars first, then the variables the playbook file defines, then the
/// role's (`scope`, when `dir`), then a variable defined once in the repo. `used` says whether an extra var was read.
#[allow(clippy::too_many_arguments)]
fn jinja(value: &str, s: &RepoSummary, file: &str, scope: &str, dir: bool, extra: &HashMap<&str, &str>, used: &mut bool, depth: usize) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("}}").map(|e| start + e) else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = rest[start + 2..end].trim();
        let mut parts = inner.split('|').map(str::trim);
        let name = parts.next().unwrap_or("");
        let filters: Vec<&str> = parts.collect();
        let simple = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        let found = if !simple || depth > MAX_DEPTH {
            None
        } else if let Some(v) = extra.get(name) {
            *used = true;
            Some(v.to_string())
        } else {
            definition(name, s, file, scope, dir).map(|v| jinja(&v, s, file, scope, dir, extra, used, depth + 1))
        };
        match found {
            Some(v) if !v.contains("{{") => out.push_str(&filtered(&v, &filters)),
            _ => out.push_str(&rest[start..end + 2]),
        }
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out
}

/// A variable's value as written, nearest definition first.
fn definition(name: &str, s: &RepoSummary, file: &str, scope: &str, dir: bool) -> Option<String> {
    let all = s.ansible.literals.iter().chain(&s.ansible.templated).filter(|(n, ..)| n == name);
    let pick = |pred: &dyn Fn(&str) -> bool| -> Option<String> {
        let mut vals: Vec<&String> = all.clone().filter(|(_, _, f)| pred(f)).map(|(_, v, _)| v).collect();
        vals.sort();
        vals.dedup();
        match vals.as_slice() {
            [one] => Some((*one).clone()),
            _ => None,
        }
    };
    pick(&|f| f == file)
        .or_else(|| if dir { pick(&|f| scope.is_empty() || f.starts_with(&format!("{scope}/"))) } else { None })
        .or_else(|| pick(&|_| true))
}

/// The filters this applies (`lower`, `upper`, `trim`, `truncate(n…)`, `string`); others leave the value as is.
fn filtered(v: &str, filters: &[&str]) -> String {
    let mut v = v.to_string();
    for f in filters {
        let (name, args) = f.split_once('(').unwrap_or((f, ""));
        match name.trim() {
            "lower" => v = v.to_lowercase(),
            "upper" => v = v.to_uppercase(),
            "trim" => v = v.trim().to_string(),
            "truncate" => {
                if let Some(n) = args.split(',').next().and_then(|n| n.trim().trim_end_matches(')').parse::<usize>().ok())
                    && v.chars().count() > n
                {
                    v = v.chars().take(n).collect();
                }
            }
            _ => {}
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(kind: &str, name: &str, value: &str, file: &str, line: u32, job: Option<&str>) -> serde_json::Value {
        serde_json::json!({"kind": kind, "name": name, "value": value, "file": file, "line": line, "symbol": job})
    }

    fn repo(root: &str, origin: &str, ci: serde_json::Value, ansible: serde_json::Value, csproj: &[&str]) -> RepoSummary {
        let packages: Vec<serde_json::Value> =
            csproj.iter().map(|p| serde_json::json!({"kind": "csproj", "name": p.rsplit('/').next().unwrap().trim_end_matches(".csproj"), "file": p, "line": 1, "targets": [], "deps": []})).collect();
        serde_json::from_value(serde_json::json!({
            "root": root, "packages": packages, "modules": [], "origin": origin, "ci": ci, "ansible": ansible, "stamp": "",
        }))
        .unwrap()
    }

    /// Facts as the extractor gives them for a CI text.
    fn ci_of(files: &[(&str, &str)]) -> serde_json::Value {
        let mut facts = Vec::new();
        let mut jobs = Vec::new();
        for (path, text) in files {
            let f = kc_extract::gitlabci::extract(path, text);
            for s in &f.symbols {
                jobs.push(serde_json::json!({"name": s.name, "file": path, "line": s.start_line}));
            }
            for a in &f.ansible {
                facts.push(fact(a.kind, &a.name, &a.value, path, a.line, a.sym.map(|i| f.symbols[i].name.as_str())));
            }
        }
        let names: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
        serde_json::json!({"files": names, "jobs": jobs, "facts": facts})
    }

    const TEMPLATES: &[(&str, &str)] = &[
        ("dotnet/main.yml", "include:\n  - local: '/dotnet/pipeline.yml'\n  - local: '/dotnet/mr.yml'\n    rules:\n      - if: $CI_MERGE_REQUEST_EVENT_TYPE == \"merge_train\"\n"),
        ("dotnet/mr.yml", ".deploy:\n  script: [echo skip]\n"),
        (
            "dotnet/pipeline.yml",
            ".deploy:\n  stage: deploy\n  variables:\n    BUILD_VERSION: $BUILD_VERSION\n  rules:\n    - if: '$CI_COMMIT_REF_NAME == $CI_DEFAULT_BRANCH'\n",
        ),
        (
            "dotnet/deploy.yml",
            "variables:\n  INVENTORY_FILE: \"${APP_DIR}/${ENV_TAG}.inventory\"\n.deploy_vm:\n  before_script:\n    - >\n      if [[ -z \"$PACKAGE_NAME\" ]]; then\n        pushd \"$APP_DIR\"\n        PACKAGE_NAME=\"$(basename *.csproj .csproj)\"\n        popd\n      fi\n    - '[[ -z \"$SERVICE_NAME\" ]] && SERVICE_NAME=\"${PACKAGE_NAME#Acme.}\"'\n    - git clone --depth 1 https://git.example.com/ops/Deploy.App.git\n  script:\n    - ansible-playbook ./Deploy.App/deploy_app.yml -i $INVENTORY_FILE --vault-password-file \"$VAULT_KEY\" --extra-vars \"APP_NAME=$PACKAGE_NAME APP_PORT=$APP_PORT SERVICE=$SERVICE_NAME VERSION=$BUILD_VERSION\"\n.stage:\n  variables:\n    ENV_TAG: Staging\n",
        ),
    ];

    fn workspace() -> Vec<RepoSummary> {
        let templates = repo("/w/pipeline-templates", "git.example.com/infra/pipeline-templates", ci_of(TEMPLATES), serde_json::json!({}), &[]);
        let app_ci = ci_of(&[
            (".gitlab-ci.yml", "include:\n  - project: 'Infra/pipeline-templates'\n    ref: master\n    file: '/dotnet/main.yml'\n  - local: '**/ci.yml'\n"),
            (
                "src/Acme.OrdersApi/ci.yml",
                ".app_vars:\n  variables:\n    APP_DIR: src/Acme.OrdersApi\n    APP_PORT: 5080\ndeploy_api:\n  extends:\n    - .deploy\n    - .app_vars\n  trigger:\n    include: \"${APP_DIR}/deploy.yml\"\n    strategy: depend\nsnapshot_deploy:\n  except: [master, main]\n  script: [ansible-playbook ./snapshot.yml]\n",
            ),
            (
                "src/Acme.OrdersApi/deploy.yml",
                "include:\n  - project: 'Infra/pipeline-templates'\n    file: '/dotnet/deploy.yml'\ndeploy_stage:\n  extends:\n    - .deploy_vm\n    - .stage\n",
            ),
        ]);
        let app = repo("/w/Acme.OrdersApi", "git.example.com/svc/acme.ordersapi", app_ci, serde_json::json!({}), &["src/Acme.OrdersApi/Acme.OrdersApi.csproj", "src/Acme.OrdersApi.Models/Acme.OrdersApi.Models.csproj"]);
        let deploy = repo(
            "/w/Deploy.App",
            "git.example.com/ops/deploy.app",
            serde_json::json!({}),
            serde_json::json!({
                "playbooks": ["deploy_app.yml"],
                "local_roles": [["deploy_app.yml", "roles/dotnet"]],
                "facts": [
                    {"kind": "deploys", "name": "service", "value": "{{ svc_name }}", "file": "roles/dotnet/tasks/main.yml", "line": 4},
                    {"kind": "deploys", "name": "path", "value": "/srv/apps/{{ app_name }}", "file": "roles/dotnet/tasks/main.yml", "line": 9},
                    {"kind": "deploys", "name": "service", "value": "consul", "file": "roles/dotnet/tasks/main.yml", "line": 12},
                ],
                "templated": [["svc_name", "{{ SERVICE | truncate(30, True, '') }}", "deploy_app.yml"], ["app_name", "{{ APP_NAME }}", "deploy_app.yml"]],
            }),
            &[],
        );
        vec![templates, app, deploy]
    }

    #[test]
    fn follows_includes_extends_and_child_pipelines_to_the_playbook() {
        let all = workspace();
        let ci = Ci::new(&all);
        let pipelines = ci.chains(Path::new("/w/Acme.OrdersApi"));
        accounted(&all, &all[1].root, &pipelines);
        let (chains, unread) = (pipelines.chains, pipelines.unread);
        assert!(unread.is_empty(), "{unread:?}");
        assert_eq!(chains.len(), 1);
        let c = &chains[0];
        let hops: Vec<(&str, &str, &str)> = c.hops.iter().map(|h| (h.how, h.name.as_str(), h.file.as_str())).collect();
        assert_eq!(
            hops,
            [
                ("job", "deploy_api", "src/Acme.OrdersApi/ci.yml"),
                ("trigger", "src/Acme.OrdersApi/deploy.yml", "src/Acme.OrdersApi/deploy.yml"),
                ("job", "deploy_stage", "src/Acme.OrdersApi/deploy.yml"),
                ("template", ".deploy_vm", "dotnet/deploy.yml"),
            ]
        );
        assert_eq!(c.hops[3].included, ["src/Acme.OrdersApi/deploy.yml"]);
        assert_eq!((c.run.file.as_str(), c.run.line), ("dotnet/deploy.yml", 14));
        let book = c.playbook.as_ref().unwrap();
        assert_eq!((c.kind, book.repo.as_deref()), ("ansible", Some(Path::new("/w/Deploy.App"))));
        assert_eq!((book.file.as_str(), book.confidence, book.found), ("deploy_app.yml", "exact", true));
        assert_eq!(c.confidence, "exact");
        assert_eq!(c.var("APP_NAME"), Some("Acme.OrdersApi"));
        assert_eq!(c.var("SERVICE"), Some("OrdersApi"));
        assert_eq!(c.var("APP_PORT"), Some("5080"));
        let app_name = c.vars.iter().find(|v| v.name == "APP_NAME").unwrap();
        assert_eq!(app_name.from.as_deref(), Some("src/Acme.OrdersApi/Acme.OrdersApi.csproj"));
        let port = c.vars.iter().find(|v| v.name == "APP_PORT").unwrap();
        assert_eq!(port.from.as_deref(), Some("src/Acme.OrdersApi/ci.yml:4"));
        let version = c.vars.iter().find(|v| v.name == "VERSION").unwrap();
        assert_eq!((version.value.as_deref(), version.unresolved.as_slice()), (None, &["BUILD_VERSION".to_string()][..]));
        assert_eq!(c.inventory.as_ref().and_then(|i| i.value.as_deref()), Some("src/Acme.OrdersApi/Staging.inventory"));
        assert_eq!(c.roles.iter().map(|r| r.dir.as_str()).collect::<Vec<_>>(), ["roles/dotnet"]);
        let deploys: Vec<(&str, Option<&str>)> = c.deploys.iter().map(|d| (d.what.as_str(), d.resolved.as_deref())).collect();
        assert_eq!(deploys, [("service", Some("OrdersApi")), ("path", Some("/srv/apps/Acme.OrdersApi"))]);
        assert!(c.through_job(Path::new("/w/pipeline-templates"), "dotnet/deploy.yml", ".deploy_vm"));
        assert!(c.through_job(Path::new("/w/pipeline-templates"), "dotnet/pipeline.yml", ".deploy"));
        assert!(c.runs_playbook(Path::new("/w/Deploy.App"), "deploy_app.yml"));
        assert!(c.uses_role(Path::new("/w/Deploy.App"), "roles/dotnet"));
        assert_eq!(ci.all().len(), 1);
    }

    #[test]
    fn shell_expansion_and_rules() {
        assert_eq!(strip("Acme.OrdersApi", "#Acme."), "OrdersApi");
        assert_eq!(strip("a.b.c", "%.*"), "a.b");
        assert_eq!(strip("a.b.c", "%%.*"), "a");
        assert_eq!(strip("a.b.c", "##*."), "c");
        assert!(glob("**/ci.yml", "src/App/ci.yml"));
        assert!(glob("**/ci.yml", "ci.yml"));
        assert!(glob("src/*/ci.yml", "src/App/ci.yml"));
        assert!(!glob("src/*/ci.yml", "src/a/b/ci.yml"));
        let all = workspace();
        let ctx = Ctx { app: &all[1], project_files: Vec::new(), cwd: std::cell::RefCell::new(Vec::new()), unset: UNSET.to_vec(), child: false };
        let mut env = Env::default();
        predefined(&mut env, &ctx, Some("deploy"), None, "push");
        assert_eq!(holds("$CI_COMMIT_REF_NAME == $CI_DEFAULT_BRANCH", &env, &ctx), Some(true));
        assert_eq!(holds("$CI_MERGE_REQUEST_EVENT_TYPE == \"merge_train\"", &env, &ctx), Some(false));
        assert_eq!(holds("$CI_PIPELINE_SOURCE != \"web\" && $CI_MERGE_REQUEST_EVENT_TYPE != \"merge_train\"", &env, &ctx), Some(true));
        assert_eq!(holds("$CI_COMMIT_TAG =~ /^v[0-9]+$/ || $LEGACY_OS != \"Linux\"", &env, &ctx), None);
        assert_eq!(holds("($CI_COMMIT_BRANCH =~ /^MAIN$/i) && $CI_COMMIT_TAG == null", &env, &ctx), Some(true));
        assert_eq!(rules_admit(&[("never", "$LEGACY_OS != \"Linux\""), ("", "")], &env, &ctx), (true, Some("$LEGACY_OS == \"Linux\"".into())));
        assert_eq!(rules_admit(&[("", "$CI_COMMIT_REF_NAME != $CI_DEFAULT_BRANCH"), ("manual", "$X == \"1\"")], &env, &ctx), (true, Some("$X == \"1\"".into())));
        assert_eq!(rules_admit(&[("never", "")], &env, &ctx), (false, None));
        assert_eq!(filtered("VeryLongServiceName", &["truncate(4, True, '')", "lower"]), "very");
    }

    /// A checkout on disk with `files`, for what deploy commands read at query time.
    fn checkout(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("kc-ci-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, text) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        root
    }

    const OTHER_CI: &str = "include:
  - template: Terraform/Base.latest.gitlab-ci.yml
  - local: /ci/optional.yml
    rules:
      - if: $FLAVOUR == \"blue\"
variables:
  TF_ROOT: ${CI_PROJECT_DIR}/infra
  REGISTRY: registry.example.com/web
terraform:build:
  extends: .terraform:build
terraform:deploy:
  extends: .terraform:deploy
worker:
  stage: deploy
  script:
    - cd site
    - pnpm run deploy
container:
  script:
    - docker build -f docker/Dockerfile -t \"$REGISTRY:$CI_COMMIT_SHORT_SHA\" .
    - docker push \"$REGISTRY:$CI_COMMIT_SHORT_SHA\"
lint:
  script: [pnpm run lint]
";

    #[test]
    fn deploys_without_ansible() {
        let root = checkout(
            "web",
            &[
                ("site/package.json", "{\n  \"name\": \"site\",\n  \"scripts\": {\n    \"lint\": \"eslint .\",\n    \"deploy\": \"pnpm build && wrangler deploy --env staging\"\n  }\n}\n"),
                ("site/wrangler.jsonc", "{\n  // the worker\n  \"name\": \"web\",\n  \"main\": \"src/index.ts\",\n  \"env\": {\"staging\": {},},\n}\n"),
                ("site/src/index.ts", "export default {};\n"),
                ("docker/Dockerfile", "FROM scratch\n"),
                ("infra/main.tf", "module \"spa\" {\n  source = \"git.example.com/terraform/terraform-aws-site/aws\"\n}\nmodule \"vpc\" {\n  source = \"terraform-aws-modules/vpc/aws\"\n}\n"),
            ],
        );
        let ci = ci_of(&[
            (".gitlab-ci.yml", OTHER_CI),
            ("ci/optional.yml", "blue:\n  script:\n    - aws s3 sync dist s3://blue-bucket --delete\n    - aws cloudfront create-invalidation --distribution-id E123 --paths '/*'\n"),
        ]);
        let mut app: RepoSummary = repo(root.to_str().unwrap(), "git.example.com/web/web", ci, serde_json::json!({}), &[]);
        app.ci.files.sort();
        let module = repo("/w/terraform-aws-site", "git.example.com/terraform/terraform-aws-site", serde_json::json!({}), serde_json::json!({}), &[]);
        let all = vec![app, module];
        let ci = Ci::new(&all);
        let p = ci.chains(&root);
        accounted(&all, &root, &p);
        assert!(p.unread.is_empty(), "{:?}", p.unread);
        assert_eq!(p.templates.len(), 1);
        assert!(p.templates[0].starts_with("Terraform/Base.latest.gitlab-ci.yml (GitLab built-in, not fetched): "), "{}", p.templates[0]);
        assert!(p.templates[0].contains(".terraform:deploy apply"));
        let summary: Vec<(&str, &str, String, &str)> = p.chains.iter().map(|c| (c.hops[0].name.as_str(), c.kind, c.target(), c.confidence)).collect();
        assert_eq!(
            summary,
            [
                ("blue", "aws", "aws s3 sync, aws cloudfront create-invalidation".to_string(), "ambiguous"),
                ("terraform:build", "terraform", "terraform plan".to_string(), "external_likely"),
                ("terraform:deploy", "terraform", "terraform apply".to_string(), "external_likely"),
                ("worker", "wrangler", "wrangler deploy".to_string(), "exact"),
                ("container", "docker", "docker build, docker push".to_string(), "exact"),
            ]
        );
        let tf = &p.chains[2];
        assert_eq!(tf.hops.iter().map(|h| (h.how, h.name.as_str(), h.line)).collect::<Vec<_>>(), [("job", "terraform:deploy", 11), ("template", ".terraform:deploy", 0)]);
        assert_eq!(tf.hops[1].repo, Path::new(BUILTIN_ROOT));
        let s = &tf.steps[0];
        assert_eq!((s.files[0].role, s.files[0].path.as_str(), s.files[0].found), ("tf_dir", "infra", true));
        let mods: Vec<(&str, Option<&Path>, &str)> = s.modules.iter().map(|m| (m.name.as_str(), m.repo.as_deref(), m.confidence)).collect();
        assert_eq!(mods, [("spa", Some(Path::new("/w/terraform-aws-site")), "unique_name"), ("vpc", None, "external_likely")]);
        // The rule names a variable nothing sets: the manual rule after it isn't reached, the condition is kept.
        assert_eq!(tf.conditions, ["$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && $TF_AUTO_DEPLOY == \"true\""]);
        let w = &p.chains[3].steps[0];
        assert_eq!((w.file.as_str(), w.line), ("site/package.json", 5));
        assert_eq!(w.via.as_ref().map(|v| (v.command.as_str(), v.line)), Some(("pnpm run deploy", 17)));
        let files: Vec<(&str, &str, bool)> = w.files.iter().map(|f| (f.role, f.path.as_str(), f.found)).collect();
        assert_eq!(files, [("package_json", "site/package.json", true), ("config", "site/wrangler.jsonc", true), ("entry", "site/src/index.ts", true)]);
        let values: Vec<(&str, Option<&str>)> = w.values.iter().map(|v| (v.name.as_str(), v.value.as_deref())).collect();
        assert_eq!(values, [("name", Some("web-staging")), ("env", Some("staging"))]);
        let d = &p.chains[4].steps;
        assert_eq!(d[0].files.iter().map(|f| (f.role, f.path.as_str(), f.found)).collect::<Vec<_>>(), [("dockerfile", "docker/Dockerfile", true), ("context", ".", true)]);
        assert_eq!(d[0].values[0].partial.as_deref(), Some("registry.example.com/web:$CI_COMMIT_SHORT_SHA"));
        assert_eq!(p.chains[0].conditions, ["$FLAVOUR == \"blue\""]);
        assert_eq!(p.chains[0].steps[0].values.iter().map(|v| v.value.as_deref().unwrap_or("")).collect::<Vec<_>>(), ["dist", "s3://blue-bucket"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Module sources link as `kc_index::terraform` links them: a git origin `exact`, a registry address under the
    /// repo's host and namespace `unique_name`, nothing for a public registry module or an archive merely named like
    /// a repo.
    #[test]
    fn module_sources_link_as_terraform_does() {
        let none = serde_json::json!({});
        let all = vec![
            repo("/w/aws-app", "github.com/x/aws-app", none.clone(), none.clone(), &[]),
            repo("/w/terraform-aws-site", "git.example.com/terraform/terraform-aws-site", none.clone(), none.clone(), &[]),
            repo("/w/net", "git.example.com/infra/net", none.clone(), none.clone(), &[]),
        ];
        let ci = Ci::new(&all);
        let link = |s: &str| ci.module_repo(s).map(|(r, c)| (r.map(|p| p.to_string_lossy().into_owned()), c));
        assert_eq!(link("github.com/x/aws-app"), Some((Some("/w/aws-app".into()), "exact")));
        assert_eq!(link("git::https://git.example.com/infra/net.git//modules/a"), Some((Some("/w/net".into()), "exact")));
        // A `ref` the checkout isn't known to be at: never exact.
        assert_eq!(link("git::https://git.example.com/infra/net.git//modules/a?ref=v2"), Some((Some("/w/net".into()), "unique_name")));
        assert_eq!(link("git.example.com/terraform/terraform-aws-site/aws"), Some((Some("/w/terraform-aws-site".into()), "unique_name")));
        assert_eq!(link("git.example.com/terraform/site/aws"), Some((Some("/w/terraform-aws-site".into()), "unique_name")));
        // The public registry: a local repo named like the module isn't it.
        assert_eq!(link("terraform-aws-modules/site/aws"), Some((None, "external_likely")));
        assert_eq!(link("https://example.com/modules/site.zip"), Some((None, "external_likely")));
        assert_eq!(link("./modules/x"), None);
    }

    /// `[[ -z "$X" ]] && X=…` in a template leaves alone a value the job sets, even one holding run-time variables;
    /// a value that is only variables nothing sets may be empty, so the default applies.
    #[test]
    fn guarded_defaults_keep_values_the_job_sets() {
        let root = checkout("guard", &[("Dockerfile", "FROM scratch\n")]);
        let ci = ci_of(&[(
            ".gitlab-ci.yml",
            ".build:\n  before_script:\n    - |\n      if [ -z \"$OutImage\" ]; then\n        OutImage=\"${BaseImage}:${ImageTag}\"\n      fi\n  script:\n    - docker build -t \"$OutImage\" .\n    - docker push \"$Other\"\nbuild:\n  extends: .build\n  variables:\n    OutImage: \"registry.example.com/app:0.0.${CI_PIPELINE_IID}\"\n    Other: \"$BUILD_TAG\"\n",
        )]);
        let all = vec![repo(root.to_str().unwrap(), "git.example.com/web/app", ci, serde_json::json!({}), &[])];
        let ci = Ci::new(&all);
        let p = ci.chains(&root);
        accounted(&all, &root, &p);
        let steps = &p.chains[0].steps;
        assert_eq!(steps[0].values[0].partial.as_deref(), Some("registry.example.com/app:0.0.${CI_PIPELINE_IID}"));
        assert_eq!(steps[1].values[0].unresolved, ["BUILD_TAG"]);
        let mut env = Env::default();
        let ctx = Ctx { app: &all[0], project_files: Vec::new(), cwd: std::cell::RefCell::new(Vec::new()), unset: UNSET.to_vec(), child: false };
        let def = |v: &str| Def { value: v.into(), expanded: false, unresolved: Vec::new(), at: None, source: None, passed: false, branch: Vec::new() };
        env.set("A", def("x:${RUNTIME}"));
        env.set("B", def("${RUNTIME}"));
        env.set("C", def(""));
        assert_eq!((env.is_set("A", &ctx), env.is_set("B", &ctx), env.is_set("C", &ctx), env.is_set("D", &ctx)), (true, false, false, false));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `include: component:` of a project in the workspace reads its template with the include's inputs over the
    /// spec's defaults, once per set of inputs; a component nothing indexed holds stays unread.
    #[test]
    fn components_resolve_with_their_inputs() {
        let comp = ci_of(&[(
            "templates/apply.yml",
            "spec:\n  inputs:\n    as:\n      default: apply\n    root_dir:\n      default: '.'\n---\ninclude:\n  - local: '/templates/base.yml'\n    inputs:\n      as: $[[ inputs.as ]]\n'$[[ inputs.as ]]':\n  extends: '.$[[ inputs.as ]]:base'\n  variables:\n    GITLAB_TOFU_ROOT_DIR: $[[ inputs.root_dir ]]\n  script:\n    - gitlab-tofu apply\n",
        ), ("templates/base.yml", "spec:\n  inputs:\n    as:\n---\n'.$[[ inputs.as ]]:base':\n  stage: deploy\n")]);
        let root = checkout("comp", &[("vault/main.tf", "resource \"x\" \"y\" {}\n")]);
        let app_ci = ci_of(&[(
            ".gitlab-ci.yml",
            "include:\n  - component: $CI_SERVER_FQDN/components/tofu/apply@$CI_COMMIT_SHA\n    inputs:\n      as: stg_apply\n      root_dir: vault/\n  - component: $CI_SERVER_FQDN/components/tofu/apply@1.0\n    inputs:\n      as: prd_apply\n  - component: $CI_SERVER_FQDN/elsewhere/lint/lint@1.0\n",
        )]);
        let all = vec![
            repo("/w/tofu", "git.example.com/components/tofu", comp, serde_json::json!({}), &[]),
            repo(root.to_str().unwrap(), "git.example.com/infra/vault", app_ci, serde_json::json!({}), &[]),
        ];
        let ci = Ci::new(&all);
        let p = ci.chains(&root);
        accounted(&all, &root, &p);
        assert_eq!(p.unread, ["component $CI_SERVER_FQDN/elsewhere/lint/lint@1.0 (not indexed: external_likely)"]);
        let got: Vec<(&str, &str, &str, &str, &str)> = p
            .chains
            .iter()
            .map(|c| (c.hops[0].name.as_str(), c.hops[0].file.as_str(), c.kind, c.steps[0].files[0].path.as_str(), c.confidence))
            .collect();
        // Both name a revision of the component (`@$CI_COMMIT_SHA`, `@1.0`) no checkout here can be checked at.
        assert_eq!(got, [("stg_apply", "templates/apply.yml", "terraform", "vault", "unique_name"), ("prd_apply", "templates/apply.yml", "terraform", ".", "unique_name")]);
        assert!(p.chains.iter().all(|c| c.assumptions == [kc_store::answer::Assumption::RevisionUnverified]));
        assert!(p.chains[0].through_job(Path::new("/w/tofu"), "templates/base.yml", ".stg_apply:base"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A role's task, handler and template files, read from the checkout, `tasks/main.yml` first.
    #[test]
    fn roles_list_their_files() {
        let root = checkout(
            "role",
            &[
                ("roles/web/tasks/install.yml", "- debug: {}\n"),
                ("roles/web/tasks/main.yml", "- import_tasks: install.yml\n"),
                ("roles/web/handlers/main.yml", "- name: restart\n  service: {name: web}\n"),
                ("roles/web/templates/nginx/site.conf.j2", "server {}\n"),
                ("roles/web/defaults/main.yml", "port: 80\n"),
            ],
        );
        let r = RoleHop::new("web".into(), root.clone(), "roles/web".into(), "exact", 1);
        assert_eq!(r.tasks, ["roles/web/tasks/main.yml", "roles/web/tasks/install.yml"]);
        assert_eq!(r.handlers, ["roles/web/handlers/main.yml"]);
        assert_eq!(r.templates, ["roles/web/templates/nginx/site.conf.j2"]);
        assert_eq!(r.more_files, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A playbook's own files: rendered templates, included task files, variable files, requirements.
    #[test]
    fn playbooks_list_the_files_they_read() {
        let root = checkout(
            "playbook",
            &[
                (
                    "deploy/site.yml",
                    "- hosts: all\n  vars_files:\n    - vars/common.yml\n    - \"secrets.yml\"\n  tasks:\n    # - include_vars: vars/old.yml\n    - include_vars:\n        file: env/{{ env }}.yml\n    - include_vars: extra.yml\n    - template:\n        src: app.conf.j2\n        dest: /etc/app.conf\n    - include_tasks: steps.yml\n",
                ),
                ("deploy/vars/common.yml", "a: 1\n"),
                ("deploy/vars/old.yml", "d: 4\n"),
                ("deploy/secrets.yml", "b: 2\n"),
                ("deploy/vars/extra.yml", "c: 3\n"),
                ("deploy/templates/app.conf.j2", "x\n"),
                ("deploy/steps.yml", "- debug: {}\n"),
                ("deploy/requirements.yml", "- src: x\n"),
            ],
        );
        assert_eq!(
            playbook_files(&root, "deploy/site.yml").0,
            ["deploy/vars/extra.yml", "deploy/templates/app.conf.j2", "deploy/steps.yml", "deploy/vars/common.yml", "deploy/secrets.yml", "deploy/requirements.yml"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `parallel: matrix:` runs a job once per combination: a child pipeline named by a matrix variable is read for
    /// each value.
    #[test]
    fn matrix_jobs_run_once_per_combination() {
        let ci = ci_of(&[
            (".gitlab-ci.yml", "tests:\n  trigger:\n    include: tests/$SUITE.gitlab-ci.yml\n  parallel:\n    matrix:\n      - SUITE: [Defaults, VarFile]\n        OS: alpine\n"),
            ("tests/Defaults.gitlab-ci.yml", "plan:\n  script:\n    - terraform plan\n"),
            ("tests/VarFile.gitlab-ci.yml", "apply:\n  script:\n    - terraform apply -var-file=x.tfvars\n"),
        ]);
        let all = vec![repo("/w/suite", "git.example.com/infra/suite", ci, serde_json::json!({}), &[])];
        let p = Ci::new(&all).chains(Path::new("/w/suite"));
        assert!(p.unread.is_empty(), "{:?}", p.unread);
        let got: Vec<(String, Vec<String>)> = p.chains.iter().map(|c| (c.target(), c.matrix.clone())).collect();
        assert_eq!(
            got,
            [
                ("terraform plan".to_string(), vec!["OS=alpine".to_string(), "SUITE=Defaults".to_string()]),
                ("terraform apply".to_string(), vec!["OS=alpine".to_string(), "SUITE=VarFile".to_string()]),
            ]
        );
    }

    /// A trigger's `inputs:` reach the child pipeline's `spec:`, with the trigger job's variables (its matrix values
    /// too) expanded in them: the child's templated includes resolve, and an include whose rule an input decides is
    /// skipped when it says so.
    #[test]
    fn trigger_inputs_reach_the_child_pipeline() {
        let ci = ci_of(&[
            (
                ".gitlab-ci.yml",
                "test:\n  variables:\n    RUN_TEST: 'true'\n  parallel:\n    matrix:\n      - EXAMPLE: https\n      - EXAMPLE: mirror\n        RUN_TEST: 'false'\n  trigger:\n    include:\n      - local: child.yml\n        inputs:\n          example_name: $EXAMPLE\n          run_test: $RUN_TEST\n",
            ),
            (
                "child.yml",
                "spec:\n  inputs:\n    example_name:\n    run_test:\n---\ninclude:\n  - local: examples/$[[ inputs.example_name ]].yml\n  - local: examples/tests/$[[ inputs.example_name ]]-test.yml\n    rules:\n      - if: '$[[ inputs.run_test ]] == \"true\"'\n",
            ),
            ("examples/https.yml", "https-push:\n  script:\n    - docker push registry/https\n"),
            ("examples/mirror.yml", "mirror-push:\n  script:\n    - docker push registry/mirror\n"),
            ("examples/tests/https-test.yml", "https-test:\n  script:\n    - docker build -t t .\n"),
        ]);
        let all = vec![repo("/w/gitcfg", "git.example.com/components/gitcfg", ci, serde_json::json!({}), &[])];
        let p = Ci::new(&all).chains(Path::new("/w/gitcfg"));
        assert!(p.unread.is_empty(), "{:?}", p.unread);
        let got: Vec<(&str, Vec<String>, Vec<String>)> = p.chains.iter().map(|c| (c.hops.last().unwrap().name.as_str(), c.matrix.clone(), c.conditions.clone())).collect();
        assert_eq!(
            got,
            [
                ("https-push", vec!["EXAMPLE=https".into()], vec![]),
                ("https-test", vec!["EXAMPLE=https".into()], vec![]),
                ("mirror-push", vec!["EXAMPLE=mirror".into(), "RUN_TEST=false".into()], vec![]),
            ]
        );
    }

    /// A file two workflows include is read once; each job takes the way in that brought its own definition, with
    /// that way's conditions.
    #[test]
    fn jobs_take_the_way_in_of_their_own_workflow() {
        let ci = ci_of(&[
            (
                ".gitlab-ci.yml",
                "include:\n  - local: /wf/app.yml\n    rules:\n      - if: $CONFIG == \"APP\"\n  - local: /wf/lib.yml\n    rules:\n      - if: $CONFIG == \"LIB\"\n",
            ),
            ("wf/app.yml", "include:\n  - local: /jobs/package.yml\napp_package:\n  extends: .package\n"),
            ("wf/lib.yml", "include:\n  - local: /jobs/package.yml\nlib_package:\n  extends: .package\n"),
            ("jobs/package.yml", ".package:\n  script:\n    - upack push p.upack https://feed/\n"),
        ]);
        let all = vec![repo("/w/lib", "git.example.com/web/lib", ci, serde_json::json!({}), &[])];
        let ci = Ci::new(&all);
        let p = ci.chains(Path::new("/w/lib"));
        let got: Vec<(&str, Vec<&str>, Vec<&str>)> = p
            .chains
            .iter()
            .map(|c| (c.hops[0].name.as_str(), c.ci_files.iter().map(String::as_str).collect(), c.conditions.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(
            got,
            [
                ("app_package", vec![".gitlab-ci.yml", "wf/app.yml", "jobs/package.yml"], vec!["$CONFIG == \"APP\""]),
                ("lib_package", vec![".gitlab-ci.yml", "wf/lib.yml", "jobs/package.yml"], vec!["$CONFIG == \"LIB\""]),
            ]
        );
        accounted(&all, Path::new("/w/lib"), &p);
    }

    /// The invariant every pipeline keeps: each `include:` entry of each file it read (the application's
    /// `.gitlab-ci.yml`, and every file a followed include read, recursively) is listed once, either followed (with
    /// the files it read) or named with why not; a `missing` one names its project (and ref) in `unread`, which the
    /// answer reports as `missing_repo`. Nothing is dropped silently.
    fn accounted(all: &[RepoSummary], app: &Path, p: &Pipelines) {
        let name = |root: &Path, file: &str| if root == app { file.to_string() } else { format!("{}:{file}", dir_name(root)) };
        let mut queue: Vec<(PathBuf, String)> = vec![(app.to_path_buf(), ".gitlab-ci.yml".to_string())];
        let mut seen: Vec<(PathBuf, String)> = Vec::new();
        while let Some((root, file)) = queue.pop() {
            if seen.contains(&(root.clone(), file.clone())) {
                continue;
            }
            seen.push((root.clone(), file.clone()));
            let Some(s) = all.iter().find(|s| s.root == root) else { continue };
            for f in s.ci.facts.iter().filter(|f| f.kind == "ci_include" && f.file == file) {
                let at = format!("{}:{}", name(&root, &file), f.line);
                let entries: Vec<&Include> = p.includes.iter().filter(|e| e.at == at && e.kind == f.name).collect();
                assert!(!entries.is_empty(), "include at {at} ({}) is in no entry: {:#?}", f.value, p.includes);
                for e in entries {
                    queue.extend(e.targets.iter().filter(|(r, _)| r != Path::new(BUILTIN_ROOT)).cloned());
                }
            }
        }
        for e in &p.includes {
            assert!(["followed", "missing", "unread", "unsupported", "skipped", "cut"].contains(&e.status), "{e:?}");
            match e.status {
                "followed" => assert!(!e.files.is_empty() && e.files.len() == e.targets.len(), "{e:?}"),
                "skipped" => assert!(e.why.is_some(), "{e:?}"),
                _ => {
                    let why = e.why.as_deref().unwrap_or_else(|| panic!("{e:?} says no why"));
                    assert!(p.unread.iter().any(|u| u == why) || e.status == "cut", "{why} not in unread {:?}", p.unread);
                    assert!(why.contains(e.target.trim_start_matches('/')) || e.status == "cut", "{why} doesn't name {}", e.target);
                    assert_eq!(e.status == "missing", why.contains("(not indexed") || why.contains("(checkout gone"), "{e:?}");
                    if e.status == "missing"
                        && let Some(r) = &e.git_ref
                    {
                        assert!(why.contains(r.as_str()), "{why} doesn't name ref {r}");
                    }
                    assert!(e.files.is_empty() && e.repo.is_none(), "{e:?}");
                }
            }
        }
    }

    /// Every `include:` form, followed into an indexed template repo (jobs, stages and deploy steps it contributes,
    /// recursively through its own includes, a cycle read once), or named missing with its project path and ref when
    /// the repo holding it isn't indexed; an unknown GitLab template is unsupported, a rule-excluded include skipped.
    #[test]
    fn invariant_every_include_is_followed_or_named_missing() {
        let templates = ci_of(&[
            ("deploy/app.yml", "include:\n  - local: /deploy/base.yml\ndeploy_app:\n  extends: .deploy_base\n  script:\n    - helm upgrade --install $CI_PROJECT_NAME ./chart\n"),
            ("deploy/base.yml", "include:\n  - local: /deploy/app.yml\n  - project: other/nested-templates\n    ref: v3\n    file: /n.yml\n.deploy_base:\n  stage: deploy\n"),
            ("remote.yml", "lint_remote:\n  stage: lint\n  script:\n    - echo lint\n"),
            ("templates/build.yml", "spec:\n  inputs:\n    stage:\n      default: build\n---\nbuild_image:\n  stage: $[[ inputs.stage ]]\n  script:\n    - docker build -t app .\n"),
        ]);
        let app_ci = ci_of(&[
            (
                ".gitlab-ci.yml",
                "include:\n  - project: 'platform/ci-templates'\n    ref: main\n    file: '/deploy/app.yml'\n  - project: 'platform/absent-templates'\n    ref: v2\n    file:\n      - '/a.yml'\n      - '/b.yml'\n  - component: $CI_SERVER_FQDN/platform/ci-templates/build@1.0\n  - component: $CI_SERVER_FQDN/platform/absent-components/scan@2.1\n  - remote: https://git.example.com/platform/ci-templates/-/raw/main/remote.yml\n  - remote: https://elsewhere.example.org/raw/ci.yml\n  - template: Terraform/Base.latest.gitlab-ci.yml\n  - template: Jobs/Unknown.gitlab-ci.yml\n  - local: /ci/local.yml\n  - local: /ci/never.yml\n    rules:\n      - if: $CI_PIPELINE_SOURCE == \"schedule\"\n  - '/ci/scalar.yml'\n",
            ),
            ("ci/local.yml", "unit:\n  stage: test\n  script:\n    - dotnet test\n"),
            ("ci/never.yml", "nightly:\n  script:\n    - echo never\n"),
            ("ci/scalar.yml", "pack:\n  script:\n    - echo pack\n"),
        ]);
        let all = vec![
            repo("/w/ci-templates", "git.example.com/platform/ci-templates", templates, serde_json::json!({}), &[]),
            repo("/w/shop-api", "git.example.com/shop/shop-api", app_ci, serde_json::json!({}), &[]),
        ];
        let ci = Ci::new(&all);
        let p = ci.chains(Path::new("/w/shop-api"));
        accounted(&all, Path::new("/w/shop-api"), &p);
        let got: Vec<(&str, &str, &str, Option<&str>)> = p.includes.iter().map(|e| (e.at.as_str(), e.kind.as_str(), e.status, e.repo.as_deref())).collect();
        assert_eq!(
            got,
            [
                (".gitlab-ci.yml:4", "project", "followed", Some("ci-templates")),
                ("ci-templates:deploy/app.yml:2", "local", "followed", Some("ci-templates")),
                ("ci-templates:deploy/base.yml:2", "local", "followed", Some("ci-templates")),
                ("ci-templates:deploy/base.yml:5", "project", "missing", None),
                (".gitlab-ci.yml:8", "project", "missing", None),
                (".gitlab-ci.yml:9", "project", "missing", None),
                (".gitlab-ci.yml:10", "component", "followed", Some("ci-templates")),
                (".gitlab-ci.yml:11", "component", "missing", None),
                (".gitlab-ci.yml:12", "remote", "followed", Some("ci-templates")),
                (".gitlab-ci.yml:13", "remote", "missing", None),
                (".gitlab-ci.yml:14", "template", "followed", None),
                (".gitlab-ci.yml:15", "template", "unsupported", None),
                (".gitlab-ci.yml:16", "local", "followed", None),
                (".gitlab-ci.yml:17", "local", "skipped", None),
                (".gitlab-ci.yml:20", "local", "followed", None),
            ],
            "{:#?}",
            p.includes
        );
        let by_at = |at: &str| p.includes.iter().find(|e| e.at == at).unwrap();
        // The template repo's jobs, with the stage they take through `extends`, and its deploy step.
        assert_eq!(by_at(".gitlab-ci.yml:4").jobs, ["deploy_app (deploy)"]);
        assert_eq!(by_at(".gitlab-ci.yml:4").deploys, ["helm upgrade (deploy_app)"]);
        assert_eq!(by_at(".gitlab-ci.yml:10").jobs, ["build_image (build)"]);
        assert_eq!(by_at(".gitlab-ci.yml:12").jobs, ["lint_remote (lint)"]);
        assert_eq!(by_at(".gitlab-ci.yml:16").jobs, ["unit (test)"]);
        // The cycle back to app.yml is read once.
        assert!(by_at("ci-templates:deploy/base.yml:2").why.as_deref().unwrap().starts_with("its files were already read"));
        assert!(by_at("ci-templates:deploy/base.yml:2").jobs.is_empty());
        // Missing ones name the project, file and ref.
        assert_eq!(by_at(".gitlab-ci.yml:8").named(), "project platform/absent-templates, file /a.yml, ref v2");
        assert_eq!(by_at("ci-templates:deploy/base.yml:5").why.as_deref(), Some("project other/nested-templates, file /n.yml, ref v3 (not indexed)"));
        assert_eq!(by_at(".gitlab-ci.yml:11").git_ref.as_deref(), Some("2.1"));
        let chains: Vec<&str> = p.chains.iter().map(|c| c.kind).collect();
        // Terraform/Base defines templates only: no job of its own runs.
        assert_eq!(chains, ["helm", "docker"], "{:#?}", p.chains);
    }
}
