//! Serve-only indexes: served by this daemon, never built by it (#8883).
//!
//! Why: a serving instance loads a pre-built index shipped from a dedicated
//! indexer. A reindex there is expensive and replaces the shipped index.
//! What: an index whose `indexes.toml` entry carries `serve_only = true`
//! restores with `IndexHandle::serve_only` set. The reindex claim
//! (`exclude_hold::claim_reindex`) then refuses it, so HTTP, the socket, MCP,
//! the CLI, the config-release catch-up and the boot reconcile all stop there.
//! The automatic paths that would write without a request skip it on their
//! own: the boot reconcile, the watcher spawn, the boot deferred-embed re-arm
//! and the vector-gap backfill. Reads are untouched.
//! Test: `crate::service::server::serve_only_8883_tests`.

use axum::http::StatusCode;

use crate::core::registry::IndexId;
use crate::service::server::SearchAppState;

/// The serve-only mark an existing registration of `id` already carries.
///
/// Why: `POST /indexes` over an id that is cold, or only in `indexes.toml`,
/// rewrites that id's whole record. A create must not clear the mark.
/// What: the cold store's record first, then the `indexes.toml` entry; `false`
/// when neither has one. An unreadable registry answers `true` and logs it:
/// failing closed costs a reindex, failing open costs the shipped index.
/// Test: `a_create_over_a_cold_serve_only_index_keeps_the_mark`.
pub(crate) fn prior_mark(state: &SearchAppState, id: &IndexId) -> bool {
    if let Some(persisted) = state.cold_store.get_persisted(id) {
        return persisted.serve_only;
    }
    match crate::service::persistence::find_index_registry_entry(&id.0) {
        Ok(entry) => entry.is_some_and(|e| e.serve_only),
        Err(e) => {
            tracing::error!(index_id = %id, "cannot read indexes.toml, treating as serve-only (#8883): {e}");
            true
        }
    }
}

/// The operator-facing reason a serve-only index refused a reindex.
///
/// What: names the index, says nothing was queued and search still works,
/// and names both remedies.
pub(crate) fn reason(index_id: &str) -> String {
    format!(
        "index '{index_id}' is serve-only: this daemon serves the pre-built index it was \
         shipped and never rebuilds it, so the reindex is refused and nothing was queued. \
         Search keeps working. Reindex on the indexer and ship the result, or remove \
         `serve_only = true` from this index's indexes.toml entry and restart the daemon (#8883)"
    )
}

/// The `403 index_serve_only` answer every reindex transport returns.
///
/// Why: 403, the allowlist's status, because the request is well-formed and
/// the index is healthy; the operator's configuration forbids it.
/// What: `error: "index_serve_only"`, the `index_id`, `reason: "serve_only"`,
/// the [`reason`] text as `message`, and `retryable: false` — only an
/// operator edit clears it. Callers add their own fields.
/// Test: `reindex_of_a_serve_only_index_is_refused_with_403`.
pub(crate) fn refusal(index_id: &str) -> (StatusCode, serde_json::Value) {
    (
        StatusCode::FORBIDDEN,
        serde_json::json!({
            "error": "index_serve_only",
            "index_id": index_id,
            "reason": "serve_only",
            "message": reason(index_id),
            "retryable": false,
        }),
    )
}
