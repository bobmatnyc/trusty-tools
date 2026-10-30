//! #10476: a normal refresh tick serves the cached Activity pane.
//!
//! Why: `/activity` costs a tmux capture on the daemon; the poll asked for it
//! on every 1 s tick.
//! What: a stub daemon counts `/activity` requests across three back-to-back
//! polls with one session selected. Only the first poll may ask.
//! Test: this is the test module.

use std::future::IntoFuture;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::project_ctl_poll_daemon;
use crate::client::DaemonClient;
use crate::tui::project_ctl::state::ProjectCtlState;

/// Serve the five routes one poll reads; count `/activity` hits.
async fn stub_daemon(activity_hits: Arc<AtomicUsize>) -> String {
    use axum::routing::get;
    let json = |v: serde_json::Value| move || std::future::ready(axum::Json(v.clone()));
    let activity = serde_json::json!({
        "raw_pane": "line one\nline two",
        "runtime_active": true,
        "state": "working",
        "summary": "editing",
        "confidence": 0.9,
        "cache_hit": false,
        "input_tokens": 0,
        "output_tokens": 0,
        "latency_ms": 0,
        "total_input_tokens": 0,
        "total_output_tokens": 0
    });
    let app = axum::Router::new()
        .route("/health", get(|| std::future::ready("ok")))
        .route(
            "/api/v1/projects",
            get(json(serde_json::json!({"projects": [{
                "name": "widget",
                "repo_url": "https://github.com/acme/widget",
                "default_branch": "main"
            }]}))),
        )
        .route(
            "/api/v1/sessions/managed/fleet",
            get(json(serde_json::json!({"projects": [{
                "project_name": "widget",
                "repo_url": "https://github.com/acme/widget",
                "sessions": [{"id": "s-10476", "name": "widget-1", "state": "active"}]
            }]}))),
        )
        .route(
            "/api/v1/sessions/managed/{id}/activity",
            get(move || {
                activity_hits.fetch_add(1, Ordering::SeqCst);
                std::future::ready(axum::Json(activity.clone()))
            }),
        )
        .route(
            "/api/v1/projects/{project}/deliverables",
            get(json(serde_json::json!({"deliverables": []}))),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub daemon");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

#[tokio::test]
async fn normal_tick_makes_no_activity_call_10476() {
    let hits = Arc::new(AtomicUsize::new(0));
    let mut client = DaemonClient::new(stub_daemon(hits.clone()).await);
    let mut state = ProjectCtlState::default();

    for _ in 0..3 {
        project_ctl_poll_daemon(&mut state, &mut client).await;
    }

    assert!(state.daemon_reachable, "the stub daemon must read as up");
    assert_eq!(
        state.activity.as_ref().map(|a| a.session_id.as_str()),
        Some("s-10476"),
        "the first poll fills the Activity pane"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "three polls within the refresh window must make one /activity call (a tmux capture), not one per tick"
    );
}
