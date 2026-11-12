//! Liveness when the socket does not serve: the `/proc` scan (#11098).
//!
//! Why: a daemon whose socket file was unlinked, or whose listener stopped
//! answering, still runs. Reading "socket absent" as "not running" let `stop`
//! report `NotRunning` and `restart` start a second daemon beside the first.
//! The socket's `SO_PEERCRED` pid cannot help once nothing answers, and the
//! daemons write no pidfile, so the remaining source is the process table.
//! What: [`refuse_unserved_daemon`] — run on every "the socket does not serve"
//! path of `stop` and `start` — errors when the lock signal's lock is held
//! ([`LifecycleError::Unidentified`]) or when [`matching_processes`] finds a
//! live process of the caller's effective uid that runs `desc.binary` with
//! every `desc.liveness_args` element ([`LifecycleError::RunningUnserved`]).
//! A match is never signalled: the scan only stops a false `NotRunning`, so a
//! wrong match fails closed. `liveness_args` is what tells the daemon
//! (`serve --foreground`) from a stdio bridge (`serve --stdio`) of the same
//! executable.
//! Test: `stop_start_and_restart_refuse_while_a_daemon_runs_without_its_socket`,
//! `stop_returns_not_running_while_only_a_bridge_of_the_same_binary_runs`,
//! `argv_match_needs_every_liveness_arg_as_a_whole_argument`,
//! `status_parse_reads_state_and_effective_uid`.
//!
//! [`LifecycleError::Unidentified`]: crate::linux_lifecycle::LifecycleError::Unidentified
//! [`LifecycleError::RunningUnserved`]: crate::linux_lifecycle::LifecycleError::RunningUnserved

use std::io;
use std::path::Path;

use super::process::exe_name;
use super::{DaemonDescriptor, LifecycleError, StoppedSignal, lock_is_free};

/// Fail when the socket does not serve but the daemon may still run.
///
/// Why: see the module docs; "socket absent ⇒ not running" is the defect.
/// What: the lock check (lock signal only), then the `/proc` scan.
/// Test: `stop_start_and_restart_refuse_while_a_daemon_runs_without_its_socket`,
/// `stop_and_restart_refuse_when_the_lock_is_held_without_a_socket`.
pub(crate) fn refuse_unserved_daemon(desc: &DaemonDescriptor) -> Result<(), LifecycleError> {
    if let StoppedSignal::LockFree(lock) = &desc.stopped_signal
        && !lock_is_free(lock)?
    {
        return Err(LifecycleError::Unidentified {
            socket: desc.socket.clone(),
            lock: lock.clone(),
        });
    }
    let pids = matching_processes(desc)?;
    if pids.is_empty() {
        return Ok(());
    }
    Err(LifecycleError::RunningUnserved {
        socket: desc.socket.clone(),
        binary: desc.binary.clone(),
        pids,
    })
}

/// Live pids, other than this process, of this effective uid, that run
/// `desc.binary` with every `desc.liveness_args` element; ascending.
///
/// What: walks `/proc`. A pid that vanishes mid-read is skipped. A live,
/// same-uid process whose argv cannot be read is an error, never a skip,
/// because it might be the daemon. An unreadable `/proc/<pid>/exe` (an
/// undumpable process) falls back to `argv[0]`'s file name.
fn matching_processes(desc: &DaemonDescriptor) -> Result<Vec<u32>, LifecycleError> {
    let me = std::process::id();
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let io_err = |context: &str, source| LifecycleError::Io {
        context: context.to_string(),
        source,
    };
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(|e| io_err("read /proc", e))? {
        let entry = entry.map_err(|e| io_err("read /proc entry", e))?;
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        if pid != me && process_matches(pid, euid, desc)? {
            found.push(pid);
        }
    }
    found.sort_unstable();
    Ok(found)
}

fn process_matches(pid: u32, euid: u32, desc: &DaemonDescriptor) -> Result<bool, LifecycleError> {
    let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
        return Ok(false);
    };
    match parse_status(&status) {
        Some((state, uid)) if uid == euid && !matches!(state, 'Z' | 'X') => {}
        _ => return Ok(false),
    }
    let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(c) => c,
        Err(e) if vanished(&e) => return Ok(false),
        Err(source) => return Err(LifecycleError::IdentityUnreadable { pid, source }),
    };
    if !argv_matches(&cmdline, &desc.liveness_args) {
        return Ok(false);
    }
    let name = match exe_name(pid) {
        Ok(name) => name,
        // A kernel thread has no exe; an exited process no longer has one.
        Err(e) if vanished(&e) => return Ok(false),
        Err(_) => argv0_name(&cmdline),
    };
    Ok(name == desc.binary)
}

fn vanished(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESRCH)
}

/// Whether `argv[1..]` of a NUL-separated `/proc/<pid>/cmdline` holds every
/// element of `wanted` as a whole argument.
///
/// Test: `argv_match_needs_every_liveness_arg_as_a_whole_argument`.
pub(crate) fn argv_matches(cmdline: &[u8], wanted: &[String]) -> bool {
    let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).skip(1).collect();
    wanted.iter().all(|w| args.contains(&w.as_bytes()))
}

fn argv0_name(cmdline: &[u8]) -> String {
    let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or_default();
    Path::new(&*String::from_utf8_lossy(argv0))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The state letter and the effective uid from `/proc/<pid>/status`.
///
/// Test: `status_parse_reads_state_and_effective_uid`.
pub(crate) fn parse_status(status: &str) -> Option<(char, u32)> {
    let field = |key: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .map(str::trim_start)
    };
    let state = field("State:")?.chars().next()?;
    // `Uid:` lists real, effective, saved and filesystem uids.
    let euid = field("Uid:")?.split_whitespace().nth(1)?.parse().ok()?;
    Some((state, euid))
}
