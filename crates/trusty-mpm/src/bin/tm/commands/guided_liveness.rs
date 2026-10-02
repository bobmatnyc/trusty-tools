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

/// Autostart saw liveness evidence (a loaded launchd service or a live lock
/// pid) but `/health` never answered. The daemon exists; it is slow.
#[derive(Debug, thiserror::Error)]
#[error("the daemon is running ({evidence}) but did not answer /health within 5 s")]
pub(crate) struct DaemonAliveUnresponsive {
    /// The evidence that the daemon exists.
    pub evidence: String,
}

/// Classify a failed session listing as down or unknown.
///
/// Why: #9034 — "down" is the only verdict that may start a daemon or redirect
/// to a managed clone, so it needs positive evidence. A timeout, an HTTP error
/// status, or an unreadable body all mean a process answered the socket.
/// What: `Down` only when the error chain carries a `reqwest` connect error
/// that is not a timeout and `live_pid` is `None`. Every other shape is
/// `Unknown`, with the reason in the payload.
/// Test: `classify_timeout_is_unknown_not_down`,
/// `classify_refused_with_live_lock_pid_is_unknown`,
/// `classify_refused_without_live_pid_is_down`.
pub(crate) fn classify_list_failure(err: &anyhow::Error, live_pid: Option<u32>) -> DaemonReach {
    let Some(re) = err.chain().find_map(|c| c.downcast_ref::<reqwest::Error>()) else {
        return DaemonReach::Unknown(format!("it answered with an unusable reply: {err}"));
    };
    // #9034: a timeout is "unknown", never "down".
    if re.is_timeout() {
        return DaemonReach::Unknown("the request timed out".to_string());
    }
    if !re.is_connect() {
        return DaemonReach::Unknown(format!("it answered with an error: {re}"));
    }
    match live_pid {
        // #9034: a refused connection with a live lock pid is a daemon that is
        // starting or restarting, not an absent one.
        Some(pid) => DaemonReach::Unknown(format!(
            "the connection failed, but daemon.lock names live pid {pid}"
        )),
        None => DaemonReach::Down,
    }
}

/// The live pid recorded in the lock at `lock_path`, when that lock is ours
/// and names the address `url` points at.
///
/// Why: a live pid is evidence about the daemon at the lock's own address
/// only; a lock for port 7880 says nothing about an explicit `--url` elsewhere.
/// What: parses without mutating (no stale-record cleanup here), then requires
/// `pid_alive` and that `url` starts with the lock's `addr`.
/// Test: `live_pid_requires_matching_addr`.
pub(crate) fn live_daemon_pid_for(url: &str, lock_path: &Path) -> Option<u32> {
    use trusty_mpm::core::daemon_identity::{parse_lock, pid_alive};
    let lock = parse_lock(&std::fs::read_to_string(lock_path).ok()?)?;
    let addr = lock.addr.trim_end_matches('/');
    (!addr.is_empty() && url.trim_end_matches('/').starts_with(addr) && pid_alive(lock.pid))
        .then_some(lock.pid)
}

/// The error bare `tm` stops with when the daemon may be running.
///
/// Test: `slow_listing_stops_without_autostart`.
pub(crate) fn slow_daemon_error(url: &str, why: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "the trusty-mpm daemon at {url} may be running but is not responding ({why}). \
         Not starting a second daemon and not redirecting to a managed clone. \
         Run `tm` again in a moment; `tm doctor` and ~/.trusty-mpm/daemon.log show its state."
    )
}

/// The guided default's picker → autostart → retry sequence.
///
/// Why: #9034 — this sequence decided "down" from any failed listing.
/// What: [`picker_or_autostart_with`] against the real lock file and
/// [`super::guided_autostart::ensure_daemon_started`].
/// Test: `slow_listing_stops_without_autostart` and siblings, through
/// [`picker_or_autostart_with`].
pub(crate) async fn picker_or_autostart(
    client: &reqwest::Client,
    url: &str,
    project: &PickerProject<'_>,
) -> PickerFlow {
    let lock = trusty_mpm::core::lock_file_path();
    picker_or_autostart_with(client, url, project, &lock, || {
        super::guided_autostart::ensure_daemon_started(client, url)
    })
    .await
}

/// [`picker_or_autostart`] with the lock path and the autostart step injected.
///
/// Why: the regression tests must drive the decision against stub servers
/// and a temp lock, and must never start a real daemon or touch the real
/// `~/.trusty-mpm/daemon.lock`.
/// What: (1) try the picker; (2) on a listing failure, classify it with the
/// lock's live pid — `Unknown` stops with [`slow_daemon_error`]; (3) `Down`
/// runs `autostart`; (4) after a successful autostart the daemon answered
/// `/health`, so a second listing failure also stops; (5) an autostart error
/// of type [`DaemonAliveUnresponsive`] stops; any other autostart error is
/// [`PickerFlow::Offline`].
/// Test: `slow_listing_stops_without_autostart`,
/// `refused_with_live_lock_pid_stops_without_autostart`,
/// `refused_without_live_pid_autostarts_then_goes_offline`,
/// `slow_listing_after_autostart_stops_not_offline`,
/// `alive_unresponsive_autostart_stops_not_offline`.
pub(crate) async fn picker_or_autostart_with<F, Fut>(
    client: &reqwest::Client,
    url: &str,
    project: &PickerProject<'_>,
    lock_path: &Path,
    autostart: F,
) -> PickerFlow
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = anyhow::Result<String>>,
{
    let err = match super::guided::try_show_picker(client, url, project).await {
        PickerAttempt::Shown(r) => return PickerFlow::Done(r),
        PickerAttempt::ListFailed(e) => e,
    };
    // #9034: only positive evidence of absence may start a daemon.
    let live_pid = live_daemon_pid_for(url, lock_path);
    if let DaemonReach::Unknown(why) = classify_list_failure(&err, live_pid) {
        return PickerFlow::Done(Err(slow_daemon_error(url, &why)));
    }
    eprintln!("tm: daemon not running — starting it…");
    match autostart().await {
        Ok(new_url) => match super::guided::try_show_picker(client, &new_url, project).await {
            PickerAttempt::Shown(r) => PickerFlow::Done(r),
            // #9034: `/health` just answered, so this daemon is up and slow.
            PickerAttempt::ListFailed(e) => {
                PickerFlow::Done(Err(slow_daemon_error(&new_url, &format!("{e:#}"))))
            }
        },
        // #9034: a loaded service or a live lock pid is a slow daemon.
        Err(e) if e.downcast_ref::<DaemonAliveUnresponsive>().is_some() => {
            PickerFlow::Done(Err(slow_daemon_error(url, &format!("{e}"))))
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
