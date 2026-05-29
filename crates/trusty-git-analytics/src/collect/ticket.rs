//! Ticket-reference detection for commit messages.
//!
//! A commit is considered *ticketed* if its message contains any reference
//! to an external work-tracking system. We currently recognize:
//!
//! - **JIRA / Linear style**: `PROJ-123`, `ENG-456`, `ABC-9` —
//!   uppercase project key, hyphen, digits. The Linear identifier format
//!   (`ENG-123`, `FE-456`) is a subset of this pattern.
//! - **GitHub action-keyword refs**: `fixes #123`, `closes #45`,
//!   `resolves #7` (case-insensitive, also matches `fix`/`close`/`resolve`).
//! - **Azure DevOps work-item refs**: `AB#123`.
//!
//! **Note on bare `#N` refs (issue #445):** A bare `#N` preceded by
//! whitespace (the `gh_bare` pattern) is explicitly *excluded* from
//! [`is_ticketed`]. It fires on almost any multi-line commit body and was
//! inflating the ticketed rate to ~100%. The `gh_bare` pattern is still
//! used by [`extract_ticket_id`] to populate `commits.ticket_id` as a
//! last-resort identifier, so the data is not lost — it just no longer
//! counts as "ticketed" for quality-metric purposes.
//!
//! Patterns are compiled exactly once on first use via [`OnceLock`].

use std::sync::OnceLock;

use regex::Regex;

/// Compiled regexes used by [`is_ticketed`].
///
/// Note: `gh_bare` is intentionally excluded from this struct (issue #445).
/// A bare `#N` reference no longer qualifies a commit as "ticketed" — only
/// JIRA/Linear, GitHub action-keyword refs, and Azure DevOps refs do.
/// The bare pattern still lives in [`ExtractPatterns`] for `ticket_id` population.
struct TicketPatterns {
    jira: Regex,
    gh_action: Regex,
    azdo: Regex,
}

/// Global, lazily-initialized pattern set.
fn patterns() -> &'static TicketPatterns {
    static PATTERNS: OnceLock<TicketPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        // SAFETY of unwrap: these literals are validated by the test
        // [`patterns_compile`] below — any regression is caught at test
        // time, not at runtime.
        TicketPatterns {
            // JIRA / Linear: uppercase letters (>=1), optional digits, '-', digits.
            // Word boundaries prevent matching inside `FOO-BAR-1`-style identifiers'
            // middle, while still catching the trailing `BAR-1` segment.
            jira: Regex::new(r"\b[A-Z][A-Z0-9]*-\d+\b").expect("jira pattern compiles"),
            // GitHub action keyword: fix(es|ed)?|close(s|d)?|resolve(s|d)?  #123
            gh_action: Regex::new(r"(?i)\b(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)\s+#\d+\b")
                .expect("gh_action pattern compiles"),
            // Azure DevOps work-item reference: AB#123.
            // Bare #N intentionally excluded — collides with GitHub PR/issue numbers.
            azdo: Regex::new(r"\bAB#\d+\b").expect("azdo pattern compiles"),
        }
    })
}

/// Compiled extraction patterns used by [`extract_ticket_id`], ordered from
/// most-specific to least-specific so the highest-fidelity match wins.
struct ExtractPatterns {
    /// Azure DevOps work-item reference: `AB#123`.
    azdo: Regex,
    /// JIRA / Linear style: `PROJ-123`, `ENG-456`, `DRE-405`.
    jira: Regex,
    /// GitHub bare issue reference: `#123`.
    gh_bare: Regex,
}

/// Global, lazily-initialized extraction pattern set.
fn extract_patterns() -> &'static ExtractPatterns {
    static EXTRACT: OnceLock<ExtractPatterns> = OnceLock::new();
    EXTRACT.get_or_init(|| {
        // SAFETY of unwrap: all literals are validated by the test
        // [`extract_patterns_compile`] — any regression is caught at test time.
        ExtractPatterns {
            azdo: Regex::new(r"\bAB#\d+\b").expect("azdo extract pattern compiles"),
            jira: Regex::new(r"\b[A-Z][A-Z0-9]*-\d+\b").expect("jira extract pattern compiles"),
            gh_bare: Regex::new(r"(?:^|\s)(#\d+)\b").expect("gh_bare extract pattern compiles"),
        }
    })
}

/// Return `true` if `message` contains any recognized ticket reference.
///
/// Why: downstream metrics (ticketed-commit rate, quality score) must only
/// count commits that are genuinely linked to a tracked work item. A bare
/// `#N` reference (e.g. `#42` from a release note) is too noisy — it fires
/// on nearly every multi-line commit body and inflates the ticketed rate to
/// ~100% (issue #445). The `gh_bare` pattern is intentionally **excluded**
/// from this OR-chain; it is still used by [`extract_ticket_id`] to populate
/// `commits.ticket_id` as a last-resort identifier.
/// What: returns `true` for JIRA/Linear identifiers (`PROJ-N`), GitHub
/// action-keyword refs (`closes #N`, `fixes #N`), and Azure DevOps refs
/// (`AB#N`). A bare `#N` with no action keyword does NOT make a commit
/// ticketed.
/// Test: `tests::ticketed_*` below; the critical regression cases are
/// `bare_hash_alone_is_NOT_ticketed`, `closes_hash_IS_ticketed`,
/// `jira_IS_ticketed`, and `azdo_IS_ticketed`.
///
/// # Examples
///
/// ```
/// use tga::collect::ticket::is_ticketed;
///
/// assert!(is_ticketed("ENG-123: add feature"));
/// assert!(is_ticketed("Fix login (closes #42)"));
/// assert!(!is_ticketed("misc cleanup"));
/// assert!(!is_ticketed("some note about #42"));
/// ```
pub fn is_ticketed(message: &str) -> bool {
    let p = patterns();
    p.jira.is_match(message) || p.gh_action.is_match(message) || p.azdo.is_match(message)
}

/// Extract the first recognizable ticket identifier from a commit message.
///
/// Why: `commits.ticket_id` must be populated at insert time so that JIRA
/// classification and ticket-rate metrics work without requiring a separate
/// `tga backfill ticket-ids` pass. Issue #316 identified that 32% of
/// uncategorized commits had clearly extractable JIRA IDs (e.g. `BB-2746`,
/// `SRE-3104`, `DRE-405`) but NULL `ticket_id` because this extraction
/// only happened during backfill, not during `tga collect`.
/// What: tests the message against ADO (`AB#N`), JIRA/Linear (`PROJ-N`),
/// and GitHub bare (`#N`) patterns in that priority order; returns the
/// first match as `Some(String)`, or `None` when no ticket ref is found.
/// Test: `tests::extract_ticket_id_*` below; also exercised by
/// `collect::git::extractor` tests that verify `ticket_id` is populated
/// at INSERT time during `tga collect`.
///
/// # Examples
///
/// ```
/// use tga::collect::ticket::extract_ticket_id;
///
/// assert_eq!(extract_ticket_id("BB-2746: refactor auth"), Some("BB-2746".to_string()));
/// assert_eq!(extract_ticket_id("SRE-3104: increase RDS timeout"), Some("SRE-3104".to_string()));
/// assert_eq!(extract_ticket_id("DRE-405 fix demand calculation"), Some("DRE-405".to_string()));
/// assert_eq!(extract_ticket_id("fixes #99"), Some("#99".to_string()));
/// assert_eq!(extract_ticket_id("misc cleanup"), None);
/// ```
pub fn extract_ticket_id(message: &str) -> Option<String> {
    let p = extract_patterns();

    // ADO: AB#123 — most specific, checked first.
    if let Some(m) = p.azdo.find(message) {
        return Some(m.as_str().to_string());
    }

    // JIRA / Linear: PROJ-123.
    if let Some(m) = p.jira.find(message) {
        return Some(m.as_str().to_string());
    }

    // GitHub bare: #123 — the capture group strips the leading whitespace
    // that the pattern uses as a left-boundary guard.
    if let Some(caps) = p.gh_bare.captures(message) {
        if let Some(m) = caps.get(1) {
            return Some(m.as_str().to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_compile() {
        // Force lazy init; if any pattern is malformed this will panic.
        let _ = patterns();
    }

    #[test]
    fn extract_patterns_compile() {
        // Force lazy init; if any pattern is malformed this will panic.
        let _ = extract_patterns();
    }

    // ── extract_ticket_id: issue #316 sample commits ──────────────────────

    #[test]
    fn extract_ticket_id_bb_2746() {
        // Sample from issue #316 — was producing NULL ticket_id before fix.
        assert_eq!(
            extract_ticket_id("BB-2746: refactor auth service"),
            Some("BB-2746".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_sre_3104() {
        // Sample from issue #316 — was producing NULL ticket_id before fix.
        assert_eq!(
            extract_ticket_id("SRE-3104: increase RDS connection timeout"),
            Some("SRE-3104".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_dre_405() {
        // Sample from issue #316 — note: no colon separator, space only.
        assert_eq!(
            extract_ticket_id("DRE-405 fix demand calculation"),
            Some("DRE-405".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_returns_none_for_plain_message() {
        assert_eq!(extract_ticket_id("misc cleanup"), None);
        assert_eq!(extract_ticket_id("update README"), None);
        assert_eq!(extract_ticket_id("bump version to 1.2.3"), None);
    }

    #[test]
    fn extract_ticket_id_github_bare_ref() {
        assert_eq!(extract_ticket_id("fixes #99"), Some("#99".to_string()));
    }

    #[test]
    fn extract_ticket_id_azdo_ref() {
        assert_eq!(
            extract_ticket_id("AB#42 implement feature"),
            Some("AB#42".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_azdo_preferred_over_jira() {
        // When both AB# and JIRA patterns appear, ADO wins (more specific).
        assert_eq!(
            extract_ticket_id("AB#10 fixes PROJ-99"),
            Some("AB#10".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_jira_preferred_over_gh_bare() {
        // JIRA is checked before bare GitHub ref.
        assert_eq!(
            extract_ticket_id("ENG-7 closes #10"),
            Some("ENG-7".to_string())
        );
    }

    #[test]
    fn extract_ticket_id_multiline_body() {
        let msg = "Refactor module structure\n\nRelates to SRE-999.\n";
        assert_eq!(extract_ticket_id(msg), Some("SRE-999".to_string()));
    }

    #[test]
    fn jira_style_is_ticketed() {
        assert!(is_ticketed("ENG-123: add feature"));
        assert!(is_ticketed("PROJ-1 initial commit"));
        assert!(is_ticketed("Backport from upstream (ABC-4567)"));
    }

    #[test]
    fn linear_style_is_ticketed() {
        // Linear identifiers are a subset of the JIRA pattern.
        assert!(is_ticketed("FE-456 fix login"));
        assert!(is_ticketed("API-9 add endpoint"));
    }

    #[test]
    fn github_action_keyword_is_ticketed() {
        assert!(is_ticketed("Fix race condition, fixes #123"));
        assert!(is_ticketed("closes #45"));
        assert!(is_ticketed("Resolves #7 by reworking auth"));
        assert!(is_ticketed("CLOSED #99")); // case-insensitive
    }

    /// Why: regression guard for issue #445. Bare `#N` refs no longer make a
    /// commit "ticketed" — only JIRA/Linear, GitHub action keywords, and ADO
    /// refs do. This test confirms bare refs are NOT ticketed while confirming
    /// they still produce a `ticket_id` via `extract_ticket_id`.
    /// What: asserts `is_ticketed` returns false for bare `#N`, and that
    /// action keywords and JIRA refs still return true.
    /// Test: this test itself.
    #[test]
    fn bare_hash_alone_is_not_ticketed() {
        // Bare #N with no action keyword is NOT ticketed (issue #445 fix).
        assert!(!is_ticketed("Bug from #123 still present"));
        assert!(!is_ticketed("#42 follow-up"));
        assert!(!is_ticketed("some note about #42"));
        // But the ticket_id is still extractable.
        assert_eq!(extract_ticket_id("#42 follow-up"), Some("#42".to_string()));
        assert_eq!(
            extract_ticket_id("Bug from #123 still present"),
            Some("#123".to_string())
        );
    }

    #[test]
    fn closes_hash_is_ticketed() {
        // Action keyword + bare ref IS ticketed.
        assert!(is_ticketed("closes #42"));
        assert!(is_ticketed("fixes #123"));
        assert!(is_ticketed("resolves #7"));
    }

    #[test]
    fn jira_is_ticketed() {
        assert!(is_ticketed("ENG-123: add feature"));
        assert!(is_ticketed("PROJ-1 initial commit"));
    }

    #[test]
    fn azdo_is_ticketed() {
        assert!(is_ticketed("AB#1234 implement new feature"));
        assert!(is_ticketed("Refactor module (AB#42)"));
    }

    #[test]
    fn plain_message_is_not_ticketed() {
        assert!(!is_ticketed("misc cleanup"));
        assert!(!is_ticketed("update README"));
        assert!(!is_ticketed("bump version to 1.2.3"));
        // Hex color shouldn't false-positive — `#abc123` is not preceded by
        // whitespace+digits-only.
        assert!(!is_ticketed("set color to #abc123"));
        // Lowercase project key is not a JIRA identifier.
        assert!(!is_ticketed("eng-123 lowercase doesn't count"));
    }

    #[test]
    fn multiline_body_with_ticket_is_ticketed() {
        // JIRA ref anywhere in body → ticketed.
        let msg = "Refactor module structure\n\nMoves things around.\nRelates to PROJ-789.\n";
        assert!(is_ticketed(msg));

        // Bare #N in body is NOT ticketed (issue #445 fix); action keyword is.
        let msg2 = "First line no ticket\n\nSee #321 for context.";
        assert!(!is_ticketed(msg2));

        let msg3 = "First line no ticket\n\nCloses #321.";
        assert!(is_ticketed(msg3));
    }

    #[test]
    fn azdo_ab_ref_is_ticketed() {
        assert!(is_ticketed("AB#1234 implement new feature"));
        assert!(is_ticketed("Refactor module (AB#42)"));
        assert!(is_ticketed("First line\n\nbody mentions AB#7 explicitly"));
    }

    #[test]
    fn bare_hash_without_ab_prefix_is_not_azdo() {
        // GitHub bare `#N` still matches via the gh_bare pattern, but it
        // must NOT match the ADO `AB#` pattern specifically.
        let p = patterns();
        assert!(!p.azdo.is_match("#1234 some work"));
        assert!(!p.azdo.is_match("fixes #99"));
        // And the existing JIRA pattern must not accidentally fire on AB#N
        // (different separator: `#` vs `-`).
        assert!(!p.jira.is_match("AB#1234"));
    }

    #[test]
    fn empty_message_is_not_ticketed() {
        assert!(!is_ticketed(""));
        assert!(!is_ticketed("\n\n"));
    }
}
