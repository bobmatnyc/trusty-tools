//! Atomic corpus swap: open/seed/commit/abort for staged reindex (#28, #603).
//!
//! Why: every reindex (force or incremental) stages its rebuilt corpus in a
//! sibling `index.redb.tmp` and atomically renames it into place only on
//! success (#603). Before this change, any reindex failure destroyed the only
//! searchable corpus. Keeping the swap plumbing in one file makes the
//! safety-critical rename path easy to audit.
//!
//! What:
//! - `staging_corpus_path` — the one place the staging path is resolved (#3979).
//! - `begin_staged_corpus_swap` — open a fresh staging store and optionally
//!   seed it from the live corpus for an incremental reindex (#839), or ADOPT an
//!   interrupted run's staging store when resuming (#3979).
//! - `commit_staged_corpus_swap` — rename staging → live on success.
//! - `abort_staged_corpus_swap` — delete staging and re-open live on failure.
//!
//! Test: `incremental_reindex_no_durable_data_loss` and
//! `incremental_reindex_carryover_failure_aborts` in `service::reindex::tests`;
//! the adopt path in `service::reindex::resume_tests`.

use crate::core::registry::{IndexHandle, IndexId};
use anyhow::Context;
use std::path::{Path, PathBuf};

use super::checkpoint::{ReindexCheckpoint, ResumeState};

/// Resolve the staging (`index.redb.tmp`) path for this index, or `None` when
/// it cannot be resolved.
///
/// Why (#3979): `begin_staged_corpus_swap` used to compute this inline, but the
/// resume probe has to look at the SAME file before staging begins. Two copies
/// of the colocated-vs-legacy routing would be a silent correctness hazard — the
/// probe could inspect one file while the swap wrote another — so both callers
/// share this one function.
/// What: returns the colocated `.trusty-search/index.redb.tmp` when the root has
/// colocated storage (#403), otherwise the daemon-global per-index staging path.
/// A resolution failure is logged at `warn` and returns `None`, which puts the
/// caller on the direct-write-to-live fallback exactly as before.
/// Test: `super::resume_tests::interrupted_reindex_resumes_to_identical_index`
/// depends on the probe and the swap agreeing on one path.
pub(super) fn staging_corpus_path(handle: &IndexHandle, index_id: &IndexId) -> Option<PathBuf> {
    let resolved = if crate::service::colocated_storage::has_colocated_storage(&handle.root_path) {
        crate::service::colocated_storage::colocated_redb_tmp_path(&handle.root_path)
    } else {
        crate::service::persistence::corpus_redb_tmp_path(&index_id.0)
    };
    match resolved {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(
                "staged corpus swap: cannot resolve staging corpus path for '{}' ({e}) — \
                 reindex will write directly to the live corpus",
                index_id.0
            );
            None
        }
    }
}

/// Begin the atomic-swap corpus staging for a reindex (issue #28, Phase 4;
/// durable-data-loss fix issue #839).
///
/// Why: every reindex (force or incremental) stages its rebuilt corpus in a
/// sibling `index.redb.tmp` and atomically renames it into place only on
/// success (#603). For a NON-force incremental reindex, before any batch
/// writes, copy every row from the LIVE corpus into the fresh staging store
/// so that hash-skipped (unchanged) files survive the atomic rename.
///
/// What: when the index has a durable corpus store, opens a fresh
/// `index.redb.tmp`, conditionally seeds it from the live corpus (when
/// `!force`), and swaps it onto the indexer. Returns `Ok(Some(path))` on
/// success; `Ok(None)` when staging is skipped (BM25-only / unresolvable
/// temp path); `Err(e)` when the live-corpus carryover copy failed for an
/// incremental reindex — caller MUST abort the reindex immediately.
///
/// Issue #3979: `resume` is `Some` only when [`super::checkpoint::probe_resume`]
/// already proved an existing staging corpus adoptable. In that case this
/// function ADOPTS that file (plain `open`, no `open_fresh`, no carryover copy —
/// the carryover is already inside it from the interrupted run) instead of
/// recreating it. When `resume` is `None` the behaviour is byte-for-byte the
/// pre-#3979 path, plus a checkpoint record stamped into the fresh staging
/// corpus so a future interruption is itself resumable.
/// Test: `incremental_reindex_no_durable_data_loss`,
/// `incremental_reindex_carryover_failure_aborts`, and
/// `super::resume_tests::interrupted_reindex_resumes_to_identical_index`.
pub(super) async fn begin_staged_corpus_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    force: bool,
    resume: Option<&ResumeState>,
    checkpoint: Option<&ReindexCheckpoint>,
) -> Result<Option<PathBuf>, anyhow::Error> {
    // #3979: adopt the already-validated staging corpus rather than deleting it.
    if let Some(state) = resume {
        return adopt_staged_corpus(handle, index_id, state).await;
    }
    // Quick read-lock probe: nothing to stage if no durable corpus.
    // Also capture the live corpus Arc for the incremental copy path (#839).
    let live_corpus = {
        let indexer = handle.indexer.read().await;
        if !indexer.has_corpus_store() {
            return Ok(None);
        }
        // For incremental reindexes we need the live corpus to copy its rows
        // into the fresh staging store.
        if !force {
            indexer.corpus_store()
        } else {
            None
        }
    };
    // Whether this is an incremental (carryover) reindex — tracked so the
    // error path can distinguish a copy failure from a staging-open failure.
    let is_incremental_carryover = live_corpus.is_some();
    // Issue #403: route tmp corpus path to colocated or legacy storage
    // (extracted to `staging_corpus_path` for #3979 — the resume probe must
    // resolve the identical path).
    let Some(tmp_path) = staging_corpus_path(handle, index_id) else {
        return Ok(None);
    };
    // Open the staging store on a blocking worker (redb's API is sync), then
    // seed it from the live corpus when performing an incremental reindex.
    //
    // IMPORTANT: if the carryover copy fails we propagate the error upward so
    // the caller can abort the reindex entirely.
    let tmp_for_open = tmp_path.clone();
    let index_id_str = index_id.0.clone();
    let staged_result = tokio::task::spawn_blocking(move || {
        let store = crate::core::corpus::CorpusStore::open_fresh(&tmp_for_open)?;
        if let Some(live) = live_corpus {
            // Copy all durable rows (chunks + entities + file_hashes + _meta)
            // from the live corpus into the fresh staging store.
            store.copy_all_from(&live).with_context(|| {
                format!(
                    "reindex[{index_id_str}]: failed to seed staging corpus from live corpus — \
                     aborting incremental reindex to preserve live corpus integrity"
                )
            })?;
        }
        Ok::<_, anyhow::Error>(store)
    })
    .await;
    let staged = match staged_result {
        Ok(Ok(store)) => store,
        Ok(Err(e)) => {
            if is_incremental_carryover {
                // Carryover copy failed: propagate so the caller aborts the reindex.
                tracing::error!(
                    "reindex[{}]: ABORTING — could not copy live corpus into staging store ({e}); \
                     live corpus remains intact",
                    index_id.0
                );
                // Best-effort removal of the orphaned staging tmp.
                if let Err(rm_err) = std::fs::remove_file(&tmp_path) {
                    tracing::warn!(
                        path = %tmp_path.display(),
                        error = %rm_err,
                        "reindex[{}]: could not remove orphaned staging tmp after \
                         carryover copy failure — stale file may remain until next \
                         daemon restart (issue #845)",
                        index_id.0
                    );
                }
                return Err(e);
            }
            // For a force reindex (no carryover), failure to open/populate staging
            // is non-fatal: fall through to direct-write mode.
            tracing::warn!(
                "staged corpus swap: could not open staging corpus for '{}' ({e}) — \
                 reindex will write directly to the live corpus",
                index_id.0
            );
            return Ok(None);
        }
        Err(e) => {
            tracing::warn!(
                "staged corpus swap: staging corpus open task panicked for '{}': {e}",
                index_id.0
            );
            return Ok(None);
        }
    };
    // Swap the staging store onto the indexer. The prior live store's `Arc` is
    // dropped here; reads during the reindex are served from the in-memory
    // `chunks` HashMap, so dropping the durable handle does not affect search.
    let mut indexer = handle.indexer.write().await;
    let _prev = indexer.swap_corpus_store(std::sync::Arc::new(staged));
    drop(indexer);
    tracing::info!(
        "staged corpus swap: staging corpus opened for '{}' at {}",
        index_id.0,
        tmp_path.display()
    );
    // #3979: stamp the fresh staging corpus so an interruption from here on is
    // resumable. Written AFTER the carryover copy so the record and the rows it
    // describes land in the same file; `copy_all_from` deliberately does not
    // carry this key over, so it can only ever describe THIS run.
    if let Some(cp) = checkpoint {
        write_checkpoint(handle, index_id, cp).await;
    }
    Ok(Some(tmp_path))
}

/// Adopt an existing, already-validated staging corpus for a resumed reindex
/// (issue #3979).
///
/// Why: the whole point of resume. `begin_staged_corpus_swap`'s normal path
/// calls `CorpusStore::open_fresh`, which DELETES the file at the staging path
/// before opening it — that single call is what discarded every committed batch
/// after a crash. Adoption opens the same file without deleting it and without
/// re-seeding from live, because the interrupted run already copied the live
/// rows in (#839) and then added its own committed batches on top.
///
/// What: opens the staging corpus (serialized against concurrent openers,
/// #3659), installs it on the indexer via `swap_corpus_store` — the same sink
/// the fresh path uses, deliberately NOT `set_corpus_store`, so the #4122 write
/// quarantine is neither lifted nor bypassed — and then drops the in-memory
/// chunk/BM25/entity caches. That last step is required for correctness, not
/// memory: those caches were rehydrated at warm boot from the LIVE corpus, so
/// for any file the interrupted run had already re-indexed they hold the OLD
/// chunks while the adopted corpus holds the NEW ones. Clearing them makes every
/// downstream reader (search lanes, the KG rebuild, `remove_file_no_kg_rebuild`)
/// lazily rehydrate from the adopted corpus through the existing
/// `ensure_corpus_rehydrated` guards, so the served state matches the corpus
/// that will be promoted.
///
/// Returns `Ok(Some(tmp_path))` on success. An open failure is NOT fatal: it
/// returns `Ok(None)` so the caller proceeds without staging (writing to the
/// live corpus, the pre-existing fallback) rather than aborting the reindex.
/// Test: `super::resume_tests::interrupted_reindex_resumes_to_identical_index`.
async fn adopt_staged_corpus(
    handle: &IndexHandle,
    index_id: &IndexId,
    state: &ResumeState,
) -> Result<Option<PathBuf>, anyhow::Error> {
    let tmp_path = state.tmp_path.clone();
    let open_path = tmp_path.clone();
    let opened = crate::core::corpus::open_serialized(&tmp_path, move || {
        crate::core::corpus::CorpusStore::open(&open_path)
            .with_context(|| format!("adopt staging corpus {}", open_path.display()))
    })
    .await;
    let staged = match opened {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "reindex[{}]: could not adopt the staging corpus at {} ({e}) — \
                 continuing without staging (issue #3979)",
                index_id.0,
                tmp_path.display(),
            );
            return Ok(None);
        }
    };
    {
        let mut indexer = handle.indexer.write().await;
        let _prev = indexer.swap_corpus_store(std::sync::Arc::new(staged));
    }
    // Drop caches built from the LIVE corpus — see the doc comment above.
    let reclaimed = handle.indexer.read().await.reclaim_memory_now().await;
    tracing::info!(
        "reindex[{}]: adopted staging corpus {} ({} staged chunk(s)); dropped {} \
         in-memory cache entr(ies) built from the pre-crash live corpus so reads \
         rehydrate from the adopted corpus (issue #3979)",
        index_id.0,
        tmp_path.display(),
        state.staged_chunks,
        reclaimed,
    );
    Ok(Some(tmp_path))
}

/// Persist `checkpoint` into the currently-installed (staging) corpus (#3979).
///
/// Why: the record has to live in the same redb file as the partial chunks so a
/// crash cannot leave the two disagreeing — redb's ACID commit means the blob is
/// either fully there or not there at all, which is strictly stronger than a
/// write-then-rename sidecar.
/// What: serializes to JSON and calls `write_reindex_checkpoint_sync` on a
/// blocking worker. A failure is logged at `warn` and ignored — the only
/// consequence is that this run's staging corpus is not adoptable, i.e. exactly
/// the pre-#3979 behaviour.
/// Test: `super::resume_tests::interrupted_reindex_resumes_to_identical_index`.
async fn write_checkpoint(
    handle: &IndexHandle,
    index_id: &IndexId,
    checkpoint: &ReindexCheckpoint,
) {
    let corpus = {
        let indexer = handle.indexer.read().await;
        indexer.corpus_store()
    };
    let Some(corpus) = corpus else {
        return;
    };
    let bytes = match serde_json::to_vec(checkpoint) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                "reindex[{}]: could not serialize the resume checkpoint ({e}) — \
                 an interruption will fall back to a full reindex (issue #3979)",
                index_id.0
            );
            return;
        }
    };
    let res =
        tokio::task::spawn_blocking(move || corpus.write_reindex_checkpoint_sync(&bytes)).await;
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(
            "reindex[{}]: could not write the resume checkpoint ({e}) — an \
             interruption will fall back to a full reindex (issue #3979)",
            index_id.0
        ),
        Err(e) => tracing::warn!(
            "reindex[{}]: resume-checkpoint write task panicked ({e}) — an \
             interruption will fall back to a full reindex (issue #3979)",
            index_id.0
        ),
    }
}

/// Clear the resume checkpoint from the staging corpus just before it is
/// promoted (issue #3979).
///
/// Why: promotion is a RENAME, so whatever sits in the staging corpus's `_meta`
/// table becomes live metadata. A completed corpus carrying an "in progress"
/// marker would be a corpus lying about itself, and the next run's probe reads
/// only `*.tmp` paths so it would never be cleaned up.
/// What: best-effort `clear_reindex_checkpoint_sync` on a blocking worker.
/// Failures are logged at `debug` and ignored — a stale record in a promoted
/// live corpus is inert (the probe never reads the live corpus, and
/// `copy_all_from` never copies this key into a staging corpus).
/// Test: `super::resume_tests::completed_reindex_leaves_no_checkpoint`.
pub(super) async fn clear_checkpoint_before_promote(handle: &IndexHandle, index_id: &IndexId) {
    let corpus = {
        let indexer = handle.indexer.read().await;
        indexer.corpus_store()
    };
    let Some(corpus) = corpus else {
        return;
    };
    let res = tokio::task::spawn_blocking(move || corpus.clear_reindex_checkpoint_sync()).await;
    if !matches!(res, Ok(Ok(()))) {
        tracing::debug!(
            "reindex[{}]: could not clear the resume checkpoint before promoting the \
             staging corpus — the stale record is inert (issue #3979)",
            index_id.0
        );
    }
}

/// Finalize (commit) the atomic corpus swap after a successful reindex
/// (issue #28, Phase 4).
///
/// Why: once the reindex has committed every batch to `index.redb.tmp`, the
/// temp file holds the complete rebuilt corpus. Renaming it over the live
/// `index.redb` makes the swap atomic.
/// What: takes the staging store out of the indexer, drops its last `Arc`
/// (redb keeps the file mapped while any handle is alive, so the handle MUST
/// be dropped before the rename), renames `index.redb.tmp` → `index.redb`,
/// re-opens a `CorpusStore` on the swapped-in file, and installs it on the
/// indexer. Any failure leaves the previous live corpus in place and logs at
/// `warn` — a botched swap must not crash the daemon.
/// Test: `incremental_reindex_no_durable_data_loss` verifies the promoted
/// corpus contains all chunks (changed + unchanged).
pub(super) async fn commit_staged_corpus_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    tmp_path: &Path,
) {
    // Issue #403: route live corpus path to colocated or legacy storage.
    let live_path = if crate::service::colocated_storage::has_colocated_storage(&handle.root_path) {
        match crate::service::colocated_storage::colocated_redb_path(&handle.root_path) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "force reindex: cannot resolve colocated live corpus path for '{}' ({e}) — \
                     staged corpus left at {}",
                    index_id.0,
                    tmp_path.display()
                );
                return;
            }
        }
    } else {
        match crate::service::persistence::corpus_redb_path(&index_id.0) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "force reindex: cannot resolve live corpus path for '{}' ({e}) — \
                     staged corpus left at {}",
                    index_id.0,
                    tmp_path.display()
                );
                return;
            }
        }
    };
    // #3979: the promote is a rename, so the staging corpus's `_meta` becomes
    // live metadata — clear the "reindex in progress" record first. Must run
    // BEFORE the store handle is dropped below, since it needs the open store.
    clear_checkpoint_before_promote(handle, index_id).await;
    // Drop the staging store's last Arc so redb releases the temp file before
    // the rename.
    {
        let mut indexer = handle.indexer.write().await;
        let _ = indexer.take_corpus_store();
    }
    let tmp = tmp_path.to_path_buf();
    let live = live_path.clone();
    let index_id_inner = index_id.0.clone();
    // Rename on a blocking worker (filesystem sync call).
    let rename_result: anyhow::Result<()> = {
        let tmp = tmp.clone();
        let live = live.clone();
        let index_id_for_msg = index_id_inner.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::rename(&tmp, &live).with_context(|| {
                format!(
                    "atomic-swap rename {} -> {} for '{index_id_for_msg}'",
                    tmp.display(),
                    live.display()
                )
            })
        })
        .await
        .unwrap_or_else(|join_err| {
            Err(anyhow::anyhow!(
                "atomic-swap rename task panicked for '{index_id_inner}': {join_err}"
            ))
        })
    };
    // Re-open the swapped-in live corpus, serialized + panic-safe (issue
    // #3659) against any other concurrent opener of this SAME path (e.g. a
    // lazy-load or another handler racing this reindex's swap) — this used
    // to call `CorpusStore::open` directly on a blocking worker with no
    // serialization against the persistence_loader path.
    let reopened: anyhow::Result<crate::core::corpus::CorpusStore> = match rename_result {
        Ok(()) => {
            let live_for_open = live.clone();
            let index_id_for_open = index_id.0.clone();
            crate::core::corpus::open_serialized(&live, move || {
                crate::core::corpus::CorpusStore::open(&live_for_open)
                    .with_context(|| format!("re-open swapped corpus for '{index_id_for_open}'"))
            })
            .await
        }
        Err(e) => Err(e),
    };
    match reopened {
        Ok(store) => {
            handle
                .indexer
                .write()
                .await
                .set_corpus_store(std::sync::Arc::new(store));
            tracing::info!(
                "force reindex: atomically swapped rebuilt corpus into {} for '{}'",
                live_path.display(),
                index_id.0
            );
        }
        Err(e) => tracing::warn!(
            "force reindex: atomic corpus swap failed for '{}' ({e}) — \
             previous corpus preserved; in-memory state is the rebuilt one",
            index_id.0
        ),
    }
}

/// Discard the staging corpus after an aborted / failed reindex
/// (issue #28, Phase 4).
///
/// Why: if the reindex aborts (memory limit) or fails, the partially-written
/// `index.redb.tmp` must not survive — leaving multi-GB stale temp files
/// on disk between reindexes wastes space. The live `index.redb` is untouched
/// by an aborted reindex, so reverting just means deleting the temp and
/// re-opening the original live store.
/// What: takes the staging store out of the indexer, drops its `Arc`, deletes
/// `index.redb.tmp`, then re-opens and re-installs the live `index.redb` store
/// so the indexer's durable corpus points back at the untouched original.
/// Test: `incremental_reindex_carryover_failure_aborts`.
pub(super) async fn abort_staged_corpus_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    tmp_path: &Path,
) {
    {
        let mut indexer = handle.indexer.write().await;
        let _ = indexer.take_corpus_store();
    }
    // Issue #403: route live corpus path to colocated or legacy storage.
    let live_path = if crate::service::colocated_storage::has_colocated_storage(&handle.root_path) {
        crate::service::colocated_storage::colocated_redb_path(&handle.root_path)
    } else {
        crate::service::persistence::corpus_redb_path(&index_id.0)
    };
    let tmp = tmp_path.to_path_buf();
    let index_id_inner = index_id.0.clone();

    // Delete the staging tmp on a blocking worker (filesystem sync call).
    {
        let tmp = tmp.clone();
        let index_id_for_msg = index_id_inner.clone();
        let removed = tokio::task::spawn_blocking(move || match std::fs::remove_file(&tmp) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        })
        .await;
        match removed {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(
                "force reindex: could not delete staging corpus {} for '{index_id_for_msg}': {e}",
                tmp_path.display()
            ),
            Err(join_err) => tracing::warn!(
                "force reindex: staging-corpus delete task panicked for '{index_id_for_msg}': \
                 {join_err}"
            ),
        }
    }

    // Re-open the live corpus, serialized + panic-safe (issue #3659) against
    // any other concurrent opener of this SAME path — this used to call
    // `CorpusStore::open` directly on a blocking worker with no
    // serialization against the persistence_loader path.
    let restored: anyhow::Result<Option<crate::core::corpus::CorpusStore>> = match live_path {
        Ok(live) => {
            let live_for_open = live.clone();
            let index_id_for_open = index_id_inner.clone();
            crate::core::corpus::open_serialized(&live, move || {
                crate::core::corpus::CorpusStore::open(&live_for_open).with_context(|| {
                    format!("re-open live corpus for '{index_id_for_open}' after abort")
                })
            })
            .await
            .map(Some)
        }
        Err(e) => {
            tracing::warn!(
                "force reindex: cannot resolve live corpus path for '{index_id_inner}' \
                 ({e}) — index left without a durable corpus until next restart"
            );
            Ok(None)
        }
    };
    match restored {
        Ok(Some(store)) => {
            handle
                .indexer
                .write()
                .await
                .set_corpus_store(std::sync::Arc::new(store));
            tracing::warn!(
                "force reindex: aborted — discarded staging corpus and restored the \
                 original durable corpus for '{}'",
                index_id.0
            );
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(
            "force reindex: could not restore the original corpus for '{}' after abort ({e})",
            index_id.0
        ),
    }
}
