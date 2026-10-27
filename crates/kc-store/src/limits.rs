//! Execution limits: deadlines for child processes, and the per-query context ([`Operation`]) a thread runs: its
//! cancellation, its freshness wait, what it collects for the answer's envelope (cut children, repos left out of a
//! fan-out, gone checkouts that mattered, repos answered from a cached summary), and the repos whose indexes it used.
//!
//! One query is one [`Operation`], created where the query starts (an MCP tool call, a CLI command) and entered for
//! the thread doing its work ([`enter`]). Everything the answer collects on the side is kept in it, never per thread,
//! so two queries never see each other's entries, wherever they run, and a query's entries go with it to any thread
//! entering it. A thread that entered none runs as its own operation, made on first use (a CLI command, a test).
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
use std::path::{Path, PathBuf};
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

/// One query's context: whether it was cancelled, how long it may wait for indexing, and what it collected on the
/// side for its answer's envelope, each taken once by the code finishing the answer.
#[derive(Default)]
pub struct Operation {
    /// Shared with the operations made [`Operation::sharing_cancellation`] of this one.
    cancelled: Arc<AtomicBool>,
    /// A thread's own operation, made because it entered none: its cuts are the process's loose ones (see [`LOOSE`]).
    own: bool,
    cuts: Mutex<Vec<Cut>>,
    /// When waiting for a repo's indexing gives up, answering from the committed snapshot; `None` waits for it.
    freshness: Mutex<Option<Instant>>,
    /// The `incomplete` entries for repos a fan-out left out because they could not be read.
    left_out: Mutex<Vec<crate::answer::Incomplete>>,
    /// Gone checkouts that would have mattered to the answer.
    missing: Mutex<Vec<PathBuf>>,
    /// Whether the answer's rows quote the lines they rest on (see [`crate::answer::Cite`]).
    cite: Mutex<crate::answer::CiteMode>,
    /// Repos whose summary the query read from the cache rather than a current index, with the summary's generation.
    cached_reads: Mutex<std::collections::HashMap<PathBuf, String>>,
    /// Repos whose indexes the query used, for cache upkeep after it.
    touched: Mutex<std::collections::BTreeSet<PathBuf>>,
    /// How many indexes the query has opened (see [`Operation::note_opened`]).
    opened: std::sync::atomic::AtomicUsize,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Operation {
    pub fn new() -> Arc<Operation> {
        Arc::new(Operation::default())
    }

    /// A new operation, with nothing collected, cancelled whenever this one is: one query of several a request runs.
    pub fn sharing_cancellation(&self) -> Arc<Operation> {
        Arc::new(Operation { cancelled: self.cancelled.clone(), ..Operation::default() })
    }

    /// Cancels the operation: its running child is killed within [`POLL`], and no new one starts.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// An error when the operation was cancelled: for the points where long work can stop early.
    pub fn check(&self) -> anyhow::Result<()> {
        if self.is_cancelled() {
            return Err(anyhow::Error::new(Cut { reason: "cancelled", detail: "the request was cancelled".into(), hint: String::new() }));
        }
        Ok(())
    }

    /// Until when the operation waits for a repo's indexing; `None` waits until it is done.
    pub fn freshness_deadline(&self) -> Option<Instant> {
        *lock(&self.freshness)
    }

    /// Sets [`Operation::freshness_deadline`]; returns the one it replaces.
    pub fn set_freshness_deadline(&self, deadline: Option<Instant>) -> Option<Instant> {
        std::mem::replace(&mut *lock(&self.freshness), deadline)
    }

    /// Whether the answer's rows quote the lines they rest on; [`crate::answer::CiteMode::Lines`] unless set.
    pub fn cite(&self) -> crate::answer::CiteMode {
        *lock(&self.cite)
    }

    /// Sets [`Operation::cite`] for the rest of the query.
    pub fn set_cite(&self, mode: crate::answer::CiteMode) {
        *lock(&self.cite) = mode;
    }

    /// Records that the answer leaves a repo out, as `entry` says.
    pub fn leave_out(&self, entry: crate::answer::Incomplete) {
        lock(&self.left_out).push(entry);
    }

    /// The entries [`Operation::leave_out`] recorded since the last call.
    pub fn take_left_out(&self) -> Vec<crate::answer::Incomplete> {
        std::mem::take(&mut *lock(&self.left_out))
    }

    /// Records that the gone checkout at `root` would have mattered to the answer; each once.
    pub fn note_missing(&self, root: &Path) {
        let mut m = lock(&self.missing);
        if !m.iter().any(|r| r == root) {
            m.push(root.to_path_buf());
        }
    }

    /// The checkouts [`Operation::note_missing`] recorded since the last call, sorted.
    pub fn take_missing(&self) -> Vec<PathBuf> {
        let mut out = std::mem::take(&mut *lock(&self.missing));
        out.sort();
        out
    }

    /// Records that the answer read the cached summary of the repo at `root`, of `generation`; the first read counts.
    pub fn note_cached_read(&self, root: &Path, generation: &str) {
        lock(&self.cached_reads).entry(root.to_path_buf()).or_insert_with(|| generation.to_string());
    }

    /// The repos [`Operation::note_cached_read`] recorded, with their summaries' generations.
    pub fn cached_reads(&self) -> std::collections::HashMap<PathBuf, String> {
        lock(&self.cached_reads).clone()
    }

    /// Records that the query used the index of the repo at `root`.
    /// Counts an index the query opens; returns how many it has opened, this one included.
    pub fn note_opened(&self) -> usize {
        self.opened.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn note_touched(&self, root: &Path) {
        lock(&self.touched).insert(root.to_path_buf());
    }

    /// The repos [`Operation::note_touched`] recorded since the last call, sorted.
    pub fn take_touched(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *lock(&self.touched)).into_iter().collect()
    }

    /// `f`, with what it notes as missing dropped.
    pub fn quietly<T>(&self, f: impl FnOnce() -> T) -> T {
        let before = std::mem::take(&mut *lock(&self.missing));
        let out = f();
        *lock(&self.missing) = before;
        out
    }

    /// The cuts made since the last call, by this operation and on threads running none; each once.
    pub fn take_cuts(&self) -> Vec<Cut> {
        let mut out = if self.own { Vec::new() } else { std::mem::take(&mut *lock(&self.cuts)) };
        out.append(&mut lock(&LOOSE));
        out.dedup();
        out
    }
}

thread_local! {
    /// The operation the thread runs: the one entered (see [`enter`]), else its own, made on first use.
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

/// The operation this thread runs: the one entered, else the thread's own.
pub fn current() -> Arc<Operation> {
    CURRENT.with(|c| c.borrow_mut().get_or_insert_with(|| Arc::new(Operation { own: true, ..Operation::default() })).clone())
}

/// Whether this thread's operation was cancelled.
pub fn cancelled() -> bool {
    current().is_cancelled()
}

/// An error when this thread's operation was cancelled: for the points where long work can stop early (before
/// opening the next repo's index).
pub fn check() -> anyhow::Result<()> {
    current().check()
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
    let op = current();
    lock(if op.own { &LOOSE } else { &op.cuts }).push(cut);
}

/// The cuts made since the last call, by this thread's operation and on threads running none; each once (see
/// [`Operation::take_cuts`]).
pub fn take_cuts() -> Vec<Cut> {
    current().take_cuts()
}

/// How often a waiting child is checked for its deadline and for cancellation.
pub const POLL: Duration = Duration::from_millis(20);

/// Runs `cmd` to completion like [`Command::output`], with `input` on its standard input, within `kind`'s deadline:
/// past it, or once this thread's operation is cancelled, the child is killed, the cut recorded (see [`take_cuts`])
/// and a [`Cut`] returned as the error. `what` names the command in the cut's detail.
pub fn output(cmd: Command, kind: Kind, what: &str, input: Option<&[u8]>) -> anyhow::Result<Output> {
    let (out, cut) = run(cmd, kind, what, input, None)?;
    match cut {
        Some(cut) => Err(anyhow::Error::new(cut)),
        None => Ok(out),
    }
}

/// [`output`] for a child whose output is useful in part (a `git log` printing newest first): it stops at `until`
/// too, when that comes before `kind`'s deadline, and a child stopped at either returns what it printed so far with
/// the cut, which is recorded as [`output`] records it. A cancelled operation is still an error.
pub fn output_until(cmd: Command, kind: Kind, what: &str, until: Option<Instant>) -> anyhow::Result<(Output, Option<Cut>)> {
    run(cmd, kind, what, None, until)
}

fn run(mut cmd: Command, kind: Kind, what: &str, input: Option<&[u8]>, until: Option<Instant>) -> anyhow::Result<(Output, Option<Cut>)> {
    let op = current();
    op.check()?;
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
    // The earlier of the kind's deadline and `until`, as a duration from the start; `budget` says `until` came first.
    let left = until.map(|u| u.saturating_duration_since(started));
    let (limit, budget) = match (deadline, left) {
        (Some(d), Some(l)) if l < d => (Some(l), true),
        (None, Some(l)) => (Some(l), true),
        (d, _) => (d, false),
    };
    let stopped = wait(&mut child, &closed, started, limit, &op)?;
    let (status, cut) = match stopped {
        Ok(status) => (status, None),
        Err(reason) => {
            if kind == Kind::UpdateInstall {
                unsafe extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
                // SAFETY: spawn above created a new group led by this unreaped child. No existing job joins it.
                unsafe { kill(-(child.id() as i32), 9); }
            }
            let _ = child.kill();
            let status = child.wait()?;
            let (var, _) = kind.setting();
            let cut = Cut {
                reason,
                detail: match reason {
                    "deadline" if budget => format!("{what} stopped at the query's time budget ({} child)", kind.name()),
                    "deadline" => format!("{what} stopped at its {}s deadline ({} child)", deadline.map_or(0, |d| d.as_secs()), kind.name()),
                    _ => format!("{what} stopped: the request was cancelled"),
                },
                hint: match reason {
                    "deadline" if budget => "run it from the CLI, which has no time budget".to_string(),
                    "deadline" => format!("raise {var} (seconds; 0 for no deadline)"),
                    _ => String::new(),
                },
            };
            record(cut.clone());
            if reason != "deadline" || until.is_none() {
                return Err(anyhow::Error::new(cut));
            }
            (status, Some(cut))
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok((Output { status, stdout, stderr }, cut))
}

/// Waits for `child` to exit; `Err(reason)` when it must be stopped first.
fn wait(
    child: &mut Child,
    closed: &std::sync::mpsc::Receiver<()>,
    started: Instant,
    deadline: Option<Duration>,
    op: &Operation,
) -> std::io::Result<Result<std::process::ExitStatus, &'static str>> {
    let mut open = 2;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Ok(status));
        }
        if op.is_cancelled() {
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

    /// A child stopped at the query's time budget gives back what it printed, with the cut; one that finishes in time
    /// gives no cut.
    #[test]
    fn a_child_stopped_at_its_budget_keeps_what_it_printed() {
        let _in = enter(Operation::new());
        // One process printing then waiting: a shell would leave its `sleep` holding the pipe open after the kill.
        let mut c = Command::new("perl");
        c.args(["-e", "$| = 1; print qq(first\\n); sleep 30"]);
        let t = Instant::now();
        let (out, cut) = output_until(c, Kind::Git, "sh", Some(Instant::now() + Duration::from_millis(300))).unwrap();
        assert!(t.elapsed() < Duration::from_secs(10));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "first\n");
        let cut = cut.expect("a cut");
        assert!(cut.detail.contains("time budget"), "{}", cut.detail);
        assert_eq!(take_cuts(), vec![cut]);

        let (out, cut) = output_until(Command::new("true"), Kind::Git, "true", Some(Instant::now() + Duration::from_secs(30))).unwrap();
        assert!(out.status.success() && cut.is_none());
        assert!(take_cuts().is_empty());
    }

    /// The cached summaries a query read and the indexes it used are the query's: a worker entering it records into
    /// it, and another query sees none of them.
    #[test]
    fn invariant_cached_reads_and_used_indexes_belong_to_the_query() {
        let op = Operation::new();
        let _in = enter(op.clone());
        let worker = op.clone();
        std::thread::spawn(move || {
            let _in = enter(worker);
            current().note_cached_read(Path::new("/w/a"), "g1");
            current().note_touched(Path::new("/w/a"));
        })
        .join()
        .unwrap();
        current().note_cached_read(Path::new("/w/a"), "g2");
        current().note_touched(Path::new("/w/b"));
        assert_eq!(current().cached_reads(), [(PathBuf::from("/w/a"), "g1".to_string())].into_iter().collect());
        {
            let other = op.sharing_cancellation();
            let _inner = enter(other);
            assert!(current().cached_reads().is_empty());
            assert!(current().take_touched().is_empty());
        }
        assert_eq!(current().take_touched(), [PathBuf::from("/w/a"), PathBuf::from("/w/b")]);
        assert!(current().take_touched().is_empty(), "each taken once");
    }

    /// Two queries running at once, on two threads, or one after the other on one thread (a call nested in another,
    /// interleaved), each see only what they collected; a query's entries go with it to any thread entering it.
    #[test]
    fn invariant_a_query_sees_only_what_it_collected() {
        use crate::answer::{Incomplete, Reason};
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let run = |name: &'static str| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let op = Operation::new();
                let _in = enter(op.clone());
                for i in 0..50 {
                    current().leave_out(Incomplete::new(Reason::Failed, format!("{name} {i}")));
                    current().note_missing(Path::new(&format!("/gone/{name}")));
                    if i == 25 {
                        // Both queries are half way: neither has taken anything yet.
                        barrier.wait();
                    }
                }
                barrier.wait();
                (current().take_left_out(), current().take_missing())
            })
        };
        let (a, b) = (run("a"), run("b"));
        for (name, (left, missing)) in [("a", a.join().unwrap()), ("b", b.join().unwrap())] {
            assert_eq!(left.len(), 50);
            assert!(left.iter().all(|e| e.detail.starts_with(&format!("{name} "))), "{left:?}");
            assert_eq!(missing, [PathBuf::from(format!("/gone/{name}"))]);
        }

        // Interleaved on one thread: a second query entered while the first runs, then the first again.
        let first = Operation::new();
        let _outer = enter(first.clone());
        current().note_missing(Path::new("/gone/first"));
        {
            let second = first.sharing_cancellation();
            let _inner = enter(second.clone());
            current().note_missing(Path::new("/gone/second"));
            current().leave_out(Incomplete::new(Reason::Failed, "second"));
            assert_eq!(current().take_missing(), [PathBuf::from("/gone/second")]);
            first.cancel();
            assert!(second.is_cancelled(), "cancelling the request cancels each of its queries");
        }
        assert!(current().take_left_out().is_empty());
        assert_eq!(current().take_missing(), [PathBuf::from("/gone/first")]);

        // Carried to a worker thread entering the same query.
        let op = Operation::new();
        let worker = op.clone();
        std::thread::spawn(move || {
            let _in = enter(worker);
            current().note_missing(Path::new("/gone/worker"));
        })
        .join()
        .unwrap();
        assert_eq!(op.take_missing(), [PathBuf::from("/gone/worker")]);

        // The freshness wait is the query's.
        let deadline = Instant::now() + Duration::from_secs(2);
        assert_eq!(op.set_freshness_deadline(Some(deadline)), None);
        assert_eq!(op.freshness_deadline(), Some(deadline));
        assert_eq!(Operation::new().freshness_deadline(), None);
    }

    #[test]
    fn input_and_output_pass_through() {
        let mut c = Command::new("cat");
        c.arg("-");
        let out = output(c, Kind::Git, "cat", Some(b"hello")).unwrap();
        assert_eq!(out.stdout, b"hello");
    }
}
