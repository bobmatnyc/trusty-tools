//! Streaming one log file from disk into an upload-ready body (#6547).
//!
//! Why: the drain used to `std::fs::read` a whole file, decode it, filter it,
//! scrub it, and gzip it — five live copies of the same bytes. That is the only
//! reason a size ceiling existed, and the ceiling is what left 29 days of
//! daemon logs permanently undrained (#6547). Reading in bounded chunks makes
//! peak memory a function of the CHUNK size and the COMPRESSED body, never of
//! the source file, so a 176 MB log costs the same working set as a 1 MB one.
//!
//! What: [`stream_file`] reads `READ_CHUNK_BYTES` at a time, hashes the raw
//! bytes, and pushes each chunk through the same fixed order the whole-file
//! path used — decode, level-filter, scrub, gzip — via [`LineFilter`] and
//! [`ScrubCarry`], which carry exactly enough state across a chunk boundary to
//! make the streamed result identical to the buffered one.
//!
//! Test: `super::tests::stream_matches_the_buffered_pipeline`,
//! `super::tests::stream_scrubs_a_secret_straddling_a_chunk_boundary`,
//! `super::tests::collect_filters_below_info`.
//!
//! # Why chunking is safe for the scrub (#6534 revisited)
//!
//! The original refusal to chunk was correct about the hazard: a needle that
//! straddles a chunk boundary is not found by scrubbing each chunk alone. It is
//! wrong that the hazard forces a whole-file read. [`ScrubCarry`] holds back the
//! last `max needle length - 1` bytes of every scrubbed chunk and prepends them
//! to the next one, so every occurrence is scrubbed in a window that contains it
//! whole. Nothing is emitted until it can no longer participate in a
//! boundary-crossing match.

use std::io::{Read, Write};
use std::path::Path;

use bytes::Bytes;
use sha2::{Digest, Sha256};

/// Plaintext held at once while streaming one file.
///
/// 1 MiB is large enough that a 176 MB log costs ~170 read syscalls and small
/// enough that the working set is invisible beside the daemon's own heap.
const READ_CHUNK_BYTES: usize = 1024 * 1024;

/// Hard cap on bytes buffered while waiting for a line terminator.
///
/// A log with no newline in 8 MiB is not line-oriented output, and buffering it
/// whole would reintroduce exactly the unbounded read this module removes. Past
/// this the chunk is flushed at a UTF-8 boundary; the level filter treats the
/// remainder as a continuation line, which is the same disposition a wrapped
/// message already gets.
const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

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

/// What streaming one file produced.
pub(super) enum StreamOutcome {
    /// The finished gzip body and the plaintext digest.
    Body {
        /// Gzipped, scrubbed, level-filtered body.
        body: Bytes,
        /// Hex SHA-256 of the plaintext source bytes as read.
        sha256_plaintext: String,
    },
    /// The compressed body passed `max_wire_bytes` before the file ran out.
    ///
    /// The only remaining bound on the streamed path: `put` takes an in-memory
    /// `Bytes`, so the COMPRESSED body is the one thing still held whole.
    CompressedTooLarge,
}

/// Read, filter, scrub, and compress one file without ever holding it whole.
///
/// Why: see the module docs — this is what makes the size ceiling a cost
/// decision rather than a memory one.
/// What: streams `path` in [`READ_CHUNK_BYTES`] chunks split on the last line
/// terminator in each read, so every chunk is whole-line and whole-UTF-8. The
/// SHA-256 is over the RAW bytes, unchanged from the buffered path, so a
/// manifest written before this change stays valid. Stops early with
/// [`StreamOutcome::CompressedTooLarge`] once the gzip output passes
/// `max_wire_bytes`, rather than growing a buffer without limit.
/// Test: `super::tests::stream_matches_the_buffered_pipeline`,
/// `super::tests::stream_scrubs_a_secret_straddling_a_chunk_boundary`.
///
/// # Errors
/// Any `std::io::Error` from opening, reading, or finishing the gzip stream.
pub(super) fn stream_file(
    path: &Path,
    level_filter: Option<Level>,
    secrets: &[String],
    max_wire_bytes: u64,
) -> std::io::Result<StreamOutcome> {
    let mut reader =
        std::io::BufReader::with_capacity(READ_CHUNK_BYTES, std::fs::File::open(path)?);
    let mut hasher = Sha256::new();
    let mut encoder =
        flate2::write::GzEncoder::new(Vec::<u8>::new(), flate2::Compression::default());
    let mut filter = LineFilter::new(level_filter);
    let mut scrub = ScrubCarry::new(secrets);

    let mut raw = vec![0u8; READ_CHUNK_BYTES];
    let mut pending: Vec<u8> = Vec::with_capacity(READ_CHUNK_BYTES);

    loop {
        let read = reader.read(&mut raw)?;
        if read == 0 {
            break;
        }
        hasher.update(&raw[..read]);
        pending.extend_from_slice(&raw[..read]);

        let Some(split) = flush_point(&pending) else {
            continue;
        };
        let chunk: Vec<u8> = pending.drain(..split).collect();
        encoder.write_all(scrub.push(&filter.push(&decode(&chunk))).as_bytes())?;
        if encoder.get_ref().len() as u64 > max_wire_bytes {
            return Ok(StreamOutcome::CompressedTooLarge);
        }
    }

    if !pending.is_empty() {
        encoder.write_all(scrub.push(&filter.push(&decode(&pending))).as_bytes())?;
    }
    // The carry is whatever could still have been the head of a straddling
    // needle. Nothing follows it, so it is emitted as-is.
    encoder.write_all(scrub.finish().as_bytes())?;

    let body = encoder.finish()?;
    if body.len() as u64 > max_wire_bytes {
        return Ok(StreamOutcome::CompressedTooLarge);
    }
    Ok(StreamOutcome::Body {
        body: Bytes::from(body),
        sha256_plaintext: format!("{:x}", hasher.finalize()),
    })
}

/// Hex SHA-256 of a byte slice. Always 64 characters.
pub(super) fn hex_digest(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("{:x}", hasher.finalize())
}

/// How many bytes of `pending` are safe to process now, or `None` to read more.
///
/// Prefers the byte after the last line terminator, so a chunk is always whole
/// lines and therefore whole UTF-8. Falls back to a character boundary once
/// [`MAX_PENDING_BYTES`] is passed with no terminator in sight.
fn flush_point(pending: &[u8]) -> Option<usize> {
    if let Some(last) = pending.iter().rposition(|b| *b == b'\n') {
        return Some(last + 1);
    }
    if pending.len() < MAX_PENDING_BYTES {
        return None;
    }
    // Defer any byte that could belong to a character whose tail has not been
    // read: continuation bytes are `10xxxxxx`, lead bytes `11xxxxxx`.
    let mut end = pending.len();
    let floor = end.saturating_sub(4);
    while end > floor && (pending[end - 1] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    if end > floor && pending[end - 1] >= 0b1100_0000 {
        end -= 1;
    }
    Some(end)
}

/// Decode one whole-line chunk, replacing invalid sequences.
fn decode(chunk: &[u8]) -> String {
    String::from_utf8_lossy(chunk).into_owned()
}

/// The level filter, as a value that survives a chunk boundary.
///
/// Why: the whole-file filter carried two pieces of state across lines —
/// whether the current line's disposition is "keep", and whether the file has
/// shown any recognisable level at all. Only the first is real: a file with NO
/// level line never flips `keeping` away from its initial `true`, so every line
/// is kept and the "return verbatim" fallback the buffered version wrote out
/// explicitly was already a no-op. Streaming keeps only the state that matters.
/// What: [`LineFilter::push`] evaluates each whole line of a chunk and keeps it
/// when its level is at or above the minimum. A line carrying no recognisable
/// level is a CONTINUATION — a wrapped message, a backtrace frame — and
/// inherits the disposition of the line above it, across a chunk boundary
/// included.
/// Test: `super::tests::collect_filters_below_info`,
/// `super::tests::collect_drops_continuation_of_a_dropped_line`,
/// `super::tests::collect_passes_through_non_tracing`.
struct LineFilter {
    min: Option<Level>,
    keeping: bool,
}

impl LineFilter {
    fn new(min: Option<Level>) -> Self {
        Self { min, keeping: true }
    }

    fn push(&mut self, text: &str) -> String {
        let Some(min) = self.min else {
            return text.to_string();
        };
        let mut kept = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            // No level token means a continuation line, which keeps whatever
            // disposition the line above it had — hence no `else` branch.
            if let Some(level) = line_level(line) {
                self.keeping = level >= min;
            }
            if self.keeping {
                kept.push_str(line);
            }
        }
        kept
    }
}

/// The scrub, as a value that survives a chunk boundary.
///
/// Why: a needle split across two chunks is found by neither, which is the
/// hazard that made #6534 refuse to chunk at all. See the module docs.
/// What: holds back the last `window` bytes of every scrubbed chunk — one less
/// than the longest needle — and prepends them to the next chunk. Any
/// occurrence starting before the emitted prefix's end lies entirely inside the
/// window that was just scrubbed; any occurrence starting inside the held-back
/// tail is scrubbed on the next push, where its continuation has arrived. With
/// no secrets the window is zero and the carry never allocates.
/// Test: `super::tests::stream_scrubs_a_secret_straddling_a_chunk_boundary`.
struct ScrubCarry {
    secrets: Vec<String>,
    window: usize,
    carry: String,
}

impl ScrubCarry {
    fn new(secrets: &[String]) -> Self {
        // An over-approximation of the longest needle is always safe: a wider
        // window only delays emission. `scrub_secrets` skips needles under its
        // own minimum length, so measuring every one costs nothing.
        let window = secrets
            .iter()
            .map(String::len)
            .max()
            .unwrap_or(0)
            .saturating_sub(1);
        Self {
            secrets: secrets.to_vec(),
            window,
            carry: String::new(),
        }
    }

    fn push(&mut self, text: &str) -> String {
        if self.window == 0 {
            return crate::credentials::scrub_secrets(text, &self.secrets);
        }
        let mut buf = std::mem::take(&mut self.carry);
        buf.push_str(text);
        let scrubbed = crate::credentials::scrub_secrets(&buf, &self.secrets);
        let split = floor_char_boundary(&scrubbed, scrubbed.len().saturating_sub(self.window));
        self.carry = scrubbed[split..].to_string();
        scrubbed[..split].to_string()
    }

    fn finish(&mut self) -> String {
        std::mem::take(&mut self.carry)
    }
}

/// Largest character boundary at or below `index`.
///
/// `str::floor_char_boundary` is still unstable, and slicing a `String` at a
/// byte index that is not a boundary panics.
fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
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
