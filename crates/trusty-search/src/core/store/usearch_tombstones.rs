//! Key-based tombstones for copy-free removal from a view-mode index (#10303).
//!
//! Why: a store served from an mmap view (`Index::view`) cannot mutate its
//! graph, so `remove` used to promote the whole index to a heap copy first.
//! A batch removal therefore cost one full copy of the vector index (#10110).
//! A tombstone file records the removed keys instead, so a view-mode removal
//! writes one small file and copies nothing.
//!
//! What: `hnsw.tombstones.json` sits beside `hnsw.usearch` and
//! `hnsw.keys.json`. It lists the `u64` keys removed since the pair was
//! published, plus the SHA-256 of the sidecar bytes it was written against.
//! `hnsw.keys.json` and the binary are left untouched until the next mutable
//! save, so a reader that predates this file loads the same pair it always
//! did (its #3970 torn-pairing guard does not fire). This binary drops the
//! tombstoned ids from the key maps at load, removes the keys from the graph
//! when the view is promoted, and deletes the file once a new pair is
//! published. A file whose digest does not match the sidecar on disk belongs
//! to an earlier pair and is ignored, so a crash between a publish and the
//! file's deletion can never drop a live key. A missing file means "no
//! tombstones"; an unreadable or corrupt one is an error, never empty.
//!
//! Test: `super::tombstones_10303_tests`.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usearch::Index;

use super::usearch_store::{staging_path, UsearchStore};

/// The only tombstone-file schema this binary reads or writes.
const TOMBSTONE_VERSION: u32 = 1;

/// The tombstone file beside the snapshot at `hnsw_path`.
///
/// `hnsw.usearch` → `hnsw.tombstones.json`, and the reindex staging snapshot
/// `hnsw.reindex-staging.usearch` → `hnsw.reindex-staging.tombstones.json`.
pub fn tombstones_path(hnsw_path: &Path) -> PathBuf {
    hnsw_path.with_extension("tombstones.json")
}

/// A failure to read or write a tombstone file (#10303).
#[derive(Debug, thiserror::Error)]
pub enum TombstoneError {
    #[error("could not read hnsw tombstone file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("hnsw tombstone file {path} is corrupt: {source}")]
    Corrupt {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("hnsw tombstone file {path} has schema version {found}; this binary reads {TOMBSTONE_VERSION}")]
    Version { path: PathBuf, found: u32 },
    #[error("could not write hnsw tombstone file {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// The on-disk shape of `hnsw.tombstones.json`.
#[derive(Debug, Serialize, Deserialize)]
struct TombstoneFile {
    version: u32,
    /// Lower-case hex SHA-256 of the `hnsw.keys.json` bytes these keys apply to.
    sidecar_sha256: String,
    keys: Vec<u64>,
}

/// In-memory tombstones of a view-mode store.
///
/// `keys` are removed from the key maps but still in the mapped graph.
/// `sidecar_digest` caches the digest of the sidecar on disk, so only the
/// first view-mode removal after a load reads the sidecar.
#[derive(Debug, Default)]
pub(super) struct TombstoneState {
    pub(super) keys: BTreeSet<u64>,
    pub(super) sidecar_digest: Option<String>,
}

/// Lower-case hex SHA-256 of `bytes`.
pub(super) fn sidecar_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Read the tombstones that apply to the sidecar `sidecar_bytes`.
///
/// Why: load needs the removed keys, and must tell "no tombstones" from "a
/// tombstone file it cannot trust".
/// What: `Ok(None)` when the file is absent, or when its digest names another
/// sidecar (a stale file, logged at info). `Err` when the file exists but
/// cannot be read, parsed, or carries an unknown version.
/// Test: `super::tombstones_10303_tests::a_corrupt_tombstone_file_fails_the_load`,
/// `super::tombstones_10303_tests::a_stale_tombstone_file_never_drops_a_live_key`.
pub(super) fn read_tombstones(
    hnsw_path: &Path,
    sidecar_bytes: &[u8],
) -> std::result::Result<Option<TombstoneState>, TombstoneError> {
    let path = tombstones_path(hnsw_path);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(TombstoneError::Read { path, source }),
    };
    let file: TombstoneFile =
        serde_json::from_slice(&bytes).map_err(|source| TombstoneError::Corrupt {
            path: path.clone(),
            source,
        })?;
    if file.version != TOMBSTONE_VERSION {
        return Err(TombstoneError::Version {
            path,
            found: file.version,
        });
    }
    let digest = sidecar_digest(sidecar_bytes);
    if file.sidecar_sha256 != digest {
        tracing::info!(
            "usearch: tombstone file {} was written against another key sidecar — \
             ignoring it (#10303)",
            path.display()
        );
        return Ok(None);
    }
    Ok(Some(TombstoneState {
        keys: file.keys.into_iter().collect(),
        sidecar_digest: Some(digest),
    }))
}

/// Write `keys` as the tombstone file for the pair at `hnsw_path`, durably.
///
/// Why: a torn tombstone file would either drop live keys or resurrect
/// removed ones, so the file is replaced whole or not at all.
/// What: process-scoped staging file, fsync, rename over the target, fsync
/// the directory. The staging file is removed on every failure path.
/// Test: `super::tombstones_10303_tests::a_view_remove_writes_tombstones_and_copies_nothing`.
fn write_tombstones(
    hnsw_path: &Path,
    digest: &str,
    keys: &BTreeSet<u64>,
) -> std::result::Result<(), TombstoneError> {
    let path = tombstones_path(hnsw_path);
    let tmp = staging_path(&path, "json");
    let file = TombstoneFile {
        version: TOMBSTONE_VERSION,
        sidecar_sha256: digest.to_string(),
        keys: keys.iter().copied().collect(),
    };
    let write = || -> std::io::Result<()> {
        let body = serde_json::to_vec(&file).map_err(std::io::Error::other)?;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &path)?;
        if let Some(dir) = path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    };
    write().map_err(|source| {
        let _ = std::fs::remove_file(&tmp);
        TombstoneError::Write { path, source }
    })
}

/// Drop every sidecar entry whose key is tombstoned; keep the keys it held.
///
/// What: mutates `id_to_key` in place and narrows `state.keys` to the keys
/// the sidecar actually mapped, so the state names only graph members.
/// Test: `super::tombstones_10303_tests::a_view_remove_survives_a_reload`.
pub(super) fn drop_tombstoned(
    id_to_key: &mut std::collections::HashMap<String, u64>,
    state: &mut TombstoneState,
) {
    let mut held = BTreeSet::new();
    id_to_key.retain(|_, key| {
        if state.keys.contains(key) {
            held.insert(*key);
            false
        } else {
            true
        }
    });
    state.keys = held;
}

impl UsearchStore {
    /// Remove `id` from a mutable store; the caller holds `save_lock`.
    ///
    /// The pre-#10303 `remove` body, unchanged, so `remove` and `remove_many`
    /// share it. Unknown ids are a no-op.
    /// Test: `super::tests::test_remove`.
    pub(super) async fn remove_mutable_locked(&self, id: &str) -> Result<()> {
        // Promote view → mutable on first write. No-op when already mutable.
        self.ensure_mutable().await?;
        let key = {
            let mut id_to_key = self.id_to_key.write().await;
            match id_to_key.remove(id) {
                Some(k) => k,
                None => return Ok(()), // idempotent: removing an unknown id is a no-op
            }
        };
        self.key_to_id.write().await.remove(&key);

        let index = self.index.write().await;
        if index.contains(key) {
            index
                .remove(key)
                .map_err(|e| anyhow!("usearch remove failed: {e}"))?;
            // Graph changed — see the `dirty` note in `upsert` (issue #2164).
            self.mark_dirty();
            // Issue #1717: credit the shrink guard only when the graph lost a
            // vector; `upsert` can leave a map entry whose `add` failed.
            self.removed_since_save.fetch_add(1, Ordering::Relaxed);
            // #9450: counted toward the next graph compaction.
            self.compact.record(1);
        }
        Ok(())
    }

    /// Remove `ids` from a view-mode store without promoting it (#10303).
    ///
    /// Why: promotion copies the whole graph to the heap; see the module docs.
    /// What: the caller holds `save_lock` and has seen `is_view`. Resolves the
    /// ids' keys, writes the union of old and new tombstones durably, then
    /// drops the ids from both key maps. A write failure changes nothing in
    /// memory. Unknown ids are a no-op, as in `remove`.
    /// Test: `super::tombstones_10303_tests::a_view_remove_writes_tombstones_and_copies_nothing`,
    /// `super::tombstones_10303_tests::a_failed_tombstone_write_keeps_the_id`.
    pub(super) async fn tombstone_remove_locked(&self, ids: &[String]) -> Result<()> {
        let doomed: Vec<(&str, u64)> = {
            let id_to_key = self.id_to_key.read().await;
            ids.iter()
                .filter_map(|id| id_to_key.get(id).map(|&k| (id.as_str(), k)))
                .collect()
        };
        if doomed.is_empty() {
            return Ok(());
        }
        let hnsw_path = self.hnsw_path.read().await.clone().ok_or_else(|| {
            anyhow!("usearch index is in view mode but has no snapshot path (#10303)")
        })?;
        let mut state = self.tombstones.lock().await;
        let digest = match &state.sidecar_digest {
            Some(d) => d.clone(),
            None => {
                let sidecar = hnsw_path.with_extension("keys.json");
                let bytes = std::fs::read(&sidecar).map_err(|e| {
                    anyhow!(
                        "read hnsw key sidecar {} for tombstones: {e} (#10303)",
                        sidecar.display()
                    )
                })?;
                sidecar_digest(&bytes)
            }
        };
        let mut keys = state.keys.clone();
        keys.extend(doomed.iter().map(|&(_, k)| k));
        write_tombstones(&hnsw_path, &digest, &keys)?;
        // #9450: a tombstone is a removal for the compaction trigger; the
        // graph apply later does not count it again.
        self.compact.record((keys.len() - state.keys.len()) as u64);
        // The write-cooldown persist and the compaction epoch read this clock.
        self.write_clock.record_write();
        state.keys = keys;
        state.sidecar_digest = Some(digest);
        drop(state);

        let mut id_to_key = self.id_to_key.write().await;
        let mut key_to_id = self.key_to_id.write().await;
        for (id, key) in doomed {
            if id_to_key.get(id) == Some(&key) {
                id_to_key.remove(id);
                key_to_id.remove(&key);
            }
        }
        Ok(())
    }

    /// Tombstoned keys no key map names any more: what a promote must unlink.
    /// Read before the promote takes the graph lock (lock order).
    pub(super) async fn tombstones_to_apply(&self) -> Vec<u64> {
        let state = self.tombstones.lock().await;
        if state.keys.is_empty() {
            return Vec::new();
        }
        let key_to_id = self.key_to_id.read().await;
        state
            .keys
            .iter()
            .copied()
            .filter(|k| !key_to_id.contains_key(k))
            .collect()
    }

    /// Unlink tombstoned keys from a freshly promoted, mutable graph.
    ///
    /// Why: `Index::load` (or the in-memory rebuild) restores the tombstoned
    /// vectors along with the rest; they must leave the graph before the next
    /// save publishes it.
    /// What: removes each key the graph holds, credits the #1717 shrink guard
    /// (churn was counted at the tombstone write), and clears the in-memory tombstones. Returns
    /// how many vectors left the graph; the file stays until a publish.
    /// Test: `super::tombstones_10303_tests::a_crash_between_tombstone_write_and_graph_apply_loses_nothing`.
    pub(super) async fn apply_tombstones_to_graph(
        &self,
        index: &Index,
        keys: &[u64],
    ) -> Result<usize> {
        let mut applied = 0usize;
        for &key in keys {
            if index.contains(key) {
                index
                    .remove(key)
                    .map_err(|e| anyhow!("usearch remove (tombstone apply) failed: {e}"))?;
                applied += 1;
            }
        }
        // #1717: credit the shrink guard now that the graph lost the vectors.
        self.removed_since_save
            .fetch_add(applied as u64, Ordering::Relaxed);
        self.tombstones.lock().await.keys.clear();
        Ok(applied)
    }

    /// Forget the tombstones of the pair just published at `hnsw_path`.
    ///
    /// What: clears the in-memory state and deletes the file. A failed delete
    /// is logged: the file's digest no longer matches the new sidecar, so a
    /// later load ignores it.
    /// Test: `super::tombstones_10303_tests::a_crash_between_tombstone_write_and_graph_apply_loses_nothing`.
    pub(super) async fn clear_tombstones_after_publish(&self, hnsw_path: &Path) {
        *self.tombstones.lock().await = TombstoneState::default();
        let path = tombstones_path(hnsw_path);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                "usearch: could not delete stale tombstone file {} ({e}); a later load \
                 ignores it because its digest names the previous sidecar (#10303)",
                path.display()
            ),
        }
    }

    /// Tombstoned keys still present in the graph, for `len` (#10303).
    pub(super) async fn tombstones_in_graph(&self, index: &Index) -> usize {
        let state = self.tombstones.lock().await;
        state.keys.iter().filter(|&&k| index.contains(k)).count()
    }

    /// `true` when a view holds tombstones and the #9450 compaction is due.
    ///
    /// Why: no write promotes a view that only takes removals, so the idle
    /// persist is the one place its tombstones get folded into a new pair.
    /// Test: `super::tests_9450::churn_past_the_threshold_compacts_at_the_next_persist`.
    pub(super) async fn tombstones_due_compaction(&self) -> bool {
        self.has_tombstones().await && self.compaction_due().await
    }

    /// `true` while view-mode tombstones are pending.
    pub(super) async fn has_tombstones(&self) -> bool {
        !self.tombstones.lock().await.keys.is_empty()
    }
}
