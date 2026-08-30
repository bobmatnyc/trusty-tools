//! Socket-versus-HTTP parity for the write surface (#6285 slice 4).
//!
//! Why: the failure this rules out is not the two transports answering a
//! question differently — it is one of them CHANGING something the other would
//! have refused, or reporting a change that did not happen. Every case here
//! drives the REAL axum router and the REAL RPC router, so a `*_report` function
//! that stopped being the body both transports serve fails these rather than
//! surfacing in a consumer.
//!
//! **Every mutating method has a failure arm, and each one checks state.** The
//! repo's fail-open family is a failure branch that downgrades to success while
//! state advances: #6363's delete answered `200 removed:false` and left the
//! `indexes.toml` row; #5505's ingest answered `200` with graph totals that
//! excluded the contribution it had just persisted. So each refusal case asserts
//! the refusal AND re-reads the thing that must not have moved — the registry
//! entry, the `indexes.toml` row, the handle's root, the merged graph.
//!
//! **A refusal changes nothing, so both transports can be pointed at ONE
//! subject.** That is what makes these true parity assertions rather than two
//! independent checks: the same index, in the same state, refused twice. The
//! SUCCESS cases cannot do that — a create that succeeded is not a create any
//! more — so each drives two subjects built identically and compares the bodies
//! with the id excused, the same one-field exclusion `without_latency` was for
//! the query surface.
//!
//! Test: this file IS the test module for `super`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;
use trusty_common::uds::server::{RpcError, RpcRouter, CODE_INTERNAL_ERROR};

use crate::allowlist::{AllowlistConfig, AllowlistEntry, AllowlistPaths};
use crate::core::corpus::CorpusStore;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::concurrency::ConcurrencyLimiter;
use crate::service::persistence;
use crate::service::query_timeout::QueryTimeoutConfig;
use crate::service::rpc::error::{
    CODE_CONFLICT, CODE_FORBIDDEN, CODE_NOT_FOUND, CODE_TOO_MANY_REQUESTS, CODE_UNAVAILABLE,
    CODE_UNAVAILABLE_PERMANENT,
};
use crate::service::rpc::writes;
use crate::service::server::{build_router_on, SearchAppState};

/// The vector dimensionality the mock embedder is built at.
const DIM: usize = 8;

// ---------------------------------------------------------------- fixtures ---

/// A real directory that passes the hard denylist.
///
/// Why not `tempfile::tempdir()`: `$TMPDIR` is `/var/folders` on macOS, which
/// `SENSITIVE_PATH_PREFIXES` denies outright — a root refused there would be
/// refused before the allowlist gate the write surface actually runs, so the
/// case would prove nothing about the decision under test. Same reason
/// `tests_allowlist_gate_767::safe_root` and `tests_6363::unapproved_root` build
/// theirs under `$HOME`.
fn safe_root(name: &str) -> PathBuf {
    let dir = dirs::home_dir()
        .expect("HOME required")
        .join(".trusty-search-6285-writes")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test root");
    std::fs::canonicalize(&dir).expect("canonicalize test root")
}

/// An allowlist over `dir` approving exactly `approved`.
fn allowlist(dir: &Path, approved: &[&Path]) -> AllowlistPaths {
    let paths = AllowlistPaths::default()
        .with_allowlist(dir.join("allowlist.toml"))
        .with_project_paths(dir.join("projects.json"));
    AllowlistConfig {
        entries: approved
            .iter()
            .map(|p| AllowlistEntry {
                path: p.to_path_buf(),
                name: None,
                exclude: Vec::new(),
                extensions: Vec::new(),
                skip_kg: false,
            })
            .collect(),
    }
    .save_to(&paths.allowlist_file())
    .expect("write allowlist");
    paths
}

/// One state, both routers.
///
/// Why both from the SAME `Arc`: that is the property under test one level down
/// — `build_router_on` and `writes::register` must read one registry, one
/// limiter and one cold store. Two `Arc::new` calls here would make every
/// assertion below pass against two independent daemons.
async fn routers(state: SearchAppState) -> (Arc<SearchAppState>, Router, RpcRouter) {
    let state = Arc::new(state);
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    state.install_embedder(embedder).await;
    let http = build_router_on(
        Arc::clone(&state),
        trusty_common::server::SelfOrigins::default(),
    );
    let rpc = writes::register(RpcRouter::new(), &state);
    (state, http, rpc)
}

/// An empty-registry state whose allowlist approves `approved`.
async fn empty_state(
    allowlist_dir: &Path,
    approved: &[&Path],
) -> (Arc<SearchAppState>, Router, RpcRouter) {
    routers(
        SearchAppState::new(IndexRegistry::new())
            .with_allowlist_paths(allowlist(allowlist_dir, approved)),
    )
    .await
}

/// Register one index backed by a real temp redb corpus.
fn index_with_corpus(
    registry: &IndexRegistry,
    id: &str,
    root: &Path,
) -> (tempfile::TempDir, Arc<CorpusStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = Arc::new(CorpusStore::open(&dir.path().join("corpus.redb")).expect("open corpus"));
    let mut indexer = CodeIndexer::new(id, root.to_str().expect("utf8 root"));
    indexer.set_corpus_store(Arc::clone(&corpus));
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ));
    (dir, corpus)
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
    let body = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("body is not JSON ({e}): {status} {text}"));
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

/// A JSON request that must answer `200`, as its parsed body.
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

/// A JSON request that must be refused, as `(status, body)`.
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

/// A bodyless request (`DELETE`), as `(status, body)`.
async fn http_bodyless(
    router: &Router,
    method: &str,
    uri: &str,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("build the request");
    http_raw(router, request).await
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

// -------------------------------------------------------------- assertions ---

/// Overwrite the id-shaped fields, which name the SUBJECT rather than the answer.
///
/// Why: a create that succeeded is not a create any more, so a success case
/// cannot point both transports at one subject the way a refusal can. Each
/// success case therefore builds two subjects identically and compares the two
/// bodies with the fields that name WHICH subject blanked — the same one-field
/// exclusion `without_latency` is for the query surface, and everything else is
/// still compared byte for byte.
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

/// Assert the socket's error frame is the HTTP refusal, rendered.
///
/// Why not tautological: this re-derives the frame from the HTTP `(status,
/// body)` pair that the OTHER transport actually produced, so it passes only
/// when both transports ran the same core and reached the same refusal. The
/// expected code is asserted separately so a mis-classification is caught
/// outright rather than agreed upon by both sides.
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

/// True iff `id` still has a row in the persisted registry.
fn row_exists(id: &str) -> bool {
    persistence::load_index_registry()
        .expect("load registry")
        .iter()
        .any(|e| e.id == id)
}

fn create_body(id: &str, root: &Path) -> serde_json::Value {
    serde_json::json!({ "id": id, "root_path": root })
}

// ------------------------------------------------------------------ create ---

/// Why: `POST /indexes` is the door every index comes through — the #767
/// allowlist gate, the #2336/#3993 collision guards and the #4110 stage
/// derivation all live behind it, and a socket that registered through a second
/// implementation would bypass all three.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn create_over_the_socket_matches_the_http_body() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let over_http_root = safe_root("create-http");
    let over_socket_root = safe_root("create-socket");
    let (state, http, rpc) =
        empty_state(isolated.path(), &[&over_http_root, &over_socket_root]).await;

    let over_http = http_ok(
        &http,
        "POST",
        "/indexes",
        create_body("create-http", &over_http_root),
    )
    .await;
    let over_socket = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_CREATE,
        create_body("create-socket", &over_socket_root),
    )
    .await;

    assert_eq!(over_socket["created"], serde_json::json!(true));
    assert_eq!(
        without_subject(over_socket, &["id"]),
        without_subject(over_http, &["id"]),
    );
    // The registration is the point, not the body: both ids are live.
    assert!(state.registry.get(&IndexId::new("create-http")).is_some());
    assert!(state.registry.get(&IndexId::new("create-socket")).is_some());
}

/// Why: #767's default-deny is the control that exists because 74 indexes
/// appeared with no operator action. A refusal that registered anyway on one
/// transport would restore exactly that, so this checks the registry as well as
/// the answer — and 403 is one of the three statuses slice 4 taught `code_for`,
/// because `internal_error` told the caller to file a bug about a root it can
/// approve.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_root_the_allowlist_has_not_approved_is_refused_and_registers_nothing() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let unapproved = safe_root("create-unapproved");
    // An EMPTY allowlist: nothing is approved, so the refusal is the #767 gate
    // rather than the hard denylist a `/var/folders` root would have tripped.
    let (state, http, rpc) = empty_state(isolated.path(), &[]).await;
    let body = create_body("create-refused", &unapproved);

    let over_http = http_err(&http, "POST", "/indexes", body.clone()).await;
    assert_eq!(over_http.0, StatusCode::FORBIDDEN, "body: {}", over_http.1);
    let over_socket = rpc_err(&rpc, writes::METHOD_INDEX_CREATE, body).await;

    assert_same_refusal(&over_http, &over_socket, CODE_FORBIDDEN, "unapproved root");
    assert!(
        state
            .registry
            .get(&IndexId::new("create-refused"))
            .is_none(),
        "neither transport may register a root the allowlist refused"
    );
    assert!(
        !row_exists("create-refused"),
        "a refused create must not leave an indexes.toml row either"
    );
}

// ------------------------------------------------------------------ delete ---

/// Why: #6363 — a registration the #767 gate dropped at warm boot lives ONLY in
/// `indexes.toml`, and `unregister_index` used to decide "does this exist?" from
/// the two in-memory stores alone. A live daemon accumulated 60 such rows, each
/// keeping `warm_boot_degraded` true and clearable only by hand-editing the
/// file. The socket must inherit that verdict exactly, so this drives the
/// SAME shape: empty stores, one seeded row, `?delete_data=true`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn an_allowlist_excluded_registration_deletes_over_the_socket_too() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let root = safe_root("delete-excluded");
    // Empty allowlist: this is exactly the state `retain_approved_entries`
    // leaves behind — a row in the file, nothing in either store.
    let (_state, http, rpc) = empty_state(isolated.path(), &[]).await;

    let mut data_dirs = Vec::new();
    for id in ["delete-http", "delete-socket"] {
        persistence::upsert_index_registry_entry(persistence::PersistedIndex::new(id, &root))
            .expect("seed indexes.toml");
        let dir = isolated.path().join("indexes").join(id);
        std::fs::create_dir_all(&dir).expect("create index data dir");
        std::fs::write(dir.join("corpus.marker"), b"real corpus bytes").expect("write marker");
        data_dirs.push(dir);
        assert!(row_exists(id), "fixture must seed the {id} row");
    }

    let (status, over_http) =
        http_bodyless(&http, "DELETE", "/indexes/delete-http?delete_data=true").await;
    assert_eq!(status, StatusCode::OK, "body: {over_http}");
    let over_socket = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_DELETE,
        serde_json::json!({ "index_id": "delete-socket", "delete_data": true }),
    )
    .await;

    assert_eq!(
        over_socket["removed"],
        serde_json::json!(true),
        "#6363: a registration-only id IS a registration — the socket must report \
         it removed, not answer a body with removed:false. Body: {over_socket}"
    );
    assert_eq!(
        without_subject(over_socket, &["id"]),
        without_subject(over_http, &["id"]),
    );
    for (id, dir) in ["delete-http", "delete-socket"].iter().zip(&data_dirs) {
        assert!(
            !row_exists(id),
            "#6363: the {id} indexes.toml row must be gone"
        );
        assert!(
            !dir.exists(),
            "#6363: `delete_data` must remove {}",
            dir.display()
        );
    }
}

/// Why: this is the fail-open shape stated outright. #6363 made a delete whose
/// durable cleanup failed answer `500 ok:false` instead of a `200` a caller
/// records as done while the row survives and comes back on the next boot. A
/// socket that swallowed the failure would hand an operator the same phantom
/// removal, so this asserts the refusal on BOTH transports and re-reads the row
/// afterwards.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_failed_delete_reports_the_failure_and_keeps_the_row_on_either_transport() {
    use std::os::unix::fs::PermissionsExt;

    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let root = safe_root("delete-failing");
    let (_state, http, rpc) = empty_state(isolated.path(), &[]).await;
    persistence::upsert_index_registry_entry(persistence::PersistedIndex::new(
        "delete-failing",
        &root,
    ))
    .expect("seed indexes.toml");

    /// RAII: an assertion below panics on failure, and an unwind must not leave
    /// a read-only directory for `TempDir::drop` to fail on.
    struct ReadOnlyDir(PathBuf);
    impl Drop for ReadOnlyDir {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    // Declared AFTER `isolated`, so it drops FIRST and the tempdir is writable
    // again by the time `TempDir::drop` removes it.
    let _restore = ReadOnlyDir(isolated.path().to_path_buf());
    std::fs::set_permissions(isolated.path(), std::fs::Permissions::from_mode(0o555))
        .expect("make data dir read-only");

    let over_http = http_bodyless(&http, "DELETE", "/indexes/delete-failing").await;
    assert_eq!(
        over_http.0,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a delete whose durable rewrite failed must not answer 2xx: {}",
        over_http.1
    );
    assert_eq!(over_http.1["ok"], serde_json::json!(false));
    assert_eq!(
        over_http.1["removed"],
        serde_json::json!(false),
        "nothing was removed — the row is still in indexes.toml: {}",
        over_http.1
    );

    // The row survived, so the socket call asks the identical question.
    let over_socket = rpc_err(
        &rpc,
        writes::METHOD_INDEX_DELETE,
        serde_json::json!({ "index_id": "delete-failing" }),
    )
    .await;
    assert_same_refusal(
        &over_http,
        &over_socket,
        CODE_INTERNAL_ERROR,
        "a delete whose indexes.toml rewrite failed",
    );

    drop(_restore);
    assert!(
        row_exists("delete-failing"),
        "the row must survive both failed deletes — otherwise the responses and \
         the file disagree in the other direction"
    );
}

/// Why: #6380 — an id is derived from its `root_path`, so a path deleted and
/// recreated between a census and the delete names a DIFFERENT, live index under
/// the same id. `expected_root_path` is what the console's prune sends to pin
/// the delete to the root it listed, and it reaches the daemon as a query
/// parameter over HTTP and as a params field over the socket. A refusal changes
/// nothing, so both transports are pointed at ONE registration.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_delete_whose_expected_root_moved_is_refused_on_either_transport() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let live = safe_root("delete-moved-live");
    let (_state, http, rpc) = empty_state(isolated.path(), &[]).await;
    persistence::upsert_index_registry_entry(persistence::PersistedIndex::new(
        "delete-moved",
        &live,
    ))
    .expect("seed indexes.toml");

    let stale = live.join("recreated-elsewhere");
    let over_http = http_bodyless(
        &http,
        "DELETE",
        &format!(
            "/indexes/delete-moved?expected_root_path={}",
            stale.display().to_string().replace('/', "%2F")
        ),
    )
    .await;
    assert_eq!(
        over_http.0,
        StatusCode::CONFLICT,
        "a registration that moved must refuse the delete: {}",
        over_http.1
    );
    assert_eq!(over_http.1["removed"], serde_json::json!(false));

    let over_socket = rpc_err(
        &rpc,
        writes::METHOD_INDEX_DELETE,
        serde_json::json!({
            "index_id": "delete-moved",
            "expected_root_path": stale.display().to_string(),
        }),
    )
    .await;
    assert_same_refusal(
        &over_http,
        &over_socket,
        CODE_CONFLICT,
        "a root that moved between the census and the delete",
    );
    assert!(
        row_exists("delete-moved"),
        "#6380: neither transport may remove the row behind a refused expectation"
    );
}

/// Why: #6363's other half — an id in no store and no registry row is a 404, not
/// a `200` that reports it did nothing. That ambiguity is what hid the bug on a
/// live daemon, so the socket must not reintroduce it.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_delete_of_an_id_in_no_store_is_not_found_on_either_transport() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let (_state, http, rpc) = empty_state(isolated.path(), &[]).await;

    let over_http = http_bodyless(&http, "DELETE", "/indexes/never-existed-6285").await;
    assert_eq!(over_http.0, StatusCode::NOT_FOUND, "body: {}", over_http.1);
    let over_socket = rpc_err(
        &rpc,
        writes::METHOD_INDEX_DELETE,
        serde_json::json!({ "index_id": "never-existed-6285" }),
    )
    .await;

    assert_same_refusal(
        &over_http,
        &over_socket,
        CODE_NOT_FOUND,
        "absent everywhere",
    );
}

// ---------------------------------------------------------------- relocate ---

/// Why: relocate swaps a live handle and rewrites `indexes.toml`, and #1088 /
/// #1089 established that it must preserve the persisted `colocated` flag and
/// the LRU timestamps rather than re-deriving them. A socket running a second
/// implementation would be free to drop any of that silently.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn relocate_over_the_socket_matches_the_http_body() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let (http_from, http_to) = (safe_root("relo-http-a"), safe_root("relo-http-b"));
    let (sock_from, sock_to) = (safe_root("relo-sock-a"), safe_root("relo-sock-b"));
    let registry = IndexRegistry::new();
    let _http_corpus = index_with_corpus(&registry, "relo-http", &http_from);
    let _sock_corpus = index_with_corpus(&registry, "relo-sock", &sock_from);
    let (state, http, rpc) = routers(SearchAppState::new(registry).with_allowlist_paths(
        allowlist(
            isolated.path(),
            &[&http_from, &http_to, &sock_from, &sock_to],
        ),
    ))
    .await;

    let over_http = http_ok(
        &http,
        "PATCH",
        "/indexes/relo-http",
        serde_json::json!({ "root_path": http_to }),
    )
    .await;
    let over_socket = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_RELOCATE,
        serde_json::json!({ "index_id": "relo-sock", "body": { "root_path": sock_to } }),
    )
    .await;

    assert_eq!(over_socket["relocated"], serde_json::json!(true));
    assert_eq!(
        without_subject(over_socket, &["id", "new_root_path"]),
        without_subject(over_http, &["id", "new_root_path"]),
    );
    // The rebind is the point: both handles now name their new tree.
    for (id, to) in [("relo-http", &http_to), ("relo-sock", &sock_to)] {
        let handle = state
            .registry
            .get(&IndexId::new(id))
            .unwrap_or_else(|| panic!("{id} must still be registered"));
        assert_eq!(&handle.root_path, to, "{id} must be rebound");
    }
}

/// Why: a refused relocate that swapped the handle anyway would strand an index
/// on a tree it must not claim, and the caller would be told it failed. Both
/// #767's gate and #2336's collision guard sit in front of that swap, so this
/// drives the collision arm — 409, one of the statuses slice 4 taught `code_for`
/// — and re-reads the handle's root on both transports.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_refused_relocate_leaves_the_root_where_it_was_on_either_transport() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let (mine, taken) = (safe_root("relo-mine"), safe_root("relo-taken"));
    let registry = IndexRegistry::new();
    let _mine_corpus = index_with_corpus(&registry, "relo-mine", &mine);
    let _taken_corpus = index_with_corpus(&registry, "relo-owner", &taken);
    let (state, http, rpc) = routers(
        SearchAppState::new(registry)
            .with_allowlist_paths(allowlist(isolated.path(), &[&mine, &taken])),
    )
    .await;

    let body = serde_json::json!({ "root_path": taken });
    let over_http = http_err(&http, "PATCH", "/indexes/relo-mine", body.clone()).await;
    assert_eq!(over_http.0, StatusCode::CONFLICT, "body: {}", over_http.1);
    let over_socket = rpc_err(
        &rpc,
        writes::METHOD_INDEX_RELOCATE,
        serde_json::json!({ "index_id": "relo-mine", "body": body }),
    )
    .await;

    assert_same_refusal(
        &over_http,
        &over_socket,
        CODE_CONFLICT,
        "root already owned",
    );
    assert_eq!(
        state
            .registry
            .get(&IndexId::new("relo-mine"))
            .expect("still registered")
            .root_path,
        mine,
        "neither refusal may leave the handle bound to the root it was refused"
    );
}

// ------------------------------------------------------------- file writes ---

/// One index with a live corpus and vector lane, over a real (empty) tree.
///
/// Why the components: `index_file` chunks, embeds and folds into the symbol
/// graph, so an indexer with no embedder would exercise a different path than
/// the daemon runs.
fn planted_registry(id: &str, root: &Path) -> IndexRegistry {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn crate::core::store::VectorStore> =
        Arc::new(crate::core::store::UsearchStore::new(DIM).expect("usearch"));
    let indexer =
        CodeIndexer::new(id, root.to_str().expect("utf8 root")).with_components(embedder, store);
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ));
    registry
}

/// The file both per-file cases write.
const FILE: &str = "src/auth.rs";
/// Its content — one function, so the chunk count is stable across writes.
const CONTENT: &str = "fn authenticate(token: &str) -> bool { verify(token) }\n";

/// Why: `index-file` is the SUPPORTED incremental path for a network-mounted
/// root (#3408), where no watcher can fire — a caller driving it from CI is the
/// only thing keeping that index current. Writing the same file twice answers
/// the same body, so both transports can be pointed at one index and the bodies
/// compared with nothing excused.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn index_file_over_the_socket_matches_the_http_body() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_state, http, rpc) =
        routers(SearchAppState::new(planted_registry("wf", tmp.path()))).await;
    let body = serde_json::json!({ "path": FILE, "content": CONTENT });

    let over_http = http_ok(&http, "POST", "/indexes/wf/index-file", body.clone()).await;
    let over_socket = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_FILE_PUT,
        serde_json::json!({ "index_id": "wf", "body": body }),
    )
    .await;

    assert_eq!(over_socket["indexed"], serde_json::json!(true));
    assert_eq!(over_socket, over_http);
}

/// Why: the delete half of the same contract. `removed_chunks` is the count a
/// caller reconciles against, so a socket that reported a different one — or
/// reported one for a removal that did not happen — would silently desynchronise
/// a network-mounted index.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn remove_file_over_the_socket_matches_the_http_body() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_state, http, rpc) =
        routers(SearchAppState::new(planted_registry("wf", tmp.path()))).await;
    let put = serde_json::json!({ "path": FILE, "content": CONTENT });
    let remove = serde_json::json!({ "path": FILE });

    http_ok(&http, "POST", "/indexes/wf/index-file", put.clone()).await;
    let over_http = http_ok(&http, "POST", "/indexes/wf/remove-file", remove.clone()).await;
    assert!(
        over_http["removed_chunks"].as_u64().unwrap_or(0) > 0,
        "the fixture must remove something, or this parity case proves nothing: {over_http}"
    );

    // Re-plant so the socket asks the identical question.
    http_ok(&http, "POST", "/indexes/wf/index-file", put).await;
    let over_socket = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_FILE_REMOVE,
        serde_json::json!({ "index_id": "wf", "body": remove }),
    )
    .await;

    assert_eq!(over_socket, over_http);
}

/// Why: #4715 made an index-scoped 404 mean "no such index ANYWHERE", and #5349
/// made a write drive the lazy load rather than refuse a cold-parked index. A
/// write against an id in no store is the one case that really is absent, and it
/// must refuse rather than register anything on the way past.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_against_an_unknown_index_is_refused_and_indexes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state, http, rpc) = routers(SearchAppState::new(planted_registry("wf", tmp.path()))).await;
    let body = serde_json::json!({ "path": FILE, "content": CONTENT });

    let over_http = http_err(
        &http,
        "POST",
        "/indexes/absent-6285/index-file",
        body.clone(),
    )
    .await;
    assert_eq!(over_http.0, StatusCode::NOT_FOUND, "body: {}", over_http.1);
    let over_socket = rpc_err(
        &rpc,
        writes::METHOD_INDEX_FILE_PUT,
        serde_json::json!({ "index_id": "absent-6285", "body": body }),
    )
    .await;

    assert_same_refusal(
        &over_http,
        &over_socket,
        CODE_NOT_FOUND,
        "write to an absent index",
    );
    assert!(
        state.registry.get(&IndexId::new("absent-6285")).is_none(),
        "a refused write must not conjure the index it was aimed at"
    );
}

// ----------------------------------------------------------------- reindex ---

/// Why: the reindex TRIGGER is this slice's; the SSE progress stream is slice
/// 5's. What the trigger owes a caller is the `stream_url` it will subscribe to,
/// and a socket that built a different one would hand back a URL that answers
/// nothing.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn reindex_over_the_socket_matches_the_http_body() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_state, http, rpc) =
        routers(SearchAppState::new(planted_registry("wf", tmp.path()))).await;

    let over_http = http_ok(&http, "POST", "/indexes/wf/reindex", serde_json::json!({})).await;
    let over_socket = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_REINDEX,
        serde_json::json!({ "index_id": "wf" }),
    )
    .await;

    assert_eq!(over_socket["queued"], serde_json::json!(true));
    assert_eq!(
        over_socket["stream_url"],
        serde_json::json!("/indexes/wf/reindex/stream"),
        "the trigger must name the slice-5 stream route: {over_socket}"
    );
    assert_eq!(over_socket, over_http);
}

/// Why: #120's cooldown exists because re-running straight after a memory-limit
/// abort hits the same limit — the unprocessed files have no content-hash
/// entries yet, so the loop never terminates. A socket that queued through the
/// cooldown would restore that loop, and 429 is one of the statuses slice 4
/// taught `code_for` precisely so the refusal does not read as "file a bug".
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_reindex_in_cooldown_is_refused_and_queues_nothing_on_either_transport() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state, http, rpc) = routers(SearchAppState::new(planted_registry("wf", tmp.path()))).await;
    state
        .last_reindex_aborted_at
        .insert(IndexId::new("wf"), std::time::Instant::now());

    let over_http = http_err(&http, "POST", "/indexes/wf/reindex", serde_json::json!({})).await;
    assert_eq!(
        over_http.0,
        StatusCode::TOO_MANY_REQUESTS,
        "body: {}",
        over_http.1
    );
    let over_socket = rpc_err(
        &rpc,
        writes::METHOD_INDEX_REINDEX,
        serde_json::json!({ "index_id": "wf" }),
    )
    .await;

    assert_same_refusal(
        &over_http,
        &over_socket,
        CODE_TOO_MANY_REQUESTS,
        "reindex during the #120 cooldown",
    );
    assert!(
        state.reindex_progress.get(&IndexId::new("wf")).is_none(),
        "a refused trigger must not publish a progress entry an SSE subscriber \
         would then wait on forever"
    );
}

// ------------------------------------------------------------ graph ingest ---

fn ingest_body(producer: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": "navigatsql/kggraph@1",
        "producer": producer,
        "producerVersion": "0.1.0",
        "nodes": [
            { "id": "m.Save", "kind": "csharp_method" },
            { "id": "dbo.usp_x", "kind": "proc" },
            { "id": "dbo.orders", "kind": "table" },
        ],
        "edges": [
            { "from": "m.Save", "to": "dbo.usp_x", "kind": "calls_proc", "provenance": ["a.sql"] },
            { "from": "dbo.usp_x", "to": "dbo.orders", "kind": "writes", "provenance": ["a.sql"] },
        ],
    })
}

/// Why: ingest is the one write on this surface whose report is a TYPED struct,
/// so it is the one that goes through [`crate::service::rpc::as_http_body`] —
/// the re-encode that keeps a socket body byte-identical to what axum writes.
/// Its body carries no id at all, so this is a whole-body comparison with
/// nothing excused.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn graph_ingest_over_the_socket_matches_the_http_body() {
    let http_tree = tempfile::tempdir().expect("tempdir");
    let sock_tree = tempfile::tempdir().expect("tempdir");
    let registry = IndexRegistry::new();
    let _http_corpus = index_with_corpus(&registry, "gi-http", http_tree.path());
    let _sock_corpus = index_with_corpus(&registry, "gi-sock", sock_tree.path());
    let (_state, http, rpc) = routers(SearchAppState::new(registry)).await;

    let over_http = http_ok(
        &http,
        "POST",
        "/indexes/gi-http/graph",
        ingest_body("navigatsql"),
    )
    .await;
    let over_socket = rpc_ok(
        &rpc,
        writes::METHOD_GRAPH_INGEST,
        serde_json::json!({ "index_id": "gi-sock", "body": ingest_body("navigatsql") }),
    )
    .await;

    assert_eq!(
        over_socket["graph_nodes"],
        serde_json::json!(3),
        "the contribution must be queryable, or this proves nothing: {over_socket}"
    );
    assert_eq!(over_socket, over_http);
}

/// Why: #5505 is the fail-open shape named outright — the contribution was
/// stored durably, the merge into the serving graph failed, and the endpoint
/// still answered `200 replaced:true` with totals that excluded it. The caller
/// was told an ingest succeeded that no query could see. This drives the same
/// fault on both transports and then CORROBORATES the verdict against what a
/// traversal can actually reach.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unmerged_contribution_is_refused_identically_and_stays_unqueryable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = IndexRegistry::new();
    let (_dir, corpus) = index_with_corpus(&registry, "gi-fail", tmp.path());
    // A row from an earlier producer that no longer deserializes: this ingest's
    // own write still succeeds, the load that follows cannot.
    crate::core::corpus::test_support::corrupt_contrib_row(&corpus, "stale-producer")
        .expect("plant corrupt contrib row");
    let (state, http, rpc) = routers(SearchAppState::new(registry)).await;

    let over_http = http_err(
        &http,
        "POST",
        "/indexes/gi-fail/graph",
        ingest_body("navigatsql"),
    )
    .await;
    assert_eq!(
        over_http.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "an unmerged contribution must not answer 2xx: {}",
        over_http.1
    );
    assert_eq!(
        over_http.1["error"],
        serde_json::json!("contrib_not_merged")
    );
    assert_eq!(
        over_http.1["retryable"],
        serde_json::json!(false),
        "another producer's unreadable row never clears on a re-send: {}",
        over_http.1
    );

    // Replace-per-producer: the socket's own row is rewritten and the same
    // stale row blocks the merge, so this is the identical question.
    let over_socket = rpc_err(
        &rpc,
        writes::METHOD_GRAPH_INGEST,
        serde_json::json!({ "index_id": "gi-fail", "body": ingest_body("navigatsql") }),
    )
    .await;
    assert_same_refusal(
        &over_http,
        &over_socket,
        CODE_UNAVAILABLE_PERMANENT,
        "a contribution that persisted but could not be merged",
    );

    // Corroboration: the verdict matches what a query can actually see.
    let neighbors = crate::service::server::graph_neighbors_report(
        &state,
        "gi-fail",
        &serde_json::from_value(serde_json::json!({ "node": "dbo.orders", "direction": "in" }))
            .expect("neighbors params"),
    )
    .await
    .expect("neighbors answers");
    assert_eq!(
        neighbors["count"],
        serde_json::json!(0),
        "the contribution really is unqueryable on both transports — a success \
         answer would have been a lie: {neighbors}"
    );
}

// ------------------------------------------------------------------- lanes ---

/// Why: the four per-index writes are axum's `bulk_limited` group, and the
/// limiter is tower middleware a second transport cannot reach. Without
/// `bulk_guarded` a socket write would bypass the admission cap that exists so a
/// burst of writes cannot degrade every other response, `/health` included
/// (#41).
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn writes_are_refused_when_the_shared_limiter_is_saturated() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = SearchAppState::new(planted_registry("wf", tmp.path())).with_query_guards(
        ConcurrencyLimiter::with_limits(1, 0),
        QueryTimeoutConfig::from_duration(std::time::Duration::from_secs(30)),
    );
    let (state, _http, rpc) = routers(state).await;

    // Hold the only permit. The socket and the axum router read the SAME
    // limiter off the state, so this saturates both doors at once.
    let _held = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .expect("the first admission must succeed");

    let refused = rpc_err(
        &rpc,
        writes::METHOD_INDEX_FILE_PUT,
        serde_json::json!({
            "index_id": "wf",
            "body": { "path": FILE, "content": CONTENT },
        }),
    )
    .await;
    assert_eq!(
        refused.code, CODE_UNAVAILABLE,
        "a saturated limiter is a busy daemon, not a permanent one: {refused:?}"
    );
}

/// Why: the three registry-level routes are in axum's `free` group — no limiter,
/// no deadline. Wrapping them for symmetry would make a `search.index.delete`
/// queue behind a running reindex that `DELETE /indexes/{id}` sails straight
/// past, which is a difference between the transports introduced by the fix for
/// a difference that was not there.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_registry_level_write_is_not_admission_limited() {
    let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let state = SearchAppState::new(IndexRegistry::new())
        .with_allowlist_paths(allowlist(isolated.path(), &[]))
        .with_query_guards(
            ConcurrencyLimiter::with_limits(1, 0),
            QueryTimeoutConfig::from_duration(std::time::Duration::from_secs(30)),
        );
    let (state, _http, rpc) = routers(state).await;
    let _held = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .expect("the first admission must succeed");

    // Reaches its core and answers the core's own verdict, rather than the
    // limiter's `503`.
    let answered = rpc_err(
        &rpc,
        writes::METHOD_INDEX_DELETE,
        serde_json::json!({ "index_id": "never-existed-6285" }),
    )
    .await;
    assert_eq!(
        answered.code, CODE_NOT_FOUND,
        "a delete must not queue behind a saturated write lane it is not in: {answered:?}"
    );
}
