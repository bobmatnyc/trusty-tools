//! Standby for a launchd-started copy that lost the daemon lock to a healthy peer.
//!
//! Why (#10358): at boot a client's `ensure_daemon_up` spawns a detached
//! `start --foreground` while launchd starts the same job (`RunAtLoad`). One
//! wins the lock. The loser used to exit 1, and `KeepAlive` respawned it every
//! 30 s: 751 runs were observed against a daemon that was serving all along.
//! `KeepAlive` must stay `Always` (#4113, #4709: with `SuccessfulExit=false` a
//! clean exit left the daemon down), so exiting 0 is no fix. The loser stays
//! alive instead and takes over if the holder goes away.
//! What: [`await_lock_or_refuse`] runs BEFORE any heavy init. With a live lock
//! holder it asks [`holder_action`]; on `Standby` it retries every
//! [`DEFAULT_RETRY`] until the lock is free or SIGTERM arrives. A holder that
//! predates the installed binary is waited out only for [`STALE_HOLDER_GRACE`]
//! (see [`stale_wait_over`]), then refused with the replace-it advice.
//! Test: `standby_tests.rs` (the decision, the interval parse) and
//! `tests/launchd_standby_10358.rs` (the loop against real daemons).

use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use super::daemon::{probe_lock_holder, refuse_if_already_running, LockHolder};
use crate::service::daemon_client::DaemonClient;

/// Gap between lock retries while standing by.
pub(super) const DEFAULT_RETRY: Duration = Duration::from_secs(5);

/// How long a holder that stopped answering is waited out once we stand by.
///
/// Why: a holder asked to stop (launchd restart, SIGTERM) closes its socket
/// first and exits after its snapshot flush, so it stops answering while still
/// holding the lock. Refusing then would restart the respawn loop at the moment
/// of takeover. A holder silent past this bound is treated as wedged.
pub(super) const UNHEALTHY_GRACE: Duration = Duration::from_secs(60);

/// How long a standby waits behind a holder that predates the installed binary.
///
/// Why: a stale holder never leaves on its own, so an unbounded standby would
/// leave an install silently not upgrading. Exiting 1 does not loop hot: launchd
/// respawns the job, which stands by for a full grace again, so a stale holder
/// costs one short-lived run per grace window (not one per 30 s, #10358) and one
/// logged refusal naming how to replace it. A holder that is merely stopping
/// (upgrade in progress) leaves well inside the window and is taken over.
pub(super) const STALE_HOLDER_GRACE: Duration = Duration::from_secs(600);

/// Test seam: stale-holder grace in milliseconds.
pub(super) const STALE_GRACE_MS_ENV: &str = "TRUSTY_SEARCH_STANDBY_STALE_GRACE_MS";

/// Test seam: retry gap in milliseconds, so a test need not wait out 5 s ticks.
pub(super) const RETRY_MS_ENV: &str = "TRUSTY_SEARCH_STANDBY_RETRY_MS";

/// Name of the test-only supervision seam; read by [`assume_supervised`].
pub(super) const ASSUME_SUPERVISED_ENV: &str = "TRUSTY_SEARCH_TEST_ASSUME_LAUNCHD_SUPERVISED";

/// Whether the test-only seam value declares this process supervised.
///
/// Why: launchd cannot be faked for a spawned test child, and `XPC_SERVICE_NAME`
/// is deliberately not read (#4469: an unsupervised child inherits it). Tests
/// need some way to reach the standby path. The name says TEST because a spawned
/// binary cannot tell it runs under a test; production never sets it.
/// What: `Some("1")` is true; unset or any other value is false. Pure, so the
/// decision is testable without touching the process environment.
/// Test: `the_test_seam_is_on_only_for_one`.
pub(super) fn assume_supervised(raw: Option<&str>) -> bool {
    raw == Some("1")
}

/// The startup warning for the test-only seam, if it is set.
///
/// Why: a stray seam in a real environment would make a manual `start
/// --foreground` stand by instead of refusing; the log must show it.
/// What: `Some(text)` whenever the variable is present, whatever its value.
/// Test: `the_test_seam_warns_whenever_it_is_set`.
pub(super) fn seam_warning(raw: Option<&str>) -> Option<String> {
    raw.map(|v| {
        format!(
            "{ASSUME_SUPERVISED_ENV}={v} is set: a test-only seam that forces launchd \
             standby behaviour. Unset it outside tests."
        )
    })
}

/// What to do about a live lock holder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HolderAction {
    /// Stay alive and retry the lock.
    Standby,
    /// Refuse, as every start did before #10358.
    Refuse,
}

/// How [`await_lock_or_refuse`] ended without an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LockWait {
    /// No live holder: continue normal startup.
    Free,
    /// SIGTERM or SIGINT arrived while standing by: exit cleanly.
    Terminated,
}

/// Decide between standing by and refusing.
///
/// Why: standing by is right only when launchd will respawn an exit anyway AND
/// the holder is really serving. An unsupervised start has no respawn loop to
/// protect and wants its error; an unhealthy holder may be wedged, and a
/// standby behind it would hide that.
/// What: `Standby` only when both inputs are true.
/// Test: `standby_only_when_supervised_and_the_holder_is_healthy`.
pub(super) fn holder_action(supervised: bool, holder_healthy: bool) -> HolderAction {
    if supervised && holder_healthy {
        HolderAction::Standby
    } else {
        HolderAction::Refuse
    }
}

/// Whether the standby behind this holder has waited long enough.
///
/// Why: see [`STALE_HOLDER_GRACE`]; only an older-image holder is bounded.
/// What: true only for `StaleImage` once `waited >= grace`.
/// Test: `a_stale_holder_is_waited_out_only_for_the_grace`.
pub(super) fn stale_wait_over(holder: LockHolder, waited: Duration, grace: Duration) -> bool {
    holder == LockHolder::StaleImage && waited >= grace
}

/// Parse [`STALE_GRACE_MS_ENV`]; anything unusable gives [`STALE_HOLDER_GRACE`].
///
/// Why: a typo must not make the standby refuse instantly, so under 500 ms is ignored.
/// What: `Some("1500")` is 1.5 s; `None`, junk and small values give the default.
/// Test: `a_stale_holder_is_waited_out_only_for_the_grace`.
pub(super) fn stale_grace(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms >= 500)
        .map_or(STALE_HOLDER_GRACE, Duration::from_millis)
}

/// Parse [`RETRY_MS_ENV`]; anything unusable gives [`DEFAULT_RETRY`].
///
/// Why: a typo must not turn the retry into a busy loop, so a value under
/// 50 ms is ignored too.
/// What: `Some("200")` is 200 ms; `None`, `"abc"`, `"0"` give the default.
/// Test: `retry_interval_parses_the_override_and_ignores_junk`.
pub(super) fn retry_interval(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms >= 50)
        .map_or(DEFAULT_RETRY, Duration::from_millis)
}

/// Ask launchd (once) whether it runs this process.
async fn launchd_supervised() -> bool {
    if assume_supervised(std::env::var(ASSUME_SUPERVISED_ENV).ok().as_deref()) {
        return true;
    }
    // `launchctl list` blocks for up to its own timeout; keep it off the reactor.
    tokio::task::spawn_blocking(trusty_common::supervision::launchd_supervision)
        .await
        .map(|s| s.is_supervised())
        .unwrap_or(false)
}

/// Gate the foreground start on the daemon lock.
///
/// Why: see the module doc. This replaces the bare `refuse_if_already_running`
/// the foreground path called before heavy init.
/// What: no live holder returns `Free`. A live holder is handled per
/// [`holder_action`]: `Refuse` returns the existing refusal error. `Standby`
/// logs the holder pid at info, then every retry gap re-reads the holder: gone
/// returns `Free`, still live loops (an unhealthy holder now refuses). The
/// health probe is `search.health` on the socket, the check
/// `DaemonClient::is_up` makes; `socket` is `start --socket`, else the
/// resolved one. SIGTERM or SIGINT returns `Terminated`.
/// Test: `tests/launchd_standby_10358.rs`.
pub(super) async fn await_lock_or_refuse(socket: Option<&Path>) -> Result<LockWait> {
    if let Some(warning) = seam_warning(std::env::var(ASSUME_SUPERVISED_ENV).ok().as_deref()) {
        tracing::warn!("{warning}");
    }
    let Some(mut pid) = crate::service::running_daemon_pid() else {
        return Ok(LockWait::Free);
    };
    let supervised = launchd_supervised().await;
    let client = match socket {
        Some(path) => Some(DaemonClient::at(path)),
        None => DaemonClient::resolve()
            .inspect_err(|e| tracing::warn!("standby: cannot resolve the daemon socket: {e:#}"))
            .ok(),
    };
    let retry = retry_interval(std::env::var(RETRY_MS_ENV).ok().as_deref());
    // #10358: tokio keeps the handlers this installs, so a stop that lands after
    // takeover must be latched for `run_daemon` rather than swallowed.
    crate::service::early_stop::arm();
    let mut stop = std::pin::pin!(trusty_common::shutdown_signal());
    let mut announced = false;
    let mut silent_since: Option<std::time::Instant> = None;
    let stale_grace = stale_grace(std::env::var(STALE_GRACE_MS_ENV).ok().as_deref());
    let mut stale_since: Option<std::time::Instant> = None;
    // Classified once per holder pid (the lock mtime only moves when a new holder
    // takes it), so the per-tick loop does no extra I/O for a same-image holder.
    let mut classified: Option<(u32, LockHolder)> = None;
    loop {
        let healthy = match (&client, supervised) {
            (Some(c), true) => c.is_up().await,
            _ => false,
        };
        // Before standing by, an unhealthy holder refuses at once. After, it is
        // waited out for `UNHEALTHY_GRACE` (it is likely shutting down).
        let waiting_out = announced
            && supervised
            && !healthy
            && silent_since
                .get_or_insert_with(std::time::Instant::now)
                .elapsed()
                < UNHEALTHY_GRACE;
        if healthy {
            silent_since = None;
        }
        if !waiting_out && holder_action(supervised, healthy) == HolderAction::Refuse {
            if supervised {
                tracing::info!(
                    "daemon lock holder (pid {pid}) does not answer search.health; refusing"
                );
            }
            return refuse_if_already_running(Some(pid)).map(|()| LockWait::Free);
        }
        // #10358: a holder older than the installed binary never leaves by
        // itself; stop standing by after `stale_grace` and say how to replace it.
        let holder_kind = match classified {
            Some((p, kind)) if p == pid => kind,
            _ => {
                let kind = probe_lock_holder();
                classified = Some((pid, kind));
                kind
            }
        };
        if healthy && holder_kind == LockHolder::StaleImage {
            let since = *stale_since.get_or_insert_with(|| {
                tracing::warn!(
                    "daemon lock holder (pid {pid}) predates the installed binary; standing by \
                     at most {stale_grace:?}, then exiting (#10358)"
                );
                std::time::Instant::now()
            });
            if stale_wait_over(LockHolder::StaleImage, since.elapsed(), stale_grace) {
                return refuse_if_already_running(Some(pid)).map(|()| LockWait::Free);
            }
        } else {
            stale_since = None;
        }
        if !announced {
            tracing::info!(
                "daemon lock held by pid {pid}, which answers search.health; standing by and \
                 retrying every {retry:?} (#10358)"
            );
            announced = true;
        }
        tokio::select! {
            () = &mut stop => {
                tracing::info!("standby: stop signal received; exiting");
                return Ok(LockWait::Terminated);
            }
            () = tokio::time::sleep(retry) => {}
        }
        match crate::service::running_daemon_pid() {
            None => {
                tracing::info!("daemon lock is free (pid {pid} is gone); taking over (#10358)");
                return Ok(LockWait::Free);
            }
            Some(next) => pid = next,
        }
    }
}
