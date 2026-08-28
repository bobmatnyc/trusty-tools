//! PATCH /api/v1/sessions/managed/{id} — rename a managed session.
//!
//! Why: `tm sessions rename` needs a daemon endpoint because the daemon owns
//! the [`crate::session_manager::SessionManager`] (the store + the live tmux
//! sessions); a rename mutates both, so it cannot be done from the thin CLI
//! client. Split into its own file (like `reactivate.rs`/`prune.rs`) so the
//! handler + request type do not grow `mod.rs`, which sits near its 500-SLOC
//! production cap.
//! What: [`RenameRequest`] (the `{ "name": … }` body) and
//! [`rename_managed_session`], which delegates to
//! [`crate::session_manager::SessionManager::rename`] and maps its typed errors
//! to HTTP status codes.
//! Test: `rename_route_*` in `tests/session_lifecycle.rs`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    response::IntoResponse,
};
use serde::Deserialize;
use tracing::warn;

use super::{parse_id, record_to_summary};
use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedError;

/// Request body for PATCH /api/v1/sessions/managed/{id}.
///
/// Why: rename takes exactly one caller input — the new name; a small typed
/// body keeps it explicit and validated (the manager rejects a bad name).
/// What: a single `name` field.
/// Test: `rename_route_renames` in `tests/session_lifecycle.rs`.
#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    /// The new session name.
    pub name: String,
}

/// PATCH /api/v1/sessions/managed/{id} — rename a managed session.
///
/// Why: keeps the tmux entity's name in sync with the record's `tmux_name` so
/// operators can give a session a meaningful name and have `tmux attach`/`ls`
/// reflect it.
/// What: parses the id, delegates to
/// [`crate::session_manager::SessionManager::rename`], and maps its result —
/// `Ok` → 200 with the updated session summary (`name` may differ from the
/// request body if it collided and was auto-suffixed, issue #3692 —
/// `rename` no longer rejects a collision); `SessionNotFound` → 404;
/// `NameCollision` → 409 with the actionable message (retained for any other
/// caller of this shared error type; `rename` itself no longer produces it);
/// `InvalidState` (an invalid name, or a terminal record) → 400; any other
/// error → 500.
/// Test: `rename_route_renames`, `rename_route_suffixes_collision`,
/// `rename_route_rejects_invalid_name` in `tests/session_lifecycle.rs`.
pub async fn rename_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    Json(body): Json<RenameRequest>,
) -> impl IntoResponse {
    rename_core(&state, &id_str, body).await // #6288
}

/// The transport-neutral body of `PATCH .../managed/{id}` (#6288), served over
/// the socket as `mpm.managed.rename`.
///
/// Test: `managed_rename_parity` in `daemon::rpc::managed_tests`.
pub(crate) async fn rename_core(
    state: &Arc<DaemonState>,
    id_str: &str,
    body: RenameRequest,
) -> RouteOutcome {
    let id = match parse_id(id_str) {
        Ok(id) => id,
        Err((code, msg)) => return RouteOutcome::text(code.as_u16(), msg),
    };
    let mgr = state.session_manager().await;
    match mgr.rename(&id, &body.name).await {
        Ok(record) => RouteOutcome::ok(&record_to_summary(&record)),
        Err(ManagedError::SessionNotFound(_)) => {
            RouteOutcome::text(404, format!("session {id_str} not found"))
        }
        Err(e @ ManagedError::NameCollision(_)) => RouteOutcome::text(409, e.to_string()),
        Err(ManagedError::InvalidState(_, reason)) => RouteOutcome::text(400, reason),
        Err(e) => {
            // #5001: the body already carried this message, but nothing logged
            // it — a 500 here left NO daemon-side trace, so diagnosing one meant
            // correlating unrelated log spam. The unmapped remainder is by
            // definition the case nobody anticipated; log it where it happens.
            warn!(id = %id_str, error = %e, "rename failed with an unmapped error");
            RouteOutcome::text(500, e.to_string())
        }
    }
}
