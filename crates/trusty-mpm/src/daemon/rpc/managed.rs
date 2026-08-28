//! Managed sessions, the SESSCTL control plane, and the L2 proxy, served as
//! JSON-RPC methods on the daemon's Unix socket (#6288 slice 4).
//!
//! Why: these three families are the daemon's lifecycle surface — they spawn
//! processes, tear down workspaces, and inject text into live panes — so they
//! are the families whose transport migration carries real risk. ADR-0032 moves
//! them onto the socket; this slice does that while HTTP keeps serving every
//! route unchanged. The bar is one implementation per route: each method calls
//! the SAME `*_core` body the axum handler calls, so the two transports cannot
//! drift.
//!
//! What: [`register`] mounts every method below. Requests are plain JSON
//! objects — a socket request has no URL, so what the HTTP route reads from a
//! path segment or a query string becomes a field. Responses are the HTTP
//! JSON body verbatim. A refusal becomes an [`RpcError`] whose message is the
//! HTTP body verbatim and whose code is
//! [`outcome::status_to_rpc_code`] of the HTTP status.
//!
//! [`RpcError`]: trusty_common::uds::server::RpcError
//!
//! ## Transport-only differences
//!
//! The parity tests compare the two transports' JSON bodies and allow exactly
//! these differences, which are properties of the transport and not of the
//! route:
//!
//!   - **The status number.** HTTP carries it in the status line; RPC carries
//!     its projection in `error.code`, and a 2xx carries nothing at all.
//!   - **`Content-Type`, and every other HTTP header.** The socket has no
//!     headers.
//!   - **The refusal envelope.** HTTP answers a refusal with a bare string
//!     body; RPC answers with `{"error":{"code":…,"message":…}}` carrying that
//!     same string. The MESSAGE is compared verbatim; the envelope is not.
//!   - **201 versus 200.** Both are `Ok` on the socket, which has no
//!     created-versus-ok distinction.
//!
//! Nothing else is allowed to differ, and no route in this slice returns a
//! different BODY on the two transports.
//!
//! ## The #6197 guard, and why it is stronger here
//!
//! `ctl_run_session` and its four siblings carry the input validation and the
//! caller check that PR #6205 (`a2c9d0f2e`) added for #6197. All four checks
//! travel:
//!
//! | #6197 check | Where it runs now | RPC regression test |
//! |---|---|---|
//! | caller must be local | [`control::CallerTrust`] | `rpc_ctl_run_refuses_an_untrusted_caller` |
//! | `claude_cmd` allowlist | `control_routes::validate_claude_cmd` | `rpc_ctl_run_rejects_claude_cmd_outside_allowlist` |
//! | `prompt_file` validation | `control_routes::validate_prompt_file` | `rpc_ctl_run_rejects_prompt_file_injection` |
//! | `workdir` validation | `control_routes::validate_workdir` | `rpc_ctl_run_rejects_relative_workdir`, `rpc_ctl_run_rejects_workdir_traversal` |
//!
//! The three validators are transport-independent and now run inside the shared
//! `ctl_run_core`, so neither transport can reach a spawn without them.
//!
//! The caller check is the one that changes shape. On HTTP it is
//! `loopback_guard`, which asks whether the peer address is loopback — that is,
//! whether the caller is on this HOST. On the socket the equivalent check is
//! `trusty_common::uds::ensure_peer_is_self`, which `serve_until` runs on every
//! accepted connection before a byte is read, and which asks whether the peer's
//! uid is THIS USER'S. Same-uid is a strict subset of same-host: every caller
//! the socket admits would also pass `loopback_guard`, and callers
//! `loopback_guard` admits (any other local user, any process in another
//! account on this machine) the socket refuses. The socket guard is therefore
//! strictly stronger, and nothing weaker is carried across. The socket path
//! additionally sits at `0600` inside a `0700` directory, so a foreign uid
//! cannot reach `connect(2)` in the first place.
//!
//! Test: `managed_tests.rs`.

pub mod control;
pub mod outcome;
pub mod proxy;
pub mod sessions;

#[cfg(test)]
#[path = "managed_tests.rs"]
mod managed_tests;

use std::sync::Arc;

use trusty_common::uds::server::RpcRouter;

use crate::daemon::state::DaemonState;

/// Mount every managed-session, control-plane, and proxy method on `router`.
///
/// Why: `socket.rs::build_router` composes the families with one call each, so
/// a slice adds exactly one line there and owns everything below it.
/// What: delegates to the three family modules in turn.
/// Test: `every_scoped_route_has_a_method`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let router = sessions::register(router, state);
    let router = control::register(router, state);
    proxy::register(router, state)
}
