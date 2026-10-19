//! Remove recorded integrations and disposable indexes without stopping running clients.

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::Value;
use toml_edit::DocumentMut;

use crate::setup::{self, Client, Registration};

#[derive(Args)]
pub struct Options {
    /// Also remove KnowCode's configuration files (otherwise offered interactively, default no).
    #[arg(long)]
    pub delete_config: bool,
    /// Also clear all notes, including legacy migration backups (otherwise offered interactively, default no).
    #[arg(long)]
    pub delete_notes: bool,
}

fn ask(question: &str) -> Result<bool> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() { return Ok(false); }
    eprint!("{question} [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn read(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn remove_server(reg: &Registration) -> Result<()> {
    let filename = if reg.client == Client::Codex { "config.toml" } else { "mcp.json" };
    let path = setup::config_path(&reg.directory, filename)?;
    let Some(before) = read(&path)? else { return Ok(()) };
    let after = if reg.client == Client::Codex {
        let mut doc: DocumentMut = before.parse().with_context(|| format!("{} is not valid TOML; retained", path.display()))?;
        let Some(servers) = doc.get_mut("mcp_servers") else { return Ok(()) };
        let servers = servers.as_table_like_mut().context("mcp_servers must be a TOML table; retained")?;
        let Some(server) = servers.get("knowcode") else { return Ok(()) };
        let matches = server.get("command").and_then(|v| v.as_str()) == reg.command.to_str()
            && server.get("args").and_then(|v| v.as_array()).is_some_and(|a| a.len() == 1 && a.get(0).and_then(|v| v.as_str()) == Some("serve"));
        if !matches { bail!("{}: knowcode registration changed since setup; retained", path.display()); }
        servers.remove("knowcode");
        doc.to_string()
    } else {
        let mut doc: Value = serde_json::from_str(&before).with_context(|| format!("{} is not valid JSON; retained", path.display()))?;
        let Some(servers) = doc.get_mut("mcpServers") else { return Ok(()) };
        let servers = servers.as_object_mut().context("mcpServers must be a JSON object; retained")?;
        let Some(server) = servers.get("knowcode") else { return Ok(()) };
        if server["command"].as_str() != reg.command.to_str() || server["args"] != serde_json::json!(["serve"]) {
            bail!("{}: knowcode registration changed since setup; retained", path.display());
        }
        servers.remove("knowcode");
        format!("{}\n", serde_json::to_string_pretty(&doc)?)
    };
    setup::write_checked(&path, Some(&before), &after)
}

fn claude_json(reg: &Registration, args: &[&str]) -> Result<Vec<Value>> {
    let args: Vec<_> = args.iter().map(std::ffi::OsStr::new).collect();
    serde_json::from_slice(&setup::claude_output(&reg.directory, &args)?).context("Claude Code returned an unexpected registration list")
}

fn remove_claude(reg: &Registration) -> Result<()> {
    let expected = reg.marketplace.clone().unwrap_or(kc_index::data_dir()?.join("integrations/claude-marketplace"));
    let marketplaces = claude_json(reg, &["plugin", "marketplace", "list", "--json"])?;
    if let Some(marketplace) = marketplaces.iter().find(|m| m["name"] == "knowcode-local")
        && (marketplace["source"] != "directory" || marketplace["path"].as_str().map(Path::new) != Some(expected.as_path()))
    {
        bail!("knowcode-local marketplace changed since setup; retained in {}", reg.directory.display());
    }
    let plugins = claude_json(reg, &["plugin", "list", "--json"])?;
    if plugins.iter().any(|p| p["id"].as_str().is_some_and(|s| s.ends_with("@knowcode-local") && s != setup::PLUGIN)) {
        bail!("knowcode-local contains another installed plugin; retained in {}", reg.directory.display());
    }
    if let Some(plugin) = plugins.iter().find(|p| p["id"] == setup::PLUGIN && p["scope"] == "user") {
        if let Some(command) = plugin.pointer("/mcpServers/knowcode/command").and_then(Value::as_str)
            && Some(command) != reg.command.to_str()
        {
            bail!("Claude KnowCode plugin command changed since setup; retained");
        }
        setup::claude_output(&reg.directory, &["plugin".as_ref(), "uninstall".as_ref(), setup::PLUGIN.as_ref(), "--scope".as_ref(), "user".as_ref(), "--keep-data".as_ref()])?;
    }
    if marketplaces.iter().any(|m| m["name"] == "knowcode-local") {
        setup::claude_output(&reg.directory, &["plugin".as_ref(), "marketplace".as_ref(), "remove".as_ref(), "knowcode-local".as_ref(), "--scope".as_ref(), "user".as_ref()])?;
    }
    Ok(())
}

pub fn run(options: Options) -> Result<()> {
    let path = setup::registrations_path()?;
    let before = read(&path)?;
    let registrations: Vec<Registration> = serde_json::from_str(before.as_deref().unwrap_or("[]"))
        .with_context(|| format!("reading {}; registrations were retained", path.display()))?;
    let mut remaining = Vec::new();
    let mut errors = Vec::new();
    let mut bundles = std::collections::BTreeSet::new();
    let mut removed_claude = Vec::new();
    for registration in registrations {
        let result = match registration.client {
            Client::Claude => remove_claude(&registration),
            Client::Codex | Client::Cursor => remove_server(&registration),
        };
        match result {
            Ok(()) => {
                println!("removed {:?} registration in {}", registration.client, registration.directory.display());
                if registration.client == Client::Claude {
                    removed_claude.push(registration.clone());
                    bundles.insert(registration.marketplace.unwrap_or(kc_index::data_dir()?.join("integrations/claude-marketplace")));
                }
            },
            Err(e) => { errors.push(format!("{e:#}")); remaining.push(registration); },
        }
    }
    if !remaining.iter().any(|r| r.client == Client::Claude) {
        for bundle in bundles {
            if let Err(e) = setup::remove_bundle(&bundle) {
                errors.push(format!("{e:#}"));
                remaining.extend(removed_claude.iter().filter(|r| r.marketplace.as_ref().is_none_or(|p| p == &bundle)).cloned());
            }
        }
    }
    if before.is_some() {
        setup::write_checked(&path, before.as_deref(), &format!("{}\n", serde_json::to_string_pretty(&remaining)?))?;
    }
    let report = kc_index::cache::purge()?;
    println!("removed {} cached indexes", report.removed.len());
    if report.deferred { errors.push("cache maintenance is already running; retry uninstall later".into()); }
    for kept in report.kept { errors.push(format!("retained index {} ({})", kept.root.display(), kept.reason)); }
    let owned = kc_index::cache::remove_owned_state()?;
    if !owned.removed.is_empty() { println!("removed {} cache coordination and update-check files", owned.removed.len()); }
    for (path, reason) in owned.retained { errors.push(format!("retained {} ({reason})", path.display())); }

    let notes = kc_index::notes::db_path()?;
    let delete_notes = options.delete_notes || ask(&format!("Clear all KnowCode notes, including migration backups? Current path: {}", notes.display()))?;
    if delete_notes {
        for (path, count) in kc_index::notes::clear_all()? {
            println!("cleared {count} notes in {}", path.display());
        }
    } else { println!("retained notes at {}", notes.display()); }

    let config = kc_index::config_dir()?;
    let delete_config = options.delete_config || ask(&format!("Remove KnowCode configuration files in {}?", config.display()))?;
    if delete_config && errors.is_empty() && remaining.is_empty() {
        for name in ["config.toml", "cache_dir", "cache_dir.migrated", "ignore", "ignore.migrated", "bot_authors", "integrations.json"] {
            let path = config.join(name);
            if path == notes && !delete_notes { continue; }
            match fs::remove_file(&path) {
                Ok(()) => println!("removed {}", path.display()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {},
                Err(e) => errors.push(format!("retained {}: {e}", path.display())),
            }
        }
        let _ = fs::remove_dir(config); // only if empty; unrelated files stay
    } else { println!("retained configuration in {}", config.display()); }
    println!("Remove the binary through the package manager or installer used to install it. Running clients were not stopped.");
    if !errors.is_empty() { bail!("uninstall is incomplete; retry after resolving:\n{}", errors.join("\n")); }
    Ok(())
}
