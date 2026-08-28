//! Tests for the daemon's Unix-socket listener (#6288 slice 1).
//!
//! Why a separate file: `socket.rs` is production source under the 500-SLOC
//! cap, and these tests drive the REAL bind / accept / unlink path rather than
//! a hand-rolled listener — which is what makes them able to fail if that path
//! regresses.
//!
//! Test: this file IS the test module for `super`.

use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{bind, serve_until_shutdown, socket_path};
use crate::daemon::state::DaemonState;

/// How long a dial or a bind is given before the test calls it stuck.
///
/// A local socket answers in microseconds; this is headroom on a loaded CI
/// machine, not a latency budget.
const GENEROUS: Duration = Duration::from_secs(10);

/// Bind `socket` and serve it on a background task, returning its stop trigger.
///
/// The shutdown future is a parameter of [`serve_until_shutdown`] for exactly
/// this reason: a test cannot deliver SIGTERM to its own process without
/// affecting the whole test binary.
async fn spawn_listener(
    socket: &Path,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let bound = bind(socket).await.expect("bind a fresh socket path");
    serve_bound(bound, socket).await
}

/// Serve an already-bound socket, returning its stop trigger.
///
/// Split from [`spawn_listener`] so a test that needs to control HOW the bind
/// happened — `bind_reclaims_a_stale_socket_file` retries it — still drives the
/// same serve path.
async fn serve_bound(
    bound: super::BoundSocket,
    socket: &Path,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    // #6288 slice 2: the listener now serves real methods, so it needs the
    // state they read. `shared()` is the process-wide daemon state, which is
    // what a test that only probes dispatch (never mutates) wants.
    let state = DaemonState::shared();
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
/// one frame arrives AND the server then closes, rather than hanging or
/// hanging up mid-frame. Only a raw stream can tell those three apart.
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

/// #6288 slice 1's whole observable contract: the listener is up and answers
/// every method with `method_not_found`.
///
/// Why: an empty router could plausibly fail three ways a client cannot tell
/// apart from a healthy "not yet implemented" — it could hang, it could accept
/// and hang up without a frame, or it could answer a malformed frame. This
/// asserts the one correct shape and rules out the other three: a well-formed
/// JSON-RPC error frame carrying `-32601`, the request's id echoed back, and a
/// clean close with no trailing bytes.
///
/// The probed name has to be one NO slice serves: since #6288 slice 2 the
/// router carries twenty real methods, and `mpm.health` — which this test used
/// while the router was empty — now answers with a result.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_method_gets_a_method_not_found_frame() {
    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-mpm.sock");
    let (stop, handle) = spawn_listener(&socket).await;

    let exchange = dial_once(
        &socket,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "mpm.no.such.method", "params": {},
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

/// Why: `bind_hardened` binds and chmods; neither it nor `UnixListener`'s
/// `Drop` removes the path, so a listener that just returned would leave a file
/// behind. This drives the real shutdown path and asserts the file is gone —
/// a test that deleted the file itself would pass whether or not the unlink in
/// `serve_until_shutdown` exists.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn serve_unlinks_its_socket_on_shutdown() {
    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-mpm.sock");
    let (stop, handle) = spawn_listener(&socket).await;
    assert!(socket.exists(), "the listener must have bound its socket");

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");

    assert!(
        !socket.exists(),
        "the socket file must be unlinked on clean shutdown: {}",
        socket.display()
    );
}

/// The Fail-Open Check (#6288 acceptance criterion 4).
///
/// Why: the failure this rules out is a daemon that shrugs off a UDS bind
/// failure and serves HTTP only. `bind` returns `Result`, not `Option`, and
/// `serve_http` propagates it with `?` — so the observable proof is that a bind
/// against a socket someone else is serving is an `Err` whose message names the
/// path, rather than a value the caller could mistake for "no socket, carry on".
/// What: binds and serves one socket, then binds the SAME path again while the
/// first is live.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn bind_refuses_a_socket_another_process_is_serving() {
    let tmp = TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-mpm.sock");
    let (stop, handle) = spawn_listener(&socket).await;

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
/// proves nobody is serving; this asserts trusty-mpm gets that behaviour.
///
/// What: the corpse is a REAL socket inode whose listener has been dropped —
/// exactly what a SIGKILLed predecessor leaves, and the only fixture the
/// takeover fires on. A plain file at the same path answers `ENOTSOCK`, which
/// is `SocketVerdict::Inconclusive` and is refused rather than reclaimed; that
/// asymmetry is deliberate (`uds::singleton`) and this test must not paper
/// over it.
///
/// Why the bind is retried: `bind_singleton_hardened` reclaims only on a
/// verdict the kernel PROVED, and its one-second probe can be starved on a
/// machine running the rest of this suite in parallel — `uds::singleton`
/// records that exact failure, "a starved probe read as `Inconclusive` and
/// refused a genuinely stale socket, which is safe but wrong", and names the
/// remedy: "one failed start that a retry fixes". So the retry models what a
/// launchd-supervised daemon does, rather than hiding a defect. The assertion
/// is unchanged and still fails if the reclaim never happens — a socket that is
/// genuinely held is refused every attempt, which is what
/// `bind_refuses_a_socket_another_process_is_serving` pins.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn bind_reclaims_a_stale_socket_file() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("sockets");
    std::fs::create_dir_all(&dir).expect("create the socket directory");
    let socket = dir.join("trusty-mpm.sock");

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

    let (stop, handle) = serve_bound(bound, &socket).await;
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
/// trusty-memory, trusty-review, and trusty-analyze already follow.
/// Test: this function IS the test.
#[test]
fn socket_path_is_the_product_named_socket_under_the_data_dir() {
    let path = socket_path().expect("the data directory must resolve");
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some("trusty-mpm.sock"),
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
