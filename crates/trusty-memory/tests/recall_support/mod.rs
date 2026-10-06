//! Shared fixture for the recall ranking tests (#8246, #9143).
//!
//! Why: `recall_temporal_rank.rs` and `recall_rulings_leg.rs` drive the same
//! palaces through the same tools; one copy of the helpers keeps them in step.
//! What: an `AppState` on a `TempDir` with the mock embedder seeded and an
//! explicit (possibly empty) rulings list, so the process environment never
//! leaks in; plus write, backdate and recall helpers over `dispatch_tool`.
//! Test: used by `recall_temporal_rank.rs` and `recall_rulings_leg.rs`.
#![allow(dead_code)] // each binary uses a different subset

use chrono::{Duration, Utc};
use serde_json::{json, Value};
use tempfile::TempDir;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;
use uuid::Uuid;

/// A ready `AppState` on `tmp` with each named palace created.
pub async fn state_with(tmp: &TempDir, palaces: &[&str], rulings: &[&str]) -> AppState {
    seed_shared_embedder_with_mock();
    let state = AppState::new(tmp.path().to_path_buf())
        .with_rulings_palaces(rulings.iter().map(|p| p.to_string()).collect());
    state.set_ready();
    create_palaces(&state, tmp, palaces).await;
    state
}

/// Create each named palace through `palace_create`.
pub async fn create_palaces(state: &AppState, tmp: &TempDir, palaces: &[&str]) {
    let cwd = tmp.path().to_string_lossy().to_string();
    for name in palaces {
        dispatch_tool(
            state,
            "palace_create",
            json!({ "name": name, "force": true, "cwd": cwd }),
        )
        .await
        .expect("palace_create");
    }
}

/// Write one drawer through `memory_remember`; returns its id.
pub async fn remember(
    state: &AppState,
    palace: &str,
    text: &str,
    tags: &[&str],
    key: Option<&str>,
) -> Uuid {
    let mut args = json!({ "palace": palace, "text": text, "tags": tags, "force": true });
    if let Some(k) = key {
        args["fact_key"] = json!(k);
    }
    let out = dispatch_tool(state, "memory_remember", args)
        .await
        .expect("memory_remember");
    out["drawer_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("drawer_id")
}

/// Move a drawer's `created_at` by `age` into the past (negative: future).
pub fn backdate(state: &AppState, palace: &str, id: Uuid, age: Duration) {
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace))
        .expect("open palace");
    let mut drawers = handle.drawers.write();
    let d = drawers
        .iter_mut()
        .find(|d| d.id == id)
        .expect("drawer present");
    d.created_at = Utc::now() - age;
}

/// The stored `fact_key` of one drawer.
pub fn fact_key(state: &AppState, palace: &str, id: Uuid) -> Option<String> {
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace))
        .expect("open palace");
    let drawers = handle.drawers.read();
    drawers
        .iter()
        .find(|d| d.id == id)
        .expect("drawer present")
        .fact_key
        .clone()
}

/// The full `memory_recall` envelope.
pub async fn recall_envelope(state: &AppState, args: Value) -> Value {
    dispatch_tool(state, "memory_recall", args)
        .await
        .expect("memory_recall never errors on a rulings failure")
}

/// The `results` of a plain `memory_recall`.
pub async fn recall(state: &AppState, palace: &str, query: &str, top_k: u64) -> Vec<Value> {
    let out = recall_envelope(
        state,
        json!({ "palace": palace, "query": query, "top_k": top_k }),
    )
    .await;
    out["results"].as_array().expect("results").clone()
}

/// Rank of the hit with drawer `id` (`drawer_id` or `id` key), or `None`.
pub fn rank_of(results: &[Value], id: Uuid) -> Option<usize> {
    let id = id.to_string();
    results.iter().position(|r| {
        r["drawer_id"].as_str() == Some(id.as_str()) || r["id"].as_str() == Some(id.as_str())
    })
}
