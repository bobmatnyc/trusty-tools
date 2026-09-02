//! Upload trusty-* log files to object storage (#6533, epic phase 1+2).
//!
//! Why: every trusty daemon writes logs to a local path that nothing prunes and
//! nothing collects. Diagnosing a failure on someone else's machine means
//! asking them to find and send a file. This module is the piece that gets
//! those bytes somewhere durable, scrubbed of credentials, without re-uploading
//! what has not changed.
//!
//! What: the drain CORE — a [`LogDestination`](crate::log_drain::LogDestination)
//! trait with `s3://` and `file://` adapters, a
//! [`DestinationUri`](crate::log_drain::DestinationUri) parser, the key layout,
//! an idempotency [`DrainManifest`](crate::log_drain::DrainManifest), the
//! [`collect`](crate::log_drain::collect) pipeline, and
//! [`run_once`](crate::log_drain::run_once). Phase 1+2 is
//! the core ONLY: there is no scheduler, no config plumbing, and no consumer
//! wired up. Phase 3 adds those, plus project-identity resolution — this module
//! deliberately does not resolve an identity, it demands one.
//!
//! Test: `tests` (sibling module). Run it with
//! `cargo test -p trusty-common --features log-drain --no-fail-fast`.
//!
//! # Key layout
//!
//! ```text
//! <destination prefix>/<owner>/<project>/<crate>/<relative file>
//! ```
//!
//! The destination prefix comes from the URI (`s3://bucket/PREFIX`); everything
//! after it is built by
//! [`DrainTarget::key_prefix`](crate::log_drain::DrainTarget::key_prefix).
//! #6657 replaced the earlier `<github_id>/<session_id>/logs/…` layout;
//! objects already uploaded under it stay where they are, because the manifest
//! is keyed by destination and key. Per-target reference
//! documentation: `docs/reference/log-drain.md`.
//!
//! # What the caller owns
//!
//! - **Identity.** [`DrainTarget`](crate::log_drain::DrainTarget) is supplied,
//!   never resolved here. An empty `owner` or `project` is refused with
//!   [`DrainError::MissingIdentity`](crate::log_drain::DrainError::MissingIdentity)
//!   rather than defaulted.
//! - **Single-flight.** [`run_once`](crate::log_drain::run_once) is exactly what
//!   its name says: one pass,
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
mod pipeline;
mod uri;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

pub use collector::{
    CollectLimits, Collected, CollectedFile, DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_WIRE_BYTES, Level,
    LogSource, OversizeFile, collect,
};
pub use destination::{LIST_LIMIT, LogDestination, ObjectMeta, ObjectStoreDestination, PutMeta};
pub use error::DrainError;
pub use manifest::{
    DrainManifest, MANIFEST_FILENAME, MANIFEST_VERSION, ManifestEntry, ManifestOrigin, SkipReason,
    SkipRecord, StatDecision,
};
pub use uri::{DestinationScheme, DestinationUri};

/// Which project's logs a drain run is uploading.
///
/// Why: the two components are the whole key namespace below the destination
/// prefix, and getting either wrong files one project's logs under another's.
/// They are the caller's to supply because the core has no business shelling
/// out to git — the consumer resolves the identity and passes the answer in.
/// What: [`DrainTarget::validate`] refuses an empty component, so no code path
/// can produce a key containing `//` or an anonymous segment.
/// Test: `tests::run_once_refuses_an_empty_owner`,
/// `tests::run_once_refuses_an_empty_project`, `tests::key_layout_shape`.
///
/// #6657 replaced the previous `<github_id>/<session_id>` pair. A per-session
/// segment made every project on a host share one namespace keyed by whoever
/// ran the daemon; the owner and project of the repo the logs came from is what
/// an operator actually looks under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainTarget {
    /// Repository owner — the GitHub user or org, verbatim from the remote.
    pub owner: String,
    /// Repository name the logs belong to, verbatim from the remote.
    pub project: String,
}

impl DrainTarget {
    /// Refuse an empty identity component.
    ///
    /// Fail-closed by design: see [`DrainError::MissingIdentity`].
    ///
    /// # Errors
    /// [`DrainError::MissingIdentity`] naming the empty field.
    pub fn validate(&self) -> Result<(), DrainError> {
        if self.owner.trim().is_empty() {
            return Err(DrainError::MissingIdentity { field: "owner" });
        }
        if self.project.trim().is_empty() {
            return Err(DrainError::MissingIdentity { field: "project" });
        }
        Ok(())
    }

    /// The `<owner>/<project>` prefix every object for this target sits beneath.
    ///
    /// Destination-relative: the adapter joins the URI's own prefix on top.
    /// Test: `tests::key_layout_shape`.
    pub fn key_prefix(&self) -> String {
        format!("{}/{}", self.owner, self.project)
    }

    /// Full key for one collected file's `relative_key`.
    pub fn object_key(&self, relative_key: &str) -> String {
        format!("{}/{}", self.key_prefix(), relative_key)
    }

    /// Key of this target's manifest object.
    pub fn manifest_key(&self) -> String {
        format!("{}/{}", self.key_prefix(), MANIFEST_FILENAME)
    }

    /// Path segment identifying this target inside the local state directory.
    ///
    /// The TARGET half only. [`DrainManifest::cache_path`] puts the
    /// destination's namespace above it, so a record made for one destination
    /// is never read back for another (#6548).
    fn cache_key(&self) -> String {
        self.key_prefix()
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
    /// Plaintext source ceiling. Files over it are skipped, never truncated.
    pub max_file_bytes: u64,
    /// Compressed body ceiling (#6547). See [`DEFAULT_MAX_WIRE_BYTES`].
    pub max_wire_bytes: u64,
}

impl DrainConfig {
    /// A config with the default [`CollectLimits`] and no secrets.
    ///
    /// A caller that leaves `secrets` empty gets no scrubbing — see the module
    /// docs.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        let limits = CollectLimits::default();
        Self {
            state_dir: state_dir.into(),
            secrets: Vec::new(),
            max_file_bytes: limits.max_file_bytes,
            max_wire_bytes: limits.max_wire_bytes,
        }
    }

    /// Set the values to scrub from every uploaded body.
    #[must_use]
    pub fn with_secrets(mut self, secrets: Vec<String>) -> Self {
        self.secrets = secrets;
        self
    }

    /// Set the plaintext source ceiling.
    #[must_use]
    pub fn with_max_file_bytes(mut self, max: u64) -> Self {
        self.max_file_bytes = max;
        self
    }

    /// Set the compressed-body ceiling (#6547).
    #[must_use]
    pub fn with_max_wire_bytes(mut self, max: u64) -> Self {
        self.max_wire_bytes = max;
        self
    }

    /// The two bounds, as [`collect`] takes them.
    pub fn limits(&self) -> CollectLimits {
        CollectLimits::new(self.max_file_bytes, self.max_wire_bytes)
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
    /// Files skipped for exceeding one of the size bounds.
    pub skipped_too_large: usize,
    /// Of those, the ones whose decision was recorded for the FIRST time (#6547).
    ///
    /// A steady state where every oversize file has already been decided reads
    /// `skipped_too_large: 29, skips_recorded: 0` — which is the signal that no
    /// warning was logged this pass, and the difference between a backlog that
    /// is settled and one that keeps churning.
    pub skips_recorded: usize,
    /// Total plaintext bytes of everything uploaded.
    pub bytes_plain: u64,
    /// Total gzipped bytes actually written to the destination.
    pub bytes_wire: u64,
    /// Per-file failures, as `(key or path, message)`. Never aborts the batch.
    pub errors: Vec<(String, String)>,
    /// Sampled remote-manifest entries whose object was missing (#6548).
    ///
    /// 0 or 1 per run — one entry is sampled, not all of them. Non-zero means
    /// the destination's own manifest lists something the destination does not
    /// have, so every file it lists is being skipped rather than uploaded.
    /// Repair is documented in `docs/reference/log-drain.md`.
    pub manifest_spot_check_missing: usize,
}

/// Run the drain once: collect, compare against the manifest, upload what changed.
///
/// Why: one entry point means the ordering — validate identity, load manifest,
/// collect, filter by manifest, upload, rewrite manifest — is fixed rather than
/// reassembled per caller.
///
/// What: validates `target` FIRST, so a bad identity costs no filesystem walk
/// and can never reach a `put`. Loads the manifest (remote authoritative, local
/// cache as fallback, and the cache is scoped to the DESTINATION as well as the
/// target — see [`DrainManifest::cache_path`]). Collects every matching file.
/// A manifest that came from the destination gets one sampled
/// [`DrainManifest::spot_check`]. For each file: the stat-only
/// fast path skips unchanged files without comparing digests; a file whose stat
/// moved but whose SHA-256 matches is still skipped, with its manifest entry
/// refreshed so the next run takes the fast path. Everything else is uploaded.
/// A per-file failure is recorded in [`DrainReport::errors`] and the batch
/// CONTINUES — one unreadable file must not strand every other log on the
/// machine. The manifest is rewritten once at the end, reflecting only what
/// actually landed.
///
/// A file over one of the size bounds is decided ONCE and the decision is
/// written to the manifest as a [`SkipRecord`] (#6547); a later pass that sees
/// the same `(file, size, mtime)` counts it and says nothing. Any file the pass
/// CAN read has its skip record dropped, so raising a bound takes effect on the
/// next pass rather than needing the manifest cleared by hand.
///
/// Single-flight is the CALLER's responsibility; see the module docs.
///
/// Test: `tests::run_once_end_to_end`, `tests::run_once_is_idempotent`,
/// `tests::run_once_reuploads_a_mutated_file`, `tests::run_once_refuses_an_empty_owner`,
/// `tests::run_once_records_an_oversize_skip_once`,
/// `tests::run_once_re_evaluates_a_skip_when_the_file_changes`.
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
    let (mut manifest, origin) =
        DrainManifest::load_with_origin(dest, &cfg.state_dir, &manifest_key, &cache_key).await?;

    let collected = collect(sources, &cfg.secrets, cfg.limits())?;

    let mut report = DrainReport {
        skipped_too_large: collected.oversize.len(),
        ..DrainReport::default()
    };
    for (path, message) in collected.errors {
        report.errors.push((path.display().to_string(), message));
    }

    // #6548: a manifest written before the cache-keying fix can list objects
    // this destination never received, and every one of them then skips
    // forever. One sampled `head` turns that into a warning an operator sees.
    if origin == ManifestOrigin::Remote
        && let Some(missing) = manifest.spot_check(dest, &target.key_prefix()).await
    {
        report.manifest_spot_check_missing += 1;
        tracing::warn!(
            key = %missing,
            manifest = %manifest_key,
            "log-drain manifest lists an object this destination does not have; \
             delete the manifest object to force a full re-upload (see #6548)"
        );
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut manifest_dirty = false;

    // #6547: decide each oversize file ONCE. A file that has already been
    // recorded at this exact size and mtime cannot have changed its answer, so
    // it is counted and passed over in silence rather than warned about again
    // every cycle.
    for over in &collected.oversize {
        if manifest.skip_recorded(&over.relative_key, over.size, over.mtime_unix) {
            tracing::debug!(
                path = %over.path.display(),
                size = over.size,
                "log-drain skip already recorded for this size and mtime"
            );
            continue;
        }
        tracing::warn!(
            path = %over.path.display(),
            size = over.size,
            limit = over.reason.limit_name(),
            "log-drain is not uploading this file; the decision is recorded in the \
             manifest and is not logged again until the file's size or mtime changes"
        );
        manifest.record_skip(SkipRecord {
            relative_file: over.relative_key.clone(),
            size: over.size,
            mtime_unix: over.mtime_unix,
            reason: over.reason,
            decided_at: now.clone(),
        });
        report.skips_recorded += 1;
        manifest_dirty = true;
    }

    for file in collected.files {
        // A file the pass CAN read has no business carrying a skip record —
        // raising a bound, or a rotation that shrank it, makes it drainable.
        if manifest.forget_skip(&file.relative_key) {
            manifest_dirty = true;
        }

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
