//! The user config file: `config.toml` in [`crate::config_dir`].
//!
//! ```toml
//! [cache]
//! dir = "~/caches/knowcode"        # where the indexes live
//!
//! [notes]
//! path = "~/notes/knowcode.sqlite" # where notes on symbols live (default: the user data directory)
//!
//! [workspace]
//! roots = ["~/Code", "~/work"]     # every git repo under these is in the workspace
//! max_depth = 4                    # how many directories deep under a root a repo may be
//!
//! [ignore]                         # repo (directory name, absolute path or "*") = patterns
//! "*" = ["Generated"]
//! aether-engine = ["records/"]
//!
//! [privacy]                        # privacy file levels (see [`crate::privacy`])
//! block_at = 1                     # content at this level or below is never indexed or answered from
//! malformed = "restricted"         # a privacy file that can't be read: "restricted" (level 1) or "default" (level 3)
//! file = ".knowcode-privacy.yml"   # the privacy file's name, and its keys:
//! global_key = "level"             #   the whole repo's level
//! rules_key = "rules"              #   the list of path rules,
//! path_key = "path"                #   each with its path
//! level_key = "level"              #   and its level
//!
//! [conventions.csharp.bus]         # names the extractors recognise (see `kc_extract::conventions`)
//! message_attributes = ["BusMessage"]
//! ```
//!
//! Every `[conventions.*]` key replaces its default (see [`CONVENTION_KEYS`]); the C# and TypeScript ones are part of
//! those languages' extractor versions, so changing one re-extracts their files.
//!
//! A setting comes from its environment variable first (`KNOWCODE_CACHE_DIR`, `KNOWCODE_DATA_DIR`), then this file, then the legacy file
//! that held it before this one existed (`cache_dir` and `ignore` beside it, read only while this file doesn't set the
//! key), then the default. A key this build doesn't know is warned about once and otherwise ignored, so a file
//! written for a newer build still loads; a known key with a bad value is an error naming the file and the key.
//! Nothing here is ever written into an indexed repo.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use toml_edit::{Array, DocumentMut, Item, Value};

/// The config file's name in [`crate::config_dir`].
pub const FILE_NAME: &str = "config.toml";
/// The legacy one-line file naming the cache directory.
pub const LEGACY_CACHE_DIR: &str = "cache_dir";
/// The legacy user-level ignore file: `<repo> <pattern>` per line.
pub const LEGACY_IGNORE: &str = "ignore";
/// How many directories deep under a workspace root a repo may be, unless `workspace.max_depth` says otherwise.
pub const DEFAULT_MAX_DEPTH: usize = 4;
/// Default managed index budget: two GiB. Zero disables size eviction (deleted checkouts are still pruned).
pub const DEFAULT_MAX_CACHE_SIZE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_DEPTH_LIMIT: i64 = 32;

/// The keys `knowcode config get|set|unset` take, besides `ignore.<repo>`.
pub const KEYS: &[&str] = &[
    "cache.dir",
    "cache.max_size",
    "notes.path",
    "workspace.roots",
    "workspace.max_depth",
    "privacy.block_at",
    "privacy.malformed",
    "privacy.file",
    "privacy.global_key",
    "privacy.rules_key",
    "privacy.path_key",
    "privacy.level_key",
    "update.check",
    "conventions.csharp.bus.message_attributes",
    "conventions.csharp.bus.consumer_bases",
    "conventions.csharp.bus.consumer_registrations",
    "conventions.csharp.bus.publish",
    "conventions.csharp.http.request_attributes",
    "conventions.csharp.http.gateway_calls",
    "conventions.csharp.http.service_host_bases",
    "conventions.csharp.di.scope_lifetimes",
    "conventions.typescript.http.request_decorators",
    "conventions.typescript.http.path_keys",
    "conventions.typescript.http.verb_keys",
    "conventions.ci.app_name_variables",
    "conventions.ansible.infra_names",
    "conventions.ansible.name_prefixes",
];
/// The `[conventions.*]` keys among [`KEYS`]: each a list (`scope_lifetimes` a table) replacing its default.
pub const CONVENTION_KEYS: &[&str] = KEYS.split_at(13).1;
/// The notes database's file name, in the data directory by default.
pub const NOTES_FILE: &str = "notes.sqlite";

/// What `config.toml` sets. Paths are `~`-expanded, otherwise as written.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    /// `[update] check`: daily release notification, enabled by default.
    pub update_check: Option<bool>,
    /// `[cache] dir`.
    pub cache_dir: Option<PathBuf>,
    /// `[cache] max_size`, in bytes; zero disables size eviction.
    pub max_cache_size: Option<u64>,
    /// `[notes] path`.
    pub notes_path: Option<PathBuf>,
    /// `[workspace] roots`.
    pub roots: Vec<PathBuf>,
    /// `[workspace] max_depth`.
    pub max_depth: Option<usize>,
    /// `[ignore]`: each repo key with its patterns, in file order; `None` when the file has no `[ignore]` table.
    pub ignore: Option<Vec<(String, Vec<String>)>>,
    /// `[privacy] block_at`: the AI privacy level at or below which content is blocked (see [`crate::privacy`]).
    pub block_at: Option<u8>,
    /// `[privacy] malformed`: how a privacy file that can't be read is taken.
    pub malformed: Option<crate::privacy::Malformed>,
    /// `[privacy] file`, `global_key`, `rules_key`, `path_key`, `level_key` (in [`PRIVACY_FORMAT_KEYS`] order): the
    /// privacy file's name and keys, `None` where not set.
    pub privacy_file: [Option<String>; 5],
    /// `[conventions.*]`, defaults filled in.
    pub conventions: kc_extract::conventions::Conventions,
    /// The `[conventions.*]` keys the file sets, dotted.
    pub conventions_set: Vec<String>,
    /// Keys this build doesn't know, dotted.
    pub unknown: Vec<String>,
}

impl Config {
    /// Parses and validates the text of a config file; `path` names it in errors.
    pub fn parse(text: &str, path: &Path) -> Result<Config> {
        let doc: DocumentMut = text.parse().map_err(|e| anyhow!("{}: not valid TOML: {e}", path.display()))?;
        Config::from_doc(&doc, path)
    }

    fn from_doc(doc: &DocumentMut, path: &Path) -> Result<Config> {
        let bad = |key: &str, what: &str| anyhow!("{}: `{key}` {what}", path.display());
        let table = |key: &str, item: &'_ Item| item.as_table_like().map(|t| t.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<Vec<_>>()).ok_or_else(|| bad(key, "must be a table"));
        let mut c = Config::default();
        for (name, item) in doc.iter() {
            match name {
                "update" => {
                    for (k, v) in table("update", item)? {
                        match k.as_str() {
                            "check" => c.update_check = Some(v.as_bool().ok_or_else(|| bad("update.check", "must be true or false"))?),
                            _ => c.unknown.push(format!("update.{k}")),
                        }
                    }
                }
                "cache" => {
                    for (k, v) in table("cache", item)? {
                        match k.as_str() {
                            "dir" => c.cache_dir = Some(path_value(&v).map_err(|w| bad("cache.dir", w))?),
                            "max_size" => c.max_cache_size = Some(size_value(&v).ok_or_else(|| bad("cache.max_size", "must be nonnegative bytes or a size such as \"2 GiB\" (0 disables size eviction)"))?),
                            _ => c.unknown.push(format!("cache.{k}")),
                        }
                    }
                }
                "notes" => {
                    for (k, v) in table("notes", item)? {
                        match k.as_str() {
                            "path" => c.notes_path = Some(path_value(&v).map_err(|w| bad("notes.path", w))?),
                            _ => c.unknown.push(format!("notes.{k}")),
                        }
                    }
                }
                "workspace" => {
                    for (k, v) in table("workspace", item)? {
                        match k.as_str() {
                            "roots" => {
                                let arr = v.as_array().ok_or_else(|| bad("workspace.roots", "must be an array of directory paths"))?;
                                for r in arr.iter() {
                                    c.roots.push(path_value(&Item::Value(r.clone())).map_err(|w| bad("workspace.roots", w))?);
                                }
                            }
                            "max_depth" => {
                                let d = v.as_integer().filter(|d| (0..=MAX_DEPTH_LIMIT).contains(d));
                                c.max_depth = Some(d.ok_or_else(|| bad("workspace.max_depth", &format!("must be a whole number from 0 to {MAX_DEPTH_LIMIT}")))? as usize);
                            }
                            _ => c.unknown.push(format!("workspace.{k}")),
                        }
                    }
                }
                "privacy" => {
                    for (k, v) in table("privacy", item)? {
                        match k.as_str() {
                            "block_at" => {
                                let n = v.as_integer().filter(|n| (0..=4).contains(n));
                                c.block_at = Some(n.ok_or_else(|| bad("privacy.block_at", "must be a whole number from 0 (block nothing) to 4"))? as u8);
                            }
                            "malformed" => {
                                let m = v.as_str().and_then(crate::privacy::Malformed::parse);
                                c.malformed = Some(m.ok_or_else(|| bad("privacy.malformed", "must be \"restricted\" or \"default\""))?);
                            }
                            k if PRIVACY_FORMAT_KEYS.contains(&k) => {
                                let key = format!("privacy.{k}");
                                let text = v.as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(&key, "must be a nonempty string"))?;
                                if k == "file" && (text.contains(['/', '\\']) || matches!(text, "." | "..")) {
                                    return Err(bad(&key, "must be a file name (no directory)"));
                                }
                                let i = PRIVACY_FORMAT_KEYS.iter().position(|n| *n == k).unwrap_or_default();
                                c.privacy_file[i] = Some(text.to_string());
                            }
                            _ => c.unknown.push(format!("privacy.{k}")),
                        }
                    }
                }
                "conventions" => conventions(item, "conventions", &mut c, &bad)?,
                "ignore" => {
                    let mut rules = Vec::new();
                    for (repo, v) in table("ignore", item)? {
                        let key = format!("ignore.{repo}");
                        let what = "must be an array of patterns";
                        let arr = v.as_array().ok_or_else(|| bad(&key, what))?;
                        let patterns = arr.iter().map(|p| p.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())).collect::<Option<Vec<_>>>();
                        rules.push((repo, patterns.ok_or_else(|| bad(&key, what))?));
                    }
                    c.ignore = Some(rules);
                }
                _ => c.unknown.push(name.to_string()),
            }
        }
        let f = c.privacy_format();
        if f.global_key == f.rules_key {
            return Err(bad("privacy.rules_key", "must differ from privacy.global_key"));
        }
        if f.path_key == f.level_key {
            return Err(bad("privacy.level_key", "must differ from privacy.path_key"));
        }
        Ok(c)
    }

    /// The privacy file's name and keys: as set, else the defaults.
    pub fn privacy_format(&self) -> crate::privacy::Format {
        let d = crate::privacy::Format::default();
        let [file, global, rules, path, level] = self.privacy_file.clone();
        crate::privacy::Format {
            file: file.unwrap_or(d.file),
            global_key: global.unwrap_or(d.global_key),
            rules_key: rules.unwrap_or(d.rules_key),
            path_key: path.unwrap_or(d.path_key),
            level_key: level.unwrap_or(d.level_key),
        }
    }
}

/// The `[privacy]` keys naming the privacy file and its keys, in [`Config::privacy_file`]'s order.
pub const PRIVACY_FORMAT_KEYS: [&str; 5] = ["file", "global_key", "rules_key", "path_key", "level_key"];

/// Reads `[conventions.*]` (the table `item` at the dotted `at`) into `c`.
fn conventions(item: &Item, at: &str, c: &mut Config, bad: &dyn Fn(&str, &str) -> anyhow::Error) -> Result<()> {
    use kc_extract::conventions::{self as kc, Arg, Call};
    let t = item.as_table_like().ok_or_else(|| bad(at, "must be a table"))?;
    for (k, v) in t.iter() {
        let key = format!("{at}.{k}");
        if !CONVENTION_KEYS.contains(&key.as_str()) {
            let prefix = format!("{key}.");
            if v.is_table_like() && CONVENTION_KEYS.iter().any(|c| c.starts_with(&prefix)) {
                conventions(v, &key, c, bad)?;
            } else {
                c.unknown.push(key);
            }
            continue;
        }
        let what = "must be an array of strings";
        let list = || -> Result<Vec<String>> {
            let arr = v.as_array().ok_or_else(|| bad(&key, what))?;
            arr.iter().map(|x| x.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or_else(|| bad(&key, what))).collect()
        };
        let names = |dots: bool| -> Result<Vec<String>> {
            let l = list()?;
            if let Some(n) = l.iter().find(|n| !n.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || (dots && ch == '.'))) {
                return Err(bad(&key, &format!("has `{n}`, which is not a name (letters, digits and _)")));
            }
            Ok(l)
        };
        let calls = |required: &[Arg], allowed: &[Arg]| -> Result<Vec<Call>> {
            list()?.iter().map(|s| Call::parse(s, required, allowed).map_err(|e| bad(&key, &format!("has {e}")))).collect()
        };
        let conv = &mut c.conventions;
        match key.rsplit_once('.').map_or("", |(_, n)| n) {
            "message_attributes" => conv.bus.message_attributes = names(false)?,
            "consumer_bases" => conv.bus.consumer_bases = names(false)?,
            "consumer_registrations" => conv.bus.consumer_registrations = names(false)?,
            "publish" => conv.bus.publish = calls(&[Arg::Message], kc::BUS_ROLES)?,
            "request_attributes" => conv.csharp_http.request_attributes = calls(kc::REQUEST_ROLES, &[])?,
            "gateway_calls" => conv.csharp_http.gateway_calls = calls(kc::REQUEST_ROLES, &[])?,
            "service_host_bases" => conv.csharp_http.service_host_bases = names(false)?,
            "scope_lifetimes" => {
                let what = format!("must be a table of scope values to lifetimes ({})", kc::LIFETIMES.join(", "));
                let table = v.as_table_like().ok_or_else(|| bad(&key, &what))?;
                let mut out = Vec::new();
                for (name, l) in table.iter() {
                    let life = l.as_str().and_then(|l| kc::LIFETIMES.iter().find(|x| **x == l)).ok_or_else(|| bad(&key, &what))?;
                    if !name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                        return Err(bad(&key, &format!("has `{name}`, which is not a name (letters, digits and _)")));
                    }
                    out.push((name.to_string(), *life));
                }
                conv.di.scope_lifetimes = out;
            }
            "request_decorators" => conv.ts_http.request_decorators = names(true)?,
            "path_keys" => conv.ts_http.path_keys = names(false)?,
            "verb_keys" => conv.ts_http.verb_keys = names(false)?,
            "app_name_variables" => conv.ci.app_name_variables = names(false)?,
            "infra_names" => conv.ansible.infra_names = list()?.iter().map(|n| name_key(n)).filter(|n| !n.is_empty()).collect(),
            "name_prefixes" => conv.ansible.name_prefixes = list()?.iter().map(|n| name_key(n)).filter(|n| !n.is_empty()).collect(),
            _ => {}
        }
        c.conventions_set.push(key);
    }
    Ok(())
}

/// A name as Ansible linking compares it: lower case, letters and digits only.
pub fn name_key(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// [`kc_extract::conventions::synthetic`] as config: the conventions of a made-up code base, which tests that index
/// fixtures written in them put in their config file.
#[doc(hidden)]
pub const SYNTHETIC_CONVENTIONS: &str = r#"[conventions.csharp.bus]
message_attributes = ["ShopMessage"]
consumer_bases = ["ShopConsumer", "SecuredShopConsumer", "IConsumer", "IHandleMessages"]
consumer_registrations = ["AddConsumer", "AddShopConsumer", "AddSecuredShopConsumer"]
publish = ["Publish(message)", "PublishToHost(message, host)", "SendToQueue(message, host, queue)", "Send(message)"]

[conventions.csharp.http]
request_attributes = ["ShopRequest(service, path, verb)"]
gateway_calls = ["CallService(service, path, verb)", "CallServiceAsync(service, path, verb)", "SendToApi(verb, service, path)", "SendToApiAsync(verb, service, path)"]
service_host_bases = ["ShopServiceStartup"]

[conventions.csharp.di]
scope_lifetimes = { PerJob = "unit_of_work" }

[conventions.typescript.http]
request_decorators = ["RequestContract"]
path_keys = ["endpoint"]
verb_keys = ["httpMethod"]
"#;

/// The conventions the config sets, defaults filled in (the defaults when the file can't be read: an error there is
/// reported by whatever reads the file for a setting that must be valid).
pub fn conventions_now() -> std::sync::Arc<kc_extract::conventions::Conventions> {
    let _ = load();
    kc_extract::conventions::current()
}

/// A path setting's value: a string naming an absolute path, or one starting with `~/`.
fn path_value(item: &Item) -> Result<PathBuf, &'static str> {
    let s = item.as_str().ok_or("must be a string (a path)")?;
    expand_tilde(s).filter(|p| p.is_absolute()).ok_or("must be an absolute path or start with ~/")
}

fn size_value(item: &Item) -> Option<u64> {
    if let Some(n) = item.as_integer() { return u64::try_from(n).ok(); }
    let text = item.as_str()?.trim();
    let end = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let value: u64 = text[..end].parse().ok()?;
    let multiplier = match text[end..].trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" => 1_000, "KIB" => 1_024,
        "M" | "MB" => 1_000_000, "MIB" => 1_048_576,
        "G" | "GB" => 1_000_000_000, "GIB" => 1_073_741_824,
        "T" | "TB" => 1_000_000_000_000, "TIB" => 1_099_511_627_776,
        _ => return None,
    };
    value.checked_mul(multiplier)
}

/// Opt-out for automatic release checks. Invalid settings never authorize network access.
pub fn update_check() -> Result<(bool, Source)> {
    if let Some(value) = std::env::var_os("KNOWCODE_UPDATE_CHECK") {
        return match value.to_str().map(str::to_ascii_lowercase).as_deref() {
            Some("0" | "false") => Ok((false, Source::Env)),
            Some("1" | "true") => Ok((true, Source::Env)),
            _ => bail!("KNOWCODE_UPDATE_CHECK must be 0, 1, false, or true"),
        };
    }
    match load()?.config.update_check {
        Some(value) => Ok((value, Source::Config)), None => Ok((true, Source::Default)),
    }
}

/// Effective managed index budget and its source. Zero disables only size eviction.
pub fn max_cache_size() -> Result<(u64, Source)> {
    Ok(match load()?.config.max_cache_size {
        Some(n) => (n, Source::Config),
        None => (DEFAULT_MAX_CACHE_SIZE, Source::Default),
    })
}

/// `~` and `~/rest` under the home directory; other paths as they are.
pub fn expand_tilde(s: &str) -> Option<PathBuf> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s == "~" {
        return dirs::home_dir();
    }
    match s.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)),
        None => Some(PathBuf::from(s)),
    }
}

/// The config file and what it sets.
#[derive(Debug)]
pub struct Loaded {
    pub path: PathBuf,
    /// Whether the file exists; when it doesn't, `config` is empty.
    pub exists: bool,
    pub config: Config,
}

/// The file's path, its stamp when last read, and what it held, so a long-running server picks up an edit.
static LOADED: Mutex<Option<(PathBuf, String, Arc<Loaded>)>> = Mutex::new(None);

/// Unknown keys already warned about in this process.
static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// The path of `config.toml`.
pub fn path() -> Result<PathBuf> {
    Ok(crate::config_dir()?.join(FILE_NAME))
}

/// The config file, read again only when it changes. No file is an empty config; a file that doesn't parse or holds a
/// bad value is an error.
pub fn load() -> Result<Arc<Loaded>> {
    let Ok(path) = path() else {
        return Ok(Arc::new(Loaded { path: PathBuf::new(), exists: false, config: Config::default() }));
    };
    let stamp = crate::git::stamp(&path);
    let mut memo = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, s, loaded)) = memo.as_ref()
        && *p == path
        && *s == stamp
    {
        return Ok(loaded.clone());
    }
    let loaded = match std::fs::read_to_string(&path) {
        Ok(text) => Loaded { config: Config::parse(&text, &path)?, path: path.clone(), exists: true },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded { path: path.clone(), exists: false, config: Config::default() },
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    // What the extractors recognise by name comes from here (see `kc_extract::conventions`).
    kc_extract::conventions::set(loaded.config.conventions.clone());
    let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    for key in &loaded.config.unknown {
        if warned.get_or_insert_with(HashSet::new).insert(format!("{}|{key}", path.display())) {
            eprintln!("knowcode: {}: unknown key `{key}`, ignored", path.display());
        }
    }
    let loaded = Arc::new(loaded);
    *memo = Some((path, stamp, loaded.clone()));
    Ok(loaded)
}

/// Where a setting's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Its environment variable.
    Env,
    /// `config.toml`.
    Config,
    /// The legacy file that held it before `config.toml`.
    Legacy,
    /// Not set anywhere.
    Default,
}

impl Source {
    /// How `status` and `config list` name it.
    pub fn describe(self, env: &str, legacy: &str) -> String {
        match self {
            Source::Env => format!("environment ({env})"),
            Source::Config => FILE_NAME.to_string(),
            Source::Legacy => format!("legacy file `{legacy}`"),
            Source::Default => "default".to_string(),
        }
    }
}

/// The cache directory and where it came from: `KNOWCODE_CACHE_DIR`, else `cache.dir`, else the legacy `cache_dir`
/// file, else the user cache directory.
pub fn cache_dir() -> Result<(PathBuf, Source)> {
    let env = std::env::var_os("KNOWCODE_CACHE_DIR");
    if env.is_some() {
        // The variable wins without reading the config file, as before the file existed.
        return Ok(resolve_cache_dir(env, &Config::default(), || None, PathBuf::new));
    }
    let loaded = load()?;
    let legacy = || crate::config_dir().ok().and_then(|d| std::fs::read_to_string(d.join(LEGACY_CACHE_DIR)).ok());
    let default = || dirs::cache_dir().map(|d| d.join("dev.bubstack.knowcode")).unwrap_or_default();
    let (dir, source) = resolve_cache_dir(None, &loaded.config, legacy, default);
    if dir.as_os_str().is_empty() {
        bail!("no user cache directory");
    }
    Ok((dir, source))
}

/// [`cache_dir`]'s precedence, given its inputs: the variable, the config, the legacy file's text (read only when the
/// config doesn't set the key) and the default.
pub fn resolve_cache_dir(
    env: Option<std::ffi::OsString>,
    config: &Config,
    legacy: impl FnOnce() -> Option<String>,
    default: impl FnOnce() -> PathBuf,
) -> (PathBuf, Source) {
    if let Some(dir) = env {
        return (PathBuf::from(dir), Source::Env);
    }
    if let Some(dir) = &config.cache_dir {
        return (dir.clone(), Source::Config);
    }
    if let Some(dir) = legacy().as_deref().and_then(legacy_path) {
        return (dir, Source::Legacy);
    }
    (default(), Source::Default)
}

/// The notes database and where its path came from: `notes.sqlite` in `KNOWCODE_DATA_DIR`, else `notes.path`, else
/// `notes.sqlite` in the user data directory (`~/Library/Application Support/dev.bubstack.knowcode` on macOS). Notes
/// are written by people and agents, not derived from the code, so they live apart from the cache: clearing the cache
/// (by hand, by the OS, or a new machine without it) never loses them.
pub fn notes_path() -> Result<(PathBuf, Source)> {
    let env = std::env::var_os("KNOWCODE_DATA_DIR");
    let config = if env.is_some() { Config::default() } else { load()?.config.clone() };
    match resolve_notes_path(env, &config, || crate::data_dir().ok()) {
        (Some(path), source) => Ok((path, source)),
        (None, _) => bail!("no user data directory: set notes.path in {FILE_NAME}"),
    }
}

/// [`notes_path`]'s precedence, given its inputs: the variable (a directory), the config (a file), the default
/// directory.
pub fn resolve_notes_path(
    env: Option<std::ffi::OsString>,
    config: &Config,
    default_dir: impl FnOnce() -> Option<PathBuf>,
) -> (Option<PathBuf>, Source) {
    if let Some(dir) = env {
        return (Some(PathBuf::from(dir).join(NOTES_FILE)), Source::Env);
    }
    if let Some(path) = &config.notes_path {
        return (Some(path.clone()), Source::Config);
    }
    (default_dir().map(|d| d.join(NOTES_FILE)), Source::Default)
}

/// The first line of a legacy one-line file that isn't blank or a `#` comment, as written.
fn legacy_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#'))
}

/// The path the legacy `cache_dir` file names, `~/` expanded.
fn legacy_path(text: &str) -> Option<PathBuf> {
    expand_tilde(legacy_line(text)?)
}

/// The user-level ignore rules, as `(repo, pattern)` pairs, and where they came from: `[ignore]` in `config.toml`,
/// else the legacy `ignore` file.
pub fn user_ignore() -> Result<(Vec<(String, String)>, Source)> {
    let loaded = load()?;
    if let Some(rules) = &loaded.config.ignore {
        return Ok((config_ignore_rules(rules), Source::Config));
    }
    match crate::config_dir().ok().and_then(|d| std::fs::read_to_string(d.join(LEGACY_IGNORE)).ok()) {
        Some(text) => Ok((legacy_ignore_rules(&text), Source::Legacy)),
        None => Ok((Vec::new(), Source::Default)),
    }
}

/// What the user-level ignore rules depend on, for the discovery cache: the legacy file's stamp (as before the config
/// file existed), or a hash of the rules when the config file holds them.
pub fn user_ignore_stamp() -> String {
    match load() {
        Ok(loaded) => match &loaded.config.ignore {
            Some(rules) => format!("config:{}", &blake3::hash(format!("{:?}", config_ignore_rules(rules)).as_bytes()).to_hex()[..16]),
            None => crate::config_dir().map(|d| crate::git::stamp(&d.join(LEGACY_IGNORE))).unwrap_or_default(),
        },
        Err(_) => "error".to_string(),
    }
}

/// `[ignore]` as `(repo, pattern)` pairs; a repo key starting with `~/` is expanded.
pub fn config_ignore_rules(rules: &[(String, Vec<String>)]) -> Vec<(String, String)> {
    rules
        .iter()
        .flat_map(|(repo, patterns)| {
            let repo = if repo.starts_with("~/") { expand_tilde(repo).map_or(repo.clone(), |p| p.to_string_lossy().into_owned()) } else { repo.clone() };
            patterns.iter().map(move |p| (repo.clone(), p.clone()))
        })
        .collect()
}

/// The legacy `ignore` file as `(repo, pattern)` pairs: one `<repo> <pattern>` per line, `#` comments; a line without
/// a pattern is skipped.
pub fn legacy_ignore_rules(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(char::is_whitespace))
        .map(|(repo, pattern)| (repo.to_string(), pattern.trim().to_string()))
        .collect()
}

/// The AI privacy settings (see [`crate::privacy`]): the level at or below which content is blocked (default
/// [`crate::privacy::DEFAULT_BLOCK_AT`]), how a malformed privacy file is taken, and where they came from. A config
/// file that can't be read gives the defaults, so an error there never blocks less.
pub fn privacy() -> (u8, crate::privacy::Malformed, Source) {
    match load() {
        Ok(l) => {
            let source = if l.config.block_at.is_some() || l.config.malformed.is_some() { Source::Config } else { Source::Default };
            (l.config.block_at.unwrap_or(crate::privacy::DEFAULT_BLOCK_AT), l.config.malformed.unwrap_or_default(), source)
        }
        Err(_) => (crate::privacy::DEFAULT_BLOCK_AT, crate::privacy::Malformed::default(), Source::Default),
    }
}

/// The privacy file's name and keys (see [`crate::privacy::Format`]). A config file that can't be read gives the
/// defaults.
pub fn privacy_format() -> crate::privacy::Format {
    load().map(|l| l.config.privacy_format()).unwrap_or_default()
}

/// The configured workspace roots (`~` expanded, as written otherwise) and how deep under them repos are looked for.
pub fn workspace() -> Result<(Vec<PathBuf>, usize)> {
    let loaded = load()?;
    Ok((loaded.config.roots.clone(), loaded.config.max_depth.unwrap_or(DEFAULT_MAX_DEPTH)))
}

// Editing. The file is edited in place, keeping its comments and layout, and checked before it is written.

fn read_doc(path: &Path) -> Result<DocumentMut> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.parse().map_err(|e| anyhow!("{}: not valid TOML: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Checks `doc` and writes it to `path` whole, renamed into place.
fn write_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    Config::from_doc(doc, path)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("toml.{}", std::process::id()));
    std::fs::write(&tmp, doc.to_string()).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Applies `f` to the config file and writes it back if `f` says it changed anything. Returns `f`'s answer.
fn edit<T>(f: impl FnOnce(&mut DocumentMut) -> Result<(bool, T)>) -> Result<T> {
    let path = path()?;
    let mut doc = read_doc(&path)?;
    let (changed, out) = f(&mut doc)?;
    if changed {
        write_doc(&path, &doc)?;
    }
    Ok(out)
}

/// A key `get`, `set` and `unset` take, split into its table (dotted, for a nested one) and name: one of [`KEYS`],
/// or `ignore.<repo>`.
fn split_key(key: &str) -> Result<(String, String)> {
    if let Some(repo) = key.strip_prefix("ignore.").filter(|r| !r.is_empty()) {
        return Ok(("ignore".into(), repo.to_string()));
    }
    match key.rsplit_once('.') {
        Some((table, name)) if KEYS.contains(&key) => Ok((table.to_string(), name.to_string())),
        _ => bail!("unknown key `{key}`: the keys are {}, and ignore.<repo>", KEYS.join(", ")),
    }
}

/// The table at the dotted path `name` in `doc`, created if missing (a nested one implicit, as `[a.b]` makes `a`);
/// an error if a part of it holds something else.
fn table_mut<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut dyn toml_edit::TableLike> {
    let not = || anyhow!("`{name}` in {FILE_NAME} is not a table");
    let mut parts = name.split('.');
    let first = parts.next().unwrap_or_default();
    let mut t = doc.entry(first).or_insert_with(|| implicit_table(name != first)).as_table_like_mut().ok_or_else(not)?;
    let mut at = first.to_string();
    for p in parts {
        at = format!("{at}.{p}");
        let nested = at != name;
        t = t.entry(p).or_insert_with(|| implicit_table(nested)).as_table_like_mut().ok_or_else(not)?;
    }
    Ok(t)
}

/// A new table; an implicit one (only holding tables) isn't written as a header of its own.
fn implicit_table(implicit: bool) -> Item {
    let mut t = toml_edit::Table::new();
    t.set_implicit(implicit);
    Item::Table(t)
}

/// The table at the dotted path `name` in `doc`, if there is one.
fn table_get_mut<'a>(doc: &'a mut DocumentMut, name: &str) -> Option<&'a mut dyn toml_edit::TableLike> {
    let mut parts = name.split('.');
    let mut t = doc.get_mut(parts.next()?)?.as_table_like_mut()?;
    for p in parts {
        t = t.get_mut(p)?.as_table_like_mut()?;
    }
    Some(t)
}

/// Sets `key` to `raw`: a TOML value (`4`, `["a", "b"]`, `"x"`), or else taken as a string. A string given for a key
/// holding a list becomes a list of one.
pub fn set(key: &str, raw: &str) -> Result<()> {
    let (table, name) = split_key(key)?;
    let mut value: Value = format!("v = {raw}")
        .parse::<DocumentMut>()
        .ok()
        .and_then(|d| d.get("v").and_then(|i| i.as_value().cloned()))
        .unwrap_or_else(|| raw.into());
    let list = matches!(key, "workspace.roots") || table == "ignore" || (CONVENTION_KEYS.contains(&key) && !key.ends_with(".scope_lifetimes"));
    if list && !value.is_array() {
        value = Value::Array(Array::from_iter([value]));
    }
    value.decor_mut().clear();
    edit(|doc| {
        table_mut(doc, &table)?.insert(&name, Item::Value(value));
        Ok((true, ()))
    })
}

/// Removes `key` (and its tables, once empty). Returns whether it was set.
pub fn unset(key: &str) -> Result<bool> {
    let (table, name) = split_key(key)?;
    edit(|doc| {
        let Some(t) = table_get_mut(doc, &table) else { return Ok((false, false)) };
        let removed = t.remove(&name).is_some();
        // Empty tables go, innermost first.
        let mut at = table.as_str();
        loop {
            let (parent, last) = match at.rsplit_once('.') {
                Some((p, l)) => (Some(p), l),
                None => (None, at),
            };
            let empty = |t: &dyn toml_edit::TableLike| t.get(last).and_then(Item::as_table_like).is_some_and(|x| x.is_empty());
            match parent {
                Some(p) => match table_get_mut(doc, p) {
                    Some(pt) if empty(&*pt) => {
                        pt.remove(last);
                    }
                    _ => break,
                },
                None => {
                    if doc.get(last).and_then(Item::as_table_like).is_some_and(|x| x.is_empty()) {
                        doc.remove(last);
                    }
                    break;
                }
            }
            at = parent.unwrap_or_default();
        }
        Ok((removed, removed))
    })
}

/// Adds `dir` (made absolute, links resolved) to `workspace.roots`. Returns the root as stored, and whether it was
/// new.
pub fn add_root(dir: &Path) -> Result<(PathBuf, bool)> {
    let dir = std::fs::canonicalize(dir).with_context(|| format!("resolving {}", dir.display()))?;
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    edit(|doc| {
        let ws = table_mut(doc, "workspace")?;
        let roots = ws.entry("roots").or_insert(Item::Value(Value::Array(Array::new())));
        let arr = roots.as_array_mut().ok_or_else(|| anyhow!("`workspace.roots` in {FILE_NAME} is not an array"))?;
        if arr.iter().any(|v| v.as_str().is_some_and(|s| same_dir(s, &dir))) {
            return Ok((false, (dir.clone(), false)));
        }
        arr.push(dir.to_string_lossy().as_ref());
        Ok((true, (dir.clone(), true)))
    })
}

/// Removes `dir` from `workspace.roots`, matched as written or by where it leads. Returns the entries removed.
pub fn remove_root(dir: &Path) -> Result<Vec<String>> {
    let given = expand_tilde(&dir.to_string_lossy()).unwrap_or_else(|| dir.to_path_buf());
    let given = if given.is_absolute() { given } else { std::env::current_dir()?.join(given) };
    let canonical = std::fs::canonicalize(&given).unwrap_or(given.clone());
    edit(|doc| {
        let Some(ws) = doc.get_mut("workspace").and_then(Item::as_table_like_mut) else { return Ok((false, Vec::new())) };
        let Some(arr) = ws.get_mut("roots").and_then(Item::as_array_mut) else { return Ok((false, Vec::new())) };
        let mut removed = Vec::new();
        arr.retain(|v| {
            let hit = v.as_str().is_some_and(|s| expand_tilde(s).is_some_and(|p| p == given) || same_dir(s, &canonical));
            if hit {
                removed.push(v.as_str().unwrap_or_default().to_string());
            }
            !hit
        });
        if arr.is_empty() {
            ws.remove("roots");
        }
        if ws.is_empty() {
            doc.remove("workspace");
        }
        Ok((!removed.is_empty(), removed))
    })
}

/// Whether the configured path `s` leads to the directory `canonical`.
fn same_dir(s: &str, canonical: &Path) -> bool {
    expand_tilde(s).is_some_and(|p| p == canonical || std::fs::canonicalize(&p).is_ok_and(|c| c == canonical))
}

/// Folds the legacy `cache_dir` and `ignore` files into `config.toml` and renames them to `*.migrated`. A key the
/// config file already sets is kept, since it already overrides the legacy file. Returns a line per thing done.
pub fn migrate() -> Result<Vec<String>> {
    let dir = crate::config_dir()?;
    let path = dir.join(FILE_NAME);
    let mut doc = read_doc(&path)?;
    let config = Config::from_doc(&doc, &path)?;
    let mut done = Vec::new();
    let mut moved = Vec::new();

    let legacy_cache = dir.join(LEGACY_CACHE_DIR);
    if let Ok(text) = std::fs::read_to_string(&legacy_cache) {
        match (legacy_line(&text), &config.cache_dir) {
            (Some(line), None) => {
                table_mut(&mut doc, "cache")?.insert("dir", toml_edit::value(line));
                done.push(format!("cache.dir = {line:?} (from {LEGACY_CACHE_DIR})"));
            }
            (Some(line), Some(set)) => done.push(format!("kept cache.dir = {:?}; {LEGACY_CACHE_DIR} named {line:?}, which it overrides", set.display())),
            (None, _) => done.push(format!("{LEGACY_CACHE_DIR} names no directory")),
        }
        moved.push(legacy_cache);
    }

    let legacy_ignore = dir.join(LEGACY_IGNORE);
    if let Ok(text) = std::fs::read_to_string(&legacy_ignore) {
        let rules = legacy_ignore_rules(&text);
        let lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).count();
        if config.ignore.is_some() {
            done.push(format!("kept [ignore]; {LEGACY_IGNORE}'s {} rules, which it overrides, were not copied", rules.len()));
        } else {
            let table = table_mut(&mut doc, "ignore")?;
            for (repo, pattern) in &rules {
                let item = table.entry(repo).or_insert(Item::Value(Value::Array(Array::new())));
                if let Some(arr) = item.as_array_mut() {
                    arr.push(pattern.as_str());
                }
            }
            done.push(format!("{} ignore rules (from {LEGACY_IGNORE})", rules.len()));
        }
        if lines > rules.len() {
            let n = lines - rules.len();
            done.push(format!("{n} line{} of {LEGACY_IGNORE} without a pattern dropped (they never applied)", if n == 1 { "" } else { "s" }));
        }
        moved.push(legacy_ignore);
    }

    if moved.is_empty() {
        return Ok(vec!["nothing to migrate".to_string()]);
    }
    write_doc(&path, &doc)?;
    for old in moved {
        let to = PathBuf::from(format!("{}.migrated", old.display()));
        std::fs::rename(&old, &to).with_context(|| format!("renaming {}", old.display()))?;
        done.push(format!("renamed {} to {}", old.display(), to.display()));
    }
    done.push(format!("wrote {}", path.display()));
    Ok(done)
}

#[cfg(test)]
mod tests {
    #[test]
    fn cache_sizes_accept_bytes_and_units_and_refuse_invalid_or_overflowing_values() {
        for (text, bytes) in [("0", 0), ("1024", 1024), ("\"2 GiB\"", 2_147_483_648), ("\"10G\"", 10_000_000_000), ("\"1 mb\"", 1_000_000)] {
            let config = super::Config::parse(&format!("[cache]\nmax_size = {text}\n"), std::path::Path::new("/cfg/config.toml")).unwrap();
            assert_eq!(config.max_cache_size, Some(bytes));
        }
        for text in ["-1", "true", "\"1.5 GiB\"", "\"unlimited\"", "\"18446744073709551615 GiB\""] {
            let err = super::Config::parse(&format!("[cache]\nmax_size = {text}\n"), std::path::Path::new("/cfg/config.toml")).unwrap_err();
            assert!(err.to_string().contains("cache.max_size"));
        }
    }
    use super::*;

    fn parse(text: &str) -> Result<Config> {
        Config::parse(text, Path::new("/cfg/config.toml"))
    }

    #[test]
    fn a_config_file_parses_to_its_settings() {
        let c = parse(
            "# mine\n[cache]\ndir = \"/tmp/kc\"\n[workspace]\nroots = [\"/src\", \"~/Code\"]\nmax_depth = 2\n[ignore]\n\"*\" = [\"Generated\"]\napp = [\"docs/\", \"x y\"]\n",
        )
        .unwrap();
        assert_eq!(c.cache_dir, Some("/tmp/kc".into()));
        assert_eq!(c.roots, vec![PathBuf::from("/src"), dirs::home_dir().unwrap().join("Code")]);
        assert_eq!(c.max_depth, Some(2));
        assert_eq!(c.ignore, Some(vec![("*".into(), vec!["Generated".into()]), ("app".into(), vec!["docs/".into(), "x y".into()])]));
        assert!(c.unknown.is_empty());
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn conventions_parse_into_what_the_extractors_read() {
        let c = parse(SYNTHETIC_CONVENTIONS).unwrap();
        assert_eq!(c.conventions, kc_extract::conventions::synthetic());
        assert!(c.unknown.is_empty(), "{:?}", c.unknown);
        assert_eq!(c.conventions_set.len(), 11);
        let d = parse("").unwrap().conventions;
        assert_eq!(d, kc_extract::conventions::Conventions::default());
        assert!(d.bus.message_attributes.is_empty() && d.csharp_http.gateway_calls.is_empty() && d.ts_http.request_decorators.is_empty());
        let a = parse("[conventions.ansible]\ninfra_names = [\"Edge-Agent\"]\nname_prefixes = [\"ACME\"]\n[conventions.ci]\napp_name_variables = [\"SVC\"]\n").unwrap();
        assert_eq!((a.conventions.ansible.infra_names, a.conventions.ansible.name_prefixes), (vec!["edgeagent".to_string()], vec!["acme".to_string()]));
        assert_eq!(a.conventions.ci.app_name_variables, ["SVC"]);
        let err = |t: &str| format!("{:#}", parse(t).unwrap_err());
        assert!(err("[conventions.csharp.bus]\npublish = [\"Publish(host)\"]\n").contains("needs `message` exactly once"));
        assert!(err("[conventions.csharp.http]\ngateway_calls = [\"Call(service, path)\"]\n").contains("`conventions.csharp.http.gateway_calls` has"));
        assert!(err("[conventions.csharp.bus]\nmessage_attributes = \"X\"\n").contains("must be an array of strings"));
        assert!(err("[conventions.csharp.bus]\nconsumer_bases = [\"I<T>\"]\n").contains("not a name"));
        assert!(err("[conventions.csharp.di]\nscope_lifetimes = { Job = \"forever\" }\n").contains("lifetimes (singleton"));
        let u = parse("[conventions.csharp.bus]\nfuture = 1\n[conventions.rust]\nx = 1\n").unwrap();
        assert_eq!(u.unknown, ["conventions.csharp.bus.future", "conventions.rust"]);
    }

    #[test]
    fn the_privacy_file_name_and_keys_are_configurable_and_checked() {
        assert_eq!(parse("").unwrap().privacy_format(), crate::privacy::Format::default());
        let c = parse("[privacy]\nfile = \".policy.yml\"\nglobal_key = \"repo_tier\"\nrules_key = \"tiers\"\npath_key = \"where\"\nlevel_key = \"tier\"\n").unwrap();
        let f = c.privacy_format();
        assert_eq!((f.file.as_str(), f.global_key.as_str(), f.rules_key.as_str(), f.path_key.as_str(), f.level_key.as_str()), (".policy.yml", "repo_tier", "tiers", "where", "tier"));
        let err = |t: &str| format!("{:#}", parse(t).unwrap_err());
        assert_eq!(err("[privacy]\nfile = \"a/b.yml\"\n"), "/cfg/config.toml: `privacy.file` must be a file name (no directory)");
        assert_eq!(err("[privacy]\nrules_key = \"level\"\n"), "/cfg/config.toml: `privacy.rules_key` must differ from privacy.global_key");
        assert_eq!(err("[privacy]\npath_key = \"level\"\n"), "/cfg/config.toml: `privacy.level_key` must differ from privacy.path_key");
        assert_eq!(err("[privacy]\nglobal_key = \"\"\n"), "/cfg/config.toml: `privacy.global_key` must be a nonempty string");
    }

    #[test]
    fn automatic_update_checks_require_a_boolean() {
        assert_eq!(parse("[update]\ncheck = false\n").unwrap().update_check, Some(false));
        assert!(parse("[update]\ncheck = \"false\"\n").unwrap_err().to_string().contains("update.check"));
    }

    #[test]
    fn a_bad_value_names_the_file_and_the_key_and_an_unknown_key_does_not_fail() {
        let err = |t: &str| format!("{:#}", parse(t).unwrap_err());
        assert_eq!(err("[cache]\ndir = 3\n"), "/cfg/config.toml: `cache.dir` must be a string (a path)");
        assert_eq!(err("[cache]\ndir = \"rel/dir\"\n"), "/cfg/config.toml: `cache.dir` must be an absolute path or start with ~/");
        assert_eq!(err("[workspace]\nroots = \"/src\"\n"), "/cfg/config.toml: `workspace.roots` must be an array of directory paths");
        assert_eq!(err("[workspace]\nmax_depth = -1\n"), "/cfg/config.toml: `workspace.max_depth` must be a whole number from 0 to 32");
        assert_eq!(err("[ignore]\napp = \"docs/\"\n"), "/cfg/config.toml: `ignore.app` must be an array of patterns");
        assert_eq!(err("cache = 1\n"), "/cfg/config.toml: `cache` must be a table");
        assert!(err("[cache\n").starts_with("/cfg/config.toml: not valid TOML"), "{}", err("[cache\n"));
        let c = parse("[cache]\ndir = \"/x\"\nmax_size = \"10G\"\n[update]\ncheck = true\n[future]\nextra = true\n").unwrap();
        assert_eq!(c.unknown, vec!["future".to_string()]);
        assert_eq!(c.update_check, Some(true));
        assert_eq!(c.max_cache_size, Some(10_000_000_000));
        assert_eq!(c.cache_dir, Some("/x".into()));
    }

    #[test]
    fn the_notes_database_comes_from_the_variable_then_the_config_then_the_data_dir_never_the_cache() {
        let c = parse("[cache]\ndir = \"/c\"\n[notes]\npath = \"~/n/notes.db\"\n").unwrap();
        assert_eq!(c.notes_path, Some(dirs::home_dir().unwrap().join("n/notes.db")));
        assert_eq!(format!("{:#}", parse("[notes]\npath = \"n.db\"\n").unwrap_err()), "/cfg/config.toml: `notes.path` must be an absolute path or start with ~/");
        let data = || Some(PathBuf::from("/data"));
        assert_eq!(resolve_notes_path(Some("/env".into()), &c, data), (Some("/env/notes.sqlite".into()), Source::Env));
        assert_eq!(resolve_notes_path(None, &c, data), (c.notes_path.clone(), Source::Config));
        // The cache directory set alone doesn't move notes into it.
        assert_eq!(resolve_notes_path(None, &parse("[cache]\ndir = \"/c\"\n").unwrap(), data), (Some("/data/notes.sqlite".into()), Source::Default));
    }

    #[test]
    fn the_cache_dir_comes_from_the_variable_then_the_config_then_the_legacy_file_then_the_default() {
        let config = Config { cache_dir: Some("/from/config".into()), ..Default::default() };
        let none = Config::default();
        let legacy = || Some("# where\n\n~/from/legacy\n".to_string());
        let default = || PathBuf::from("/default");
        assert_eq!(resolve_cache_dir(Some("/from/env".into()), &config, legacy, default), ("/from/env".into(), Source::Env));
        let mut read = false;
        let r = resolve_cache_dir(None, &config, || { read = true; None }, default);
        assert_eq!(r, ("/from/config".into(), Source::Config));
        assert!(!read, "the legacy file isn't read when the config sets the key");
        assert_eq!(resolve_cache_dir(None, &none, legacy, default), (dirs::home_dir().unwrap().join("from/legacy"), Source::Legacy));
        assert_eq!(resolve_cache_dir(None, &none, || Some("\n# nothing\n".into()), default), ("/default".into(), Source::Default));
        assert_eq!(resolve_cache_dir(None, &none, || None, default), ("/default".into(), Source::Default));
    }

    #[test]
    fn ignore_rules_from_toml_are_the_legacy_files_rules() {
        let legacy = "# mine\naether-engine records/\n* Generated\nlonely\n/abs/repo  docs/old\n";
        let toml = parse("[ignore]\naether-engine = [\"records/\"]\n\"*\" = [\"Generated\"]\n\"/abs/repo\" = [\"docs/old\"]\n").unwrap();
        let mut a = legacy_ignore_rules(legacy);
        let mut b = config_ignore_rules(&toml.ignore.unwrap());
        a.sort();
        b.sort();
        assert_eq!(a, b);
        assert_eq!(a.len(), 3, "a line without a pattern is skipped");
    }
}
