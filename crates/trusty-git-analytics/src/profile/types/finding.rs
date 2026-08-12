//! Finding DTOs and longitudinal trend classification.
//!
//! Why: profiling has to name the code-quality issues it observes, but the
//! review pipeline's own `Finding` carries verdict-gate state — citation flags,
//! verification outcome, issue eligibility — that profiling never reads.
//! Reusing it would put trusty-review's review contract on tga's critical path,
//! which is the dependency edge #5468 exists to reverse, so tga owns its own
//! minimal DTO instead.
//! What: defines [`Effort`], [`Finding`], [`TrendTag`], and
//! [`LongitudinalFinding`].
//! Test: `finding_confidence_is_clamped`, `trend_tag_serde_roundtrip`, and
//! `longitudinal_finding_serde_roundtrip` in the parent `types` test module.

use serde::{Deserialize, Serialize};

// ─── Effort ───────────────────────────────────────────────────────────────────

/// Estimated remediation effort for a [`Finding`].
///
/// Serialised as lowercase (`"low"`, `"medium"`, `"high"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Small change, typically under an hour.
    Low,
    /// Moderate change, typically a few hours.
    Medium,
    /// Large refactoring or cross-cutting change.
    High,
}

// ─── Finding ──────────────────────────────────────────────────────────────────

/// A single code-quality observation attributed to a contributor's commits.
///
/// Why: the narrative pass and the Markdown report both need the same flat,
/// serialisable shape, and the profile JSON is a stored artefact — so the field
/// set is deliberately small and stable rather than mirroring whatever the
/// review pipeline currently tracks.
/// What: file, category, description, suggestion, a `[0.0, 1.0]` confidence,
/// and an [`Effort`] estimate.
/// Test: `finding_confidence_is_clamped`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// File path the finding refers to; `"unknown"` when the source named none.
    pub file: String,
    /// Short category label, e.g. `"security"`, `"error_handling"`.
    pub kind: String,
    /// Description of the issue observed.
    pub description: String,
    /// Concrete improvement suggestion.
    pub suggestion: String,
    /// Confidence in `[0.0, 1.0]`, clamped at construction.
    pub confidence: f32,
    /// Estimated remediation effort.
    pub effort: Effort,
}

impl Finding {
    /// Construct a finding, clamping `confidence` into `[0.0, 1.0]`.
    ///
    /// Why: the confidence value originates in model output, so an out-of-range
    /// number is expected rather than exceptional. Clamping keeps a malformed
    /// value from skewing the trend comparison in `assign_trend_tags`, which
    /// treats confidence as a severity proxy.
    /// What: moves every field in and clamps `confidence`.
    /// Test: `finding_confidence_is_clamped`.
    pub fn new(
        file: impl Into<String>,
        kind: impl Into<String>,
        description: impl Into<String>,
        suggestion: impl Into<String>,
        confidence: f32,
        effort: Effort,
    ) -> Self {
        Self {
            file: file.into(),
            kind: kind.into(),
            description: description.into(),
            suggestion: suggestion.into(),
            confidence: confidence.clamp(0.0, 1.0),
            effort,
        }
    }
}

// ─── TrendTag ─────────────────────────────────────────────────────────────────

/// How a finding moved across the profiled periods.
///
/// Why: "you have 40 findings" is not actionable; "this one has recurred in
/// every period since January" is. The tag is what lets the report separate a
/// standing habit from a one-off.
/// What: four variants, serialised as `snake_case`.
/// Test: `trend_tag_serde_roundtrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrendTag {
    /// The same issue appeared in the latest period and at least one earlier one.
    Recurring,
    /// The issue appeared for the first time in the latest period.
    New,
    /// The issue appeared earlier but not in the latest period.
    Resolved,
    /// The issue recurred and its confidence rose relative to its first sighting.
    Worsening,
}

// ─── LongitudinalFinding ──────────────────────────────────────────────────────

/// A [`Finding`] bound to the period it was observed in, plus its trend.
///
/// `trend_tag` is `None` until `assign_trend_tags` has seen every period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongitudinalFinding {
    /// Period label the finding was observed in, e.g. `"2026-W01..W04"`.
    pub period_label: String,

    /// The underlying finding.
    pub finding: Finding,

    /// Trend classification relative to the other periods.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trend_tag: Option<TrendTag>,
}
