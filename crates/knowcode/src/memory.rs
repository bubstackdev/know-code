//! Giving memory back to the system. Most of what a long-lived process holds after indexing or a large query is not
//! in use: it is memory freed but kept by the allocator. On macOS the default allocator keeps freed large blocks in a
//! cache of its own, which `malloc_zone_pressure_relief` does not empty: on bitwarden/server (8,274 files) a cold
//! `index` peaked at 254 to 262 MB of footprint with 33 MB in use, and a server held 162 MB once idle. With the cache
//! off, the same runs took 92 to 94 MB and 21 to 22 MB, for about 1% more instructions.

/// The variable macOS's allocator reads at startup to turn its cache of freed large blocks off (`0`). Read only when
/// the process starts, so it is set by running the binary again (see [`lean_allocator`]).
#[cfg(target_os = "macos")]
const LARGE_CACHE: &str = "MallocLargeCache";

/// For a long-lived command (`serve`, `index`): on macOS, runs this binary again in place of this process with the
/// allocator's large-block cache off, unless the environment already says how it should be. The process keeps its
/// id, arguments and standard streams, so an MCP client sees no difference. Short commands skip it: they exit before
/// what the cache holds matters, and a hook pays for every millisecond. If the binary can't be run again, carries on
/// as it is.
pub(crate) fn lean_allocator() {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        if std::env::var_os(LARGE_CACHE).is_some() {
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let mut args = std::env::args_os();
        let arg0 = args.next().unwrap_or_else(|| exe.clone().into_os_string());
        // Only returns on failure.
        let _ = std::process::Command::new(&exe).arg0(arg0).args(args).env(LARGE_CACHE, "0").exec();
    }
}

/// Hands freed pages back to the system: after each server call and each background job, so resident memory tracks
/// what is in use rather than the high-water mark of the biggest one.
pub(crate) fn release() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        // SAFETY: a null zone means all zones; goal 0 means release as much as possible. No pointers are kept.
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: glibc's `malloc_trim` only returns free memory at the top of each arena and unused pages inside it.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}
