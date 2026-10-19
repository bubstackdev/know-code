//! Client registration is isolated from the real user's profiles and must preserve their other settings.

use std::fs;
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
        self.write("bin/claude", "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$KNOWCODE_TEST_CLAUDE_LOG\"\nif [ \"$1 $2\" = 'plugin install' ] && [ \"${KNOWCODE_TEST_FAIL_INSTALL:-0}\" = 1 ]; then echo 'installation failed' >&2; exit 1; fi\n");
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
    for skill in ["knowcode-explore", "knowcode-before-edit", "knowcode-debug-stack-trace", "knowcode-safe-refactor"] {
        let text = fs::read_to_string(plugin.join(format!("skills/{skill}/SKILL.md"))).unwrap();
        assert!(text.contains("incomplete") && text.contains("assumptions"));
    }
    let hook: Value = serde_json::from_slice(&fs::read(plugin.join("hooks/hooks.json")).unwrap()).unwrap();
    assert_eq!(hook["hooks"]["PreToolUse"][0]["matcher"], "Grep|Glob");
    let mut cmd = p.command("claude");
    cmd.env("PATH", path).env("KNOWCODE_TEST_CLAUDE_LOG", &log);
    success(cmd.output().unwrap());
    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("marketplace\nadd\n"));
    assert!(log.contains("plugin\ninstall\nknowcode@knowcode-local\n--scope\nuser"));
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
    let hook = p.dir("data/integrations/claude-marketplace/knowcode/hooks/knowcode-grep-hint.sh");
    let out = Command::new("sh").arg(hook).current_dir(&p.root).stdin(Stdio::null()).output().unwrap();
    success(out);
    assert!(!p.dir("injected").exists());
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
