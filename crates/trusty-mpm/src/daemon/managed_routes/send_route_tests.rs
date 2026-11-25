//! Tests for `POST /api/v1/sessions/managed/{id}/send` (#11796).
//!
//! Why: the REST route behind `tm sessions send` typed into a Claude Code
//! input box that already held a draft, pressed Enter, and answered
//! `sent: true`, submitting the draft and the message as one turn. MCP
//! `session_send` already refused that (#10747); the route must too.
//! What: drives [`super::cores::send_core`] (the body both HTTP and the
//! socket serve) over a [`FakeTmuxDriver`] whose input box holds a draft, and
//! over one whose box is empty.
//! Test: this file IS the test module.

use std::path::PathBuf;
use std::sync::Arc;

use super::SendInputRequest;
use super::cores::send_core;
use crate::daemon::rpc::managed::outcome::{RouteBody, RouteOutcome};
use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedSessionState;
use crate::session_manager::tests::FakeTmuxDriver;

/// The #11796 incident: the owner's unsent draft sits in the `❯` box.
const DRAFT_BOX: &str = "out\n────────────\n❯ draft do not send\n────────────\n";

/// A Claude Code input box with nothing typed.
const EMPTY_BOX: &str = "out\n────────────\n❯ \n────────────\n";

/// An Active Claude Code session on pane `%1`; `escaped` is the box the
/// pre-type read sees, `plain` every other capture.
async fn active_session(
    root: &tempfile::TempDir,
    escaped: &str,
    plain: &str,
) -> (Arc<DaemonState>, Arc<FakeTmuxDriver>, String) {
    let fake = FakeTmuxDriver::new();
    *fake.pane_id_override.lock().unwrap() = Some("%1".into());
    fake.escaped_capture_responses
        .lock()
        .unwrap()
        .insert("%1".into(), escaped.into());
    fake.capture_responses
        .lock()
        .unwrap()
        .insert("%1".into(), plain.into());
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(root.path().to_path_buf(), fake.clone())
            .await,
    );
    let mgr = state.session_manager().await;
    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).await.expect("record");
        r.state = ManagedSessionState::Active;
        store.upsert(r).await.expect("mark active");
    }
    (state, fake, record.id.to_string())
}

/// The JSON body of a route outcome.
fn json_body(outcome: &RouteOutcome) -> serde_json::Value {
    match &outcome.body {
        RouteBody::Json(v) => v.clone(),
        RouteBody::Text(s) => panic!("expected a JSON body, got text: {s}"),
    }
}

/// #11796: a draft in the box refuses the send. Nothing is typed, no key is
/// pressed (no C-u), and the answer is a 409 with `sent: false` and the
/// reason. Pre-fix the route pressed Enter on the draft and answered 200.
/// Test: this function IS the test.
#[tokio::test]
async fn rest_send_refuses_a_typed_draft() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let (state, fake, id) = active_session(&root, DRAFT_BOX, DRAFT_BOX).await;

    let outcome = send_core(
        &state,
        &id,
        SendInputRequest {
            text: "hello control".into(),
        },
    )
    .await;

    let body = json_body(&outcome);
    assert_eq!(outcome.status, 409, "{body}");
    assert_eq!(body["sent"], serde_json::json!(false), "{body}");
    assert_eq!(body["submit_state"], "input_not_empty", "{body}");
    assert!(body["reason"].is_string(), "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("input_not_empty")),
        "the client prints `error`: {body}"
    );
    assert!(fake.pane_literal_calls.lock().unwrap().is_empty(), "typed");
    assert!(
        fake.pane_key_calls.lock().unwrap().is_empty(),
        "key pressed"
    );
    assert!(
        fake.pane_send_calls.lock().unwrap().is_empty(),
        "text+Enter"
    );
}

/// #11796: an empty box still delivers: 200, `sent: true`, `submitted`.
/// Paired with `rest_send_refuses_a_typed_draft`, this proves the verdict
/// tracks the box rather than refusing every send.
/// Test: this function IS the test.
#[tokio::test]
async fn rest_send_delivers_into_an_empty_box() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let (state, fake, id) = active_session(&root, EMPTY_BOX, "● Working…\n❯ hello control\n").await;

    let outcome = send_core(
        &state,
        &id,
        SendInputRequest {
            text: "hello control".into(),
        },
    )
    .await;

    let body = json_body(&outcome);
    assert_eq!(outcome.status, 200, "{body}");
    assert_eq!(body["sent"], serde_json::json!(true), "{body}");
    assert_eq!(body["submit_state"], "submitted", "{body}");
    assert!(body.get("error").is_none(), "{body}");
    let keys = fake.pane_key_calls.lock().unwrap().clone();
    assert_eq!(keys, vec![("%1".to_string(), "Enter".to_string())]);
}
