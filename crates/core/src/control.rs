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
fn request_with_timeout(
    root: &Path,
    request: ControlRequest,
    timeout: u32,
) -> Result<ControlResponse> {
    let mut socket = UnixStream::connect(socket_path(root))?;
    socket.set_write_timeout(Some(Duration::from_millis(u64::from(timeout.max(1)))))?;
    socket.set_read_timeout(Some(Duration::from_millis(u64::from(timeout.max(1)))))?;
    protocol::write_frame(
        &mut socket,
        &Envelope {
            protocol_version: VERSION,
            job_id: String::new(),
            payload: Some(envelope::Payload::ControlRequest(request)),
        },
    )?;
    let response = protocol::read_frame(&mut socket)?
        .ok_or_else(|| Error::Protocol("coordinator closed the control channel".into()))?;
    match response.payload {
        Some(envelope::Payload::ControlResponse(response)) if response.error_code.is_empty() => {
            Ok(response)
        }
        Some(envelope::Payload::ControlResponse(response))
            if response.error_code == "update_required" =>
        {
            Err(Error::UpdateRequired(response.error_message))
        }
        Some(envelope::Payload::ControlResponse(response)) => Err(Error::Remote {
            code: response.error_code,
            message: response.error_message,
        }),
        _ => Err(Error::Protocol("unexpected coordinator response".into())),
    }
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
}
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOperation {
    Open,
    Status,
    Refresh,
    Cancel,
    Wait,
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
                WorkspaceOperation::Refresh => request_one(
                    &data,
                    ControlRequest {
                        operation: "refresh".into(),
                        workspace_root: request_status(&data, &database, executable.as_deref())?
                            .workspace_root,
                        configuration: request.configuration.unwrap_or_default(),
                        ..Default::default()
                    },
                    executable.as_deref(),
                )?,
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
    if probe(data)? {
        return Ok(());
    }
    let _guard = STARTUP
        .lock()
        .map_err(|_| Error::CoordinatorUnavailable("coordinator startup lock failed".into()))?;
    if probe(data)? {
        return Ok(());
    }
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data)?;
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
    let Some(root) = root_for_database(database) else {
        return Ok(None);
    };
    let key = database.to_path_buf();
    if let Some(certification) = crate::live::certified(root, &database.to_string_lossy())
        && checkpoint_live(root, &key, certification.activity_hints)
    {
        return Ok(Some(certification.status));
    }
    let response = request_with_timeout(
        root,
        ControlRequest {
            operation: "wait".into(),
            database: database.to_string_lossy().into_owned(),
            wait_ms,
            ..Default::default()
        },
        deadline_ms.unwrap_or(wait_ms.saturating_add(50).max(100)),
    )?;
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
    })
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
        drop(fence);
        let error = ensure_running(&root, Some(Path::new("/missing/coordinator"))).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot start Know Code coordinator")
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
