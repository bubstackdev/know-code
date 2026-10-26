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
    ("hooks/knowcode-session-start.sh", include_str!("../../../integrations/claude-code/hooks/knowcode-session-start.sh")),
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
            codex_hook(&mut doc, command)?;
            return write_checked(&path, before.as_deref(), &doc.to_string());
        }
    }
    servers.insert("knowcode", Item::Table(toml_server(command, env)));
    codex_hook(&mut doc, command)?;
    write_checked(&path, before.as_deref(), &doc.to_string())
}

/// What a Codex session-start hook of ours runs: the binary, with the directory overrides its server has. Only the
/// binary's path and those overrides are in it, never the line it prints, so the line can change without the user
/// trusting the hook again (Codex trusts a hook by its definition's hash).
pub(crate) const CODEX_HOOK: &str = "hook session-start";

fn codex_hook_command(command: &Path, env: &BTreeMap<String, String>) -> String {
    let mut line = String::new();
    for (key, value) in env.iter().filter(|(k, _)| *k != "KNOWCODE_TOOLS") {
        line.push_str(&format!("{key}={} ", shell_quote(value)));
    }
    format!("{line}{} {CODEX_HOOK}", shell_quote(&command.to_string_lossy()))
}

/// Whether a `[[hooks.SessionStart]]` entry is one setup wrote: every hook in it runs KnowCode's session-start hook.
pub(crate) fn is_codex_hook(group: &Table) -> bool {
    group.get("hooks").and_then(Item::as_array_of_tables).is_some_and(|hooks| {
        !hooks.is_empty() && hooks.iter().all(|h| h.get("command").and_then(Item::as_str).is_some_and(|c| c.ends_with(&format!(" {CODEX_HOOK}"))))
    })
}

/// Adds the user-level `[[hooks.SessionStart]]` entry that runs KnowCode's session-start hook, in place of any
/// earlier one of ours, with the overrides the knowcode server entry ends up with. Other hooks are left alone.
fn codex_hook(doc: &mut DocumentMut, command: &Path) -> Result<()> {
    let env: BTreeMap<String, String> = doc.get("mcp_servers").and_then(|s| s.get("knowcode")).and_then(|k| k.get("env"))
        .and_then(Item::as_table_like).into_iter().flat_map(|t| t.iter())
        .filter(|(k, _)| OVERRIDES.contains(k))
        .filter_map(|(k, v)| Some((k.to_string(), v.as_str()?.to_string())))
        .collect();
    let hooks = doc.entry("hooks").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    }).as_table_like_mut().context("hooks must be a TOML table")?;
    let groups = hooks.entry("SessionStart").or_insert(Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut().context("hooks.SessionStart must be an array of tables")?;
    groups.retain(|g| !is_codex_hook(g));
    let mut hook = Table::new();
    hook.insert("type", toml_edit::value("command"));
    hook.insert("command", toml_edit::value(codex_hook_command(command, &env)));
    hook.insert("timeout", toml_edit::value(5));
    let mut list = toml_edit::ArrayOfTables::new();
    list.push(hook);
    let mut group = Table::new();
    // A resumed or forked session already has the line from its first start.
    group.insert("matcher", toml_edit::value("^(startup|clear|compact)$"));
    group.insert("hooks", Item::ArrayOfTables(list));
    groups.push(group);
    Ok(())
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

fn marketplace_dir() -> Result<PathBuf> { Ok(kc_index::data_dir()?.join("integrations/claude-marketplace")) }

fn bundle(command: &Path, env: &BTreeMap<String, String>) -> Result<PathBuf> {
    let marketplace = marketplace_dir()?;
    let plugin = marketplace.join("knowcode");
    let mut writes = Vec::new();
    let mut manifest = None;
    for (name, text) in FILES {
        let text = match *name {
            // Claude Code keys its cached copy by the manifest's version, so it always names this binary's. It is
            // written last: a manifest at this version means the rest of the bundle is too.
            ".claude-plugin/plugin.json" => {
                let mut value: Value = serde_json::from_str(text)?;
                value["version"] = json!(env!("CARGO_PKG_VERSION"));
                manifest = Some((plugin.join(name), format!("{}\n", serde_json::to_string_pretty(&value)?)));
                continue;
            }
            // The plugin's server takes the default profile, the single `kc` tool, unless the user picked one; the
            // skills name tools as its ops (see [`skill_text`]).
            ".mcp.json" => {
                let mut env = env.clone();
                env.entry("KNOWCODE_TOOLS".into()).or_insert_with(|| crate::mcp::DEFAULT_PROFILE.into());
                format!("{}\n", serde_json::to_string_pretty(&json!({"mcpServers": {"knowcode": server(command, &env)}}))?)
            }
            "hooks/knowcode-grep-hint.sh" | "hooks/knowcode-session-start.sh" => {
                let mut prefix = format!("#!/bin/sh\nexport KNOWCODE_BIN={}\n", shell_quote(&command.to_string_lossy()));
                for (key, value) in env { prefix.push_str(&format!("export {key}={}\n", shell_quote(value))); }
                prefix + text
            }
            skill if skill.ends_with("SKILL.md") => skill_text(text, env.get("KNOWCODE_TOOLS").map_or(crate::mcp::DEFAULT_PROFILE, String::as_str)),
            _ => text.to_string(),
        };
        writes.push((plugin.join(name), text));
    }
    let catalog = json!({"name": MARKETPLACE, "description": "KnowCode's bundled local code-context plugin.", "owner": {"name": "KnowCode contributors"},
        "plugins": [{"name": "knowcode", "source": "./knowcode", "version": env!("CARGO_PKG_VERSION"), "description": "Local code context and cross-repository tracing."}]});
    writes.push((marketplace.join(".claude-plugin/marketplace.json"), format!("{}\n", serde_json::to_string_pretty(&catalog)?)));
    writes.extend(manifest);
    for (path, text) in writes {
        let before = read(&path)?;
        write_checked(&path, before.as_deref(), &text)?;
    }
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

fn update_plugin(directory: &Path) -> Result<()> {
    claude_command(directory, &["plugin".as_ref(), "update".as_ref(), PLUGIN.as_ref(), "--scope".as_ref(), "user".as_ref()])
}

/// The version in the plugin manifest of the bundle at `marketplace`, if it has a readable one.
/// A skill as the plugin's profile calls the tools: under `one`, `kc_impact(symbol: "X")` is
/// `kc {op: impact, symbol: "X"}`; under the others, as written.
fn skill_text(text: &str, profile: &str) -> String {
    if !crate::mcp::is_one(profile) {
        return text.to_string();
    }
    let text = text.replace(
        "Use KnowCode's advertised `kc_*` tools; the client may add a plugin or server prefix to their names.",
        "Use KnowCode's `kc` tool, its `op` naming what to run; the client may add a plugin or server prefix to its name.",
    );
    crate::mcp::one_text(&text, true)
}

fn bundle_version(marketplace: &Path) -> Option<semver::Version> {
    let text = fs::read_to_string(marketplace.join("knowcode/.claude-plugin/plugin.json")).ok()?;
    let manifest: Value = serde_json::from_str(&text).ok()?;
    semver::Version::parse(manifest["version"].as_str()?).ok()
}

fn stale(marketplace: &Path) -> bool {
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("package SemVer");
    bundle_version(marketplace).is_some_and(|v| v < current)
}

/// After an update: when a Claude Code plugin bundle this install wrote is older than this binary, rewrite it and
/// have Claude Code update its copy in each profile it was installed in. Never fails; a problem is reported once on
/// stderr with the `knowcode setup claude` command that finishes the job. Codex and Cursor registrations name only
/// the binary, so they need nothing.
pub fn refresh_claude_plugin() {
    let mut profiles = Vec::new();
    if let Err(e) = refresh(&mut profiles) {
        // Another server process may have finished the same refresh first.
        if marketplace_dir().is_ok_and(|m| !stale(&m)) && profiles.is_empty() { return; }
        let commands: Vec<String> = if profiles.is_empty() { vec!["knowcode setup claude".into()] } else { profiles.iter().map(|d| finish_command(d)).collect() };
        eprintln!("knowcode: could not update the Claude Code plugin to {}: {e:#}. Run `{}` to finish.", env!("CARGO_PKG_VERSION"), commands.join("` and `"));
    }
}

fn finish_command(directory: &Path) -> String {
    let default = user_dir(Client::Claude).ok().and_then(|p| std::path::absolute(p).ok());
    if default.as_deref() == Some(directory) { "knowcode setup claude".into() }
    else { format!("knowcode setup claude --client-dir {}", shell_quote(&directory.to_string_lossy())) }
}

/// Fills `failed` with the profiles whose plugin update did not go through.
fn refresh(failed: &mut Vec<PathBuf>) -> Result<()> {
    let marketplace = marketplace_dir()?;
    // Only a bundle and marketplace this install created, at the path recorded for it.
    let ours: Vec<Registration> = registrations()?.into_iter()
        .filter(|r| r.client == Client::Claude && r.marketplace.as_deref() == Some(marketplace.as_path()))
        .collect();
    if ours.is_empty() || !marketplace.ends_with("integrations/claude-marketplace") || !stale(&marketplace) { return Ok(()); }
    // The latest registration wrote the bundle; keep its binary and the overrides its server configuration carries.
    let latest = ours.last().expect("not empty").command.clone();
    let config: Value = read(&marketplace.join("knowcode/.mcp.json"))?.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut env: BTreeMap<String, String> = config["mcpServers"]["knowcode"]["env"].as_object().into_iter().flatten()
        .filter(|(key, _)| OVERRIDES.contains(&key.as_str()))
        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
        .collect();
    // Before 0.4 the bundle wrote `KNOWCODE_TOOLS=all` as its default: an update takes the new default instead.
    if bundle_version(&marketplace).is_some_and(|v| v < semver::Version::new(0, 4, 0)) && env.get("KNOWCODE_TOOLS").is_some_and(|t| t == "all") {
        env.remove("KNOWCODE_TOOLS");
    }
    bundle(&latest, &env)?;
    let mut errors = Vec::new();
    for registration in &ours {
        if let Err(e) = update_plugin(&registration.directory) {
            errors.push(format!("{e:#}"));
            failed.push(registration.directory.clone());
        }
    }
    if !errors.is_empty() { bail!("the plugin files are updated, but Claude Code's copy is not ({})", errors.join("; ")); }
    eprintln!("knowcode: updated the Claude Code plugin to {}; start a new Claude Code session to use it.", env!("CARGO_PKG_VERSION"));
    Ok(())
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
            // `install` leaves an already installed plugin at the version it recorded; `update` moves it to this one.
            update_plugin(&directory)?;
        }
    }
    remember(Registration { client: options.client, directory: directory.clone(), command, marketplace: installed_marketplace })?;
    println!("KnowCode is configured for {:?} in {}. Start a new client session; the first query indexes automatically.", options.client, directory.display());
    if options.client == Client::Codex {
        println!("Codex runs KnowCode's session-start hook only once you trust it: in Codex, run /hooks and trust the SessionStart hook that runs `{CODEX_HOOK}`.");
    }
    Ok(())
}
