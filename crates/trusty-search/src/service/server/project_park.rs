//! How a parked index is served — or not (#9169 PR-B1).
//!
//! Why: a parked index keeps its corpus and its `indexes.toml` row, so every
//! path that reaches an index by id must decide what parking means for it:
//! a direct search must say where to go instead of returning empty hits, the
//! fan-out must skip it, and the listing must still show it.
//! What: [`parked_refusal`] is the `409` (socket `-32009`) a direct request
//! gets; [`without_parked`] filters a fan-out; [`with_map_parked`] adds the
//! `parked: true` rows to `search.indexes.list`; [`detach_parked`] unloads a
//! newly parked index; [`reindex_after_unpark`] reloads one and queues its
//! reindex.
//! Test: `project_park_tests.rs`.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::Json;

use super::state::SearchAppState;
use super::ListIndexesParams;
use crate::core::project_map::ProjectMap;
use crate::core::project_map_io;
use crate::core::registry::IndexId;
use crate::service::persistence::PersistedIndex;
use crate::service::reindex::ReindexStatus;

/// The map beside this daemon's registry, never migrated. `None` when it is
/// absent or unreadable.
///
/// Why: ruling — a request by explicit `index_id` keeps working when the map
/// cannot be read. `search.project.resolve` and `search.project.map` report
/// the error; this path does not invent parks it cannot read.
fn readable_map(state: &SearchAppState) -> Option<ProjectMap> {
    let loaded =
        crate::service::project_map_state::registry_path(state.registry_path_override.as_deref())
            .and_then(|registry| project_map_io::load(&project_map_io::map_path_beside(&registry)));
    match loaded {
        Ok(map) => map,
        Err(e) => {
            // #9169: explicit ids keep working; resolve/map surface the error.
            tracing::debug!(error = %e, "project-map unreadable; serving by explicit id");
            None
        }
    }
}

/// The refusal a request naming a parked index gets (#9169).
///
/// What: `409 index_parked` with `parked: true`, the group `key`, the group's
/// pinned `canonical_index_id` (`null` when the group has no pin) and
/// `retryable: false`. The socket renders it as `-32009` with this body as
/// `data`. Never empty hits, never a redirect.
/// Test: `a_search_on_a_parked_id_is_a_conflict_naming_the_canonical_index`.
pub(crate) fn parked_refusal(
    state: &SearchAppState,
    index_id: &str,
) -> Option<(StatusCode, serde_json::Value)> {
    let map = readable_map(state)?;
    let row = map.parked_row(index_id)?;
    let canonical = map.pin_for(&row.key).map(|p| p.index_id.clone());
    let instead = canonical.as_deref().unwrap_or("its project's live index");
    Some((
        StatusCode::CONFLICT,
        serde_json::json!({
            "error": "index_parked",
            "message": format!(
                "index {index_id} is parked; query {instead} instead, or run \
                 `trusty-search project unpark {index_id}`"
            ),
            "index_id": index_id,
            "parked": true,
            "canonical_index_id": canonical,
            "key": row.key,
            "reason": row.reason,
            "retryable": false,
        }),
    ))
}

/// [`parked_refusal`] in the `Json` shape the index resolver returns.
pub(super) fn parked_refusal_json(
    state: &SearchAppState,
    index_id: &str,
) -> Option<(StatusCode, Json<serde_json::Value>)> {
    parked_refusal(state, index_id).map(|(status, body)| (status, Json(body)))
}

/// `ids` without the parked ones — the fan-out never searches a parked index.
/// Test: `the_fan_out_skips_a_parked_index_that_is_still_resident`.
pub(super) fn without_parked(state: &SearchAppState, ids: Vec<IndexId>) -> Vec<IndexId> {
    match readable_map(state) {
        Some(map) => ids.into_iter().filter(|id| !map.is_parked(&id.0)).collect(),
        None => ids,
    }
}

/// Add every parked index to a `search.indexes.list` body as a `parked: true`
/// row of its `parked` array (#9169).
///
/// Why: a parked index is not resident, so the resident listing omits it; the
/// operator still needs to see it and why.
/// What: one row per map-parked id `{id, root_path, parked, reason, key,
/// canonical_index_id}`, merged into an existing cold row of the same id.
/// A `?repo_identity=` filter keeps only rows of that group key. The tree
/// format is unchanged. With nothing parked the body is unchanged.
/// Test: `park_and_unpark_keep_the_corpus_and_queue_a_reindex`.
pub(crate) fn with_map_parked(
    state: &SearchAppState,
    params: &ListIndexesParams,
    mut body: serde_json::Value,
) -> serde_json::Value {
    if params.format.as_deref() == Some("tree") {
        return body;
    }
    let Some(map) = readable_map(state) else {
        return body;
    };
    let filter = params.repo_identity.as_deref().map(|raw| {
        trusty_common::repo_identity::RepoIdentity::parse(raw.trim())
            .map(|r| r.canonical())
            .unwrap_or_else(|| raw.trim().to_string())
    });
    for row in map
        .parked
        .iter()
        .filter(|r| filter.as_ref().is_none_or(|f| *f == r.key))
    {
        let fields = serde_json::json!({
            "id": row.index_id,
            "root_path": row.root_path.to_string_lossy(),
            "parked": true,
            "reason": row.reason,
            "key": row.key,
            "canonical_index_id": map.pin_for(&row.key).map(|p| p.index_id.clone()),
        });
        let rows = body
            .as_object_mut()
            .map(|o| o.entry("parked").or_insert_with(|| serde_json::json!([])));
        let Some(serde_json::Value::Array(rows)) = rows else {
            continue;
        };
        match rows.iter_mut().find(|r| r["id"] == fields["id"]) {
            Some(serde_json::Value::Object(existing)) => {
                if let serde_json::Value::Object(add) = fields {
                    existing.extend(add);
                }
            }
            _ => rows.push(fields),
        }
    }
    body
}

/// Unload a just-parked index: resident → cold-parked → out of the cold store,
/// with its watcher stopped (#9169: parked indexes are not watched).
///
/// What: a resident index is detached through the residency park, which saves
/// its vector store first and refuses while a reindex runs; then the cold
/// entry is dropped so nothing lazy-loads it. Returns whether the index is
/// now unloaded. The corpus and the registry row are never touched.
/// Test: `park_and_unpark_keep_the_corpus_and_queue_a_reindex`.
pub(crate) async fn detach_parked(
    state: &Arc<SearchAppState>,
    index_id: &str,
    entry: Option<PersistedIndex>,
) -> bool {
    let id = IndexId::new(index_id);
    if state.registry.get(&id).is_some() {
        let Some(entry) = entry else {
            return false;
        };
        let running = || {
            state
                .reindex_progress
                .get(&id)
                .is_some_and(|p| p.status.load() == ReindexStatus::Running)
        };
        let parked = crate::service::lazy_loader::cold_park_index(
            &id,
            &state.registry,
            &state.cold_store,
            entry,
            running,
        )
        .await;
        if !parked {
            return false;
        }
        state.watcher_manager.stop_for_index(&id).await;
    }
    let token = state.cold_store.entry_token(&id);
    state.cold_store.mark_loaded_if(&id, token);
    true
}

/// Reload an unparked index if needed, then queue its reindex (#9169).
///
/// What: a non-resident index is registered cold from its `indexes.toml` row
/// and loaded through the ordinary lazy-load path; then the ordinary reindex
/// core runs. Every refusal is the reindex or load path's own.
/// Test: `park_and_unpark_keep_the_corpus_and_queue_a_reindex`.
pub(crate) async fn reindex_after_unpark(
    state: &Arc<SearchAppState>,
    index_id: &str,
    entry: Option<PersistedIndex>,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let id = IndexId::new(index_id);
    if state.registry.get(&id).is_none() {
        let Some(entry) = entry else {
            return Err((
                StatusCode::NOT_FOUND,
                serde_json::json!({ "error": format!("unknown index: {index_id}") }),
            ));
        };
        state.cold_store.register_cold_entries(vec![entry]);
        super::index_resolve::resolve_or_load_index(state, &id)
            .await
            .map_err(|(status, Json(body))| (status, body))?;
    }
    super::reindex_handlers::reindex_report(state, index_id, None).await
}
