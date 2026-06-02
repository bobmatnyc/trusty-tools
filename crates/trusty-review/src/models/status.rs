//! Review run status — distinguishes an authoritative verdict from a skip or a
//! loudly-labelled degraded run (#590).
//!
//! Why: trusty-review's value is the code/static-analysis context it injects.  A
//! review produced WITHOUT that context is actively harmful (false confidence).
//! When a REQUIRED dependency (trusty-search / trusty-analyze) is unavailable the
//! pipeline must NOT emit a normal verdict — it must skip loudly.  And when an
//! operator explicitly opts into a degraded run, the result must be visibly
//! non-authoritative.  A typed status field lets every sink (CLI exit code, the
//! `/review` JSON, the webhook) make that distinction without string-matching the
//! free-form `error` field.
//!
//! What: `ReviewStatus` with `Completed` (authoritative), `Skipped` (a required
//! dependency was unavailable — no verdict was produced), and `Degraded` (an
//! opted-in context-free run whose verdict is explicitly non-authoritative).
//! Serialises `snake_case`.
//!
//! Test: `review_status_serde_roundtrip`, `review_status_is_authoritative`.

use serde::{Deserialize, Serialize};

/// Outcome class of a review run.
///
/// Why: callers need to branch on whether the verdict is trustworthy: a skipped
/// run has no real verdict (the CLI should exit non-zero, the service should not
/// post), and a degraded run carries a verdict that must be flagged as
/// non-authoritative.  A clean APPROVE and a context-free APPROVE must never be
/// indistinguishable.
/// What: three variants; `Completed` is the default so existing call-sites and
/// deserialised older logs read as authoritative.
/// Test: `review_status_serde_roundtrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// The review ran with full required context; the verdict is authoritative.
    #[default]
    Completed,
    /// A REQUIRED context dependency (trusty-search / trusty-analyze) was
    /// unavailable, so the review was skipped — NO verdict was produced.  This is
    /// not an APPROVE; the caller must surface the skip (non-zero exit / no post).
    Skipped,
    /// The review ran WITHOUT a required context dependency because the operator
    /// explicitly opted out (`require_search`/`require_analyze = false`).  The
    /// verdict is present but explicitly NON-AUTHORITATIVE and loudly labelled.
    Degraded,
}

impl ReviewStatus {
    /// Returns `true` only for `Completed` — a verdict produced with full context.
    ///
    /// Why: sinks gate posting/exit codes on authoritativeness; centralising the
    /// check avoids each call-site re-deriving the rule.
    /// What: `matches!(self, Completed)`.
    /// Test: `review_status_is_authoritative`.
    pub fn is_authoritative(&self) -> bool {
        matches!(self, ReviewStatus::Completed)
    }

    /// Returns `true` when no real verdict was produced (a required dep was down).
    ///
    /// Why: the CLI must exit non-zero and the service must not post a skipped
    /// review; this is the single predicate both paths consult.
    /// What: `matches!(self, Skipped)`.
    /// Test: `review_status_is_authoritative`.
    pub fn is_skipped(&self) -> bool {
        matches!(self, ReviewStatus::Skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_status_serde_roundtrip() {
        let cases = [
            (ReviewStatus::Completed, "\"completed\""),
            (ReviewStatus::Skipped, "\"skipped\""),
            (ReviewStatus::Degraded, "\"degraded\""),
        ];
        for (status, expected) in cases {
            let json = serde_json::to_string(&status).expect("serialise");
            assert_eq!(json, expected, "serialise mismatch for {status:?}");
            let back: ReviewStatus = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(back, status);
        }
    }

    #[test]
    fn review_status_default_is_completed() {
        assert_eq!(ReviewStatus::default(), ReviewStatus::Completed);
    }

    #[test]
    fn review_status_is_authoritative() {
        assert!(ReviewStatus::Completed.is_authoritative());
        assert!(!ReviewStatus::Skipped.is_authoritative());
        assert!(!ReviewStatus::Degraded.is_authoritative());

        assert!(ReviewStatus::Skipped.is_skipped());
        assert!(!ReviewStatus::Completed.is_skipped());
        assert!(!ReviewStatus::Degraded.is_skipped());
    }
}
