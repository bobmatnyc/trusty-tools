//! End-to-end HTTP tests for the registry-B project routes (#2114/#2115) and the
//! Deliverable/Milestone client methods (#2381), driven through the REAL
//! `DaemonClient` against the REAL `api::router` on a loopback port.
//!
//! Why: the in-crate unit tests cover the pure pieces (wire-shape serde in
//! `client::http_client::{projects,deliverables}::tests`, the daemon handlers'
//! request DTOs). This file is the literal proof that the CLI's client methods
//! and the daemon's new routes agree over the wire — `registry_register_project`
//! → `registry_get_project` → `registry_list_projects` → `project_status`
//! round-trips, the §10.3 illegal-transition 409 surfaces as
//! `SetStatusError::Rejected` carrying the legal next states, and the
//! Deliverable/Milestone CRUD path works through the client. Mirrors the harness
//! in `tests/project_status_route.rs` (axum::serve on an ephemeral port +
//! reqwest), so no external daemon is needed in CI.
//! What: binds the router once per test, points a `DaemonClient` at it, and drives
//! the full verb surface.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test project_registry_routes`.

use std::future::IntoFuture;
use std::sync::Arc;

use trusty_mpm::client::DaemonClient;
use trusty_mpm::client::http_client::deliverables::{
    CreateDeliverableArgs, CreateMilestoneArgs, SetStatusError,
};
use trusty_mpm::client::http_client::projects::RegisterProjectArgs;
use trusty_mpm::daemon::{api, state::DaemonState};
use trusty_mpm::deliverable::{DeliverableKind, DeliverableStatus, EstimationTier};

/// Bind the real router on an ephemeral loopback port; return a client for it.
async fn serve() -> DaemonClient {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    DaemonClient::new(format!("http://{addr}"))
}

/// register → get → list → status round-trips over real HTTP.
#[tokio::test]
async fn register_get_list_status_round_trip() {
    let client = serve().await;

    let args = RegisterProjectArgs {
        name: "widget".into(),
        repo_url: "https://github.com/acme/widget".into(),
        default_branch: Some("develop".into()),
        description: Some("the widget".into()),
        tags: Some(vec!["backend".into()]),
        stack_hint: Some("rust".into()),
        gh_user: Some("acme-bot".into()),
    };
    let registered = client.registry_register_project(&args).await.unwrap();
    assert_eq!(registered.name, "widget");
    assert_eq!(registered.default_branch, "develop");
    assert_eq!(registered.gh_user.as_deref(), Some("acme-bot"));

    let got = client.registry_get_project("widget").await.unwrap();
    assert_eq!(got, registered);

    let all = client.registry_list_projects(None).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "widget");

    // Tag filter keeps the match and drops non-matches.
    assert_eq!(
        client
            .registry_list_projects(Some("backend"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        client
            .registry_list_projects(Some("nope"))
            .await
            .unwrap()
            .len(),
        0
    );

    // Status rollup: no sessions yet, gh_user flag set.
    let status = client.project_status("widget").await.unwrap();
    assert_eq!(status.project_name, "widget");
    assert_eq!(status.sessions.total, 0);
    assert!(status.config.gh_user_set);
    assert!(status.last_activity_at.is_none());
}

/// A re-register (upsert) preserves an existing per-project identity binding that
/// the register body cannot express (parity with `project_register`).
#[tokio::test]
async fn register_is_idempotent_upsert() {
    let client = serve().await;
    let base = RegisterProjectArgs {
        name: "widget".into(),
        repo_url: "https://github.com/acme/widget".into(),
        default_branch: None,
        description: None,
        tags: None,
        stack_hint: None,
        gh_user: None,
    };
    client.registry_register_project(&base).await.unwrap();
    // Re-register with a different branch — upsert on name, not a duplicate.
    let mut updated = base.clone();
    updated.default_branch = Some("release".into());
    let out = client.registry_register_project(&updated).await.unwrap();
    assert_eq!(out.default_branch, "release");
    assert_eq!(client.registry_list_projects(None).await.unwrap().len(), 1);
}

/// Fetching an unregistered project surfaces an error (the daemon 404s).
#[tokio::test]
async fn get_unknown_project_errors() {
    let client = serve().await;
    assert!(client.registry_get_project("nope").await.is_err());
}

/// Deliverable create → list → set-status legal → illegal-409 round-trips, and
/// the illegal transition surfaces as `SetStatusError::Rejected` with the legal
/// next states (#2380).
#[tokio::test]
async fn deliverable_crud_and_illegal_transition_409() {
    let client = serve().await;
    // No project registration is required — deliverables defer referential
    // integrity by design (daemon `CreateDeliverable` doc).
    let created = client
        .create_deliverable(
            "widget",
            &CreateDeliverableArgs {
                name: "OAuth2 flow".into(),
                description: None,
                kind: DeliverableKind::Feature,
                estimated_effort: EstimationTier::L,
                ticket_ref: Some("#2117".into()),
                spec_ref: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(created.status, DeliverableStatus::Proposed);

    let id = created.id.to_string();
    let listed = client.list_deliverables("widget", None).await.unwrap();
    assert_eq!(listed.len(), 1);

    // Status filter: proposed matches, in-progress does not (yet).
    assert_eq!(
        client
            .list_deliverables("widget", Some(DeliverableStatus::Proposed))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        client
            .list_deliverables("widget", Some(DeliverableStatus::InProgress))
            .await
            .unwrap()
            .len(),
        0
    );

    // Legal transition proposed → in-progress.
    let moved = client
        .set_deliverable_status("widget", &id, DeliverableStatus::InProgress)
        .await
        .unwrap();
    assert_eq!(moved.status, DeliverableStatus::InProgress);

    // Illegal transition in-progress → delivered → structured 409.
    let err = client
        .set_deliverable_status("widget", &id, DeliverableStatus::Delivered)
        .await
        .expect_err("must be rejected");
    match err {
        SetStatusError::Rejected {
            from,
            to,
            allowed_next,
        } => {
            assert_eq!(from, "in-progress");
            assert_eq!(to, "delivered");
            // in-progress → {blocked, complete} are the legal successors.
            assert!(
                allowed_next.contains(&"complete".to_string()),
                "{allowed_next:?}"
            );
            assert!(
                allowed_next.contains(&"blocked".to_string()),
                "{allowed_next:?}"
            );
        }
        SetStatusError::Other(e) => panic!("expected Rejected, got {e}"),
    }

    // get round-trips the updated record.
    let fetched = client.get_deliverable("widget", &id).await.unwrap();
    assert_eq!(fetched.status, DeliverableStatus::InProgress);
}

/// Milestone create → list → get round-trips over real HTTP.
#[tokio::test]
async fn milestone_crud_round_trip() {
    let client = serve().await;
    let created = client
        .create_milestone(
            "widget",
            &CreateMilestoneArgs {
                name: "v1.0 Alpha".into(),
                description: Some("first alpha".into()),
                target_date: chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            },
        )
        .await
        .unwrap();
    assert_eq!(created.name, "v1.0 Alpha");

    let id = created.id.to_string();
    let listed = client.list_milestones("widget").await.unwrap();
    assert_eq!(listed.len(), 1);

    let got = client.get_milestone("widget", &id).await.unwrap();
    assert_eq!(got, created);
}
