//! Upload trusty-* log files to object storage (#6533, epic phase 1+2).
//!
//! Why: every trusty daemon writes logs to a local path that nothing prunes and
//! nothing collects. Diagnosing a failure on someone else's machine means
//! asking them to find and send a file. This module is the piece that gets
//! those bytes somewhere durable, scrubbed of credentials, without re-uploading
//! what has not changed.
//!
//! What: the drain CORE — a [`LogDestination`] trait with `s3://` and `file://`
//! adapters, a [`DestinationUri`] parser, the key layout, an idempotency
//! [`DrainManifest`], the [`collect`] pipeline, and [`run_once`]. Phase 1+2 is
//! the core ONLY: there is no scheduler, no config plumbing, and no consumer
//! wired up. Phase 3 adds those, plus GitHub-identity resolution — this module
//! deliberately does not resolve an identity, it demands one.
//!
//! Test: `tests` (sibling module). Run it with
//! `cargo test -p trusty-common --features log-drain --no-fail-fast`.
//!
//! # Key layout
//!
//! ```text
//! <destination prefix>/<github_id>/<session_id>/logs/<crate>/<relative file>
//! ```
//!
//! The destination prefix comes from the URI (`s3://bucket/PREFIX`); everything
//! after it is built by [`DrainTarget::logs_prefix`]. Per-target reference
//! documentation: `docs/reference/log-drain.md`.
//!
//! # What the caller owns
//!
//! - **Identity.** [`DrainTarget`] is supplied, never resolved here. An empty
//!   `github_id` or `session_id` is refused with
//!   [`DrainError::MissingIdentity`] rather than defaulted.
//! - **Single-flight.** [`run_once`] is exactly what its name says: one pass,
//!   no locking. Two concurrent runs against one target will both upload and
//!   both rewrite the manifest, and the loser's entries are lost. The scheduler
//!   Phase 3 adds owns that mutual exclusion.
//! - **The secret list.** [`crate::credentials::scrub_secrets`] removes
//!   values it is GIVEN; it does not detect secret-shaped strings. A caller that
//!   passes an empty list gets no scrubbing.

mod collector;
mod destination;
mod error;
mod manifest;
mod uri;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

pub use collector::{
    Collected, CollectedFile, DEFAULT_MAX_FILE_BYTES, Level, LogSource, OversizeFile, collect,
};
pub use destination::{LIST_LIMIT, LogDestination, ObjectMeta, ObjectStoreDestination, PutMeta};
pub use error::DrainError;
pub use manifest::{
    DrainManifest, MANIFEST_FILENAME, MANIFEST_VERSION, ManifestEntry, StatDecision,
};
pub use uri::{DestinationScheme, DestinationUri};

/// Who and what a drain run is uploading for.
///
/// Why: the two components are the whole key namespace below the destination
/// prefix, and getting either wrong mixes one user's logs into another's. They
/// are the caller's to supply because the core has no business shelling out to
/// `gh` — Phase 3 owns identity resolution and passes the answer in.
/// What: [`DrainTarget::validate`] refuses an empty component, so no code path
/// can produce a key containing `//` or an anonymous segment.
/// Test: `tests::run_once_refuses_empty_github_id`,
/// `tests::run_once_refuses_empty_session_id`, `tests::key_layout_shape`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainTarget {
    /// GitHub login the logs belong to. Never resolved here.
    pub github_id: String,
    /// The consumer's own session id. Opaque to the drain.
    pub session_id: String,
}

impl DrainTarget {
    /// Refuse an empty identity component.
    ///
    /// Fail-closed by design: see [`DrainError::MissingIdentity`].
    ///
    /// # Errors
    /// [`DrainError::MissingIdentity`] naming the empty field.
    pub fn validate(&self) -> Result<(), DrainError> {
        if self.github_id.trim().is_empty() {
            return Err(DrainError::MissingIdentity { field: "github_id" });
        }
        if self.session_id.trim().is_empty() {
            return Err(DrainError::MissingIdentity {
                field: "session_id",
            });
        }
        Ok(())
    }

    /// The `logs/` prefix every object for this target sits beneath.
    ///
    /// Destination-relative: the adapter joins the URI's own prefix on top.
    /// Test: `tests::key_layout_shape`.
    pub fn logs_prefix(&self) -> String {
        format!("{}/{}/logs", self.github_id, self.session_id)
    }

    /// Full key for one collected file's `relative_key`.
    pub fn object_key(&self, relative_key: &str) -> String {
        format!("{}/{}", self.logs_prefix(), relative_key)
    }

    /// Key of this target's manifest object.
    pub fn manifest_key(&self) -> String {
        format!("{}/{}", self.logs_prefix(), MANIFEST_FILENAME)
    }

    /// Path segment identifying this target inside the local state directory.
    fn cache_key(&self) -> String {
        format!("{}/{}", self.github_id, self.session_id)
    }
}

/// Everything a run needs that is not the destination, target, or sources.
///
/// `#[non_exhaustive]` so Phase 3 can add scheduling knobs without a breaking
/// change; construct it with [`DrainConfig::new`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DrainConfig {
    /// Where the local manifest cache lives.
    pub state_dir: PathBuf,
    /// Values [`collect`] removes from every body before upload.
    pub secrets: Vec<String>,
    /// Files larger than this are skipped, never truncated.
    pub max_file_bytes: u64,
}

impl DrainConfig {
    /// A config with [`DEFAULT_MAX_FILE_BYTES`] and no secrets.
    ///
    /// A caller that leaves `secrets` empty gets no scrubbing — see the module
    /// docs.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            secrets: Vec::new(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }

    /// Set the values to scrub from every uploaded body.
    #[must_use]
    pub fn with_secrets(mut self, secrets: Vec<String>) -> Self {
        self.secrets = secrets;
        self
    }

    /// Set the per-file size ceiling.
    #[must_use]
    pub fn with_max_file_bytes(mut self, max: u64) -> Self {
        self.max_file_bytes = max;
        self
    }
}

/// What one [`run_once`] pass did.
///
/// Why: the scheduler Phase 3 adds decides whether to back off from these
/// counts, and an operator reading a doctor check needs to tell "nothing
/// changed" from "everything failed". Both are zero uploads.
/// What: four counters, two byte totals, and the per-file errors that did not
/// abort the batch.
/// Test: `tests::run_once_end_to_end`, `tests::run_once_collects_per_file_errors_without_aborting`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DrainReport {
    /// Files uploaded this pass.
    pub uploaded: usize,
    /// Files skipped because the manifest already had them.
    pub skipped_unchanged: usize,
    /// Files skipped for exceeding `max_file_bytes`.
    pub skipped_too_large: usize,
    /// Total plaintext bytes of everything uploaded.
    pub bytes_plain: u64,
    /// Total gzipped bytes actually written to the destination.
    pub bytes_wire: u64,
    /// Per-file failures, as `(key or path, message)`. Never aborts the batch.
    pub errors: Vec<(String, String)>,
}

/// Run the drain once: collect, compare against the manifest, upload what changed.
///
/// Why: one entry point means the ordering — validate identity, load manifest,
/// collect, filter by manifest, upload, rewrite manifest — is fixed rather than
/// reassembled per caller.
///
/// What: validates `target` FIRST, so a bad identity costs no filesystem walk
/// and can never reach a `put`. Loads the manifest (remote authoritative, local
/// cache as fallback). Collects every matching file. For each: the stat-only
/// fast path skips unchanged files without comparing digests; a file whose stat
/// moved but whose SHA-256 matches is still skipped, with its manifest entry
/// refreshed so the next run takes the fast path. Everything else is uploaded.
/// A per-file failure is recorded in [`DrainReport::errors`] and the batch
/// CONTINUES — one unreadable file must not strand every other log on the
/// machine. The manifest is rewritten once at the end, reflecting only what
/// actually landed.
///
/// Single-flight is the CALLER's responsibility; see the module docs.
///
/// Test: `tests::run_once_end_to_end`, `tests::run_once_is_idempotent`,
/// `tests::run_once_reuploads_a_mutated_file`, `tests::run_once_refuses_empty_github_id`.
///
/// # Errors
/// - [`DrainError::MissingIdentity`] when `target` has an empty component.
/// - [`DrainError::Uri`] for a malformed include glob.
/// - [`DrainError::Transport`] when the manifest itself could not be read or
///   written. A per-FILE transport failure is collected, not raised.
pub async fn run_once(
    cfg: &DrainConfig,
    dest: &dyn LogDestination,
    target: &DrainTarget,
    sources: &[LogSource],
) -> Result<DrainReport, DrainError> {
    // #6533: fail closed before anything touches the filesystem or the network.
    target.validate()?;

    let manifest_key = target.manifest_key();
    let cache_key = target.cache_key();
    let mut manifest = DrainManifest::load(dest, &cfg.state_dir, &manifest_key, &cache_key).await?;

    let collected = collect(sources, &cfg.secrets, cfg.max_file_bytes)?;

    let mut report = DrainReport {
        skipped_too_large: collected.oversize.len(),
        ..DrainReport::default()
    };
    for (path, message) in collected.errors {
        report.errors.push((path.display().to_string(), message));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut manifest_dirty = false;

    for file in collected.files {
        // Fast path: stat alone says nothing changed, so never compare digests.
        if manifest.decide(&file.relative_key, file.plaintext_len, file.mtime_unix)
            == StatDecision::SkipUnchanged
        {
            report.skipped_unchanged += 1;
            continue;
        }

        // Slow path: the stat moved. SHA-256 decides, and it wins over mtime —
        // identical bytes are never re-uploaded just because a timestamp moved.
        if manifest.digest_matches(&file.relative_key, &file.sha256_plaintext) {
            report.skipped_unchanged += 1;
            manifest.record(entry_for(&file, &now));
            manifest_dirty = true;
            continue;
        }

        let key = target.object_key(&file.relative_key);
        let wire_len = file.body.len() as u64;
        match dest
            .put(&key, file.body.clone(), PutMeta::gzipped_text())
            .await
        {
            Ok(()) => {
                report.uploaded += 1;
                report.bytes_plain += file.plaintext_len;
                report.bytes_wire += wire_len;
                manifest.record(entry_for(&file, &now));
                manifest_dirty = true;
            }
            Err(e) => report.errors.push((key, e.to_string())),
        }
    }

    if manifest_dirty {
        manifest
            .save(dest, &cfg.state_dir, &manifest_key, &cache_key)
            .await?;
    }

    Ok(report)
}

/// Build the manifest entry recording one collected file.
fn entry_for(file: &CollectedFile, uploaded_at: &str) -> ManifestEntry {
    ManifestEntry {
        relative_file: file.relative_key.clone(),
        size: file.plaintext_len,
        mtime_unix: file.mtime_unix,
        sha256: file.sha256_plaintext.clone(),
        uploaded_at: uploaded_at.to_string(),
    }
}
