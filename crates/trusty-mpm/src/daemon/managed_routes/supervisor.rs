//! POST /api/v1/sessions/managed/supervisor — register the Architect (#8942).
//!
//! Why: `tm fleet init` starts the Architect outside the daemon; this route
//! is how its sessions get protected, listed records.
//! What: [`supervisor_routes`] mounts the route; [`register_supervisor_core`]
//! is the transport-neutral body, also served on the socket as
//! `mpm.managed.register_supervisor`. [`LaunchRecordBinding`] is the
//! production [`BindingVerifier`]: the `claude` in the session must be the one
//! `tm fleet init` recorded for the directory, under the daemon's framework
//! root. Status: 200 with the [`RegistrationReport`]; 400 for a malformed
//! request; 403 when the session is not the bound Architect; 500 when the
//! store cannot be written.
//! Test: `the_route_refuses_an_unbound_session_with_403`,
//! `the_route_rejects_a_relative_dir_with_400`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use tracing::warn;

use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;
use crate::session_manager::{BindingVerifier, RegisterError, SupervisorRegistration};

/// The Architect registration route, as a sub-router merged by `api.rs`.
pub fn supervisor_routes() -> Router<Arc<DaemonState>> {
    Router::new().route(
        "/api/v1/sessions/managed/supervisor",
        post(register_supervisor_route),
    )
}

/// `POST /api/v1/sessions/managed/supervisor`; see the module doc.
pub async fn register_supervisor_route(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<SupervisorRegistration>,
) -> impl IntoResponse {
    let verifier = LaunchRecordBinding {
        root: state.framework_root().to_path_buf(),
    };
    register_supervisor_core(&state, &body, &verifier).await
}

/// The transport-neutral body of the registration route (#8942).
///
/// What: runs the manager's registration and maps [`RegisterError`] onto
/// 400/403/500.
/// Test: `the_route_refuses_an_unbound_session_with_403`.
pub(crate) async fn register_supervisor_core(
    state: &Arc<DaemonState>,
    body: &SupervisorRegistration,
    verifier: &dyn BindingVerifier,
) -> RouteOutcome {
    let mgr = state.session_manager().await;
    match mgr.register_supervisor(body, verifier).await {
        Ok(report) => RouteOutcome::ok(&report),
        Err(e @ RegisterError::Invalid(_)) => RouteOutcome::text(400, e.to_string()),
        Err(e @ RegisterError::Unbound(_)) => {
            warn!("#8942: refused to register the Architect: {e}");
            RouteOutcome::text(403, e.to_string())
        }
        Err(e @ RegisterError::Managed(_)) => RouteOutcome::text(500, e.to_string()),
    }
}

/// The production binding check: the launch record under `root` (#8942).
pub struct LaunchRecordBinding {
    /// The daemon's framework root, `~/.trusty-mpm` in production.
    pub root: PathBuf,
}

impl BindingVerifier for LaunchRecordBinding {
    /// One probe for the `claude` in `session`, then `check_session_binding`.
    fn verify(&self, dir: &Path, session: &str) -> Result<(), String> {
        let pid =
            crate::core::process::find_claude_pid_in_tmux(session, 1, std::time::Duration::ZERO)
                .ok_or_else(|| format!("no `claude` process runs in tmux session {session}"))?;
        crate::core::architect_session::check_session_binding(&self.root, dir, session, pid)
            .map_err(|why| format!("the claude (pid {pid}) in {session} is not bound: {why}"))
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod supervisor_tests;
