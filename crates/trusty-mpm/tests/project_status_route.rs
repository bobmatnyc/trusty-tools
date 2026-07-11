//! HTTP-level integration tests for the deterministic project status-aggregation
//! endpoint `GET /api/v1/projects/{name}/status` (#2117, #2382, DOC-35 §4.1 +
//! §4.1 extension).
//!
//! Why: the in-crate `daemon::managed_routes::project_status::tests` drive the
//! pure `aggregate_project_status` rollup directly; this file instead binds the
//! REAL `api::router` on a loopback port and drives the route with `reqwest`, so
//! it is the literal proof that the endpoint is reachable over the daemon HTTP
//! API (the deterministic CLI #2115 and TUI #2118 will call it exactly this way).
//! Mirrors the real-network harness in `tests/proxy_routes.rs` (axum::serve on an
//! ephemeral port + reqwest).
//! What: registers one project, seeds a mixed-state set of managed sessions bound
//! to it (plus one bound to a DIFFERENT project, which must be excluded), serves
//! the router, and asserts the JSON rollup body — per-state counts, `total`,
//! config-completeness flags, and (as of #2382) the Deliverable/Milestone status
//! histograms — plus the 404 path for an unregistered project.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test project_status_route`.

use std::future::IntoFuture;
use std::sync::Arc;

use chrono::Utc;
use trusty_mpm::daemon::{api, state::DaemonState};
use trusty_mpm::deliverable::{
    Deliverable, DeliverableId, DeliverableKind, DeliverableStatus, EstimationTier, Milestone,
    MilestoneId, MilestoneStatus,
};
use trusty_mpm::project::Project;
use trusty_mpm::runtime::RuntimeKind;
use trusty_mpm::session_manager::ManagedSessionId;

/// Register a project fixture on the given daemon state.
async fn register_project(
    state: &Arc<DaemonState>,
    name: &str,
    repo_url: &str,
    gh_user: Option<&str>,
) {
    state
        .project_registry()
        .await
        .register(Project {
            name: name.to_string(),
            repo_url: repo_url.to_string(),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: gh_user.map(str::to_string),
            github: None,
            commit_name: None,
            commit_email: None,
        })
        .await
        .expect("register project");
}

/// Seed a managed session bound to `repo_url`; returns its id.
///
/// `create_with_id` starts the session in `Provisioning`; the caller can then
/// transition it (e.g. via `mark_errored`) to build a mixed-state fixture with
/// no real tmux side effects (the isolated state uses `FakeNoopTmuxDriver`).
async fn seed_session(state: &Arc<DaemonState>, repo_url: &str, task: &str) -> ManagedSessionId {
    let id = ManagedSessionId::new();
    state
        .session_manager()
        .await
        .create_with_id(
            id,
            task.to_string(),
            None,
            None,
            None,
            Some(repo_url.to_string()),
            Some("main".to_string()),
            RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");
    id
}

/// The status endpoint returns the deterministic rollup for a named project over
/// real HTTP: per-state counts, `total`, config flags, and exclusion of sessions
/// bound to other projects.
#[tokio::test]
async fn status_route_returns_deterministic_rollup() {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);

    let url = "https://github.com/acme/widget";
    register_project(&state, "widget", url, Some("acme-bot")).await;
    register_project(&state, "other", "https://github.com/acme/other", None).await;

    // Two Provisioning + one Errored bound to `widget`.
    seed_session(&state, url, "prov-a").await;
    seed_session(&state, url, "prov-b").await;
    let errored = seed_session(&state, url, "will-error").await;
    state
        .session_manager()
        .await
        .mark_errored(&errored, "boom")
        .await
        .expect("mark errored");
    // One bound to a DIFFERENT project — must be excluded from widget's rollup.
    seed_session(&state, "https://github.com/acme/other", "other-proj").await;

    let router = api::router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/v1/projects/widget/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["project_name"], "widget");
    assert_eq!(body["repo_url"], url);
    assert_eq!(body["sessions"]["provisioning"], 2);
    assert_eq!(body["sessions"]["errored"], 1);
    assert_eq!(body["sessions"]["active"], 0);
    assert_eq!(
        body["sessions"]["total"], 3,
        "only the three widget-bound sessions count, not the `other` session: {body}"
    );
    assert_eq!(body["config"]["gh_user_set"], true);
    assert_eq!(body["config"]["github_binding_set"], false);
}

/// An unregistered project name yields 404 (not a 500 or an empty rollup).
#[tokio::test]
async fn status_route_unknown_project_is_404() {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);

    let router = api::router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/v1/projects/nope/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

/// Seed a Deliverable scoped to `project_name` with the given status.
async fn seed_deliverable(
    state: &Arc<DaemonState>,
    project_name: &str,
    status: DeliverableStatus,
) -> DeliverableId {
    let d = Deliverable {
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
    };
    let id = d.id;
    state
        .deliverable_manager()
        .await
        .upsert_deliverable(d)
        .await
        .expect("seed deliverable");
    id
}

/// Seed a Milestone scoped to `project_name` with the given status and member
/// Deliverable ids.
async fn seed_milestone(
    state: &Arc<DaemonState>,
    project_name: &str,
    status: MilestoneStatus,
    deliverables: Vec<DeliverableId>,
) {
    let m = Milestone {
        id: MilestoneId::new(),
        project_name: project_name.to_string(),
        name: "fixture milestone".to_string(),
        description: String::new(),
        target_date: Utc::now(),
        status,
        deliverables,
        created_at: Utc::now(),
    };
    state
        .deliverable_manager()
        .await
        .upsert_milestone(m)
        .await
        .expect("seed milestone");
}

/// The status endpoint's `deliverables`/`milestones` histograms (#2382) reflect
/// the Deliverable/Milestone records scoped to the requested project over real
/// HTTP, excluding records bound to a DIFFERENT project, and surface a dangling
/// Milestone→Deliverable reference as a read-only count (never a crash, never a
/// repair — #2378's write-path deferral).
#[tokio::test]
async fn status_route_includes_deliverable_and_milestone_histograms() {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);

    let url = "https://github.com/acme/widget";
    register_project(&state, "widget", url, None).await;
    register_project(&state, "other", "https://github.com/acme/other", None).await;

    let live_id = seed_deliverable(&state, "widget", DeliverableStatus::InProgress).await;
    seed_deliverable(&state, "widget", DeliverableStatus::Complete).await;
    // Bound to a DIFFERENT project — must be excluded from widget's rollup.
    seed_deliverable(&state, "other", DeliverableStatus::Proposed).await;

    let missing_id = DeliverableId::new();
    seed_milestone(
        &state,
        "widget",
        MilestoneStatus::InProgress,
        vec![live_id, missing_id],
    )
    .await;
    // Bound to a DIFFERENT project — must be excluded from widget's rollup.
    seed_milestone(&state, "other", MilestoneStatus::Shipped, vec![]).await;

    let router = api::router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/v1/projects/widget/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(
        body["deliverables"]["total"], 2,
        "the `other`-project one is excluded: {body}"
    );
    assert_eq!(body["deliverables"]["in_progress"], 1);
    assert_eq!(body["deliverables"]["complete"], 1);
    assert_eq!(
        body["milestones"]["total"], 1,
        "the `other`-project one is excluded: {body}"
    );
    assert_eq!(body["milestones"]["in_progress"], 1);
    assert_eq!(
        body["milestones"]["dangling_deliverable_refs"], 1,
        "widget's milestone references one missing id alongside the live one: {body}"
    );
}
