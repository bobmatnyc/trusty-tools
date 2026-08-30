//! The delete-time root-path re-check `search.index.delete` applies (#6380).
//!
//! Why: an index id is derived deterministically from its `root_path`, so a
//! path that is deleted and recreated between the moment a caller decided to
//! remove a registration and the moment the delete arrives yields the SAME id
//! for a different, live index. Every caller that acts on a census — the
//! console's batch prune above all — reads a fact and then acts on it later,
//! and nothing in the delete path noticed that the fact had changed. The caller
//! cannot close that window itself: only the daemon holds the registry, so only
//! the daemon can compare the expectation against the registration it is about
//! to tear down.
//!
//! What: [`refuse_unless_root_matches`] resolves the registration's CURRENT
//! `root_path` the same three ways `unregister_index` does — the hot registry
//! handle, the cold store, then the `indexes.toml` row — and refuses the delete
//! unless it equals the caller's expectation. It runs BEFORE any teardown, so a
//! refusal changes nothing.
//!
//! Every arm that is not an exact match refuses. A registration that is absent,
//! and a registry file that cannot be read, are both refusals rather than
//! permission to proceed: "I could not check" is not "it still matches". The
//! guard is opt-in per request — a delete that sends no expectation behaves
//! exactly as it did — but once an expectation IS sent, no arm downgrades to
//! proceed-anyway.
//!
//! Test: `crate::service::server::tests_6380`.

use std::path::Path;
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};

use crate::core::registry::IndexId;

use super::state::SearchAppState;

/// Refuse the delete unless `id`'s current `root_path` is `expected`.
///
/// Why: see the module docs — this is the only place in the process that can
/// compare the caller's expectation against the registry, and it must run
/// before `unregister_index` touches anything.
/// What: reads the current root from the hot registry, else the cold store,
/// else `indexes.toml`, and compares it to `expected` as the same display
/// string the census and `GET /indexes/{id}/status` report. No canonicalisation:
/// the roots this guard exists for are GONE from disk, so `canonicalize` would
/// fail on exactly the rows a prune is about to remove.
///
/// # Errors
///
/// `409` when the registration's root is not `expected`; `404` when the id has
/// no registration to compare against; `500` when the registry file could not
/// be read, because an unreadable registry leaves the comparison unmade.
///
/// Test: `a_delete_whose_expected_root_moved_is_refused`,
/// `a_delete_whose_expected_root_matches_proceeds`,
/// `a_delete_expectation_against_an_absent_registration_is_refused`,
/// `an_unreadable_registry_refuses_an_expected_root_delete`.
pub(crate) fn refuse_unless_root_matches(
    state: &Arc<SearchAppState>,
    id: &str,
    expected: &str,
) -> Result<(), (StatusCode, Value)> {
    let index_id = IndexId::new(id.to_string());
    if let Some(handle) = state.registry.get(&index_id) {
        return compare(id, expected, &handle.root_path);
    }
    if let Some(entry) = state.cold_store.get_persisted(&index_id) {
        return compare(id, expected, &entry.root_path);
    }
    match crate::service::persistence::find_index_registry_entry(id) {
        Ok(Some(entry)) => compare(id, expected, &entry.root_path),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            body(
                id,
                format!(
                    "unknown index: {id} — no registration to check the expected root \
                     path against"
                ),
            ),
        )),
        // #6380: an unreadable registry is not proof the root still matches. The
        // same argument #6363 records for the 404 it refuses to invent from one.
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            body(
                id,
                format!(
                    "could not read indexes.toml to confirm '{id}' still points at the \
                     expected root path ({e:#}); refusing the delete"
                ),
            ),
        )),
    }
}

/// Compare the caller's expectation against the root the registry holds now.
fn compare(id: &str, expected: &str, current: &Path) -> Result<(), (StatusCode, Value)> {
    let current = current.display().to_string();
    if current == expected {
        return Ok(());
    }
    Err((
        StatusCode::CONFLICT,
        body(
            id,
            format!(
                "'{id}' now points at {current}, not at the expected {expected}; the \
                 registration changed after it was listed, so the delete was refused"
            ),
        ),
    ))
}

/// The refusal body, in the shape every other delete verdict answers with.
///
/// `removed` and `data_deleted` are stated rather than omitted: a caller that
/// reads them off a refusal must see that nothing happened.
fn body(id: &str, error: String) -> Value {
    json!({
        "id": id,
        "ok": false,
        "error": error,
        "removed": false,
        "data_deleted": false,
    })
}
