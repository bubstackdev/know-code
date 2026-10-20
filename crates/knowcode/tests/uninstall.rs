//! Uninstall operates only on test-owned profiles, preserving other settings and live work.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use toml_edit::DocumentMut;

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

struct Profile { root: PathBuf }
impl Profile {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("knowcode-test-uninstall-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn path(&self, name: &str) -> PathBuf { self.root.join(name) }
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(KC);
        cmd.args(args).current_dir(&self.root).stdin(Stdio::null())
            .env("KNOWCODE_CONFIG_DIR", self.path("config")).env("KNOWCODE_CACHE_DIR", self.path("cache"))
            .env("KNOWCODE_DATA_DIR", self.path("data"));
        cmd
    }
    fn run(&self, args: &[&str]) -> Output { self.command(args).output().unwrap() }
    fn write(&self, name: &str, text: &str) {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn setup(&self, client: &str) {
        success(self.run(&["setup", client, "--client-dir", self.path(client).to_str().unwrap()]));
    }
}
impl Drop for Profile { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }
fn success(out: Output) {
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}
fn notes(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    rusqlite::Connection::open(path).unwrap().execute_batch("CREATE TABLE notes (text TEXT); INSERT INTO notes VALUES ('a durable decision');").unwrap();
}
fn note_count(path: &Path) -> i64 {
    rusqlite::Connection::open(path).unwrap().query_row("SELECT count(*) FROM notes", [], |r| r.get(0)).unwrap()
}

#[test]
fn invariant_uninstall_preserves_settings_notes_and_unrelated_files_by_default() {
    let p = Profile::new("preserve");
    p.write("actual.toml", "# keep this\nmodel = 'example'\n[mcp_servers.other]\ncommand = 'other-server'\n");
    fs::set_permissions(p.path("actual.toml"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::create_dir_all(p.path("codex")).unwrap();
    symlink(p.path("actual.toml"), p.path("codex/config.toml")).unwrap();
    p.write("cursor/mcp.json", &json!({"custom": true, "mcpServers": {"other": {"url": "https://example.test/mcp"}}}).to_string());
    p.setup("codex");
    p.setup("cursor");
    p.write("config/config.toml", "[cache]\nmax_size = 0\n");
    p.write("cache/unrelated/work", "keep");
    p.write("config/unrelated", "keep");
    notes(&p.path("data/notes.sqlite"));
    fs::create_dir_all(p.path("repo")).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).arg(p.path("repo")).status().unwrap().success());
    p.write("repo/api.ts", "export function handler() { return 42; }\n");
    success(p.run(&["index", p.path("repo").to_str().unwrap(), "--threads", "1"]));
    assert!(p.path("cache/locks").is_dir(), "indexing leaves coordination locks");
    p.write("cache/updates/state.json", "{\"format\":1}");
    p.write("cache/updates/unrelated", "keep");
    p.write("cache/locks/unrelated", "keep");
    success(p.run(&["uninstall"]));
    assert!(!p.path("cache/updates/state.json").exists(), "the update check's state is KnowCode's");
    assert_eq!(fs::read_to_string(p.path("cache/updates/unrelated")).unwrap(), "keep");
    let locks: Vec<String> = fs::read_dir(p.path("cache/locks")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(locks, vec!["unrelated".to_string()], "KnowCode's locks are removed, other files stay");
    let text = fs::read_to_string(p.path("actual.toml")).unwrap();
    let doc: DocumentMut = text.parse().unwrap();
    assert!(text.contains("# keep this"));
    assert!(doc["mcp_servers"].get("knowcode").is_none());
    assert_eq!(doc["mcp_servers"]["other"]["command"].as_str(), Some("other-server"));
    assert!(fs::symlink_metadata(p.path("codex/config.toml")).unwrap().is_symlink());
    assert_eq!(fs::metadata(p.path("actual.toml")).unwrap().permissions().mode() & 0o777, 0o640);
    let cursor: Value = serde_json::from_str(&fs::read_to_string(p.path("cursor/mcp.json")).unwrap()).unwrap();
    assert_eq!(cursor["custom"], true);
    assert!(cursor["mcpServers"].get("knowcode").is_none());
    assert_eq!(cursor["mcpServers"]["other"]["url"], "https://example.test/mcp");
    assert_eq!(note_count(&p.path("data/notes.sqlite")), 1);
    assert!(p.path("config/config.toml").exists());
    assert_eq!(fs::read_to_string(p.path("cache/unrelated/work")).unwrap(), "keep");
    assert!(!p.path("cache/repos").exists());
    success(p.run(&["uninstall"]));
    success(p.run(&["uninstall", "--delete-config", "--delete-notes"]));
    assert_eq!(note_count(&p.path("data/notes.sqlite")), 0);
    assert!(!p.path("config/config.toml").exists());
    assert_eq!(fs::read_to_string(p.path("config/unrelated")).unwrap(), "keep");
}

#[test]
fn invariant_changed_or_malformed_client_settings_are_retained_with_a_retry_ledger() {
    let p = Profile::new("changed");
    p.setup("codex");
    p.setup("cursor");
    let changed = "[mcp_servers.knowcode]\ncommand = 'different-program'\nargs = ['serve']\n";
    p.write("codex/config.toml", changed);
    p.write("cursor/mcp.json", "{broken");
    let out = p.run(&["uninstall", "--delete-config"]);
    assert!(!out.status.success());
    assert_eq!(fs::read_to_string(p.path("codex/config.toml")).unwrap(), changed);
    assert_eq!(fs::read_to_string(p.path("cursor/mcp.json")).unwrap(), "{broken");
    let ledger: Vec<Value> = serde_json::from_slice(&fs::read(p.path("config/integrations.json")).unwrap()).unwrap();
    assert_eq!(ledger.len(), 2);
    fs::remove_file(p.path("codex/config.toml")).unwrap();
    fs::remove_file(p.path("cursor/mcp.json")).unwrap();
    success(p.run(&["uninstall", "--delete-config"]));
    assert!(!p.path("config/integrations.json").exists());
}

#[test]
fn invariant_note_deletion_clears_migration_copies_and_respects_a_live_writer() {
    let p = Profile::new("notes");
    notes(&p.path("data/notes.sqlite"));
    notes(&p.path("cache/notes.sqlite"));
    notes(&p.path("cache/notes.sqlite.migrated"));
    notes(&p.path("cache/notes.sqlite.migrated.1"));
    p.write("cache/notes.sqlite.migrated.unrelated", "keep");
    let writer = rusqlite::Connection::open(p.path("data/notes.sqlite")).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let out = p.run(&["uninstall", "--delete-notes"]);
    assert!(!out.status.success());
    assert_eq!(note_count(&p.path("data/notes.sqlite")), 1);
    writer.execute_batch("ROLLBACK").unwrap();
    success(p.run(&["uninstall", "--delete-notes"]));
    for path in ["data/notes.sqlite", "cache/notes.sqlite", "cache/notes.sqlite.migrated", "cache/notes.sqlite.migrated.1"] {
        assert_eq!(note_count(&p.path(path)), 0, "{path}");
    }
    assert_eq!(fs::read_to_string(p.path("cache/notes.sqlite.migrated.unrelated")).unwrap(), "keep");
}

/// A stand-in for the `claude` CLI that keeps its marketplace and plugin lists in files, so removals show in later
/// listings. `KNOWCODE_TEST_UNDECLARED=1` rejects the user-scoped marketplace removal the way Claude Code does when the
/// marketplace is known but not declared in user settings; `KNOWCODE_TEST_STUCK=1` reports success yet keeps it listed.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$KNOWCODE_TEST_CLAUDE_LOG"
case "$*" in
  'plugin marketplace list --json') cat "$KNOWCODE_TEST_MARKETS";;
  'plugin list --json') cat "$KNOWCODE_TEST_PLUGINS";;
  'plugin marketplace add '*|'plugin install '*|'plugin update '*) ;;
  'plugin uninstall knowcode@knowcode-local --scope user')
    if [ "${KNOWCODE_TEST_FAIL_REMOVE:-0}" = 1 ]; then echo 'try again' >&2; exit 1; fi
    printf '%s\n' "$KNOWCODE_TEST_OTHER_PLUGINS" > "$KNOWCODE_TEST_PLUGINS";;
  'plugin marketplace remove knowcode-local --scope user')
    if [ "${KNOWCODE_TEST_UNDECLARED:-0}" = 1 ]; then
      echo "Failed to remove marketplace: Marketplace 'knowcode-local' is not declared in user settings. Omit --scope to remove it from all scopes." >&2
      exit 1
    fi
    [ "${KNOWCODE_TEST_STUCK:-0}" = 1 ] || echo '[]' > "$KNOWCODE_TEST_MARKETS";;
  'plugin marketplace remove knowcode-local')
    pwd > "$KNOWCODE_TEST_CLAUDE_LOG.cwd"
    [ "${KNOWCODE_TEST_STUCK:-0}" = 1 ] || echo '[]' > "$KNOWCODE_TEST_MARKETS";;
  *) echo "unexpected claude command: $*" >&2; exit 2;;
esac
"#;

struct Claude { p: Profile, path: std::ffi::OsString }
impl Claude {
    fn new(name: &str) -> Self {
        let p = Profile::new(name);
        p.write("bin/claude", FAKE_CLAUDE);
        fs::set_permissions(p.path("bin/claude"), fs::Permissions::from_mode(0o700)).unwrap();
        let mut paths = vec![p.path("bin")];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        let path = std::env::join_paths(paths).unwrap();
        let marketplace = p.path("data/integrations/claude-marketplace");
        p.write("markets.json", &json!([{"name": "knowcode-local", "source": "directory", "path": marketplace}]).to_string());
        p.write("plugins.json", &json!([{"id": "knowcode@knowcode-local", "scope": "user", "mcpServers": {"knowcode": {"command": KC}}}, {"id": "other@another", "scope": "user"}]).to_string());
        fs::create_dir_all(p.path("claude")).unwrap();
        let claude = Self { p, path };
        success(claude.command(&["setup", "claude", "--client-dir", claude.p.path("claude").to_str().unwrap()]).output().unwrap());
        claude
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = self.p.command(args);
        cmd.env("PATH", &self.path).env("KNOWCODE_TEST_CLAUDE_LOG", self.p.path("claude.log"))
            .env("KNOWCODE_TEST_MARKETS", self.p.path("markets.json")).env("KNOWCODE_TEST_PLUGINS", self.p.path("plugins.json"))
            .env("KNOWCODE_TEST_OTHER_PLUGINS", json!([{"id": "other@another", "scope": "user"}]).to_string());
        cmd
    }
    fn log(&self) -> String { fs::read_to_string(self.p.path("claude.log")).unwrap() }
    fn ledger(&self) -> Vec<Value> { serde_json::from_slice(&fs::read(self.p.path("config/integrations.json")).unwrap()).unwrap() }
}

#[test]
fn claude_removal_is_user_scoped_preserves_extra_bundle_files_and_retries_failures() {
    let c = Claude::new("claude");
    let p = &c.p;
    let marketplace = p.path("data/integrations/claude-marketplace");
    p.write("data/integrations/claude-marketplace/keep.txt", "user addition");
    let out = c.command(&["uninstall", "--delete-config"]).env("KNOWCODE_TEST_FAIL_REMOVE", "1").output().unwrap();
    assert!(!out.status.success());
    assert!(p.path("config/integrations.json").exists());
    assert!(marketplace.join("knowcode/.mcp.json").exists());
    success(c.command(&["uninstall", "--delete-config"]).output().unwrap());
    assert!(!p.path("config/integrations.json").exists());
    assert!(!marketplace.join("knowcode/.mcp.json").exists());
    assert_eq!(fs::read_to_string(marketplace.join("keep.txt")).unwrap(), "user addition");
    let log = c.log();
    assert!(log.lines().any(|l| l == "plugin uninstall knowcode@knowcode-local --scope user"), "{log}");
    assert!(!log.contains("--keep-data"), "Claude Code deletes the plugin's data directory: {log}");
    assert!(log.contains("plugin marketplace remove knowcode-local --scope user"), "{log}");
    assert!(!log.lines().any(|l| l == "plugin marketplace remove knowcode-local"), "a user-scoped removal that worked needs no other: {log}");
    assert!(!log.contains("plugin uninstall other"));
}

#[test]
fn claude_marketplace_not_declared_in_user_settings_is_removed_from_every_scope() {
    let c = Claude::new("undeclared");
    let p = &c.p;
    p.write("cache/updates/check.lock", "");
    p.write("cache/updates/state.json", "{\"format\":1}");
    let out = c.command(&["uninstall", "--delete-config"]).env("KNOWCODE_TEST_UNDECLARED", "1").output().unwrap();
    success(out);
    let log = c.log();
    let removes: Vec<_> = log.lines().filter(|l| l.starts_with("plugin marketplace remove")).collect();
    assert_eq!(removes, ["plugin marketplace remove knowcode-local --scope user", "plugin marketplace remove knowcode-local"], "{log}");
    let cwd = fs::read_to_string(p.path("claude.log.cwd")).unwrap();
    assert_eq!(fs::canonicalize(cwd.trim()).unwrap(), fs::canonicalize(p.path("claude")).unwrap(), "the scope-less removal runs outside any project");
    assert_eq!(fs::read_to_string(p.path("markets.json")).unwrap().trim(), "[]");
    assert!(!p.path("config/integrations.json").exists());
    assert!(!p.path("data/integrations").exists(), "the emptied integrations directory is KnowCode's");
    assert!(!p.path("cache/updates").exists(), "the update check's lock and directory are KnowCode's");
}

#[test]
fn claude_marketplace_still_listed_after_removal_is_reported_and_retried() {
    let c = Claude::new("stuck");
    let p = &c.p;
    let out = c.command(&["uninstall", "--delete-config"]).env("KNOWCODE_TEST_STUCK", "1").output().unwrap();
    assert!(!out.status.success(), "a registration Claude Code still lists is a failure");
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!stdout.contains("removed Claude registration"), "{stdout}");
    assert!(stderr.contains("still lists the knowcode-local marketplace"), "{stderr}");
    let ledger = c.ledger();
    assert_eq!(ledger.len(), 1, "the registration stays for a rerun");
    assert_eq!(ledger[0]["client"], "claude");
    assert!(p.path("data/integrations/claude-marketplace/knowcode/.mcp.json").exists(), "the bundle the marketplace points at stays");
    assert!(p.path("config/integrations.json").exists());
    success(c.command(&["uninstall", "--delete-config"]).output().unwrap());
    assert!(!p.path("config/integrations.json").exists());
    assert!(!p.path("data/integrations").exists());
}

#[test]
fn shell_installer_removal_steps_name_the_binary_receipt_and_profile_lines() {
    let p = Profile::new("installer");
    let home = p.path("home");
    let prefix = home.join(".local/bin");
    fs::create_dir_all(&prefix).unwrap();
    let installed = prefix.join("knowcode");
    if fs::hard_link(KC, &installed).is_err() { fs::copy(KC, &installed).unwrap(); }
    let receipt = home.join(".config/knowcode");
    p.write("home/.config/knowcode/knowcode-receipt.json", &json!({"provider": {"source": "cargo-dist"},
        "source": {"owner": "bubstackdev", "name": "knowcode", "app_name": "knowcode"}, "install_layout": "flat",
        "install_prefix": prefix}).to_string());
    p.write("home/.config/knowcode/env.sh", "#!/bin/sh\n");
    p.write("home/.config/knowcode/env.fish", "\n");
    let line = ". \"$HOME/.config/knowcode/env.sh\"";
    let profile = format!("# mine\nexport EDITOR=vi\n\n{line}\n");
    p.write("home/.profile", &profile);
    p.write("home/.bashrc", &format!("alias ll='ls -l'\n\n{line}\n"));
    p.write("home/.zshrc", "# nothing of KnowCode's\n");
    p.write("home/.config/fish/conf.d/knowcode.env.fish", "\nsource \"$HOME/.config/knowcode/env.fish\"\n");
    let out = Command::new(&installed).args(["uninstall"]).current_dir(&p.root).stdin(Stdio::null())
        .env("KNOWCODE_CONFIG_DIR", p.path("config")).env("KNOWCODE_CACHE_DIR", p.path("cache")).env("KNOWCODE_DATA_DIR", p.path("data"))
        .env("HOME", &home).env("XDG_CONFIG_HOME", home.join(".config")).env_remove("ZDOTDIR").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("installed with the shell installer"), "{stdout}");
    assert!(stdout.contains(&format!("{}:4: {line}", home.join(".profile").display())), "{stdout}");
    assert!(stdout.contains(&format!("{}:3: {line}", home.join(".bashrc").display())), "{stdout}");
    assert!(!stdout.contains(".zshrc"), "{stdout}");
    assert!(stdout.contains(&format!("rm {} {}", home.join(".config/fish/conf.d/knowcode.env.fish").display(), installed.display())), "{stdout}");
    assert!(stdout.contains(&format!("rm {r}/knowcode-receipt.json {r}/env.sh {r}/env.fish && rmdir {r}", r = receipt.display())), "{stdout}");
    let (lines, rm) = (stdout.find(".profile:4").unwrap(), stdout.find(&format!("rm {}", home.join(".config/fish").display())).unwrap());
    assert!(lines < rm, "the profile lines go before env.sh: {stdout}");
    assert!(!stdout.contains("package manager"), "{stdout}");
    assert_eq!(fs::read_to_string(home.join(".profile")).unwrap(), profile, "profiles are only read");
    assert!(installed.exists() && receipt.join("env.sh").exists(), "the steps are printed, not run");
}
