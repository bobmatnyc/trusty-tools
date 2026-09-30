//! Unit tests for the `tm doctor` daemon-reachability row (#6336).

use super::*;

/// The transport label a socket-only client reports.
const SOCKET: &str = "unix socket /tmp/t/trusty-mpm.sock";

/// A reachable daemon is the only outcome that reads healthy.
#[test]
fn daemon_row_is_ok_when_reachable() {
    let check = daemon_check(DaemonReachability::Reachable, SOCKET);
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
    let check = daemon_check(DaemonReachability::NotRunning, SOCKET);
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
    let check = daemon_check(DaemonReachability::Unresponsive, SOCKET);
    assert_eq!(check.status, CheckStatus::Unknown);
    assert!(
        check.message.contains("trusty-mpm daemon: unresponsive"),
        "got: {}",
        check.message
    );
}

/// #6288 step 1: the row names the transport the probe used, and still never
/// a port — `7880` is the literal the issue reported.
#[test]
fn daemon_row_names_its_transport_and_no_port() {
    for reachability in [
        DaemonReachability::Reachable,
        DaemonReachability::NotRunning,
        DaemonReachability::Denied(Some(1)),
        DaemonReachability::Unresponsive,
    ] {
        let message = daemon_check(reachability, SOCKET).message;
        assert!(message.contains(SOCKET), "{reachability:?}: {message}");
        for forbidden in ["7880", "port", "tcp"] {
            assert!(
                !message.to_lowercase().contains(forbidden),
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
    let (reachability, snapshot) = probe_daemon(&DaemonClient::new("http://127.0.0.1:1")).await;
    assert_eq!(reachability, DaemonReachability::NotRunning);
    assert!(snapshot.is_none());
}

/// #6288 step 1: over the socket, an absent socket is `NotRunning` — the row
/// warns, and it is never read as reachable.
#[tokio::test]
async fn daemon_probe_over_an_absent_socket_is_not_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = DaemonClient::over_socket(dir.path().join("absent.sock"));
    let (reachability, snapshot) = probe_daemon(&client).await;
    assert_eq!(reachability, DaemonReachability::NotRunning);
    assert!(snapshot.is_none());
}

/// #6288 critic LOW: a dial the OS refuses (EPERM under a sandbox) names the
/// errno and gives no `tm start` advice, since starting a daemon cannot fix it.
/// The error is built as the socket transport builds it: the kernel refuses
/// `connect(2)` itself, which no filesystem fixture reproduces, because the
/// hardened dial refuses a wrong-mode socket before it connects.
#[test]
fn daemon_probe_names_the_errno_when_the_os_denies_the_dial() {
    use trusty_common::uds::{UdsRpcError, UdsSecurityError};

    let path = std::path::PathBuf::from("/tmp/t/trusty-mpm.sock");
    let dial = UdsRpcError::Dial {
        path: path.clone(),
        source: UdsSecurityError::Connect {
            path,
            source: std::io::Error::from_raw_os_error(1),
        },
    };
    let err = trusty_mpm::client::DaemonCallError::Unreachable {
        target: SOCKET.to_string(),
        source: dial.into(),
    };
    let (reachability, snapshot) = classify_probe(Err(err));
    assert_eq!(reachability, DaemonReachability::Denied(Some(1)));
    assert!(snapshot.is_none());
    let row = daemon_check(reachability, SOCKET);
    assert!(row.message.contains("EPERM"), "{}", row.message);
    assert!(!row.message.contains("tm start"), "{}", row.message);
}
