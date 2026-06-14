//! Atomic corpus swap: open/seed/commit/abort for staged reindex (#28, #603).
//!
//! Why: every reindex (force or incremental) stages its rebuilt corpus in a
//! sibling `index.redb.tmp` and atomically renames it into place only on
//! success (#603). Before this change, any reindex failure destroyed the only
//! searchable corpus. Keeping the swap plumbing in one file makes the
//! safety-critical rename path easy to audit.
//!
//! What:
//! - `begin_staged_corpus_swap` — open a fresh staging store and optionally
//!   seed it from the live corpus for an incremental reindex (#839).
//! - `commit_staged_corpus_swap` — rename staging → live on success.
//! - `abort_staged_corpus_swap` — delete staging and re-open live on failure.
//!
//! Test: `incremental_reindex_no_durable_data_loss` and
//! `incremental_reindex_carryover_failure_aborts` in `service::reindex::tests`.

use crate::core::registry::{IndexHandle, IndexId};
use anyhow::Context;
use std::path::{Path, PathBuf};

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
/// Test: `incremental_reindex_no_durable_data_loss` and
/// `incremental_reindex_carryover_failure_aborts`.
pub(super) async fn begin_staged_corpus_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    force: bool,
) -> Result<Option<PathBuf>, anyhow::Error> {
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
    // Issue #403: route tmp corpus path to colocated or legacy storage.
    let tmp_path = if crate::service::colocated_storage::has_colocated_storage(&handle.root_path) {
        match crate::service::colocated_storage::colocated_redb_tmp_path(&handle.root_path) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "staged corpus swap: cannot resolve colocated staging corpus path for '{}' ({e}) — \
                     reindex will write directly to the live corpus",
                    index_id.0
                );
                return Ok(None);
            }
        }
    } else {
        match crate::service::persistence::corpus_redb_tmp_path(&index_id.0) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "staged corpus swap: cannot resolve staging corpus path for '{}' ({e}) — \
                     reindex will write directly to the live corpus",
                    index_id.0
                );
                return Ok(None);
            }
        }
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
    Ok(Some(tmp_path))
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
    // Drop the staging store's last Arc so redb releases the temp file before
    // the rename.
    {
        let mut indexer = handle.indexer.write().await;
        let _ = indexer.take_corpus_store();
    }
    let tmp = tmp_path.to_path_buf();
    let live = live_path.clone();
    let index_id_inner = index_id.0.clone();
    // rename + re-open on a blocking worker (filesystem + redb sync calls).
    let reopened = tokio::task::spawn_blocking(
        move || -> anyhow::Result<crate::core::corpus::CorpusStore> {
            std::fs::rename(&tmp, &live).with_context(|| {
                format!(
                    "atomic-swap rename {} -> {} for '{index_id_inner}'",
                    tmp.display(),
                    live.display()
                )
            })?;
            crate::core::corpus::CorpusStore::open(&live)
                .with_context(|| format!("re-open swapped corpus for '{index_id_inner}'"))
        },
    )
    .await;
    match reopened {
        Ok(Ok(store)) => {
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
        Ok(Err(e)) => tracing::warn!(
            "force reindex: atomic corpus swap failed for '{}' ({e}) — \
             previous corpus preserved; in-memory state is the rebuilt one",
            index_id.0
        ),
        Err(e) => tracing::warn!(
            "force reindex: atomic corpus swap task panicked for '{}': {e}",
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
    let restored = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Option<crate::core::corpus::CorpusStore>> {
            match std::fs::remove_file(&tmp) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(
                    "force reindex: could not delete staging corpus {} for '{index_id_inner}': {e}",
                    tmp.display()
                ),
            }
            match live_path {
                Ok(live) => Ok(Some(crate::core::corpus::CorpusStore::open(&live)?)),
                Err(e) => {
                    tracing::warn!(
                        "force reindex: cannot resolve live corpus path for '{index_id_inner}' \
                         ({e}) — index left without a durable corpus until next restart"
                    );
                    Ok(None)
                }
            }
        },
    )
    .await;
    match restored {
        Ok(Ok(Some(store))) => {
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
        Ok(Ok(None)) => {}
        Ok(Err(e)) => tracing::warn!(
            "force reindex: could not restore the original corpus for '{}' after abort ({e})",
            index_id.0
        ),
        Err(e) => tracing::warn!(
            "force reindex: corpus-restore task panicked for '{}': {e}",
            index_id.0
        ),
    }
}
