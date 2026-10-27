//! Discovery remains useful without persistent indexes and never upgrades partial evidence into graph facts.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use kc_index::{config, light, workspace};

static TURN: Mutex<()> = Mutex::new(());
struct Fixture { base: &'static Path, _turn: MutexGuard<'static, ()> }
impl Fixture {
    fn new() -> Self {
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        static BASE: OnceLock<PathBuf> = OnceLock::new();
        let base = BASE.get_or_init(|| {
            let base = std::env::temp_dir().join(format!("knowcode-test-light-{}", std::process::id()));
            // Set once; all filesystem fixtures and in-process API calls take TURN.
            unsafe {
                std::env::set_var("KNOWCODE_CACHE_DIR", base.join("cache"));
                std::env::set_var("KNOWCODE_CONFIG_DIR", base.join("config"));
                std::env::set_var("KNOWCODE_DATA_DIR", base.join("data"));
            }
            base
        });
        let _ = fs::remove_dir_all(base);
        fs::create_dir_all(base.join("repos")).unwrap();
        config::add_root(&base.join("repos")).unwrap();
        Self { base, _turn: turn }
    }
    fn repo(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = self.base.join("repos").join(name);
        fs::create_dir_all(&root).unwrap();
        assert!(Command::new("git").args(["init", "-q"]).arg(&root).status().unwrap().success());
        for (name, text) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        kc_index::repo_root(&root).unwrap()
    }
}

#[test]
fn invariant_light_discovery_records_manifests_and_names_without_creating_indexes() {
    let f = Fixture::new();
    let lib = f.repo("lib", &[("package.json", "{\"name\":\"qzx-light-lib\",\"version\":\"1.0.0\"}"), ("api.ts", "export function sharedHandler() { return 42; }\n")]);
    let app = f.repo("app", &[("package.json", "{\"name\":\"qzx-light-app\",\"dependencies\":{\"qzx-light-lib\":\"file:../lib\"}}"), ("app.ts", "import { sharedHandler } from 'qzx-light-lib';\nexport const result = sharedHandler();\n")]);
    assert_eq!(light::discover_roots().unwrap(), 2);
    let before = workspace::summaries(&[]).unwrap();
    assert!(before.iter().all(|s| s.light && s.may_define("unknownBeforeScan")));
    light::scan(&lib).unwrap(); light::scan(&app).unwrap();
    assert!(light::pending().unwrap().is_empty());
    let summaries = workspace::summaries(&[]).unwrap();
    let l = summaries.iter().find(|s| s.root == lib).unwrap();
    let a = summaries.iter().find(|s| s.root == app).unwrap();
    assert!(l.light && l.may_define("sharedHandler"));
    assert!(a.light && a.may_reference("sharedHandler"));
    assert!(!l.may_define("qzxEntirelyAbsentDiscoveredName"));
    assert_eq!(l.packages[0].name, "qzx-light-lib");
    let (dependencies, _) = workspace::neighbours(&app, &summaries);
    assert!(dependencies.iter().any(|s| s.root == lib));
    assert!(kc_index::indexed_roots().unwrap().contains(&app));
    assert!(!kc_index::db_path(&lib).unwrap().exists());
    assert!(!kc_index::db_path(&app).unwrap().exists());
}

#[test]
fn invariant_a_light_negative_is_trusted_only_while_the_tree_matches_its_scan() {
    let f = Fixture::new();
    let root = f.repo("app", &[("api.ts", "export function oldHandler() { return 42; }\n")]);
    light::discover_roots().unwrap(); light::scan(&root).unwrap();
    assert!(light::unchanged(&root));
    assert!(!kc_index::changed_repos(std::slice::from_ref(&root)).contains(&root));
    fs::write(root.join("api.ts"), "export function newlyAddedHandler() { return 4242; }\n").unwrap();
    assert!(!light::unchanged(&root));
    assert!(kc_index::changed_repos(std::slice::from_ref(&root)).contains(&root));
    let mut store = kc_index::open(&root).unwrap();
    kc_index::sync(&root, &mut store, &kc_index::SyncOptions { threads: 1, ..Default::default() }).unwrap();
    let summaries = workspace::summaries(&[]).unwrap();
    assert!(!summaries.iter().find(|s| s.root == root).unwrap().light);
    assert!(!fs::read_to_string(light::path().unwrap()).unwrap().contains("oldHandler"));
    assert!(!kc_index::changed_repos(std::slice::from_ref(&root)).contains(&root));
}

#[test]
fn invariant_privacy_changes_purge_light_metadata_without_creating_an_index() {
    let f = Fixture::new();
    let root = f.repo("app", &[("package.json", "{\"name\":\"qzx-pre-policy-package\"}"), ("api.ts", "export function qzxPrePolicyHandler() { return 42; }\n")]);
    light::discover_roots().unwrap(); light::scan(&root).unwrap();
    assert!(fs::read_to_string(light::path().unwrap()).unwrap().contains("qzx-pre-policy-package"));
    fs::write(root.join(".knowcode-privacy.yml"), "level: 1\n").unwrap();
    let summaries = workspace::summaries(&[]).unwrap();
    let summary = summaries.iter().find(|s| s.root == root).unwrap();
    assert!(summary.blocked.whole && summary.packages.is_empty());
    assert!(summary.modules.is_empty() && summary.routes.is_empty());
    assert!(!fs::read_to_string(light::path().unwrap()).unwrap().contains("qzx-pre-policy-package"));
    assert!(!kc_index::db_path(&root).unwrap().exists());
}

#[test]
fn invariant_discovery_yields_to_queries_and_cleanup_preserves_unrelated_files() {
    let f = Fixture::new();
    let root = f.repo("app", &[("api.ts", "export function qzxHandler() { return 42; }\n")]);
    light::discover_roots().unwrap();
    assert!(!light::scan_until(&root, || true).unwrap());
    assert!(light::pending().unwrap().contains(&root));
    fs::write(f.base.join("cache/unrelated"), "keep").unwrap();
    fs::remove_dir_all(&root).unwrap();
    kc_index::cache::prune().unwrap();
    assert!(workspace::summaries(&[]).unwrap().is_empty());
    kc_index::cache::purge().unwrap();
    assert!(!light::path().unwrap().exists());
    assert_eq!(fs::read_to_string(f.base.join("cache/unrelated")).unwrap(), "keep");
}

#[test]
fn invariant_a_new_nested_checkout_is_discovered_after_the_inventory_was_cached() {
    let f = Fixture::new();
    fs::create_dir_all(f.base.join("repos/group")).unwrap();
    assert!(workspace::summaries(&[]).unwrap().is_empty());
    let root = f.repo("group/new-checkout", &[("api.ts", "export function newHandler() { return 42; }\n")]);
    let summaries = workspace::summaries(&[]).unwrap();
    assert!(summaries.iter().any(|s| s.root == root && s.light));
    assert!(!kc_index::db_path(&root).unwrap().exists());
}
