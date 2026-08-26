//! Tests for `TrustyMemoryClient` and `MemoryBackend` (issue #3225).
//!
//! Why: split out of `mod.rs` to keep the production file under the 500-SLOC
//! cap (mirrors the `redb_usearch/{mod,tests}.rs` split already established in
//! this crate).
//! What: unit tests for `ns_id`/health-check/auto_detect plus a mock-daemon
//! round-trip proving insert/get/delete/search against the real method names.
//! The mock serves a real Unix socket since #6286 — the REST routes it used to
//! mount no longer exist.
//! Test: this *is* the test module.

use super::*;
use crate::uds_mock::{self, MockMemoryDaemon, RpcError};
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;

/// A socket path nothing can be serving.
///
/// Why (#6286): the retired rigs pointed at `127.0.0.1:19999` — a port assumed
/// unused — so a dial failed immediately rather than timing out. A path under a
/// directory that cannot exist is the socket equivalent, and unlike a port it
/// cannot be taken by something else on the machine.
fn unreachable_socket() -> &'static Path {
    Path::new("/nonexistent/trusty-memory/trusty-memory.sock")
}

/// Why: If the daemon is not running we must report unreachable rather
/// than block startup. Pointing at a socket nothing serves is the simplest way
/// to verify that without spinning up real infra.
/// What: Build a client on an absent socket and call `health_check`; assert
/// false.
/// Test: This test.
#[tokio::test]
async fn health_check_false_when_daemon_absent() {
    let client = TrustyMemoryClient::new(unreachable_socket());
    assert!(!client.health_check().await);
}

/// Why: `auto_detect` is the single entry point callers use; if it
/// silently chose Trusty when the daemon is down, every subsequent
/// memory call would fail. Verify the fallback path picks Local.
/// What: Open a temp `RedbUsearchStore`, call `auto_detect_at` pointing at the
/// same dead socket, and assert the resulting backend is `Local`.
/// Test: This test.
#[tokio::test]
async fn auto_detect_falls_back_to_local() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(RedbUsearchStore::open(dir.path(), 4).unwrap());
    let backend = MemoryBackend::auto_detect_at(unreachable_socket(), store).await;
    match backend {
        MemoryBackend::Local(_) => {}
        MemoryBackend::Trusty(_) => panic!("expected Local fallback"),
    }
}

/// Why (issue #3336, then #6286): this defaulted to a hard-coded `7775` that
/// had drifted from the daemon's real bind port, so auto-detection probed a
/// port nothing listens on and every install fell back to the embedded store.
/// The fix pinned it to `trusty_memory::DEFAULT_HTTP_PORT`; ADR-0032 removed
/// both the port and the constant, and the path is now derived — so there is no
/// literal left to drift. What can still go wrong is the client and the daemon
/// deriving DIFFERENT paths, which this pins by asserting the client's answer
/// against `trusty_memory::socket_path`, the daemon's own call.
/// Test: This test.
#[test]
fn default_trusty_socket_is_the_derived_daemon_path() {
    let expected = trusty_memory::socket_path().expect("the daemon resolves its own socket");
    assert_eq!(default_trusty_socket(), expected);
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
/// What: Call `search` on a client pointed at an absent socket (the error must
/// come from the "not supported" short-circuit, not a transport failure — this
/// must be true even with zero I/O).
/// Test: This test.
#[tokio::test]
async fn search_returns_descriptive_unsupported_error() {
    let client = TrustyMemoryClient::new(unreachable_socket());
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
// Mock daemon mirroring trusty-memory's REAL method names.
//
// Why: `trusty-agents` cannot take a dev-dependency on `trusty-memory` without
// an awkward cross-crate coupling for a single test module (pulling in the
// whole daemon crate and its ONNX embedder just to prove five methods and their
// DTO shapes are wired correctly is disproportionate). Before #6286 this served
// trusty-memory's five REST routes with axum; those routes are gone, so it
// serves the five methods that replaced them over the same framed socket the
// real daemon binds: `memory.health`, `palace_create`, `memory.drawer_create`,
// `memory.drawers_list` and `memory.drawer_delete`.
// What: An in-memory drawer store keyed by tag, guarded by a
// `std::sync::Mutex`, mounted as the mock router's catch-all.
// -----------------------------------------------------------------

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

async fn spawn_mock_server() -> (MockMemoryDaemon, Arc<MockState>) {
    let state = Arc::new(MockState::default());
    let served = Arc::clone(&state);

    let daemon = uds_mock::spawn(move |method: &str, params: Value| {
        let state = Arc::clone(&served);
        let method = method.to_string();
        Box::pin(async move {
            match method.as_str() {
                "memory.health" => Ok(json!({"status": "ok"})),
                "palace_create" => Ok(json!({"id": params["name"].clone()})),
                "memory.drawer_create" => {
                    let id = Uuid::new_v4();
                    state.drawers.lock().unwrap().push(MockDrawer {
                        id,
                        content: params["content"].as_str().unwrap_or_default().to_string(),
                        tags: params["tags"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        force: params["force"].as_bool().unwrap_or(false),
                    });
                    Ok(json!({"id": id}))
                }
                "memory.drawers_list" => {
                    let wanted = params["tag"].as_str().map(str::to_string);
                    let drawers = state.drawers.lock().unwrap();
                    let rows: Vec<Value> = drawers
                        .iter()
                        .filter(|d| match &wanted {
                            Some(t) => d.tags.iter().any(|x| x == t),
                            None => true,
                        })
                        .map(|d| json!({"id": d.id, "content": d.content, "tags": d.tags}))
                        .collect();
                    Ok(Value::Array(rows))
                }
                "memory.drawer_delete" => {
                    let wanted = params["drawer_id"].as_str().unwrap_or_default().to_string();
                    state
                        .drawers
                        .lock()
                        .unwrap()
                        .retain(|d| d.id.to_string() != wanted);
                    Ok(json!({"deleted": true}))
                }
                other => Err(RpcError::method_not_found(other, &[])),
            }
        })
    })
    .await;

    (daemon, state)
}

/// Why: this is the core regression test for issue #3225 — proves
/// `auto_detect` selects the HTTP backend (not the local fallback) when
/// the daemon answers `memory.health` on its socket.
/// What: Spin the mock daemon, `auto_detect_at` against it, assert
/// `MemoryBackend::Trusty`.
/// Test: This test.
#[tokio::test]
async fn auto_detect_selects_trusty_when_daemon_reachable() {
    let (daemon, _state) = spawn_mock_server().await;
    let dir = TempDir::new().unwrap();
    let store = Arc::new(RedbUsearchStore::open(dir.path(), 4).unwrap());
    let backend = MemoryBackend::auto_detect_at(daemon.socket(), store).await;
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
    let (daemon, _state) = spawn_mock_server().await;
    let client = TrustyMemoryClient::new(daemon.socket());

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
    let (daemon, _state) = spawn_mock_server().await;
    let client = TrustyMemoryClient::new(daemon.socket());

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
    let (daemon, state) = spawn_mock_server().await;
    let client = TrustyMemoryClient::new(daemon.socket());

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
    let (daemon, _state) = spawn_mock_server().await;
    let client = TrustyMemoryClient::new(daemon.socket());

    assert_eq!(client.get(Segment::Brief, "missing").await.unwrap(), None);
    client.delete(Segment::Brief, "missing").await.unwrap();
}
