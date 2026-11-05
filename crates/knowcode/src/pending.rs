//! A place (`file:line`, `path#name`) that resolved nowhere because the index doesn't hold its file yet, said as such.
//!
//! A query indexes what changed before it answers, within its budget; past the budget it answers from the committed
//! snapshot and says `stale` (see [`crate::refresh::pending`]). A place whose file that snapshot doesn't hold yet (a
//! repo's first index still running, or a file added since) isn't absent: refusing it `not_found`, "no indexed file
//! has the path", would read as "there is no such file" when the file is on disk and only waits to be indexed.

use std::path::{Path, PathBuf};

use kc_store::answer::{Code, Refusal};

/// The refusal for `place`, which resolved in none of `roots` for the reason `why`: `indexing` when one of `roots`
/// has the place's file on disk and changes its index hasn't caught up with yet, `not_found` otherwise.
pub fn unresolved(place: &str, roots: &[PathBuf], why: String) -> Refusal {
    match waiting(place, roots) {
        Some(repo) => Refusal::new(
            Code::Indexing,
            format!("{why}; {repo} is still being indexed and its index may not hold that file yet: ask again after indexing finishes"),
        ),
        None => Refusal::new(Code::NotFound, why),
    }
}

/// The name of the first of `roots` with `place`'s file on disk (the path relative to it, or after its name) whose
/// working tree changed since its last committed index.
fn waiting(place: &str, roots: &[PathBuf]) -> Option<String> {
    let (file, _) = kc_store::query::place(place)?;
    roots.iter().find_map(|root| {
        let name = crate::ops::repo_name(root);
        let rel = file.strip_prefix(&format!("{name}/")).filter(|r| root.join(r).is_file()).unwrap_or(file);
        // A file the privacy file blocks is never indexed: no wait would bring it.
        let blocked = kc_index::privacy::Privacy::load(root).blocks(rel);
        (root.join(rel).is_file() && !blocked && behind(root)).then_some(name)
    })
}

/// Whether `root`'s working tree changed since its index was last committed, or it has no index yet.
fn behind(root: &Path) -> bool {
    match kc_index::db_path(root) {
        Ok(db) => !db.exists() || kc_index::changed_since_sync(root, &db),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::ops::tests::{repo, test_cache};
    use crate::ops::{self, Scope};
    use kc_store::answer::{Code, Refusal};

    /// The refusal `refs` gives for `place`, answered from the index as it is: as when indexing outlasts the call's
    /// budget, or a first index is still running.
    fn refused(root: &std::path::Path, place: &str) -> (Code, String) {
        let err = ops::refs(place, &[], 10, &Scope { repos: vec![root.to_path_buf()], no_sync: true }).err().expect("refused");
        let r = err.downcast_ref::<Refusal>().expect("a refusal");
        (r.code, r.message.clone())
    }

    #[test]
    fn a_place_in_a_file_the_index_doesnt_hold_yet_is_indexing_not_absent() {
        let cache = test_cache();
        let base = cache.dir("pending-place");
        let root = repo(&base, "app", &[("src/old.py", "def old():\n    pass\n")]);
        std::fs::write(root.join("src/new.py"), "def new():\n    old()\n").unwrap();
        let (code, message) = refused(&root, "src/new.py:1");
        assert_eq!(code, Code::Indexing, "{message}");
        assert!(message.contains("no indexed file has the path src/new.py") && message.contains("app is still being indexed"), "{message}");
        // Written after the repo's name, as an answer from several repos names it.
        assert_eq!(refused(&root, "app/src/new.py:1").0, Code::Indexing);
        // A file that isn't there at all is absent.
        assert_eq!(refused(&root, "src/gone.py:1").0, Code::NotFound);
        // What `kc_impact` refuses an empty answer with (see `mcp::placed`): the same.
        let why = ops::unresolved_place("src/new.py:1", &Scope { repos: vec![root.clone()], no_sync: true }).unwrap().unwrap();
        assert_eq!(super::unresolved("src/new.py:1", std::slice::from_ref(&root), why).code, Code::Indexing);
        // Indexed, the file's place resolves, and one that names no definition there is `not_found` again.
        let synced = Scope { repos: vec![root.clone()], no_sync: false };
        assert!(ops::refs("src/new.py:1", &[], 10, &synced).is_ok());
        let err = ops::refs("src/new.py:9", &[], 10, &synced).err().expect("no definition there");
        assert_eq!(err.downcast_ref::<Refusal>().unwrap().code, Code::NotFound);
        let _ = std::fs::remove_dir_all(&base);
    }
}
