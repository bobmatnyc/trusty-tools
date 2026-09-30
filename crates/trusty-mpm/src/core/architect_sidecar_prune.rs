//! `tm fleet init`'s prune of stale Architect launch sidecars (#8942, ruling 2).
//!
//! Why: every `*.architect-session` sidecar's name is protected from
//! kill-by-name whether or not its launch is live, so without a prune a
//! crashed Architect's name stays protected forever. Removing a live one would
//! unprotect the Architect, so a sidecar is removed only when three separate
//! checks all say its launch is gone.
//! What: [`prune_stale_sidecars`] removes a sidecar only when its pid is dead
//! ([`SidecarProbes::pid_alive`] is `Some(false)`), the process table holds no
//! process with the recorded start time ([`SidecarProbes::start_time`]), and
//! no tmux session with its name exists ([`SidecarProbes::session_exists`] is
//! `Some(false)`). Any probe that cannot answer, and any sidecar that does not
//! read, keeps the sidecar. The `<pid>.architect` record is left alone.
//! Test: `architect_sidecar_prune_tests.rs`.

use std::path::Path;

use crate::core::architect_launch::ARCHITECT_DIR;
use crate::core::architect_session::{SESSION_EXT, read_sidecar};

/// The three facts the prune asks about one sidecar; injectable for tests.
#[derive(Clone, Copy)]
pub struct SidecarProbes<'a> {
    /// Whether `pid` is alive; `None` when it cannot be told.
    pub pid_alive: &'a dyn Fn(u32) -> Option<bool>,
    /// `pid`'s start time; `Ok(None)` when no such process; `Err` when it
    /// cannot be told.
    pub start_time: &'a dyn Fn(u32) -> Result<Option<u64>, String>,
    /// Whether tmux session `name` exists; `None` when tmux cannot say.
    pub session_exists: &'a dyn Fn(&str) -> Option<bool>,
}

/// What one prune pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Session names whose sidecar was removed.
    pub pruned: Vec<String>,
    /// Sidecars kept although not proven live, each with the reason.
    pub kept: Vec<String>,
}

/// Remove the stale sidecars under `root/architect-launch/`; see the module doc.
///
/// # Errors
///
/// When the directory exists but cannot be listed; nothing is removed then.
pub fn prune_stale_sidecars(root: &Path, probes: SidecarProbes<'_>) -> Result<PruneReport, String> {
    let dir = root.join(ARCHITECT_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PruneReport::default()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
        Ok(entries) => entries,
    };
    let mut report = PruneReport::default();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some(SESSION_EXT) {
            continue;
        }
        let (pid, recorded, session) = match read_sidecar(&path) {
            Ok(sidecar) => sidecar,
            Err(why) => {
                report.kept.push(format!("unreadable sidecar kept: {why}"));
                continue;
            }
        };
        match stale(pid, recorded, &session, probes) {
            Ok(()) => match std::fs::remove_file(&path) {
                Ok(()) => report.pruned.push(session),
                Err(e) => report.kept.push(format!("{session}: cannot remove: {e}")),
            },
            Err(Some(why)) => report.kept.push(format!("{session}: {why}")),
            // Proven live: the ordinary case, not worth a line.
            Err(None) => {}
        }
    }
    Ok(report)
}

/// `Ok(())` when all three checks prove the launch gone; `Err(None)` when a
/// check proves it live; `Err(Some(why))` when a check cannot answer.
fn stale(
    pid: u32,
    recorded: u64,
    session: &str,
    p: SidecarProbes<'_>,
) -> Result<(), Option<String>> {
    match (p.pid_alive)(pid) {
        Some(false) => {}
        Some(true) => return Err(None),
        None => return Err(Some(format!("cannot tell whether pid {pid} is alive"))),
    }
    match (p.start_time)(pid) {
        Ok(Some(start)) if start == recorded => return Err(None),
        Ok(_) => {}
        Err(why) => return Err(Some(format!("cannot read pid {pid}'s start time: {why}"))),
    }
    match (p.session_exists)(session) {
        Some(false) => Ok(()),
        Some(true) => Err(None),
        None => Err(Some(format!(
            "cannot tell whether tmux session {session} exists"
        ))),
    }
}

/// The production liveness probe: `kill(pid, 0)`. `ESRCH` is dead; success
/// or `EPERM` (someone else's live process) is alive; anything else, and a
/// pid `kill` would read as a process group, is `None`.
pub fn pid_alive(pid: u32) -> Option<bool> {
    #[cfg(unix)]
    {
        let raw = libc::pid_t::try_from(pid).ok().filter(|p| *p > 0)?;
        // SAFETY: signal 0 checks existence and permission only; nothing is sent.
        if unsafe { libc::kill(raw, 0) } == 0 {
            return Some(true);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Some(false),
            Some(libc::EPERM) => Some(true),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// The production start-time probe: the process table's entry for `pid`, if
/// it holds one.
pub fn start_time(pid: u32) -> Result<Option<u64>, String> {
    Ok(crate::core::twin_arming::process_facts(pid)
        .ok()
        .map(|facts| facts.start_time))
}

#[cfg(test)]
#[path = "architect_sidecar_prune_tests.rs"]
mod tests;
