//! Client registration is isolated from the real user's profiles and must preserve their other settings.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use toml_edit::DocumentMut;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

struct Profile { root: PathBuf }

impl Profile {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("knowcode-test-setup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Profile { root }
    }

    fn dir(&self, name: &str) -> PathBuf { self.root.join(name) }

    fn command(&self, client: &str) -> Command {
        let mut cmd = Command::new(KC);
        cmd.args(["setup", client, "--client-dir"]).arg(self.dir(client))
            .env("KNOWCODE_CONFIG_DIR", self.dir("settings"))
            .env("KNOWCODE_CACHE_DIR", self.dir("cache"))
            .env("KNOWCODE_DATA_DIR", self.dir("data"));
        cmd
    }

    fn run(&self, client: &str, args: &[&str]) -> Output {
        self.command(client).args(args).output().unwrap()
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.dir(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn mock_claude(&self) -> std::ffi::OsString {
        self.write("bin/claude", "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$KNOWCODE_TEST_CLAUDE_LOG\"\nif [ \"$1 $2\" = 'plugin install' ] && [ \"${KNOWCODE_TEST_FAIL_INSTALL:-0}\" = 1 ]; then echo 'installation failed' >&2; exit 1; fi\nif [ \"$1 $2\" = 'plugin update' ] && [ \"${KNOWCODE_TEST_FAIL_UPDATE:-0}\" = 1 ]; then echo 'update failed' >&2; exit 1; fi\n");
        fs::set_permissions(self.dir("bin/claude"), fs::Permissions::from_mode(0o700)).unwrap();
        let mut paths = vec![self.dir("bin")];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        std::env::join_paths(paths).unwrap()
    }
}

impl Drop for Profile {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); }
}

fn success(out: Output) -> String {
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn codex_setup_keeps_comments_other_servers_and_a_symlink_and_is_idempotent() {
    let p = Profile::new("codex");
    p.write("actual.toml", "# keep this comment\nmodel = 'example-model'\n[mcp_servers.other]\ncommand = 'other-server'\n");
    fs::set_permissions(p.dir("actual.toml"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::create_dir_all(p.dir("codex")).unwrap();
    symlink(p.dir("actual.toml"), p.dir("codex/config.toml")).unwrap();
    success(p.run("codex", &[]));
    let text = fs::read_to_string(p.dir("actual.toml")).unwrap();
    let doc: DocumentMut = text.parse().unwrap();
    assert!(text.contains("# keep this comment"));
    assert_eq!(doc["model"].as_str(), Some("example-model"));
    assert_eq!(doc["mcp_servers"]["other"]["command"].as_str(), Some("other-server"));
    assert_eq!(doc["mcp_servers"]["knowcode"]["command"].as_str(), Some(KC));
    assert_eq!(doc["mcp_servers"]["knowcode"]["args"][0].as_str(), Some("serve"));
    assert!(fs::symlink_metadata(p.dir("codex/config.toml")).unwrap().is_symlink());
    assert_eq!(fs::metadata(p.dir("actual.toml")).unwrap().permissions().mode() & 0o777, 0o640);
    success(p.run("codex", &[]));
    assert_eq!(fs::read_to_string(p.dir("actual.toml")).unwrap(), text);
    let ledger: Value = serde_json::from_slice(&fs::read(p.dir("settings/integrations.json")).unwrap()).unwrap();
    assert_eq!(ledger.as_array().unwrap().len(), 1);
    let mut command = p.command("codex");
    command.env("KNOWCODE_CACHE_DIR", p.dir("moved-cache"));
    success(command.output().unwrap());
    let doc: DocumentMut = fs::read_to_string(p.dir("actual.toml")).unwrap().parse().unwrap();
    assert_eq!(doc["mcp_servers"]["knowcode"]["env"]["KNOWCODE_CACHE_DIR"].as_str(), p.dir("moved-cache").to_str());
    assert_eq!(doc["mcp_servers"]["other"]["command"].as_str(), Some("other-server"));
}

/// `setup codex` adds a user-level SessionStart hook beside the server entry, in `CODEX_HOME`: one entry however often
/// setup runs, the user's own hooks kept, its command naming only the binary and the directory overrides (so the line
/// it prints can change without Codex asking for trust again), and the output says it must be trusted in /hooks.
#[test]
fn codex_setup_adds_a_session_start_hook_to_trust() {
    let p = Profile::new("codex-hook");
    let user_hook = "[[hooks.SessionStart]]\nmatcher = \"^startup$\"\n\n[[hooks.SessionStart.hooks]]\ntype = \"command\"\ncommand = \"echo mine\"\n";
    p.write("home/config.toml", user_hook);
    let setup = |cache: &str| {
        let mut cmd = Command::new(KC);
        cmd.args(["setup", "codex"]).env("CODEX_HOME", p.dir("home")).env("KNOWCODE_CONFIG_DIR", p.dir("settings"))
            .env("KNOWCODE_CACHE_DIR", p.dir(cache)).env("KNOWCODE_DATA_DIR", p.dir("data"));
        success(cmd.output().unwrap())
    };
    let out = setup("cache");
    assert!(out.contains("/hooks") && out.contains("trust"), "setup says the hook needs trust: {out}");
    let hooks = |doc: &DocumentMut| -> Vec<(String, String)> {
        doc["hooks"]["SessionStart"].as_array_of_tables().unwrap().iter()
            .map(|g| (g["matcher"].as_str().unwrap().to_string(), g["hooks"].as_array_of_tables().unwrap().get(0).unwrap()["command"].as_str().unwrap().to_string()))
            .collect()
    };
    let text = fs::read_to_string(p.dir("home/config.toml")).unwrap();
    let doc: DocumentMut = text.parse().unwrap();
    assert_eq!(doc["mcp_servers"]["knowcode"]["command"].as_str(), Some(KC));
    let cache = p.dir("cache");
    let ours = format!("KNOWCODE_CACHE_DIR='{}' KNOWCODE_CONFIG_DIR='{}' KNOWCODE_DATA_DIR='{}' '{KC}' hook session-start",
        cache.display(), p.dir("settings").display(), p.dir("data").display());
    assert_eq!(hooks(&doc), vec![("^startup$".into(), "echo mine".into()), ("^(startup|clear|compact)$".into(), ours)]);
    assert_eq!(doc["hooks"]["SessionStart"][1]["hooks"][0]["timeout"].as_integer(), Some(5));
    setup("cache");
    assert_eq!(fs::read_to_string(p.dir("home/config.toml")).unwrap(), text, "setup again changes nothing");
    // A moved cache: the same one entry, with the server's new override.
    setup("moved");
    let doc: DocumentMut = fs::read_to_string(p.dir("home/config.toml")).unwrap().parse().unwrap();
    let h = hooks(&doc);
    assert_eq!(h.len(), 2, "{h:?}");
    assert!(h[1].1.starts_with(&format!("KNOWCODE_CACHE_DIR='{}' ", p.dir("moved").display())), "{h:?}");
    assert_eq!(h[0].1, "echo mine");
}

#[test]
fn cursor_setup_preserves_other_settings_and_conflicts_require_replacement() {
    let p = Profile::new("cursor");
    let initial = json!({"otherSetting": true, "mcpServers": {"other": {"url": "https://example.test/mcp"}, "knowcode": {"command": "another-program", "args": ["custom"]}}});
    p.write("cursor/mcp.json", &initial.to_string());
    let failed = p.run("cursor", &[]);
    assert!(!failed.status.success());
    assert_eq!(fs::read_to_string(p.dir("cursor/mcp.json")).unwrap(), initial.to_string());
    assert!(!p.dir("settings/integrations.json").exists());
    success(p.run("cursor", &["--replace"]));
    let text = fs::read_to_string(p.dir("cursor/mcp.json")).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["otherSetting"], true);
    assert_eq!(doc["mcpServers"]["other"], initial["mcpServers"]["other"]);
    assert_eq!(doc["mcpServers"]["knowcode"]["command"], KC);
    success(p.run("cursor", &[]));
    assert_eq!(fs::read_to_string(p.dir("cursor/mcp.json")).unwrap(), text);
}

#[test]
fn malformed_configs_are_unchanged_and_print_never_installs() {
    let p = Profile::new("invalid");
    for (client, file, text) in [("codex", "config.toml", "mcp_servers = ["), ("cursor", "mcp.json", "{broken")] {
        p.write(&format!("{client}/{file}"), text);
        assert!(!p.run(client, &[]).status.success());
        assert_eq!(fs::read_to_string(p.dir(&format!("{client}/{file}"))).unwrap(), text);
        let printed = success(p.run(client, &["--print"]));
        assert!(printed.contains("serve") && printed.contains("knowcode"));
        assert!(!p.dir("settings").exists());
    }
    let p = Profile::new("print-claude");
    success(p.run("claude", &["--print"]));
    assert_eq!(fs::read_dir(&p.root).unwrap().count(), 0);
}

#[test]
fn claude_installs_embedded_plugin_and_records_partial_install_for_removal() {
    let p = Profile::new("claude");
    let path = p.mock_claude();
    let log = p.dir("claude.log");
    let mut cmd = p.command("claude");
    cmd.env("PATH", &path).env("KNOWCODE_TEST_CLAUDE_LOG", &log).env("KNOWCODE_TEST_FAIL_INSTALL", "1");
    assert!(!cmd.output().unwrap().status.success());
    assert!(p.dir("settings/integrations.json").exists(), "a partial install must remain removable");
    let plugin = p.dir("data/integrations/claude-marketplace/knowcode");
    let config: Value = serde_json::from_slice(&fs::read(plugin.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["knowcode"]["command"], KC);
    assert_eq!(config["mcpServers"]["knowcode"]["args"], json!(["serve"]));
    // Claude Code defers the non-core tools, and the skills use them: the plugin's server advertises them all.
    assert_eq!(config["mcpServers"]["knowcode"]["env"]["KNOWCODE_TOOLS"], "all");
    for skill in ["knowcode-explore", "knowcode-before-edit", "knowcode-debug-stack-trace", "knowcode-safe-refactor"] {
        let text = fs::read_to_string(plugin.join(format!("skills/{skill}/SKILL.md"))).unwrap();
        assert!(text.contains("incomplete") && text.contains("assumptions"));
    }
    let hook: Value = serde_json::from_slice(&fs::read(plugin.join("hooks/hooks.json")).unwrap()).unwrap();
    assert_eq!(hook["hooks"]["PreToolUse"][0]["matcher"], "Grep|Glob|Bash");
    // The session-start note: a new session, /clear and compaction, which leave the context without it.
    assert_eq!(hook["hooks"]["SessionStart"][0]["matcher"], "startup|clear|compact");
    let command = hook["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str().unwrap();
    let script = command.split('"').nth(1).unwrap().replace("${CLAUDE_PLUGIN_ROOT}", plugin.to_str().unwrap());
    let text = fs::read_to_string(&script).unwrap();
    assert!(text.starts_with(&format!("#!/bin/sh\nexport KNOWCODE_BIN='{KC}'\n")) && text.contains("hook session-start"), "{text}");
    let mut cmd = p.command("claude");
    cmd.env("PATH", path).env("KNOWCODE_TEST_CLAUDE_LOG", &log);
    success(cmd.output().unwrap());
    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("marketplace\nadd\n"));
    assert!(log.contains("plugin\ninstall\nknowcode@knowcode-local\n--scope\nuser"));
    assert!(log.contains("plugin\nupdate\nknowcode@knowcode-local\n--scope\nuser"), "setup moves an installed plugin to this version");
    let manifest: Value = serde_json::from_slice(&fs::read(plugin.join(".claude-plugin/plugin.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn bundled_hook_quotes_a_binary_path_with_shell_metacharacters() {
    let p = Profile::new("quote");
    let binary = p.dir("knowcode ' $(touch injected)");
    symlink(KC, &binary).unwrap();
    let path = p.mock_claude();
    let mut cmd = Command::new(&binary);
    cmd.args(["setup", "claude", "--client-dir"]).arg(p.dir("claude"))
        .env("KNOWCODE_CONFIG_DIR", p.dir("settings")).env("KNOWCODE_DATA_DIR", p.dir("data"))
        .env("KNOWCODE_CACHE_DIR", p.dir("cache")).env("PATH", path)
        .env("KNOWCODE_TEST_CLAUDE_LOG", p.dir("claude.log"));
    success(cmd.output().unwrap());
    for script in ["knowcode-grep-hint.sh", "knowcode-session-start.sh"] {
        let hook = p.dir(&format!("data/integrations/claude-marketplace/knowcode/hooks/{script}"));
        let out = Command::new("sh").arg(hook).current_dir(&p.root).stdin(Stdio::null()).output().unwrap();
        success(out);
        assert!(!p.dir("injected").exists(), "{script}");
    }
}

#[test]
fn codex_name_conflicts_are_preserved_until_replace_is_explicit() {
    let p = Profile::new("codex-conflict");
    let original = "[mcp_servers.knowcode]\nurl = 'https://example.test/other'\n[mcp_servers.other]\ncommand = 'keep'\n";
    p.write("codex/config.toml", original);
    assert!(!p.run("codex", &[]).status.success());
    assert_eq!(fs::read_to_string(p.dir("codex/config.toml")).unwrap(), original);
    success(p.run("codex", &["--replace"]));
    let doc: DocumentMut = fs::read_to_string(p.dir("codex/config.toml")).unwrap().parse().unwrap();
    assert_eq!(doc["mcp_servers"]["other"]["command"].as_str(), Some("keep"));
    assert!(doc["mcp_servers"]["knowcode"].get("url").is_none());
    assert_eq!(doc["mcp_servers"]["knowcode"]["command"].as_str(), Some(KC));
}

#[test]
fn claude_registration_deadline_leaves_a_removable_partial_install() {
    let p = Profile::new("deadline");
    let path = p.mock_claude();
    p.write("bin/claude", "#!/bin/sh\nexec sleep 30\n");
    let start = std::time::Instant::now();
    let out = p.command("claude").env("PATH", path).env("KNOWCODE_SETUP_DEADLINE", "1").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("deadline"));
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert!(p.dir("settings/integrations.json").exists());
}

const PLUGIN_DIR: &str = "data/integrations/claude-marketplace/knowcode";

/// Sets Claude Code up with the mock CLI, then makes the bundle look like an older version wrote it.
fn older_bundle(p: &Profile) -> std::ffi::OsString {
    let path = p.mock_claude();
    let mut cmd = p.command("claude");
    cmd.env("PATH", &path).env("KNOWCODE_TEST_CLAUDE_LOG", p.dir("setup.log"));
    success(cmd.output().unwrap());
    let manifest = p.dir(&format!("{PLUGIN_DIR}/.claude-plugin/plugin.json"));
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["version"] = json!("0.0.1");
    fs::write(&manifest, value.to_string()).unwrap();
    p.write(&format!("{PLUGIN_DIR}/hooks/knowcode-grep-hint.sh"), "#!/bin/sh\nexec knowcode hook claude-code\n");
    path
}

/// `knowcode serve` as a client runs it: it answers `initialize`, then stdin closes and it exits. Returns its stderr.
fn serve(p: &Profile, path: &std::ffi::OsStr, fail_update: bool) -> String {
    let mut child = Command::new(KC);
    child.arg("serve").current_dir(&p.root)
        .env("KNOWCODE_CONFIG_DIR", p.dir("settings")).env("KNOWCODE_CACHE_DIR", p.dir("cache"))
        .env("KNOWCODE_DATA_DIR", p.dir("data")).env("KNOWCODE_UPDATE_CHECK", "0")
        .env("PATH", path).env("KNOWCODE_TEST_CLAUDE_LOG", p.dir("claude.log"))
        .env("KNOWCODE_TEST_FAIL_UPDATE", if fail_update { "1" } else { "0" })
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = child.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}})).unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("the server answers initialize: {line}"));
    assert_eq!(reply["id"], 1);
    assert!(reply["result"]["serverInfo"].is_object(), "{reply}");
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stderr).unwrap()
}

/// A binary newer than its bundle with no self-update run: how `brew upgrade` (or any package manager) leaves it.
#[test]
fn serve_refreshes_an_older_plugin_bundle_and_has_claude_update_its_copy() {
    let p = Profile::new("refresh");
    let path = older_bundle(&p);
    let stderr = serve(&p, &path, false);
    assert!(stderr.contains(&format!("updated the Claude Code plugin to {}", env!("CARGO_PKG_VERSION"))), "{stderr}");
    let plugin = p.dir(PLUGIN_DIR);
    let manifest: Value = serde_json::from_slice(&fs::read(plugin.join(".claude-plugin/plugin.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    let hook = fs::read_to_string(plugin.join("hooks/knowcode-grep-hint.sh")).unwrap();
    assert!(hook.starts_with(&format!("#!/bin/sh\nexport KNOWCODE_BIN='{KC}'\n")), "{hook}");
    assert!(hook.contains("\nKC=\"${KNOWCODE_BIN:-knowcode}\"\n"), "the current hook script: {hook}");
    // The overrides setup recorded stay in the server configuration.
    let config: Value = serde_json::from_slice(&fs::read(plugin.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["knowcode"]["env"]["KNOWCODE_CACHE_DIR"], p.dir("cache").to_str().unwrap());
    let log = fs::read_to_string(p.dir("claude.log")).unwrap();
    assert_eq!(log, "plugin\nupdate\nknowcode@knowcode-local\n--scope\nuser\n");
    // A current bundle is left alone.
    let stderr = serve(&p, &path, false);
    assert!(!stderr.contains("Claude Code plugin"), "{stderr}");
    assert_eq!(fs::read_to_string(p.dir("claude.log")).unwrap(), log);
}

#[test]
fn a_failed_plugin_update_is_reported_once_and_the_server_still_works() {
    let p = Profile::new("refresh-fails");
    let path = older_bundle(&p);
    let stderr = serve(&p, &path, true);
    assert_eq!(stderr.matches("could not update the Claude Code plugin").count(), 1, "{stderr}");
    assert!(stderr.contains("update failed"), "{stderr}");
    let finish = format!("knowcode setup claude --client-dir '{}'", p.dir("claude").display());
    assert!(stderr.contains(&finish), "{stderr}");
    // Finishing it: setup rewrites the bundle and moves Claude's copy to this version.
    let mut cmd = p.command("claude");
    cmd.env("PATH", &path).env("KNOWCODE_TEST_CLAUDE_LOG", p.dir("finish.log"));
    success(cmd.output().unwrap());
    assert!(fs::read_to_string(p.dir("finish.log")).unwrap().contains("plugin\nupdate\nknowcode@knowcode-local"));
}

#[test]
fn the_refresh_after_self_update_reports_a_missing_claude_and_never_fails() {
    let p = Profile::new("refresh-no-claude");
    older_bundle(&p);
    fs::create_dir_all(p.dir("empty-bin")).unwrap();
    let out = Command::new(KC).arg("_refresh-plugin")
        .env("KNOWCODE_CONFIG_DIR", p.dir("settings")).env("KNOWCODE_CACHE_DIR", p.dir("cache"))
        .env("KNOWCODE_DATA_DIR", p.dir("data")).env("PATH", format!("{}:/usr/bin:/bin", p.dir("empty-bin").display()))
        .output().unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("could not update the Claude Code plugin") && stderr.contains("install the claude CLI"), "{stderr}");
    assert!(stderr.contains("knowcode setup claude --client-dir"), "{stderr}");
    let manifest: Value = serde_json::from_slice(&fs::read(p.dir(&format!("{PLUGIN_DIR}/.claude-plugin/plugin.json"))).unwrap()).unwrap();
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"), "the bundle itself is current");
}

#[test]
fn only_the_bundle_at_the_recorded_path_is_refreshed() {
    let p = Profile::new("refresh-elsewhere");
    let path = older_bundle(&p);
    // A process with another data directory finds no bundle of its own to refresh.
    let out = Command::new(KC).arg("_refresh-plugin")
        .env("KNOWCODE_CONFIG_DIR", p.dir("settings")).env("KNOWCODE_CACHE_DIR", p.dir("cache"))
        .env("KNOWCODE_DATA_DIR", p.dir("other-data")).env("PATH", &path).env("KNOWCODE_TEST_CLAUDE_LOG", p.dir("claude.log"))
        .output().unwrap();
    assert!(out.status.success() && out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!p.dir("claude.log").exists());
    assert!(!p.dir("other-data").exists());
}
