//! #9034 regression tests: a slow daemon is not a down daemon, and autostart
//! keeps a live lock.
//!
//! Every test runs against a stub server on an ephemeral port or an address
//! whose connect fails at once (#9837), a temp-dir lock file, and an injected
//! autostart step. None
//! touches the daemon on 7880, `~/.trusty-mpm/daemon.lock`, or launchd.

use super::*;
use crate::commands::daemon_pid_identity::PidIdentity;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A URL whose connect fails at once with a connect error, never a timeout.
///
/// #9837: not `127.0.0.1:1` — a host that drops a SYN to a closed loopback
/// port turns that connect into a timeout, which classifies as `Unknown`.
fn refused_url() -> String {
    format!("http://{}", crate::commands::dead_addr::dead_addr())
}

/// A pid no process can hold (above the default `pid_max`).
const DEAD_PID: u32 = 4_194_303;

/// A server that accepts connections and never answers — the #9034 stalled
/// store, seen from the client. Returns its base URL.
async fn serve_silent() -> String {
    use tokio::io::AsyncReadExt as _;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            held.push(sock);
        }
    });
    format!("http://{addr}")
}

/// A client whose request timeout is short enough for a test.
fn short_timeout_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
        .expect("client")
}

/// The error the picker's listing produces against `url`.
async fn listing_error(client: &reqwest::Client, url: &str) -> anyhow::Error {
    let daemon = trusty_mpm::client::DaemonClient::with_client(client.clone(), url);
    crate::commands::session_picker::fetch_live_sessions(&daemon, Some("o/r"), false)
        .await
        .expect_err("the listing must fail against a stub")
}

/// Write a lock naming `addr` and `pid` at `path`.
fn write_lock(path: &std::path::Path, addr: &str, pid: u32) {
    std::fs::write(
        path,
        format!("product = \"trusty-mpm\"\npid = {pid}\naddr = \"{addr}\"\nstarted_at = \"\"\n"),
    )
    .expect("write lock");
}

/// The restart command the stop message must name.
const RESTART_CMD: &str = "launchctl kickstart -k gui/$(id -u)/com.trusty.mpm";

/// [`run_flow_with`] where every live pid counts as a daemon.
async fn run_flow(
    client: &reqwest::Client,
    url: &str,
    lock: &std::path::Path,
    autostart_result: anyhow::Result<String>,
) -> (PickerFlow, bool) {
    run_flow_with(client, url, lock, PidIdentity::Daemon, autostart_result).await
}

/// Run [`picker_or_autostart_with`] with a recording autostart stub; every
/// pid has `identity`.
async fn run_flow_with(
    client: &reqwest::Client,
    url: &str,
    lock: &std::path::Path,
    identity: PidIdentity,
    autostart_result: anyhow::Result<String>,
) -> (PickerFlow, bool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = PickerProject {
        source_id: "o/r",
        workspace: tmp.path(),
        repo_url: "/nonexistent/repo",
        cwd: tmp.path(),
    };
    let called = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&called);
    let identify = move |_: u32| identity;
    let host = HostProbe {
        lock_path: lock,
        identify: &identify,
        restart_cmd: RESTART_CMD,
    };
    let flow = picker_or_autostart_with(client, url, &project, &host, move || async move {
        flag.store(true, Ordering::SeqCst);
        autostart_result
    })
    .await;
    (flow, called.load(Ordering::SeqCst))
}

fn assert_stopped(flow: PickerFlow) -> String {
    match flow {
        PickerFlow::Done(Err(e)) => e.to_string(),
        PickerFlow::Done(Ok(())) => panic!("a slow daemon must report an error, not succeed"),
        PickerFlow::Offline => panic!("a slow daemon must never reach the managed-clone fallback"),
    }
}

#[tokio::test]
async fn classify_timeout_is_unknown_not_down() {
    let client = short_timeout_client();
    let err = listing_error(&client, &serve_silent().await).await;
    let reach = classify_list_failure(&err, None);
    assert!(
        matches!(reach, DaemonReach::Unknown(ref why) if why.contains("timed out")),
        "a timeout must be Unknown, got {reach:?}"
    );
}

#[tokio::test]
async fn classify_refused_with_live_lock_pid_is_unknown() {
    let client = short_timeout_client();
    let err = listing_error(&client, &refused_url()).await;
    let reach = classify_list_failure(&err, Some("daemon.lock names live pid 1"));
    assert!(
        matches!(reach, DaemonReach::Unknown(_)),
        "refused with a live pid must be Unknown, got {reach:?}"
    );
}

#[tokio::test]
async fn classify_refused_without_live_pid_is_down() {
    let client = short_timeout_client();
    let err = listing_error(&client, &refused_url()).await;
    assert_eq!(classify_list_failure(&err, None), DaemonReach::Down);
}

/// 🔴 #6288 fail-open check: over the daemon socket, an absent socket is the
/// positive evidence that lets bare `tm` start the daemon, and a socket the
/// client refuses to dial is not. Before the cutover every socket error was
/// `Unknown`, so a first run never autostarted.
#[tokio::test]
async fn classify_an_absent_socket_is_down_and_a_refused_one_unknown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = short_timeout_client();
    let absent = trusty_mpm::core::socket_locator(&tmp.path().join("absent.sock"));
    let err = listing_error(&client, &absent).await;
    assert_eq!(classify_list_failure(&err, None), DaemonReach::Down);

    let wide = tmp.path().join("w").join("trusty-mpm.sock");
    std::fs::create_dir_all(wide.parent().expect("parent")).expect("dir");
    let _listener = trusty_common::uds::bind_hardened(&wide).expect("bind");
    std::fs::set_permissions(&wide, std::os::unix::fs::PermissionsExt::from_mode(0o666))
        .expect("widen");
    let err = listing_error(&client, &trusty_mpm::core::socket_locator(&wide)).await;
    assert!(
        matches!(classify_list_failure(&err, None), DaemonReach::Unknown(_)),
        "a socket the client refuses to dial is not a down daemon"
    );
}

#[test]
fn live_pid_requires_matching_addr() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    let daemon = |_: u32| PidIdentity::Daemon;
    write_lock(&lock, "http://127.0.0.1:47001", std::process::id());
    assert!(lock_evidence_for("http://127.0.0.1:47001", &lock, &daemon).is_some());
    assert!(lock_evidence_for("http://127.0.0.1:47001/", &lock, &daemon).is_some());
    assert!(
        lock_evidence_for("http://127.0.0.1:47002", &lock, &daemon).is_none(),
        "a lock for another address is no evidence about this one"
    );
    write_lock(&lock, "http://127.0.0.1:47001", DEAD_PID);
    assert!(lock_evidence_for("http://127.0.0.1:47001", &lock, &daemon).is_none());
    assert!(lock.exists(), "classification must not delete the lock");
    let absent = tmp.path().join("absent");
    assert!(lock_evidence_for("http://127.0.0.1:47001", &absent, &daemon).is_none());
}

/// 🔴 #6288 fail-open check: the client's `url` is the daemon socket locator,
/// so a live lock must be matched by its `socket_path`. Before the cutover the
/// match read only `addr`, so a socket `url` saw no evidence and a slow daemon
/// was autostarted over.
#[test]
fn live_pid_matches_the_socket_locator() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    let socket = tmp.path().join("trusty-mpm.sock");
    let daemon = |_: u32| PidIdentity::Daemon;
    std::fs::write(
        &lock,
        format!(
            "product = \"trusty-mpm\"\npid = {}\naddr = \"http://127.0.0.1:47001\"\n\
             started_at = \"\"\nsocket_path = \"{}\"\n",
            std::process::id(),
            socket.display()
        ),
    )
    .expect("write lock");
    let url = trusty_mpm::core::socket_locator(&socket);
    assert!(lock_evidence_for(&url, &lock, &daemon).is_some());
    let other = trusty_mpm::core::socket_locator(&tmp.path().join("other.sock"));
    assert!(
        lock_evidence_for(&other, &lock, &daemon).is_none(),
        "a lock for another socket is no evidence about this one"
    );
}

#[tokio::test]
async fn slow_listing_stops_without_autostart() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = short_timeout_client();
    let url = serve_silent().await;
    let (flow, autostarted) = run_flow(
        &client,
        &url,
        &tmp.path().join("daemon.lock"),
        Ok(url.clone()),
    )
    .await;
    assert!(
        !autostarted,
        "a timed-out listing must not start a second daemon"
    );
    let msg = assert_stopped(flow);
    assert!(
        msg.contains("not responding"),
        "message must name a slow daemon: {msg}"
    );
    assert!(
        msg.contains(RESTART_CMD),
        "message must name the restart command: {msg}"
    );
}

#[tokio::test]
async fn refused_with_live_lock_pid_stops_without_autostart() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    write_lock(&lock, &refused_url(), std::process::id());
    let client = short_timeout_client();
    let (flow, autostarted) = run_flow(&client, &refused_url(), &lock, Ok(String::new())).await;
    assert!(
        !autostarted,
        "a live lock pid must not lead to a second daemon"
    );
    assert_stopped(flow);
}

#[tokio::test]
async fn refused_without_live_pid_autostarts_then_goes_offline() {
    // Positive control: a down daemon still gets autostart and the fallback.
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    write_lock(&lock, &refused_url(), DEAD_PID);
    let client = short_timeout_client();
    let (flow, autostarted) = run_flow(
        &client,
        &refused_url(),
        &lock,
        Err(anyhow::anyhow!("stub: spawn failed")),
    )
    .await;
    assert!(autostarted, "a down daemon must be auto-started");
    assert!(
        matches!(flow, PickerFlow::Offline),
        "a down daemon that cannot start goes offline"
    );
}

#[tokio::test]
async fn slow_listing_after_autostart_stops_not_offline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = short_timeout_client();
    let slow = serve_silent().await;
    let (flow, autostarted) = run_flow(
        &client,
        &refused_url(),
        &tmp.path().join("daemon.lock"),
        Ok(slow),
    )
    .await;
    assert!(autostarted);
    assert_stopped(flow);
}

#[tokio::test]
async fn alive_unresponsive_autostart_stops_not_offline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = short_timeout_client();
    let alive = DaemonAliveUnresponsive {
        evidence: "the launchd service is loaded".to_string(),
    };
    let (flow, _) = run_flow(
        &client,
        &refused_url(),
        &tmp.path().join("daemon.lock"),
        Err(alive.into()),
    )
    .await;
    assert_stopped(flow);
}

#[test]
fn flags_connect_phase_timeout_is_unknown() {
    // A connect-phase timeout sets both flags; it is a busy listener.
    let reach = reach_from_flags(true, true, None, "connect timed out");
    assert!(
        matches!(reach, DaemonReach::Unknown(ref why) if why.contains("timed out")),
        "a connect-phase timeout must be Unknown, got {reach:?}"
    );
}

#[test]
fn unverified_lock_pid_is_evidence_not_absence() {
    // #9034: a live pid whose identity is unknown, or that is another program,
    // is still evidence — never "down", never a reason to delete.
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    write_lock(&lock, "http://127.0.0.1:47001", std::process::id());
    for identity in [PidIdentity::Unknown, PidIdentity::NotDaemon] {
        let identify = move |_: u32| identity;
        let evidence = lock_evidence_for("http://127.0.0.1:47001", &lock, &identify)
            .expect("a live unverified pid is evidence");
        assert!(
            evidence.contains("remove ~/.trusty-mpm/daemon.lock if no tm daemon is running"),
            "the recovery must be named: {evidence}"
        );
    }
}

#[tokio::test]
async fn refused_with_unknown_identity_lock_pid_stops_and_keeps_the_lock() {
    // #9034 regression: live pid, unknown identity, refused port → neither
    // delete the lock nor spawn.
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    write_lock(&lock, &refused_url(), std::process::id());
    let client = short_timeout_client();
    let (flow, autostarted) = run_flow_with(
        &client,
        &refused_url(),
        &lock,
        PidIdentity::Unknown,
        Err(anyhow::anyhow!("stub: spawn failed")),
    )
    .await;
    assert!(!autostarted, "an unverified live lock pid must not spawn");
    let msg = assert_stopped(flow);
    assert!(
        msg.contains("daemon.lock"),
        "the stop names the lock: {msg}"
    );
    assert!(lock.exists(), "an unverified live lock is never deleted");
}

#[tokio::test]
async fn unverified_lock_autostart_error_stops_not_offline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = short_timeout_client();
    let blocked = DaemonLockUnverified {
        evidence: "daemon.lock names live pid 1".to_string(),
    };
    let (flow, _) = run_flow(
        &client,
        &refused_url(),
        &tmp.path().join("daemon.lock"),
        Err(blocked.into()),
    )
    .await;
    assert_stopped(flow);
}

#[tokio::test]
async fn refused_with_live_lock_pid_message_names_recovery() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock = tmp.path().join("daemon.lock");
    write_lock(&lock, &refused_url(), std::process::id());
    let client = short_timeout_client();
    let (flow, _) = run_flow(&client, &refused_url(), &lock, Ok(String::new())).await;
    let msg = assert_stopped(flow);
    assert!(
        msg.contains("tm start") && msg.contains("daemon.lock"),
        "a lock-pid stop must name its recovery: {msg}"
    );
}
