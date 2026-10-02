//! Is this pid a trusty-mpm daemon? Three answers, read from argv (#9034).
//!
//! Why: `find_daemon_pids` refreshed the process table with
//! `ProcessRefreshKind::nothing()`, which never loads argv, so `cmd()` was
//! always empty and no process ever matched. The autostart lock check then
//! called a LIVE daemon "not a daemon" and deleted its lock. A pid whose argv
//! cannot be read (another user's process, KERN_PROCARGS2 denied) must not be
//! mistaken for "not a daemon" either.
//! What: [`classify_process`] is the pure rule over a process name and argv;
//! [`refresh_with_cmd`] is the one refresh that loads argv; [`pid_identity`]
//! answers for one pid.
//! Test: `classify_process_*`, `real_pid_identity_reads_argv_for_this_process`,
//! `real_pid_identity_never_calls_sleep_a_daemon`,
//! `find_daemon_pids_finds_a_tm_daemon_process` in
//! `daemon_pid_identity_tests.rs`.

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// What the process table says about one pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidIdentity {
    /// A tm/trusty-mpm binary running the `daemon` subcommand.
    Daemon,
    /// Positively another program, or a tm CLI invocation.
    NotDaemon,
    /// The argv could not be read, or the pid is not in the table.
    Unknown,
}

/// Classify a process from its name and argv.
///
/// Why: one rule for `tm stop` and for the autostart lock check.
/// What: empty `cmd` → `Unknown` (argv unreadable; never a verdict); a name in
/// `OWN_BINARY_NAMES` with a `daemon` argument → `Daemon`; else `NotDaemon`.
/// Test: `classify_process_daemon`, `classify_process_cli_is_not_daemon`,
/// `classify_process_empty_argv_is_unknown`.
pub(crate) fn classify_process(name: &str, cmd: &[String]) -> PidIdentity {
    if cmd.is_empty() {
        return PidIdentity::Unknown;
    }
    let is_tm_binary = trusty_mpm::core::own_binary_names::OWN_BINARY_NAMES.contains(&name);
    if is_tm_binary && cmd.iter().any(|a| a == "daemon") {
        PidIdentity::Daemon
    } else {
        PidIdentity::NotDaemon
    }
}

/// Refresh `which` processes WITH argv loaded.
///
/// Why: #9034 — `ProcessRefreshKind::nothing()` leaves `cmd()` empty.
/// Test: `real_pid_identity_reads_argv_for_this_process`.
pub(crate) fn refresh_with_cmd(which: ProcessesToUpdate<'_>) -> System {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        which,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet),
    );
    sys
}

/// The name and argv of a process in `sys`, as strings.
pub(crate) fn name_and_cmd(proc_: &sysinfo::Process) -> (String, Vec<String>) {
    let name = proc_.name().to_string_lossy().into_owned();
    let cmd = proc_
        .cmd()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    (name, cmd)
}

/// The production identity check for one pid.
///
/// What: refreshes only `pid` with argv, then [`classify_process`]; a pid
/// missing from the table is `Unknown`.
/// Test: `real_pid_identity_reads_argv_for_this_process`,
/// `real_pid_identity_never_calls_sleep_a_daemon`.
pub(crate) fn pid_identity(pid: u32) -> PidIdentity {
    let target = Pid::from_u32(pid);
    let sys = refresh_with_cmd(ProcessesToUpdate::Some(&[target]));
    match sys.process(target) {
        Some(p) => {
            let (name, cmd) = name_and_cmd(p);
            classify_process(&name, &cmd)
        }
        None => PidIdentity::Unknown,
    }
}

#[cfg(test)]
#[path = "daemon_pid_identity_tests.rs"]
mod tests;
