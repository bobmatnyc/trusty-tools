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

/// A daemon serving a real router on a tempdir socket.
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
            run_daemon_on(binding, &serve_socket, Some(&serve_data), async move {
                let _ = stopped.await;
            })
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

/// How long a daemon future given a shutdown signal that never fires may run
/// before the test calls it a regression.
///
/// Why a bound at all (#7312): this test's shutdown signal is
/// `std::future::pending()`, so a daemon that manages to bind serves forever.
/// On Linux `bind_singleton_hardened` did exactly that — it read the regular
/// file below as a dead socket, unlinked it, and bound — and the test consumed
/// its whole CI shard until the job's 45-minute cancel, with the shard's other
/// tests never reported. A regression must fail in seconds and say what
/// happened.
const BIND_FAILURE_BUDGET: Duration = Duration::from_secs(10);

/// Run a daemon on `socket` with a shutdown signal that never fires, bounded by
/// [`BIND_FAILURE_BUDGET`], and return the error it must stop with.
async fn expect_refused(socket: &Path) -> anyhow::Error {
    let data = tempfile::tempdir().expect("data tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let binding =
        ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind");
    timeout(
        BIND_FAILURE_BUDGET,
        run_daemon_on(binding, socket, Some(data.path()), std::future::pending()),
    )
    .await
    .expect("the daemon began serving instead of failing the bind — see #7312")
    .expect_err("an unbindable socket must stop the daemon")
}

/// A socket the kernel cannot bind stops the daemon outright (#6637 Fail-Open
/// Check). With the TCP listener gone there is no other door, so a daemon that
/// logged the failure and kept running would be a process nobody can reach.
///
/// What: a path longer than `sun_path` holds (104 bytes on macOS, 108 on
/// Linux), so the bind itself fails rather than any pre-check on the path.
/// Test: this function IS the test.
#[tokio::test]
async fn run_uds_socket_bind_failure_is_fatal() {
    let dir = tempfile::tempdir().expect("socket tempdir");
    let socket = dir.path().join(format!("{}.sock", "s".repeat(120)));
    let rendered = format!("{:#}", expect_refused(&socket).await);
    assert!(
        rendered.contains("bind the daemon socket"),
        "the failure must name the bind, got: {rendered}"
    );
    assert!(!socket.exists(), "nothing may be left at the path");
}

/// Something at the socket path that is not a socket is refused and left
/// alone (#7328) — never unlinked and replaced, which would let a daemon
/// delete a file it did not create.
///
/// Why the bound: #7312 — on Linux `bind_singleton_hardened` once read this
/// regular file as a dead socket, unlinked it, and served forever.
/// Test: this function IS the test.
#[tokio::test]
async fn run_daemon_refuses_a_non_socket_at_the_path() {
    let dir = tempfile::tempdir().expect("socket tempdir");
    let socket = dir.path().join("tcode.sock");
    std::fs::write(&socket, b"not a socket").expect("occupy the socket path");
    let rendered = format!("{:#}", expect_refused(&socket).await);
    assert!(
        rendered.contains("bind the daemon socket"),
        "the failure must name the bind, got: {rendered}"
    );
    assert_eq!(
        std::fs::read(&socket).expect("the occupying file must survive"),
        b"not a socket",
        "the occupying file must survive the refused bind unchanged"
    );
}

/// A socket directory left wider than `0700` is REPAIRED to `0700` before the
/// socket is created, and the socket comes up `0600` (#6637 criterion 1).
///
/// Why repaired, not refused: every install before #5099 created this
/// directory with `create_dir_all`, so it is `0755` on existing machines; the
/// directory's owner is still checked, and a foreign-owned one is refused
/// (`trusty_common::uds::dir::classify_existing_dir`).
/// Test: this function IS the test.
#[tokio::test]
async fn run_daemon_repairs_a_wide_socket_dir() {
    let sockets = tempfile::tempdir().expect("socket tempdir");
    let dir = sockets.path().join("wide");
    std::fs::create_dir(&dir).expect("create the socket dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("widen");
    let socket = dir.join("tcode.sock");
    let data = tempfile::tempdir().expect("data tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let binding =
        ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind");

    let (stop, stopped) = oneshot::channel::<()>();
    let serve_socket = socket.clone();
    let serve_data = data.path().to_path_buf();
    let joined = tokio::spawn(async move {
        run_daemon_on(binding, &serve_socket, Some(&serve_data), async move {
            let _ = stopped.await;
        })
        .await
    });
    wait_until_serving(&socket).await;

    let dir_mode = std::fs::metadata(&dir).expect("dir").permissions().mode() & 0o777;
    let socket_mode = std::fs::metadata(&socket)
        .expect("socket")
        .permissions()
        .mode()
        & 0o777;
    let _ = stop.send(());
    timeout(Duration::from_secs(10), joined)
        .await
        .expect("the serve loop must return within the budget")
        .expect("the serve task must not panic")
        .expect("a requested shutdown is a clean stop");
    assert_eq!(
        dir_mode, 0o700,
        "a wide socket dir must be narrowed to 0700"
    );
    assert_eq!(socket_mode, 0o600, "the socket must be owner-only");
}

/// #6637 Fail-Open Check: a serve task that panics ends the daemon with an
/// error. Without this the panic payload dies with the `JoinHandle` and the
/// process lingers with no listener.
/// Test: this function IS the test.
#[tokio::test]
async fn join_serve_task_errors_when_the_loop_panics() {
    let requested = std::sync::atomic::AtomicBool::new(false);
    let task = tokio::spawn(async { panic!("the accept loop died") });
    let err = super::join_serve_task(task, &requested)
        .await
        .expect_err("a panicked accept loop must not read as a clean stop");
    assert!(
        format!("{err:#}").contains("socket accept loop died"),
        "the error names the dead loop: {err:#}"
    );
}

/// #6637 Fail-Open Check: a loop that returns while no shutdown was asked for
/// is an error, so the process exits non-zero instead of 0.
/// Test: this function IS the test.
#[tokio::test]
async fn join_serve_task_errors_when_the_loop_ends_without_a_shutdown() {
    let requested = std::sync::atomic::AtomicBool::new(false);
    let task = tokio::spawn(async {});
    let err = super::join_serve_task(task, &requested)
        .await
        .expect_err("an unrequested return must not read as a clean stop");
    assert!(
        format!("{err:#}").contains("without a shutdown signal"),
        "the error says no shutdown was requested: {err:#}"
    );
}

/// The ordinary stop: the shutdown was requested, then the loop returned.
/// Proves the two error arms above are not an always-`Err` join.
/// Test: this function IS the test.
#[tokio::test]
async fn join_serve_task_is_ok_after_a_requested_shutdown() {
    let requested = std::sync::atomic::AtomicBool::new(true);
    let task = tokio::spawn(async {});
    super::join_serve_task(task, &requested)
        .await
        .expect("a requested shutdown is a clean stop");
}

/// A second daemon on a socket a live daemon holds is refused, and the first
/// keeps answering — the bind is the only duplicate guard (#6637).
/// Test: this function IS the test; `tests/daemon_no_tcp_6637.rs` covers the
/// real binary's exit status.
#[tokio::test]
async fn a_second_daemon_on_a_live_socket_is_refused() {
    let daemon = TestDaemon::start().await;
    let rendered = format!("{:#}", expect_refused(&daemon.socket).await);
    assert!(
        rendered.contains("bind the daemon socket"),
        "the failure must name the bind, got: {rendered}"
    );
    let response = daemon.call("ping", json!({})).await;
    assert!(response.error.is_none(), "the first daemon still answers");
    daemon.shutdown().await.expect("clean shutdown");
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

/// `workstream.events` must refuse an id no workstream carries with
/// `not_found` — the code the retired HTTP route answered as a `404`.
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

/// #4600: two projects get two sockets, one project always gets the same one,
/// projectless gets its own, and every one sits beside the shared socket so a
/// data-dir override isolates them all.
#[test]
fn project_socket_path_is_per_project_and_stable() {
    let shared = Path::new("/data/trusty-code/trusty-code.sock");
    let a = super::project_socket_path(shared, Some(Path::new("/work/a")));
    let b = super::project_socket_path(shared, Some(Path::new("/work/b")));
    let none = super::project_socket_path(shared, None);

    assert_ne!(a, b, "two projects must not share a socket");
    assert_eq!(
        a,
        super::project_socket_path(shared, Some(Path::new("/work/a"))),
        "one project must always derive the same socket"
    );
    assert_eq!(
        none,
        Path::new("/data/trusty-code/trusty-code-projectless.sock")
    );
    for path in [&a, &b, &none] {
        assert_ne!(path.as_path(), shared, "never the shared socket itself");
        assert_eq!(path.parent(), shared.parent(), "beside the shared socket");
    }
    let name = a.file_name().and_then(|n| n.to_str()).expect("utf-8 name");
    assert_eq!(
        name.len(),
        "trusty-code-".len() + 12 + ".sock".len(),
        "{name}"
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

/// `workstream.events` must refuse an id that is not a UUID with
/// `invalid_params` — the code the retired HTTP route answered as a `400`.
///
/// #6637: ported from `sse_tests::malformed_id_returns_400`; guards the
/// `Uuid::parse_str` branch of `workstreams::events_stream::open`.
/// Test: this function IS the test.
#[tokio::test]
async fn workstream_events_refuses_a_malformed_id() {
    let daemon = TestDaemon::start().await;
    let request = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "workstream.events",
        "params": {"workstream_id": "not-a-uuid"},
        "stream": true,
    });
    let frame =
        send_framed_request::<Value, RpcStreamFrame>(&daemon.socket, &request, CALL_TIMEOUT)
            .await
            .expect("a terminal error frame must be written");
    let error = frame.error.expect("the terminal frame must carry an error");
    assert_eq!(
        error.code,
        i64::from(crate::jsonrpc::RpcError::invalid_params("x").code),
        "got: {}",
        error.message
    );

    daemon.shutdown().await.expect("clean shutdown");
}

/// A `workstream.events` stream must deliver a live event published for a
/// session bound to the workstream, tagged with that session and its type.
///
/// #6637: ported from `sse_tests::route_streams_live_tagged_event_for_bound_
/// session`; the only end-to-end proof that the workstream fan-out reaches a
/// transport. It publishes in a loop because the daemon subscribes to the bus
/// when it handles the open, which this client cannot observe.
/// Test: this function IS the test.
#[tokio::test]
async fn workstream_events_streams_a_live_tagged_event_for_a_bound_session() {
    let daemon = TestDaemon::start().await;
    let created = daemon
        .call("workstream.create", json!({"name": "fan-out"}))
        .await;
    let workstream_id = created
        .result
        .as_ref()
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .expect("workstream.create returns an id")
        .to_string();
    let session = daemon
        .call(
            "session.create",
            json!({"task": "fan-out", "workstream_id": workstream_id}),
        )
        .await;
    let session_id = session
        .result
        .as_ref()
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .expect("session.create returns an id")
        .to_string();

    let request = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "workstream.events",
        "params": {"workstream_id": workstream_id},
        "stream": true,
    });
    let mut stream =
        send_framed_stream_request::<Value, Value>(&daemon.socket, &request, CALL_TIMEOUT)
            .await
            .expect("the stream must open");

    let publish = || {
        crate::events::publish(SessionEventEnvelope::new(
            session_id.clone(),
            1,
            chrono::Utc::now(),
            crate::events::Event::SessionStarted {
                session_id: session_id.clone(),
                project: "p".to_string(),
            },
        ));
    };
    let frame = timeout(Duration::from_secs(10), async {
        loop {
            publish();
            if let Ok(next) = timeout(Duration::from_millis(100), stream.next_frame()).await {
                let frame = next
                    .expect("the stream must stay open")
                    .expect("the frame must not be a terminal error");
                // `session.create` may have emitted its own event first.
                if frame["session_id"] == session_id.as_str() {
                    return frame;
                }
            }
        }
    })
    .await
    .expect("a tagged frame for the bound session must arrive");
    assert_eq!(frame["event_type"], "session_started", "got: {frame}");

    daemon.shutdown().await.expect("clean shutdown");
}
