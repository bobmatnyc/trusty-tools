//! Tests for the sandbox-only inbound route (#9873 Q1, F19).
//!
//! Why: the route turns an HTTP body into an owner's reply; outside the
//! sandbox it must not exist. Each test goes red against a route that is
//! mounted in production or that answers 200 for a refusal.
//! What: the router over a hermetic daemon (this test process never sets
//! the sandbox latch), the status map over every outcome, and a source scan
//! of who mounts the route.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use super::{INBOUND_FAKE_ROUTE, router};
use crate::channels::inbound::Dropped;
use crate::channels::question_table::Answer;
use crate::daemon::managed_routes::inbound::{InboundReply, reply};

/// A hermetic daemon whose home is a temp dir.
async fn daemon() -> (Arc<crate::daemon::DaemonState>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let state = Arc::new(crate::daemon::DaemonState::with_root_isolated_managed(root).await);
    (state, tmp)
}

fn event_body(channel: &str) -> serde_json::Value {
    serde_json::json!({
        "channel": channel,
        "sender": "U0OWNER1",
        "dm_channel_id": "D0OWNERDM",
        "thread_ts": "1700000000.000100",
        "text": "approve",
    })
}

/// POST `body` to `app`; the status and the JSON body.
async fn post(
    app: axum::Router<Arc<crate::daemon::DaemonState>>,
    state: &Arc<crate::daemon::DaemonState>,
    body: &serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method("POST")
        .uri(INBOUND_FAKE_ROUTE)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app
        .with_state(Arc::clone(state))
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// 🔴 #9873 (F19, Q1): outside the sandbox the route is absent (404), from
/// the daemon's own channel router too; inside it, it answers.
#[tokio::test(flavor = "multi_thread")]
async fn inbound_fake_route_is_absent_outside_the_sandbox() {
    assert!(
        !crate::secret_source::sandboxed(),
        "no lib test sets the latch"
    );
    let (state, _tmp) = daemon().await;
    let (status, _) = post(super::super::channels_router(), &state, &event_body("slack")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the production router");
    let (status, _) = post(router(false), &state, &event_body("slack")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not sandboxed");
    let (status, json) = post(router(true), &state, &event_body("slack")).await;
    assert_ne!(status, StatusCode::NOT_FOUND, "sandboxed: {json}");
    assert!(json["error"].is_string(), "a hermetic daemon refuses: {json}");
}

/// 🔴 #9873 (plan §4, Q6, AC3, AC9): every outcome's status and body.
#[tokio::test(flavor = "multi_thread")]
async fn inbound_fake_route_status_map() {
    let drop = |code| InboundReply::Dropped(Dropped { code, first: None });
    for (code, status) in [
        ("unknown_sender", StatusCode::FORBIDDEN),
        ("not_recipient", StatusCode::FORBIDDEN),
        ("no_question_kind", StatusCode::FORBIDDEN),
        ("not_owned_by_route", StatusCode::FORBIDDEN),
        ("no_open_question", StatusCode::NOT_FOUND),
        ("unknown_question", StatusCode::NOT_FOUND),
        ("no_binding", StatusCode::UNPROCESSABLE_ENTITY),
        ("ambiguous_binding", StatusCode::UNPROCESSABLE_ENTITY),
        ("answer_too_long", StatusCode::UNPROCESSABLE_ENTITY),
        ("answer_empty", StatusCode::UNPROCESSABLE_ENTITY),
        ("rate_limited", StatusCode::TOO_MANY_REQUESTS),
        ("snapshot_stale", StatusCode::SERVICE_UNAVAILABLE),
        ("snapshot_never_loaded", StatusCode::SERVICE_UNAVAILABLE),
        ("table_unavailable", StatusCode::SERVICE_UNAVAILABLE),
        ("table_poisoned", StatusCode::SERVICE_UNAVAILABLE),
        ("rate_limiter_poisoned", StatusCode::SERVICE_UNAVAILABLE),
        ("asker_unavailable", StatusCode::SERVICE_UNAVAILABLE),
        ("a_code_with_no_status", StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let (got, body) = reply(&drop(code));
        assert_eq!(got, status, "{code}");
        assert_eq!(body.0["error"], code);
        assert_eq!(body.0["resolved"], false, "{code}");
    }
    let first = Answer {
        answerer: "U0OWNER1".into(),
        answered_at: "2026-10-13T23:59:00.000Z".into(),
        text_sha256: "00".into(),
        text_len: 7,
    };
    let (got, body) = reply(&InboundReply::Dropped(Dropped {
        code: "already_resolved",
        first: Some(first.clone()),
    }));
    assert_eq!(got, StatusCode::CONFLICT);
    assert_eq!(body.0["resolved"], true);
    assert_eq!(body.0["already_answered_by"], first.answerer);
    assert_eq!(body.0["at"], first.answered_at);
    let (got, body) = reply(&InboundReply::DeliveryFailed {
        question_id: 7,
        code: "send_failed",
        sent: false,
    });
    assert_eq!(got, StatusCode::BAD_GATEWAY);
    assert_eq!((body.0["resolved"].clone(), body.0["delivered"].clone()), (true.into(), false.into()));
    let (got, _) = reply(&InboundReply::Delivered { question_id: 7 });
    assert_eq!(got, StatusCode::OK);
    let (got, body) = reply(&InboundReply::Aborted("join error".into()));
    assert_eq!(got, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body.0["error"], "inbound_aborted");
    // Through the route: a channel the daemon does not serve, and a field
    // the body does not have, are refused before any decision.
    let (state, _tmp) = daemon().await;
    let (status, json) = post(router(true), &state, &event_body("gchat")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{json}");
    assert_eq!(json["error"], "channel_not_served");
    let mut extra = event_body("slack");
    extra["question_id"] = 1.into();
    let (status, _) = post(router(true), &state, &extra).await;
    assert!(status.is_client_error(), "an unknown field: {status}");
}

/// The crate's `src` directory.
fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every production `.rs` file under `dir`.
fn production_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src").flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                production_sources(&path, out);
            }
        } else if name.ends_with(".rs") && !name.ends_with("tests.rs") {
            out.push(path);
        }
    }
}

/// `text` without its `//` comment lines.
fn code(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect()
}

/// 🔴 #9873 (Q1, F19): only `channels_router` mounts the route, and only
/// behind the sandbox latch; nothing else calls `inbound_op`.
#[test]
fn inbound_route_has_no_production_source() {
    let root = src();
    let mut files = Vec::new();
    production_sources(&root, &mut files);
    let routes = root.join("daemon/managed_routes");
    let mut mounts = Vec::new();
    let mut ops = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        for line in code(&text) {
            if line.contains("inbound_fake::") {
                mounts.push((file.clone(), line.trim().to_owned()));
            }
            if line.contains("inbound_op(") && !line.contains("fn inbound_op(") {
                ops.push(file.clone());
            }
        }
    }
    assert!(!mounts.is_empty(), "the scan found the mount");
    for (file, line) in &mounts {
        assert_eq!(file, &routes.join("mod.rs"), "{line}");
        assert!(
            line.contains("inbound_fake::router(crate::secret_source::sandboxed())"),
            "mounted without the latch: {line}"
        );
    }
    assert_eq!(ops, [routes.join("inbound_fake.rs")], "inbound_op callers");
}
