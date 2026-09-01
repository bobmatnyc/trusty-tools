//! The idempotency record that makes a re-run cheap (#6533).
//!
//! Why: the drain runs on a schedule over directories that mostly do not
//! change. Without a record of what was already uploaded, every run re-uploads
//! every log file — unbounded egress for zero new information. The manifest is
//! what turns the second run into a stat-only pass.
//! What: [`DrainManifest`], a per-target JSON document stored both remotely (at
//! [`MANIFEST_FILENAME`] under the target's `logs/` prefix, where it is
//! authoritative) and locally under the caller's `state_dir` (a cache, so an
//! unchanged run needs no network read at all). [`DrainManifest::decide`]
//! answers upload-or-skip for one source file.
//! Test: `super::tests::manifest_stat_fast_path_and_sha_tiebreak`,
//! `super::tests::run_once_reuploads_a_mutated_file`,
//! `super::tests::run_once_sha_beats_a_moved_mtime`,
//! `super::tests::manifest_remote_wins_over_local_cache`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::destination::LogDestination;
use super::error::DrainError;

/// Object name the per-target manifest is written under, inside `logs/`.
pub const MANIFEST_FILENAME: &str = ".drain-manifest.json";

/// Schema version of the manifest document.
///
/// A manifest carrying an unrecognised version is treated as absent rather than
/// as an error: re-uploading is always safe, mis-reading a schema is not.
pub const MANIFEST_VERSION: u32 = 1;

/// One uploaded source file, as the manifest remembers it.
///
/// Why: the four recorded facts are exactly what [`DrainManifest::decide`]
/// needs — `size` and `mtime_unix` for the cheap stat-only comparison,
/// `sha256` for the authoritative content comparison, and `uploaded_at` for an
/// operator reading the manifest by hand.
/// What: `relative_file` is the key suffix beneath the target's `logs/` prefix,
/// i.e. `<crate>/<relative_file>`. It is the entry's identity.
/// Test: `super::tests::manifest_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// `<crate>/<relative path>` — the portion of the object key below `logs/`.
    pub relative_file: String,
    /// Size in bytes of the PLAINTEXT source file, not the gzipped body.
    pub size: u64,
    /// Source file's modification time, Unix seconds.
    pub mtime_unix: i64,
    /// Hex SHA-256 of the PLAINTEXT source bytes, before scrubbing and gzip.
    pub sha256: String,
    /// When this entry was written, RFC 3339.
    pub uploaded_at: String,
}

/// The per-target upload record.
///
/// Why: see the module docs.
/// What: a versioned list of [`ManifestEntry`]. Stored as a list rather than a
/// map so the on-disk document is a stable, diffable, order-independent
/// artifact an operator can read; the lookup index is built on demand.
/// Test: `super::tests::manifest_roundtrip`, `super::tests::manifest_stat_fast_path_and_sha_tiebreak`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrainManifest {
    /// Schema version — [`MANIFEST_VERSION`] for anything this code writes.
    pub version: u32,
    /// One entry per successfully uploaded source file, sorted by `relative_file`.
    pub entries: Vec<ManifestEntry>,
}

impl Default for DrainManifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            entries: Vec::new(),
        }
    }
}

/// What [`DrainManifest::decide`] concluded about one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatDecision {
    /// Size AND mtime match the recorded entry — skip without opening the file.
    SkipUnchanged,
    /// No entry, or the stat differs. The file must be read and hashed.
    NeedsHash,
}

impl DrainManifest {
    /// Build a `relative_file` → entry index for repeated lookups.
    fn index(&self) -> HashMap<&str, &ManifestEntry> {
        self.entries
            .iter()
            .map(|e| (e.relative_file.as_str(), e))
            .collect()
    }

    /// The cheap first-pass decision, from `stat` alone.
    ///
    /// Why: an unchanged run must not read gigabytes of log text to conclude
    /// nothing changed. Size plus mtime is what a filesystem can answer without
    /// opening the file, and it is correct for the overwhelmingly common case
    /// (an append-only log either grew or did not).
    /// What: returns [`StatDecision::SkipUnchanged`] only when an entry exists
    /// AND both size and mtime match it. Everything else needs the hash.
    /// Test: `super::tests::manifest_stat_fast_path_and_sha_tiebreak`.
    pub fn decide(&self, relative_file: &str, size: u64, mtime_unix: i64) -> StatDecision {
        match self.index().get(relative_file) {
            Some(entry) if entry.size == size && entry.mtime_unix == mtime_unix => {
                StatDecision::SkipUnchanged
            }
            _ => StatDecision::NeedsHash,
        }
    }

    /// Does the recorded entry's digest match `sha256`?
    ///
    /// Why: this is where SHA-256 WINS over mtime. A file whose mtime moved but
    /// whose bytes are identical — a `touch`, a log rotated back into place, a
    /// checkout that rewrote timestamps — is NOT re-uploaded. Content identity
    /// is the thing the drain actually cares about; mtime is only ever an
    /// optimisation for avoiding the read.
    /// What: `true` when an entry exists and its `sha256` equals the argument.
    /// The caller then refreshes the entry's size and mtime via
    /// [`DrainManifest::record`] so the NEXT run hits the stat-only fast path.
    /// Test: `super::tests::run_once_sha_beats_a_moved_mtime`.
    pub fn digest_matches(&self, relative_file: &str, sha256: &str) -> bool {
        self.index()
            .get(relative_file)
            .is_some_and(|entry| entry.sha256 == sha256)
    }

    /// Insert or replace the entry for one source file.
    pub fn record(&mut self, entry: ManifestEntry) {
        match self
            .entries
            .iter_mut()
            .find(|e| e.relative_file == entry.relative_file)
        {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
        self.entries
            .sort_by(|a, b| a.relative_file.cmp(&b.relative_file));
    }

    /// Load the manifest for a target, preferring the remote copy.
    ///
    /// Why: the local cache exists so an unchanged run costs no network read,
    /// but it can be stale or simply absent — a fresh machine, a cleared state
    /// directory, or an upload that another host performed. When both exist and
    /// disagree, the REMOTE copy wins, because it is the only one that
    /// describes what is actually in the bucket. A stale local cache that won
    /// would make the drain skip files that were never uploaded.
    /// What: reads the remote object first; on success, refreshes the local
    /// cache from it and returns it. On absence or an undecodable document,
    /// falls back to the local cache, and to an empty manifest if that is
    /// missing too. An undecodable manifest is never fatal — re-uploading is
    /// strictly safer than skipping something that was never written.
    /// Test: `super::tests::manifest_remote_wins_over_local_cache`,
    /// `super::tests::manifest_corrupt_remote_falls_back_to_cache`.
    ///
    /// # Errors
    /// [`DrainError::Transport`] only when the destination itself failed —
    /// a missing or corrupt manifest is handled, not raised.
    pub async fn load(
        dest: &dyn LogDestination,
        state_dir: &Path,
        remote_key: &str,
        cache_key: &str,
    ) -> Result<Self, DrainError> {
        let cache_path = cache_file(state_dir, cache_key);

        if let Some(raw) = dest.get(remote_key).await? {
            match Self::decode(&raw) {
                Ok(remote) => {
                    // Remote is authoritative: overwrite whatever the cache held.
                    write_cache(&cache_path, &remote);
                    return Ok(remote);
                }
                Err(reason) => {
                    tracing::warn!(
                        key = %remote_key,
                        %reason,
                        "log-drain manifest is undecodable; treating as absent and re-uploading"
                    );
                }
            }
        }

        Ok(read_cache(&cache_path).unwrap_or_default())
    }

    /// Write the manifest to the destination and refresh the local cache.
    ///
    /// Called after each successful batch so a run interrupted partway still
    /// leaves the next run able to skip what did land.
    pub async fn save(
        &self,
        dest: &dyn LogDestination,
        state_dir: &Path,
        remote_key: &str,
        cache_key: &str,
    ) -> Result<(), DrainError> {
        let body = serde_json::to_vec_pretty(self).map_err(|e| DrainError::Manifest {
            key: remote_key.to_string(),
            reason: format!("could not serialise: {e}"),
        })?;

        dest.put(
            remote_key,
            bytes::Bytes::from(body),
            super::destination::PutMeta {
                content_type: Some("application/json".to_string()),
                content_encoding: None,
            },
        )
        .await?;

        write_cache(&cache_file(state_dir, cache_key), self);
        Ok(())
    }

    /// Decode a manifest document, rejecting an unrecognised schema version.
    fn decode(raw: &[u8]) -> Result<Self, String> {
        let parsed: Self = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
        if parsed.version != MANIFEST_VERSION {
            return Err(format!(
                "schema version {} is not the supported {MANIFEST_VERSION}",
                parsed.version
            ));
        }
        Ok(parsed)
    }
}

/// Where the local cache copy of a target's manifest lives.
fn cache_file(state_dir: &Path, cache_key: &str) -> PathBuf {
    state_dir
        .join("log-drain")
        .join(cache_key)
        .join("manifest.json")
}

/// Read the cache, returning `None` for anything unreadable or undecodable.
///
/// A cache miss is ordinary; a corrupt cache is a re-upload, never an error.
fn read_cache(path: &Path) -> Option<DrainManifest> {
    let raw = std::fs::read(path).ok()?;
    DrainManifest::decode(&raw).ok()
}

/// Best-effort cache write.
///
/// A failure here costs the NEXT run one extra remote read and nothing else, so
/// it is logged and swallowed rather than failing an otherwise-successful
/// upload.
fn write_cache(path: &Path, manifest: &DrainManifest) {
    let Some(parent) = path.parent() else { return };
    if let Err(e) = std::fs::create_dir_all(parent) {
        tracing::warn!(path = %parent.display(), error = %e, "log-drain cache dir unwritable");
        return;
    }
    match serde_json::to_vec_pretty(manifest) {
        Ok(body) => {
            if let Err(e) = std::fs::write(path, body) {
                tracing::warn!(path = %path.display(), error = %e, "log-drain cache write failed");
            }
        }
        Err(e) => tracing::warn!(error = %e, "log-drain cache serialisation failed"),
    }
}
