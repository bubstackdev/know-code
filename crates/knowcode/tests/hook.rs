//! `knowcode hook claude-code` and `knowcode hook session-start` as Claude Code runs them: a PreToolUse or
//! SessionStart event on stdin, a note or nothing on stdout, and exit 0 whatever happens. Bash commands get a note only when they search code for a symbol the index knows.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const KC: &str = env!("CARGO_BIN_EXE_knowcode");

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("knowcode-test-hook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Fixture { dir: std::fs::canonicalize(&dir).unwrap() }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(KC);
        cmd.env("KNOWCODE_CACHE_DIR", self.dir.join("cache"))
            .env("KNOWCODE_DATA_DIR", self.dir.join("data"))
            .env("KNOWCODE_CONFIG_DIR", self.dir.join("config"))
            .env("KNOWCODE_UPDATE_CHECK", "0")
            .env_remove("KNOWCODE_TOOLS");
        cmd
    }

    /// The hook's stdout for `input`, after checking it exited 0.
    fn hook(&self, input: &str) -> String {
        let mut child = self.command().args(["hook", "claude-code"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "the hook always exits 0");
        String::from_utf8(out.stdout).unwrap()
    }

    fn bash(&self, cwd: &Path, command: &str) -> String {
        let event = json!({"session_id": "t", "hook_event_name": "PreToolUse", "cwd": cwd, "tool_name": "Bash",
            "tool_input": {"command": command, "description": "Search the code"}, "tool_use_id": "toolu_test"});
        self.hook(&event.to_string())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn bash_searches_get_a_note_and_other_commands_nothing() {
    let f = Fixture::new("bash");
    let repo = f.dir.join("shop");
    for (path, text) in [
        ("src/Catalog.API/Catalog.API.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>\n"),
        (
            "src/Catalog.API/Apis/CatalogApi.cs",
            "namespace Catalog.API;\npublic static class CatalogApi\n{\n    public static int GetItemById(int id) { return id; }\n}\n",
        ),
        (
            "src/Catalog.API/Apis/Items.cs",
            "namespace Catalog.API;\npublic class Items\n{\n    public int One() { return CatalogApi.GetItemById(1); }\n}\n",
        ),
    ] {
        let p = repo.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    assert!(Command::new("git").arg("init").arg("-q").arg(&repo).status().unwrap().success());
    let out = f.command().arg("index").arg(&repo).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = f.bash(&repo, "grep -rn \"GetItemById\" --include=*.cs .");
    let v: Value = serde_json::from_str(&out).expect("one JSON answer");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let note = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(note.contains("`GetItemById`") && note.contains("src/Catalog.API/Apis/CatalogApi.cs:4"), "{note}");
    // The default profile, `one`: the calls as ops of the one tool the plugin loads.
    assert!(note.contains("kc {op: context, symbol: \"GetItemById\"}") && note.contains("(mcp__plugin_knowcode_knowcode__kc)"), "{note}");
    assert!(!note.contains("kc_"), "{note}");
    assert!(!f.bash(&f.dir, "cd shop && grep -rn \"CatalogApi\" .").is_empty(), "a `cd` chain searches where it went");

    for command in ["cargo build", "git status", "sed -n 1,20p src/Catalog.API/Apis/CatalogApi.cs", "cat x | grep -c foo"] {
        assert_eq!(f.bash(&repo, command), "", "{command}");
    }
    assert_eq!(f.hook("{\"tool_name\": \"Bash\", \"tool_input\": "), "", "malformed JSON");
    assert_eq!(f.hook(""), "", "no input");
}

/// The hook script decides first, in the shell: a call that can't be a search exits without starting the binary.
#[test]
fn the_hook_script_starts_the_binary_only_for_possible_searches() {
    let f = Fixture::new("script");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/claude-code/hooks/knowcode-grep-hint.sh");
    let called = f.dir.join("called");
    let recorder = f.dir.join("recorder");
    std::fs::write(&recorder, format!("#!/bin/sh\ncat >/dev/null\ntouch '{}'\n", called.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let starts = |input: &str| -> bool {
        let _ = std::fs::remove_file(&called);
        let mut child = Command::new("sh").arg(&script).env("KNOWCODE_BIN", &recorder)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success() && out.stdout.is_empty());
        called.exists()
    };
    let tool = |name: &str| json!({"tool_name": name, "tool_input": {"pattern": "CatalogApi"}}).to_string();
    let bash = |command: &str| json!({"cwd": "/work/grep-tools", "tool_name": "Bash", "tool_input": {"command": command}}).to_string();
    assert!(starts(&tool("Grep")) && starts(&tool("Glob")));
    for command in ["grep -rn CatalogApi .", "cat x | egrep Items", "fgrep -r Items src", "git grep -n Items", "cd src && rg Items",
        "ag Items", "ack Items", "find . -name 'Items*'", "find . -iname 'items*'", "find . -path '*/Apis/*'",
        "find . -wholename '*/Items.cs'", "cd src\nrg Items"] {
        assert!(starts(&bash(command)), "{command}");
    }
    for command in ["cargo build", "git status", "sed -n 1,20p README.md", "find . -type f", "cargo fmt && npm run tag-release"] {
        assert!(!starts(&bash(command)), "{command}");
    }
    assert!(!starts(&tool("Read")));
    assert!(!starts(""));
}

/// A small indexed repo for the session-start tests: `name` under the fixture, one C# class.
fn indexed_repo(f: &Fixture, name: &str) -> PathBuf {
    let repo = plain_repo(f, name);
    let out = f.command().arg("index").arg(&repo).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    repo
}

fn plain_repo(f: &Fixture, name: &str) -> PathBuf {
    let repo = f.dir.join(name);
    for (path, text) in [
        ("src/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("src/CartStore.cs", "namespace App;\npublic class CartStore\n{\n    public int Save() { return 1; }\n}\n"),
    ] {
        let p = repo.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    assert!(Command::new("git").arg("init").arg("-q").arg(&repo).status().unwrap().success());
    repo
}

impl Fixture {
    /// What `knowcode hook session-start` adds for a session starting in `cwd` (empty: nothing).
    fn session(&self, cwd: &Path, source: &str) -> String {
        let event = json!({"session_id": "t", "hook_event_name": "SessionStart", "source": source, "cwd": cwd, "transcript_path": "/dev/null"});
        let mut child = self.command().args(["hook", "session-start"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(event.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "the hook always exits 0");
        let out = String::from_utf8(out.stdout).unwrap();
        if out.is_empty() {
            return out;
        }
        let v: Value = serde_json::from_str(&out).expect("one JSON answer");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string()
    }
}

#[test]
fn session_start_says_what_is_indexed_only_where_there_is_an_index() {
    let f = Fixture::new("session");
    // No index anywhere: nothing, and nothing is created.
    let first = plain_repo(&f, "orders");
    assert_eq!(f.session(&first, "startup"), "");
    assert!(!f.dir.join("cache").exists(), "the hook never creates the cache");

    let shop = indexed_repo(&f, "shop");
    let note = f.session(&shop.join("src"), "startup");
    assert_eq!(note, "KnowCode indexes shop. Callers, impact, HTTP: kc {op: impact}; definitions: kc {op: find}.");
    assert_eq!(f.session(&shop, "clear"), note);
    assert_eq!(f.session(&shop, "compact"), note);
    // A resumed or forked session has the line in its transcript already.
    assert_eq!(f.session(&shop, "resume"), "");
    assert_eq!(f.session(&shop, "fork"), "");

    // Under the separate tools' profiles, their names.
    let mut cmd = f.command();
    cmd.env("KNOWCODE_TOOLS", "core").args(["hook", "session-start"]).stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let event = json!({"session_id": "t", "hook_event_name": "SessionStart", "source": "startup", "cwd": shop, "transcript_path": "/dev/null"});
    child.stdin.take().unwrap().write_all(event.to_string().as_bytes()).unwrap();
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    assert!(out.contains("KnowCode indexes shop. Callers, impact, HTTP: kc_impact; definitions: kc_find."), "{out}");

    let api = indexed_repo(&f, "api");
    assert_eq!(f.session(&api, "startup"), "KnowCode indexes api. Callers, impact, HTTP: kc {op: impact}; definitions: kc {op: find}.");
    // A repo with no index says nothing, or, under a workspace root, that its first query indexes it.
    assert_eq!(f.session(&first, "startup"), "");
    assert_eq!(f.session(&f.dir.join("nowhere"), "startup"), "");
    let out = f.command().args(["workspace", "add"]).arg(&f.dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let note = f.session(&first, "startup");
    assert!(note.starts_with("KnowCode indexes orders on first use. Callers"), "{note}");
    let note = f.session(&f.dir, "startup");
    assert!(note.starts_with("KnowCode indexes 2 repos here. Callers"), "a workspace root that isn't a repo: {note}");

    // A privacy file that blocks the repo: nothing.
    std::fs::write(api.join(".knowcode-privacy.yml"), "level: 1\n").unwrap();
    assert_eq!(f.session(&api, "startup"), "");
    assert_ne!(f.session(&shop, "startup"), "");

    // Turned off in the config: nothing anywhere.
    let out = f.command().args(["config", "set", "hooks.session_start", "false"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(f.session(&shop, "startup"), "");
    let out = f.command().args(["config", "get", "hooks.session_start"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
}

/// Both bundled hook scripts, as Claude Code runs them from the plugin: the SessionStart line and the PreToolUse note
/// work side by side on the same index, and say nothing when they have nothing to say.
#[test]
fn the_bundled_scripts_run_both_hooks() {
    let f = Fixture::new("scripts");
    let shop = indexed_repo(&f, "shop");
    let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/claude-code/hooks");
    let run = |script: &str, event: Value| -> String {
        let mut child = Command::new("sh").arg(hooks.join(script)).env("KNOWCODE_BIN", KC)
            .env("KNOWCODE_CACHE_DIR", f.dir.join("cache")).env("KNOWCODE_DATA_DIR", f.dir.join("data"))
            .env("KNOWCODE_CONFIG_DIR", f.dir.join("config")).env("KNOWCODE_UPDATE_CHECK", "0")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(event.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success() && out.stderr.is_empty(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    let start = run("knowcode-session-start.sh", json!({"hook_event_name": "SessionStart", "source": "startup", "cwd": shop}));
    let v: Value = serde_json::from_str(&start).unwrap();
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().starts_with("KnowCode indexes shop."), "{start}");
    let grep = run("knowcode-grep-hint.sh", json!({"hook_event_name": "PreToolUse", "cwd": shop, "tool_name": "Grep", "tool_input": {"pattern": "CartStore"}}));
    let v: Value = serde_json::from_str(&grep).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("`CartStore` is a class"), "{grep}");
    assert_eq!(run("knowcode-session-start.sh", json!({"hook_event_name": "SessionStart", "source": "resume", "cwd": shop})), "");
    assert_eq!(run("knowcode-session-start.sh", json!({"hook_event_name": "SessionStart", "source": "startup", "cwd": f.dir})), "");
}
