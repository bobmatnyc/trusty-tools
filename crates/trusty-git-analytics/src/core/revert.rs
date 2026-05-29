//! Shared commit-message revert detection.
//!
//! Why: prior to issue #377 two independent revert heuristics existed — a
//! narrow byte-prefix check in `commands::backfill` (3 forms) and a broader
//! regex pair in `report::aggregator` (`^revert`, `^fix.*revert`). The two
//! disagreed, so the persisted `commits.is_revert` column and the
//! report-time revert rate were computed from different rules (the backfill
//! path missed ~87% of reverts the aggregator caught). This module is the
//! single source of truth both paths now call.
//!
//! What: a message is a revert when its **first line** (subject) matches any
//! of the recognized forms:
//!
//! - `Revert "<subject>"` — git's auto-generated revert subject
//!   (case-insensitive).
//! - `revert:` / `revert(scope):` — Conventional Commits revert type.
//! - `^revert` — any subject beginning with the word "revert"
//!   (case-insensitive), e.g. `Revert this change`.
//! - `^fix.*revert` — a fix commit that mentions a revert in its subject,
//!   e.g. `Fix botched revert of #42`.
//!
//! False-positive guard: matching is anchored to the start of the *first
//! line only* and requires the literal token `revert` at a word boundary.
//! Prose such as `Refactor revert handling` (subject starts with "Refactor")
//! or `reverting the decision` (no `^revert` word boundary — `reverting`
//! does not match `\brevert\b` followed by a non-word boundary... see below)
//! are deliberately NOT flagged as reverts: the issue defines the metric as
//! "share of commits that ARE reverts", so we only fire on subjects that
//! clearly announce a revert, not subjects that merely discuss one.

use std::sync::OnceLock;

use regex::Regex;

/// Compiled revert-detection patterns.
struct RevertPatterns {
    /// `^revert` — subject begins with the word "revert" (covers
    /// `Revert "..."`, `revert:`, `revert(scope):`, `Revert this change`).
    leading_revert: Regex,
    /// `^fix.*revert` — a fix subject that references a revert.
    fix_revert: Regex,
}

/// Global, lazily-initialized revert pattern set.
fn patterns() -> &'static RevertPatterns {
    static PATTERNS: OnceLock<RevertPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        // SAFETY of expect: these literals are validated by the test
        // [`tests::patterns_compile`] — any regression is caught at test
        // time, not at runtime.
        RevertPatterns {
            // `(?i)` case-insensitive; `^\s*` tolerates leading whitespace;
            // `revert\b` requires a word boundary so `reverting` /
            // `reverted` (which continue the word) do NOT match.
            leading_revert: Regex::new(r"(?i)^\s*revert\b")
                .expect("leading_revert pattern compiles"),
            // `fix` (optionally `fix:` / `fixes` / `fixed`) followed anywhere
            // on the subject line by the word `revert`.
            fix_revert: Regex::new(r"(?i)^\s*fix\w*\b.*\brevert\b")
                .expect("fix_revert pattern compiles"),
        }
    })
}

/// Return `true` if `message` looks like a revert commit.
///
/// Why: the persisted `commits.is_revert` column and the report-time revert
/// rate must agree; routing both through this function guarantees parity
/// (issue #377).
/// What: tests the message's first line against the recognized revert forms
/// (`^revert`, `^fix.*revert`, case-insensitive). Multi-line bodies are
/// ignored — only the subject can declare a revert.
/// Test: [`tests`] below cover each accepted form and the false-positive
/// guards.
pub fn is_revert(message: &str) -> bool {
    let first_line = message.lines().next().unwrap_or(message);
    let p = patterns();
    p.leading_revert.is_match(first_line) || p.fix_revert.is_match(first_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_compile() {
        // Force lazy init; malformed patterns panic here.
        let _ = patterns();
    }

    #[test]
    fn matches_git_auto_revert() {
        assert!(is_revert("Revert \"feat: add login\""));
        assert!(is_revert("revert \"fix: thing\""));
    }

    #[test]
    fn matches_conventional_commit_revert() {
        assert!(is_revert("revert: bad merge"));
        assert!(is_revert("revert(auth): drop broken guard"));
        assert!(is_revert("Revert(scope): something"));
    }

    #[test]
    fn matches_leading_revert_word() {
        assert!(is_revert("Revert this change"));
        assert!(is_revert("REVERT everything"));
        assert!(is_revert("  revert with leading spaces"));
    }

    #[test]
    fn matches_fix_revert() {
        assert!(is_revert("Fix botched revert of #42"));
        assert!(is_revert("fix: re-apply after revert"));
        assert!(is_revert("fixes the revert that broke CI"));
    }

    #[test]
    fn ignores_body_only_mentions() {
        // The subject does not announce a revert; a body mention must not
        // flip the flag.
        let msg = "feat: add caching\n\nThis effectively reverts the earlier approach.";
        assert!(!is_revert(msg));
    }

    #[test]
    fn rejects_prose_and_false_positives() {
        // Subject discusses reverts but does not begin with `revert` and is
        // not a `fix...revert`.
        assert!(!is_revert("Refactor revert handling"));
        assert!(!is_revert("Add tests for revert path"));
        // Word-boundary guard: `reverting` / `reverted` continue the word.
        assert!(!is_revert("reverting the decision later"));
        assert!(!is_revert("reverted earlier per discussion"));
        // Unrelated subjects.
        assert!(!is_revert("feat: add login"));
        assert!(!is_revert("Fix bug in feature"));
        assert!(!is_revert(""));
    }
}
