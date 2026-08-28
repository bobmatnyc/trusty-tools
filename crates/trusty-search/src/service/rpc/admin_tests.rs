//! Socket-versus-HTTP parity for the operational remainder (#6285 slice 5.5).
//!
//! Why: two of these four methods are WRITES, and the slice-4 bar applies to
//! both — a refusal case asserts the refusal AND re-reads the thing that must
//! not have moved. The other two are reads whose whole value is that they answer
//! the same document HTTP answers: `search.logs.tail` replaces the monitor's
//! `GET /logs/tail`, and `search.registry.orphans` replaces the console cleanup
//! tab's `GET /registry/orphans`. A socket that answered either differently
//! would be discovered in a consumer, after the retire slice deleted the route
//! it could have been compared against.
//!
//! Every case drives the REAL axum router and the REAL RPC router over ONE
//! shared state, so a `*_report` core that stopped being the body both
//! transports serve fails here.
//!
//! **The two config writes touch process-global state, so every case is
//! `#[serial]` and runs under an isolated `TRUSTY_DATA_DIR`.** The memory limits
//! are `AtomicU64` cells shared by the whole test binary, and the config PATCH
//! rewrites `indexes.toml`.
//!
//! Test: this file IS the test module for `super`.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt as _;
use trusty_common::uds::server::{RpcError, RpcRouter, CODE_INVALID_PARAMS};

use crate::core::indexer::CodeIndexer;
use crate::core::memguard::{
    index_memory_limit_mb, memory_limit_mb, set_index_memory_limit_mb, set_memory_limit_mb,
};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence::{self, PersistedIndex};
use crate::service::rpc::admin;
use crate::service::rpc::error::CODE_NOT_FOUND;
use crate::service::server::tests_components::IsolatedDataDir;
use crate::service::server::{build_router_on, SearchAppState};

// ---------------------------------------------------------------- fixtures ---

/// One state carrying `ids` as registered indexes, plus both routers on it.
///
/// Why both from the SAME `Arc`: the property under test one level down is that
/// `build_router_on` and `admin::register` read ONE registry. Two `Arc::new`
/// calls would make every comparison below a comparison of two daemons.
fn routers(ids: &[&str]) -> (Arc<SearchAppState>, Router, RpcRouter) {
    let registry = IndexRegistry::new();
    for id in ids {
        let root = format!("/nonexistent/admin-{id}");
        registry.register(IndexHandle::bare(
            IndexId::new((*id).to_string()),
            Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(*id, &root))),
            root.into(),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry));
    let http = build_router_on(
        Arc::clone(&state),
        trusty_common::server::SelfOrigins::default(),
    );
    let rpc = admin::register(RpcRouter::new(), &state);
    (state, http, rpc)
}

/// Restore both memory limits when a config-write case ends.
///
/// Why a guard rather than a trailing statement: an assertion that fails leaves
/// the process-global cells wherever the case put them, and the next test in the
/// binary would read them as its baseline.
struct RestoreLimits {
    memory_limit_mb: Option<u64>,
    index_memory_limit_mb: Option<u64>,
}

impl RestoreLimits {
    fn capture() -> Self {
        Self {
            memory_limit_mb: memory_limit_mb(),
            index_memory_limit_mb: index_memory_limit_mb(),
        }
    }
}

impl Drop for RestoreLimits {
    fn drop(&mut self) {
        set_memory_limit_mb(self.memory_limit_mb);
        set_index_memory_limit_mb(self.index_memory_limit_mb);
    }
}

// ------------------------------------------------------------- transports ---

async fn http_raw(router: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router must answer");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let body = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    (status, body)
}

fn json_request(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("encode")))
        .expect("build the request")
}

async fn http_ok(
    router: &Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let (status, body) = http_raw(router, json_request(method, uri, body)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{method} {uri} answered {status}: {body}"
    );
    body
}

async fn http_get(router: &Router, uri: &str) -> serde_json::Value {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build the request");
    let (status, body) = http_raw(router, request).await;
    assert_eq!(status, StatusCode::OK, "GET {uri} answered {status}: {body}");
    body
}

async fn http_err(
    router: &Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let (status, body) = http_raw(router, json_request(method, uri, body)).await;
    assert!(
        status.is_client_error() || status.is_server_error(),
        "{method} {uri} must be refused, got {status}: {body}"
    );
    (status, body)
}

async fn dispatch(
    rpc: &RpcRouter,
    method: &str,
    params: serde_json::Value,
) -> trusty_common::uds::server::RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .expect("encode the frame");
    rpc.dispatch(&frame).await
}

async fn rpc_ok(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> serde_json::Value {
    let response = dispatch(rpc, method, params).await;
    assert!(
        response.error.is_none(),
        "{method} must answer a result: {:?}",
        response.error
    );
    response.result.expect("a non-error frame carries a result")
}

async fn rpc_err(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> RpcError {
    let response = dispatch(rpc, method, params).await;
    response
        .error
        .unwrap_or_else(|| panic!("{method} must be refused; got {:?}", response.result))
}

/// Assert the socket's error frame is the HTTP refusal, rendered.
///
/// Why not tautological: it re-derives the frame from the `(status, body)` pair
/// the OTHER transport actually produced, so it passes only when both ran the
/// same core and reached the same refusal. The expected code is asserted
/// separately so a mis-classification is caught outright rather than agreed on
/// by both sides. Same shape as `writes_tests::assert_same_refusal`.
fn assert_same_refusal(
    http: &(StatusCode, serde_json::Value),
    over_socket: &RpcError,
    expected_code: i64,
    what: &str,
) {
    let (status, body) = http;
    let rendered = crate::service::rpc::error::rpc_error_from_http(*status, body);
    assert_eq!(
        over_socket.code, expected_code,
        "{what}: the socket must classify this refusal as {expected_code}, \
         got {} (HTTP said {status}: {body})",
        over_socket.code
    );
    assert_eq!(
        rendered.code, expected_code,
        "{what}: HTTP's {status} must project onto {expected_code}: {body}"
    );
    assert_eq!(
        over_socket.message, rendered.message,
        "{what}: the socket's message must be the HTTP body's own wording"
    );
}

/// Overwrite the id-shaped fields, which name the SUBJECT rather than the answer.
fn without_subject(mut body: serde_json::Value, fields: &[&str]) -> serde_json::Value {
    if let Some(obj) = body.as_object_mut() {
        for field in fields {
            if obj.contains_key(*field) {
                obj[*field] = serde_json::Value::String("<subject>".into());
            }
        }
    }
    body
}

/// The persisted hygiene row for `id`, or `None` when it has none.
fn persisted_row(id: &str) -> Option<PersistedIndex> {
    persistence::load_index_registry()
        .expect("load registry")
        .into_iter()
        .find(|e| e.id == id)
}

/// The live config view for `id`, read through the GET core both transports use.
fn live_config(state: &Arc<SearchAppState>, id: &str) -> serde_json::Value {
    serde_json::to_value(
        crate::service::server::index_config_report(state, id).expect("the index is registered"),
    )
    .expect("the view serialises")
}

// ------------------------------------------------- search.index.config.set ---

/// Why: `PATCH /indexes/{id}/config` is the widest write on this surface — it
/// re-registers the handle, rewrites the `indexes.toml` row, and can start a
/// background component catch-up. A socket that reached those through a second
/// implementation would bypass the zero-cap validation, the per-index
/// mutual-exclusion permit (#2984 Phase 1) and the #3049 teardown guard at once.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn index_config_set_over_the_socket_matches_the_http_body() {
    let _isolated = IsolatedDataDir::new();
    let (state, http, rpc) = routers(&["cfg-http", "cfg-socket"]);

    let patch = serde_json::json!({ "include_docs": false, "extra_skip_dirs": ["vendor"] });
    let over_http = http_ok(&http, "PATCH", "/indexes/cfg-http/config", patch.clone()).await;
    let over_socket = rpc_ok(
        &rpc,
        admin::METHOD_INDEX_CONFIG_SET,
        serde_json::json!({ "index_id": "cfg-socket", "body": patch }),
    )
    .await;

    assert_eq!(over_socket["persisted"], serde_json::json!(true));
    assert_eq!(over_socket["reindex_required"], serde_json::json!(true));
    assert_eq!(
        without_subject(over_socket, &["id"]),
        without_subject(over_http, &["id"]),
    );

    // The edit is the point, not the body: both handles and both rows moved.
    for id in ["cfg-http", "cfg-socket"] {
        assert_eq!(
            live_config(&state, id)["include_docs"],
            serde_json::json!(false),
            "{id}: the live handle must carry the edit"
        );
        let row = persisted_row(id).unwrap_or_else(|| panic!("{id} must have a persisted row"));
        assert_eq!(
            row.extra_skip_dirs,
            vec!["vendor".to_string()],
            "{id}: the edit must reach indexes.toml"
        );
    }
}

/// Why: `data_file_max_bytes: 0` would prune every data file, and the route
/// rejects it BEFORE it touches the handle. A socket that validated after
/// mutating — or not at all — would leave an index with a cap its HTTP twin
/// refuses to set. Both transports are pointed at ONE subject, which a refusal
/// permits, so this is a parity assertion rather than two separate checks.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_zero_data_cap_is_refused_and_changes_no_config_on_either_transport() {
    let _isolated = IsolatedDataDir::new();
    let (state, http, rpc) = routers(&["cfg-zero"]);
    let before = live_config(&state, "cfg-zero");

    let patch = serde_json::json!({ "data_file_max_bytes": 0 });
    let over_http = http_err(&http, "PATCH", "/indexes/cfg-zero/config", patch.clone()).await;
    let over_socket = rpc_err(
        &rpc,
        admin::METHOD_INDEX_CONFIG_SET,
        serde_json::json!({ "index_id": "cfg-zero", "body": patch }),
    )
    .await;

    assert_eq!(over_http.0, StatusCode::BAD_REQUEST);
    assert_same_refusal(&over_http, &over_socket, CODE_INVALID_PARAMS, "zero cap");
    assert_eq!(
        live_config(&state, "cfg-zero"),
        before,
        "a refused cap must leave the live config exactly where it was"
    );
    assert!(
        persisted_row("cfg-zero").is_none(),
        "a refused cap must not write a registry row"
    );
}

/// Why: #4715 made an index-scoped 404 mean "no such index ANYWHERE". A config
/// set against an unknown id must be that same refusal on both transports and
/// must not seed a row — the persist path SEEDS a minimal entry when the
/// registry has none, so a socket that reached it would register an index the
/// HTTP route refuses to.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_config_set_against_an_unknown_index_is_refused_and_registers_nothing() {
    let _isolated = IsolatedDataDir::new();
    let (_state, http, rpc) = routers(&[]);

    let patch = serde_json::json!({ "include_docs": false });
    let over_http = http_err(&http, "PATCH", "/indexes/cfg-ghost/config", patch.clone()).await;
    let over_socket = rpc_err(
        &rpc,
        admin::METHOD_INDEX_CONFIG_SET,
        serde_json::json!({ "index_id": "cfg-ghost", "body": patch }),
    )
    .await;

    assert_eq!(over_http.0, StatusCode::NOT_FOUND);
    assert_same_refusal(&over_http, &over_socket, CODE_NOT_FOUND, "unknown index");
    assert!(
        persisted_row("cfg-ghost").is_none(),
        "a refused config set must not seed an indexes.toml row"
    );
}

// ------------------------------------------------------- search.config.set ---

/// Why: `PATCH /config` retunes two process-global memory limits without a
/// restart, and its whole contract is the three-state field encoding — absent
/// leaves the limit alone, a number sets it, an explicit `null` disables it. A
/// socket that decoded `Option<u64>` instead of `Option<Option<u64>>` would
/// collapse the first and third into one and silently disable a limit a caller
/// meant to leave alone. This drives all three arms across both transports.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn config_set_over_the_socket_matches_the_http_body() {
    let _restore = RestoreLimits::capture();
    let (_state, http, rpc) = routers(&[]);

    set_memory_limit_mb(Some(4096));
    set_index_memory_limit_mb(Some(512));
    let over_http = http_ok(
        &http,
        "PATCH",
        "/config",
        serde_json::json!({ "memory_limit_mb": 2048 }),
    )
    .await;
    assert_eq!(over_http["memory_limit_mb"], serde_json::json!(2048));
    assert_eq!(
        over_http["index_memory_limit_mb"],
        serde_json::json!(512),
        "an absent field must leave its limit alone"
    );

    // The same request, from the same starting point, over the socket.
    set_memory_limit_mb(Some(4096));
    set_index_memory_limit_mb(Some(512));
    let over_socket = rpc_ok(
        &rpc,
        admin::METHOD_CONFIG_SET,
        serde_json::json!({ "memory_limit_mb": 2048 }),
    )
    .await;
    assert_eq!(over_socket, over_http);
    assert_eq!(memory_limit_mb(), Some(2048));
    assert_eq!(index_memory_limit_mb(), Some(512));

    // An explicit null disables, and must do so identically.
    let disabled = rpc_ok(
        &rpc,
        admin::METHOD_CONFIG_SET,
        serde_json::json!({ "index_memory_limit_mb": null }),
    )
    .await;
    assert_eq!(disabled["index_memory_limit_mb"], serde_json::Value::Null);
    assert_eq!(index_memory_limit_mb(), None);
}

/// Why: this write's only refusal is a malformed field, and the failure that
/// matters is a partial apply — a body whose second field is unparseable must
/// not leave the first one applied. The decoder runs before the core on both
/// transports, so neither limit may move. The two REFUSALS are deliberately not
/// compared frame-for-frame: axum reports a JSON decode failure as `422` and the
/// router reports it as `invalid_params`, which is the transport's own layer
/// answering rather than a decision either core made.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_malformed_config_set_applies_neither_field_on_either_transport() {
    let _restore = RestoreLimits::capture();
    let (_state, http, rpc) = routers(&[]);

    set_memory_limit_mb(Some(4096));
    set_index_memory_limit_mb(Some(512));
    let malformed = serde_json::json!({ "memory_limit_mb": 1024, "index_memory_limit_mb": "lots" });

    let (status, body) = http_err(&http, "PATCH", "/config", malformed.clone()).await;
    assert_eq!(
        (memory_limit_mb(), index_memory_limit_mb()),
        (Some(4096), Some(512)),
        "HTTP refused with {status} ({body}) but a limit moved anyway"
    );

    let over_socket = rpc_err(&rpc, admin::METHOD_CONFIG_SET, malformed).await;
    assert_eq!(
        over_socket.code, CODE_INVALID_PARAMS,
        "a body that does not decode is the caller's fault: {over_socket:?}"
    );
    assert_eq!(
        (memory_limit_mb(), index_memory_limit_mb()),
        (Some(4096), Some(512)),
        "the socket refused but a limit moved anyway"
    );
}

// -------------------------------------------------------- search.logs.tail ---

/// Why: this is the method `trusty-common`'s monitor dials once the retire slice
/// moves it off `GET /logs/tail`, and the monitor renders the `total` beside the
/// lines to tell whether the ring has wrapped. Both fields are compared whole.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn logs_tail_over_the_socket_matches_the_http_body() {
    let (state, http, rpc) = routers(&[]);
    for i in 0..5 {
        state.log_buffer.push(format!("line {i}"));
    }

    let over_http = http_get(&http, "/logs/tail?n=3").await;
    let over_socket = rpc_ok(
        &rpc,
        admin::METHOD_LOGS_TAIL,
        serde_json::json!({ "n": 3 }),
    )
    .await;

    assert_eq!(
        over_socket["lines"].as_array().map(Vec::len),
        Some(3),
        "n must bound the page: {over_socket}"
    );
    assert_eq!(over_socket, over_http);
}

/// Why: the clamp lives in the core precisely so it cannot be skipped by
/// dialling the socket — an unclamped `n` would let a caller ask for more lines
/// than the ring holds and read a different page than the HTTP route serves for
/// the same request. This drives both ends of the clamp and the defaulted call.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn logs_tail_clamps_n_on_the_socket_too() {
    let (state, http, rpc) = routers(&[]);
    for i in 0..3 {
        state.log_buffer.push(format!("line {i}"));
    }

    // Over the ceiling: both transports answer the whole buffer, not an error.
    let huge = serde_json::json!({ "n": 10_000_000 });
    assert_eq!(
        rpc_ok(&rpc, admin::METHOD_LOGS_TAIL, huge).await,
        http_get(&http, "/logs/tail?n=10000000").await,
    );

    // Absent: the socket must reach the route's own default, not `usize`'s zero.
    assert_eq!(
        rpc_ok(&rpc, admin::METHOD_LOGS_TAIL, serde_json::Value::Null).await,
        http_get(&http, "/logs/tail").await,
    );
}

// ------------------------------------------------- search.registry.orphans ---

/// Plant an `indexes.toml` row for `id` at `root` in the isolated data dir.
fn plant_row(id: &str, root: impl Into<std::path::PathBuf>) {
    persistence::upsert_index_registry_entry(PersistedIndex::new(id.to_string(), root.into()))
        .expect("write the registry row");
}

/// Why (#6371): this is the census `trusty-console`'s cleanup tab reads before
/// offering rows for deletion, and its whole contract is that `orphans` and
/// `indeterminate` do not mix. A socket that flattened them — or that re-derived
/// the classification instead of reading the same census — would hand the
/// console an unmounted volume's roster as deletion candidates.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn registry_orphans_over_the_socket_matches_the_http_body() {
    let _isolated = IsolatedDataDir::new();
    let live = tempfile::tempdir().expect("tempdir");
    plant_row("orphans-live", live.path());
    plant_row("orphans-wiped", live.path().join("wiped-root"));
    plant_row("orphans-unplugged", Path::new("/Volumes/Kemono/project"));

    let (_state, http, rpc) = routers(&[]);
    let over_http = http_get(&http, "/registry/orphans").await;
    let over_socket = rpc_ok(
        &rpc,
        admin::METHOD_REGISTRY_ORPHANS,
        serde_json::Value::Null,
    )
    .await;

    assert_eq!(over_socket, over_http);
    assert_eq!(over_socket["total"], serde_json::json!(3));
    assert_eq!(over_socket["live_count"], serde_json::json!(1));
    assert_eq!(
        over_socket["orphans"]
            .as_array()
            .expect("orphans is an array")
            .iter()
            .map(|o| o["id"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["orphans-wiped"],
        "only the deleted root is a deletion candidate: {over_socket}"
    );
    assert_eq!(
        over_socket["indeterminate"]
            .as_array()
            .expect("indeterminate is an array")
            .iter()
            .map(|u| u["id"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["orphans-unplugged"],
        "the external volume must be reported, not offered: {over_socket}"
    );
}

/// The Fail-Open Check.
///
/// Why: an unreadable registry answered as an empty census reads to the console
/// as "nothing to clean up", which is the one wrong answer this route must never
/// give. Both transports must refuse. What makes the file unreadable is a
/// DIRECTORY at `indexes.toml` — a missing file is legitimately an empty
/// registry, so it would prove nothing.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn an_unreadable_registry_is_refused_on_either_transport() {
    let _isolated = IsolatedDataDir::new();
    let path = persistence::indexes_toml_path().expect("resolve the registry path");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the data dir");
    }
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(&path).expect("make indexes.toml unreadable as a file");

    let (_state, http, rpc) = routers(&[]);
    let request = Request::builder()
        .method("GET")
        .uri("/registry/orphans")
        .body(Body::empty())
        .expect("build the request");
    let over_http = http_raw(&http, request).await;
    assert_eq!(
        over_http.0,
        StatusCode::INTERNAL_SERVER_ERROR,
        "an unreadable registry is an error, never an empty census: {}",
        over_http.1
    );

    let over_socket = rpc_err(
        &rpc,
        admin::METHOD_REGISTRY_ORPHANS,
        serde_json::Value::Null,
    )
    .await;
    assert_same_refusal(
        &over_http,
        &over_socket,
        trusty_common::uds::server::CODE_INTERNAL_ERROR,
        "unreadable registry",
    );
}

