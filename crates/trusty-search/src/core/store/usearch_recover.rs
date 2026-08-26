//! Snapshot-adoption recovery for [`UsearchStore`] (issue #4707).
//!
//! Why: the #1711 data-loss guard in [`UsearchStore::save`] correctly refuses
//! to overwrite a populated on-disk snapshot with an empty in-memory index —
//! but before this module that refusal was the END of the story. The store
//! stayed empty, every later save was refused for the same reason, and the
//! index served zero vectors forever while a perfectly good snapshot sat on
//! disk. Refusing the write protects the DATA; adopting the snapshot restores
//! the SERVICE. Both are required.
//! What: [`UsearchStore::adopt_on_disk_snapshot`] re-reads the on-disk
//! snapshot through the ordinary [`UsearchStore::load_from`] path (so every
//! existing discard guard — the #2922 size floor, the zero-vector-vs-populated
//! sidecar check, the #3970 torn-pairing check — applies unchanged) and, only
//! on a clean load, transplants that state into the live store.
//! Test: `super::tests::test_save_refusal_adopts_populated_snapshot`,
//! `super::tests::test_adopt_declines_when_snapshot_is_unrecoverable`,
//! `super::tests::test_adopt_declines_on_dim_mismatch`.
//!
//! Issue #6299 added a second recovery to the same file, for the opposite
//! failure: the snapshot the store recorded is not merely stale, it is GONE.
//! [`UsearchStore::resolve_staged_snapshot_inner`] repairs the store when a
//! reindex's staged→live swap renames or deletes the file underneath it, and
//! [`UsearchStore::rebuild_mutable_from_view`] is the last-resort promote
//! fallback for a recorded path that has vanished for any other reason.

use std::path::Path;
use std::sync::atomic::Ordering;

use anyhow::{anyhow, Result};
use usearch::Index;

use super::super::store_config::MmapServeMode;
use super::types::StagedSwapOutcome;
use super::usearch_store::UsearchStore;

/// Outcome of the guarded write inside [`UsearchStore::save`].
///
/// Why (issue #4707): `save` previously reported "did we write?" as a bare
/// `bool`, which collapsed two very different refusals into one value. Only
/// the #1711 empty-over-populated refusal is RECOVERABLE — it is precisely
/// the case where the on-disk snapshot is known-good and strictly better than
/// what is in memory, so adopting it is safe and correct. The #1717 shrink
/// refusal is NOT: a partially-populated in-memory index may hold vectors the
/// on-disk snapshot does not, so blindly adopting disk there would discard
/// live state. Naming the two refusals apart is what lets `save` recover from
/// exactly one of them.
/// Test: exercised through `save` by the tests named in the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SaveVerdict {
    /// The snapshot was written; the caller must finish the rename + sidecar.
    Saved,
    /// #1711: in-memory index is empty and the on-disk snapshot is populated.
    /// Recoverable — the caller should adopt the on-disk snapshot.
    RefusedEmptyOverPopulated,
    /// #1717: in-memory index shrank catastrophically without explanation.
    /// NOT recoverable by adoption — see the enum doc.
    RefusedShrink,
}

impl UsearchStore {
    /// Replace this store's in-memory HNSW state with the snapshot on disk at
    /// `hnsw_path`, when that snapshot passes every ordinary load guard.
    ///
    /// Why (issue #4707): see the module doc. This is the recovery half of the
    /// #1711 guard — the guard keeps the bytes, this restores the service.
    ///
    /// CRITICAL correctness: this method NEVER writes to disk and never
    /// weakens the #1711 guard. It only ever moves in-memory state in one
    /// direction — towards whatever is already durably on disk — so the worst
    /// case is that it declines to act (`Ok(false)`) and the store is left
    /// exactly as it was. Validation is not reimplemented here: the candidate
    /// is loaded through [`UsearchStore::load_from`], so a truncated (#2922),
    /// empty-versus-populated, or torn binary/sidecar pairing (#3970) snapshot
    /// is rejected by the same code that rejects it at warm-boot, and can
    /// never be adopted.
    ///
    /// What: loads the on-disk snapshot into a throwaway probe store; refuses
    /// (`Ok(false)`) when the probe is absent/discarded by a guard, when its
    /// dimensionality disagrees with this store's, or when it carries no
    /// vectors at all (adopting an empty snapshot would be a no-op that
    /// falsely reports recovery). Otherwise takes the probe's validated key
    /// maps, drops the probe, and re-opens the same file on this store's own
    /// `Index` handle — mirroring [`UsearchStore::try_demote_to_view`]'s
    /// in-place `Index::view` — then publishes the maps and marks the store
    /// clean (the in-memory graph now IS the on-disk snapshot). Honours
    /// `TRUSTY_HNSW_MMAP_SERVE` exactly as `load_from` does.
    ///
    /// Test: see the module doc.
    pub(super) async fn adopt_on_disk_snapshot(&self, hnsw_path: &Path) -> Result<bool> {
        let path_str = hnsw_path
            .to_str()
            .ok_or_else(|| anyhow!("non-utf8 hnsw path: {}", hnsw_path.display()))?
            .to_string();

        // Load through the ordinary path so every discard guard applies.
        let probe = match Self::load_from(hnsw_path).await {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(false),
            Err(e) => {
                tracing::warn!(
                    "usearch: cannot adopt snapshot {} — load failed ({e}) (issue #4707)",
                    hnsw_path.display()
                );
                return Ok(false);
            }
        };
        if probe.dim() != self.dim {
            tracing::warn!(
                "usearch: refusing to adopt snapshot {} — its dim is {} but this store is {} \
                 (issue #4707)",
                hnsw_path.display(),
                probe.dim(),
                self.dim
            );
            return Ok(false);
        }

        // Take the probe's validated maps rather than re-parsing the sidecar.
        let new_id_to_key = std::mem::take(&mut *probe.id_to_key.write().await);
        let new_key_to_id = std::mem::take(&mut *probe.key_to_id.write().await);
        let new_next_key = probe.next_key.load(Ordering::Relaxed);
        let restored = new_id_to_key.len();
        // Release the probe (and its mmap) before re-opening the same file.
        drop(probe);

        if restored == 0 {
            // Nothing to adopt. Reporting `true` here would claim a recovery
            // that left the store just as empty as it started.
            return Ok(false);
        }

        {
            let index = self.index.write().await;
            if let Err(e) = index.view(&path_str) {
                tracing::warn!(
                    "usearch: cannot adopt snapshot {} — view() failed ({e}); leaving the \
                     store as it was (issue #4707)",
                    hnsw_path.display()
                );
                return Ok(false);
            }
            let mut id_map = self.id_to_key.write().await;
            let mut key_map = self.key_to_id.write().await;
            *id_map = new_id_to_key;
            *key_map = new_key_to_id;
        }
        self.next_key.store(new_next_key.max(1), Ordering::Relaxed);
        self.is_view.store(true, Ordering::Release);
        // The in-memory graph is now byte-for-byte the on-disk snapshot.
        self.dirty.store(false, Ordering::Release);
        *self.hnsw_path.write().await = Some(hnsw_path.to_path_buf());

        if MmapServeMode::from_env().promote_on_load() {
            self.promote_view_to_mutable().await?;
        }

        tracing::warn!(
            "usearch: RECOVERED index from its on-disk snapshot {} ({restored} vectors) after \
             an empty in-memory index was refused by the #1711 data-loss guard — the vector \
             lane is queryable again without a reindex (issue #4707)",
            hnsw_path.display(),
        );
        Ok(true)
    }

    /// Repair this store's `hnsw_path` / `is_view` after a reindex's staged
    /// HNSW swap resolved (issue #6299).
    ///
    /// Why: see [`super::types::VectorStore::resolve_staged_snapshot`]. A
    /// store whose recorded path was renamed or deleted under it fails every
    /// subsequent write with `No such file or directory`, forever.
    ///
    /// What: does nothing unless the store actually recorded `staged` — a
    /// store still pointing at its live snapshot was never affected by the
    /// swap, and re-pointing it would be the lie this method exists to
    /// remove. When it did record `staged`:
    /// - [`StagedSwapOutcome::Committed`]: the staging file was renamed onto
    ///   `live`, so the bytes are unchanged and only the name moved. Record
    ///   `live`. Any mmap the store holds is of that same inode and stays
    ///   valid, and a later promote or demote now names a file that exists.
    /// - [`StagedSwapOutcome::Aborted`]: the staging file was deleted and the
    ///   reindex's state is discarded (the redb corpus rolls back in the same
    ///   step — `finish_teardown::resolve_hnsw_swap` shares one
    ///   `StagingResolution` with `resolve_corpus_swap`), so the pre-reindex
    ///   live snapshot is authoritative again. Adopt it through
    ///   [`Self::adopt_on_disk_snapshot`], which re-validates it through every
    ///   ordinary load guard. When there is no adoptable live snapshot — a
    ///   first-ever reindex that aborted before one existed — keep the
    ///   in-memory graph, record `live` as where a future save belongs, and
    ///   mark the store dirty so the idle sweep never re-views a file that
    ///   does not match what is in memory.
    ///
    /// Test: `super::tests::test_resolve_staged_snapshot_commit_repoints_to_live`,
    /// `super::tests::test_resolve_staged_snapshot_abort_restores_live_snapshot`,
    /// `super::tests::test_resolve_staged_snapshot_leaves_unrelated_path_alone`.
    pub(super) async fn resolve_staged_snapshot_inner(
        &self,
        staged: &Path,
        live: &Path,
        outcome: StagedSwapOutcome,
    ) -> Result<()> {
        let recorded_staged = {
            let guard = self.hnsw_path.read().await;
            guard.as_deref() == Some(staged)
        };
        if !recorded_staged {
            return Ok(());
        }

        if outcome == StagedSwapOutcome::Committed {
            *self.hnsw_path.write().await = Some(live.to_path_buf());
            tracing::info!(
                "usearch: re-pointed store from the committed staging snapshot {} to {} \
                 (issue #6299)",
                staged.display(),
                live.display(),
            );
            return Ok(());
        }

        let adopted = match self.adopt_on_disk_snapshot(live).await {
            Ok(adopted) => adopted,
            Err(e) => {
                tracing::warn!(
                    "usearch: could not adopt the live snapshot {} after an aborted reindex \
                     ({e}) — keeping the in-memory graph (issue #6299)",
                    live.display(),
                );
                false
            }
        };
        if !adopted {
            *self.hnsw_path.write().await = Some(live.to_path_buf());
            // Nothing on disk matches the in-memory graph: the idle sweep must
            // not re-view `live` (it would silently roll the graph back), and a
            // future save has somewhere truthful to write.
            self.dirty.store(true, Ordering::Release);
            tracing::warn!(
                "usearch: aborted reindex deleted the staging snapshot {} and no live snapshot \
                 at {} could be adopted — keeping the in-memory graph, marked dirty \
                 (issue #6299)",
                staged.display(),
                live.display(),
            );
        }
        Ok(())
    }

    /// Last-resort promote fallback: rebuild a mutable heap graph out of the
    /// mmap view this store is already holding (issue #6299).
    ///
    /// Why: [`UsearchStore::promote_view_to_mutable`] re-reads the recorded
    /// snapshot with `Index::load`, so a recorded path that no longer exists
    /// made every write fail permanently — a failed promote leaves `is_view`
    /// set, so the store could never heal itself. The vectors themselves are
    /// not lost when that happens: a mapping outlives the unlink or rename of
    /// the file it was made from, so the graph is still readable in memory
    /// even though no path names it any more. Rebuilding from what is mapped
    /// turns a permanent wedge into a self-heal that costs one serialize plus
    /// one deserialize.
    ///
    /// What: serializes the mapped index into a heap buffer
    /// (`serialized_length` + `save_to_buffer`) and deserializes it back into
    /// the same handle with `load_from_buffer`, which — unlike
    /// `view_from_buffer` — copies into owned memory, so the graph survives
    /// the buffer being dropped. Called with the caller's `index` write guard
    /// already held; it does not touch `is_view` / `dirty`, which the caller
    /// sets. Returns the original load error alongside its own on failure, so
    /// the log names both the vanished path and why the fallback could not
    /// stand in for it.
    ///
    /// Test: `super::tests::test_promote_rebuilds_from_view_when_snapshot_vanished`.
    pub(super) fn rebuild_mutable_from_view(
        &self,
        index: &Index,
        source: &Path,
        load_err: &str,
    ) -> Result<()> {
        let vectors = index.size();
        let mut buffer = vec![0u8; index.serialized_length()];
        index.save_to_buffer(&mut buffer).map_err(|e| {
            anyhow!(
                "usearch failed to promote view → mutable load: {load_err}; and the mapped \
                 snapshot {} could not be serialized for an in-memory rebuild either: {e} \
                 (issue #6299)",
                source.display(),
            )
        })?;
        index.load_from_buffer(&buffer).map_err(|e| {
            anyhow!(
                "usearch failed to promote view → mutable load: {load_err}; and the in-memory \
                 rebuild of {} could not be deserialized: {e} (issue #6299)",
                source.display(),
            )
        })?;
        let size = index.size();
        if index.capacity() < size {
            index
                .reserve(size.max(1))
                .map_err(|e| anyhow!("usearch reserve after in-memory rebuild failed: {e}"))?;
        }
        tracing::error!(
            "usearch: snapshot {} could not be re-read to promote this index ({load_err}) — \
             rebuilt {vectors} vector(s) from the mapping still held in memory so writes \
             proceed instead of failing forever (issue #6299). The next save re-creates the \
             snapshot.",
            source.display(),
        );
        Ok(())
    }
}
