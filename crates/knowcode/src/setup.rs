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
/// The path goes into client configuration, and into the Codex hook Codex trusts by its text, so it must not change
/// when KnowCode is updated.
fn executable() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let same = |p: &Path| fs::canonicalize(p).is_ok_and(|p| p == exe);
    let chosen = (|| -> Result<PathBuf> {
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
        Ok(exe.clone())
    })()?;
    Ok(homebrew_opt(&chosen).unwrap_or(chosen))
}

/// For a binary in a Homebrew keg (`<prefix>/Cellar/knowcode/<version>/bin/knowcode`), which `brew upgrade` removes,
/// the keg-independent `<prefix>/opt/knowcode/bin/knowcode` that Homebrew keeps pointing at the current version.
fn homebrew_opt(path: &Path) -> Option<PathBuf> {
    let bin = path.parent()?;
    let keg = bin.parent()?;
    let formula = keg.parent()?;
    let cellar = formula.parent()?;
    if bin.file_name()? != "bin" || formula.file_name()? != "knowcode" || cellar.file_name()? != "Cellar" { return None; }
    let opt = cellar.parent()?.join("opt/knowcode/bin").join(path.file_name()?);
    (fs::canonicalize(&opt).ok()? == fs::canonicalize(path).ok()?).then_some(opt)
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

/// Which session sources the hook runs for. A resumed or forked session already has the line from its first start.
const CODEX_MATCHER: &str = "^(startup|clear|compact)$";

/// Above the empty entry uninstall leaves where our hook was when the user's own hooks follow it. Codex keys a hook's
/// trust by its entry's position (`config.toml:session_start:<entry>:<hook>`), so removing an entry would move every
/// later one and Codex would ask the user to trust them again. An entry with no hooks runs nothing.
const CODEX_PLACEHOLDER: &str = "# Left by `knowcode uninstall` where its hook was. It runs nothing. It keeps the hooks after it in place, so\n# Codex still trusts them; deleting it makes Codex ask you to trust those hooks again.\n";

/// The empty entry uninstall leaves (or one like it): our matcher and no hooks.
fn is_codex_placeholder(group: &Table) -> bool {
    group.get("matcher").and_then(Item::as_str) == Some(CODEX_MATCHER)
        && group.get("hooks").is_none_or(|h| h.as_array_of_tables().is_some_and(|a| a.is_empty()) || h.as_array().is_some_and(|a| a.is_empty()))
}

/// Adds the user-level `[[hooks.SessionStart]]` entry that runs KnowCode's session-start hook, with the overrides the
/// knowcode server entry ends up with. An entry of ours, or the empty one uninstall left, is updated where it is; a new
/// one goes last. Moving an entry would move the user's hooks after it, and Codex would ask to trust them again. An
/// entry already as it should be is left as it is, and other hooks are left alone.
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
    let line = codex_hook_command(command, &env);
    let ours = (0..groups.len()).find(|&i| groups.get(i).is_some_and(is_codex_hook))
        .or_else(|| (0..groups.len()).find(|&i| groups.get(i).is_some_and(is_codex_placeholder)));
    let as_it_should_be = |g: &Table| g.len() == 2 && g.get("matcher").and_then(Item::as_str) == Some(CODEX_MATCHER)
        && g.get("hooks").and_then(Item::as_array_of_tables).is_some_and(|hooks| hooks.len() == 1 && hooks.get(0).is_some_and(|h| {
            h.len() == 3 && h.get("type").and_then(Item::as_str) == Some("command")
                && h.get("command").and_then(Item::as_str) == Some(line.as_str()) && h.get("timeout").and_then(Item::as_integer) == Some(5)
        }));
    if ours.and_then(|i| groups.get(i)).is_some_and(as_it_should_be) { return Ok(()); }
    let mut hook = Table::new();
    hook.insert("type", toml_edit::value("command"));
    hook.insert("command", toml_edit::value(line.as_str()));
    hook.insert("timeout", toml_edit::value(5));
    let mut list = toml_edit::ArrayOfTables::new();
    list.push(hook);
    let mut group = Table::new();
    group.insert("matcher", toml_edit::value(CODEX_MATCHER));
    group.insert("hooks", Item::ArrayOfTables(list));
    match ours {
        Some(i) => *groups.get_mut(i).expect("found above") = group,
        None => groups.push(group),
    }
    Ok(())
}

/// Takes out the entry `codex_hook` added, and the record of the user trusting it. When the user's own hooks follow
/// it, the entry stays, emptied, so they keep their place and Codex's trust in them (see `CODEX_PLACEHOLDER`). Empty
/// entries of ours left last are removed, and so are the `[hooks]` tables that leaves empty. Returns whether an
/// emptied entry was left.
pub(crate) fn remove_codex_hook(doc: &mut DocumentMut, directory: &Path) -> bool {
    // Codex's records for our entry's place: the user trusting our hook (this or an earlier version of it), or turning
    // it off. Setup never moves its entry, so nothing else was there; and with no record, a hook later put there
    // would be reviewed, never trusted unseen.
    let records: Vec<String> = codex_hook_index(doc).into_iter().flat_map(|(index, _)| {
        let state = codex_hook_states(doc);
        codex_state_keys(directory, index).into_iter().filter(|key| state.is_some_and(|s| s.contains_key(key))).collect::<Vec<_>>()
    }).collect();
    if !records.is_empty()
        && let Some(state) = doc.get_mut("hooks").and_then(|h| h.get_mut("state")).and_then(Item::as_table_like_mut)
    {
        for key in &records { state.remove(key); }
        if state.is_empty() && let Some(hooks) = doc.get_mut("hooks").and_then(Item::as_table_like_mut) { hooks.remove("state"); }
    }
    let Some(hooks) = doc.get_mut("hooks").and_then(|h| h.as_table_like_mut()) else { return false };
    let Some(groups) = hooks.get_mut("SessionStart").and_then(|g| g.as_array_of_tables_mut()) else {
        if hooks.is_empty() { doc.remove("hooks"); }
        return false;
    };
    let mut changed = false;
    for i in 0..groups.len() {
        let group = groups.get_mut(i).expect("in range");
        if is_codex_hook(group) {
            let mut placeholder = Table::new();
            placeholder.insert("matcher", toml_edit::value(CODEX_MATCHER));
            placeholder.decor_mut().set_prefix(format!("\n{CODEX_PLACEHOLDER}"));
            *group = placeholder;
            changed = true;
        }
    }
    if !changed { return false; }
    while let Some(last) = groups.len().checked_sub(1) && groups.get(last).is_some_and(is_codex_placeholder) { groups.remove(last); }
    let left = groups.iter().any(is_codex_placeholder);
    if groups.is_empty() { hooks.remove("SessionStart"); }
    if hooks.is_empty() { doc.remove("hooks"); }
    left
}

/// Whether Codex will run KnowCode's session-start hook, read from Codex's `config.toml` without changing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexHook {
    /// No entry of ours in `config.toml`.
    Missing,
    /// `[features] hooks = false`: Codex runs no hooks.
    HooksOff,
    /// The user turned it off in Codex's `/hooks`.
    Off,
    /// Codex has no record of the user trusting it, so it skips it.
    NotTrusted,
    /// The user trusted an earlier definition; Codex skips it until they review it again.
    Changed,
    /// Codex runs it.
    Trusted,
}

impl CodexHook {
    /// The one line `setup codex` and `status` give for this state, with the step that is left, if any.
    pub fn advice(self) -> &'static str {
        match self {
            CodexHook::Missing => "KnowCode's session-start hook is not in Codex's config.toml; run `knowcode setup codex` to add it.",
            CodexHook::HooksOff => "Hooks are turned off in Codex's config.toml (`[features] hooks = false`), so Codex doesn't run KnowCode's session-start hook.",
            CodexHook::Off => "You turned KnowCode's session-start hook off in Codex's /hooks; turn it on there to have it run.",
            CodexHook::NotTrusted => "One step left: Codex runs a hook only once you trust it. Start Codex; at \"Hooks need review\", choose \"Review hooks\" and trust the SessionStart hook that runs `knowcode hook session-start` (or do it later in /hooks). KnowCode updates don't change the hook, so you do this once.",
            CodexHook::Changed => "KnowCode's session-start hook is not the one you trusted (setup wrote a different binary path or directories), so Codex skips it until you review it again: start Codex and, at \"Hooks need review\", choose \"Review hooks\" (or use /hooks).",
            CodexHook::Trusted => "Codex trusts KnowCode's session-start hook; it runs when a session starts.",
        }
    }
}

/// The state of our hook in the Codex profile at `directory` (`CODEX_HOME`).
pub fn codex_hook_state(directory: &Path) -> Result<CodexHook> {
    let path = config_path(directory, "config.toml")?;
    let doc: DocumentMut = read(&path)?.unwrap_or_default().parse().with_context(|| format!("{} is not valid TOML", path.display()))?;
    Ok(codex_hook_state_in(&doc, directory))
}

fn codex_hook_state_in(doc: &DocumentMut, directory: &Path) -> CodexHook {
    let Some((index, hash)) = codex_hook_index(doc) else { return CodexHook::Missing };
    let features = doc.get("features").and_then(Item::as_table_like);
    // `codex_hooks` is the setting's older name.
    let feature = |name: &str| features.and_then(|f| f.get(name)).and_then(Item::as_bool);
    if feature("hooks").or(feature("codex_hooks")) == Some(false) { return CodexHook::HooksOff; }
    let state = codex_hook_states(doc);
    let records: Vec<&dyn toml_edit::TableLike> = codex_state_keys(directory, index).iter()
        .filter_map(|key| state.and_then(|s| s.get(key)).and_then(Item::as_table_like)).collect();
    if records.iter().any(|r| r.get("enabled").and_then(Item::as_bool) == Some(false)) { return CodexHook::Off; }
    let trusted: Vec<&str> = records.iter().filter_map(|r| r.get("trusted_hash").and_then(Item::as_str)).collect();
    if trusted.contains(&hash.as_str()) { CodexHook::Trusted }
    else if trusted.is_empty() { CodexHook::NotTrusted }
    else { CodexHook::Changed }
}

/// `[hooks.state]`, where Codex records which hooks the user trusted or turned off, by key.
fn codex_hook_states(doc: &DocumentMut) -> Option<&dyn toml_edit::TableLike> {
    doc.get("hooks").and_then(|h| h.get("state")).and_then(Item::as_table_like)
}

/// Our entry's place among the `SessionStart` entries, and the hash Codex gives its hook.
fn codex_hook_index(doc: &DocumentMut) -> Option<(usize, String)> {
    let groups = doc.get("hooks")?.get("SessionStart")?.as_array_of_tables()?;
    let (index, group) = groups.iter().enumerate().find(|(_, g)| is_codex_hook(g))?;
    Some((index, codex_hook_hash(group)?))
}

/// The keys Codex may record our hook's state under: `<config.toml>:session_start:<entry>:<hook>`, where the path is
/// `CODEX_HOME` with symlinks resolved when that is set, and `~/.codex` as it is otherwise.
fn codex_state_keys(directory: &Path, index: usize) -> Vec<String> {
    let mut dirs = vec![directory.to_path_buf()];
    if let Ok(real) = fs::canonicalize(directory) && real != directory { dirs.push(real); }
    dirs.iter().map(|d| format!("{}:session_start:{index}:0", d.join("config.toml").display())).collect()
}

/// The hash Codex trusts a SessionStart hook by: SHA-256 over the compact JSON, keys sorted, of the event, the
/// entry's matcher and the hook as Codex reads it (its timeout filled in). Codex's `hook_hash` and `version_for_toml`
/// (openai/codex, codex-rs/hooks/src/engine/discovery.rs and codex-rs/config/src/fingerprint.rs). Only the first hook
/// of the entry, the one setup writes; `None` for anything but a command hook.
fn codex_hook_hash(group: &Table) -> Option<String> {
    let hook = group.get("hooks")?.as_array_of_tables()?.get(0)?;
    if hook.get("type").and_then(Item::as_str) != Some("command") { return None; }
    // Keys are inserted in sorted order, so the JSON is sorted whether or not serde_json keeps insertion order.
    let mut handler = serde_json::Map::new();
    let limit = hook.get("additionalContextLimit").and_then(Item::as_integer).filter(|l| *l != 2500);
    if let Some(limit) = limit { handler.insert("additionalContextLimit".into(), json!(limit)); }
    handler.insert("async".into(), json!(hook.get("async").and_then(Item::as_bool).unwrap_or(false)));
    handler.insert("command".into(), json!(hook.get("command")?.as_str()?));
    if let Some(message) = hook.get("statusMessage").and_then(Item::as_str) { handler.insert("statusMessage".into(), json!(message)); }
    handler.insert("timeout".into(), json!(hook.get("timeout").and_then(Item::as_integer).unwrap_or(600).max(1)));
    handler.insert("type".into(), json!("command"));
    let mut identity = serde_json::Map::new();
    identity.insert("event_name".into(), json!("session_start"));
    identity.insert("hooks".into(), json!([handler]));
    if let Some(matcher) = group.get("matcher").and_then(Item::as_str) { identity.insert("matcher".into(), json!(matcher)); }
    let digest = sha256(serde_json::to_string(&Value::Object(identity)).ok()?.as_bytes());
    Some(format!("sha256:{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

/// SHA-256 (FIPS 180-4), for `codex_hook_hash`.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 { message.push(0); }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() { w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]); }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let t1 = hh.wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g)).wrapping_add(K[i]).wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)).wrapping_add((a & b) ^ (a & c) ^ (b & c));
            hh = g; g = f; f = e; e = d.wrapping_add(t1); d = c; c = b; b = a; a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) { *x = x.wrapping_add(y); }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() { out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes()); }
    out
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
        // Read back from what Codex will read; nothing here records trust, which is the user's to give in Codex.
        match codex_hook_state(&directory) {
            Ok(state) => println!("{}", state.advice()),
            Err(e) => println!("Codex runs KnowCode's session-start hook only once you trust it, in Codex's /hooks ({e:#})."),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // Two blocks: the padding does not fit after 56 bytes.
        assert_eq!(hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        assert_eq!(hex(&sha256(&[b'a'; 1000])), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    }

    /// The hashes codex-cli 0.163.0 reported for these entries (its app-server's `hooks/list`, `currentHash`).
    #[test]
    fn the_hook_hash_is_the_one_codex_reports() {
        let doc: DocumentMut = "[[hooks.SessionStart]]\nmatcher = \"^(startup|clear|compact)$\"\n\n[[hooks.SessionStart.hooks]]\ntype = \"command\"\ncommand = \"'/opt/example/bin/knowcode' hook session-start\"\ntimeout = 5\n\n[[hooks.SessionStart]]\nmatcher = \"^startup$\"\n\n[[hooks.SessionStart.hooks]]\ntype = \"command\"\ncommand = \"echo before\"\n".parse().unwrap();
        let groups = doc["hooks"]["SessionStart"].as_array_of_tables().unwrap();
        assert_eq!(codex_hook_hash(groups.get(0).unwrap()).unwrap(), "sha256:dade3b57ce42a5807f905fc8ea008b27f97a170616c2e2d4fbf29746a45379b8");
        // No timeout: Codex fills in its default of 600 seconds.
        assert_eq!(codex_hook_hash(groups.get(1).unwrap()).unwrap(), "sha256:255dd5e27df1666494531350237b79a97128e3217bf71a8f0d540cf2977eba72");
    }

    #[test]
    fn the_hook_state_is_read_from_codexs_own_records() {
        let dir = std::env::temp_dir().join(format!("knowcode-codex-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let entry = |command: &str| format!("[[hooks.SessionStart]]\nmatcher = \"^startup$\"\n\n[[hooks.SessionStart.hooks]]\ntype = \"command\"\ncommand = \"{command}\"\n\n");
        let ours = "[[hooks.SessionStart]]\nmatcher = \"^(startup|clear|compact)$\"\n\n[[hooks.SessionStart.hooks]]\ntype = \"command\"\ncommand = \"'/opt/example/bin/knowcode' hook session-start\"\ntimeout = 5\n\n";
        let hash = "sha256:dade3b57ce42a5807f905fc8ea008b27f97a170616c2e2d4fbf29746a45379b8";
        let key = |index: usize| format!("{}:session_start:{index}:0", dir.join("config.toml").display());
        let state = |text: &str| codex_hook_state_in(&text.parse().unwrap(), &dir);
        assert_eq!(state(&entry("echo mine")), CodexHook::Missing);
        // Ours is the second entry: its record is under index 1; the record under 0 is the user's hook's.
        let base = format!("{}{ours}", entry("echo mine"));
        assert_eq!(state(&base), CodexHook::NotTrusted);
        assert_eq!(state(&format!("{base}[hooks.state.\"{}\"]\ntrusted_hash = \"{hash}\"\n", key(0))), CodexHook::NotTrusted);
        let trusted = format!("{base}[hooks.state.\"{}\"]\ntrusted_hash = \"{hash}\"\n", key(1));
        assert_eq!(state(&trusted), CodexHook::Trusted);
        assert_eq!(state(&format!("{base}[hooks.state.\"{}\"]\ntrusted_hash = \"sha256:{}\"\n", key(1), "0".repeat(64))), CodexHook::Changed);
        assert_eq!(state(&format!("{trusted}enabled = false\n")), CodexHook::Off);
        assert_eq!(state(&format!("[features]\nhooks = false\n\n{trusted}")), CodexHook::HooksOff);
        assert_eq!(state(&format!("[features]\ncodex_hooks = false\n\n{trusted}")), CodexHook::HooksOff);
        // Uninstall drops our record with the entry and keeps the user's.
        let mut doc: DocumentMut = format!("{trusted}[hooks.state.\"{}\"]\ntrusted_hash = \"sha256:mine\"\n", key(0)).parse().unwrap();
        assert!(!remove_codex_hook(&mut doc, &dir));
        let records: Vec<String> = doc["hooks"]["state"].as_table_like().unwrap().iter().map(|(k, _)| k.to_string()).collect();
        assert_eq!(records, [key(0)]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_homebrew_keg_path_becomes_the_opt_path_that_survives_upgrades() {
        let root = std::env::temp_dir().join(format!("knowcode-brew-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Cellar/knowcode/0.4.0/bin")).unwrap();
        fs::create_dir_all(root.join("opt")).unwrap();
        fs::write(root.join("Cellar/knowcode/0.4.0/bin/knowcode"), "").unwrap();
        std::os::unix::fs::symlink("../Cellar/knowcode/0.4.0", root.join("opt/knowcode")).unwrap();
        assert_eq!(homebrew_opt(&root.join("Cellar/knowcode/0.4.0/bin/knowcode")), Some(root.join("opt/knowcode/bin/knowcode")));
        // Already stable, or not a keg of ours: left as it is.
        assert_eq!(homebrew_opt(&root.join("opt/knowcode/bin/knowcode")), None);
        fs::create_dir_all(root.join("Cellar/other/1.0/bin")).unwrap();
        fs::write(root.join("Cellar/other/1.0/bin/knowcode"), "").unwrap();
        assert_eq!(homebrew_opt(&root.join("Cellar/other/1.0/bin/knowcode")), None);
        // An opt link to another version does not stand for this binary.
        fs::create_dir_all(root.join("Cellar/knowcode/0.3.0/bin")).unwrap();
        fs::write(root.join("Cellar/knowcode/0.3.0/bin/knowcode"), "").unwrap();
        assert_eq!(homebrew_opt(&root.join("Cellar/knowcode/0.3.0/bin/knowcode")), None);
        let _ = fs::remove_dir_all(&root);
    }
}
