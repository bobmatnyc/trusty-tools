//! The embedding pause/resume write cores (#6524).
//!
//! Why: the owner ruling scoped the pause to embedding — "let's just pause
//! embedding, since that's the heavy process" — so an operator needs two verbs
//! that flip one flag and nothing else. These are the transport-free bodies
//! `search.index.pause_embedding` and `search.index.resume_embedding` run.
//!
//! What: a registry lookup, an atomic flip, and the flag's new value. There is
//! no HTTP twin: #6285 is retiring this daemon's HTTP listener, so a surface
//! added now is added on the socket only.
//!
//! The state is in-memory and does not survive a daemon restart — see
//! [`crate::core::embed_pause`]. A caller that needs a pause to outlive one
//! wants `PATCH /indexes/{id}/config { vector: false }`, which is a different,
//! persisted decision: it turns the vector lane OFF rather than parking it.
//!
//! Test: `embedding_pause_tests.rs`.

use axum::http::StatusCode;
use std::sync::Arc;

use crate::core::registry::IndexId;

use super::state::SearchAppState;

/// Stop this index's embedding stage at its next batch boundary.
///
/// Why: idempotent because an operator clicking twice, or two consoles pausing
/// the same index, must not be an error — the question the caller asked is
/// "is it paused now", and the answer is the same either way.
/// What: sets the handle's flag and answers `{"index_id", "embedding_paused"}`.
/// A running pass stops within one wave, commits what it embedded, and
/// re-queues the remainder; the semantic stage stays `in_progress` with
/// `paused: true` beside it.
///
/// # Errors
///
/// `404` when the index is not in the registry — the same body every
/// index-scoped read answers with.
///
/// Test: `pause_then_resume_round_trips_over_the_socket`,
/// `pausing_an_unknown_index_is_refused`, `pause_is_idempotent`.
pub(crate) fn pause_embedding_report(
    state: &Arc<SearchAppState>,
    id: &str,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let handle = resolve(state, id)?;
    let was_paused = handle.embedding_pause.pause();
    if !was_paused {
        tracing::info!(index_id = %handle.id, "embedding paused by operator (#6524)");
    }
    Ok(serde_json::json!({
        "index_id": handle.id.0,
        "embedding_paused": true,
    }))
}

/// Let this index's embedding stage continue.
///
/// Why: the half of the pair that undoes the other. A parked stage wakes on
/// this call and resumes at the first chunk the vector store does not already
/// hold, so no work is repeated and none is lost.
/// What: clears the handle's flag, waking every parked stage, and answers the
/// same shape [`pause_embedding_report`] does. Idempotent for the same reason.
///
/// # Errors
///
/// `404` when the index is not in the registry.
///
/// Test: `pause_then_resume_round_trips_over_the_socket`,
/// `resuming_an_unknown_index_is_refused`, `resume_is_idempotent`.
pub(crate) fn resume_embedding_report(
    state: &Arc<SearchAppState>,
    id: &str,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let handle = resolve(state, id)?;
    let was_paused = handle.embedding_pause.resume();
    if was_paused {
        tracing::info!(index_id = %handle.id, "embedding resumed by operator (#6524)");
    }
    Ok(serde_json::json!({
        "index_id": handle.id.0,
        "embedding_paused": false,
    }))
}

/// Look one index up, or refuse with the shared `404` body.
fn resolve(
    state: &Arc<SearchAppState>,
    id: &str,
) -> Result<Arc<crate::core::registry::IndexHandle>, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    state.registry.get(&index_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("unknown index '{}'", index_id.0) }),
        )
    })
}
