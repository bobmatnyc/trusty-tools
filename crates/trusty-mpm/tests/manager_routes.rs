//! HTTP-level integration tests for the Layer-3 `tm manager` surface
//! (`/api/v1/manager/*`, epic #2109, DOC-36 phase 1a: WI-1 #2578 + WI-2 #2579).
//!
//! Why: the in-crate unit tests drive the pure `aggregate_portfolio_status`
//! rollup and the palace provisioning directly; this file instead binds the REAL
//! `api::router` on a loopback port and drives the routes with `reqwest`, so it
//! is the literal proof the manager scaffold is reachable over the daemon HTTP
//! API with NO channel/bot token and NO live LLM (DOC-36 §4 local-testability
//! bar). Mirrors the real-network harness in `tests/proxy_routes.rs` /
//! `tests/project_status_route.rs` (axum::serve on an ephemeral port + reqwest).
//! What: exercises the capabilities stub (`GET /manager/version`) and the
//! deterministic cross-project rollup (`GET /manager/status`) — the latter
//! against a multi-project fixture, asserting the portfolio totals sum the
//! per-project histograms and the per-project breakdown is present and sorted.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test manager_routes`.

use std::future::IntoFuture;
use std::sync::Arc;

use trusty_mpm::daemon::{api, state::DaemonState};
use trusty_mpm::deliverable::{
    Deliverable, DeliverableId, DeliverableKind, DeliverableStatus, EstimationTier,
};
use trusty_mpm::project::Project;
use trusty_mpm::runtime::RuntimeKind;
use trusty_mpm::session_manager::ManagedSessionId;

/// Serve the real router on an ephemeral loopback port; return its base URL.
async fn serve(state: Arc<DaemonState>) -> String {
    let router = api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// Register a project fixture on the given daemon state.
async fn register_project(state: &Arc<DaemonState>, name: &str, repo_url: &str) {
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
            gh_user: None,
            github: None,
            commit_name: None,
            commit_email: None,
        })
        .await
        .expect("register project");
}

/// Seed a managed session bound to `repo_url` (starts in `Provisioning`).
async fn seed_session(state: &Arc<DaemonState>, repo_url: &str, task: &str) {
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
}

/// Seed a Deliverable scoped to `project_name` with the given status.
async fn seed_deliverable(state: &Arc<DaemonState>, project_name: &str, status: DeliverableStatus) {
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
        created_at: chrono::Utc::now(),
        target_date: None,
    };
    state
        .deliverable_manager()
        .await
        .upsert_deliverable(d)
        .await
        .expect("seed deliverable");
}

/// `GET /api/v1/manager/version` returns the self-describing capabilities snapshot
/// over real HTTP — API version, phase, the advertised verb set (status+version
/// live, later endpoints planned), and the portfolio palace status (unavailable
/// in a default build with no `manager-memory` feature, per the §4 degrade bar).
#[tokio::test]
async fn manager_version_route_reports_capabilities() {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let base = serve(Arc::clone(&state)).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/manager/version"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["manager_api_version"], "0.1.0");
    assert_eq!(body["phase"], 1);
    assert_eq!(body["palace"]["id"], "tm-manager-portfolio");
    // Palace availability is feature-dependent: a default build (no
    // `manager-memory`) reports the palace as unavailable but still answers 200
    // (§4 degrade bar); a `--features manager-memory` build provisions the live
    // palace, so it reports available. Either way the surface answered 200 with a
    // palace status block.
    #[cfg(not(feature = "manager-memory"))]
    assert_eq!(body["palace"]["available"], false, "body: {body}");
    #[cfg(feature = "manager-memory")]
    assert_eq!(body["palace"]["available"], true, "body: {body}");

    let endpoints = body["endpoints"].as_array().expect("endpoints array");
    let status_ep = endpoints
        .iter()
        .find(|e| e["path"] == "/api/v1/manager/status")
        .expect("status endpoint advertised");
    assert_eq!(status_ep["available"], true);
    let digest_ep = endpoints
        .iter()
        .find(|e| e["path"] == "/api/v1/manager/digest")
        .expect("digest endpoint advertised");
    assert_eq!(
        digest_ep["available"], true,
        "digest ships in phase 1 (WI-3, #2580)"
    );
    let chat_ep = endpoints
        .iter()
        .find(|e| e["path"] == "/api/v1/manager/chat")
        .expect("chat endpoint advertised");
    assert_eq!(
        chat_ep["available"], true,
        "chat ships in phase 1 (WI-4, #2581)"
    );
    let route_task_ep = endpoints
        .iter()
        .find(|e| e["path"] == "/api/v1/manager/route-task")
        .expect("route-task endpoint advertised");
    assert_eq!(
        route_task_ep["available"], false,
        "route-task is a phase-2 WI, advertised as planned"
    );
}

/// `GET /api/v1/manager/status` returns the deterministic cross-project rollup
/// over real HTTP: it composes the per-project rollup across EVERY registered
/// project, sums the histograms into portfolio totals, and orders projects by
/// name — the thing L2 cannot do, with no LLM call.
#[tokio::test]
async fn manager_status_route_rolls_up_all_projects() {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);

    let beta_url = "https://github.com/acme/beta";
    let alpha_url = "https://github.com/acme/alpha";
    // Registered out of order — output must be name-sorted.
    register_project(&state, "beta", beta_url).await;
    register_project(&state, "alpha", alpha_url).await;

    // alpha: two provisioning sessions + one in-progress deliverable.
    seed_session(&state, alpha_url, "a-1").await;
    seed_session(&state, alpha_url, "a-2").await;
    seed_deliverable(&state, "alpha", DeliverableStatus::InProgress).await;
    // beta: one provisioning session + one complete deliverable.
    seed_session(&state, beta_url, "b-1").await;
    seed_deliverable(&state, "beta", DeliverableStatus::Complete).await;

    let base = serve(Arc::clone(&state)).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/manager/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["project_count"], 2);
    // Portfolio totals sum across BOTH projects.
    assert_eq!(
        body["totals"]["sessions"]["provisioning"], 3,
        "body: {body}"
    );
    assert_eq!(body["totals"]["sessions"]["total"], 3);
    assert_eq!(body["totals"]["deliverables"]["total"], 2);
    assert_eq!(body["totals"]["deliverables"]["in_progress"], 1);
    assert_eq!(body["totals"]["deliverables"]["complete"], 1);

    // Per-project breakdown is present and name-sorted.
    let projects = body["projects"].as_array().expect("projects array");
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[0]["project_name"], "alpha");
    assert_eq!(projects[1]["project_name"], "beta");
    assert_eq!(projects[0]["sessions"]["provisioning"], 2);
    assert_eq!(projects[1]["sessions"]["provisioning"], 1);
}

/// `GET /api/v1/manager/status` on an empty portfolio returns a zeroed rollup
/// (not a 404 or an error) — the manager surface is operable before any project
/// is registered.
#[tokio::test]
async fn manager_status_route_empty_portfolio() {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let base = serve(Arc::clone(&state)).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/manager/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["project_count"], 0);
    assert_eq!(body["totals"]["sessions"]["total"], 0);
    assert_eq!(body["totals"]["deliverables"]["total"], 0);
    assert!(body["projects"].as_array().unwrap().is_empty());
}
