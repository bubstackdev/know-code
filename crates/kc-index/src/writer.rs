//! Cross-process serialization of index planning and writes, independent of SQLite read snapshots.
//!
//! Lock files live outside `repos/` and are never removed during normal pruning: unlinking a held lock would let
//! another process lock a different inode for the same repository. The OS releases a lock when its owner exits.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

thread_local! {
    // Nested operations on this thread share the lease rather than waiting on their own file lock.
    static HELD: RefCell<HashMap<PathBuf, Weak<File>>> = RefCell::new(HashMap::new());
}

/// A repository's exclusive indexing lease. Readers do not acquire this lock.
pub struct WriterLock {
    _file: Rc<File>,
}

impl WriterLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        Self::at(&crate::db_path(root)?.with_extension("lock"))
    }

    fn at(db_lock: &Path) -> Result<Self> {
        Self::get(db_lock, true)?.context("index writer lock was not acquired")
    }

    /// Nonblocking: maintenance must skip a repo another process is indexing.
    pub fn try_acquire(root: &Path) -> Result<Option<Self>> {
        Self::get(&crate::db_path(root)?.with_extension("lock"), false)
    }

    fn get(db_lock: &Path, wait: bool) -> Result<Option<Self>> {
        let path = lock_path(db_lock)?;
        if let Some(file) = HELD.with(|h| h.borrow().get(&path).and_then(Weak::upgrade)) {
            return Ok(Some(Self { _file: file }));
        }
        let file = lock_file(&path)?;
        loop {
            kc_store::limits::check()?;
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if !wait => return Ok(None),
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
                Err(TryLockError::Error(e)) => return Err(e).context("locking repository index"),
            }
        }
        let file = Rc::new(file);
        HELD.with(|h| {
            let mut held = h.borrow_mut();
            held.retain(|_, v| v.strong_count() > 0);
            held.insert(path, Rc::downgrade(&file));
        });
        Ok(Some(Self { _file: file }))
    }
}

fn lock_path(db_lock: &Path) -> Result<PathBuf> {
    let name = db_lock.file_name().context("index lock has no file name")?;
    let parent = db_lock.parent().and_then(Path::parent).context("index lock has no cache directory")?;
    let dir = parent.join("locks");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(name))
}

pub(crate) fn lock_file(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new().create(true).truncate(false).read(true).write(true).mode(0o600).open(path)
        .with_context(|| format!("opening index lock {}", path.display()))
}

/// Held for an index connection's entire lifetime, independently of its SQLite snapshots and writer lease.
pub(crate) fn reader(db: &Path) -> Result<File> {
    let file = lock_file(&lock_path(&db.with_extension("use"))?)?;
    file.lock_shared()?;
    Ok(file)
}

/// Cache eviction never waits for another session's readers.
pub(crate) fn idle(db: &Path) -> Result<Option<File>> {
    idle_within(db, Duration::ZERO)
}

/// [`idle`], waiting up to `wait` for the index's readers to close it (a query or a cross-repo link step reading it
/// for a moment). A cancelled operation (see `kc_store::limits`) stops waiting.
pub(crate) fn idle_within(db: &Path, wait: Duration) -> Result<Option<File>> {
    let file = lock_file(&lock_path(&db.with_extension("use"))?)?;
    let until = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(TryLockError::WouldBlock) if Instant::now() >= until => return Ok(None),
            Err(TryLockError::WouldBlock) => {
                kc_store::limits::check()?;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TryLockError::Error(e)) => return Err(e).context("locking idle index"),
        }
    }
}

/// Serializes summary-file edits. Eviction takes this before trying any per-repo exclusive lifetime lease;
/// ordinary summary reads may then open shared index leases without a lock-order cycle.
pub(crate) fn workspace(wait: bool) -> Result<Option<WriterLock>> {
    WriterLock::get(&crate::cache_dir()?.join("repos/workspace-meta.lock"), wait)
}

/// Release a reader snapshot before waiting for a writer; otherwise the current writer's checkpoint can wait on
/// this reader while this reader waits on its lease. Restore snapshot isolation even when waiting or writing fails.
pub(crate) fn with_store<T>(root: &Path, store: &mut kc_store::Store, f: impl FnOnce(&mut kc_store::Store) -> Result<T>) -> Result<T> {
    let snapshot = store.end_snapshot()?;
    let result = (|| {
        let _writer = WriterLock::acquire(root)?;
        f(store)
    })();
    if snapshot {
        store.begin_snapshot()?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_excludes_other_threads_and_allows_nested_calls() {
        let dir = std::env::temp_dir().join(format!("kc-writer-{}", std::process::id()));
        let path = dir.join("repos/repo.lock");
        let first = WriterLock::at(&path).unwrap();
        let nested = WriterLock::at(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let other = std::thread::spawn(move || {
            let _lease = WriterLock::at(&path).unwrap();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first);
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(nested);
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        other.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lease_excludes_another_process() {
        const CHILD: &str = "KNOWCODE_TEST_WRITER_CHILD";
        if let Some(dir) = std::env::var_os(CHILD) {
            let dir = PathBuf::from(dir);
            let _lease = WriterLock::at(&dir.join("repos/repo.lock")).unwrap();
            std::fs::write(dir.join("acquired"), b"yes").unwrap();
            return;
        }
        let dir = std::env::temp_dir().join(format!("kc-writer-process-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lease = WriterLock::at(&dir.join("repos/repo.lock")).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "writer::tests::lease_excludes_another_process"])
            .env(CHILD, &dir)
            .stdout(std::process::Stdio::null())
            .spawn().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(child.try_wait().unwrap().is_none());
        assert!(!dir.join("acquired").exists());
        drop(lease);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("child failed to acquire released lease");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(dir.join("acquired").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
