//! Unit tests for `tools::recall_supersede_view` (#10609, ADR-0074).

use super::*;
use crate::engine::palace::Drawer;
use crate::engine::retrieval::RecallResult;
use crate::tools::recall_supersede::{LOOKUP_BUDGET, SupersessionLookup, fail_open};
use serde_json::json;
use std::time::Duration;
use uuid::Uuid;

fn hit(marked_by: Option<Uuid>) -> RecallResult {
    let mut drawer = Drawer::new(Uuid::new_v4(), "fact");
    if let Some(new) = marked_by {
        drawer.mark_retired_by(new);
    }
    RecallResult {
        drawer,
        score: 0.8,
        layer: 2,
    }
}

#[test]
fn include_superseded_defaults_to_false() {
    assert!(!include_superseded_arg(&json!({})));
    assert!(!include_superseded_arg(
        &json!({"include_superseded": "yes"})
    ));
    assert!(include_superseded_arg(&json!({"include_superseded": true})));
}

/// Why (#10609): a slot retiree carries its marker on the row, so it is
/// excluded with an EMPTY edge map, which is what a failed KG lookup returns.
#[test]
fn a_marked_drawer_is_excluded_without_any_edge() {
    let new = Uuid::new_v4();
    let (retiree, live) = (hit(Some(new)), hit(None));
    let live_id = live.drawer.id;
    let mut results = vec![retiree, live];
    let mut sup = Supersessions::new();
    let removed = settle_superseded(&mut results, &mut sup, false);
    assert_eq!(removed, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].drawer.id, live_id);
}

/// Why (D83): a supersession no slot retirement made, known only from the KG
/// edge, is excluded by default too.
#[test]
fn an_edge_only_supersession_is_excluded() {
    let (old, live) = (hit(None), hit(None));
    let (old_id, live_id) = (old.drawer.id, live.drawer.id);
    let mut results = vec![old, live];
    let mut sup = Supersessions::from([(old_id, live_id)]);
    assert_eq!(settle_superseded(&mut results, &mut sup, false), 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].drawer.id, live_id);
}

#[test]
fn include_superseded_keeps_and_records_the_replacement() {
    let new = Uuid::new_v4();
    let retiree = hit(Some(new));
    let id = retiree.drawer.id;
    let mut results = vec![retiree];
    let mut sup = Supersessions::new();
    assert_eq!(settle_superseded(&mut results, &mut sup, true), 0);
    assert_eq!(results.len(), 1);
    assert_eq!(sup.get(&id), Some(&new));

    let mut out = json!({"results": [{"drawer_id": id.to_string()}, {"drawer_id": Uuid::new_v4().to_string()}]});
    annotate_response(&mut out, &sup, false);
    assert_eq!(out["results"][0]["superseded"], json!(true));
    assert_eq!(out["results"][0]["superseded_by"], json!(new.to_string()));
    assert!(out["results"][1].get("superseded").is_none());
    assert!(out.get("supersession_unverified").is_none());
}

#[test]
fn an_unverified_lookup_is_flagged_on_the_response() {
    let mut out = json!({"results": []});
    annotate_response(&mut out, &Supersessions::new(), true);
    assert_eq!(out["supersession_unverified"], json!(true));
}

/// Settle a marked retiree, an edge-only superseded drawer and a live drawer
/// against a lookup that failed, and report what a caller would see.
fn settle_after_failed_lookup(found: SupersessionLookup) -> (Vec<Uuid>, Value, Uuid, Uuid) {
    let (retiree, edge_only, live) = (hit(Some(Uuid::new_v4())), hit(None), hit(None));
    let (retiree_id, edge_only_id, live_id) =
        (retiree.drawer.id, edge_only.drawer.id, live.drawer.id);
    assert!(found.unverified && found.edges.is_empty());
    let mut results = vec![retiree, edge_only, live];
    let mut sup = found.edges;
    settle_superseded(&mut results, &mut sup, false);
    let kept: Vec<Uuid> = results.iter().map(|r| r.drawer.id).collect();
    let rows: Vec<Value> = kept
        .iter()
        .map(|id| json!({ "drawer_id": id.to_string() }))
        .collect();
    let mut out = json!({ "results": rows });
    annotate_response(&mut out, &sup, true);
    assert!(
        !kept.contains(&retiree_id),
        "a slot retiree leaked: {kept:?}"
    );
    (kept, out, edge_only_id, live_id)
}

/// Why (#10609): a KG read error must not reproduce the field bug. The slot
/// retiree is excluded from its row marker; a supersession known only from
/// the KG cannot be checked, so it is kept and the response says so.
#[tokio::test]
async fn a_lookup_error_does_not_leak_a_slot_retiree() {
    let found = fail_open(
        "p-10609",
        async { Err(anyhow::anyhow!("kg.redb unreadable")) },
        LOOKUP_BUDGET,
    )
    .await;
    let (kept, out, edge_only, live) = settle_after_failed_lookup(found);
    assert!(kept.contains(&edge_only) && kept.contains(&live));
    assert_eq!(out["supersession_unverified"], json!(true));
}

/// Why (#10609): the same for a lookup that runs past its budget.
#[tokio::test]
async fn a_lookup_overrun_does_not_leak_a_slot_retiree() {
    let stalled = std::future::pending::<anyhow::Result<Supersessions>>();
    let found = fail_open("p-10609", stalled, Duration::from_millis(20)).await;
    let (kept, out, _, _) = settle_after_failed_lookup(found);
    assert_eq!(kept.len(), 2);
    assert_eq!(out["supersession_unverified"], json!(true));
}
