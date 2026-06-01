//! Longitudinal finding and trend-tag types.
//!
//! Why: the profile narrative needs to surface which findings are new vs.
//! recurring vs. improving across periods; separating these types keeps each
//! file focused and under the 500-line cap.
//! What: defines `TrendTag` and `LongitudinalFinding`.
//! Test: `trend_tag_serde_roundtrip` and `longitudinal_finding_serde_roundtrip`
//! in the parent `tests` module.

use serde::{Deserialize, Serialize};

use crate::models::Finding;

// ─── TrendTag ────────────────────────────────────────────────────────────────

/// Longitudinal trend classification for a finding across periods.
///
/// Why: the profile pass must distinguish findings that appear persistently
/// (recurring), newly, or are improving / worsening so the narrative can
/// offer targeted feedback.
/// What: four-variant enum serialised as `snake_case` strings.
/// Test: see `trend_tag_serde_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrendTag {
    /// The same finding class appeared in multiple prior periods.
    Recurring,
    /// The finding appeared for the first time in this period.
    New,
    /// A previously recurring finding was not observed in the most recent
    /// period, suggesting improvement.
    Resolved,
    /// The finding severity or frequency has increased relative to prior
    /// periods.
    Worsening,
}

// ─── LongitudinalFinding ─────────────────────────────────────────────────────

/// A code-review finding annotated with its longitudinal trend.
///
/// Why: the profile narrative needs to surface which findings are new vs.
/// recurring vs. improving so the reviewer can give targeted feedback.
/// What: pairs a `Finding` (reused from the MVP review loop) with the period
/// label in which it was observed and an optional trend classification.
/// Test: see `longitudinal_finding_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongitudinalFinding {
    /// Human-readable period label (e.g. `"2026-W01..W04"`).
    pub period_label: String,

    /// The underlying code-review finding.
    pub finding: Finding,

    /// Trend classification relative to prior periods.
    /// `None` when only one period has been analysed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trend_tag: Option<TrendTag>,
}
