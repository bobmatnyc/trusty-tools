//! Down versus slow: what bare `tm` may conclude from a failed session listing.
//!
//! Why: bare `tm` read every failed listing as "daemon down", auto-started a
//! second daemon and then redirected the PM pane to a managed clone. A daemon
//! whose store stalled for 60 s took that path in seven panes at once (#9034).
//! A timeout proves the daemon accepted the request; it proves nothing about
//! the daemon being absent.
//! What: [`classify_list_failure`] maps a listing error plus the lock file's
//! live pid to [`DaemonReach`]. [`picker_or_autostart`] is the picker,
//! autostart and retry sequence of the guided default; only a `Down` verdict
//! reaches autostart, and only a failed autostart of a down daemon reaches the
//! offline fallback.
//! Test: `guided_liveness_tests.rs`.

use std::future::Future;
use std::path::Path;

/// The project a picker attempt is about — the four values `try_show_picker`
/// needs, passed as one borrow.
#[derive(Clone, Copy)]
pub(crate) struct PickerProject<'a> {
    /// `owner/repo` the listing is filtered by.
    pub source_id: &'a str,
    /// Managed workspace path shown in the project context.
    pub workspace: &'a Path,
    /// Git root, sent as `repo_url` so the daemon resolves `source_id`.
    pub repo_url: &'a str,
    /// The operator's working directory, for the banner.
    pub cwd: &'a Path,
}

/// Result of one picker attempt: it ran, or the listing it needs failed.
pub(crate) enum PickerAttempt {
    /// The daemon answered and the picker (or non-TTY summary) ran.
    Shown(anyhow::Result<()>),
    /// The session listing failed; the error decides down versus slow.
    ListFailed(anyhow::Error),
}

/// What the evidence says about the daemon after a failed listing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DaemonReach {
    /// Positive evidence it is not running: the connection was refused and no
    /// live pid holds the lock for this address.
    Down,
    /// It may be running. The string says why `tm` will not treat it as down.
    Unknown(String),
}

/// Where the guided default goes after [`picker_or_autostart`].
pub(crate) enum PickerFlow {
    /// Finished: the picker ran, or a slow daemon was reported.
    Done(anyhow::Result<()>),
    /// The daemon is down and could not be started; take the offline fallback.
    Offline,
}

/// Autostart saw liveness evidence (a launchd job reporting `state = running`,
/// a live daemon lock pid, or a spawned child still starting) but `/health`
/// never answered. The daemon exists; it is slow.
#[derive(Debug, thiserror::Error)]
#[error("the daemon is running ({evidence}) but did not answer /health within 5 s")]
pub(crate) struct DaemonAliveUnresponsive {
    /// The evidence that the daemon exists.
    pub evidence: String,
}

/// Autostart found a live `daemon.lock` pid it could not confirm as a
/// daemon, and kept the lock (#9034). The operator decides.
#[derive(Debug, thiserror::Error)]
#[error("not starting a daemon: {evidence}")]
pub(crate) struct DaemonLockUnverified {
    /// The lock evidence and its recovery.
    pub evidence: String,
}

/// The host facts [`picker_or_autostart_with`] reads, injected for tests.
pub(crate) struct HostProbe<'a> {
    /// The daemon lock file.
    pub lock_path: &'a Path,
    /// What the process table says about a pid.
    pub identify: super::guided_autostart_plan::IdentifyPid<'a>,
    /// The restart command named in the stop message.
    pub restart_cmd: &'a str,
}

/// Classify a failed session listing as down or unknown.
///
/// Why: #9034 — "down" is the only verdict that may start a daemon or redirect
/// to a managed clone, so it needs positive evidence. A timeout, an HTTP error
/// status, or an unreadable body all mean a process answered the socket.
/// What: a non-`reqwest` error is `Unknown`; a `reqwest` error is decided by
/// [`reach_from_flags`].
/// Test: `classify_timeout_is_unknown_not_down`,
/// `classify_refused_with_live_lock_pid_is_unknown`,
/// `classify_refused_without_live_pid_is_down`.
pub(crate) fn classify_list_failure(
    err: &anyhow::Error,
    lock_evidence: Option<&str>,
) -> DaemonReach {
    let Some(re) = err.chain().find_map(|c| c.downcast_ref::<reqwest::Error>()) else {
        return DaemonReach::Unknown(format!("it answered with an unusable reply: {err}"));
    };
    reach_from_flags(
        re.is_timeout(),
        re.is_connect(),
        lock_evidence,
        &re.to_string(),
    )
}

/// The down/unknown decision over a `reqwest` error's flags.
///
/// Why: a connect-phase timeout sets BOTH `is_connect` and `is_timeout`; it is
/// a listener too busy to accept, so the timeout check must win.
/// What: timeout → `Unknown`; not a connect error → `Unknown`; a connect error
/// while the lock holds a live pid (`lock_evidence`, daemon or unverified) →
/// `Unknown` naming the recovery; otherwise `Down`.
/// Test: `flags_connect_phase_timeout_is_unknown`,
/// `classify_refused_with_live_lock_pid_is_unknown`.
pub(crate) fn reach_from_flags(
    is_timeout: bool,
    is_connect: bool,
    lock_evidence: Option<&str>,
    detail: &str,
) -> DaemonReach {
    // #9034: timeout check must precede is_connect — a connect-phase timeout
    // carries both flags and is a slow daemon, not an absent one.
    if is_timeout {
        return DaemonReach::Unknown("the request timed out".to_string());
    }
    if !is_connect {
        return DaemonReach::Unknown(format!("it answered with an error: {detail}"));
    }
    match lock_evidence {
        // #9034: a refused connection while the lock names a live pid is not
        // positive evidence of absence — the same rule as lock removal.
        Some(evidence) => DaemonReach::Unknown(format!("the connection failed, but {evidence}")),
        None => DaemonReach::Down,
    }
}

/// What the lock at `lock_path` proves about the daemon `url` points at.
///
/// Why: a live pid is evidence about the daemon at the lock's own address
/// only; a lock for port 7880 says nothing about an explicit `--url` elsewhere.
/// #9034: the same rule as lock removal — a live pid counts unless it is
/// positively dead, whether or not its identity is confirmed.
/// What: `None` when the lock is absent, not ours, for another address, or
/// stale; otherwise [`super::guided_autostart_plan::lock_evidence`] of
/// [`super::guided_autostart_plan::lock_verdict`] (no mutation).
/// Test: `live_pid_requires_matching_addr`,
/// `unverified_lock_pid_is_evidence_not_absence`.
pub(crate) fn lock_evidence_for(
    url: &str,
    lock_path: &Path,
    identify: super::guided_autostart_plan::IdentifyPid<'_>,
) -> Option<String> {
    use super::guided_autostart_plan::{lock_evidence, lock_verdict};
    let text = std::fs::read_to_string(lock_path).ok()?;
    let lock = trusty_mpm::core::daemon_identity::parse_lock(&text)?;
    let addr = lock.addr.trim_end_matches('/');
    if addr.is_empty() || !url.trim_end_matches('/').starts_with(addr) {
        return None;
    }
    lock_evidence(lock_verdict(lock_path, identify))
}

/// The error bare `tm` stops with when the daemon may be running.
///
/// What: names the evidence and, #9034, the host's restart command
/// ([`super::launchd_probe::daemon_restart_command_for`]).
/// Test: `slow_listing_stops_without_autostart`.
pub(crate) fn slow_daemon_error(url: &str, why: &str, restart_cmd: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "the trusty-mpm daemon at {url} may be running but is not responding ({why}). \
         Not starting a second daemon and not redirecting to a managed clone. \
         Run `tm` again in a moment; if it stays unresponsive, restart it with \
         `{restart_cmd}`. `tm doctor` and ~/.trusty-mpm/daemon.log show its state."
    )
}

/// The guided default's picker → autostart → retry sequence.
///
/// Why: #9034 — this sequence decided "down" from any failed listing.
/// What: [`picker_or_autostart_with`] against the real lock file, the
/// argv-reading [`super::daemon_pid_identity::pid_identity`], the host's
/// restart command, and
/// [`super::guided_autostart::ensure_daemon_started`].
/// Test: `slow_listing_stops_without_autostart` and siblings, through
/// [`picker_or_autostart_with`].
pub(crate) async fn picker_or_autostart(
    client: &reqwest::Client,
    url: &str,
    project: &PickerProject<'_>,
) -> PickerFlow {
    let lock = trusty_mpm::core::lock_file_path();
    let identify = super::daemon_pid_identity::pid_identity;
    let restart_cmd = super::launchd_probe::daemon_restart_command();
    let host = HostProbe {
        lock_path: &lock,
        identify: &identify,
        restart_cmd: &restart_cmd,
    };
    picker_or_autostart_with(client, url, project, &host, || {
        super::guided_autostart::ensure_daemon_started(client, url)
    })
    .await
}

/// [`picker_or_autostart`] with the host facts and the autostart step injected.
///
/// Why: the regression tests must drive the decision against stub servers
/// and a temp lock, and must never start a real daemon or touch the real
/// `~/.trusty-mpm/daemon.lock`.
/// What: (1) try the picker; (2) on a listing failure, classify it with the
/// lock's live daemon pid — `Unknown` stops with [`slow_daemon_error`];
/// (3) `Down` runs `autostart`; (4) after a successful autostart the daemon
/// answered `/health`, so a second listing failure also stops; (5) an
/// autostart error of type [`DaemonAliveUnresponsive`] stops; any other
/// autostart error is [`PickerFlow::Offline`].
/// Test: `slow_listing_stops_without_autostart`,
/// `refused_with_live_lock_pid_stops_without_autostart`,
/// `refused_with_unknown_identity_lock_pid_stops_and_keeps_the_lock`,
/// `unverified_lock_autostart_error_stops_not_offline`,
/// `refused_without_live_pid_autostarts_then_goes_offline`,
/// `slow_listing_after_autostart_stops_not_offline`,
/// `alive_unresponsive_autostart_stops_not_offline`.
pub(crate) async fn picker_or_autostart_with<F, Fut>(
    client: &reqwest::Client,
    url: &str,
    project: &PickerProject<'_>,
    host: &HostProbe<'_>,
    autostart: F,
) -> PickerFlow
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = anyhow::Result<String>>,
{
    let stop =
        |at: &str, why: &str| PickerFlow::Done(Err(slow_daemon_error(at, why, host.restart_cmd)));
    let err = match super::guided::try_show_picker(client, url, project).await {
        PickerAttempt::Shown(r) => return PickerFlow::Done(r),
        PickerAttempt::ListFailed(e) => e,
    };
    // #9034: only positive evidence of absence may start a daemon.
    let evidence = lock_evidence_for(url, host.lock_path, host.identify);
    if let DaemonReach::Unknown(why) = classify_list_failure(&err, evidence.as_deref()) {
        return stop(url, &why);
    }
    eprintln!("tm: daemon not running — starting it…");
    match autostart().await {
        Ok(new_url) => match super::guided::try_show_picker(client, &new_url, project).await {
            PickerAttempt::Shown(r) => PickerFlow::Done(r),
            // #9034: `/health` just answered, so this daemon is up and slow.
            PickerAttempt::ListFailed(e) => stop(&new_url, &format!("{e:#}")),
        },
        // #9034: a running launchd job, a live daemon lock pid, or a spawned
        // child still starting is a slow daemon.
        // #9034: an unverified live lock pid fails closed too.
        Err(e)
            if e.downcast_ref::<DaemonAliveUnresponsive>().is_some()
                || e.downcast_ref::<DaemonLockUnverified>().is_some() =>
        {
            stop(url, &format!("{e}"))
        }
        Err(e) => {
            eprintln!("tm: auto-start failed ({e}); falling back to offline mode");
            PickerFlow::Offline
        }
    }
}

#[cfg(test)]
#[path = "guided_liveness_tests.rs"]
mod tests;
