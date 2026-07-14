use super::*;
use crate::session_manager::{ManagedSessionId, SessionRecord};
use chrono::TimeZone;
use std::path::PathBuf;

/// Build a minimal [`Project`] fixture with the given name/url and no config.
fn project(name: &str, repo_url: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        github: None,
        commit_name: None,
        commit_email: None,
    }
}

/// Build a [`SessionRecord`] fixture in `state` bound to `repo_url`, with an
/// optional `last_activity_at`.
fn session(
    state: ManagedSessionState,
    repo_url: Option<&str>,
    activity: Option<DateTime<Utc>>,
) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tm-test".to_string(),
        cwd: PathBuf::from("/tmp"),
        task: "fixture".to_string(),
        state,
        created_at: Utc::now(),
        last_activity_at: activity,
        workspace_path: None,
        repo_url: repo_url.map(str::to_string),
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
    }
}

/// The histogram counts each state exactly once and `total` is their sum;
/// sessions bound to a DIFFERENT repo (or none) are excluded.
#[test]
fn aggregate_project_status_counts_by_state() {
    let url = "https://github.com/acme/widget";
    let proj = project("widget", url);
    let sessions = vec![
        session(ManagedSessionState::Active, Some(url), None),
        session(ManagedSessionState::Active, Some(url), None),
        session(ManagedSessionState::Stopped, Some(url), None),
        session(ManagedSessionState::Errored, Some(url), None),
        session(ManagedSessionState::Provisioning, Some(url), None),
        session(ManagedSessionState::Decommissioned, Some(url), None),
        // Bound to a different project — must be excluded.
        session(
            ManagedSessionState::Active,
            Some("https://github.com/acme/other"),
            None,
        ),
        // No repo_url — must be excluded.
        session(ManagedSessionState::Active, None, None),
    ];

    let out = aggregate_project_status(&proj, &sessions, &[], &[]);

    assert_eq!(out.sessions.active, 2);
    assert_eq!(out.sessions.stopped, 1);
    assert_eq!(out.sessions.errored, 1);
    assert_eq!(out.sessions.provisioning, 1);
    assert_eq!(out.sessions.decommissioned, 1);
    assert_eq!(out.sessions.total, 6, "only the six bound sessions count");
    assert_eq!(out.project_name, "widget");
    assert_eq!(out.repo_url, url);
}

/// `last_activity_at` is the maximum across bound sessions; sessions with no
/// activity contribute nothing, and an all-`None` set yields `None`.
#[test]
fn aggregate_project_status_max_activity() {
    let url = "https://github.com/acme/widget";
    let proj = project("widget", url);
    let t_old = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let t_new = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();

    let out = aggregate_project_status(
        &proj,
        &[
            session(ManagedSessionState::Stopped, Some(url), Some(t_old)),
            session(ManagedSessionState::Active, Some(url), Some(t_new)),
            session(ManagedSessionState::Active, Some(url), None),
        ],
        &[],
        &[],
    );
    assert_eq!(out.last_activity_at, Some(t_new));

    // No activity anywhere → None.
    let none = aggregate_project_status(
        &proj,
        &[session(ManagedSessionState::Provisioning, Some(url), None)],
        &[],
        &[],
    );
    assert_eq!(none.last_activity_at, None);

    // No bound sessions at all → all zero, None activity.
    let empty = aggregate_project_status(&proj, &[], &[], &[]);
    assert_eq!(empty.sessions.total, 0);
    assert_eq!(empty.last_activity_at, None);
}

/// Config flags are pure `is_some()` reads over the project record.
#[test]
fn aggregate_project_status_config_flags() {
    let url = "https://github.com/acme/widget";
    let mut proj = project("widget", url);
    let bare = aggregate_project_status(&proj, &[], &[], &[]);
    assert!(!bare.config.gh_user_set);
    assert!(!bare.config.github_binding_set);

    proj.gh_user = Some("acme-bot".to_string());
    let with_user = aggregate_project_status(&proj, &[], &[], &[]);
    assert!(with_user.config.gh_user_set);
    assert!(!with_user.config.github_binding_set);
}

/// Re-running the rollup with unchanged inputs yields identical output —
/// the DOC-35 §11 determinism test made executable.
#[test]
fn aggregate_project_status_is_deterministic() {
    let url = "https://github.com/acme/widget";
    let proj = project("widget", url);
    let t = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
    let sessions = vec![
        session(ManagedSessionState::Active, Some(url), Some(t)),
        session(ManagedSessionState::Errored, Some(url), None),
    ];
    let deliverables = vec![deliverable("widget", DeliverableStatus::InProgress)];
    let milestones = vec![milestone("widget", MilestoneStatus::Proposed, vec![])];
    let a = aggregate_project_status(&proj, &sessions, &deliverables, &milestones);
    let b = aggregate_project_status(&proj, &sessions, &deliverables, &milestones);
    assert_eq!(a, b);
}

/// Build a [`Deliverable`] fixture scoped to `project_name` with the given
/// status; the other fields are filler, not under test.
fn deliverable(project_name: &str, status: DeliverableStatus) -> Deliverable {
    Deliverable {
        id: DeliverableId::new(),
        project_name: project_name.to_string(),
        name: "fixture".to_string(),
        description: String::new(),
        kind: crate::deliverable::DeliverableKind::Feature,
        ticket_ref: None,
        spec_ref: None,
        status,
        estimated_effort: crate::deliverable::EstimationTier::M,
        created_at: Utc::now(),
        target_date: None,
    }
}

/// Build a [`Milestone`] fixture scoped to `project_name` with the given
/// status and member Deliverable ids.
fn milestone(
    project_name: &str,
    status: MilestoneStatus,
    deliverables: Vec<DeliverableId>,
) -> Milestone {
    Milestone {
        id: crate::deliverable::MilestoneId::new(),
        project_name: project_name.to_string(),
        name: "fixture milestone".to_string(),
        description: String::new(),
        target_date: Utc::now(),
        status,
        deliverables,
        created_at: Utc::now(),
    }
}

/// An empty Deliverable slice tallies to all zeros.
#[test]
fn count_deliverable_statuses_empty_is_all_zero() {
    let out = count_deliverable_statuses(&[]);
    assert_eq!(
        out,
        DeliverableStatusCounts {
            proposed: 0,
            in_progress: 0,
            blocked: 0,
            complete: 0,
            delivered: 0,
            shipped: 0,
            total: 0,
        }
    );
}

/// One Deliverable per status variant tallies each bucket to exactly one,
/// with `total` equal to the variant count.
#[test]
fn count_deliverable_statuses_mixed_tally() {
    let p = "widget";
    let owned = [
        deliverable(p, DeliverableStatus::Proposed),
        deliverable(p, DeliverableStatus::InProgress),
        deliverable(p, DeliverableStatus::InProgress),
        deliverable(p, DeliverableStatus::Blocked),
        deliverable(p, DeliverableStatus::Complete),
        deliverable(p, DeliverableStatus::Delivered),
        deliverable(p, DeliverableStatus::Shipped),
    ];
    let refs: Vec<&Deliverable> = owned.iter().collect();
    let out = count_deliverable_statuses(&refs);
    assert_eq!(
        out,
        DeliverableStatusCounts {
            proposed: 1,
            in_progress: 2,
            blocked: 1,
            complete: 1,
            delivered: 1,
            shipped: 1,
            total: 7,
        }
    );
}

/// An empty Milestone slice tallies to all zeros, including zero dangling
/// refs.
#[test]
fn count_milestone_statuses_empty_is_all_zero() {
    let out = count_milestone_statuses(&[], &HashSet::new());
    assert_eq!(
        out,
        MilestoneStatusCounts {
            proposed: 0,
            in_progress: 0,
            complete: 0,
            shipped: 0,
            total: 0,
            dangling_deliverable_refs: 0,
        }
    );
}

/// One Milestone per status variant tallies each bucket to exactly one.
#[test]
fn count_milestone_statuses_mixed_tally() {
    let p = "widget";
    let owned = [
        milestone(p, MilestoneStatus::Proposed, vec![]),
        milestone(p, MilestoneStatus::InProgress, vec![]),
        milestone(p, MilestoneStatus::Complete, vec![]),
        milestone(p, MilestoneStatus::Shipped, vec![]),
    ];
    let refs: Vec<&Milestone> = owned.iter().collect();
    let out = count_milestone_statuses(&refs, &HashSet::new());
    assert_eq!(
        out,
        MilestoneStatusCounts {
            proposed: 1,
            in_progress: 1,
            complete: 1,
            shipped: 1,
            total: 4,
            dangling_deliverable_refs: 0,
        }
    );
}

/// A Milestone referencing a Deliverable id absent from the project's
/// Deliverable set is counted as dangling — WITHOUT crashing or attempting
/// to fix/drop the reference (§11, #2378's write-path deferral note). A
/// Milestone whose refs all resolve is NOT counted, even alongside a
/// dangling one.
#[test]
fn count_milestone_statuses_flags_dangling_deliverable_refs() {
    let p = "widget";
    let live_id = DeliverableId::new();
    let missing_id = DeliverableId::new();
    let mut known_ids = HashSet::new();
    known_ids.insert(live_id);

    let clean = milestone(p, MilestoneStatus::Complete, vec![live_id]);
    let dangling = milestone(p, MilestoneStatus::InProgress, vec![missing_id]);
    let mixed_dangling = milestone(p, MilestoneStatus::Proposed, vec![live_id, missing_id]);
    let refs = [&clean, &dangling, &mixed_dangling];

    let out = count_milestone_statuses(&refs, &known_ids);
    assert_eq!(out.total, 3);
    assert_eq!(
        out.dangling_deliverable_refs, 2,
        "only the two Milestones with an unresolved id are flagged, \
         the clean one is not"
    );
}

/// The full rollup includes both histograms, scoped to the requested
/// project only — a Deliverable/Milestone bound to a DIFFERENT project is
/// excluded, mirroring the existing session-scoping behavior.
#[test]
fn aggregate_project_status_includes_deliverable_and_milestone_histograms() {
    let url = "https://github.com/acme/widget";
    let proj = project("widget", url);
    let live_id = DeliverableId::new();

    let deliverables = vec![
        deliverable("widget", DeliverableStatus::InProgress),
        {
            let mut d = deliverable("widget", DeliverableStatus::Complete);
            d.id = live_id;
            d
        },
        // Bound to a different project — must be excluded.
        deliverable("other", DeliverableStatus::Proposed),
    ];
    let milestones = vec![
        milestone("widget", MilestoneStatus::InProgress, vec![live_id]),
        // Bound to a different project — must be excluded.
        milestone("other", MilestoneStatus::Shipped, vec![]),
    ];

    let out = aggregate_project_status(&proj, &[], &deliverables, &milestones);

    assert_eq!(
        out.deliverables.total, 2,
        "the `other`-project one is excluded"
    );
    assert_eq!(out.deliverables.in_progress, 1);
    assert_eq!(out.deliverables.complete, 1);
    assert_eq!(
        out.milestones.total, 1,
        "the `other`-project one is excluded"
    );
    assert_eq!(out.milestones.in_progress, 1);
    assert_eq!(
        out.milestones.dangling_deliverable_refs, 0,
        "widget's milestone references a live, in-project Deliverable id"
    );
}
