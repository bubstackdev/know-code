//! Same-named definitions: live code ranks before tests and copies (backups, old versions, template variants), and
//! one is picked by place (`path#name`, `file:line`, or a page's path).

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync};
use kc_store::Store;
use kc_store::query;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ok = Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap().success();
    assert!(ok, "git init");
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn index(root: &Path) -> Store {
    let mut store = Store::open(&root.join(".git/knowcode-test.sqlite")).unwrap();
    sync(root, &mut store, &SyncOptions { threads: 1, ..Default::default() }).unwrap();
    store
}

fn files(rows: &[query::SymbolRow]) -> Vec<&str> {
    rows.iter().map(|r| r.file.as_str()).collect()
}

#[test]
fn live_code_ranks_first_and_a_place_picks_one() {
    let root = scratch("same-names");
    let page = "<cfquery name=\"q\">SELECT * FROM plans</cfquery>\n";
    // Sorted by path alone, every copy would come before the live page.
    write(&root, "_templates/blank/login.cfm", page);
    write(&root, "2020_backup/login.cfm", page);
    write(&root, "login.cfm", page);
    write(&root, "old/Billing.cs", "public class Billing { public void Run() { } }\n");
    write(&root, "src/Billing.cs", "public class Billing\n{\n    public void Run() { }\n}\n");
    write(&root, "src/Tests/Billing.cs", "public class Billing { public void Run() { } }\n");
    let store = index(&root);
    let conn = store.conn();

    assert_eq!(files(&query::definitions(conn, "login").unwrap()), ["login.cfm", "2020_backup/login.cfm", "_templates/blank/login.cfm"]);
    assert_eq!(files(&query::definitions(conn, "Billing").unwrap()), ["src/Billing.cs", "src/Tests/Billing.cs", "old/Billing.cs"]);

    // By place: `path#name` (the whole path or its end), `file:line` (the innermost definition there), a page's path.
    assert_eq!(files(&query::definitions(conn, "_templates/blank/login.cfm#login").unwrap()), ["_templates/blank/login.cfm"]);
    assert_eq!(files(&query::definitions(conn, "old/Billing.cs#Billing").unwrap()), ["old/Billing.cs"]);
    let at = query::definitions(conn, "src/Billing.cs:3").unwrap();
    assert_eq!((at[0].qualified().as_str(), at[0].file.as_str()), ("Billing.Run", "src/Billing.cs"));
    assert_eq!(files(&query::definitions(conn, "2020_backup/login.cfm").unwrap()), ["2020_backup/login.cfm"]);
    // A place naming nothing finds nothing; a plain name with no place is unaffected.
    assert!(query::definitions(conn, "nowhere/x.cs#Billing").unwrap().is_empty());
    assert_eq!(query::definitions(conn, "Billing.Run").unwrap()[0].file, "src/Billing.cs");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn copy_directories_are_told_from_live_ones() {
    for p in ["2020_backup/a.yml", "old/a.cs", "x/Copy of site/a.cfm", "_templates/v2/a.cfm", "src/legacy_old/a.cs", "db/archive/a.sql"] {
        assert!(kc_store::is_copy_path(p), "{p}");
    }
    for p in ["src/a.cs", "roles/web/templates/a.j2", "src/Old.cs", "golden/a.cs", "src/Backup.cs"] {
        assert!(!kc_store::is_copy_path(p), "{p}");
    }
    assert_eq!((kc_store::path_rank("src/a.cs"), kc_store::path_rank("tests/a.cs"), kc_store::path_rank("bak/a.cs")), (0, 1, 2));
}
