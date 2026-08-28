//! The L3 portfolio-manager surface as RPC methods (#6288 slice 5).
//!
//! Why registration only: every `manager/*` submodule had SLOC headroom, so the
//! `*_op` bodies stayed beside their handlers.
//!
//! ## What `mpm.manager.digest` answers, and why it is not three error frames
//!
//! `GET /api/v1/manager/digest` answers `503` and `502` with a COMPLETE body —
//! the deterministic fallback narrative plus the full rollup — because DOC-16
//! D1 requires the numbers to survive an inference failure. A JSON-RPC error
//! frame is `{code, message}` and can carry neither. So `digest_op` reports
//! those two as `Ok` values ([`DigestOutcome`]), the socket returns the body,
//! and the caller reads the `error` field that is already on it
//! (`"inference_unavailable"` / `"inference_failed"`) — the same field an HTTP
//! caller reads. Only the three empty refusals (bad scope, unknown project,
//! store read failed) are RPC errors.
//!
//! ## Guard parity
//!
//! No `/api/v1/manager/*` route carries an HTTP-side guard: `origin_allowed` is
//! referenced from one call site in the crate, the action-capable coordinator
//! chat endpoint, and none of these six routes is layered with it. There is
//! therefore no guard stronger than the socket's peer-uid check to carry over.
//! The mutating verb, `mpm.manager.act`, keeps its own gate — it executes only
//! on `confirm: true`, decided inside the shared body, not by a transport.
//!
//! Test: the `parity_manager_*` cases in `super::tests`.
//!
//! [`DigestOutcome`]: crate::daemon::manager::digest::DigestOutcome

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::manager::{
    ActRequest, ActResponse, ChatReplyBody, ChatRequestBody, ManagerVersionResponse,
    PortfolioStatusResponse, RouteTaskRequest, RouteTaskResponse, act, chat, digest, route_task,
    status, version,
};
use crate::daemon::state::DaemonState;

use super::NoParams;

/// `mpm.manager.digest` parameters — the same optional `scope` selector the
/// HTTP query string carries.
///
/// Why the registration wraps this in `Option`: every field is optional, so
/// `GET /api/v1/manager/digest` with no query string is the ordinary call — and
/// its socket counterpart sends no `params` at all, which arrives as `null`.
/// Serde refuses `null` for a struct even when every field defaults, so a bare
/// `DigestParams` would make the most common digest call fail with
/// `invalid_params`. `Option<DigestParams>` accepts `null` and an object alike.
/// Test: `parity_manager_digest_agrees_across_transports` calls it with no
/// params; `rpc_manager_digest_rejects_a_malformed_scope` calls it with some.
#[derive(Debug, Default, Deserialize)]
pub struct DigestParams {
    /// `portfolio` | `project:<name>`; absent means portfolio.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Mount the six manager methods.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let held = Arc::clone(state);
    let r =
        router.typed::<NoParams, ManagerVersionResponse, _, _>("mpm.manager.version", move |_| {
            let s = Arc::clone(&held);
            async move { Ok(version::version_op(&s)) }
        });

    let held = Arc::clone(state);
    let r = r.typed::<NoParams, PortfolioStatusResponse, _, _>("mpm.manager.status", move |_| {
        let s = Arc::clone(&held);
        async move { status::load_portfolio_status(&s).await.map_err(Into::into) }
    });

    let held = Arc::clone(state);
    let r = r.typed::<Option<DigestParams>, digest::DigestResponse, _, _>(
        "mpm.manager.digest",
        move |p| {
            let s = Arc::clone(&held);
            async move {
                let scope = p.unwrap_or_default().scope;
                digest::digest_op(&s, digest::DigestQuery { scope })
                    .await
                    .map(digest::DigestOutcome::into_body)
                    .map_err(Into::into)
            }
        },
    );

    let held = Arc::clone(state);
    let r = r.typed::<ChatRequestBody, ChatReplyBody, _, _>("mpm.manager.chat", move |p| {
        let s = Arc::clone(&held);
        async move { chat::chat_op(&s, p).await.map_err(Into::into) }
    });

    let held = Arc::clone(state);
    let r =
        r.typed::<RouteTaskRequest, RouteTaskResponse, _, _>("mpm.manager.route_task", move |p| {
            let s = Arc::clone(&held);
            async move { route_task::route_task_op(&s, p).await.map_err(Into::into) }
        });

    let held = Arc::clone(state);
    r.typed::<ActRequest, ActResponse, _, _>("mpm.manager.act", move |p| {
        let s = Arc::clone(&held);
        async move { act::act_op(&s, p).await.map_err(Into::into) }
    })
}
