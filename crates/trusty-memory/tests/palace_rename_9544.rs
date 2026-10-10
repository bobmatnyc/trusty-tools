//! #9544 (PR-C2): `palace_rename` through the tool and raw-RPC surfaces.
//!
//! Why: the rename's promise is about what callers see afterwards — the old id
//! keeps answering, nothing is lost or duplicated, `<root>/<old>` never comes
//! back — and only the public surfaces can show that.
//! What: drives `dispatch_tool` and `transport::dispatch` against an
//! `AppState` with the BM25 lane armed. Its own binary because it seeds the
//! process-wide mock embedder.
//! Test: this *is* the test file.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_memory::bm25_lane::Bm25Lane;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::transport::{dispatch, JsonRpcRequest};
use trusty_memory::AppState;

struct Fixture {
    _tmp: tempfile::TempDir,
    state: AppState,
}

impl Fixture {
    fn new() -> Self {
        trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let lane = Bm25Lane::with_limits(root.clone(), 3, None);
        let state = AppState::new(root).with_bm25_lane(lane);
        state.set_ready();
        Self { _tmp: tmp, state }
    }

    fn palace(&self, id: &str) {
        let root = &self.state.data_root;
        self.state
            .registry
            .create_palace(
                root,
                Palace {
                    id: PalaceId::new(id),
                    name: id.to_string(),
                    description: None,
                    created_at: chrono::Utc::now(),
                    data_dir: root.join(id),
                },
            )
            .expect("create palace");
    }

    async fn tool(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        dispatch_tool(&self.state, name, args).await
    }

    async fn drawer_ids(&self, palace: &str) -> Vec<String> {
        let listed = self
            .tool("memory_list", json!({"palace": palace, "limit": 1000}))
            .await
            .expect("memory_list");
        listed["drawers"]
            .as_array()
            .expect("drawers")
            .iter()
            .filter_map(|d| d["drawer_id"].as_str().map(str::to_string))
            .collect()
    }

    async fn shutdown(&self) {
        if let Some(lane) = self.state.bm25_lane() {
            lane.shutdown().await;
        }
    }
}

fn note(i: usize) -> String {
    format!("palace rename integration note {i} carries a separate durable fact for recall")
}

/// Why (#9544, A2): after a rename every read and write through the old id
/// must reach the renamed palace, and nothing — the write path, the BM25 lane,
/// a session store — may recreate `<root>/<old>`.
/// What: seeds three notes, renames through `dispatch_tool`, then recalls and
/// remembers through the old id, checks the new palace holds four drawers,
/// that no old dir exists, and that creating the old id is refused.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_old_id_reads_and_writes_land_in_new_and_old_dir_not_recreated() {
    let fx = Fixture::new();
    fx.palace("rw-src");
    for i in 0..3 {
        fx.tool("memory_remember", json!({"palace": "rw-src", "text": note(i)}))
            .await
            .expect("remember");
    }
    let out = fx
        .tool("palace_rename", json!({"palace_id": "rw-src", "new_id": "rw-dst"}))
        .await
        .expect("palace_rename");
    assert_eq!(out["new"], "rw-dst", "{out}");

    assert_eq!(fx.drawer_ids("rw-src").await.len(), 3, "a read through the old id");
    // The recall also runs the BM25 lane under the old id.
    fx.tool(
        "memory_recall",
        json!({"palace": "rw-src", "query": "integration note durable fact", "top_k": 10}),
    )
    .await
    .expect("recall through the old id");
    fx.tool("memory_remember", json!({"palace": "rw-src", "text": note(3)}))
        .await
        .expect("remember through the old id");
    assert_eq!(fx.drawer_ids("rw-dst").await.len(), 4);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let root = &fx.state.data_root;
    assert!(!root.join("rw-src").exists(), "the old palace dir was recreated");

    let created = fx
        .tool("palace_create", json!({"name": "rw-src", "force": true}))
        .await;
    let refused = created.expect_err("creating the old id must be refused");
    assert!(format!("{refused:#}").contains("alias"), "{refused:#}");
    fx.shutdown().await;
}

/// Why (#9544, A2/A6): writes racing a rename must each land exactly once —
/// an acknowledged write that vanishes, or lands twice, is the failure.
/// What: a writer task remembers distinct notes through the old id while the
/// rename runs; every acknowledged drawer id must be in the new palace exactly
/// once, and the palace must hold nothing else.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_concurrent_remember_loses_and_duplicates_nothing() {
    let fx = Arc::new(Fixture::new());
    fx.palace("race-src");
    let writer = {
        let fx = Arc::clone(&fx);
        tokio::spawn(async move {
            let mut acked = Vec::new();
            for i in 0..30 {
                let out = fx
                    .tool("memory_remember", json!({"palace": "race-src", "text": note(i)}))
                    .await;
                if let Ok(out) = out {
                    if let Some(id) = out["drawer_id"].as_str() {
                        acked.push(id.to_string());
                    }
                }
            }
            acked
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    fx.tool("palace_rename", json!({"palace_id": "race-src", "new_id": "race-dst"}))
        .await
        .expect("palace_rename");
    let acked = writer.await.expect("join");
    assert!(!acked.is_empty(), "no write was acknowledged");

    let stored = fx.drawer_ids("race-dst").await;
    let unique: HashSet<&String> = stored.iter().collect();
    assert_eq!(unique.len(), stored.len(), "a drawer was stored twice: {stored:?}");
    for id in &acked {
        assert!(unique.contains(id), "acknowledged drawer {id} is missing");
    }
    assert_eq!(stored.len(), acked.len(), "the palace holds unacknowledged drawers");
    assert!(!fx.state.data_root.join("race-src").exists());
    fx.shutdown().await;
}

/// Why (#9544, A3): `tm memory rename` calls the raw method; a missing source
/// must read as "not found" (-32004), not a daemon fault.
/// Test: itself.
#[tokio::test]
async fn palace_rename_not_found_is_32004() {
    let fx = Fixture::new();
    let req = JsonRpcRequest {
        jsonrpc: Some("2.0".to_string()),
        id: Some(json!(1)),
        method: "palace_rename".to_string(),
        params: Some(json!({"palace_id": "nowhere", "new_id": "somewhere"})),
    };
    let err = dispatch(&fx.state, req).await.error.expect("error");
    assert_eq!(err.code, -32004, "{}", err.message);
    fx.shutdown().await;
}
