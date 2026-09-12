//! `POST /indexes/:id/roots` — add index roots to a live index (#7434).
//!
//! Why: #7434 slice 1 taught the walk, the corpus-path encoding and every
//! guard to handle an index that spans several trees, but nothing could put a
//! second tree on an index: `additional_roots` was only ever written by warm
//! boot, from a record no API could produce. This module is the mutation —
//! the one door through which a root joins an index, and therefore the one
//! place the root-collision rule has to be re-asked before it does.
//!
//! What: [`add_index_roots_report`], the transport-neutral core behind
//! [`add_index_roots_handler`], plus [`resolve_added_roots`] — the
//! canonicalise / no-op / dedupe / collision pipeline that `POST /indexes`
//! also runs so a root supplied at creation passes exactly the same gate as
//! one added later. The core is written in the #6285 slice-4 shape (a
//! `(StatusCode, Value)` pair per refusal) so `service::rpc::writes` can
//! register it as a socket method without a second implementation.
//!
//! Ordering is the load-bearing part, and it differs from
//! [`super::indexes_relocate`] in two ways that are deliberate:
//!
//! 1. This handler holds the per-index MUTUAL-EXCLUSION permit
//!    (`reindex::index_semaphore`) — the one `run_reindex` holds for its whole
//!    run — not just the teardown lock's read side. A reindex walking the old
//!    root table while this call swaps in a new one would prune the corpus
//!    against a root set neither of them agreed on. Read, append, persist and
//!    swap all happen under that single permit, so two concurrent adds
//!    serialise and the second one appends to the first one's result instead
//!    of overwriting it from a stale snapshot.
//! 2. The replacement handle does NOT `Arc::clone` `stages`,
//!    `walk_diagnostics` or `last_indexed_at`. See
//!    [`add_index_roots_report`] for what sharing them lets a stale reindex
//!    claim.
//!
//! Test: `add_root_refuses_another_indexes_primary_root`,
//! `add_root_refuses_another_indexes_additional_root`,
//! `add_root_refuses_cold_entrys_root`, `add_root_of_own_root_is_idempotent`,
//! `add_root_dedupes_within_request`, `concurrent_add_roots_both_survive`,
//! `add_root_during_reindex_does_not_mark_new_root_ready` in
//! `tests_7434_roots.rs`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

use crate::core::registry::{IndexHandle, IndexId};
use crate::service::persistence::PersistedIndex;
use crate::service::reindex::{spawn_reindex_with_cleanup, ReindexProgress};

use super::helpers::{
    find_root_path_collision, identifies_any_same_root, identifies_same_root,
    root_path_collision_response, validate_root_path,
};
use super::state::{DaemonEvent, SearchAppState};

/// Request body for `POST /indexes/:id/roots` (#7434).
///
/// Why: adding a root is an APPEND to an ordered table whose positions are
/// baked into every `@root<n>/…` corpus path, so the wire shape deliberately
/// offers no way to reorder or remove — a caller that could rewrite the list
/// would invalidate stored paths it cannot see.
/// What: one `roots` array of absolute directory paths. Each is canonicalised
/// and validated independently; the whole request is refused if any one of
/// them collides with another index (see [`resolve_added_roots`]).
/// Test: `add_root_dedupes_within_request` in `tests_7434_roots.rs`.
#[derive(Deserialize)]
pub(crate) struct AddRootsRequest {
    /// Absolute directory paths to add to this index's root table.
    pub roots: Vec<PathBuf>,
}

/// `POST /indexes/:id/roots` — axum wrapper over [`add_index_roots_report`].
pub(super) async fn add_index_roots_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<AddRootsRequest>,
) -> Response {
    match add_index_roots_report(&state, &id, req).await {
        Ok(body) => Json(body).into_response(),
        Err((status, body)) => (status, Json(body)).into_response(),
    }
}

/// Validate, de-duplicate and admit a set of candidate index roots.
///
/// Why: two doors add a root — this module's endpoint and `POST /indexes`'s
/// `roots` field — and the rule they have to enforce is the #2336/#3993
/// one-index-per-tree rule, hardened three times over. A second spelling of
/// it in the create handler is how the two drift, so the create path calls
/// this function rather than re-deriving the checks.
/// What: for each candidate, in request order — `validate_root_path`
/// (absolute, exists, denylist, #767 allowlist, canonicalised), then three
/// skips and one refusal:
///
/// | Candidate | Outcome |
/// |---|---|
/// | equals `primary_root`, or already in `existing` | skipped — idempotent no-op |
/// | equals an earlier candidate in this same request | skipped — deduped |
/// | owned by any OTHER live handle or cold entry | `409`, whole request refused |
/// | anything else | appended |
///
/// The comparison is `identifies_same_root`'s `(dev, ino)` identity, not
/// string equality, so a case-variant spelling on APFS is caught the same way
/// #2519 taught the collision guard to catch it. Returns only the NEWLY added
/// roots so the caller can both report them and append them in order —
/// positions in the table are corpus-path ordinals and only ever grow.
/// Test: `add_root_of_own_root_is_idempotent`, `add_root_dedupes_within_request`,
/// `add_root_refuses_another_indexes_primary_root`,
/// `add_root_refuses_another_indexes_additional_root`,
/// `add_root_refuses_cold_entrys_root` in `tests_7434_roots.rs`.
pub(super) async fn resolve_added_roots(
    state: &Arc<SearchAppState>,
    index_id: &IndexId,
    primary_root: &std::path::Path,
    existing: &[PathBuf],
    requested: &[PathBuf],
) -> Result<Vec<PathBuf>, (StatusCode, serde_json::Value)> {
    // #5827's reasoning applies here too: `list_handles` Arc-clones every live
    // handle and `snapshot` deep-clones every cold record, so a request that
    // names no roots — every ordinary `POST /indexes` — must not pay for them.
    if requested.is_empty() {
        return Ok(Vec::new());
    }
    let handles = state.registry.list_handles();
    let cold_entries = state.cold_store.snapshot();

    let mut added: Vec<PathBuf> = Vec::new();
    for candidate in requested {
        let canonical = validate_root_path(candidate, false, &state.allowlist_paths).await?;
        // The index's own primary root, or a root it already covers: adding it
        // changes nothing, so say so rather than growing the table with a
        // duplicate ordinal that would encode the same file two ways.
        if identifies_any_same_root(primary_root, existing, &canonical) {
            continue;
        }
        if added.iter().any(|r| identifies_same_root(r, &canonical)) {
            continue;
        }
        // #2336/#3993/#7434: any-of-N, over live handles AND cold entries.
        // `exclude_id` is this index, whose own roots the two skips above
        // already answered.
        if let Some(owner) =
            find_root_path_collision(&handles, &cold_entries, &canonical, Some(index_id))
        {
            tracing::warn!(
                "add_roots[{}]: refusing {} — that tree is already covered by '{}' \
                 (issues #2336, #3993, #7434)",
                index_id.0,
                canonical.display(),
                owner,
            );
            return Err(root_path_collision_response(&owner, &canonical));
        }
        added.push(canonical);
    }
    Ok(added)
}

/// The body `POST /indexes/{id}/roots` serves, without the transport.
///
/// Why: an index's root table is read by the reindex walk, the prune pass and
/// the corpus-path encoder, and those three must never disagree about it. The
/// whole read-append-persist-swap sequence therefore runs under the per-index
/// mutual-exclusion permit `run_reindex` holds for its entire run, which is
/// stronger than the teardown read lock `relocate_index_report` takes: two
/// adds cannot interleave, and an add cannot land mid-walk.
///
/// What, in order: acquire the per-index permit, then the teardown read guard
/// (the same order `run_reindex` takes them, so the pair cannot deadlock);
/// read the handle THROUGH the registry inside the critical section, so a
/// second caller sees the first one's appended list rather than a snapshot
/// taken before it ran; validate the candidates ([`resolve_added_roots`]);
/// persist the registry entry; then swap the handle and queue the walk. A
/// persist failure refuses with `500` and swaps nothing — unlike relocate,
/// which only warns, because a root the daemon forgets on restart would leave
/// `@root<n>/…` chunks in the corpus with no table to decode them.
///
/// Three fields are deliberately NOT `Arc::clone`d onto the replacement
/// handle. `spawn_reindex_with_cleanup` captures its `Arc<IndexHandle>` at
/// spawn time, so a run queued before this call still walks the OLD root
/// table; sharing `stages`, `walk_diagnostics` or `last_indexed_at` would let
/// that run write its completion into the handle that now advertises a root it
/// never saw — an index reporting `Ready` over a tree nothing walked. Fresh
/// `Arc`s seeded with the CURRENT values break that link while keeping what is
/// still true: the primary root's corpus is as built and as searchable as it
/// was a moment ago. Resetting them to `Pending` instead would revoke every
/// entry in `search_capabilities` (which requires `lexical.is_ready()`) for the
/// whole duration of the added root's walk — an outage on an index that only
/// gained coverage.
///
/// The walk is TRIGGERED, not scheduled: until it runs the index covers fewer
/// trees than its own table names, and nothing else in the daemon would ever
/// close that gap (the per-root watcher is a later slice, and boot reconcile
/// runs only at boot). It goes on the BACKGROUND lane — the caller already has
/// its answer, and a bulk walk must not consume the two interactive permits an
/// operator's own reindex needs (#458) — and does not force, so the hash cache
/// spares every file already indexed under the primary root.
///
/// Test: `add_root_during_reindex_does_not_mark_new_root_ready`,
/// `concurrent_add_roots_both_survive` in `tests_7434_roots.rs`.
pub(crate) async fn add_index_roots_report(
    state: &Arc<SearchAppState>,
    id: &str,
    req: AddRootsRequest,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    // #7434: the FULL per-index permit, in `run_reindex`'s own acquisition
    // order (mutual exclusion, then teardown-read) so the two cannot deadlock.
    let _index_permit = crate::service::reindex::index_semaphore(&index_id)
        .acquire_owned()
        .await
        .expect("per-index semaphore is never closed — one fresh Semaphore per IndexId");
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(&index_id).await;

    // Read INSIDE the critical section: a snapshot taken before the permit
    // would be stale by exactly the append a concurrent caller just made.
    let existing = state.registry.get(&index_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("unknown index: {id}") }),
        )
    })?;

    let added = resolve_added_roots(
        state,
        &index_id,
        &existing.root_path,
        &existing.additional_roots,
        &req.roots,
    )
    .await?;

    if added.is_empty() {
        return Ok(roots_body(
            &existing,
            &existing.additional_roots,
            &added,
            false,
        ));
    }

    let mut roots = existing.additional_roots.clone();
    roots.extend(added.iter().cloned());

    // Persist BEFORE the swap so a daemon that dies between the two comes back
    // covering the new root rather than holding an undecodable corpus.
    let on_disk = crate::service::persistence::load_index_registry()
        .ok()
        .and_then(|entries| entries.into_iter().find(|e| e.id == id));
    let entry = persisted_with_roots(&existing, on_disk, roots.clone());
    if let Err(e) = crate::service::persistence::upsert_index_registry_entry(entry) {
        tracing::error!("add_roots[{id}]: refusing — could not persist the root table: {e}");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({
                "error": format!(
                    "could not persist the new root table to indexes.toml ({e}); no root \
                     was added, because a root the daemon forgets on restart leaves \
                     chunks in the corpus it can no longer resolve"
                ),
            }),
        ));
    }

    // Deliberately NOT written to `roots.toml`: that file is the startup
    // scanner's DISCOVERY list, so a tree listed there is a candidate for its
    // own index. An additional root belongs to this index alone.
    let new_handle = handle_with_roots(&existing, roots.clone()).await;

    // #4951's rule: the indexer resolves stored paths against its own mirror of
    // the table, so it has to move with the handle, and the write lock is held
    // across `register` so no reader sees the two disagree.
    let indexer_for_guard = Arc::clone(&existing.indexer);
    let mut indexer_guard = indexer_for_guard.write().await;
    indexer_guard.set_additional_roots(roots.clone());
    let registered = state.registry.register(new_handle);
    drop(indexer_guard);

    state.emit(DaemonEvent::IndexRegistered { id: id.to_string() });
    tracing::info!(
        "add_roots[{id}]: added {} root(s); the index now covers {} tree(s)",
        added.len(),
        roots.len() + 1,
    );

    // #7434: start watching the added root in this request, not at the next
    // registration event. The walk below closes the coverage gap once; without
    // a watch, every edit made in the new tree afterwards is invisible until
    // someone reindexes, which is the same silent staleness this slice exists
    // to remove. The resync is per root and idempotent, so the existing roots'
    // watches are untouched. A failure to spawn is RECORDED against that root
    // (`GET /indexes/:id/status` → `watcher.roots`) and does not fail the add:
    // the root is in the table, the walk is queued, and the index is strictly
    // better covered than before — refusing here would undo a persisted append.
    state.watcher_manager.spawn_for_index(&registered).await;

    let progress = Arc::new(ReindexProgress::new());
    state
        .reindex_progress
        .insert(index_id.clone(), Arc::clone(&progress));
    spawn_reindex_with_cleanup(
        registered,
        progress,
        false,
        Some(Arc::clone(&state.reindex_progress)),
        Some(Arc::clone(&state.last_reindex_aborted_at)),
        Some(Arc::clone(&state.embedderd_pid_slot)),
        // Background lane — see this function's doc comment.
        false,
        None,
    );

    Ok(roots_body(&existing, &roots, &added, true))
}

/// Build the replacement handle carrying `roots`.
///
/// Why: separated from [`add_index_roots_report`] only so the three
/// deliberately-fresh `Arc`s sit together where a reader can see them as one
/// decision rather than three lines lost in a 30-field literal.
/// What: every field carried from `existing`, except `additional_roots` (the
/// new table) and the three progress fields, which are fresh `Arc`s seeded
/// with the current values.
/// Test: `add_root_during_reindex_does_not_mark_new_root_ready`.
async fn handle_with_roots(existing: &IndexHandle, roots: Vec<PathBuf>) -> IndexHandle {
    let stages = existing.stages.read().await.clone();
    let diagnostics = existing.walk_diagnostics.read().await.clone();
    let last_indexed_at = existing.last_indexed_at.read().await.clone();
    IndexHandle {
        // #7434: the whole point of this handler.
        additional_roots: roots,
        id: existing.id.clone(),
        indexer: Arc::clone(&existing.indexer),
        root_path: existing.root_path.clone(),
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
        lexical_only: existing.lexical_only,
        skip_kg: existing.skip_kg,
        skip_vector: existing.skip_vector,
        defer_embed: existing.defer_embed,
        search_pressure: Arc::clone(&existing.search_pressure),
        // #6524: an operator's pause and the event feed survive a rebuild.
        embedding_pause: Arc::clone(&existing.embedding_pause),
        file_events: Arc::clone(&existing.file_events),
        // #7434: fresh, not `Arc::clone`d — a reindex queued before this add
        // still holds the OLD handle and would otherwise write its completion
        // into the table it never walked.
        stages: Arc::new(tokio::sync::RwLock::new(stages)),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(diagnostics)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(last_indexed_at)),
    }
}

/// The registry entry to persist, carrying `roots`.
///
/// Why: `upsert_index_registry_entry` overwrites the WHOLE record, so an entry
/// assembled from the handle alone would silently drop the fields only
/// `indexes.toml` holds — `colocated` (#1088/#1089, a flip wipes central-store
/// data), the LRU timestamps (#993), the HEAD stamp (#4391) and the deferred
/// pass marker (#4390). Reading the on-disk record and changing one field is
/// what keeps every one of them.
/// What: the on-disk entry with `additional_roots` replaced; when there is no
/// entry (a registration that never reached disk) it falls back to the handle's
/// own configuration with the create-time `colocated: true`.
/// Test: covered by `add_root_dedupes_within_request`, which reads the
/// persisted table back.
fn persisted_with_roots(
    existing: &IndexHandle,
    on_disk: Option<PersistedIndex>,
    roots: Vec<PathBuf>,
) -> PersistedIndex {
    let mut entry = on_disk.unwrap_or_else(|| PersistedIndex {
        id: existing.id.0.clone(),
        root_path: existing.root_path.clone(),
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
        follow_links: existing.follow_links,
        extra_skip_dirs: existing.extra_skip_dirs.clone(),
        data_file_max_bytes: Some(existing.data_file_max_bytes),
        lexical_only: existing.lexical_only,
        skip_kg: existing.skip_kg,
        skip_vector: existing.skip_vector,
        defer_embed: existing.defer_embed,
        colocated: true,
        ..Default::default()
    });
    entry.additional_roots = roots;
    entry
}

/// The response body: the index's FULL root table, primary first.
///
/// Why: a caller that just added a root needs to see the table it landed in —
/// the ordinals are what every `@root<n>/…` corpus path encodes, and an
/// idempotent no-op is indistinguishable from a successful add without it.
/// What: `roots` is the whole table (primary at index 0); `added` names only
/// what this request appended, which is empty for a no-op.
/// Test: `add_root_of_own_root_is_idempotent`.
fn roots_body(
    existing: &IndexHandle,
    additional: &[PathBuf],
    added: &[PathBuf],
    reindex_queued: bool,
) -> serde_json::Value {
    let all: Vec<String> = std::iter::once(existing.root_path.display().to_string())
        .chain(additional.iter().map(|p| p.display().to_string()))
        .collect();
    serde_json::json!({
        "id": existing.id.0,
        "root_path": existing.root_path.display().to_string(),
        "roots": all,
        "added": added
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        "reindex_queued": reindex_queued,
    })
}
