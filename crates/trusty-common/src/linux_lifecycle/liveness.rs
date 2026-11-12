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
//! live process of the caller's effective uid that is this daemon instance
//! ([`LifecycleError::RunningUnserved`]). A process is this instance when it
//! runs `desc.binary` with every `desc.liveness_args` element and its
//! environment holds every `desc.instance_env` value. `liveness_args` tells the
//! daemon (`serve --foreground`) from a stdio bridge (`serve --stdio`) of the
//! same executable; `instance_env` tells it from a sibling instance with the
//! same argv and another data dir, such as a sandbox. A match is never
//! signalled: the scan only stops a false `NotRunning`. A read the scan cannot
//! classify fails closed with an error; only a vanished process, or another
//! uid's entry hidden by `hidepid`, is skipped.
//! Test: `stop_start_and_restart_refuse_while_a_daemon_runs_without_its_socket`,
//! `restart_of_one_instance_leaves_a_sibling_with_the_same_argv_serving`,
//! `start_and_restart_start_a_stopped_instance_while_a_sibling_runs`,
//! `stop_and_start_fail_closed_when_a_matching_process_hides_its_environment`,
//! `stop_fails_closed_when_the_scan_cannot_open_proc_files`,
//! `stop_returns_not_running_while_only_a_bridge_of_the_same_binary_runs`,
//! `argv_match_needs_every_liveness_arg_as_a_whole_argument`,
//! `environ_match_needs_every_instance_value_and_unset_equals_unset`,
//! `read_error_is_skipped_only_for_a_vanished_or_foreign_entry`,
//! `status_parse_reads_state_and_effective_uid`.
//!
//! [`LifecycleError::Unidentified`]: crate::linux_lifecycle::LifecycleError::Unidentified
//! [`LifecycleError::RunningUnserved`]: crate::linux_lifecycle::LifecycleError::RunningUnserved

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::MetadataExt as _;
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

/// Live pids, other than this process, of this effective uid, that are the
/// instance `desc` describes; ascending.
///
/// What: walks `/proc`. A pid that vanishes mid-read, or another uid's entry
/// `hidepid` hides, is skipped; every other read error is an error, never a
/// skip, because the process might be the daemon. An unreadable
/// `/proc/<pid>/exe` (an undumpable process) falls back to `argv[0]`'s file
/// name; an unreadable `environ` cannot fall back and is an error.
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
    // #11098: only a vanished or foreign entry is skipped; EMFILE, ENOMEM or
    // EIO here once read a live daemon as not running.
    let status = match std::fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(s) => s,
        Err(e) if skippable(&e, pid, euid) => return Ok(false),
        Err(source) => {
            return Err(LifecycleError::Io {
                context: format!("read /proc/{pid}/status"),
                source,
            });
        }
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
        // An undumpable process hides its exe; any other error fails closed.
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => argv0_name(&cmdline),
        Err(source) => return Err(LifecycleError::IdentityUnreadable { pid, source }),
    };
    if name != desc.binary {
        return Ok(false);
    }
    if desc.instance_env.is_empty() {
        return Ok(true);
    }
    // #11098: a sibling instance (same argv, another data dir) is not this one.
    match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(environ) => Ok(environ_matches(&environ, &desc.instance_env)),
        Err(e) if vanished(&e) => Ok(false),
        Err(source) => Err(LifecycleError::InstanceUnreadable {
            pid,
            binary: desc.binary.clone(),
            source,
        }),
    }
}

fn vanished(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESRCH)
}

/// Whether a failed read of a `/proc/<pid>` file may be skipped.
///
/// What: a vanished process, or `EACCES` on an entry `/proc` reports as
/// owned by another uid (`hidepid`), is skipped; every other error is not.
/// Test: `read_error_is_skipped_only_for_a_vanished_or_foreign_entry`.
pub(crate) fn skippable(e: &io::Error, pid: u32, euid: u32) -> bool {
    if vanished(e) {
        return true;
    }
    if e.kind() != io::ErrorKind::PermissionDenied {
        return false;
    }
    match std::fs::metadata(format!("/proc/{pid}")) {
        Ok(meta) => meta.uid() != euid,
        Err(stat) => vanished(&stat),
    }
}

/// Whether `argv[1..]` of a NUL-separated `/proc/<pid>/cmdline` holds every
/// element of `wanted` as a whole argument.
///
/// Test: `argv_match_needs_every_liveness_arg_as_a_whole_argument`.
pub(crate) fn argv_matches(cmdline: &[u8], wanted: &[String]) -> bool {
    let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).skip(1).collect();
    wanted.iter().all(|w| args.contains(&w.as_bytes()))
}

/// Whether a NUL-separated environment block holds every `wanted` value,
/// where `None` requires the name to be absent. The first entry of a name
/// counts, as with `getenv`.
///
/// Test: `environ_match_needs_every_instance_value_and_unset_equals_unset`.
pub(crate) fn environ_matches(environ: &[u8], wanted: &[(String, Option<OsString>)]) -> bool {
    wanted.iter().all(|(name, value)| {
        let found = environ
            .split(|b| *b == 0)
            .find_map(|entry| entry.strip_prefix(name.as_bytes())?.strip_prefix(b"="));
        found == value.as_ref().map(|v| v.as_encoded_bytes())
    })
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
