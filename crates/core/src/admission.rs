//! Account/host-scoped, nonqueued query ownership across processes and images.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub const DEFAULT_MAX_ACTIVE_QUERIES: u32 = 4;
pub const MAX_ACTIVE_QUERIES: u32 = 64;
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
const VERSION: u32 = 1;
const MAX_POLICY_BYTES: u64 = 4096;
const GUARD: &str = "policy.lock";
const POLICY: &str = "policy.json";
const NEXT: &str = "policy.next";

pub fn check_request_bytes(length: usize) -> Result<()> {
    if length > MAX_REQUEST_BYTES {
        return Err(Error::InvalidRequest(format!(
            "serialized query request exceeds {MAX_REQUEST_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: u32,
    pub max_active_queries: u32,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            version: VERSION,
            max_active_queries: DEFAULT_MAX_ACTIVE_QUERIES,
        }
    }
}
fn capacity(value: u32) -> Result<()> {
    if !(1..=MAX_ACTIVE_QUERIES).contains(&value) {
        return Err(Error::InvalidRequest(format!(
            "max_active_queries must be between 1 and {MAX_ACTIVE_QUERIES}"
        )));
    }
    Ok(())
}
fn regular(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(Error::InvalidRequest(format!(
            "query admission state is not an owned regular file: {}",
            path.display()
        )));
    }
    Ok(())
}
fn open(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    regular(path, &file.metadata()?)?;
    Ok(file)
}
fn lock(file: &File, shared: bool, message: &str) -> Result<()> {
    let result = if shared {
        file.try_lock_shared()
    } else {
        file.try_lock()
    };
    match result {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::Busy(message.into())),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

#[derive(Clone, Debug)]
pub struct AdmissionScope {
    path: PathBuf,
}
impl AdmissionScope {
    pub fn local() -> Result<Self> {
        if let Some(scope) = std::env::var_os("KNOW_CODE_QUERY_SCOPE") {
            return Self::at(scope);
        }
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .ok_or_else(|| {
                Error::InvalidRequest("HOME is required to locate account query admission".into())
            })?;
        Self::at(PathBuf::from(home).join("Library/Application Support/Know Code/query-admission"))
    }
    pub fn at(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(Error::InvalidRequest(
                "query admission scope must not be empty".into(),
            ));
        }
        Ok(Self {
            path: if path.is_absolute() {
                path.to_owned()
            } else {
                std::env::current_dir()?.join(path)
            },
        })
    }
    pub fn directory(&self) -> &Path {
        &self.path
    }
    fn prepare(&self) -> Result<Self> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(&self.path)?;
        let path = fs::canonicalize(&self.path)?;
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(Error::InvalidRequest(format!(
                "query admission scope is not an owned directory: {}",
                path.display()
            )));
        }
        Ok(Self { path })
    }
    fn guard(&self, shared: bool) -> Result<File> {
        let file = open(&self.path.join(GUARD), true)?;
        lock(
            &file,
            shared,
            "query admission policy is in use; retry the operation",
        )?;
        Ok(file)
    }
    fn read_policy(&self) -> Result<Policy> {
        let mut file = match open(&self.path.join(POLICY), false) {
            Ok(file) => file,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Policy::default());
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > MAX_POLICY_BYTES {
            return Err(Error::InvalidRequest(
                "query admission policy exceeds its size bound".into(),
            ));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_POLICY_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_POLICY_BYTES {
            return Err(Error::InvalidRequest(
                "query admission policy exceeds its size bound".into(),
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let version = value
            .get("version")
            .and_then(|version| version.as_u64())
            .ok_or_else(|| {
                Error::InvalidRequest("query admission policy requires an integer version".into())
            })?;
        if version != u64::from(VERSION) {
            return Err(Error::UpdateRequired(format!(
                "query admission policy version {version} is unsupported; update the runtime and clients together"
            )));
        }
        let policy: Policy = serde_json::from_value(value)?;
        capacity(policy.max_active_queries)?;
        Ok(policy)
    }
    fn slot(&self, index: u32) -> Result<File> {
        open(&self.path.join(format!("slot-{index:02}.lock")), true)
    }
    pub fn policy(&self) -> Result<Policy> {
        let scope = self.prepare()?;
        let _guard = scope.guard(true)?;
        scope.read_policy()
    }
    pub fn configure(&self, max_active_queries: u32) -> Result<Policy> {
        capacity(max_active_queries)?;
        let scope = self.prepare()?;
        let _guard = scope.guard(false)?;
        let previous = scope.read_policy()?;
        let mut slots = Vec::new();
        for index in 0..previous.max_active_queries.max(max_active_queries) {
            let file = scope.slot(index)?;
            lock(
                &file,
                false,
                "query admission configuration requires quiescence; active queries remain",
            )?;
            slots.push(file);
        }
        let policy = Policy {
            version: VERSION,
            max_active_queries,
        };
        let mut file = open(&scope.path.join(NEXT), true)?;
        let bytes = serde_json::to_vec(&policy)?;
        file.set_len(0)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(scope.path.join(NEXT), scope.path.join(POLICY))?;
        File::open(&scope.path)?.sync_all()?;
        Ok(policy)
    }
    pub fn try_reserve(&self) -> Result<Reservation> {
        let scope = self.prepare()?;
        let _guard = scope.guard(true)?;
        let policy = scope.read_policy()?;
        for index in 0..policy.max_active_queries {
            let file = scope.slot(index)?;
            match file.try_lock() {
                Ok(()) => {
                    return Ok(Reservation {
                        lease: Arc::new(Lease {
                            _slot: file,
                            submitted: AtomicBool::new(false),
                        }),
                    });
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        Err(Error::Busy(format!(
            "query capacity is exhausted (max_active_queries={}); retry after active work finishes",
            policy.max_active_queries
        )))
    }
}

#[derive(Debug)]
struct Lease {
    _slot: File,
    submitted: AtomicBool,
}
/// Public ownership may be placed in Arc by a host, but can submit only once.
#[derive(Debug)]
pub struct Reservation {
    lease: Arc<Lease>,
}
impl Reservation {
    pub(crate) fn submit(&self) -> Result<Permit> {
        self.lease
            .submitted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                Error::InvalidRequest("query reservation has already been submitted".into())
            })?;
        Ok(Permit {
            _lease: Arc::clone(&self.lease),
        })
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Permit {
    _lease: Arc<Lease>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::fd::AsRawFd,
        process::{Child, Command, Stdio},
        sync::atomic::AtomicU64,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    fn fixture() -> AdmissionScope {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/query-admission-tests")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        AdmissionScope::at(path).unwrap()
    }
    fn busy<T: std::fmt::Debug>(result: Result<T>) {
        assert!(matches!(result, Err(Error::Busy(_))), "{result:?}");
    }
    fn identity(path: &Path) -> (u64, u64) {
        let metadata = fs::metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    }
    #[test]
    fn pure_selection_default_four_and_independent_opens_share_one_scope() {
        let scope = fixture();
        assert!(!scope.directory().exists());
        let second = AdmissionScope::at(scope.directory()).unwrap();
        assert_eq!(scope.policy().unwrap(), Policy::default());
        let reservations = (0..4)
            .map(|_| second.try_reserve().unwrap())
            .collect::<Vec<_>>();
        busy(scope.try_reserve());
        drop(reservations);
        assert!(scope.try_reserve().is_ok());
        let alias = scope.directory().with_extension("alias");
        std::os::unix::fs::symlink(scope.directory(), &alias).unwrap();
        let alias = AdmissionScope::at(alias).unwrap();
        let held = (0..4)
            .map(|_| scope.try_reserve().unwrap())
            .collect::<Vec<_>>();
        busy(alias.try_reserve());
        drop(held);
        assert!(alias.try_reserve().is_ok());
        let isolated = fixture();
        assert!(isolated.try_reserve().is_ok());
    }
    #[test]
    fn one_submission_and_permit_clones_hold_exactly_one_slot() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Reservation>();
        send_sync::<Permit>();
        let scope = fixture();
        let reservation = scope.try_reserve().unwrap();
        let permit = reservation.submit().unwrap();
        let clone = permit.clone();
        assert!(matches!(
            reservation.submit(),
            Err(Error::InvalidRequest(_))
        ));
        let other = (0..3)
            .map(|_| scope.try_reserve().unwrap())
            .collect::<Vec<_>>();
        busy(scope.try_reserve());
        drop(reservation);
        drop(permit);
        busy(scope.try_reserve());
        drop(clone);
        let replacement = scope.try_reserve().unwrap();
        busy(scope.try_reserve());
        drop((replacement, other));
        assert!(scope.try_reserve().is_ok());
        let reservation = Arc::new(scope.try_reserve().unwrap());
        let outcomes = std::thread::scope(|threads| {
            (0..8)
                .map(|_| {
                    let reservation = Arc::clone(&reservation);
                    threads.spawn(move || reservation.submit())
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|r| matches!(r, Err(Error::InvalidRequest(_))))
                .count(),
            7
        );
    }
    #[test]
    fn policy_changes_require_all_old_slots_and_preserve_kernel_inodes() {
        let scope = fixture();
        scope.configure(8).unwrap();
        let peer = AdmissionScope::at(scope.directory()).unwrap();
        let paths = std::iter::once(scope.directory().join(GUARD))
            .chain((0..8).map(|index| scope.directory().join(format!("slot-{index:02}.lock"))))
            .collect::<Vec<_>>();
        let identities = paths.iter().map(|path| identity(path)).collect::<Vec<_>>();
        let mut reservations = (0..8)
            .map(|_| scope.try_reserve().unwrap())
            .collect::<Vec<_>>();
        let high = reservations.pop().unwrap();
        drop(reservations);
        busy(peer.configure(2));
        assert_eq!(peer.policy().unwrap().max_active_queries, 8);
        drop(high);
        scope.configure(2).unwrap();
        assert_eq!(peer.policy().unwrap().max_active_queries, 2);
        let two = (0..2)
            .map(|_| peer.try_reserve().unwrap())
            .collect::<Vec<_>>();
        busy(scope.try_reserve());
        busy(scope.configure(8));
        drop(two);
        scope.configure(8).unwrap();
        assert_eq!(
            paths.iter().map(|path| identity(path)).collect::<Vec<_>>(),
            identities
        );
        for bad in [0, MAX_ACTIVE_QUERIES + 1] {
            assert!(matches!(
                scope.configure(bad),
                Err(Error::InvalidRequest(_))
            ));
        }
    }
    #[test]
    fn policy_guard_contention_and_state_failures_are_not_capacity_failures() {
        let scope = fixture();
        scope.configure(4).unwrap();
        let guard = open(&scope.directory().join(GUARD), false).unwrap();
        guard.try_lock().unwrap();
        busy(scope.try_reserve());
        busy(scope.policy());
        busy(scope.configure(2));
        drop(guard);
        let policy = scope.directory().join(POLICY);
        for (bytes, code) in [
            (
                br#"{"version":99,"max_active_queries":4,"future":true}"#.as_slice(),
                "update_required",
            ),
            (
                br#"{"version":1,"max_active_queries":0}"#.as_slice(),
                "invalid_request",
            ),
            (b"{broken".as_slice(), "invalid_request"),
        ] {
            fs::write(&policy, bytes).unwrap();
            assert_eq!(scope.try_reserve().unwrap_err().code(), code);
            assert_eq!(scope.configure(2).unwrap_err().code(), code);
            assert_eq!(fs::read(&policy).unwrap(), bytes);
        }
        fs::write(&policy, serde_json::to_vec(&Policy::default()).unwrap()).unwrap();
        let next = scope.directory().join(NEXT);
        fs::create_dir(&next).unwrap();
        let before = fs::read(&policy).unwrap();
        let inode = identity(&policy);
        assert!(matches!(
            scope.configure(2),
            Err(Error::Io(_) | Error::InvalidRequest(_))
        ));
        assert_eq!(fs::read(&policy).unwrap(), before);
        assert_eq!(identity(&policy), inode);
        fs::remove_dir(next).unwrap();
        scope.configure(2).unwrap();
        let two = (0..2)
            .map(|_| scope.try_reserve().unwrap())
            .collect::<Vec<_>>();
        busy(scope.try_reserve());
        drop(two);
        let unsafe_scope = fixture();
        unsafe_scope.policy().unwrap();
        let target = unsafe_scope.directory().join("foreign");
        fs::write(&target, b"untouched").unwrap();
        std::os::unix::fs::symlink(&target, unsafe_scope.directory().join(POLICY)).unwrap();
        assert!(matches!(unsafe_scope.try_reserve(), Err(Error::Io(_))));
        assert_eq!(fs::read(target).unwrap(), b"untouched");
        let unsafe_slot = fixture();
        unsafe_slot.policy().unwrap();
        let target = unsafe_slot.directory().join("foreign");
        fs::write(&target, b"untouched").unwrap();
        fs::hard_link(&target, unsafe_slot.directory().join("slot-00.lock")).unwrap();
        assert!(matches!(
            unsafe_slot.try_reserve(),
            Err(Error::InvalidRequest(_))
        ));
        assert_eq!(fs::read(target).unwrap(), b"untouched");
    }
    #[test]
    fn request_guard_uses_utf8_bytes_and_has_one_uniform_error() {
        let text = "😀".repeat(MAX_REQUEST_BYTES / 4);
        assert_eq!(text.len(), MAX_REQUEST_BYTES);
        check_request_bytes(text.len()).unwrap();
        let error = check_request_bytes((text + "π").len()).unwrap_err();
        assert_eq!(error.code(), "invalid_request");
        assert_eq!(
            error.to_string(),
            "serialized query request exceeds 262144 UTF-8 bytes"
        );
        assert!(check_request_bytes(usize::MAX).is_err());
    }
    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn child(scope: &AdmissionScope, mode: &str) -> Process {
        scope.policy().unwrap();
        let ready = scope.directory().join(format!("ready-{mode}"));
        let mut process = Process(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "admission::tests::child_process", "--nocapture"])
                .env("KNOW_CODE_ADMISSION_TEST_SCOPE", scope.directory())
                .env("KNOW_CODE_ADMISSION_TEST_MODE", mode)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let until = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                Instant::now() < until,
                "admission child did not become ready"
            );
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "admission child exited early"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        process
    }
    #[test]
    fn child_process() {
        let Some(path) = std::env::var_os("KNOW_CODE_ADMISSION_TEST_SCOPE") else {
            return;
        };
        let mode = std::env::var("KNOW_CODE_ADMISSION_TEST_MODE").unwrap();
        let scope = AdmissionScope::at(path).unwrap();
        let _reservation = (mode == "hold").then(|| scope.try_reserve().unwrap());
        fs::write(scope.directory().join(format!("ready-{mode}")), b"ready").unwrap();
        loop {
            std::thread::park_timeout(Duration::from_secs(1));
        }
    }
    #[test]
    fn independent_process_capacity_and_crash_release_are_kernel_owned() {
        let scope = fixture();
        let mut child = child(&scope, "hold");
        let remaining = (0..3)
            .map(|_| scope.try_reserve().unwrap())
            .collect::<Vec<_>>();
        busy(scope.try_reserve());
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let recovered = scope.try_reserve().unwrap();
        busy(scope.try_reserve());
        drop((recovered, remaining));
        assert!(scope.try_reserve().is_ok());
    }
    #[test]
    fn close_on_exec_and_unwind_release_without_explicit_unlock() {
        let scope = fixture();
        let reservation = scope.try_reserve().unwrap();
        let flags = unsafe { libc::fcntl(reservation.lease._slot.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        let _child = child(&scope, "idle");
        drop(reservation);
        let four = (0..4)
            .map(|_| scope.try_reserve().unwrap())
            .collect::<Vec<_>>();
        busy(scope.try_reserve());
        drop(four);
        let outcome = std::panic::catch_unwind(|| {
            let _reservation = scope.try_reserve().unwrap();
            panic!("owned fixture unwind");
        });
        assert!(outcome.is_err());
        let four = (0..4)
            .map(|_| scope.try_reserve().unwrap())
            .collect::<Vec<_>>();
        busy(scope.try_reserve());
        drop(four);
    }
}
