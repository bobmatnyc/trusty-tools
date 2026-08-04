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

use std::path::Path;
use std::sync::atomic::Ordering;

use anyhow::{anyhow, Result};

use super::super::store_config::MmapServeMode;
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
}
