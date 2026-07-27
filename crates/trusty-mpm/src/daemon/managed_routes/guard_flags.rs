//! `GET /api/v1/sessions/managed/{id}/guard-flags` — read `pm_guard`'s
//! daemon-held kill-switch flags for one managed session (issue #3981 Part 2).
//!
//! Why: a dedicated, minimal read endpoint rather than adding
//! `disable_hooks`/`pm_unrestricted` to the general-purpose [`super::SessionSummary`]
//! (used by every list/get/mutate handler and rendered in `tm sessions ls`
//! tables) — those two booleans are an internal enforcement input, not
//! session metadata operators browse, so they get their own tiny response
//! shape instead of growing an already-large shared DTO.
//! What: [`get_guard_flags`] resolves the record and returns
//! [`GuardFlagsResponse`], including `pane_id` so the CALLER (`pm_guard`'s
//! resolver) can cross-check pane identity exactly the way
//! `pm_guard_deny_by_default::persona_status_for_session` already does for
//! the sibling `#3600` lesson — a sibling pane inheriting the same
//! `TM_MANAGED_SESSION_ID` must never be treated as evidence for THIS pane.
//! Test: `guard_flags_route_returns_persisted_values`,
//! `guard_flags_route_404s_for_unknown_id` in `super::tests`.

use std::sync::Arc;

use axum::{
    Json, extract::Path as AxumPath, extract::State, http::StatusCode, response::IntoResponse,
};
use serde::Serialize;

use crate::daemon::state::DaemonState;

use super::summary::parse_id;

/// Response body for `GET /api/v1/sessions/managed/{id}/guard-flags`.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct GuardFlagsResponse {
    /// Mirrors `SessionRecord::disable_hooks`.
    pub disable_hooks: bool,
    /// Mirrors `SessionRecord::pm_unrestricted`.
    pub pm_unrestricted: bool,
    /// The tmux `pane_id` of this session's original pane, if captured —
    /// see `SessionRecord::pane_id`'s doc for the #2453/#3600 identity
    /// rationale the caller cross-checks against.
    pub pane_id: Option<String>,
}

/// `GET /api/v1/sessions/managed/{id}/guard-flags` handler.
///
/// Why: `pm_guard`'s Guards 2/3 need a fast, minimal round trip — see this
/// module's doc.
/// What: parses the id, resolves the record, and returns
/// [`GuardFlagsResponse`]; an unknown id is a `404` (empty body — the caller
/// treats any non-2xx identically, as "flags unresolved").
pub async fn get_guard_flags(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    match state.session_manager().await.get(&id).await {
        Ok(record) => Json(GuardFlagsResponse {
            disable_hooks: record.disable_hooks,
            pm_unrestricted: record.pm_unrestricted,
            pane_id: record.pane_id,
        })
        .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response(),
    }
}
