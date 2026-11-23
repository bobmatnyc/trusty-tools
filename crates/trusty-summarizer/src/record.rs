//! The JSON line written for each file.
//!
//! Why: the output contract. One [`FileRecord`] per input file, `status`
//! `ok` or `error`, so a consumer can stream the batch line by line.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::backend::Usage;
use crate::error::Error;
use crate::text::Span;

/// One output line. Serialized with a `status` field: `ok` or `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FileRecord {
    /// The file was summarized, or served from cache.
    Ok(SummaryRecord),
    /// The file could not be processed; the batch went on.
    Error(ErrorRecord),
}

impl FileRecord {
    /// The input path this record is about.
    pub fn path(&self) -> &str {
        match self {
            FileRecord::Ok(r) => &r.path,
            FileRecord::Error(r) => &r.path,
        }
    }
}

/// Estimated model tokens for `chars` characters of text: `ceil(chars / 4)`.
///
/// Why: the reader decides from a document's token count whether to call
/// this tool first; no tokenizer is linked, so the count is an estimate.
/// What: characters / 4, rounded up. Measured against one model's own count
/// on the measurement set: median error 4.6 %, from -20 % to +33 % per
/// document (README, "Output fields").
/// Test: `token_estimate_is_chars_over_four_rounded_up`
pub fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(4)
}

/// A successful result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryRecord {
    /// The path as given or found by the directory walk.
    pub path: String,
    /// `sha256:<hex>` of the file's bytes.
    pub hash: String,
    /// File size in bytes.
    pub bytes: u64,
    /// Line count of the (converted) text.
    pub lines: usize,
    /// Characters (Unicode scalar values) of the converted text.
    pub text_chars: usize,
    /// Estimated model tokens of the converted text: see [`estimate_tokens`].
    pub tokens_est: usize,
    /// Detected format name.
    pub format: String,
    /// Classification, or the explicit no-classifier value.
    pub class: ClassResult,
    /// The summary text.
    pub summary: String,
    /// Topics in document order.
    pub outline: Vec<OutlineEntry>,
    /// The configured names of the converter and backends that produced this.
    pub backends: BackendsUsed,
    /// Whether this result came from the cache.
    pub cache: CacheStatus,
    /// Cost of each stage that ran in this invocation.
    pub stages: Vec<StageStats>,
}

/// The `class` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ClassResult {
    /// `classifier.backend = "none"`.
    NoClassifierConfigured,
    /// The best-scoring label.
    Classified {
        /// Label name.
        label: String,
        /// Its score.
        score: f32,
        /// The second-best label, when there are at least two labels.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runner_up: Option<LabelScore>,
    },
}

/// A label and its score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelScore {
    /// Label name.
    pub label: String,
    /// Its score.
    pub score: f32,
}

/// One outline topic. With references off, `lines` and `paragraphs` are
/// absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineEntry {
    /// Topic title.
    pub title: String,
    /// 1-based inclusive line range, `[start, end]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Span>,
    /// 1-based inclusive paragraph range, `[start, end]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paragraphs: Option<Span>,
    /// Names, numbers and terms that distinguish this range
    /// (`outline.describe`); absent when there are none or it is off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terms: Vec<String>,
}

/// The `backends` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendsUsed {
    /// Converter name, e.g. `direct`.
    pub converter: String,
    /// Inference backend name for the summary.
    pub summary: String,
    /// Classifier backend name, `none` when not configured.
    pub classifier: String,
}

/// The `cache` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    /// Served from cache; nothing was recomputed.
    Hit,
    /// Computed now, and stored.
    Miss,
    /// The cache is off.
    Disabled,
}

/// A pipeline stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Reading and hashing the file.
    Read,
    /// Cache lookup and store.
    Cache,
    /// Conversion to text.
    Convert,
    /// Outline segmentation.
    Segment,
    /// Classification.
    Classify,
    /// Summary.
    Summarize,
}

/// Time, tokens and cost of one stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageStats {
    /// Which stage.
    pub stage: Stage,
    /// Wall time in milliseconds, microsecond precision.
    pub ms: f64,
    /// Model tokens in.
    pub tokens_in: u64,
    /// Model tokens out (reasoning included).
    pub tokens_out: u64,
    /// Of `tokens_out`, reasoning tokens, when the response reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Of `tokens_in`, prompt tokens served from the provider's cache, when
    /// the response reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens_in: Option<u64>,
    /// Cost in US dollars; null when unknown (see `cost_source`).
    pub cost_usd: Option<f64>,
    /// `none` (no billable model ran), `provider` (the provider's
    /// `usage.cost`), `configured_prices` (tokens times configured prices)
    /// or `unknown`.
    pub cost_source: String,
    /// Characters of document text sent to the backend, for the classify
    /// and summarize stages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chars_in: Option<usize>,
}

impl StageStats {
    /// A stage with no model usage.
    pub fn timed(stage: Stage, elapsed: Duration) -> Self {
        StageStats::with_usage(stage, elapsed, Usage::default())
    }

    /// A stage with model usage.
    pub fn with_usage(stage: Stage, elapsed: Duration, usage: Usage) -> Self {
        StageStats {
            stage,
            ms: (elapsed.as_secs_f64() * 1e6).round() / 1e3,
            tokens_in: usage.tokens_in,
            tokens_out: usage.tokens_out,
            reasoning_tokens: usage.reasoning_tokens,
            cached_tokens_in: usage.cached_tokens_in,
            cost_usd: usage.cost.usd(),
            cost_source: usage.cost.source().to_string(),
            chars_in: None,
        }
    }

    /// Records how many characters of document text the stage sent.
    pub fn sent(mut self, chars: usize) -> Self {
        self.chars_in = Some(chars);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate_is_chars_over_four_rounded_up() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);
        assert_eq!(estimate_tokens(60_000), 15_000);
    }
}

/// A failed file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorRecord {
    /// The path as given or found.
    pub path: String,
    /// What went wrong.
    pub error: ErrorInfo,
    /// Stages that ran before the failure.
    pub stages: Vec<StageStats>,
}

/// The `error` field of an error record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    /// Stable code from [`Error::kind`].
    pub kind: String,
    /// Human-readable message.
    pub message: String,
    /// The format name, for `unsupported_format`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

impl From<&Error> for ErrorInfo {
    fn from(e: &Error) -> Self {
        ErrorInfo {
            kind: e.kind().to_string(),
            message: e.to_string(),
            format: e.format().map(str::to_string),
        }
    }
}

/// The part of a result that is stored in the cache: everything except the
/// path and this invocation's stage costs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedResult {
    /// See [`SummaryRecord::hash`].
    pub hash: String,
    /// See [`SummaryRecord::bytes`].
    pub bytes: u64,
    /// See [`SummaryRecord::lines`].
    pub lines: usize,
    /// See [`SummaryRecord::text_chars`]. Required: an entry stored before
    /// the field existed fails to parse and counts as a miss.
    pub text_chars: usize,
    /// See [`SummaryRecord::tokens_est`].
    pub tokens_est: usize,
    /// See [`SummaryRecord::format`].
    pub format: String,
    /// See [`SummaryRecord::class`].
    pub class: ClassResult,
    /// See [`SummaryRecord::summary`].
    pub summary: String,
    /// See [`SummaryRecord::outline`].
    pub outline: Vec<OutlineEntry>,
    /// See [`SummaryRecord::backends`].
    pub backends: BackendsUsed,
}

impl CachedResult {
    /// Builds the record for `path` from this stored result.
    pub fn into_record(
        self,
        path: String,
        cache: CacheStatus,
        stages: Vec<StageStats>,
    ) -> SummaryRecord {
        SummaryRecord {
            path,
            hash: self.hash,
            bytes: self.bytes,
            lines: self.lines,
            text_chars: self.text_chars,
            tokens_est: self.tokens_est,
            format: self.format,
            class: self.class,
            summary: self.summary,
            outline: self.outline,
            backends: self.backends,
            cache,
            stages,
        }
    }
}
