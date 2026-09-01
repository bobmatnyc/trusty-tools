//! Reading log files and preparing them for upload (#6533).
//!
//! Why: everything that must happen to a log line before it leaves the machine
//! happens here, in one order, once. Splitting the level filter, the secret
//! scrub, and the compression across call sites is how a body eventually
//! reaches a bucket having skipped one of them.
//! What: [`collect`] walks each [`LogSource`]'s root, matches its globs, and
//! for every file: filters by level, scrubs secrets, gzips, and yields a
//! [`CollectedFile`]. The SHA-256 it reports is over the PLAINTEXT bytes as
//! read, before filtering — so the manifest's identity tracks the source file,
//! not the drain's own processing, and a change to the filter never invalidates
//! every recorded digest.
//! Test: `super::tests::collect_filters_below_info`,
//! `super::tests::collect_passes_through_non_tracing`,
//! `super::tests::collect_scrubs_secrets_before_they_reach_the_destination`, `super::tests::collect_skips_oversize`.

use std::io::Write;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use globset::{Glob, GlobSetBuilder};
use sha2::{Digest, Sha256};

use super::error::DrainError;

/// Default ceiling on a single source file, in bytes.
///
/// 64 MiB is well above the trusty daemons' daily rolled logs and well below
/// anything that would embarrass a machine holding one plaintext copy, one
/// scrubbed copy, and one gzip buffer at once.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Log levels the drain can filter on, ordered least to most severe.
///
/// Deliberately not `tracing::Level`: the drain reads level names out of
/// already-written TEXT, and coupling that string parsing to `tracing`'s type
/// would make the drain depend on the crate that produced the file rather than
/// on the file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Level {
    /// `TRACE`
    Trace,
    /// `DEBUG`
    Debug,
    /// `INFO`
    Info,
    /// `WARN`
    Warn,
    /// `ERROR`
    Error,
}

impl Level {
    /// Map a bare level token to its variant.
    fn parse(token: &str) -> Option<Self> {
        match token {
            "TRACE" => Some(Self::Trace),
            "DEBUG" => Some(Self::Debug),
            "INFO" => Some(Self::Info),
            "WARN" | "WARNING" => Some(Self::Warn),
            "ERROR" => Some(Self::Error),
            _ => None,
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

/// A source file the collector declined to read.
#[derive(Debug, Clone)]
pub struct OversizeFile {
    /// The path that was skipped.
    pub path: PathBuf,
    /// Its size in bytes.
    pub size: u64,
}

/// What one [`collect`] pass found.
#[derive(Debug, Default)]
pub struct Collected {
    /// Files read and ready to upload.
    pub files: Vec<CollectedFile>,
    /// Files skipped for exceeding `max_file_bytes`.
    pub oversize: Vec<OversizeFile>,
    /// Per-file failures. A failure here never aborts the pass.
    pub errors: Vec<(PathBuf, String)>,
}

/// Enumerate, read, filter, scrub, and compress every matching log file.
///
/// Why: see the module docs — one ordered pipeline, so no body can reach a
/// destination having skipped the scrub.
/// What: for each source, compiles its globs once, walks `root`, and processes
/// each matching file. Files larger than `max_file_bytes` are SKIPPED with a
/// `warn!` and an [`OversizeFile`] entry rather than streamed in chunks: the
/// scrub has to see a whole body to catch a secret that straddles a chunk
/// boundary, so a chunked path could only ever upload partially-scrubbed text.
/// Refusing to upload is the safe half of that trade, and the report says so
/// out loud rather than silently truncating.
/// `secrets` is passed straight to [`crate::credentials::scrub_secrets`],
/// which ignores needles under its own minimum length.
/// Test: `super::tests::collect_skips_oversize`, `super::tests::collect_scrubs_secrets_before_they_reach_the_destination`.
///
/// # Errors
/// Returns [`DrainError::Uri`] only for a malformed glob pattern, which is a
/// caller bug rather than a runtime condition. Per-file IO failures land in
/// [`Collected::errors`] and do not stop the pass.
pub fn collect(
    sources: &[LogSource],
    secrets: &[String],
    max_file_bytes: u64,
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
            process_file(source, path, relative, secrets, max_file_bytes, &mut out);
        }
    }

    Ok(out)
}

/// Read one matched file into `out`, recording an error rather than failing.
fn process_file(
    source: &LogSource,
    path: &Path,
    relative: &Path,
    secrets: &[String],
    max_file_bytes: u64,
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
    if size > max_file_bytes {
        // #6533: skip, never truncate — see `collect`'s docs for why a chunked
        // path cannot scrub a secret that straddles a boundary.
        tracing::warn!(
            path = %path.display(),
            size,
            max_file_bytes,
            "log-drain skipping oversize file; it will not be uploaded"
        );
        out.oversize.push(OversizeFile {
            path: path.to_path_buf(),
            size,
        });
        return;
    }

    let plaintext = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            out.errors.push((path.to_path_buf(), e.to_string()));
            return;
        }
    };

    let sha256_plaintext = hex_digest(&plaintext);
    let mtime_unix = mtime_seconds(&metadata);

    // Order is load-bearing: decode, then filter, then scrub, then compress.
    let text = String::from_utf8_lossy(&plaintext);
    let filtered = match source.level_filter {
        Some(min) => filter_by_level(&text, min),
        None => text.into_owned(),
    };
    let scrubbed = crate::credentials::scrub_secrets(&filtered, secrets);

    let body = match gzip(scrubbed.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            out.errors.push((path.to_path_buf(), e.to_string()));
            return;
        }
    };

    out.files.push(CollectedFile {
        relative_key: format!("{}/{}", source.crate_name, relative.to_string_lossy()),
        body: Bytes::from(body),
        sha256_plaintext,
        plaintext_len: size,
        mtime_unix,
        source_path: path.to_path_buf(),
    });
}

/// Drop lines below `min`, leaving non-tracing files untouched.
///
/// Why: a daemon's DEBUG output is the bulk of its log bytes and almost never
/// the reason anyone reads it later. Dropping it before compression is where
/// the drain's egress saving actually comes from.
/// What: recognises the `tracing_subscriber::fmt` default line shape —
/// `<timestamp> <LEVEL> <target>: <message>` — and keeps a line when its level
/// is at or above `min`. A line carrying no recognisable level is a
/// CONTINUATION (a wrapped message, a backtrace frame) and inherits the
/// disposition of the line above it. If the file contains no recognisable
/// level line at all, it is not tracing output and is returned VERBATIM rather
/// than filtered to nothing.
/// Test: `super::tests::collect_filters_below_info`,
/// `super::tests::collect_passes_through_non_tracing`.
fn filter_by_level(text: &str, min: Level) -> String {
    let mut saw_any_level = false;
    let mut keeping = true;
    let mut kept = String::with_capacity(text.len());

    for line in text.split_inclusive('\n') {
        // No level token means a continuation line, which keeps whatever
        // disposition the line above it had — hence no `else` branch.
        if let Some(level) = line_level(line) {
            saw_any_level = true;
            keeping = level >= min;
        }
        if keeping {
            kept.push_str(line);
        }
    }

    if saw_any_level {
        kept
    } else {
        text.to_string()
    }
}

/// Extract the level from a `tracing_subscriber::fmt` line, if it has one.
///
/// Scans the first few whitespace-separated tokens with ANSI escapes stripped,
/// so a colourised log (`fmt` with `with_ansi(true)`, which the console layer
/// uses) is recognised the same as a plain file appender's output.
fn line_level(line: &str) -> Option<Level> {
    let plain = strip_ansi(line);
    // The level is the second token in the default format; allow a little slack
    // for a prefixed thread name or span without scanning the whole message.
    plain
        .split_whitespace()
        .take(4)
        .find_map(|token| Level::parse(token.trim_matches(|c: char| !c.is_ascii_alphabetic())))
}

/// Remove ANSI CSI escape sequences so level detection survives colour output.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // ESC [ … <final byte in @-~>. Consume through the terminator.
        if chars.next() != Some('[') {
            continue;
        }
        for tail in chars.by_ref() {
            if ('\u{40}'..='\u{7e}').contains(&tail) {
                break;
            }
        }
    }
    out
}

/// Gzip a body at the default compression level.
fn gzip(body: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body)?;
    encoder.finish()
}

/// Hex SHA-256 of a byte slice. Always 64 characters.
pub(super) fn hex_digest(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("{:x}", hasher.finalize())
}

/// Modification time in Unix seconds, or `0` when the platform withholds it.
fn mtime_seconds(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}
