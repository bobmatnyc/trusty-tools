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
//! typed reason. #9070 slice 2: [`has_agent_ancestor`], the walk
//! `secrets.grant` uses to judge whether its registrar runs under Claude Code
//! (DOC-74 §15.8), from the process table and never from an env marker.
//!
//! Known limit: a pid is resolved when it is read. A process that exits
//! while it is checked and whose pid is reused in between is judged as the
//! new process. A grandchild whose parent (the granted child) has exited is
//! re-parented away from the child and is refused. A same-uid process that
//! detaches from its parent (re-parents to init) leaves the agent's tree, so
//! the agent check no longer sees Claude Code above it.
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
    /// Whether `pid` is a Claude Code process.
    // #9070: judged on the process's own name or executable path.
    fn is_agent(&self, pid: u32) -> Result<bool, ProcessError>;
}

/// Whether `pid`, or any process above it, is a Claude Code process.
///
/// Why: DOC-74 §15.8 — a grant registered from inside Claude Code's process
/// tree may name only keys flagged "agents may use". The decision reads
/// process ancestry; an env marker is never authorization (L1186-1190).
/// What: walks from `pid` itself up the parent chain, asking
/// [`ProcessTable::is_agent`] of each process. Answers `true` at the first
/// agent, `false` on reaching pid 0 or 1 or a self-parented pid. Any read
/// error is returned, so the caller refuses; a chain longer than
/// [`MAX_CHAIN_DEPTH`] is [`ProcessError::ChainTooDeep`].
/// Test: `agent_ancestor_is_found_above_a_shell`,
/// `agent_ancestor_check_fails_closed_on_unreadable_table`.
pub fn has_agent_ancestor(table: &dyn ProcessTable, pid: u32) -> Result<bool, ProcessError> {
    let mut current = pid;
    for _ in 0..MAX_CHAIN_DEPTH {
        if current <= 1 {
            return Ok(false);
        }
        if table.is_agent(current)? {
            return Ok(true);
        }
        let parent = table.parent(current)?;
        if parent == current {
            return Ok(false);
        }
        current = parent;
    }
    Err(ProcessError::ChainTooDeep)
}

/// Whether a process name or executable path names Claude Code.
///
/// Why: the native Claude Code binary runs as
/// `~/.local/share/claude/versions/<version>`, so on macOS its short name is
/// the version string and only the path names it; on Linux, exec'd through
/// the `claude` symlink, its `comm` is `claude`.
/// What: true when one `/`-separated segment of `label`, ASCII-lowercased,
/// is `claude` or starts with `claude-` or `claude.`. A dot-directory such
/// as `.claude/worktrees` does not match. A false positive only narrows a
/// grant.
/// Test: `agent_label_matches_name_and_versioned_path`.
pub(crate) fn names_agent(label: &str) -> bool {
    label.split('/').any(|segment| {
        let segment = segment.to_ascii_lowercase();
        segment == "claude" || segment.starts_with("claude-") || segment.starts_with("claude.")
    })
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
/// #9070: `is_agent` also reads `/proc/<pid>/exe` (Linux) or `proc_pidpath`
/// (macOS) when it can.
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

    fn is_agent(&self, pid: u32) -> Result<bool, ProcessError> {
        os::is_agent(pid)
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

/// Field 2 (`comm`) of a `/proc/<pid>/stat` line: from the first `(` to the
/// last `)`.
///
/// Test: `linux_stat_parser_reads_parent_and_start_time`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn stat_comm(pid: u32, text: &str) -> Result<&str, ProcessError> {
    let open = text.find('(');
    let close = text.rfind(')');
    match (open, close) {
        (Some(open), Some(close)) if open < close => text
            .get(open + 1..close)
            .ok_or(ProcessError::Unreadable { pid }),
        _ => Err(ProcessError::Unreadable { pid }),
    }
}

#[cfg(target_os = "linux")]
mod os {
    use super::{ProcessError, StartTime, names_agent, stat_comm, stat_parent, stat_start_time};

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

    /// `comm`, then the `/proc/<pid>/exe` target when it is readable.
    // #9070: `exe` of another uid's process (a root `sshd` above a login
    // shell) is unreadable; that process is judged on `comm` alone, since a
    // same-uid Claude Code always has a readable `exe`.
    pub(super) fn is_agent(pid: u32) -> Result<bool, ProcessError> {
        let text = stat(pid)?;
        if names_agent(stat_comm(pid, &text)?) {
            return Ok(true);
        }
        Ok(std::fs::read_link(format!("/proc/{pid}/exe"))
            .is_ok_and(|exe| names_agent(&exe.to_string_lossy())))
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

    /// The executable path when `proc_pidpath` answers, then `pbi_name` and
    /// `pbi_comm`.
    // #9070: the native Claude Code binary's short name is its version
    // string (`2.1.295`); only its path names it.
    pub(super) fn is_agent(pid: u32) -> Result<bool, ProcessError> {
        let info = bsd_info(pid)?;
        if let Some(path) = executable_path(pid)
            && super::names_agent(&path)
        {
            return Ok(true);
        }
        Ok(super::names_agent(&c_text(&info.pbi_name))
            || super::names_agent(&c_text(&info.pbi_comm)))
    }

    /// `proc_pidpath` for `pid`, or `None` when it fails.
    fn executable_path(pid: u32) -> Option<String> {
        let raw = libc::c_int::try_from(pid).ok()?;
        let size = usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).ok()?;
        let capacity = u32::try_from(size).ok()?;
        let mut buf = vec![0u8; size];
        // SAFETY: `buf` is writable for `capacity` bytes, and `proc_pidpath`
        // writes at most `buffersize` bytes and returns the length written.
        let written = unsafe { libc::proc_pidpath(raw, buf.as_mut_ptr().cast(), capacity) };
        let len = usize::try_from(written)
            .ok()
            .filter(|n| *n > 0 && *n <= size)?;
        buf.truncate(len);
        Some(String::from_utf8_lossy(&buf).into_owned())
    }

    /// A NUL-terminated `c_char` field as text.
    fn c_text(field: &[libc::c_char]) -> String {
        let bytes: Vec<u8> = field
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| c.to_ne_bytes()[0])
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
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

    pub(super) fn is_agent(pid: u32) -> Result<bool, ProcessError> {
        Err(ProcessError::Unreadable { pid })
    }
}

#[cfg(test)]
#[path = "ancestry_tests.rs"]
mod tests;
