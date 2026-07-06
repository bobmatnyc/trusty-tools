//! Proof-of-life JSON-RPC methods for `tcode serve` (#2053).
//!
//! Why: the transport + router need at least one real method to prove the
//! whole path works end-to-end without pulling in `session.*`/`task.*`/
//! `harness.describe` scope (those land in #2054/#2056/#2066). `ping` and
//! `health` are the minimal, side-effect-free pair for that job, shared
//! verbatim across both transports: the STDIO `health` JSON-RPC method and
//! the HTTP `GET /health` route (`crate::serve::http::health_handler`) both
//! call [`health_payload`] so the two transports can never drift into
//! different shapes.
//! What: `register` wires both methods into a [`Router`]; `ping` returns
//! `{"pong": true}`; `health` returns [`health_payload`]'s server name,
//! crate version, and a static `"ok"` status.
//! Test: `methods::tests::*`.

use serde_json::{Value, json};

use crate::jsonrpc::{Router, RpcError};

/// Register every proof-of-life method onto `router`.
///
/// Why: keeps `crate::serve::build_router` a one-line call as more method
/// groups (session/task/harness) register themselves the same way later.
/// What: registers `"ping"` and `"health"`.
/// Test: `methods_register_wires_ping_and_health`.
pub fn register(router: &mut Router) {
    router.register("ping", ping);
    router.register("health", health);
}

/// `ping` — returns `{"pong": true}` regardless of `params`.
///
/// Why: the cheapest possible round-trip proof that a transport and the
/// router are wired correctly.
/// What: ignores `params`, never fails.
/// Test: `ping_returns_pong_true`.
async fn ping(_params: Value) -> Result<Value, RpcError> {
    Ok(json!({"pong": true}))
}

/// `health` — returns server identity, crate version, and status.
///
/// Why: a slightly richer proof-of-life than `ping` that a caller can use to
/// confirm which build of `tcode` it is talking to.
/// What: ignores `params`, never fails; the payload is [`health_payload`].
/// Test: `health_returns_server_identity`.
async fn health(_params: Value) -> Result<Value, RpcError> {
    Ok(health_payload())
}

/// The shared `health` payload: `{"server","version","status"}`.
///
/// Why: pulled out of the `health` JSON-RPC handler so
/// `crate::serve::http::health_handler` (`GET /health`) can return the
/// identical shape without duplicating the `json!` literal — one source of
/// truth for what "healthy" means over either transport.
/// What: `server = "tcode"`, `version = crate::VERSION`
/// (`CARGO_PKG_VERSION`), `status = "ok"`. Never fails — there is no
/// fallible state to check yet; a future ticket that adds real readiness
/// checks (e.g. project root validity) will extend this function.
/// Test: `health_payload_has_expected_shape`.
pub(crate) fn health_payload() -> Value {
    json!({
        "server": "tcode",
        "version": crate::VERSION,
        "status": "ok",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ping` must return `{"pong": true}` and never fail.
    #[tokio::test]
    async fn ping_returns_pong_true() {
        let result = ping(Value::Null).await.expect("ping must not fail");
        assert_eq!(result, json!({"pong": true}));
    }

    /// `health` must report the server name, version, and "ok" status.
    #[tokio::test]
    async fn health_returns_server_identity() {
        let result = health(Value::Null).await.expect("health must not fail");
        assert_eq!(result["server"], "tcode");
        assert_eq!(result["status"], "ok");
        assert_eq!(result["version"], crate::VERSION);
    }

    /// `health_payload` (shared with the HTTP `GET /health` route) must
    /// carry the same three fields.
    #[test]
    fn health_payload_has_expected_shape() {
        let v = health_payload();
        assert_eq!(v["server"], "tcode");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["version"], crate::VERSION);
    }

    /// `register` must wire both methods so the router recognises them
    /// (rather than returning method-not-found).
    #[tokio::test]
    async fn methods_register_wires_ping_and_health() {
        use trusty_common::mcp::Request;

        let mut router = Router::new();
        register(&mut router);

        for method in ["ping", "health"] {
            let req = Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(1)),
                method: method.to_string(),
                params: None,
            };
            let resp = router.dispatch(req).await;
            assert!(
                resp.error.is_none(),
                "{method} should be registered, got {:?}",
                resp.error
            );
        }
    }
}
