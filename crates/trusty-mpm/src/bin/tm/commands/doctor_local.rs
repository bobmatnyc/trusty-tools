//! `tm doctor` — the standalone, one-shot diagnostic (#6336).
//!
//! Why: doctor used to be a daemon round-trip. `CommandExecutor` called
//! `GET /api/v1/doctor`, so when the daemon was not running the command printed
//! `doctor failed: daemon unreachable: …` and ran none of the 27 purely local
//! checks — the exact moment an operator most needs a diagnostic is the moment
//! it refused to produce one. Doctor never needed the daemon for those checks:
//! they read the filesystem, `gh`, launchd, and this process's own binary.
//! What: builds a READ-ONLY [`SessionManager`] over the on-disk session store,
//! runs [`run_doctor_for_manager`] in-process — the same battery
//! `GET /api/v1/doctor` serves — then appends the daemon-reachability row
//! (`doctor_daemon_row`) and, only when a daemon actually answered, the two
//! client-side checks that reason about that answer: `doctor_stale` (#2332) and
//! `doctor_orphan` (#4230). It never starts, restarts, or requires a daemon.
//! This lives beside `misc` rather than inside it because `misc.rs` had 24 SLOC
//! of headroom under the 500-SLOC production cap.
//! Test: `tests/tm_doctor_standalone.rs` drives the real binary against an
//! address nothing listens on.

use std::path::PathBuf;
use std::sync::Arc;

use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::daemon::doctor::run_doctor_for_manager;
use trusty_mpm::session_manager::real_tmux::{NoopTmuxDriver, RealTmuxDriver};
use trusty_mpm::session_manager::{ManagedTmuxDriver, SessionManager};

use super::doctor_daemon_row::{self, DaemonReachability};

/// `doctor` subcommand — run and print the full system diagnostic.
///
/// Why: a misconfigured trusty-mpm stack fails confusingly; `tm doctor` runs
/// every health probe in one command and prints a formatted verdict so the
/// operator can confirm — or fix — a broken install at a glance. It is a
/// one-shot CLI diagnostic: it never runs inside the daemon, and a missing
/// daemon costs it exactly one row rather than the whole report (#6336).
/// What: runs the check battery in-process via [`local_report`], prints one
/// status-tagged line per check, appends the daemon rows from
/// [`daemon_rows`], and folds every status into one overall verdict. When
/// `prune_stale_skills` is set (hidden `--prune-stale-skills` flag), also runs
/// `prune_stale_skills_locally` as a manual troubleshooting escape hatch —
/// normal operation cleans up pre-rename `mpm-*` skill directories
/// automatically and silently via the one-time
/// `core::stale_skills::run_stale_mpm_skills_migration_once` migration at `tm`
/// startup (#1905), so this flag should rarely be needed.
/// Test: `cli_parses_doctor` / `cli_parses_doctor_prune_stale_skills` cover
/// parsing; `tm_doctor_reports_every_local_check_with_no_daemon` covers the
/// daemonless path end to end; the stale-daemon comparison logic is covered by
/// `core::version_staleness`'s own unit tests.
pub(crate) async fn doctor(url: &str, flags: &crate::cli::DoctorFlags) -> anyhow::Result<()> {
    let report = local_report().await?;

    println!("trusty-mpm doctor");
    let mut overall = report.overall;
    for check in &report.checks {
        print_check(check);
    }
    for check in daemon_rows(url).await {
        overall = overall.worst(check.status);
        print_check(&check);
    }

    println!(
        "\noverall: {} {}",
        status_icon(overall),
        match overall {
            CheckStatus::Ok => "all checks passed",
            CheckStatus::Warn => "passed with warnings",
            // Issue #4005: deliberately not phrased as a pass. A check that
            // could not be determined has not passed.
            CheckStatus::Unknown =>
                "one or more checks could not be determined — health is UNKNOWN",
            CheckStatus::Fail => "one or more checks failed",
        },
    );

    // #4948: every opt-in local action the report can be followed by lives in
    // `doctor_repair`, so this file stays the report and the actions stay
    // together.
    super::doctor_repair::run_post_report_actions(flags);

    Ok(())
}

/// Run the daemon's own check battery in this process.
///
/// Why (#6336): the battery reads the filesystem, `gh`, launchd, and this
/// binary — none of it needs a server. Routing it through
/// [`run_doctor_for_manager`] rather than re-deriving the fleet inputs here is
/// what guarantees `tm doctor` and `GET /api/v1/doctor` report the same checks.
/// What: opens the managed-session store read-only over a real tmux driver
/// (falling back to a no-op driver when tmux is absent, as `tm supervisor`
/// does) and delegates. `SessionStore::load` returns an empty store when the
/// file is absent, so a machine that has never run a managed session needs no
/// daemon, no store, and no tmux.
/// Test: `tm_doctor_reports_every_local_check_with_no_daemon`.
async fn local_report() -> anyhow::Result<DoctorReport> {
    let data_dir = FrameworkPaths::default().root.join("session-manager");
    let tmux: Arc<dyn ManagedTmuxDriver> = match RealTmuxDriver::discover() {
        Ok(driver) => Arc::new(driver),
        // Not an error for a diagnostic: without tmux the worktree probe simply
        // observes no live panes, which its own classification already reports
        // as UNKNOWN rather than as a reclaimable orphan.
        Err(_) => Arc::new(NoopTmuxDriver),
    };
    let mgr = SessionManager::new(&data_dir, tmux).await?;
    let project_dir: Option<PathBuf> = std::env::current_dir().ok();
    Ok(run_doctor_for_manager(&mgr, project_dir.as_deref()).await)
}

/// The rows that depend on a daemon answering, in the order they print.
///
/// Why: exactly one row reports whether the daemon is there (#6336). The other
/// two — #2332 staleness and #4230 orphan detection — are comparisons ABOUT a
/// daemon's answer, so with no answer there is nothing to compare and they are
/// SKIPPED rather than reported as a warning or an unknown. Reporting them
/// anyway is what used to drag a perfectly healthy daemonless run to an overall
/// verdict of UNKNOWN, restating the daemon row twice in worse words.
/// What: one bounded `/health` probe, shared by all three rows so they can
/// never straddle a restart and describe two different daemons (#4230 review).
/// Test: `tm_doctor_reports_every_local_check_with_no_daemon` covers the
/// skip; the two comparisons keep their own unit tests.
async fn daemon_rows(url: &str) -> Vec<DoctorCheck> {
    let (reachability, snapshot) = doctor_daemon_row::probe_daemon(url).await;
    let mut rows = vec![doctor_daemon_row::daemon_check(reachability)];
    if let Some(snapshot) = snapshot.as_ref() {
        debug_assert_eq!(reachability, DaemonReachability::Reachable);
        // #4230: the restart hint is resolved from this host's launchd state,
        // because `tm restart` is a hard error where launchd owns the daemon.
        let restart_hint = super::launchd_probe::daemon_restart_command();
        rows.push(super::doctor_stale::stale_daemon_check(
            snapshot,
            &restart_hint,
        ));
        rows.push(super::doctor_orphan::orphan_daemon_check(Some(snapshot)));
    }
    rows
}

/// Print one check as an icon, a padded name, and its message.
fn print_check(check: &DoctorCheck) {
    println!(
        "  {} {:<21} {}",
        status_icon(check.status),
        check.name,
        check.message,
    );
}

/// Map a check status to its one-glance icon.
fn status_icon(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Ok => "\u{2705}",
        CheckStatus::Warn => "\u{26a0}\u{fe0f}",
        // Never ✅ (issue #4005): an indeterminate check must not read as
        // healthy at a glance.
        CheckStatus::Unknown => "\u{2754}",
        CheckStatus::Fail => "\u{274c}",
    }
}
