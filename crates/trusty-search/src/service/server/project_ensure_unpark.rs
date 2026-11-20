//! `search.project.ensure` re-activates a parked index (#11417).
//!
//! Why: ensure found a cold or map-parked row at the project root, answered
//! `created: false`, and left it parked. Nothing that lists live indexes —
//! `review_pr` among them — could then see the project's index, and the only
//! remedy on offer was a reindex. ensure is the supported way back.
//! What: [`parked_id_at_root`] finds a map-parked row at the root (a park
//! drops the cold entry, so the collision scan misses it);
//! [`reactivate_parked`] un-parks a non-resident or map-parked index whose
//! root is not a worktree, through the same load path `search.project.unpark`
//! uses, records `repo_identity` from the project key when the row has none,
//! and queues the reindex an unpark queues.
//! Test: `project_ensure_unpark_tests.rs`.

use std::path::Path;
use std::sync::Arc;

use axum::http::StatusCode;

use super::state::SearchAppState;
use crate::core::registry::IndexId;
use crate::service::persistence::PersistedIndex;
use crate::service::project_map_state::MapContext;
use crate::service::project_resolve::RootKind;

/// The project map, or `None` when it is absent or unreadable — the rule
/// `project_park` applies to requests by explicit id.
fn map(ctx: &MapContext) -> Option<crate::core::project_map::ProjectMap> {
    crate::core::project_map_io::load(&ctx.map_path)
        .inspect_err(|e| tracing::debug!(error = %e, "project.ensure: map unreadable (#11417)"))
        .ok()
        .flatten()
}

/// The id of a map-parked row registered at `root`, if any.
///
/// Why: a park unloads the index and drops its cold entry, so the root
/// collision scan finds nothing and ensure would register a second index.
/// Test: `ensure_unparks_a_map_parked_main_checkout`.
pub(super) fn parked_id_at_root(state: &SearchAppState, root: &Path) -> Option<String> {
    let ctx = MapContext::capture(state).ok()?;
    let map = map(&ctx)?;
    map.parked
        .iter()
        .find(|row| super::helpers::identifies_same_root(&row.root_path, root))
        .map(|row| row.index_id.clone())
}

/// A refusal of the re-activation; never a silent success.
fn refused(
    index_id: &str,
    status: StatusCode,
    detail: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let message = format!(
        "index {index_id} is parked and could not be re-activated, so it is not live. \
         Retry `search.project.ensure`; do not reindex to clear a park"
    );
    (
        status,
        serde_json::json!({
            "error": "ensure_unpark_failed",
            "message": message,
            "index_id": index_id,
            "parked": true,
            "cause": detail,
        }),
    )
}

/// The canonical identity `project_key` names, or `None` for a `local:` key.
fn identity_of(project_key: &str) -> Option<String> {
    trusty_common::repo_identity::RepoIdentity::parse(project_key.trim()).map(|r| r.canonical())
}

/// Re-activate `index_id` when ensure found it parked.
///
/// Why: see the module doc. Fail-Open Check: a re-activation that cannot load
/// the index is an error, never `created: false` success.
/// What: `Ok(None)` when there is nothing to do — the index is resident and
/// not map-parked, or its root is a worktree (today's behaviour). Otherwise:
/// the `indexes.toml` row gains `repo_identity` from `project_key` when it has
/// none; a map park row is cleared; the index loads through
/// [`super::project_park::reload_after_unpark`]; then a reindex is queued as
/// `search.project.unpark` queues one. Answers the fields to merge into the
/// ensure body: `{unparked: true, reindex_queued, reindex | reindex_error,
/// repo_identity_set}`. A reindex that cannot be queued is reported, not
/// fatal: the index is live. A missing row, a map or registry write that fails,
/// or a load that fails is `Err` with `error: ensure_unpark_failed`.
/// Test: `ensure_reactivates_a_cold_parked_root_and_records_its_identity`,
/// `ensure_unparks_a_map_parked_main_checkout`,
/// `ensure_leaves_a_cold_worktree_root_parked`,
/// `ensure_reports_an_unpark_that_cannot_load_as_an_error`.
pub(super) async fn reactivate_parked(
    state: &Arc<SearchAppState>,
    index_id: &str,
    root: &Path,
    project_key: &str,
) -> Result<Option<serde_json::Value>, (StatusCode, serde_json::Value)> {
    let internal = |e: String| refused(index_id, StatusCode::INTERNAL_SERVER_ERROR, e.into());
    let ctx = MapContext::capture(state).map_err(|e| internal(e.to_string()))?;
    let map_parked = map(&ctx).is_some_and(|m| m.is_parked(index_id));
    let resident = state.registry.get(&IndexId::new(index_id)).is_some();
    if resident && !map_parked {
        return Ok(None);
    }
    let names = crate::service::constants::ephemeral_dir_names();
    if crate::service::project_resolve::classify_root_kind(root, &names) == RootKind::Worktree {
        return Ok(None);
    }
    let Some(mut row) = ctx.persisted.iter().find(|e| e.id == index_id).cloned() else {
        return Err(internal(format!("no indexes.toml row for {index_id}")));
    };
    let mut identity_set = false;
    let missing = row.repo_identity.is_none();
    if let Some(identity) = identity_of(project_key).filter(|_| missing) {
        row.repo_identity = Some(identity);
        let (path, updated) = (ctx.registry_path.clone(), row.clone());
        tokio::task::spawn_blocking(move || {
            crate::service::persistence::upsert_index_registry_entry_at(&path, updated)
        })
        .await
        .map_err(|e| internal(e.to_string()))?
        .map_err(|e| internal(format!("{e:#}")))?;
        identity_set = true;
    }
    if map_parked {
        let (path, id) = (ctx.map_path.clone(), index_id.to_string());
        tokio::task::spawn_blocking(move || {
            crate::service::project_map_state::unpark_decision(&path, &id)
        })
        .await
        .map_err(|e| internal(e.to_string()))?
        .map_err(|e| internal(e.to_string()))?;
    }
    // The load re-registers the cold entry from `row`, identity included.
    load(state, index_id, row).await?;
    let mut out = serde_json::json!({ "unparked": true, "repo_identity_set": identity_set });
    match super::reindex_handlers::reindex_report(state, index_id, None).await {
        Ok(reindex) => {
            out["reindex_queued"] = true.into();
            out["reindex"] = reindex;
        }
        Err((status, body)) => {
            tracing::warn!(
                "project.ensure[{index_id}]: un-park reindex not queued ({status}): {body}"
            );
            out["reindex_queued"] = false.into();
            out["reindex_error"] = serde_json::json!({ "status": status.as_u16(), "body": body });
        }
    }
    Ok(Some(out))
}

/// Load through the unpark path; a failure is the ensure's error.
async fn load(
    state: &Arc<SearchAppState>,
    index_id: &str,
    row: PersistedIndex,
) -> Result<(), (StatusCode, serde_json::Value)> {
    super::project_park::reload_after_unpark(state, index_id, Some(row))
        .await
        .map_err(|(status, body)| {
            tracing::warn!("project.ensure[{index_id}]: un-park load refused ({status}): {body}");
            refused(index_id, status, body)
        })
}
