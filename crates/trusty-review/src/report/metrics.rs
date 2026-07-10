//! v0 trusty-analyze metrics schema and loader (M1, #2313).
//!
//! Why: deterministic report fill consumes a pre-produced trusty-analyze metrics
//! JSON per repository.  Pinning a small, pragmatic v0 schema (rather than the
//! full future analyzer output) lets M1 ship without blocking on the analyzer's
//! final format, while keeping every field optional so a partial metrics file
//! still parses and unmapped template fields fall through to honesty markers.
//! What: defines [`AnalyzeMetrics`] (LoC totals + per-language breakdown, file /
//! function counts, complexity distribution buckets, and a top-findings list
//! with severity) and [`load_metrics`] which parses the JSON from disk.
//! Test: `metrics.rs` tests cover a full parse, a minimal `{}` parse, and the
//! derived helpers (`primary_languages`, `total_loc`).

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::ReportError;

/// The v0 trusty-analyze metrics document for a single repository.
///
/// Why: the deterministic renderer maps these fields onto per-application
/// template placeholders (tech stack, LoC, file/function counts); a stable v0
/// shape decouples report M1 from the analyzer's evolving output.
/// What: every field defaults, so a file containing only the fields the analyzer
/// currently emits still deserializes; missing data becomes honesty markers.
/// Test: `metrics.rs::parse_full_metrics`, `parse_minimal_metrics`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AnalyzeMetrics {
    /// Schema version tag (informational; defaults to empty).
    #[serde(default)]
    pub schema_version: String,
    /// Repository identifier the metrics describe (informational).
    #[serde(default)]
    pub repository: String,
    /// Lines-of-code totals and per-language breakdown.
    #[serde(default)]
    pub loc: LocMetrics,
    /// File and function counts.
    #[serde(default)]
    pub counts: CountMetrics,
    /// Cyclomatic-complexity distribution as labelled buckets.
    #[serde(default)]
    pub complexity: ComplexityDistribution,
    /// Top findings with severity (deterministic subset; no prose in M1).
    #[serde(default)]
    pub findings: Vec<MetricFinding>,
}

/// Lines-of-code totals plus a per-language breakdown.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LocMetrics {
    /// Total lines of code across all languages.
    #[serde(default)]
    pub total: u64,
    /// Per-language LoC breakdown (largest first is not required).
    #[serde(default)]
    pub by_language: Vec<LanguageLoc>,
}

/// LoC for a single language.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LanguageLoc {
    /// Language name (e.g. `Rust`, `TypeScript`).
    #[serde(default)]
    pub language: String,
    /// Lines of code attributed to this language.
    #[serde(default)]
    pub loc: u64,
}

/// File and function counts for a repository.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CountMetrics {
    /// Number of source files analyzed.
    #[serde(default)]
    pub files: u64,
    /// Number of functions/methods analyzed.
    #[serde(default)]
    pub functions: u64,
}

/// A cyclomatic-complexity distribution as labelled buckets.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ComplexityDistribution {
    /// One entry per bucket (e.g. `low (1-5)` → 120 functions).
    #[serde(default)]
    pub buckets: Vec<ComplexityBucket>,
}

/// A single complexity bucket.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ComplexityBucket {
    /// Bucket label (e.g. `low (1-5)`, `high (>20)`).
    #[serde(default)]
    pub label: String,
    /// Number of functions falling in this bucket.
    #[serde(default)]
    pub count: u64,
}

/// Severity of a metric finding, mapped to the report's RED/AMBER/GREEN bands.
///
/// Why: the report groups findings by severity band; a typed enum with a
/// lenient default keeps unknown severities from breaking the parse.
/// What: three bands; unknown/absent values deserialize to `Green` (least
/// alarming, honesty-preserving default).
/// Test: `metrics.rs::parse_full_metrics`.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Critical / high-risk finding.
    Red,
    /// Medium-risk finding.
    Amber,
    /// Positive / healthy finding (topic-list only, per the no-green rule).
    #[default]
    Green,
}

/// A single deterministic finding from trusty-analyze (no LLM prose in M1).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MetricFinding {
    /// Short finding title.
    #[serde(default)]
    pub title: String,
    /// Severity band.
    #[serde(default)]
    pub severity: Severity,
    /// Finding category (e.g. `security`, `maintainability`).
    #[serde(default)]
    pub category: String,
    /// Affected component/path, if known.
    #[serde(default)]
    pub component: String,
}

impl AnalyzeMetrics {
    /// The primary language names, largest LoC first, up to `n`.
    ///
    /// Why: the per-application tech-stack field wants a compact language list
    /// rather than the full breakdown.
    /// What: sorts `by_language` by descending LoC and returns the first `n`
    /// language names; empty when no breakdown is present.
    /// Test: `metrics.rs::primary_languages_orders_by_loc`.
    pub fn primary_languages(&self, n: usize) -> Vec<String> {
        let mut langs: Vec<&LanguageLoc> = self.loc.by_language.iter().collect();
        langs.sort_by_key(|l| std::cmp::Reverse(l.loc));
        langs
            .into_iter()
            .take(n)
            .map(|l| l.language.clone())
            .collect()
    }
}

/// Load and parse a v0 metrics JSON file from disk.
///
/// Why: the report pipeline reads one metrics file per repository when the
/// manifest declares a `metrics` path; a dedicated loader gives typed errors.
/// What: reads `path`, parses against [`AnalyzeMetrics`]; I/O and parse errors
/// map to [`ReportError`].
/// Test: `metrics.rs::load_roundtrip` writes a temp file and reads it back.
pub fn load_metrics(path: &Path) -> std::result::Result<AnalyzeMetrics, ReportError> {
    let text = std::fs::read_to_string(path).map_err(|source| ReportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| ReportError::Metrics {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
