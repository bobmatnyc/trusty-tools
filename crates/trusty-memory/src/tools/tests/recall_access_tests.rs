//! #9881: a recall that serves a drawer records an access on it.
//!
//! Why: `Drawer::access_count` feeds the access boost that recall rank, dream
//! prune and fading read, but no production path ever raised it, so a drawer
//! recalled every day looked exactly as unused as one nobody had read.
//! What: drives the real MCP recall handlers through `dispatch_tool` against a
//! tempdir-rooted `AppState`, then drops the state and reads the drawer back
//! from a cold one, so only what reached disk can pass.
//! Test: this IS the test module.

use super::*;
use serde_json::Value;
use std::collections::HashMap;

/// Remember `text` in `palace` and return the new drawer's id.
async fn remember(state: &AppState, palace: &str, text: &str) -> String {
    let out = dispatch_tool(
        state,
        "memory_remember",
        json!({"palace": palace, "text": text, "room": "General"}),
    )
    .await
    .expect("memory_remember");
    out["drawer_id"].as_str().expect("drawer_id").to_string()
}

/// Ids of the hits a recall response served from a retrieval lane (layer >= 2).
fn lane_hits(out: &Value) -> Vec<(String, String)> {
    out["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter(|r| r["layer"].as_u64().expect("layer") >= 2)
        .map(|r| {
            let palace = r["palace_id"].as_str().unwrap_or_default().to_string();
            (
                palace,
                r["drawer_id"].as_str().expect("drawer_id").to_string(),
            )
        })
        .collect()
}

/// The persisted `(access_count, last_accessed_at.is_some())` of `drawer_id`,
/// read from a cold `AppState` over `root`.
fn persisted_access(root: &std::path::Path, palace: &str, drawer_id: &str) -> (u32, bool) {
    let cold = AppState::new(root.to_path_buf());
    cold.set_ready();
    let handle = crate::tools::helpers::open_palace_handle(&cold, palace).expect("reopen palace");
    let drawers = handle.drawers.read();
    let drawer = drawers
        .iter()
        .find(|d| d.id.to_string() == drawer_id)
        .expect("drawer survives the reopen");
    (drawer.access_count, drawer.last_accessed_at.is_some())
}

/// Why (#9881): the defect itself. Every recall left `access_count` at 0, so
/// the boost that keeps a used drawer out of dream prune never applied.
/// What: remembers one drawer, serves it through `memory_recall` twice and
/// `memory_recall_deep` once, counts how often it came back as a lane hit,
/// then drops the state and reads the drawer from disk. The persisted count
/// must equal the lane-hit count and `last_accessed_at` must be set.
/// Test: this function.
#[tokio::test]
async fn a_served_lane_hit_persists_its_access_count_across_reopen() {
    let (state, tmp) = test_state();
    dispatch_tool(&state, "palace_create", json!({"name": "acc"}))
        .await
        .expect("palace_create");
    let text = "The access counter rises each time recall serves this drawer";
    let id = remember(&state, "acc", text).await;

    let mut served = 0u32;
    for tool in ["memory_recall", "memory_recall", "memory_recall_deep"] {
        let out = dispatch_tool(
            &state,
            tool,
            json!({"palace": "acc", "query": text, "top_k": 3}),
        )
        .await
        .expect("recall");
        served += lane_hits(&out).iter().filter(|(_, d)| *d == id).count() as u32;
    }
    assert!(
        served >= 2,
        "the fixture must serve the drawer as a lane hit"
    );
    drop(state);

    let (count, stamped) = persisted_access(tmp.path(), "acc", &id);
    assert_eq!(count, served, "each served lane hit is one recorded access");
    assert!(stamped, "a served drawer carries its last access time");
}

/// Why (#9881, Q4): a cross-palace recall serves drawers from many palaces;
/// each access belongs to the palace that owns the drawer.
/// What: two palaces with one drawer each, one `memory_recall_all`. Every lane
/// hit it returns must be persisted, with count 1, in its own palace.
/// Test: this function.
#[tokio::test]
async fn recall_all_records_each_hit_in_its_own_palace() {
    let (state, tmp) = test_state();
    let text = "Cross palace recall records the access in the owning palace";
    let mut owned = HashMap::new();
    for palace in ["acc-a", "acc-b"] {
        dispatch_tool(&state, "palace_create", json!({"name": palace}))
            .await
            .expect("palace_create");
        owned.insert(remember(&state, palace, text).await, palace);
    }
    let out = dispatch_tool(&state, "memory_recall_all", json!({"q": text, "top_k": 10}))
        .await
        .expect("memory_recall_all");
    let hits = lane_hits(&out);
    for (palace, drawer) in &hits {
        assert_eq!(owned.get(drawer), Some(&palace.as_str()), "{out:#}");
    }
    assert_eq!(hits.len(), 2, "both palaces serve their drawer: {out:#}");
    drop(state);

    for (palace, drawer) in &hits {
        let (count, stamped) = persisted_access(tmp.path(), palace, drawer);
        assert_eq!((count, stamped), (1, true), "{palace}/{drawer}");
    }
}
