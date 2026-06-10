//! Test-coverage ingestion and verdict-gating for trusty-review (issue #1014).
//!
//! Why: the existing system prompt says "do not block on test coverage" and the
//! reviewer ingests NO coverage data.  This module makes coverage a first-class,
//! OPTIONAL, configurable gating input without breaking any existing behaviour.
//!
//! When `CoveragePolicy::enabled` is false (the default), every function in this
//! module is a no-op and the pipeline is completely unchanged.  When enabled by
//! the operator (opt-in), coverage can lower the letter grade and floor the
//! verdict to REQUEST_CHANGES when:
//!   - new-code coverage falls below `min_new_code_pct` (default 80%), OR
//!   - net coverage drops more than `max_net_drop_pct` pp (default 1pp).
//!
//! What: two submodules:
//!   - `lcov`   — LCOV format parser (`CoverageReport`, `parse_lcov`, `new_code_coverage`).
//!   - `policy` — `CoveragePolicy`, `CoverageVerdictContrib`, `evaluate_coverage`.
//!
//! The runner calls `apply_coverage_floor` (re-exported here) AFTER the LLM-grade
//! derivation step but BEFORE finalisation, so coverage can only TIGHTEN, never
//! loosen, the verdict.
//!
//! Configuration:
//!   | Env var                            | TOML key                  | Default |
//!   |------------------------------------|---------------------------|---------|
//!   | `TRUSTY_REVIEW_COVERAGE_ENABLED`   | `[coverage] enabled`      | false   |
//!   | `TRUSTY_REVIEW_MIN_NEW_CODE_PCT`   | `[coverage] min_new_code_pct` | 80.0 |
//!   | `TRUSTY_REVIEW_MAX_NET_DROP_PCT`   | `[coverage] max_net_drop_pct` | 1.0  |
//!   | `TRUSTY_REVIEW_LCOV_PATH`          | `[coverage] lcov_path`    | (none)  |
//!
//! Test: see `lcov` and `policy` submodule tests.

pub mod lcov;
pub mod policy;

pub use lcov::{
    CoverageReport, FileCoverage, LcovError, new_code_coverage, parse_lcov, parse_lcov_file,
};
pub use policy::{CoverageFileConfig, CoveragePolicy, CoverageVerdictContrib, evaluate_coverage};

use crate::{
    models::Verdict,
    pipeline::letter_grade::{Grade, clamp_grade_to_verdict},
};

/// Apply the coverage floor to the current verdict and grade, if triggered.
///
/// Why: the runner calls this as a final floor AFTER the LLM + severity derivation.
/// It can only TIGHTEN (REQUEST_CHANGES) — it never softens a BLOCK or escalates
/// APPROVE to BLOCK (coverage is not a correctness bug).
/// What: if `contrib.floor` is `Some(Verdict::RequestChanges)` and the current
/// verdict is weaker (APPROVE or APPROVE*), the verdict is raised to REQUEST_CHANGES
/// and the grade is clamped to at most D+.  A BLOCK verdict is never weakened.
/// When `contrib.floor` is None (coverage passed or policy disabled), returns
/// the inputs unchanged.
/// Test: `apply_coverage_floor_noop_when_no_floor`,
/// `apply_coverage_floor_tightens_approve`,
/// `apply_coverage_floor_does_not_weaken_block`.
pub fn apply_coverage_floor(
    verdict: Verdict,
    grade: Grade,
    contrib: &CoverageVerdictContrib,
) -> (Verdict, Grade) {
    let Some(floor) = contrib.floor.as_ref() else {
        // Coverage passed (or policy disabled) — return unchanged.
        return (verdict, grade);
    };

    // Only REQUEST_CHANGES is a valid coverage floor (coverage cannot BLOCK).
    // A BLOCK verdict must never be softened; a REQUEST_CHANGES verdict is
    // idempotent if already set.
    let new_verdict = match (&verdict, floor) {
        // Already at BLOCK — leave it (don't soften).
        (Verdict::Block, _) => Verdict::Block,
        // Coverage floor tightens APPROVE or APPROVE* to REQUEST_CHANGES.
        (Verdict::Approve | Verdict::ApproveWithReservations, Verdict::RequestChanges) => {
            Verdict::RequestChanges
        }
        // Already at or beyond REQUEST_CHANGES — leave it.
        _ => verdict,
    };

    let new_grade = if let Some(ceiling) = contrib.grade_ceiling {
        // Clamp grade to the ceiling implied by the floor (D+ for REQUEST_CHANGES).
        if grade > ceiling { ceiling } else { grade }
    } else {
        // Ensure grade/verdict consistency regardless.
        clamp_grade_to_verdict(grade, &new_verdict)
    };

    (new_verdict, new_grade)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::policy::CoverageVerdictContrib;

    fn pass_contrib() -> CoverageVerdictContrib {
        CoverageVerdictContrib {
            floor: None,
            grade_ceiling: None,
            summary: "pass".to_string(),
        }
    }

    fn fail_contrib() -> CoverageVerdictContrib {
        CoverageVerdictContrib {
            floor: Some(Verdict::RequestChanges),
            grade_ceiling: Some(Grade::DPlus),
            summary: "fail".to_string(),
        }
    }

    /// apply_coverage_floor is a no-op when contrib has no floor.
    ///
    /// Why: the off-by-default path must return the inputs unchanged.
    /// Test: pass_contrib → same verdict/grade.
    #[test]
    fn apply_coverage_floor_noop_when_no_floor() {
        let (v, g) = apply_coverage_floor(Verdict::Approve, Grade::A, &pass_contrib());
        assert_eq!(v, Verdict::Approve);
        assert_eq!(g, Grade::A);
    }

    /// apply_coverage_floor tightens APPROVE to REQUEST_CHANGES.
    ///
    /// Why: a PR with zero new-code coverage must floor to REQUEST_CHANGES
    /// even if the LLM gave it an A.
    /// Test: fail_contrib on APPROVE/A → REQUEST_CHANGES/D+.
    #[test]
    fn apply_coverage_floor_tightens_approve() {
        let (v, g) = apply_coverage_floor(Verdict::Approve, Grade::A, &fail_contrib());
        assert_eq!(v, Verdict::RequestChanges);
        assert_eq!(g, Grade::DPlus, "grade must be clamped to D+");
    }

    /// apply_coverage_floor tightens APPROVE* to REQUEST_CHANGES.
    ///
    /// Why: APPROVE* is still weaker than REQUEST_CHANGES; coverage must tighten.
    /// Test: fail_contrib on APPROVE*/C → REQUEST_CHANGES/D+.
    #[test]
    fn apply_coverage_floor_tightens_approve_star() {
        let (v, g) =
            apply_coverage_floor(Verdict::ApproveWithReservations, Grade::C, &fail_contrib());
        assert_eq!(v, Verdict::RequestChanges);
        assert_eq!(g, Grade::DPlus);
    }

    /// apply_coverage_floor does NOT weaken a BLOCK verdict.
    ///
    /// Why: a compile error (BLOCK) must never be softened by coverage gating.
    /// Test: fail_contrib on BLOCK/F → BLOCK/F unchanged.
    #[test]
    fn apply_coverage_floor_does_not_weaken_block() {
        let (v, g) = apply_coverage_floor(Verdict::Block, Grade::F, &fail_contrib());
        assert_eq!(v, Verdict::Block, "BLOCK must not be softened by coverage");
        assert_eq!(g, Grade::F);
    }

    /// apply_coverage_floor is idempotent on REQUEST_CHANGES.
    ///
    /// Why: if severity floors already gave REQUEST_CHANGES, coverage adding
    /// the same floor must not change the outcome.
    /// Test: fail_contrib on REQUEST_CHANGES/D → REQUEST_CHANGES/D+.
    #[test]
    fn apply_coverage_floor_idempotent_on_request_changes() {
        let (v, g) = apply_coverage_floor(Verdict::RequestChanges, Grade::D, &fail_contrib());
        assert_eq!(v, Verdict::RequestChanges);
        // Grade D < D+ ceiling → clamped up to D+ (grade can only improve from D to D+)
        // Actually D+ ceiling means grade must be AT MOST D+.  D < D+, so D stays D.
        // The `grade > ceiling` comparison uses Ord where D+ > D (ordinal 3 > 2),
        // so D does NOT exceed the D+ ceiling — grade stays D.
        assert_eq!(g, Grade::D, "D does not exceed D+ ceiling so stays D");
    }
}
