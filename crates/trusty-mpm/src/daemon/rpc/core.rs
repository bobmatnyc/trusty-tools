//! The core request/response routes, served as JSON-RPC methods (#6288 slice 2).
//!
//! Why: slice 1 bound a hardened Unix socket beside the daemon's HTTP listener
//! and registered nothing on it. This slice puts the first twenty methods on it.
//! HTTP is untouched and still serves every one of these routes — the socket is
//! an ADDITIONAL way to reach the same body, not a replacement, until the retire
//! slice deletes the axum surface.
//!
//! What: the name-to-route table below, `METHODS` as an assertable array of it,
//! and [`register`], which mounts each name on the router `daemon::socket`
//! builds. Every handler is a thin adapter over a function in
//! [`super::core_ops`] or `daemon::api::claude_config_routes` — this file
//! decides WHICH method exists and what it decodes, never what it does.
//!
//! ## Method → route
//!
//! | Method | HTTP route |
//! |---|---|
//! | `mpm.health` | `GET /health` |
//! | `mpm.doctor` | `GET /api/v1/doctor` |
//! | `mpm.errors.list` | `GET /api/v1/errors` |
//! | `mpm.report_bug` | `POST /api/v1/report-bug` |
//! | `mpm.breakers` | `GET /breakers` |
//! | `mpm.optimizer` | `GET /optimizer` |
//! | `mpm.overseer` | `GET /overseer` |
//! | `mpm.llm.chat` | `POST /llm/chat` |
//! | `mpm.tmux.sessions` | `GET /tmux/sessions` |
//! | `mpm.tmux.snapshot` | `GET /tmux/sessions/{name}/snapshot` |
//! | `mpm.tmux.adopt` | `POST /tmux/adopt` |
//! | `mpm.claude_config.get` | `GET /claude-config` |
//! | `mpm.claude_config.apply` | `POST /claude-config/apply` |
//! | `mpm.claude_config.checkpoints.list` | `GET /claude-config/checkpoints` |
//! | `mpm.claude_config.checkpoints.create` | `POST /claude-config/checkpoints` |
//! | `mpm.claude_config.checkpoints.delete` | `DELETE /claude-config/checkpoints/{id}` |
//! | `mpm.claude_config.restore` | `POST /claude-config/restore` |
//! | `mpm.claude_config.profiles` | `GET /claude-config/profiles` |
//! | `mpm.claude_config.deploy` | `POST /claude-config/deploy` |
//! | `mpm.claude_config.restart` | `POST /claude-config/restart` |
//!
//! A route whose HTTP form splits its arguments across a path segment and a
//! query string carries them as one `params` object here — `mpm.tmux.snapshot`
//! takes `{"name": …}` and `mpm.claude_config.checkpoints.delete` takes
//! `{"project": …, "id": …}`. Everything else decodes the same struct the axum
//! extractor decodes, so a caller's payload is unchanged by the transport.
//!
//! ## What guards these methods
//!
//! Two of them cost something. `mpm.report_bug` files a GitHub issue when the
//! caller sets `confirm: true`, and `mpm.llm.chat` spends OpenRouter credit. So
//! the guards have to be at least as strong here as on HTTP, and they are:
//!
//! - **HTTP** layers `trusty_common::server::guard_write_origin` router-wide
//!   (`daemon::mod`, via `api::origin_guard::guard_router`). It is a CSRF
//!   defence: it inspects the `Origin` header and, per
//!   `origin_guard::origin_allowed`, ALLOWS any request that carries none —
//!   which is every server-side caller. It authenticates nobody.
//! - **The socket** runs `ensure_peer_is_self` on every accepted connection
//!   before a byte is read (`trusty_common::uds::server`), over a `0600` socket
//!   in a `0700` directory. It authenticates the calling process's uid.
//!
//! Nothing is dropped by moving across. The origin guard has no counterpart to
//! carry — there is no `Origin` header on a Unix socket and no browser that
//! could send one — and the peer-uid check is the stronger of the two, since it
//! refuses a caller the origin guard would have waved through. The `/rpc`
//! loopback gate (`api::rpc::is_loopback`) guards a route this slice does not
//! own and is untouched.
//!
//! Test: `core_tests.rs` — `rpc_*` for the wire behaviour, `parity_*` for the
//! HTTP-versus-socket comparison.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use trusty_common::uds::server::RpcRouter;

use crate::daemon::api::claude_config_routes as cc;
use crate::daemon::state::DaemonState;

use super::core_ops;

#[cfg(test)]
#[path = "core_tests.rs"]
mod tests;

/// Every method this slice registers, in registration order.
///
/// Why: `tm doctor`'s daemon probe and the slice-7 client swap both dial these
/// names by literal, from code with no compile-time link to this table. An
/// array here is what a contract test can compare the router against, so a
/// rename surfaces as a failing assertion rather than as a consumer that
/// silently reports `method_not_found`.
/// Test: `rpc_router_registers_every_documented_method`.
pub const METHODS: &[&str] = &[
    "mpm.health",
    "mpm.doctor",
    "mpm.errors.list",
    "mpm.report_bug",
    "mpm.breakers",
    "mpm.optimizer",
    "mpm.overseer",
    "mpm.llm.chat",
    "mpm.tmux.sessions",
    "mpm.tmux.snapshot",
    "mpm.tmux.adopt",
    "mpm.claude_config.get",
    "mpm.claude_config.apply",
    "mpm.claude_config.checkpoints.list",
    "mpm.claude_config.checkpoints.create",
    "mpm.claude_config.checkpoints.delete",
    "mpm.claude_config.restore",
    "mpm.claude_config.profiles",
    "mpm.claude_config.deploy",
    "mpm.claude_config.restart",
];

/// The params of a method that takes no arguments.
///
/// Why: `RpcRouter::typed` decodes `params` into the handler's request type
/// before the handler runs, and `params` is absent — `serde_json::Value::Null` —
/// on a well-formed call to a no-argument method. A plain unit struct refuses
/// `null`, so every `mpm.health` probe would answer `invalid_params`.
/// What: accepts anything and keeps nothing. A caller that sends a stray field
/// is not refused: these methods have no arguments to get wrong, and refusing
/// would turn an additive client change into an outage.
/// Test: `rpc_health_answers_with_no_params`,
/// `rpc_health_answers_with_a_stray_params_object`.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct NoParams;

impl<'de> Deserialize<'de> for NoParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(NoParams)
    }
}

/// The one argument `GET /tmux/sessions/{name}/snapshot` carries in its path.
///
/// Why a struct: JSON-RPC carries one `params` value, so a bare string would
/// give this method a shape none of its siblings has. Test:
/// `rpc_tmux_snapshot_unknown_session_reports_a_coded_error`.
#[derive(Debug, Deserialize)]
pub struct TmuxSnapshotParams {
    /// tmux session name to capture.
    pub name: String,
}

/// Mount every method in [`METHODS`] onto `router`.
///
/// Why: the only trusty-mpm-specific half of the socket server. Hardening,
/// framing, the envelope and the accept loop are all
/// `trusty_common::uds::server`'s.
/// What: one `typed` registration per method, each cloning the `Arc` handle to
/// the shared [`DaemonState`] — the daemon's ONE state, the same value the axum
/// router was built with, so the two transports read and write the same
/// sessions, breakers, and manager rather than two copies of them.
/// Every fallible body returns `DaemonError`, which crosses the wire through
/// `From<DaemonError> for RpcError` (`daemon::error`) — the same seam that
/// picks the HTTP status, so a failure cannot be one thing over HTTP and
/// another over the socket.
/// Test: `rpc_router_registers_every_documented_method`, and the `parity_*`
/// cases drive each registration against its HTTP twin.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    /// One method whose body cannot fail.
    macro_rules! ok {
        ($router:expr, $name:literal, $req:ty, $resp:ty, |$s:ident, $r:ident| $call:expr) => {{
            let held = Arc::clone(state);
            $router.typed::<$req, $resp, _, _>($name, move |$r| {
                let $s = Arc::clone(&held);
                async move { Ok($call) }
            })
        }};
    }

    /// One method whose body returns `Result<_, DaemonError>`.
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
    use crate::daemon::api::{AdoptRequest, DoctorQuery, ErrorsQuery, ReportBugApiRequest};

    let r = router;

    // ---- health / doctor / errors / report-bug ------------------------------
    let r = ok!(r, "mpm.health", NoParams, t::HealthResponse, |s, _p| {
        core_ops::health(&s).await
    });
    let r = ok!(
        r,
        "mpm.doctor",
        DoctorQuery,
        crate::core::doctor::DoctorReport,
        |s, p| core_ops::doctor(&s, p).await
    );
    let r = ok!(
        r,
        "mpm.errors.list",
        ErrorsQuery,
        t::ErrorsResponse,
        |s, p| core_ops::list_errors(&s, p)
    );
    let r = ok!(
        r,
        "mpm.report_bug",
        ReportBugApiRequest,
        t::ReportBugHttpResponse,
        |s, p| core_ops::report_bug(&s, p).await
    );

    // ---- breakers / optimizer / overseer / llm-chat -------------------------
    let r = ok!(r, "mpm.breakers", NoParams, t::BreakersResponse, |s, _p| {
        core_ops::breakers(&s)
    });
    let r = ok!(
        r,
        "mpm.optimizer",
        NoParams,
        t::OptimizerResponse,
        |s, _p| core_ops::optimizer(&s)
    );
    let r = ok!(r, "mpm.overseer", NoParams, t::OverseerResponse, |s, _p| {
        core_ops::overseer(&s)
    });
    let r = fallible!(
        r,
        "mpm.llm.chat",
        t::LlmChatRequest,
        t::LlmChatResponse,
        |s, p| core_ops::llm_chat(&s, p).await
    );

    // ---- tmux ---------------------------------------------------------------
    let r = ok!(
        r,
        "mpm.tmux.sessions",
        NoParams,
        t::TmuxSessionsResponse,
        |s, _p| core_ops::list_tmux_sessions(&s)
    );
    let r = fallible!(
        r,
        "mpm.tmux.snapshot",
        TmuxSnapshotParams,
        t::TmuxSnapshotResponse,
        |s, p| core_ops::tmux_snapshot(&s, &p.name)
    );
    let r = fallible!(
        r,
        "mpm.tmux.adopt",
        AdoptRequest,
        t::AdoptResponse,
        |s, p| core_ops::adopt_tmux(&s, p)
    );

    // ---- claude-config ------------------------------------------------------
    let r = ok!(
        r,
        "mpm.claude_config.get",
        cc::ClaudeConfigQuery,
        t::ClaudeConfigResponse,
        |s, p| cc::get_claude_config_op(&s, p)
    );
    let r = fallible!(
        r,
        "mpm.claude_config.apply",
        cc::ApplyConfigRequest,
        t::ApplyConfigResponse,
        |s, p| cc::apply_claude_config_op(&s, p)
    );
    let r = ok!(
        r,
        "mpm.claude_config.checkpoints.list",
        cc::CheckpointQuery,
        t::CheckpointsResponse,
        |s, p| cc::list_checkpoints_op(&s, p)
    );
    let r = fallible!(
        r,
        "mpm.claude_config.checkpoints.create",
        cc::CreateCheckpointRequest,
        t::CreateCheckpointResponse,
        |s, p| cc::create_checkpoint_op(&s, p)
    );
    let r = fallible!(
        r,
        "mpm.claude_config.checkpoints.delete",
        cc::DeleteCheckpointParams,
        t::DeleteCheckpointResponse,
        |s, p| cc::delete_checkpoint_op(&s, p)
    );
    let r = fallible!(
        r,
        "mpm.claude_config.restore",
        cc::RestoreRequest,
        t::RestoreResponse,
        |s, p| cc::restore_checkpoint_op(&s, p)
    );
    let r = ok!(
        r,
        "mpm.claude_config.profiles",
        NoParams,
        t::ProfilesResponse,
        |s, _p| cc::list_profiles_op(&s)
    );
    let r = fallible!(
        r,
        "mpm.claude_config.deploy",
        cc::DeployProfileRequest,
        t::DeployProfileResponse,
        |s, p| cc::deploy_profile_op(&s, p)
    );
    fallible!(
        r,
        "mpm.claude_config.restart",
        cc::RestartRequest,
        t::RestartResponse,
        |s, p| cc::restart_claude_code_op(&s, p).await
    )
}
