//! Finding log files and handing each one to the streaming pipeline (#6533).
//!
//! Why: everything that must happen to a log line before it leaves the machine
//! happens in one order, once. Splitting the level filter, the secret scrub,
//! and the compression across call sites is how a body eventually reaches a
//! bucket having skipped one of them.
//! What: [`collect`] walks each [`LogSource`]'s root, matches its globs, and
//! hands every match to [`super::pipeline::stream_file`], which reads it in
//! bounded chunks. The SHA-256 it reports is over the PLAINTEXT bytes as read,
//! before filtering — so the manifest's identity tracks the source file, not
//! the drain's own processing, and a change to the filter never invalidates
//! every recorded digest.
//! Test: `super::tests::collect_filters_below_info`,
//! `super::tests::collect_passes_through_non_tracing`,
//! `super::tests::collect_scrubs_secrets_before_they_reach_the_destination`,
//! `super::tests::collect_skips_oversize`.
//!
//! # The ceiling is a cost decision, not a memory one (#6547)
//!
//! Before #6547 a file over `max_file_bytes` was skipped because the collector
//! read it whole; five copies of a 176 MB log is not a working set a daemon can
//! take. [`super::pipeline::stream_file`] removed that constraint, so the
//! DEFAULT ceiling moved from 64 MiB to [`DEFAULT_MAX_FILE_BYTES`] — high
//! enough that no daily-rotated daemon log reaches it. What survives is an
//! operator-facing guard against spending a pass on an absurd file, plus
//! [`CollectLimits::max_wire_bytes`], which bounds the one thing still held
//! whole: the COMPRESSED body `LogDestination::put` takes.
//!
//! A file that does trip either bound is reported as an [`OversizeFile`] and
//! the decision is recorded in the manifest by [`super::run_once`], so it is
//! made ONCE per `(file, size, mtime)` rather than re-logged every cycle.

use std::path::{Path, PathBuf};

use bytes::Bytes;

use globset::{Glob, GlobSetBuilder};

use super::error::DrainError;
use super::manifest::SkipReason;
use super::pipeline::{StreamOutcome, stream_file};

pub use super::pipeline::Level;

/// Default ceiling on a single source file, in bytes.
///
/// 4 GiB. Since #6547 the collector streams, so this no longer bounds memory —
/// it bounds how much reading and hashing one pass will spend on one file. No
/// daily-rotated trusty daemon log has come within two orders of magnitude of
/// it; the largest observed was 176 MB.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Default ceiling on one file's COMPRESSED body, in bytes.
///
/// 64 MiB — the number the source ceiling used to carry, moved to where memory
/// is actually spent. `LogDestination::put` takes an in-memory `Bytes`, so the
/// gzip output is the one buffer that still scales with the file. At the ~20x
/// ratio daemon log text compresses at, this admits well over a gigabyte of
/// source.
pub const DEFAULT_MAX_WIRE_BYTES: u64 = 64 * 1024 * 1024;

/// The two size bounds one [`collect`] pass enforces.
///
/// Why: two adjacent `u64` parameters are two parameters waiting to be
/// transposed, and the pair grew from one at #6547. Naming them makes a call
/// site say which bound it is setting.
/// What: `max_file_bytes` bounds the plaintext source, `max_wire_bytes` the
/// compressed body. Either being exceeded yields an [`OversizeFile`], never a
/// truncated upload.
/// Test: `super::tests::collect_skips_oversize`,
/// `super::tests::collect_skips_a_body_over_the_wire_cap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CollectLimits {
    /// Plaintext source ceiling. See [`DEFAULT_MAX_FILE_BYTES`].
    pub max_file_bytes: u64,
    /// Compressed body ceiling. See [`DEFAULT_MAX_WIRE_BYTES`].
    pub max_wire_bytes: u64,
}

impl Default for CollectLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_wire_bytes: DEFAULT_MAX_WIRE_BYTES,
        }
    }
}

impl CollectLimits {
    /// Both bounds, in bytes.
    pub fn new(max_file_bytes: u64, max_wire_bytes: u64) -> Self {
        Self {
            max_file_bytes,
            max_wire_bytes,
        }
    }
}

/// One directory of log files to drain, and how to read them.
///
/// Why: each producing crate writes a different shape into a different place.
/// Naming the crate here is what puts its files under their own key segment, so
/// two crates' `daemon.log` never collide.
/// What: `root` is walked, `include` globs are matched against each path
/// RELATIVE to `root`, and `level_filter` — when set — drops lines below that
/// level in files the drain recognises as `tracing_subscriber::fmt` output.
/// Test: `super::tests::collect_filters_below_info`.
#[derive(Debug, Clone)]
pub struct LogSource {
    /// Producing crate, e.g. `trusty-mpm`. Becomes a key segment.
    pub crate_name: String,
    /// Directory to walk.
    pub root: PathBuf,
    /// Glob patterns, matched against paths relative to `root`.
    pub include: Vec<String>,
    /// Drop lines below this level. `None` uploads every line.
    pub level_filter: Option<Level>,
}

/// One file, read and ready to upload.
#[derive(Debug, Clone)]
pub struct CollectedFile {
    /// `<crate>/<relative path>` — the key suffix beneath the target's `logs/`.
    pub relative_key: String,
    /// Gzipped, scrubbed, level-filtered body.
    pub body: Bytes,
    /// Hex SHA-256 of the PLAINTEXT source bytes as read from disk.
    pub sha256_plaintext: String,
    /// Size in bytes of the plaintext source file.
    pub plaintext_len: u64,
    /// Source file's modification time, Unix seconds.
    pub mtime_unix: i64,
    /// Where it was read from, for error reporting.
    pub source_path: PathBuf,
}

/// A source file the collector declined to upload.
///
/// Why: `path` and `size` were enough to log a warning, and logging a warning
/// every cycle for a file that can never change its answer is exactly what
/// #6547 is about. The extra three fields are what [`super::run_once`] needs to
/// record the decision durably: the key it is recorded under, the mtime half of
/// the identity that invalidates it, and which bound was hit.
/// Test: `super::tests::run_once_records_an_oversize_skip_once`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OversizeFile {
    /// The path that was skipped.
    pub path: PathBuf,
    /// `<crate>/<relative path>` — the manifest's identity for this file.
    pub relative_key: String,
    /// Its size in bytes.
    pub size: u64,
    /// Source file's modification time, Unix seconds.
    pub mtime_unix: i64,
    /// Which bound the file hit.
    pub reason: SkipReason,
}

/// What one [`collect`] pass found.
#[derive(Debug, Default)]
pub struct Collected {
    /// Files read and ready to upload.
    pub files: Vec<CollectedFile>,
    /// Files skipped for exceeding one of the [`CollectLimits`].
    pub oversize: Vec<OversizeFile>,
    /// Per-file failures. A failure here never aborts the pass.
    pub errors: Vec<(PathBuf, String)>,
}

/// Enumerate every matching log file and stream each one into an upload body.
///
/// Why: see the module docs — one ordered pipeline, so no body can reach a
/// destination having skipped the scrub.
/// What: for each source, compiles its globs once, walks `root`, and hands each
/// matching file to [`super::pipeline::stream_file`]. A file over
/// [`CollectLimits::max_file_bytes`] is never opened; one whose compressed body
/// passes [`CollectLimits::max_wire_bytes`] is abandoned mid-stream. Both land
/// in [`Collected::oversize`] rather than being truncated, and neither logs
/// here — [`super::run_once`] owns that, because only it can tell a new
/// decision from one already recorded (#6547).
/// `secrets` is passed straight to [`crate::credentials::scrub_secrets`],
/// which ignores needles under its own minimum length.
/// Test: `super::tests::collect_skips_oversize`,
/// `super::tests::collect_scrubs_secrets_before_they_reach_the_destination`.
///
/// # Errors
/// Returns [`DrainError::Uri`] only for a malformed glob pattern, which is a
/// caller bug rather than a runtime condition. Per-file IO failures land in
/// [`Collected::errors`] and do not stop the pass.
pub fn collect(
    sources: &[LogSource],
    secrets: &[String],
    limits: CollectLimits,
) -> Result<Collected, DrainError> {
    let mut out = Collected::default();

    for source in sources {
        let mut builder = GlobSetBuilder::new();
        for pattern in &source.include {
            let glob = Glob::new(pattern).map_err(|e| DrainError::Uri {
                uri: pattern.clone(),
                reason: format!("invalid include glob: {e}"),
            })?;
            builder.add(glob);
        }
        let globs = builder.build().map_err(|e| DrainError::Uri {
            uri: source.include.join(","),
            reason: format!("could not compile include globs: {e}"),
        })?;

        for entry in walkdir::WalkDir::new(&source.root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(&source.root) else {
                continue;
            };
            if !globs.is_match(relative) {
                continue;
            }
            process_file(source, path, relative, secrets, limits, &mut out);
        }
    }

    Ok(out)
}

/// Stream one matched file into `out`, recording an error rather than failing.
fn process_file(
    source: &LogSource,
    path: &Path,
    relative: &Path,
    secrets: &[String],
    limits: CollectLimits,
    out: &mut Collected,
) {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            out.errors.push((path.to_path_buf(), e.to_string()));
            return;
        }
    };

    let size = metadata.len();
    let mtime_unix = mtime_seconds(&metadata);
    let relative_key = format!("{}/{}", source.crate_name, relative.to_string_lossy());

    // #6547: no `warn!` on either arm. `run_once` decides whether the skip is
    // news, because only it can see the manifest's record of the last answer.
    let reason = if size > limits.max_file_bytes {
        SkipReason::SourceTooLarge
    } else {
        match stream_file(path, source.level_filter, secrets, limits.max_wire_bytes) {
            Ok(StreamOutcome::Body {
                body,
                sha256_plaintext,
            }) => {
                out.files.push(CollectedFile {
                    relative_key,
                    body,
                    sha256_plaintext,
                    plaintext_len: size,
                    mtime_unix,
                    source_path: path.to_path_buf(),
                });
                return;
            }
            Ok(StreamOutcome::CompressedTooLarge) => SkipReason::CompressedTooLarge,
            Err(e) => {
                out.errors.push((path.to_path_buf(), e.to_string()));
                return;
            }
        }
    };

    out.oversize.push(OversizeFile {
        path: path.to_path_buf(),
        relative_key,
        size,
        mtime_unix,
        reason,
    });
}

/// Modification time in Unix seconds, or `0` when the platform withholds it.
fn mtime_seconds(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}
