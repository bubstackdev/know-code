use crate::{
    Error, Result,
    protocol::{self, *},
};
use std::{
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

pub fn data_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("KNOW_CODE_DATA_DIR") {
        let path = PathBuf::from(path);
        return Ok(if path.is_absolute() {
            path
        } else {
            std::env::current_dir()?.join(path)
        });
    }
    Ok(PathBuf::from(std::env::var_os("HOME").ok_or_else(|| {
        Error::InvalidRequest("HOME is required to locate per-user state".into())
    })?)
    .join("Library/Application Support/Know Code"))
}
pub fn socket_path(root: &Path) -> PathBuf {
    root.join("c.sock")
}
/// An explicit DATA override designates a dedicated per-user state directory.
/// Tighten only its owned leaf, preserving caller path spelling and aliases.
pub fn prepare_data_root(root: &Path) -> Result<()> {
    let configured_home = std::env::var_os("HOME").map(PathBuf::from);
    let user = unsafe { libc::geteuid() };
    let actual_home = user_home(user)?;
    prepare_data_root_owned(root, user, &actual_home, configured_home.as_deref())
}
fn user_home(user: libc::uid_t) -> Result<PathBuf> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};
    // Directory-service records can exceed the usual suggested size. Bound
    // both allocation and retries; an unavailable account never permits chmod.
    let mut size = 16 * 1024;
    loop {
        let mut buffer = vec![0u8; size];
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        // SAFETY: all outputs and the backing buffer remain live until pw_dir
        // has been copied. getpwuid_r initializes record only on success.
        let status = unsafe {
            libc::getpwuid_r(
                user,
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE && size < 1024 * 1024 {
            size *= 2;
            continue;
        }
        if status != 0 || found.is_null() {
            return Err(Error::InvalidRequest(format!(
                "cannot resolve effective-user home safely (getpwuid_r status {status}); restore the local account record before using coordinator DATA"
            )));
        }
        let record = unsafe { record.assume_init() };
        if record.pw_dir.is_null() {
            return Err(Error::InvalidRequest(
                "effective-user account has no home directory; cannot secure coordinator DATA"
                    .into(),
            ));
        }
        // SAFETY: successful getpwuid_r supplies a NUL-terminated pw_dir in
        // the still-live backing buffer.
        let bytes = unsafe { CStr::from_ptr(record.pw_dir) }.to_bytes();
        let home = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
        if !home.is_absolute() {
            return Err(Error::InvalidRequest(
                "effective-user account home must be absolute; cannot secure coordinator DATA"
                    .into(),
            ));
        }
        return Ok(home);
    }
}
fn prepare_data_root_owned(
    root: &Path,
    expected: libc::uid_t,
    actual_home: &Path,
    configured_home: Option<&Path>,
) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    let actual_home = std::fs::canonicalize(actual_home)?;
    let configured_home = configured_home.map(std::fs::canonicalize).transpose()?;
    let absolute = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    let git_metadata = |path: &Path| {
        path.components().any(|part| {
            part.as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(b".git")
        })
    };
    // Resolve only the nearest existing ancestor before creating a new leaf,
    // so an alias into Git metadata cannot create state there before refusal.
    for ancestor in absolute.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(existing) => {
                if git_metadata(&existing) {
                    return Err(Error::InvalidRequest(
                        "DATA cannot use Git metadata directories".into(),
                    ));
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)?;
    let canonical = std::fs::canonicalize(root)?;
    if canonical.parent().is_none()
        || actual_home.starts_with(&canonical)
        || configured_home
            .as_ref()
            .is_some_and(|path| path.starts_with(&canonical))
        || git_metadata(&canonical)
    {
        return Err(Error::InvalidRequest("DATA must be a dedicated state directory, not HOME, its ancestor, filesystem root or Git metadata".into()));
    }
    match std::fs::symlink_metadata(canonical.join(".git")) {
        Ok(_) => {
            return Err(Error::InvalidRequest(
                "DATA must be outside source repository roots; choose a dedicated state directory"
                    .into(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&canonical)?;
    let owned = directory.metadata()?;
    if !owned.is_dir() || owned.uid() != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "coordinator DATA is not a directory owned by the effective user",
        )
        .into());
    }
    let same_directory = || -> Result<()> {
        let current = std::fs::symlink_metadata(&canonical)?;
        let alias = std::fs::metadata(root)?;
        if !current.is_dir()
            || current.uid() != expected
            || (current.dev(), current.ino()) != (owned.dev(), owned.ino())
            || (alias.dev(), alias.ino()) != (owned.dev(), owned.ino())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "coordinator DATA directory changed during ownership validation",
            )
            .into());
        }
        Ok(())
    };
    same_directory()?;
    if owned.mode() & 0o7777 != 0o700 {
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    clear_data_acl(&directory)?;
    same_directory()?;
    Ok(())
}
#[cfg(target_os = "macos")]
fn clear_data_acl(directory: &std::fs::File) -> Result<()> {
    data_acl::clear(directory).map_err(|error| {
        std::io::Error::new(error.kind(), format!("cannot remove extended access from owned coordinator DATA: {error}; secure this dedicated directory before retrying")).into()
    })
}
#[cfg(not(target_os = "macos"))]
fn clear_data_acl(_: &std::fs::File) -> Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "securing coordinator DATA ACLs requires supported macOS APIs",
    )
    .into())
}
// The Darwin extended-ACL ABI is declared by the installed SDK sys/acl.h;
// libc does not expose these functions. All file changes use the owned fd.
#[cfg(target_os = "macos")]
mod data_acl {
    use std::{io, os::fd::AsRawFd};
    const EXTENDED: libc::c_int = 0x100;
    unsafe extern "C" {
        fn acl_init(count: libc::c_int) -> *mut libc::c_void;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_set_fd_np(fd: libc::c_int, acl: *mut libc::c_void, kind: libc::c_int)
        -> libc::c_int;
        fn acl_valid(acl: *mut libc::c_void) -> libc::c_int;
        fn acl_get_entry(
            acl: *mut libc::c_void,
            entry: libc::c_int,
            output: *mut *mut libc::c_void,
        ) -> libc::c_int;
        #[cfg(test)]
        fn acl_to_text(acl: *mut libc::c_void, length: *mut libc::ssize_t) -> *mut libc::c_char;
    }
    pub(super) struct Acl(*mut libc::c_void);
    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: Acl owns exactly one successful ACL allocation.
            unsafe {
                acl_free(self.0);
            }
        }
    }
    impl Acl {
        fn allocated(raw: *mut libc::c_void) -> io::Result<Self> {
            if raw.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self(raw))
            }
        }
        pub(super) fn get(file: &std::fs::File) -> io::Result<Self> {
            Self::get_fd(file.as_raw_fd())
        }
        fn get_fd(fd: libc::c_int) -> io::Result<Self> {
            // SAFETY: the kernel validates fd; successful allocation is owned.
            let raw = unsafe { acl_get_fd_np(fd, EXTENDED) };
            if raw.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ENOENT) {
                    return Err(error);
                }
                // Darwin represents an absent extended ACL as ENOENT, also
                // immediately after successfully setting an empty ACL. Only
                // that exact absence becomes an owned empty representation.
                return Self::allocated(unsafe { acl_init(0) });
            }
            Ok(Self(raw))
        }
        pub(super) fn set(&self, file: &std::fs::File) -> io::Result<()> {
            // SAFETY: live fd and ACL; the kernel copies rather than takes it.
            if unsafe { acl_set_fd_np(file.as_raw_fd(), self.0, EXTENDED) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        pub(super) fn has_entries(&self) -> io::Result<bool> {
            // Darwin returns 0 for a found entry, -1/EINVAL for no first entry.
            // Validate first, so an invalid ACL is never mistaken for empty.
            if unsafe { acl_valid(self.0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut entry = std::ptr::null_mut();
            if unsafe { acl_get_entry(self.0, 0, &mut entry) } == 0 {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINVAL) {
                Ok(false)
            } else {
                Err(error)
            }
        }
        #[cfg(test)]
        pub(super) fn text(&self) -> io::Result<Vec<u8>> {
            // SAFETY: valid live ACL; returned text is owned until acl_free.
            let text = unsafe { acl_to_text(self.0, std::ptr::null_mut()) };
            if text.is_null() {
                return Err(io::Error::last_os_error());
            }
            let copy = unsafe { std::ffi::CStr::from_ptr(text) }
                .to_bytes()
                .to_vec();
            unsafe {
                acl_free(text.cast());
            }
            Ok(copy)
        }
    }
    #[test]
    fn invalid_descriptor_acl_error_is_not_treated_as_absence() {
        let error = match Acl::get_fd(-1) {
            Ok(_) => panic!("invalid descriptor must not become an empty ACL"),
            Err(error) => error,
        };
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
    }
    pub(super) fn clear(file: &std::fs::File) -> io::Result<()> {
        if !Acl::get(file)?.has_entries()? {
            return Ok(());
        }
        // SAFETY: zero entries requests an owned empty extended ACL.
        let empty = Acl::allocated(unsafe { acl_init(0) })?;
        empty.set(file)?;
        if Acl::get(file)?.has_entries()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "extended ACL remains after removal",
            ));
        }
        Ok(())
    }
}
/// The local coordinator is per effective user, even when a pathname or
/// existing descriptor was supplied through a less-private directory.
pub fn verify_peer(socket: &UnixStream) -> std::io::Result<()> {
    verify_peer_uid(socket, unsafe { libc::geteuid() })
}
fn verify_peer_uid(socket: &UnixStream, expected: libc::uid_t) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let mut user: libc::uid_t = 0;
        let mut group: libc::gid_t = 0;
        // SAFETY: the live socket descriptor and both writable credential
        // outputs belong to this call. Failed lookup never authenticates a peer.
        if unsafe { libc::getpeereid(socket.as_raw_fd(), &mut user, &mut group) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if user != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "coordinator socket peer belongs to a different effective user",
            ));
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (socket, expected);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "coordinator peer verification requires supported macOS getpeereid",
        ))
    }
}
pub fn root_for_database(database: &Path) -> Option<&Path> {
    let workspaces = database.parent()?.parent()?;
    (workspaces.file_name()? == "workspaces")
        .then(|| workspaces.parent())
        .flatten()
}
pub fn request(root: &Path, request: ControlRequest) -> Result<ControlResponse> {
    let timeout = request.wait_ms.saturating_add(500);
    request_with_timeout(root, request, timeout)
}
/// Installer-only bridge for quiescing the known previous lifecycle peer.
/// Ordinary consumers always require the current lifecycle capability.
pub fn request_installer(
    root: &Path,
    mut request: ControlRequest,
    lifecycle_version: u32,
) -> Result<ControlResponse> {
    if !matches!(
        lifecycle_version,
        1 | 2 | 3 | protocol::INDEX_LIFECYCLE_VERSION
    ) || !matches!(
        request.operation.as_str(),
        "status" | "maintenance" | "resume" | "shutdown"
    ) {
        return Err(Error::UpdateRequired(
            "installer control supports only known lifecycle status/quiescence operations".into(),
        ));
    }
    request.index_lifecycle_version = lifecycle_version;
    let timeout = request.wait_ms.saturating_add(500).max(1);
    let mut socket = UnixStream::connect(socket_path(root))?;
    verify_peer(&socket)?;
    socket.set_write_timeout(Some(Duration::from_millis(u64::from(timeout))))?;
    socket.set_read_timeout(Some(Duration::from_millis(u64::from(timeout))))?;
    let response = exchange_raw(&mut socket, request)?;
    if response.index_lifecycle_version != lifecycle_version
        || response.resource_control_version != crate::resource::CONTROL_VERSION
        || response.analysis_settings_version != crate::settings::VERSION
    {
        return Err(Error::UpdateRequired("installed coordinator does not match its declared lifecycle/resource/settings contract".into()));
    }
    checked_error(response)
}

/// Read-only client control sharing the query cancellation/deadline path.
pub fn request_cancellable(
    root: &Path,
    mut request: ControlRequest,
    start: crate::execution::QueryStart,
    cancellation: crate::execution::QueryCancellation,
) -> Result<ControlResponse> {
    if !matches!(request.operation.as_str(), "status" | "wait") {
        return Err(Error::InvalidRequest(
            "cancellable control supports read-only status/wait only".into(),
        ));
    }
    let execution =
        crate::execution::Execution::new(start, None)?.with_cancellation(Some(cancellation));
    let result = (|| {
        let stream = crate::execution::connect(&socket_path(root), execution.clone())?;
        request.wait_ms = request
            .wait_ms
            .min(execution.remaining()?.as_millis().min(u128::from(u32::MAX)) as u32);
        exchange(
            &mut crate::execution::TimedStream::new(stream, execution.clone())?,
            request,
        )
    })();
    execution.check()?;
    result
}
fn request_with_timeout(
    root: &Path,
    request: ControlRequest,
    timeout: u32,
) -> Result<ControlResponse> {
    let mut socket = UnixStream::connect(socket_path(root))?;
    verify_peer(&socket)?;
    socket.set_write_timeout(Some(Duration::from_millis(u64::from(timeout.max(1)))))?;
    socket.set_read_timeout(Some(Duration::from_millis(u64::from(timeout.max(1)))))?;
    exchange(&mut socket, request)
}
fn exchange(
    socket: &mut (impl std::io::Read + std::io::Write),
    request: ControlRequest,
) -> Result<ControlResponse> {
    checked_response(exchange_raw(socket, request)?)
}
pub(crate) fn exchange_raw(
    socket: &mut (impl std::io::Read + std::io::Write),
    mut request: ControlRequest,
) -> Result<ControlResponse> {
    if request.index_lifecycle_version == 0 {
        request.index_lifecycle_version = protocol::INDEX_LIFECYCLE_VERSION;
    }
    protocol::write_frame(
        &mut *socket,
        &Envelope {
            protocol_version: VERSION,
            job_id: String::new(),
            payload: Some(envelope::Payload::ControlRequest(request)),
        },
    )?;
    let response = protocol::read_frame(socket)?
        .ok_or_else(|| Error::Protocol("coordinator closed the control channel".into()))?;
    match response.payload {
        Some(envelope::Payload::ControlResponse(response)) => Ok(response),
        _ => Err(Error::Protocol("unexpected coordinator response".into())),
    }
}
pub(crate) fn checked_response(response: ControlResponse) -> Result<ControlResponse> {
    if response.index_lifecycle_version != protocol::INDEX_LIFECYCLE_VERSION {
        return Err(Error::UpdateRequired("update the running coordinator and native libraries together; index lifecycle safety is unavailable or incompatible".into()));
    }
    checked_error(response)
}
fn checked_error(response: ControlResponse) -> Result<ControlResponse> {
    if response.error_code.is_empty() {
        Ok(response)
    } else if response.error_code == "update_required" {
        Err(Error::UpdateRequired(response.error_message))
    } else {
        Err(Error::Remote {
            code: response.error_code,
            message: response.error_message,
        })
    }
}

// Reuse the existing nonblocking advisory channel even if a query has no
// reporting deadline left. A hint never proves corruption; the latch does.
pub(crate) fn index_fault_hint(database: &Path) {
    if let Some(root) = root_for_database(database) {
        let _ = notify_activity(root, &database.to_string_lossy());
    }
}

// A durable BAD latch is the proof; this bounded RPC is only a prompt for
// the coordinator to observe it. Never turn a reporting error into stale data.
pub(crate) fn report_index_corruption(error: &Error, execution: &crate::execution::Execution) {
    let Error::IndexCorrupt {
        database,
        incarnation,
        message,
    } = error
    else {
        return;
    };
    if incarnation.is_empty() {
        return;
    }
    let Some(root) = root_for_database(database) else {
        return;
    };
    let _ = (|| -> Result<()> {
        let now = crate::execution::continuous_time()?;
        let reporting = execution
            .clone()
            .cap_at(now.saturating_add(Duration::from_millis(100)))?;
        let stream = crate::execution::connect(&socket_path(root), reporting.clone())?;
        let mut end = message.len().min(8192);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        exchange(
            &mut crate::execution::TimedStream::new(stream, reporting)?,
            ControlRequest {
                operation: "index-corrupt".into(),
                database: path_string(database)?,
                corruption: Some(IndexCorruptionReport {
                    incarnation: incarnation.clone(),
                    detail: message[..end].into(),
                }),
                ..Default::default()
            },
        )?;
        Ok(())
    })();
}

/// Workspace control is shared by native libraries and the CLI/MCP host.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRequest {
    pub op: WorkspaceOperation,
    pub workspace_root: Option<PathBuf>,
    pub database: Option<String>,
    pub data_directory: Option<PathBuf>,
    pub coordinator_executable: Option<PathBuf>,
    pub configuration: Option<String>,
    pub wait_ms: Option<u32>,
    pub personal_settings: Option<crate::settings::PersonalSettings>,
    pub grant: Option<bool>,
}
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOperation {
    Open,
    Status,
    Refresh,
    Cancel,
    Wait,
    Settings,
    Configure,
    Trust,
}
#[derive(Debug, serde::Serialize)]
pub struct WorkspaceResult {
    pub data_directory: String,
    pub coordinator_executable: Option<String>,
    pub status: WorkspaceStatus,
}
fn path_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidRequest("workspace/control paths must be UTF-8".into()))
}
fn absolute(path: PathBuf) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
pub fn workspace_operation(request: WorkspaceRequest) -> Result<WorkspaceResult> {
    if request.grant.is_some() && !matches!(request.op, WorkspaceOperation::Trust) {
        return Err(Error::InvalidRequest(
            "grant is only valid for the explicit trust operation".into(),
        ));
    }
    let data = absolute(request.data_directory.map(Ok).unwrap_or_else(data_root)?)?;
    let executable = request
        .coordinator_executable
        .or_else(|| std::env::var_os("KNOW_CODE_COORDINATOR").map(PathBuf::from))
        .map(|p| {
            if !p.is_absolute() && p.components().count() > 1 {
                absolute(p)
            } else {
                Ok(p)
            }
        })
        .transpose()?;
    let status = match request.op {
        WorkspaceOperation::Open => {
            let root = request
                .workspace_root
                .ok_or_else(|| Error::InvalidRequest("open requires workspace_root".into()))?;
            attach_workspace(
                &data,
                &root,
                request.configuration.unwrap_or_default(),
                false,
                executable.as_deref(),
            )?
        }
        operation => {
            let database = request.database.filter(|s| !s.is_empty()).ok_or_else(|| {
                Error::InvalidRequest("workspace operation requires database from open".into())
            })?;
            let wait = request.wait_ms.unwrap_or(500);
            if wait > 120000 {
                return Err(Error::InvalidRequest("wait_ms cannot exceed 120000".into()));
            }
            // Resolve the registered root from coordinator state, not a caller's
            // possibly missing/replaced/symlinked path. Status works offline too.
            match operation {
                WorkspaceOperation::Status => {
                    request_status(&data, &database, executable.as_deref())?
                }
                WorkspaceOperation::Refresh => {
                    ensure_running(&data, executable.as_deref())?;
                    require_analysis_settings(&data)?;
                    request_one(
                        &data,
                        ControlRequest {
                            operation: "refresh".into(),
                            workspace_root: request_status(
                                &data,
                                &database,
                                executable.as_deref(),
                            )?
                            .workspace_root,
                            configuration: request.configuration.unwrap_or_default(),
                            ..Default::default()
                        },
                        executable.as_deref(),
                    )?
                }
                WorkspaceOperation::Cancel => {
                    ensure_running(&data, executable.as_deref())?;
                    require_resource_control(&data)?;
                    request_one(
                        &data,
                        ControlRequest {
                            operation: "cancel".into(),
                            database,
                            ..Default::default()
                        },
                        executable.as_deref(),
                    )?
                }
                WorkspaceOperation::Wait => request_one(
                    &data,
                    ControlRequest {
                        operation: "wait".into(),
                        database,
                        wait_ms: wait,
                        ..Default::default()
                    },
                    executable.as_deref(),
                )?,
                WorkspaceOperation::Trust => {
                    let grant = request.grant.ok_or_else(|| {
                        Error::InvalidRequest("trust requires an explicit grant boolean".into())
                    })?;
                    request_one(
                        &data,
                        ControlRequest {
                            operation: "trust".into(),
                            database,
                            settings_json: Some(serde_json::to_string(&grant)?),
                            ..Default::default()
                        },
                        executable.as_deref(),
                    )?
                }
                WorkspaceOperation::Settings | WorkspaceOperation::Configure => {
                    let settings_json = match operation {
                        WorkspaceOperation::Configure => Some(serde_json::to_string(
                            request.personal_settings.as_ref().ok_or_else(|| {
                                Error::InvalidRequest(
                                    "configure requires a personal_settings object".into(),
                                )
                            })?,
                        )?),
                        _ if request.personal_settings.is_some() => {
                            return Err(Error::InvalidRequest(
                                "settings is read-only; use configure to replace personal settings"
                                    .into(),
                            ));
                        }
                        _ => None,
                    };
                    request_one(
                        &data,
                        ControlRequest {
                            operation: "settings".into(),
                            database,
                            settings_json,
                            ..Default::default()
                        },
                        executable.as_deref(),
                    )?
                }
                WorkspaceOperation::Open => unreachable!(),
            }
        }
    };
    Ok(WorkspaceResult {
        data_directory: path_string(root_for_database(Path::new(&status.database)).ok_or_else(
            || Error::Protocol("coordinator returned an invalid workspace database path".into()),
        )?)?,
        coordinator_executable: executable.as_deref().map(path_string).transpose()?,
        status,
    })
}
fn request_one(
    data: &Path,
    value: ControlRequest,
    executable: Option<&Path>,
) -> Result<WorkspaceStatus> {
    let settings_operation = matches!(
        value.operation.as_str(),
        "register" | "refresh" | "settings" | "cancel"
    );
    if settings_operation {
        ensure_running(data, executable)?;
        require_analysis_settings(data)?;
    }
    let response = match request(data, value.clone()) {
        Err(Error::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            ensure_running(data, executable)?;
            request(data, value)?
        }
        result => result?,
    };
    if response.analysis_settings_version != crate::settings::VERSION
        || response.resource_control_version != crate::resource::CONTROL_VERSION
    {
        return Err(Error::UpdateRequired("the coordinator changed during the request; update the running runtime to use workspace settings".into()));
    }
    response.workspaces.into_iter().next().ok_or_else(|| {
        Error::Coordinator("workspace is no longer registered; open it again explicitly".into())
    })
}
fn request_status(
    data: &Path,
    database: &str,
    executable: Option<&Path>,
) -> Result<WorkspaceStatus> {
    request_one(
        data,
        ControlRequest {
            operation: "status".into(),
            database: database.into(),
            ..Default::default()
        },
        executable,
    )
}
pub fn attach_workspace(
    data: &Path,
    root: &Path,
    configuration: String,
    refresh: bool,
    executable: Option<&Path>,
) -> Result<WorkspaceStatus> {
    let root = std::fs::canonicalize(root).map_err(|e| {
        Error::InvalidRequest(format!("cannot open workspace {}: {e}", root.display()))
    })?;
    if !root.is_dir() {
        return Err(Error::InvalidRequest(
            "workspace must be an existing directory".into(),
        ));
    }
    request_one(
        data,
        ControlRequest {
            operation: if refresh { "refresh" } else { "register" }.into(),
            workspace_root: path_string(&root)?,
            configuration,
            ..Default::default()
        },
        executable,
    )
}

static STARTUP: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn probe(data: &Path) -> Result<bool> {
    match request(
        data,
        ControlRequest {
            operation: "status".into(),
            ..Default::default()
        },
    ) {
        Ok(_) => Ok(true),
        Err(Error::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}
/// Connect first; only a missing/refused socket permits starting a known host.
/// Never launch the embedding process (node/dotnet) as though it were know-code.
pub fn ensure_running(data: &Path, executable: Option<&Path>) -> Result<()> {
    prepare_data_root(data)?;
    if probe(data)? {
        return Ok(());
    }
    let _guard = STARTUP
        .lock()
        .map_err(|_| Error::CoordinatorUnavailable("coordinator startup lock failed".into()))?;
    if probe(data)? {
        return Ok(());
    }
    use std::os::unix::fs::OpenOptionsExt;
    let lifecycle = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(data.join("lifecycle.lock"))?;
    lifecycle.try_lock_shared().map_err(|_| {
        Error::CoordinatorUnavailable(
            "a Know Code runtime operation is in progress; retry when it finishes".into(),
        )
    })?;
    if socket_path(data).as_os_str().as_encoded_bytes().len() >= 104 {
        return Err(Error::InvalidRequest(
            "Unix socket path is too long; choose a shorter data_directory/KNOW_CODE_DATA_DIR"
                .into(),
        ));
    }
    let executable = executable
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("KNOW_CODE_COORDINATOR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("know-code"));
    let mut child = std::process::Command::new(&executable)
        .args(["coordinator","run"]).env("KNOW_CODE_DATA_DIR",data)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().map_err(|e| Error::CoordinatorUnavailable(format!("cannot start Know Code coordinator {}: {e}; install know-code on PATH, set KNOW_CODE_COORDINATOR, or supply coordinator_executable",executable.display())))?;
    let until = std::time::Instant::now() + Duration::from_secs(5);
    let mut exit = None;
    let result = loop {
        match probe(data) {
            Ok(true) => break Ok(()),
            Err(error) => break Err(error),
            Ok(false) => {}
        }
        if exit.is_none() {
            exit = child.try_wait()?;
        }
        if std::time::Instant::now() >= until {
            break Err(Error::CoordinatorUnavailable(format!(
                "coordinator {} did not become ready within 5 seconds (exit: {exit:?}); run that executable with `coordinator run` and the same KNOW_CODE_DATA_DIR to inspect its startup error",
                executable.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // A library host can outlive the service. Reap only a child we launched,
    // without changing its host's SIGCHLD policy or adding per-workspace threads.
    // Normally the login service already exists and this path allocates no thread.
    if exit.is_none() {
        std::thread::Builder::new()
            .name("know-code-reaper".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let _ = child.wait();
            })?;
    }
    result
}

pub fn wait_for_database(
    database: &Path,
    wait_ms: u32,
    deadline_ms: Option<u32>,
) -> Result<Option<WorkspaceStatus>> {
    wait_for_database_inner(database, wait_ms, deadline_ms, None)
}
pub(crate) fn wait_for_database_controlled(
    database: &Path,
    wait_ms: u32,
    execution: crate::execution::Execution,
) -> Result<Option<WorkspaceStatus>> {
    execution.check()?;
    wait_for_database_inner(database, wait_ms, None, Some(execution))
}
fn wait_for_database_inner(
    database: &Path,
    wait_ms: u32,
    deadline_ms: Option<u32>,
    execution: Option<crate::execution::Execution>,
) -> Result<Option<WorkspaceStatus>> {
    let Some(root) = root_for_database(database) else {
        return Ok(None);
    };
    let key = database.to_path_buf();
    if let Some(certification) = crate::live::certified(root, &database.to_string_lossy())
        && checkpoint_live(root, &key, certification.activity_hints)
    {
        return Ok(Some(certification.status));
    }
    let mut request = ControlRequest {
        operation: "wait".into(),
        database: database.to_string_lossy().into_owned(),
        wait_ms,
        ..Default::default()
    };
    let response = if let Some(execution) = execution {
        let execution = execution.phase(Duration::from_millis(u64::from(
            wait_ms.saturating_add(50).max(100),
        )))?;
        let stream = crate::execution::connect(&socket_path(root), execution.clone())?;
        request.wait_ms = request
            .wait_ms
            .min(execution.remaining()?.as_millis().min(u128::from(u32::MAX)) as u32);
        exchange(
            &mut crate::execution::TimedStream::new(stream, execution)?,
            request,
        )?
    } else {
        request_with_timeout(
            root,
            request,
            deadline_ms.unwrap_or(wait_ms.saturating_add(50).max(100)),
        )?
    };
    if response.analysis_settings_version != crate::settings::VERSION
        || response.resource_control_version != crate::resource::CONTROL_VERSION
    {
        return Err(Error::UpdateRequired("update the running Know Code coordinator before querying managed workspace analysis and cancellation state".into()));
    }
    if !response.workspaces.is_empty() {
        record_activity(key);
    }
    Ok(response.workspaces.into_iter().next())
}

// Advisory activity only: a sent hint is not freshness evidence or an acknowledgement.
// Coalesce across all host threads/workspaces. No reply wait or blocking socket I/O
// occurs while this bounded cache is locked.
static ACTIVITY: std::sync::Mutex<Vec<(PathBuf, std::time::Instant)>> =
    std::sync::Mutex::new(Vec::new());
fn checkpoint_live(root: &Path, key: &Path, hints_supported: bool) -> bool {
    let mut entries = ACTIVITY.lock().unwrap();
    entries.retain(|(_, t)| t.elapsed() < Duration::from_secs(5));
    if entries.iter().any(|(p, _)| p == key) {
        return true;
    }
    if !hints_supported || notify_activity(root, &key.to_string_lossy()).is_err() {
        return false;
    }
    remember_activity(&mut entries, key.to_path_buf());
    true
}
fn remember_activity(entries: &mut Vec<(PathBuf, std::time::Instant)>, key: PathBuf) {
    if entries.len() == 32 {
        entries.remove(0);
    }
    entries.push((key, std::time::Instant::now()));
}
fn record_activity(key: PathBuf) {
    let mut entries = ACTIVITY.lock().unwrap();
    entries.retain(|(p, t)| p != &key && t.elapsed() < Duration::from_secs(5));
    remember_activity(&mut entries, key);
}
fn notify_activity(root: &Path, database: &str) -> Result<()> {
    use std::{
        io::{Error as IoError, ErrorKind, Write},
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
    };
    if database.len() > 3000 {
        return Err(
            IoError::new(ErrorKind::InvalidInput, "activity path exceeds hint bound").into(),
        );
    }
    let mut frame = Vec::new();
    protocol::write_frame(
        &mut frame,
        &Envelope {
            protocol_version: VERSION,
            job_id: String::new(),
            payload: Some(envelope::Payload::ControlRequest(ControlRequest {
                operation: "activity".into(),
                index_lifecycle_version: protocol::INDEX_LIFECYCLE_VERSION,
                database: database.into(),
                ..Default::default()
            })),
        },
    )?;
    if frame.len() > 4096 {
        return Err(
            IoError::new(ErrorKind::InvalidInput, "activity frame exceeds hint bound").into(),
        );
    }
    let path = socket_path(root);
    let bytes = path.as_os_str().as_encoded_bytes();
    // The descriptor is nonblocking before connect, including a full listen queue.
    // OwnedFd closes every unsuccessful setup/write, including partial frames.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(IoError::new(ErrorKind::InvalidInput, "invalid activity socket path").into());
    }
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(IoError::last_os_error().into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let one: libc::c_int = 1;
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0
        || unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&one as *const libc::c_int).cast(),
                std::mem::size_of_val(&one) as libc::socklen_t,
            )
        } < 0
    {
        return Err(IoError::last_os_error().into());
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    address.sun_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as u8;
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    if unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            address.sun_len.into(),
        )
    } < 0
    {
        return Err(IoError::last_os_error().into());
    }
    let mut socket = UnixStream::from(fd);
    verify_peer(&socket)?;
    if socket.write(&frame)? != frame.len() {
        return Err(IoError::new(ErrorKind::WriteZero, "incomplete activity hint").into());
    }
    Ok(())
}

/// Explicit capability check: older coordinators must not silently ignore controls.
pub fn require_resource_control(data: &Path) -> Result<()> {
    let response = request(
        data,
        ControlRequest {
            operation: "status".into(),
            ..Default::default()
        },
    )?;
    if response.resource_control_version != crate::resource::CONTROL_VERSION {
        return Err(Error::UpdateRequired("update the running Know Code coordinator to use resource controls or indexing cancellation".into()));
    }
    Ok(())
}
pub fn require_analysis_settings(data: &Path) -> Result<()> {
    let response = request(
        data,
        ControlRequest {
            operation: "status".into(),
            ..Default::default()
        },
    )?;
    if response.analysis_settings_version != crate::settings::VERSION
        || response.resource_control_version != crate::resource::CONTROL_VERSION
    {
        return Err(Error::UpdateRequired("update the running Know Code coordinator to use workspace settings and recorded cancellation boundaries".into()));
    }
    Ok(())
}
pub fn resources(
    data: &Path,
    limits: Option<crate::resource::Limits>,
    executable: Option<&Path>,
) -> Result<crate::resource::Limits> {
    if let Some(limits) = &limits {
        limits.validate()?;
    }
    ensure_running(data, executable)?;
    require_resource_control(data)?;
    let response = request(
        data,
        ControlRequest {
            operation: "resources".into(),
            resources: limits.map(|l| ResourceLimits {
                cpu_cores: l.cpu_cores,
                memory_mib: l.memory_mib,
                rebuild_disk_mib: l.rebuild_disk_mib,
                min_free_disk_mib: l.min_free_disk_mib,
            }),
            ..Default::default()
        },
    )?;
    let limits = response.resources.ok_or_else(|| {
        Error::Protocol("resource settings missing from coordinator response".into())
    })?;
    Ok(crate::resource::Limits {
        cpu_cores: limits.cpu_cores,
        memory_mib: limits.memory_mib,
        rebuild_disk_mib: limits.rebuild_disk_mib,
        min_free_disk_mib: limits.min_free_disk_mib,
    })
}

#[cfg(all(test, target_os = "macos"))]
mod privacy_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::{
            fd::OwnedFd,
            unix::fs::{MetadataExt, PermissionsExt, symlink},
        },
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn root() -> PathBuf {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.artifacts/control-privacy");
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }
    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o777
    }
    fn different_uid() -> libc::uid_t {
        if unsafe { libc::geteuid() } == 0 {
            1
        } else {
            0
        }
    }

    #[test]
    fn actual_unix_peer_credentials_accept_current_uid_and_refuse_mismatch_without_reading() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        verify_peer(&client).unwrap();
        verify_peer(&server).unwrap();
        client.set_nonblocking(true).unwrap();
        server.set_nonblocking(true).unwrap();
        client.write_all(b"untouched control bytes").unwrap();
        for peer in [&client, &server] {
            assert_eq!(
                verify_peer_uid(peer, different_uid()).unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
        }
        verify_peer(&server).unwrap();
        let mut bytes = [0; 23];
        server.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"untouched control bytes");
        // A real kernel lookup failure must not authenticate default uid zero.
        let path = root().join("not-a-socket");
        std::fs::write(&path, b"preserve").unwrap();
        let descriptor: OwnedFd = std::fs::File::open(&path).unwrap().into();
        let not_socket = UnixStream::from(descriptor);
        assert!(verify_peer(&not_socket).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"preserve");
    }

    #[test]
    fn dedicated_data_leaf_is_tightened_with_aliases_and_retained_files_preserved() {
        for initial in [0o755, 0o777] {
            let root = root();
            let data = root.join("data");
            std::fs::create_dir(&data).unwrap();
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(initial)).unwrap();
            std::fs::write(data.join(".DS_Store"), b"retained Finder metadata").unwrap();
            std::fs::write(data.join("retained-state"), b"not a filename whitelist").unwrap();
            let alias = root.join("data-alias");
            symlink(data.canonicalize().unwrap(), &alias).unwrap();
            let parent_mode = mode(&root);
            prepare_data_root(&alias).unwrap();
            assert_eq!(mode(&data), 0o700);
            assert_eq!(mode(&root), parent_mode);
            assert!(std::fs::symlink_metadata(&alias).unwrap().is_symlink());
            assert_eq!(
                std::fs::read(data.join(".DS_Store")).unwrap(),
                b"retained Finder metadata"
            );
            assert_eq!(
                std::fs::read(data.join("retained-state")).unwrap(),
                b"not a filename whitelist"
            );
        }
    }

    #[test]
    fn actual_account_home_is_protected_with_missing_or_misleading_configured_home() {
        let root = root();
        let actual = root.join("actual-account-home");
        let configured = root.join("configured-home");
        for path in [&actual, &configured] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let uid = unsafe { libc::geteuid() };
        // Inject two owned paths rather than changing process HOME or touching
        // the real account directory. Both homes and their ancestors refuse.
        for home in [None, Some(configured.as_path())] {
            for path in [&actual, &root] {
                let before = mode(path);
                assert!(prepare_data_root_owned(path, uid, &actual, home).is_err());
                assert_eq!(mode(path), before);
            }
        }
        assert!(prepare_data_root_owned(&configured, uid, &actual, Some(&configured)).is_err());
        assert_eq!(mode(&configured), 0o755);
        let unresolved = root.join("missing-account-home");
        assert!(prepare_data_root_owned(&configured, uid, &unresolved, None).is_err());
        assert_eq!(mode(&configured), 0o755);
        // The actual platform lookup must return an absolute account path,
        // independently of environment HOME. This does not mutate that path.
        assert!(user_home(uid).unwrap().is_absolute());
    }

    #[test]
    fn inherited_extended_acl_is_removed_only_from_the_owned_data_leaf() {
        let root = root();
        let parent = root.join("acl-parent");
        std::fs::create_dir(&parent).unwrap();
        // chmod operates only on this unique owned fixture. Grant a real
        // inheritable everyone ACL so mode0700 alone cannot satisfy this test.
        let output = std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow list,search,file_inherit,directory_inherit",
            ])
            .arg(&parent)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "owned ACL setup failed: {output:?}"
        );
        let parent_file = std::fs::File::open(&parent).unwrap();
        let parent_acl = data_acl::Acl::get(&parent_file).unwrap();
        assert!(parent_acl.has_entries().unwrap());
        let before_acl = parent_acl.text().unwrap();
        let before_mode = mode(&parent);
        let data = parent.join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
        let data_file = std::fs::File::open(&data).unwrap();
        assert!(
            data_acl::Acl::get(&data_file)
                .unwrap()
                .has_entries()
                .unwrap()
        );
        let retained = data.join("retained");
        std::fs::write(&retained, b"retained state").unwrap();
        let retained_file = std::fs::File::open(&retained).unwrap();
        let retained_acl = data_acl::Acl::get(&retained_file).unwrap().text().unwrap();
        let retained_mode = mode(&retained);
        let alias = root.join("data-alias");
        symlink(data.canonicalize().unwrap(), &alias).unwrap();
        prepare_data_root(&alias).unwrap();
        assert_eq!(mode(&data), 0o700);
        assert!(
            !data_acl::Acl::get(&data_file)
                .unwrap()
                .has_entries()
                .unwrap()
        );
        assert_eq!(
            data_acl::Acl::get(&parent_file).unwrap().text().unwrap(),
            before_acl
        );
        assert_eq!(mode(&parent), before_mode);
        assert_eq!(
            data_acl::Acl::get(&retained_file).unwrap().text().unwrap(),
            retained_acl
        );
        assert_eq!(mode(&retained), retained_mode);
        assert_eq!(std::fs::read(&retained).unwrap(), b"retained state");
        assert!(std::fs::symlink_metadata(&alias).unwrap().is_symlink());
        // Repeating privacy validation on the now-empty ACL performs no ACL
        // replacement (the same read-only path is used by ensure_running).
        let before = std::fs::metadata(&data).unwrap();
        prepare_data_root(&alias).unwrap();
        let after = std::fs::metadata(&data).unwrap();
        assert_eq!(
            (before.ctime(), before.ctime_nsec()),
            (after.ctime(), after.ctime_nsec())
        );
    }

    #[test]
    fn data_root_refusal_never_chmods_foreign_owner_home_repo_or_git_metadata() {
        let root = root();
        let account_home = root.join("account-home");
        let data = root.join("data");
        let repository = root.join("repository");
        for path in [&account_home, &data, &repository] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let uid = unsafe { libc::geteuid() };
        assert!(
            prepare_data_root_owned(&data, different_uid(), &account_home, Some(&account_home))
                .is_err()
        );
        assert_eq!(mode(&data), 0o755);
        std::fs::create_dir(repository.join(".git")).unwrap();
        for path in [&root, &account_home, &repository, &repository.join(".git")] {
            let before = mode(path);
            assert!(
                prepare_data_root_owned(path, uid, &account_home, Some(&account_home)).is_err()
            );
            assert_eq!(mode(path), before);
        }
        let alias = root.join("git-alias");
        symlink(repository.join(".git").canonicalize().unwrap(), &alias).unwrap();
        assert!(
            prepare_data_root_owned(
                &alias.join("new-state"),
                uid,
                &account_home,
                Some(&account_home)
            )
            .is_err()
        );
        assert!(!repository.join(".git/new-state").exists());
        let before = mode(Path::new("/"));
        assert!(prepare_data_root_owned(Path::new("/"), uid, &account_home, None).is_err());
        assert_eq!(mode(Path::new("/")), before);
        // Dedicated descendants underneath a source checkout remain valid.
        prepare_data_root_owned(
            &repository.join(".artifacts/data"),
            uid,
            &account_home,
            Some(&account_home),
        )
        .unwrap();
        assert_eq!(mode(&repository.join(".artifacts/data")), 0o700);
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;
    use std::{
        io::ErrorKind,
        os::unix::net::UnixListener,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Instant,
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    fn root() -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
            .join(".artifacts/at")
            .join(format!(
                "{}-{}-{:x}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn controlled_wait_reads_capable_peer_and_caps_server_wait() {
        let root = root();
        let database = root.join("workspaces/test/index.db");
        let listener = UnixListener::bind(socket_path(&root)).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = protocol::read_frame(&mut stream).unwrap().unwrap();
            let Some(envelope::Payload::ControlRequest(request)) = request.payload else {
                panic!("expected request")
            };
            assert!(
                request.wait_ms <= 100,
                "uncapped server wait: {}",
                request.wait_ms
            );
            protocol::write_frame(
                &mut stream,
                &Envelope {
                    protocol_version: VERSION,
                    payload: Some(envelope::Payload::ControlResponse(ControlResponse {
                        resource_control_version: crate::resource::CONTROL_VERSION,
                        index_lifecycle_version: protocol::INDEX_LIFECYCLE_VERSION,
                        analysis_settings_version: crate::settings::VERSION,
                        workspaces: vec![WorkspaceStatus {
                            generation: 42,
                            ..Default::default()
                        }],
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            )
            .unwrap();
        });
        let execution = crate::execution::Execution::new(Default::default(), Some(100)).unwrap();
        let response = wait_for_database_controlled(&database, 120_000, execution);
        assert_eq!(response.unwrap().unwrap().generation, 42);
        server.join().unwrap();
    }
    #[test]
    fn lifecycle_safety_is_required_before_accepting_any_managed_response() {
        let mut response = ControlResponse {
            resource_control_version: crate::resource::CONTROL_VERSION,
            analysis_settings_version: crate::settings::VERSION,
            ..Default::default()
        };
        assert!(matches!(
            checked_response(response.clone()),
            Err(Error::UpdateRequired(_))
        ));
        response.index_lifecycle_version = protocol::INDEX_LIFECYCLE_VERSION;
        assert!(checked_response(response.clone()).is_ok());
        response.error_code = "index_recovering".into();
        response.error_message = "replacement pending".into();
        assert!(
            matches!(checked_response(response),Err(Error::Remote{code,..}) if code=="index_recovering")
        );
    }
    #[test]
    fn old_coordinator_cannot_silently_accept_resource_controls() {
        let root = root();
        let listener = UnixListener::bind(socket_path(&root)).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            protocol::read_frame(&mut stream).unwrap();
            protocol::write_frame(
                &mut stream,
                &Envelope {
                    protocol_version: VERSION,
                    job_id: String::new(),
                    payload: Some(envelope::Payload::ControlResponse(
                        ControlResponse::default(),
                    )),
                },
            )
            .unwrap();
        });
        assert!(matches!(
            require_resource_control(&root),
            Err(Error::UpdateRequired(_))
        ));
        server.join().unwrap();
    }
    #[test]
    fn runtime_operation_prevents_automatic_startup() {
        let root = root();
        let fence = std::fs::File::create(root.join("lifecycle.lock")).unwrap();
        fence.try_lock().unwrap();
        let error = ensure_running(&root, Some(Path::new("/missing/coordinator"))).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("runtime operation is in progress")
        );
        // Another concurrent test can fork while this lock is held. Explicitly
        // unlock the shared file description rather than waiting for inherited
        // descriptors to close in that child.
        fence.unlock().unwrap();
        drop(fence);
        let error = ensure_running(&root, Some(Path::new("/missing/coordinator"))).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot start Know Code coordinator"),
            "{error}"
        );
    }
    #[test]
    fn coalesced_hints_do_not_wait_for_a_reply_and_expiry_retries() {
        let root = Arc::new(root());
        let listener = UnixListener::bind(socket_path(&root)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let database = root.join("workspaces/test/index.db");
        let mut threads = Vec::new();
        for _ in 0..24 {
            let root = Arc::clone(&root);
            let database = database.clone();
            threads.push(std::thread::spawn(move || {
                assert!(checkpoint_live(&root, &database, true))
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        // No server has accepted, read or replied to any request yet.
        let (mut stream, _) = listener.accept().unwrap();
        let frame = protocol::read_frame(&mut stream).unwrap().unwrap();
        let Some(envelope::Payload::ControlRequest(request)) = frame.payload else {
            panic!("not a request")
        };
        assert_eq!(request.operation, "activity");
        assert_eq!(request.database, database.to_string_lossy());
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        {
            let mut entries = ACTIVITY.lock().unwrap();
            entries.iter_mut().find(|(p, _)| p == &database).unwrap().1 =
                Instant::now() - Duration::from_secs(6);
        }
        assert!(checkpoint_live(&root, &database, true));
        assert!(listener.accept().is_ok());
        assert!(!checkpoint_live(&root, &root.join("unknown"), false));
    }
    #[test]
    fn full_listen_queue_never_blocks_hint_submission() {
        let root = root();
        let _listener = UnixListener::bind(socket_path(&root)).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut failed = 0;
            for _ in 0..1024 {
                if notify_activity(&root, "workspaces/test/index.db").is_err() {
                    failed += 1;
                }
            }
            send.send(failed).unwrap();
        });
        assert!(
            receive
                .recv_timeout(Duration::from_secs(5))
                .expect("nonblocking submission hung")
                > 0
        );
    }
    #[test]
    fn failed_or_oversized_notifications_do_not_create_activity_checkpoints() {
        let root = root();
        let database = root.join("workspaces/test/index.db");
        assert!(!checkpoint_live(&root, &database, true));
        assert!(!ACTIVITY.lock().unwrap().iter().any(|(p, _)| p == &database));
        assert!(notify_activity(&root, &"x".repeat(3001)).is_err());
        let listener = UnixListener::bind(socket_path(&root)).unwrap();
        listener.set_nonblocking(true).unwrap();
        assert!(checkpoint_live(&root, &database, true));
        assert!(listener.accept().is_ok());
    }
}

#[cfg(test)]
mod installer_bridge_tests {
    use super::*;
    #[test]
    fn prior_peer_bridge_preserves_strict_consumers_and_rejects_other_operations() {
        for peer_version in [1, 2] {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()
                .unwrap()
                .join(".artifacts/ib")
                .join(format!(
                    "{:x}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            std::fs::create_dir_all(&root).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(socket_path(&root)).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let frame = protocol::read_frame(&mut stream).unwrap().unwrap();
                let Some(envelope::Payload::ControlRequest(request)) = frame.payload else {
                    panic!("control request")
                };
                assert_eq!(request.index_lifecycle_version, peer_version);
                assert_eq!(request.operation, "status");
                protocol::write_frame(
                    &mut stream,
                    &Envelope {
                        protocol_version: VERSION,
                        payload: Some(envelope::Payload::ControlResponse(ControlResponse {
                            index_lifecycle_version: peer_version,
                            resource_control_version: crate::resource::CONTROL_VERSION,
                            analysis_settings_version: crate::settings::VERSION,
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                )
                .unwrap();
            });
            let response = request_installer(
                &root,
                ControlRequest {
                    operation: "status".into(),
                    ..Default::default()
                },
                peer_version,
            )
            .unwrap();
            server.join().unwrap();
            assert!(matches!(
                checked_response(response),
                Err(Error::UpdateRequired(_))
            ));
            for (operation, version) in [
                ("refresh", 1),
                ("index-corrupt", 1),
                ("status", 0),
                ("status", 5),
            ] {
                assert!(matches!(
                    request_installer(
                        &root,
                        ControlRequest {
                            operation: operation.into(),
                            ..Default::default()
                        },
                        version
                    ),
                    Err(Error::UpdateRequired(_))
                ));
            }
        }
    }
}
