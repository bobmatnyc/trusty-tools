//! Pipeline-tuning constants for trusty-review.
//!
//! Why: centralises all confidence-threshold constants so they have one
//! authoritative definition and are easy to audit, override in config files,
//! or extend.  Magic numbers scattered across the pipeline lead to
//! inconsistent gate values (lesson learned §12.6).
//! What: every constant matches the Python predecessor's default and is
//! annotated with its spec reference.
//! Test: `constants_are_in_unit_interval` asserts that all confidence
//! thresholds are in `[0.0, 1.0]`.

// ─── Confidence thresholds (spec §06 REV-502, source-analysis §2.3) ──────────

/// Minimum confidence to include a finding as a fix suggestion in the review.
///
/// Why: filters out low-confidence hunches before they reach the review body.
/// What: findings below this threshold are omitted entirely.
pub const FIX_ISSUE_MIN_CONFIDENCE: f32 = 0.60;

/// Minimum confidence for a finding to be eligible to file a tracker issue.
///
/// Why: only high-confidence findings justify opening a GitHub/JIRA issue.
/// What: corresponds to `issue_threshold` in per-repo config; overrideable.
pub const BLOCK_ISSUE_MIN_CONFIDENCE: f32 = 0.75;

/// Minimum confidence for a finding to be a BLOCK-tier verdict candidate.
///
/// Why: BLOCK is the strongest verdict tier; it requires very high confidence.
/// What: corresponds to `block_threshold` in per-repo config.
pub const BLOCK_VERDICT_MIN_CONFIDENCE: f32 = 0.90;

/// Minimum confidence for a finding to be flagged as high-confidence in the
/// PR comment.
///
/// Why: the PR comment can distinguish "FYI" from "definitely fix this".
/// What: corresponds to `pr_threshold` in per-repo config.
pub const VERIFY_CANDIDATE_MIN_CONFIDENCE: f32 = 0.95;

/// Minimum confidence to include a finding in the verification round.
///
/// Why: low-confidence findings are not worth the latency cost of a verifier
/// LLM call.
/// What: findings below this are skipped by the verifier and treated as
/// unverified.
pub const VERIFICATION_MIN_CONFIDENCE: f32 = 0.65;

// ─── Suppression (spec §06 REV-530) ──────────────────────────────────────────

/// Jaccard overlap threshold for suppression pattern matching.
///
/// Why: substring matching alone misses paraphrases; word-overlap matching
/// catches them.
/// What: if the normalised word-set Jaccard similarity between the finding
/// description and a suppression pattern reaches this value, the finding is
/// suppressed.
pub const SUPPRESS_OVERLAP_THRESHOLD: f32 = 0.70;

/// Finding similarity threshold used by the related-finding dedup helper
/// (distinct from suppression — spec §06 REV-530 note).
pub const FINDING_SIMILARITY_THRESHOLD: f32 = 0.60;

// ─── Dedup / pipeline ─────────────────────────────────────────────────────────

/// Seconds after which a dedup claim is considered stale and may be purged.
///
/// Why: a crashed reviewer leaves a claim in the store forever without this.
/// What: claims older than this value are ignored and overwritten on the next
/// claim attempt.
pub const DEDUP_STALE_SECS: u64 = 7200; // 2 hours.

/// Maximum length of the full diff text (characters) fed to the LLM.
///
/// Why: very large diffs cause token budget overruns and poor review quality.
/// What: the diff summarizer truncates after this many characters.
pub const MAX_DIFF_CHARS: usize = 60_000;

/// Maximum number of context files retrieved from trusty-search per review.
pub const MAX_CONTEXT_FILES: usize = 20;

/// Maximum additional enrichment rounds (spec REV-502).
pub const MAX_ENRICHMENT_ROUNDS: u32 = 3;

/// Maximum tracker issues filed per PR.
pub const FIX_ISSUE_MAX_PER_PR: u32 = 3;

// ─── Effort gate (spec §07 REV-605) ──────────────────────────────────────────

/// Effort levels that are eligible for tracker-issue filing.
///
/// Why: HIGH-effort findings are unlikely to be actioned quickly; only
/// Low/Medium findings are issue-filed by default.
/// What: matches `FIX_ISSUE_ALLOWED_EFFORTS` in the Python predecessor
/// (source-analysis §2.3).
pub const FIX_ISSUE_ALLOWED_EFFORTS: &[&str] = &["low", "medium"];

// ─── Review version string ────────────────────────────────────────────────────

/// Pipeline version identifier embedded in every `ReviewResult`.
///
/// Why: allows tooling to distinguish review logs produced by different
/// pipeline versions without parsing the review body.
/// What: written to `ReviewResult::review_version` on every review.
pub const REVIEW_VERSION: &str = "tr-0.1";

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_in_unit_interval() {
        for (name, val) in [
            ("FIX_ISSUE_MIN_CONFIDENCE", FIX_ISSUE_MIN_CONFIDENCE),
            ("BLOCK_ISSUE_MIN_CONFIDENCE", BLOCK_ISSUE_MIN_CONFIDENCE),
            ("BLOCK_VERDICT_MIN_CONFIDENCE", BLOCK_VERDICT_MIN_CONFIDENCE),
            (
                "VERIFY_CANDIDATE_MIN_CONFIDENCE",
                VERIFY_CANDIDATE_MIN_CONFIDENCE,
            ),
            ("VERIFICATION_MIN_CONFIDENCE", VERIFICATION_MIN_CONFIDENCE),
            ("SUPPRESS_OVERLAP_THRESHOLD", SUPPRESS_OVERLAP_THRESHOLD),
            ("FINDING_SIMILARITY_THRESHOLD", FINDING_SIMILARITY_THRESHOLD),
        ] {
            assert!(
                (0.0..=1.0).contains(&val),
                "{name} = {val} is outside [0.0, 1.0]"
            );
        }
    }

    #[test]
    fn threshold_ordering() {
        // Issue threshold must be <= block threshold (per spec REV-511).
        const _: () = assert!(
            BLOCK_ISSUE_MIN_CONFIDENCE <= BLOCK_VERDICT_MIN_CONFIDENCE,
            "block_issue must be <= block_verdict"
        );
        // Block threshold must be <= pr_threshold.
        const _: () = assert!(
            BLOCK_VERDICT_MIN_CONFIDENCE <= VERIFY_CANDIDATE_MIN_CONFIDENCE,
            "block_verdict must be <= verify_candidate"
        );
    }

    #[test]
    fn review_version_is_tr_prefixed() {
        assert!(
            REVIEW_VERSION.starts_with("tr-"),
            "REVIEW_VERSION must start with 'tr-'"
        );
    }
}
