//! Tests for the `ask_owner` route and its asker proof (#9856 Q1).
//!
//! Why: the asker must be the live managed Architect, proven from the
//! kernel's peer and the Architect record's own pane. Each test goes red
//! against a check that accepts a self-asserted or wrong asker.
//! What: [`architect_asker_with`] over injected process walks and records;
//! the HTTP route and the MCP body over a hermetic daemon.
//! Test: this is the test module.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use super::{architect_asker_with, router};
use crate::channels::ask_owner::ASK_OWNER_ROUTE;
use crate::channels::audit_log::audit_path;
use crate::core::twin_identity::ClaudeProcess;
use crate::session_manager::{ManagedSessionState, SessionKind, SessionRecord};

const ARCHITECT_ID: &str = "6f1c1d0e-0000-4000-8000-0000000000a1";
const PM_ID: &str = "6f1c1d0e-0000-4000-8000-0000000000b2";

const ARCHITECT_CLAUDE: ClaudeProcess = ClaudeProcess {
    pid: 4100,
    start_time: 1_700_000_000,
};
const PM_CLAUDE: ClaudeProcess = ClaudeProcess {
    pid: 4200,
    start_time: 1_700_000_100,
};

fn record(id: &str, name: &str, kind: SessionKind, state: ManagedSessionState) -> SessionRecord {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "tmux_name": name,
        "cwd": "/work/architect",
        "task": "test",
        "state": state,
        "created_at": "2026-10-13T00:00:00Z",
        "kind": kind,
        "pane_id": "%1",
    }))
    .expect("a test record")
}

fn architect() -> SessionRecord {
    record(
        ARCHITECT_ID,
        "tm-architect",
        SessionKind::Architect,
        ManagedSessionState::Active,
    )
}

fn pm() -> SessionRecord {
    record(
        PM_ID,
        "tm-pm-01",
        SessionKind::Ordinary,
        ManagedSessionState::Active,
    )
}

/// The `claude` each record's pane runs.
fn pane_claude(r: &SessionRecord) -> Result<ClaudeProcess, String> {
    match r.tmux_name.as_str() {
        "tm-architect" => Ok(ARCHITECT_CLAUDE),
        _ => Ok(PM_CLAUDE),
    }
}

/// A peer walk that lands on `claude`.
fn walks_to(claude: ClaudeProcess) -> impl Fn(u32, u64) -> Result<ClaudeProcess, String> {
    move |_, _| Ok(claude)
}

#[test]
fn the_live_architects_claude_is_the_asker() {
    let got = architect_asker_with(
        9001,
        1_700_000_500,
        &[pm(), architect()],
        walks_to(ARCHITECT_CLAUDE),
        pane_claude,
    );
    assert_eq!(got, Ok(ARCHITECT_ID.to_owned()));
}

#[test]
fn a_pm_sessions_claude_is_not_the_asker() {
    // The PM's own record matches its claude, but it is not the Architect.
    let err = architect_asker_with(
        9002,
        1_700_000_500,
        &[pm(), architect()],
        walks_to(PM_CLAUDE),
        pane_claude,
    )
    .expect_err("a PM is not the Architect");
    assert!(err.contains("not the live Architect"), "{err}");
}

#[test]
fn a_reused_pid_with_another_start_time_is_not_the_asker() {
    let reused = ClaudeProcess {
        pid: ARCHITECT_CLAUDE.pid,
        start_time: ARCHITECT_CLAUDE.start_time + 1,
    };
    let got = architect_asker_with(
        9003,
        1_700_000_500,
        &[architect()],
        walks_to(reused),
        pane_claude,
    );
    assert!(got.is_err(), "{got:?}");
}

#[test]
fn a_stopped_architect_record_proves_nothing() {
    let mut stopped = architect();
    stopped.state = ManagedSessionState::Stopped;
    let err = architect_asker_with(
        9004,
        1_700_000_500,
        &[stopped, pm()],
        walks_to(ARCHITECT_CLAUDE),
        pane_claude,
    )
    .expect_err("no live Architect");
    assert!(err.contains("no live managed Architect"), "{err}");
}

#[test]
fn a_failed_peer_walk_is_refused() {
    let err = architect_asker_with(
        9005,
        1_700_000_500,
        &[architect()],
        |_, _| Err("no claude above the caller".to_owned()),
        pane_claude,
    )
    .expect_err("an unwalked peer is refused");
    assert!(err.contains("no claude above"), "{err}");
    // A pane that cannot be proven is refused and named.
    let err = architect_asker_with(
        9005,
        1_700_000_500,
        &[architect()],
        walks_to(ARCHITECT_CLAUDE),
        |_| Err("pane %1 is gone".to_owned()),
    )
    .expect_err("an unproven pane proves nothing");
    assert!(err.contains("pane %1 is gone"), "{err}");
}

/// A hermetic daemon whose home is a temp dir.
async fn daemon() -> (Arc<crate::daemon::DaemonState>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let state = Arc::new(crate::daemon::DaemonState::with_root_isolated_managed(root).await);
    (state, tmp)
}

#[tokio::test(flavor = "multi_thread")]
async fn http_route_refuses_ask_owner_as_unproven() {
    let (state, _tmp) = daemon().await;
    let home = state.user_home().expect("pinned home");
    let body = serde_json::json!({ "recipient": "U0OWNER1", "question": "Merge?" });
    let request = Request::builder()
        .method("POST")
        .uri(ASK_OWNER_ROUTE)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = router()
        .with_state(Arc::clone(&state))
        .oneshot(request)
        .await
        .expect("the router answers");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON body");
    assert_eq!(v["error"], "asker_unproven", "{v}");
    let message = v["message"].as_str().expect("message");
    assert!(message.contains("daemon socket"), "{message}");
    let audit = std::fs::read_to_string(audit_path(&home)).expect("one audit line");
    assert_eq!(audit.lines().count(), 1, "{audit}");
    assert!(audit.contains("asker_unproven"), "{audit}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_ask_owner_over_http_rpc_is_refused_as_unproven() {
    let (state, _tmp) = daemon().await;
    // Outside a socket dispatch there is no kernel peer, as over `POST /rpc`.
    let err = crate::daemon::mcp_ask_owner::ask_owner(&state, "U0OWNER1", "Merge?")
        .await
        .expect_err("refused");
    assert!(err.starts_with("asker_unproven"), "{err}");
}
