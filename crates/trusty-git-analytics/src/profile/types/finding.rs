//! Finding DTO, trend-tag, and longitudinal-finding types.
//!
//! Why: the profile narrative must say which findings are new, recurring, or
//! resolved across periods. The finding itself arrives as JSON produced by an
//! external code-review pass, so tga defines its OWN structurally-equivalent
//! DTO rather than importing a review crate's type — #5463 exists precisely to
//! stop the profiling code depending on trusty-review, and a `use
//! trusty_review::models::Finding` here would rebuild the cycle it removes.
//! What: defines [`FindingEffort`], [`ProfileFinding`], [`TrendTag`], and
//! [`LongitudinalFinding`]. Field names and serde representations match the
//! review-side JSON exactly, so a serialised review finding deserialises into
//! `ProfileFinding` with the extra review-only fields ignored.
//! Test: `trend_tag_serde_roundtrip`, `longitudinal_finding_serde_roundtrip`,
//! and `profile_finding_parses_review_json` in the parent `tests` module.

use serde::{Deserialize, Serialize};

// ─── FindingEffort ───────────────────────────────────────────────────────────

/// Estimated remediation effort for a finding.
///
/// Why: effort is the axis a manager reads a longitudinal profile along — a
/// recurring `High` finding is a very different signal from a recurring `Low`
/// one.
/// What: three-variant enum serialised as lowercase strings, matching the
/// review-side `effort` field so the JSON round-trips.
/// Test: `profile_finding_parses_review_json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingEffort {
    /// Small change, typically under an hour.
    Low,
    /// Moderate change, typically a few hours.
    Medium,
    /// Large refactoring or cross-cutting change.
    High,
}

impl std::fmt::Display for FindingEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingEffort::Low => write!(f, "low"),
            FindingEffort::Medium => write!(f, "medium"),
            FindingEffort::High => write!(f, "high"),
        }
    }
}

// ─── ProfileFinding ──────────────────────────────────────────────────────────

/// A single code-review finding as the profile pipeline consumes it.
///
/// Why: the profile pipeline needs a finding's file, kind, description, and
/// effort to cluster findings across periods and render them. It does NOT need
/// the review pipeline's transient state (verification outcome, issue
/// eligibility, suggested replacement), so this DTO carries the durable subset
/// and nothing else (#5463).
/// What: a serde type whose field names match the review-side finding JSON.
/// Unknown fields are ignored on deserialisation, which is what lets a
/// full review finding be read straight into this type.
/// Test: `profile_finding_parses_review_json`,
/// `profile_finding_clamps_confidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileFinding {
    /// Path of the file this finding refers to.
    pub file: String,

    /// Line number within the file, when the finding is line-anchored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,

    /// Finding type / category (e.g. `"security"`, `"logic-error"`).
    pub kind: String,

    /// Human-readable description of the issue.
    pub description: String,

    /// What goes wrong if the finding is left unaddressed.
    #[serde(default)]
    pub consequence: String,

    /// Proposed fix or remediation, as prose.
    pub suggestion: String,

    /// Confidence score in `[0.0, 1.0]`.
    pub confidence: f32,

    /// Estimated remediation effort.
    pub effort: FindingEffort,
}

impl ProfileFinding {
    /// Construct a finding, clamping `confidence` into `[0.0, 1.0]`.
    ///
    /// Why: confidence usually originates from a model's JSON output, which can
    /// be out of range; clamping keeps every downstream comparison meaningful
    /// without an error path for malformed input.
    /// What: sets `line` to `None` and `consequence` to the empty string; every
    /// other field comes from the arguments.
    /// Test: `profile_finding_clamps_confidence`.
    pub fn new(
        file: impl Into<String>,
        kind: impl Into<String>,
        description: impl Into<String>,
        suggestion: impl Into<String>,
        confidence: f32,
        effort: FindingEffort,
    ) -> Self {
        Self {
            file: file.into(),
            line: None,
            kind: kind.into(),
            description: description.into(),
            consequence: String::new(),
            suggestion: suggestion.into(),
            confidence: confidence.clamp(0.0, 1.0),
            effort,
        }
    }
}

// ─── TrendTag ────────────────────────────────────────────────────────────────

/// Longitudinal trend classification for a finding across periods.
///
/// Why: the profile must distinguish findings that persist, appear for the
/// first time, or stop appearing, so the narrative can give targeted feedback
/// instead of restating the same list every period.
/// What: four-variant enum serialised as `snake_case` strings.
/// Test: `trend_tag_serde_roundtrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrendTag {
    /// The same finding class appeared in multiple periods.
    Recurring,
    /// The finding appeared for the first time in the latest period.
    New,
    /// The finding appeared in earlier periods but not the latest one.
    Resolved,
    /// The finding's confidence rose materially between its first and latest
    /// occurrence.
    Worsening,
}

// ─── LongitudinalFinding ─────────────────────────────────────────────────────

/// A code-review finding annotated with the period it was observed in and its
/// cross-period trend.
///
/// Why: a finding alone says nothing longitudinal; pairing it with a period
/// label and a trend tag is what turns a pile of per-period findings into a
/// profile.
/// What: pairs a [`ProfileFinding`] with its period label and an optional
/// [`TrendTag`]. `trend_tag` is `None` until
/// [`assign_trend_tags`](crate::profile::synthesizer::assign_trend_tags) runs.
/// Test: `longitudinal_finding_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongitudinalFinding {
    /// Human-readable period label (e.g. `"2026-W01..W04"`).
    pub period_label: String,

    /// The underlying code-review finding.
    pub finding: ProfileFinding,

    /// Trend classification relative to the other periods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trend_tag: Option<TrendTag>,
}
