//! Per-engineer-per-week quality scoring (1–5 T-shirt, parallel to effort).
//!
//! Implements the v1 quality metric from issue #377 so downstream warehouses
//! (e.g. Duetto cto analytics) can report a Quality column alongside the
//! existing Effort T-shirt.
//!
//! # Formula orientation (v1)
//!
//! The raw issue proposed:
//!
//! ```text
//! quality = 0.35·revert_rate + 0.40·bugfix_rate + 0.25·ticket_linkage_rate
//! ```
//!
//! That formula is **ambiguous on direction**: reverts and bugfixes are
//! *negative* quality signals (more reverts ⇒ worse quality) while ticket
//! linkage is *positive* (more linkage ⇒ better quality). Summing them
//! directly would make a high-revert engineer score the same as a
//! well-ticketed one. We therefore **invert the negative signals** so that a
//! higher score is unambiguously better, matching the effort scale where
//! T-shirt 5 = largest/best:
//!
//! ```text
//! quality = 0.35·(1 - revert_rate) + 0.40·(1 - bugfix_rate) + 0.25·ticket_linkage_rate
//! ```
//!
//! Each `*_rate` is the per-engineer-per-week fraction of that engineer's
//! commit count for the week. The result lies in `[0.0, 1.0]`:
//!
//! - An engineer with **no** reverts/bugfixes and **all** ticketed commits
//!   scores `0.35 + 0.40 + 0.25 = 1.00` ⇒ T-shirt **5** (best).
//! - An engineer whose commits are **all** reverts and bugfixes and **none**
//!   ticketed scores `0.0` ⇒ T-shirt **1** (worst).
//! - The neutral midpoint (no reverts, no bugfixes, but also no ticket
//!   linkage) scores `0.75`.
//!
//! # T-shirt bucketing (1–5)
//!
//! The continuous `[0,1]` score is bucketed into integer 1–5 with even
//! 0.20-wide bands so the labels read like the effort sizes:
//!
//! | T-shirt | Score band      |
//! |---------|-----------------|
//! | 1       | `[0.00, 0.20]`  |
//! | 2       | `(0.20, 0.40]`  |
//! | 3       | `(0.40, 0.60]`  |
//! | 4       | `(0.60, 0.80]`  |
//! | 5       | `(0.80, 1.00]`  |

/// Formula version string for the quality metric.
///
/// Why: lets downstream consumers distinguish scores computed with different
/// coefficient sets / orientations when the formula evolves.
/// What: static `"v1"` for the current coefficient set and orientation.
/// Test: referenced as a literal; equality checked in unit tests.
pub const QUALITY_FORMULA_VERSION: &str = "v1";

/// v1 quality coefficients (must sum to 1.0).
const W_REVERT: f64 = 0.35;
const W_BUGFIX: f64 = 0.40;
const W_TICKET: f64 = 0.25;

/// T-shirt band width (5 even bands across `[0,1]`).
const BAND: f64 = 0.20;

/// Inputs to [`quality_score`] for a single (engineer, week) bucket.
///
/// Why: bundling the three counts plus the denominator keeps the call site
/// readable and makes the "0 commits" edge case impossible to get wrong (it
/// is handled once, here).
/// What: holds the per-bucket commit total and the revert / bugfix / ticketed
/// sub-counts.
/// Test: [`tests`] below exercise every combination.
#[derive(Debug, Clone, Copy)]
pub struct QualityInputs {
    /// Total commits in the bucket (the denominator for every rate).
    pub commits: usize,
    /// Commits detected as reverts.
    pub reverts: usize,
    /// Commits classified as `bugfix`.
    pub bugfixes: usize,
    /// Commits carrying a ticket reference.
    pub ticketed: usize,
}

/// Compute the v1 quality score in `[0.0, 1.0]` (higher is better).
///
/// Why: a single comparable number lets stakeholders rank engineers on
/// quality the same way the effort score ranks them on volume (issue #377).
/// What: applies `0.35·(1-revert_rate) + 0.40·(1-bugfix_rate) +
/// 0.25·ticket_rate` where each rate is `count / commits`. With **zero
/// commits** there is no signal, so the neutral midpoint `0.75` is returned
/// (no reverts/bugfixes observed, no ticket linkage observed) — this avoids
/// punishing an empty bucket with a `0` score it did not earn.
/// Test: [`tests::all_good_scores_one`], [`tests::all_reverts_and_bugfixes`],
/// [`tests::zero_commits_is_neutral`], and the bucketing tests.
pub fn quality_score(inputs: QualityInputs) -> f64 {
    if inputs.commits == 0 {
        // No commits ⇒ no negative signal and no positive signal. Mirror the
        // formula with all rates at zero: 0.35 + 0.40 + 0 = 0.75.
        return W_REVERT + W_BUGFIX;
    }
    let n = inputs.commits as f64;
    let revert_rate = (inputs.reverts as f64 / n).clamp(0.0, 1.0);
    let bugfix_rate = (inputs.bugfixes as f64 / n).clamp(0.0, 1.0);
    let ticket_rate = (inputs.ticketed as f64 / n).clamp(0.0, 1.0);

    let score =
        W_REVERT * (1.0 - revert_rate) + W_BUGFIX * (1.0 - bugfix_rate) + W_TICKET * ticket_rate;
    score.clamp(0.0, 1.0)
}

/// Bucket a `[0.0, 1.0]` quality score into an integer T-shirt size `1..=5`.
///
/// Why: mirrors [`crate::core::effort::size_for_score`] so consumers can join
/// `quality_tshirt` exactly like `effort_tshirt` (both are 1–5 strings).
/// What: 5 even 0.20-wide bands; `5` is the best. Scores below `0` clamp to
/// `1`, above `1` clamp to `5`.
/// Test: [`tests::bucketing_band_edges`].
pub fn size_for_quality_score(score: f64) -> u8 {
    let s = score.clamp(0.0, 1.0);
    if s <= BAND {
        1
    } else if s <= 2.0 * BAND {
        2
    } else if s <= 3.0 * BAND {
        3
    } else if s <= 4.0 * BAND {
        4
    } else {
        5
    }
}

/// Convenience: compute the score and its T-shirt bucket in one call.
///
/// Why: every per-week-per-engineer row needs both the raw score and the
/// bucket; pairing them avoids recomputing the score and prevents the two
/// from drifting apart.
/// What: returns `(score, tshirt)` where `tshirt` is the string `"1".."5"`
/// (string form so the column joins like `effort_tshirt`).
/// Test: [`tests::score_and_tshirt_pairs`].
pub fn score_and_tshirt(inputs: QualityInputs) -> (f64, String) {
    let score = quality_score(inputs);
    let tshirt = size_for_quality_score(score).to_string();
    (score, tshirt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inp(commits: usize, reverts: usize, bugfixes: usize, ticketed: usize) -> QualityInputs {
        QualityInputs {
            commits,
            reverts,
            bugfixes,
            ticketed,
        }
    }

    #[test]
    fn formula_version_is_v1() {
        assert_eq!(QUALITY_FORMULA_VERSION, "v1");
    }

    #[test]
    fn all_good_scores_one() {
        // No reverts, no bugfixes, all ticketed ⇒ perfect 1.0 ⇒ T-shirt 5.
        let (score, tshirt) = score_and_tshirt(inp(10, 0, 0, 10));
        assert!((score - 1.0).abs() < 1e-9, "score = {score}");
        assert_eq!(tshirt, "5");
    }

    #[test]
    fn all_reverts_and_bugfixes() {
        // Every commit is both a revert and a bugfix, none ticketed ⇒ 0.0.
        let (score, tshirt) = score_and_tshirt(inp(4, 4, 4, 0));
        assert!((score - 0.0).abs() < 1e-9, "score = {score}");
        assert_eq!(tshirt, "1");
    }

    #[test]
    fn all_ticketed_no_reverts_or_bugfixes_is_best() {
        let score = quality_score(inp(8, 0, 0, 8));
        assert!((score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn neutral_midpoint_without_ticketing() {
        // No reverts, no bugfixes, no ticket linkage ⇒ 0.35 + 0.40 = 0.75.
        let score = quality_score(inp(5, 0, 0, 0));
        assert!((score - 0.75).abs() < 1e-9, "score = {score}");
        assert_eq!(size_for_quality_score(score), 4);
    }

    #[test]
    fn zero_commits_is_neutral() {
        // Empty bucket scores the no-signal midpoint, not 0.0.
        let score = quality_score(inp(0, 0, 0, 0));
        assert!((score - 0.75).abs() < 1e-9, "score = {score}");
    }

    #[test]
    fn half_reverts_lowers_score() {
        // 50% reverts, no bugfixes, no tickets:
        // 0.35*(1-0.5) + 0.40*(1-0) + 0.25*0 = 0.175 + 0.40 = 0.575.
        let score = quality_score(inp(4, 2, 0, 0));
        assert!((score - 0.575).abs() < 1e-9, "score = {score}");
        assert_eq!(size_for_quality_score(score), 3);
    }

    #[test]
    fn rates_clamp_when_subcounts_exceed_commits() {
        // Defensive: a revert that is also a bugfix can push a naive sum past
        // the commit count; each rate is clamped to 1.0 independently so the
        // score stays in range.
        let score = quality_score(inp(2, 3, 3, 5));
        assert!((0.0..=1.0).contains(&score), "score = {score}");
    }

    #[test]
    fn bucketing_band_edges() {
        assert_eq!(size_for_quality_score(0.0), 1);
        assert_eq!(size_for_quality_score(0.20), 1);
        assert_eq!(size_for_quality_score(0.2001), 2);
        assert_eq!(size_for_quality_score(0.40), 2);
        assert_eq!(size_for_quality_score(0.60), 3);
        assert_eq!(size_for_quality_score(0.80), 4);
        assert_eq!(size_for_quality_score(0.8001), 5);
        assert_eq!(size_for_quality_score(1.0), 5);
        // Out-of-range inputs clamp.
        assert_eq!(size_for_quality_score(-1.0), 1);
        assert_eq!(size_for_quality_score(2.0), 5);
    }

    #[test]
    fn score_and_tshirt_pairs() {
        let (score, tshirt) = score_and_tshirt(inp(10, 0, 0, 5));
        // 0.35 + 0.40 + 0.25*0.5 = 0.875 ⇒ band 5.
        assert!((score - 0.875).abs() < 1e-9, "score = {score}");
        assert_eq!(tshirt, "5");
    }
}
