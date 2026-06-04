//! Letter-grade type for PR reviews — 13 half-step variants A+ through F.
//!
//! Why: a letter grade gives reviewers an at-a-glance quality signal and makes
//! the verdict thresholds explicit.  The grade is the LLM's primary quality
//! assessment; the verdict is *derived* from it (with a safety floor applied
//! on top so severity/verification never weaken the verdict below what the
//! grade implies).
//!
//! What: exposes the `Grade` enum (13 variants, A+ through F), `Display`,
//! `FromStr` / serde (using the standard notation "A+", "B-", "C", …, "F"),
//! `PartialOrd` / `Ord` so bands can be compared (A+ > A > … > F),
//! `verdict_for_grade` — the single source of truth for the grade→verdict mapping,
//! and `default_grade_for_verdict` — the inverse used when the LLM omits the grade.
//!
//! Grade → Verdict mapping (FIXED by product decision, APPROVE floor = B-):
//!
//! | Grade band        | Verdict              |
//! |-------------------|----------------------|
//! | A+, A, A-, B+, B, B- | APPROVE              |
//! | C+, C, C-         | APPROVE* (approve w/ reservations) |
//! | D+, D, D-         | REQUEST_CHANGES      |
//! | F                 | BLOCK                |
//!
//! Test: `grade_serde_roundtrip`, `grade_ordering`, `verdict_for_grade_boundaries`,
//! `default_grade_for_verdict_roundtrips`.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::models::Verdict;

// ─── Grade enum ───────────────────────────────────────────────────────────────

/// Letter grade for a PR review, with half-step +/- modifiers.
///
/// Why: provides an at-a-glance quality signal and makes the verdict thresholds
/// explicit and tunable.  The grade is the LLM's primary quality assessment;
/// `verdict_for_grade` derives the corresponding action-verdict from it.
/// What: 13 ordered variants from A+ (best) to F (worst).  Serde uses the
/// standard notation ("A+", "A", "A-", …, "F").  `Ord` is defined so A+ > A >
/// … > F (higher ordinal = higher quality).
/// Test: `grade_serde_roundtrip`, `grade_ordering`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Grade {
    /// A+ — exceptional, near-perfect change.
    #[serde(rename = "A+")]
    APlus,
    /// A — excellent, clean change.
    A,
    /// A- — very good, minor nits only.
    #[serde(rename = "A-")]
    AMinus,
    /// B+ — good, small concerns.
    #[serde(rename = "B+")]
    BPlus,
    /// B — solid, some room to improve.
    B,
    /// B- — acceptable; lowest APPROVE grade.
    #[serde(rename = "B-")]
    BMinus,
    /// C+ — marginal; advisory concerns noted.
    #[serde(rename = "C+")]
    CPlus,
    /// C — below standard; notable issues.
    C,
    /// C- — needs work; author should reconsider.
    #[serde(rename = "C-")]
    CMinus,
    /// D+ — significant problems.
    #[serde(rename = "D+")]
    DPlus,
    /// D — major issues requiring changes before merge.
    D,
    /// D- — severe issues.
    #[serde(rename = "D-")]
    DMinus,
    /// F — critical failure (compile-break, data corruption, security bypass).
    F,
}

// ─── Ordering (A+ = best = highest) ──────────────────────────────────────────

impl Grade {
    /// Numeric ordinal: A+ = 12 (best), F = 0 (worst).
    ///
    /// Why: drives `PartialOrd`/`Ord` so comparisons read naturally (`A > B`).
    /// What: returns a u8 in 0..=12.
    /// Test: `grade_ordering`.
    fn ordinal(self) -> u8 {
        match self {
            Grade::APlus => 12,
            Grade::A => 11,
            Grade::AMinus => 10,
            Grade::BPlus => 9,
            Grade::B => 8,
            Grade::BMinus => 7,
            Grade::CPlus => 6,
            Grade::C => 5,
            Grade::CMinus => 4,
            Grade::DPlus => 3,
            Grade::D => 2,
            Grade::DMinus => 1,
            Grade::F => 0,
        }
    }
}

impl PartialOrd for Grade {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Grade {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ordinal().cmp(&other.ordinal())
    }
}

// ─── Display ─────────────────────────────────────────────────────────────────

impl std::fmt::Display for Grade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Grade::APlus => "A+",
            Grade::A => "A",
            Grade::AMinus => "A-",
            Grade::BPlus => "B+",
            Grade::B => "B",
            Grade::BMinus => "B-",
            Grade::CPlus => "C+",
            Grade::C => "C",
            Grade::CMinus => "C-",
            Grade::DPlus => "D+",
            Grade::D => "D",
            Grade::DMinus => "D-",
            Grade::F => "F",
        };
        write!(f, "{s}")
    }
}

// ─── FromStr ─────────────────────────────────────────────────────────────────

/// Parsing error for `Grade::from_str`.
///
/// Why: `FromStr` requires an `Err` type; a simple tuple-struct wrapping the
/// rejected token is sufficient.
/// What: carries the unrecognised string for diagnostic messages.
/// Test: `grade_from_str_invalid`.
#[derive(Debug, Clone)]
pub struct InvalidGrade(pub String);

impl std::fmt::Display for InvalidGrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid grade: {:?}", self.0)
    }
}

impl FromStr for Grade {
    type Err = InvalidGrade;

    /// Parse a grade string ("A+", "B-", "C", "F", …).
    ///
    /// Why: the parser and CLI need to convert the LLM's grade string to the
    /// typed enum; a `FromStr` impl makes that idiomatic.
    /// What: case-sensitive match on the 13 canonical grade strings.  Trims
    /// surrounding whitespace before matching.
    /// Test: `grade_from_str_all_variants`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "A+" => Ok(Grade::APlus),
            "A" => Ok(Grade::A),
            "A-" => Ok(Grade::AMinus),
            "B+" => Ok(Grade::BPlus),
            "B" => Ok(Grade::B),
            "B-" => Ok(Grade::BMinus),
            "C+" => Ok(Grade::CPlus),
            "C" => Ok(Grade::C),
            "C-" => Ok(Grade::CMinus),
            "D+" => Ok(Grade::DPlus),
            "D" => Ok(Grade::D),
            "D-" => Ok(Grade::DMinus),
            "F" => Ok(Grade::F),
            other => Err(InvalidGrade(other.to_string())),
        }
    }
}

// ─── Grade → Verdict mapping ─────────────────────────────────────────────────

/// Derive the review verdict from a letter grade.
///
/// Why: the grade→verdict table is a FIXED product decision (APPROVE floor =
/// B-).  This function is the **single source of truth** for that mapping.
/// It is used in two places:
///   1. After the LLM produces a grade — the verdict is derived here and then
///      reconciled with the severity floor (`grade::derive_verdict_with_grade`).
///   2. After verification — the grade may be clamped down to stay consistent
///      with a stricter post-verification verdict.
///
/// What: implements the table:
///
/// | Grade band           | Verdict              |
/// |----------------------|----------------------|
/// | A+, A, A-, B+, B, B- | APPROVE              |
/// | C+, C, C-            | APPROVE*             |
/// | D+, D, D-            | REQUEST_CHANGES      |
/// | F                    | BLOCK                |
///
/// Test: `verdict_for_grade_boundaries` — covers every grade band edge.
pub fn verdict_for_grade(grade: Grade) -> Verdict {
    match grade {
        Grade::APlus | Grade::A | Grade::AMinus | Grade::BPlus | Grade::B | Grade::BMinus => {
            Verdict::Approve
        }
        Grade::CPlus | Grade::C | Grade::CMinus => Verdict::ApproveWithReservations,
        Grade::DPlus | Grade::D | Grade::DMinus => Verdict::RequestChanges,
        Grade::F => Verdict::Block,
    }
}

/// Return the default (mildest representative) grade for a verdict.
///
/// Why: when the LLM omits or emits an unparseable grade, the pipeline must
/// still populate `ReviewResult.grade`.  Rather than leaving it absent, we
/// derive a conservative default — the mildest grade in the verdict's band —
/// so grade and verdict are always consistent in the output.
/// What: A+ for APPROVE, C for APPROVE*, D for REQUEST_CHANGES, F for BLOCK.
/// UNKNOWN maps to F (maximally conservative — unknown is not a pass).
/// Test: `default_grade_for_verdict_roundtrips`.
pub fn default_grade_for_verdict(verdict: &Verdict) -> Grade {
    match verdict {
        Verdict::Approve => Grade::APlus,
        Verdict::ApproveWithReservations => Grade::C,
        Verdict::RequestChanges => Grade::D,
        Verdict::Block => Grade::F,
        Verdict::Unknown => Grade::F,
    }
}

/// Clamp a grade down so it is consistent with the given verdict.
///
/// Why: after verification tightens the verdict (e.g. APPROVE → REQUEST_CHANGES
/// due to a confirmed High finding), the original model grade may be inconsistent
/// (e.g. grade "B+" vs verdict REQUEST_CHANGES).  This function ensures grade and
/// verdict never disagree in the final output by lowering the grade to the ceiling
/// of the verdict's band when the grade implies a milder verdict than the actual one.
///
/// Precedence rule:
///   final_grade = min(model_grade, ceiling_for_verdict(actual_verdict))
///
/// The ceiling is the BEST (highest) grade still consistent with the verdict:
///   APPROVE → A+ (no clamping needed for any grade)
///   APPROVE* → C+ (clamp any B or above down to C+)
///   REQUEST_CHANGES → D+ (clamp any C or above down to D+)
///   BLOCK → F (clamp anything above F down to F)
///   UNKNOWN → F (same as BLOCK)
///
/// What: returns the grade unchanged when it is already consistent, or the
/// band ceiling when it implies a milder verdict than `actual_verdict`.
/// Test: `clamp_grade_to_verdict_block`, `clamp_grade_to_verdict_request_changes`.
pub fn clamp_grade_to_verdict(grade: Grade, actual_verdict: &Verdict) -> Grade {
    let grade_verdict = verdict_for_grade(grade);
    // The grade is already at least as strict as the actual verdict — no clamping.
    if is_at_least_as_strict(&grade_verdict, actual_verdict) {
        return grade;
    }
    // Grade implies a milder verdict — clamp to the ceiling of the actual band.
    match actual_verdict {
        Verdict::Approve => grade, // Approve accepts any grade.
        Verdict::ApproveWithReservations => Grade::CPlus, // C+ is ceiling for APPROVE*.
        Verdict::RequestChanges => Grade::DPlus, // D+ is ceiling for REQUEST_CHANGES.
        Verdict::Block | Verdict::Unknown => Grade::F, // Only F for BLOCK/UNKNOWN.
    }
}

/// Return true if `a` implies a verdict at least as strict as `b`.
///
/// Why: needed by `clamp_grade_to_verdict` to avoid clamping when the grade
/// is already consistent or stricter than the actual verdict.
/// What: uses the verdict ordinal ordering APPROVE(0) < APPROVE*(1) <
/// REQUEST_CHANGES(2) < BLOCK(3).  UNKNOWN(4) is treated as maximally strict.
/// Test: transitively covered by `clamp_grade_to_verdict_*`.
fn is_at_least_as_strict(a: &Verdict, b: &Verdict) -> bool {
    verdict_ordinal(a) >= verdict_ordinal(b)
}

/// Ordinal for strict ordering of verdicts (higher = more severe).
///
/// Why: used by `is_at_least_as_strict` to compare strictness.
/// What: APPROVE=0, APPROVE*=1, REQUEST_CHANGES=2, BLOCK=3, UNKNOWN=4.
/// Test: transitively covered.
fn verdict_ordinal(v: &Verdict) -> u8 {
    match v {
        Verdict::Approve => 0,
        Verdict::ApproveWithReservations => 1,
        Verdict::RequestChanges => 2,
        Verdict::Block => 3,
        Verdict::Unknown => 4,
    }
}

// ─── Unit tests ─────────────────────────────────────────────────────────────
// Tests extracted to letter_grade_tests.rs to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "letter_grade_tests.rs"]
mod tests;
