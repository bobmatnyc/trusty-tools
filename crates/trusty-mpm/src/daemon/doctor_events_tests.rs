//! Tests for the `events` doctor row (#10042).
//!
//! Why: the row is fail-closed — only a completed connect may read `Ok`. Each
//! error arm has its own test, observed against a real socket file in a
//! tempdir wherever the state can be built, so a regression that folds one of
//! them into healthy fails here.
//! Test: `cargo test -p trusty-mpm --lib doctor_events`.

use super::*;

/// A socket path inside a fresh tempdir.
fn sock_in(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("trusty-events.sock")
}

/// Why (fail-open check): a socket file left by a crashed daemon still exists;
/// a row that only looked for the file would call a dead bus healthy.
/// What: a socket whose listener is gone observes `Stale` and grades `Fail`,
/// naming the stale socket.
/// Test: This is the test.
#[tokio::test]
async fn doctor_events_reports_a_dead_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = sock_in(&dir);
    drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
    assert!(path.exists(), "the stale socket file must remain");

    let state = observe_socket(&path, CONNECT_TIMEOUT).await;
    assert_eq!(state, SocketState::Stale);
    let row = grade(&path, &state);
    assert_eq!(row.name, CHECK_NAME);
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("stale"), "{}", row.message);
}

/// Why: no socket is the not-running case and must read as a failure.
/// What: a missing path observes `Absent` and grades `Fail` with the install
/// and start remedies.
/// Test: This is the test.
#[tokio::test]
async fn a_missing_socket_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = sock_in(&dir);
    let state = observe_socket(&path, CONNECT_TIMEOUT).await;
    assert_eq!(state, SocketState::Absent);
    let row = grade(&path, &state);
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("not running"), "{}", row.message);
    assert!(
        row.message.contains("tctl install trusty-events"),
        "{}",
        row.message
    );
}

/// Why: the one healthy verdict, and the one place the #10071 limit is stated.
/// What: a bound socket observes `Serving` and grades `Ok` naming #10071.
/// Test: This is the test.
#[tokio::test]
async fn a_bound_socket_is_serving() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = sock_in(&dir);
    let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let state = observe_socket(&path, CONNECT_TIMEOUT).await;
    assert_eq!(state, SocketState::Serving);
    let row = grade(&path, &state);
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
}

/// Why (#10071): "the socket accepts" is weaker than "the bus is moving
/// events"; an `Ok` that did not say so would overstate what was observed.
/// What: the `Ok` detail names #10071.
/// Test: This is the test.
#[test]
fn serving_is_ok_and_names_the_liveness_limit() {
    let row = grade(Path::new("/x/trusty-events.sock"), &SocketState::Serving);
    assert_eq!(row.status, CheckStatus::Ok);
    assert!(row.message.contains("#10071"), "{}", row.message);
}

/// Why: a regular file at the socket path blocks the bind; it is not a bus.
/// What: a regular file observes `NotASocket` and grades `Fail`, without ever
/// dialling it.
/// Test: This is the test.
#[tokio::test]
async fn a_regular_file_is_not_a_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = sock_in(&dir);
    std::fs::write(&path, b"not a socket").expect("write");
    let state = observe_socket(&path, CONNECT_TIMEOUT).await;
    assert_eq!(state, SocketState::NotASocket);
    assert_eq!(grade(&path, &state).status, CheckStatus::Fail);
}

/// Why (fail-closed): a path the doctor cannot inspect proves nothing about the
/// bus; it must never read as healthy.
/// What: a socket under a directory with no search permission observes
/// `Unreadable` and grades `Fail`. Root ignores directory modes, so under root
/// the observation is skipped and only the grade is asserted.
/// Test: This is the test.
#[tokio::test]
async fn an_unreadable_parent_is_unreadable() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).expect("mkdir");
    let path = locked.join("trusty-events.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let state = observe_socket(&path, CONNECT_TIMEOUT).await;
    // Restore before asserting so the tempdir can be removed either way.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    if state == SocketState::Serving {
        // Running as root: directory modes do not apply, nothing to observe.
        return;
    }
    assert!(
        matches!(state, SocketState::Unreadable(_)),
        "expected Unreadable, got {state:?}"
    );
    assert_eq!(grade(&path, &state).status, CheckStatus::Fail);
}

/// Why (fail-closed): every observation other than `Serving` is a fault or an
/// unknown, never health — a refactor that defaulted an arm to `Ok` would ship
/// a green row for a dead bus.
/// What: each non-serving state grades `Fail`, except `Undetermined`, which is
/// `Warn`; none is `Ok`.
/// Test: This is the test.
#[test]
fn every_non_serving_state_is_never_ok() {
    let p = Path::new("/x/trusty-events.sock");
    for (state, want) in [
        (SocketState::Absent, CheckStatus::Fail),
        (SocketState::Stale, CheckStatus::Fail),
        (SocketState::NotASocket, CheckStatus::Fail),
        (
            SocketState::Unreadable("Permission denied".to_owned()),
            CheckStatus::Fail,
        ),
        (SocketState::Undetermined, CheckStatus::Warn),
    ] {
        let row = grade(p, &state);
        assert_eq!(row.status, want, "{state:?}: {}", row.message);
        assert_ne!(row.status, CheckStatus::Ok, "{state:?}");
    }
}

/// Why: a data directory that cannot be resolved leaves no producer able to
/// reach the bus either.
/// What: a resolution error grades `Fail` and carries the reason.
/// Test: This is the test.
#[tokio::test]
async fn an_unresolvable_socket_path_fails() {
    let row = grade_resolved(Err(anyhow::anyhow!("no data dir"))).await;
    assert_eq!(row.name, CHECK_NAME);
    assert_eq!(row.status, CheckStatus::Fail);
    assert!(row.message.contains("no data dir"), "{}", row.message);
}
