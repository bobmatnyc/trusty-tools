//! `POST /api/console/assistants/{id}/ask` through the real router (#11748).
//!
//! Why: the ask route is the one console route that runs a model, served with
//! no login; every failure path must be a refusal, never a `200`.
//! What: each case seeds a temp assistants root and a temp `0700` data
//! directory, and — where a daemon is meant to run — a fake ask daemon on
//! `trusty-agents-<id>.sock` that speaks the one-line protocol and records
//! what it saw. No real `ta` daemon runs. Requests go through the production
//! loopback guard (`guard_loopback_listener`); the ungated case serves the
//! bare router on `127.0.0.1:0` with neither guard.
//! Test: this file IS the test.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, PoisonError};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tower::ServiceExt;
use trusty_console::assistants_uds::ask::AskLimits;
use trusty_console::server::{AppState, build_router, guard_loopback_listener};

/// Stands for the listener's own origin, `http://<bound addr>`; the port is
/// only known once [`post_ask`] binds.
const ORIGIN: &str = "self";

// ─── log capture ─────────────────────────────────────────────────────────────

static LOGS: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct LogSink;

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LOGS.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Install one global INFO subscriber for this test binary.
fn capture_logs() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(|| LogSink)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

fn logged() -> String {
    String::from_utf8_lossy(&LOGS.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
}

// ─── fixtures ────────────────────────────────────────────────────────────────

/// One case's assistants root and data directory.
struct Fixture {
    _root: tempfile::TempDir,
    _data: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
}

impl Fixture {
    /// A root holding instance homes `ids`, and an empty `0700` data dir.
    fn new(ids: &[&str]) -> Self {
        let root_dir = tempfile::tempdir().expect("tempdir");
        let data_dir = tempfile::tempdir().expect("tempdir");
        for id in ids {
            let home = root_dir.path().join(id);
            std::fs::create_dir_all(&home).expect("mkdir");
            std::fs::write(home.join("instructions.md"), "be helpful\n").expect("write");
        }
        std::fs::set_permissions(data_dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        Self {
            root: root_dir.path().to_path_buf(),
            data: data_dir.path().to_path_buf(),
            _root: root_dir,
            _data: data_dir,
        }
    }

    fn socket(&self, id: &str) -> PathBuf {
        self.data.join(format!("trusty-agents-{id}.sock"))
    }

    fn state(&self) -> AppState {
        AppState::new(vec![])
            .with_assistants_root(self.root.clone())
            .with_assistants_data_dir(self.data.clone())
    }
}

/// What the fake daemon saw on its one connection.
#[derive(Debug)]
struct Seen {
    request: Value,
    /// The client closed its write half before the reply was sent.
    eof_before_reply: bool,
}

/// Bind a `0600` socket at `path` and serve one connection: read the request
/// line, check for an early EOF, wait `delay`, then write `reply` verbatim.
fn fake_daemon(path: &Path, delay: Duration, reply: Vec<u8>) -> JoinHandle<Seen> {
    let listener = UnixListener::bind(path).expect("bind fake daemon");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await.expect("read request");
        let mut probe = [0u8; 1];
        // #11748: the daemon cancels on EOF, so the client must not send one.
        let eof_before_reply = matches!(
            tokio::time::timeout(Duration::from_millis(150), read.read(&mut probe)).await,
            Ok(Ok(0))
        );
        tokio::time::sleep(delay).await;
        let _ = write.write_all(&reply).await;
        let _ = write.flush().await;
        Seen {
            request: serde_json::from_str(&line).expect("request is JSON"),
            eof_before_reply,
        }
    })
}

fn reply_line(value: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).expect("json");
    line.push(b'\n');
    line
}

fn answer(text: &str) -> Vec<u8> {
    reply_line(&json!({"protocol": 1, "daemon_pid": 4242, "outcome": "answer", "answer": text}))
}

/// POST `body` to `/api/console/assistants/{id}/ask` on the loopback listener.
async fn post_ask(
    state: AppState,
    id: &str,
    body: &str,
    origin: Option<&str>,
) -> (StatusCode, Value) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let host = listener.local_addr().expect("addr").to_string();
    let router = guard_loopback_listener(build_router(state), &listener).expect("loopback guard");
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/api/console/assistants/{id}/ask"))
        .header("host", host.clone())
        .header("content-type", "application/json");
    if let Some(origin) = origin {
        let origin = if origin == ORIGIN {
            format!("http://{host}")
        } else {
            origin.to_owned()
        };
        req = req.header("origin", origin);
    }
    let resp = router
        .oneshot(req.body(Body::from(body.to_owned())).expect("request"))
        .await
        .expect("router response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "{status}: not JSON ({e}): {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, value)
}

fn question(text: &str) -> String {
    json!({ "question": text }).to_string()
}

// ─── cases ───────────────────────────────────────────────────────────────────

/// AC1, and the write half stays open until the reply (item 1).
#[tokio::test]
async fn ask_returns_the_answer_and_holds_the_write_half_open() {
    let fx = Fixture::new(&["izzie"]);
    let daemon = fake_daemon(&fx.socket("izzie"), Duration::ZERO, answer("forty-two"));
    let (status, body) =
        post_ask(fx.state(), "izzie", &question("what is it?"), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "instance": "izzie", "answer": "forty-two" }));
    let seen = daemon.await.expect("daemon task");
    assert_eq!(
        seen.request,
        json!({ "protocol": 1, "question": "what is it?" })
    );
    assert!(
        !seen.eof_before_reply,
        "the console closed its write half before the reply"
    );
}

#[tokio::test]
async fn ask_with_a_daemon_error_is_502_with_its_kind() {
    let fx = Fixture::new(&["izzie"]);
    let reply = reply_line(&json!({
        "protocol": 1, "daemon_pid": 1, "outcome": "error", "kind": "ask",
        "message": "model call failed reading /Users/someone/.secret/key"
    }));
    let _daemon = fake_daemon(&fx.socket("izzie"), Duration::ZERO, reply);
    let (status, body) = post_ask(fx.state(), "izzie", &question("q"), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["kind"], "ask");
    assert!(
        !body.to_string().contains("/Users/"),
        "a path leaked: {body}"
    );
}

/// AC2: no socket → 503 with the `ta ask-daemon` hint; nothing is started.
#[tokio::test]
async fn ask_with_no_socket_is_503_with_the_hint() {
    let fx = Fixture::new(&["izzie"]);
    let (status, body) = post_ask(fx.state(), "izzie", &question("q"), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "assistant not running");
    // #11748 review: `ta ask` lets its daemon exit when idle; a hand-run
    // `ta ask-daemon` stays, so the hint names that verb.
    assert_eq!(
        body["hint"],
        "run `ta ask-daemon izzie` in a terminal and leave it running; the console never starts an assistant"
    );
    assert!(
        !fx.socket("izzie").exists(),
        "the console must never start a daemon"
    );
}

/// AC2: a socket file nobody listens on → 503.
#[tokio::test]
async fn ask_with_a_stale_socket_is_503() {
    let fx = Fixture::new(&["izzie"]);
    let path = fx.socket("izzie");
    drop(UnixListener::bind(&path).expect("bind"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(path.exists(), "the stale socket file stays behind");
    let (status, body) = post_ask(fx.state(), "izzie", &question("q"), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["hint"]
            .as_str()
            .is_some_and(|h| h.contains("`ta ask-daemon izzie`"))
    );
}

#[tokio::test]
async fn ask_to_a_slow_daemon_is_504() {
    let fx = Fixture::new(&["izzie"]);
    let _daemon = fake_daemon(&fx.socket("izzie"), Duration::from_secs(30), answer("late"));
    let limits = AskLimits {
        ask_timeout: Duration::from_millis(400),
        ..AskLimits::default()
    };
    let started = std::time::Instant::now();
    let (status, body) = post_ask(
        fx.state().with_assistants_ask_limits(limits),
        "izzie",
        &question("q"),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert_eq!(body["kind"], "timeout");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn ask_with_an_oversize_question_is_413() {
    let fx = Fixture::new(&["izzie"]);
    let big = "x".repeat(trusty_console::assistants_uds::ask_route::MAX_QUESTION_BYTES + 1);
    let (status, body) = post_ask(fx.state(), "izzie", &question(&big), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    let huge = "x".repeat(trusty_console::assistants_uds::ask_route::MAX_ASK_BODY_BYTES + 1);
    let (status, body) = post_ask(fx.state(), "izzie", &question(&huge), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
}

#[tokio::test]
async fn ask_with_a_blank_question_is_400() {
    let fx = Fixture::new(&["izzie"]);
    for body in [
        question("   "),
        "{\"q\":1}".to_owned(),
        "not json".to_owned(),
    ] {
        let (status, got) = post_ask(fx.state(), "izzie", &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {got}");
    }
}

#[tokio::test]
async fn ask_with_an_oversize_reply_is_502() {
    let fx = Fixture::new(&["izzie"]);
    let _daemon = fake_daemon(
        &fx.socket("izzie"),
        Duration::ZERO,
        answer(&"y".repeat(4096)),
    );
    let limits = AskLimits {
        max_reply_bytes: 1024,
        ..AskLimits::default()
    };
    let (status, body) = post_ask(
        fx.state().with_assistants_ask_limits(limits),
        "izzie",
        &question("q"),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["kind"], "reply_too_large");
}

#[tokio::test]
async fn ask_with_a_hangup_or_garbage_reply_is_502() {
    for (reply, kind) in [
        (Vec::new(), "disconnected"),
        (b"not json\n".to_vec(), "bad_reply"),
        (
            reply_line(
                &json!({"protocol": 2, "daemon_pid": 1, "outcome": "answer", "answer": "a"}),
            ),
            "protocol_mismatch",
        ),
    ] {
        let fx = Fixture::new(&["izzie"]);
        let _daemon = fake_daemon(&fx.socket("izzie"), Duration::ZERO, reply);
        let (status, body) = post_ask(fx.state(), "izzie", &question("q"), Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{kind}: {body}");
        assert_eq!(body["kind"], kind);
    }
}

/// A socket that fails `ta`'s trust check (world-writable) is never dialled.
#[tokio::test]
async fn ask_to_an_untrusted_socket_is_502_without_a_path() {
    let fx = Fixture::new(&["izzie"]);
    let path = fx.socket("izzie");
    let _listener = UnixListener::bind(&path).expect("bind");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    let (status, body) = post_ask(fx.state(), "izzie", &question("q"), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["kind"], "unreachable");
    assert!(
        !body.to_string().contains(&*fx.data.to_string_lossy()),
        "a path leaked: {body}"
    );
}

#[tokio::test]
async fn ask_for_an_unknown_id_is_404() {
    let fx = Fixture::new(&["izzie", "bob"]);
    let (status, body) = post_ask(fx.state(), "nobody", &question("q"), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["available"], json!(["bob", "izzie"]));
}

/// A directory without `instructions.md` is not an instance, even with a socket.
#[tokio::test]
async fn ask_for_an_unlisted_directory_is_404() {
    let fx = Fixture::new(&[]);
    std::fs::create_dir_all(fx.root.join("hollow")).expect("mkdir");
    let _daemon = fake_daemon(&fx.socket("hollow"), Duration::ZERO, answer("no"));
    let (status, body) = post_ask(fx.state(), "hollow", &question("q"), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn ask_with_an_invalid_id_is_400_before_the_filesystem() {
    // The root does not exist: a 400 proves validation ran first.
    let fx = Fixture::new(&[]);
    let state = AppState::new(vec![])
        .with_assistants_root(fx.root.join("absent"))
        .with_assistants_data_dir(fx.data.join("absent"));
    for id in ["..", "UPPER", "a%2Fb", "-"] {
        let (status, body) = post_ask(state.clone(), id, &question("q"), Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{id}: {body}");
    }
}

/// AC3: a foreign Origin never reaches the socket.
#[tokio::test]
async fn ask_with_a_foreign_origin_is_403() {
    let fx = Fixture::new(&["izzie"]);
    let daemon = fake_daemon(&fx.socket("izzie"), Duration::ZERO, answer("leak"));
    let (status, _) = post_ask(
        fx.state(),
        "izzie",
        &question("q"),
        Some("http://evil.example"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        !daemon.is_finished(),
        "the daemon must not have been dialled"
    );
    daemon.abort();
}

#[tokio::test]
async fn ask_with_no_origin_is_403() {
    let fx = Fixture::new(&["izzie"]);
    let daemon = fake_daemon(&fx.socket("izzie"), Duration::ZERO, answer("leak"));
    let (status, body) = post_ask(fx.state(), "izzie", &question("q"), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !daemon.is_finished(),
        "the daemon must not have been dialled"
    );
    daemon.abort();
}

/// AC4 / item 5: the bare router served with neither the loopback guard nor
/// the peer gate — so no `AdmittedBy` marker — refuses all three assistants
/// routes. Bound on `127.0.0.1:0`, the only address a test may bind; the
/// refusal comes from the missing marker, not the address.
#[tokio::test]
async fn ungated_listener_cannot_reach_assistants() {
    let fx = Fixture::new(&["izzie"]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let app = build_router(fx.state());
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    for (method, path) in [
        ("GET", "/api/console/assistants"),
        ("GET", "/api/console/assistants/izzie/turns"),
        ("POST", "/api/console/assistants/izzie/ask"),
    ] {
        let req = client
            .request(method.parse().expect("method"), format!("{base}{path}"))
            .header("origin", base.clone())
            .body(question("q"));
        let status = req.send().await.expect("send").status();
        assert_eq!(
            status.as_u16(),
            403,
            "{method} {path} on an ungated listener"
        );
    }
    server.abort();
}

/// Item 7: one line per ask — instance, peer, length, outcome — and never the
/// question or the answer.
#[tokio::test]
async fn ask_log_names_the_peer_and_never_the_text() {
    capture_logs();
    let fx = Fixture::new(&["logcase"]);
    let secret_q = "QUESTION-TEXT-7f3a";
    let secret_a = "ANSWER-TEXT-91bc";
    let _daemon = fake_daemon(&fx.socket("logcase"), Duration::ZERO, answer(secret_a));
    let (status, _) = post_ask(fx.state(), "logcase", &question(secret_q), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::OK);
    let logs = logged();
    let line = logs
        .lines()
        .find(|l| l.contains("assistants: ask") && l.contains("instance=\"logcase\""))
        .unwrap_or_else(|| panic!("no ask line in:\n{logs}"));
    for field in [
        "listener=\"loopback\"",
        "peer_login=loopback",
        "question_bytes=18",
        "outcome=\"answer\"",
        "status=200",
        "duration_ms=",
    ] {
        assert!(line.contains(field), "`{field}` missing from: {line}");
    }
    assert!(
        !logs.contains(secret_q),
        "the question text reached the log"
    );
    assert!(!logs.contains(secret_a), "the answer text reached the log");
}

/// A fake daemon that accepts every connection and never replies.
fn silent_daemon(path: &Path) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("bind fake daemon");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    })
}

/// #11748 review: N+1 concurrent asks against a slow daemon get exactly one
/// `429`, and the permits come back once the asks time out.
#[tokio::test]
async fn concurrent_asks_past_the_cap_are_429() {
    const CAP: usize = 2;
    let fx = Fixture::new(&["izzie"]);
    let _daemon = silent_daemon(&fx.socket("izzie"));
    let state = fx
        .state()
        .with_assistants_max_concurrent_asks(CAP)
        .with_assistants_ask_limits(AskLimits {
            ask_timeout: Duration::from_millis(600),
            ..AskLimits::default()
        });
    let mut in_flight = Vec::new();
    for _ in 0..CAP {
        let state = state.clone();
        in_flight.push(tokio::spawn(async move {
            post_ask(state, "izzie", &question("q"), Some(ORIGIN)).await
        }));
    }
    // Let the first CAP asks take their permits and reach the daemon.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (status, body) = post_ask(state.clone(), "izzie", &question("q"), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body, json!({ "error": "assistants busy", "kind": "busy" }));
    for task in in_flight {
        let (status, body) = task.await.expect("ask task");
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    }
    // The timed-out asks released their permits: the next ask is not busy.
    let (status, body) = post_ask(state, "izzie", &question("q"), Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
}
