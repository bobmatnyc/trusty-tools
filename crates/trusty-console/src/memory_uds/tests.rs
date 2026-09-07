//! Unit coverage for the trusty-memory RPC client and the mapping table
//! (#6155).
//!
//! Why here rather than in `tests/`: these assert the module's own contracts —
//! the code-to-status projection, the four ways an exchange fails, the frame
//! budget's floor under the listener's, and every row of the mapping table.
//! `tests/memory_uds_bridge.rs` covers the same bridge through the real router;
//! this is the layer below it.
//! Test: this file IS the test.

use super::map::{Call, map_request};
use super::*;
use axum::http::Method;

// ─── the RPC client ──────────────────────────────────────────────────────────

/// Bind a socket that answers exactly one framed request with `reply`.
///
/// The same stub shape `routes::memory_rpc`'s tests use, so the console's two
/// trusty-memory clients are exercised against the same kind of daemon.
fn stub_daemon(dir: &Path, reply: impl Into<String>) -> PathBuf {
    let socket = dir.join("sockets").join("memory.sock");
    let reply = reply.into();
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let Ok((mut conn, _)) = listener.accept().await else {
            return;
        };
        let mut sink = Vec::new();
        let _ = conn.read_to_end(&mut sink).await;
        let _ = conn.write_all(reply.as_bytes()).await;
        let _ = conn.write_all(b"\n").await;
        let _ = conn.flush().await;
    });
    socket
}

/// Why: the SPA branches on HTTP status, so every code trusty-memory's
/// `api_error` can produce has to come back out as the status the retired HTTP
/// route sent for the same condition. An unmapped code must be a 5xx, never a
/// success.
/// Test: this is the test.
#[test]
fn error_status_maps_every_documented_code() {
    let cases = [
        (CODE_NOT_FOUND, StatusCode::NOT_FOUND),
        (CODE_REFUSED, StatusCode::CONFLICT),
        (CODE_INVALID_PARAMS, StatusCode::BAD_REQUEST),
        (CODE_INVALID_REQUEST, StatusCode::BAD_REQUEST),
        (CODE_METHOD_NOT_FOUND, StatusCode::NOT_IMPLEMENTED),
        (CODE_INTERNAL_ERROR, StatusCode::INTERNAL_SERVER_ERROR),
        (CODE_PARSE_ERROR, StatusCode::INTERNAL_SERVER_ERROR),
        (-31999, StatusCode::INTERNAL_SERVER_ERROR),
    ];
    for (code, expected) in cases {
        let err = MemoryRpcError::Refused {
            code,
            message: "refused".to_string(),
        };
        assert_eq!(err.status(), expected, "code {code}");
    }
}

/// Why: a daemon that is not running must never be reported as one that
/// answered.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn call_reports_a_dead_socket_as_unreachable() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let err = call(
        &tmp.path().join("absent.sock"),
        METHOD_HEALTH,
        json!({}),
        Duration::from_secs(2),
    )
    .await
    .expect_err("a dead socket is not a success");
    assert!(matches!(err, MemoryRpcError::Unreachable(_)), "{err:?}");
    assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
}

/// Why: the fail-open branch this module closes. A JSON-RPC `error` is the
/// daemon refusing, and it must reach the caller as the HTTP status the same
/// refusal carried over HTTP — carrying the daemon's own wording.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn call_reports_a_jsonrpc_error_with_the_http_status_it_came_from() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(
        tmp.path(),
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"no palace 'ghost'"}}"#,
    );
    let err = call(
        &socket,
        METHOD_PALACE_GET,
        json!({ "palace_id": "ghost" }),
        Duration::from_secs(5),
    )
    .await
    .expect_err("an error frame is not a success");
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
    assert!(err.message().contains("no palace 'ghost'"), "{err:?}");
}

/// Why: a frame with neither half is a broken contract, and reading it as an
/// empty success would render a healthy-but-empty dashboard for a daemon that
/// told us nothing.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn call_reports_an_empty_answer_as_malformed() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), r#"{"jsonrpc":"2.0","id":1}"#);
    let err = call(&socket, METHOD_HEALTH, json!({}), Duration::from_secs(5))
        .await
        .expect_err("an empty answer is not a success");
    assert!(matches!(err, MemoryRpcError::Malformed(_)), "{err:?}");
    assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
}

/// Why: the success path has to hand back the daemon's own document, since the
/// SPA reads fields the console does not model.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn call_returns_the_daemon_result() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(
        tmp.path(),
        r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"9.9.9"}}"#,
    );
    let result = call(&socket, METHOD_HEALTH, json!({}), Duration::from_secs(5))
        .await
        .expect("the exchange succeeds");
    assert_eq!(result["version"], json!("9.9.9"));
}

/// Why: `memory.kg_graph` returns a whole palace's active triple set and
/// `memory.activity` can be asked for 500 arbitrary event bodies, both past the
/// shared 8 MiB default — and that default applies to the RESPONSE read, so
/// under the plain helper the call would fail AFTER the daemon did the work.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_response_over_the_shared_default_is_read() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let big = "x".repeat(9 * 1024 * 1024);
    let reply = json!({ "jsonrpc": "2.0", "id": 1, "result": { "blob": big } }).to_string();
    assert!(
        reply.len() as u64 > trusty_common::uds::MAX_FRAME_BYTES,
        "the fixture must exceed the shared default to prove anything"
    );
    let socket = stub_daemon(tmp.path(), reply);
    let result = call(&socket, METHOD_KG_GRAPH, json!({}), Duration::from_secs(20))
        .await
        .expect("an oversized-but-permitted response is read");
    assert_eq!(result["blob"].as_str().map(str::len), Some(9 * 1024 * 1024));
}

/// Why: a stream that cannot be opened must not read as an empty stream — the
/// SPA treats a closed activity stream as a feed that ended.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn open_stream_reports_a_dead_socket_as_unreachable() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let err = open_stream(
        &tmp.path().join("absent.sock"),
        METHOD_ACTIVITY_STREAM,
        json!({}),
        STREAM_OPEN_TIMEOUT,
    )
    .await
    .expect_err("a dead socket cannot open a stream");
    assert!(matches!(err, MemoryRpcError::Unreachable(_)), "{err:?}");
}

/// Why: [`MAX_FRAME_BYTES`] is a floor under the listener's own figure, and
/// nothing in a `cargo check` couples the two — trusty-console does not depend
/// on trusty-memory, and adding a dev-dependency to compare the constants would
/// build a vector store and an embedder into this crate's test run. Reading the
/// declaration out of the listener's source keeps the two coupled at the cost of
/// one file read.
/// What: parses `pub const MAX_FRAME_BYTES` out of
/// `crates/trusty-memory/src/transport/uds.rs` and asserts the direction. Fails
/// when the declaration moves rather than passing on a missed match, which is
/// what makes the check worth having.
/// Test: this is the test.
#[test]
fn the_frame_budget_is_at_least_the_listeners() {
    let listener = declared_u64_const(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trusty-memory/src/transport/uds.rs"),
        "pub const MAX_FRAME_BYTES: u64 = ",
    );
    assert!(
        MAX_FRAME_BYTES >= listener,
        "this client's frame budget ({MAX_FRAME_BYTES}) is under the listener's ({listener}); \
         a response trusty-memory already produced would come back as FrameTooLarge"
    );
}

/// Why: the method literals in this module are a client-side copy of
/// `trusty_memory::transport::uds::FOLDED_METHODS` and `STREAM_METHODS`, and
/// nothing at compile time couples them — a rename on the daemon side would show
/// up only as a live `method_not_found` behind a dashboard panel that renders an
/// error toast. Reading the daemon's own declarations back is what turns that
/// into a failing assertion here.
/// What: every `memory.*` name this bridge dials must appear in one of the two
/// tables in the daemon's source. `kg_query` is deliberately excluded — it is a
/// dispatcher tool, not a folded method (see [`METHOD_KG_QUERY_TOOL`]).
/// Test: this is the test.
#[test]
fn every_memory_method_this_bridge_dials_is_declared_by_the_daemon() {
    let uds_rs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../trusty-memory/src/transport/uds.rs");
    let source = std::fs::read_to_string(&uds_rs)
        .unwrap_or_else(|e| panic!("read {}: {e}", uds_rs.display()));
    // The two tables are the only places a `"memory.*"` string literal is
    // declared in that file, so a quoted-literal search over it is exact enough
    // to catch a rename and specific enough not to match prose.
    for method in [
        METHOD_HEALTH,
        METHOD_STATUS,
        METHOD_CONFIG,
        METHOD_PALACES_LIST,
        METHOD_PALACE_GET,
        METHOD_DRAWERS_LIST,
        METHOD_LOGS_TAIL,
        METHOD_DREAM_STATUS,
        METHOD_DREAM_RUN,
        METHOD_ADMIN_STOP,
        METHOD_KG_SUBJECTS_WITH_COUNTS,
        METHOD_KG_ALL,
        METHOD_KG_COUNT,
        METHOD_KG_GRAPH,
        METHOD_KG_GRAPH_SEED,
        METHOD_KG_GRAPH_NEIGHBORS,
        METHOD_ACTIVITY,
        METHOD_ACTIVITY_STREAM,
    ] {
        // `METHOD_HEALTH` is declared as its own const rather than inline, so
        // accept either spelling.
        let quoted = format!("\"{method}\"");
        assert!(
            source.contains(&quoted),
            "{method} is not declared in {} — the daemon renamed it and this bridge \
             would answer method_not_found",
            uds_rs.display()
        );
    }
}

/// Read one `pub const <name>: u64 = <product of integers>;` out of a source
/// file, panicking when the declaration is not there to read.
fn declared_u64_const(path: &std::path::Path, prefix: &str) -> u64 {
    let source =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let decl = source
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix(prefix)?
                .strip_suffix(';')
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            panic!(
                "no `{prefix}…;` in {} — the declaration moved, so this floor is unverified",
                path.display()
            )
        });
    decl.split('*')
        .map(|factor| {
            factor
                .trim()
                .parse::<u64>()
                .unwrap_or_else(|e| panic!("parse `{decl}`: {e}"))
        })
        .product()
}

/// Why: the console and the daemon must compute the SAME path, or the console
/// dials a socket nothing is bound to and reports a running daemon as down.
/// `trusty_memory::transport::uds::socket_path` is
/// `daemon_socket_path("trusty-memory")` and so is this.
/// What: points `resolve_data_dir` at a tempdir — under the shared
/// `detect::ENV_LOCK`, because that env var is process-global and the connector
/// tests read it too — and asserts the whole resolved path.
/// Test: this is the test.
#[test]
fn socket_path_matches_the_daemon_resolver() {
    let _guard = crate::detect::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
    }
    let resolved = socket_path();
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }
    assert_eq!(
        resolved.expect("the override resolves"),
        tmp.path().join(MEMORY_SERVICE).join("trusty-memory.sock")
    );
}

// ─── the mapping table ───────────────────────────────────────────────────────

fn mapped(method: &Method, path: &str, query: Option<&str>) -> Call {
    map_request(method, path, query).expect("mapped")
}

fn unary(method: &'static str, params: Value) -> Call {
    Call::Unary { method, params }
}

/// Why: this table is the whole contract between the SPA and the socket. If one
/// row is wrong the dashboard loses a feature silently — the SPA renders an
/// error toast, not a crash — so every endpoint `api.js` and `ActivityFeed`
/// call is asserted here against the method it must reach.
/// Test: this is the test.
#[test]
fn maps_every_endpoint_the_spa_calls() {
    assert_eq!(
        mapped(&Method::GET, "health", None),
        unary(METHOD_HEALTH, json!({}))
    );
    assert_eq!(
        mapped(&Method::GET, "api/v1/status", None),
        unary(METHOD_STATUS, json!({}))
    );
    assert_eq!(
        mapped(&Method::GET, "api/v1/config", None),
        unary(METHOD_CONFIG, json!({}))
    );
    assert_eq!(
        mapped(&Method::GET, "api/v1/palaces", None),
        unary(METHOD_PALACES_LIST, json!({}))
    );
    assert_eq!(
        mapped(&Method::GET, "api/v1/palaces/izzie", None),
        unary(METHOD_PALACE_GET, json!({ "palace_id": "izzie" }))
    );
    assert_eq!(
        mapped(
            &Method::GET,
            "api/v1/palaces/izzie/drawers",
            Some("limit=200")
        ),
        unary(
            METHOD_DRAWERS_LIST,
            json!({ "palace_id": "izzie", "limit": 200 })
        )
    );
    assert_eq!(
        mapped(&Method::GET, "api/v1/logs/tail", Some("n=200")),
        unary(METHOD_LOGS_TAIL, json!({ "n": 200 }))
    );
    assert_eq!(
        mapped(&Method::GET, "api/v1/dream/status", None),
        unary(METHOD_DREAM_STATUS, json!({}))
    );
    assert_eq!(
        mapped(&Method::POST, "api/v1/dream/run", None),
        unary(METHOD_DREAM_RUN, json!({}))
    );
    assert_eq!(
        mapped(&Method::POST, "api/v1/admin/stop", None),
        unary(METHOD_ADMIN_STOP, json!({}))
    );
    assert_eq!(
        mapped(&Method::GET, "api/v1/activity", Some("limit=50&offset=0")),
        unary(METHOD_ACTIVITY, json!({ "limit": 50, "offset": 0 }))
    );

    // The knowledge graph.
    assert_eq!(
        mapped(
            &Method::GET,
            "api/v1/palaces/izzie/kg",
            Some("subject=tag%3Arust")
        ),
        unary(
            METHOD_KG_QUERY_TOOL,
            json!({ "palace": "izzie", "subject": "tag:rust" })
        )
    );
    assert_eq!(
        mapped(
            &Method::GET,
            "api/v1/palaces/izzie/kg/subjects_with_counts",
            Some("limit=200")
        ),
        unary(
            METHOD_KG_SUBJECTS_WITH_COUNTS,
            json!({ "palace_id": "izzie", "limit": 200 })
        )
    );
    assert_eq!(
        mapped(
            &Method::GET,
            "api/v1/palaces/izzie/kg/all",
            Some("limit=50&offset=100")
        ),
        unary(
            METHOD_KG_ALL,
            json!({ "palace_id": "izzie", "limit": 50, "offset": 100 })
        )
    );
    assert_eq!(
        mapped(&Method::GET, "api/v1/palaces/izzie/kg/count", None),
        unary(METHOD_KG_COUNT, json!({ "palace_id": "izzie" }))
    );
    assert_eq!(
        mapped(&Method::GET, "api/v1/palaces/izzie/kg/graph", None),
        unary(METHOD_KG_GRAPH, json!({ "palace_id": "izzie" }))
    );
    assert_eq!(
        mapped(
            &Method::GET,
            "api/v1/palaces/izzie/kg/graph/seed",
            Some("limit=75")
        ),
        unary(
            METHOD_KG_GRAPH_SEED,
            json!({ "palace_id": "izzie", "limit": 75 })
        )
    );
    assert_eq!(
        mapped(
            &Method::GET,
            "api/v1/palaces/izzie/kg/graph/neighbors",
            Some("node=rust&direction=both&max_hops=1")
        ),
        unary(
            METHOD_KG_GRAPH_NEIGHBORS,
            json!({
                "palace_id": "izzie",
                "node": "rust",
                "direction": "both",
                "max_hops": 1
            })
        )
    );

    // The live feed — the one streaming row.
    assert_eq!(
        mapped(&Method::GET, "sse", None),
        Call::Stream {
            method: METHOD_ACTIVITY_STREAM,
            params: json!({})
        }
    );
}

/// Why: a path this table does not name must be refused, not forwarded
/// somewhere approximate — the whole reason the mapping is written down.
/// Test: this is the test.
#[test]
fn refuses_an_unmapped_path() {
    let err = map_request(&Method::POST, "api/v1/palaces", None).expect_err("not mapped");
    assert!(
        err.contains("no trusty-memory socket method serves"),
        "{err}"
    );
    assert!(
        err.contains("api/v1/palaces"),
        "the refusal must name itself: {err}"
    );
}

/// Why: `kg_query` needs a subject the caller already knows, and calling it
/// without one would reach the daemon and be refused there. Refusing here names
/// the missing parameter instead.
/// Test: this is the test.
#[test]
fn a_kg_read_without_a_subject_is_refused() {
    let err = map_request(&Method::GET, "api/v1/palaces/izzie/kg", None).expect_err("no subject");
    assert!(err.contains("`subject`"), "{err}");
}

/// Why: a query string is text and the RPC params are typed, so `limit=200` has
/// to arrive as `200` or every paged read answers `invalid_params`.
/// Test: this is the test.
#[test]
fn query_json_coerces_integers() {
    let Call::Unary { params, .. } = mapped(&Method::GET, "api/v1/logs/tail", Some("n=500")) else {
        panic!("logs/tail is unary");
    };
    assert_eq!(params["n"], json!(500), "must arrive as a number");
}

/// Why: a no-argument method sent `null` params answers `invalid_params`; an
/// empty OBJECT is what every param type accepts.
/// Test: this is the test.
#[test]
fn query_json_is_an_empty_object_for_no_query() {
    let Call::Unary { params, .. } = mapped(&Method::GET, "api/v1/status", None) else {
        panic!("status is unary");
    };
    assert_eq!(params, json!({}));
    assert!(params.is_object(), "never null");
}
