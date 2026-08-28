//! The transport-neutral result of a route body, and its JSON-RPC projection
//! (#6288 slice 4).
//!
//! Why: slice 4 serves the managed-session, control-plane, and L2 proxy routes
//! over BOTH axum and the Unix socket, and the acceptance bar is one
//! implementation per route rather than two that drift. A handler body that
//! builds an `axum::response::Response` cannot be called from the socket, and
//! one that builds an `RpcResponse` cannot be called from axum. This type is
//! what both bodies return instead: a status plus a payload, with no transport
//! in it.
//!
//! What: [`RouteOutcome`] carries an HTTP-shaped status number and a
//! [`RouteBody`]. The socket side projects it through [`RouteOutcome::into_rpc`];
//! the axum side projects it through the `IntoResponse` impl that lives beside
//! the HTTP handlers, in `crate::daemon::managed_routes::route_outcome_http` —
//! deliberately NOT here, so nothing under `daemon::rpc` imports axum at all.
//!
//! The status number is not a transport leak. It is the routes' own refusal
//! vocabulary — "not found", "conflict", "forbidden" — which predates this
//! slice and is what the two projections have to agree on.
//! [`status_to_rpc_code`] is the single place that agreement is written down.
//!
//! Test: `outcome_tests` below, and the per-route parity tests in
//! `daemon::rpc::managed_tests`.

use serde::Serialize;
use trusty_common::uds::server::RpcError;

// #6288: the domain codes live in `daemon::error`, beside slice 2's
// `From<DaemonError> for RpcError` — one declaration per code, re-exported here
// so a route body reads them without reaching across families.
pub use crate::daemon::error::{
    CODE_CONFLICT, CODE_FORBIDDEN, CODE_NOT_FOUND, CODE_PANE_GONE, CODE_UNPROCESSABLE,
    CODE_WORKSPACE_GONE,
};

/// A route's payload, before any transport has been chosen.
///
/// `Json` is what every success arm produces; `Text` is what the refusal arms
/// produce, because the HTTP handlers answer a refusal with a bare string body
/// rather than a JSON envelope. Keeping both means each projection preserves
/// what the route actually returns instead of inventing a wrapper on one side.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteBody {
    /// A JSON document — the shape every 2xx arm returns.
    Json(serde_json::Value),
    /// A bare string — the shape every 4xx/5xx arm returns.
    Text(String),
}

/// One route's answer: a status plus a body, with no transport attached.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteOutcome {
    /// The status the HTTP surface reports, and the input to
    /// [`status_to_rpc_code`] on the socket surface.
    pub status: u16,
    /// The payload.
    pub body: RouteBody,
    /// A code the socket must use INSTEAD of [`status_to_rpc_code`]`(status)`.
    ///
    /// Why: exactly one route family — resume — distinguishes two refusals that
    /// share HTTP 422 using a response header, and the socket has no headers.
    /// Set by [`RouteOutcome::with_rpc_code`] so the discriminant survives as
    /// the RPC code instead of being dropped. Ignored by the HTTP projection.
    pub rpc_code: Option<i64>,
}

impl RouteOutcome {
    /// 200 with `value` serialized as the body.
    pub fn ok(value: &impl Serialize) -> Self {
        Self::json(200, value)
    }

    /// 201 with `value` serialized as the body.
    pub fn created(value: &impl Serialize) -> Self {
        Self::json(201, value)
    }

    /// `status` with `value` serialized as the body.
    ///
    /// A body that will not serialize becomes a 500 carrying the serde message
    /// on both transports, never a panic — these bodies run inside a daemon
    /// that must outlive one bad response.
    pub fn json(status: u16, value: &impl Serialize) -> Self {
        match serde_json::to_value(value) {
            Ok(v) => Self {
                status,
                body: RouteBody::Json(v),
                rpc_code: None,
            },
            Err(e) => Self::text(500, format!("serialize response: {e}")),
        }
    }

    /// `status` with `message` as a bare string body — the refusal shape.
    pub fn text(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            body: RouteBody::Text(message.into()),
            rpc_code: None,
        }
    }

    /// Override the RPC code this refusal projects onto. See [`Self::rpc_code`].
    pub fn with_rpc_code(mut self, code: i64) -> Self {
        self.rpc_code = Some(code);
        self
    }

    /// True when the status is 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Project onto the result/error pair the socket writes.
    ///
    /// Why: `RpcError` has no status field, so a refusal's identity survives as
    /// a CODE plus the SAME message the HTTP body carries. A caller comparing
    /// the two transports reads identical message text; only the numeric label
    /// differs, and [`status_to_rpc_code`] is that mapping.
    /// What: a 2xx becomes `Ok(value)` — the JSON body verbatim, a text body as
    /// a JSON string. Anything else becomes `Err(RpcError)` whose message is the
    /// text body verbatim.
    /// Test: `success_projects_the_body_verbatim`,
    /// `refusal_keeps_the_http_message_and_maps_the_status`.
    pub fn into_rpc(self) -> Result<serde_json::Value, RpcError> {
        let success = self.is_success();
        let code = self
            .rpc_code
            .unwrap_or_else(|| status_to_rpc_code(self.status));
        match (success, self.body) {
            (true, RouteBody::Json(v)) => Ok(v),
            (true, RouteBody::Text(s)) => Ok(serde_json::Value::String(s)),
            (false, RouteBody::Text(s)) => Err(RpcError::new(code, s)),
            (false, RouteBody::Json(v)) => Err(RpcError::new(code, v.to_string())),
        }
    }
}

/// The JSON-RPC error code a refusal status maps to.
///
/// The mapping itself is `rpc_code_for_status` in `daemon::error`, shared with
/// slice 2's `From<DaemonError> for RpcError` so one status cannot answer two
/// codes depending on which family served the route. This alias is the name the
/// route bodies and the parity tests read.
pub use crate::daemon::error::rpc_code_for_status as status_to_rpc_code;

#[cfg(test)]
mod outcome_tests {
    use trusty_common::uds::server::{CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS};

    use super::*;

    #[test]
    fn success_projects_the_body_verbatim() {
        let outcome = RouteOutcome::ok(&serde_json::json!({"id": "abc", "n": 3}));
        let value = outcome.into_rpc().expect("2xx is Ok");
        assert_eq!(value, serde_json::json!({"id": "abc", "n": 3}));
    }

    #[test]
    fn created_is_a_success_like_ok() {
        let outcome = RouteOutcome::created(&serde_json::json!({"id": "abc"}));
        assert!(outcome.is_success());
        assert!(outcome.into_rpc().is_ok());
    }

    #[test]
    fn refusal_keeps_the_http_message_and_maps_the_status() {
        let outcome = RouteOutcome::text(404, "session zzz not found");
        let err = outcome.into_rpc().expect_err("4xx is Err");
        assert_eq!(err.code, CODE_NOT_FOUND);
        assert_eq!(
            err.message, "session zzz not found",
            "the socket must carry the HTTP body verbatim, not a paraphrase"
        );
    }

    #[test]
    fn status_to_rpc_code_maps_each_refusal_class() {
        assert_eq!(status_to_rpc_code(400), CODE_INVALID_PARAMS);
        assert_eq!(status_to_rpc_code(403), CODE_FORBIDDEN);
        assert_eq!(status_to_rpc_code(404), CODE_NOT_FOUND);
        assert_eq!(status_to_rpc_code(409), CODE_CONFLICT);
        assert_eq!(status_to_rpc_code(422), CODE_UNPROCESSABLE);
        assert_eq!(status_to_rpc_code(500), CODE_INTERNAL_ERROR);
        assert_eq!(status_to_rpc_code(502), CODE_INTERNAL_ERROR);
    }

    /// The two 422 resume refusals keep distinct codes, since the socket has no
    /// place for the `x-trusty-resume-reason` header that separates them on
    /// HTTP (#6288).
    #[test]
    fn an_rpc_code_override_replaces_the_status_projection() {
        let err = RouteOutcome::text(422, "workspace /gone no longer exists")
            .with_rpc_code(CODE_WORKSPACE_GONE)
            .into_rpc()
            .expect_err("422 is Err");
        assert_eq!(err.code, CODE_WORKSPACE_GONE);
        assert_eq!(err.message, "workspace /gone no longer exists");

        let pane = RouteOutcome::text(422, "pane %42 no longer exists")
            .with_rpc_code(CODE_PANE_GONE)
            .into_rpc()
            .expect_err("422 is Err");
        assert_ne!(
            pane.code, err.code,
            "the two 422 classes must stay distinguishable without a header"
        );
    }

    #[test]
    fn an_unserializable_body_is_a_500_not_a_panic() {
        let bad: std::collections::HashMap<(u8, u8), u8> =
            std::collections::HashMap::from([((1, 2), 3)]);
        let outcome = RouteOutcome::ok(&bad);
        assert_eq!(outcome.status, 500);
        assert!(outcome.into_rpc().is_err());
    }
}
