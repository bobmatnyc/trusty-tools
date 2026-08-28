//! Tests for the daemon's Unix-socket listener (#6285 slice 1).
//!
//! Why a separate file: `socket.rs` is production source under the 500-SLOC
//! cap, and these tests drive the REAL bind / accept / unlink path rather than
//! a hand-rolled listener — which is what makes them able to fail if that path
//! regresses.
//!
//! Test: this file IS the test module for `super`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{bind, serve_until_shutdown, socket_path, BoundSocket, METHODS, METHOD_HEALTH};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::server::SearchAppState;

/// How long a dial or a bind is given before the test calls it stuck.
///
/// A local socket answers in microseconds; this is headroom on a loaded CI
/// machine, not a latency budget.
const GENEROUS: Duration = Duration::from_secs(10);

/// A state carrying `count` registered indexes and nothing else.
///
/// Enough for the health method, which reports the registry's size. Nothing
/// here touches disk: `CodeIndexer::new` builds an empty in-memory indexer, so
/// no corpus is opened and no allowlist gate is involved.
fn state_with_indexes(count: usize) -> Arc<SearchAppState> {
    let registry = IndexRegistry::new();
    for i in 0..count {
        let id = IndexId::new(format!("socket-test-{i}"));
        let root = format!("/nonexistent/socket-test-{i}");
        registry.register(IndexHandle::bare(
            id.clone(),
            Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(
                id.0.as_str(),
                &root,
            ))),
            root.into(),
        ));
    }
    Arc::new(SearchAppState::new(registry))
}

/// Bind `socket` and serve it on a background task, returning its stop trigger.
///
/// The shutdown future is a parameter of [`serve_until_shutdown`] for exactly
/// this reason: a test cannot deliver SIGTERM to its own process without
/// affecting the whole test binary.
async fn spawn_listener(
    socket: &Path,
    state: Arc<SearchAppState>,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let bound = bind(socket).await.expect("bind a fresh socket path");
    serve_bound(bound, socket, state).await
}

/// Serve an already-bound socket, returning its stop trigger.
///
/// Split from [`spawn_listener`] so a test that needs to control HOW the bind
/// happened — `bind_reclaims_a_stale_socket_file` retries it — still drives the
/// same serve path.
async fn serve_bound(
    bound: BoundSocket,
    socket: &Path,
    state: Arc<SearchAppState>,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        serve_until_shutdown(bound, state, async {
            let _ = stop_rx.await;
        })
        .await;
    });

    // Wait for the accept loop rather than sleeping: a socket that never
    // answers fails the dial below, which reports better than a short sleep.
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (stop_tx, handle)
}

/// What one raw dial observed: the response frame, and whether the server then
/// closed the connection without sending anything more.
struct RawExchange {
    /// The single response frame, parsed.
    frame: serde_json::Value,
    /// Bytes the server sent after that frame. Empty means a clean close.
    trailing: Vec<u8>,
}

/// Dial `socket`, write one request frame, and read what comes back.
///
/// Why raw rather than `trusty_common::uds::send_framed_request`: that helper
/// answers "did a frame arrive". The assertion this slice owes is stronger —
/// one frame arrives AND the server then closes, rather than hanging or hanging
/// up mid-frame. Only a raw stream can tell those three apart.
async fn dial_once(socket: &Path, request: &serde_json::Value) -> RawExchange {
    let stream = tokio::time::timeout(GENEROUS, UnixStream::connect(socket))
        .await
        .expect("connect must not hang")
        .expect("a live socket must accept a connection");
    let mut reader = BufReader::new(stream);

    let mut body = serde_json::to_vec(request).expect("encode the request");
    body.push(b'\n');
    reader
        .get_mut()
        .write_all(&body)
        .await
        .expect("write the request frame");

    let mut line = String::new();
    let read = tokio::time::timeout(GENEROUS, reader.read_line(&mut line))
        .await
        .expect("the server must answer rather than hang")
        .expect("read the response frame");
    assert!(read > 0, "the server closed without writing a frame at all");

    let mut trailing = Vec::new();
    tokio::time::timeout(GENEROUS, reader.read_to_end(&mut trailing))
        .await
        .expect("the server must close rather than hold the connection open")
        .expect("read to EOF");

    RawExchange {
        frame: serde_json::from_str(&line).expect("the answer must be one JSON frame"),
        trailing,
    }
}

/// Why: [`METHODS`] is what a consumer contract test compares against once the
/// retire slice moves the eleven dialling crates onto these names. An array
/// that drifts from the registrations is worse than no array — it would let a
/// rename pass review and surface as `method_not_found` in a crate with no
/// Cargo edge on this one.
/// Test: this function IS the test.
#[test]
fn rpc_router_registers_every_documented_method() {
    let router = super::build_router(&state_with_indexes(0));
    let mut registered: Vec<&str> = router.method_names().collect();
    registered.sort_unstable();
    let mut documented: Vec<&str> = METHODS.to_vec();
    documented.sort_unstable();
    assert_eq!(
        registered, documented,
        "METHODS must list exactly what build_router registers"
    );
}

/// Why: an unregistered name could plausibly fail three ways a client cannot
/// tell apart from a healthy "not yet implemented" — the server could hang, it
/// could accept and hang up without a frame, or it could answer a malformed
/// frame. This asserts the one correct shape and rules out the other three: a
/// well-formed JSON-RPC error frame carrying `-32601`, the request's id echoed
/// back, and a clean close with no trailing bytes. Every later slice inherits
/// that contract for the names it has not claimed yet.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_reports_method_not_found_for_an_unknown_method() {
    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-search.sock");
    let (stop, handle) = spawn_listener(&socket, state_with_indexes(0)).await;

    let exchange = dial_once(
        &socket,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "search.no.such.method", "params": {},
        }),
    )
    .await;

    assert_eq!(exchange.frame["jsonrpc"], "2.0");
    assert_eq!(exchange.frame["id"], 7);
    assert!(
        exchange.frame.get("result").is_none(),
        "an unregistered method must not answer a result: {}",
        exchange.frame
    );
    assert_eq!(
        exchange.frame["error"]["code"],
        serde_json::json!(trusty_common::uds::server::CODE_METHOD_NOT_FOUND),
        "unknown methods answer method_not_found: {}",
        exchange.frame
    );
    assert!(
        exchange.trailing.is_empty(),
        "the server must close cleanly after one frame, not send {} more bytes",
        exchange.trailing.len()
    );

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");
}

/// Why: `params` is absent on a well-formed call to a no-argument method, and a
/// plain unit struct refuses `null` — every health probe would answer
/// `invalid_params`. [`super::NoParams`] is what stops that, and this is the
/// test that fails if it is replaced with a derived `Deserialize`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_health_answers_with_no_params() {
    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-search.sock");
    let (stop, handle) = spawn_listener(&socket, state_with_indexes(2)).await;

    let exchange = dial_once(
        &socket,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": METHOD_HEALTH }),
    )
    .await;

    assert_eq!(
        exchange.frame["result"]["indexes"],
        serde_json::json!(2),
        "health must count the registry it was handed: {}",
        exchange.frame
    );
    assert_eq!(
        exchange.frame["result"]["version"],
        serde_json::json!(env!("CARGO_PKG_VERSION")),
        "health must report this binary's version: {}",
        exchange.frame
    );

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");
}

/// Why: a caller that sends a stray field is not refused — this method has no
/// arguments to get wrong, and refusing would turn an additive client change
/// into an outage. Pinned so a later tightening of [`super::NoParams`] is a
/// deliberate decision rather than a side effect.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_health_answers_with_a_stray_params_object() {
    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-search.sock");
    let (stop, handle) = spawn_listener(&socket, state_with_indexes(1)).await;

    let exchange = dial_once(
        &socket,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": METHOD_HEALTH,
            "params": { "unrecognised": true },
        }),
    )
    .await;

    assert!(
        exchange.frame.get("error").is_none(),
        "a stray params field must not be refused: {}",
        exchange.frame
    );
    assert_eq!(exchange.frame["result"]["indexes"], serde_json::json!(1));

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");
}

/// Why (#6285): the socket and `GET /health` are two doors onto one daemon, and
/// the failure this rules out is the two answering differently — a consumer
/// migrated to the socket seeing a different index count, status, or version
/// than the HTTP probe it replaced. Both go through
/// `service::server::health_report`, and this asserts the socket's frame
/// carries exactly that body rather than a re-derived one.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn health_over_the_socket_matches_the_http_body() {
    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-search.sock");
    let state = state_with_indexes(3);
    let (stop, handle) = spawn_listener(&socket, Arc::clone(&state)).await;

    let exchange = dial_once(
        &socket,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 9, "method": METHOD_HEALTH }),
    )
    .await;

    // The same call the axum route makes, against the same state.
    let mut direct = crate::service::server::health_report(state).await;
    let mut over_socket = exchange.frame["result"].clone();
    // Three fields are sampled from the host at answer time rather than derived
    // from state, so two reads microseconds apart legitimately differ: wall-clock
    // uptime, process RSS, and process CPU. Excluding host-sampled values is what
    // #6358 established for the doctor parity test, for the same reason. Every
    // other field is derived and must match byte for byte.
    const HOST_SAMPLED: [&str; 3] = ["uptime_secs", "rss_mb", "cpu_pct"];
    for body in [&mut direct, &mut over_socket] {
        if let Some(obj) = body.as_object_mut() {
            for field in HOST_SAMPLED {
                obj.remove(field);
            }
        }
    }
    assert_eq!(
        over_socket, direct,
        "the socket must serve the HTTP health body, not a re-derived one"
    );

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");
}

/// Why: `bind_hardened` binds and chmods; neither it nor `UnixListener`'s
/// `Drop` removes the path, so a listener that just returned would leave a file
/// behind. This drives the real shutdown path and asserts the file is gone — a
/// test that deleted the file itself would pass whether or not the unlink in
/// `serve_until_shutdown` exists.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn serve_unlinks_its_socket_on_shutdown() {
    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-search.sock");
    let (stop, handle) = spawn_listener(&socket, state_with_indexes(0)).await;
    assert!(socket.exists(), "the listener must have bound its socket");

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");

    assert!(
        !socket.exists(),
        "the socket file must be unlinked on clean shutdown: {}",
        socket.display()
    );
}

/// The Fail-Open Check.
///
/// Why: the failure this rules out is a daemon that shrugs off a UDS bind
/// failure and serves HTTP only. [`bind`] returns `Result`, not `Option`, and
/// `run_daemon` propagates it before it writes the port file — so the
/// observable proof is that a bind against a socket someone else is serving is
/// an `Err` whose message names the path, rather than a value the caller could
/// mistake for "no socket, carry on".
/// What: binds and serves one socket, then binds the SAME path again while the
/// first is live.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn bind_refuses_a_socket_another_process_is_serving() {
    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-search.sock");
    let (stop, handle) = spawn_listener(&socket, state_with_indexes(0)).await;

    let err = bind(&socket)
        .await
        .expect_err("a second bind against a live socket must fail, never degrade");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains(&socket.display().to_string()),
        "the error must name the path an operator has to act on: {rendered}"
    );

    // The refusal must not have disturbed the live owner.
    assert!(
        trusty_common::uds::socket_is_serving(&socket, GENEROUS).await,
        "the incumbent must still be serving after the refused bind"
    );

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");
}

/// Why: launchd SIGKILLs a daemon at the `ExitTimeOut` boundary, and that
/// daemon never reaches the unlink in `serve_until_shutdown`. If the successor
/// refused the leftover file, every such stop would wedge the restart into a
/// crash loop. `bind_singleton_hardened` reclaims a socket file the kernel
/// proves nobody is serving; this asserts trusty-search gets that behaviour.
///
/// What: the corpse is a REAL socket inode whose listener has been dropped —
/// exactly what a SIGKILLed predecessor leaves, and the only fixture the
/// takeover fires on. A plain file at the same path answers `ENOTSOCK`, which
/// is `SocketVerdict::Inconclusive` and is refused rather than reclaimed; that
/// asymmetry is deliberate (`uds::singleton`) and this test must not paper over
/// it.
///
/// Why the bind is retried: `bind_singleton_hardened` reclaims only on a
/// verdict the kernel PROVED, and its one-second probe can be starved on a
/// machine running the rest of this suite in parallel — `uds::singleton`
/// records that exact failure and names the remedy, "one failed start that a
/// retry fixes". The retry models what a launchd-supervised daemon does rather
/// than hiding a defect: a socket that is genuinely held is refused every
/// attempt, which is what `bind_refuses_a_socket_another_process_is_serving`
/// pins.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn bind_reclaims_a_stale_socket_file() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("sockets");
    std::fs::create_dir_all(&dir).expect("create the socket directory");
    let socket = dir.join("trusty-search.sock");

    // `UnixListener`'s `Drop` does not unlink, so this leaves the inode behind
    // with nothing listening — a corpse, not a live owner.
    let corpse = std::os::unix::net::UnixListener::bind(&socket).expect("bind the predecessor");
    drop(corpse);
    assert!(socket.exists(), "the corpse must outlive its listener");

    let mut last_err = None;
    let mut bound = None;
    for _ in 0..10 {
        match bind(&socket).await {
            Ok(b) => {
                bound = Some(b);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    let bound = bound.unwrap_or_else(|| {
        panic!(
            "a socket nobody is serving must be reclaimed within ten starts; last error: {:#}",
            last_err.expect("a failed loop records its error")
        )
    });

    let (stop, handle) = serve_bound(bound, &socket, state_with_indexes(0)).await;
    assert!(
        trusty_common::uds::socket_is_serving(&socket, GENEROUS).await,
        "the successor must be serving the reclaimed path"
    );

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");
}

/// Why: the path is DERIVED rather than published, so a typo in the file name
/// is not caught by a mismatched discovery file — it is caught by a consumer
/// dialling a path nothing binds. This pins the name against the convention
/// trusty-memory, trusty-review, trusty-analyze and trusty-mpm already follow.
/// Test: this function IS the test.
#[test]
fn socket_path_is_the_product_named_socket_under_the_data_dir() {
    let path = socket_path().expect("the data directory must resolve");
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some("trusty-search.sock"),
        "socket path {} does not follow the <product>.sock convention",
        path.display()
    );
    assert!(
        path.is_absolute(),
        "a relative socket path would resolve against the daemon's cwd (/ under \
         launchd): {}",
        path.display()
    );
}
