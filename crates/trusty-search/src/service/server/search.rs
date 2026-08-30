//! Single-index search and delete handlers.
//!
//! Why: Groups the per-index search and delete paths together. Global fan-out
//! search lives in `search_global.rs` (extracted to keep both files under the
//! 500-line cap after issue #993 added cold-store lazy-load logic here).
//! What: `delete_index_handler`, `search_handler`. Routing helpers
//! (`RoutingMode`, `compute_context_weights`) and `search_similar_handler`
//! live in `routing.rs`. Global fan-out lives in `search_global.rs`.
//! Test: `search_handler_meta_includes_stale_index_root_field` and related.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::core::{classifier::QueryClassifier, indexer::SearchQuery, registry::IndexId};
use crate::service::lazy_loader::LAST_QUERIED_WRITE_INTERVAL_SECS;

use super::helpers::file_is_within_root;
use super::state::{DaemonEvent, SearchAppState};
use super::status::index_last_indexed;

// Re-export global fan-out handler so the router in `mod.rs` can reach it
// through the `search` path without knowing about `search_global`.
pub(super) use super::search_global::global_search_handler;

/// Query parameters for `DELETE /indexes/{id}` (issue #4123).
///
/// Why: before #4123 the handler hardcoded `delete_data=true`, so the HTTP
/// surface had NO way to deregister an index while keeping its on-disk data.
/// Registry hygiene (removing stale entries — issues #4094, #4095) therefore
/// could not be done through the API at all: a mis-typed id destroyed a real
/// corpus. An operator clearing 49 stale entries had to stop the daemon and
/// hand-edit `indexes.toml` instead. Two callers had ALREADY documented the
/// safe semantics that did not exist — the UI's confirm dialog ("On-disk data
/// is preserved.", `ui/src/lib/views/Indexes.svelte`) and `trusty-search index
/// remove`'s help ("The on-disk redb / HNSW snapshot is preserved") — so
/// `false` restores the contract the product already advertised.
/// What: a single optional `delete_data` flag, absent ⇒ `false` (deregister
/// only). Destroying data is now strictly opt-in via `?delete_data=true`. An
/// unparseable value (e.g. `?delete_data=maybe`) is rejected by axum's `Query`
/// extractor with `400` rather than silently defaulting either way — a
/// destructive toggle must never be guessed at.
/// Test: `delete_index_without_param_preserves_data`,
/// `delete_index_with_delete_data_true_destroys_data`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct DeleteIndexParams {
    /// Opt in to destroying the on-disk data directory alongside the
    /// registration. Defaults to `false` — see the type's `Why`.
    #[serde(default)]
    pub(crate) delete_data: bool,
    /// The `root_path` the caller believes this registration still has (#6380).
    ///
    /// Absent ⇒ unchecked, which is what every pre-#6380 caller sends. Present ⇒
    /// the delete is refused unless the registration's CURRENT root is this
    /// exact string — see [`super::delete_guard::refuse_unless_root_matches`].
    #[serde(default)]
    pub(crate) expected_root_path: Option<String>,
}

/// `DELETE /indexes/{id}` — deregister an index, optionally destroying its data.
///
/// Why: see [`DeleteIndexParams`]. BREAKING (issue #4123): a bare `DELETE` no
/// longer deletes on-disk data. Callers that genuinely want the disk reclaimed
/// must now pass `?delete_data=true`.
///
/// #6363: the response also carries the two verdicts the body could not express
/// before. An id that exists in NO store and no `indexes.toml` row answers
/// `404`, where it used to answer `200 {"removed": false}` — indistinguishable
/// from a delete that found the index and declined to remove it. And a delete
/// whose durable cleanup FAILED (the `indexes.toml` rewrite, or the data-dir
/// removal) answers `500` with `ok: false` and an `error` string, so no caller
/// records a removal that did not happen.
/// What: delegates to [`unregister_index`] — the same function the
/// orphan-reaper already drives with `delete_data=false` — and echoes
/// `data_deleted` so a caller can confirm which of the two semantics ran
/// instead of inferring it from the request it sent.
/// Test: `delete_index_without_param_preserves_data`,
/// `delete_index_with_delete_data_true_destroys_data`,
/// `delete_of_an_allowlist_excluded_registration_removes_the_row`,
/// `delete_of_an_id_in_no_store_and_no_registry_is_404`,
/// `a_failed_indexes_toml_rewrite_is_reported_not_swallowed`.
pub(super) async fn delete_index_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<DeleteIndexParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    delete_index_report(&state, &id, params)
        .await
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
}

/// The body `DELETE /indexes/{id}` serves, without the transport (#6285 slice 4).
///
/// Why: `search.index.delete` is the most destructive method on the socket, and
/// its two #6363 verdicts are exactly the ones a second implementation would get
/// wrong: an id recorded ONLY in `indexes.toml` (the #767 allowlist gate drops it
/// before either store sees it) still deletes, and a delete whose durable
/// cleanup failed answers `500 ok:false` rather than a `200` that records a
/// removal which did not happen. One core is what makes both true on the socket
/// by construction.
/// What: [`delete_index_handler`]'s whole former body, taking the already-parsed
/// [`DeleteIndexParams`] so the destructive `delete_data` toggle is decoded once
/// and never guessed at.
/// Test: `a_delete_of_an_id_in_no_store_is_not_found_on_either_transport`,
/// `an_allowlist_excluded_registration_deletes_over_the_socket_too`,
/// `a_failed_delete_reports_the_failure_and_keeps_the_row_on_either_transport`
/// in `crate::service::rpc::writes`.
pub(crate) async fn delete_index_report(
    state: &Arc<SearchAppState>,
    id: &str,
    params: DeleteIndexParams,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    // #6380: before anything is torn down. An id is derived from its root path,
    // so a path deleted and recreated between a census and this call names a
    // DIFFERENT, live index under the same id.
    if let Some(expected) = params.expected_root_path.as_deref() {
        super::delete_guard::refuse_unless_root_matches(state, id, expected)?;
    }
    let outcome = unregister_index(state, id, params.delete_data).await;
    // #6363: absent from the hot registry, the cold store AND `indexes.toml` —
    // there is nothing here to delete, and saying so is the only answer that
    // distinguishes a typo from a delete that failed.
    if !outcome.registered {
        return Err((
            StatusCode::NOT_FOUND,
            serde_json::json!({
                "id": id,
                "error": format!("unknown index: {id}"),
                "ok": false,
                "removed": false,
                "data_deleted": false,
                "quiesced": outcome.quiesced,
            }),
        ));
    }
    let mut body = serde_json::json!({
        "id": id,
        // #6363: `true` only when every durable step this delete attempted
        // succeeded. `removed` alone cannot carry that: a hot index whose
        // `indexes.toml` rewrite failed really was deregistered in memory, and
        // still comes back on the next warm boot.
        "ok": outcome.error.is_none(),
        "removed": outcome.removed,
        // #3049: reported from what the delete actually DID, not from the flag
        // the request sent. This used to read `removed && params.delete_data`,
        // so a `remove_index_data_dir` failure — downgraded to a `warn!` — still
        // answered `data_deleted: true` and the caller recorded the corpus as
        // reclaimed while every byte of it was still on disk.
        "data_deleted": outcome.data_deleted,
        // #3049: false when an in-flight writer never released the teardown lock
        // within `DELETE_QUIESCE_TIMEOUT`. Paired with `removed: false` it means
        // the delete was ABANDONED and nothing changed — retry it. Paired with
        // `removed: true` (only reachable without `?delete_data=true`) it means
        // the deregistration went ahead while a writer was still running.
        "quiesced": outcome.quiesced,
    });
    // #6363: a durable-cleanup failure is a FAILED delete, not a delete with a
    // footnote. Before this it was a `warn!` in the daemon log and a `200` on
    // the wire; the caller recorded the registration as gone while the row was
    // still in `indexes.toml`.
    if let Some(error) = outcome.error {
        body["error"] = serde_json::Value::String(error);
        return Err((StatusCode::INTERNAL_SERVER_ERROR, body));
    }
    Ok(body)
}

/// What a call to [`unregister_index`] actually did, as opposed to what its
/// caller asked for (issue #3049).
///
/// Why: the delete handler used to derive its `data_deleted` response field from
/// the REQUEST (`removed && params.delete_data`) while the on-disk removal was a
/// best-effort call whose failure was downgraded to a `warn!`. The two could
/// disagree, and when they did the API reported a destroyed corpus that was
/// still fully present — a failure that advances state and reports success. The
/// three fields here are the three independent facts a caller needs, so no
/// caller has to infer one from another again.
/// What: a plain value type. `removed` is registration removal (hot registry or
/// cold store), `data_deleted` is TRUE only when `remove_index_data_dir`
/// actually returned `Ok`, and `quiesced` is whether in-flight writers drained
/// before teardown.
/// Test: `service::server::tests_3049`, plus the existing
/// `delete_index_without_param_preserves_data` /
/// `delete_index_with_delete_data_true_destroys_data`.
pub(super) struct UnregisterOutcome {
    /// Whether a registration (hot or cold) was actually removed.
    pub(super) removed: bool,
    /// Whether the on-disk data directory was actually destroyed. Always
    /// `false` when `delete_data` was not requested.
    pub(super) data_deleted: bool,
    /// Whether every in-flight writer released this index's teardown lock before
    /// teardown ran. `false` means the wait timed out — and when `delete_data`
    /// was requested, that nothing at all was changed (`removed` is then `false`
    /// too). See [`unregister_index`].
    pub(super) quiesced: bool,
    /// Whether the id existed in ANY of the three places an index can be
    /// recorded: the hot registry, the cold store, or the `indexes.toml` row
    /// (#6363). `false` is the 404 verdict — nothing here to delete. Distinct
    /// from `removed`, which is whether this call actually removed something:
    /// a delete that found the index and then failed to rewrite the registry is
    /// `registered: true, removed: false`.
    pub(super) registered: bool,
    /// The durable-cleanup failure this delete hit, if any (#6363): the
    /// `indexes.toml` rewrite, the data-dir removal, or a registry file that
    /// could not be read at all. `Some` means the delete did not fully happen,
    /// so no caller may record it as done. Both failures used to be a `warn!`
    /// with no representation on the wire.
    pub(super) error: Option<String>,
}

/// How long `unregister_index` waits for in-flight writers to drain before it
/// gives up and tears down anyway (issue #3049).
///
/// Why: writers poll the cancel flag at batch boundaries, so the expected wait
/// is one batch, not one corpus — 30s is generous for that and still bounded, so
/// a stuck writer cannot wedge the HTTP handler indefinitely. The timeout is not
/// a licence to delete: on expiry a `delete_data` delete abandons itself and
/// changes nothing (see [`unregister_index`]).
/// Test: `delete_with_delete_data_refuses_removal_when_a_writer_never_quiesces`,
/// `a_second_delete_after_an_abandoned_one_reclaims_the_data`.
#[cfg(not(test))]
const DELETE_QUIESCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Test build uses a short deadline so the refusal path is exercised in
/// milliseconds. See the production value above for the real semantics.
#[cfg(test)]
const DELETE_QUIESCE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Fully unregister an index from the running daemon.
///
/// Why: shared by the interactive `DELETE /indexes/:id` handler and the
/// automatic orphan-reaper ticker (orphan self-heal). Both must drop the
/// in-memory registration, stop its filesystem watcher, and rewrite
/// `indexes.toml` + `roots.toml` so the index cannot resurrect on the next
/// warm-boot. The callers differ only in whether the on-disk *data* directory
/// is destroyed — the reaper passes `delete_data=false` so a false-positive
/// orphan detection can never delete real index data, and since issue #4123
/// the HTTP handler passes the request's `?delete_data` flag (default `false`,
/// i.e. the same safe semantics the reaper has always used).
/// What: atomically removes the handle from the registry (capturing its
/// `root_path` in the same `remove` so a concurrent PATCH cannot make it stale
/// — issue #1090/#1097), stops the watcher (issue #1621), deletes the registry
/// entry (issue #118), optionally removes the data dir (issue #85), scrubs the
/// root from `roots.toml` so the colocated rescan cannot rediscover it (issue
/// #1090), emits `IndexRemoved`, and re-syncs the index-count gauge (issue #41).
/// Returns whether an index was actually removed.
///
/// #5075: also purges this id's cold-store records (`entries`,
/// `failed_entries`). Without that the id stayed "registered but not resident"
/// to the #5057 guards forever, so `GET /indexes/:id/status`, `/chunks`, and
/// `grep` answered 503 for an index that no longer exists — and a
/// cold-parked-only index was never removed from `indexes.toml` at all, so the
/// next warm boot resurrected it. A cold record now counts as "an index was
/// removed" for the durable-cleanup half below.
/// #6363: also handles the id that is in `indexes.toml` and in NEITHER store.
/// The #767 allowlist gate drops an unapproved root at warm boot
/// (`retain_approved_entries`, `commands/start/restore.rs`) before it reaches
/// `registry` or `cold_store`, so those rows were invisible to both existence
/// checks above: the durable-cleanup branch never ran, and the handler answered
/// `200 {"removed": false}` with the row and the data dir untouched. One host
/// accumulated 60 such rows, each of them keeping `warm_boot_degraded` true on
/// every boot and clearable only by hand-editing `indexes.toml`. A registry-only
/// id is now removed like any other, and the durable failures — the rewrite, the
/// data-dir removal, an unreadable registry — are REPORTED through
/// `UnregisterOutcome::error` instead of being logged and answered as success.
///
/// #3049: the delete QUIESCES the index before tearing it down. It signals this
/// id's cancel flag, then waits up to [`DELETE_QUIESCE_TIMEOUT`] for the
/// EXCLUSIVE side of `index_teardown_lock`, and holds it across teardown. Before
/// this, a delete landing mid-write removed the registration and
/// `remove_dir_all`'d the data directory while a live writer was still writing
/// into it; recreating the same id immediately then let a new-epoch task
/// interleave with the old one on identical on-disk paths.
///
/// On timeout the behaviour splits on `delete_data` (round 3):
/// - `delete_data=true` — the whole delete is ABANDONED and nothing is changed,
///   so re-issuing it once the writer stops actually works. Round 2 refused only
///   the `remove_dir_all` while still removing the registration, which stranded
///   the data directory forever: the retry it told operators to make hits
///   `removed=false` and never reaches the removal branch, and
///   `spawn_orphan_reaper_ticker` reaps registrations whose root_path vanished,
///   never data directories with no registration.
/// - `delete_data=false` — deregistration proceeds. No data was going to be
///   removed, so no orphan is possible, and the orphan reaper (which runs in
///   exactly this mode) must stay able to drop a dead registration.
///
/// Residual: `embed_deferred_chunks` still has no interior cancel checkpoint, so
/// a delete landing during a deferred-embed pass waits for the whole pass and
/// abandons itself if that outlasts the timeout. That is now a clean retryable
/// failure rather than a silent orphan; see the PR discussion on #3049.
///
/// Every writer must hold the SHARED side for the span of its write. The
/// complete set, and why each entry is or is not guarded, is the table in
/// `service::reindex::semaphore`'s [`INDEX_TEARDOWN_LOCKS`] docs — add a new
/// write path there and here in the same change, or this guard silently stops
/// covering it. Round 1 of this fix waited on `index_semaphore` instead and
/// missed six paths that hold no permit.
/// Test: delete-path handler tests; reaper behaviour covered by `orphan_reaper`
/// unit tests plus the `spawn_orphan_reaper_ticker` wiring; the quiesce and
/// refusal behaviour by `service::server::tests_3049`; the registry-only,
/// unknown-id and failed-cleanup arms by `service::server::tests_6363`.
pub(super) async fn unregister_index(
    state: &Arc<SearchAppState>,
    id: &str,
    delete_data: bool,
) -> UnregisterOutcome {
    let index_id = IndexId::new(id.to_string());
    // #3049: signal BEFORE waiting so a writer that is between batches stops at
    // its next checkpoint instead of running the whole corpus to completion.
    crate::service::reindex::signal_index_cancel(&index_id);
    // #3049: take the EXCLUSIVE side of this index's teardown lock. Every write
    // path holds its SHARED side for the span of its write, so acquiring it means
    // no writer of any kind is in flight. Round 1 of this fix waited on
    // `index_semaphore` instead, which covers only the three long-running
    // writers — `index_file_handler`, `remove_file_handler`, `ingest_graph_handler`,
    // the watch loop, boot reconcile, and relocate hold no permit, so that wait
    // returned instantly and reported a false `quiesced: true`. Held across teardown.
    let quiesce_permit = tokio::time::timeout(
        DELETE_QUIESCE_TIMEOUT,
        crate::service::reindex::acquire_index_teardown_write(&index_id),
    )
    .await
    .ok();
    let quiesced = quiesce_permit.is_some();
    if !quiesced && delete_data {
        // #3049 round 3: ABANDON the delete, changing nothing. Round 2 refused
        // only the `remove_dir_all` and tore the registration down anyway, which
        // orphaned the data directory permanently: a re-issued delete finds
        // `remove_and_get` returning `removed=false` and never reaches the
        // data-removal branch at all, and no reaper covers a data directory with
        // no registration. Leaving the registration in place is what makes the
        // "re-issue the delete" instruction below TRUE.
        tracing::error!(
            "delete[{id}]: ABANDONED — in-flight work did not drain within {:?}, so \
             nothing was changed (registration, indexes.toml and on-disk data are all \
             intact). Re-issue the delete once the writer has stopped (issue #3049).",
            DELETE_QUIESCE_TIMEOUT,
        );
        // The cancel we signalled above must not outlive the delete we just gave
        // up on, or the surviving index's next reindex aborts at its first
        // checkpoint. Clears the VALUE on the flag in-flight writers already hold
        // — evicting the map entry would leave those readers seeing `true`.
        crate::service::reindex::clear_index_cancel(&index_id);
        return UnregisterOutcome {
            removed: false,
            data_deleted: false,
            quiesced: false,
            // #6363: an id with a writer holding its teardown lock exists by
            // construction, and the #3049 contract for this branch is "nothing
            // changed — re-issue the delete". Reporting it as unknown (404)
            // would tell the caller to stop retrying the one thing that works.
            registered: true,
            // Not an `error`: `quiesced: false` with `removed: false` already IS
            // the #3049 verdict for this branch, and it is retryable rather than
            // a failed cleanup.
            error: None,
        };
    }
    if !quiesced {
        // `delete_data=false` cannot orphan anything — preserving the data IS the
        // requested semantics (issue #4123), and this is the mode the orphan
        // reaper runs in, which must still be able to deregister an index whose
        // root_path has vanished. Deregistration proceeds as before.
        tracing::warn!(
            "delete[{id}]: in-flight work did not drain within {:?} — deregistering \
             anyway. No data was going to be removed (delete_data=false), so there is \
             nothing to refuse (issue #3049).",
            DELETE_QUIESCE_TIMEOUT,
        );
    }
    let (removed_hot, removed_handle) = state.registry.remove_and_get(&index_id);
    let root_path_for_cleanup = removed_handle.map(|h| h.root_path.clone());
    // #5075: drop the cold-store records too, or the #5057 guards answer 503
    // forever for an id that is now absent from every store. Sampled BEFORE the
    // purge because a cold-parked or restore-failed index is not in the hot
    // registry — `removed` is false for exactly the ids this is meant to reap,
    // and the durable cleanup below must still run for them.
    let was_cold = state.cold_store.contains(&index_id) || state.cold_store.is_failed(&index_id);
    let cold_root = state
        .cold_store
        .get_persisted(&index_id)
        .map(|e| e.root_path);
    state.cold_store.purge(&index_id);
    let root_path_for_cleanup = root_path_for_cleanup.or(cold_root);
    let in_memory_removed = removed_hot || was_cold;
    // #6363: `indexes.toml` is the THIRD place an index can be recorded, and the
    // only one the #767 allowlist gate leaves an entry in — `retain_approved_entries`
    // drops an unapproved root before it reaches either store, so a warm boot
    // produces exactly this shape. Consulting it here is what turns those rows
    // from undeletable (200 `removed:false`, row and data dir intact) into an
    // ordinary delete, and what lets a genuinely unknown id 404 instead.
    let mut errors: Vec<String> = Vec::new();
    let registry_entry = if in_memory_removed {
        // Already proven to exist; skip the file read on the hot path.
        None
    } else {
        match crate::service::persistence::find_index_registry_entry(id) {
            Ok(entry) => entry,
            Err(e) => {
                // An unreadable registry is not proof of absence (#4317/#4871),
                // so this must never become a 404. Report it as the failure it
                // is and let the caller retry once the file is readable.
                tracing::error!(
                    "delete[{id}]: could not read indexes.toml to check for a \
                     registration-only row ({e:#}) — refusing to report this id as \
                     unknown (issue #6363)"
                );
                errors.push(format!("indexes.toml unreadable: {e:#}"));
                None
            }
        }
    };
    let registry_only = registry_entry.is_some();
    let root_path_for_cleanup = root_path_for_cleanup.or(registry_entry.map(|e| e.root_path));
    // A registry read that failed leaves existence UNKNOWN; treating it as
    // "registered" keeps the answer a reportable failure rather than a 404.
    let registered = in_memory_removed || registry_only || !errors.is_empty();
    state.reindex_progress.remove(&index_id);
    state.watcher_manager.stop_for_index(&index_id).await;
    let mut data_deleted = false;
    let mut removed = in_memory_removed;
    if in_memory_removed || registry_only {
        match crate::service::persistence::remove_index_registry_entry(id) {
            // #6363: for a registry-only id this rewrite IS the removal — there
            // was no in-memory registration to drop — so `removed` becomes true
            // here rather than being decided before the durable work ran.
            Ok(()) => removed = true,
            Err(e) => {
                // #6363: was a `warn!` that nothing on the wire reflected. A
                // registry-only delete whose rewrite fails removed NOTHING, so
                // `removed` stays false; a hot delete really did drop the
                // in-memory registration, so it stays true — and both report the
                // failure through `error`.
                tracing::error!(
                    "delete[{id}]: could not remove the indexes.toml row ({e:#}) — the \
                     registration survives a restart; reporting the delete as FAILED \
                     rather than logging and answering 200 (issue #6363)"
                );
                errors.push(format!("indexes.toml rewrite failed: {e:#}"));
            }
        }
        if delete_data {
            // Reaching here means the quiesce wait SUCCEEDED — a `delete_data`
            // delete that timed out returned above without touching anything, so
            // no `remove_dir_all` can run under an active writer.
            debug_assert!(quiesced, "delete_data teardown runs only when quiesced");
            match crate::service::persistence::remove_index_data_dir(id) {
                // #3049: this is the only assignment of `data_deleted` —
                // the response field can no longer disagree with the disk.
                Ok(()) => data_deleted = true,
                Err(e) => {
                    tracing::error!(
                        "delete[{id}]: on-disk data removal FAILED ({e}) — reporting \
                         data_deleted=false so the caller does not record this corpus \
                         as reclaimed (issue #3049)"
                    );
                    // #6363: `data_deleted: false` says the bytes are still
                    // there but not that anything went WRONG — a bare DELETE
                    // reports the same value on its success path. Carry the
                    // failure itself so the response can be a 500.
                    errors.push(format!("data-dir removal failed: {e:#}"));
                }
            }
        }
        if let Some(ref root) = root_path_for_cleanup {
            if let Err(e) = crate::service::roots_registry::remove_root(root) {
                tracing::warn!(
                    "could not remove '{id}' root {} from roots.toml: {e} \
                     (warm-boot may rediscover this index — issue #1090)",
                    root.display()
                );
            } else {
                tracing::debug!(
                    "delete[{id}]: removed root {} from roots.toml",
                    root.display()
                );
            }
        }
        // Push event so connected dashboards drop the row without refresh.
        state.emit(DaemonEvent::IndexRemoved { id: id.to_string() });
        // Keep the index-count gauge in sync.
        crate::service::metrics::set_index_count(state.registry.list().len());
    }
    // #3049 round 4: evicting the IDENTITY primitives is conditional on
    // `quiesced`, and happens WHILE STILL HOLDING the exclusive guard.
    //
    // Eviction is what hands a racing caller — a recreate, or a second delete —
    // a fresh, uncontended primitive for this id. That is correct only once no
    // writer of the previous generation is left running, which is exactly what
    // `quiesced` means. Round 3 evicted unconditionally, so the
    // `!quiesced && delete_data=false` branch above (the DEFAULT endpoint
    // behaviour, reached whenever a writer outlasts the 30s wait) handed the next
    // caller a lock disconnected from the still-running writer: a later
    // `?delete_data=true` then found no contention, reported `quiesced: true`,
    // and removed the directory that writer was writing into. Not evicting leaves
    // at most one map entry per id behind until the writer stops and a delete
    // succeeds — a bounded leak, against silent corruption.
    if quiesced {
        // Issue #2984 Phase 1 delta-review MEDIUM finding: `INDEX_LOCKS` (the
        // per-index reindex/catch-up mutual-exclusion semaphore registry) never
        // shrinks on its own. Safe here even if the id was never registered
        // (no-op) — see `remove_index_semaphore`'s doc comment.
        crate::service::reindex::remove_index_semaphore(&index_id);
        // A writer already parked on the evicted teardown lock re-validates and
        // retries against the fresh one — see
        // `reindex::semaphore::acquire_index_teardown_read`.
        crate::service::reindex::remove_index_teardown_lock(&index_id);
    }
    // The cancel flag is evicted on BOTH paths, and the asymmetry is deliberate.
    // Reaching here at all means the deregistration went through, so the flag's
    // `true` must keep reaching the in-flight writer — telling a writer of an
    // index that no longer exists to stop is the outcome we want, and it already
    // holds its own `Arc`, which eviction does not touch. What eviction adds is
    // that an index recreated under this id gets a fresh `false` flag instead of
    // being born cancelled. Contrast the abandon branch above, which
    // `clear_index_cancel`s the VALUE: there the index stays registered, so the
    // surviving writer must be told to CONTINUE.
    crate::service::reindex::remove_index_cancel_flag(&index_id);
    drop(quiesce_permit);
    UnregisterOutcome {
        removed,
        data_deleted,
        quiesced,
        registered,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

pub(super) async fn search_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(query): Json<SearchQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    search_report(&state, &id, query)
        .await
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
}

/// The body `POST /indexes/{id}/search` serves, without the transport
/// (#6285 slice 3).
///
/// Why: `search.query` answers the same question over the socket, and this is
/// the daemon's most-read body — the lazy cold-load, the facet routing (#5069),
/// the lane down-shift, the five drop counters and the whole `meta` block. Two
/// implementations of it would diverge on the first change to any of them.
/// What: [`search_handler`]'s whole former body. A refusal keeps its HTTP status
/// beside its body, because that status is what
/// [`crate::service::rpc::error::rpc_error_from_http`] turns into the JSON-RPC
/// code — so `vector_unavailable`'s `retryable` split reaches a socket caller as
/// the retryable/permanent code pair rather than one 503 class.
/// Test: `search_over_the_socket_matches_the_http_body`,
/// `an_unknown_index_reports_not_found_on_every_query_method`,
/// `an_empty_query_reports_invalid_params_on_both_transports`.
pub(crate) async fn search_report(
    state: &Arc<SearchAppState>,
    id: &str,
    mut query: SearchQuery,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    // Issue #882: reject empty / whitespace-only queries before touching the
    // index. An empty query falls through to a pure k-NN vector search that
    // returns arbitrary top-k results — not useful and potentially expensive.
    if query.text.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "query must not be empty" }),
        ));
    }
    let index_id = IndexId::new(id.to_string());
    // Issue #993: hot registry first, then the cold-store lazy load. #5349 moved
    // that flow into `index_resolve` so the write endpoints drive the identical
    // load instead of 503-ing against the same daemon state.
    let handle = super::index_resolve::resolve_or_load_index(state, &index_id)
        .await
        .map_err(|(status, body)| (status, body.0))?;
    // #4087: a registered index whose durable corpus failed to open holds no
    // chunks at all, so every search against it returned HTTP 200 with
    // `results: []` — a total outage presented as "no matches". Fail loudly
    // instead, carrying the #4333 classification so the caller knows whether
    // to retry (transient) or escalate (permanent). Placed before the watcher
    // wake and the `last_queried` write so a broken index neither spawns a
    // watcher (which #4122 quarantines anyway) nor records a phantom query.
    if let Some((status, body)) =
        super::degraded::corpus_failure_response(&index_id.0, &handle).await
    {
        return Err((status, body.0));
    }
    // Idle-watcher wake: an index served without a live watcher was either
    // idle-suspended (`server::tickers::spawn_watcher_idle_suspend_ticker`) or
    // lazily restored from the cold store (which never spawns a watcher). Resume
    // watching so future saves index incrementally, and — only when we actually
    // (re)establish a watcher — kick a background reconcile to catch edits made
    // while it was unwatched. The `is_watching` re-check keeps this a no-op when
    // `TRUSTY_DISABLE_WATCHER=1` (spawn is a no-op), so we never spin a reconcile
    // on every query in that mode. The query itself served current in-memory
    // state; the reconcile converges any missed edits for subsequent queries.
    if !state.watcher_manager.is_watching(&index_id).await {
        state.watcher_manager.spawn_for_index(&handle).await;
        if state.watcher_manager.is_watching(&index_id).await {
            let woken = Arc::clone(&handle);
            let summary = Arc::clone(&state.reconcile_summary);
            tokio::spawn(crate::service::reconcile::reconcile_one_index(
                woken, summary,
            ));
        }
    }
    // Issue #993: rate-limited write of last_queried_unix (max once per
    // LAST_QUERIED_WRITE_INTERVAL_SECS) so the LRU sort key stays current for
    // future selective warm-boots without hammering indexes.toml on every query.
    //
    // PR #1103 PERF: the previous code called `persistence::read_last_queried_unix`
    // here, which opens + parses indexes.toml synchronously on the async handler
    // for EVERY query to a warm index. Replace with the in-memory
    // `last_queried_write_cache` DashMap so the hot path does zero disk I/O.
    // The background write task below updates the map after a successful write so
    // the rate-limit semantics are fully preserved.
    {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Consult the in-memory cache — no disk read.
        let stale = state
            .last_queried_write_cache
            .get(&index_id)
            .map(|prev| now_unix.saturating_sub(*prev) >= LAST_QUERIED_WRITE_INTERVAL_SECS)
            .unwrap_or(true); // absent = never written for this session → write now
        if stale {
            let id_str = index_id.0.clone();
            // Update the in-memory cache immediately so concurrent queries within
            // the same interval don't all race to spawn a write task.
            state
                .last_queried_write_cache
                .insert(index_id.clone(), now_unix);
            // Review MEDIUM: `spawn_blocking`, not `spawn`. This body takes the
            // process-wide registry `std::sync::Mutex` (#4871) and then does
            // synchronous file I/O — parking an async worker thread on a lock
            // another blocking writer holds. On the blocking pool that is what
            // the pool is for; on a worker it starves the runtime.
            tokio::task::spawn_blocking(move || {
                if let Err(e) =
                    crate::service::persistence::update_last_queried_unix(&id_str, now_unix)
                {
                    tracing::debug!("last_queried_unix update failed for '{id_str}': {e}");
                }
            });
        }
    }
    // Use the same domain-aware classifier as `CodeIndexer::search` so the
    // intent reported back to the caller matches what was used for routing.
    let intent = QueryClassifier::classify_with_domain(&query.text, &handle.domain_terms);
    // Issue #109 Phase 1: derive lane availability from the staged-pipeline
    // status surface. The search handler MUST consult `search_capabilities`
    // (NOT the legacy top-level `status` field) when deciding whether the
    // semantic / KG lanes are queryable. The indexer's `search` honours
    // `query.stage = Some(Lexical)`, so we down-shift the query to lexical
    // when either (a) the caller explicitly asked for it, or (b) the
    // semantic stage is not yet ready. Doing this here keeps the indexer
    // unaware of the index-handle-level capability surface.
    let mut handle = handle;
    let mut index_id = index_id;
    let mut stages_snapshot = { handle.stages.read().await.clone() };
    // #5069: names the index the caller asked for when a sibling facet answered.
    let mut routed_from: Option<String> = None;
    // #5068: a caller that PINNED the semantic lane asked a question this index
    // cannot answer. Serving BM25 rows under a 200 answers a different question
    // silently, so refuse with the same shape `search_kg` uses for `skip_kg`.
    if query.stage == Some(crate::core::indexer::SearchStage::Semantic) {
        if let Some((refusal_status, refusal_body)) = super::degraded::vector_lane_unavailable(
            &index_id.0,
            handle.skip_vector,
            &stages_snapshot,
        ) {
            // #5069: worktree indexes are `skip_vector` (#5060), so run the
            // caller's declared lane against the facet of the same repo that
            // holds the vectors before refusing outright.
            let Some((served_id, served_handle)) =
                super::facet_route::resolve_semantic_facet(state, &index_id).await
            else {
                return Err((refusal_status, refusal_body.0));
            };
            tracing::info!(
                requested = %index_id,
                served_by = %served_id,
                "search_handler: routed a declared semantic lane to the repo's vector-carrying facet"
            );
            routed_from = Some(index_id.0.clone());
            stages_snapshot = served_handle.stages.read().await.clone();
            index_id = served_id;
            handle = served_handle;
        }
    }
    let caps = stages_snapshot.search_capabilities();
    let semantic_ready = caps.contains(&"vector");
    if query.stage.is_none() && !semantic_ready {
        // Force lexical lane until the embedder catches up. The caller's
        // request is preserved if they explicitly asked for `mode = all`
        // / similar; we only override the lane selector, not the file-type
        // filter.
        query.stage = Some(crate::core::indexer::SearchStage::Lexical);
    }
    // Issue #109 Phase 1 backpressure stub: ping the per-index pressure
    // notifier so the background Stage-2 task briefly yields. The notifier
    // is a hint — the embedder loop waits at most 100 ms.
    handle.search_pressure.notify_one();
    let started = std::time::Instant::now();
    let indexer = handle.indexer.read().await;
    // #2203: `search_with_drops`, not `search` — the tally is what lets a
    // caller tell "3 results because 3 matched" from "3 results because 7 were
    // dropped". Published as `meta.dropped` below.
    let (mut results, mut dropped) = indexer.search_with_drops(&query).await.map_err(|e| {
        // #5917: a durable-corpus read failure is not an internal error, and a
        // body naming neither the index nor the fault is what let this state
        // read as "no matches" for a whole daemon lifetime. Report it as the
        // outage it is, under the same code the chunk endpoints already use.
        // The downcast matters here and not in the grep/call_chain wrapper:
        // `search_with_drops` also fails for reasons that have nothing to do
        // with the corpus (an embed call, the vector store), and those stay 500.
        if let Some((status, body)) = super::degraded::corpus_read_failure_from(&e) {
            tracing::warn!(
                index_id = %index_id,
                error = %e,
                "search: the durable corpus could not be read"
            );
            return (status, body.0);
        }
        tracing::warn!(index_id = %index_id, error = %e, "search failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({
                "error": "internal search error",
                "index_id": index_id.0,
                "message": format!("{e:#}"),
            }),
        )
    })?;
    // Issue #64: defense-in-depth post-filter. Chunks are stored with `file`
    // paths relative to the index root, so anything that escapes the root
    // (absolute path pointing elsewhere, `..` traversal, or simply a path
    // that's also absolute and outside `root_path`) is a sign of stale data
    // from a previously-misregistered index (see #63) or a bug elsewhere in
    // the pipeline. Drop those rows rather than returning cross-project
    // results to the caller. `file_is_within_root` uses a cheap lexical
    // check first; only absolute-path results that fail the fast path pay the
    // `canonicalize` syscall cost (issue #541 approach b).
    let root = handle.root_path.clone();
    let before = results.len();
    results.retain(|r| file_is_within_root(&r.file, &root));
    let filtered_out = before.saturating_sub(results.len());
    // #2203: the fifth drop site. `stale_index_root` already flagged it as a
    // boolean; the count joins the other four so `meta.dropped` accounts for
    // every row the pipeline removed.
    dropped.out_of_root = filtered_out;
    // Issue #3683 code-critic review finding 3 (HIGH): read the degraded
    // flag while the indexer read-guard is still held (it's about to be
    // dropped below) so the response reflects THIS query's lane state, not
    // a state that could shift between the search call and the response
    // being built. `true` here means the lexical (BM25/grep-fallback) lane
    // above may have returned fewer/no results because the detached
    // rehydrate hadn't converged yet — an `Ok(vec![])`/short result set is
    // otherwise indistinguishable from a genuine empty match. Mirrors how
    // `WarmBootSummary.warm_boot_degraded` surfaces warm-boot degradation
    // on `/health`, just scoped to a single query instead of a boot.
    let bm25_lane_degraded = indexer.lane_degraded();
    if filtered_out > 0 {
        // Issue #541: increment the process-wide Prometheus counter so operators
        // can alert on a rising drop rate without log scraping.
        metrics::counter!(
            "trusty_search_dropped_out_of_root_total",
            "index_id" => index_id.0.clone(),
        )
        .increment(filtered_out as u64);
        tracing::warn!(
            index_id = %index_id,
            root = %root.display(),
            dropped = filtered_out,
            "search_handler: dropped {} result(s) whose file path falls outside index root {} \
             — index root is stale (symlink rename or daemon restart without \
             re-canonicalization). Re-register to fix: `trusty-search index {}`",
            filtered_out,
            root.display(),
            root.display(),
        );
    }
    drop(indexer);

    let latency_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        index_id = %index_id,
        intent = %format!("{intent:?}"),
        latency_ms = latency_ms,
        results = results.len(),
        query = %crate::truncate_at_char_boundary(&query.text, 80),
        "search"
    );

    // Issue #75: surface index freshness in the response `meta` block so
    // callers can show staleness banners without a follow-up status call.
    //
    // `last_indexed` is the mtime of `chunks.json` (rewritten on every
    // successful commit) and matches what `GET /indexes/:id/status`
    // already returns.
    //
    // `results_may_be_stale` compares the current git HEAD SHA against the
    // SHA captured at index-registration time. False whenever either SHA
    // is unavailable (non-git directory, missing git binary) or the SHAs
    // match — i.e. defaults to "not stale" rather than scaring callers
    // about indexes whose freshness we cannot verify.
    // #4706: mtime only — the byte count was discarded here, and computing it
    // cost a recursive walk of both storage directories on every query.
    let last_indexed = index_last_indexed(&index_id.0, &handle.root_path);
    let indexed_sha = handle.indexed_head_sha.read().await.clone();
    let current_sha = crate::core::git::head_sha(&handle.root_path);
    let results_may_be_stale = match (indexed_sha.as_deref(), current_sha.as_deref()) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };
    let mut body = serde_json::json!({
        "results": results,
        "intent": format!("{:?}", intent),
        "latency_ms": latency_ms,
        "meta": {
            "last_indexed": last_indexed,
            "results_may_be_stale": results_may_be_stale,
            // Issue #109 Phase 1: surface which lanes contributed to this
            // result set. Lets clients display "lexical-only" badges or
            // retry once the semantic lane is ready.
            "search_capabilities": caps,
            // Issue #541: machine-readable signal that results were dropped
            // because the index root is stale. Clients (Claude Code, UI) can
            // show a remediation banner without log scraping. `false` is the
            // normal case (no drops); `true` means the operator should run
            // `trusty-search index <path>` to re-register with a fresh root.
            "stale_index_root": filtered_out > 0,
            // #2203: how many candidates this query retrieved and then
            // discarded, per drop site. Without it a short `results` array was
            // indistinguishable from a small match set — `top_k: 10` returning
            // 3 rows read the same whether 3 matched or 7 were deleted on the
            // way out, which is how #2203 presented as "search is broken" on an
            // intact corpus. `unresolved_corpus` is the fault case (the corpus
            // could not be read); the rest are the requested `mode` /
            // `exclude_archived` / root scope doing their job.
            "dropped": dropped,
            "dropped_total": dropped.total(),
            // Issue #3683 code-critic review finding 3 (HIGH): `true` means
            // this query's lexical lane (BM25 and/or grep-fallback) degraded
            // to empty/partial because the detached corpus rehydrate hadn't
            // converged within its bounded wait — NOT a genuine "no match"
            // result. Clients should treat results under a `true` flag as
            // provisional and may retry shortly; the background rehydrate
            // keeps running regardless and the next query is warm.
            "bm25_lane_degraded": bm25_lane_degraded,
            // #5068: the counterpart flag to `bm25_lane_degraded` for the OTHER
            // lane. `true` means this hybrid query ran with no vector
            // contribution at all — the results are lexical, however
            // conceptual the query was. A pinned `stage: semantic` gets a 503
            // instead (see `degraded::vector_lane_unavailable`); this flag is
            // for the unpinned caller, who legitimately gets whatever lanes are
            // ready but must be able to tell which ones those were without
            // diffing `search_capabilities` against a schema it does not have.
            "vector_unavailable": !semantic_ready,
            // #5068: separates "off for this index" from "not built yet" — the
            // same split `vector_unavailable`'s 503 body carries, so a caller
            // handles one contract, not two.
            "vector_disabled_by_config": handle.skip_vector,
        },
    });
    // #5069: a routed query's results belong to the SERVING facet's tree, which
    // sits on a different commit than the worktree the caller asked about — so
    // name both indexes and the root the paths are relative to. Added only when
    // routing actually happened, so an unrouted response is byte-identical to
    // what it was before.
    if let Some(from) = routed_from {
        if let Some(meta) = body.get_mut("meta").and_then(|m| m.as_object_mut()) {
            meta.insert("routed_from_index".into(), serde_json::json!(from));
            meta.insert("served_by_index".into(), serde_json::json!(index_id.0));
            meta.insert(
                "served_root_path".into(),
                serde_json::json!(handle.root_path.display().to_string()),
            );
        }
    }
    Ok(body)
}
