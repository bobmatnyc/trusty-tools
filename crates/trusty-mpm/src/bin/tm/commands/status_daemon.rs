//! `tm status`'s daemon line, derived from the SAME probe `tm doctor` uses
//! (#8025).
//!
//! Why: after a launchd restart on 2026-09-15 the two commands disagreed in the
//! same minute — `tm status` printed `daemon: unreachable` while `tm doctor`
//! printed `trusty_mpm_daemon trusty-mpm daemon: reachable` and named pid
//! 65875. They disagreed because they asked different questions.
//! `commands::daemon::daemon_healthy` required `/health` AND `/sessions` to
//! both answer 200, so a daemon whose session listing was slow, wedged, or
//! erroring — the exact state the same session reported minutes earlier, when
//! `…/sessions/<id>/delegations/builder-slot` "did not complete" — read as
//! UNREACHABLE. `tm doctor` asked `/health` alone and read it as reachable.
//! Neither was lying; "the fleet listing failed" is simply not the same fact as
//! "the daemon is unreachable", and collapsing them cost an operator the one
//! signal that would have told them which.
//!
//! What: [`run`] issues ONE
//! [`super::doctor_daemon_row::probe_daemon`] — doctor's probe, not a second
//! copy of it — renders the verdict through [`daemon_line`], and only then
//! fetches the session listing. A listing that fails is reported as an
//! unavailable LISTING, on its own line, with the daemon line left as the probe
//! found it.
//!
//! The pid comes from that probe's `/health` snapshot and from nowhere else.
//! `~/.trusty-mpm/daemon.lock` records the pid of whichever daemon wrote it,
//! which after a restart is not necessarily the one answering — so it is not an
//! input here, and [`daemon_line`] cannot consult it.
//!
//! Test: `src/bin/tm/commands/status_daemon_tests.rs`.

use trusty_mpm::client::HealthSnapshot;

use super::doctor_daemon_row::{DaemonReachability, probe_daemon};

/// Render the one daemon line `tm status` prints, from the probe alone.
///
/// Why: a pure function over the probe's own two outputs is what makes "status
/// and doctor cannot disagree" assertable — there is no second source of truth
/// left for it to read. It is also why the pid argument is the snapshot rather
/// than a `u32` the caller resolved: a caller holding only a pid could have got
/// it from the lock file.
/// What: `daemon: reachable (pid N, version V)` when `/health` answered;
/// `daemon: unreachable` verbatim when nothing is listening, which is the
/// string `tm status` has always printed for that case; `daemon: unresponsive`
/// plus the remedy when a socket accepted but the probe went unanswered. A
/// daemon too old to report `pid`/`version` yields `pid unknown` /
/// `version unknown` rather than a fabricated zero.
/// Test: `daemon_line_names_the_probed_pid`,
/// `daemon_line_keeps_the_historical_unreachable_wording`,
/// `daemon_line_separates_unresponsive_from_unreachable`,
/// `daemon_line_reports_an_old_daemon_pid_as_unknown`.
pub(crate) fn daemon_line(
    reachability: DaemonReachability,
    snapshot: Option<&HealthSnapshot>,
) -> String {
    match reachability {
        DaemonReachability::Reachable => {
            // A pre-#4230 daemon omits `pid` entirely, which deserialises to
            // `None`. Saying "unknown" is the only honest reading — and it must
            // NOT be answered from the lock file, whose pid may name the daemon
            // that just died rather than the one that answered.
            let pid = match snapshot.and_then(|s| s.pid) {
                Some(pid) => pid.to_string(),
                None => "unknown".to_string(),
            };
            let version = snapshot
                .map(|s| s.version.as_str())
                .filter(|v| !v.is_empty())
                .unwrap_or("unknown");
            format!("daemon: reachable (pid {pid}, version {version})")
        }
        DaemonReachability::NotRunning => "daemon: unreachable".to_string(),
        DaemonReachability::Unresponsive => "daemon: unresponsive — it accepted the connection \
             but did not answer the health probe in time; restart it and re-run `tm doctor`"
            .to_string(),
    }
}

/// The line printed when the daemon answered but its session listing did not.
///
/// Why: this is the case the old `daemon_healthy` conflated away. Naming the
/// listing — not the daemon — is what keeps `tm status` and `tm doctor` from
/// contradicting each other about reachability while still telling the operator
/// that something is wrong.
/// Test: `status_reports_the_live_daemon_when_the_listing_fails`.
pub(crate) fn listing_unavailable_line(err: &anyhow::Error) -> String {
    format!("sessions: listing unavailable ({err:#}) — the daemon itself answered /health")
}

/// `status` subcommand — probe daemon health, then list sessions.
///
/// Why: the first thing an operator runs to see if the daemon is alive, and
/// (#8025) the answer must be the same one `tm doctor` gives.
/// What: one [`probe_daemon`], [`daemon_line`], and — only when the probe
/// reached the daemon — the session listing, whose own failure is reported
/// through [`listing_unavailable_line`] rather than as an exit code or a
/// contradictory daemon verdict.
/// Test: `src/bin/tm/commands/status_daemon_tests.rs` drives both halves
/// against a loopback server.
pub(crate) async fn run(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    let (reachability, snapshot) = probe_daemon(url).await;
    println!("{}", daemon_line(reachability, snapshot.as_ref()));
    if reachability != DaemonReachability::Reachable {
        return Ok(());
    }
    // #8058: a daemon that has parked a subsystem answers `/health` 200 all the
    // same, so the operator only learns of it if something prints it. This is
    // that reader.
    for reason in snapshot.iter().flat_map(|s| s.degraded.iter()) {
        println!("degraded: {reason}");
    }
    if let Err(e) = super::daemon::print_sessions(client, url).await {
        println!("{}", listing_unavailable_line(&e));
    }
    Ok(())
}

#[cfg(test)]
#[path = "status_daemon_tests.rs"]
mod status_daemon_tests;
