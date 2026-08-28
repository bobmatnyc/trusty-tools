//! The legacy session registry, hook relay, and polled event feeds, served as
//! JSON-RPC methods (#6288 slice 3).
//!
//! Why: slice 2 put the first twenty methods on the socket slice 1 bound. This
//! slice adds the `/sessions*` registry family, the `/hooks` relay, and the two
//! `/events/poll` legs. HTTP is untouched and still serves every one of these
//! routes — the socket is an ADDITIONAL way to reach the same body, not a
//! replacement, until the retire slice deletes the axum surface.
//!
//! What: the name-to-route table below, `METHODS` as an assertable array of it,
//! and [`register`], which mounts each name on the router `daemon::socket`
//! builds. Every handler is a thin adapter over a function in
//! [`super::sessions_legacy_ops`] — this file decides WHICH method exists and
//! what it decodes, never what it does.
//!
//! ## Method → route
//!
//! | Method | HTTP route |
//! |---|---|
//! | `mpm.sessions.list` | `GET /sessions` |
//! | `mpm.sessions.register` | `POST /sessions` |
//! | `mpm.sessions.connect` | `POST /api/v1/sessions/connect` |
//! | `mpm.sessions.discover` | `POST /sessions/discover` |
//! | `mpm.sessions.reap` | `DELETE /sessions/dead` |
//! | `mpm.sessions.get` | `GET /sessions/{id}` |
//! | `mpm.sessions.delete` | `DELETE /sessions/{id}` |
//! | `mpm.sessions.pause` | `POST /sessions/{id}/pause` |
//! | `mpm.sessions.resume` | `POST /sessions/{id}/resume` |
//! | `mpm.sessions.command` | `POST /sessions/{id}/command` |
//! | `mpm.sessions.output` | `GET /sessions/{id}/output` |
//! | `mpm.sessions.pane` | `GET /sessions/{id}/pane` |
//! | `mpm.sessions.set_pid` | `PATCH /sessions/{id}/pid` |
//! | `mpm.sessions.events_poll` | `GET /sessions/{id}/events/poll` |
//! | `mpm.events.poll` | `GET /events/poll` |
//! | `mpm.hooks.ingest` | `POST /hooks` |
//!
//! Three naming notes, so a reader does not have to diff the table against
//! `api::router` to find them:
//!
//! - `mpm.sessions.pane` and `mpm.sessions.output` are one body under two
//!   names, mirroring `/sessions/{id}/pane` and `/sessions/{id}/output`, which
//!   axum registers against one handler. The alias is carried rather than
//!   dropped because a client that dials `pane` today must keep working.
//! - `mpm.sessions.set_pid` is named for what it does, not for its path leaf
//!   (`PATCH …/pid`), since a bare `pid` would read as a getter.
//! - `mpm.sessions.reap` is `DELETE /sessions/dead`. The slice-3 brief listed
//!   twelve `/sessions*` verbs and this was not among them; it is registered
//!   here anyway because it is a `/sessions*` legacy-registry route that no
//!   other slice owns, and leaving it out would strand it between slices.
//!
//! `mpm.sessions.list` takes every argument optionally, so it decodes an absent
//! `params` as well as an object. A route whose HTTP form splits its arguments across a path segment, a query
//! string, and a body carries them as one `params` object here —
//! `mpm.sessions.command` takes `{"id": …, "command": …, "compress": …}`.
//! Everything else decodes the same struct the axum extractor decodes, so a
//! caller's payload is unchanged by the transport.
//!
//! ## What guards these methods
//!
//! Every write here costs something: `mpm.sessions.register` can spawn a tmux
//! session running `claude`, `mpm.sessions.delete` kills one, and
//! `mpm.hooks.ingest` mutates the registry and the event log. So the guards have
//! to be at least as strong on the socket, and they are:
//!
//! - **HTTP** layers `trusty_common::server::guard_write_origin` router-wide
//!   (`daemon::mod`, via `api::origin_guard::guard_router`). `/hooks` carries
//!   nothing beyond it — no hook token, no shared secret, no per-route loopback
//!   check (`git grep` over this crate finds no such credential, and the
//!   forwarder shim posts a bare JSON body). That guard is CSRF defence: it
//!   inspects the `Origin` header and ALLOWS any request that carries none,
//!   which is every server-side caller. It authenticates nobody.
//! - **The socket** runs `ensure_peer_is_self` on every accepted connection
//!   before a byte is read (`trusty_common::uds::server`), over a `0600` socket
//!   in a `0700` directory. It authenticates the calling process's uid.
//!
//! Nothing is dropped by moving across. The origin guard has no counterpart to
//! carry — there is no `Origin` header on a Unix socket and no browser that
//! could send one — and the peer-uid check is the stronger of the two, since it
//! refuses a caller the origin guard would have waved through. The overseer veto
//! on `mpm.hooks.ingest` is not a transport guard at all: it lives in the shared
//! body and fires identically either way.
//!
//! Test: `sessions_legacy_tests.rs` — `rpc_*` for the wire behaviour, `parity_*`
//! for the HTTP-versus-socket comparison.
//!
//! [`register`]: crate::daemon::rpc::sessions_legacy::register
//! [`super::sessions_legacy_ops`]: crate::daemon::rpc::sessions_legacy_ops

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::core::compress::CompressionLevel;
use crate::daemon::state::DaemonState;

use super::core::NoParams;
use super::sessions_legacy_ops as ops;

#[cfg(test)]
#[path = "sessions_legacy_tests.rs"]
mod tests;

/// Every method this slice registers, in registration order.
///
/// Why: the slice-7 client swap will dial these names by literal, from code with
/// no compile-time link to this table. An array here is what a contract test can
/// compare the router against, so a rename surfaces as a failing assertion
/// rather than as a consumer that silently reports `method_not_found`.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub const METHODS: &[&str] = &[
    "mpm.sessions.list",
    "mpm.sessions.register",
    "mpm.sessions.connect",
    "mpm.sessions.discover",
    "mpm.sessions.reap",
    "mpm.sessions.get",
    "mpm.sessions.delete",
    "mpm.sessions.pause",
    "mpm.sessions.resume",
    "mpm.sessions.command",
    "mpm.sessions.output",
    "mpm.sessions.pane",
    "mpm.sessions.set_pid",
    "mpm.sessions.events_poll",
    "mpm.events.poll",
    "mpm.hooks.ingest",
];

/// The one path segment a session route takes: `GET`/`DELETE /sessions/{id}`,
/// `POST …/resume`, `GET …/events/poll`.
///
/// `id` accepts a UUID everywhere, and additionally a friendly tmux name on the
/// routes whose HTTP form does — the resolution rule is the body's, not this
/// struct's.
#[derive(Deserialize)]
pub struct SessionIdParams {
    /// Session UUID, or a friendly tmux name where the route resolves one.
    pub id: String,
}

/// `POST /sessions/{id}/pause`'s path segment plus its optional body note.
#[derive(Deserialize)]
pub struct PauseParams {
    /// Session UUID or friendly name.
    pub id: String,
    /// Optional note about where the session was left off.
    #[serde(default)]
    pub summary: Option<String>,
}

/// `PATCH /sessions/{id}/pid`'s path segment plus its body.
#[derive(Deserialize)]
pub struct SetPidParams {
    /// Session UUID.
    pub id: String,
    /// OS-level `claude` process id discovered inside the session's tmux pane.
    pub pid: u32,
}

/// `POST /sessions/{id}/command`'s path segment, query, and body as one object.
#[derive(Deserialize)]
pub struct CommandParams {
    /// Session UUID or friendly name.
    pub id: String,
    /// The command line to send to the session's tmux pane.
    pub command: String,
    /// Compression level to apply to the captured output before returning.
    #[serde(default)]
    pub compress: Option<CompressionLevel>,
}

/// `GET /sessions/{id}/output` (and its `/pane` alias) as one object.
#[derive(Deserialize)]
pub struct OutputParams {
    /// Session UUID or friendly name.
    pub id: String,
    /// Trailing pane lines to capture (default 50 when absent).
    #[serde(default)]
    pub lines: Option<u32>,
    /// Compression level to apply to the captured output before returning.
    #[serde(default)]
    pub compress: Option<CompressionLevel>,
}

/// Mount every method in [`METHODS`] onto `router`.
///
/// Why a free function rather than a builder method: `daemon::socket` names one
/// `register` call per family, so a family arrives without the accept loop, the
/// framing, or the peer check being touched.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    macro_rules! ok {
        ($router:expr, $name:literal, $req:ty, $resp:ty, |$s:ident, $r:ident| $call:expr) => {{
            let held = Arc::clone(state);
            $router.typed::<$req, $resp, _, _>($name, move |$r| {
                let $s = Arc::clone(&held);
                async move { Ok($call) }
            })
        }};
    }
    macro_rules! fallible {
        ($router:expr, $name:literal, $req:ty, $resp:ty, |$s:ident, $r:ident| $call:expr) => {{
            let held = Arc::clone(state);
            $router.typed::<$req, $resp, _, _>($name, move |$r| {
                let $s = Arc::clone(&held);
                async move { $call.map_err(Into::into) }
            })
        }};
    }

    use crate::daemon::api::types as t;
    use crate::daemon::api::{HookPost, RegisterSession, SessionQuery};

    let r = router;
    // `Option<SessionQuery>` rather than `SessionQuery`: every argument this
    // method takes is optional, so a caller may send no `params` at all, and
    // serde's derive refuses `null` for a struct — the same trap `NoParams`
    // exists for on the no-argument methods (#6288).
    let r = ok!(
        r,
        "mpm.sessions.list",
        Option<SessionQuery>,
        t::SessionsResponse,
        |s, p| ops::list_sessions(&s, p.unwrap_or_default())
    );
    let r = fallible!(
        r,
        "mpm.sessions.register",
        RegisterSession,
        t::RegisterSessionResponse,
        |s, p| ops::register_session(&s, p)
    );
    // `connect` is a distinct name over the same body, exactly as
    // `POST /api/v1/sessions/connect` is a distinct route over the same handler
    // — the daemon does no deployment in either path (#6288).
    let r = fallible!(
        r,
        "mpm.sessions.connect",
        RegisterSession,
        t::RegisterSessionResponse,
        |s, p| ops::register_session(&s, p)
    );
    let r = ok!(
        r,
        "mpm.sessions.discover",
        NoParams,
        t::DiscoverResponse,
        |s, _p| ops::discover_sessions(&s).await
    );
    let r = ok!(
        r,
        "mpm.sessions.reap",
        NoParams,
        t::ReapResponse,
        |s, _p| ops::reap_sessions(&s)
    );
    let r = fallible!(
        r,
        "mpm.sessions.get",
        SessionIdParams,
        crate::core::session::Session,
        |s, p| ops::get_session(&s, &p.id)
    );
    let r = fallible!(
        r,
        "mpm.sessions.delete",
        SessionIdParams,
        t::RemoveSessionResponse,
        |s, p| ops::remove_session(&s, &p.id).await
    );
    let r = fallible!(
        r,
        "mpm.sessions.pause",
        PauseParams,
        t::PauseResponse,
        |s, p| ops::pause_session(&s, &p.id, p.summary)
    );
    let r = fallible!(
        r,
        "mpm.sessions.resume",
        SessionIdParams,
        t::ResumeResponse,
        |s, p| ops::resume_session(&s, &p.id)
    );
    let r = fallible!(
        r,
        "mpm.sessions.command",
        CommandParams,
        t::CommandResponse,
        |s, p| ops::send_command(&s, &p.id, &p.command, p.compress).await
    );
    let r = fallible!(
        r,
        "mpm.sessions.output",
        OutputParams,
        t::OutputResponse,
        |s, p| ops::get_output(&s, &p.id, p.lines, p.compress)
    );
    let r = fallible!(
        r,
        "mpm.sessions.pane",
        OutputParams,
        t::OutputResponse,
        |s, p| ops::get_output(&s, &p.id, p.lines, p.compress)
    );
    let r = fallible!(
        r,
        "mpm.sessions.set_pid",
        SetPidParams,
        t::SetPidResponse,
        |s, p| ops::set_session_pid(&s, &p.id, p.pid)
    );
    let r = fallible!(
        r,
        "mpm.sessions.events_poll",
        SessionIdParams,
        t::EventsResponse,
        |s, p| ops::session_events(&s, &p.id)
    );
    let r = ok!(
        r,
        "mpm.events.poll",
        NoParams,
        t::EventsResponse,
        |s, _p| ops::recent_events(&s)
    );
    fallible!(
        r,
        "mpm.hooks.ingest",
        HookPost,
        t::HookAcceptedResponse,
        |s, p| ops::ingest_hook(&s, p).await
    )
}
