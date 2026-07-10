//! Per-index hygiene-config inspector + updater (issue #1372).
//!
//! Why: indexing hygiene (which directories to skip, the data-file size cap,
//! the extension allow-list, gitignore handling, doc inclusion) used to be
//! hardcoded constants. The product decision for #1372 is that these are
//! per-index config defaults that an operator can read and override at runtime
//! (and, in a follow-up, edit from the dashboard). These two handlers are the
//! daemon-side read/update surface: `GET /indexes/:id/config` returns the
//! current hygiene config; `PATCH /indexes/:id/config` updates the in-memory
//! handle AND persists to `indexes.toml`. Neither auto-triggers a reindex —
//! the PATCH response carries `reindex_required: true` so the caller knows the
//! change takes effect on the next reindex.
//!
//! What: `index_config_handler` (GET), `patch_index_config_handler` (PATCH),
//! and the serde request/response types.
//!
//! Test: `service::server::tests_index_config`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::registry::{IndexHandle, IndexId};

use super::state::{DaemonEvent, SearchAppState};

/// JSON shape returned by `GET /indexes/:id/config` and accepted (all fields
/// optional) by `PATCH /indexes/:id/config`.
///
/// Why: a single typed struct keeps the read and update wire formats in sync —
/// the GET serialises every field; the PATCH deserialises the same names with
/// `Option` so a partial update only touches the fields the caller sends.
/// What: the per-index hygiene knobs. `extra_skip_dirs` / `extensions` /
/// `exclude_globs` are arrays; `data_file_max_bytes` is the resolved concrete
/// cap (bytes); the three booleans mirror the walker / staged-pipeline flags.
/// Test: `get_then_patch_then_get_round_trips` in
/// `service::server::tests_index_config`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexConfigView {
    /// Extra directory basenames pruned on top of the built-in `SKIP_DIRS`.
    pub extra_skip_dirs: Vec<String>,
    /// Tighter size cap (bytes) applied only to data-ish extensions
    /// (json/xml/txt/log). The resolved concrete value (never null on read).
    pub data_file_max_bytes: u64,
    /// Extension allow-list (without leading dot). Empty = all supported.
    pub extensions: Vec<String>,
    /// Glob patterns excluded on top of the built-in ignores.
    pub exclude_globs: Vec<String>,
    /// Whether prose docs (`*.md`, CHANGELOG, …) are indexed.
    pub include_docs: bool,
    /// Whether the walk honours `.gitignore` / `.ignore` / `.rgignore`.
    pub respect_gitignore: bool,
}

impl IndexConfigView {
    /// Build the view from a live handle.
    ///
    /// Why: both GET and the PATCH echo-back need the same projection of the
    /// handle's hygiene fields.
    /// What: clones the relevant handle fields into the wire struct.
    /// Test: `get_returns_current_config` in `tests_index_config`.
    fn from_handle(handle: &IndexHandle) -> Self {
        Self {
            extra_skip_dirs: handle.extra_skip_dirs.clone(),
            data_file_max_bytes: handle.data_file_max_bytes,
            extensions: handle.extensions.clone(),
            exclude_globs: handle.exclude_globs.clone(),
            include_docs: handle.include_docs,
            respect_gitignore: handle.respect_gitignore,
        }
    }
}

/// Partial-update body for `PATCH /indexes/:id/config`. Every field is optional;
/// only the fields present in the request are applied.
///
/// Why: a partial PATCH lets the dashboard send just the field the user edited
/// without having to round-trip the whole config first.
/// What: each field maps to one `IndexConfigView` field. `data_file_max_bytes`
/// of `Some(0)` is rejected as invalid (a zero cap would skip every data file).
/// Test: `patch_updates_only_supplied_fields`, `patch_rejects_zero_cap`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PatchIndexConfigRequest {
    #[serde(default)]
    pub extra_skip_dirs: Option<Vec<String>>,
    #[serde(default)]
    pub data_file_max_bytes: Option<u64>,
    #[serde(default)]
    pub extensions: Option<Vec<String>>,
    #[serde(default)]
    pub exclude_globs: Option<Vec<String>>,
    #[serde(default)]
    pub include_docs: Option<bool>,
    #[serde(default)]
    pub respect_gitignore: Option<bool>,
}

/// `GET /indexes/:id/config` — return the index's current hygiene config.
///
/// Why: lets the CLI / dashboard show the active hygiene settings before
/// editing them.
/// What: looks up the handle, projects its hygiene fields into
/// `IndexConfigView`, returns `200` with the JSON body or `404` for an unknown
/// id.
/// Test: `get_returns_current_config`, `get_unknown_index_404`.
pub(super) async fn index_config_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Response {
    let index_id = IndexId::new(id);
    let Some(handle) = state.registry.get(&index_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("unknown index '{}'", index_id.0) })),
        )
            .into_response();
    };
    Json(IndexConfigView::from_handle(&handle)).into_response()
}

/// `PATCH /indexes/:id/config` — update the index's hygiene config.
///
/// Why: applies an operator's hygiene edit to the running daemon and persists
/// it so the change survives a restart. Does NOT auto-reindex — the response's
/// `reindex_required: true` hint tells the caller a reindex is needed for the
/// new filters to take effect on already-indexed files.
/// What: validates inputs (rejects `data_file_max_bytes == 0` and blank /
/// duplicate-free not required but empty-string entries are dropped), rebuilds
/// the in-memory handle with the merged fields (preserving the live indexer and
/// all Arc-shared state), re-registers it, then upserts the matching
/// `indexes.toml` entry. Returns the updated `IndexConfigView` plus
/// `reindex_required` on success. A persistence failure returns **500** with a
/// clear error body so the UI never claims success while `indexes.toml` silently
/// went stale (the in-memory change is left applied, but the response is
/// truthful).
///
/// Concurrency note (review #1372): the `dashmap::Ref` returned by
/// `registry.get` is dropped (its scope ends at `read_existing_fields`) BEFORE
/// `registry.register` takes a shard write-lock or `persist_hygiene_update`
/// performs blocking file I/O — holding a read-guard across either could
/// self-deadlock on the same shard. We clone every field we need out of the
/// guard up front, then operate only on owned locals.
/// Test: `patch_updates_only_supplied_fields`, `patch_rejects_zero_cap`,
/// `patch_persists_to_toml`, `patch_persist_failure_returns_500`,
/// `patch_unknown_index_404`.
pub(super) async fn patch_index_config_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<PatchIndexConfigRequest>,
) -> Response {
    let index_id = IndexId::new(id);

    // Validate: a zero (or absurd) data cap would prune every data file.
    if matches!(req.data_file_max_bytes, Some(0)) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "data_file_max_bytes must be greater than zero"
            })),
        )
            .into_response();
    }

    // Read everything we need out of the handle guard and DROP the guard before
    // any write-lock (`register`) or file I/O (`persist_hygiene_update`). The
    // inner scope bounds the `dashmap::Ref` lifetime so it cannot be held across
    // those operations (review #1372 — deadlock guard).
    let new_handle = {
        let Some(existing) = state.registry.get(&index_id) else {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("unknown index '{}'", index_id.0) })),
            )
                .into_response();
        };

        // Merge: start from the current handle values, overlay the supplied
        // fields.
        let extra_skip_dirs = req
            .extra_skip_dirs
            .map(sanitize_dirs)
            .unwrap_or_else(|| existing.extra_skip_dirs.clone());
        let data_file_max_bytes = req
            .data_file_max_bytes
            .unwrap_or(existing.data_file_max_bytes);
        let extensions = req
            .extensions
            .map(sanitize_extensions)
            .unwrap_or_else(|| existing.extensions.clone());
        let exclude_globs = req
            .exclude_globs
            .map(sanitize_dirs)
            .unwrap_or_else(|| existing.exclude_globs.clone());
        let include_docs = req.include_docs.unwrap_or(existing.include_docs);
        let respect_gitignore = req.respect_gitignore.unwrap_or(existing.respect_gitignore);

        // Rebuild the handle, preserving the live indexer + all Arc-shared state
        // (stages, context, SHA, …). Mirrors the reindex/relocate rebuild
        // pattern. Built entirely from clones of the guard's fields so the
        // guard can be released as this block returns.
        IndexHandle {
            id: index_id.clone(),
            indexer: Arc::clone(&existing.indexer),
            root_path: existing.root_path.clone(),
            include_paths: existing.include_paths.clone(),
            exclude_globs,
            extensions,
            domain_terms: existing.domain_terms.clone(),
            include_docs,
            respect_gitignore,
            // The symlink policy is a create-time setting, not part of the
            // hygiene PATCH surface; preserve the existing value across the
            // rebuild so an unrelated config edit never flips it.
            follow_links: existing.follow_links,
            extra_skip_dirs,
            data_file_max_bytes,
            path_filter: existing.path_filter.clone(),
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
        }
        // `existing` (the dashmap::Ref) is dropped here as the block ends.
    };

    // Snapshot the owned fields we need for persistence + the response view.
    // The guard is already released, so these are all owned clones.
    let view = IndexConfigView::from_handle(&new_handle);
    let root_path = new_handle.root_path.clone();
    let extra_skip_dirs = new_handle.extra_skip_dirs.clone();
    let data_file_max_bytes = new_handle.data_file_max_bytes;
    let extensions = new_handle.extensions.clone();
    let exclude_globs = new_handle.exclude_globs.clone();
    let include_docs = new_handle.include_docs;
    let respect_gitignore = new_handle.respect_gitignore;

    // Apply the in-memory change (shard write-lock; guard already dropped).
    state.registry.register(new_handle);

    // Persist: load the existing entry (to preserve fields the handle doesn't
    // carry — colocated, LRU timestamps), overlay the hygiene fields, upsert.
    // A failure here means the in-memory change took but `indexes.toml` did not
    // — surface it as 500 so the caller does NOT report success and revert on
    // the next daemon restart (review #1372).
    if let Err(e) = persist_hygiene_update(
        &index_id.0,
        &root_path,
        &extra_skip_dirs,
        data_file_max_bytes,
        &extensions,
        &exclude_globs,
        include_docs,
        respect_gitignore,
    ) {
        tracing::error!(
            "patch_index_config[{}]: persistence failed: {e}",
            index_id.0
        );
        // The handle was re-registered so the live daemon honours the new
        // config until restart; still emit the event so subscribers stay in
        // sync with in-memory state.
        state.emit(DaemonEvent::IndexRegistered {
            id: index_id.0.clone(),
        });
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!(
                    "config applied in memory but could not be persisted to indexes.toml: {e}. \
                     The change will be lost on the next daemon restart."
                ),
                "config": view,
                "persisted": false,
            })),
        )
            .into_response();
    }

    state.emit(DaemonEvent::IndexRegistered {
        id: index_id.0.clone(),
    });

    Json(serde_json::json!({
        "id": index_id.0,
        "config": view,
        "reindex_required": true,
        "persisted": true,
    }))
    .into_response()
}

/// Trim empties from a directory / glob list and drop blank entries.
///
/// Why: an operator pasting a list may leave trailing blanks; storing them
/// would never match anything and clutter the persisted config.
/// What: trims each entry and drops the empty ones.
/// Test: covered by `patch_updates_only_supplied_fields`.
fn sanitize_dirs(v: Vec<String>) -> Vec<String> {
    v.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Normalise an extension allow-list: strip a leading dot and drop blanks.
///
/// Why: callers write `.rs` or `rs` interchangeably; the walker matches on the
/// bare extension.
/// What: trims, strips a leading `.`, drops empties.
/// Test: covered by `patch_updates_only_supplied_fields`.
fn sanitize_extensions(v: Vec<String>) -> Vec<String> {
    v.into_iter()
        .map(|e| e.trim().trim_start_matches('.').to_string())
        .filter(|e| !e.is_empty())
        .collect()
}

/// Load the persisted entry for `id`, overlay the hygiene fields, and upsert.
///
/// Why: `indexes.toml` carries fields the in-memory handle does not (colocated,
/// LRU timestamps); a blind rebuild would lose them. Loading-then-overlaying
/// preserves everything else. The write is NOT best-effort: a failed `upsert`
/// is returned to the caller so the PATCH response can report 500 rather than
/// claim success while the on-disk registry silently diverges (review #1372).
/// A load failure is still tolerated (the registry file may not exist yet) —
/// we log it and seed a minimal entry, because that path still produces a
/// correct persisted record.
/// What: reads the registry file, finds the matching entry (or seeds a minimal
/// one), updates the six hygiene fields, and writes back via
/// `upsert_index_registry_entry`, propagating any write error.
/// Test: `patch_persists_to_toml` drives the success path against a tempfile;
/// `patch_persist_failure_returns_500` forces an unwritable registry path and
/// asserts the error surfaces.
#[allow(clippy::too_many_arguments)]
fn persist_hygiene_update(
    id: &str,
    root_path: &std::path::Path,
    extra_skip_dirs: &[String],
    data_file_max_bytes: u64,
    extensions: &[String],
    exclude_globs: &[String],
    include_docs: bool,
    respect_gitignore: bool,
) -> anyhow::Result<()> {
    use crate::service::persistence::{
        load_index_registry, upsert_index_registry_entry, PersistedIndex,
    };
    let mut entry = match load_index_registry() {
        Ok(entries) => entries
            .into_iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| PersistedIndex {
                id: id.to_string(),
                root_path: root_path.to_path_buf(),
                ..Default::default()
            }),
        Err(e) => {
            tracing::warn!("patch_index_config[{id}]: could not load registry to persist: {e}");
            PersistedIndex {
                id: id.to_string(),
                root_path: root_path.to_path_buf(),
                ..Default::default()
            }
        }
    };
    entry.extra_skip_dirs = extra_skip_dirs.to_vec();
    entry.data_file_max_bytes = Some(data_file_max_bytes);
    entry.extensions = extensions.to_vec();
    entry.exclude_globs = exclude_globs.to_vec();
    entry.include_docs = include_docs;
    entry.respect_gitignore = respect_gitignore;
    upsert_index_registry_entry(entry)
}
