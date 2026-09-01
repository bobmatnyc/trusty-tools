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
//!
//! # The cache is per DESTINATION as well as per target (#6548)
//!
//! [`DrainManifest::cache_path`] puts
//! [`LogDestination::cache_namespace`] above the target's own segment. Before
//! that, the cache lived at `<state_dir>/log-drain/<github_id>/<session_id>/`
//! and described no particular destination, so switching one session from
//! bucket A to bucket B found A's record — and, since the fresh bucket had no
//! remote manifest of its own to override it, skipped every file A already
//! held. A skip decision is only ever valid for the destination it was made
//! against.

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

/// Which bound a [`SkipRecord`] was written for.
///
/// Why: an operator reading the manifest needs to know whether raising
/// `max_file_bytes` or `max_wire_bytes` is the lever that would drain the file.
/// Test: `super::tests::run_once_records_an_oversize_skip_once`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SkipReason {
    /// The plaintext file is over `CollectLimits::max_file_bytes`.
    SourceTooLarge,
    /// The compressed body passed `CollectLimits::max_wire_bytes` mid-stream.
    CompressedTooLarge,
}

impl SkipReason {
    /// The knob an operator would raise to make this file drain.
    pub fn limit_name(self) -> &'static str {
        match self {
            Self::SourceTooLarge => "max_file_bytes",
            Self::CompressedTooLarge => "max_wire_bytes",
        }
    }
}

/// One source file the drain decided NOT to upload, and the facts behind it.
///
/// Why: before #6547 an oversize file was re-stat'd, re-decided, and re-warned
/// every cycle — 1,276 identical WARNs in 48 hours over ~40 files that can
/// never shrink. The decision is a function of `(relative_file, size,
/// mtime_unix)` and the configured bounds, so recording it makes it once.
/// What: the same identity pair [`ManifestEntry`] uses, plus the bound that was
/// hit. A file whose size OR mtime moves no longer matches its record, so the
/// next pass re-evaluates it from scratch — which is also how a file becomes
/// drainable again after the operator raises the bound and it next rotates.
/// Test: `super::tests::run_once_records_an_oversize_skip_once`,
/// `super::tests::run_once_re_evaluates_a_skip_when_the_file_changes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipRecord {
    /// `<crate>/<relative path>` — the entry's identity, as [`ManifestEntry`].
    pub relative_file: String,
    /// Size in bytes of the plaintext source file when the decision was made.
    pub size: u64,
    /// Source file's modification time at that moment, Unix seconds.
    pub mtime_unix: i64,
    /// Which bound was hit.
    pub reason: SkipReason,
    /// When this decision was made, RFC 3339.
    pub decided_at: String,
}

/// The per-target upload record.
///
/// Why: see the module docs.
/// What: a versioned list of [`ManifestEntry`], plus the [`SkipRecord`] list
/// #6547 added. Stored as lists rather than maps so the on-disk document is a
/// stable, diffable, order-independent artifact an operator can read; the
/// lookup index is built on demand.
/// Test: `super::tests::manifest_roundtrip`, `super::tests::manifest_stat_fast_path_and_sha_tiebreak`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrainManifest {
    /// Schema version — [`MANIFEST_VERSION`] for anything this code writes.
    pub version: u32,
    /// One entry per successfully uploaded source file, sorted by `relative_file`.
    pub entries: Vec<ManifestEntry>,
    /// One entry per file the drain decided not to upload (#6547).
    ///
    /// `#[serde(default)]` so a manifest written before #6547 still decodes —
    /// it simply carries no recorded decisions, and the next pass makes them.
    #[serde(default)]
    pub skips: Vec<SkipRecord>,
}

impl Default for DrainManifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            entries: Vec::new(),
            skips: Vec::new(),
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

/// Which copy [`DrainManifest::load_with_origin`] actually returned.
///
/// Why: only a manifest that came from the destination makes a claim about what
/// that destination holds, so only that one is worth spot-checking against it
/// (#6548). A cached or empty manifest has nothing to be caught lying about.
/// Test: `super::tests::run_once_spot_checks_a_lying_remote_manifest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ManifestOrigin {
    /// The destination's own copy — the authoritative one.
    Remote,
    /// The local cache; the destination had no decodable manifest.
    LocalCache,
    /// Neither copy existed, so the manifest is empty.
    Absent,
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

    /// Has this exact `(file, size, mtime)` already been decided un-drainable?
    ///
    /// Why: the answer for an oversize file cannot change while the file does
    /// not, so re-deciding it every 15 minutes is 96 identical warnings a day
    /// per file (#6547). A `true` here is what turns the second cycle's log
    /// line off.
    /// What: matches on all three fields. A file that grew, was rotated, or was
    /// rewritten no longer matches and is evaluated again from scratch.
    /// Test: `super::tests::run_once_records_an_oversize_skip_once`,
    /// `super::tests::run_once_re_evaluates_a_skip_when_the_file_changes`.
    pub fn skip_recorded(&self, relative_file: &str, size: u64, mtime_unix: i64) -> bool {
        self.skips.iter().any(|s| {
            s.relative_file == relative_file && s.size == size && s.mtime_unix == mtime_unix
        })
    }

    /// Insert or replace the skip decision for one source file.
    ///
    /// One record per file: a new decision supersedes the old one rather than
    /// accumulating, so the manifest cannot grow without bound as a daily log
    /// is appended to and re-decided.
    pub fn record_skip(&mut self, record: SkipRecord) {
        match self
            .skips
            .iter_mut()
            .find(|s| s.relative_file == record.relative_file)
        {
            Some(existing) => *existing = record,
            None => self.skips.push(record),
        }
        self.skips
            .sort_by(|a, b| a.relative_file.cmp(&b.relative_file));
    }

    /// Drop any skip decision for `relative_file`; `true` when one was there.
    ///
    /// Called for every file a pass CAN read, so raising a bound — or a
    /// rotation that shrinks a file back under one — clears the record rather
    /// than leaving a stale "we decided not to upload this" beside a successful
    /// upload of the same key.
    /// Test: `super::tests::run_once_re_evaluates_a_skip_when_the_file_changes`.
    pub fn forget_skip(&mut self, relative_file: &str) -> bool {
        let before = self.skips.len();
        self.skips.retain(|s| s.relative_file != relative_file);
        self.skips.len() != before
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
        Self::load_with_origin(dest, state_dir, remote_key, cache_key)
            .await
            .map(|(manifest, _)| manifest)
    }

    /// [`DrainManifest::load`], also saying which copy answered.
    ///
    /// Why: [`DrainManifest::spot_check`] is only meaningful against a manifest
    /// the destination itself supplied, and `load` alone cannot say whether it
    /// returned one (#6548).
    /// What: identical to `load` in every other respect; the second element is
    /// [`ManifestOrigin`].
    /// Test: `super::tests::run_once_spot_checks_a_lying_remote_manifest`.
    ///
    /// # Errors
    /// As [`DrainManifest::load`].
    pub async fn load_with_origin(
        dest: &dyn LogDestination,
        state_dir: &Path,
        remote_key: &str,
        cache_key: &str,
    ) -> Result<(Self, ManifestOrigin), DrainError> {
        let cache_path = Self::cache_path(state_dir, dest, cache_key);

        if let Some(raw) = dest.get(remote_key).await? {
            match Self::decode(&raw) {
                Ok(remote) => {
                    // Remote is authoritative: overwrite whatever the cache held.
                    write_cache(&cache_path, &remote);
                    return Ok((remote, ManifestOrigin::Remote));
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

        Ok(match read_cache(&cache_path) {
            Some(cached) => (cached, ManifestOrigin::LocalCache),
            None => (Self::default(), ManifestOrigin::Absent),
        })
    }

    /// Confirm that one sampled entry's object really is at the destination.
    ///
    /// Why: the runs made before #6548 was fixed wrote manifests that LIE — a
    /// pass against a new bucket saved a record copied from a different
    /// bucket's cache, so the document lists objects that bucket never
    /// received, and every listed file skips forever. Keying the cache by
    /// destination stops new ones being written; it cannot repair the ones
    /// already in a bucket. One `head` per run buys an operator that signal
    /// without putting a `head` per FILE on every steady-state pass — roughly
    /// 150 extra round trips a run, forever, to guard against a defect that can
    /// no longer occur.
    ///
    /// What: samples ONE entry, `head`s its key, and returns that key when the
    /// object is absent. Detection only: nothing is re-uploaded and the
    /// manifest is not rewritten, because an object a bucket lifecycle rule
    /// legitimately expired would otherwise re-upload the whole session on
    /// every run. The repair is deliberate and manual — delete the remote
    /// manifest object; see `docs/reference/log-drain.md`. A transport error is
    /// not an answer either way, so it yields `None` rather than failing a run
    /// whose uploads are fine.
    ///
    /// Test: `super::tests::run_once_spot_checks_a_lying_remote_manifest`.
    pub async fn spot_check(&self, dest: &dyn LogDestination, logs_prefix: &str) -> Option<String> {
        let entry = self.entries.get(sample_index(self.entries.len())?)?;
        let key = format!("{logs_prefix}/{}", entry.relative_file);
        match dest.head(&key).await {
            Ok(Some(_)) => None,
            Ok(None) => Some(key),
            Err(e) => {
                tracing::debug!(
                    %key,
                    error = %e,
                    "log-drain manifest spot check could not reach the destination"
                );
                None
            }
        }
    }

    /// Where this destination's local cache copy of a target's manifest lives.
    ///
    /// Why: public because clearing the cache is the operator-facing half of
    /// the #6548 story, and a path an operator has to reconstruct by hand is a
    /// path that will be reconstructed wrongly.
    /// What: `<state_dir>/log-drain/<destination namespace>/<cache_key>/manifest.json`.
    /// Test: `super::tests::manifest_remote_wins_over_local_cache`.
    pub fn cache_path(state_dir: &Path, dest: &dyn LogDestination, cache_key: &str) -> PathBuf {
        state_dir
            .join("log-drain")
            .join(dest.cache_namespace())
            .join(cache_key)
            .join("manifest.json")
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

        write_cache(&Self::cache_path(state_dir, dest, cache_key), self);
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

/// Pick one entry index out of `len`, or `None` when there is nothing to pick.
///
/// Sub-second wall clock is the source of variation, so consecutive scheduled
/// runs land on different entries and a lying manifest is found within a few
/// passes rather than only when the same entry keeps being drawn.
fn sample_index(len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    Some(nanos % len)
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
