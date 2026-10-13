//! Pane-output route for managed sessions (#9162).
//!
//! Why: `GET /sessions/{id}/output` resolves only the project-session
//! registry, so `tm sessions output <managed name>` read "not found" for every
//! managed session. Managed sessions live in the session manager, whose
//! `capture_pane` reads only the record's own proven pane.
//! What: [`router`] registers `GET /api/v1/sessions/managed/{id}/output`
//! (`?lines=N&compress=<level>`); [`output_core`] is the transport-neutral body,
//! served over the socket as `mpm.managed.output`. The answer is an
//! [`OutputResponse`] with `input_box` always `null` (no classification from a
//! pane capture).
//! Test: `managed_output_*_9162` in `daemon::rpc::managed_tests`.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Path as AxumPath, Query, State},
    response::IntoResponse,
    routing::get,
};
use serde::Deserialize;

use super::parse_id;
use crate::core::compress::CompressionLevel;
use crate::daemon::api::types::OutputResponse;
use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::rpc::sessions_legacy_ops::{apply_compression, default_output_lines};
use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedError;

/// Build the output sub-router, merged into the daemon router by `api.rs`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new().route(
        "/api/v1/sessions/managed/{id}/output",
        get(get_managed_output),
    )
}

/// The route's query: the same two knobs as the project output route.
#[derive(Debug, Default, Deserialize)]
pub struct ManagedOutputQuery {
    /// Number of trailing pane lines to capture (default 50 when absent).
    #[serde(default)]
    pub lines: Option<u32>,
    /// Compression level applied before returning; raw output when absent.
    #[serde(default)]
    pub compress: Option<CompressionLevel>,
}

/// `GET /api/v1/sessions/managed/{id}/output`.
///
/// Test: `managed_output_returns_pane_text_for_a_managed_session_9162`.
async fn get_managed_output(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ManagedOutputQuery>,
) -> impl IntoResponse {
    output_core(&state, &id, query.lines, query.compress).await
}

/// The transport-neutral body of `GET .../{id}/output`, served over the socket
/// as `mpm.managed.output`.
///
/// Why: both transports must answer the same body (#6288).
/// What: captures the last `lines` of the record's OWN pane through
/// `SessionManager::capture_pane`, which proves the pane with `owned_pane`
/// first. A refusal is never turned into empty output: an unknown id is 404;
/// a record with no pane id, a gone pane or an unprovable pane is 409; a tmux
/// failure is 500.
/// Test: `managed_output_returns_pane_text_for_a_managed_session_9162`,
/// `managed_output_with_no_owned_pane_fails_closed_9162`,
/// `managed_output_parity_9162`.
pub(crate) async fn output_core(
    state: &Arc<DaemonState>,
    id_str: &str,
    lines: Option<u32>,
    compress: Option<CompressionLevel>,
) -> RouteOutcome {
    let id = match parse_id(id_str) {
        Ok(id) => id,
        Err((code, msg)) => return RouteOutcome::text(code.as_u16(), msg),
    };
    let lines = lines.unwrap_or_else(default_output_lines);
    let mgr = state.session_manager().await;
    // #9162: `capture_pane` is the owned-pane guard; no session-scoped capture
    // is attempted on any failure.
    let raw = match mgr.capture_pane(&id, lines as usize).await {
        Ok(raw) => raw,
        Err(ManagedError::SessionNotFound(_)) => {
            return RouteOutcome::text(404, format!("session {id_str} not found"));
        }
        Err(e @ (ManagedError::InvalidState(..) | ManagedError::PaneGone(..))) => {
            return RouteOutcome::text(409, format!("session {id_str} has no readable pane: {e}"));
        }
        Err(e) => {
            return RouteOutcome::text(500, format!("session {id_str} pane capture failed: {e}"));
        }
    };
    let compressed = apply_compression(compress, &raw);
    RouteOutcome::ok(&OutputResponse {
        output: compressed.text,
        lines,
        original_bytes: compressed.stats.original_bytes,
        compressed_bytes: compressed.stats.compressed_bytes,
        compress_level: compressed.level_label,
        input_box: None,
    })
}
