//! Full user-cycle end-to-end test for the session lifecycle.
//!
//! Why: the per-handler unit tests drive functions directly and the `e2e`
//! suite covers individual scenario areas. This test walks the *entire*
//! operator-facing path — start, command, pause, resume, stop — over the live
//! HTTP API in one continuous flow, the way the CLI / TUI / Telegram bot drive
//! the daemon.
//! What: a standalone integration-test binary that binds the daemon's axum
//! router to a random loopback port and exercises the lifecycle with `reqwest`.
//! Test: `cargo test -p trusty-mpm-daemon --test test_session_lifecycle`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_mpm::daemon::state::DaemonState;

/// A live daemon for this test, bound to a random loopback port.
///
/// Why: the lifecycle test needs a real HTTP endpoint; bundling the server task
/// handle lets `Drop` abort it so the port is released between tests.
/// What: holds the base URL and the background server task handle.
struct TestServer {
    url: String,
    handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// Bind the daemon router to `127.0.0.1:0` and wait until it is healthy.
    ///
    /// Why: `axum::serve` accepts connections asynchronously; firing a request
    /// the instant after spawn can race the listener, so we poll `/health`.
    /// What: builds an in-memory `DaemonState`, serves the router on a task, and
    /// blocks until `GET /health` returns `200` (max ~2s).
    async fn spawn() -> Self {
        let state = DaemonState::shared();
        let app = trusty_mpm::daemon::api::router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback port");
        let addr: SocketAddr = listener.local_addr().expect("resolve bound addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let url = format!("http://{addr}");
        wait_for_health(&url).await;
        Self { url, handle }
    }

    /// Build an absolute URL for `path` (which should start with `/`).
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Poll `GET /health` until the daemon answers, or panic after ~2s.
async fn wait_for_health(base: &str) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    let health = format!("{base}/health");
    loop {
        if let Ok(resp) = client.get(&health).send().await
            && resp.status().is_success()
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!("daemon at {base} did not become healthy within 2s");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Full user cycle: start → command (API-only) → pause → resume → stop.
///
/// Why: end-to-end validation that the session lifecycle flows correctly
/// through the HTTP API — the operator-facing path for the full user cycle.
/// What: starts a daemon with an in-memory test state, walks the full
/// lifecycle via HTTP, asserts each transition.
/// Note: tmux send/capture are not exercised (tmux may not be installed in CI);
/// the command and output endpoints are called but the underlying driver ops
/// are best-effort and errors are logged rather than propagated to the test.
#[tokio::test]
async fn full_user_cycle() {
    let server = TestServer::spawn().await;
    let client = reqwest::Client::new();

    // 1. Start a session.
    let created = client
        .post(server.url("/sessions"))
        .json(&json!({ "project": "/tmp/lifecycle" }))
        .send()
        .await
        .expect("create session");
    // The handler returns axum::Json, which serialises with a 200 status even
    // though the OpenAPI annotation documents a semantic 201.
    assert!(
        created.status() == 200 || created.status() == 201,
        "create status: {}",
        created.status()
    );
    let created: Value = created.json().await.expect("create body");
    let name = created["name"].as_str().expect("name present").to_string();
    // The friendly name resolves pause/command/output; `DELETE /sessions/{id}`
    // resolves strictly by UUID, so keep the id for the stop step.
    let id = created["id"].as_str().expect("id present").to_string();
    assert!(!id.is_empty());

    // 2. List sessions — exactly one, in a live state.
    let listed: Value = client
        .get(server.url("/sessions"))
        .send()
        .await
        .expect("list sessions")
        .json()
        .await
        .expect("list body");
    let sessions = listed["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 1);
    let status = sessions[0]["status"].as_str().expect("status string");
    assert!(
        status == "Starting" || status == "Active",
        "post-start status: {status}"
    );

    // 3. Send a command, requesting a summarized capture. tmux errors are
    //    swallowed by the handler; the `?compress=summarise` query exercises the
    //    "summarize output" step of the full user cycle.
    let cmd = client
        .post(server.url(&format!("/sessions/{name}/command?compress=summarise")))
        .json(&json!({ "command": "help" }))
        .send()
        .await
        .expect("send command");
    assert_eq!(cmd.status(), 200);
    let cmd_body: Value = cmd.json().await.expect("command body");
    assert_eq!(cmd_body["sent"], true);
    assert!(cmd_body["output"].is_string());
    // A summarized command response carries the compression byte counts.
    assert!(
        cmd_body.get("original_bytes").is_some(),
        "original_bytes key present"
    );
    assert!(
        cmd_body.get("compressed_bytes").is_some(),
        "compressed_bytes key present"
    );

    // 4. Capture output.
    let out = client
        .get(server.url(&format!("/sessions/{name}/output")))
        .send()
        .await
        .expect("get output");
    assert_eq!(out.status(), 200);
    let out_body: Value = out.json().await.expect("output body");
    assert!(out_body.get("output").is_some(), "output key present");

    // 5. Pause the session.
    let paused = client
        .post(server.url(&format!("/sessions/{name}/pause")))
        .json(&json!({ "summary": "mid-task" }))
        .send()
        .await
        .expect("pause session");
    assert_eq!(paused.status(), 200);
    let paused_body: Value = paused.json().await.expect("pause body");
    assert_eq!(paused_body["paused"], true);

    // 6. List — status is now Paused.
    let listed: Value = client
        .get(server.url("/sessions"))
        .send()
        .await
        .expect("list after pause")
        .json()
        .await
        .expect("list body");
    assert_eq!(
        listed["sessions"][0]["status"], "Paused",
        "session must be Paused after pause"
    );

    // 7. Resume the session.
    let resumed = client
        .post(server.url(&format!("/sessions/{name}/resume")))
        .send()
        .await
        .expect("resume session");
    assert_eq!(resumed.status(), 200);

    // 8. List — status is back to a live state, not Paused.
    let listed: Value = client
        .get(server.url("/sessions"))
        .send()
        .await
        .expect("list after resume")
        .json()
        .await
        .expect("list body");
    let status = listed["sessions"][0]["status"]
        .as_str()
        .expect("status string");
    assert!(
        status == "Active" || status == "Starting",
        "post-resume status must be live, got {status}"
    );

    // 9. Stop (delete) the session.
    let deleted = client
        .delete(server.url(&format!("/sessions/{id}")))
        .send()
        .await
        .expect("delete session");
    assert!(
        deleted.status() == 200 || deleted.status() == 204,
        "delete status: {}",
        deleted.status()
    );

    // 10. List — the registry is empty again.
    let listed: Value = client
        .get(server.url("/sessions"))
        .send()
        .await
        .expect("list after stop")
        .json()
        .await
        .expect("list body");
    assert!(
        listed["sessions"]
            .as_array()
            .expect("sessions array")
            .is_empty(),
        "session must be gone after stop"
    );
}
