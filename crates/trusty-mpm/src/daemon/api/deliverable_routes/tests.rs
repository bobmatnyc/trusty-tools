//! Handler tests for the Deliverable/Milestone CRUD routes (#2378, #2380).
//!
//! Why: these drive every handler in-process against a temp-rooted `DaemonState`
//! so the CRUD contract and — critically — the #2380 status-transition
//! enforcement are exercised end-to-end through the exact code the router calls.
//! What: create/get/list/patch happy paths, 404 scoping, and the full illegal-
//! transition rejection path (including the structured `allowed_next` body).
//! Test: this IS the test module.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{Json, http::StatusCode};
use chrono::Utc;
use tempfile::TempDir;

use super::*;
use crate::daemon::state::DaemonState;
use crate::deliverable::{DeliverableKind, DeliverableStatus, EstimationTier, MilestoneStatus};

/// Build a temp-rooted daemon state; the deliverable stores live under its root.
fn state() -> (Arc<DaemonState>, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let st = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    (st, dir)
}

fn create_body(name: &str) -> CreateDeliverable {
    CreateDeliverable {
        name: name.into(),
        description: "desc".into(),
        kind: DeliverableKind::Feature,
        estimated_effort: EstimationTier::M,
        ticket_ref: Some("#2117".into()),
        spec_ref: Some("docs/specs/tm-project-control-plane.md".into()),
        target_date: None,
    }
}

async fn create(st: &Arc<DaemonState>, project: &str, name: &str) -> Deliverable {
    let (code, Json(d)) = create_deliverable(
        State(Arc::clone(st)),
        Path(project.to_string()),
        Ok(Json(create_body(name))),
    )
    .await
    .expect("create ok");
    assert_eq!(code, StatusCode::CREATED);
    d
}

#[tokio::test]
async fn create_then_get_round_trips() {
    let (st, _g) = state();
    let created = create(&st, "trusty-tools", "OAuth flow").await;
    assert_eq!(created.status, DeliverableStatus::Proposed);
    assert_eq!(created.project_name, "trusty-tools");

    let Json(got) = get_deliverable(
        State(Arc::clone(&st)),
        Path(("trusty-tools".into(), created.id.to_string())),
    )
    .await
    .expect("get ok");
    assert_eq!(got, created);
}

#[tokio::test]
async fn create_rejects_empty_name() {
    let (st, _g) = state();
    let err = create_deliverable(State(st), Path("p".into()), Ok(Json(create_body("   "))))
        .await
        .expect_err("empty name rejected");
    assert!(matches!(err, DaemonError::InvalidRequest(_)));
}

#[tokio::test]
async fn list_filters_by_status_and_scopes_to_project() {
    let (st, _g) = state();
    let d1 = create(&st, "p1", "a").await;
    let _d2 = create(&st, "p1", "b").await;
    let _other = create(&st, "p2", "c").await;

    // Move d1 to in-progress so a status filter can distinguish it.
    let Json(_moved) = patch_deliverable(
        State(Arc::clone(&st)),
        Path(("p1".into(), d1.id.to_string())),
        Ok(Json(PatchDeliverable {
            status: Some(DeliverableStatus::InProgress),
            ..Default::default()
        })),
    )
    .await
    .expect("patch ok");

    // Project scoping: p1 has two, p2 has one.
    let Json(all_p1) = list_deliverables(
        State(Arc::clone(&st)),
        Path("p1".into()),
        Query(DeliverableListQuery { status: None }),
    )
    .await
    .expect("list ok");
    assert_eq!(all_p1.deliverables.len(), 2);

    // Status filter: only the in-progress one.
    let Json(in_prog) = list_deliverables(
        State(Arc::clone(&st)),
        Path("p1".into()),
        Query(DeliverableListQuery {
            status: Some(DeliverableStatus::InProgress),
        }),
    )
    .await
    .expect("list ok");
    assert_eq!(in_prog.deliverables.len(), 1);
    assert_eq!(in_prog.deliverables[0].id, d1.id);
}

#[tokio::test]
async fn get_unknown_deliverable_is_404() {
    let (st, _g) = state();
    let err = get_deliverable(
        State(st),
        Path(("p".into(), uuid::Uuid::new_v4().to_string())),
    )
    .await
    .expect_err("unknown 404");
    assert!(matches!(err, DaemonError::DeliverableNotFound { .. }));
}

#[tokio::test]
async fn get_wrong_project_is_404() {
    let (st, _g) = state();
    let d = create(&st, "right", "x").await;
    // Same id, wrong project path → must 404, never leak the record.
    let err = get_deliverable(
        State(Arc::clone(&st)),
        Path(("wrong".into(), d.id.to_string())),
    )
    .await
    .expect_err("wrong project 404");
    assert!(matches!(err, DaemonError::DeliverableNotFound { .. }));
}

#[tokio::test]
async fn patch_updates_fields() {
    let (st, _g) = state();
    let d = create(&st, "p", "orig").await;
    let Json(updated) = patch_deliverable(
        State(Arc::clone(&st)),
        Path(("p".into(), d.id.to_string())),
        Ok(Json(PatchDeliverable {
            name: Some("renamed".into()),
            estimated_effort: Some(EstimationTier::Xl),
            ..Default::default()
        })),
    )
    .await
    .expect("patch ok");
    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.estimated_effort, EstimationTier::Xl);
    // Status untouched by a field-only patch.
    assert_eq!(updated.status, DeliverableStatus::Proposed);
}

#[tokio::test]
async fn patch_full_legal_lifecycle_succeeds() {
    let (st, _g) = state();
    let d = create(&st, "p", "x").await;
    let id = d.id.to_string();
    // proposed → in-progress → blocked → in-progress → complete → shipped
    for step in [
        DeliverableStatus::InProgress,
        DeliverableStatus::Blocked,
        DeliverableStatus::InProgress,
        DeliverableStatus::Complete,
        DeliverableStatus::Shipped,
    ] {
        let Json(updated) = patch_deliverable(
            State(Arc::clone(&st)),
            Path(("p".into(), id.clone())),
            Ok(Json(PatchDeliverable {
                status: Some(step),
                ..Default::default()
            })),
        )
        .await
        .unwrap_or_else(|e| panic!("legal transition to {step} failed: {e}"));
        assert_eq!(updated.status, step);
    }
}

#[tokio::test]
async fn patch_illegal_transition_is_409_with_allowed_next() {
    let (st, _g) = state();
    let d = create(&st, "p", "x").await;
    // proposed → complete is the illegal case named in #2380.
    let err = patch_deliverable(
        State(Arc::clone(&st)),
        Path(("p".into(), d.id.to_string())),
        Ok(Json(PatchDeliverable {
            status: Some(DeliverableStatus::Complete),
            ..Default::default()
        })),
    )
    .await
    .expect_err("illegal transition rejected");

    match &err {
        DaemonError::InvalidTransition { from, to, allowed } => {
            assert_eq!(from, "proposed");
            assert_eq!(to, "complete");
            assert_eq!(allowed, &vec!["in-progress".to_string()]);
        }
        other => panic!("expected InvalidTransition, got {other:?}"),
    }
    assert_eq!(err.status(), StatusCode::CONFLICT);

    // The wire body must carry the structured `allowed_next` array (#2380).
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    assert_eq!(json["from"], "proposed");
    assert_eq!(json["to"], "complete");
    assert_eq!(json["allowed_next"][0], "in-progress");
    assert!(
        json["error"].as_str().unwrap().contains("in-progress"),
        "error message names legal next state"
    );

    // The rejected transition must NOT have mutated the record.
    let Json(unchanged) =
        get_deliverable(State(Arc::clone(&st)), Path(("p".into(), d.id.to_string())))
            .await
            .expect("get ok");
    assert_eq!(unchanged.status, DeliverableStatus::Proposed);
}

#[tokio::test]
async fn patch_same_status_is_noop() {
    let (st, _g) = state();
    let d = create(&st, "p", "x").await;
    // Echoing the current status (proposed) alongside a field edit must succeed —
    // a self-"transition" is a no-op, not a rejected transition.
    let Json(updated) = patch_deliverable(
        State(Arc::clone(&st)),
        Path(("p".into(), d.id.to_string())),
        Ok(Json(PatchDeliverable {
            name: Some("edited".into()),
            status: Some(DeliverableStatus::Proposed),
            ..Default::default()
        })),
    )
    .await
    .expect("no-op status patch ok");
    assert_eq!(updated.name, "edited");
    assert_eq!(updated.status, DeliverableStatus::Proposed);
}

#[tokio::test]
async fn patch_deliverable_rejects_blank_name() {
    // #2395 review MEDIUM: create enforces non-empty trimmed name; PATCH must
    // apply the same rule rather than allowing a record to end up blank.
    let (st, _g) = state();
    let d = create(&st, "p", "x").await;
    let err = patch_deliverable(
        State(Arc::clone(&st)),
        Path(("p".into(), d.id.to_string())),
        Ok(Json(PatchDeliverable {
            name: Some("   ".into()),
            ..Default::default()
        })),
    )
    .await
    .expect_err("blank name on PATCH must be rejected");
    assert!(matches!(err, DaemonError::InvalidRequest(_)));

    // The rejection must happen before touching the store — the record is
    // untouched.
    let Json(unchanged) =
        get_deliverable(State(Arc::clone(&st)), Path(("p".into(), d.id.to_string())))
            .await
            .expect("get ok");
    assert_eq!(unchanged.name, "x");
}

// ───────────────────────── Milestones ────────────────────────────────

#[tokio::test]
async fn milestone_create_get_list_patch() {
    let (st, _g) = state();
    let (code, Json(m)) = create_milestone(
        State(Arc::clone(&st)),
        Path("p".into()),
        Ok(Json(CreateMilestone {
            name: "v1.0 Alpha".into(),
            description: "first".into(),
            target_date: Utc::now(),
            deliverables: vec![],
            status: MilestoneStatus::Proposed,
        })),
    )
    .await
    .expect("create milestone");
    assert_eq!(code, StatusCode::CREATED);

    let Json(got) = get_milestone(State(Arc::clone(&st)), Path(("p".into(), m.id.to_string())))
        .await
        .expect("get milestone");
    assert_eq!(got.name, "v1.0 Alpha");

    let Json(list) = list_milestones(State(Arc::clone(&st)), Path("p".into()))
        .await
        .expect("list milestones");
    assert_eq!(list.milestones.len(), 1);

    // Milestone status is a rollup field: it is settable directly with no
    // transition enforcement (§10.3), unlike Deliverable.
    let Json(patched) = patch_milestone(
        State(Arc::clone(&st)),
        Path(("p".into(), m.id.to_string())),
        Ok(Json(PatchMilestone {
            name: Some("v1.0".into()),
            status: Some(MilestoneStatus::Shipped),
            ..Default::default()
        })),
    )
    .await
    .expect("patch milestone");
    assert_eq!(patched.name, "v1.0");
    assert_eq!(patched.status, MilestoneStatus::Shipped);
}

#[tokio::test]
async fn patch_milestone_rejects_blank_name() {
    // #2395 review MEDIUM: same rule as `patch_deliverable_rejects_blank_name`.
    let (st, _g) = state();
    let (_code, Json(m)) = create_milestone(
        State(Arc::clone(&st)),
        Path("p".into()),
        Ok(Json(CreateMilestone {
            name: "v1".into(),
            description: String::new(),
            target_date: Utc::now(),
            deliverables: vec![],
            status: MilestoneStatus::Proposed,
        })),
    )
    .await
    .expect("create milestone");

    let err = patch_milestone(
        State(Arc::clone(&st)),
        Path(("p".into(), m.id.to_string())),
        Ok(Json(PatchMilestone {
            name: Some("".into()),
            ..Default::default()
        })),
    )
    .await
    .expect_err("blank name on PATCH must be rejected");
    assert!(matches!(err, DaemonError::InvalidRequest(_)));

    let Json(unchanged) =
        get_milestone(State(Arc::clone(&st)), Path(("p".into(), m.id.to_string())))
            .await
            .expect("get ok");
    assert_eq!(unchanged.name, "v1");
}

#[tokio::test]
async fn milestone_unknown_is_404() {
    let (st, _g) = state();
    let err = get_milestone(
        State(st),
        Path(("p".into(), uuid::Uuid::new_v4().to_string())),
    )
    .await
    .expect_err("unknown milestone 404");
    assert!(matches!(err, DaemonError::MilestoneNotFound { .. }));
}
