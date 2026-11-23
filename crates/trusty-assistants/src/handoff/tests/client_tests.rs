//! The production clients' decoding, and `UdsTc` against a mock tcode
//! socket (#11650 items 2, 3, 5).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use trusty_code::tui_client::EngineError;
use trusty_common::uds::rpc::UdsRpcError;

use super::super::clients::{ToolAnswer, classify_tc_error, decode_session_state};
use super::super::{CallError, TcClient, TmSessionState, UdsTc};
use crate::uds_mock::{self, RpcError, tools_call_envelope};

/// Why: tm answers every tool as a text frame; `isError` must never be read
/// as a result.
/// Test: itself.
#[test]
fn tool_answers_decode() {
    let ok = tools_call_envelope(&json!({"id": "abc", "sent": true}));
    assert_eq!(
        ToolAnswer::from_result(&ok),
        ToolAnswer::Json(json!({"id": "abc", "sent": true}))
    );
    let err = json!({"content": [{"type": "text", "text": "boom"}], "isError": true});
    assert_eq!(
        ToolAnswer::from_result(&err),
        ToolAnswer::Error("boom".into())
    );
    assert_eq!(
        ToolAnswer::from_result(&json!({})),
        ToolAnswer::Json(Value::Null)
    );
}

/// Why: item 2 — gone (not found, decommissioned, deleted) is told apart
/// from parked (stopped, errored) and live, and any other error stops the
/// handoff without a send.
/// Test: itself.
#[test]
fn session_states_decode() {
    let state = |s: &str| decode_session_state(ToolAnswer::Json(json!({"state": s})));
    assert_eq!(state("active"), Ok(TmSessionState::Live));
    assert_eq!(state("provisioning"), Ok(TmSessionState::Live));
    for s in ["decommissioned", "deleted"] {
        assert!(matches!(state(s), Ok(TmSessionState::Gone { .. })), "{s}");
    }
    for s in ["stopped", "errored"] {
        assert_eq!(
            state(s),
            Ok(TmSessionState::Parked { state: s.into() }),
            "{s}"
        );
    }
    assert!(matches!(
        decode_session_state(ToolAnswer::Error("session not found: 1234".into())),
        Ok(TmSessionState::Gone { .. })
    ));
    assert!(matches!(
        decode_session_state(ToolAnswer::Error("daemon unreachable".into())),
        Err(CallError::NotSent(_))
    ));
    assert!(matches!(
        decode_session_state(ToolAnswer::Json(Value::Null)),
        Err(CallError::NotSent(_))
    ));
}

/// Why: Fail-Open Check — only a failure that provably sent nothing may be
/// recorded as failed; a read failure after the write is unknown.
/// Test: itself.
#[test]
fn tc_errors_classify() {
    let rpc = EngineError::Rpc {
        code: -32003,
        message: "task_description must not be empty".into(),
        data: None,
    };
    assert!(matches!(classify_tc_error(rpc), CallError::NotSent(_)));
    let read = EngineError::Transport {
        socket: "/s".into(),
        source: Box::new(UdsRpcError::Read {
            path: "/s".into(),
            source: std::io::Error::other("reset"),
        }),
    };
    assert!(matches!(classify_tc_error(read), CallError::Unknown(_)));
}

/// Why: item 3 — the real tc client calls `task.run` with the task and the
/// project, and returns the `session_id` it answers with.
/// Test: itself.
#[tokio::test]
async fn uds_tc_calls_task_run_and_returns_its_session_id() {
    let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let log = Arc::clone(&seen);
    let daemon = uds_mock::spawn(move |method, params| {
        log.lock()
            .expect("log")
            .push((method.to_string(), params.clone()));
        Box::pin(async move {
            Ok(json!({"session_id": "sess-42", "status": "running", "mode": "daily-driver"}))
        })
    })
    .await;
    let tc = UdsTc::new(daemon.socket());
    let handle = tc
        .task_run("ship it\nnow", Some(std::path::Path::new("/p")))
        .await
        .expect("task.run");
    assert_eq!(handle, "sess-42");
    let seen = seen.lock().expect("seen").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "task.run");
    assert_eq!(
        seen[0].1,
        json!({"task_description": "ship it\nnow", "project": "/p"})
    );
}

/// Why: item 3 / Fail-Open Check — the daemon's refusal is `NotSent`, an
/// answer without a handle is `Unknown`, and no daemon is `NotSent`.
/// Test: itself.
#[tokio::test]
async fn uds_tc_failures_are_never_a_handle() {
    let refusing = uds_mock::spawn(|_, _| {
        Box::pin(async { Err(RpcError::new(-32003, "task_description must not be empty")) })
    })
    .await;
    let err = UdsTc::new(refusing.socket())
        .task_run("x", None)
        .await
        .expect_err("refused");
    assert!(matches!(err, CallError::NotSent(_)), "{err:?}");

    let handleless = uds_mock::spawn(uds_mock::always(json!({"status": "running"}))).await;
    let err = UdsTc::new(handleless.socket())
        .task_run("x", None)
        .await
        .expect_err("no handle");
    assert!(matches!(err, CallError::Unknown(_)), "{err:?}");

    let dir = tempfile::tempdir().expect("dir");
    let err = UdsTc::new(dir.path().join("none.sock"))
        .task_run("x", None)
        .await
        .expect_err("no daemon");
    assert!(matches!(err, CallError::NotSent(_)), "{err:?}");
}
