//! The SESSCTL control-plane routes as JSON-RPC methods, and the caller-trust
//! token the #6197 guard travels on (#6288 slice 4).
//!
//! Why: `ctl_run_session` spawns a caller-named executable from a
//! caller-supplied request. #6197 was that it did so with no validation and no
//! proof the caller was local; PR #6205 (`a2c9d0f2e`) closed it with a caller
//! check and three input validators, all written as HTTP-extractor code. Moving
//! this family onto a second transport is exactly the change that could leave
//! those checks behind, so they were moved INTO the shared body first
//! (`control_routes::ctl_run_core`) and the caller check was given a type.
//!
//! What: [`CallerTrust`] is that type — an opaque newtype over a PRIVATE enum,
//! so a trusted verdict has no literal any caller can write. Its only two
//! constructors are `pub(crate)` and each belongs to one transport's guard:
//! `from_loopback_check` for `control_routes::loopback_guard`, and
//! `from_peer_checked_socket` for the socket's accept-time peer-uid check. The
//! `compile_fail` doctests on the type prove an outside caller cannot reach
//! either, nor name a variant, nor build one by hand.
//!
//! | Method | Route |
//! |---|---|
//! | `mpm.control.list` | `GET /api/v1/control/sessions` |
//! | `mpm.control.run` | `POST /api/v1/control/sessions/run` |
//! | `mpm.control.connect` | `POST /api/v1/control/sessions/{id}/connect` (SSE → stream) |
//! | `mpm.control.stop` | `POST /api/v1/control/sessions/{id}/stop` |
//! | `mpm.control.auth` | `GET /api/v1/control/sessions/{id}/auth` |
//!
//! `mpm.control.connect` is the one route whose HTTP form is an SSE stream. It
//! is registered with [`RpcRouter::typed_stream`], so a caller sends
//! `"stream": true` and reads one frame per `SessionEvent` — the same events,
//! in the same order, that the SSE `data:` lines carry.
//!
//! Test: `rpc_ctl_*` in `daemon::rpc::managed_tests`.

use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use trusty_common::uds::server::{RpcError, RpcRouter, RpcStreamItems};

use super::outcome::{CODE_NOT_FOUND, RouteOutcome};
use crate::control::ControlSessionId;
use crate::daemon::api::control_routes::{
    CtlListQuery, CtlRunRequest, ctl_auth_core, ctl_list_core, ctl_run_core, ctl_stop_core,
};
use crate::daemon::state::DaemonState;

/// Whether the transport proved the caller is entitled to the control plane
/// (#6197, carried across transports by #6288).
///
/// Why: the check is a property of the CONNECTION, not of the request body, so
/// it cannot live inside a route body that both transports share — but its
/// RESULT must, or the body has no way to refuse. This enum is that result, and
/// making it a parameter means a new caller of a control-plane body has to say
/// which transport vouched for it. There is no `Default`, deliberately.
///
/// **The socket's proof is strictly stronger than HTTP's.** `loopback_guard`
/// asks whether the peer address is loopback — whether the caller is on this
/// HOST. `trusty_common::uds::ensure_peer_is_self`, which `serve_until` runs on
/// every accepted connection before a byte is read, asks whether the peer's uid
/// is THIS USER'S. Same-uid is a strict subset of same-host, so every caller the
/// socket admits would also have passed `loopback_guard`, while a different
/// local user — admitted by `loopback_guard` — is refused by the socket. The
/// socket path is additionally `0600` inside a `0700` directory, so a foreign
/// uid cannot reach `connect(2)` at all. Nothing weaker is carried across.
///
/// Test: `caller_trust_refusal_matches_the_http_403`,
/// `rpc_ctl_run_refuses_an_untrusted_caller`.
/// The three `compile_fail` blocks below are the proof, and this one is what
/// keeps them honest: a `compile_fail` test passes for ANY compile error, a
/// mistyped import path included, so one block that MUST compile pins the path
/// they all use. If `CallerTrust` ever stops being reachable at this path, this
/// block fails and the three below stop proving anything silently.
///
/// ```
/// use trusty_mpm::daemon::rpc::managed::control::CallerTrust;
/// assert!(CallerTrust::REFUSAL.contains("loopback-only"));
/// ```
///
/// ```compile_fail
/// // #6288: the verdict has no public variant, so a caller cannot name one.
/// use trusty_mpm::daemon::rpc::managed::control::CallerTrust;
/// let _trusted = CallerTrust::LocalVerified;
/// ```
///
/// ```compile_fail
/// // #6288: nor build one by hand — the inner verdict is private.
/// use trusty_mpm::daemon::rpc::managed::control::CallerTrust;
/// let _trusted = CallerTrust(());
/// ```
///
/// ```compile_fail
/// // #6288: nor call a constructor — both are crate-internal.
/// use trusty_mpm::daemon::rpc::managed::control::CallerTrust;
/// let _trusted = CallerTrust::from_peer_checked_socket();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerTrust(Verdict);

/// The verdict [`CallerTrust`] wraps.
///
/// Private on purpose, and the whole mechanism behind the type's guarantee: a
/// newtype over a private type has no expressible literal outside this module,
/// so `CallerTrust`'s only constructors are the two below. Making this `pub`
/// would restore exactly the hole #6347's review found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The transport proved the caller local. On HTTP: a loopback peer address.
    /// On the socket: a peer uid equal to this process's.
    LocalVerified,
    /// The transport did not. Every control-plane body refuses.
    Unverified,
}

impl CallerTrust {
    /// The refusal body, byte-identical on both transports (#6197).
    pub const REFUSAL: &'static str =
        "control-plane session routes are loopback-only; remote access is forbidden";

    /// The trust a request arriving on the peer-checked socket carries.
    ///
    /// Why a named constructor rather than a variant: this is the ONE place the
    /// socket's claim is made, so the invariant behind it has one place to be
    /// written down and one place to be reviewed. The claim is that
    /// `serve_until` ran `ensure_peer_is_self` on this connection before any
    /// byte of the request was read — see `daemon::socket`, which is the only
    /// module that mounts this router.
    pub(crate) fn from_peer_checked_socket() -> Self {
        Self(Verdict::LocalVerified)
    }

    /// The trust an HTTP request carries, given whether its peer is loopback.
    ///
    /// Why it takes a `bool` rather than a `SocketAddr`: the address check is
    /// `control_routes::loopback_guard`'s, which owns the logging and the
    /// `is_loopback` predicate. This is only the verdict's constructor, and
    /// keeping it address-free is what lets `daemon::rpc` stay free of HTTP
    /// types (#6288).
    /// Test: `loopback_guard_accepts_loopback_refuses_remote`.
    pub(crate) fn from_loopback_check(peer_is_loopback: bool) -> Self {
        if peer_is_loopback {
            Self(Verdict::LocalVerified)
        } else {
            Self(Verdict::Unverified)
        }
    }

    /// A caller no transport vouched for. Refused by every control-plane body.
    ///
    /// Test-only: both production paths mint their verdict from a real check
    /// (`from_loopback_check`, `from_peer_checked_socket`), so nothing outside a
    /// test needs to name a refusal directly. Gated rather than left dead so a
    /// future production caller has to justify itself in review.
    #[cfg(test)]
    pub(crate) fn unverified() -> Self {
        Self(Verdict::Unverified)
    }

    /// Whether a transport vouched for this caller.
    ///
    /// Test: `loopback_guard_accepts_loopback_refuses_remote`.
    pub fn is_local(self) -> bool {
        self.0 == Verdict::LocalVerified
    }

    /// `Ok(())` when the caller is entitled; otherwise the shared 403.
    ///
    /// # Errors
    ///
    /// An unverified caller — as a 403 [`RouteOutcome`] carrying
    /// [`Self::REFUSAL`], which projects onto
    /// [`super::outcome::CODE_FORBIDDEN`] on the socket.
    ///
    /// Test: `caller_trust_refusal_matches_the_http_403`,
    /// `rpc_ctl_run_refuses_an_untrusted_caller`.
    pub fn ensure_local(self) -> Result<(), RouteOutcome> {
        match self.0 {
            Verdict::LocalVerified => Ok(()),
            Verdict::Unverified => Err(RouteOutcome::text(403, Self::REFUSAL)),
        }
    }
}

/// A control-plane request naming a session by id.
#[derive(Debug, Deserialize)]
pub struct CtlSessionParams {
    /// The control-plane session id the HTTP route reads from its path.
    pub id: String,
}

/// `mpm.control.stop` parameters: the id, plus the `?force=` query flag.
#[derive(Debug, Deserialize)]
pub struct CtlStopParams {
    /// The control-plane session id.
    pub id: String,
    /// When true, send `ForceStop` rather than `Stop`. Absent means false, as
    /// an absent `?force=` does.
    #[serde(default)]
    pub force: bool,
}

/// Register the five control-plane methods on `router`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let list_state = Arc::clone(state);
    let run_state = Arc::clone(state);
    let stop_state = Arc::clone(state);
    let auth_state = Arc::clone(state);
    let connect_state = Arc::clone(state);

    router
        .typed("mpm.control.list", move |query: CtlListQuery| {
            let state = Arc::clone(&list_state);
            async move {
                ctl_list_core(&state, CallerTrust::from_peer_checked_socket(), query)
                    .await
                    .into_rpc()
            }
        })
        .typed("mpm.control.run", move |req: CtlRunRequest| {
            let state = Arc::clone(&run_state);
            async move {
                ctl_run_core(&state, CallerTrust::from_peer_checked_socket(), req)
                    .await
                    .into_rpc()
            }
        })
        .typed("mpm.control.stop", move |req: CtlStopParams| {
            let state = Arc::clone(&stop_state);
            async move {
                ctl_stop_core(
                    &state,
                    CallerTrust::from_peer_checked_socket(),
                    &req.id,
                    req.force,
                )
                .await
                .into_rpc()
            }
        })
        .typed("mpm.control.auth", move |req: CtlSessionParams| {
            let state = Arc::clone(&auth_state);
            async move {
                ctl_auth_core(&state, CallerTrust::from_peer_checked_socket(), &req.id)
                    .await
                    .into_rpc()
            }
        })
        .typed_stream("mpm.control.connect", move |req: CtlSessionParams| {
            let state = Arc::clone(&connect_state);
            async move { connect_stream(&state, req.id).await }
        })
}

/// `mpm.control.connect` — the SSE route's socket form (#6288).
///
/// Why a stream and not a unary method: the HTTP route answers with an
/// open-ended `text/event-stream`, one `data:` line per `SessionEvent`. A unary
/// method could only return a snapshot, which is a different route, so this one
/// is registered with `typed_stream` and answers in many frames.
///
/// What: refuses an untrusted caller and an unknown id up front — before any
/// frame is written — then forwards each broadcast event as one stream item.
/// The write-lock CAS the HTTP route performs is NOT done here: the HTTP
/// handler's `WriteLockGuard` is owned by the SSE stream and released when that
/// stream drops, and this slice has no equivalent drop point on the socket. A
/// socket connect is therefore an OBSERVER, and `writer` is reported false.
/// That is a deliberate narrowing, not a parity gap to paper over — an RPC
/// caller that acquired the lock with no drop path would leak it permanently.
///
/// # Errors
///
/// [`CODE_NOT_FOUND`] for an unknown session; the shared 403 code for an
/// untrusted caller.
///
/// Test: `rpc_ctl_connect_streams_events`,
/// `rpc_ctl_connect_unknown_session_refuses_before_any_frame`.
async fn connect_stream(
    state: &Arc<DaemonState>,
    id_str: String,
) -> Result<RpcStreamItems, RpcError> {
    if let Err(refusal) = CallerTrust::from_peer_checked_socket().ensure_local() {
        return Err(refusal.into_rpc().expect_err("a 403 is always an error"));
    }
    let id = ControlSessionId(id_str.clone());
    let Some(handle) = state.session_registry.get(&id).await else {
        return Err(RpcError::new(
            CODE_NOT_FOUND,
            format!("session {id_str} not found"),
        ));
    };

    let rx = handle.event_tx.subscribe();
    let (tx, items) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut stream = BroadcastStream::new(rx);
        while let Some(item) = stream.next().await {
            // A lag or a closed channel ends the stream, exactly as the SSE
            // `filter_map` ends it on `Err(_)`.
            let Ok(event) = item else { break };
            let frame = match serde_json::to_value(&event) {
                Ok(v) => Ok(v),
                Err(e) => Err(RpcError::internal(format!("serialize session event: {e}"))),
            };
            let failed = frame.is_err();
            if tx.send(frame).await.is_err() || failed {
                break;
            }
        }
    });
    Ok(items)
}
