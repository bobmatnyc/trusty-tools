//! v0 trusty-analyze metrics schema and loader (M1, #2313).
//!
//! Why: deterministic report fill consumes a pre-produced trusty-analyze metrics
//! JSON per repository.  Pinning a small, pragmatic v0 schema (rather than the
//! full future analyzer output) lets M1 ship without blocking on the analyzer's
//! final format, while keeping every field optional so a partial metrics file
//! still parses and unmapped template fields fall through to honesty markers.
//! What: defines [`AnalyzeMetrics`] (LoC totals + per-language breakdown, file /
//! function counts, complexity distribution buckets, and a top-findings list
//! with severity) and [`load_metrics`] which parses the JSON from disk and
//! refuses a schema major this build does not read (#5747).
//! Test: `metrics.rs` tests cover a full parse, a minimal `{}` parse, the
//! derived helpers (`primary_languages`, `total_loc`), and the schema-major
//! refusal.

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
    /// Schema version tag, e.g. `v0`; empty when the artifact declared none.
    ///
    /// #5747: no longer informational — [`load_metrics`] refuses a tag whose
    /// major it does not read. Empty still loads, because nothing in this
    /// workspace writes the artifact and the field shipped documented as
    /// informational.
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
///
/// Why (#5317): `description` and `remediation` were absent, so every
/// analyze-derived finding rendered with `not stated in source data` in all
/// three prose slots — an entry carrying a title and a path and nothing else.
/// The data was never missing: the daemon returns a rationale and a suggested
/// action per refactor suggestion and a message per diagnostic, and the adapter
/// discarded both. These two fields carry them. They are deterministic tool
/// output, not synthesis, and synthesis still overwrites them when it runs.
/// What: `title`/`severity`/`category`/`component` as before, plus the two
/// prose-slot fields, each defaulting to empty so an older metrics JSON still
/// parses.
/// Test: `analyze_adapter_tests.rs::refactor_finding_carries_rationale_and_action`.
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
    /// What the tool observed, verbatim (e.g. `cyclomatic complexity 31
    /// (grade F)`). Empty when the source stated none.
    #[serde(default)]
    pub description: String,
    /// The action the tool suggested, verbatim. Empty when the source stated
    /// none.
    #[serde(default)]
    pub remediation: String,
}

impl MetricFinding {
    /// Whether this finding would render as nothing but a title and a path.
    ///
    /// Why (#5317): a findings entry whose every prose slot falls through to
    /// the honesty marker tells a reader nothing they can act on while
    /// occupying a numbered slot in a severity band. Such an entry is dropped
    /// rather than rendered.
    /// What: true when both [`Self::description`] and [`Self::remediation`] are
    /// empty after trimming.
    /// Test: `analyze_adapter_tests.rs::contentless_findings_are_dropped`.
    pub fn is_contentless(&self) -> bool {
        self.description.trim().is_empty() && self.remediation.trim().is_empty()
    }
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

/// The artifact schema major this build reads (#5747).
///
/// Why: the file is written by a process this one does not ship with, so the
/// loader must be able to say "not mine" about a file it can nonetheless
/// deserialize.
/// What: `0`, matching the `v0` tag the schema has carried since #2317.
const SUPPORTED_SCHEMA_MAJOR: u32 = 0;

/// Load and parse a v0 metrics JSON file from disk.
///
/// Why: the report pipeline reads one metrics file per repository when the
/// manifest declares a `metrics` path; a dedicated loader gives typed errors.
/// #5747 extends that to a file that reads but is not this schema: every field
/// defaults, so a renamed key in a later producer's output would otherwise
/// render as a stated zero in a client-facing due-diligence report.
/// What: reads `path`, parses against [`AnalyzeMetrics`], then refuses a
/// declared tag whose [`super::schema::major`] is not
/// [`SUPPORTED_SCHEMA_MAJOR`]. A newer MINOR of that major loads — the
/// added-field case `#[serde(default)]` exists for.
///
/// An ABSENT tag is read as v0 rather than refused, which is where this
/// diverges from [`super::ticketing::load_ticketing`]. Nothing in this
/// workspace writes a metrics artifact — DOC-67 §7 has `tga audit` omit
/// `RepositoryEntry.metrics` so the live `--analyze` fetch is not blocked — so
/// the file is hand-authored against a field documented as informational since
/// #2317. An untagged file is an author who read that documentation, and v0 is
/// the only schema this artifact has ever had. Refusing one would break working
/// setups to guard against a rename that cannot have happened yet.
///
/// Test: `metrics.rs::{load_roundtrip,
/// an_artifact_from_an_unknown_schema_major_is_a_named_error,
/// an_artifact_with_an_uninterpretable_schema_tag_is_a_named_error,
/// an_artifact_with_a_newer_minor_of_a_known_major_still_parses,
/// an_untagged_artifact_is_read_as_v0}`.
///
/// # Errors
///
/// [`ReportError::Io`] when the file cannot be read,
/// [`ReportError::Metrics`] when it does not parse, and
/// [`ReportError::MetricsSchema`] when it declares an unreadable major.
pub fn load_metrics(path: &Path) -> std::result::Result<AnalyzeMetrics, ReportError> {
    let text = std::fs::read_to_string(path).map_err(|source| ReportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let metrics: AnalyzeMetrics =
        serde_json::from_str(&text).map_err(|source| ReportError::Metrics {
            path: path.to_path_buf(),
            source,
        })?;

    // #5747: a declared tag must name a major this build reads; an absent one
    // predates the tag and is v0. See the doc comment for why they differ.
    let declared = metrics.schema_version.trim();
    if !declared.is_empty() && super::schema::major(declared) != Some(SUPPORTED_SCHEMA_MAJOR) {
        return Err(ReportError::MetricsSchema {
            path: path.to_path_buf(),
            found: metrics.schema_version,
            supported: SUPPORTED_SCHEMA_MAJOR,
        });
    }
    Ok(metrics)
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
