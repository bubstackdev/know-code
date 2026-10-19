//! Client registration without repository-local edits. The bundled Claude plugin can be installed offline.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use toml_edit::{Array, DocumentMut, Item, Table};

const MARKETPLACE: &str = "knowcode-local";
pub const PLUGIN: &str = "knowcode@knowcode-local";
const OVERRIDES: &[&str] = &["KNOWCODE_CACHE_DIR", "KNOWCODE_CONFIG_DIR", "KNOWCODE_DATA_DIR", "KNOWCODE_TOOLS"];

const FILES: &[(&str, &str)] = &[
    (".claude-plugin/plugin.json", include_str!("../../../integrations/claude-code/.claude-plugin/plugin.json")),
    (".mcp.json", include_str!("../../../integrations/claude-code/.mcp.json")),
    ("hooks/hooks.json", include_str!("../../../integrations/claude-code/hooks/hooks.json")),
    ("hooks/knowcode-grep-hint.sh", include_str!("../../../integrations/claude-code/hooks/knowcode-grep-hint.sh")),
    ("skills/knowcode-explore/SKILL.md", include_str!("../../../integrations/claude-code/skills/knowcode-explore/SKILL.md")),
    ("skills/knowcode-before-edit/SKILL.md", include_str!("../../../integrations/claude-code/skills/knowcode-before-edit/SKILL.md")),
    ("skills/knowcode-debug-stack-trace/SKILL.md", include_str!("../../../integrations/claude-code/skills/knowcode-debug-stack-trace/SKILL.md")),
    ("skills/knowcode-safe-refactor/SKILL.md", include_str!("../../../integrations/claude-code/skills/knowcode-safe-refactor/SKILL.md")),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Client { Claude, Codex, Cursor }

#[derive(Args)]
pub struct Options {
    #[arg(value_enum)]
    pub client: Client,
    /// Client settings directory (default: the client's user directory). Useful for isolated profiles.
    #[arg(long)]
    pub client_dir: Option<PathBuf>,
    /// Print the server configuration without changing files or installing a plugin.
    #[arg(long = "print")]
    pub print_only: bool,
    /// Replace a different server already registered as knowcode (Codex or Cursor).
    #[arg(long)]
    pub replace: bool,
}

/// Only the registration we own, for removing it later without touching unrelated client settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    pub client: Client,
    pub directory: PathBuf,
    pub command: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<PathBuf>,
}

pub fn registrations_path() -> Result<PathBuf> {
    Ok(kc_index::config_dir()?.join("integrations.json"))
}

pub fn registrations() -> Result<Vec<Registration>> {
    let path = registrations_path()?;
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn remember(registration: Registration) -> Result<()> {
    let path = registrations_path()?;
    let old = read(&path)?;
    let mut saved: Vec<Registration> = serde_json::from_str(old.as_deref().unwrap_or("[]"))
        .with_context(|| format!("reading {}", path.display()))?;
    saved.retain(|r| r.client != registration.client || r.directory != registration.directory);
    saved.push(registration);
    write_checked(&path, old.as_deref(), &format!("{}\n", serde_json::to_string_pretty(&saved)?))
}

fn user_dir(client: Client) -> Result<PathBuf> {
    let home = || dirs::home_dir().context("no home directory; pass --client-dir");
    match client {
        Client::Claude => match std::env::var_os("CLAUDE_CONFIG_DIR") {
            Some(path) => Ok(PathBuf::from(path)), None => Ok(home()?.join(".claude")),
        },
        Client::Codex => match std::env::var_os("CODEX_HOME") {
            Some(path) => Ok(PathBuf::from(path)), None => Ok(home()?.join(".codex")),
        },
        Client::Cursor => Ok(home()?.join(".cursor")),
    }
}

/// Prefer the invocation's symlink (a package manager's stable bin path) to the versioned executable it points at.
fn executable() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let same = |p: &Path| fs::canonicalize(p).is_ok_and(|p| p == exe);
    if let Some(arg) = std::env::args_os().next() {
        let given = PathBuf::from(arg);
        if given.components().count() > 1 {
            let given = std::path::absolute(given)?;
            if same(&given) { return Ok(given); }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = std::path::absolute(dir.join("knowcode"))?;
            if same(&candidate) { return Ok(candidate); }
        }
    }
    Ok(exe)
}

fn overrides() -> BTreeMap<String, String> {
    OVERRIDES.iter().filter_map(|name| std::env::var(name).ok().map(|v| ((*name).to_string(), v))).collect()
}

fn server(command: &Path, env: &BTreeMap<String, String>) -> Value {
    let mut value = json!({"command": command, "args": ["serve"]});
    if !env.is_empty() { value["env"] = json!(env); }
    value
}

fn toml_server(command: &Path, env: &BTreeMap<String, String>) -> Table {
    let mut table = Table::new();
    table.insert("command", toml_edit::value(command.to_string_lossy().as_ref()));
    table.insert("args", toml_edit::value(Array::from_iter(["serve"])));
    if !env.is_empty() {
        let mut vars = Table::new();
        for (key, value) in env { vars.insert(key, toml_edit::value(value)); }
        table.insert("env", Item::Table(vars));
    }
    table
}

fn read(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Preserve existing file permissions, create new settings privately, and refuse an observed concurrent edit.
pub(crate) fn write_checked(path: &Path, before: Option<&str>, text: &str) -> Result<()> {
    if before == Some(text) { return Ok(()); }
    let parent = path.parent().context("settings path has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".knowcode-{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
    let mut file = options.open(&tmp).with_context(|| format!("writing {}", path.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(text.as_bytes())?;
        if let Ok(metadata) = fs::metadata(path) { file.set_permissions(metadata.permissions())?; }
        file.sync_all()?;
        if read(path)?.as_deref() != before { bail!("{} changed during the operation; retry", path.display()); }
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(tmp); }
    result
}

pub(crate) fn config_path(directory: &Path, file: &str) -> Result<PathBuf> {
    let path = directory.join(file);
    // Edit the target of an existing symlink without replacing the user's symlink.
    if fs::symlink_metadata(&path).is_ok_and(|m| m.is_symlink()) {
        return fs::canonicalize(&path).with_context(|| format!("resolving {}", path.display()));
    }
    Ok(path)
}

fn codex(directory: &Path, command: &Path, env: &BTreeMap<String, String>, replace: bool) -> Result<()> {
    let path = config_path(directory, "config.toml")?;
    let before = read(&path)?;
    let mut doc: DocumentMut = before.as_deref().unwrap_or("").parse()
        .map_err(|_| anyhow!("{} is not valid TOML; setup left it unchanged", path.display()))?;
    let servers = doc.entry("mcp_servers").or_insert_with(toml_edit::table).as_table_like_mut()
        .context("mcp_servers must be a TOML table")?;
    if let Some(existing) = servers.get("knowcode") {
        let matches = existing.get("command").and_then(Item::as_str) == command.to_str()
            && existing.get("args").and_then(Item::as_array).is_some_and(|a| a.len() == 1 && a.get(0).and_then(toml_edit::Value::as_str) == Some("serve"));
        if !matches && !replace { bail!("knowcode is already configured differently in {}; use --replace to replace that server", path.display()); }
        if matches && !replace {
            if !env.is_empty() {
                let existing = servers.get_mut("knowcode").and_then(Item::as_table_like_mut).context("knowcode must be a TOML table")?;
                let vars = existing.entry("env").or_insert_with(toml_edit::table).as_table_like_mut().context("knowcode.env must be a TOML table")?;
                for (key, value) in env {
                    if vars.get(key).and_then(Item::as_str) != Some(value.as_str()) {
                        vars.insert(key, toml_edit::value(value));
                    }
                }
            }
            return write_checked(&path, before.as_deref(), &doc.to_string());
        }
    }
    servers.insert("knowcode", Item::Table(toml_server(command, env)));
    write_checked(&path, before.as_deref(), &doc.to_string())
}

fn cursor(directory: &Path, command: &Path, env: &BTreeMap<String, String>, replace: bool) -> Result<()> {
    let path = config_path(directory, "mcp.json")?;
    let before = read(&path)?;
    let mut doc: Value = serde_json::from_str(before.as_deref().unwrap_or("{}"))
        .with_context(|| format!("{} is not valid JSON; setup left it unchanged", path.display()))?;
    let root = doc.as_object_mut().context("MCP settings must be a JSON object")?;
    let servers = root.entry("mcpServers").or_insert_with(|| json!({})).as_object_mut().context("mcpServers must be a JSON object")?;
    let value = server(command, env);
    if let Some(existing) = servers.get("knowcode") {
        if existing["command"] == value["command"] && existing["args"] == value["args"] && !replace {
            if !env.is_empty() {
                let existing = servers.get_mut("knowcode").and_then(Value::as_object_mut).context("knowcode must be a JSON object")?;
                let vars = existing.entry("env").or_insert_with(|| json!({})).as_object_mut().context("knowcode.env must be a JSON object")?;
                for (key, value) in env { vars.insert(key.clone(), json!(value)); }
            }
            // Preserve the original bytes on a no-op; otherwise merge only the explicit KnowCode overrides.
            let new: Value = serde_json::from_str(before.as_deref().unwrap_or("{}"))?;
            if new == doc { return Ok(()); }
            return write_checked(&path, before.as_deref(), &format!("{}\n", serde_json::to_string_pretty(&doc)?));
        }
        if !replace { bail!("knowcode is already configured differently in {}; use --replace to replace that server", path.display()); }
    }
    servers.insert("knowcode".into(), value);
    write_checked(&path, before.as_deref(), &format!("{}\n", serde_json::to_string_pretty(&doc)?))
}

fn shell_quote(text: &str) -> String { format!("'{}'", text.replace('\'', "'\\''")) }

fn bundle(command: &Path, env: &BTreeMap<String, String>) -> Result<PathBuf> {
    let marketplace = kc_index::data_dir()?.join("integrations/claude-marketplace");
    let plugin = marketplace.join("knowcode");
    for (name, text) in FILES {
        let text = match *name {
            ".mcp.json" => format!("{}\n", serde_json::to_string_pretty(&json!({"mcpServers": {"knowcode": server(command, env)}}))?),
            "hooks/knowcode-grep-hint.sh" => {
                let mut prefix = format!("#!/bin/sh\nexport KNOWCODE_BIN={}\n", shell_quote(&command.to_string_lossy()));
                for (key, value) in env { prefix.push_str(&format!("export {key}={}\n", shell_quote(value))); }
                prefix + text
            }
            _ => text.to_string(),
        };
        let path = plugin.join(name);
        let before = read(&path)?;
        write_checked(&path, before.as_deref(), &text)?;
    }
    let catalog = json!({"name": MARKETPLACE, "description": "KnowCode's bundled local code-context plugin.", "owner": {"name": "KnowCode contributors"},
        "plugins": [{"name": "knowcode", "source": "./knowcode", "version": env!("CARGO_PKG_VERSION"), "description": "Local code context and cross-repository tracing."}]});
    let path = marketplace.join(".claude-plugin/marketplace.json");
    let before = read(&path)?;
    write_checked(&path, before.as_deref(), &format!("{}\n", serde_json::to_string_pretty(&catalog)?))?;
    Ok(marketplace)
}

/// Remove only bundled assets, leaving user-added files and durable notes in place.
pub(crate) fn remove_bundle(marketplace: &Path) -> Result<()> {
    if !marketplace.ends_with("integrations/claude-marketplace") { bail!("unrecognized bundle path {}; retained", marketplace.display()); }
    let notes = kc_index::notes::db_path()?;
    let notes = fs::canonicalize(&notes).unwrap_or(notes);
    let mut dirs = std::collections::BTreeSet::new();
    let mut files: Vec<_> = FILES.iter().map(|(name, _)| marketplace.join("knowcode").join(name)).collect();
    files.push(marketplace.join(".claude-plugin/marketplace.json"));
    for path in files {
        if fs::canonicalize(&path).unwrap_or_else(|_| path.clone()) == notes { continue; }
        for dir in path.ancestors().skip(1).take_while(|d| d.starts_with(marketplace)) { dirs.insert(dir.to_path_buf()); }
        match fs::remove_file(&path) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e).with_context(|| format!("removing bundled asset {}", path.display())),
        }
    }
    let mut dirs: Vec<_> = dirs.into_iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for dir in dirs { let _ = fs::remove_dir(dir); }
    // `integrations/` holds only what setup writes; it goes once empty.
    if let Some(parent) = marketplace.parent() { let _ = fs::remove_dir(parent); }
    Ok(())
}

pub(crate) fn claude_output(directory: &Path, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("claude");
    cmd.args(args).env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1").stdin(Stdio::null());
    // Leave the ordinary profile's environment alone: setting CLAUDE_CONFIG_DIR explicitly can also change where
    // Claude keeps its global state. Only override it when selecting a different profile directory.
    let inherited = user_dir(Client::Claude).ok().and_then(|p| std::path::absolute(p).ok());
    if inherited.as_deref() != Some(directory) { cmd.env("CLAUDE_CONFIG_DIR", directory); }
    // Every command KnowCode runs is user-scoped. Running it from the profile directory keeps a scope-less one
    // (uninstall's fallback removal) away from the settings of whatever project the user is in.
    if directory.is_dir() { cmd.current_dir(directory); }
    let out = kc_store::limits::output(cmd, kc_store::limits::Kind::Setup, "Claude Code plugin registration", None)
        .context("running Claude Code; install the claude CLI first, or use `knowcode setup claude --print`")?;
    if !out.status.success() {
        bail!("Claude Code command failed: {}{}", String::from_utf8_lossy(&out.stdout).trim(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

fn claude_command(directory: &Path, args: &[&std::ffi::OsStr]) -> Result<()> {
    claude_output(directory, args).map(|_| ())
}

pub fn run(options: Options) -> Result<()> {
    let command = executable()?;
    command.to_str().context("the executable path must be valid UTF-8 for MCP client configuration")?;
    let env = overrides();
    if options.print_only {
        if options.client == Client::Codex {
            let mut doc = DocumentMut::new();
            let mut servers = Table::new();
            servers.set_implicit(true);
            servers.insert("knowcode", Item::Table(toml_server(&command, &env)));
            doc.insert("mcp_servers", Item::Table(servers));
            print!("{doc}");
        } else {
            println!("{}", serde_json::to_string_pretty(&json!({"mcpServers": {"knowcode": server(&command, &env)}}))?);
        }
        return Ok(());
    }
    let directory = std::path::absolute(match options.client_dir {
        Some(path) => path, None => user_dir(options.client)?,
    })?;
    // Ensure the removal ledger is readable before changing a client.
    registrations()?;
    let mut installed_marketplace = None;
    match options.client {
        Client::Codex => codex(&directory, &command, &env, options.replace)?,
        Client::Cursor => cursor(&directory, &command, &env, options.replace)?,
        Client::Claude => {
            let marketplace = bundle(&command, &env)?;
            installed_marketplace = Some(marketplace.clone());
            eprintln!("knowcode: installing the bundled Claude Code plugin");
            // A helper can time out after making a partial change. Record this profile before invoking it so
            // removal can find both a marketplace-only registration and a completed plugin installation.
            remember(Registration { client: options.client, directory: directory.clone(), command: command.clone(), marketplace: installed_marketplace.clone() })?;
            claude_command(&directory, &["plugin".as_ref(), "marketplace".as_ref(), "add".as_ref(), marketplace.as_os_str(), "--scope".as_ref(), "user".as_ref()])?;
            claude_command(&directory, &["plugin".as_ref(), "install".as_ref(), PLUGIN.as_ref(), "--scope".as_ref(), "user".as_ref()])?;
        }
    }
    remember(Registration { client: options.client, directory: directory.clone(), command, marketplace: installed_marketplace })?;
    println!("KnowCode is configured for {:?} in {}. Start a new client session; the first query indexes automatically.", options.client, directory.display());
    Ok(())
}
