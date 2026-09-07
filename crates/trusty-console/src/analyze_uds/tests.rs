//! Unit coverage for the trusty-analyze RPC client and the mapping table
//! (#6155).
//!
//! Why here rather than in `tests/`: these assert the module's own contracts —
//! the code-to-status projection, the four ways an exchange fails, the frame
//! budget's floor under the listener's, and every row of the mapping table.
//! `tests/analyze_uds_bridge.rs` covers the same bridge through the real router;
//! this is the layer below it.
//! Test: this file IS the test.

use super::map::{Call, map_request};
use super::*;
use axum::http::Method;

// ─── the RPC client ──────────────────────────────────────────────────────────

/// Bind a socket that answers exactly one framed request with `reply`.
fn stub_daemon(dir: &Path, reply: impl Into<String>) -> PathBuf {
    let socket = dir.join("sockets").join("analyze.sock");
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

/// Why: the SPA branches on HTTP status, so every code trusty-analyze's
/// `From<ApiError> for RpcError` can produce has to come back out as the status
/// the retired HTTP route sent for the same condition. An unmapped code must be
/// a 5xx, never a success.
/// Test: this is the test.
#[test]
fn error_status_maps_every_documented_code() {
    let cases = [
        (CODE_NOT_FOUND, StatusCode::NOT_FOUND),
        (CODE_DEADLINE_EXCEEDED, StatusCode::GATEWAY_TIMEOUT),
        (CODE_INVALID_PARAMS, StatusCode::BAD_REQUEST),
        (CODE_INVALID_REQUEST, StatusCode::BAD_REQUEST),
        (CODE_METHOD_NOT_FOUND, StatusCode::NOT_IMPLEMENTED),
        (CODE_INTERNAL_ERROR, StatusCode::INTERNAL_SERVER_ERROR),
        (CODE_PARSE_ERROR, StatusCode::INTERNAL_SERVER_ERROR),
        (-31999, StatusCode::INTERNAL_SERVER_ERROR),
    ];
    for (code, expected) in cases {
        let err = AnalyzeRpcError::Refused {
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
    assert!(matches!(err, AnalyzeRpcError::Unreachable(_)), "{err:?}");
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
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"no index 'ghost'"}}"#,
    );
    let err = call(
        &socket,
        METHOD_QUALITY,
        json!({ "index_id": "ghost" }),
        Duration::from_secs(5),
    )
    .await
    .expect_err("an error frame is not a success");
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
    assert!(err.message().contains("no index 'ghost'"), "{err:?}");
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
    assert!(matches!(err, AnalyzeRpcError::Malformed(_)), "{err:?}");
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

/// Why: `analyze.smells` answers up to 500 chunks with their source text and
/// `analyze.clusters` a whole index's cluster assignment, both past the shared
/// 8 MiB default — and that default applies to the RESPONSE read, so under the
/// plain helper the call would fail AFTER the daemon did the analysis.
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
    let result = call(&socket, METHOD_SMELLS, json!({}), Duration::from_secs(20))
        .await
        .expect("an oversized-but-permitted response is read");
    assert_eq!(result["blob"].as_str().map(str::len), Some(9 * 1024 * 1024));
}

/// Why: [`MAX_FRAME_BYTES`] is a floor under the listener's own figure, and
/// nothing in a `cargo check` couples the two — trusty-console does not depend
/// on trusty-analyze, and adding a dev-dependency to compare the constants would
/// build fifteen tree-sitter grammars into this crate's test run. Reading the
/// declaration out of the listener's source keeps the two coupled at the cost of
/// one file read.
/// What: parses `pub const MAX_FRAME_BYTES` out of
/// `crates/trusty-analyze/src/service/rpc.rs` and asserts the direction. Fails
/// when the declaration moves rather than passing on a missed match, which is
/// what makes the check worth having.
/// Test: this is the test.
#[test]
fn the_frame_budget_is_at_least_the_listeners() {
    let listener = declared_u64_const(&daemon_rpc_rs(), "pub const MAX_FRAME_BYTES: u64 = ");
    assert!(
        MAX_FRAME_BYTES >= listener,
        "this client's frame budget ({MAX_FRAME_BYTES}) is under the listener's ({listener}); \
         a response trusty-analyze already produced would come back as FrameTooLarge"
    );
}

/// Why: the method literals in this module are a client-side copy of
/// `trusty_analyze::service::rpc::METHODS`, and nothing at compile time couples
/// them — a rename on the daemon side would show up only as a live
/// `method_not_found` behind a dashboard panel that renders an error toast.
/// Reading the daemon's own declarations back is what turns that into a failing
/// assertion here.
/// What: every `analyze.*` name this bridge dials must appear in that array in
/// the daemon's source.
/// Test: this is the test.
#[test]
fn every_analyze_method_this_bridge_dials_is_declared_by_the_daemon() {
    let rpc_rs = daemon_rpc_rs();
    let source = std::fs::read_to_string(&rpc_rs)
        .unwrap_or_else(|e| panic!("read {}: {e}", rpc_rs.display()));
    for method in EVERY_METHOD {
        // `METHOD_HEALTH` is declared as its own const rather than inline in
        // `METHODS`, so a quoted-literal search over the file accepts either.
        let quoted = format!("\"{method}\"");
        assert!(
            source.contains(&quoted),
            "{method} is not declared in {} — the daemon renamed it and this bridge \
             would answer method_not_found",
            rpc_rs.display()
        );
    }
}

/// Why: `map_request` reaching a method this module never declared would be a
/// literal typed twice, and the contract test above only checks the constants.
/// Test: `every_analyze_method_this_bridge_dials_is_declared_by_the_daemon`,
/// `every_declared_method_is_reachable_from_the_table`.
const EVERY_METHOD: [&str; 10] = [
    METHOD_HEALTH,
    METHOD_LIST_INDEXES,
    METHOD_COMPLEXITY_HOTSPOTS,
    METHOD_SMELLS,
    METHOD_QUALITY,
    METHOD_REFACTOR_SUGGESTIONS,
    METHOD_CLUSTERS,
    METHOD_FACTS_LIST,
    METHOD_FACTS_UPSERT,
    METHOD_FACTS_DELETE,
];

/// Where the daemon declares its own method names and frame budget.
fn daemon_rpc_rs() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../trusty-analyze/src/service/rpc.rs")
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
/// `trusty_analyze::service::rpc::socket_path` is
/// `daemon_socket_path("trusty-analyze")` and so is this.
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
        tmp.path().join(ANALYZE_SERVICE).join("trusty-analyze.sock")
    );
}

// ─── the mapping table ───────────────────────────────────────────────────────

fn mapped(method: &Method, path: &str, query: Option<&str>) -> Call {
    map_request(method, path, query, b"").expect("mapped")
}

fn unary(method: &'static str, params: Value) -> Call {
    Call::Unary { method, params }
}

/// Why: this table is the whole contract between the SPA and the socket. If one
/// row is wrong the dashboard loses a feature silently — the SPA renders an
/// error toast, not a crash — so every endpoint `api.js` calls is asserted here
/// against the method it must reach.
/// Test: this is the test.
#[test]
fn maps_every_endpoint_the_spa_calls() {
    // api.health()
    assert_eq!(
        mapped(&Method::GET, "health", None),
        unary(METHOD_HEALTH, json!({}))
    );
    // api.indexes()
    assert_eq!(
        mapped(&Method::GET, "indexes", None),
        unary(METHOD_LIST_INDEXES, json!({}))
    );
    // api.complexityHotspots(id, topK)
    assert_eq!(
        mapped(
            &Method::GET,
            "indexes/proj/complexity_hotspots",
            Some("top_k=20")
        ),
        unary(
            METHOD_COMPLEXITY_HOTSPOTS,
            json!({ "index_id": "proj", "top_n": 20 })
        )
    );
    // api.smells(id, category)
    assert_eq!(
        mapped(
            &Method::GET,
            "indexes/proj/smells",
            Some("category=long_function")
        ),
        unary(
            METHOD_SMELLS,
            json!({ "index_id": "proj", "category": "long_function" })
        )
    );
    // api.quality(id)
    assert_eq!(
        mapped(&Method::GET, "indexes/proj/quality", None),
        unary(METHOD_QUALITY, json!({ "index_id": "proj" }))
    );
    // api.refactorSuggestions(id, { minSeverity, topK })
    assert_eq!(
        mapped(
            &Method::GET,
            "indexes/proj/refactor-suggestions",
            Some("min_severity=low&top_k=20")
        ),
        unary(
            METHOD_REFACTOR_SUGGESTIONS,
            json!({ "index_id": "proj", "min_severity": "low", "top_k": 20 })
        )
    );
    // api.clusters(id, { k, method })
    assert_eq!(
        mapped(
            &Method::GET,
            "indexes/proj/clusters",
            Some("k=8&method=bow")
        ),
        unary(
            METHOD_CLUSTERS,
            json!({ "index_id": "proj", "k": 8, "method": "bow" })
        )
    );
    // api.listFacts(subject, predicate) — and the no-filter form.
    assert_eq!(
        mapped(
            &Method::GET,
            "facts",
            Some("subject=fn+auth&predicate=uses")
        ),
        unary(
            METHOD_FACTS_LIST,
            json!({ "subject": "fn auth", "predicate": "uses" })
        )
    );
    assert_eq!(
        mapped(&Method::GET, "facts", None),
        unary(METHOD_FACTS_LIST, json!({}))
    );
    // api.deleteFact(id)
    assert_eq!(
        mapped(&Method::DELETE, "facts/42", None),
        unary(METHOD_FACTS_DELETE, json!({ "id": 42 }))
    );
}

/// Why: `upsertFact` is the one call carrying a body, and the daemon's
/// `UpsertFactRequest` requires four named fields. A row that dropped the body
/// would answer `invalid_params` for every write.
/// Test: this is the test.
#[test]
fn a_fact_write_forwards_its_body_as_the_params() {
    let body = br#"{"subject":"fn auth","predicate":"uses","object":"bcrypt","index_id":"proj"}"#;
    assert_eq!(
        map_request(&Method::POST, "facts", None, body).expect("mapped"),
        unary(
            METHOD_FACTS_UPSERT,
            json!({
                "subject": "fn auth",
                "predicate": "uses",
                "object": "bcrypt",
                "index_id": "proj",
            })
        )
    );
}

/// Why: the daemon spells the hotspot cap `top_n` and the SPA sends `top_k`. The
/// retired axum route did that rename in its query extractor; losing it here
/// silently caps every hotspot list at the daemon's default of 20 regardless of
/// what the operator asked for.
/// Test: this is the test.
#[test]
fn the_hotspot_top_k_query_becomes_the_daemons_top_n() {
    let Call::Unary { params, .. } = mapped(
        &Method::GET,
        "indexes/proj/complexity_hotspots",
        Some("top_k=50"),
    );
    assert_eq!(params["top_n"], json!(50));
    assert!(
        params.get("top_k").is_none(),
        "the SPA's spelling must not also reach the daemon: {params}"
    );
}

/// Why: every method this module declares must be reachable through the table,
/// or a constant is dead and the contract test above proves nothing about it.
/// Test: this is the test.
#[test]
fn every_declared_method_is_reachable_from_the_table() {
    let rows: [(Method, &str, Option<&str>, &[u8]); 10] = [
        (Method::GET, "health", None, b""),
        (Method::GET, "indexes", None, b""),
        (Method::GET, "indexes/p/complexity_hotspots", None, b""),
        (Method::GET, "indexes/p/smells", None, b""),
        (Method::GET, "indexes/p/quality", None, b""),
        (Method::GET, "indexes/p/refactor-suggestions", None, b""),
        (Method::GET, "indexes/p/clusters", None, b""),
        (Method::GET, "facts", None, b""),
        (Method::POST, "facts", None, b"{}"),
        (Method::DELETE, "facts/1", None, b""),
    ];
    let mut reached: Vec<&str> = rows
        .iter()
        .map(|(m, p, q, b)| {
            let Call::Unary { method, .. } = map_request(m, p, *q, b).expect("mapped");
            method
        })
        .collect();
    reached.sort_unstable();
    let mut declared = EVERY_METHOD.to_vec();
    declared.sort_unstable();
    assert_eq!(
        reached, declared,
        "table and constants must cover each other"
    );
}

/// Why: a path this table does not name must be refused with a body an operator
/// can act on, rather than forwarded somewhere approximate.
/// Test: this is the test.
#[test]
fn refuses_an_unmapped_path() {
    let err = map_request(&Method::GET, "indexes/proj/graph", None, b"")
        .expect_err("graph is not part of the dashboard's surface");
    assert!(err.contains("indexes/proj/graph"), "{err}");
    assert!(err.contains("trusty-analyze"), "{err}");
}

/// Why: `facts::DeleteFactRequest::id` is a `u64`. A string id forwarded blind
/// would answer `invalid_params` from the daemon; refusing here names the id.
/// Test: this is the test.
#[test]
fn a_fact_delete_with_a_non_numeric_id_is_refused() {
    let err = map_request(&Method::DELETE, "facts/not-a-number", None, b"")
        .expect_err("a non-numeric fact id is not mappable");
    assert!(err.contains("not-a-number"), "{err}");
}

#[test]
fn body_json_reads_an_empty_body_as_absent() {
    // An empty POST reaches the daemon as `null`, which `UpsertFactRequest`
    // refuses by naming its missing fields — the correct refusal to inherit.
    assert_eq!(
        map_request(&Method::POST, "facts", None, b"  \n").expect("mapped"),
        unary(METHOD_FACTS_UPSERT, Value::Null)
    );
}

#[test]
fn refuses_a_body_that_is_not_json() {
    let err = map_request(&Method::POST, "facts", None, b"not json")
        .expect_err("a non-JSON body is not mappable");
    assert!(err.contains("not JSON"), "{err}");
}

#[test]
fn query_json_coerces_integers() {
    let Call::Unary { params, .. } = mapped(&Method::GET, "indexes/p/clusters", Some("k=8"));
    assert_eq!(params["k"], json!(8), "must arrive as a number");
}

#[test]
fn query_json_is_an_empty_object_for_no_query() {
    assert_eq!(
        mapped(&Method::GET, "facts", Some("")),
        unary(METHOD_FACTS_LIST, json!({}))
    );
}
