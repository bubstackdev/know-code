//! Execution limits: deadlines for child processes, and cancellation of the operation a thread is running.
//!
//! Output limits (`max_chars`, `limit`) cap what an answer shows, not what computing it costs. Here a child process
//! (a `git` command, a search run in a child) gets a deadline by its kind, and is killed when it passes it or when the
//! operation running it is cancelled (an MCP client's `notifications/cancelled`). Each cut is recorded, so the answer
//! says it is incomplete (reason `deadline`) instead of passing an empty git log off as a repo without history; see
//! [`take_cuts`]. One default per kind, each overridden by an environment variable (seconds, `0` for none):
//! `KNOWCODE_GIT_DEADLINE` (git commands, 60 s) and `KNOWCODE_SEARCH_DEADLINE` (a search child, 120 s).
//! Explicit client registration commands use `KNOWCODE_SETUP_DEADLINE` (120 s).

use std::cell::RefCell;
use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a child process does, which sets its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A `git` command.
    Git,
    /// A search (`search`, `grep-ast`) run in a child process.
    Search,
    /// An explicitly requested local client registration command.
    Setup,
    /// A bounded, notify-only release metadata request.
    UpdateCheck,
    /// An explicitly requested update download or installer.
    UpdateInstall,
}

impl Kind {
    /// The environment variable overriding the deadline, and its default in seconds.
    fn setting(self) -> (&'static str, u64) {
        match self {
            Kind::Git => ("KNOWCODE_GIT_DEADLINE", 60),
            Kind::Search => ("KNOWCODE_SEARCH_DEADLINE", 120),
            Kind::Setup => ("KNOWCODE_SETUP_DEADLINE", 120),
            Kind::UpdateCheck => ("KNOWCODE_UPDATE_CHECK_DEADLINE", 8),
            Kind::UpdateInstall => ("KNOWCODE_UPDATE_DEADLINE", 180),
        }
    }

    /// The deadline for one child of this kind; `None` when the variable is `0` (no deadline).
    pub fn deadline(self) -> Option<Duration> {
        let (var, default) = self.setting();
        let secs = std::env::var(var).ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(default);
        (secs > 0).then(|| Duration::from_secs(secs))
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Git => "git",
            Kind::Search => "search",
            Kind::Setup => "client setup",
            Kind::UpdateCheck => "update check",
            Kind::UpdateInstall => "self-update",
        }
    }
}

/// A child process stopped before it finished. Errors carrying it can be told from other failures with
/// `anyhow::Error::downcast_ref::<Cut>()`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Cut {
    /// `deadline`, or `cancelled` when the operation was cancelled.
    pub reason: &'static str,
    /// What was stopped and after how long.
    pub detail: String,
    /// The environment variable raising the deadline.
    pub hint: String,
}

impl std::fmt::Display for Cut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.detail, self.reason)
    }
}

impl std::error::Error for Cut {}

/// The operation a thread runs: whether it was cancelled, and the cuts made while running it.
#[derive(Default)]
pub struct Operation {
    cancelled: AtomicBool,
    cuts: Mutex<Vec<Cut>>,
}

impl Operation {
    pub fn new() -> Arc<Operation> {
        Arc::new(Operation::default())
    }

    /// Cancels the operation: its running child is killed within [`POLL`], and no new one starts.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Operation>>> = const { RefCell::new(None) };
}

/// Cuts made on threads running no operation (workers an operation spawned, a CLI command): taken by the next
/// [`take_cuts`] anywhere.
static LOOSE: Mutex<Vec<Cut>> = Mutex::new(Vec::new());

/// Runs this thread's work as `op` until the guard drops.
pub fn enter(op: Arc<Operation>) -> Entered {
    let before = CURRENT.with(|c| c.replace(Some(op)));
    Entered(before)
}

/// Restores the thread's previous operation when dropped (see [`enter`]).
pub struct Entered(Option<Arc<Operation>>);

impl Drop for Entered {
    fn drop(&mut self) {
        let before = self.0.take();
        CURRENT.with(|c| *c.borrow_mut() = before);
    }
}

fn current() -> Option<Arc<Operation>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Whether this thread's operation was cancelled.
pub fn cancelled() -> bool {
    current().is_some_and(|op| op.is_cancelled())
}

/// An error when this thread's operation was cancelled: for the points where long work can stop early (before
/// opening the next repo's index).
pub fn check() -> anyhow::Result<()> {
    if cancelled() {
        return Err(anyhow::Error::new(Cut { reason: "cancelled", detail: "the request was cancelled".into(), hint: String::new() }));
    }
    Ok(())
}

/// Every cut made in the process so far, never reset: a value computed while it moved may be missing what a cut
/// child would have given, so it is not cached (see [`cuts_so_far`]).
static CUT_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many cuts the process has made. A cache compares it before and after computing a value, and keeps the value
/// only when it didn't move.
pub fn cuts_so_far() -> usize {
    CUT_COUNT.load(Ordering::Relaxed)
}

fn record(cut: Cut) {
    CUT_COUNT.fetch_add(1, Ordering::Relaxed);
    match current() {
        Some(op) => op.cuts.lock().unwrap_or_else(|e| e.into_inner()).push(cut),
        None => LOOSE.lock().unwrap_or_else(|e| e.into_inner()).push(cut),
    }
}

/// The cuts made since the last call, by this thread's operation and on threads running none; each once.
pub fn take_cuts() -> Vec<Cut> {
    let mut out = current().map(|op| std::mem::take(&mut *op.cuts.lock().unwrap_or_else(|e| e.into_inner()))).unwrap_or_default();
    out.append(&mut LOOSE.lock().unwrap_or_else(|e| e.into_inner()));
    out.dedup();
    out
}

/// How often a waiting child is checked for its deadline and for cancellation.
pub const POLL: Duration = Duration::from_millis(20);

/// Runs `cmd` to completion like [`Command::output`], with `input` on its standard input, within `kind`'s deadline:
/// past it, or once this thread's operation is cancelled, the child is killed, the cut recorded (see [`take_cuts`])
/// and a [`Cut`] returned as the error. `what` names the command in the cut's detail.
pub fn output(mut cmd: Command, kind: Kind, what: &str, input: Option<&[u8]>) -> anyhow::Result<Output> {
    let op = current();
    if op.as_ref().is_some_and(|o| o.is_cancelled()) {
        return Err(check().expect_err("cancelled"));
    }
    // Update installers can spawn curl/tar. Give this explicitly requested child its own group, so a timeout
    // can clean up its descendants without signalling any pre-existing terminal session or process.
    if kind == Kind::UpdateInstall {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    // Pipes drained on threads: a child filling one would otherwise stall until killed.
    let stdin = child.stdin.take();
    let writer = input.map(|i| i.to_vec()).zip(stdin).map(|(bytes, mut pipe)| {
        std::thread::spawn(move || {
            use std::io::Write;
            // A child killed early closes the pipe: that write error is the cut's, not reported.
            let _ = pipe.write_all(&bytes);
        })
    });
    // Each drain says when its pipe closed, which is when the child exits (or closes it): a quick child is seen to
    // finish at once, not at the next poll.
    let (closed_tx, closed) = std::sync::mpsc::channel::<()>();
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let closed_tx = closed_tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = closed_tx.send(());
            buf
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    drop(closed_tx);
    let started = Instant::now();
    let deadline = kind.deadline();
    let stopped = wait(&mut child, &closed, started, deadline, op.as_deref())?;
    let status = match stopped {
        Ok(status) => status,
        Err(reason) => {
            if kind == Kind::UpdateInstall {
                unsafe extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
                // SAFETY: spawn above created a new group led by this unreaped child. No existing job joins it.
                unsafe { kill(-(child.id() as i32), 9); }
            }
            let _ = child.kill();
            let _ = child.wait();
            let (var, _) = kind.setting();
            let cut = Cut {
                reason,
                detail: match reason {
                    "deadline" => format!("{what} stopped at its {}s deadline ({} child)", deadline.map_or(0, |d| d.as_secs()), kind.name()),
                    _ => format!("{what} stopped: the request was cancelled"),
                },
                hint: if reason == "deadline" { format!("raise {var} (seconds; 0 for no deadline)") } else { String::new() },
            };
            record(cut.clone());
            return Err(anyhow::Error::new(cut));
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok(Output { status, stdout, stderr })
}

/// Waits for `child` to exit; `Err(reason)` when it must be stopped first.
fn wait(
    child: &mut Child,
    closed: &std::sync::mpsc::Receiver<()>,
    started: Instant,
    deadline: Option<Duration>,
    op: Option<&Operation>,
) -> std::io::Result<Result<std::process::ExitStatus, &'static str>> {
    let mut open = 2;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Ok(status));
        }
        if op.is_some_and(Operation::is_cancelled) {
            return Ok(Err("cancelled"));
        }
        if deadline.is_some_and(|d| started.elapsed() >= d) {
            return Ok(Err("deadline"));
        }
        if open > 0 {
            // Until both pipes close, wake when one does.
            match closed.recv_timeout(POLL) {
                Ok(()) => open -= 1,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => open = 0,
            }
        } else {
            // Both closed: the child is exiting, or runs on without output.
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sleeper(secs: &str) -> Command {
        let mut c = Command::new("sleep");
        c.arg(secs);
        c
    }

    /// One test for the deadline setting: the environment is process-wide.
    #[test]
    fn invariant_a_child_past_its_deadline_or_cancelled_is_killed_and_the_cut_recorded() {
        // SAFETY: only this test reads the variable.
        unsafe { std::env::set_var("KNOWCODE_SEARCH_DEADLINE", "1") };
        let op = Operation::new();
        let _in = enter(op.clone());
        let t = Instant::now();
        let err = output(sleeper("30"), Kind::Search, "sleep", None).unwrap_err();
        assert!(t.elapsed() < Duration::from_secs(10), "killed at the deadline, not after the child finished");
        let cut = err.downcast_ref::<Cut>().expect("a cut");
        assert_eq!(cut.reason, "deadline");
        assert!(cut.hint.contains("KNOWCODE_SEARCH_DEADLINE"), "{}", cut.hint);
        assert_eq!(take_cuts(), vec![cut.clone()]);
        assert!(take_cuts().is_empty(), "each cut is taken once");

        // A child that finishes in time is untouched.
        let ok = output(Command::new("true"), Kind::Search, "true", None).unwrap();
        assert!(ok.status.success());
        assert!(take_cuts().is_empty());

        // Cancelled from another thread while waiting: killed within a poll or two.
        let other = op.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            other.cancel();
        });
        unsafe { std::env::set_var("KNOWCODE_SEARCH_DEADLINE", "0") };
        let t = Instant::now();
        let err = output(sleeper("30"), Kind::Search, "sleep", None).unwrap_err();
        canceller.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(10));
        assert_eq!(err.downcast_ref::<Cut>().map(|c| c.reason), Some("cancelled"));
        // Nothing new starts once cancelled.
        assert!(check().is_err());
        assert!(output(Command::new("true"), Kind::Search, "true", None).is_err());
        unsafe { std::env::remove_var("KNOWCODE_SEARCH_DEADLINE") };
    }

    #[test]
    fn input_and_output_pass_through() {
        let mut c = Command::new("cat");
        c.arg("-");
        let out = output(c, Kind::Git, "cat", Some(b"hello")).unwrap();
        assert_eq!(out.stdout, b"hello");
    }
}
