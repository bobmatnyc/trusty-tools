//! `POST /indexes/:id/roots` and create-time `roots` — the multi-root
//! mutation (#7434).
//!
//! Why: an index's root table is read by the reindex walk, the prune pass,
//! the corpus-path encoder, the watcher and every containment guard. This
//! module is the one door through which a root joins an index, so the
//! one-index-per-tree rules (#2336, #3993, #4289) are re-asked here, for
//! every root, before it does.
//! What: [`add_index_roots_report`] (the transport-neutral core behind the
//! HTTP route and the `search.index.roots.add` socket method),
//! [`admit_create_roots`] (the same gate for `POST /indexes {roots}`) and
//! [`refuse_unapplied_roots`] (an existing id never silently drops `roots`).
//! Test: `tests_7434_roots.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::{Path as UrlPath, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;

use crate::core::index_roots::IndexRoots;
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::persistence::PersistedIndex;
use crate::service::reindex::{spawn_claimed_reindex, ReindexProgress};

use super::create_layout::{claim_registration, RegistrationClaim};
use super::helpers::{
    find_root_path_collision, identifies_any_same_root, identifies_same_root,
    root_path_collision_response, validate_root_path,
};
use super::root_overlap::{
    find_root_overlap, overlap_check_failed_response, root_overlap_response,
};
use super::state::{DaemonEvent, SearchAppState};

type Refusal = (StatusCode, serde_json::Value);

/// Request body for `POST /indexes/:id/roots` (#7434).
///
/// Why: adding a root APPENDS to an ordered table whose positions are baked
/// into every `@root<n>/…` stored path, so the wire shape offers no reorder
/// and no removal.
/// What: absolute directory paths, each validated independently; the whole
/// request is refused when any one is.
/// Test: `add_root_over_http_and_socket_returns_the_full_table`.
#[derive(Debug, Deserialize)]
pub struct AddRootsRequest {
    /// Absolute directory paths to add to this index's root table.
    pub roots: Vec<PathBuf>,
}

/// `POST /indexes/:id/roots` — axum wrapper over [`add_index_roots_report`].
pub(super) async fn add_index_roots_handler(
    State(state): State<Arc<SearchAppState>>,
    UrlPath(id): UrlPath<String>,
    Json(req): Json<AddRootsRequest>,
) -> Response {
    match add_index_roots_report(&state, &id, req).await {
        Ok(body) => Json(body).into_response(),
        Err((status, body)) => (status, Json(body)).into_response(),
    }
}

/// Add roots to a live index, persist the table, watch them and walk them.
///
/// Why: until it runs, an index covers only the trees it was created with.
/// The table must change only while no reindex walks the old one, or that
/// run's prune would judge the corpus against a root set nobody agreed on.
/// What, in order: the teardown read guard (#3049); a no-op answer when every
/// root is already in the table; the #8105 quarantine refusal; the #8889
/// reindex claim (`409 reindex_already_running` while a reindex runs — which
/// also serialises two adds) and the per-index permit (`409` while a relocate,
/// config catch-up or deferred embed holds it); the handle re-read under both;
/// [`resolve_added_roots`]; persist (a failure answers `500` and changes
/// nothing — a root forgotten on restart leaves undecodable `@root<n>/` rows);
/// swap handle and indexer table together; start the new roots' watches; and
/// queue a background, non-forced reindex under the same claim.
/// Test: `add_root_over_http_and_socket_returns_the_full_table`,
/// `add_root_is_refused_while_a_reindex_runs`,
/// `add_root_of_own_root_is_a_no_op`.
pub(crate) async fn add_index_roots_report(
    state: &Arc<SearchAppState>,
    id: &str,
    req: AddRootsRequest,
) -> Result<serde_json::Value, Refusal> {
    let index_id = IndexId::new(id);
    let _teardown = crate::service::reindex::acquire_index_teardown_read(&index_id).await;
    let unknown = || {
        let body = serde_json::json!({ "error": format!("unknown index: {id}"), "index_id": id });
        (StatusCode::NOT_FOUND, body)
    };
    let first = state.registry.get(&index_id).ok_or_else(unknown)?;
    let first_roots = first.roots();
    if req.roots.iter().all(|r| holds_root(&first_roots, r)) {
        return Ok(roots_body(id, &first_roots, &[], false));
    }
    if let Some(refusal) = super::degraded::write_quarantine_refusal(id, &first).await {
        return Err(refusal);
    }
    let claim = crate::service::exclude_hold::claim_reindex(&first, "add-root", false)
        .map_err(|refused| super::reindex_handlers::claim_refusal(&index_id, refused))?;
    let Ok(permit) = crate::service::reindex::index_semaphore(&index_id).try_acquire_owned() else {
        return Err(busy(id));
    };
    // Re-read under the claim: an add that finished since `first` swapped it.
    let existing = state.registry.get(&index_id).ok_or_else(unknown)?;
    let (added, _claims) = resolve_added_roots(state, &existing, &req.roots).await?;
    if added.is_empty() {
        return Ok(roots_body(id, &existing.roots(), &[], false));
    }
    let mut table = existing.additional_roots.clone();
    table.extend(added.iter().cloned());
    persist_roots(&existing, table.clone())?;
    let registered = {
        let mut indexer = existing.indexer.write().await;
        indexer.set_additional_roots(table.clone());
        state.registry.register(handle_with_roots(&existing, table))
    };
    drop(permit);
    state.emit(DaemonEvent::IndexRegistered { id: id.to_string() });
    tracing::info!(
        "add_roots[{id}]: added {} root(s); the index now covers {} tree(s) (#7434)",
        added.len(),
        registered.additional_roots.len() + 1,
    );
    state.watcher_manager.spawn_for_index(&registered).await;
    let progress = Arc::new(ReindexProgress::new());
    state
        .reindex_progress
        .insert(index_id.clone(), Arc::clone(&progress));
    let body = roots_body(id, &registered.roots(), &added, true);
    spawn_claimed_reindex(
        claim,
        registered,
        progress,
        false,
        Some(Arc::clone(&state.reindex_progress)),
        Some(Arc::clone(&state.last_reindex_aborted_at)),
        Some(Arc::clone(&state.embedderd_pid_slot)),
        // Background lane: the caller already has its answer (#458).
        false,
        None,
    );
    Ok(body)
}

/// Validate, de-duplicate and admit candidate roots for `existing`.
///
/// Why: two doors add a root — the add route and `POST /indexes {roots}` —
/// and a second spelling of the one-index-per-tree rule is how they drift.
/// What, per candidate in request order: `validate_root_path` (absolute,
/// exists, denylist, #767 allowlist, canonical); skip a root the index or this
/// request already holds; refuse `409 index_root_overlap` for a root that
/// nests with one of the index's own roots (two watches over one file would
/// store it under two keys). Then, under a registration claim per root
/// (#8499), refuse a root another index owns, live or cold (#2336/#3993,
/// any-of-N) or overlaps (#4289); an overlap check that cannot run is a
/// refusal. Returns the new roots in order, plus the claims to hold until the
/// table is persisted.
/// Test: `add_root_refuses_another_indexes_additional_root`,
/// `add_root_refuses_a_root_nested_in_its_own_primary`,
/// `add_root_of_own_root_is_a_no_op`.
pub(super) async fn resolve_added_roots(
    state: &Arc<SearchAppState>,
    existing: &IndexHandle,
    requested: &[PathBuf],
) -> Result<(Vec<PathBuf>, Vec<RegistrationClaim>), Refusal> {
    let own = existing.roots();
    let mut added: Vec<PathBuf> = Vec::new();
    for candidate in requested {
        let canonical = validate_root_path(candidate, false, &state.allowlist_paths).await?;
        if holds_root(&own, &canonical) || added.iter().any(|r| identifies_same_root(r, &canonical))
        {
            continue;
        }
        let mine = own.all().into_iter().chain(added.iter().cloned());
        if let Some(conflict) = mine
            .filter_map(|r| {
                super::root_overlap::classify_root_overlap(&canonical, &r).map(|o| (o, r))
            })
            .next()
        {
            return Err(own_overlap_response(&existing.id, &canonical, &conflict.1));
        }
        added.push(canonical);
    }
    let mut claims = Vec::with_capacity(added.len());
    for (n, root) in added.iter().enumerate() {
        claims.push(claim_registration(&format!("{}\u{1f}root{n}", existing.id), root).await);
    }
    let handles = state.registry.list_handles();
    let cold = state.cold_store.snapshot();
    for root in &added {
        if let Some(owner) = find_root_path_collision(&handles, &cold, root, Some(&existing.id)) {
            tracing::warn!(
                "add_roots[{}]: refusing {} — already covered by '{owner}' (#2336, #7434)",
                existing.id,
                root.display(),
            );
            return Err(root_path_collision_response(&owner, root));
        }
        match find_root_overlap(&handles, &cold, root, Some(&existing.id)) {
            Ok(None) => {}
            Ok(Some(conflict)) => return Err(root_overlap_response(&conflict, root)),
            Err(failure) => return Err(overlap_check_failed_response(&failure)),
        }
    }
    Ok((added, claims))
}

/// Run the add gate for `POST /indexes {roots}` against the index about to be
/// created (#7434).
///
/// What: an absent or empty `roots` costs nothing. Otherwise a bare handle at
/// the request's primary root stands in for the index, so the create path
/// runs exactly [`resolve_added_roots`].
/// Test: `create_index_accepts_additional_roots`,
/// `create_index_refuses_a_root_another_index_owns`.
pub(super) async fn admit_create_roots(
    state: &Arc<SearchAppState>,
    id: &IndexId,
    req: &super::router::CreateIndexRequest,
) -> Result<(Vec<PathBuf>, Vec<RegistrationClaim>), Refusal> {
    let requested = req.roots.as_deref().unwrap_or_default();
    if requested.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let indexer = crate::core::CodeIndexer::new(id.0.clone(), req.root_path.clone());
    let stand_in = IndexHandle::bare(
        id.clone(),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        req.root_path.clone(),
    );
    resolve_added_roots(state, &stand_in, requested).await
}

/// Refuse `POST /indexes {roots}` on an id that is already registered when it
/// names a root the index does not hold (#7434).
///
/// Why: the existing-id arm answers `200 created: false` and changes nothing;
/// a root silently dropped there reads as covered when it is not.
/// What: `409 roots_not_applied` naming the add route; `Ok` when `roots` is
/// absent or every entry is already in the table.
/// Test: `create_with_new_roots_on_an_existing_id_is_refused`.
pub(super) fn refuse_unapplied_roots(
    req: &super::router::CreateIndexRequest,
    registered: &IndexHandle,
) -> Result<(), Refusal> {
    let own = registered.roots();
    let missing: Vec<String> = req
        .roots
        .iter()
        .flatten()
        .filter(|r| !holds_root(&own, r))
        .map(|r| r.display().to_string())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err((
        StatusCode::CONFLICT,
        serde_json::json!({
            "error": "roots_not_applied",
            "index_id": registered.id.0,
            "roots": missing,
            "message": format!(
                "index '{}' is already registered and `roots` on POST /indexes is applied only \
                 at creation; add these with POST /indexes/{}/roots",
                registered.id, registered.id
            ),
        }),
    ))
}

/// `true` when `candidate` names a root already in `roots`.
fn holds_root(roots: &IndexRoots, candidate: &Path) -> bool {
    identifies_any_same_root(roots.primary(), roots.additional(), candidate)
}

/// The `409` for a candidate that nests with one of the index's own roots.
fn own_overlap_response(id: &IndexId, candidate: &Path, own: &Path) -> Refusal {
    (
        StatusCode::CONFLICT,
        serde_json::json!({
            "error": "index_root_overlap",
            "index_id": id.0,
            "requested_root_path": candidate.display().to_string(),
            "existing_root_path": own.display().to_string(),
            "message": format!(
                "{} nests with {}, which index '{id}' already covers; an index's roots must not \
                 overlap, or one file is stored under two keys (#7434)",
                candidate.display(),
                own.display(),
            ),
        }),
    )
}

/// The `409` while the per-index permit is held by other work.
fn busy(id: &str) -> Refusal {
    (
        StatusCode::CONFLICT,
        serde_json::json!({
            "error": "index_busy",
            "index_id": id,
            "retryable": true,
            "message": format!(
                "a relocate, config catch-up or embed pass holds index '{id}'; retry once it ends"
            ),
        }),
    )
}

/// Persist `table` as the index's additional roots, keeping every other
/// field of its `indexes.toml` record (the upsert overwrites the record).
///
/// What: an unreadable registry is a refusal, never a rebuilt default record —
/// that would overwrite `colocated`, the timestamps and the serve-only mark
/// with defaults. Only a readable registry with no row for this id (a
/// registration that never reached disk) is rebuilt from the handle. Both
/// failures answer `500 roots_not_persisted` and add nothing.
/// Test: `add_root_with_an_unreadable_registry_adds_nothing`;
/// the row decision by `an_unreadable_registry_never_yields_a_rebuilt_row`.
fn persist_roots(existing: &IndexHandle, table: Vec<PathBuf>) -> Result<(), Refusal> {
    let refuse = |e: &dyn std::fmt::Display| {
        tracing::error!(
            "add_roots[{}]: could not persist the root table: {e}",
            existing.id
        );
        let message = format!("could not persist the root table ({e}); no root was added");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": "roots_not_persisted", "index_id": existing.id.0, "message": message }),
        )
    };
    let loaded = crate::service::persistence::load_index_registry();
    let entry = roots_row(loaded, existing, table).map_err(|e| refuse(&e))?;
    crate::service::persistence::upsert_index_registry_entry(entry).map_err(|e| refuse(&e))
}

/// The `indexes.toml` row an add-root writes: the on-disk row with `table` as
/// its additional roots, or one rebuilt from the handle when the registry has
/// no row for this id. An unreadable registry is an error, never a rebuilt row.
///
/// Why: a separate function because the upsert re-reads the registry and
/// refuses a corrupt file on its own, so only a unit test over the load
/// result can show this decision fail (#7434 review).
/// Test: `an_unreadable_registry_never_yields_a_rebuilt_row`.
pub(super) fn roots_row(
    loaded: anyhow::Result<Vec<PersistedIndex>>,
    existing: &IndexHandle,
    table: Vec<PathBuf>,
) -> anyhow::Result<PersistedIndex> {
    let on_disk = loaded?.into_iter().find(|e| e.id == existing.id.0);
    let mut entry = on_disk.unwrap_or_else(|| PersistedIndex {
        id: existing.id.0.clone(),
        root_path: existing.root_path.clone(),
        serve_only: existing.serve_only,
        ..Default::default()
    });
    entry.additional_roots = table;
    Ok(entry)
}

/// The replacement handle: `existing` with `table` as its additional roots.
///
/// Every `Arc` is shared: the caller holds the reindex claim and the index
/// permit, so no run of the old table can write into this handle.
fn handle_with_roots(existing: &IndexHandle, table: Vec<PathBuf>) -> IndexHandle {
    IndexHandle {
        id: existing.id.clone(),
        indexer: Arc::clone(&existing.indexer),
        root_path: existing.root_path.clone(),
        additional_roots: table,
        include_paths: existing.include_paths.clone(),
        exclude_globs: existing.exclude_globs.clone(),
        extensions: existing.extensions.clone(),
        domain_terms: existing.domain_terms.clone(),
        include_docs: existing.include_docs,
        respect_gitignore: existing.respect_gitignore,
        follow_links: existing.follow_links,
        extra_skip_dirs: existing.extra_skip_dirs.clone(),
        data_file_max_bytes: existing.data_file_max_bytes,
        path_filter: existing.path_filter.clone(),
        context_embedding: Arc::clone(&existing.context_embedding),
        context_summary: Arc::clone(&existing.context_summary),
        indexed_head_sha: Arc::clone(&existing.indexed_head_sha),
        last_indexed_at: Arc::clone(&existing.last_indexed_at),
        lexical_only: existing.lexical_only,
        skip_kg: existing.skip_kg,
        skip_vector: existing.skip_vector,
        serve_only: existing.serve_only,
        defer_embed: existing.defer_embed,
        stages: Arc::clone(&existing.stages),
        search_pressure: Arc::clone(&existing.search_pressure),
        walk_diagnostics: Arc::clone(&existing.walk_diagnostics),
        embedding_pause: Arc::clone(&existing.embedding_pause),
        file_events: Arc::clone(&existing.file_events),
    }
}

/// The response body: the full root table (primary first), what this request
/// added, and the queued reindex's stream when one was queued.
fn roots_body(id: &str, roots: &IndexRoots, added: &[PathBuf], queued: bool) -> serde_json::Value {
    let show = |p: &PathBuf| p.display().to_string();
    serde_json::json!({
        "id": id,
        "root_path": roots.primary().display().to_string(),
        "roots": roots.all().iter().map(show).collect::<Vec<_>>(),
        "added": added.iter().map(show).collect::<Vec<_>>(),
        "reindex_queued": queued,
        "stream_url": queued.then(|| format!("/indexes/{id}/reindex/stream")),
    })
}
