//! The one place a request turns an index id into a live `IndexHandle`,
//! loading a cold-parked index on the way (#993, #5349).
//!
//! Why: this flow used to exist only inside `search_handler`, so `search` was
//! the only endpoint that could bring a cold-parked index back. `index-file`
//! and `remove-file` did a bare hot-registry lookup and answered
//! `503 index_not_resident` against the identical daemon state (#5349) — the
//! caller was told to issue an unrelated `search` purely for its side effect,
//! then retry the write. On a network-mounted root those two endpoints are the
//! ONLY thing keeping the index current (#3408), so the endpoint that must
//! never stall was the one that could not help itself.
//!
//! What: [`resolve_or_load_index`] — hot registry, then the residency verdict,
//! then `get_or_load_index`. Every failure is an `Err`, so a caller written as
//! `resolve_or_load_index(..).await?` cannot advance its own state on a load
//! that did not happen.
//!
//! Test: `service::server::tests_5349`.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::Json;

use crate::core::registry::{IndexHandle, IndexId};
use crate::service::lazy_loader::{cold_reload_timeout, get_or_load_index, LazyLoadError};
use crate::service::lazy_restore::restore_index_on_demand;
use crate::service::warm_boot::restore_one_index_bounded;

use super::state::SearchAppState;

/// Resolve `index_id` to a live handle, restoring it from the cold store if it
/// is parked.
///
/// Why: see the module header. One implementation means a write and a read
/// against the same cold-parked index can never disagree about whether that
/// index is reachable.
///
/// What: (1) hot registry; (2) `503 index_restore_failed` / `404` for the two
/// verdicts reachable without attempting a load, via the shared
/// [`super::degraded::residency_miss_response`] builder; (3)
/// `503 embedder_initializing` when no embedder is installed yet, since
/// rebuilding the vector lane needs one; (4) `get_or_load_index`, whose
/// per-index loading gate serialises concurrent callers so N simultaneous
/// requests drive at most one restore — the losers re-check the hot registry
/// after the gate and get the handle the winner registered.
///
/// A failed load NEVER yields `Ok`: `RestoreFailed` and `NotFound` render the
/// residency verdict, and a load still in flight renders
/// `503 index_loading` with `retry_after_secs`. Callers propagate with `?`
/// before touching the index, so a write against an index that could not be
/// loaded is refused rather than reported as applied.
///
/// Test: `cold_parked_index_accepts_a_write_by_driving_the_load`,
/// `write_against_an_unloadable_cold_index_fails_loudly`,
/// `concurrent_writes_on_a_cold_index_both_land_in_one_loaded_index`.
pub(super) async fn resolve_or_load_index(
    state: &Arc<SearchAppState>,
    index_id: &IndexId,
) -> Result<Arc<IndexHandle>, (StatusCode, Json<serde_json::Value>)> {
    // Hot path first — a warm index never pays for the embedder check.
    if let Some(handle) = state.registry.get(index_id) {
        return Ok(handle);
    }
    // #1106 / #5061: permanently-failed and genuinely-unknown are the two
    // residency verdicts reachable without attempting a load. The remaining
    // verdict (`index_not_resident`) is unreachable from here by construction:
    // a cold-but-not-failed index falls through to the lazy load below.
    if state.cold_store.is_failed(index_id) || !state.cold_store.contains(index_id) {
        return Err(super::degraded::residency_miss_response(
            &state.cold_store,
            index_id,
        ));
    }
    let Some(embedder) = state.current_embedder().await else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "embedder_initializing",
                // #5061: every 5xx on this path carries `retryable` and
                // `index_id`. A client branching on `retryable` must never meet
                // a body that omits it — absent reads as undefined, and both
                // interpretations are wrong for some state.
                "index_id": index_id.0,
                "retryable": true,
                "message": "embedder not yet ready — retry after /health reports embedder:ready",
            })),
        ));
    };
    let s = Arc::clone(state);
    let load_result = get_or_load_index(
        index_id,
        &state.registry,
        &state.cold_store,
        cold_reload_timeout(),
        move |entry| async move {
            let e = Arc::clone(&embedder);
            // #4087 review follow-up: `is_complete()` preserves the exact prior
            // contract — anything short of a finished restore is a lazy-load
            // failure, surfaced loudly as a 503 below.
            restore_one_index_bounded(entry, move |en| async move {
                restore_index_on_demand(&s, &e, en).await;
            })
            .await
            .is_complete()
        },
    )
    .await;
    match load_result {
        Ok(handle) => Ok(handle),
        // #5061: both terminal load outcomes are residency verdicts and go
        // through the shared builder. `RestoreFailed` lands on its
        // `index_restore_failed` arm rather than a 404 because BOTH paths that
        // produce this variant guarantee the entry is in the failed set —
        // `loader.rs` returns it *because* `is_failed` is already true, or
        // calls `mark_failed` immediately before returning it.
        Err(LazyLoadError::NotFound) | Err(LazyLoadError::RestoreFailed) => Err(
            super::degraded::residency_miss_response(&state.cold_store, index_id),
        ),
        // A load in flight is not a residency miss — the index is neither
        // absent nor failed, it is arriving. Its own body carries the same
        // `retryable` / `index_id` contract.
        Err(LazyLoadError::Loading { retry_after_secs }) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "index_loading",
                "index_id": index_id.0,
                "retryable": true,
                "retry_after_secs": retry_after_secs,
            })),
        )),
    }
}
