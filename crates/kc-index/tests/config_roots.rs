//! The config file, its migration from the legacy files, and workspace roots: which repos are found under a root,
//! and which indexed repos the workspace (what cross-repo answers draw on) holds with and without roots.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use kc_index::config::{self, Source};
use kc_index::{SyncOptions, roots, workspace};

/// The config and cache directories are process-wide, so the tests here take turns.
static LOCK: Mutex<()> = Mutex::new(());

fn base() -> &'static Path {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("knowcode-test-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("config")).unwrap();
        let base = std::fs::canonicalize(&base).unwrap();
        // SAFETY: set once, before any test in this binary reads them.
        unsafe {
            std::env::set_var("KNOWCODE_CACHE_DIR", base.join("cache"));
            std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config"));
            std::env::set_var("KNOWCODE_DATA_DIR", base.join("data"));
        }
        base
    })
}

/// Takes the lock and starts from an empty config directory.
fn fresh() -> (MutexGuard<'static, ()>, PathBuf) {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = base().join("config");
    for entry in std::fs::read_dir(&dir).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    (guard, dir)
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@example.com"]).args(args).status().unwrap().success();
    assert!(ok, "git {args:?} in {}", dir.display());
}

/// A git repo at `dir` with one commit holding `files`.
fn repo(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    for (path, text) in files {
        std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
        std::fs::write(dir.join(path), text).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    std::fs::canonicalize(dir).unwrap()
}

fn index(root: &Path) {
    kc_index::sync(root, &mut kc_index::open(root).unwrap(), &SyncOptions { threads: 1, full_discovery: true, ..Default::default() }).unwrap();
}

#[test]
fn legacy_files_keep_working_until_migrated_into_config_toml() {
    let (_guard, dir) = fresh();
    std::fs::write(dir.join("cache_dir"), "# moved\n~/elsewhere/kc\n").unwrap();
    std::fs::write(dir.join("ignore"), "# mine\napp records/\n* Generated\nlonely\napp docs/old\n").unwrap();

    let (rules, source) = config::user_ignore().unwrap();
    assert_eq!(source, Source::Legacy);
    let legacy_rules = rules.clone();
    assert_eq!(rules.len(), 3);
    let legacy = config::resolve_cache_dir(None, &config::load().unwrap().config, || std::fs::read_to_string(dir.join("cache_dir")).ok(), PathBuf::new);
    assert_eq!(legacy, (dirs::home_dir().unwrap().join("elsewhere/kc"), Source::Legacy));
    let stamp_before = config::user_ignore_stamp();

    let done = config::migrate().unwrap();
    assert!(done.iter().any(|l| l.contains("cache.dir")), "{done:?}");
    assert!(dir.join("cache_dir.migrated").exists() && !dir.join("cache_dir").exists());
    assert!(dir.join("ignore.migrated").exists() && !dir.join("ignore").exists());
    let text = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains("dir = \"~/elsewhere/kc\""), "{text}");

    let loaded = config::load().unwrap();
    assert!(loaded.exists);
    assert_eq!(loaded.config.cache_dir, Some(dirs::home_dir().unwrap().join("elsewhere/kc")));
    let (rules, source) = config::user_ignore().unwrap();
    assert_eq!(source, Source::Config);
    let (mut a, mut b) = (legacy_rules, rules);
    a.sort();
    b.sort();
    assert_eq!(a, b, "the same rules, now from config.toml");
    assert_ne!(config::user_ignore_stamp(), stamp_before, "a change of source rediscovers files");
    // The variable still wins over the file.
    assert_eq!(config::cache_dir().unwrap(), (base().join("cache"), Source::Env));

    assert_eq!(config::migrate().unwrap(), vec!["nothing to migrate".to_string()]);
}

#[test]
fn migration_keeps_what_config_toml_already_sets() {
    let (_guard, dir) = fresh();
    std::fs::write(dir.join("config.toml"), "# keep me\n[cache]\ndir = \"/kept\"\n\n[ignore]\n\"*\" = [\"Fixtures\"]\n").unwrap();
    std::fs::write(dir.join("cache_dir"), "/legacy\n").unwrap();
    std::fs::write(dir.join("ignore"), "* Generated\n").unwrap();
    // While config.toml sets the keys, the legacy files are not read.
    assert_eq!(config::user_ignore().unwrap(), (vec![("*".to_string(), "Fixtures".to_string())], Source::Config));
    config::migrate().unwrap();
    let text = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.starts_with("# keep me\n"), "comments survive an edit: {text}");
    assert!(text.contains("\"/kept\"") && !text.contains("legacy") && !text.contains("Generated"), "{text}");
    assert!(dir.join("ignore.migrated").exists());
}

#[test]
fn set_and_unset_edit_config_toml_in_place_and_refuse_bad_values() {
    let (_guard, dir) = fresh();
    std::fs::write(dir.join("config.toml"), "# my settings\n[workspace]\nmax_depth = 2 # shallow\n").unwrap();
    config::set("workspace.roots", "~/Code").unwrap();
    config::set("ignore.*", "[\"Generated\", \"docs/\"]").unwrap();
    let loaded = config::load().unwrap();
    assert_eq!(loaded.config.roots, vec![dirs::home_dir().unwrap().join("Code")]);
    assert_eq!(loaded.config.max_depth, Some(2));
    assert_eq!(loaded.config.ignore, Some(vec![("*".to_string(), vec!["Generated".to_string(), "docs/".to_string()])]));
    let text = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(text.contains("# my settings") && text.contains("# shallow"), "{text}");

    let err = format!("{:#}", config::set("workspace.max_depth", "deep").unwrap_err());
    assert!(err.ends_with("`workspace.max_depth` must be a whole number from 0 to 32"), "{err}");
    assert!(config::set("cache.size", "1").is_err(), "unknown keys are refused by set");
    assert_eq!(config::load().unwrap().config.max_depth, Some(2), "a refused value is not written");

    assert!(config::unset("workspace.max_depth").unwrap());
    assert!(config::unset("workspace.roots").unwrap());
    assert!(!config::unset("workspace.roots").unwrap());
    let text = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(!text.contains("[workspace]"), "an emptied table goes: {text}");
}

#[test]
fn roots_find_checkouts_and_nested_worktrees_within_the_depth_skipping_dependencies() {
    let (_guard, _) = fresh();
    let ws = base().join("discover");
    let app = repo(&ws.join("app"), &[("a.py", "x = 1\n")]);
    let deep = repo(&ws.join("group/sub/deep"), &[]);
    repo(&ws.join("node_modules/dep"), &[]);
    repo(&ws.join(".cache/hidden"), &[]);
    let vendor = repo(&ws.join("vendor"), &[]);
    // A repo inside another checkout (not a worktree of it) is the outer repo's business.
    repo(&app.join("third/inner"), &[]);
    // Worktrees: one nested in the checkout, one elsewhere under the root, one outside the root.
    git(&app, &["worktree", "add", "-q", ".claude/worktrees/feature", "-b", "feature"]);
    git(&app, &["worktree", "add", "-q", ws.join("side").to_str().unwrap(), "-b", "side"]);
    git(&app, &["worktree", "add", "-q", base().join("outside-wt").to_str().unwrap(), "-b", "outside"]);
    let nested = std::fs::canonicalize(app.join(".claude/worktrees/feature")).unwrap();
    let side = std::fs::canonicalize(ws.join("side")).unwrap();

    assert_eq!(roots::discover(&ws, 4), vec![app.clone(), nested.clone(), deep.clone(), side.clone(), vendor.clone()]);
    assert_eq!(roots::discover(&ws, 2), vec![app.clone(), nested.clone(), side.clone(), vendor.clone()], "deep is 3 levels down");
    assert_eq!(roots::discover(&app, 4), vec![app.clone(), nested.clone()], "a root that is a repo");
    assert!(roots::discover(&ws.join("missing"), 4).is_empty());

    // Testing a repo against the roots without walking them agrees with the walk, for every repo here.
    let every: Vec<PathBuf> = [
        &app, &nested, &deep, &side, &vendor,
        &ws.join("node_modules/dep"), &ws.join(".cache/hidden"), &app.join("third/inner"), &base().join("outside-wt"),
    ]
    .iter()
    .map(|p| std::fs::canonicalize(p).unwrap())
    .collect();
    for (root, depth) in [(&ws, 4), (&ws, 2), (&ws, 1), (&app, 4), (&ws.join("group"), 4)] {
        let walked = roots::discover(root, depth);
        let mut r = roots::Roots::new(std::slice::from_ref(root), depth);
        let tested: Vec<PathBuf> = every.iter().filter(|p| r.contains(p)).cloned().collect();
        let mut tested = tested;
        tested.sort();
        assert_eq!(tested, walked, "root {} depth {depth}", root.display());
    }
}

#[test]
fn the_workspace_is_every_indexed_repo_without_roots_and_the_roots_repos_plus_the_queried_one_with_them() {
    let (_guard, _) = fresh();
    let b = base().join("members");
    let lib = repo(&b.join("ws/lib"), &[("package.json", r#"{ "name": "@acme/lib", "version": "1.0.0" }"#), ("src/index.ts", "export function f() {}\n")]);
    let web = repo(&b.join("ws/web"), &[("package.json", r#"{ "name": "web" }"#)]);
    let out = repo(&b.join("elsewhere/out"), &[("package.json", r#"{ "name": "out", "dependencies": { "@acme/lib": "1.0.0" } }"#)]);
    for r in [&lib, &web, &out] {
        index(r);
    }
    let ours = |v: Vec<PathBuf>| v.into_iter().filter(|r| r.starts_with(&b)).collect::<Vec<_>>();
    let roots_of = |s: &[workspace::RepoSummary]| ours(s.iter().map(|s| s.root.clone()).collect());

    // No roots: every indexed repo, as before roots existed.
    let all = workspace::summaries(&[]).unwrap();
    assert_eq!(roots_of(&all), vec![out.clone(), lib.clone(), web.clone()]);
    let dependents = workspace::neighbours(&lib, &all).1.iter().map(|s| s.root.clone()).collect::<Vec<_>>();
    assert_eq!(dependents, vec![out.clone()]);

    // Roots: the indexed repos under them.
    config::add_root(&b.join("ws")).unwrap();
    let ws = workspace::summaries(&[]).unwrap();
    assert_eq!(roots_of(&ws), vec![lib.clone(), web.clone()]);
    assert!(workspace::neighbours(&lib, &ws).1.is_empty(), "a repo outside the roots is not a dependent");
    assert_eq!(ours(kc_index::indexed_roots().unwrap()), vec![lib.clone(), web.clone()]);
    assert_eq!(ours(kc_index::all_indexed_roots().unwrap()), vec![out.clone(), lib.clone(), web.clone()]);

    // A query about a repo outside every root answers for it and links it to the roots' repos.
    let focused = workspace::summaries(std::slice::from_ref(&out)).unwrap();
    assert_eq!(roots_of(&focused), vec![out.clone(), lib.clone(), web.clone()]);
    assert_eq!(workspace::neighbours(&out, &focused).0.iter().map(|s| s.root.clone()).collect::<Vec<_>>(), vec![lib.clone()]);
    assert_eq!(roots_of(&workspace::summaries(&[]).unwrap()), vec![lib.clone(), web.clone()], "the focus is per call");
    // A focus already under a root changes nothing.
    assert_eq!(roots_of(&workspace::summaries(std::slice::from_ref(&web)).unwrap()), vec![lib.clone(), web.clone()]);

    // Removing the root brings back every indexed repo.
    assert_eq!(config::remove_root(&b.join("ws")).unwrap().len(), 1);
    assert_eq!(roots_of(&workspace::summaries(&[]).unwrap()), vec![out, lib, web]);
}
