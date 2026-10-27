//! The workspace summary of a repo is kept across a sync that changes nothing it reads (an edit inside a method body)
//! and read again after one that does; either way it is the summary its index gives (#35: the first query after an
//! edit read every table of the edited repo again for its summary).

use std::path::{Path, PathBuf};
use std::process::Command;

use kc_index::{SyncOptions, sync, workspace};
use serde_json::Value;

fn base() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("knowcode-summary-reuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: set once, before anything in this test binary reads them (one test).
    unsafe {
        std::env::set_var("KNOWCODE_CACHE_DIR", dir.join("cache"));
        std::env::set_var("KNOWCODE_CONFIG_DIR", dir.join("config"));
        std::env::set_var("KNOWCODE_DATA_DIR", dir.join("data"));
    }
    dir
}

const CONTROLLER: &str = "using Microsoft.AspNetCore.Mvc;
namespace Shop.Api;
[Route(\"api/orders\")]
public class OrdersController : ControllerBase
{
    [HttpGet(\"{id}\")]
    public string Get(int id)
    {
        var label = \"order\";
        return label + id;
    }
}
";

fn without_stamps(v: Option<&workspace::RepoSummary>) -> Value {
    let mut v = serde_json::to_value(v.expect("a summary")).unwrap();
    let m = v.as_object_mut().unwrap();
    m.remove("stamp");
    m.remove("indexed_at");
    v
}

/// The summary the workspace gives for `root`, and the one its index gives read afresh.
fn both(root: &Path) -> (Value, Value) {
    let kept = workspace::summaries(&[root.to_path_buf()]).unwrap();
    let kept = kept.iter().find(|s| s.root == root).cloned();
    let fresh = workspace::read_summary(&kc_index::db_path(root).unwrap()).unwrap();
    (without_stamps(kept.as_ref()), without_stamps(fresh.as_ref()))
}

fn summary_gen(root: &Path) -> Option<String> {
    kc_index::open(root).unwrap().meta(kc_index::SUMMARY_GEN).unwrap()
}

#[test]
fn invariant_a_kept_summary_is_the_one_its_index_gives() {
    let dir = base();
    let repo = dir.join("shop");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    assert!(Command::new("git").arg("init").arg("-q").arg(&repo).status().unwrap().success());
    let root = kc_index::repo_root(&repo).unwrap();
    let file = root.join("src/OrdersController.cs");
    let opts = SyncOptions { threads: 1, ..Default::default() };
    let resync = |text: &str| {
        std::fs::write(&file, text).unwrap();
        sync(&root, &mut kc_index::open(&root).unwrap(), &opts).unwrap();
    };
    resync(CONTROLLER);
    let (kept, fresh) = both(&root);
    assert_eq!(kept, fresh);
    let first = summary_gen(&root).expect("set by the first sync");

    // Inside a body, in place: nothing the summary reads changes. The summary generation holds and the kept
    // summary, revalidated rather than read again, is still the index's.
    resync(&CONTROLLER.replace("\"order\"", "\"an order\""));
    assert_eq!(summary_gen(&root).as_deref(), Some(first.as_str()), "a body-only edit keeps the summary generation");
    let (kept, fresh) = both(&root);
    assert_eq!(kept, fresh);

    // Each of these changes what the summary reads: a new reference, lines moving (route rows carry them), a new
    // route. The generation moves and the summary is read again.
    let edits = [
        ("return label + id;", "Audit.Record(id);\n        return label + id;"),
        ("namespace Shop.Api;", "namespace Shop.Api;\n"),
        ("    }\n}\n", "    }\n    [HttpPost]\n    public void Post() { }\n}\n"),
    ];
    let mut last = first;
    let mut text = CONTROLLER.to_string();
    for (i, (from, to)) in edits.iter().enumerate() {
        text = text.replacen(from, to, 1);
        resync(&text);
        let now = summary_gen(&root).unwrap();
        assert_ne!(now, last, "edit {i} changes what the summary reads");
        last = now;
        let (kept, fresh) = both(&root);
        assert_eq!(kept, fresh, "edit {i}");
    }
    let s = workspace::summaries(std::slice::from_ref(&root)).unwrap();
    let s = s.iter().find(|s| s.root == root).unwrap();
    assert!(s.may_reference("Record"), "the new reference reaches the summary");
    assert_eq!(s.routes.iter().filter(|r| r.side == "server").count(), 2, "{:?}", s.routes);

    // Removing a file moves it too.
    std::fs::write(root.join("src/Extra.cs"), "namespace Shop.Api;\npublic class Extra { }\n").unwrap();
    sync(&root, &mut kc_index::open(&root).unwrap(), &opts).unwrap();
    let with_extra = summary_gen(&root).unwrap();
    std::fs::remove_file(root.join("src/Extra.cs")).unwrap();
    sync(&root, &mut kc_index::open(&root).unwrap(), &opts).unwrap();
    assert_ne!(summary_gen(&root).unwrap(), with_extra);
    let (kept, fresh) = both(&root);
    assert_eq!(kept, fresh);
    let _ = std::fs::remove_dir_all(&dir);
}
