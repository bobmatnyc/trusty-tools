//! Publish a prepared HNSW binary with its matching key sidecar (#6961).
//! Why: a sidecar I/O failure must not replace the last usable binary.
//! What: stage the sidecar, publish it first, and restore it if binary rename fails.
//! Test: `super::snapshot_tests` covers staging and publication failures.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

use super::types::StoreKeyMap;
use super::usearch_store::staging_path;

/// Publish under the caller's store mutation gate; never resets removal credit.
///
/// The pair uses two renames, matching the reindex swap's sidecar-first
/// contract. A process crash between them can leave the sidecar ahead; ordinary
/// I/O failures restore the previous sidecar or explicitly report rollback failure.
/// Test: `super::snapshot_tests::binary_publication_failure_restores_removed_and_rewritten_keys`.
pub(super) fn publish_snapshot(path: &Path, key_map: &StoreKeyMap) -> Result<()> {
    let binary_tmp = staging_path(path, "usearch");
    let sidecar = path.with_extension("keys.json");
    let sidecar_tmp = staging_path(&sidecar, "json");
    let publish = || -> Result<()> {
        let json = serde_json::to_vec(key_map).context("serialize hnsw key map")?;
        std::fs::write(&sidecar_tmp, json).context("write hnsw key sidecar tmp")?;
        let previous = match std::fs::read(&sidecar) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).context("read previous hnsw key sidecar"),
        };
        std::fs::rename(&sidecar_tmp, &sidecar).context("rename hnsw key sidecar")?;
        if let Err(publish_error) = std::fs::rename(&binary_tmp, path) {
            let rollback = match previous {
                Some(bytes) => std::fs::write(&sidecar_tmp, bytes)
                    .and_then(|()| std::fs::rename(&sidecar_tmp, &sidecar)),
                None => std::fs::remove_file(&sidecar),
            };
            if let Err(rollback_error) = rollback {
                return Err(anyhow!("rename hnsw snapshot: {publish_error}; restore previous hnsw key sidecar: {rollback_error}"));
            }
            return Err(publish_error).context("rename hnsw snapshot (previous sidecar restored)");
        }
        Ok(())
    };
    let result = publish();
    if result.is_err() {
        let _ = std::fs::remove_file(&binary_tmp);
        let _ = std::fs::remove_file(&sidecar_tmp);
    }
    result
}
