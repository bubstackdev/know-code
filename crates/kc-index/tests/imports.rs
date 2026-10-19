//! Names resolve through the module their import names, placed as the compiler or interpreter would: TypeScript by
//! tsconfig `paths` and `baseUrl` (never by a file name that only looks right), Python by its module path.

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

fn repo(name: &str, files: &[(&str, &str)]) -> (PathBuf, Store) {
    let dir = std::env::temp_dir().join(format!("knowcode-test-imports-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success());
    for (path, text) in files {
        write(&dir, path, text);
    }
    let mut store = Store::open(&dir.join(".git/knowcode-test.sqlite")).unwrap();
    sync(&dir, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    (dir, store)
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// The call of `name` in `file`: its confidence, its targets' files, and the basis given.
fn call(store: &Store, name: &str, file: &str) -> (&'static str, Vec<String>, Option<&'static str>) {
    let (hits, _) = query::refs(store.conn(), name, &["call"], 50).unwrap();
    let h = hits.into_iter().find(|h| h.file == file).unwrap_or_else(|| panic!("no call of {name} in {file}"));
    (h.confidence, h.targets.iter().map(|t| t.file.clone()).collect(), h.basis)
}

const SAVE: &str = "export function save() {}\n";

#[test]
fn tsconfig_paths_place_an_alias_over_a_file_named_like_it() {
    let (root, store) = repo(
        "alias",
        &[
            ("tsconfig.json", r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@alias/save": ["actual/worker.ts"] } } }"#),
            ("actual/worker.ts", SAVE),
            ("wrong/save.ts", SAVE),
            ("main.ts", "import { save } from '@alias/save';\nexport function run() { save(); }\n"),
        ],
    );
    assert_eq!(call(&store, "save", "main.ts"), ("exact", vec!["actual/worker.ts".to_string()], None));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tsconfig_patterns_substitute_try_fallbacks_and_follow_extends() {
    let (root, store) = repo(
        "patterns",
        &[
            // The base sets `paths` relative to its own `baseUrl`; the project overrides `baseUrl`, which `paths`
            // then follows, as the compiler does.
            (
                "tsconfig.base.json",
                r#"{
                  // comments and trailing commas, as tsconfig allows
                  "compilerOptions": {
                    "baseUrl": "./elsewhere",
                    "paths": {
                      "@lib/*": ["generated/*", "libs/*/src/index.ts"],
                      "@lib/core/*": ["core/*"],
                    },
                  },
                }"#,
            ),
            ("web/tsconfig.json", r#"{ "extends": "../tsconfig.base", "compilerOptions": { "baseUrl": ".." } }"#),
            ("libs/cart/src/index.ts", SAVE),
            ("core/x.ts", "export function load() {}\n"),
            // Same-named files the old tail matching would have picked.
            ("web/cart.ts", SAVE),
            ("web/libs/core/x.ts", "export function load() {}\n"),
            (
                "web/app/main.ts",
                "import { save } from '@lib/cart';\nimport { load } from '@lib/core/x';\nexport function run() { save(); load(); }\n",
            ),
        ],
    );
    // `generated/cart` isn't there: the second substitution is.
    assert_eq!(call(&store, "save", "web/app/main.ts"), ("exact", vec!["libs/cart/src/index.ts".to_string()], None));
    // The longest prefix wins.
    assert_eq!(call(&store, "load", "web/app/main.ts"), ("exact", vec!["core/x.ts".to_string()], None));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn each_file_follows_its_nearest_tsconfig() {
    let (root, store) = repo(
        "nearest",
        &[
            ("a/tsconfig.json", r#"{ "compilerOptions": { "paths": { "@shared/*": ["./one/*"] } } }"#),
            ("b/jsconfig.json", r#"{ "compilerOptions": { "baseUrl": "src", "paths": { "@shared/*": ["two/*"] } } }"#),
            ("a/one/save.ts", SAVE),
            ("b/src/two/save.ts", SAVE),
            ("a/main.ts", "import { save } from '@shared/save';\nexport function run() { save(); }\n"),
            ("b/src/main.js", "import { save } from '@shared/save';\nexport function run() { save(); }\n"),
        ],
    );
    assert_eq!(call(&store, "save", "a/main.ts"), ("exact", vec!["a/one/save.ts".to_string()], None));
    assert_eq!(call(&store, "save", "b/src/main.js"), ("exact", vec!["b/src/two/save.ts".to_string()], None));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_alias_a_config_maps_to_no_indexed_file_is_never_guessed() {
    let (root, store) = repo(
        "missing",
        &[
            ("tsconfig.json", r#"{ "compilerOptions": { "paths": { "@gen/*": ["./dist/gen/*"] } } }"#),
            ("src/save.ts", SAVE),
            ("main.ts", "import { save } from '@gen/save';\nexport function run() { save(); }\n"),
        ],
    );
    assert_eq!(call(&store, "save", "main.ts"), ("unresolved", vec![], Some(query::UNPLACED_MODULE)));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn invariant_without_a_config_a_file_name_guess_is_at_most_unique_name() {
    let (root, store) = repo(
        "guess",
        &[
            ("src/app/save.ts", SAVE),
            ("lib/load.ts", "export function load() {}\n"),
            ("src/app/main.ts", "import { save } from '@app/save';\nimport { load } from 'lib/load';\nexport function run() { save(); load(); }\n"),
            ("src/app/local.ts", "import { save } from './save';\nexport function go() { save(); }\n"),
        ],
    );
    // A bundler's alias, matched by its tail; a path from the repo root without a `baseUrl` saying so.
    assert_eq!(call(&store, "save", "src/app/main.ts"), ("unique_name", vec!["src/app/save.ts".to_string()], Some(query::GUESSED_MODULE)));
    assert_eq!(call(&store, "load", "src/app/main.ts"), ("unique_name", vec!["lib/load.ts".to_string()], Some(query::GUESSED_MODULE)));
    // A relative import needs no config.
    assert_eq!(call(&store, "save", "src/app/local.ts"), ("exact", vec!["src/app/save.ts".to_string()], None));
    // Invariant: nothing guessed is ever exact.
    let (hits, _) = query::refs(store.conn(), "save", &[], 50).unwrap();
    let (more, _) = query::refs(store.conn(), "load", &[], 50).unwrap();
    for h in hits.iter().chain(&more) {
        assert!(!(h.basis == Some(query::GUESSED_MODULE) && h.confidence == "exact"), "{}:{} {}", h.file, h.line, h.name);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_tsconfig_edit_is_seen_by_the_next_query() {
    let (root, mut store) = repo(
        "edit",
        &[
            ("tsconfig.json", r#"{ "compilerOptions": { "paths": { "@x/*": ["./one/*"] } } }"#),
            ("one/save.ts", SAVE),
            ("two/save.ts", SAVE),
            ("main.ts", "import { save } from '@x/save';\nexport function run() { save(); }\n"),
        ],
    );
    assert_eq!(call(&store, "save", "main.ts").1, ["one/save.ts"]);
    write(&root, "tsconfig.json", r#"{ "compilerOptions": { "paths": { "@x/*": ["./two/*"] } } }"#);
    sync(&root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    assert_eq!(call(&store, "save", "main.ts").1, ["two/save.ts"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn python_names_resolve_in_the_module_they_are_imported_from() {
    let (root, store) = repo(
        "python",
        &[
            ("a.py", "def save():\n    return 1\n"),
            ("b.py", "def save():\n    return 2\n"),
            ("caller.py", "from b import save\ndef run():\n    return save()\n"),
            ("pkg/__init__.py", "from .impl import save\n"),
            ("pkg/impl.py", "def save():\n    return 3\n"),
            ("pkg/use.py", "from . import impl\nfrom .impl import save as store\ndef go():\n    impl.save()\n    return store()\n"),
            ("other.py", "import a\nimport pkg\nfrom requests import save\ndef run():\n    a.save()\n    pkg.save()\n    save()\n"),
        ],
    );
    let (hits, _) = query::refs(store.conn(), "save", &["call"], 50).unwrap();
    let at = |file: &str, line: u32| -> (&'static str, Vec<String>) {
        let h = hits.iter().find(|h| h.file == file && h.line == line).unwrap_or_else(|| panic!("no save at {file}:{line}"));
        (h.confidence, h.targets.iter().map(|t| t.file.clone()).collect())
    };
    let exact = |f: &str| ("exact", vec![f.to_string()]);
    assert_eq!(at("caller.py", 3), exact("b.py"));
    // A module imported whole, a submodule imported from a package, a package re-exporting a name.
    assert_eq!(at("pkg/use.py", 4), exact("pkg/impl.py"));
    assert_eq!(at("other.py", 5), exact("a.py"));
    assert_eq!(at("other.py", 6), exact("pkg/impl.py"));
    // From a package outside the repo: never the repo's same-named functions.
    assert_eq!(at("other.py", 7), ("unresolved", vec![]));
    let (aliased, _) = query::refs(store.conn(), "store", &["call"], 50).unwrap();
    assert_eq!((aliased[0].confidence, aliased[0].targets[0].file.as_str()), ("exact", "pkg/impl.py"));
    let _ = std::fs::remove_dir_all(&root);
}
