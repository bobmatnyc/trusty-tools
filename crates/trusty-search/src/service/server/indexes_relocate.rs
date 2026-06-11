//! `PATCH /indexes/:id` — in-place root relocation (issue #1073).
//!
//! Why: when a project directory moves on disk the colocated `.trusty-search/`
//! bundle (HNSW snapshot + redb corpus + persisted file-hash cache) moves with
//! it. This module provides the handler that rebinds the daemon's in-memory
//! registry and `indexes.toml` to the new path WITHOUT clearing the hash cache,
//! so a subsequent incremental reindex skips all unchanged files (zero re-embeds
//! for a pure directory move).
//! What: `RelocateIndexRequest` + `relocate_index_handler` — the handler
//! validates the new path, rebuilds the `IndexHandle`, persists the change, and
//! updates `indexed_root` in the corpus `_meta` table.
//!
//! Issue #1089: the handler previously hardcoded `colocated: true` when building
//! the `PersistedIndex` to persist. This caused two problems:
//!   1. It would rewrite the on-disk entry with `colocated = true` even if the
//!      stored entry had `colocated = false` (legacy central-store layout).
//!   2. Because `upsert_index_registry_entry` overwrites the whole record,
//!      other indexes' entries in `indexes.toml` retained their values — but
//!      the PATCHED index's `colocated` field was forcibly toggled.
//!
//! Fix: load the existing `PersistedIndex` from `indexes.toml` first and
//! preserve its `colocated` flag (along with all other persisted fields not
//! supplied by the PATCH request).
//!
//! Issue #1088: colocated-flag flips wiped central-store data. Root cause is
//! the same `colocated: true` hardcode — when a user manually edited
//! `indexes.toml` to set `colocated = false` (to match reality) and the daemon
//! restarted, `create_index_handler` or this PATCH handler would restore
//! `colocated = true`, and `build_indexer_from_entry` would create a fresh
//! `.trusty-search/` dir that shadowed the real central-store data. Fix: read
//! the persisted `colocated` value and refuse to silently toggle it — see
//! the guard added below.
//!
//! Review finding (PR #1103): the handler set `last_queried_unix` and
//! `last_indexed_unix` to `None` even though the comment said "preserve them".
//! Fix: load both timestamps from the on-disk entry in the same read that
//! fetches `colocated` so relocation never silently zeroes the LRU sort key.
//!
//! Test: `relocate_index_updates_root_path` in `tests_index.rs`;
//!       `relocate_preserves_colocated_flag` in `tests_index.rs`;
//!       `relocate_preserves_lru_timestamps` in `tests_index.rs`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use std::sync::Arc;

use crate::core::registry::{IndexHandle, IndexId};

use super::helpers::{embedder_error_response, embedder_initializing_response, validate_root_path};
use super::state::{DaemonEvent, SearchAppState};

/// Request body for `PATCH /indexes/:id` — in-place root relocation (#1073).
///
/// Why: exposes the new `root_path` for an existing index so callers can update
/// the daemon's registry after a project directory has moved on disk, without
/// triggering a full re-embed of unchanged files.
/// What: a single `root_path` field containing the new absolute directory path.
/// Test: `relocate_index_updates_root_path` in `tests_index.rs`.
#[derive(Deserialize)]
pub(super) struct RelocateIndexRequest {
    /// New absolute path to which the index's project directory has moved.
    pub root_path: std::path::PathBuf,
}

/// `PATCH /indexes/:id` — rebind an existing index to a new root path (#1073).
///
/// Why: when a project directory moves (volume remount, machine migration,
/// worktree relocation) the colocated `.trusty-search/` data (HNSW snapshot +
/// redb corpus + persisted file-hash cache) moves with it. This endpoint
/// updates the in-memory registry and `indexes.toml` to reflect the new path
/// WITHOUT clearing the hash cache, so a subsequent reindex skips all
/// unchanged files (zero re-embeds for a pure move).
///
/// What: (1) validates the new path is absolute, exists, and is a directory;
/// (2) rebuilds the `IndexHandle` from the updated `PersistedIndex` entry (so
/// the colocated HNSW/redb at the new location are opened correctly); (3)
/// writes the new `root_path` to `indexes.toml` via
/// `upsert_index_registry_entry`; (4) updates the in-memory DashMap; (5) also
/// updates `indexed_root` in the corpus's `_meta` table so the next reindex
/// does NOT see a root-move (which would otherwise clear the hash cache for
/// non-colocated legacy indexes). Emits `IndexRegistered` so connected UIs
/// refresh.
///
/// Returns 404 when `id` is not in the registry, 400 for an invalid path, 500
/// on internal rebuild failure. On success returns
/// `{ "id": "…", "relocated": true, "new_root_path": "…" }`.
///
/// Test: `relocate_index_updates_root_path` in `tests_index.rs`.
pub(super) async fn relocate_index_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<RelocateIndexRequest>,
) -> Response {
    let index_id = IndexId::new(id.clone());

    // Retrieve the existing handle so we can clone its configuration.
    let existing = match state.registry.get(&index_id) {
        Some(h) => h,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("unknown index: {id}") })),
            )
                .into_response();
        }
    };

    // Validate and canonicalize the new root path.
    let new_root = match validate_root_path(&req.root_path).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    // Require an embedder so we can rebuild the indexer (it needs to open
    // the colocated HNSW/redb at the new location).
    let Some(embedder) = state.current_embedder().await else {
        if let Some(err) = state.current_embedder_error() {
            return embedder_error_response(&err);
        }
        return embedder_initializing_response();
    };

    // Load the existing on-disk entry from indexes.toml once so we can preserve
    // all persisted fields that the PATCH request does not supply.
    //
    // Issue #1088/#1089: the previous code hardcoded `colocated: true` here,
    // which would silently toggle a legitimately `colocated = false` entry and
    // re-introduce the #1088 data-wipe on the next save. Disk entry wins for
    // `colocated` — the user's manual edit must be honoured.
    //
    // Issue #1097: fall back to `false` (central-store) on IO error or missing
    // entry. `false` is safe — it keeps the index in the global data dir.
    //
    // Issue #993 / review finding: the previous code set `last_queried_unix` and
    // `last_indexed_unix` to `None` even though the comment said "preserve them".
    // Read both from the on-disk entry so a PATCH /indexes/:id does not silently
    // zero the LRU sort key and demote a heavily-used index to the cold-store
    // tail on the next selective warm-boot.
    let on_disk = crate::service::persistence::load_index_registry()
        .ok()
        .and_then(|entries| entries.into_iter().find(|e| e.id == id));

    let on_disk_colocated = on_disk.as_ref().map(|e| e.colocated).unwrap_or(false);
    let on_disk_last_queried = on_disk.as_ref().and_then(|e| e.last_queried_unix);
    let on_disk_last_indexed = on_disk.as_ref().and_then(|e| e.last_indexed_unix);

    // Build a PersistedIndex from the existing handle's metadata, substituting
    // the new root path. We preserve all other settings (filters, extensions,
    // lexical_only, etc.) so the handle stays consistent.
    let existing_entry = crate::service::persistence::PersistedIndex {
        id: id.clone(),
        root_path: new_root.clone(),
        include_paths: existing
            .include_paths
            .iter()
            .filter_map(|p| p.to_str().map(str::to_string))
            .collect(),
        exclude_globs: existing.exclude_globs.clone(),
        extensions: existing.extensions.clone(),
        domain_terms: existing.domain_terms.clone(),
        path_filter: existing.path_filter.clone(),
        include_docs: existing.include_docs,
        respect_gitignore: existing.respect_gitignore,
        lexical_only: existing.lexical_only,
        skip_kg: existing.skip_kg,
        defer_embed: existing.defer_embed,
        // Issue #1088/#1089: preserve the persisted colocated flag rather than
        // hardcoding true. Toggling this flag silently would route the indexer
        // to a different data directory and could destroy central-store data.
        colocated: on_disk_colocated,
        // Issue #993: preserve LRU timestamps from disk so relocation doesn't
        // reset warm-boot priority for recently-queried or recently-indexed
        // indexes. `None` here would zero the sort key and move this index to
        // the cold tail on the next selective warm-boot start.
        last_queried_unix: on_disk_last_queried,
        last_indexed_unix: on_disk_last_indexed,
    };

    // Rebuild the indexer from the new entry so the colocated HNSW/redb at
    // the new root are opened (or created if missing — the directory existed
    // per validate_root_path above).
    let new_indexer = match crate::service::persistence_loader::build_indexer_from_entry(
        &existing_entry,
        &embedder,
    )
    .await
    {
        Ok(idx) => idx,
        Err(e) => {
            tracing::error!(
                "relocate[{id}]: failed to rebuild indexer at {}: {e}",
                new_root.display()
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("indexer rebuild failed: {e}") })),
            )
                .into_response();
        }
    };

    // Persist the updated entry to indexes.toml BEFORE replacing the handle,
    // so a daemon restart sees the new root even if the in-memory swap below
    // is interrupted.
    if let Err(e) = crate::service::persistence::upsert_index_registry_entry(existing_entry.clone())
    {
        tracing::warn!("relocate[{id}]: could not persist new root_path to indexes.toml: {e}");
    }

    // Also update roots.toml so the startup scanner can find the new location.
    if let Err(e) = crate::service::roots_registry::upsert_root(new_root.clone()) {
        tracing::warn!("relocate[{id}]: could not update roots.toml: {e}");
    }

    // Build the replacement handle, preserving all in-memory fields from
    // the existing handle (stage states, context embedding, …).
    let new_handle = IndexHandle {
        id: index_id.clone(),
        indexer: Arc::new(tokio::sync::RwLock::new(new_indexer)),
        root_path: new_root.clone(),
        include_paths: existing.include_paths.clone(),
        exclude_globs: existing.exclude_globs.clone(),
        extensions: existing.extensions.clone(),
        domain_terms: existing.domain_terms.clone(),
        include_docs: existing.include_docs,
        respect_gitignore: existing.respect_gitignore,
        path_filter: existing.path_filter.clone(),
        // Preserve in-memory context/stage/SHA fields from the existing handle
        // so ongoing searches see a coherent state.
        context_embedding: Arc::clone(&existing.context_embedding),
        context_summary: Arc::clone(&existing.context_summary),
        indexed_head_sha: Arc::clone(&existing.indexed_head_sha),
        last_indexed_at: Arc::clone(&existing.last_indexed_at),
        lexical_only: existing.lexical_only,
        skip_kg: existing.skip_kg,
        defer_embed: existing.defer_embed,
        stages: Arc::clone(&existing.stages),
        search_pressure: Arc::clone(&existing.search_pressure),
        walk_diagnostics: Arc::clone(&existing.walk_diagnostics),
    };

    // Atomically replace the in-memory registry entry.
    state.registry.register(new_handle);

    // Update `indexed_root` in the corpus `_meta` table so the root-move
    // detection in `spawn_reindex_with_cleanup` does NOT fire on the next
    // incremental reindex (which would otherwise clear the hash cache for
    // non-colocated legacy indexes).
    if let Some(h) = state.registry.get(&index_id) {
        if let Err(e) = h.write_indexed_root(&new_root).await {
            tracing::warn!(
                "relocate[{id}]: failed to update indexed_root in corpus \
                 (next reindex may re-detect root move): {e}"
            );
        }
    }

    state.emit(DaemonEvent::IndexRegistered { id: id.clone() });
    tracing::info!("relocate[{id}]: rebind complete → {}", new_root.display());

    Json(serde_json::json!({
        "id": id,
        "relocated": true,
        "new_root_path": new_root.to_string_lossy(),
    }))
    .into_response()
}
