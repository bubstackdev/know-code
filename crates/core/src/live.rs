//! Current-state pages certified by a live coordinator; uncertain state uses control RPC.
use crate::{
    Result,
    protocol::{self, ControlResponse, Envelope, WorkspaceStatus, envelope::Payload},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
const MAGIC: &[u8; 8] = b"KCLIVE01";
const OWNER_MAGIC: &[u8; 8] = b"KCOWNER4";
const MAX_PAGE: u64 = 64 * 1024;
#[derive(Serialize, Deserialize)]
struct Owner {
    version: u32,
    protocol: u32,
    pid: u32,
    start_sec: u64,
    start_usec: u64,
    token: [u8; 16],
    #[serde(default)]
    activity_hints: bool,
    #[serde(default)]
    analysis_settings_version: u32,
    #[serde(default)]
    resource_control_version: u32,
    #[serde(default)]
    index_lifecycle_version: u32,
}
fn process(pid: u32) -> Option<(u64, u64)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let n = unsafe {
        libc::proc_pidinfo(
            pid.try_into().ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if n != size as i32 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    // BSD active/sleeping only. Stopped, exiting, zombie and unknown processes fail closed.
    if info.pbi_pid != pid
        || !matches!(info.pbi_status, libc::SSLEEP | libc::SRUN)
        || info.pbi_flags & 4 != 0
    {
        return None;
    }
    Some((info.pbi_start_tvsec, info.pbi_start_tvusec))
}
fn eligible(s: &WorkspaceStatus) -> bool {
    s.available
        && s.phase == "idle"
        && s.observed_epoch == s.published_epoch
        && s.generation != 0
        && s.error.is_empty()
        && s.progress.is_none()
}
fn page_path(dir: &Path, db: &str) -> PathBuf {
    dir.join(format!("{:x}.page", Sha256::digest(db.as_bytes())))
}
fn bounded(file: &mut File, max: u64) -> std::io::Result<Vec<u8>> {
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > max {
        return Err(std::io::Error::other("invalid live page size/type"));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(std::io::Error::other("oversized live page"));
    }
    Ok(bytes)
}
fn same_file(file: &File, path: &Path) -> bool {
    file.metadata()
        .ok()
        .zip(std::fs::metadata(path).ok())
        .is_some_and(|(a, b)| a.dev() == b.dev() && a.ino() == b.ino())
}
fn owned(file: &File) -> bool {
    match file.try_lock_shared() {
        Err(std::fs::TryLockError::WouldBlock) => true,
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => false,
    }
}
struct Gate {
    ptr: std::ptr::NonNull<u8>,
}
// SAFETY: fixed-size private inode; all shared mutable access uses aligned atomics.
unsafe impl Send for Gate {}
unsafe impl Sync for Gate {}
impl Gate {
    fn new(file: &File, write: bool) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;
        let protection = libc::PROT_READ | if write { libc::PROT_WRITE } else { 0 };
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4096,
                protection,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            ptr: std::ptr::NonNull::new(ptr.cast()).unwrap(),
        })
    }
    fn word(&self, n: usize) -> &AtomicU64 {
        unsafe { &*self.ptr.as_ptr().add(n * 8).cast::<AtomicU64>() }
    }
    fn value(&self, n: usize) -> Option<u32> {
        let bits = self.word(n).load(Ordering::Acquire);
        let low = bits as u32;
        ((bits >> 32) as u32 == !low).then_some(low)
    }
    fn store(&self, n: usize, value: u32) {
        self.word(n).store(
            (u64::from(!value) << 32) | u64::from(value),
            Ordering::Release,
        );
    }
    fn adjust(&self, n: usize, increment: bool) -> bool {
        let mut old = self.word(n).load(Ordering::Acquire);
        loop {
            let low = old as u32;
            if (old >> 32) as u32 != !low {
                return false;
            }
            let Some(next) = (if increment {
                low.checked_add(1)
            } else {
                low.checked_sub(1)
            }) else {
                return false;
            };
            let encoded = (u64::from(!next) << 32) | u64::from(next);
            match self.word(n).compare_exchange_weak(
                old,
                encoded,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(new) => old = new,
            }
        }
    }
    fn clear(&self) -> bool {
        // Do not pair an old overflow sequence with a newer acknowledgement.
        // Completed input handling can retire pages while these words change.
        let pending = self.value(0);
        let observed = self.value(1);
        let handled = self.value(2);
        pending == Some(0)
            && observed.is_some()
            && observed == handled
            && observed == self.value(1)
            && self.value(0) == Some(0)
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr.as_ptr().cast(), 4096);
        }
    }
}
pub struct Input {
    publisher: std::sync::Arc<Publisher>,
    counted: bool,
}
impl Drop for Input {
    fn drop(&mut self) {
        if self.counted
            && self
                .publisher
                .gate
                .as_ref()
                .is_some_and(|gate| !gate.adjust(0, false))
        {
            self.publisher.disable();
        }
        self.publisher.pending_inputs.fetch_sub(1, Ordering::AcqRel);
    }
}
pub struct Publisher {
    gate: Option<Gate>,
    dir: PathBuf,
    owner: Option<Owner>,
    lease: Mutex<Option<File>>,
    published: Mutex<BTreeSet<String>>,
    serial: AtomicU64,
    pending_inputs: AtomicUsize,
}
impl Publisher {
    /// Called while the coordinator holds its root writer fence. Acceleration
    /// is optional: failed setup must not prevent baseline indexing or RPC.
    pub fn new(root: &Path) -> Self {
        Self::try_new(root).unwrap_or_else(|_| Self {
            gate: None,
            dir: root.join("live"),
            owner: None,
            lease: Mutex::new(None),
            published: Mutex::new(BTreeSet::new()),
            serial: AtomicU64::new(0),
            pending_inputs: AtomicUsize::new(0),
        })
    }
    fn try_new(root: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let dir = root.join("live");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let (start_sec, start_usec) = process(std::process::id())
            .ok_or_else(|| std::io::Error::other("cannot identify live publisher"))?;
        let mut token = [0; 16];
        File::open("/dev/urandom")?.read_exact(&mut token)?;
        let owner = Owner {
            version: 4,
            protocol: protocol::VERSION,
            pid: std::process::id(),
            start_sec,
            start_usec,
            token,
            activity_hints: true,
            analysis_settings_version: crate::settings::VERSION,
            resource_control_version: crate::resource::CONTROL_VERSION,
            index_lifecycle_version: protocol::INDEX_LIFECYCLE_VERSION,
        };
        let name = format!(".owner-{}-{}", owner.pid, u128::from_le_bytes(token));
        let staging = dir.join(name);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)?;
        let initialized = (|| -> Result<Self> {
            file.try_lock().map_err(std::io::Error::other)?;
            file.set_len(4096)?;
            let gate = Gate::new(&file, true)?;
            for n in 0..3 {
                gate.store(n, 0);
            }
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(64))?;
            file.write_all(OWNER_MAGIC)?;
            let body = serde_json::to_vec(&owner)?;
            file.write_all(&(body.len() as u32).to_le_bytes())?;
            file.write_all(&body)?;
            file.write_all(&Sha256::digest(&body))?;
            std::fs::rename(&staging, dir.join("owner"))?;
            Ok(Self {
                gate: Some(gate),
                dir,
                owner: Some(owner),
                lease: Mutex::new(Some(file)),
                published: Mutex::new(BTreeSet::new()),
                serial: AtomicU64::new(0),
                pending_inputs: AtomicUsize::new(0),
            })
        })();
        if initialized.is_err() {
            let _ = std::fs::remove_file(&staging);
        }
        initialized
    }
    pub fn input(self: &std::sync::Arc<Self>) -> Input {
        self.pending_inputs.fetch_add(1, Ordering::AcqRel);
        let counted = self.gate.as_ref().is_some_and(|gate| gate.adjust(0, true));
        if self.gate.is_some() && !counted {
            self.disable();
        }
        Input {
            counted,
            publisher: std::sync::Arc::clone(self),
        }
    }
    pub fn inputs_clear(&self) -> bool {
        self.pending_inputs.load(Ordering::Acquire) == 0
    }
    pub fn overflow(&self) {
        if self.gate.as_ref().is_some_and(|gate| !gate.adjust(1, true)) {
            self.disable();
        }
    }
    pub fn overflow_observed(&self) -> u64 {
        u64::from(
            self.gate
                .as_ref()
                .and_then(|gate| gate.value(1))
                .unwrap_or(0),
        )
    }
    pub fn overflow_handled(&self, seen: u64) {
        if let Some(gate) = &self.gate {
            gate.store(2, seen as u32);
        }
    }
    pub fn disable(&self) {
        if let Some(gate) = &self.gate {
            // An invalid encoding permanently retires this input gate.
            gate.word(0).store(0, Ordering::Release);
        }
        if let Some(file) = self.lease.lock().unwrap().take() {
            // Closing alone leaves the flock alive in a duplicated/inherited
            // descriptor. Explicitly unlock the shared open-file description.
            let _ = file.unlock();
        }
    }
    pub fn invalidate(&self, db: &str) {
        if self.published.lock().unwrap().remove(db) {
            match std::fs::remove_file(page_path(&self.dir, db)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => self.disable(),
            }
        }
    }
    pub fn publish(&self, status: &WorkspaceStatus) {
        if !eligible(status) || self.lease.lock().unwrap().is_none() {
            return;
        }
        if self.published.lock().unwrap().contains(&status.database) {
            return;
        }
        if self.write(status).is_err() {
            self.disable();
        }
    }
    fn write(&self, status: &WorkspaceStatus) -> Result<()> {
        let owner = self
            .owner
            .as_ref()
            .ok_or_else(|| std::io::Error::other("live publication disabled"))?;
        let mut bytes = Vec::new();
        protocol::write_frame(
            &mut bytes,
            &Envelope {
                protocol_version: protocol::VERSION,
                job_id: String::new(),
                payload: Some(Payload::ControlResponse(ControlResponse {
                    workspaces: vec![status.clone()],
                    ..Default::default()
                })),
            },
        )?;
        if bytes.len() as u64 + 56 > MAX_PAGE {
            return Err(std::io::Error::other("live status exceeds page limit").into());
        }
        let staging = self.dir.join(format!(
            ".page-{}-{}",
            u128::from_le_bytes(owner.token),
            self.serial.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)?;
        let written = (|| -> Result<()> {
            file.write_all(MAGIC)?;
            file.write_all(&owner.token)?;
            file.write_all(&Sha256::digest(&bytes))?;
            file.write_all(&bytes)?;
            drop(file);
            std::fs::rename(&staging, page_path(&self.dir, &status.database))?;
            self.published
                .lock()
                .unwrap()
                .insert(status.database.clone());
            Ok(())
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&staging);
        }
        written
    }
}
/// Missing, uncertain, corrupt or retired state always returns None for normal RPC fallback.
pub fn current(root: &Path, database: &str) -> Option<WorkspaceStatus> {
    certified(root, database).map(|value| value.status)
}
pub(crate) struct Certification {
    pub status: WorkspaceStatus,
    pub activity_hints: bool,
}
pub(crate) fn certified(root: &Path, database: &str) -> Option<Certification> {
    read_certified(root, database, || {}).ok().flatten()
}
#[cfg(test)]
fn read_current(
    root: &Path,
    database: &str,
    before_final: impl FnOnce(),
) -> Result<Option<WorkspaceStatus>> {
    Ok(read_certified(root, database, before_final)?.map(|value| value.status))
}
fn read_certified(
    root: &Path,
    database: &str,
    before_final: impl FnOnce(),
) -> Result<Option<Certification>> {
    let dir = root.join("live");
    let owner_path = dir.join("owner");
    let mut lease = File::open(&owner_path)?;
    if !owned(&lease) {
        return Ok(None);
    }
    let header = bounded(&mut lease, 4096)?;
    if header.len() != 4096 || &header[64..72] != OWNER_MAGIC {
        return Ok(None);
    }
    let length = u32::from_le_bytes(header[72..76].try_into().unwrap()) as usize;
    if length > 1024 || 76 + length + 32 > header.len() {
        return Ok(None);
    }
    let body = &header[76..76 + length];
    if Sha256::digest(body)[..] != header[76 + length..76 + length + 32] {
        return Ok(None);
    }
    let owner: Owner = serde_json::from_slice(body)?;
    if owner.version != 4
        || owner.protocol != protocol::VERSION
        || owner.analysis_settings_version != crate::settings::VERSION
        || owner.resource_control_version != crate::resource::CONTROL_VERSION
        || owner.index_lifecycle_version != protocol::INDEX_LIFECYCLE_VERSION
        || process(owner.pid) != Some((owner.start_sec, owner.start_usec))
    {
        return Ok(None);
    }
    let gate = Gate::new(&lease, false)?;
    if !gate.clear() {
        return Ok(None);
    }
    let path = page_path(&dir, database);
    let mut file = File::open(&path)?;
    let bytes = bounded(&mut file, MAX_PAGE)?;
    if bytes.len() < 60
        || &bytes[..8] != MAGIC
        || bytes[8..24] != owner.token
        || Sha256::digest(&bytes[56..]).as_slice() != &bytes[24..56]
    {
        return Ok(None);
    }
    let mut cursor = std::io::Cursor::new(&bytes[56..]);
    let Some(envelope) = protocol::read_frame(&mut cursor)? else {
        return Ok(None);
    };
    if cursor.position() as usize != bytes.len() - 56 {
        return Ok(None);
    }
    let Some(Payload::ControlResponse(response)) = envelope.payload else {
        return Ok(None);
    };
    if !response.error_code.is_empty()
        || !response.error_message.is_empty()
        || response.workspaces.len() != 1
    {
        return Ok(None);
    }
    let status = response.workspaces.into_iter().next().unwrap();
    if !gate.clear() {
        return Ok(None);
    }
    // A pending event may be fully handled (clear -> busy -> clear) after the
    // gate check. Its retired page must still fail the final inode checks.
    before_final();
    if !eligible(&status)
        || status.database != database
        || !same_file(&file, &path)
        || !same_file(&lease, &owner_path)
        || !gate.clear()
        || !owned(&lease)
        || process(owner.pid) != Some((owner.start_sec, owner.start_usec))
    {
        return Ok(None);
    }
    Ok(Some(Certification {
        status,
        activity_hints: owner.activity_hints,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::Arc,
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn root() -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/live-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    fn status(root: &Path, generation: u64) -> WorkspaceStatus {
        WorkspaceStatus {
            workspace_root: root.join("repo").to_string_lossy().into(),
            database: root
                .join("workspaces/test/index.db")
                .to_string_lossy()
                .into(),
            phase: "idle".into(),
            observed_epoch: 7,
            published_epoch: 7,
            generation,
            available: true,
            ..Default::default()
        }
    }
    #[test]
    fn unavailable_live_directory_preserves_rpc_only_operation() {
        let root = root();
        std::fs::write(root.join("live"), b"not a directory").unwrap();
        let publisher = Arc::new(Publisher::new(&root));
        let status = status(&root, 1);
        publisher.publish(&status);
        let input = publisher.input();
        publisher.overflow();
        publisher.overflow_handled(publisher.overflow_observed());
        drop(input);
        assert!(publisher.lease.lock().unwrap().is_none());
        assert!(current(&root, &status.database).is_none());
    }
    #[test]
    fn publication_failure_retires_the_lease_and_cleans_staging() {
        let root = root();
        let publisher = Publisher::new(&root);
        let status = status(&root, 1);
        let path = page_path(&root.join("live"), &status.database);
        std::fs::create_dir(&path).unwrap();
        publisher.publish(&status);
        assert!(publisher.lease.lock().unwrap().is_none());
        assert!(current(&root, &status.database).is_none());
        assert!(std::fs::read_dir(root.join("live")).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".page-")
        }));
    }
    #[test]
    fn failed_invalidation_disables_all_certificates_before_mutation() {
        let root = root();
        let publisher = Publisher::new(&root);
        let status = status(&root, 1);
        publisher.publish(&status);
        let path = page_path(&root.join("live"), &status.database);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(path).unwrap();
        publisher.invalidate(&status.database);
        assert!(publisher.lease.lock().unwrap().is_none());
    }
    #[test]
    fn malformed_owner_length_is_rejected_before_mapping() {
        let root = root();
        let publisher = Publisher::new(&root);
        let status = status(&root, 1);
        publisher.publish(&status);
        // The private producer never truncates a published inode. A malformed
        // pre-existing length must be rejected before the consumer maps it.
        OpenOptions::new()
            .write(true)
            .open(root.join("live/owner"))
            .unwrap()
            .set_len(128)
            .unwrap();
        assert!(current(&root, &status.database).is_none());
        publisher.disable();
    }
    #[test]
    fn current_state_retires_before_mutation_and_republishes() {
        let r = root();
        let p = Publisher::new(&r);
        let mut s = status(&r, 1);
        p.publish(&s);
        assert_eq!(current(&r, &s.database).unwrap().generation, 1);
        p.invalidate(&s.database);
        assert!(current(&r, &s.database).is_none());
        s.generation = 2;
        p.publish(&s);
        assert_eq!(current(&r, &s.database).unwrap().generation, 2);
        p.disable();
        assert!(current(&r, &s.database).is_none());
    }
    #[test]
    fn queued_inputs_and_overflow_gate_previously_current_pages() {
        let r = root();
        let p = Arc::new(Publisher::new(&r));
        let s = status(&r, 1);
        p.publish(&s);
        let a = p.input();
        let b = p.input();
        assert!(current(&r, &s.database).is_none());
        drop(a);
        assert!(current(&r, &s.database).is_none());
        drop(b);
        assert!(current(&r, &s.database).is_some());
        p.overflow();
        let seen = p.overflow_observed();
        p.overflow();
        p.overflow_handled(seen);
        assert!(current(&r, &s.database).is_none());
        p.overflow_handled(p.overflow_observed());
        assert!(current(&r, &s.database).is_some());
        p.gate.as_ref().unwrap().word(0).store(0, Ordering::Release);
        assert!(current(&r, &s.database).is_none());
        let input = p.input();
        drop(input);
        assert!(p.lease.lock().unwrap().is_none());
    }
    #[test]
    fn completed_input_between_gate_and_identity_check_cannot_reuse_retired_page() {
        let root = root();
        let publisher = Arc::new(Publisher::new(&root));
        let status = status(&root, 1);
        publisher.publish(&status);
        let answer = read_current(&root, &status.database, || {
            let input = publisher.input();
            publisher.invalidate(&status.database);
            let mut updated = status.clone();
            updated.generation = 2;
            publisher.publish(&updated);
            drop(input);
            assert!(publisher.gate.as_ref().unwrap().clear());
        })
        .unwrap();
        assert!(answer.is_none());
        assert_eq!(current(&root, &status.database).unwrap().generation, 2);
    }
    #[test]
    fn incomplete_and_error_states_never_create_current_pages() {
        let r = root();
        let p = Publisher::new(&r);
        for mut s in [
            status(&r, 0),
            status(&r, 1),
            status(&r, 1),
            status(&r, 1),
            status(&r, 1),
        ]
        .into_iter()
        .enumerate()
        {
            match s.0 {
                0 => {}
                1 => s.1.phase = "queued".into(),
                2 => s.1.available = false,
                3 => s.1.error = "analyzer failed".into(),
                4 => s.1.observed_epoch += 1,
                _ => unreachable!(),
            };
            p.publish(&s.1);
            assert!(current(&r, &s.1.database).is_none());
        }
    }
    #[test]
    fn corruption_truncation_oversize_and_wrong_identity_fall_back() {
        let r = root();
        let p = Publisher::new(&r);
        let mut s = status(&r, 1);
        p.publish(&s);
        let path = page_path(&r.join("live"), &s.database);
        let bytes = std::fs::read(&path).unwrap();
        for bad in [
            vec![],
            bytes[..40].to_vec(),
            vec![0; MAX_PAGE as usize + 1],
            {
                let mut b = bytes.clone();
                b[0] ^= 1;
                b
            },
            {
                let mut b = bytes.clone();
                let end = b.len() - 1;
                b[end] ^= 1;
                b
            },
        ] {
            std::fs::write(&path, bad).unwrap();
            assert!(current(&r, &s.database).is_none());
        }
        std::fs::write(&path, bytes).unwrap();
        assert!(current(&r, &s.database).is_some());
        assert!(current(&r, &format!("{}x", s.database)).is_none());
        p.invalidate(&s.database);
        s.generation = 2;
        p.publish(&s);
        assert_eq!(current(&r, &s.database).unwrap().generation, 2);
    }
    #[test]
    fn invalidation_restart_and_lease_loss_during_read_are_rejected() {
        let r = root();
        let p = Publisher::new(&r);
        let mut s = status(&r, 1);
        p.publish(&s);
        let database = s.database.clone();
        assert!(
            read_current(&r, &database, || {
                p.invalidate(&s.database);
                s.generation = 2;
                p.publish(&s);
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(current(&r, &s.database).unwrap().generation, 2);
        // A concurrent process spawn can briefly inherit the same open-file
        // description before close-on-exec. Retirement must revoke that lease
        // even while such a duplicate descriptor still exists.
        let inherited = p
            .lease
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_clone()
            .unwrap();
        assert!(
            read_current(&r, &s.database, || p.disable())
                .unwrap()
                .is_none()
        );
        // Older readers also fail closed through the original lock check.
        assert!(!owned(&File::open(r.join("live/owner")).unwrap()));
        drop(inherited);
        let next = Publisher::new(&r);
        assert!(current(&r, &s.database).is_none());
        next.publish(&s);
        assert!(current(&r, &s.database).is_some());
        let mut replacement = None;
        assert!(
            read_current(&r, &s.database, || {
                let next = Publisher::new(&r);
                next.publish(&s);
                replacement = Some(next);
            })
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn owner_pid_start_identity_and_unknown_versions_are_rejected() {
        let r = root();
        let p = Publisher::new(&r);
        let s = status(&r, 1);
        p.publish(&s);
        let owner_path = r.join("live/owner");
        let original = std::fs::read(&owner_path).unwrap();
        for change in [0, 1, 2, 3, 4, 5, 6] {
            let mut owner: Owner = serde_json::from_slice(
                &original
                    [76..76 + u32::from_le_bytes(original[72..76].try_into().unwrap()) as usize],
            )
            .unwrap();
            match change {
                0 => owner.start_usec += 1,
                1 => owner.version += 1,
                2 => owner.protocol += 1,
                3 => owner.analysis_settings_version += 1,
                4 => owner.analysis_settings_version = 0,
                5 => owner.resource_control_version += 1,
                6 => owner.resource_control_version = 1,
                _ => unreachable!(),
            };
            let body = serde_json::to_vec(&owner).unwrap();
            let mut bytes = original.clone();
            bytes[72..76].copy_from_slice(&(body.len() as u32).to_le_bytes());
            bytes[76..76 + body.len()].copy_from_slice(&body);
            bytes[76 + body.len()..76 + body.len() + 32].copy_from_slice(&Sha256::digest(&body));
            use std::os::unix::fs::FileExt;
            OpenOptions::new()
                .write(true)
                .open(&owner_path)
                .unwrap()
                .write_all_at(&bytes[64..], 64)
                .unwrap();
            assert!(current(&r, &s.database).is_none());
        }
        use std::os::unix::fs::FileExt;
        OpenOptions::new()
            .write(true)
            .open(owner_path)
            .unwrap()
            .write_all_at(&original[64..], 64)
            .unwrap();
        assert!(current(&r, &s.database).is_some());
    }
    #[test]
    fn older_owner_without_activity_capability_retains_rpc_checkpoint_policy() {
        let r = root();
        let p = Publisher::new(&r);
        let s = status(&r, 1);
        p.publish(&s);
        assert!(certified(&r, &s.database).unwrap().activity_hints);
        let path = r.join("live/owner");
        let mut original = std::fs::read(&path).unwrap();
        let length = u32::from_le_bytes(original[72..76].try_into().unwrap()) as usize;
        let mut owner: serde_json::Value =
            serde_json::from_slice(&original[76..76 + length]).unwrap();
        owner.as_object_mut().unwrap().remove("activity_hints");
        let body = serde_json::to_vec(&owner).unwrap();
        original[72..76].copy_from_slice(&(body.len() as u32).to_le_bytes());
        original[76..76 + body.len()].copy_from_slice(&body);
        original[76 + body.len()..76 + body.len() + 32].copy_from_slice(&Sha256::digest(&body));
        use std::os::unix::fs::FileExt;
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .write_all_at(&original[64..], 64)
            .unwrap();
        let certificate = certified(&r, &s.database).unwrap();
        assert!(!certificate.activity_hints);
        assert_eq!(certificate.status, s);
    }
    #[test]
    fn lease_child() {
        let Some(path) = std::env::var_os("KC_LIVE_CHILD") else {
            return;
        };
        let r = PathBuf::from(path);
        let p = Publisher::new(&r);
        let s = status(&r, 7);
        p.publish(&s);
        println!("READY");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    }
    #[test]
    fn killed_and_stopped_publishers_cannot_certify_current() {
        let r = root();
        let s = status(&r, 7);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "live::tests::lease_child", "--nocapture"])
            .env("KC_LIVE_CHILD", &r)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        while !lines.next().unwrap().unwrap().contains("READY") {}
        assert!(current(&r, &s.database).is_some());
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGSTOP) }, 0);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while current(&r, &s.database).is_some() {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGCONT) }, 0);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(current(&r, &s.database).is_none());
    }
}
