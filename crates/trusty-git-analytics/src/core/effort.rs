//! Empirical effort scoring for git commits.
//!
//! Implements the v1 formula shared between `tga backfill effort` (Rust) and
//! the parallel `scripts/compute-effort.sh` (bash) so both storage paths —
//! `fact_commit_effort` rows and `Effort:` git trailers — produce identical
//! T-shirt sizes.
//!
//! # Formula (v1)
//!
//! ```text
//! tests_factor = 1 - 0.3 * min(test_LoC / max(LoC, 1), 1)
//! score        = α·log₂(LoC+1) + β·log₂(files+1) + δ·tests_factor
//! ```
//!
//! Coefficients:  α=1.0, β=1.5, γ=0.0 (deferred), δ=1.0
//!
//! T-shirt thresholds (calibrated against 99 trusty-tools commits; see PR #308):
//! | Size | Score   |
//! |------|---------|
//! | XS   | ≤ 6     |
//! | S    | (6,10]  |
//! | M    | (10,14] |
//! | L    | (14,18] |
//! | XL   | > 18    |
//!
//! # Test-LoC detection (path globs, case-insensitive)
//!
//! - `**/tests/**`
//! - `**/*_test.rs`, `**/test_*.rs`
//! - `**/*.spec.*`, `**/*.test.*`
//! - `**/__tests__/**`

/// Formula version string — baked into every persisted row.
///
/// Why: lets callers distinguish scores computed with different coefficient
/// sets when the formula changes (e.g., v2 that adds cyclomatic complexity).
/// What: static string "v1" for the current coefficient set.
/// Test: used as a literal in `EffortRecord`; equality checked in unit tests.
pub const FORMULA_VERSION: &str = "v1";

/// v1 formula coefficients.
const ALPHA: f64 = 1.0; // LoC weight
const BETA: f64 = 1.5; // file-count weight
const DELTA: f64 = 1.0; // tests-factor weight

/// T-shirt size boundaries (calibrated against 99 trusty-tools commits; see PR #308).
///
/// These must be kept in sync with the identical constants in
/// `scripts/compute-effort.sh` (`XS_MAX`, `S_MAX`, `M_MAX`, `L_MAX`).
const THRESHOLD_XS: f64 = 6.0;
const THRESHOLD_S: f64 = 10.0;
const THRESHOLD_M: f64 = 14.0;
const THRESHOLD_L: f64 = 18.0;

/// T-shirt effort size.
///
/// Why: a human-readable bucketing of the continuous effort score that matches
/// the bucket used in the bash compute script and commit-effort trailers.
/// What: one of XS, S, M, L, XL corresponding to the score thresholds above.
/// Test: [`EffortResult::size_label`] and [`size_for_score`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortSize {
    /// Extra-small: score ≤ 6.0
    Xs,
    /// Small: 6.0 < score ≤ 10.0
    S,
    /// Medium: 10.0 < score ≤ 14.0
    M,
    /// Large: 14.0 < score ≤ 18.0
    L,
    /// Extra-large: score > 18.0
    Xl,
}

impl EffortSize {
    /// Return the canonical text label stored in `fact_commit_effort.size`.
    ///
    /// Why: matches the string stored by the bash script so JOIN queries and
    /// reports can group by size uniformly.
    /// What: `"XS"` | `"S"` | `"M"` | `"L"` | `"XL"`.
    /// Test: checked by [`tests::size_labels_match_spec`].
    pub fn label(self) -> &'static str {
        match self {
            EffortSize::Xs => "XS",
            EffortSize::S => "S",
            EffortSize::M => "M",
            EffortSize::L => "L",
            EffortSize::Xl => "XL",
        }
    }
}

/// Computed effort result for a single commit.
///
/// Why: bundles all derived values so callers (the backfill command, tests)
/// can inspect individual components without recomputing.
/// What: holds the raw measurements, intermediate `tests_factor`, the
/// continuous `score`, and the discrete `size`.
/// Test: produced by [`compute_effort`]; checked extensively in unit tests.
#[derive(Debug, Clone)]
pub struct EffortResult {
    /// Insertions + deletions across all changed files.
    pub loc: u32,
    /// Number of changed files.
    pub files: u32,
    /// Lines changed in files identified as test files.
    pub test_loc: u32,
    /// Computed tests factor: `1.0 - 0.3 * min(test_loc / max(loc, 1), 1.0)`.
    pub tests_factor: f64,
    /// Continuous effort score.
    pub score: f64,
    /// Bucketed T-shirt size derived from `score`.
    pub size: EffortSize,
}

impl EffortResult {
    /// Convenience accessor that returns the string label of `size`.
    ///
    /// Why: avoids a match at every call site that needs the persisted label.
    /// What: delegates to [`EffortSize::label`].
    /// Test: indirectly covered by all tests that check the label output.
    pub fn size_label(&self) -> &'static str {
        self.size.label()
    }
}

/// Map a T-shirt size text label to its numeric integer (1–5).
///
/// Why: `fact_commit_effort.effort_tshirt` stores an integer alongside the
/// `size` TEXT column so SQL range queries (`effort_tshirt <= 3`) and ORDER BY
/// work without string comparisons (issue #445). The mapping is a static
/// constant agreed on by all tooling.
/// What: `"XS"` → 1, `"S"` → 2, `"M"` → 3, `"L"` → 4, `"XL"` → 5.
/// Any unrecognised label falls back to 0 so callers can detect corrupt data.
/// Test: `tests::effort_tshirt_mapping`.
///
/// | Label | Integer |
/// |-------|---------|
/// | `"XS"` | 1 |
/// | `"S"`  | 2 |
/// | `"M"`  | 3 |
/// | `"L"`  | 4 |
/// | `"XL"` | 5 |
/// | other  | 0 |
pub fn effort_tshirt_from_size(size: &str) -> i64 {
    match size {
        "XS" => 1,
        "S" => 2,
        "M" => 3,
        "L" => 4,
        "XL" => 5,
        _ => 0,
    }
}

/// Map an [`EffortSize`] enum variant to its numeric T-shirt integer (1–5).
///
/// Why: convenience wrapper over [`effort_tshirt_from_size`] for callers that
/// already have an [`EffortSize`] value and want the integer without converting
/// to a label string first.
/// What: delegates to [`effort_tshirt_from_size`] via the label.
/// Test: covered by `tests::effort_tshirt_mapping` via the text-label path.
pub fn effort_tshirt(size: EffortSize) -> i64 {
    effort_tshirt_from_size(size.label())
}

/// Classify a continuous score into a T-shirt size bucket.
///
/// Why: the bash script uses the same thresholds; keeping them in one place
/// makes it trivial to verify parity.
/// What: `score ≤ 6 → XS`, `(6,10] → S`, `(10,14] → M`, `(14,18] → L`, `>18 → XL`.
/// Test: [`tests::thresholds_match_spec`].
pub fn size_for_score(score: f64) -> EffortSize {
    if score <= THRESHOLD_XS {
        EffortSize::Xs
    } else if score <= THRESHOLD_S {
        EffortSize::S
    } else if score <= THRESHOLD_M {
        EffortSize::M
    } else if score <= THRESHOLD_L {
        EffortSize::L
    } else {
        EffortSize::Xl
    }
}

/// Return `true` if `path` matches any of the test-file heuristics.
///
/// Why: isolating test LoC from production LoC lets the formula reward commits
/// that include test coverage, lowering their effective effort score.
/// What: checks the path (case-insensitively) against a fixed set of patterns
/// that cover Rust, TypeScript, JavaScript, and generic `tests/` directories.
/// Test: [`tests::test_file_detection`].
///
/// Patterns (all case-insensitive):
/// - Contains `/tests/` or starts with `tests/`
/// - Ends with `_test.rs`
/// - Filename starts with `test_` (e.g., `test_foo.rs`)
/// - Ends with `.spec.<ext>` (e.g., `.spec.ts`, `.spec.js`)
/// - Ends with `.test.<ext>` (e.g., `.test.ts`, `.test.js`)
/// - Contains `/__tests__/` or starts with `__tests__/`
pub fn is_test_file(path: &str) -> bool {
    let lower = path.to_lowercase();

    // /tests/ directory segment or tests/ at root
    if lower.contains("/tests/") || lower.starts_with("tests/") || lower == "tests" {
        return true;
    }

    // __tests__ directory (Jest convention)
    if lower.contains("/__tests__/") || lower.starts_with("__tests__/") {
        return true;
    }

    // Extract the filename portion for suffix/prefix checks.
    let filename = lower.rsplit('/').next().unwrap_or(&lower);

    // Rust: foo_test.rs
    if filename.ends_with("_test.rs") {
        return true;
    }

    // Rust: test_foo.rs
    if filename.starts_with("test_") {
        return true;
    }

    // .spec.<ext>  and  .test.<ext>  (TypeScript/JavaScript)
    //   e.g. "auth.spec.ts", "auth.test.js"
    // We check for the pattern by splitting on `.` and looking for
    // "spec" or "test" as the second-to-last segment.
    let parts: Vec<&str> = filename.split('.').collect();
    if parts.len() >= 3 {
        let discriminator = parts[parts.len() - 2];
        if discriminator == "spec" || discriminator == "test" {
            return true;
        }
    }

    false
}

/// Core effort computation for a single commit.
///
/// Why: the formula must be deterministic, pure, and identical to the bash
/// compute script so both storage paths produce the same T-shirt size.
/// What: given per-file diff records (path + insertions + deletions), computes
/// LoC, test_LoC, tests_factor, the v1 score, and the bucketed size.
/// Test: [`tests::formula_known_values`], [`tests::formula_empty_commit`],
/// and the cross-validation integration test in `tests::effort_cross_validate`
/// (marked `#[ignore]` until the parallel bash PR lands).
///
/// # Arguments
///
/// * `files` — iterator of `(path, insertions, deletions)` tuples; one entry
///   per changed file in the commit.
///
/// # Returns
///
/// An [`EffortResult`] with all intermediate values populated.
pub fn compute_effort<'a, I>(files: I) -> EffortResult
where
    I: IntoIterator<Item = (&'a str, u32, u32)>,
{
    let mut loc: u32 = 0;
    let mut test_loc: u32 = 0;
    let mut file_count: u32 = 0;

    for (path, ins, del) in files {
        let file_lines = ins.saturating_add(del);
        loc = loc.saturating_add(file_lines);
        file_count = file_count.saturating_add(1);
        if is_test_file(path) {
            test_loc = test_loc.saturating_add(file_lines);
        }
    }

    // tests_factor = 1 - 0.3 * min(test_LoC / max(LoC, 1), 1)
    let loc_f = loc as f64;
    let test_loc_f = test_loc as f64;
    let ratio = (test_loc_f / loc_f.max(1.0)).min(1.0);
    let tests_factor = 1.0 - 0.3 * ratio;

    // score = α·log₂(LoC+1) + β·log₂(files+1) + δ·tests_factor
    let score = ALPHA * (loc_f + 1.0).log2()
        + BETA * (file_count as f64 + 1.0).log2()
        + DELTA * tests_factor;

    let size = size_for_score(score);

    EffortResult {
        loc,
        files: file_count,
        test_loc,
        tests_factor,
        score,
        size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `effort_tshirt_from_size` drives the v17 migration backfill;
    /// every label must map to the correct integer (issue #445).
    /// What: asserts all five valid labels and one invalid label.
    /// Test: this test itself.
    #[test]
    fn effort_tshirt_mapping() {
        assert_eq!(effort_tshirt_from_size("XS"), 1);
        assert_eq!(effort_tshirt_from_size("S"), 2);
        assert_eq!(effort_tshirt_from_size("M"), 3);
        assert_eq!(effort_tshirt_from_size("L"), 4);
        assert_eq!(effort_tshirt_from_size("XL"), 5);
        assert_eq!(
            effort_tshirt_from_size("??"),
            0,
            "unknown label falls back to 0"
        );
        // Round-trip: EffortSize → label → tshirt
        assert_eq!(effort_tshirt(EffortSize::M), 3);
    }

    /// Why: guard that T-shirt labels match the spec and the bash script.
    /// What: asserts every variant returns the canonical string.
    /// Test: this test itself.
    #[test]
    fn size_labels_match_spec() {
        assert_eq!(EffortSize::Xs.label(), "XS");
        assert_eq!(EffortSize::S.label(), "S");
        assert_eq!(EffortSize::M.label(), "M");
        assert_eq!(EffortSize::L.label(), "L");
        assert_eq!(EffortSize::Xl.label(), "XL");
    }

    /// Why: the threshold boundaries must match the spec exactly (PR #308 calibration).
    /// What: probes at and immediately above each boundary value.
    /// Test: this test itself.
    #[test]
    fn thresholds_match_spec() {
        // Exact boundaries are inclusive at the upper bound.
        assert_eq!(size_for_score(6.0), EffortSize::Xs);
        assert_eq!(size_for_score(6.01), EffortSize::S);
        assert_eq!(size_for_score(10.0), EffortSize::S);
        assert_eq!(size_for_score(10.01), EffortSize::M);
        assert_eq!(size_for_score(14.0), EffortSize::M);
        assert_eq!(size_for_score(14.01), EffortSize::L);
        assert_eq!(size_for_score(18.0), EffortSize::L);
        assert_eq!(size_for_score(18.01), EffortSize::Xl);
        assert_eq!(size_for_score(0.0), EffortSize::Xs);
        assert_eq!(size_for_score(100.0), EffortSize::Xl);
    }

    /// Why: ensure the file-detection heuristics cover all specified patterns.
    /// What: positive and negative cases for each pattern family.
    /// Test: this test itself.
    #[test]
    fn test_file_detection() {
        // /tests/ directory segment
        assert!(is_test_file("src/tests/auth_tests.rs"));
        assert!(is_test_file("tests/integration.rs"));

        // __tests__ Jest convention
        assert!(is_test_file("src/__tests__/foo.test.ts"));
        assert!(is_test_file("__tests__/auth.js"));

        // Rust _test.rs suffix
        assert!(is_test_file("src/auth_test.rs"));

        // Rust test_ prefix
        assert!(is_test_file("src/test_auth.rs"));

        // .spec.<ext>
        assert!(is_test_file("src/auth.spec.ts"));
        assert!(is_test_file("src/auth.spec.js"));

        // .test.<ext>
        assert!(is_test_file("src/auth.test.ts"));
        assert!(is_test_file("src/auth.test.js"));

        // Non-test files
        assert!(!is_test_file("src/main.rs"));
        assert!(!is_test_file("src/auth.rs"));
        assert!(!is_test_file("Cargo.toml"));
        assert!(!is_test_file("src/context.ts"));
        assert!(!is_test_file("docs/testing.md"));

        // Testable but not a test file — contains "test" in name but wrong pattern
        assert!(!is_test_file("src/attestation.rs"));
    }

    /// Why: verify the formula against hand-computed reference values.
    /// What: a small (2-file, 50-line) commit with no test files.
    /// Test: this test itself.
    #[test]
    fn formula_known_values() {
        // 2 files, 30 insertions + 20 deletions = 50 LoC, 0 test LoC
        // tests_factor = 1 - 0.3 * 0 = 1.0
        // score = 1.0*log2(51) + 1.5*log2(3) + 1.0*1.0
        //       ≈ 5.672 + 2.378 + 1.0 = 9.05
        // With PR #308 calibrated thresholds: 9.05 ∈ (6,10] → S
        let result = compute_effort([("src/auth.rs", 25, 15), ("src/config.rs", 5, 5)]);
        assert_eq!(result.loc, 50);
        assert_eq!(result.files, 2);
        assert_eq!(result.test_loc, 0);
        assert!((result.tests_factor - 1.0).abs() < 1e-9);

        let expected_score = 1.0 * 51_f64.log2() + 1.5 * 3_f64.log2() + 1.0 * 1.0;
        assert!((result.score - expected_score).abs() < 1e-9);
        assert_eq!(result.size, EffortSize::S);
    }

    /// Why: a commit that is entirely test code should have a reduced score
    /// (tests_factor < 1.0), making it appear smaller than an equivalent
    /// production-code change.
    /// What: 20 lines in a test file; tests_factor = 1 - 0.3 = 0.7.
    /// Test: this test itself.
    #[test]
    fn formula_all_test_code_reduces_score() {
        let result = compute_effort([("src/auth_test.rs", 10, 10)]);
        assert_eq!(result.loc, 20);
        assert_eq!(result.test_loc, 20);
        // ratio = 1.0, tests_factor = 1 - 0.3 = 0.7
        assert!((result.tests_factor - 0.7).abs() < 1e-9);
        // score = 1.0*log2(21) + 1.5*log2(2) + 1.0*0.7
        let expected = 1.0 * 21_f64.log2() + 1.5 * 2_f64.log2() + 0.7;
        assert!((result.score - expected).abs() < 1e-9);
    }

    /// Why: empty commit (root or merge with no diff) must not panic.
    /// What: score = α*log2(1) + β*log2(1) + δ*1.0 = 0 + 0 + 1 = 1.0 → XS.
    /// Test: this test itself.
    #[test]
    fn formula_empty_commit() {
        let result = compute_effort(std::iter::empty::<(&str, u32, u32)>());
        assert_eq!(result.loc, 0);
        assert_eq!(result.files, 0);
        assert_eq!(result.test_loc, 0);
        assert!((result.tests_factor - 1.0).abs() < 1e-9);
        assert!((result.score - 1.0).abs() < 1e-9);
        assert_eq!(result.size, EffortSize::Xs);
    }

    /// Why: guard that very large commits (72k LoC) do not overflow u32 or
    /// produce NaN/infinity in the f64 calculation.
    /// What: 40k insertions + 32k deletions in a single file.
    /// Test: this test itself.
    #[test]
    fn formula_large_commit_does_not_overflow() {
        let result = compute_effort([("src/generated.rs", 40_000, 32_000)]);
        assert_eq!(result.loc, 72_000);
        assert!(result.score.is_finite());
        assert_eq!(result.size, EffortSize::Xl);
    }

    /// Cross-validation placeholder.
    ///
    /// Why: ensures Rust formula output matches bash script output exactly once
    /// both sides have landed.  Marked `#[ignore]` so it does not break CI
    /// before `scripts/compute-effort.sh` from the parallel PR is merged.
    /// What: will build a temp git repo, run `compute_effort` on each commit,
    /// shell out to `scripts/compute-effort.sh`, and assert score within ±0.01.
    /// Test: run manually with `cargo test -p tga -- --include-ignored cross_validate`.
    #[ignore]
    #[test]
    fn effort_cross_validate() {
        // TODO: implement once scripts/compute-effort.sh has landed from the
        // parallel PR (feat/commit-effort-scope).
        //
        // Steps:
        //  1. Create a tempdir git repo.
        //  2. Make 5-10 commits of varying sizes using git2.
        //  3. For each commit:
        //     a. Run `compute_effort` from Rust.
        //     b. Run `scripts/compute-effort.sh <sha> <repo>` via std::process::Command.
        //     c. Assert size labels are equal.
        //     d. Assert scores are within ±0.01 (float rounding tolerance).
    }
}
