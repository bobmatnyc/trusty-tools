//! Text size of a file, and the per-user miss log of the one-week trial.
//!
//! Why: during the trial, agents call the tool first on large documents and
//! report the questions it failed on (a "miss"). Any session, from any
//! working directory, appends to one log the owner reads afterwards; the
//! caller also needs a document's size in tokens to apply the threshold.
//!
//! What: [`text_size`] measures a file's converted text. [`MissEntry::build`]
//! describes a miss from local facts only (path, content hash, format, text
//! size when known) and never copies document text. [`append`] adds one
//! JSON line to the log through [`crate::logfile`] (exclusive `flock`, one
//! `write`, no symlink followed). The log is `$TRUSTY_SUMMARIZER_MISS_LOG`,
//! else `$HOME/.local/state/trusty-summarizer/misses.jsonl`; directories the
//! tool creates are 0700, the file 0600. Nothing here makes a network call or starts the warm server.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cache::{Cache, content_hash, hex};
use crate::config::Config;
use crate::convert::{ConvertInput, Converters, convert_cached, detect_format, read_limited};
use crate::error::{Error, Result};
use crate::record::estimate_tokens;
use crate::text::LineIndex;

/// Overrides the miss log's path (a file path).
pub const MISS_LOG_ENV: &str = "TRUSTY_SUMMARIZER_MISS_LOG";

/// The miss log: `$TRUSTY_SUMMARIZER_MISS_LOG`, else
/// `$HOME/.local/state/trusty-summarizer/misses.jsonl`.
pub fn miss_log_path() -> io::Result<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(p) = env(MISS_LOG_ENV) {
        return Ok(PathBuf::from(p));
    }
    env("HOME")
        .map(|h| PathBuf::from(h).join(".local/state/trusty-summarizer/misses.jsonl"))
        .ok_or_else(|| io::Error::other(format!("no miss log path: set HOME or {MISS_LOG_ENV}")))
}

/// What went wrong, picked from a fixed list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// The outline or `find` pointed at the wrong lines.
    WrongLines,
    /// The lines read did not hold the answer, though the document does.
    NotInLines,
    /// `find` returned no hit for a term the document holds.
    FindEmpty,
    /// The converted text lost content the original has.
    ConversionLost,
    /// The tool failed (error record, crash, hang).
    ToolError,
    /// None of the above.
    Other,
}

/// A file's converted text, measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSize {
    /// Detected format name.
    pub format: String,
    /// Converter name.
    pub converter: String,
    /// Source file size in bytes.
    pub bytes: u64,
    /// Line count of the converted text.
    pub lines: usize,
    /// Characters of the converted text.
    pub text_chars: usize,
    /// `ceil(text_chars / 4)`; see [`estimate_tokens`].
    pub tokens_est: usize,
}

impl TextSize {
    fn of(format: String, converter: &str, bytes: u64, text: &str) -> Self {
        let text_chars = text.chars().count();
        TextSize {
            format,
            converter: converter.to_string(),
            bytes,
            lines: LineIndex::new(text).line_count(),
            text_chars,
            tokens_est: estimate_tokens(text_chars),
        }
    }
}

/// Measures `path`'s converted text, the same values a `summarize` record
/// carries. Converts through the converted-text cache when `cache` is
/// given, so a document already summarized is not converted again.
///
/// Errors: as [`Converters::load_text_cached`].
/// Test: `size_command_matches_summarize_on_text`,
/// `workers/summarizer-convert/tests/sizes.rs` (size_fields_match_warm_and_no_warm_on_converted_documents)
pub fn text_size(config: &Config, path: &Path, cache: Option<&Cache>) -> Result<TextSize> {
    let bytes = read_limited(path, config.max_file_bytes)?;
    let format = detect_format(path, &bytes);
    let converters = Converters::standard(config);
    let converter = converters.get(&format)?;
    let input = ConvertInput {
        path,
        bytes: &bytes,
        format: &format,
    };
    let text = convert_cached(converter.as_ref(), &input, cache)?;
    let name = converter.name().to_string();
    Ok(TextSize::of(format, &name, bytes.len() as u64, &text))
}

/// The size from cached or directly read text only: never converts with
/// the worker and never writes the cache. `None` when unknown.
fn known_size(config: &Config, path: &Path, bytes: &[u8]) -> Option<TextSize> {
    let format = detect_format(path, bytes);
    let converters = Converters::standard(config);
    let converter = converters.get(&format).ok()?;
    let input = ConvertInput {
        path,
        bytes,
        format: &format,
    };
    let text = if converter.caches_text() {
        let dir = config.cache_dir().ok()?;
        // Look only; never create the cache directory.
        if !config.cache.enabled || !dir.join("v1").is_dir() {
            return None;
        }
        let cache = Cache::open(&dir).ok()?;
        let key = crate::convert::converted_text_key(&content_hash(bytes), converter.name());
        cache.get_text(&key)?
    } else {
        converter.convert(&input).ok()?
    };
    Some(TextSize::of(
        format,
        converter.name(),
        bytes.len() as u64,
        &text,
    ))
}

/// One line of the miss log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissEntry {
    /// UTC, `YYYY-MM-DDTHH:MM:SSZ`.
    pub ts: String,
    /// The document's absolute path.
    pub path: String,
    /// `sha256:<hex>` of its bytes; null when unreadable.
    pub hash: Option<String>,
    /// Detected format; null when unreadable.
    pub format: Option<String>,
    /// Converter for that format; null when none or unreadable.
    pub converter: Option<String>,
    /// Source file size in bytes; null when unreadable.
    pub bytes: Option<u64>,
    /// Characters of the converted text; null when not known without
    /// converting (a converted format not yet in the cache).
    pub text_chars: Option<usize>,
    /// `ceil(text_chars / 4)`; null with `text_chars`.
    pub tokens_est: Option<usize>,
    /// What was asked.
    pub question: String,
    /// What went wrong, free text.
    pub note: Option<String>,
    /// The picked category.
    pub category: Option<Category>,
    /// Why the document could not be read; null when it was read.
    pub doc_error: Option<String>,
    /// `trusty-summarizer` version.
    pub tool_version: String,
    /// `outline.describe` in effect.
    pub outline_describe: bool,
    /// `references` in effect.
    pub references: bool,
    /// The reporting process's working directory.
    pub cwd: String,
}

impl MissEntry {
    /// Describes a miss on `path` (relative to `cwd`). Reads the file only
    /// to hash it and, for directly read formats, to count its text; a
    /// converted format's size comes from the converted-text cache or stays
    /// null. An unreadable file gives an entry with `doc_error` set.
    /// Test: `miss_records_the_fields_and_no_document_text`, `miss_on_unreadable_document_is_recorded_and_exits_0`
    pub fn build(
        config: &Config,
        cwd: &Path,
        path: &Path,
        question: &str,
        note: Option<&str>,
        category: Option<Category>,
    ) -> MissEntry {
        let joined = cwd.join(path);
        let abs = joined.canonicalize().unwrap_or(joined);
        let mut entry = MissEntry {
            ts: utc_now(),
            path: abs.display().to_string(),
            hash: None,
            format: None,
            converter: None,
            bytes: None,
            text_chars: None,
            tokens_est: None,
            question: question.to_string(),
            note: note.map(str::to_string),
            category,
            doc_error: None,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            outline_describe: config.outline.describe,
            references: config.references,
            cwd: cwd.display().to_string(),
        };
        match hash_file(&abs) {
            Err(e) => entry.doc_error = Some(Error::io(&abs, e).to_string()),
            Ok((hash, len)) => {
                entry.hash = Some(hash);
                entry.bytes = Some(len);
                match read_limited(&abs, config.max_file_bytes) {
                    Err(e) => entry.doc_error = Some(e.to_string()),
                    Ok(bytes) => {
                        let format = detect_format(&abs, &bytes);
                        entry.converter = Converters::standard(config)
                            .get(&format)
                            .ok()
                            .map(|c| c.name().to_string());
                        entry.format = Some(format);
                        if let Some(size) = known_size(config, &abs, &bytes) {
                            entry.text_chars = Some(size.text_chars);
                            entry.tokens_est = Some(size.tokens_est);
                        }
                    }
                }
            }
        }
        entry
    }
}

/// `sha256:<hex>` and length of a file, streamed (no size limit).
pub(crate) fn hash_file(path: &Path) -> io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let len = io::copy(&mut file, &mut hasher)?;
    Ok((format!("sha256:{}", hex(&hasher.finalize())), len))
}

/// Appends `entry` as one JSON line to `log` through
/// [`crate::logfile::append_line`]: only directories it creates get 0700, an
/// existing parent keeps its mode, a symlink at `log` is refused.
///
/// Errors: as [`crate::logfile::append_line`].
/// Test: `concurrent_appends_from_many_processes_keep_every_line`, `unwritable_log_is_an_error`
pub fn append(log: &Path, entry: &MissEntry) -> io::Result<()> {
    let line = serde_json::to_vec(entry).map_err(io::Error::other)?;
    crate::logfile::append_line(log, &line)
}

/// The log's lines (empty when the file does not exist).
///
/// Errors: the file exists but cannot be read.
pub fn read_log(log: &Path) -> io::Result<Vec<String>> {
    crate::logfile::read_lines(log)
}

/// The current time, UTC, `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_utc(secs)
}

/// Formats seconds since the epoch (civil-from-days, proleptic Gregorian).
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formatting_matches_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_794_701_160), "2026-11-15T00:06:00Z");
    }
}
