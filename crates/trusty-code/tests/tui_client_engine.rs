//! Integration tests for [`trusty_code::tui_client::CodeEngine`] against a
//! real `tcode` daemon socket (issue #3415, DOC-50 §3.3/§3.4; retransported in
//! #6637).
//!
//! Why the mock HTTP daemon is gone: `CodeEngine` no longer speaks HTTP. Its
//! correctness is still entirely about the wire contract — which RPC methods
//! it calls, with what params, and how it turns the daemon's answers into
//! `trusty_code_tui::ReplEvent`s — but that contract is now framed JSON-RPC
//! over a `0600` Unix socket, so the fixture is a REAL daemon
//! (`trusty_code::serve::uds::run_daemon_on`) with its HTTP listener switched
//! off, plus a hand-rolled stub socket for the two scenarios that need a
//! failure the real daemon never produces on purpose.
//!
//! What: `tui_engine_end_to_end_with_http_port_closed` is the load-bearing one
//! — a full create/send/attach-tail with no TCP listener anywhere, which is
//! what proves no TUI path falls back to HTTP. The rest cover `setup`'s
//! workstream reporting, `cancel_session`'s thin-client axiom, the
//! reconnect-exhaustion failure surface, and the workstream-activation wire
//! field names.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::oneshot;
use tokio::time::timeout;
use trusty_code::binding::ProjectBinding;
use trusty_code::tui_client::CodeEngine;
use trusty_code::tui_client::uds_rpc::UdsRpcClient;
use trusty_code_tui::{ReplEvent, TuiEngine};

const WS_NAME: &str = "Feature X";

/// A real daemon on a throwaway socket, serving NO HTTP.
struct RealDaemon {
    socket: PathBuf,
    project: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    _sockets: tempfile::TempDir,
    _data: tempfile::TempDir,
    _project_dir: tempfile::TempDir,
}

impl RealDaemon {
    async fn start() -> Self {
        let sockets = tempfile::tempdir().expect("socket tempdir");
        let data = tempfile::tempdir().expect("data tempdir");
        let project_dir = tempfile::tempdir().expect("project tempdir");
        let socket = sockets.path().join("tcode.sock");
        let project = project_dir
            .path()
            .canonicalize()
            .expect("canonicalize project");
        let binding = ProjectBinding::resolve(Some(project.clone())).expect("tempdir must bind");

        let (stop, stopped) = oneshot::channel();
        let serve_socket = socket.clone();
        let serve_data = data.path().to_path_buf();
        tokio::spawn(async move {
            trusty_code::serve::uds::run_daemon_on(
                binding,
                &serve_socket,
                Some(&serve_data),
                // The whole point: no TCP listener exists for anything to fall
                // back to.
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
            project,
            stop: Some(stop),
            _sockets: sockets,
            _data: data,
            _project_dir: project_dir,
        }
    }

    fn engine(&self) -> CodeEngine {
        CodeEngine::with_socket(self.socket.clone(), Some(self.project.clone()))
    }

    fn client(&self) -> UdsRpcClient {
        UdsRpcClient::new(self.socket.clone())
    }
}

impl Drop for RealDaemon {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn wait_until_serving(socket: &Path) {
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(200)).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the daemon never began serving {}", socket.display());
}

/// Drain everything queued on `rx` right now.
fn drain(rx: &mut UnboundedReceiver<ReplEvent>) -> Vec<ReplEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

/// **The proof that no TUI path falls back to HTTP.**
///
/// The daemon binds its socket and nothing else — `run_daemon_on` is called
/// with `http_port: None`, so there is no loopback port in this process for a
/// client to reach even by accident. The engine then drives a full session
/// through: `setup` creates one, a `session.send` writes into it, and the
/// engine's own `session.events` stream tails it back with the ring-buffer
/// replay. A regression that reintroduced an HTTP call anywhere on this path
/// would fail here with a transport error, not merely a slower answer.
#[tokio::test]
async fn tui_engine_end_to_end_with_http_port_closed() {
    let daemon = RealDaemon::start().await;
    let engine = daemon.engine();
    let (tx, mut rx) = unbounded_channel();

    engine.setup(tx).await.expect("setup over the socket");
    let events = drain(&mut rx);
    let session_line = events
        .iter()
        .find_map(|event| match event {
            ReplEvent::StatusMessage(text) if text.contains("connected to tcode daemon") => {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("setup must report the daemon it connected to");
    assert!(
        session_line.contains(&daemon.socket.display().to_string()),
        "the status line must name the socket, not a URL: {session_line}"
    );

    // The session the engine created is real and reachable over the same
    // socket — `session.list` is served through the fallback.
    let client = daemon.client();
    let listed = client
        .call("session.list", json!({}))
        .await
        .expect("session.list over the socket");
    let sessions = listed["sessions"]
        .as_array()
        .expect("session.list returns an array");
    assert_eq!(sessions.len(), 1, "setup must have created one session");
    let session_id = sessions[0]["id"]
        .as_str()
        .expect("a session carries an id")
        .to_string();

    // Send, then tail: the attach half, over a stream method.
    let mut stream = client
        .open_stream::<Value>("session.events", json!({"session_id": session_id}))
        .await
        .expect("session.events must open");
    client
        .call(
            "session.send",
            json!({"session_id": session_id, "input": "over the socket"}),
        )
        .await
        .expect("session.send over the socket");

    let mut saw_input = false;
    for _ in 0..10 {
        let frame = timeout(Duration::from_secs(10), stream.next_frame())
            .await
            .expect("a frame must arrive within the budget")
            .expect("the stream must still be open")
            .expect("the frame must not be a terminal error");
        if frame.to_string().contains("over the socket") {
            saw_input = true;
            break;
        }
    }
    assert!(saw_input, "the tail must carry the input that was sent");

    engine
        .cancel_session()
        .await
        .expect("cancel_session over the socket");
    let status = client
        .call("session.status", json!({"session_id": session_id}))
        .await
        .expect("session.status over the socket");
    assert_eq!(
        status["status"], "cancelled",
        "the daemon performs the cancellation, not the client (DOC-39 §2.1 C-2)"
    );
}

/// `setup` must report the daemon's active workstream and populate the
/// synchronous `commands()`/`picker()` caches (#3428) before it returns.
#[tokio::test]
async fn setup_creates_session_and_reports_active_workstream() {
    let daemon = RealDaemon::start().await;
    let client = daemon.client();
    let created = client
        .call("workstream.create", json!({"name": WS_NAME}))
        .await
        .expect("workstream.create over the socket");
    let ws_id = created["id"].as_str().expect("a workstream carries an id");
    client
        .call("workstream.activate", json!({"id": ws_id}))
        .await
        .expect("workstream.activate over the socket");

    let engine = daemon.engine();
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx).await.expect("setup");

    let events = drain(&mut rx);
    let updated = events
        .iter()
        .any(|event| matches!(event, ReplEvent::WorkstreamUpdated(ws) if ws.name == WS_NAME));
    assert!(
        updated,
        "setup must report the active workstream: {events:?}"
    );
    assert!(
        !engine.commands().is_empty(),
        "setup must populate the commands cache"
    );
    assert!(
        engine.picker("workstream").is_some(),
        "setup must populate the workstream picker cache"
    );
}

// ── Stub-socket fixtures, for failures a real daemon never produces ──

/// A stub daemon answering unary calls from `answers` and streaming
/// `stream_frames` for `session.events`/`workstream.events`.
///
/// Why hand-rolled: the two scenarios below need a stream that ends early over
/// and over, and a workstream event injected on demand. A real daemon does
/// neither on purpose.
struct StubDaemon {
    socket: PathBuf,
    _dir: tempfile::TempDir,
    /// Every method name the stub was asked for, in order.
    seen: Arc<Mutex<Vec<String>>>,
}

impl StubDaemon {
    fn start(answers: Value, stream_frames: Vec<Value>) -> Self {
        let dir = tempfile::tempdir().expect("socket tempdir");
        let socket = dir.path().join("stub.sock");
        // `connect_hardened` refuses a socket whose containing directory is
        // wider than `0700`, and `tempfile` creates one at the process umask.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("harden the stub socket directory");
        let listener = UnixListener::bind(&socket).expect("bind the stub socket");
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
            .expect("harden the stub socket");
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let answers = answers.clone();
                let frames = stream_frames.clone();
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let request: Value = match serde_json::from_str(&line) {
                        Ok(request) => request,
                        Err(_) => return,
                    };
                    let method = request["method"].as_str().unwrap_or_default().to_string();
                    recorded
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(method.clone());
                    let id = request["id"].clone();

                    if request["stream"] == json!(true) {
                        for frame in frames {
                            let body = format!(
                                "{}\n",
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "stream": "item",
                                    "result": frame,
                                })
                            );
                            let _ = reader.get_mut().write_all(body.as_bytes()).await;
                        }
                        let _ = reader.get_mut().flush().await;
                        // No terminal frame: the stream is truncated, which is
                        // what every reconnect-behaviour case below needs.
                        return;
                    }

                    let result = answers.get(&method).cloned().unwrap_or_else(|| json!({}));
                    let body = format!(
                        "{}\n",
                        json!({"jsonrpc": "2.0", "id": id, "result": result})
                    );
                    let _ = reader.get_mut().write_all(body.as_bytes()).await;
                    let _ = reader.get_mut().flush().await;
                });
            }
        });
        Self {
            socket,
            _dir: dir,
            seen,
        }
    }

    fn engine(&self) -> CodeEngine {
        CodeEngine::with_socket(self.socket.clone(), None)
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The `session.create`/`workstream.list`/`task.run` answers every stub
/// scenario needs.
fn stub_answers(active: Option<&str>) -> Value {
    json!({
        "session.create": {"id": "sess-1", "task": "tcode tui session", "status": "running"},
        "task.run": {"session_id": "sess-1", "status": "running"},
        "session.cancel": {},
        "workstream.list": {
            "active_workstream_id": active,
            "workstreams": [{"id": "ws-1", "name": WS_NAME}],
        },
    })
}

/// Regression test for epic #3411's deferred Slice 3 review item: once
/// `pump_session_events` exhausts every reconnect attempt — the stream ending
/// with no terminal session event, over and over — `handle_input` must return
/// `Err` (not silently `Ok(true)`) AND the TUI must already have received a
/// VISIBLE `done: true, is_error: true` `AssistantOutput`, not just a trail of
/// `ConnectionLost`s that never clear `ReplApp::busy`.
///
/// The 60s outer timeout turns "hangs forever" into "fails deterministically":
/// this test's whole purpose is guarding against an un-exhaustible reconnect
/// loop, and an un-timed-out await would hang the test binary if one returned.
#[tokio::test]
async fn handle_input_surfaces_visible_error_after_exhausting_reconnects() {
    // No frames at all: every `session.events` request is answered with an
    // immediate hang-up, which is a truncated stream.
    let stub = StubDaemon::start(stub_answers(Some("ws-1")), Vec::new());
    let engine = stub.engine();
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx.clone()).await.expect("setup");

    let result = timeout(
        Duration::from_secs(60),
        engine.handle_input("hello".to_string(), tx),
    )
    .await
    .expect("handle_input must return rather than loop forever");
    assert!(
        result.is_err(),
        "exhausting the reconnect budget must surface as an error"
    );

    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            ReplEvent::AssistantOutput {
                done: true,
                is_error: true,
                ..
            }
        )),
        "the TUI must receive a visible terminal error, not just ConnectionLost: {events:?}"
    );
}

/// `subscribe_workstream_events` must open `workstream.events` and translate a
/// `WorkstreamActivationChanged` into `ReplEvent::WorkstreamActivationChanged`,
/// preserving the DOC-48 §5.3 wire field names (`new_active_id`/`prior_id`)
/// exactly.
#[tokio::test]
async fn subscribe_workstream_events_emits_activation_changed_with_wire_field_names() {
    let frame = json!({
        "session_id": "sess-1",
        "event_type": "workstream_activation_changed",
        "payload": {
            "type": "workstream_activation_changed",
            "new_active_id": "ws-2",
            "prior_id": "ws-1",
        },
    });
    let stub = StubDaemon::start(stub_answers(Some("ws-1")), vec![frame]);
    let engine = stub.engine();
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx.clone()).await.expect("setup");
    engine
        .subscribe_workstream_events(tx)
        .await
        .expect("subscribe");

    let observed = timeout(Duration::from_secs(20), async {
        loop {
            match rx.recv().await {
                Some(ReplEvent::WorkstreamActivationChanged {
                    new_active_id,
                    prior_id,
                }) => return (new_active_id, prior_id),
                Some(_) => continue,
                None => panic!("the event channel closed before the activation arrived"),
            }
        }
    })
    .await
    .expect("an activation change must be forwarded");

    assert_eq!(observed.0.as_deref(), Some("ws-2"));
    assert_eq!(observed.1.as_deref(), Some("ws-1"));
    assert!(
        stub.seen().iter().any(|m| m == "workstream.events"),
        "the subscription must call the stream method: {:?}",
        stub.seen()
    );
}

/// Regression test (HIGH finding, code-critic review of PR #3436): a
/// `WorkstreamActivationChanged` with `new_active_id: null` — this workstream
/// deactivated with no replacement (DOC-48 §4.2/§4.3) — must NOT be silently
/// dropped. The engine must refresh its cache and surface the structured
/// signal, so the status line clears rather than showing a stale workstream.
#[tokio::test]
async fn deactivation_with_no_replacement_refreshes_and_is_surfaced() {
    let frame = json!({
        "session_id": "sess-1",
        "event_type": "workstream_activation_changed",
        "payload": {
            "type": "workstream_activation_changed",
            "new_active_id": null,
            "prior_id": "ws-1",
        },
    });
    // The stub reports an active workstream at setup, which is what makes
    // `subscribe_workstream_events` open a stream at all — a client with no
    // known workstream has nothing to subscribe to (DOC-48 §5.3 is per-id).
    let stub = StubDaemon::start(stub_answers(Some("ws-1")), vec![frame]);
    let engine = stub.engine();
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx.clone()).await.expect("setup");
    engine
        .subscribe_workstream_events(tx)
        .await
        .expect("subscribe");

    let observed = timeout(Duration::from_secs(20), async {
        loop {
            match rx.recv().await {
                Some(ReplEvent::WorkstreamActivationChanged {
                    new_active_id,
                    prior_id,
                }) => return (new_active_id, prior_id),
                Some(_) => continue,
                None => panic!("the event channel closed before the deactivation arrived"),
            }
        }
    })
    .await
    .expect("a deactivation must be forwarded");

    assert_eq!(observed.0, None, "deactivation carries no new active id");
    assert_eq!(observed.1.as_deref(), Some("ws-1"));
    let calls = stub.seen();
    let refreshes = calls.iter().filter(|m| *m == "workstream.list").count();
    assert!(
        refreshes >= 2,
        "the cache must be refreshed beyond setup's own call: {calls:?}"
    );
}
