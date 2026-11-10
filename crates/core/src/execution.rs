//! One monotonic budget for query admission, reads, projection and encoding.
use crate::{Error, Result};
use serde::Serialize;
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::Path,
    time::{Duration, Instant},
};

pub const DEFAULT_DEADLINE_MS: u32 = 5_000;
pub const MAX_DEADLINE_MS: u32 = 120_000;
pub const QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_RESPONSE_BYTES: u32 = 256 * 1024;
pub const MAX_RESPONSE_BYTES: u32 = 2 * 1024 * 1024;
pub const MIN_RESPONSE_BYTES: u32 = 1024;

pub(crate) fn response_budget(value: Option<u32>) -> Result<usize> {
    let bytes = value.unwrap_or(DEFAULT_RESPONSE_BYTES);
    if !(MIN_RESPONSE_BYTES..=MAX_RESPONSE_BYTES).contains(&bytes) {
        return Err(Error::InvalidRequest(format!(
            "response_max_bytes must be between {MIN_RESPONSE_BYTES} and {MAX_RESPONSE_BYTES}"
        )));
    }
    Ok(bytes as usize)
}

/// Capture before dispatching asynchronous native work. Host wrappers may charge
/// time already spent preparing/queuing the call without sharing clock epochs.
#[derive(Clone, Copy, Debug)]
pub struct QueryStart {
    entered: Instant,
    elapsed: Duration,
}
impl Default for QueryStart {
    fn default() -> Self {
        Self::with_elapsed_ms(0)
    }
}
impl QueryStart {
    pub fn with_elapsed_ms(elapsed: u64) -> Self {
        Self {
            entered: Instant::now(),
            elapsed: Duration::from_millis(elapsed),
        }
    }
    fn elapsed(self) -> Duration {
        self.elapsed + self.entered.elapsed()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Execution {
    start: QueryStart,
    timeout: Duration,
}
impl Execution {
    pub(crate) fn new(start: QueryStart, deadline: Option<u32>) -> Result<Self> {
        let deadline = deadline.unwrap_or(DEFAULT_DEADLINE_MS);
        if deadline > MAX_DEADLINE_MS {
            return Err(Error::InvalidRequest(
                "deadline_ms cannot exceed 120000".into(),
            ));
        }
        Ok(Self {
            start,
            timeout: Duration::from_millis(u64::from(deadline)),
        })
    }
    pub(crate) fn remaining(self) -> Result<Duration> {
        self.timeout
            .checked_sub(self.start.elapsed())
            .filter(|d| !d.is_zero())
            .ok_or(Error::DeadlineExceeded)
    }
    pub(crate) fn check(self) -> Result<()> {
        self.remaining().map(|_| ())
    }
    pub(crate) fn phase(self, maximum: Duration) -> Self {
        Self {
            timeout: self.timeout.min(self.start.elapsed() + maximum),
            ..self
        }
    }
}

/// A local socket connect must not sit in a full listen queue past the budget.
pub(crate) fn connect(path: &Path, execution: Execution) -> Result<UnixStream> {
    execution.check()?;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is an ordinary zero-initializable C address; its
    // initialized length and NUL-terminated pathname are checked below.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(Error::InvalidRequest(
            "invalid or overlong coordinator socket path".into(),
        ));
    }
    address.sun_family = libc::AF_UNIX as _;
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as _;
    }
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = length as u8;
    }
    // SAFETY: socket returns a new descriptor, transferred to exactly one owner.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    #[cfg(target_os = "macos")]
    {
        // Match std's socket construction without changing an embedding host's
        // signal handlers: a peer-close must be an I/O error, never SIGPIPE.
        let enabled: libc::c_int = 1;
        if unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&enabled as *const libc::c_int).cast(),
                std::mem::size_of_val(&enabled) as _,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(true)?;
    // SAFETY: pointer addresses the initialized address for the stated length.
    let result = unsafe {
        libc::connect(
            stream.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS)
            && error.kind() != std::io::ErrorKind::WouldBlock
        {
            return Err(error.into());
        }
        ready(&stream, libc::POLLOUT, execution)?;
        if let Some(error) = stream.take_error()? {
            return Err(error.into());
        }
    }
    execution.check()?;
    Ok(stream)
}

pub(crate) struct TimedStream {
    stream: UnixStream,
    execution: Execution,
}
impl TimedStream {
    pub(crate) fn new(stream: UnixStream, execution: Execution) -> Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self { stream, execution })
    }
}
fn io_remaining(execution: Execution) -> std::io::Result<Duration> {
    execution
        .remaining()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::TimedOut, e))
}
fn ready(stream: &UnixStream, events: i16, execution: Execution) -> std::io::Result<()> {
    loop {
        let timeout = io_remaining(execution)?
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: poll receives one initialized descriptor for this owned stream.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        io_remaining(execution)?;
        if result > 0 {
            return Ok(());
        }
    }
}
impl Read for TimedStream {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            ready(&self.stream, libc::POLLIN, self.execution)?;
            match self.stream.read(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                result => return result,
            }
        }
    }
}
impl Write for TimedStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            ready(&self.stream, libc::POLLOUT, self.execution)?;
            match self.stream.write(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        io_remaining(self.execution)?;
        Ok(())
    }
}

fn write_json<W: Write>(
    value: &impl Serialize,
    execution: Option<Execution>,
    maximum: usize,
    output: W,
) -> Result<W> {
    struct Encoder<W> {
        output: W,
        execution: Option<Execution>,
        remaining: usize,
        exceeded: bool,
    }
    impl<W: Write> Write for Encoder<W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(execution) = self.execution {
                io_remaining(execution)?;
            }
            if bytes.is_empty() {
                return Ok(0);
            }
            if self.remaining == 0 {
                self.exceeded = true;
                return Err(std::io::Error::other("response byte budget exceeded"));
            }
            let count = self
                .output
                .write(&bytes[..bytes.len().min(16 * 1024).min(self.remaining)])?;
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if let Some(execution) = self.execution {
                io_remaining(execution)?;
            }
            Ok(())
        }
    }
    let mut writer = Encoder {
        output,
        execution,
        remaining: maximum,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut writer, value);
    if let Some(execution) = execution {
        execution.check()?;
    }
    if writer.exceeded {
        return Err(Error::ResponseLimitExceeded);
    }
    result?;
    Ok(writer.output)
}

#[derive(Serialize)]
struct Response<'a, T> {
    result: &'a T,
}

pub(crate) fn check_response(
    value: &impl Serialize,
    execution: Execution,
    maximum: usize,
) -> Result<()> {
    write_json(
        &Response { result: value },
        Some(execution),
        maximum,
        std::io::sink(),
    )
    .map(|_| ())
}

pub(crate) fn encode(
    value: &impl Serialize,
    execution: Execution,
    maximum: usize,
) -> Result<String> {
    let bytes = write_json(
        &Response { result: value },
        Some(execution),
        maximum,
        Vec::new(),
    )?;
    let result = String::from_utf8(bytes).expect("JSON serializer emits UTF-8");
    execution.check()?;
    Ok(result)
}

pub(crate) fn error_json(code: &str, message: &str, maximum: usize) -> String {
    #[derive(Serialize)]
    struct Failure<'a> {
        error: Details<'a>,
    }
    #[derive(Serialize)]
    struct Details<'a> {
        code: &'a str,
        message: &'a str,
    }
    let result = write_json(
        &Failure {
            error: Details { code, message },
        },
        None,
        maximum,
        Vec::new(),
    );
    match result {
        Ok(bytes) => String::from_utf8(bytes).expect("JSON serializer emits UTF-8"),
        Err(_) => r#"{"error":{"code":"response_limit_exceeded","message":"error details exceed response_max_bytes"}}"#.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_response_budget_counts_framing_escaping_and_metadata() {
        let value = serde_json::json!({"metadata":{"coverage":"x".repeat(512)},"source":"\0\n\\\"😀".repeat(512)});
        let expected = serde_json::to_vec(&serde_json::json!({"result":value})).unwrap();
        let execution = Execution::new(Default::default(), None).unwrap();
        assert_eq!(
            encode(&value, execution, expected.len())
                .unwrap()
                .as_bytes(),
            expected
        );
        assert!(matches!(
            encode(&value, execution, expected.len() - 1),
            Err(Error::ResponseLimitExceeded)
        ));
        let emoji = std::str::from_utf8(&expected).unwrap().find('😀').unwrap();
        for maximum in emoji + 1..emoji + 4 {
            assert!(matches!(
                encode(&value, execution, maximum),
                Err(Error::ResponseLimitExceeded)
            ));
        }
        assert!(check_response(&value, execution, expected.len()).is_ok());
        assert!(matches!(
            check_response(&value, execution, expected.len() - 1),
            Err(Error::ResponseLimitExceeded)
        ));
        assert_eq!(response_budget(None).unwrap(), 262_144);
        assert_eq!(response_budget(Some(2_097_152)).unwrap(), 2_097_152);
        for invalid in [0, 1023, 2_097_153] {
            assert!(response_budget(Some(invalid)).is_err());
        }
        let error = error_json("invalid_request", &"\0😀".repeat(2048), 1024);
        assert!(error.len() <= 1024);
        let error: serde_json::Value = serde_json::from_str(&error).unwrap();
        assert_eq!(error["error"]["code"], "response_limit_exceeded");
        let request = serde_json::json!({"x".repeat(300_000):true}).to_string();
        let rejected = crate::query_json("unused", &request);
        assert!(rejected.len() <= DEFAULT_RESPONSE_BYTES as usize);
        let rejected: serde_json::Value = serde_json::from_str(&rejected).unwrap();
        assert_eq!(rejected["error"]["code"], "response_limit_exceeded");
        let request =
            serde_json::json!({"response_max_bytes":1024,"x".repeat(5000):true}).to_string();
        let rejected = crate::query_json("unused", &request);
        assert!(
            rejected.len() <= 1024,
            "pre-validation error ignored requested minimum"
        );
        let rejected: serde_json::Value = serde_json::from_str(&rejected).unwrap();
        assert_eq!(rejected["error"]["code"], "response_limit_exceeded");
        let expired = Execution::new(Default::default(), Some(0)).unwrap();
        assert!(matches!(
            encode(&value, expired, 1),
            Err(Error::DeadlineExceeded)
        ));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn owned_socket_does_not_raise_sigpipe_in_an_embedding_host() {
        const CHILD: &str = "KNOW_CODE_TEST_SIGPIPE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "execution::tests::owned_socket_does_not_raise_sigpipe_in_an_embedding_host",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{:?}\n{}\n{}",
                result.status,
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.artifacts/qd");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(format!("sigpipe-{}", std::process::id()));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let peer = std::thread::spawn(move || {
            drop(listener.accept().unwrap());
        });
        let execution = Execution::new(Default::default(), None).unwrap();
        let mut stream = TimedStream::new(connect(&path, execution).unwrap(), execution).unwrap();
        peer.join().unwrap();
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        // Only this isolated fixture child changes its own signal disposition.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        assert_eq!(
            stream.write(b"closed peer").unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn timed_connect_exchanges_a_complete_frame() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.artifacts/qd");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(format!(
            "{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0; 4];
            stream.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"ping");
            stream.write_all(b"pong").unwrap();
        });
        let execution = Execution::new(Default::default(), None).unwrap();
        let stream = connect(&path, execution).unwrap();
        let mut stream = TimedStream::new(stream, execution).unwrap();
        stream.write_all(b"ping").unwrap();
        peer.join().unwrap();
        let mut bytes = [0; 4];
        stream.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"pong");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn defaults_and_elapsed_admission_share_one_budget() {
        let execution = Execution::new(QueryStart::with_elapsed_ms(5_000), None).unwrap();
        assert!(matches!(execution.check(), Err(Error::DeadlineExceeded)));
        assert!(Execution::new(Default::default(), Some(120_001)).is_err());
        assert!(matches!(
            Execution::new(Default::default(), Some(0)).unwrap().check(),
            Err(Error::DeadlineExceeded)
        ));
        let execution = Execution::new(QueryStart::with_elapsed_ms(40), Some(100)).unwrap();
        assert!(
            execution
                .phase(Duration::from_secs(10))
                .remaining()
                .unwrap()
                <= Duration::from_millis(60)
        );
    }
    #[test]
    fn expiry_during_encoding_never_returns_partial_success() {
        struct Slow;
        impl Serialize for Slow {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                std::thread::sleep(Duration::from_millis(20));
                serializer.serialize_str("must not escape as success")
            }
        }
        let execution = Execution::new(Default::default(), Some(5)).unwrap();
        assert!(matches!(
            encode(&Slow, execution, DEFAULT_RESPONSE_BYTES as usize),
            Err(Error::DeadlineExceeded)
        ));
        let execution = Execution::new(Default::default(), None).unwrap();
        let text = "😀\\\"".repeat(20_000);
        let result: serde_json::Value = serde_json::from_str(
            &encode(&text, execution, DEFAULT_RESPONSE_BYTES as usize).unwrap(),
        )
        .unwrap();
        assert_eq!(result["result"], text);
    }
    #[test]
    fn trickling_socket_bytes_do_not_restart_the_deadline() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let producer = std::thread::spawn(move || {
            for byte in [8, 0, 0, 0, 8, 7, 8, 7, 8, 7, 8, 7] {
                if peer.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        let execution = Execution::new(Default::default(), Some(50)).unwrap();
        let mut reader = TimedStream::new(stream, execution).unwrap();
        assert!(crate::protocol::read_frame(&mut reader).is_err());
        assert!(matches!(execution.check(), Err(Error::DeadlineExceeded)));
        drop(reader);
        producer.join().unwrap();
    }
}
