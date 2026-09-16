//! Unit tests for `tm status`'s daemon line (#8025).
//!
//! Why: the defect was a DISAGREEMENT between two commands, so the tests that
//! matter drive the real probe against a real loopback server and compare what
//! both commands would say about the same sample. A test over `daemon_line`
//! alone could pass while the two probes still diverged.
//! What: renderer cases for the three reachability outcomes, plus a live
//! `/health` server that answers with one pid while a stale lock file on disk
//! names a different, dead one.

use std::net::SocketAddr;

use axum::Router;
use axum::routing::get;
use trusty_mpm::client::HealthSnapshot;

use super::*;

/// A pid no process on this host owns, standing in for the daemon that died.
///
/// Why: `libc::kill(pid, 0)` must report it dead so `read_lock_at` treats the
/// record as stale. The max pid on Darwin is 99998, so this is unallocatable.
const DEAD_PID: u32 = 4_294_967_000;

/// The pid the live `/health` server reports — the one both commands must name.
const LIVE_PID: u32 = 424_242;

/// Build a snapshot the way a live daemon's `/health` deserialises.
fn snapshot(pid: Option<u32>, version: &str) -> HealthSnapshot {
    HealthSnapshot {
        status: "ok".to_string(),
        catalog_stale: false,
        catalog_unknown: false,
        version: version.to_string(),
        build_id: String::new(),
        supervised: Some(true),
        pid,
        unsupervised_forced: false,
        launchd_supervision: String::new(),
        degraded: Vec::new(),
    }
}

/// Why (#8025): the pid on the status line must be the one that ANSWERED, which
/// is the only pid the probe carries.
#[test]
fn daemon_line_names_the_probed_pid() {
    let line = daemon_line(
        DaemonReachability::Reachable,
        Some(&snapshot(Some(LIVE_PID), "1.5.37")),
    );
    assert!(line.contains("reachable"), "{line}");
    assert!(line.contains("pid 424242"), "{line}");
    assert!(line.contains("version 1.5.37"), "{line}");
}

/// Why: `daemon: unreachable` is what `tm status` has always printed for a down
/// daemon, and scripts read it. The fix reclassifies WHICH failures reach this
/// branch; it does not reword the branch.
#[test]
fn daemon_line_keeps_the_historical_unreachable_wording() {
    assert_eq!(
        daemon_line(DaemonReachability::NotRunning, None),
        "daemon: unreachable"
    );
}

/// Why: a socket that accepts and then says nothing is a different operator
/// situation with a different remedy — the distinction `tm doctor` already
/// draws, now drawn identically here.
#[test]
fn daemon_line_separates_unresponsive_from_unreachable() {
    let line = daemon_line(DaemonReachability::Unresponsive, None);
    assert!(line.contains("unresponsive"), "{line}");
    assert!(!line.contains("unreachable"), "{line}");
}

/// Why: a daemon predating #4230 omits `pid`, and printing a fabricated `0`
/// would be worse than admitting the gap — `0` is a pid an operator might act on.
#[test]
fn daemon_line_reports_an_old_daemon_pid_as_unknown() {
    let line = daemon_line(DaemonReachability::Reachable, Some(&snapshot(None, "")));
    assert!(line.contains("pid unknown"), "{line}");
    assert!(line.contains("version unknown"), "{line}");
}

/// Serve `/health` with [`LIVE_PID`] and a `/sessions` that always 500s.
///
/// Why: this is the live shape of the #8025 incident — the daemon answers its
/// health probe while the fleet listing does not. Returning the bound address
/// lets the test dial the ephemeral port it actually got.
async fn spawn_health_only_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route(
            "/health",
            get(|| async {
                axum::Json(serde_json::json!({
                    "status": "ok",
                    "catalog_stale": false,
                    "catalog_unknown": false,
                    "version": "1.5.37",
                    "pid": LIVE_PID,
                }))
            }),
        )
        .route(
            "/sessions",
            get(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, handle)
}

/// The whole #8025 regression, in one case.
///
/// Why: before the fix `tm status` called this daemon UNREACHABLE — its probe
/// required `/sessions` to answer 200 as well — while `tm doctor`, probing
/// `/health` alone, called the same daemon reachable and named its pid. A stale
/// `daemon.lock` naming a dead pid sits beside it, because after a launchd
/// restart that is exactly what is on disk; neither command may read the pid
/// from there.
/// What: asserts the shared probe reports `Reachable` with the SERVING pid, that
/// the status line and the doctor row agree, and that the stale lock record is
/// rejected rather than consulted.
#[tokio::test]
async fn status_reports_the_live_daemon_when_the_listing_fails() {
    let (addr, server) = spawn_health_only_server().await;
    let url = format!("http://{addr}");

    let dir = tempfile::tempdir().expect("tempdir");
    let stale = dir.path().join("daemon.lock");
    std::fs::write(
        &stale,
        trusty_mpm::core::daemon_identity::render_lock(
            &trusty_mpm::core::daemon_identity::DaemonLock {
                product: trusty_mpm::core::daemon_identity::LOCK_PRODUCT.to_string(),
                pid: DEAD_PID,
                addr: url.clone(),
                started_at: String::new(),
                socket_path: String::new(),
            },
        ),
    )
    .expect("write stale lock");
    assert!(
        trusty_mpm::core::daemon_identity::read_lock_at(&stale).is_none(),
        "a lock file naming a dead pid must never be a pid source"
    );

    // ONE probe, the same one `tm doctor` issues.
    let (reachability, snapshot) = crate::commands::doctor_daemon_row::probe_daemon(&url).await;
    assert_eq!(reachability, DaemonReachability::Reachable);
    let snapshot = snapshot.expect("a reachable daemon returns its snapshot");
    assert_eq!(snapshot.pid, Some(LIVE_PID));

    // `tm status` and `tm doctor` render the same sample.
    let status = daemon_line(reachability, Some(&snapshot));
    assert!(status.contains("pid 424242"), "{status}");
    assert!(!status.contains("unreachable"), "{status}");
    let doctor = crate::commands::doctor_daemon_row::daemon_check(reachability);
    assert_eq!(
        doctor.status,
        trusty_mpm::core::doctor::CheckStatus::Ok,
        "doctor row: {}",
        doctor.message
    );

    // The DISAGREEMENT itself, pinned against live code. `daemon_healthy` is the
    // gate `tm status` used before #8025 and `tm start` still uses: it requires
    // /health AND /sessions, so it calls this same daemon down in the same
    // breath the probe above calls it up. If this ever stops being false, the
    // sample no longer reproduces #8025 and the case below proves nothing.
    let client = trusty_mpm::client::http_client::default_client();
    assert!(
        !crate::commands::daemon::daemon_healthy(&client, &url).await,
        "the pre-#8025 gate must still read this daemon as unreachable — that is \
         the disagreement `tm status` inherited"
    );

    // The failing listing is reported as a failing LISTING, never as a down daemon.
    let listing = crate::commands::daemon::print_sessions(&client, &url).await;
    let err = listing.expect_err("a 500 listing must surface as an error");
    let line = listing_unavailable_line(&err);
    assert!(line.contains("listing unavailable"), "{line}");
    assert!(
        line.contains("the daemon itself answered /health"),
        "{line}"
    );

    server.abort();
}
