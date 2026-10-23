//! How much of the machine indexing may use: what the machine has (cores, memory) and how it is doing (load, memory
//! pressure, power), read with a few system calls, and a pure policy ([`plan`]) turning that into how many repos to
//! index at once and with how many threads each.
//!
//! An explicit `knowcode index` may use most of the machine. Background indexing (a server indexing the repos under
//! its workspace roots) scales up only while the machine is idle and on AC power, and backs off to one repo on one
//! thread under load or on battery, and stops under critical memory pressure. Queries always come first: background
//! work starts no repo while a query's indexing is queued (see `knowcode::refresh`).

/// What the machine has and how it is doing, as last read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Machine {
    /// Logical CPUs.
    pub cores: usize,
    /// Of them, the fastest kind (performance cores on Apple silicon; all of them where the OS doesn't tell kinds
    /// apart).
    pub fast_cores: usize,
    /// Physical memory, in bytes.
    pub memory: u64,
    pub pressure: Pressure,
    /// The one-minute load average.
    pub load: f64,
    pub power: Power,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    Normal,
    /// The OS is short of memory and reclaiming it.
    Warn,
    /// About to kill processes for memory.
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Ac,
    Battery,
    /// No power source reported (a desktop or server without one is on AC; treated as such).
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `knowcode index`: the user asked for it and waits.
    Explicit,
    /// A server indexing on its own.
    Background,
}

/// How many repos to index at once, and with how many parser threads each. `repos` 0: start none now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub repos: usize,
    pub threads: usize,
}

/// Memory one cold index may take at its peak, for sizing how many run at once: measured peaks were 32 to 193 MiB
/// resident on public repos of 100 to 8,000 files (`/usr/bin/time -l knowcode index`), so this leaves room for bigger.
pub const REPO_MEMORY: u64 = 512 << 20;

/// Parser threads past this buy little: one thread writes the index, and it bounds the parse phase on medium and large
/// repos (measured with `index --phases`).
pub const MAX_THREADS_PER_REPO: usize = 8;

/// Memory one more thread building a repo's reference graph (see `kc_store::rank::ref_graph`) takes at its peak: its
/// own connection and page cache, the references it is resolving and what resolution keeps, its part of the graph.
/// Measured at 13 to 42 MiB per thread on public repos of 2,000 to 8,000 files (peak resident size at one, two and
/// four threads); this leaves room for bigger ones.
pub const RANK_THREAD_MEMORY: u64 = 128 << 20;

/// Threads building one reference graph past this cost more than they bring: they share SQLite's page cache, whose
/// lock they wait on, and each reads the repo's imports and project files again. Measured on a loaded machine, two
/// threads took 5 to 25% more instructions than one, four 37 to 47% more; to be measured again on a quiet one.
pub const MAX_RANK_THREADS: usize = 2;

/// How many threads build one repo's reference graph under `plan`: its threads per repo, up to [`MAX_RANK_THREADS`],
/// within the memory a repo is planned for ([`REPO_MEMORY`]).
pub fn rank_threads(plan: Plan) -> usize {
    plan.threads.min(MAX_RANK_THREADS).min((REPO_MEMORY / RANK_THREAD_MEMORY) as usize).max(1)
}

/// Below this load per core (other than indexing's own threads), the machine counts as idle.
const IDLE_LOAD: f64 = 0.25;

/// How many of `pending` repos to index at once and with how many threads, on machine `m`, in `mode`, while `own`
/// indexing threads already run (they count in the load average, and are not the user's load).
pub fn plan(m: &Machine, mode: Mode, pending: usize, own: usize) -> Plan {
    if pending == 0 {
        return Plan { repos: 0, threads: 0 };
    }
    let cores = m.cores.max(1);
    let fit = |share: u64| ((m.memory / share) / REPO_MEMORY).max(1) as usize;
    match mode {
        Mode::Explicit => {
            // Most of the machine: every core but one, half the memory (a quarter under pressure, one repo when it
            // is critical).
            let cpu = (cores - 1).max(1);
            let memory = match m.pressure {
                Pressure::Normal => fit(2),
                Pressure::Warn => fit(4),
                Pressure::Critical => 1,
            };
            let repos = pending.min(memory).min(cpu).max(1);
            Plan { repos, threads: (cpu / repos).clamp(1, MAX_THREADS_PER_REPO) }
        }
        Mode::Background => {
            if m.pressure == Pressure::Critical {
                return Plan { repos: 0, threads: 0 };
            }
            let others = (m.load - own as f64).max(0.0) / cores as f64;
            let idle = others < IDLE_LOAD && m.power != Power::Battery && m.pressure == Pressure::Normal;
            if !idle {
                return Plan { repos: 1, threads: 1 };
            }
            // Idle on power: a repo per four cores, within a quarter of the memory, one thread each, so that a user
            // coming back finds most of the machine free.
            let repos = pending.min((cores / 4).max(1)).min(fit(4));
            Plan { repos, threads: 1 }
        }
    }
}

/// Reads the machine now: a few system calls (on macOS the power source is asked of IOKit), cheap enough to repeat
/// every few seconds.
pub fn sniff() -> Machine {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    Machine {
        cores,
        fast_cores: sys::fast_cores().filter(|&n| n > 0 && n <= cores).unwrap_or(cores),
        memory: sys::memory().unwrap_or(8 << 30),
        pressure: sys::pressure(),
        load: load(),
        power: sys::power(),
    }
}

fn load() -> f64 {
    #[cfg(unix)]
    {
        let mut avg = [0f64; 3];
        // SAFETY: room for the three averages asked for.
        if unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) } >= 1 {
            return avg[0];
        }
    }
    0.0
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`'s text, in bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let field = |name: &str| {
        text.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(':')?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()).map(|k| k * 1024)
    };
    (field("MemTotal"), field("MemAvailable"))
}

/// Memory pressure from how much is still available: under a tenth is pressure, under a twentieth critical.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn pressure_from(total: u64, available: u64) -> Pressure {
    if total == 0 {
        Pressure::Normal
    } else if available * 20 < total {
        Pressure::Critical
    } else if available * 10 < total {
        Pressure::Warn
    } else {
        Pressure::Normal
    }
}

/// The power source from `/sys/class/power_supply` entries, each `(type, online, status)`: AC when a mains or USB
/// supply is online, battery when a battery discharges, otherwise unknown.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn power_from(supplies: &[(String, String, String)]) -> Power {
    if supplies.iter().any(|(t, online, _)| (t == "Mains" || t == "USB") && online == "1") {
        Power::Ac
    } else if supplies.iter().any(|(t, _, status)| t == "Battery" && status == "Discharging") {
        Power::Battery
    } else {
        Power::Unknown
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use super::{Power, Pressure};
    use std::ffi::{CString, c_char, c_void};

    fn sysctl(name: &str) -> Option<u64> {
        let name = CString::new(name).ok()?;
        let mut value = 0u64;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: `value` has `len` bytes for the kernel to fill; integers are 4 or 8 bytes, little-endian.
        let ok = unsafe { libc::sysctlbyname(name.as_ptr(), (&raw mut value).cast(), &mut len, std::ptr::null_mut(), 0) } == 0;
        ok.then_some(if len == 4 { value & 0xffff_ffff } else { value })
    }

    pub(super) fn fast_cores() -> Option<usize> {
        sysctl("hw.perflevel0.logicalcpu").map(|n| n as usize)
    }

    pub(super) fn memory() -> Option<u64> {
        sysctl("hw.memsize")
    }

    pub(super) fn pressure() -> Pressure {
        match sysctl("kern.memorystatus_vm_pressure_level") {
            Some(4) => Pressure::Critical,
            Some(2) => Pressure::Warn,
            _ => Pressure::Normal,
        }
    }

    type CFTypeRef = *const c_void;
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFTypeRef;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFStringGetCString(string: CFTypeRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    }
    const UTF8: u32 = 0x0800_0100;

    pub(super) fn power() -> Power {
        // SAFETY: the snapshot is copied (released below); the type string belongs to it and is read before then.
        unsafe {
            let snapshot = IOPSCopyPowerSourcesInfo();
            if snapshot.is_null() {
                return Power::Unknown;
            }
            let kind = IOPSGetProvidingPowerSourceType(snapshot);
            let mut buf = [0 as c_char; 64];
            let read = !kind.is_null() && CFStringGetCString(kind, buf.as_mut_ptr(), buf.len() as isize, UTF8) != 0;
            CFRelease(snapshot);
            if !read {
                return Power::Unknown;
            }
            match std::ffi::CStr::from_ptr(buf.as_ptr()).to_str() {
                Ok("AC Power") => Power::Ac,
                Ok("Battery Power") => Power::Battery,
                _ => Power::Unknown,
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use super::{Power, Pressure};

    pub(super) fn fast_cores() -> Option<usize> {
        None
    }

    pub(super) fn memory() -> Option<u64> {
        super::meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?).0
    }

    pub(super) fn pressure() -> Pressure {
        match std::fs::read_to_string("/proc/meminfo").map(|t| super::meminfo(&t)) {
            Ok((Some(total), Some(available))) => super::pressure_from(total, available),
            _ => Pressure::Normal,
        }
    }

    pub(super) fn power() -> Power {
        let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else { return Power::Unknown };
        let read = |p: &std::path::Path, f: &str| std::fs::read_to_string(p.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
        let supplies: Vec<(String, String, String)> =
            entries.flatten().map(|e| e.path()).map(|p| (read(&p, "type"), read(&p, "online"), read(&p, "status"))).collect();
        super::power_from(&supplies)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod sys {
    use super::{Power, Pressure};

    pub(super) fn fast_cores() -> Option<usize> {
        None
    }

    pub(super) fn memory() -> Option<u64> {
        None
    }

    pub(super) fn pressure() -> Pressure {
        Pressure::Normal
    }

    pub(super) fn power() -> Power {
        Power::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn machine() -> Machine {
        Machine { cores: 16, fast_cores: 12, memory: 32 * GIB, pressure: Pressure::Normal, load: 0.5, power: Power::Ac }
    }

    #[test]
    fn nothing_pending_starts_nothing() {
        assert_eq!(plan(&machine(), Mode::Explicit, 0, 0), Plan { repos: 0, threads: 0 });
        assert_eq!(plan(&machine(), Mode::Background, 0, 0), Plan { repos: 0, threads: 0 });
    }

    #[test]
    fn a_reference_graph_takes_the_repos_threads_within_its_memory() {
        let m = machine();
        assert_eq!(rank_threads(plan(&m, Mode::Explicit, 1, 0)), MAX_RANK_THREADS);
        assert_eq!(rank_threads(plan(&m, Mode::Explicit, 100, 0)), 1);
        assert_eq!(rank_threads(plan(&m, Mode::Background, 1, 0)), 1);
        assert_eq!(rank_threads(Plan { repos: 0, threads: 0 }), 1);
        assert!(MAX_RANK_THREADS as u64 * RANK_THREAD_MEMORY <= REPO_MEMORY);
    }

    #[test]
    fn an_explicit_index_uses_most_of_the_machine() {
        let m = machine();
        // One repo: every core but one, up to the threads one writer keeps busy.
        assert_eq!(plan(&m, Mode::Explicit, 1, 0), Plan { repos: 1, threads: MAX_THREADS_PER_REPO });
        // Many: as many at once as half the memory holds (16 here) and the cores allow (15), the cores shared out.
        assert_eq!(plan(&m, Mode::Explicit, 100, 0), Plan { repos: 15, threads: 1 });
        assert_eq!(plan(&m, Mode::Explicit, 3, 0), Plan { repos: 3, threads: 5 });
        // Memory bounds it before the cores do on a small machine: half of 4 GiB holds four.
        let small = Machine { memory: 4 * GIB, ..m };
        assert_eq!(plan(&small, Mode::Explicit, 100, 0), Plan { repos: 4, threads: 3 });
        // Under memory pressure, a quarter of the memory; when critical, one repo.
        assert_eq!(plan(&Machine { pressure: Pressure::Warn, ..small }, Mode::Explicit, 100, 0).repos, 2);
        assert_eq!(plan(&Machine { pressure: Pressure::Critical, ..m }, Mode::Explicit, 100, 0).repos, 1);
        // A single core still indexes.
        assert_eq!(plan(&Machine { cores: 1, fast_cores: 1, ..m }, Mode::Explicit, 5, 0), Plan { repos: 1, threads: 1 });
    }

    #[test]
    fn background_indexing_scales_up_only_idle_and_on_power() {
        let m = machine();
        // Idle on AC: a repo per four cores, one thread each.
        assert_eq!(plan(&m, Mode::Background, 100, 0), Plan { repos: 4, threads: 1 });
        assert_eq!(plan(&m, Mode::Background, 2, 0), Plan { repos: 2, threads: 1 });
        // Its own threads in the load average don't count as the user's load.
        assert_eq!(plan(&Machine { load: 4.5, ..m }, Mode::Background, 100, 4), Plan { repos: 4, threads: 1 });
        // Load from elsewhere, battery, or memory pressure: one repo on one thread.
        assert_eq!(plan(&Machine { load: 8.0, ..m }, Mode::Background, 100, 0), Plan { repos: 1, threads: 1 });
        assert_eq!(plan(&Machine { power: Power::Battery, ..m }, Mode::Background, 100, 0), Plan { repos: 1, threads: 1 });
        assert_eq!(plan(&Machine { pressure: Pressure::Warn, ..m }, Mode::Background, 100, 0), Plan { repos: 1, threads: 1 });
        // A machine without a battery reports no power source: as on AC.
        assert_eq!(plan(&Machine { power: Power::Unknown, ..m }, Mode::Background, 100, 0).repos, 4);
        // Critical memory pressure: none started.
        assert_eq!(plan(&Machine { pressure: Pressure::Critical, ..m }, Mode::Background, 100, 0), Plan { repos: 0, threads: 0 });
        // Memory bounds it too: a quarter of 4 GiB holds two, of 2 GiB one.
        assert_eq!(plan(&Machine { memory: 4 * GIB, ..m }, Mode::Background, 100, 0).repos, 2);
        assert_eq!(plan(&Machine { memory: 2 * GIB, ..m }, Mode::Background, 100, 0).repos, 1);
    }

    #[test]
    fn linux_probes_parse_what_the_kernel_writes() {
        let text = "MemTotal:       16318412 kB\nMemFree:          512000 kB\nMemAvailable:    4000000 kB\n";
        assert_eq!(meminfo(text), (Some(16318412 * 1024), Some(4000000 * 1024)));
        assert_eq!(pressure_from(16318412, 4000000), Pressure::Normal);
        assert_eq!(pressure_from(100, 9), Pressure::Warn);
        assert_eq!(pressure_from(100, 4), Pressure::Critical);
        let s = |t: &str, o: &str, st: &str| (t.to_string(), o.to_string(), st.to_string());
        assert_eq!(power_from(&[s("Mains", "1", ""), s("Battery", "", "Charging")]), Power::Ac);
        assert_eq!(power_from(&[s("Mains", "0", ""), s("Battery", "", "Discharging")]), Power::Battery);
        assert_eq!(power_from(&[]), Power::Unknown);
    }

    #[test]
    fn this_machine_reads_as_something_sane() {
        let m = sniff();
        assert!(m.cores >= 1 && m.fast_cores >= 1 && m.fast_cores <= m.cores, "{m:?}");
        assert!(m.memory > 0 && m.load >= 0.0, "{m:?}");
    }
}
