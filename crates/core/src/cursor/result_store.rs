//! Coordinator-owned opaque result files. Quota and cursor lifetime belong to
//! the caller; all calls must be serialized under its ownership fence.
use super::{MAX_RESULT_BYTES, MAX_RESULT_CHUNK};
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString, OsString},
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::MetadataExt,
        },
    },
    path::{Component, Path, PathBuf},
};

const MAX_CLEANUP_ENTRIES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredResult {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}
struct Upload {
    expected: u64,
    written: u64,
    identity: Identity,
}
struct Ready {
    result: StoredResult,
    identity: Identity,
}

/// `open` never restores old cursors or silently clears files. After acquiring
/// coordinator ownership, call `reset` before admitting a new incarnation.
pub struct ResultStore {
    directory: PathBuf,
    handle: File,
    identity: Identity,
    uploads: BTreeMap<String, Upload>,
    ready: BTreeMap<String, Ready>,
}

fn invalid(message: &str) -> Error {
    Error::InvalidRequest(message.into())
}
fn unsafe_file(message: &str) -> Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message).into()
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn key(value: &str) -> Result<()> {
    if hex(value, 32) {
        Ok(())
    } else {
        Err(invalid(
            "cursor result key must be 32 lowercase hexadecimal characters",
        ))
    }
}
fn name(key: &str, ready: bool) -> CString {
    CString::new(format!("{key}.{}", if ready { "ready" } else { "part" }))
        .expect("validated ASCII key")
}
fn open_at(directory: RawFd, path: &CStr, flags: libc::c_int, mode: libc::c_uint) -> Result<File> {
    // SAFETY: directory is a live descriptor; path is NUL terminated. Successful
    // openat returns a new descriptor transferred to exactly one File owner.
    let fd = unsafe {
        libc::openat(
            directory,
            path.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn open_directory(path: &Path) -> Result<(PathBuf, File)> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(invalid(
            "cursor result directory cannot contain parent traversal",
        ));
    }
    let parts: Vec<_> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return Err(invalid(
            "cursor result directory must be a dedicated directory",
        ));
    }
    let mut handle = File::open("/")?;
    for part in parts {
        let part = CString::new(part.as_bytes())
            .map_err(|_| invalid("cursor result directory contains NUL"))?;
        handle = match open_at(
            handle.as_raw_fd(),
            &part,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        ) {
            Ok(next) => next,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                // SAFETY: the parent descriptor and component are valid. An
                // existing racing entry is opened again with O_NOFOLLOW.
                if unsafe { libc::mkdirat(handle.as_raw_fd(), part.as_ptr(), 0o700) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error.into());
                    }
                }
                open_at(
                    handle.as_raw_fd(),
                    &part,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                    0,
                )?
            }
            Err(error) => return Err(error),
        };
    }
    let metadata = handle.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "cursor result directory must be owned by this user with mode 0700",
        )
        .into());
    }
    Ok((path, handle))
}
fn regular(file: &File) -> Result<Metadata> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(unsafe_file(
            "cursor result entry must be a private, singly linked regular file owned by this user",
        ));
    }
    Ok(metadata)
}

impl ResultStore {
    pub fn open(directory: &Path) -> Result<Self> {
        let (directory, handle) = open_directory(directory)?;
        let identity = Identity::of(&handle.metadata()?);
        Ok(Self {
            directory,
            handle,
            identity,
            uploads: BTreeMap::new(),
            ready: BTreeMap::new(),
        })
    }
    fn check_directory(&self) -> Result<()> {
        let metadata = std::fs::symlink_metadata(&self.directory)?;
        if !metadata.is_dir()
            || Identity::of(&metadata) != self.identity
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(unsafe_file(
                "cursor result directory changed or is no longer private",
            ));
        }
        Ok(())
    }
    fn file(&self, key: &str, ready: bool, write: bool) -> Result<File> {
        let flags = if write { libc::O_RDWR } else { libc::O_RDONLY };
        let file = open_at(
            self.handle.as_raw_fd(),
            &name(key, ready),
            flags | libc::O_NONBLOCK,
            0,
        )?;
        regular(&file)?;
        Ok(file)
    }
    pub fn begin(&mut self, id: &str, expected_bytes: u64) -> Result<()> {
        key(id)?;
        self.check_directory()?;
        if expected_bytes > MAX_RESULT_BYTES {
            return Err(invalid("saved cursor result exceeds the 16 MiB file guard"));
        }
        if self.ready.contains_key(id) {
            return Err(invalid("cursor result is already complete"));
        }
        if let Some(upload) = self.uploads.get(id) {
            if upload.expected != expected_bytes {
                return Err(invalid("conflicting cursor result length"));
            }
            let file = self.file(id, false, false)?;
            if Identity::of(&file.metadata()?) != upload.identity
                || file.metadata()?.len() != upload.written
            {
                return Err(unsafe_file("cursor upload changed outside its owner"));
            }
            return Ok(());
        }
        // A stale completed entry must never be replaced by a new upload.
        match self.file(id, true, false) {
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(invalid(
                    "completed cursor result already exists; cleanup is required",
                ));
            }
            Err(error) => return Err(error),
        }
        let file = open_at(
            self.handle.as_raw_fd(),
            &name(id, false),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        // Normalize a restrictive inherited umask without widening access.
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let metadata = regular(&file)?;
        self.uploads.insert(
            id.into(),
            Upload {
                expected: expected_bytes,
                written: 0,
                identity: Identity::of(&metadata),
            },
        );
        Ok(())
    }
    pub fn append(&mut self, id: &str, offset: u64, bytes: &[u8]) -> Result<u64> {
        key(id)?;
        self.check_directory()?;
        if bytes.len() > MAX_RESULT_CHUNK {
            return Err(invalid("cursor result chunk exceeds 64 KiB"));
        }
        let upload = self.uploads.get(id).ok_or(Error::CursorInvalidated)?;
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("cursor result offset overflow"))?;
        if end > upload.expected || offset > upload.written {
            return Err(invalid(
                "cursor result chunk is out of order or exceeds its declared length",
            ));
        }
        let mut file = self.file(id, false, true)?;
        let metadata = file.metadata()?;
        if Identity::of(&metadata) != upload.identity || metadata.len() != upload.written {
            return Err(unsafe_file("cursor upload changed outside its owner"));
        }
        file.seek(SeekFrom::Start(offset))?;
        if offset < upload.written {
            if end > upload.written {
                return Err(invalid("cursor result retry overlaps uncommitted data"));
            }
            let mut existing = vec![0; bytes.len()];
            file.read_exact(&mut existing)?;
            if existing != bytes {
                return Err(invalid("cursor result retry conflicts with accepted data"));
            }
            return Ok(upload.written);
        }
        file.write_all(bytes)?;
        self.uploads.get_mut(id).expect("checked upload").written = end;
        Ok(end)
    }
    pub fn complete(&mut self, id: &str, sha256: &str) -> Result<StoredResult> {
        self.complete_with(id, sha256, || Ok(()))
    }
    /// The coordinator supplies its shared deadline/cancellation check. Called
    /// between bounded reads and immediately before immutable publication.
    pub fn complete_with(
        &mut self,
        id: &str,
        sha256: &str,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<StoredResult> {
        check()?;
        key(id)?;
        self.check_directory()?;
        if !hex(sha256, 64) {
            return Err(invalid(
                "cursor result SHA-256 must be 64 lowercase hexadecimal characters",
            ));
        }
        if let Some(ready) = self.ready.get(id) {
            if ready.result.sha256 != sha256 {
                return Err(invalid(
                    "cursor result completion hash conflicts with the completed file",
                ));
            }
            return self.get(id);
        }
        let upload = self.uploads.get(id).ok_or(Error::CursorInvalidated)?;
        if upload.written != upload.expected {
            return Err(invalid("cursor result is incomplete"));
        }
        let mut file = self.file(id, false, false)?;
        let metadata = file.metadata()?;
        if Identity::of(&metadata) != upload.identity || metadata.len() != upload.expected {
            return Err(unsafe_file("cursor upload changed outside its owner"));
        }
        let mut digest = Sha256::new();
        let mut buffer = [0u8; MAX_RESULT_CHUNK];
        let mut left = upload.expected;
        while left > 0 {
            check()?;
            let count = file.read(
                &mut buffer
                    [..usize::try_from(left.min(MAX_RESULT_CHUNK as u64)).expect("bounded chunk")],
            )?;
            if count == 0 {
                return Err(unsafe_file(
                    "cursor result ended before its declared length",
                ));
            }
            digest.update(&buffer[..count]);
            left -= count as u64;
        }
        if format!("{:x}", digest.finalize()) != sha256 {
            return Err(invalid("cursor result SHA-256 mismatch"));
        }
        if file.metadata()?.len() != upload.expected {
            return Err(unsafe_file(
                "cursor upload length changed during completion",
            ));
        }
        check()?;
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o400) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let from = name(id, false);
        let to = name(id, true);
        check()?;
        // SAFETY: both names are validated single components, relative to the
        // owned directory. Exclusive rename cannot replace a completed file.
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                self.handle.as_raw_fd(),
                from.as_ptr(),
                self.handle.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                self.handle.as_raw_fd(),
                from.as_ptr(),
                self.handle.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let result = return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "exclusive cursor result publication is unavailable on this platform",
        )
        .into());
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let stored = StoredResult {
            path: self.directory.join(to.to_str().expect("ASCII filename")),
            bytes: upload.expected,
            sha256: sha256.into(),
        };
        let identity = upload.identity;
        self.uploads.remove(id);
        self.ready.insert(
            id.into(),
            Ready {
                result: stored.clone(),
                identity,
            },
        );
        Ok(stored)
    }
    pub fn get(&self, id: &str) -> Result<StoredResult> {
        key(id)?;
        self.check_directory()?;
        let ready = self.ready.get(id).ok_or(Error::CursorInvalidated)?;
        let file = match self.file(id, true, false) {
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::CursorInvalidated);
            }
            other => other?,
        };
        let metadata = file.metadata()?;
        if Identity::of(&metadata) != ready.identity
            || metadata.len() != ready.result.bytes
            || metadata.mode() & 0o777 != 0o400
        {
            return Err(unsafe_file(
                "completed cursor result changed outside its owner",
            ));
        }
        Ok(ready.result.clone())
    }
    fn unlink(&self, filename: &CStr) -> Result<()> {
        // Cleanup must also work for owned regular files that lost read bits.
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: metadata is writable C storage; directory/name are live and
        // AT_SYMLINK_NOFOLLOW inspects a link rather than its target.
        if unsafe {
            libc::fstatat(
                self.handle.as_raw_fd(),
                filename.as_ptr(),
                &mut metadata,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(error.into());
        }
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG
            || metadata.st_nlink != 1
            || metadata.st_uid != unsafe { libc::geteuid() }
            || metadata.st_mode & 0o077 != 0
        {
            return Err(unsafe_file(
                "cursor result cleanup entry must be a private singly linked regular file owned by this user",
            ));
        }
        // SAFETY: a checked regular entry is unlinked relative to the owned
        // directory; no traversal or symlink target is followed.
        if unsafe { libc::unlinkat(self.handle.as_raw_fd(), filename.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    pub fn remove(&mut self, id: &str) -> Result<()> {
        key(id)?;
        self.check_directory()?;
        self.unlink(&name(id, false))?;
        self.unlink(&name(id, true))?;
        self.uploads.remove(id);
        self.ready.remove(id);
        Ok(())
    }
    pub fn reset(&mut self) -> Result<()> {
        self.check_directory()?;
        let names = self.entries()?;
        for filename in names {
            let text = filename
                .to_str()
                .ok_or_else(|| unsafe_file("foreign entry in cursor result directory"))?;
            let id = text
                .strip_suffix(".part")
                .or_else(|| text.strip_suffix(".ready"))
                .filter(|id| hex(id, 32))
                .ok_or_else(|| unsafe_file("foreign entry in cursor result directory"))?;
            key(id)?;
            self.unlink(&CString::new(filename.as_bytes()).expect("directory entry has no NUL"))?;
        }
        self.uploads.clear();
        self.ready.clear();
        Ok(())
    }
    fn entries(&self) -> Result<BTreeSet<OsString>> {
        let file = open_at(
            self.handle.as_raw_fd(),
            c".",
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        use std::os::fd::IntoRawFd;
        let fd = file.into_raw_fd();
        // SAFETY: fdopendir takes ownership on success; failure leaves fd to
        // close below. DIR is used only on this thread and closed by the guard.
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(error.into());
        }
        struct Directory(*mut libc::DIR);
        impl Drop for Directory {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let directory = Directory(raw);
        let mut names = BTreeSet::new();
        loop {
            // POSIX readdir distinguishes EOF from failure using errno.
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0;
            }
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0;
            }
            let entry = unsafe { libc::readdir(directory.0) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) != 0 {
                    return Err(error.into());
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if names.len() == MAX_CLEANUP_ENTRIES {
                return Err(Error::ResourceLimited(
                    "cursor result cleanup directory exceeds its bounded entry scan".into(),
                ));
            }
            names.insert(OsString::from_vec(bytes.to_vec()));
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    const A: &str = "00000000000000000000000000000001";
    const B: &str = "ffffffffffffffffffffffffffffffff";
    fn directory() -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(".artifacts/result-file-tests")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ))
            .join("files")
    }
    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn finish(store: &mut ResultStore, id: &str, bytes: &[u8]) -> StoredResult {
        store.begin(id, bytes.len() as u64).unwrap();
        store.append(id, 0, bytes).unwrap();
        store.complete(id, &hash(bytes)).unwrap()
    }

    #[test]
    fn completed_payload_is_immutable_idempotent_and_open_readers_survive_unlink() {
        let path = directory();
        let mut store = ResultStore::open(&path).unwrap();
        let bytes = b"saved \xf0\x9f\x98\x80 code context";
        store.begin(A, bytes.len() as u64).unwrap();
        store.begin(A, bytes.len() as u64).unwrap();
        assert!(matches!(store.get(A), Err(Error::CursorInvalidated)));
        assert!(!path.join(format!("{A}.ready")).exists());
        assert_eq!(store.append(A, 0, &bytes[..6]).unwrap(), 6);
        assert_eq!(store.append(A, 0, &bytes[..6]).unwrap(), 6);
        assert_eq!(store.append(A, 6, &bytes[6..]).unwrap(), bytes.len() as u64);
        let stored = store.complete(A, &hash(bytes)).unwrap();
        assert_eq!(store.complete(A, &hash(bytes)).unwrap(), stored);
        assert_eq!(store.get(A).unwrap(), stored);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&stored.path).unwrap().permissions().mode() & 0o777,
            0o400
        );
        assert!(!path.join(format!("{A}.part")).exists());
        assert!(store.begin(A, bytes.len() as u64).is_err());
        assert!(store.append(A, 0, b"change").is_err());
        assert!(store.complete(A, &hash(b"different")).is_err());
        assert_eq!(fs::read(&stored.path).unwrap(), bytes);
        let mut reader = File::open(&stored.path).unwrap();
        store.remove(A).unwrap();
        store.remove(A).unwrap();
        assert!(!stored.path.exists());
        assert!(matches!(store.get(A), Err(Error::CursorInvalidated)));
        let mut retained = Vec::new();
        reader.read_to_end(&mut retained).unwrap();
        assert_eq!(retained, bytes);
    }

    #[test]
    fn bounds_and_retry_conflicts_do_not_change_accepted_bytes() {
        let path = directory();
        let mut store = ResultStore::open(&path).unwrap();
        assert!(store.begin(A, MAX_RESULT_BYTES + 1).is_err());
        store.begin(A, MAX_RESULT_BYTES).unwrap();
        assert_eq!(
            fs::metadata(path.join(format!("{A}.part"))).unwrap().len(),
            0
        );
        store.remove(A).unwrap();
        store.begin(A, 6).unwrap();
        assert!(store.begin(A, 7).is_err());
        assert!(store.append(A, 1, b"a").is_err());
        assert!(store.append(A, u64::MAX, b"a").is_err());
        assert!(store.append(A, 0, &vec![0; MAX_RESULT_CHUNK + 1]).is_err());
        store.append(A, 0, b"abc").unwrap();
        assert!(store.append(A, 1, b"bcd").is_err());
        assert!(store.append(A, 0, b"bad").is_err());
        assert!(store.append(A, 3, b"defg").is_err());
        assert!(store.complete(A, &hash(b"abc")).is_err());
        assert_eq!(fs::read(path.join(format!("{A}.part"))).unwrap(), b"abc");
        store.append(A, 3, b"def").unwrap();
        assert!(store.complete(A, &hash(b"wrong")).is_err());
        assert!(!path.join(format!("{A}.ready")).exists());
        let stored = store.complete(A, &hash(b"abcdef")).unwrap();
        assert_eq!(fs::read(stored.path).unwrap(), b"abcdef");
        store.begin(B, 0).unwrap();
        assert_eq!(store.complete(B, &hash(b"")).unwrap().bytes, 0);
    }

    #[test]
    fn completion_hashes_actual_bytes_and_never_replaces_a_ready_entry() {
        let path = directory();
        let mut store = ResultStore::open(&path).unwrap();
        store.begin(A, 3).unwrap();
        store.append(A, 0, b"abc").unwrap();
        fs::write(path.join(format!("{A}.part")), b"xyz").unwrap();
        assert!(store.complete(A, &hash(b"abc")).is_err());
        assert!(!path.join(format!("{A}.ready")).exists());
        assert!(store.append(A, 0, b"abc").is_err());
        store.remove(A).unwrap();
        store.begin(A, 3).unwrap();
        store.append(A, 0, b"abc").unwrap();
        let occupied = path.join(format!("{A}.ready"));
        fs::write(&occupied, b"existing").unwrap();
        fs::set_permissions(&occupied, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(store.complete(A, &hash(b"abc")).is_err());
        assert_eq!(fs::read(&occupied).unwrap(), b"existing");
        assert!(path.join(format!("{A}.part")).exists());
        assert!(matches!(store.get(A), Err(Error::CursorInvalidated)));
    }

    #[test]
    fn unsafe_names_directories_symlinks_and_hardlinks_are_rejected() {
        let path = directory();
        let mut store = ResultStore::open(&path).unwrap();
        let base = path.parent().unwrap();
        let index = base.join("index.db");
        fs::write(&index, b"unrelated index").unwrap();
        for id in [
            "",
            "../index.db",
            "A0000000000000000000000000000000",
            "0000000000000000000000000000000/",
            "0000000000000000000000000000000\0",
        ] {
            assert!(store.begin(id, 0).is_err());
            assert!(store.append(id, 0, b"").is_err());
            assert!(store.complete(id, &hash(b"")).is_err());
            assert!(store.get(id).is_err());
            assert!(store.remove(id).is_err());
        }
        let alias = base.join("alias");
        symlink(&path, &alias).unwrap();
        assert!(ResultStore::open(&alias).is_err());
        assert!(ResultStore::open(&alias.join("nested")).is_err());
        assert!(!path.join("nested").exists());
        assert!(ResultStore::open(&path.join("../escape")).is_err());
        assert!(!base.join("escape").exists());
        store.begin(A, 3).unwrap();
        let part = path.join(format!("{A}.part"));
        fs::remove_file(&part).unwrap();
        symlink(&index, &part).unwrap();
        assert!(store.append(A, 0, b"bad").is_err());
        assert!(store.remove(A).is_err());
        assert!(store.reset().is_err());
        assert_eq!(fs::read(&index).unwrap(), b"unrelated index");
        fs::remove_file(&part).unwrap();
        store.remove(A).unwrap();
        store.begin(A, 3).unwrap();
        store.append(A, 0, b"abc").unwrap();
        let linked = base.join("linked");
        fs::hard_link(&part, &linked).unwrap();
        assert!(store.complete(A, &hash(b"abc")).is_err());
        assert!(store.remove(A).is_err());
        fs::remove_file(linked).unwrap();
        store.remove(A).unwrap();
        let stored = finish(&mut store, A, b"owned");
        fs::remove_file(&stored.path).unwrap();
        symlink(&index, &stored.path).unwrap();
        assert!(store.get(A).is_err());
        assert!(store.remove(A).is_err());
        assert!(store.reset().is_err());
        fs::remove_file(stored.path).unwrap();
        store.remove(A).unwrap();
        assert_eq!(fs::read(index).unwrap(), b"unrelated index");
    }

    #[test]
    fn cleanup_failure_retains_state_and_does_not_follow_nonregular_entries() {
        let path = directory();
        let mut store = ResultStore::open(&path).unwrap();
        let stored = finish(&mut store, A, b"ready");
        store.begin(B, 1).unwrap();
        let bad = path.join(format!("{B}.part"));
        fs::remove_file(&bad).unwrap();
        let bad_name = CString::new(bad.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(bad_name.as_ptr(), 0o600) }, 0);
        assert!(store.append(B, 0, b"x").is_err());
        assert!(store.remove(B).is_err());
        assert!(store.reset().is_err());
        assert!(!stored.path.exists());
        assert!(store.ready.contains_key(A) && store.uploads.contains_key(B));
        assert!(matches!(store.get(A), Err(Error::CursorInvalidated)));
        fs::remove_file(bad).unwrap();
        store.reset().unwrap();
        assert!(store.ready.is_empty() && store.uploads.is_empty());
        let stored = finish(&mut store, A, b"next");
        fs::remove_file(&stored.path).unwrap();
        fs::create_dir(&stored.path).unwrap();
        assert!(store.get(A).is_err());
        assert!(store.remove(A).is_err());
        assert!(store.reset().is_err());
        fs::remove_dir(stored.path).unwrap();
        store.reset().unwrap();
        let unreadable = finish(&mut store, A, b"cleanup");
        fs::set_permissions(&unreadable.path, fs::Permissions::from_mode(0o0)).unwrap();
        store.remove(A).unwrap();
        assert!(!unreadable.path.exists());
    }

    #[test]
    fn restart_cleanup_is_explicit_and_preserves_foreign_files() {
        let path = directory();
        let mut first = ResultStore::open(&path).unwrap();
        first.begin(A, 10).unwrap();
        first.append(A, 0, b"partial").unwrap();
        finish(&mut first, B, b"completed");
        drop(first);
        let mut next = ResultStore::open(&path).unwrap();
        assert!(matches!(next.get(B), Err(Error::CursorInvalidated)));
        assert!(next.begin(B, 9).is_err());
        let foreign = path.join("index.db");
        fs::write(&foreign, b"keep me").unwrap();
        assert!(next.reset().is_err());
        assert_eq!(fs::read(&foreign).unwrap(), b"keep me");
        fs::rename(&foreign, path.parent().unwrap().join("preserved-index.db")).unwrap();
        next.reset().unwrap();
        assert!(fs::read_dir(&path).unwrap().next().is_none());
        next.begin(A, 0).unwrap();
        next.reset().unwrap();
    }

    #[test]
    fn interrupted_hashing_never_publishes_and_can_be_retried() {
        let path = directory();
        let mut store = ResultStore::open(&path).unwrap();
        let bytes = vec![17; MAX_RESULT_CHUNK * 2];
        store.begin(A, bytes.len() as u64).unwrap();
        store.append(A, 0, &bytes[..MAX_RESULT_CHUNK]).unwrap();
        store
            .append(A, MAX_RESULT_CHUNK as u64, &bytes[MAX_RESULT_CHUNK..])
            .unwrap();
        let mut checks = 0;
        let error = store.complete_with(A, &hash(&bytes), || {
            checks += 1;
            if checks == 3 {
                Err(Error::DeadlineExceeded)
            } else {
                Ok(())
            }
        });
        assert!(matches!(error, Err(Error::DeadlineExceeded)));
        assert_eq!(checks, 3);
        assert!(!path.join(format!("{A}.ready")).exists());
        assert_eq!(
            fs::metadata(path.join(format!("{A}.part")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            store.complete(A, &hash(&bytes)).unwrap().bytes,
            bytes.len() as u64
        );
    }

    #[test]
    fn directory_replacement_is_rejected_and_reset_scan_is_bounded() {
        let path = directory();
        let mut store = ResultStore::open(&path).unwrap();
        let stored = finish(&mut store, A, b"held");
        let held = path.parent().unwrap().join("held");
        fs::rename(&path, &held).unwrap();
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(store.get(A).is_err());
        assert!(store.remove(A).is_err());
        assert!(store.reset().is_err());
        assert_eq!(
            fs::read(held.join(stored.path.file_name().unwrap())).unwrap(),
            b"held"
        );
        let mut current = ResultStore::open(&path).unwrap();
        current.begin(A, 1).unwrap();
        for i in 0..MAX_CLEANUP_ENTRIES {
            File::create(path.join(format!("foreign-{i}"))).unwrap();
        }
        assert!(matches!(current.reset(), Err(Error::ResourceLimited(_))));
        assert!(path.join(format!("{A}.part")).exists());
        assert!(current.uploads.contains_key(A));
    }
}
