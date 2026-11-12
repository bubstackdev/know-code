use crate::{
    Error, Result,
    protocol::{self, envelope::Payload, *},
    resource::Control,
    store::Writer,
};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::{BufReader, Read, Write},
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// PATH shared by top-level compiler workers, their nested tools and installed
/// login jobs. Empty/relative entries must never select the workspace directory.
pub fn trusted_path(value: &OsStr) -> Result<OsString> {
    if value.as_bytes().contains(&0) {
        return Err(Error::InvalidRequest("toolchain PATH contains NUL".into()));
    }
    let mut seen = std::collections::BTreeSet::new();
    let paths = std::env::split_paths(value)
        .filter(|path| path.is_absolute() && seen.insert(path.clone()))
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Err(Error::InvalidRequest("toolchain PATH has no absolute directories; configure an absolute PATH entry for installed tools".into()));
    }
    std::env::join_paths(paths)
        .map_err(|error| Error::InvalidRequest(format!("invalid toolchain PATH: {error}")))
}
fn worker_executable(program: &str, path: &OsStr) -> Result<PathBuf> {
    let selected = Path::new(program);
    let mut components = selected.components();
    let candidates = if selected.is_absolute() {
        vec![selected.to_owned()]
    } else if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
    {
        std::env::split_paths(path)
            .map(|dir| dir.join(selected))
            .collect()
    } else {
        return Err(Error::InvalidRequest(
            "worker executable must be absolute or a toolchain PATH name".into(),
        ));
    };
    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        let absolute = std::fs::canonicalize(&candidate)?;
        let bytes = std::ffi::CString::new(absolute.as_os_str().as_bytes())
            .map_err(|_| Error::InvalidRequest("worker executable path contains NUL".into()))?;
        if unsafe { libc::access(bytes.as_ptr(), libc::X_OK) } == 0 {
            return Ok(absolute);
        }
    }
    Err(Error::Provider(format!(
        "cannot locate executable {program} on the absolute toolchain PATH; install the required toolchain separately and configure an absolute PATH entry"
    )))
}

/// Owns one parent pipe endpoint. An escaped descendant can retain the other
/// endpoint, but cannot postpone cancellation or the original worker's EOF.
struct WorkerPipe<P> {
    pipe: P,
    control: Control,
    leader: libc::pid_t,
    reaped: Arc<AtomicBool>,
    drain: Option<usize>,
    drain_start: Option<usize>,
    read_bytes: u64,
    eof: Option<&'static str>,
}
impl<P: AsRawFd> WorkerPipe<P> {
    fn new(
        pipe: P,
        control: &Control,
        leader: u32,
        reaped: &Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let fd = pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            pipe,
            control: control.clone(),
            leader: leader as libc::pid_t,
            reaped: Arc::clone(reaped),
            drain: None,
            drain_start: None,
            read_bytes: 0,
            eof: None,
        })
    }
    fn check(&self) -> std::io::Result<()> {
        self.control.check().map_err(std::io::Error::other)
    }
    fn exited(&self) -> std::io::Result<bool> {
        if self.reaped.load(Ordering::Acquire) {
            return Ok(true);
        }
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.leader as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if result == 0 {
            return Ok(unsafe { info.assume_init() }.si_pid != 0);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECHILD) {
            return Ok(true);
        }
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        Err(error)
    }
    fn poll(&self, events: i16) -> std::io::Result<()> {
        let mut fd = libc::pollfd {
            fd: self.pipe.as_raw_fd(),
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut fd, 1, 20) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if fd.revents & libc::POLLNVAL != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "worker pipe closed unexpectedly",
            ));
        }
        self.check()
    }
}
impl<P: Read + AsRawFd> Read for WorkerPipe<P> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            self.check()?;
            if self.drain.is_none() && self.exited()? {
                // Drain exactly the bytes already buffered when the owned
                // leader exits. Later descendant writes are not worker output.
                let mut available: libc::c_int = 0;
                if unsafe { libc::ioctl(self.pipe.as_raw_fd(), libc::FIONREAD, &mut available) } < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                self.drain = Some(available.max(0) as usize);
                self.drain_start = self.drain;
            }
            let count = self
                .drain
                .map_or(buffer.len(), |left| left.min(buffer.len()));
            if count == 0 {
                self.eof = Some("owned-exit drain exhausted");
                return Ok(0);
            }
            match self.pipe.read(&mut buffer[..count]) {
                Ok(count) => {
                    self.read_bytes = self.read_bytes.saturating_add(count as u64);
                    if count == 0 {
                        self.eof = Some("pipe read returned zero");
                    }
                    if let Some(left) = &mut self.drain {
                        *left = left.saturating_sub(count);
                    }
                    return Ok(count);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if self.drain.is_some() {
                        self.eof = Some("owned-exit drain would block");
                        return Ok(0);
                    }
                    self.poll(libc::POLLIN)?;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

// Failure-only observation: WNOWAIT keeps the leader PID reserved until the
// owned Worker removes its group, cleans up descendants, and reaps it.
fn worker_exit_snapshot(leader: u32) -> String {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            leader,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    if result == 0 {
        let info = unsafe { info.assume_init() };
        format!(
            "si_pid={} si_code={} si_status={}",
            info.si_pid, info.si_code, info.si_status
        )
    } else {
        let error = std::io::Error::last_os_error();
        format!(
            "waitid error kind={:?} errno={:?}",
            error.kind(),
            error.raw_os_error()
        )
    }
}
impl<P: Write + AsRawFd> Write for WorkerPipe<P> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            self.check()?;
            if self.exited()? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "worker exited before consuming the request",
                ));
            }
            match self.pipe.write(buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    self.poll(libc::POLLOUT)?
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.check()?;
        self.pipe.flush()
    }
}

pub struct WorkerCommand {
    pub program: String,
    pub args: Vec<String>,
    pub request: AnalyzeRequest,
    pub execution_grant: crate::settings::ProjectExecutionGrant,
}

pub fn index(database: &Path, workers: Vec<WorkerCommand>) -> Result<u64> {
    index_scoped(database, workers, None)
}
pub fn index_scoped(
    database: &Path,
    workers: Vec<WorkerCommand>,
    replacements: Option<&[String]>,
) -> Result<u64> {
    index_checked(database, workers, replacements, &mut |_| {}, || Ok(()))
}

pub fn index_checked(
    database: &Path,
    workers: Vec<WorkerCommand>,
    replacements: Option<&[String]>,
    progress: &mut impl FnMut(Progress),
    before_publish: impl FnOnce() -> Result<()>,
) -> Result<u64> {
    index_controlled(
        database,
        workers,
        replacements,
        progress,
        before_publish,
        &Control::default(),
    )
}
pub fn index_controlled(
    database: &Path,
    workers: Vec<WorkerCommand>,
    replacements: Option<&[String]>,
    progress: &mut impl FnMut(Progress),
    before_publish: impl FnOnce() -> Result<()>,
    control: &Control,
) -> Result<u64> {
    index_published(
        database,
        workers,
        replacements,
        progress,
        before_publish,
        control,
        Publication::Serving(None),
    )
}
#[derive(Clone, Copy)]
pub(crate) enum Publication<'a> {
    Serving(Option<&'a crate::cursor::CursorService>),
    Replacement(&'a crate::recovery::Candidate),
}
pub(crate) fn index_published(
    database: &Path,
    workers: Vec<WorkerCommand>,
    replacements: Option<&[String]>,
    progress: &mut impl FnMut(Progress),
    before_publish: impl FnOnce() -> Result<()>,
    control: &Control,
    publication: Publication<'_>,
) -> Result<u64> {
    control.check()?;
    // No writer, toolchain, dependency setup or process is opened before the
    // trusted host's separate identity-bound authorization has been checked.
    for worker in &workers {
        worker
            .execution_grant
            .check_root(Path::new(&worker.request.workspace_root))?;
    }
    let execution_grants: Vec<_> = workers.iter().map(|w| w.execution_grant.clone()).collect();
    let path = trusted_path(&std::env::var_os("PATH").unwrap_or_default())?;
    let mut writer = match publication {
        Publication::Replacement(candidate) => Writer::open_destination(
            database,
            &candidate.database,
            None,
            candidate.minimum_generation,
            control,
        )?,
        Publication::Serving(_) => Writer::open_controlled(database, replacements, control)?,
    };
    let preparation_path = database.with_extension("preparation.json");
    let mut preparations: BTreeMap<String, PreparationResult> =
        match std::fs::read(&preparation_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
    for (number, mut worker) in workers.into_iter().enumerate() {
        let requested_policy = protocol::prepare_analysis_policy(&mut worker.request)?;
        let target_policy = worker.request.target_equivalence_policy;
        if target_policy != 0 && target_policy != crate::grouping::POLICY_VERSION {
            return Err(Error::UpdateRequired(
                "unsupported target equivalence policy".into(),
            ));
        }
        let classification_policy = worker.request.test_classification_policy;
        if classification_policy != 0 && classification_policy != crate::testing::POLICY_VERSION {
            return Err(Error::UpdateRequired(
                "unsupported test classification policy".into(),
            ));
        }
        worker.request.preparations = preparations.values().cloned().collect();
        let job_id = format!("{}-{number}", writer.generation);
        control.check()?;
        // A prior worker can outlive a personal revocation in a raw embedding;
        // do not start another compiler/dependency process using its old grant.
        worker
            .execution_grant
            .check_root(Path::new(&worker.request.workspace_root))?;
        let executable = worker_executable(&worker.program, &path)?;
        let mut child = control
            .spawn(
                Command::new(executable)
                    .args(&worker.args)
                    .current_dir(&worker.request.workspace_root)
                    .env("PATH", &path)
                    .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
                    .env("DOTNET_NOLOGO", "1")
                    .env(
                        "KNOW_CODE_BUILD_PROFILE",
                        if cfg!(debug_assertions) {
                            "debug"
                        } else {
                            "release"
                        },
                    )
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
            )
            .map_err(|e| match e {
                Error::Io(e) => Error::Provider(format!(
                    "cannot launch {}: {e}; install the required toolchain and put it on PATH",
                    worker.program
                )),
                other => other,
            })?;
        let reaped = Arc::new(AtomicBool::new(false));
        let leader = child.child.id();
        let mut stderr = WorkerPipe::new(
            child.child.stderr.take().expect("piped stderr"),
            control,
            leader,
            &reaped,
        )?;
        let errors = std::thread::spawn(move || {
            let mut retained = std::collections::VecDeque::with_capacity(8192);
            let mut buffer = [0; 1024];
            let read_error = loop {
                let n = match stderr.read(&mut buffer) {
                    Ok(0) => break None,
                    Ok(n) => n,
                    Err(error) => {
                        break Some(format!(
                            "stderr read error kind={:?} errno={:?}",
                            error.kind(),
                            error.raw_os_error()
                        ));
                    }
                };
                for byte in &buffer[..n] {
                    if retained.len() == 8192 {
                        retained.pop_front();
                    }
                    retained.push_back(*byte);
                }
            };
            (
                String::from_utf8_lossy(&retained.into_iter().collect::<Vec<_>>()).into_owned(),
                read_error,
            )
        });
        let mut stdout = None;
        let execution = (|| -> Result<()> {
            protocol::write_frame(
                &mut WorkerPipe::new(
                    child.child.stdin.take().expect("piped stdin"),
                    control,
                    leader,
                    &reaped,
                )?,
                &Envelope {
                    protocol_version: VERSION,
                    job_id: job_id.clone(),
                    payload: Some(Payload::Analyze(worker.request)),
                },
            )?;
            let stdout = stdout.insert(BufReader::new(WorkerPipe::new(
                child.child.stdout.take().expect("piped stdout"),
                control,
                leader,
                &reaped,
            )?));
            let mut hello = false;
            let mut completed = false;
            while let Some(message) = protocol::read_frame(&mut *stdout)? {
                control.check()?;
                if message.job_id != job_id || completed {
                    return Err(Error::Protocol(
                        "unexpected job identity or messages after completion".into(),
                    ));
                }
                match message.payload {
                    Some(Payload::Provider(info)) if !hello => {
                        protocol::check_analysis_policy(requested_policy, &info)?;
                        if target_policy != 0
                            && (info.target_equivalence_policy != target_policy
                                || info.language != "csharp")
                        {
                            return Err(Error::UpdateRequired("worker must acknowledge target equivalence policy; update the runtime bundle".into()));
                        }
                        if classification_policy != 0
                            && (info.test_classification_policy != classification_policy
                                || !["typescript", "csharp"].contains(&info.language.as_str()))
                        {
                            return Err(Error::UpdateRequired("worker must acknowledge test classification policy; update the runtime bundle".into()));
                        }
                        eprintln!(
                            "analyzing {} with compiler {}",
                            info.language, info.compiler_version
                        );
                        hello = true;
                    }
                    Some(Payload::Complete(done)) if hello && done.success => completed = true,
                    Some(Payload::Progress(status)) if hello => {
                        if status.stage.len() > 64
                            || status.project_path.len() > 4096
                            || status.message.len() > 8192
                        {
                            return Err(Error::Protocol("oversized progress message".into()));
                        }
                        progress(status);
                    }
                    Some(Payload::Preparation(state)) if hello => {
                        if state.key.is_empty()
                            || state.key.len() > 4096
                            || state.fingerprint.len() > 128
                            || state.error.len() > 32768
                        {
                            return Err(Error::Protocol("invalid preparation checkpoint".into()));
                        }
                        preparations.insert(state.key.clone(), state);
                        let next = preparation_path.with_extension("json.next");
                        std::fs::write(&next, serde_json::to_vec(&preparations)?)?;
                        std::fs::rename(next, &preparation_path)?;
                    }
                    Some(payload) if hello => {
                        if let Payload::Compilation(compilation) = &payload
                            && target_policy != 0
                            && compilation.target_equivalence_policy != target_policy
                        {
                            return Err(Error::Protocol(
                                "requested target equivalence metadata missing or incompatible"
                                    .into(),
                            ));
                        }
                        if classification_policy != 0 {
                            let classification = match &payload {
                                Payload::Compilation(c) => Some(c.test_classification.as_ref()),
                                Payload::Source(s) if s.test_classification.is_some() => {
                                    Some(s.test_classification.as_ref())
                                }
                                _ => None,
                            };
                            if classification.is_some_and(|c| {
                                c.is_none_or(|c| c.policy_version != classification_policy)
                            }) {
                                return Err(Error::Protocol("requested test classification metadata missing or incompatible".into()));
                            }
                        }
                        writer.stage(payload)?;
                    }
                    _ => {
                        return Err(Error::Protocol(
                            "missing provider handshake or invalid completion".into(),
                        ));
                    }
                }
            }
            if !completed {
                return Err(Error::Protocol(
                    "worker ended without successful completion; previous generation preserved"
                        .into(),
                ));
            }
            Ok(())
        })();
        let failure_context = execution.is_err().then(|| {
            let pipe_context = stdout.as_ref().map(|stdout| {
                let pipe = stdout.get_ref();
                format!("stdout_eof={:?} stdout_read_bytes={} drain_start={:?} drain_remaining={:?}",
                    pipe.eof, pipe.read_bytes, pipe.drain_start, pipe.drain)
            }).unwrap_or_else(|| "stdout not initialized".into());
            format!(
                "leader={leader} before_cleanup=[{}] {pipe_context}", worker_exit_snapshot(leader)
            )
        });
        if execution.is_err() {
            child.terminate();
        }
        let exit = child.wait()?;
        reaped.store(true, Ordering::Release);
        let (mut details, stderr_error) = errors
            .join()
            .unwrap_or_else(|_| (String::new(), Some("stderr reader panicked".into())));
        if let Some(error) = stderr_error {
            details.push('\n');
            details.push_str(&error);
        }
        control.check()?;
        execution.map_err(|error| match error {
            Error::UpdateRequired(_)
            | Error::IndexCorrupt { .. }
            | Error::IndexRecovering(_)
            | Error::ResourceLimited(_)
            | Error::DiskPressure(_)
            | Error::Cancelled
            | Error::Sqlite(_)
            | Error::Io(_) => error,
            other => Error::Provider(format!(
                "{other}\nworker failure diagnostics: {}; forced_cleanup_requested=true; owned_wait={exit}\n{details}",
                failure_context.as_deref().unwrap_or("unavailable")
            )),
        })?;
        if !exit.success() {
            return Err(Error::Provider(format!(
                "worker {} exited with {exit}; previous generation preserved\n{details}",
                worker.program
            )));
        }
    }
    control.check()?;
    for grant in execution_grants {
        grant.check()?;
    }
    before_publish()?;
    match publication {
        Publication::Replacement(_) => writer.publish_controlled(&control.staging()),
        Publication::Serving(Some(cursors)) => cursors.publish(database, writer, control),
        Publication::Serving(None) => writer.publish_controlled(control),
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn path_ignores_cwd_relative_and_empty_entries_without_losing_absolute_priority() {
        let clean = trusted_path(OsStr::new(":.:bin:/usr/bin::/bin:/usr/bin")).unwrap();
        assert_eq!(clean, OsStr::new("/usr/bin:/bin"));
        assert!(matches!(
            trusted_path(OsStr::new(":.:bin")),
            Err(Error::InvalidRequest(_))
        ));
        assert!(matches!(
            trusted_path(OsStr::new("/usr/bin\0hidden")),
            Err(Error::InvalidRequest(_))
        ));
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.artifacts/trusted-path")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let bin = root.join("tools");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        for path in [bin.join("owned-tool"), workspace.join("owned-tool")] {
            std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let selected =
            std::env::join_paths([Path::new("."), Path::new("relative"), bin.as_path()]).unwrap();
        let clean = trusted_path(&selected).unwrap();
        assert_eq!(
            worker_executable("owned-tool", &clean).unwrap(),
            std::fs::canonicalize(bin.join("owned-tool")).unwrap()
        );
        assert!(matches!(
            worker_executable("./owned-tool", &clean),
            Err(Error::InvalidRequest(_))
        ));
        assert!(
            worker_executable(
                "owned-tool",
                &trusted_path(OsStr::new("/usr/bin:/bin")).unwrap()
            )
            .is_err()
        );
        assert_eq!(
            worker_executable(bin.join("owned-tool").to_str().unwrap(), &clean).unwrap(),
            std::fs::canonicalize(bin.join("owned-tool")).unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod pipe_tests {
    use super::*;
    use std::{
        io::BufRead,
        os::unix::process::CommandExt,
        sync::mpsc,
        time::{Duration, Instant},
    };
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.artifacts/worker-pipes")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn release(&self) {
            let _ = std::fs::write(self.0.join("release"), b"done");
            let until = Instant::now() + Duration::from_secs(3);
            while !self.0.join("retainer-done").exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        fn ready(&self) {
            let until = Instant::now() + Duration::from_secs(3);
            while !self.0.join("retainer-pid").exists() {
                assert!(
                    Instant::now() < until,
                    "fixture worker failed to start retained-pipe child"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        fn command(&self, mode: &str) -> Command {
            let mut cmd = Command::new(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "update::pipe_tests::fixture_worker",
                "--nocapture",
            ])
            .env("KNOW_CODE_PIPE_FIXTURE", &self.0)
            .env("KNOW_CODE_PIPE_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
            cmd
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.release();
        }
    }
    #[test]
    fn fixture_worker() {
        let Some(root) = std::env::var_os("KNOW_CODE_PIPE_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(root);
        // This owned shell leaves the worker's process group, retains both
        // output endpoints and exits on release or watchdog. Complete-stream
        // cases emit late bytes on release, after the owned leader's EOF.
        // Tests release it through a file; they never kill an escaped process.
        let mut retainer=Command::new("/bin/sh").args(["-c","n=0; while [ ! -f \"$KNOW_CODE_PIPE_FIXTURE/release\" ] && [ $n -lt 500 ]; do /bin/sleep 0.02; n=$((n+1)); done; case \"$KNOW_CODE_PIPE_MODE\" in complete*) printf late-descendant-output;; esac; : > \"$KNOW_CODE_PIPE_FIXTURE/retainer-done\""])
            .env("KNOW_CODE_PIPE_FIXTURE",&root).process_group(0).stdin(Stdio::null()).spawn().unwrap();
        std::fs::write(root.join("retainer-pid"), retainer.id().to_string()).unwrap();
        // The fixture leader exits explicitly below; while alive it still
        // arranges normal reaping if the retained child finishes first.
        let _retainer_waiter = std::thread::spawn(move || {
            let _ = retainer.wait();
        });
        println!("--worker-stream--");
        std::io::stdout().flush().unwrap();
        let mode = std::env::var("KNOW_CODE_PIPE_MODE").unwrap();
        if mode.starts_with("complete") {
            for payload in [
                Payload::Provider(ProviderInfo {
                    language: "typescript".into(),
                    ..Default::default()
                }),
                Payload::Source(SourceArtifact {
                    id: "owned".into(),
                    utf8: vec![
                        b'b';
                        if mode == "complete-small" {
                            128
                        } else {
                            2 * 1024 * 1024
                        }
                    ],
                    ..Default::default()
                }),
                Payload::Complete(Completion { success: true }),
            ] {
                protocol::write_frame(
                    &mut std::io::stdout(),
                    &Envelope {
                        protocol_version: VERSION,
                        job_id: "pipe-complete".into(),
                        payload: Some(payload),
                    },
                )
                .unwrap();
            }
            std::process::exit(0);
        }
        std::io::stdout().write_all(&[1, 0]).unwrap();
        std::io::stdout().flush().unwrap();
        eprintln!("owned-worker-stderr");
        if std::env::var("KNOW_CODE_PIPE_MODE").unwrap() == "hold" {
            let until = Instant::now() + Duration::from_secs(10);
            while !root.join("release").exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        std::process::exit(0); // no harness epilogue in the framed stream
    }
    fn framed<R: Read>(reader: R) -> BufReader<R> {
        let mut reader = BufReader::new(reader);
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "--worker-stream--\n" {
                return reader;
            }
        }
    }
    #[test]
    fn owned_exit_finishes_partial_frame_and_stderr_despite_live_escaped_pipe_holder() {
        let fixture = Fixture::new();
        let control = Control::default();
        let mut worker = control.spawn(&mut fixture.command("exit")).unwrap();
        let reaped = Arc::new(AtomicBool::new(false));
        let pid = worker.child.id();
        let stdout =
            WorkerPipe::new(worker.child.stdout.take().unwrap(), &control, pid, &reaped).unwrap();
        let mut stderr =
            WorkerPipe::new(worker.child.stderr.take().unwrap(), &control, pid, &reaped).unwrap();
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut stdout = framed(stdout);
            send.send(protocol::read_frame(&mut stdout)).unwrap();
        });
        fixture.ready();
        let error = receive
            .recv_timeout(Duration::from_secs(3))
            .expect("escaped stdout holder postponed worker EOF")
            .unwrap_err();
        assert!(
            matches!(error,Error::Io(error) if error.kind()==std::io::ErrorKind::UnexpectedEof)
        );
        assert!(
            !fixture.0.join("retainer-done").exists(),
            "EOF must not depend on escaped holder exit"
        );
        let mut details = String::new();
        stderr.read_to_string(&mut details).unwrap();
        assert!(details.contains("owned-worker-stderr"));
        assert!(worker.wait().unwrap().success());
        reaped.store(true, Ordering::Release);
        reader.join().unwrap();
        fixture.release();
        assert!(fixture.0.join("retainer-done").exists());
    }
    #[test]
    fn complete_stream_survives_fast_exit_pause_and_late_escaped_output() {
        for (size, paused) in [
            (128, false),
            (2 * 1024 * 1024, false),
            (2 * 1024 * 1024, true),
        ] {
            let fixture = Fixture::new();
            let control = Control::test_managed();
            let mode = if size == 128 {
                "complete-small"
            } else {
                "complete"
            };
            let mut worker = control.spawn(&mut fixture.command(mode)).unwrap();
            let reaped = Arc::new(AtomicBool::new(false));
            let pid = worker.child.id();
            let mut stdout =
                WorkerPipe::new(worker.child.stdout.take().unwrap(), &control, pid, &reaped)
                    .unwrap();
            fixture.ready();
            if size == 128 {
                let until = Instant::now() + Duration::from_secs(3);
                while !stdout.exited().unwrap() {
                    assert!(
                        Instant::now() < until,
                        "small complete stream leader did not exit"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let mut stdout = framed(stdout);
            if paused {
                // The 2 MiB synchronous frame exceeds pipe capacity, keeping the
                // leader alive until this reader starts consuming its payload.
                assert_eq!(unsafe { libc::kill(-(pid as i32), libc::SIGSTOP) }, 0);
                let until = Instant::now() + Duration::from_secs(3);
                loop {
                    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
                    assert_eq!(
                        unsafe {
                            libc::waitid(
                                libc::P_PID,
                                pid,
                                info.as_mut_ptr(),
                                libc::WSTOPPED | libc::WNOWAIT | libc::WNOHANG,
                            )
                        },
                        0
                    );
                    let info = unsafe { info.assume_init() };
                    if info.si_pid != 0 {
                        assert_eq!(info.si_code, libc::CLD_STOPPED);
                        break;
                    }
                    assert!(Instant::now() < until, "owned leader did not stop");
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(
                    !stdout.get_mut().exited().unwrap(),
                    "WEXITED reported a stopped leader"
                );
                control.test_pause(true);
            }
            let (send, receive) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                let mut messages = Vec::new();
                while let Some(message) = protocol::read_frame(&mut stdout).unwrap() {
                    messages.push(message);
                }
                send.send((messages, stdout)).unwrap();
            });
            if paused {
                let paused_result = receive.recv_timeout(Duration::from_millis(30));
                let resumed = unsafe { libc::kill(-(pid as i32), libc::SIGCONT) };
                control.test_pause(false);
                assert!(matches!(
                    paused_result,
                    Err(mpsc::RecvTimeoutError::Timeout)
                ));
                assert_eq!(resumed, 0);
            }
            let (messages, mut stdout) = receive
                .recv_timeout(Duration::from_secs(3))
                .expect("complete worker stream did not reach bounded EOF");
            reader.join().unwrap();
            assert_eq!(messages.len(), 3);
            assert!(
                messages
                    .iter()
                    .all(|message| message.job_id == "pipe-complete")
            );
            assert!(
                matches!(&messages[0].payload, Some(Payload::Provider(info)) if info.language == "typescript")
            );
            assert!(
                matches!(&messages[1].payload, Some(Payload::Source(source)) if source.utf8 == vec![b'b'; size])
            );
            assert!(matches!(&messages[2].payload, Some(Payload::Complete(done)) if done.success));
            assert!(
                !fixture.0.join("retainer-done").exists(),
                "EOF waited for escaped holder"
            );
            assert!(worker.wait().unwrap().success());
            reaped.store(true, Ordering::Release);
            fixture.release();
            assert!(fixture.0.join("retainer-done").exists());
            assert_eq!(
                stdout.read(&mut [0; 64]).unwrap(),
                0,
                "late descendant output became worker protocol"
            );
        }
    }
    #[test]
    fn cancellation_unblocks_full_stdin_and_retained_stdout_stderr_and_releases_the_slot() {
        let fixture = Fixture::new();
        let governor = crate::resource::Governor::new(crate::resource::Limits {
            cpu_cores: 64.0,
            memory_mib: 65536,
            ..Default::default()
        })
        .unwrap();
        let session = governor.start().unwrap();
        let control = session.control();
        let mut worker = control.spawn(&mut fixture.command("hold")).unwrap();
        let reaped = Arc::new(AtomicBool::new(false));
        let pid = worker.child.id();
        let mut stdin =
            WorkerPipe::new(worker.child.stdin.take().unwrap(), &control, pid, &reaped).unwrap();
        let stdout =
            WorkerPipe::new(worker.child.stdout.take().unwrap(), &control, pid, &reaped).unwrap();
        let mut stderr =
            WorkerPipe::new(worker.child.stderr.take().unwrap(), &control, pid, &reaped).unwrap();
        fixture.ready();
        let mut filled = 0;
        loop {
            match stdin.pipe.write(&[0; 8192]) {
                Ok(n) => {
                    filled += n;
                    assert!(filled <= 2 * 1024 * 1024, "fixture pipe never became full");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("fixture stdin fill failed: {error}"),
            }
        }
        let (send, receive) = mpsc::channel();
        let out = send.clone();
        let (started, ready) = mpsc::channel();
        let stdout_thread = std::thread::spawn(move || {
            let mut stdout = framed(stdout);
            started.send(()).unwrap();
            let result = protocol::read_frame(&mut stdout);
            out.send(("stdout", result.is_err())).unwrap();
        });
        let err = send.clone();
        let stderr_thread = std::thread::spawn(move || {
            let mut bytes = vec![];
            let result = stderr.read_to_end(&mut bytes);
            err.send(("stderr", result.is_err())).unwrap();
        });
        let stdin_thread = std::thread::spawn(move || {
            send.send(("stdin", stdin.write_all(b"x").is_err()))
                .unwrap();
        });
        ready
            .recv_timeout(Duration::from_secs(3))
            .expect("stdout reader did not reach the partial frame");
        assert!(control.cancel());
        for _ in 0..3 {
            let (stream, stopped) = receive
                .recv_timeout(Duration::from_secs(3))
                .expect("cancelled retained pipe did not release");
            assert!(stopped, "{stream}");
        }
        assert!(matches!(control.check(), Err(Error::Cancelled)));
        worker.wait().unwrap();
        reaped.store(true, Ordering::Release);
        stdout_thread.join().unwrap();
        stderr_thread.join().unwrap();
        stdin_thread.join().unwrap();
        assert!(!fixture.0.join("retainer-done").exists());
        drop(session);
        let next = governor
            .start()
            .expect("stopped worker retained the shared analysis slot");
        drop(next);
        fixture.release();
        assert!(fixture.0.join("retainer-done").exists());
    }
}
