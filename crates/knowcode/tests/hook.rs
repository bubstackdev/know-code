//! `knowcode hook claude-code` as Claude Code runs it: a PreToolUse event on stdin, a note or nothing on stdout, and
//! exit 0 whatever happens. Bash commands get a note only when they search code for a symbol the index knows.

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
            .env("KNOWCODE_UPDATE_CHECK", "0");
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
    assert!(note.contains("kc_context(symbol: \"GetItemById\")") && note.contains("mcp__plugin_knowcode_knowcode__kc_context"), "{note}");
    assert!(!f.bash(&f.dir, "cd shop && grep -rn \"CatalogApi\" .").is_empty(), "a `cd` chain searches where it went");

    for command in ["cargo build", "git status", "sed -n 1,20p src/Catalog.API/Apis/CatalogApi.cs", "cat x | grep -c foo"] {
        assert_eq!(f.bash(&repo, command), "", "{command}");
    }
    assert_eq!(f.hook("{\"tool_name\": \"Bash\", \"tool_input\": "), "", "malformed JSON");
    assert_eq!(f.hook(""), "", "no input");
}
