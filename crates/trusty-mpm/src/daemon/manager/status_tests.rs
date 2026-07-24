//! Pure-rollup tests for the cross-project portfolio aggregation (WI-2, #2579).
//!
//! Why: prove the DOC-36 §11 contract for `aggregate_portfolio_status` — it sums
//! the per-project histograms correctly, is deterministic (byte-identical output
//! for identical inputs, name-sorted project order), and handles the empty
//! portfolio without panicking. These drive the pure function directly; the
//! real-HTTP composition is covered by `tests/manager_routes.rs`.
//! What: hand-built `Project`/`Deliverable`/`Milestone` fixtures (sessions left
//! empty — session summation is already covered by the per-project rollup tests
//! and end-to-end by the HTTP test) exercised through `aggregate_portfolio_status`.

use super::*;

use chrono::Utc;

use crate::deliverable::{
    Deliverable, DeliverableId, DeliverableKind, DeliverableStatus, EstimationTier, Milestone,
    MilestoneId, MilestoneStatus,
};
use crate::project::Project;

/// Build a bare project fixture with the given name.
fn project(name: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: format!("https://github.com/acme/{name}"),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

/// Build a Deliverable scoped to `project_name` with `status`.
fn deliverable(project_name: &str, status: DeliverableStatus) -> Deliverable {
    Deliverable {
        id: DeliverableId::new(),
        project_name: project_name.to_string(),
        name: "fixture".to_string(),
        description: String::new(),
        kind: DeliverableKind::Feature,
        ticket_ref: None,
        spec_ref: None,
        status,
        estimated_effort: EstimationTier::M,
        created_at: Utc::now(),
        target_date: None,
    }
}

/// Build a Milestone scoped to `project_name` with `status` and member ids.
fn milestone(
    project_name: &str,
    status: MilestoneStatus,
    deliverables: Vec<DeliverableId>,
) -> Milestone {
    Milestone {
        id: MilestoneId::new(),
        project_name: project_name.to_string(),
        name: "fixture milestone".to_string(),
        description: String::new(),
        target_date: Utc::now(),
        status,
        deliverables,
        created_at: Utc::now(),
    }
}

/// Totals sum every project's histograms, `project_count` matches, and a dangling
/// Milestone→Deliverable ref is surfaced as a portfolio-wide count.
#[test]
fn aggregate_portfolio_status_sums_across_projects() {
    let projects = vec![project("alpha"), project("beta")];

    let missing = DeliverableId::new();
    let alpha_live = deliverable("alpha", DeliverableStatus::InProgress);
    let alpha_live_id = alpha_live.id;
    let deliverables = vec![
        alpha_live,
        deliverable("beta", DeliverableStatus::Complete),
        deliverable("beta", DeliverableStatus::Blocked),
    ];
    let milestones = vec![
        // alpha milestone references one live + one missing id → 1 dangling.
        milestone(
            "alpha",
            MilestoneStatus::InProgress,
            vec![alpha_live_id, missing],
        ),
        milestone("beta", MilestoneStatus::Shipped, vec![]),
    ];

    let rollup = aggregate_portfolio_status(&projects, &[], &deliverables, &milestones);

    assert_eq!(rollup.project_count, 2);
    assert_eq!(rollup.totals.deliverables.total, 3);
    assert_eq!(rollup.totals.deliverables.in_progress, 1);
    assert_eq!(rollup.totals.deliverables.complete, 1);
    assert_eq!(rollup.totals.deliverables.blocked, 1);
    assert_eq!(rollup.totals.milestones.total, 2);
    assert_eq!(rollup.totals.milestones.in_progress, 1);
    assert_eq!(rollup.totals.milestones.shipped, 1);
    assert_eq!(
        rollup.totals.milestones.dangling_deliverable_refs, 1,
        "alpha's milestone references a missing deliverable id: {rollup:?}"
    );
    // No sessions were supplied, so the session histogram is all-zero.
    assert_eq!(rollup.totals.sessions.total, 0);
    assert_eq!(rollup.totals.last_activity_at, None);
}

/// Identical inputs yield byte-identical output, and projects come back sorted by
/// name regardless of registration order (determinism, DOC-35 §11).
#[test]
fn aggregate_portfolio_status_is_deterministic() {
    // Registered out of order — output must be name-sorted (alpha before beta).
    let projects = vec![project("beta"), project("alpha")];
    let deliverables = vec![deliverable("alpha", DeliverableStatus::Proposed)];

    let first = aggregate_portfolio_status(&projects, &[], &deliverables, &[]);
    let second = aggregate_portfolio_status(&projects, &[], &deliverables, &[]);
    assert_eq!(first, second, "same inputs must produce identical output");

    let names: Vec<&str> = first
        .projects
        .iter()
        .map(|p| p.project_name.as_str())
        .collect();
    assert_eq!(names, vec!["alpha", "beta"], "projects must be name-sorted");
}

/// An empty portfolio rolls up to zero everywhere without panicking.
#[test]
fn aggregate_portfolio_status_empty_portfolio() {
    let rollup = aggregate_portfolio_status(&[], &[], &[], &[]);
    assert_eq!(rollup.project_count, 0);
    assert!(rollup.projects.is_empty());
    assert_eq!(rollup.totals.sessions.total, 0);
    assert_eq!(rollup.totals.deliverables.total, 0);
    assert_eq!(rollup.totals.milestones.total, 0);
    assert_eq!(rollup.totals.last_activity_at, None);
}
