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
//! runs `desc.binary` with every `desc.liveness_args` element, its environment
//! holds every `desc.instance_env` value, and its argv gives no
//! `desc.instance_path_args` flag only another path (#11085). `liveness_args` tells the
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
//! `stop_ignores_an_unrelated_process_whose_name_is_not_utf8`,
//! `a_foreign_uid_entry_in_the_process_table_is_skipped_by_stop_start_and_restart`,
//! `a_same_uid_entry_with_an_unreadable_environment_in_the_process_table_fails_closed`,
//! `argv_match_needs_every_liveness_arg_as_a_whole_argument`,
//! `environ_match_needs_every_instance_value_and_unset_equals_unset`,
//! `read_error_is_skipped_only_for_a_vanished_or_foreign_entry`,
//! `status_parse_reads_state_and_effective_uid`,
//! `unparsable_status_fails_the_scan_instead_of_skipping_the_pid`.
//!
//! [`LifecycleError::Unidentified`]: crate::linux_lifecycle::LifecycleError::Unidentified
//! [`LifecycleError::RunningUnserved`]: crate::linux_lifecycle::LifecycleError::RunningUnserved

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use super::instance::{PathVerdict, path_verdict};
use super::process::exe_name_at;
use super::{DaemonDescriptor, LifecycleError, StoppedSignal, lock_is_free};

/// The process table the `/proc` scan reads (#11085).
///
/// Why: the scan read the host's live `/proc`, so a consumer's lifecycle test
/// changed result with the processes the host ran. A same-uid daemon whose
/// `environ` cannot be read fails the scan closed, by design (#11098), and
/// that failed a test on a host that ran one. A test injects a table that
/// holds only its own processes and the entries it fakes.
/// What: a directory laid out as `/proc`: one `<pid>` entry per process, with
/// `status`, `cmdline`, `environ` and the `exe` link. The scan reads those
/// and the entry's owner; the caller's effective uid stays this process's.
/// An entry may be a symlink to `/proc/<pid>`, which shows the scan that one
/// real process. [`ProcessTable::system`] is `/proc`, and every entry point
/// without `_with_table` uses it. Only the scan reads the table: a stop by pid
/// reads its pinned process in the real `/proc`, because the pidfd proves
/// only that process.
/// Test: `a_foreign_uid_entry_in_the_process_table_is_skipped_by_stop_start_and_restart`,
/// `a_same_uid_entry_with_an_unreadable_environment_in_the_process_table_fails_closed`,
/// `the_system_process_table_is_proc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTable {
    root: PathBuf,
}

impl ProcessTable {
    /// The real process table, `/proc`.
    pub fn system() -> Self {
        Self::at("/proc")
    }

    /// The table laid out under `root`, which should be absolute.
    ///
    /// A missing or unreadable `root` fails the scan with
    /// [`LifecycleError::Io`], as an unreadable `/proc` does.
    ///
    /// [`LifecycleError::Io`]: crate::linux_lifecycle::LifecycleError::Io
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory the scan lists.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<pid>/<file>`.
    fn entry(&self, pid: u32, file: &str) -> PathBuf {
        self.root.join(pid.to_string()).join(file)
    }
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::system()
    }
}

/// Fail when the socket does not serve but the daemon may still run.
///
/// Why: see the module docs; "socket absent ⇒ not running" is the defect.
/// What: the lock check (lock signal only), then the scan of `table`.
/// Test: `stop_start_and_restart_refuse_while_a_daemon_runs_without_its_socket`,
/// `stop_and_restart_refuse_when_the_lock_is_held_without_a_socket`.
pub(crate) fn refuse_unserved_daemon(
    desc: &DaemonDescriptor,
    table: &ProcessTable,
) -> Result<(), LifecycleError> {
    if let StoppedSignal::LockFree(lock) = &desc.stopped_signal
        && !lock_is_free(lock)?
    {
        return Err(LifecycleError::Unidentified {
            socket: desc.socket.clone(),
            lock: lock.clone(),
        });
    }
    let pids = matching_processes(desc, &[], table)?;
    if pids.is_empty() {
        return Ok(());
    }
    Err(LifecycleError::RunningUnserved {
        socket: desc.socket.clone(),
        binary: desc.binary.clone(),
        pids,
    })
}

/// Fail before a restart's stop when another process is this instance.
///
/// Why: #11085 — `start` refuses while the scan finds this instance running,
/// so a restart that stopped a healthy daemon and then met such a process left
/// the daemon down. Run before the stop, the same scan costs nothing to undo.
/// What: the scan, with `serving` (the running daemon's own pids) left out.
/// Any match is [`LifecycleError::RestartBlocked`]; a scan error is returned
/// as is. The lock signal is not checked: the running daemon holds it.
/// Test: `restart_refuses_before_any_signal_when_another_process_is_this_instance`,
/// `restart_pid_refuses_before_any_signal_when_another_process_is_this_instance`.
///
/// [`LifecycleError::RestartBlocked`]: crate::linux_lifecycle::LifecycleError::RestartBlocked
pub(crate) fn refuse_other_instances(
    desc: &DaemonDescriptor,
    serving: &[u32],
    table: &ProcessTable,
) -> Result<(), LifecycleError> {
    let others = matching_processes(desc, serving, table)?;
    if others.is_empty() {
        return Ok(());
    }
    Err(LifecycleError::RestartBlocked {
        serving: serving.to_vec(),
        binary: desc.binary.clone(),
        others,
    })
}

/// Live pids, other than this process and `exclude`, of this effective uid,
/// that are the instance `desc` describes; ascending.
///
/// What: walks `table` (`/proc` in production). A pid that vanishes mid-read,
/// or another uid's entry `hidepid` hides, is skipped; every other read error
/// is an error, never a skip, because the process might be the daemon. An
/// unreadable `/proc/<pid>/exe` (an undumpable process) falls back to
/// `argv[0]`'s file name; an unreadable `environ` cannot fall back and is an
/// error.
fn matching_processes(
    desc: &DaemonDescriptor,
    exclude: &[u32],
    table: &ProcessTable,
) -> Result<Vec<u32>, LifecycleError> {
    let me = std::process::id();
    let euid = effective_uid();
    let io_err = |context: &str, source| LifecycleError::Io {
        context: context.to_string(),
        source,
    };
    let mut found = Vec::new();
    // #11085: the table root is injectable; production passes `/proc`.
    let root = table.root.display();
    let entries = std::fs::read_dir(&table.root).map_err(|e| io_err(&format!("read {root}"), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_err(&format!("read {root} entry"), e))?;
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        if pid != me
            && !exclude.contains(&pid)
            && process_instance(pid, euid, desc, table)? != Instance::Not
        {
            found.push(pid);
        }
    }
    found.sort_unstable();
    Ok(found)
}

/// This process's effective uid.
pub(crate) fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Whether a process is the instance a descriptor describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Instance<'a> {
    /// Gone, another uid, a zombie, or a mismatch.
    Not,
    /// Binary, `liveness_args`, `instance_path_args` and `instance_env` match.
    This,
    /// All else matches, but this `instance_path_args` flag does not decide.
    Undecided(&'a str),
}

/// Whether `pid`, as `table` shows it, is a live process of `euid` that is
/// the instance `desc` describes: binary, `liveness_args`,
/// `instance_path_args`, `instance_env`.
///
/// What: [`Instance::Not`] for a vanished process, another uid, a zombie, or a
/// mismatch; [`Instance::Undecided`] when only a path flag cannot be decided
/// (the scan counts it as a match, a stop by pid refuses); an error for a read
/// that cannot be classified. The uid comes from `status`, before `cmdline`,
/// `exe` or `environ` is read.
pub(crate) fn process_instance<'d>(
    pid: u32,
    euid: u32,
    desc: &'d DaemonDescriptor,
    table: &ProcessTable,
) -> Result<Instance<'d>, LifecycleError> {
    // #11098: only a vanished or foreign entry is skipped; EMFILE, ENOMEM or
    // EIO here once read a live daemon as not running.
    let status_path = table.entry(pid, "status");
    let status = match std::fs::read(&status_path) {
        Ok(s) => s,
        Err(e) if skippable(&e, table, pid, euid) => return Ok(Instance::Not),
        Err(source) => {
            return Err(LifecycleError::Io {
                context: format!("read {}", status_path.display()),
                source,
            });
        }
    };
    if !status_admits(&status, pid, euid)? {
        return Ok(Instance::Not);
    }
    let cmdline = match std::fs::read(table.entry(pid, "cmdline")) {
        Ok(c) => c,
        Err(e) if vanished(&e) => return Ok(Instance::Not),
        Err(source) => return Err(LifecycleError::IdentityUnreadable { pid, source }),
    };
    if !argv_matches(&cmdline, &desc.liveness_args) {
        return Ok(Instance::Not);
    }
    let name = match exe_name_at(&table.entry(pid, "exe")) {
        Ok(name) => name,
        // A kernel thread has no exe; an exited process no longer has one.
        Err(e) if vanished(&e) => return Ok(Instance::Not),
        // An undumpable process hides its exe; any other error fails closed.
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => argv0_name(&cmdline),
        Err(source) => return Err(LifecycleError::IdentityUnreadable { pid, source }),
    };
    if name != desc.binary {
        return Ok(Instance::Not);
    }
    // #11085: `--data-dir X` with the data-dir variable unset is a sibling.
    let path = match path_verdict(&cmdline, &desc.instance_path_args) {
        PathVerdict::Other => return Ok(Instance::Not),
        PathVerdict::Same => Instance::This,
        PathVerdict::Undecided(flag) => Instance::Undecided(flag),
    };
    if desc.instance_env.is_empty() {
        return Ok(path);
    }
    // #11098: a sibling instance (same argv, another data dir) is not this one.
    match std::fs::read(table.entry(pid, "environ")) {
        Ok(environ) if environ_matches(&environ, &desc.instance_env) => Ok(path),
        Ok(_) => Ok(Instance::Not),
        Err(e) if vanished(&e) => Ok(Instance::Not),
        Err(source) => Err(LifecycleError::InstanceUnreadable {
            pid,
            binary: desc.binary.clone(),
            source,
        }),
    }
}

/// Whether a `/proc/<pid>/status` body is a live process of `euid`.
///
/// What: `Ok(false)` for another uid or a zombie/dead state; an unparsable
/// body is [`LifecycleError::Io`], never a skip.
/// Test: `unparsable_status_fails_the_scan_instead_of_skipping_the_pid`.
///
/// [`LifecycleError::Io`]: crate::linux_lifecycle::LifecycleError::Io
pub(crate) fn status_admits(status: &[u8], pid: u32, euid: u32) -> Result<bool, LifecycleError> {
    // #11098: a skipped unparsable status once read a live daemon as not running.
    let Some((state, uid)) = parse_status(status) else {
        return Err(LifecycleError::Io {
            context: format!("/proc/{pid}/status: unparsable"),
            source: io::Error::new(io::ErrorKind::InvalidData, "no State: or Uid: field"),
        });
    };
    Ok(uid == euid && !matches!(state, 'Z' | 'X'))
}

fn vanished(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESRCH)
}

/// Whether a failed read of a `/proc/<pid>` file may be skipped.
///
/// What: a vanished process, or `EACCES` on an entry `table` reports as
/// owned by another uid (`hidepid`), is skipped; every other error is not.
/// Test: `read_error_is_skipped_only_for_a_vanished_or_foreign_entry`.
pub(crate) fn skippable(e: &io::Error, table: &ProcessTable, pid: u32, euid: u32) -> bool {
    if vanished(e) {
        return true;
    }
    if e.kind() != io::ErrorKind::PermissionDenied {
        return false;
    }
    match std::fs::metadata(table.root.join(pid.to_string())) {
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

/// The state letter and the effective uid from `/proc/<pid>/status` bytes.
///
/// What: only the `State:` and `Uid:` lines are decoded as UTF-8; other lines,
/// such as a `Name:` holding a non-UTF-8 `comm`, are never decoded.
/// Test: `status_parse_reads_state_and_effective_uid`.
pub(crate) fn parse_status(status: &[u8]) -> Option<(char, u32)> {
    // #11098: a whole-body UTF-8 read let one non-UTF-8 `comm` fail the scan.
    let field = |key: &[u8]| {
        let value = status
            .split(|b| *b == b'\n')
            .find_map(|l| l.strip_prefix(key))?;
        std::str::from_utf8(value).ok().map(str::trim_start)
    };
    let state = field(b"State:")?.chars().next()?;
    // `Uid:` lists real, effective, saved and filesystem uids.
    let euid = field(b"Uid:")?.split_whitespace().nth(1)?.parse().ok()?;
    Some((state, euid))
}
