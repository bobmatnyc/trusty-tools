//! Unit tests for the `tm doctor` daemon-reachability row (#6336).

use super::*;

/// A reachable daemon is the only outcome that reads healthy.
#[test]
fn daemon_row_is_ok_when_reachable() {
    let check = daemon_check(DaemonReachability::Reachable);
    assert_eq!(check.name, CHECK_NAME);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("trusty-mpm daemon: reachable"),
        "got: {}",
        check.message
    );
}

/// An absent daemon degrades the report; it never aborts it, and the row says
/// so explicitly so an operator reading only this line knows the rest ran.
#[test]
fn daemon_row_warns_when_not_running() {
    let check = daemon_check(DaemonReachability::NotRunning);
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        check.message.contains("trusty-mpm daemon: not running"),
        "got: {}",
        check.message
    );
    assert!(
        check.message.contains("every local check above still ran"),
        "got: {}",
        check.message
    );
}

/// A socket that accepts and then says nothing has told us nothing — `Unknown`,
/// never `Ok` and never `Warn` (#4005 precedent).
#[test]
fn daemon_row_is_unknown_when_unresponsive() {
    let check = daemon_check(DaemonReachability::Unresponsive);
    assert_eq!(check.status, CheckStatus::Unknown);
    assert!(
        check.message.contains("trusty-mpm daemon: unresponsive"),
        "got: {}",
        check.message
    );
}

/// The row must survive the #6288 move to a Unix socket without rewording, so
/// no outcome may name a port or a transport. `7880` is the literal the issue
/// reported; `port` and `http` are the general form of the same mistake.
#[test]
fn daemon_row_never_names_a_port() {
    for reachability in [
        DaemonReachability::Reachable,
        DaemonReachability::NotRunning,
        DaemonReachability::Unresponsive,
    ] {
        let message = daemon_check(reachability).message.to_lowercase();
        for forbidden in ["7880", "port", "http", "tcp", "socket"] {
            assert!(
                !message.contains(forbidden),
                "{reachability:?} row names {forbidden:?}: {message}"
            );
        }
    }
}

/// The probe distinguishes "nothing is listening" from every other failure.
///
/// Port 1 on loopback is the same never-listening address the `tm hook`
/// fail-open suite uses, so the connect is refused rather than timing out.
#[tokio::test]
async fn daemon_probe_reports_not_running_when_nothing_listens() {
    let (reachability, snapshot) = probe_daemon("http://127.0.0.1:1").await;
    assert_eq!(reachability, DaemonReachability::NotRunning);
    assert!(snapshot.is_none());
}
