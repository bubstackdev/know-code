//! Restricted mode: `knowcode serve --restricted`, for an automated pipeline that gives an agent a bounded KnowCode.
//! The server answers about the repos it is started with and nothing else, runs only the ops it is allowed, and keeps
//! to the directories it is given:
//!
//! - no workspace discovery: the workspace is the bound repos (see [`kc_index::config::bind`]), and a call naming
//!   any other repo, or a path outside them, is refused (`not_allowed`);
//! - explicit config and cache directories (and data, inside the cache unless given), so nothing of the user's is
//!   read or written;
//! - no update checks, no plugin refresh, and git kept off the network (no lazy fetches, local protocols only);
//! - an op allowlist: any other op is refused (`not_allowed`); `capabilities` is always allowed, and says the scope.
//!
//! The scope is stated when the server starts: in the `initialize` reply (`capabilities.experimental` under
//! `knowcode/scope`), on stderr, and in the capabilities.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kc_store::answer::{Code, Refusal};
use serde_json::{Value, json};

/// What `serve --restricted` was asked for.
pub struct Args {
    pub repos: Vec<PathBuf>,
    pub ops: Option<String>,
    pub cache_dir: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

/// The scope a restricted server runs with.
#[derive(Debug, Clone)]
pub struct Restriction {
    /// The bound repos, canonical.
    pub repos: Vec<PathBuf>,
    /// The tools its ops run (`kc_find`), `kc_capabilities` among them.
    pub tools: Vec<String>,
    pub cache: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
}

/// The directory a flag or its variable names; refused when neither does.
fn explicit(flag: Option<PathBuf>, var: &str, what: &str) -> Result<PathBuf> {
    match flag.or_else(|| std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)) {
        Some(p) => Ok(p),
        None => bail!(Refusal::new(Code::InvalidArgument, format!("restricted mode needs an explicit {what} directory: --{what}-dir, or {var}"))),
    }
}

/// The directory `p`, made if missing, canonical.
fn made(p: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
    std::fs::canonicalize(p).with_context(|| format!("resolving {}", p.display()))
}

/// Checks `args` and sets the process up for them: the directories and switches every part of KnowCode reads from
/// the environment, and the bound repos. Called first, before anything reads the config or starts a thread.
pub fn prepare(args: Args) -> Result<Restriction> {
    if args.repos.is_empty() {
        bail!(Refusal::new(Code::InvalidArgument, "restricted mode needs the repos it answers about: --repo, once per repo"));
    }
    let mut repos = Vec::new();
    for r in &args.repos {
        let root = kc_index::repo_root(r).with_context(|| format!("--repo {}", r.display()))?;
        if std::fs::canonicalize(r).ok().as_deref() != Some(root.as_path()) {
            bail!(Refusal::new(Code::InvalidArgument, format!("--repo {}: not a repo's root (its repo is {})", r.display(), root.display())));
        }
        if !repos.contains(&root) {
            repos.push(root);
        }
    }
    let Some(ops) = args.ops.filter(|o| !o.trim().is_empty()) else {
        bail!(Refusal::new(Code::InvalidArgument, "restricted mode needs an op allowlist: --ops find,refs,context (`knowcode capabilities` lists the ops)"));
    };
    let known: Vec<&str> = crate::mcp::ONE_OPS.iter().map(|(o, _, _)| *o).collect();
    let mut tools = vec!["kc_capabilities".to_string()];
    for op in ops.split(',').map(|o| o.trim().trim_start_matches("kc_")).filter(|o| !o.is_empty()) {
        if !known.contains(&op) {
            bail!(Refusal::new(Code::InvalidArgument, format!("--ops: no op {op:?}: one of {}", known.join(", "))));
        }
        let tool = format!("kc_{op}");
        if !tools.contains(&tool) {
            tools.push(tool);
        }
    }
    let cache = made(&explicit(args.cache_dir, "KNOWCODE_CACHE_DIR", "cache")?)?;
    let config = made(&explicit(args.config_dir, "KNOWCODE_CONFIG_DIR", "config")?)?;
    let data = made(&args.data_dir.or_else(|| std::env::var_os("KNOWCODE_DATA_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)).unwrap_or_else(|| cache.join("data")))?;
    for (var, value) in [
        ("KNOWCODE_CACHE_DIR", cache.as_os_str()),
        ("KNOWCODE_CONFIG_DIR", config.as_os_str()),
        ("KNOWCODE_DATA_DIR", data.as_os_str()),
        ("KNOWCODE_UPDATE_CHECK", "0".as_ref()),
        // git stays local: a partial clone's missing objects are not fetched, and no remote protocol is allowed.
        ("GIT_NO_LAZY_FETCH", "1".as_ref()),
        ("GIT_ALLOW_PROTOCOL", "file".as_ref()),
        ("GIT_TERMINAL_PROMPT", "0".as_ref()),
    ] {
        // SAFETY: the process has one thread yet: nothing reads the environment concurrently.
        unsafe { std::env::set_var(var, value) };
    }
    kc_index::config::bind(repos.clone());
    Ok(Restriction { repos, tools, cache, config, data })
}

impl Restriction {
    /// Whether `tool` (`kc_find`) may run.
    pub fn allows(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    /// Whether `path` is inside a bound repo.
    pub fn holds(&self, path: &Path) -> bool {
        let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.repos.iter().any(|r| p.starts_with(r))
    }

    /// The bound repo named `name` (its directory's name), or holding the path `name`.
    pub fn repo(&self, name: &str) -> Option<&PathBuf> {
        let path = Path::new(name);
        self.repos.iter().find(|r| crate::ops::repo_name(r) == name || (path.is_absolute() && self.holds(path) && path.starts_with(r)))
    }

    /// Refuses a call this server isn't allowed to answer: an op not in its allowlist, a `repo` (or `repos`) that
    /// isn't bound, or an absolute path outside the bound repos. `tool` is the tool called; for `kc`, its op.
    pub fn check(&self, tool: &str, args: Option<&serde_json::Map<String, Value>>) -> Result<(), Refusal> {
        let tool = if tool == "kc" {
            let op = args.and_then(|a| a.get("op")).and_then(Value::as_str).unwrap_or("").trim().trim_start_matches("kc_");
            format!("kc_{op}")
        } else {
            tool.to_string()
        };
        if !self.allows(&tool) {
            let ops: Vec<&str> = self.tools.iter().map(|t| t.trim_start_matches("kc_")).collect();
            return Err(Refusal::new(Code::NotAllowed, format!("op {} is not allowed by this server; it runs {}", tool.trim_start_matches("kc_"), ops.join(", "))));
        }
        let Some(args) = args else { return Ok(()) };
        // A cursor continues another op's answer: that op must be allowed too.
        if tool == "kc_more" {
            let cursor = args.get("cursor").or_else(|| args.get("q")).and_then(Value::as_str).unwrap_or("");
            let full = crate::compact::resolve(cursor).map_err(|e| Refusal::new(Code::InvalidArgument, e))?;
            let of = crate::cursor::tool_of(&full).map_err(|e| match e.downcast::<Refusal>() {
                Ok(r) => r,
                Err(e) => Refusal::new(Code::InvalidArgument, format!("{e:#}")),
            })?;
            if !self.allows(of) {
                return Err(Refusal::new(Code::NotAllowed, format!("the cursor continues op {}, which this server doesn't run", of.trim_start_matches("kc_"))));
            }
        }
        let mut repos: Vec<&str> = args.get("repo").and_then(Value::as_str).into_iter().collect();
        repos.extend(args.get("repos").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str));
        for r in repos {
            if self.repo(r).is_none() {
                return Err(Refusal::new(Code::NotAllowed, format!("repo {r} is not one this server answers about; it answers about {}", self.names().join(", "))));
            }
        }
        let mut strings = Vec::new();
        fn collect<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
            match v {
                Value::String(s) => out.push(s),
                Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
                Value::Object(m) => m.values().for_each(|x| collect(x, out)),
                _ => {}
            }
        }
        for (k, v) in args {
            if k != "repo" && k != "repos" {
                collect(v, &mut strings);
            }
        }
        for s in strings {
            let path = s.split(['#', ':']).next().unwrap_or(s);
            // A path that is there: a route (`/api/orders`) or a pattern is text.
            if path.starts_with('/') && Path::new(path).exists() && !self.holds(Path::new(path)) {
                return Err(Refusal::new(Code::NotAllowed, "a path outside the repos this server answers about"));
            }
        }
        Ok(())
    }

    /// Names each `repo` (and `repos`) argument [`check`](Self::check) let through by its bound repo's path, so a name
    /// is never read as a path relative to the current directory, where another repo of that name may be.
    pub fn resolve(&self, args: Option<&mut serde_json::Map<String, Value>>) {
        let Some(args) = args else { return };
        let path = |v: &mut Value| {
            if let Some(r) = v.as_str().and_then(|n| self.repo(n)) {
                *v = Value::from(r.to_string_lossy().into_owned());
            }
        };
        if let Some(v) = args.get_mut("repo") {
            path(v);
        }
        if let Some(Value::Array(a)) = args.get_mut("repos") {
            a.iter_mut().for_each(path);
        }
    }

    fn names(&self) -> Vec<String> {
        self.repos.iter().map(|r| crate::ops::repo_name(r)).collect()
    }

    /// The scope as the server states it.
    pub fn scope(&self) -> Value {
        json!({
            "restricted": true,
            "repos": self.repos.iter().map(|r| json!({ "name": crate::ops::repo_name(r), "path": r })).collect::<Vec<_>>(),
            "ops": self.tools.iter().map(|t| t.trim_start_matches("kc_")).collect::<Vec<_>>(),
            "cache_dir": self.cache,
            "config_dir": self.config,
            "data_dir": self.data,
            "discovery": false,
            "update_checks": false,
            "plugin_refresh": false,
            "git_network": false,
        })
    }
}
