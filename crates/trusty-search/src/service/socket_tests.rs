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

use super::{
    bind, serve_until_shutdown, socket_path, BoundSocket, MAX_FRAME_BYTES, METHODS, METHOD_HEALTH,
};
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
///
/// Test: this function IS the test.
#[test]
fn rpc_router_registers_every_documented_method() {
    let router = super::build_router(&state_with_indexes(0));
    let mut registered: Vec<&str> = router.method_names().chain(router.stream_names()).collect();
    registered.sort_unstable();
    let mut documented: Vec<&str> = METHODS.to_vec();
    documented.sort_unstable();
    assert_eq!(
        registered, documented,
        "METHODS must list exactly what build_router registers"
    );
}

/// Why: #6285 slice 5 registers its two names into the router's STREAMING
/// table, and `rpc_router_registers_every_documented_method` reads the union of
/// both tables — so registering a stream with `typed` instead of `typed_stream`
/// keeps that test green while changing what a caller gets. It is not a
/// degraded stream: `dispatch_streaming` answers a `"stream": true` request
/// against a unary name with `CODE_STREAM_UNSUPPORTED`, so the dashboard sees a
/// refusal rather than one frame. This pins the table each name lives in.
/// Test: this function IS the test.
#[test]
fn rpc_router_registers_the_two_streams_as_streams() {
    let router = super::build_router(&state_with_indexes(0));
    let mut streaming: Vec<&str> = router.stream_names().collect();
    streaming.sort_unstable();
    let mut expected: Vec<&str> = crate::service::rpc::streams::METHODS.to_vec();
    expected.sort_unstable();
    assert_eq!(
        streaming, expected,
        "exactly the streams family may be registered as streaming"
    );

    let unary: Vec<&str> = router.method_names().collect();
    for method in crate::service::rpc::streams::METHODS {
        assert!(
            !unary.contains(method),
            "{method} must not also be a unary method — one name is one or the other"
        );
    }
}

/// Why: [`METHODS`] splices in each family's constants one by one, so a family
/// can grow a name that never reaches this list. That name is then registered
/// nowhere and listed nowhere, which
/// [`rpc_router_registers_every_documented_method`] cannot see — both sides of
/// its `assert_eq!` are simply missing it, and they compare equal. The family's
/// own array still claims the name, and the retire slice's consumers will read
/// THAT array.
///
/// #6285 slice 2 shipped this as a loop inside the assert_eq test, justified as
/// catching a dropped `register` call. It does not: `METHODS` names each
/// family constant explicitly, so dropping `reads::register` leaves `documented`
/// holding names `registered` lacks and the `assert_eq!` fails first. Split out
/// here, comparing the two CONSTANT arrays and never the router, it is
/// independent of that test rather than a weaker restatement of it.
/// Test: this function IS the test.
#[test]
fn every_family_method_is_spliced_into_the_socket_method_list() {
    for (family, names) in [
        ("health", &[METHOD_HEALTH][..]),
        ("reads", crate::service::rpc::reads::METHODS),
        ("queries", crate::service::rpc::queries::METHODS),
        ("writes", crate::service::rpc::writes::METHODS),
        ("streams", crate::service::rpc::streams::METHODS),
    ] {
        for method in names {
            assert!(
                METHODS.contains(method),
                "{method} is named by the {family} family but METHODS does not splice it in"
            );
        }
    }
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

// ------------------------------------------------ the frame budget (5.5) ---

/// Why: [`MAX_FRAME_BYTES`] is only a constant until [`serve_options`] carries
/// it, and the raise exists precisely because the shared control-plane default
/// is too small for this surface. Asserting both halves — the listener takes the
/// figure, and the figure is above the shared default — is what makes the two
/// tests below meaningful rather than tautological.
/// Test: this function IS the test.
#[test]
fn serve_options_carries_the_raised_frame_budget() {
    assert_eq!(
        super::serve_options().max_frame_bytes,
        MAX_FRAME_BYTES,
        "the listener must serve with this surface's budget, not the default"
    );
    const {
        assert!(
            MAX_FRAME_BYTES > trusty_common::uds::MAX_FRAME_BYTES,
            "a budget at or below the shared default would be a no-op raise"
        );
    }
}

/// Why: `search.graph.ingest` carries a document `POST /indexes/{id}/graph`
/// accepts up to 64 MiB, and until this slice the socket refused at 8 MiB — the
/// same request served on one door and refused on the other. This drives a
/// REQUEST frame over the shared default through a real listener, and pins the
/// control beside it: the identical frame against a listener serving
/// `RpcServeOptions::default()` is refused, so the raise is what carries it
/// rather than the frame having been small enough all along.
///
/// `search.health` is the method because `NoParams` ignores its payload, so the
/// filler is carried by the framing without a decode step of its own.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_frame_over_the_shared_default_is_accepted_and_refused_at_the_default() {
    let dir = TempDir::new().expect("tempdir");
    let raised = dir.path().join("raised.sock");
    let (stop, handle) = spawn_listener(&raised, state_with_indexes(1)).await;

    // One mebibyte past the shared default: over it, and far under this one.
    let filler = "x".repeat(trusty_common::uds::MAX_FRAME_BYTES as usize + 1024 * 1024);
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": METHOD_HEALTH, "params": { "filler": filler },
    });

    let answered = dial_once(&raised, &request).await;
    assert!(
        answered.frame.get("result").is_some(),
        "a frame under this listener's budget must be served: {}",
        answered.frame
    );

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");

    // The control: the same frame, a listener serving the shared default.
    let defaulted = dir.path().join("defaulted.sock");
    let bound = bind(&defaulted).await.expect("bind the control socket");
    let router = Arc::new(super::build_router(&state_with_indexes(1)));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let control = tokio::spawn(async move {
        trusty_common::uds::server::serve_until(
            &bound.listener,
            router,
            trusty_common::uds::server::RpcServeOptions::default(),
            async {
                let _ = stop_rx.await;
            },
        )
        .await;
    });
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(&defaulted, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let refused: Result<serde_json::Value, _> =
        trusty_common::uds::send_framed_request(&defaulted, &request, GENEROUS).await;
    assert!(
        refused.is_err(),
        "the shared default must refuse the frame this surface accepts: {refused:?}"
    );

    let _ = stop_tx.send(());
    control
        .await
        .expect("the control serve task must not panic");
}

/// Why: raising the LISTENER alone only moves which end refuses — the client
/// applies its own budget to the response frame it reads, and
/// [`trusty_common::uds::send_framed_request`] applies the 8 MiB shared default.
/// A consumer moving onto these names therefore dials through
/// `send_framed_request_capped` with [`MAX_FRAME_BYTES`], and this pins that:
/// the same response is delivered under this surface's budget and refused as
/// `FrameTooLarge` under the shared one.
///
/// `search.logs.tail` is the method because the log ring is the one surface a
/// test can fill to a known size without indexing anything.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_budget_below_this_listeners_refuses_a_response_it_serves() {
    let dir = TempDir::new().expect("tempdir");
    let socket = dir.path().join("tail.sock");
    let state = state_with_indexes(0);

    // 1000 lines is the ring's capacity; 9 KiB each puts the response frame past
    // the shared 8 MiB default and well under this surface's 64 MiB.
    let line = "y".repeat(9 * 1024);
    for _ in 0..trusty_common::log_buffer::DEFAULT_LOG_CAPACITY {
        state.log_buffer.push(line.clone());
    }
    let (stop, handle) = spawn_listener(&socket, Arc::clone(&state)).await;

    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "search.logs.tail", "params": { "n": 1000 },
    });

    let served: serde_json::Value = trusty_common::uds::send_framed_request_capped(
        &socket,
        &request,
        GENEROUS,
        MAX_FRAME_BYTES,
    )
    .await
    .expect("this surface's budget must carry the response");
    assert_eq!(
        served["result"]["lines"].as_array().map(Vec::len),
        Some(trusty_common::log_buffer::DEFAULT_LOG_CAPACITY),
        "the whole ring must come back: {served}"
    );

    let refused: Result<serde_json::Value, _> =
        trusty_common::uds::send_framed_request(&socket, &request, GENEROUS).await;
    match refused {
        Err(trusty_common::uds::UdsRpcError::FrameTooLarge { limit, .. }) => assert_eq!(
            limit,
            trusty_common::uds::MAX_FRAME_BYTES,
            "the refusal must name the CLIENT's budget, not the listener's"
        ),
        other => panic!("the shared default must refuse this response: {other:?}"),
    }

    let _ = stop.send(());
    handle.await.expect("the serve task must not panic");
}
