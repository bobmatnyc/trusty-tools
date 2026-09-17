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
use trusty_code_tui::{PermissionAnswer, ReplEvent, TuiEngine};

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

    /// (#8184) `tcode tui --delegate` — the PM opt-in.
    fn delegating_engine(&self) -> CodeEngine {
        CodeEngine::with_socket_delegating(self.socket.clone(), Some(self.project.clone()))
    }

    /// (#8184) `tcode tui` with no `--project`.
    fn projectless_engine(&self) -> CodeEngine {
        CodeEngine::with_socket(self.socket.clone(), None)
    }

    fn client(&self) -> UdsRpcClient {
        UdsRpcClient::new(self.socket.clone())
    }
}

/// The one session `setup` created, read back over the socket.
async fn only_session(daemon: &RealDaemon) -> Value {
    let listed = daemon
        .client()
        .call("session.list", json!({}))
        .await
        .expect("session.list over the socket");
    let sessions = listed["sessions"]
        .as_array()
        .expect("session.list returns an array")
        .clone();
    assert_eq!(sessions.len(), 1, "setup must have created one session");
    sessions[0].clone()
}

/// The startup splash `setup` published (#8164), joined into one string.
fn splash_text(rx: &mut UnboundedReceiver<ReplEvent>) -> String {
    drain(rx)
        .iter()
        .find_map(|event| match event {
            ReplEvent::SplashUpdated(lines) => Some(lines.join("\n")),
            _ => None,
        })
        .expect("setup must publish a startup splash")
}

/// The `connected to tcode daemon` line `setup` published.
fn connect_line(rx: &mut UnboundedReceiver<ReplEvent>) -> String {
    drain(rx)
        .iter()
        .find_map(|event| match event {
            ReplEvent::StatusMessage(text) if text.contains("connected to tcode daemon") => {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("setup must report the daemon it connected to")
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
    /// Every whole request the stub was asked for, in order (#3422) — the
    /// method name alone cannot prove a decision word crossed the socket.
    requests: Arc<Mutex<Vec<Value>>>,
}

impl StubDaemon {
    /// A stub that serves the TUI's prompter-claim stream (#8184).
    ///
    /// Why: `setup` now opens a `session.events` stream and REFUSES to return
    /// until a frame confirms the daemon took the claim, so every stub test
    /// needs that one frame — and it must not come out of `stream_frames`,
    /// which is what the TAIL under test is meant to see (the reconnect
    /// -exhaustion case, for one, depends on that tail being empty).
    /// What: a claim request is a `session.events` with NO `after_seq` key —
    /// `prompter_claim::open` omits it, `pump_session_events` always sends it
    /// — and gets one synthetic frame; every other stream, `workstream.events`
    /// included, gets `stream_frames` unchanged.
    fn start(answers: Value, stream_frames: Vec<Value>) -> Self {
        Self::start_inner(answers, stream_frames, true)
    }

    /// A stub that serves NOTHING on any stream, so the claim cannot confirm.
    fn start_refusing_the_claim(answers: Value) -> Self {
        Self::start_inner(answers, Vec::new(), false)
    }

    fn start_inner(answers: Value, stream_frames: Vec<Value>, serve_claim: bool) -> Self {
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
        let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded_requests = requests.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let answers = answers.clone();
                let frames = stream_frames.clone();
                let serve_claim = serve_claim;
                let recorded = recorded.clone();
                let recorded_requests = recorded_requests.clone();
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
                    recorded_requests
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(request.clone());
                    let id = request["id"].clone();

                    if request["stream"] == json!(true) {
                        // #8184: the prompter claim asks for a bare tail; the
                        // per-turn pump always carries `after_seq`.
                        let is_claim = method == "session.events"
                            && request["params"].get("after_seq").is_none();
                        let frames = if is_claim {
                            if serve_claim {
                                vec![json!({"session_id": "sess-1", "seq": 1})]
                            } else {
                                Vec::new()
                            }
                        } else {
                            frames
                        };
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
            requests,
        }
    }

    fn engine(&self) -> CodeEngine {
        CodeEngine::with_socket(self.socket.clone(), None)
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The whole request the stub received for `method`, if any (#3422).
    fn request_for(&self, method: &str) -> Option<Value> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|r| r["method"] == method)
            .cloned()
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

/// #3422: answering a prompt must become a real `session.permission.respond`
/// call carrying the daemon's own decision word, because only the daemon's
/// broker can release the suspended tool call (ADR-0063). A TUI that resolved
/// the prompt locally would clear its own screen while the call stayed parked.
#[tokio::test]
async fn respond_permission_releases_a_suspended_call() {
    let mut answers = stub_answers(Some("ws-1"));
    answers["session.permission.respond"] = json!({"accepted": true});
    let stub = StubDaemon::start(answers, Vec::new());
    let engine = stub.engine();
    let (tx, _rx) = unbounded_channel();
    engine.setup(tx).await.expect("setup");

    engine
        .respond_permission(
            "req-1".to_string(),
            PermissionAnswer::AllowForSession {
                pattern: Some("git *".to_string()),
            },
        )
        .await
        .expect("respond_permission");

    let request = stub
        .request_for("session.permission.respond")
        .expect("the answer must reach the daemon");
    let params = &request["params"];
    assert_eq!(params["session_id"], "sess-1", "{request}");
    assert_eq!(params["request_id"], "req-1", "{request}");
    assert_eq!(
        params["decision"], "allow_for_session",
        "the daemon's own wire word, not a TUI-local spelling: {request}"
    );
    assert_eq!(
        params["pattern"], "git *",
        "an explicit grant width must cross verbatim: {request}"
    );
}

/// #3422: before `setup` there is no session, so no request can exist to
/// answer — a no-op, not an error, so a stray key press during startup cannot
/// surface as a failure the user has to read.
#[tokio::test]
async fn respond_permission_without_a_session_is_a_noop() {
    let stub = StubDaemon::start(stub_answers(Some("ws-1")), Vec::new());
    let engine = stub.engine();

    engine
        .respond_permission("req-1".to_string(), PermissionAnswer::Deny)
        .await
        .expect("must not error without a session");

    assert!(
        stub.request_for("session.permission.respond").is_none(),
        "no session means nothing to answer: {:?}",
        stub.seen()
    );
}

/// #8184: `setup` claims a prompter BEFORE any run can issue a gated call.
///
/// Why: the daemon suspends an `ask` only while someone is watching that
/// session (#8100), and on this transport the only thing that claims a
/// prompter is an open `session.events` stream. `run_chat_turn` issued
/// `task.run` first and opened its stream afterwards, so the interactive
/// default's first `write_file`/`bash` — gated since #8184 — raced the stream
/// open and a call that lost was DENIED with no prompt to answer. Ordering the
/// claim into `setup` makes the race unreachable for every later turn.
/// What: drives `setup` against the stub and asserts the claim stream was
/// opened, right after the session was created and before anything else. FAILS
/// before this change: `setup` requested `session.create` and never a
/// `session.events`, so nothing held a claim until mid-turn.
/// Test: this test.
#[tokio::test]
async fn setup_opens_a_prompter_claim_stream() {
    let stub = StubDaemon::start(stub_answers(Some("ws-1")), vec![json!({"seq": 1})]);
    let engine = stub.engine();
    let (tx, _rx) = unbounded_channel();

    engine.setup(tx).await.expect("setup against the stub");

    let seen = stub.seen();
    assert_eq!(
        seen.first().map(String::as_str),
        Some("session.create"),
        "the session must exist before anything can watch it: {seen:?}"
    );
    let claim = seen
        .iter()
        .position(|m| m == "session.events")
        .unwrap_or_else(|| panic!("setup must open the prompter-claim stream: {seen:?}"));
    assert_eq!(
        claim, 1,
        "the claim must be the FIRST thing after the session is created, so no \
         run can precede it: {seen:?}"
    );
    assert!(
        !seen.contains(&"task.run".to_string()),
        "setup must not run anything: {seen:?}"
    );
}

/// #8184: a claim stream the daemon does not serve must FAIL `setup`.
///
/// Why: `open_stream` returns once the request is written, so a successful
/// dial proves nothing about the daemon having run the handler that takes the
/// claim — and a refusal (`session_not_found`) arrives as a terminal frame on
/// the stream, never as a dial error. A `setup` that ignored the first frame
/// would report success over a session nothing is watching, and every gated
/// call in it would then be denied with no prompt: the precise failure the
/// claim exists to prevent, reintroduced silently.
/// What: a stub that serves the session but closes the stream with no frame.
/// Asserts `setup` errors. FAILS while the first frame is discarded: `setup`
/// returned `Ok`.
/// Test: this test.
#[tokio::test]
async fn setup_fails_when_the_claim_stream_is_refused() {
    let stub = StubDaemon::start_refusing_the_claim(stub_answers(None));
    let engine = stub.engine();
    let (tx, _rx) = unbounded_channel();

    let err = engine
        .setup(tx)
        .await
        .expect_err("an unwatchable session must not report a successful setup");

    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("closed"),
        "the failure must name the stream that never opened: {rendered}"
    );
    assert!(
        stub.seen().contains(&"session.events".to_string()),
        "the claim must have been attempted: {:?}",
        stub.seen()
    );
}

// ── #8184: the interactive session's default agent shape ────────────────────

/// #8184: a plain `tcode tui` session runs the SOLO agent, against the bound
/// project root — end to end, against a real daemon.
///
/// Why: this is the wiring the unit tests cannot see — that the engine's
/// `session.create` really produces the solo shape on the daemon, and that the
/// connect line the user reads says so and names where edits will land.
/// What: `setup` against a project-bound daemon, then reads the session back
/// over the socket.
/// Test: this test.
#[tokio::test]
async fn tui_default_session_runs_the_solo_agent() {
    let daemon = RealDaemon::start().await;
    let engine = daemon.engine();
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx).await.expect("setup over the socket");

    let session = only_session(&daemon).await;
    assert_eq!(
        session["no_delegate"],
        json!(true),
        "a default TUI session runs the solo agent: {session}"
    );
    assert_eq!(
        session["binding"]["root"],
        json!(daemon.project.display().to_string()),
        "the session must be scoped to the bound project root: {session}"
    );

    let line = connect_line(&mut rx);
    assert!(
        line.contains("solo agent (no delegation)"),
        "the connect line must state the agent shape: {line}"
    );
    assert!(
        line.contains(&daemon.project.display().to_string()),
        "the connect line must name the working root: {line}"
    );
}

/// #8184: `tcode tui --delegate` still gets the delegating PM.
///
/// Why: the opt-in has to survive the same hop, or PM mode is unreachable from
/// the TUI and the default change becomes a removal.
/// What: the same flow through `CodeEngine::with_socket_delegating`.
/// Test: this test.
#[tokio::test]
async fn tui_delegate_opt_in_creates_a_delegating_session() {
    let daemon = RealDaemon::start().await;
    let engine = daemon.delegating_engine();
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx).await.expect("setup over the socket");

    let session = only_session(&daemon).await;
    assert_eq!(
        session["no_delegate"],
        json!(false),
        "--delegate must mint the PM shape: {session}"
    );
    let line = connect_line(&mut rx);
    assert!(
        line.contains("delegating PM"),
        "the connect line must state the agent shape: {line}"
    );
}

/// #8184: `tcode tui` with no `--project` still succeeds, still runs solo, and
/// says its edits land in a scratch workspace.
///
/// Why: a projectless session is a first-class state, and its run works in the
/// executor's ephemeral scratch root — never the launch directory. A user who
/// is not told that would read an "edited the file" report as a claim about
/// their own tree.
/// What: `setup` with `project_path: None`, asserting the projectless binding,
/// the solo shape, and the connect line's wording.
/// Test: this test.
#[tokio::test]
async fn tui_projectless_default_session_runs_solo_in_a_scratch_root() {
    let daemon = RealDaemon::start().await;
    let engine = daemon.projectless_engine();
    let (tx, mut rx) = unbounded_channel();
    engine
        .setup(tx)
        .await
        .expect("a projectless setup must succeed");

    let session = only_session(&daemon).await;
    assert_eq!(session["binding"]["state"], json!("projectless"));
    assert_eq!(
        session["no_delegate"],
        json!(true),
        "a projectless TUI session runs solo too: {session}"
    );
    let line = connect_line(&mut rx);
    assert!(
        line.contains("projectless — file tools rooted at a scratch workspace"),
        "the connect line must say where a projectless session edits: {line}"
    );
}

// ── #8164: the startup splash ───────────────────────────────────────────────

/// #8164: `setup` publishes a splash naming the client build, the DAEMON's
/// build, the session, the project and the agent shape.
///
/// Why: the splash's text assembly is unit-tested, but nothing there proves
/// the facts are actually fetched — in particular that the daemon's `build`
/// field crosses the socket. This daemon runs in THIS process, so its build
/// is by construction the client's, which is also what makes the
/// "no mismatch warning" assertion meaningful rather than incidental.
/// What: `setup` against a project-bound daemon; asserts the splash's header,
/// the daemon build line, the session id, the bound root, and the absence of
/// the mismatch warning.
/// Test: this test.
#[tokio::test]
async fn setup_publishes_a_splash_naming_the_project() {
    let daemon = RealDaemon::start().await;
    let engine = daemon.engine();
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx).await.expect("setup over the socket");

    let session = only_session(&daemon).await;
    let session_id = session["id"].as_str().expect("a session carries an id");
    let splash = splash_text(&mut rx);

    assert!(
        splash.starts_with("🤖🤖🤖 tcode v"),
        "the splash must lead with the robot header and the BINARY version: {splash}"
    );
    assert!(
        splash.contains(&format!("({})", trusty_code::build_info::GIT_HASH)),
        "the daemon must report its build sha over `health`: {splash}"
    );
    assert!(
        splash.contains(&format!("session {session_id}")),
        "the splash must name the session: {splash}"
    );
    assert!(
        splash.contains(&format!("project {}", daemon.project.display())),
        "the splash must name the bound project: {splash}"
    );
    assert!(
        splash.contains("agent solo agent (no delegation)"),
        "the splash must name the delegation shape: {splash}"
    );
    assert!(
        !splash.contains("warning:"),
        "client and daemon are the same build here, so nothing must warn: {splash}"
    );
}

/// #8164: an unbound session says "projectless" on the splash.
///
/// Why: #8205's transcript failed precisely because an unbound session looked
/// identical to a bound one at launch. The word has to be on screen.
/// What: `setup` with `project_path: None`.
/// Test: this test.
#[tokio::test]
async fn setup_splash_says_projectless_when_unbound() {
    let daemon = RealDaemon::start().await;
    let engine = daemon.projectless_engine();
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx).await.expect("setup over the socket");

    let splash = splash_text(&mut rx);
    assert!(
        splash.contains("project projectless"),
        "an unbound session must say so: {splash}"
    );
}
