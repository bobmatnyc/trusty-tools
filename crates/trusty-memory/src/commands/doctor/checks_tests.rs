//! Tests for the daemon-health probe seam and the run verdict (issue #4001).
//!
//! Why: `doctor/mod.rs` carries its tests inline and sits near the 500-SLOC
//! cap, so the #4001 probe and verdict tests live beside `checks.rs` instead.
//! What: drives [`check_daemon_health_at`] against in-process sockets that
//! answer at once, answer slowly inside the budget, answer without a health
//! body, or accept and never answer, and pins [`summarize`]'s rule that an
//! undetermined check is not a pass.
//! Test: this IS the test module.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::super::{CheckResult, CheckStatus};
use super::{check_daemon_health_at, summarize};

/// How a stand-in daemon treats each request.
#[derive(Clone, Copy)]
enum Reply {
    /// Answer with a healthy body after this delay.
    After(Duration),
    /// Answer at once with this raw frame.
    Frame(&'static str),
    /// Accept the connection and never write a byte.
    Never,
}

/// Serve `reply` on a hardened socket under a fresh temp dir.
///
/// Why: the probe dials through `connect_hardened`, which refuses a socket that
/// is not `0600` in a `0700` directory, so a bare `UnixListener` would test the
/// refusal arm instead of the one named. Nothing here touches a live daemon.
/// What: returns the socket path, the temp dir that owns it, and the accept
/// task, which the caller aborts.
async fn stand_in(reply: Reply) -> (PathBuf, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("sockets").join("stand-in.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind hardened socket");
    let task = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            match reply {
                Reply::Never => held.push(stream),
                Reply::After(delay) => {
                    let body = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "status": "ok",
                            "daemon_state": "ready",
                            "worker": {"in_flight": 0, "wedged": false},
                        },
                    });
                    tokio::spawn(answer_once(stream, delay, body.to_string()));
                }
                Reply::Frame(raw) => {
                    tokio::spawn(answer_once(stream, Duration::ZERO, raw.to_string()));
                }
            }
        }
    });
    (socket, tmp, task)
}

/// Read one request line from `stream`, wait `delay`, then write `frame` as one line.
async fn answer_once(stream: tokio::net::UnixStream, delay: Duration, frame: String) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return;
    }
    tokio::time::sleep(delay).await;
    let _ = reader
        .get_mut()
        .write_all(format!("{frame}\n").as_bytes())
        .await;
}

/// Probe `socket` with `budget`, failing the test if the probe itself hangs.
async fn probe(socket: &std::path::Path, budget: Duration) -> CheckResult {
    tokio::time::timeout(
        budget + Duration::from_secs(5),
        check_daemon_health_at("daemon socket".to_string(), socket, budget),
    )
    .await
    .expect("the probe must finish within its own budget")
}

/// Why (issue #4001, error arm): a daemon wedged hard enough to accept a
/// connection and never answer must neither hang the doctor nor read as a pass.
/// What: a stand-in that holds every connection silently, probed with a 200 ms
/// budget; asserts `Unknown` and a message naming the timeout.
/// Test: itself.
#[tokio::test]
async fn a_socket_that_accepts_and_never_answers_is_unknown_within_the_budget() {
    let (socket, _tmp, task) = stand_in(Reply::Never).await;
    let result = probe(&socket, Duration::from_millis(200)).await;
    task.abort();

    assert_eq!(result.status, CheckStatus::Unknown, "{result:?}");
    let detail = result.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("did not answer") && detail.contains("could not be determined"),
        "the timeout must be named, not dressed as a verdict: {detail}"
    );
    assert!(
        !summarize(&[result]).healthy,
        "a timed-out probe must not end the run healthy"
    );
}

/// Why (issue #4001, error arms): an answer that carries no readable health
/// body has not shown that the workers are moving, so it must never end the
/// run green.
/// What: stand-ins answering with a JSON-RPC error, a frame with neither
/// `result` nor `error`, and a frame that is not JSON; asserts `Fail`,
/// `Unknown` and `Fail`, and that none summarizes as a healthy run.
/// Test: itself.
#[tokio::test]
async fn an_answer_without_a_health_body_is_never_a_healthy_run() {
    let cases = [
        (
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"boom"}}"#,
            CheckStatus::Fail,
        ),
        (r#"{"jsonrpc":"2.0","id":1}"#, CheckStatus::Unknown),
        ("not json", CheckStatus::Fail),
    ];
    for (frame, expected) in cases {
        let (socket, _tmp, task) = stand_in(Reply::Frame(frame)).await;
        let result = probe(&socket, Duration::from_secs(2)).await;
        task.abort();

        assert_eq!(result.status, expected, "frame {frame}: {result:?}");
        assert!(
            !summarize(&[result]).healthy,
            "frame {frame} must not end the run healthy"
        );
    }
}

/// Why (issue #4001, no false alarm): the fix must still let a responsive
/// daemon pass, including one that is slow but answers inside the budget.
/// What: stand-ins answering at once and after 300 ms, probed with a 2 s
/// budget; both must be `Pass` and a healthy run.
/// Test: itself.
#[tokio::test]
async fn a_responsive_daemon_passes() {
    for delay in [Duration::ZERO, Duration::from_millis(300)] {
        let (socket, _tmp, task) = stand_in(Reply::After(delay)).await;
        let result = probe(&socket, Duration::from_secs(2)).await;
        task.abort();

        assert_eq!(
            result.status,
            CheckStatus::Pass,
            "a daemon answering after {delay:?} must pass: {result:?}"
        );
        assert!(summarize(&[result]).healthy, "delay {delay:?}");
    }
}

/// Why (issue #4001, dream-cycle shape): a wedge caused by a held handle lock
/// has nothing in flight, so the gauge wording would read "0s with 0 in
/// flight". Doctor must fail and name the lock and palace instead.
/// What: a stand-in reporting `wedged` with a `stalled_lock`; asserts `Fail`,
/// the palace and lock in the message, and an unhealthy run.
/// Test: itself.
#[tokio::test]
async fn a_wedge_from_a_held_handle_lock_fails_and_names_the_lock() {
    let frame = r#"{"jsonrpc":"2.0","id":1,"result":{"status":"wedged","daemon_state":"ready","worker":{"in_flight":0,"wedged":true,"stalled_lock":{"palace":"trusty-tools","lock":"write","age_secs":131}}}}"#;
    let (socket, _tmp, task) = stand_in(Reply::Frame(frame)).await;
    let result = probe(&socket, Duration::from_secs(2)).await;
    task.abort();

    assert_eq!(result.status, CheckStatus::Fail, "{result:?}");
    let detail = result.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("\"trusty-tools\"")
            && detail.contains("\"write\"")
            && detail.contains("131s"),
        "the held lock must be named: {detail}"
    );
    assert!(!summarize(&[result]).healthy);
}

/// Why (issue #4001): the run used to end green with exit 0 whenever nothing
/// had `Fail`ed, so one undetermined check beside passes read as healthy.
/// What: asserts a pass plus an unknown is not healthy, and the tally names
/// the undetermined column.
/// Test: itself.
#[test]
fn an_undetermined_check_is_not_a_healthy_run() {
    let summary = summarize(&[
        CheckResult::pass("a", "fine"),
        CheckResult::unknown("daemon socket", "did not answer"),
    ]);
    assert!(!summary.healthy);
    assert_eq!(
        summary.line,
        "1 passed, 0 warnings, 1 undetermined, 0 failed."
    );
}

/// Why: `Warn` has never flipped the exit code, and #4001 must not start.
/// What: asserts passes plus a warning is still a healthy run.
/// Test: itself.
#[test]
fn warnings_alone_are_a_healthy_run() {
    let summary = summarize(&[
        CheckResult::pass("a", "fine"),
        CheckResult::warn("b", "minor"),
    ]);
    assert!(summary.healthy);
    assert!(!summarize(&[CheckResult::fail("c", "broken")]).healthy);
}
