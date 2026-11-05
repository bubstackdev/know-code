//! Every test process runs in a sandbox (scripts/test-runner, cargo's runner in .cargo/config.toml): no test may
//! resolve the developer's real config, cache or data directories, read their config.toml (and so index their
//! workspace roots), or touch their index cache and its locks. This fails if the runner isn't in effect, or stops
//! covering a directory or file KnowCode resolves.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The account's home from the user database, not from `$HOME` (which the sandbox replaces): sh expands `~user`
/// from the user database, but only unquoted, hence the `eval`.
fn real_home() -> PathBuf {
    let out = Command::new("sh").args(["-c", r#"eval "cd ~$(id -un)" && pwd -P"#]).output().expect("sh");
    assert!(out.status.success(), "can't find the account's home: {}", String::from_utf8_lossy(&out.stderr));
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
}

/// Where the platform keeps per-user config, cache and data under the real home.
fn real_user_dirs(home: &Path) -> Vec<PathBuf> {
    ["Library/Application Support", "Library/Caches", ".config", ".cache", ".local/share", ".local/state"]
        .iter()
        .map(|d| home.join(d))
        .collect()
}

#[test]
fn invariant_tests_cannot_resolve_the_real_user_directories() {
    let sandbox = std::env::var_os("KNOWCODE_TEST_SANDBOX")
        .map(PathBuf::from)
        .expect("KNOWCODE_TEST_SANDBOX unset: tests aren't running through scripts/test-runner (.cargo/config.toml)");
    let home = real_home();
    let real = real_user_dirs(&home);
    let resolved = [
        ("HOME", std::env::var_os("HOME").map(PathBuf::from)),
        ("dirs::config_dir", dirs::config_dir()),
        ("dirs::cache_dir", dirs::cache_dir()),
        ("dirs::data_dir", dirs::data_dir()),
        ("kc_index::config_dir", kc_index::config_dir().ok()),
        ("kc_index::cache_dir", kc_index::cache_dir().ok()),
        ("kc_index::data_dir", kc_index::data_dir().ok()),
    ];
    for (what, path) in resolved {
        let path = path.unwrap_or_else(|| panic!("{what} resolved nothing"));
        assert!(path.starts_with(&sandbox), "{what} is {} outside the sandbox {}", path.display(), sandbox.display());
        assert!(path != home, "{what} is the real home {}", home.display());
        for dir in &real {
            assert!(!path.starts_with(dir), "{what} is {} under the real {}", path.display(), dir.display());
        }
    }
    for var in ["KNOWCODE_CACHE_DIR", "KNOWCODE_CONFIG_DIR", "KNOWCODE_DATA_DIR"] {
        assert!(std::env::var_os(var).is_none(), "{var} leaked into the test process");
    }
    // The config file is what does the damage when it leaks: the user's `[workspace] roots` would send a test (or an
    // agent's run) indexing their real repositories. The one KnowCode would load must be the sandbox's, which
    // doesn't exist, so it holds no roots.
    let path = kc_index::config::path().expect("config path");
    assert!(path.starts_with(&sandbox), "config.toml would be read from {} outside the sandbox", path.display());
    let loaded = kc_index::config::load().expect("load config");
    assert!(loaded.path.starts_with(&sandbox), "config.toml was loaded from {}", loaded.path.display());
    assert!(!loaded.exists, "a config.toml exists in the fresh sandbox at {}", loaded.path.display());
    assert!(loaded.config.roots.is_empty(), "workspace roots leaked into the test process: {:?}", loaded.config.roots);
}
