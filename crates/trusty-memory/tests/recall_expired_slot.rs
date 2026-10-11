//! An expired Tier C slot is not returned by recall on the embedder-ready
//! path (#9390).
//!
//! Why: the issue's acceptance line "an expired slot is not returned by
//! recall" held at the retrieval layer (`retrieve_l0_l1`, `score_candidates`)
//! but no test drove it through a recall surface. The warming-path twin lives
//! in `recall_degraded_lane.rs`, whose binary must keep the embedder cold.
//! What: two slots written through `memory_remember` with a `fact_key`, one
//! then moved past its `expires_at` in the drawer table, recalled on every
//! surface. Its own binary, because it seeds the mock embedder.
//! Test: this IS the test module.

mod recall_support;

use chrono::{Duration, Utc};
use recall_support::{rank_of, recall_on, remember, state_with, Surface};
use serde_json::json;
use trusty_common::memory_core::palace::PalaceId;
use trusty_memory::tools::dispatch_tool;

/// Why (#9390): the ready path must exclude an expired slot on every surface,
/// and must not do it by dropping every slot.
/// What: on all five recall surfaces the expired id is absent and the live id
/// present; `memory_list` still lists the expired drawer (excluded, not
/// deleted).
#[tokio::test]
async fn an_expired_slot_is_not_returned_on_any_ready_recall_surface() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let expired = remember(
        &state,
        "project-a",
        "release head sha for pr one is abc123",
        &[],
        Some("pr:1/state"),
    )
    .await;
    let live = remember(
        &state,
        "project-a",
        "release head sha for pr two is def456",
        &[],
        Some("pr:2/state"),
    )
    .await;
    // A past `expires_at` is refused at admission, so expire it in place.
    {
        let handle = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new("project-a"))
            .expect("open palace");
        let mut drawers = handle.drawers.write();
        let d = drawers
            .iter_mut()
            .find(|d| d.id == expired)
            .expect("drawer present");
        assert_eq!(d.fact_key.as_deref(), Some("pr:1/state"), "a Tier C slot");
        d.expires_at = Some(Utc::now() - Duration::hours(1));
    }

    for surface in [
        Surface::McpRecall,
        Surface::McpRecallDeep,
        Surface::McpRecallAll,
        Surface::ServiceRecall,
        Surface::ServiceRecallAll,
    ] {
        let results = recall_on(&state, surface, "project-a", "release head sha").await;
        assert!(
            rank_of(&results, expired).is_none(),
            "{surface:?}: an expired slot must not be recalled: {results:#?}"
        );
        assert!(
            rank_of(&results, live).is_some(),
            "{surface:?}: an unexpired slot must still be recalled: {results:#?}"
        );
    }

    let listed = dispatch_tool(&state, "memory_list", json!({ "palace": "project-a" }))
        .await
        .expect("memory_list");
    let still_listed = listed["drawers"]
        .as_array()
        .expect("drawers")
        .iter()
        .any(|d| d["drawer_id"].as_str() == Some(expired.to_string().as_str()));
    assert!(still_listed, "expired slot still listed: {listed:#}");
}
