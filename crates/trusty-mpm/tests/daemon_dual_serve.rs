//! The daemon serves HTTP and the RPC socket at once, and ONE shutdown drains
//! both (#6288 slice 1, acceptance criterion 1).
//!
//! Why its own test binary: the proof reassigns `$HOME` to a scratch directory
//! so `core::host_state_gate` classifies this process as a scratch environment
//! and the startup tmux/host-process adoption is skipped — without that, booting
//! a daemon inside the lib test binary would adopt the operator's live Claude
//! Code panes into a temp registry. `$HOME` is process-global, so it cannot be
//! reassigned inside a binary that runs other tests concurrently; this file
//! holds exactly ONE test, which is what makes the reassignment safe. Same
//! rationale as `scratch_home_tmux_gate.rs`.
//!
//! Why not the e2e harness: `tests/e2e/harness.rs` serves `api::router` on a
//! bare `axum::serve`, so it never reaches `daemon::serve_with_shutdown` and
//! cannot observe the fan-out at all. This drives the real body.
//!
//! What: binds both listeners, spawns `serve_with_shutdown` with a oneshot in
//! place of SIGTERM, proves both answer, fires the single shutdown, and asserts
//! both ended and the socket file is gone.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test daemon_dual_serve`.

#![cfg(feature = "daemon")]

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::daemon::state::DaemonState;

/// Headroom for a drain on a loaded machine, not a latency budget.
///
/// A regressed fan-out does not drain slowly — it never drains at all, because
/// the listener that was left waiting on its own signal is waiting for a
/// SIGTERM this test never sends. So this timeout is what turns that regression
/// into a failure rather than a hung suite.
const DRAIN_BUDGET: Duration = Duration::from_secs(30);

/// Send one JSON-RPC frame over `socket` and read the answer back.
async fn call_over_socket(
    socket: &std::path::Path,
    method: &str,
) -> Result<serde_json::Value, trusty_common::uds::UdsRpcError> {
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": {},
    });
    trusty_common::uds::send_framed_request(socket, &request, Duration::from_secs(10)).await
}

/// Criterion 1: both listeners serve concurrently, and one shutdown drains both.
///
/// Why this is the test the slice owes: `socket_tests.rs` drives the socket's
/// own accept loop with a synthetic shutdown, and the e2e suite drives HTTP with
/// no socket at all. Neither reaches `serve_with_shutdown`'s cancellation
/// fan-out, which is the one piece of code that makes a single signal stop two
/// listeners. Replacing the fan-out with two independent `shutdown_signal()`
/// awaits makes this test hang past `DRAIN_BUDGET` and fail — a real SIGTERM is
/// exactly what a test process cannot deliver to itself.
///
/// What: one HTTP request and one UDS request against a live daemon, then one
/// shutdown, then three assertions — the serve future returned, the socket file
/// is unlinked, and the HTTP port no longer answers.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn serve_http_drains_both_listeners_on_shutdown() {
    let scratch_home = TempDir::new().expect("scratch home");
    // SAFETY: this binary holds exactly one test, so nothing races these
    // process-global writes. `$HOME` puts `host_state_gate` into its scratch
    // arm; the two flags switch off the sweeps that would otherwise touch real
    // repositories and managed-session state on a developer machine.
    unsafe {
        std::env::set_var("HOME", scratch_home.path());
        std::env::set_var("TRUSTY_MPM_ORPHAN_GC", "0");
        std::env::set_var("TRUSTY_MPM_INPROJECT_HYGIENE", "0");
    }

    let tmp = TempDir::new().expect("tempdir");
    let paths = FrameworkPaths::under(tmp.path());
    std::fs::create_dir_all(&paths.hooks).expect("hooks dir");
    std::fs::create_dir_all(&paths.instructions).expect("instructions dir");
    std::fs::create_dir_all(&paths.agents).expect("agents dir");
    let state = Arc::new(DaemonState::with_paths(&paths));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port");
    let addr = listener.local_addr().expect("local addr");
    let health_url = format!("http://{addr}/health");

    let socket = tmp.path().join("sockets").join("trusty-mpm.sock");
    let rpc = trusty_mpm::daemon::socket::bind(&socket)
        .await
        .expect("bind the rpc socket");

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(trusty_mpm::daemon::serve_with_shutdown(
        state,
        listener,
        rpc,
        async move {
            let _ = stop_rx.await;
        },
    ));

    // ── Both listeners answer ────────────────────────────────────────────────
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client");

    let mut http_ok = false;
    for _ in 0..200 {
        if let Ok(r) = client.get(&health_url).send().await
            && r.status().is_success()
        {
            http_ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(http_ok, "the HTTP listener must answer /health at {addr}");

    // The socket serves an empty router in slice 1, so the answer is an error
    // frame. That it is a FRAME is the point: the request was read and answered
    // over the socket while HTTP was serving on the same runtime.
    let answer = call_over_socket(&socket, "mpm.health")
        .await
        .expect("the rpc listener must answer while HTTP is also serving");
    assert_eq!(
        answer["error"]["code"],
        serde_json::json!(trusty_common::uds::server::CODE_METHOD_NOT_FOUND),
        "slice 1 registers no methods: {answer}"
    );

    // ── One shutdown drains both ─────────────────────────────────────────────
    stop_tx.send(()).expect("the serve task must still be running");

    let joined = tokio::time::timeout(DRAIN_BUDGET, served)
        .await
        .expect("both listeners must drain on ONE shutdown — a listener still \
                 waiting on its own signal never returns")
        .expect("the serve task must not panic");
    joined.expect("a clean drain is not an error");

    assert!(
        !socket.exists(),
        "the socket file must be unlinked on the drain: {}",
        socket.display()
    );
    assert!(
        client.get(&health_url).send().await.is_err(),
        "the HTTP listener must be closed after the drain, not still answering"
    );
}
