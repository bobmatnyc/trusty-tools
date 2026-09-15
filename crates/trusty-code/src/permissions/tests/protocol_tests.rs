//! JSON-RPC surface tests for `session.permission.respond` (#7948).
//!
//! Why: a method that is never registered, or that mis-maps its decision
//! word, would pass every gate test and still leave a client unable to answer
//! a prompt.
//! What: drives a real `Router` end to end — a gate suspended on an `ask` is
//! released by a `session.permission.respond` request.
//! Test: this module.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use trusty_mcp::Request;

use crate::jsonrpc::{ConnectionContext, Router};
use crate::permissions::protocol::{PERMISSION_RESPOND_METHOD, PermissionRespondParams, register};
use crate::permissions::{
    Outcome, PermissionBroker, PermissionContext, PermissionDecision, PermissionEvents,
    PermissionMode,
};

use super::agent_with_permissions;

fn params(decision: &str, pattern: Option<&str>) -> PermissionRespondParams {
    PermissionRespondParams {
        session_id: "s".into(),
        request_id: "r".into(),
        decision: decision.into(),
        pattern: pattern.map(str::to_string),
    }
}

/// Every wire word maps onto its decision, and nothing else does.
#[test]
fn decision_from_params_maps_every_word() {
    assert_eq!(
        params("allow_once", None).decision().expect("maps"),
        PermissionDecision::AllowOnce
    );
    assert_eq!(
        params("allow_for_session", Some("git *"))
            .decision()
            .expect("maps"),
        PermissionDecision::AllowForSession {
            pattern: Some("git *".into())
        }
    );
    assert_eq!(
        params("deny", None).decision().expect("maps"),
        PermissionDecision::Deny
    );
    assert_eq!(PermissionDecision::AllowOnce.as_str(), "allow_once");
    assert_eq!(
        PermissionDecision::AllowForSession { pattern: None }.as_str(),
        "allow_for_session"
    );
    assert_eq!(PermissionDecision::Deny.as_str(), "deny");
}

/// Dispatch one `session.permission.respond` request through `router`.
async fn dispatch(router: &Router, body: serde_json::Value) -> trusty_mcp::Response {
    let (tx, _rx) = mpsc::unbounded_channel();
    router
        .dispatch(
            Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(1)),
                method: PERMISSION_RESPOND_METHOD.to_string(),
                params: Some(body),
            },
            &ConnectionContext::new(tx),
        )
        .await
}

fn router_with(broker: &Arc<PermissionBroker>) -> Router {
    let mut router = Router::new();
    register(&mut router, Arc::clone(broker));
    router
}

/// The method is registered — a client can actually reach it.
#[tokio::test]
async fn register_wires_the_respond_method() {
    let router = router_with(&Arc::new(PermissionBroker::new()));
    let resp = dispatch(
        &router,
        json!({"session_id": "s", "request_id": "r", "decision": "deny"}),
    )
    .await;
    let err = resp
        .error
        .expect("no request is pending, so this must error");
    assert_ne!(
        err.code, -32601,
        "method-not-found means it was never registered"
    );
}

/// An unknown decision word is `-32602`, never a guessed fallback.
#[tokio::test]
async fn respond_rejects_an_unknown_decision_word() {
    let router = router_with(&Arc::new(PermissionBroker::new()));
    let resp = dispatch(
        &router,
        json!({"session_id": "s", "request_id": "r", "decision": "maybe"}),
    )
    .await;
    let err = resp.error.expect("must reject");
    assert_eq!(err.code, -32602, "{err:?}");
    assert!(err.message.contains("maybe"), "{}", err.message);
}

/// A request id nobody is waiting on is `-32602` naming the id.
#[tokio::test]
async fn respond_rejects_an_unknown_request_id() {
    let broker = Arc::new(PermissionBroker::new());
    // Materialise the session so the failure is about the REQUEST.
    let _ = broker.session("s");
    let resp = dispatch(
        &router_with(&broker),
        json!({"session_id": "s", "request_id": "ghost", "decision": "deny"}),
    )
    .await;
    let err = resp.error.expect("must reject");
    assert_eq!(err.code, -32602, "{err:?}");
    assert!(err.message.contains("ghost"), "{}", err.message);
}

/// END TO END: a gate suspended on an `ask` is released by a real dispatch.
#[tokio::test]
async fn respond_resolves_a_pending_request() {
    let broker = Arc::new(PermissionBroker::new());
    let router = router_with(&broker);
    let events = Arc::new(RequestIdCapture::default());
    let ctx = PermissionContext {
        mode: PermissionMode::Default,
        timeout: Duration::from_secs(5),
        session_id: "s-rpc".into(),
        ask: Some(broker.session("s-rpc")),
        events: Some(Arc::clone(&events) as Arc<dyn PermissionEvents>),
        root: None,
    };
    let gate = ctx.gate_for(
        Arc::new(agent_with_permissions("permissions:\n  bash: ask\n")),
        "agent-1",
    );
    let resolving =
        tokio::spawn(async move { gate.resolve("bash", &json!({"command": "ls"})).await });

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let request_id = loop {
        if let Some(id) = events.last() {
            break id;
        }
        assert!(std::time::Instant::now() < deadline, "no request within 5s");
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    let resp = dispatch(
        &router,
        json!({"session_id": "s-rpc", "request_id": request_id, "decision": "allow_once"}),
    )
    .await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    assert_eq!(resp.result.expect("result"), json!({"accepted": true}));
    assert_eq!(
        resolving.await.expect("gate task must not panic"),
        Outcome::Allow
    );
}

/// Captures request ids off the event stream.
#[derive(Default)]
struct RequestIdCapture {
    ids: Mutex<Vec<String>>,
}

impl RequestIdCapture {
    fn last(&self) -> Option<String> {
        self.ids.lock().expect("lock").last().cloned()
    }
}

impl PermissionEvents for RequestIdCapture {
    fn permission_requested(
        &self,
        _session_id: &str,
        request_id: &str,
        _agent: &str,
        _agent_id: &str,
        _tool: &str,
        _subject: &str,
        _rule: &str,
    ) {
        self.ids.lock().expect("lock").push(request_id.to_string());
    }

    fn permission_resolved(
        &self,
        _session_id: &str,
        _request_id: &str,
        _agent: &str,
        _agent_id: &str,
        _decision: &str,
        _source: &str,
    ) {
    }
}
