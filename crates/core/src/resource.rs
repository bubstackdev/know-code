//! Shared analysis controls. No worker/provider owns storage or scheduling policy.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::Path,
    process::{Child, Command},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const CONTROL_VERSION: u32 = 3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub cpu_cores: f64,
    pub memory_mib: u64,
    pub rebuild_disk_mib: u64,
    pub min_free_disk_mib: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            cpu_cores: 2.0,
            memory_mib: 2048,
            rebuild_disk_mib: 2048,
            min_free_disk_mib: 512,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<()> {
        if !self.cpu_cores.is_finite()
            || self.cpu_cores <= 0.0
            || self.memory_mib == 0
            || self.memory_mib.checked_mul(1024 * 1024).is_none()
            || self.rebuild_disk_mib == 0
            || self.rebuild_disk_mib.checked_mul(1024 * 1024).is_none()
            || self.min_free_disk_mib.checked_mul(1024 * 1024).is_none()
        {
            return Err(Error::InvalidRequest("cpu_cores must be finite and positive; memory_mib and rebuild_disk_mib must be positive representable sizes; min_free_disk_mib must be representable".into()));
        }
        Ok(())
    }
    pub fn load(data: &Path) -> Result<Self> {
        let limits = match std::fs::read(data.join("resources.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        limits.validate()?;
        Ok(limits)
    }
    pub fn save(&self, data: &Path) -> Result<()> {
        use std::io::Write;
        self.validate()?;
        let path = data.join("resources.json.next");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.sync_all()?;
        std::fs::rename(path, data.join("resources.json"))?;
        std::fs::File::open(data)?.sync_all()?;
        Ok(())
    }
}
#[derive(Clone, Debug)]
enum Stop {
    Cancelled,
    Limited(String),
}
impl Stop {
    fn error(&self) -> Error {
        match self {
            Self::Cancelled => Error::Cancelled,
            Self::Limited(s) => Error::ResourceLimited(s.clone()),
        }
    }
}
#[derive(Default)]
struct State {
    reason: Option<Stop>,
    paused: bool,
    finished: bool,
    committed: bool,
    groups: BTreeSet<i32>,
    sqlite: Option<rusqlite::InterruptHandle>,
}
struct Inner {
    state: Mutex<State>,
    changed: Condvar,
}
/// Clones cancel the same analysis. The default control is explicitly unmanaged.
#[derive(Clone, Default)]
pub struct Control(Option<Arc<Inner>>);
impl Control {
    pub fn committed(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|inner| inner.state.lock().unwrap().committed)
    }
    pub fn resource_limited(&self) -> bool {
        self.0.as_ref().is_some_and(|inner| {
            matches!(inner.state.lock().unwrap().reason, Some(Stop::Limited(_)))
        })
    }
    pub fn cancel(&self) -> bool {
        self.stop(Stop::Cancelled)
    }
    fn stop(&self, reason: Stop) -> bool {
        let Some(inner) = &self.0 else {
            return false;
        };
        let mut state = inner.state.lock().unwrap();
        if state.finished || state.committed || state.reason.is_some() {
            return false;
        }
        state.reason = Some(reason);
        if let Some(sqlite) = &state.sqlite {
            sqlite.interrupt();
        }
        for &pid in &state.groups {
            signal(pid, libc::SIGCONT);
            signal(pid, libc::SIGTERM);
        }
        state.paused = false;
        inner.changed.notify_all();
        true
    }
    pub fn check(&self) -> Result<()> {
        let Some(inner) = &self.0 else {
            return Ok(());
        };
        let mut state = inner.state.lock().unwrap();
        while state.paused && state.reason.is_none() && !state.finished {
            state = inner.changed.wait(state).unwrap();
        }
        match &state.reason {
            Some(reason) => Err(reason.error()),
            None => Ok(()),
        }
    }
    pub fn interruptible<T>(
        &self,
        sqlite: rusqlite::InterruptHandle,
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.check()?;
        let Some(inner) = &self.0 else {
            return action();
        };
        {
            let mut state = inner.state.lock().unwrap();
            if let Some(reason) = &state.reason {
                return Err(reason.error());
            }
            state.sqlite = Some(sqlite);
        }
        let result = action();
        inner.state.lock().unwrap().sqlite.take();
        self.check()?;
        result
    }
    pub fn publish<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        self.check()?;
        let Some(inner) = &self.0 else {
            return action();
        };
        // Cancellation and publication have one linearization point. A committed
        // generation remains valid if cancellation arrives after this operation.
        let mut state = inner.state.lock().unwrap();
        if let Some(reason) = &state.reason {
            return Err(reason.error());
        }
        let result = action();
        if result.is_ok() {
            state.committed = true;
        }
        result
    }
    pub fn spawn(&self, command: &mut Command) -> Result<Worker> {
        self.check()?;
        let child = command
            .process_group(0)
            .env("MSBUILDDISABLENODEREUSE", "1")
            .env("DOTNET_CLI_USE_MSBUILD_SERVER", "0")
            .spawn()?;
        let pid = child.id() as i32;
        if let Some(inner) = &self.0 {
            let mut state = inner.state.lock().unwrap();
            state.groups.insert(pid);
            if state.reason.is_some() {
                signal(pid, libc::SIGTERM);
            }
            if state.paused {
                signal(pid, libc::SIGSTOP);
            }
        }
        Ok(Worker {
            child,
            control: self.clone(),
            reaped: false,
        })
    }
}
/// Reap only after group cleanup so a reused leader PID cannot be signaled.
pub struct Worker {
    pub child: Child,
    control: Control,
    reaped: bool,
}
impl Worker {
    pub fn wait(&mut self) -> Result<std::process::ExitStatus> {
        // Observe exit without reaping; keep the group leader PID reserved until
        // the monitor has stopped signaling and descendant cleanup is complete.
        loop {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id(),
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if unsafe { info.assume_init() }.si_pid != 0 {
                break;
            }
            if self.control.check().is_err() {
                self.terminate();
            }
            thread::sleep(Duration::from_millis(5));
        }
        let inner = self.control.0.clone();
        let mut state = inner.as_ref().map(|s| s.state.lock().unwrap());
        if let Some(state) = &mut state {
            state.groups.remove(&(self.child.id() as i32));
        }
        self.terminate();
        let result = self.child.wait();
        self.reaped = true;
        Ok(result?)
    }
    pub fn terminate(&self) {
        let pid = self.child.id() as i32;
        signal(pid, libc::SIGCONT);
        signal(pid, libc::SIGKILL);
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let inner = self.control.0.clone();
        let mut state = inner.as_ref().map(|s| s.state.lock().unwrap());
        if let Some(state) = &mut state {
            state.groups.remove(&(self.child.id() as i32));
        }
        self.terminate();
        let _ = self.child.wait();
    }
}
fn signal(group: i32, signal: i32) {
    if group > 1 {
        unsafe {
            libc::kill(-group, signal);
        }
    }
}

/// One instance for the coordinator; the existing scheduler admits one job.
/// Limits are shared across all of that job's providers and include coordinator
/// caches/allocations. Nothing reserves memory or changes embedding-host policy.
pub struct Governor {
    meter: Arc<Mutex<Meter>>,
    limits: Arc<Mutex<Limits>>,
    active: AtomicBool,
}
impl Governor {
    pub fn new(limits: Limits) -> Result<Arc<Self>> {
        limits.validate()?;
        Ok(Arc::new(Self {
            meter: Arc::new(Mutex::new(Meter {
                cpu: None,
                time: Instant::now(),
                credit: 0.0,
            })),
            limits: Arc::new(Mutex::new(limits)),
            active: AtomicBool::new(false),
        }))
    }
    pub fn limits(&self) -> Limits {
        self.limits.lock().unwrap().clone()
    }
    pub fn configure(&self, data: &Path, limits: Limits) -> Result<bool> {
        limits.validate()?;
        let mut current = self.limits.lock().unwrap();
        if *current == limits {
            return Ok(false);
        }
        limits.save(data)?;
        *current = limits;
        Ok(true)
    }
    pub fn start(self: &Arc<Self>) -> Result<Session> {
        if self.active.swap(true, Ordering::AcqRel) {
            return Err(Error::InvalidRequest(
                "the shared analysis slot is already occupied".into(),
            ));
        }
        let control = Control(Some(Arc::new(Inner {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        })));
        let limits = Arc::clone(&self.limits);
        let monitor = control.clone();
        let meter = Arc::clone(&self.meter);
        let baseline = match sample(std::process::id() as i32) {
            Ok(s) => s,
            Err(e) => {
                control.stop(Stop::Limited(format!(
                    "resource accounting unavailable: {e}"
                )));
                Usage::default()
            }
        };
        let handle = match thread::Builder::new()
            .name("know-code-resources".into())
            .spawn(move || monitor_loop(monitor, limits, meter, baseline))
        {
            Ok(h) => Some(h),
            Err(e) => {
                control.stop(Stop::Limited(format!("cannot start resource monitor: {e}")));
                None
            }
        };
        Ok(Session {
            control,
            monitor: handle,
            governor: Arc::clone(self),
        })
    }
}
pub struct Session {
    control: Control,
    monitor: Option<JoinHandle<()>>,
    governor: Arc<Governor>,
}
impl Session {
    pub fn control(&self) -> Control {
        self.control.clone()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let inner = self.control.0.as_ref().unwrap();
        {
            let mut state = inner.state.lock().unwrap();
            state.finished = true;
            for &pid in &state.groups {
                signal(pid, libc::SIGCONT);
            }
            state.paused = false;
            inner.changed.notify_all();
        }
        if let Some(handle) = self.monitor.take() {
            let _ = handle.join();
        }
        self.governor.active.store(false, Ordering::Release);
    }
}
#[derive(Default)]
struct Usage {
    cpu: u64,
    memory: u64,
}
// Aggregate live descendants plus each live process's reaped-child CPU. Reaped
// children are no longer in the live tree, avoiding permanent double counting.
fn sample(root: i32) -> std::io::Result<Usage> {
    let mut stack = vec![root];
    let mut seen = BTreeSet::new();
    let mut usage = Usage::default();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if seen.len() > 4096 {
            return Err(std::io::Error::other(
                "analysis process tree exceeds the census bound",
            ));
        }
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
        let code =
            unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) };
        if code != 0 {
            let error = std::io::Error::last_os_error();
            if pid != root && error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(error);
        }
        let info = unsafe { info.assume_init() };
        usage.cpu = usage
            .cpu
            .saturating_add(info.ri_user_time)
            .saturating_add(info.ri_system_time)
            .saturating_add(info.ri_child_user_time)
            .saturating_add(info.ri_child_system_time);
        usage.memory = usage.memory.saturating_add(info.ri_phys_footprint);
        let mut capacity = 16;
        loop {
            let mut children = vec![0i32; capacity];
            let n = unsafe {
                libc::proc_listchildpids(pid, children.as_mut_ptr().cast(), (capacity * 4) as i32)
            };
            if n < 0 {
                let error = std::io::Error::last_os_error();
                if pid != root && error.raw_os_error() == Some(libc::ESRCH) {
                    break;
                }
                return Err(error);
            }
            if n as usize >= capacity {
                if capacity >= 4096 {
                    return Err(std::io::Error::other(
                        "analysis child list exceeds the census bound",
                    ));
                }
                capacity *= 2;
                continue;
            }
            stack.extend(children[..n as usize].iter().copied().filter(|p| *p > 0));
            break;
        }
    }
    Ok(usage)
}
struct Meter {
    cpu: Option<u64>,
    time: Instant,
    credit: f64,
}
impl Meter {
    fn record(&mut self, cpu: u64, cores: f64, seconds_per_tick: f64) -> bool {
        let now = Instant::now();
        let previous = self.cpu.unwrap_or(cpu);
        self.credit = (self.credit + now.duration_since(self.time).as_secs_f64() * cores
            - cpu.saturating_sub(previous) as f64 * seconds_per_tick)
            .min(0.1 * cores);
        self.cpu = Some(previous.max(cpu));
        self.time = now;
        self.credit < 0.0
    }
}
// libc deprecates its Mach wrappers in favor of another crate; the system ABI
// remains supported. Keep this one call rather than adding a Mach dependency.
#[allow(deprecated)]
fn seconds_per_tick() -> std::io::Result<f64> {
    let mut timebase = std::mem::MaybeUninit::<libc::mach_timebase_info>::uninit();
    let code = unsafe { libc::mach_timebase_info(timebase.as_mut_ptr()) };
    if code != 0 {
        return Err(std::io::Error::other(
            "CPU timebase accounting is unavailable",
        ));
    }
    let timebase = unsafe { timebase.assume_init() };
    if timebase.numer == 0 || timebase.denom == 0 {
        return Err(std::io::Error::other("CPU timebase is invalid"));
    }
    Ok(f64::from(timebase.numer) / f64::from(timebase.denom) / 1e9)
}
fn monitor_loop(
    control: Control,
    settings: Arc<Mutex<Limits>>,
    meter: Arc<Mutex<Meter>>,
    baseline: Usage,
) {
    let inner = control.0.as_ref().unwrap();
    let seconds_per_tick = match seconds_per_tick() {
        Ok(value) => value,
        Err(e) => {
            control.stop(Stop::Limited(e.to_string()));
            0.0
        }
    };
    {
        let mut meter = meter.lock().unwrap();
        if meter.cpu.is_none() {
            meter.cpu = Some(baseline.cpu);
            meter.time = Instant::now();
        }
    }
    let mut stopping = None;
    loop {
        let limits = settings.lock().unwrap().clone();
        let usage = match sample(std::process::id() as i32) {
            Ok(u) => u,
            Err(e) => {
                control.stop(Stop::Limited(format!(
                    "resource accounting unavailable: {e}"
                )));
                Usage::default()
            }
        };
        let now = Instant::now();
        // Carry CPU credit/debt across workspaces rather than granting each
        // newly scheduled repository a separate budget or a fresh burst.
        let pause = meter
            .lock()
            .unwrap()
            .record(usage.cpu, limits.cpu_cores, seconds_per_tick);
        if usage.memory > limits.memory_mib * 1024 * 1024 {
            // Serialize the decision with configuration: a stale sampled limit
            // must not abort a job just after the user raised its budget.
            let current = settings.lock().unwrap();
            if usage.memory > current.memory_mib * 1024 * 1024 {
                control.stop(Stop::Limited(format!("shared memory limit exceeded: {} MiB observed, {} MiB allowed; raise the personal limit or reduce the project", usage.memory.div_ceil(1024*1024), current.memory_mib)));
            }
        }
        let mut state = inner.state.lock().unwrap();
        if state.finished {
            break;
        }
        if state.reason.is_some() {
            // Repeat while stopped: sqlite3_interrupt does not latch when no
            // statement is running, and a statement may start between checks.
            if let Some(sqlite) = &state.sqlite {
                sqlite.interrupt();
            }
            let since = stopping.get_or_insert(now);
            if now.duration_since(*since) >= Duration::from_millis(200) {
                for &pid in &state.groups {
                    signal(pid, libc::SIGKILL);
                }
            }
        } else {
            if pause != state.paused {
                for &pid in &state.groups {
                    signal(pid, if pause { libc::SIGSTOP } else { libc::SIGCONT });
                }
                state.paused = pause;
                inner.changed.notify_all();
            }
        }
        let _ = inner
            .changed
            .wait_timeout(state, Duration::from_millis(20))
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    #[test]
    fn disk_limits_default_for_existing_settings_and_reject_overflow() {
        let existing: Limits =
            serde_json::from_str(r#"{"cpu_cores":1.0,"memory_mib":1024}"#).unwrap();
        assert_eq!(existing.rebuild_disk_mib, 2048);
        assert_eq!(existing.min_free_disk_mib, 512);
        existing.validate().unwrap();
        for limits in [
            Limits {
                rebuild_disk_mib: 0,
                ..Limits::default()
            },
            Limits {
                rebuild_disk_mib: u64::MAX,
                ..Limits::default()
            },
            Limits {
                min_free_disk_mib: u64::MAX,
                ..Limits::default()
            },
        ] {
            assert!(limits.validate().is_err());
        }
        Limits {
            min_free_disk_mib: 0,
            ..Limits::default()
        }
        .validate()
        .unwrap();
    }
    #[test]
    fn cancelled_analysis_cannot_publish_and_committed_analysis_cannot_be_cancelled() {
        let governor = Governor::new(Limits::default()).unwrap();
        let session = governor.start().unwrap();
        assert!(
            governor.start().is_err(),
            "budgets must not multiply with jobs"
        );
        let control = session.control();
        assert!(control.cancel());
        assert!(!control.committed());
        assert!(matches!(
            control.publish::<()>(|| panic!("cancelled publication ran")),
            Err(Error::Cancelled)
        ));
        drop(session);
        let session = governor.start().unwrap();
        let control = session.control();
        assert_eq!(control.publish(|| Ok(123)).unwrap(), 123);
        assert!(control.committed());
        assert!(!control.cancel());
    }
    #[test]
    fn cancellation_interrupts_sql_that_starts_after_the_first_interrupt() {
        let governor = Governor::new(Limits::default()).unwrap();
        let session = governor.start().unwrap();
        let control = session.control();
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let started = Instant::now();
        let result: Result<i64> = control.interruptible(connection.get_interrupt_handle(), || {
            assert!(control.cancel());
            // No statement was active for the initial interrupt. The monitor
            // must still stop a statement that starts immediately afterward.
            Ok(connection.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n", [], |r| r.get(0))?)
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn cancellation_cleans_a_stubborn_process_group_without_reaping_the_leader_early() {
        let governor = Governor::new(Limits::default()).unwrap();
        let session = governor.start().unwrap();
        let control = session.control();
        let mut worker = control
            .spawn(
                Command::new("/bin/sh")
                    .args(["-c", "trap '' TERM; /bin/sleep 60 & echo ready; wait"])
                    .stdout(Stdio::piped()),
            )
            .unwrap();
        let mut line = String::new();
        BufReader::new(worker.child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line.trim(), "ready");
        let started = Instant::now();
        control.cancel();
        assert!(!worker.wait().unwrap().success());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(control.check(), Err(Error::Cancelled)));
    }
}
