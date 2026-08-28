//! The four `/pair/*` verbs as RPC methods, plus their shared bodies (#6288
//! slice 5).
//!
//! Why the bodies are here: the handlers live in `api.rs`, which sits on a
//! frozen SLOC ratchet budget, so four `*_op` functions could not be added
//! there. Each is a single call into [`PairingService`], which is where the
//! code, its five-minute TTL, and the on-disk `pairing.json` all already live —
//! so "the body" is genuinely one line and both transports share it.
//!
//! ## Guard parity
//!
//! The HTTP `/pair/*` routes carry NO guard beyond the daemon's loopback bind:
//! `api::origin_guard::origin_allowed` is referenced from exactly one call site
//! in the whole crate (`api/coordinator_routes.rs:193`, the action-capable chat
//! endpoint), and no `/pair/*` route is layered with it. So there is nothing
//! stronger than the socket's own peer-uid check to carry across, and the RPC
//! path is not weaker than the HTTP one it mirrors — it is strictly narrower,
//! since the socket is `0600` in a `0700` directory and a browser cannot reach
//! it at all. The same comparison holds for every `/api/v1/manager/*` route.
//!
//! What matters for `pair/confirm` specifically: the code check itself is
//! [`PairingService::confirm`]'s, not a transport's, and it is unchanged. A
//! wrong or expired code answers `success: false` on both transports.
//!
//! Test: the `parity_pair_*` cases in `super::tests`.
//!
//! [`PairingService`]: crate::daemon::services::PairingService
//! [`PairingService::confirm`]: crate::daemon::services::PairingService::confirm

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::api::PairConfirmRequest;
use crate::daemon::api::types::{PairConfirmResponse, PairResetResponse};
use crate::daemon::services::{PairCode, PairStatus, PairingService};
use crate::daemon::state::DaemonState;

use super::NoParams;

/// `POST /pair/request`, `mpm.pair.request` — mint a one-time pairing code.
///
/// Test: `parity_pair_request_agrees_across_transports`.
pub fn request_op(state: &Arc<DaemonState>) -> PairCode {
    PairingService::new(state).request_code()
}

/// `POST /pair/confirm`, `mpm.pair.confirm` — validate a code and bind a chat.
///
/// Why this answers `200`-shaped on both transports even when the code is
/// wrong: the route has always reported a rejection INSIDE the body
/// (`success: false`, `error: "invalid or expired code"`) rather than as a
/// status, because a bot polling this endpoint needs to distinguish "your code
/// was wrong" from "the daemon is unreachable". Turning the rejection into an
/// RPC error frame on the socket would erase that distinction for a socket
/// caller, so the body stays the body. The check itself is unchanged.
/// Test: `parity_pair_confirm_agrees_across_transports`,
/// `rpc_pair_confirm_rejects_a_bad_code_in_the_body`.
pub fn confirm_op(state: &Arc<DaemonState>, req: &PairConfirmRequest) -> PairConfirmResponse {
    match PairingService::new(state).confirm(&req.code, req.chat_id) {
        Ok(()) => PairConfirmResponse {
            success: true,
            chat_id: Some(req.chat_id),
            error: None,
        },
        Err(_) => PairConfirmResponse {
            success: false,
            chat_id: None,
            error: Some("invalid or expired code".to_string()),
        },
    }
}

/// `GET /pair/status`, `mpm.pair.status` — is a chat paired?
///
/// Test: `parity_pair_status_agrees_across_transports`.
pub fn status_op(state: &Arc<DaemonState>) -> PairStatus {
    PairingService::new(state).status()
}

/// `POST /pair/reset`, `mpm.pair.reset` — drop the binding, memory and disk.
///
/// Test: `parity_pair_reset_agrees_across_transports`.
pub fn reset_op(state: &Arc<DaemonState>) -> PairResetResponse {
    PairingService::new(state).reset();
    PairResetResponse { reset: true }
}

/// `mpm.pair.confirm` parameters — the same two fields the HTTP body carries.
#[derive(Debug, Deserialize)]
pub struct ConfirmParams {
    /// The one-time pairing code.
    pub code: String,
    /// The Telegram chat id to bind.
    pub chat_id: i64,
}

/// Mount the four pairing methods.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let held = Arc::clone(state);
    let r = router.typed::<NoParams, PairCode, _, _>("mpm.pair.request", move |_| {
        let s = Arc::clone(&held);
        async move { Ok(request_op(&s)) }
    });

    let held = Arc::clone(state);
    let r = r.typed::<ConfirmParams, PairConfirmResponse, _, _>("mpm.pair.confirm", move |p| {
        let s = Arc::clone(&held);
        async move {
            Ok(confirm_op(
                &s,
                &PairConfirmRequest {
                    code: p.code,
                    chat_id: p.chat_id,
                },
            ))
        }
    });

    let held = Arc::clone(state);
    let r = r.typed::<NoParams, PairStatus, _, _>("mpm.pair.status", move |_| {
        let s = Arc::clone(&held);
        async move { Ok(status_op(&s)) }
    });

    let held = Arc::clone(state);
    r.typed::<NoParams, PairResetResponse, _, _>("mpm.pair.reset", move |_| {
        let s = Arc::clone(&held);
        async move { Ok(reset_op(&s)) }
    })
}
