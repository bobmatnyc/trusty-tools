//! Coverage for the API daemon's Unix-socket transport (#6433 slice 2).
//!
//! What these prove, in the order the daemon uses the code:
//!   - the socket path convention, and the one env override;
//!   - the method table matches the two documented constants;
//!   - the bind is hardened — `0700` directory, `0600` socket;
//!   - a stale socket file is taken over, a LIVE one is refused loudly;
//!   - a request round-trips through the real axum router;
//!   - no TCP address is bound or published on the default path.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use tokio::sync::oneshot;

use super::client::ApiSocketClient;
use super::dispatch::MAX_BODY_BYTES;
use super::wire::{HttpRequestFrame, HttpResponseFrame, METHOD_HEALTH, METHOD_REQUEST};
use super::{
    SOCKET_PATH_ENV, api_socket_path, build_rpc_router, health_ok, self_socket_path,
    serve_with_shutdown,
};
use crate::api::server::{AppState, build_router};

/// Serializes the tests that mutate this module's two env vars.
///
/// Why not `test_env::HOME_LOCK`: neither variable is `$HOME`, and taking the
/// crate-wide HOME lock for them would serialize this module against every
/// HOME-sandboxing test in the crate for no reason.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A router with no manager attached — enough for `/api/health` and for any
/// route whose "not configured" answer is the thing under test.
fn test_router() -> Router {
    build_router(AppState::default())
}

/// Start a daemon on `socket`, returning the handle and its shutdown trigger.
///
/// Why a helper: eight tests need a live daemon and each would otherwise
/// re-spell the same `oneshot` plumbing, with the same chance of forgetting to
/// await the join handle and leaving a socket behind for the next test.
async fn spawn_daemon(socket: &Path) -> (tokio::task::JoinHandle<()>, oneshot::Sender<()>) {
    let (tx, rx) = oneshot::channel::<()>();
    let socket = socket.to_path_buf();
    let handle = tokio::spawn(async move {
        serve_with_shutdown(test_router(), &socket, async {
            let _ = rx.await;
        })
        .await
        .expect("daemon serves");
    });
    // The bind is the first thing `serve_with_shutdown` does; wait for the
    // socket file rather than sleeping a fixed interval.
    for _ in 0..200 {
        if socket_is_live(&socket).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (handle, tx)
}

async fn socket_is_live(socket: &Path) -> bool {
    socket.exists() && health_ok(socket).await
}

/// A socket path short enough for `sun_path` on both Linux and macOS.
fn socket_in(dir: &Path) -> PathBuf {
    dir.join("a.api.sock")
}

// ---------------------------------------------------------------- path convention

/// Why: the path is the daemon's whole address since #6433 — there is no
/// discovery file for a caller to disagree with — so the convention itself is
/// the contract that keeps caller and daemon on the same socket.
#[test]
fn api_socket_path_uses_project_id() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: single-threaded within this lock; restored before the guard drops.
    unsafe { std::env::remove_var(SOCKET_PATH_ENV) };
    let path = api_socket_path(Path::new("/tmp/demo-project"));
    let rendered = path.to_string_lossy().to_string();
    assert!(
        rendered.ends_with("/.trusty-agents/sockets/demo-project.api.sock")
            || rendered.ends_with("/.trusty-agents/state/api.sock"),
        "unexpected socket path: {rendered}"
    );
}

/// Why: a test and a sandbox own neither the home directory nor the project
/// root the derived path is built from, and #6433 kept one override rather than
/// a flag that would also have to be threaded through the `--api` early
/// dispatch in `runtime::startup`.
#[test]
fn api_socket_path_honors_the_env_override() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: single-threaded within this lock; removed before returning.
    unsafe { std::env::set_var(SOCKET_PATH_ENV, "/tmp/explicit.api.sock") };
    let path = api_socket_path(Path::new("/tmp/demo-project"));
    unsafe { std::env::remove_var(SOCKET_PATH_ENV) };
    assert_eq!(path, PathBuf::from("/tmp/explicit.api.sock"));
}

/// Why: `self_socket_path` is what every production caller uses; if it resolved
/// a different root from `api_socket_path` the daemon and its clients would
/// bind and dial different files and neither would report an error.
#[test]
fn self_socket_path_matches_the_derived_path() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var(SOCKET_PATH_ENV, "/tmp/self-check.api.sock") };
    let resolved = self_socket_path();
    unsafe { std::env::remove_var(SOCKET_PATH_ENV) };
    assert_eq!(resolved, PathBuf::from("/tmp/self-check.api.sock"));
}

// ---------------------------------------------------------------- method table

/// Why: the two method-name constants are the wire contract every in-repo
/// client spells. A rename that missed one would compile on both sides and fail
/// only at runtime, as a `method_not_found` frame.
#[test]
fn rpc_router_registers_every_documented_method() {
    let router = build_rpc_router(test_router());
    let mut names: Vec<&str> = router.method_names().collect();
    names.sort_unstable();
    assert_eq!(names, vec![METHOD_HEALTH, METHOD_REQUEST]);
    // #6621: answering a health probe must not count as the traffic that keeps
    // a serve loop alive.
    let liveness: Vec<&str> = router.liveness_names().collect();
    assert_eq!(liveness, vec![METHOD_HEALTH]);
}

// ---------------------------------------------------------------- hardening

/// Why: the socket IS the trust boundary since #6433 — no CSRF machinery, no
/// bearer token, no origin check. A group- or world-reachable socket would hand
/// an arbitrary-subprocess-spawning API to any local account.
#[tokio::test]
async fn serve_binds_a_hardened_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sockets");
    let socket = socket_in(&dir);
    let (handle, stop) = spawn_daemon(&socket).await;

    let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    let sock_mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "socket directory must be 0700");
    assert_eq!(sock_mode, 0o600, "socket must be 0600");

    let _ = stop.send(());
    handle.await.unwrap();
}

/// Why: a daemon that is SIGKILLed never reaches the unlink and leaves its
/// socket file behind. `bind_hardened` refuses an occupied path, so without the
/// singleton probe the next `--api` would fail to start with no
/// operator-visible cause — the failure slice 1 recorded live for the search
/// daemon.
#[tokio::test]
async fn serve_takes_over_a_socket_left_by_a_dead_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sockets");
    std::fs::create_dir_all(&dir).unwrap();
    let socket = socket_in(&dir);
    // A corpse: a plain file where a socket used to be. Nothing answers it.
    std::fs::write(&socket, b"stale").unwrap();

    let (handle, stop) = spawn_daemon(&socket).await;
    assert!(
        health_ok(&socket).await,
        "a socket file nobody serves must be taken over"
    );
    let _ = stop.send(());
    handle.await.unwrap();
}

/// Why: the takeover above must never clobber a LIVE daemon. Two API servers on
/// one path would each own half the connections and neither would say so.
#[tokio::test]
async fn a_second_bind_against_a_live_daemon_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = socket_in(&tmp.path().join("sockets"));
    let (handle, stop) = spawn_daemon(&socket).await;

    let (never_tx, never_rx) = oneshot::channel::<()>();
    let second = serve_with_shutdown(test_router(), &socket, async {
        let _ = never_rx.await;
    })
    .await;
    drop(never_tx);
    let err = second.expect_err("a second bind on a live socket must fail");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("bind trusty-agents API socket"),
        "the failure must name the bind: {rendered}"
    );

    let _ = stop.send(());
    handle.await.unwrap();
}

/// Why: `bind_hardened` chmods but does not unlink, and `UnixListener`'s `Drop`
/// does not either, so a daemon that just returned would leave a file behind.
#[tokio::test]
async fn serve_unlinks_its_socket_on_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = socket_in(&tmp.path().join("sockets"));
    let (handle, stop) = spawn_daemon(&socket).await;
    assert!(socket.exists());

    let _ = stop.send(());
    handle.await.unwrap();
    assert!(
        !socket.exists(),
        "the socket file must be gone after shutdown"
    );
}

// ---------------------------------------------------------------- round trip

/// Why: this is the whole claim of slice 2 — the 47 routes are unchanged and
/// the transport under them is a socket. If `GET /api/health` does not answer
/// over the socket with the same body the router produces, the tunnel is not a
/// transport, it is a rewrite.
#[tokio::test]
async fn a_request_round_trips_through_the_axum_router() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = socket_in(&tmp.path().join("sockets"));
    let (handle, stop) = spawn_daemon(&socket).await;

    let client = ApiSocketClient::at(&socket);
    let response = client.get("/api/health").await.expect("health answers");
    assert_eq!(response.status, 200);
    let body = response.json().expect("health body is JSON");
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string(), "health body: {body}");

    let _ = stop.send(());
    handle.await.unwrap();
}

/// Why: a 404 is an answer, not a transport failure. A client that turned every
/// non-2xx into an `Err` would make `session attach`'s "no such session" branch
/// unreachable.
#[tokio::test]
async fn a_non_2xx_status_comes_back_as_a_status_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = socket_in(&tmp.path().join("sockets"));
    let (handle, stop) = spawn_daemon(&socket).await;

    let response = ApiSocketClient::at(&socket)
        .get("/api/definitely-not-a-route")
        .await
        .expect("an unknown route still answers");
    assert_eq!(response.status, 404);
    assert!(!response.is_success());

    let _ = stop.send(());
    handle.await.unwrap();
}

/// Why: every in-repo caller that used to `reqwest::post` a JSON body now goes
/// through `post_json`. If the body or the `content-type` were dropped in
/// translation, axum's `Json` extractor would answer 415 or 422 and the caller
/// would read it as a server bug.
#[tokio::test]
async fn a_json_post_body_reaches_the_handler() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = socket_in(&tmp.path().join("sockets"));
    let (handle, stop) = spawn_daemon(&socket).await;

    // `/api/ctrl/sessions` requires `project_path`; posting without it must be
    // rejected BY THE HANDLER (a 4xx), which proves the body arrived and was
    // parsed rather than never reaching the extractor.
    let response = ApiSocketClient::at(&socket)
        .post_json("/api/ctrl/sessions", &serde_json::json!({ "nonsense": 1 }))
        .await
        .expect("the exchange completes");
    assert!(
        (400..500).contains(&response.status),
        "a malformed body must be refused by the handler, got {}",
        response.status
    );

    let _ = stop.send(());
    handle.await.unwrap();
}

/// Why: a dial against a path nobody serves must be an error the caller can
/// report, not a hang and not a silent empty answer.
#[tokio::test]
async fn client_reports_a_dead_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = socket_in(&tmp.path().join("sockets"));
    let err = ApiSocketClient::at(&socket)
        .with_timeout(Duration::from_millis(200))
        .get("/api/health")
        .await
        .expect_err("no daemon is serving this path");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("trusty-agents API socket"),
        "the error must name the socket: {rendered}"
    );
}

/// Why: `is_service_running` decides whether the REPL reports a running
/// service. Answering `true` off a socket file's mere existence is the failure
/// #6433 removed from the pid-file path.
#[tokio::test]
async fn health_ok_is_false_with_no_socket() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!health_ok(&socket_in(tmp.path())).await);
}

// ---------------------------------------------------------------- no TCP

/// Why: the console proxy resolved this daemon from `http_addr`, and that file
/// predates the migration on every existing machine. A kept file would forward
/// `/api/agents/*` to whatever now holds the port it names — the exact hazard
/// #6285/#6287 record for the retired search and analyze proxy rows.
#[tokio::test]
async fn serve_publishes_no_tcp_address() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    // SAFETY: serialized by ENV_LOCK; removed before this test returns.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", &data_dir) };

    let agents_dir = trusty_common::resolve_data_dir("trusty-agents").unwrap();
    std::fs::create_dir_all(&agents_dir).unwrap();
    let addr_file = agents_dir.join("http_addr");
    std::fs::write(&addr_file, "127.0.0.1:8765").unwrap();

    let socket = socket_in(&tmp.path().join("sockets"));
    let (handle, stop) = spawn_daemon(&socket).await;
    assert!(health_ok(&socket).await, "the daemon answers on the socket");
    assert!(
        !addr_file.exists(),
        "a pre-migration http_addr file must be removed, not left to name a dead port"
    );

    let _ = stop.send(());
    handle.await.unwrap();
    assert!(
        !addr_file.exists(),
        "shutdown must not re-publish an http_addr file"
    );
    unsafe { std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE") };
}

/// Why: the mandate for slice 2 is that the TCP listener is gone from the
/// default path, not merely unpublished. A port this process had bound would be
/// unbindable while the daemon runs.
///
/// The two ports are the only ones this daemon ever defaulted to:
/// `8080` (`service::DEFAULT_SERVICE_PORT`, retired here) and `8765` (the port
/// the Tauri shell launched `--api` on). A port already held by SOMETHING ELSE
/// on the host — a developer's own live daemon — is not evidence either way, so
/// the assertion is made against a port this test itself proves was free a
/// moment earlier.
#[tokio::test]
async fn serve_binds_no_tcp_port() {
    // Reserve and release an ephemeral port: whatever the kernel just handed
    // out is free right now, so a daemon that grabs "a free port" would take
    // one from this range.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let free_port = probe.local_addr().unwrap().port();
    drop(probe);

    let tmp = tempfile::tempdir().unwrap();
    let socket = socket_in(&tmp.path().join("sockets"));
    let (handle, stop) = spawn_daemon(&socket).await;
    assert!(health_ok(&socket).await);

    let rebound = std::net::TcpListener::bind(("127.0.0.1", free_port));
    assert!(
        rebound.is_ok(),
        "the API daemon must hold no TCP port; {free_port} became unbindable"
    );

    let _ = stop.send(());
    handle.await.unwrap();
}

// ---------------------------------------------------------------- wire types

/// Why: the frame is the contract between two binaries that are upgraded
/// independently; a field rename that both sides compile is caught here.
#[test]
fn request_frame_round_trips() {
    let frame = HttpRequestFrame::new("POST", "/api/task?wait=1")
        .with_json(&serde_json::json!({ "task": "hello" }))
        .unwrap();
    let encoded = serde_json::to_string(&frame).unwrap();
    let decoded: HttpRequestFrame = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.method, "POST");
    assert_eq!(decoded.path, "/api/task?wait=1");
    assert!(
        decoded
            .headers
            .iter()
            .any(|(n, v)| n == "content-type" && v == "application/json")
    );
    let body: serde_json::Value = serde_json::from_slice(&decoded.body_bytes().unwrap()).unwrap();
    assert_eq!(body["task"], "hello");
}

/// Why: the UI's assets are binary. A `String` body field would either corrupt
/// them or force a second encoding path for the JSON routes; base64 is the one
/// path both take.
#[test]
fn body_bytes_survive_a_base64_round_trip() {
    let raw: Vec<u8> = (0u8..=255).collect();
    let frame = HttpResponseFrame::new(200, Vec::new(), &raw);
    assert_eq!(frame.body_bytes().unwrap(), raw);
    let decoded: HttpResponseFrame =
        serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
    assert_eq!(decoded.body_bytes().unwrap(), raw);
    assert_eq!(decoded.status, 200);
}

/// Why: `MAX_BODY_BYTES` is set so that base64 of a maximal body still fits
/// `trusty_common::uds::MAX_FRAME_BYTES`. If it drifted above that, the daemon
/// would produce frames its own clients refuse to read.
#[test]
fn max_body_encodes_within_the_frame_budget() {
    let encoded = MAX_BODY_BYTES.div_ceil(3) * 4;
    assert!(
        encoded as u64 <= trusty_common::uds::MAX_FRAME_BYTES,
        "base64 of a maximal body ({encoded} bytes) must fit the frame budget"
    );
}
