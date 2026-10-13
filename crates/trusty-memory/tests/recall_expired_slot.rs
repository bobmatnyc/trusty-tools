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
use recall_support::{Surface, rank_of, recall_on, remember, state_with};
use serde_json::json;
use trusty_memory::engine::palace::PalaceId;
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

/// How many expired slots outrank the live one: more than the widest first
/// fetch any surface makes at `top_k` 5 (L2's `3 * ranking_window`, 30).
const EXPIRED_PILE: usize = 40;

/// Why (#9390, PR A critic MEDIUM): expired Tier C slots are never deleted
/// (ADR-0028 D6), so a pile of them that matches the query better than the
/// live slot fills the vector lane's fetched window, the expiry filter empties
/// it, and the live slot is never returned.
/// What: one live slot written first, then [`EXPIRED_PILE`] slots on other
/// keys whose text is closer to the query, each moved past its `expires_at`.
/// Every ready recall surface must still return the live slot and no expired
/// one.
#[tokio::test]
async fn recall_returns_the_live_slot_past_twenty_expired_ones() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let live = remember(
        &state,
        "project-a",
        "release head sha for the live resume slot",
        &[],
        Some("pr:999/state"),
    )
    .await;
    let mut expired = Vec::new();
    for n in 0..EXPIRED_PILE {
        let key = format!("pr:{n}/state");
        let text = format!("release head sha {n}");
        expired.push(remember(&state, "project-a", &text, &[], Some(&key)).await);
    }
    {
        let handle = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new("project-a"))
            .expect("open palace");
        let mut drawers = handle.drawers.write();
        for d in drawers.iter_mut().filter(|d| expired.contains(&d.id)) {
            d.expires_at = Some(Utc::now() - Duration::hours(1));
        }
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
            rank_of(&results, live).is_some(),
            "{surface:?}: the live slot must be recalled past {EXPIRED_PILE} expired ones: \
             {results:#?}"
        );
        for id in &expired {
            assert!(
                rank_of(&results, *id).is_none(),
                "{surface:?}: an expired slot was recalled: {results:#?}"
            );
        }
    }
}
