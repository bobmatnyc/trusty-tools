//! The `palace locks` doctor row: stray lock files, and the maintenance lease.
//!
//! Why (#8751): the maintenance lease from #8733 is a `*.lock` file in the
//! registry root that a running daemon holds for its whole lifetime. The old
//! scan listed it beside crash leftovers and told the operator the files "can
//! be removed", so a healthy daemon's live lease read as a stale lock to delete.
//! What: [`check_stale_palace_locks`] scans as before, then classifies the
//! lease by probing the pid its holder recorded. A live holder is reported as
//! held, a dead holder as stale, and a probe that learns nothing fails closed
//! as undetermined — never as "stale, delete it".
//! Test: `palace_locks_tests`.

use std::io;
use std::path::{Path, PathBuf};

use trusty_common::memory_core::maintenance_lease::MAINTENANCE_LOCK_FILE;

use super::checks::find_lock_files;
use super::CheckResult;

/// What the pid recorded in `maintenance.lock` says about its holder (#8751).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LeaseHolder {
    /// The recorded pid is a running process: the lease is live.
    Live(u32),
    /// The recorded pid is not running: the lease file is left over.
    Dead(u32),
    /// The file or the probe gave no answer. Carries the reason.
    Undetermined(String),
}

/// Classify the lease at `path` by probing its recorded holder pid.
///
/// Why (#8751): only a dead holder makes the file safe to remove, and a read or
/// probe error is not evidence of death. Every arm that did not observe a dead
/// process maps to `Undetermined`, so an error can never advise deletion.
/// What: reads the file, parses one pid (0 is refused: `kill(0, 0)` probes the
/// caller's process group), and asks `probe` whether it runs. `Ok(true)` is
/// `Live`, `Ok(false)` is `Dead`, and a read, parse or probe error is
/// `Undetermined`.
/// Test: `a_live_holder_lease_is_not_stale`, `a_dead_holder_lease_is_stale`,
/// `an_unreadable_or_unparseable_lease_is_undetermined`.
pub(super) fn classify_lease(path: &Path, probe: impl Fn(u32) -> io::Result<bool>) -> LeaseHolder {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => return LeaseHolder::Undetermined(format!("could not read it: {e}")),
    };
    let pid = match content.trim().parse::<u32>() {
        Ok(pid) if pid != 0 => pid,
        _ => {
            return LeaseHolder::Undetermined(format!(
                "it does not hold a holder pid (content {:?})",
                content.trim()
            ));
        }
    };
    match probe(pid) {
        Ok(true) => LeaseHolder::Live(pid),
        Ok(false) => LeaseHolder::Dead(pid),
        Err(e) => LeaseHolder::Undetermined(format!("probing holder pid {pid} failed: {e}")),
    }
}

/// Whether `pid` names a running process (#8751).
///
/// Why: the existing `kill -0` helper in `stop.rs` maps every error to "not
/// running", which here would turn a probe failure into "stale, delete it".
/// What: `kill(pid, 0)` sends no signal. Success or `EPERM` (exists, not ours)
/// is `Ok(true)`, `ESRCH` is `Ok(false)`, and anything else is an error.
/// Test: `a_live_holder_lease_is_not_stale` (probes this test process's pid).
#[cfg(unix)]
pub(super) fn pid_liveness(pid: u32) -> io::Result<bool> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid exceeds pid_t"))?;
    // SAFETY: signal 0 delivers nothing; kill only checks that `pid` exists and
    // may be signalled. No memory is passed.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EPERM) => Ok(true),
        Some(libc::ESRCH) => Ok(false),
        _ => Err(err),
    }
}

/// Non-unix hosts have no pid probe, so every lease is undetermined (#8751).
#[cfg(not(unix))]
pub(super) fn pid_liveness(_pid: u32) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no pid probe on this platform",
    ))
}

/// Scan the data directory for stray `*.lock` files and classify the lease.
///
/// Why: redb leaves a sidecar lock file when a previous owner exits uncleanly,
/// and a fresh daemon then cannot open the palace; surfacing it saves a
/// confusing "palace won't load" hunt. #8751: the maintenance lease must not
/// be listed with them while its holder runs.
/// What: resolves the registry dir and hands it to [`palace_locks_verdict`]
/// with the real [`pid_liveness`] probe.
/// Test: `a_live_holder_lease_is_not_stale`, `a_dead_holder_lease_is_stale`.
pub fn check_stale_palace_locks() -> CheckResult {
    let label = "palace locks".to_string();
    let data_dir = match trusty_common::resolve_data_dir("trusty-memory") {
        Ok(d) => d,
        Err(e) => return CheckResult::fail(label, format!("could not resolve data dir: {e}")),
    };
    let root = crate::resolve_palace_registry_dir(data_dir);
    palace_locks_verdict(label, &root, pid_liveness)
}

/// The `palace locks` verdict for `root`, with the pid probe supplied (#8751).
///
/// Why: a test must drive a live, a dead, and an unprobeable holder without a
/// real daemon, and the data-dir resolver reads process-wide env.
/// What: `Unknown` when the lease holder is undetermined — the reason is named
/// and deletion is advised against. Otherwise `Warn` listing every stray lock
/// plus a dead-holder lease, with the removal hint; `Pass` when that list is
/// empty. A live lease is named in the detail and never listed as removable.
/// Test: `a_live_holder_lease_is_not_stale`, `a_dead_holder_lease_is_stale`,
/// `an_unreadable_or_unparseable_lease_is_undetermined`,
/// `a_stray_lock_beside_a_live_lease_still_warns`.
pub(super) fn palace_locks_verdict(
    label: String,
    root: &Path,
    probe: impl Fn(u32) -> io::Result<bool>,
) -> CheckResult {
    let lease_path = root.join(MAINTENANCE_LOCK_FILE);
    let (lease, mut stale): (Vec<PathBuf>, Vec<PathBuf>) = find_lock_files(root)
        .into_iter()
        .partition(|p| p == &lease_path);
    // #8751: the lease is removable only when its holder is observed dead.
    let mut notes = Vec::new();
    match lease.first().map(|p| classify_lease(p, &probe)) {
        None => {}
        Some(LeaseHolder::Live(pid)) => notes.push(format!(
            "maintenance lease {} held by running pid {pid} (not stale; do not remove)",
            lease_path.display()
        )),
        Some(LeaseHolder::Dead(pid)) => {
            notes.push(format!("maintenance lease holder pid {pid} is not running"));
            stale.push(lease_path.clone());
        }
        Some(LeaseHolder::Undetermined(reason)) => {
            // #8751: fail closed — an error is never "stale, delete it".
            return CheckResult::unknown(
                label,
                format!(
                    "cannot determine whether {} is held: {reason}. Do not remove it while \
                     any trusty-memory process may be running.{}",
                    lease_path.display(),
                    stray_suffix(&stale)
                ),
            );
        }
    }
    let notes = notes.iter().map(|n| format!("; {n}")).collect::<String>();
    if stale.is_empty() {
        return CheckResult::pass(label, format!("{} clean{notes}", root.display()));
    }
    CheckResult::warn(
        label,
        format!(
            "{} lock file(s) found: {} — if the daemon is stopped, these can be removed{notes}",
            stale.len(),
            preview(&stale)
        ),
    )
}

/// Up to three paths, comma-joined, with a "(+N more)" tail.
fn preview(paths: &[PathBuf]) -> String {
    let shown = paths
        .iter()
        .take(3)
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    match paths.len() {
        n if n > 3 => format!("{shown} (+{} more)", n - 3),
        _ => shown,
    }
}

/// The stray-lock sentence appended to an undetermined verdict, if any.
fn stray_suffix(stale: &[PathBuf]) -> String {
    if stale.is_empty() {
        return String::new();
    }
    format!(
        " Separately, {} other lock file(s) found: {}.",
        stale.len(),
        preview(stale)
    )
}

#[cfg(test)]
#[path = "palace_locks_tests.rs"]
mod palace_locks_tests;
