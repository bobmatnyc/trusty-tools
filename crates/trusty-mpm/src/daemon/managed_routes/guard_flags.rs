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
    Json, Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use serde::{Deserialize, Serialize};

use crate::daemon::state::DaemonState;
use crate::session_manager::SessionRecord;

use super::summary::parse_id;

/// This route's own tiny sub-router. Merged in at `daemon::serve_http`'s
/// top-level composition point (NOT inside `api::router` itself, unlike the
/// `sync_assets`/`provision_status` sub-routers) — `api.rs` is already at its
/// frozen SLOC budget (`.line-cap-allowlist.tsv`) with zero headroom for even
/// one more `.merge(...)` chain link, while `daemon::mod` has room to spare.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new().route(
        "/api/v1/sessions/managed/{id}/guard-flags",
        get(get_guard_flags),
    )
}

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

/// Persist operator-captured guard flags after a spawn, updating `record` to
/// match (so the immediate spawn response already reflects them).
///
/// Why: extracted out of `lifecycle::spawn_managed` — that file is already
/// well over its grandfathered SLOC budget, and this write is a single
/// self-contained, non-fatal side effect (mirrors the Deliverable-linkage
/// persist immediately after it in that function). Skips the round trip
/// entirely when both flags are `false` (the common case — the record
/// already defaults to `false`/`false` at creation, see
/// `session_manager::create`), so an ordinary spawn pays no extra store
/// write.
/// What: no-op when `disable_hooks`/`pm_unrestricted` are both `false`;
/// otherwise calls `SessionManager::set_guard_flags`, updating `record` on
/// success and logging (never failing the spawn) on error.
/// Test: `guard_flags_persist_on_session` in `session_manager::tests`
/// (via `SessionManager::set_guard_flags` directly) covers the store
/// write; this wrapper's skip-when-false and record-sync behavior is
/// exercised transitively by the HTTP spawn tests in `tests/session_manager_mvp.rs`.
pub(super) async fn persist_after_spawn(
    state: &Arc<DaemonState>,
    record: &mut SessionRecord,
    disable_hooks: bool,
    pm_unrestricted: bool,
) {
    if !disable_hooks && !pm_unrestricted {
        return;
    }
    match state
        .session_manager()
        .await
        .set_guard_flags(&record.id, disable_hooks, pm_unrestricted)
        .await
    {
        Ok(()) => {
            record.disable_hooks = disable_hooks;
            record.pm_unrestricted = pm_unrestricted;
        }
        Err(e) => tracing::warn!(
            id = %record.id,
            "set_guard_flags failed after spawn: {e}; guard stays fully active"
        ),
    }
}

/// Query params for POST /api/v1/sessions/managed/{id}/resume (issue #3981 Part 2).
///
/// Why: `resume` re-spawns the session's PM process, so it is the OTHER
/// moment (besides spawn) an operator's launching-shell env can legitimately
/// re-arm or re-confirm `pm_guard`'s kill-switch flags — "resume with the
/// flag set" is the sanctioned way to disable the guard, per Bob's decision
/// against a mid-session flip. A query string (not a JSON body) keeps every
/// existing bodyless `POST .../resume` caller (the GUI, the picker,
/// programmatic callers) working unchanged — the extractor defaults absent
/// params to `false`/`false`. That default is DELIBERATELY re-asserted (not
/// skipped) on every resume: a bypass must be re-declared on the SPECIFIC
/// `tm sessions resume` call that wants it, never left silently "sticky"
/// from an earlier resume once an unrelated bodyless resume (GUI, picker)
/// comes along — that would be the exact sticky-bypass shape this issue
/// exists to close, just moved from settings.json to the session record.
/// What: `#[serde(default)]` bools, mirroring `super::DeleteQuery`'s pattern.
/// Moved here (out of `mod.rs`, review follow-up) alongside
/// [`persist_after_resume`], the only handler that consumes it.
/// Test: `resume_route_persists_guard_flags` in `super::tests`.
#[derive(Debug, Deserialize, Default)]
pub struct ResumeQuery {
    #[serde(default)]
    pub disable_hooks: bool,
    #[serde(default)]
    pub pm_unrestricted: bool,
}

/// Re-assert operator-captured guard flags after a resume, updating `record`
/// to match.
///
/// Why: extracted out of `mod::resume_managed_session` for the same reason
/// as [`persist_after_spawn`]. Unlike that function, this one is called
/// UNCONDITIONALLY (never skipped when both flags are `false`) — see
/// [`ResumeQuery`]'s doc for why a bodyless resume must deliberately
/// re-arm the guard rather than leaving a prior bypass silently sticky.
/// What: calls `SessionManager::set_guard_flags`, updating `record` on
/// success and logging (never failing the resume) on error.
pub(super) async fn persist_after_resume(
    state: &Arc<DaemonState>,
    record: &mut SessionRecord,
    disable_hooks: bool,
    pm_unrestricted: bool,
) {
    match state
        .session_manager()
        .await
        .set_guard_flags(&record.id, disable_hooks, pm_unrestricted)
        .await
    {
        Ok(()) => {
            record.disable_hooks = disable_hooks;
            record.pm_unrestricted = pm_unrestricted;
        }
        Err(e) => tracing::warn!(
            id = %record.id,
            "set_guard_flags failed after resume: {e}; guard flags unchanged"
        ),
    }
}
