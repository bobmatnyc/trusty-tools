//! Process ancestry for exec grants (DOC-74 §15.8, S8 slice 1, #9070).
//!
//! Why: `secrets.resolve` answers only a caller that is the granted child or
//! one of its descendants (DOC-74 §15.8 condition 3). A bare pid is not an
//! identity — the kernel reuses pids — so a grant records the child's start
//! time too, and the ancestry walk compares it before it trusts the pid.
//! What: [`ProcessTable`], the injectable view of the process table (tests
//! use a fake); [`OsProcessTable`], the real one (Linux `/proc`, macOS
//! `proc_pidinfo`; any other target refuses every read); and
//! [`is_self_or_descendant`], the walk itself. Every read failure is an
//! error, never a `false` and never a `true`, so the caller can deny with a
//! typed reason.
//!
//! Known limit: a pid is resolved when it is read. A process that exits
//! while it is checked and whose pid is reused in between is judged as the
//! new process. A grandchild whose parent (the granted child) has exited is
//! re-parented away from the child and is refused.
//! Test: `ancestry_tests.rs` beside this module.

use thiserror::Error;

/// The longest parent chain [`is_self_or_descendant`] walks before it gives up.
pub const MAX_CHAIN_DEPTH: usize = 1024;

/// An opaque process start time, comparable only for equality.
///
/// Why: (pid, start time) names one process for its whole life, where a pid
/// alone names whichever process holds it now.
/// What: Linux clock ticks since boot (`/proc/<pid>/stat` field 22); macOS
/// microseconds since the epoch (`pbi_start_tvsec`, `pbi_start_tvusec`).
/// Test: `pid_reuse_with_new_start_time_is_refused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StartTime(u64);

impl StartTime {
    /// Wrap a raw platform start time.
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// Why a process-table read failed. Each one denies.
///
/// Test: `descendant_check_fails_closed_on_unreadable_table`,
/// `unreadable_start_time_denies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ProcessError {
    /// The process could not be read: it does not exist, the table is not
    /// mounted or not readable, or its record did not parse.
    #[error("process table unreadable for pid {pid}")]
    Unreadable {
        /// The pid that was read.
        pid: u32,
    },
    /// The process was read but its start time was missing or did not parse.
    #[error("start time unreadable for pid {pid}")]
    StartTimeUnreadable {
        /// The pid that was read.
        pid: u32,
    },
    /// The parent chain was longer than [`MAX_CHAIN_DEPTH`] (a loop).
    #[error("parent chain longer than {MAX_CHAIN_DEPTH} links")]
    ChainTooDeep,
}

/// The process table, behind a trait so a test can supply a fake one.
///
/// Why: AC 7 of #9070 — the ancestry rules are proven against a table the
/// test controls (sibling, pid reuse, unreadable), not the host's.
/// What: two reads per pid. Neither may guess: an unreadable value is an
/// `Err`.
/// Test: `descendant_check_accepts_child_and_grandchild`,
/// `os_table_sees_a_spawned_child_as_a_descendant`.
pub trait ProcessTable: Send + Sync {
    /// The parent pid of `pid`.
    fn parent(&self, pid: u32) -> Result<u32, ProcessError>;
    /// The start time of `pid`.
    fn start_time(&self, pid: u32) -> Result<StartTime, ProcessError>;
}

/// Whether `candidate` is the process (`root`, `root_start`) or a descendant.
///
/// Why: DOC-74 §15.8 condition 3, with pid reuse refused (AC 3 of #9070).
/// What: walks `candidate`'s parent chain. On reaching `root`'s pid it reads
/// that pid's start time and answers whether it equals `root_start`; a
/// different start time means the pid was reused and answers `false`.
/// Reaching pid 0 or 1, or a self-parented pid, answers `false`. Any read
/// error is returned, and a chain longer than [`MAX_CHAIN_DEPTH`] is
/// [`ProcessError::ChainTooDeep`].
/// Test: `descendant_check_accepts_child_and_grandchild`,
/// `descendant_check_refuses_sibling_parent_and_unrelated`,
/// `descendant_check_fails_closed_on_unreadable_table`,
/// `descendant_check_refuses_a_parent_loop`,
/// `pid_reuse_with_new_start_time_is_refused`.
pub fn is_self_or_descendant(
    table: &dyn ProcessTable,
    candidate: u32,
    root: u32,
    root_start: StartTime,
) -> Result<bool, ProcessError> {
    let mut pid = candidate;
    for _ in 0..MAX_CHAIN_DEPTH {
        if pid == root {
            return Ok(table.start_time(pid)? == root_start);
        }
        if pid <= 1 {
            return Ok(false);
        }
        let parent = table.parent(pid)?;
        if parent == pid {
            return Ok(false);
        }
        pid = parent;
    }
    Err(ProcessError::ChainTooDeep)
}

/// The host's process table.
///
/// Why: the production [`ProcessTable`].
/// What: Linux reads `/proc/<pid>/stat`; macOS calls `proc_pidinfo` with
/// `PROC_PIDTBSDINFO`; every other target answers
/// [`ProcessError::Unreadable`] for every pid, so no grant can be minted.
/// Test: `os_table_reads_this_process`,
/// `os_table_sees_a_spawned_child_as_a_descendant`,
/// `os_table_refuses_a_pid_that_does_not_exist`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsProcessTable;

impl ProcessTable for OsProcessTable {
    fn parent(&self, pid: u32) -> Result<u32, ProcessError> {
        os::parent(pid)
    }

    fn start_time(&self, pid: u32) -> Result<StartTime, ProcessError> {
        os::start_time(pid)
    }
}

/// The fields of a `/proc/<pid>/stat` line after the `comm` field.
///
/// What: `comm` is parenthesised and may itself hold spaces and `)`, so the
/// split starts after the LAST `)`. Index 0 is field 3 (`state`).
#[cfg(any(target_os = "linux", test))]
fn stat_tail(text: &str) -> Option<Vec<&str>> {
    let close = text.rfind(')')?;
    text.get(close + 1..)
        .map(|tail| tail.split_whitespace().collect())
}

/// Field 4 (`ppid`) of a `/proc/<pid>/stat` line.
///
/// Test: `linux_stat_parser_reads_parent_and_start_time`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn stat_parent(pid: u32, text: &str) -> Result<u32, ProcessError> {
    stat_tail(text)
        .and_then(|fields| fields.get(1).and_then(|f| f.parse().ok()))
        .ok_or(ProcessError::Unreadable { pid })
}

/// Field 22 (`starttime`) of a `/proc/<pid>/stat` line.
///
/// Test: `linux_stat_parser_reads_parent_and_start_time`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn stat_start_time(pid: u32, text: &str) -> Result<StartTime, ProcessError> {
    stat_tail(text)
        .and_then(|fields| fields.get(19).and_then(|f| f.parse().ok()))
        .map(StartTime)
        .ok_or(ProcessError::StartTimeUnreadable { pid })
}

#[cfg(target_os = "linux")]
mod os {
    use super::{ProcessError, StartTime, stat_parent, stat_start_time};

    fn stat(pid: u32) -> Result<String, ProcessError> {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map_err(|_| ProcessError::Unreadable { pid })
    }

    pub(super) fn parent(pid: u32) -> Result<u32, ProcessError> {
        stat_parent(pid, &stat(pid)?)
    }

    pub(super) fn start_time(pid: u32) -> Result<StartTime, ProcessError> {
        stat_start_time(pid, &stat(pid)?)
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::{ProcessError, StartTime};

    /// One `PROC_PIDTBSDINFO` record for `pid`.
    fn bsd_info(pid: u32) -> Result<libc::proc_bsdinfo, ProcessError> {
        let unreadable = ProcessError::Unreadable { pid };
        let raw = libc::c_int::try_from(pid).map_err(|_| unreadable)?;
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
            .map_err(|_| unreadable)?;
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        // SAFETY: `info` is a writable buffer of exactly `size` bytes, and
        // `proc_pidinfo` writes at most `buffersize` bytes into it.
        let written = unsafe {
            libc::proc_pidinfo(
                raw,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return Err(unreadable);
        }
        // SAFETY: `proc_bsdinfo` is plain integers, so the zeroed buffer was
        // already a valid value, and the kernel filled all `size` bytes.
        let info = unsafe { info.assume_init() };
        if info.pbi_pid != pid {
            return Err(unreadable);
        }
        Ok(info)
    }

    pub(super) fn parent(pid: u32) -> Result<u32, ProcessError> {
        Ok(bsd_info(pid)?.pbi_ppid)
    }

    pub(super) fn start_time(pid: u32) -> Result<StartTime, ProcessError> {
        let info = bsd_info(pid)?;
        let missing = ProcessError::StartTimeUnreadable { pid };
        if info.pbi_start_tvsec == 0 && info.pbi_start_tvusec == 0 {
            return Err(missing);
        }
        info.pbi_start_tvsec
            .checked_mul(1_000_000)
            .and_then(|us| us.checked_add(info.pbi_start_tvusec))
            .map(StartTime)
            .ok_or(missing)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod os {
    use super::{ProcessError, StartTime};

    pub(super) fn parent(pid: u32) -> Result<u32, ProcessError> {
        Err(ProcessError::Unreadable { pid })
    }

    pub(super) fn start_time(pid: u32) -> Result<StartTime, ProcessError> {
        Err(ProcessError::Unreadable { pid })
    }
}

#[cfg(test)]
#[path = "ancestry_tests.rs"]
mod tests;
