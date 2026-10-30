//! The process reads the Architect launch-record check needs (#10459).
//!
//! Why: tm records the Architect's `claude` by PID, `sysinfo` start time
//! (whole Unix seconds) and launch directory. `secrets.set` and a local
//! `secrets.copy` compare a live process against that record, so the host
//! table must answer both in tm's units.
//! What: [`start_unix_secs`] and [`cwd`] for [`super::OsProcessTable`]. Every
//! read failure is an error, never a guess; any other target refuses.
//! Test: `os_table_reads_its_own_unix_start_and_cwd` in `ancestry_tests.rs`.

use std::path::PathBuf;

use super::ProcessError;

/// Microseconds per second: macOS [`super::StartTime`] is microseconds.
#[cfg(target_os = "macos")]
const MICROS: u64 = 1_000_000;

/// `pbi_start_tvsec`: the whole seconds of the microsecond start time.
#[cfg(target_os = "macos")]
pub(super) fn start_unix_secs(pid: u32) -> Result<u64, ProcessError> {
    Ok(super::os::start_time(pid)?.0 / MICROS)
}

/// `btime` plus `starttime / CLK_TCK`, the integer arithmetic `sysinfo` uses.
#[cfg(target_os = "linux")]
pub(super) fn start_unix_secs(pid: u32) -> Result<u64, ProcessError> {
    let missing = ProcessError::StartTimeUnreadable { pid };
    let ticks = super::os::start_time(pid)?.0;
    // SAFETY: `sysconf` takes an integer name and touches no caller memory.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = u64::try_from(hz).ok().filter(|hz| *hz > 0).ok_or(missing)?;
    let stat = std::fs::read_to_string("/proc/stat").map_err(|_| missing)?;
    let btime = boot_time(&stat).ok_or(missing)?;
    btime.checked_add(ticks / hz).ok_or(missing)
}

/// The `btime` line of `/proc/stat`, seconds since the epoch.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn boot_time(stat: &str) -> Option<u64> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|rest| rest.trim().parse().ok())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn start_unix_secs(pid: u32) -> Result<u64, ProcessError> {
    Err(ProcessError::StartTimeUnreadable { pid })
}

/// `/proc/<pid>/cwd`.
#[cfg(target_os = "linux")]
pub(super) fn cwd(pid: u32) -> Result<PathBuf, ProcessError> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).map_err(|_| ProcessError::Unreadable { pid })
}

/// `proc_pidinfo` flavor `PROC_PIDVNODEPATHINFO`: `pvi_cdir.vip_path`.
///
/// What: anything but exactly `size_of` bytes written, or an empty path, is
/// [`ProcessError::Unreadable`]. Same uid only, like the start time.
#[cfg(target_os = "macos")]
pub(super) fn cwd(pid: u32) -> Result<PathBuf, ProcessError> {
    use std::os::unix::ffi::OsStrExt as _;
    let unreadable = ProcessError::Unreadable { pid };
    let raw = libc::c_int::try_from(pid).map_err(|_| unreadable)?;
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>())
        .map_err(|_| unreadable)?;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    // SAFETY: `info` is a writable buffer of exactly `size` bytes, and
    // `proc_pidinfo` writes at most `buffersize` bytes into it.
    let written = unsafe {
        libc::proc_pidinfo(
            raw,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return Err(unreadable);
    }
    // SAFETY: the struct is plain integers and `c_char` arrays, so the zeroed
    // buffer was already a valid value, and the kernel filled all `size` bytes.
    let info = unsafe { info.assume_init() };
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|c| c.to_ne_bytes()[0])
        .take_while(|b| *b != 0)
        .collect();
    if bytes.is_empty() {
        return Err(unreadable);
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn cwd(pid: u32) -> Result<PathBuf, ProcessError> {
    Err(ProcessError::Unreadable { pid })
}
