//! See #10754: the launch form's request and its dispatch.

use std::future::IntoFuture;
use std::sync::{Arc, Mutex};

use super::*;
use crate::tui::project_ctl::actions::dispatch;
use crate::tui::project_ctl::events::PendingAction;

fn project(repo_url: &str, default_branch: &str) -> Project {
    serde_json::from_value(serde_json::json!({
        "name": "alpha",
        "repo_url": repo_url,
        "default_branch": default_branch,
    }))
    .expect("a minimal project record")
}

fn request(git_ref: Option<&str>) -> LaunchRequest {
    LaunchRequest {
        project: "alpha".to_string(),
        task: "fix the flaky test".to_string(),
        git_ref: git_ref.map(str::to_string),
    }
}

#[test]
fn launch_command_carries_project_task_and_ref() {
    let cmd = launch_command(&request(Some("feat/x")), &project("/work/alpha", "main"))
        .expect("an absolute checkout path");
    assert_eq!(
        cmd,
        TrustyCommand::ManagedNew {
            repo_url: "/work/alpha".to_string(),
            git_ref: "feat/x".to_string(),
            task: "fix the flaky test".to_string(),
            name_hint: None,
            runtime: Some("claude-code".to_string()),
            inject_task: None,
            deliverable_id: None,
            force_new: true,
        }
    );
}

#[test]
fn launch_command_defaults_the_ref_to_the_projects_default_branch() {
    let cmd = launch_command(&request(None), &project("/work/alpha", "develop"))
        .expect("an absolute checkout path");
    match cmd {
        TrustyCommand::ManagedNew { git_ref, .. } => assert_eq!(git_ref, "develop"),
        other => panic!("expected ManagedNew, got {other:?}"),
    }
}

#[test]
fn launch_command_refuses_a_project_with_no_local_checkout() {
    let err = launch_command(&request(None), &project("not-a-repo", "main"))
        .expect_err("a one-segment repo_url names no checkout");
    assert!(err.contains("'alpha'"), "{err}");
    assert!(err.contains("no local checkout"), "{err}");
}

#[tokio::test]
async fn launch_reports_the_error_when_the_daemon_is_unreachable() {
    let client = DaemonClient::new(crate::test_support::dead_loopback_url());
    let mut state = ProjectCtlState::default();
    dispatch(&mut state, &client, PendingAction::Launch(request(None))).await;
    let notice = state.notice.clone().unwrap_or_default();
    assert!(notice.starts_with("launch failed:"), "{notice}");
    assert!(!state.take_repoll(), "a failed launch does not repoll");
}

/// The form's request reaches `POST /api/v1/sessions/managed` with the
/// project's checkout, the ref and the task, and the notice names the session.
#[tokio::test]
async fn launch_posts_the_spawn_and_notices_the_session_name() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let checkout = tmp.path().to_string_lossy().into_owned();
    let record = serde_json::json!({
        "name": "alpha",
        "repo_url": checkout,
        "default_branch": "develop",
    });
    let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::default();
    let sink = seen.clone();
    let app = axum::Router::new()
        .route(
            "/api/v1/projects/{name}",
            axum::routing::get(move || std::future::ready(axum::Json(record.clone()))),
        )
        .route(
            "/api/v1/sessions/managed",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                if let Ok(mut slot) = sink.lock() {
                    *slot = Some(body);
                }
                std::future::ready(axum::Json(serde_json::json!({
                    "id": "s-1",
                    "name": "alpha-fix",
                    "state": "provisioning",
                })))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());

    let client = DaemonClient::new(format!("http://{addr}"));
    let mut state = ProjectCtlState::default();
    dispatch(&mut state, &client, PendingAction::Launch(request(None))).await;

    assert_eq!(state.notice.as_deref(), Some("launched alpha-fix"));
    assert!(state.take_repoll(), "a launch repolls the fleet");
    let body = seen.lock().unwrap().clone().expect("the spawn was posted");
    assert_eq!(body["repo_url"], checkout);
    assert_eq!(body["ref"], "develop", "the wire key is `ref`");
    assert_eq!(body["task"], "fix the flaky test");
    assert_eq!(body["force_new"], true);
}

/// See #10754: the submit draws `launching <project>…` before the spawn POST
/// leaves, then replaces it with the outcome.
#[tokio::test]
async fn launch_draws_the_launching_notice_before_the_spawn_is_sent() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let record = serde_json::json!({
        "name": "alpha",
        "repo_url": tmp.path().to_string_lossy(),
        "default_branch": "main",
    });
    let posted: Arc<Mutex<bool>> = Arc::default();
    let sink = posted.clone();
    let app = axum::Router::new()
        .route(
            "/api/v1/projects/{name}",
            axum::routing::get(move || std::future::ready(axum::Json(record.clone()))),
        )
        .route(
            "/api/v1/sessions/managed",
            axum::routing::post(move || {
                if let Ok(mut flag) = sink.lock() {
                    *flag = true;
                }
                std::future::ready(axum::Json(serde_json::json!({
                    "id": "s-1",
                    "name": "alpha-fix",
                    "state": "provisioning",
                })))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());

    // Each draw records the notice on screen and whether the spawn was posted.
    let mut frames: Vec<(Option<String>, bool)> = Vec::new();
    let seen = posted.clone();
    let mut screen = |s: &mut ProjectCtlState| -> anyhow::Result<()> {
        frames.push((s.notice.clone(), *seen.lock().unwrap()));
        Ok(())
    };
    let client = DaemonClient::new(format!("http://{addr}"));
    let mut state = ProjectCtlState::default();
    crate::tui::project_ctl::actions::dispatch_on(
        &mut state,
        &client,
        PendingAction::Launch(request(None)),
        &mut screen,
    )
    .await
    .expect("the draw never fails");

    assert_eq!(
        frames,
        vec![(Some("launching alpha…".to_string()), false)],
        "one frame shows the pending launch before the spawn is posted"
    );
    assert!(*posted.lock().unwrap(), "the spawn was posted");
    assert_eq!(state.notice.as_deref(), Some("launched alpha-fix"));
}
