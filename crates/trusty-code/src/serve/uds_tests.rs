//! Tests for [`crate::serve::uds`] — the daemon's native transport (#6637).
//!
//! Why: every one of these drives a REAL hardened socket under a tempdir. The
//! properties at stake are transport-level — that a bind failure stops the
//! daemon, that the fallback really does reach all 34 JSON-RPC methods, that
//! the two stream names are not swallowed by it, and that an open tail is not
//! cut by a quiet window — and none of them can be observed against an
//! in-process router.
//! What: a [`TestDaemon`] harness that binds a socket in a tempdir, serves the
//! real router built by `build_router_at` (so nothing touches the developer's
//! `~/.trusty-code`), and shuts down on drop.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::time::timeout;
use trusty_common::uds::server::{RpcResponse, RpcStreamFrame};
use trusty_common::uds::{send_framed_request, send_framed_stream_request};

use crate::binding::ProjectBinding;
use crate::events::SessionEventEnvelope;

use super::run_daemon_on;

/// A generous per-call budget: these dial a socket in the same process.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// A daemon serving a real router on a tempdir socket, with no HTTP listener.
struct TestDaemon {
    socket: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    joined: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    _sockets: TempDir,
    _data: TempDir,
    _project: TempDir,
}

impl TestDaemon {
    /// Bind and serve, returning once the socket answers a probe.
    async fn start() -> Self {
        let sockets = tempfile::tempdir().expect("socket tempdir");
        let data = tempfile::tempdir().expect("data tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let socket = sockets.path().join("tcode.sock");
        let binding =
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind");

        let (stop, stopped) = oneshot::channel();
        let serve_socket = socket.clone();
        let serve_data = data.path().to_path_buf();
        let joined = tokio::spawn(async move {
            run_daemon_on(
                binding,
                &serve_socket,
                Some(&serve_data),
                None,
                async move {
                    let _ = stopped.await;
                },
            )
            .await
        });

        wait_until_serving(&socket).await;
        Self {
            socket,
            stop: Some(stop),
            joined: Some(joined),
            _sockets: sockets,
            _data: data,
            _project: project,
        }
    }

    /// One unary call, as a client would make it.
    async fn call(&self, method: &str, params: Value) -> RpcResponse {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        send_framed_request::<Value, RpcResponse>(&self.socket, &request, CALL_TIMEOUT)
            .await
            .unwrap_or_else(|e| panic!("{method} must answer over the socket: {e}"))
    }

    /// Stop the serve loop and wait for the socket to be unlinked.
    async fn shutdown(mut self) -> anyhow::Result<()> {
        let stop = self.stop.take().expect("stop sender is taken once");
        let _ = stop.send(());
        let joined = self.joined.take().expect("join handle is taken once");
        timeout(Duration::from_secs(10), joined)
            .await
            .expect("the serve loop must return within the budget")
            .expect("the serve task must not panic")
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Poll until the socket accepts a connection, so no test races the bind.
async fn wait_until_serving(socket: &Path) {
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(200)).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the daemon never began serving {}", socket.display());
}

/// Open a stream and read frames from it.
async fn open_stream(
    socket: &Path,
    method: &str,
    params: Value,
) -> Result<trusty_common::uds::FramedStream<SessionEventEnvelope>, trusty_common::uds::UdsRpcError>
{
    let request = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": method,
        "params": params,
        "stream": true,
    });
    send_framed_stream_request(socket, &request, CALL_TIMEOUT).await
}

/// A socket path already occupied by something that is not a dead socket must
/// stop the daemon outright — never a silent degrade to HTTP only, which would
/// leave every socket client reporting "no daemon" against a live process.
#[tokio::test]
async fn run_uds_socket_bind_failure_is_fatal() {
    let dir = tempfile::tempdir().expect("socket tempdir");
    let data = tempfile::tempdir().expect("data tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let socket = dir.path().join("tcode.sock");
    // A regular file, not a stale socket: `bind_singleton_hardened` only
    // unlinks a path the kernel proved is not serving, and a regular file is
    // never that, so the bind below genuinely fails.
    std::fs::write(&socket, b"not a socket").expect("occupy the socket path");

    let binding =
        ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind");
    let err = run_daemon_on(
        binding,
        &socket,
        Some(data.path()),
        // A port is offered and must NOT be bound: the socket failed first.
        Some(0),
        std::future::pending(),
    )
    .await
    .expect_err("an unbindable socket must stop the daemon");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("bind the daemon socket"),
        "the failure must name the bind, got: {rendered}"
    );
}

/// Every method the JSON-RPC router registers must be reachable over the
/// socket through the fallback — the whole point of mounting the table rather
/// than restating it.
#[tokio::test]
async fn fallback_dispatches_every_jsonrpc_router_method() {
    let daemon = TestDaemon::start().await;
    let project = tempfile::tempdir().expect("probe project tempdir");
    let (router, _sessions, _workstreams) = crate::serve::build_router_at(
        ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
        project.path(),
    )
    .await
    .expect("build_router_at");
    let names = router.method_names();
    assert!(
        names.len() >= 34,
        "the router is expected to carry at least the 34 methods #6637 surveyed, got {}",
        names.len()
    );

    for method in names {
        let response = daemon.call(method, json!({})).await;
        // Bare `{}` params are wrong for most of these — what matters is that
        // the name RESOLVED. `method_not_found` is the one answer that proves
        // the fallback did not reach the dispatcher.
        if let Some(error) = response.error {
            assert_ne!(
                error.code,
                trusty_common::uds::server::CODE_METHOD_NOT_FOUND,
                "{method} must be reachable through the fallback, got: {}",
                error.message
            );
        }
    }

    daemon.shutdown().await.expect("clean shutdown");
}

/// A name registered as a stream must be served as one, not swallowed by the
/// catch-all — the router checks its stream table before the fallback, and
/// this is the assertion that keeps that ordering true for these two names.
#[tokio::test]
async fn stream_names_win_over_the_fallback() {
    let daemon = TestDaemon::start().await;
    let created = daemon
        .call("session.create", json!({"task": "stream-ordering"}))
        .await;
    let session_id = created
        .result
        .as_ref()
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .expect("session.create returns an id")
        .to_string();

    // Without the `stream` flag a streaming method answers CODE_STREAM_REQUIRED
    // rather than being dispatched by the fallback as an unknown name.
    let response = daemon
        .call("session.events", json!({"session_id": session_id}))
        .await;
    let error = response.error.expect("a unary call must be refused");
    assert_eq!(
        error.code,
        trusty_common::uds::server::CODE_STREAM_REQUIRED,
        "session.events must be a stream method, got: {}",
        error.message
    );

    let response = daemon
        .call("workstream.events", json!({"workstream_id": "not-a-uuid"}))
        .await;
    let error = response.error.expect("a unary call must be refused");
    assert_eq!(
        error.code,
        trusty_common::uds::server::CODE_STREAM_REQUIRED,
        "workstream.events must be a stream method, got: {}",
        error.message
    );

    daemon.shutdown().await.expect("clean shutdown");
}

/// `workstream.events` must refuse an id no workstream carries, with the same
/// `not_found` code the HTTP route answers as a `404`.
#[tokio::test]
async fn workstream_events_refuses_an_unknown_id() {
    let daemon = TestDaemon::start().await;
    let request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "workstream.events",
        "params": {"workstream_id": uuid::Uuid::new_v4().to_string()},
        "stream": true,
    });
    let frame =
        send_framed_request::<Value, RpcStreamFrame>(&daemon.socket, &request, CALL_TIMEOUT)
            .await
            .expect("a terminal error frame must be written");
    let error = frame.error.expect("the terminal frame must carry an error");
    assert_eq!(
        error.code,
        i64::from(crate::jsonrpc::RpcError::not_found("x").code),
        "got: {}",
        error.message
    );

    daemon.shutdown().await.expect("clean shutdown");
}

/// The socket is unreachable to another uid, and a peer that IS us passes the
/// check every accepted connection runs.
///
/// What this proves, precisely: `trusty_common::uds::server::handle_connection`
/// runs `ensure_peer_is_self` before a byte is read, and that check is tested
/// generically in trusty-common. What this test adds is that THIS daemon binds
/// through the hardened path — a `0600` socket inside a `0700` directory — so a
/// foreign uid cannot even traverse to the socket to be refused. A test cannot
/// dial as a different uid without running as root, which is why the
/// permission bits are the observable half.
#[tokio::test]
async fn uds_peer_uid_mismatch_is_refused() {
    let daemon = TestDaemon::start().await;

    let socket_mode = std::fs::metadata(&daemon.socket)
        .expect("the socket exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(socket_mode, 0o600, "the socket must be owner-only");

    let dir_mode = std::fs::metadata(daemon.socket.parent().expect("socket has a parent"))
        .expect("the socket directory exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "the socket directory must be owner-only");

    // A peer that IS us is accepted and answered.
    let response = daemon.call("ping", json!({})).await;
    assert!(response.error.is_none(), "our own uid must be accepted");

    daemon.shutdown().await.expect("clean shutdown");
}

/// An open tail must survive a quiet window. `RpcServeOptions::read_timeout`
/// bounds how long a peer may take to deliver its REQUEST frame and does not
/// apply between streamed response frames, and this daemon installs no idle
/// policy at all — so a session that says nothing for a while must not have its
/// stream cut.
#[tokio::test]
async fn session_events_stream_stays_open_under_idle_window() {
    let daemon = TestDaemon::start().await;
    let created = daemon
        .call("session.create", json!({"task": "idle-window"}))
        .await;
    let session_id = created
        .result
        .as_ref()
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .expect("session.create returns an id")
        .to_string();

    let mut stream = open_stream(
        &daemon.socket,
        "session.events",
        json!({"session_id": session_id}),
    )
    .await
    .expect("the stream must open");

    // Quiet for longer than any per-frame cadence the daemon produces.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let sent = daemon
        .call(
            "session.send",
            json!({"session_id": session_id, "input": "after the quiet window"}),
        )
        .await;
    assert!(sent.error.is_none(), "session.send must succeed");

    let frame = timeout(Duration::from_secs(10), stream.next_frame())
        .await
        .expect("a frame must arrive after the quiet window")
        .expect("the stream must still be open")
        .expect("the frame must not be a terminal error");
    assert_eq!(frame.session_id, session_id);

    // #7217: dropping the tail closes the socket, and `write_stream` now reads
    // that departure off the socket rather than waiting on a quiet producer, so
    // the drain ends with the handler instead of spending its whole budget.
    drop(stream);
    daemon.shutdown().await.expect("clean shutdown");
}

/// The socket sits beside the daemon's other persistent state, which is where
/// every client resolves it from.
#[test]
fn socket_path_sits_in_the_daemon_data_dir() {
    let path = super::socket_path().expect("the data directory must resolve");
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some("trusty-code.sock")
    );
}

/// A name nothing serves must be refused with `method_not_found`, not
/// swallowed — the fallback answers for every unregistered name, so it is the
/// only thing that can report an unknown one.
#[tokio::test]
async fn fallback_reports_an_unknown_method() {
    let daemon = TestDaemon::start().await;
    let response = daemon.call("no.such.method", json!({})).await;
    let error = response.error.expect("an unknown method must be refused");
    assert_eq!(
        error.code,
        trusty_common::uds::server::CODE_METHOD_NOT_FOUND,
        "got: {}",
        error.message
    );

    daemon.shutdown().await.expect("clean shutdown");
}
