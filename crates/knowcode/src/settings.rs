//! `knowcode config` and `knowcode workspace`: the settings in `config.toml` and the workspace roots, and what
//! `status` reports of them.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use kc_index::config::{self, Source};
use serde_json::{Value, json};

/// The config file, the cache directory with where it came from, the notes database with where it came from and how
/// many notes it holds, the workspace roots and the user-level ignore rules' source, for `status`.
pub fn status() -> Result<Value> {
    let loaded = config::load()?;
    let (cache, source) = config::cache_dir()?;
    let (notes, notes_source) = config::notes_path()?;
    let (rules, rules_source) = config::user_ignore()?;
    Ok(json!({
        "file": loaded.path,
        "exists": loaded.exists,
        "cache_dir": { "path": cache, "source": source },
        "cache_max_size": { "bytes": config::max_cache_size()?.0, "source": if loaded.config.max_cache_size.is_some() { Source::Config } else { Source::Default } },
        "notes": { "path": notes, "source": notes_source, "count": kc_index::notes::count()? },
        "workspace_roots": loaded.config.roots,
        "ignore_rules": { "count": rules.len(), "source": rules_source },
    }))
}

/// One effective setting: its key, its value as TOML, and where it came from.
struct Setting {
    key: String,
    value: Value,
    source: String,
}

/// Every setting as it takes effect, whether `config.toml` sets it or not.
fn settings() -> Result<Vec<Setting>> {
    let loaded = config::load()?;
    let c = &loaded.config;
    let (cache, source) = config::cache_dir()?;
    let set_or_default = |set: bool| if set { Source::Config } else { Source::Default };
    let mut out = vec![
        {
            let (value, source) = config::update_check()?;
            Setting { key: "update.check".into(), value: json!(value), source: source.describe("KNOWCODE_UPDATE_CHECK", "") }
        },
        {
            let (value, source) = config::session_hook()?;
            Setting { key: "hooks.session_start".into(), value: json!(value), source: source.describe("", "") }
        },
        Setting {
            key: "cache.dir".into(),
            value: json!(cache),
            source: source.describe("KNOWCODE_CACHE_DIR", config::LEGACY_CACHE_DIR),
        },
        Setting { key: "cache.max_size".into(), value: json!(config::max_cache_size()?.0), source: set_or_default(c.max_cache_size.is_some()).describe("", "") },
        {
            let (notes, source) = config::notes_path()?;
            Setting { key: "notes.path".into(), value: json!(notes), source: source.describe("KNOWCODE_DATA_DIR", "") }
        },
        Setting { key: "workspace.roots".into(), value: json!(c.roots), source: set_or_default(!c.roots.is_empty()).describe("", "") },
        Setting {
            key: "workspace.max_depth".into(),
            value: json!(c.max_depth.unwrap_or(config::DEFAULT_MAX_DEPTH)),
            source: set_or_default(c.max_depth.is_some()).describe("", ""),
        },
        Setting {
            key: "privacy.block_at".into(),
            value: json!(c.block_at.unwrap_or(kc_index::privacy::DEFAULT_BLOCK_AT)),
            source: set_or_default(c.block_at.is_some()).describe("", ""),
        },
        Setting {
            key: "privacy.malformed".into(),
            value: json!(c.malformed.unwrap_or_default().as_str()),
            source: set_or_default(c.malformed.is_some()).describe("", ""),
        },
    ];
    let format = c.privacy_format();
    let format_values = [&format.file, &format.global_key, &format.rules_key, &format.path_key, &format.level_key];
    for (i, (name, value)) in config::PRIVACY_FORMAT_KEYS.iter().zip(format_values).enumerate() {
        out.push(Setting { key: format!("privacy.{name}"), value: json!(value), source: set_or_default(c.privacy_file[i].is_some()).describe("", "") });
    }
    let v = &c.conventions;
    let calls = |cs: &[kc_extract::conventions::Call]| json!(cs.iter().map(|c| c.render()).collect::<Vec<_>>());
    for key in config::CONVENTION_KEYS {
        let value = match key.rsplit_once('.').map_or("", |(_, n)| n) {
            "message_attributes" => json!(v.bus.message_attributes),
            "consumer_bases" => json!(v.bus.consumer_bases),
            "consumer_registrations" => json!(v.bus.consumer_registrations),
            "publish" => calls(&v.bus.publish),
            "request_attributes" => calls(&v.csharp_http.request_attributes),
            "gateway_calls" => calls(&v.csharp_http.gateway_calls),
            "service_host_bases" => json!(v.csharp_http.service_host_bases),
            "scope_lifetimes" => Value::Object(v.di.scope_lifetimes.iter().map(|(n, l)| (n.clone(), json!(l))).collect()),
            "request_decorators" => json!(v.ts_http.request_decorators),
            "path_keys" => json!(v.ts_http.path_keys),
            "verb_keys" => json!(v.ts_http.verb_keys),
            "app_name_variables" => json!(v.ci.app_name_variables),
            "infra_names" => json!(v.ansible.infra_names),
            "name_prefixes" => json!(v.ansible.name_prefixes),
            _ => continue,
        };
        out.push(Setting { key: key.to_string(), value, source: set_or_default(c.conventions_set.iter().any(|k| k == key)).describe("", "") });
    }
    let (rules, source) = config::user_ignore()?;
    let mut by_repo: Vec<(String, Vec<String>)> = Vec::new();
    for (repo, pattern) in rules {
        match by_repo.iter_mut().find(|(r, _)| *r == repo) {
            Some((_, ps)) => ps.push(pattern),
            None => by_repo.push((repo, vec![pattern])),
        }
    }
    for (repo, patterns) in by_repo {
        out.push(Setting { key: format!("ignore.{repo}"), value: json!(patterns), source: source.describe("", config::LEGACY_IGNORE) });
    }
    Ok(out)
}

/// A value as TOML writes it: strings quoted, lists in brackets.
fn toml_text(v: &Value) -> String {
    match v {
        Value::Array(items) => format!("[{}]", items.iter().map(toml_text).collect::<Vec<_>>().join(", ")),
        // An inline table; a key that isn't bare is quoted.
        Value::Object(map) if map.is_empty() => "{}".to_string(),
        Value::Object(map) => {
            let key = |k: &String| if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { k.clone() } else { Value::String(k.clone()).to_string() };
            format!("{{ {} }}", map.iter().map(|(k, v)| format!("{} = {}", key(k), toml_text(v))).collect::<Vec<_>>().join(", "))
        }
        // A JSON string is a TOML basic string.
        other => other.to_string(),
    }
}

/// `config list`: every setting with its value and source.
pub fn list() -> Result<()> {
    let loaded = config::load()?;
    println!("# {}{}", loaded.path.display(), if loaded.exists { "" } else { " (not created yet)" });
    for s in settings()? {
        println!("{} = {}  # {}", s.key, toml_text(&s.value), s.source);
    }
    Ok(())
}

/// `config get KEY`: the setting's value as it takes effect; a list prints one item per line.
pub fn get(key: &str) -> Result<()> {
    let all = settings()?;
    let Some(s) = all.iter().find(|s| s.key == key) else {
        if key.starts_with("ignore.") {
            return Ok(());
        }
        bail!("unknown key `{key}`: the keys are {}, and ignore.<repo>", config::KEYS.join(", "));
    };
    match &s.value {
        Value::String(v) => println!("{v}"),
        Value::Array(items) => items.iter().for_each(|i| println!("{}", i.as_str().map_or_else(|| i.to_string(), str::to_string))),
        v => println!("{v}"),
    }
    Ok(())
}

/// `config set KEY VALUE`.
pub fn set(key: &str, value: &str) -> Result<()> {
    config::set(key, value)?;
    println!("set {key} in {}", config::path()?.display());
    Ok(())
}

/// `config unset KEY`.
pub fn unset(key: &str) -> Result<()> {
    if config::unset(key)? {
        println!("unset {key} in {}", config::path()?.display());
    } else {
        println!("{key} is not set in {}", config::path()?.display());
    }
    Ok(())
}

/// `config migrate`.
pub fn migrate() -> Result<()> {
    config::migrate()?.iter().for_each(|l| println!("{l}"));
    Ok(())
}

/// The repos under a root and how many of them are indexed.
fn counts(repos: &[PathBuf]) -> Result<(usize, usize)> {
    let mut indexed = 0;
    for r in repos {
        indexed += kc_index::db_path(r)?.exists() as usize;
    }
    Ok((repos.len(), indexed))
}

fn repos_line(root: &Path, repos: &[PathBuf]) -> Result<String> {
    if !root.is_dir() {
        return Ok(format!("{}  (missing)", root.display()));
    }
    let (n, indexed) = counts(repos)?;
    Ok(format!("{}  {n} git repo{}, {indexed} indexed", root.display(), if n == 1 { "" } else { "s" }))
}

/// `workspace add DIR`.
pub fn add(dir: &Path) -> Result<()> {
    let (root, new) = config::add_root(dir)?;
    kc_index::light::discover_roots()?;
    let depth = config::workspace()?.1;
    let line = repos_line(&root, &kc_index::roots::discover(&root, depth))?;
    println!("{} {line}", if new { "added" } else { "already a root:" });
    Ok(())
}

/// `workspace remove DIR`.
pub fn remove(dir: &Path) -> Result<()> {
    let removed = config::remove_root(dir)?;
    if removed.is_empty() {
        bail!("{} is not a workspace root (see `knowcode workspace list`)", dir.display());
    }
    removed.iter().for_each(|r| println!("removed {r}"));
    kc_index::light::discover_roots()?;
    Ok(())
}

/// `workspace list`: each root with its git repos and how many are indexed.
pub fn list_roots() -> Result<()> {
    let roots = kc_index::roots::configured()?;
    if roots.is_empty() {
        let n = kc_index::all_indexed_roots()?.len();
        println!("no workspace roots: the workspace is every indexed repo ({n}). Add one with `knowcode workspace add <dir>`.");
        return Ok(());
    }
    for (root, repos) in &roots {
        println!("{}", repos_line(root, repos)?);
    }
    Ok(())
}
