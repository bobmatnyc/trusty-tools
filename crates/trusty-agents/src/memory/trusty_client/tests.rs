//! Tests for `TrustyMemoryClient` and `MemoryBackend` (issue #3225).
//!
//! Why: split out of `mod.rs` to keep the production file under the 500-SLOC
//! cap (mirrors the `redb_usearch/{mod,tests}.rs` split already established in
//! this crate).
//! What: unit tests for `ns_id`/health-check/auto_detect plus a mock-server
//! (axum) round-trip proving insert/get/delete/search against the real route
//! paths.
//! Test: this *is* the test module.

use super::*;
use serde_json::json;
use std::net::SocketAddr;
use tempfile::TempDir;
use tokio::net::TcpListener;

/// Why: If the daemon is not running we must report unreachable rather
/// than block startup. Pointing at a port nothing listens on is the
/// simplest way to verify that without spinning up real infra.
/// What: Build a client at `127.0.0.1:19999` (assumed unused) and call
/// `health_check`; assert false.
/// Test: This test.
#[tokio::test]
async fn health_check_false_when_daemon_absent() {
    let client = TrustyMemoryClient::new("http://127.0.0.1:19999");
    assert!(!client.health_check().await);
}

/// Why: `auto_detect` is the single entry point callers use; if it
/// silently chose Trusty when the daemon is down, every subsequent
/// memory call would fail. Verify the fallback path picks Local.
/// What: Open a temp `RedbUsearchStore`, call `auto_detect_with_url`
/// pointing at the same dead port, and assert the resulting backend is
/// `Local`.
/// Test: This test.
#[tokio::test]
async fn auto_detect_falls_back_to_local() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(RedbUsearchStore::open(dir.path(), 4).unwrap());
    let backend = MemoryBackend::auto_detect_with_url("http://127.0.0.1:19999", store).await;
    match backend {
        MemoryBackend::Local(_) => {}
        MemoryBackend::Trusty(_) => panic!("expected Local fallback"),
    }
}

/// Why (issue #3336): `default_trusty_url` used to hard-code a port
/// (`7775`) that had silently drifted from `trusty_memory::DEFAULT_HTTP_PORT`
/// (`7070`) — auto-detection probed a port nothing listens on, so every
/// install with the daemon up on its default port fell back to the
/// embedded store. Asserting the URL embeds the daemon crate's own
/// constant (rather than a re-typed literal) makes a future edit to either
/// side's default fail this test instead of silently drifting again.
/// What: parses the port out of `default_trusty_url()` and asserts it
/// equals `trusty_memory::DEFAULT_HTTP_PORT` exactly.
/// Test: This test.
#[test]
fn default_trusty_url_port_matches_trusty_memory_default() {
    let url = default_trusty_url();
    let expected = format!("http://127.0.0.1:{}", trusty_memory::DEFAULT_HTTP_PORT);
    assert_eq!(url, expected);
}

/// Why: `ns_id` is what keeps separate segments from colliding in the
/// daemon's flat key namespace. Lock the format with a test so refactors
/// don't silently break cross-segment isolation.
/// What: Build ns ids for two different segments + same logical id;
/// assert they differ and contain the segment prefix.
/// Test: This test.
#[test]
fn ns_id_namespaces_by_segment() {
    let a = TrustyMemoryClient::ns_id(Segment::Brief, "abc");
    let b = TrustyMemoryClient::ns_id(Segment::History, "abc");
    assert_ne!(a, b);
    assert!(a.contains(Segment::Brief.prefix()));
    assert!(b.contains(Segment::History.prefix()));
}

/// Why: `search` must fail loudly and descriptively rather than
/// silently return an empty/wrong result — see the method's doc comment
/// for the architectural reason a vector query can't reach
/// trusty-memory's text-only recall endpoint.
/// What: Call `search` on a client pointed at an unreachable port (the
/// error must come from the "not supported" short-circuit, not a
/// network failure — this must be true even with zero network I/O).
/// Test: This test.
#[tokio::test]
async fn search_returns_descriptive_unsupported_error() {
    let client = TrustyMemoryClient::new("http://127.0.0.1:19999");
    let err = client
        .search(Segment::AgentMemory, &[0.0_f32; 4], 5)
        .await
        .expect_err("search must be rejected, not silently succeed");
    assert!(
        err.to_string().contains("not supported"),
        "unexpected error message: {err}"
    );
}

// -----------------------------------------------------------------
// Mock HTTP server mirroring trusty-memory's REAL route paths.
//
// Why: `trusty-agents` cannot take a dev-dependency on `trusty-memory`
// without an awkward cross-crate coupling for a single test module
// (pulling in the whole daemon crate, its full route table, and ONNX
// embedder just to prove five route paths and DTO shapes are wired
// correctly is disproportionate). `axum` is already a `trusty-agents`
// dependency (used by `src/api/server` and `src/search/service`, whose
// `router_serves_health_with_mock_indexer` test established this exact
// `TcpListener` + `axum::serve` pattern — mirrored here rather than
// reinvented). This mock's route table is a deliberately narrow subset
// of `crates/trusty-memory/src/web/mod.rs`'s real router: `/health`,
// `POST /api/v1/palaces`, `POST /api/v1/palaces/{id}/drawers`,
// `GET /api/v1/palaces/{id}/drawers`, and
// `DELETE /api/v1/palaces/{id}/drawers/{drawer_id}` — so `auto_detect`
// and the CRUD path are proven against the real paths/methods/status
// codes, not a fabricated shape.
// What: An in-memory drawer store keyed by tag, guarded by a
// `std::sync::Mutex`, wired into a minimal `axum::Router`.
// -----------------------------------------------------------------

use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use std::sync::Mutex as StdMutex;

#[derive(Clone)]
struct MockDrawer {
    id: Uuid,
    content: String,
    tags: Vec<String>,
    /// Captured `force` flag from the create request — lets tests assert the
    /// client actually sent `force: true` (issue #3225 finding 2: the real
    /// daemon's quality gate rejects JSON-shaped `content` without it).
    force: bool,
}

#[derive(Default)]
struct MockState {
    drawers: StdMutex<Vec<MockDrawer>>,
}

async fn mock_health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn mock_create_palace(Json(body): Json<Value>) -> Json<Value> {
    Json(json!({"id": body.get("name").cloned().unwrap_or(Value::Null)}))
}

#[derive(Deserialize)]
struct MockCreateDrawerBody {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    force: bool,
}

async fn mock_create_drawer(
    State(state): State<Arc<MockState>>,
    AxumPath(_palace_id): AxumPath<String>,
    Json(body): Json<MockCreateDrawerBody>,
) -> Json<Value> {
    let id = Uuid::new_v4();
    state.drawers.lock().unwrap().push(MockDrawer {
        id,
        content: body.content,
        tags: body.tags,
        force: body.force,
    });
    Json(json!({"id": id}))
}

#[derive(Deserialize)]
struct MockListDrawersQuery {
    tag: Option<String>,
}

async fn mock_list_drawers(
    State(state): State<Arc<MockState>>,
    AxumPath(_palace_id): AxumPath<String>,
    Query(q): Query<MockListDrawersQuery>,
) -> Json<Value> {
    let drawers = state.drawers.lock().unwrap();
    let rows: Vec<Value> = drawers
        .iter()
        .filter(|d| match &q.tag {
            Some(t) => d.tags.iter().any(|x| x == t),
            None => true,
        })
        .map(|d| json!({"id": d.id, "content": d.content, "tags": d.tags}))
        .collect();
    Json(Value::Array(rows))
}

async fn mock_delete_drawer(
    State(state): State<Arc<MockState>>,
    AxumPath((_palace_id, drawer_id)): AxumPath<(String, Uuid)>,
) -> axum::http::StatusCode {
    state.drawers.lock().unwrap().retain(|d| d.id != drawer_id);
    axum::http::StatusCode::NO_CONTENT
}

async fn spawn_mock_server() -> (SocketAddr, Arc<MockState>) {
    let state = Arc::new(MockState::default());
    let app = Router::new()
        .route("/health", get(mock_health))
        .route("/api/v1/palaces", post(mock_create_palace))
        .route(
            "/api/v1/palaces/{id}/drawers",
            get(mock_list_drawers).post(mock_create_drawer),
        )
        .route(
            "/api/v1/palaces/{id}/drawers/{drawer_id}",
            delete(mock_delete_drawer),
        )
        .with_state(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Give the listener a moment to come up (mirrors the existing
    // `router_serves_health_with_mock_indexer` pattern in
    // `search/service/tests.rs`).
    tokio::time::sleep(Duration::from_millis(50)).await;

    (addr, state)
}

/// Why: this is the core regression test for issue #3225 — proves
/// `auto_detect` selects the HTTP backend (not the local fallback) when
/// the daemon answers `GET /health` on the real bare path.
/// What: Spin the mock server, `auto_detect_with_url` against it, assert
/// `MemoryBackend::Trusty`.
/// Test: This test.
#[tokio::test]
async fn auto_detect_selects_trusty_when_daemon_reachable() {
    let (addr, _state) = spawn_mock_server().await;
    let dir = TempDir::new().unwrap();
    let store = Arc::new(RedbUsearchStore::open(dir.path(), 4).unwrap());
    let backend = MemoryBackend::auto_detect_with_url(&format!("http://{addr}"), store).await;
    match backend {
        MemoryBackend::Trusty(_) => {}
        MemoryBackend::Local(_) => panic!("expected Trusty backend when daemon is reachable"),
    }
}

/// Why: proves the full insert -> get -> delete round trip hits the
/// real route paths (`POST /api/v1/palaces`,
/// `POST/GET /api/v1/palaces/{id}/drawers`,
/// `DELETE /api/v1/palaces/{id}/drawers/{uuid}`) with correctly-shaped
/// bodies, and that `get` losslessly recovers the original payload.
/// What: insert a JSON payload, `get` it back and assert equality,
/// delete it, then assert `get` returns `None`.
/// Test: This test.
#[tokio::test]
async fn insert_get_delete_round_trip_against_mock_daemon() {
    let (addr, _state) = spawn_mock_server().await;
    let client = TrustyMemoryClient::new(format!("http://{addr}"));

    let payload = json!({"text": "the quokka is a marsupial", "n": 7});
    client
        .insert(Segment::Brief, "rec-1", &[0.1, 0.2, 0.3], payload.clone())
        .await
        .expect("insert should succeed");

    let got = client
        .get(Segment::Brief, "rec-1")
        .await
        .expect("get should succeed");
    assert_eq!(got, Some(payload));

    client
        .delete(Segment::Brief, "rec-1")
        .await
        .expect("delete should succeed");
    let after = client
        .get(Segment::Brief, "rec-1")
        .await
        .expect("get after delete should succeed");
    assert!(after.is_none());
}

/// Why: repeated `insert` calls for the same id must overwrite, not
/// accumulate — the local redb-backed store is upsert-keyed and callers
/// (e.g. a future recall path) should not see stale duplicates.
/// What: insert twice with different payloads under the same id; assert
/// only the second payload is visible via `get`.
/// Test: This test.
#[tokio::test]
async fn insert_upserts_existing_id() {
    let (addr, _state) = spawn_mock_server().await;
    let client = TrustyMemoryClient::new(format!("http://{addr}"));

    client
        .insert(Segment::Brief, "rec-1", &[0.0], json!({"v": 1}))
        .await
        .unwrap();
    client
        .insert(Segment::Brief, "rec-1", &[0.0], json!({"v": 2}))
        .await
        .unwrap();

    let got = client.get(Segment::Brief, "rec-1").await.unwrap();
    assert_eq!(got, Some(json!({"v": 2})));
}

/// Why: issue #3225 finding 2 — the REAL trusty-memory daemon runs a
/// signal/noise quality gate on `content` that rejects JSON-shaped payloads
/// by design (`non_alphabetic_ratio` — see
/// `crates/trusty-memory/src/service/core.rs`'s `create_drawer` doc comment
/// and its `create_drawer_rejects_json_content_without_force` /
/// `create_drawer_force_bypasses_quality_gate_for_json_content` daemon-side
/// tests). `insert_get_delete_round_trip_against_mock_daemon` above passed
/// even before `force` existed only because its payload happened to embed
/// an English sentence (`"the quokka is a marsupial"`), which would NOT
/// reproduce a rejection against the real daemon. This test uses a
/// realistic low-prose payload — the shape `TrustyMemoryClient` actually
/// needs to support (scores, ids, small structured records) — and proves
/// the client always sets `force: true` on the wire, not just that the
/// (gate-less) mock round-trips successfully.
/// What: inserts `{"score":0.42,"id":"a1b2","tier":3}`, then inspects the
/// mock server's captured request directly and asserts `force == true`.
/// Test: this test.
#[tokio::test]
async fn insert_sends_force_true_for_realistic_low_prose_payload() {
    let (addr, state) = spawn_mock_server().await;
    let client = TrustyMemoryClient::new(format!("http://{addr}"));

    let payload = json!({"score": 0.42, "id": "a1b2", "tier": 3});
    client
        .insert(Segment::Brief, "rec-1", &[0.0], payload.clone())
        .await
        .expect("insert should succeed even for a low-prose JSON payload");

    let drawers = state.drawers.lock().unwrap();
    assert_eq!(drawers.len(), 1, "expected exactly one drawer");
    assert!(
        drawers[0].force,
        "TrustyMemoryClient::insert must set force:true so the real daemon's \
         quality gate does not reject JSON-shaped content"
    );
    let got: Value = serde_json::from_str(&drawers[0].content).unwrap();
    assert_eq!(got, payload);
}

/// Why: `get`/`delete` must behave correctly when nothing matches the
/// requested id instead of erroring.
/// What: `get` on a never-inserted id returns `None`; `delete` on it is
/// a silent no-op success.
/// Test: This test.
#[tokio::test]
async fn get_and_delete_are_clean_when_absent() {
    let (addr, _state) = spawn_mock_server().await;
    let client = TrustyMemoryClient::new(format!("http://{addr}"));

    assert_eq!(client.get(Segment::Brief, "missing").await.unwrap(), None);
    client.delete(Segment::Brief, "missing").await.unwrap();
}
