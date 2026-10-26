//! What each indexing phase costs: wall time, CPU time and, where the OS counts them, instructions retired and
//! cycles. For measuring where indexing goes (`index --phases`); off unless [`enable`]d, when a mark is one
//! `Instant`.
//!
//! Wall time on a shared machine moves with everything else running on it; instructions retired barely do, so
//! compare phases and builds by instructions. Counters are process-wide (every thread, those that have exited
//! included), except a [`ThreadMark`]'s, which counts the calling thread alone: the writer, while parse workers run
//! beside it. Child processes (`git ls-files`, `git log`) are counted apart, in CPU time only (`child_cpu_ms`), once
//! they have exited. Instructions and cycles come from `proc_pid_rusage` and `PROC_PIDTHREADCOUNTS` on macOS; elsewhere
//! only time is counted. A machine that offers the calls but doesn't count (a virtual Mac reads 0) reports them as
//! unavailable, not as no work.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::Serialize;

static ENABLED: AtomicBool = AtomicBool::new(false);
static LOG: Mutex<Vec<Phase>> = Mutex::new(Vec::new());

/// Starts recording phases (for the rest of the process).
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The phases recorded since the last call, in the order they ended.
pub fn take() -> Vec<Phase> {
    std::mem::take(&mut *LOG.lock().unwrap_or_else(|e| e.into_inner()))
}

#[derive(Clone, Debug, Serialize)]
pub struct Phase {
    pub name: String,
    #[serde(flatten)]
    pub cost: Cost,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Cost {
    pub wall_ms: f64,
    pub cpu_ms: f64,
    /// CPU time of child processes that exited in the phase.
    pub child_cpu_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycles: Option<u64>,
}

impl std::ops::Sub for Cost {
    type Output = Cost;
    fn sub(self, o: Cost) -> Cost {
        Cost {
            wall_ms: (self.wall_ms - o.wall_ms).max(0.0),
            cpu_ms: (self.cpu_ms - o.cpu_ms).max(0.0),
            child_cpu_ms: (self.child_cpu_ms - o.child_cpu_ms).max(0.0),
            instructions: self.instructions.zip(o.instructions).map(|(a, b)| a.saturating_sub(b)),
            cycles: self.cycles.zip(o.cycles).map(|(a, b)| a.saturating_sub(b)),
        }
    }
}

impl std::ops::Add for Cost {
    type Output = Cost;
    fn add(self, o: Cost) -> Cost {
        Cost {
            wall_ms: self.wall_ms + o.wall_ms,
            cpu_ms: self.cpu_ms + o.cpu_ms,
            child_cpu_ms: self.child_cpu_ms + o.child_cpu_ms,
            instructions: self.instructions.zip(o.instructions).map(|(a, b)| a + b),
            cycles: self.cycles.zip(o.cycles).map(|(a, b)| a + b),
        }
    }
}

/// Records `cost` as phase `name`, when enabled.
pub fn record(name: &str, cost: Cost) {
    if enabled() {
        LOG.lock().unwrap_or_else(|e| e.into_inner()).push(Phase { name: name.to_string(), cost });
    }
}

#[derive(Clone, Copy, Default)]
struct Counts {
    cpu_ns: u64,
    child_cpu_ns: u64,
    instructions: Option<u64>,
    cycles: Option<u64>,
}

/// The process's counters at one moment.
pub struct Mark {
    at: Instant,
    counts: Option<Counts>,
}

impl Mark {
    pub fn now() -> Mark {
        Mark { at: Instant::now(), counts: enabled().then(sys::process) }
    }

    /// What the process spent since this mark.
    pub fn cost(&self) -> Cost {
        cost(self.at, self.counts, enabled().then(sys::process))
    }

    /// Records what the process spent since this mark as phase `name`, and returns it.
    pub fn record(&self, name: &str) -> Cost {
        let c = self.cost();
        record(name, c);
        c
    }
}

/// The calling thread's counters at one moment.
pub struct ThreadMark {
    at: Instant,
    counts: Option<Counts>,
}

impl ThreadMark {
    pub fn now() -> ThreadMark {
        ThreadMark { at: Instant::now(), counts: enabled().then(sys::thread) }
    }

    /// What this thread spent since the mark (on the thread that took it).
    pub fn cost(&self) -> Cost {
        cost(self.at, self.counts, enabled().then(sys::thread))
    }
}

fn cost(at: Instant, before: Option<Counts>, now: Option<Counts>) -> Cost {
    let wall_ms = at.elapsed().as_secs_f64() * 1000.0;
    match before.zip(now) {
        Some((b, n)) => Cost {
            wall_ms,
            cpu_ms: n.cpu_ns.saturating_sub(b.cpu_ns) as f64 / 1e6,
            child_cpu_ms: n.child_cpu_ns.saturating_sub(b.child_cpu_ns) as f64 / 1e6,
            instructions: n.instructions.zip(b.instructions).map(|(n, b)| n.saturating_sub(b)),
            cycles: n.cycles.zip(b.cycles).map(|(n, b)| n.saturating_sub(b)),
        },
        None => Cost { wall_ms, ..Cost::default() },
    }
}

#[cfg(unix)]
fn clock_ns(clock: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid timespec for the call to fill.
    if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
        return 0;
    }
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// User and system time of the child processes waited for so far.
#[cfg(unix)]
fn children_ns() -> u64 {
    let mut ru = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `ru` is a valid rusage for the call to fill, and zeroed should it fail.
    let ru = unsafe {
        libc::getrusage(libc::RUSAGE_CHILDREN, ru.as_mut_ptr());
        ru.assume_init()
    };
    let ns = |t: libc::timeval| t.tv_sec as u64 * 1_000_000_000 + t.tv_usec as u64 * 1000;
    ns(ru.ru_utime) + ns(ru.ru_stime)
}

#[cfg(target_os = "macos")]
mod sys {
    use super::{Counts, children_ns, clock_ns};

    pub(super) fn process() -> Counts {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: `info` is large enough for the V4 flavor, and zeroed should the call fail.
        let ok = unsafe { libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, info.as_mut_ptr().cast()) } == 0;
        let info = unsafe { info.assume_init() };
        Counts {
            cpu_ns: clock_ns(libc::CLOCK_PROCESS_CPUTIME_ID),
            child_cpu_ns: children_ns(),
            // A running process has retired instructions: a count of 0 means the machine doesn't count them.
            instructions: (ok && info.ri_instructions > 0).then_some(info.ri_instructions),
            cycles: (ok && info.ri_cycles > 0).then_some(info.ri_cycles),
        }
    }

    /// `PROC_PIDTHREADCOUNTS` (macOS 13 and later): a thread's counters per kind of core.
    const PROC_PIDTHREADCOUNTS: libc::c_int = 34;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ThreadCountsData {
        instructions: u64,
        cycles: u64,
        user_time_mach: u64,
        system_time_mach: u64,
        energy_nj: u64,
    }

    #[repr(C)]
    struct ThreadCounts {
        len: u16,
        reserved0: u16,
        reserved1: u32,
        /// One entry per kind of core (performance, efficiency); room for more than any Mac has.
        counts: [ThreadCountsData; 4],
    }

    pub(super) fn thread() -> Counts {
        let cpu_ns = clock_ns(libc::CLOCK_THREAD_CPUTIME_ID);
        let mut tid = 0u64;
        // SAFETY: the current thread's handle, and a valid out pointer.
        if unsafe { libc::pthread_threadid_np(libc::pthread_self(), &mut tid) } != 0 {
            return Counts { cpu_ns, ..Counts::default() };
        }
        let mut buf = ThreadCounts { len: 0, reserved0: 0, reserved1: 0, counts: [ThreadCountsData::default(); 4] };
        let size = std::mem::size_of::<ThreadCounts>() as libc::c_int;
        // SAFETY: `buf` is `size` bytes; the kernel writes at most that many.
        let n = unsafe { libc::proc_pidinfo(libc::getpid(), PROC_PIDTHREADCOUNTS, tid, (&raw mut buf).cast(), size) };
        if n <= 0 {
            return Counts { cpu_ns, ..Counts::default() };
        }
        let levels = &buf.counts[..usize::from(buf.len).min(buf.counts.len())];
        let (instructions, cycles): (u64, u64) = (levels.iter().map(|c| c.instructions).sum(), levels.iter().map(|c| c.cycles).sum());
        Counts {
            cpu_ns,
            instructions: (instructions > 0).then_some(instructions),
            cycles: (cycles > 0).then_some(cycles),
            child_cpu_ns: 0,
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod sys {
    use super::{Counts, children_ns, clock_ns};

    pub(super) fn process() -> Counts {
        Counts { cpu_ns: clock_ns(libc::CLOCK_PROCESS_CPUTIME_ID), child_cpu_ns: children_ns(), ..Counts::default() }
    }

    pub(super) fn thread() -> Counts {
        Counts { cpu_ns: clock_ns(libc::CLOCK_THREAD_CPUTIME_ID), ..Counts::default() }
    }
}

#[cfg(not(unix))]
mod sys {
    use super::Counts;

    pub(super) fn process() -> Counts {
        Counts::default()
    }

    pub(super) fn thread() -> Counts {
        Counts::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_subtract_without_going_negative() {
        let a = Cost { wall_ms: 5.0, cpu_ms: 4.0, child_cpu_ms: 2.0, instructions: Some(100), cycles: None };
        let b = Cost { wall_ms: 6.0, cpu_ms: 1.0, child_cpu_ms: 0.5, instructions: Some(30), cycles: Some(7) };
        assert_eq!(a - b, Cost { wall_ms: 0.0, cpu_ms: 3.0, child_cpu_ms: 1.5, instructions: Some(70), cycles: None });
    }

    #[test]
    fn marks_count_work_once_enabled() {
        enable();
        let (m, t) = (Mark::now(), ThreadMark::now());
        let mut x = 0u64;
        for i in 0..2_000_000u64 {
            x = x.wrapping_mul(31).wrapping_add(std::hint::black_box(i));
        }
        std::hint::black_box(x);
        let (process, thread) = (m.cost(), t.cost());
        assert!(process.cpu_ms > 0.0 && thread.cpu_ms > 0.0);
        if cfg!(target_os = "macos") {
            // A virtual Mac offers the calls but doesn't count (CI runners): unavailable, never a count of 0.
            assert!(process.instructions.is_none_or(|i| i > 2_000_000), "{process:?}");
            assert_eq!(process.instructions.is_some(), process.cycles.is_some(), "{process:?}");
            // Some kernels don't offer per-thread counts; when they do, they count this loop.
            assert!(thread.instructions.is_none_or(|i| i > 2_000_000), "{thread:?}");
        }
    }
}
