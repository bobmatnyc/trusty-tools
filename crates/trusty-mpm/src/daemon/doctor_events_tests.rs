//! Tests for the `events` doctor row (#10042).
//!
//! Why: the row is fail-closed — only a frame from the bus may read `Ok`. Each
//! error arm has its own test, observed against a real socket file in a
//! tempdir wherever the state can be built, so a regression that folds one of
//! them into healthy fails here.
//! Test: `cargo test -p trusty-mpm --lib doctor_events`.

use super::*;

/// A socket path inside a fresh tempdir.
fn sock_in(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("trusty-events.sock")
}

/// Wait until `path`'s listener is gone from the kernel, so a dead-socket test
/// observes a socket that is actually dead.
///
/// Why (#7215): a child that a concurrent test forks between `bind` and `drop`
/// inherits the listener fd and holds it until its `exec`, so the dropped
/// socket briefly still accepts. Once a connect is refused, no fd keeps the
/// listener alive and it cannot come back.
/// What: probes every 20 ms for up to 5 s until the connect is refused.
/// Test: `doctor_events_reports_a_dead_socket`.
async fn await_listener_gone(path: &Path) {
    for _ in 0..250 {
        if probe_socket_verdict(path, CONNECT_TIMEOUT).await == SocketVerdict::NotServing {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "{} still accepts 5 s after its listener dropped",
        path.display()
    );
}

/// Why (fail-open check): a socket file left by a crashed daemon still exists;
/// a row that only looked for the file would call a dead bus healthy.
/// What: a socket whose listener is gone observes `Stale` and grades `Fail`,
/// naming the stale socket.
/// Test: This is the test.
#[tokio::test]
async fn doctor_events_reports_a_dead_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    // See #7215: bind in a 0700 directory, as the bus does, so a late connect
    // reaches the socket rather than the directory-mode refusal.
    let path = hardened_sock_in(&dir);
    drop(trusty_common::uds::bind_hardened(&path).expect("bind"));
    assert!(path.exists(), "the stale socket file must remain");
    // See #7215: a forked child's inherited fd can keep the listener alive.
    await_listener_gone(&path).await;

    let state = observe_socket(&path, CONNECT_TIMEOUT).await;
    assert_eq!(state, SocketState::Stale);
    let row = grade(&path, &state);
    assert_eq!(row.name, CHECK_NAME);
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("stale"), "{}", row.message);
}

/// Why: no socket on a host with the bus installed is the not-running case
/// and must read as a failure.
/// What: a missing path observes `Absent` and grades `Fail` with the start
/// remedy. #10151: never `tctl install trusty-events`, which exits 3.
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
        row.message
            .contains("Its LaunchAgent is installed; start it with `tctl start trusty-events`."),
        "{}",
        row.message
    );
    assert!(!row.message.contains("tctl install"), "{}", row.message);
}

/// Why (#10042): the row's remedy must actually bring the bus back. `tctl
/// start` bootstraps an unloaded job and answers a loaded one with "already
/// loaded"; that is only a working remedy because the LaunchAgent restarts the
/// bus after ANY exit (`KeepAlive` `<true/>`, pinned on the installer side by
/// `events_plist_restarts_the_bus_after_any_exit`). With `SuccessfulExit:
/// false` a stray exit 0 left a loaded, stopped job that `tctl start` skipped.
/// What: the not-running remedy names `tctl start trusty-events` and, since
/// #10151, not `tctl install trusty-events` (it exits 3, #10130); the stale
/// remedy says launchd restarts it.
/// Test: This is the test.
#[test]
fn the_not_running_remedy_is_tctl_start() {
    let p = Path::new("/x/trusty-events.sock");
    let absent = grade(p, &SocketState::Absent);
    assert!(
        absent.message.contains("`tctl start trusty-events`"),
        "{}",
        absent.message
    );
    assert!(
        !absent.message.contains("tctl install"),
        "{}",
        absent.message
    );
    let stale = grade(p, &SocketState::Stale);
    assert!(
        stale.message.contains("launchd restarts"),
        "{}",
        stale.message
    );
}

/// Why (#10071 review): a wedged bus keeps a backlog slot for every connect,
/// so once its backlog fills the connect is refused and the row reads `Stale`.
/// `tctl start` answers "already loaded" for that hung job, so the stale
/// remedy must be one that boots a loaded job out and back in.
/// What: the stale text names the full-backlog hang and `tctl restart
/// trusty-events`, never `tctl start trusty-events`; the grade stays `Fail`.
/// Test: This is the test.
#[test]
fn the_stale_remedy_restarts_a_hung_bus() {
    let stale = grade(Path::new("/x/trusty-events.sock"), &SocketState::Stale);
    assert_eq!(stale.status, CheckStatus::Fail, "{}", stale.message);
    assert!(
        stale.message.contains("hung with a full backlog"),
        "{}",
        stale.message
    );
    assert!(
        stale.message.contains("`tctl restart trusty-events`"),
        "{}",
        stale.message
    );
    assert!(
        !stale.message.contains("`tctl start trusty-events`"),
        "{}",
        stale.message
    );
}

/// Why (#10071): the kernel completes a connect into the listen backlog for a
/// stopped (SIGSTOP) or wedged daemon, so "the socket accepts" was read as
/// healthy for a bus that serves nothing.
/// What: a hardened listener that never accepts grades `Fail`, naming `hung`.
/// Test: This is the test.
#[tokio::test]
async fn a_listener_that_never_accepts_is_hung() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = hardened_sock_in(&dir);
    let _listener = trusty_common::uds::bind_hardened(&path).expect("bind");
    let row = grade_resolved(Ok(path), dir.path()).await;
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("hung"), "{}", row.message);
}

/// Why (#10071): `Ok` now means the bus answered the subscription, so the
/// "liveness is not checked" caveat would understate what was observed.
/// What: the `Ok` detail says the bus answered and drops the old caveat.
/// Test: This is the test.
#[test]
fn serving_is_ok_and_says_the_bus_answered() {
    let row = grade(Path::new("/x/trusty-events.sock"), &SocketState::Serving);
    assert_eq!(row.status, CheckStatus::Ok);
    assert!(
        !row.message
            .contains("Liveness beyond the accept is not checked"),
        "{}",
        row.message
    );
    assert!(row.message.contains("answered"), "{}", row.message);
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
/// What: each non-serving state grades `Fail`, except `Undetermined`,
/// `VersionSkew` and `NotInstalled`, which are `Warn`; none is `Ok`.
/// Test: This is the test.
#[test]
fn every_non_serving_state_is_never_ok() {
    let p = Path::new("/x/trusty-events.sock");
    for (state, want) in [
        (SocketState::Absent, CheckStatus::Fail),
        // #10151: a host without the bus is supported.
        (SocketState::NotInstalled, CheckStatus::Warn),
        (SocketState::Stale, CheckStatus::Fail),
        (SocketState::NotASocket, CheckStatus::Fail),
        (
            SocketState::Unreadable("Permission denied".to_owned()),
            CheckStatus::Fail,
        ),
        (SocketState::Undetermined, CheckStatus::Warn),
        // #10071: an accept with no frame, an old bus, a subscription that
        // ended without a frame.
        (SocketState::Hung, CheckStatus::Fail),
        (
            SocketState::VersionSkew("unknown field `heartbeat_ms`".to_owned()),
            CheckStatus::Warn,
        ),
        (
            SocketState::Unanswered("closed the connection".to_owned()),
            CheckStatus::Fail,
        ),
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
    let dir = tempfile::tempdir().expect("tempdir");
    let row = grade_resolved(Err(anyhow::anyhow!("no data dir")), dir.path()).await;
    assert_eq!(row.name, CHECK_NAME);
    assert_eq!(row.status, CheckStatus::Fail);
    assert!(row.message.contains("no data dir"), "{}", row.message);
}

/// A LaunchAgents directory in `dir`, holding the trusty-events plist when
/// `installed`.
fn agents_in(dir: &tempfile::TempDir, installed: bool) -> std::path::PathBuf {
    let agents = dir.path().join("LaunchAgents");
    std::fs::create_dir_all(&agents).expect("agents dir");
    if installed {
        std::fs::write(agents.join("com.trusty.events.plist"), "<plist/>").expect("plist");
    }
    agents
}

/// Why (#10151, Architect ruling 2026-10-26): a host without the bus is a
/// supported setup. It read `Fail` and named `tctl install trusty-events`,
/// which exits 3 because the bus is internal (#10130).
/// What: no socket and no trusty-events plist grades `Warn`, says no action is
/// needed, and names no `tctl` command — every `tctl` verb on the bus exits
/// non-zero on a host without its plist, so the only remedy that exits 0 is
/// none.
/// Test: This is the test.
#[tokio::test]
async fn a_host_without_the_bus_warns_and_names_no_failing_remedy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agents = agents_in(&dir, false);
    let row = grade_resolved(Ok(sock_in(&dir)), &agents).await;
    assert_eq!(row.name, CHECK_NAME);
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(row.message.contains("not installed"), "{}", row.message);
    assert!(row.message.contains("needs no action"), "{}", row.message);
    assert!(!row.message.contains("tctl"), "{}", row.message);
}

/// Why (#10151 fail-open check): only "no socket and no plist" may be
/// downgraded. A host that HAS the bus must still fail when the bus is down,
/// refuses the dial, or answers without a frame; a blanket "any error is
/// `Warn`" would hide a dead bus.
/// What: with the plist installed, a missing socket is `Fail` naming `tctl
/// start`. A dead socket is `Fail` with or without the plist (the file proves a
/// bus ran here), as is a bus that hangs up without a frame. A LaunchAgents
/// path that cannot be inspected counts as installed, so it stays `Fail`.
/// Test: This is the test.
#[tokio::test]
async fn a_host_with_the_bus_still_fails_on_every_fault() {
    let dir = tempfile::tempdir().expect("tempdir");
    let installed = agents_in(&dir, true);
    let missing = grade_resolved(Ok(sock_in(&dir)), &installed).await;
    assert_eq!(missing.status, CheckStatus::Fail, "{}", missing.message);
    assert!(
        missing.message.contains("`tctl start trusty-events`"),
        "{}",
        missing.message
    );

    // A file where the directory should be: the plist lookup errors.
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, "").expect("blocker file");
    let unknown = grade_resolved(Ok(sock_in(&dir)), &blocker).await;
    assert_eq!(unknown.status, CheckStatus::Fail, "{}", unknown.message);

    // See #7215: bind in a 0700 directory and wait out an inherited fd.
    let stale_dir = tempfile::tempdir().expect("tempdir");
    let path = hardened_sock_in(&stale_dir);
    drop(trusty_common::uds::bind_hardened(&path).expect("bind"));
    await_listener_gone(&path).await;
    let not_installed = agents_in(&stale_dir, false);
    for agents in [&installed, &not_installed] {
        let row = grade_resolved(Ok(path.clone()), agents).await;
        assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
        assert!(row.message.contains("stale"), "{}", row.message);
    }

    let bus_dir = tempfile::tempdir().expect("tempdir");
    let bus = spawn_stub(&bus_dir, Stub::HangUp);
    let row = grade_resolved(Ok(bus.path.clone()), &installed).await;
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
}

// #10071: stub buses. Each binds a hardened socket, as `trusty-events` does,
// skips the connect-only probe, reads the subscribe request, and answers it
// one way.

/// A socket path in a 0700 directory, which `connect_hardened` requires.
fn hardened_sock_in(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("sockets").join("trusty-events.sock")
}

/// How a stub bus answers the subscribe request.
#[derive(Clone, Copy)]
enum Stub {
    /// Reads the request and never writes: a stopped or wedged bus.
    Silent,
    /// Sends one heartbeat item: a serving bus.
    Heartbeat,
    /// Refuses `heartbeat_ms` as a pre-#10071 bus did.
    PreHeartbeat,
    /// Closes the connection without a frame.
    HangUp,
    /// Refuses the subscription for a reason that is not `heartbeat_ms`.
    Refuse,
}

/// The subscribe params a pre-#10071 bus accepted, `deny_unknown_fields` as
/// they were, so its refusal text is the real one.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // Read only for the refusal its decode produces.
struct PreHeartbeatParams {
    #[serde(default)]
    since_seq: Option<u64>,
    #[serde(default)]
    since_id: Option<serde_json::Value>,
    #[serde(default)]
    filters: serde_json::Value,
}

/// What the test reads back from a stub bus.
struct StubBus {
    path: std::path::PathBuf,
    /// The `params` of the subscribe request the stub read.
    params: tokio::sync::oneshot::Receiver<serde_json::Value>,
    /// Tell the stub the probe has returned.
    probe_done: tokio::sync::oneshot::Sender<()>,
    /// Whether the probe's connection was closed when the probe returned.
    peer_closed: tokio::sync::oneshot::Receiver<bool>,
}

/// Bind a stub bus in `dir` that answers the first subscribe request as `stub`.
fn spawn_stub(dir: &tempfile::TempDir, stub: Stub) -> StubBus {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
    use trusty_common::control_bus::SubscribeItem;
    use trusty_common::uds::server::{RpcError, RpcStreamFrame};

    let path = hardened_sock_in(dir);
    let listener = trusty_common::uds::bind_hardened(&path).expect("bind");
    let (params_tx, params) = tokio::sync::oneshot::channel();
    let (probe_done, done_rx) = tokio::sync::oneshot::channel();
    let (closed_tx, peer_closed) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        loop {
            let Ok((conn, _)) = listener.accept().await else {
                return;
            };
            let mut reader = tokio::io::BufReader::new(conn);
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line).await.unwrap_or(0) == 0 {
                continue; // The connect-only socket probe sends nothing.
            }
            let request: serde_json::Value = serde_json::from_slice(&line).expect("json request");
            let params = request["params"].clone();
            let _ = params_tx.send(params.clone());
            let mut conn = reader.into_inner();
            let id = request["id"].clone();
            match stub {
                Stub::Silent => {}
                Stub::Heartbeat => {
                    let item = serde_json::to_value(SubscribeItem::Heartbeat { head: 7 })
                        .expect("encode heartbeat");
                    trusty_common::uds::write_frame(&mut conn, &RpcStreamFrame::item(id, item))
                        .await
                        .expect("write heartbeat");
                }
                Stub::PreHeartbeat => {
                    let refusal = serde_json::from_value::<PreHeartbeatParams>(params)
                        .err()
                        .expect("a pre-#10071 bus refuses heartbeat_ms");
                    let error =
                        RpcError::invalid_params(format!("params do not decode: {refusal}"));
                    let _ = trusty_common::uds::write_frame(
                        &mut conn,
                        &RpcStreamFrame::error(id, error),
                    )
                    .await;
                    return;
                }
                Stub::HangUp => return,
                Stub::Refuse => {
                    let error = RpcError::internal("every subscriber slot is in use");
                    let _ = trusty_common::uds::write_frame(
                        &mut conn,
                        &RpcStreamFrame::error(id, error),
                    )
                    .await;
                    return;
                }
            }
            let _ = done_rx.await;
            // A write to a socket whose peer closed fails with EPIPE. Polled:
            // macOS sets FD_CLOEXEC after `socket()`, so a child another test
            // spawns in that window can hold the probe's fd until it exits.
            let deadline = tokio::time::Instant::now() + CLOSE_BOUND;
            let closed = loop {
                if conn.write_all(b"\n").await.is_err() {
                    break true;
                }
                if tokio::time::Instant::now() >= deadline {
                    break false;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            };
            let _ = closed_tx.send(closed);
            return;
        }
    });
    StubBus {
        path,
        params,
        probe_done,
        peer_closed,
    }
}

/// The subscribe params a stub read, bounded so a probe that never sent one
/// fails rather than hangs.
async fn read_params(bus: &mut StubBus) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(1), &mut bus.params)
        .await
        .expect("the probe sent a subscribe request")
        .expect("the stub reported the request")
}

/// How long the stub waits to see the probe's connection close.
const CLOSE_BOUND: Duration = Duration::from_secs(10);

/// Whether the probe's connection was closed once it returned.
async fn closed_after_probe(bus: StubBus) -> bool {
    let _ = bus.probe_done.send(());
    tokio::time::timeout(CLOSE_BOUND + Duration::from_secs(1), bus.peer_closed)
        .await
        .expect("the stub reported")
        .expect("the stub answered")
}

/// Why (#10071, Dd1): a bus that accepts and never answers is hung, and the
/// row must say so within the 2 s budget rather than read `Ok`.
/// What: a silent stub grades `Fail`, names `hung`, gives the restart remedy
/// and the log, after waiting 2 s but no more than 2.5 s. The probe asked for
/// `heartbeat_ms: 500`, closed its connection, and left no task behind.
/// Test: This is the test.
#[tokio::test]
async fn a_silent_bus_is_hung_within_the_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut bus = spawn_stub(&dir, Stub::Silent);
    let tasks = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let started = std::time::Instant::now();
    let row = grade_resolved(Ok(bus.path.clone()), dir.path()).await;
    let took = started.elapsed();

    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("hung"), "{}", row.message);
    assert!(
        row.message.contains("`tctl restart trusty-events`"),
        "{}",
        row.message
    );
    assert!(row.message.contains("stderr.log"), "{}", row.message);
    assert!(took >= Duration::from_millis(1_900), "waited only {took:?}");
    assert!(took <= Duration::from_millis(2_500), "took {took:?}");
    assert_eq!(read_params(&mut bus).await["heartbeat_ms"], 500);
    assert_eq!(
        tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks(),
        tasks,
        "the probe left a task running"
    );
    assert!(
        closed_after_probe(bus).await,
        "the probe left its connection open"
    );
}

/// Why (#10071, Dd1): a frame on the subscription is the evidence a serving
/// bus gives; only it may read `Ok`.
/// What: a stub that sends one heartbeat grades `Ok` at once; the probe asked
/// for `heartbeat_ms: 500` and closed its connection.
/// Test: This is the test.
#[tokio::test]
async fn a_bus_that_sends_a_heartbeat_is_ok() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut bus = spawn_stub(&dir, Stub::Heartbeat);
    let started = std::time::Instant::now();
    let row = grade_resolved(Ok(bus.path.clone()), dir.path()).await;

    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "an answer is not a wait"
    );
    assert_eq!(read_params(&mut bus).await["heartbeat_ms"], 500);
    assert!(
        closed_after_probe(bus).await,
        "the probe left its connection open"
    );
}

/// Why (#10071: lockstep, no fallback): a bus older than the heartbeat refuses
/// `heartbeat_ms`; that is version skew, neither healthy nor hung.
/// What: a stub refusing `heartbeat_ms` the way a pre-#10071 bus did grades
/// `Warn`, naming the skew and the upgrade remedy.
/// Test: This is the test.
#[tokio::test]
async fn a_bus_that_refuses_heartbeat_ms_is_version_skew() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bus = spawn_stub(&dir, Stub::PreHeartbeat);
    let row = grade_resolved(Ok(bus.path.clone()), dir.path()).await;

    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        row.message.contains("older than this tm"),
        "{}",
        row.message
    );
    assert!(row.message.contains("heartbeat_ms"), "{}", row.message);
    assert!(
        row.message.contains("`tctl upgrade trusty-events`"),
        "{}",
        row.message
    );
}

/// Why (fail-closed): a bus that closes the subscription without a frame did
/// not answer; it must not read as serving.
/// What: a stub that hangs up after the request grades `Fail`.
/// Test: This is the test.
#[tokio::test]
async fn a_bus_that_hangs_up_without_a_frame_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bus = spawn_stub(&dir, Stub::HangUp);
    let row = grade_resolved(Ok(bus.path.clone()), dir.path()).await;
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
}

/// Why (#10071 review): only a refusal of `heartbeat_ms` is version skew; any
/// other refusal from a current bus is a fault, and calling it "older" would
/// send the operator to upgrade a bus that is already current.
/// What: a stub refusing with an internal error that does not name
/// `heartbeat_ms` grades `Fail`, and the text does not call the bus older.
/// Test: This is the test.
#[tokio::test]
async fn a_refusal_not_naming_heartbeat_ms_is_a_fault_not_skew() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bus = spawn_stub(&dir, Stub::Refuse);
    let row = grade_resolved(Ok(bus.path.clone()), dir.path()).await;
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(
        !row.message.contains("older than this tm"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("every subscriber slot is in use"),
        "{}",
        row.message
    );
}
