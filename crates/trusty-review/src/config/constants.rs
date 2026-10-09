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
/// What: corresponds to `pr_threshold` in per-repo config.  Renamed from the
/// former `VERIFY_CANDIDATE_MIN_CONFIDENCE` (Phase 2, #583); that later name, a
/// verifier candidate floor, was removed by #8904 when every finding became a
/// candidate. This constant always meant the PR high-confidence flag.
pub const PR_HIGH_CONFIDENCE_THRESHOLD: f32 = 0.95;

/// Minimum confidence to include a finding in the verification round.
///
/// Why: low-confidence findings are not worth the latency cost of a verifier
/// LLM call.
/// What: findings below this are skipped by the verifier and treated as
/// unverified.
pub const VERIFICATION_MIN_CONFIDENCE: f32 = 0.65;

/// Demoted confidence assigned to a finding the verifier REFUTED (Phase 2, #583).
///
/// Why: a refuted finding must not surface or drive a verdict, but the spec
/// (REV-606) requires we keep it on the result for transparency rather than
/// silently dropping it.  Demoting its confidence below every advisory / block
/// gate makes `derive_verdict` treat it as noise while the `verified` field
/// records *why* it was demoted.
/// What: set below `FIX_ISSUE_MIN_CONFIDENCE` (0.60), `VERIFICATION_MIN_CONFIDENCE`
/// (0.65), and `LOW_CONFIDENCE_THRESHOLD` (0.65 in grade.rs) so a refuted
/// finding is always treated as advisory-only noise and collapses the floor.
/// Test: `refuted_finding_is_demoted_below_advisory_tier` in `verify_tests.rs`.
pub const VERIFY_REFUTED_CONFIDENCE: f32 = 0.10;

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

/// The owner every non-GitHub diff source reviews under (#9194).
///
/// Why: `post::finalize_review` never posts for this owner, and a source that
/// queries GitHub by owner must not query it for a local diff.
/// What: the `subject_of` sentinel; a real GitHub org named `local` is
/// assumed not to exist (Architect ruling Q7).
pub(crate) const LOCAL_OWNER: &str = "local";

/// Maximum length of the full diff text (characters) fed to the LLM.
///
/// Why: the reviewer default when this cap was set (Bedrock Claude Sonnet 4.6)
/// had a 200 K-token context window; the old 60 K-char cap (~15 K tokens) was overly conservative and
/// caused real PRs with large fixture churn to drop substantive code changes.
/// Raised to 160 K chars (≈40 K tokens) — still ~5× under the 200 K window —
/// to give the DiffAnalyzer noise filter enough headroom to work.
/// What: `truncate_diff` and `DiffAnalyzer::render_for_prompt` both use this cap
/// as their final safety net.  Closes: #624.
pub const MAX_DIFF_CHARS: usize = 160_000;

/// Maximum characters of each caller-context field (PR description, PR
/// discussion, referenced code) the reviewer and verifier receive.
///
/// Why: #8654 — `review_diff`'s `context` and `run`'s PR-context flags were
/// unbounded, so one field could crowd the diff out of the context window.
/// What: 64 K chars (≈16 K tokens) per field; all three plus a full
/// `MAX_DIFF_CHARS` diff stay well under the 200 K-token window. Applied once
/// in `run_review` by `cap_caller_context`, which marks every truncation.
pub const MAX_CALLER_CONTEXT_CHARS: usize = 64_000;

/// Maximum characters of the fetched PR body the reviewer receives (#9192).
///
/// Why: `include_pr_body` merges third-party text into the reviewer's PR
/// description; it gets its own cap, apart from the caller's own text.
/// What: 64 K chars, the same budget as one caller-context field; a cut is
/// marked and recorded `truncated` in the source ledger.
pub const MAX_PR_BODY_CHARS: usize = 64_000;

// ─── Issue docs (#9197) ───────────────────────────────────────────────────────
//
// Drop order, shared by B2a (caller `issue_docs`) and B2b (fetched issues):
// docs are kept in priority order — caller-supplied docs in the order given,
// then fetched docs in PR-body order. A doc that would pass its origin's item
// limit (`MAX_ISSUE_DOCS` supplied, `MAX_LINKED_ISSUE_FETCHES` fetched) or
// `MAX_ISSUE_SECTION_CHARS` is omitted whole, and so is every later doc of
// its origin, so the fetched tail goes first, a supplied doc is never dropped
// for a fetched one, and no doc is ever cut to fit the section.
// A doc whose id repeats an earlier one is omitted and takes no budget.

/// Maximum characters of one issue doc's body the reviewer receives (#9197).
///
/// Why: one long issue must not crowd out the others or the diff.
/// What: 16 K chars per body; a cut is marked and recorded `truncated`.
pub const MAX_ISSUE_DOC_CHARS: usize = 16_000;

/// Maximum number of issue docs one review renders (#9197).
pub const MAX_ISSUE_DOCS: usize = 8;

/// Maximum total body characters across every issue doc (#9197), shared with
/// fetched issues (B2b). See the drop order above.
pub const MAX_ISSUE_SECTION_CHARS: usize = 48_000;

/// Issue docs past which the dropped tail is one ledger item (#9197).
///
/// Why: `issue_docs` is an unbounded MCP array; one ledger item per dropped
/// doc would grow the response with the input.
pub const MAX_ISSUE_DOCS_LISTED: usize = 64;

/// Maximum characters of an issue doc's `title` or `url` line (#9197).
///
/// Why: both sit outside the data fence, so each must be one bounded line.
pub const MAX_ISSUE_DOC_LINE_CHARS: usize = 512;

/// Largest `--issue-docs-file` `run` reads: 256 KiB (#9197).
pub const MAX_ISSUE_DOCS_FILE_BYTES: u64 = 256 * 1024;

/// Most issues `fetch_linked_issues` fetches for one review (#9197, B2b).
///
/// Why: each fetch is a GitHub API call; a hostile PR body may link
/// thousands. Refs skipped as supplied, the PR itself, or another repository
/// take none of the budget.
pub const MAX_LINKED_ISSUE_FETCHES: usize = 5;

/// Seconds one linked-issue fetch may take (#9197, B2b ruling Q7).
///
/// Why: the GitHub tickets backend's HTTP client has no timeout, so this
/// bound is the only one; a fetch past it is recorded `unavailable`.
pub const LINKED_ISSUE_TIMEOUT_SECS: u64 = 10;

/// Maximum characters of one ADR/spec/SLD doc the reviewer sees (#9193).
///
/// Why: Architect ruling Q6: the issue-doc caps, applied per doc read at the
/// PR head. A cut is marked and recorded `truncated`.
pub const MAX_SPEC_DOC_CHARS: usize = 16_000;
/// Maximum docs one review renders under `## Referenced docs` (#9193).
pub const MAX_SPEC_DOCS: usize = 6;
/// Maximum kept characters across every rendered doc (#9193); the first doc
/// past it is omitted whole, with every doc after it.
pub const MAX_SPEC_SECTION_CHARS: usize = 48_000;
/// Maximum Contents API reads for `spec_docs` in one review (#9193).
pub const MAX_SPEC_DOC_FETCHES: usize = 12;
/// Maximum characters of the CLAUDE.md section, and of one CLAUDE.md (#9193).
pub const MAX_CLAUDE_MD_CHARS: usize = 16_000;
/// Maximum Contents API reads for `claude_md`: the root and three nested files.
pub const MAX_CLAUDE_MD_FETCHES: usize = 4;
/// Search hits one doc-discovery query asks trusty-search for (#9193).
pub const MAX_DOC_DISCOVERY_HITS: u32 = 8;
/// Seconds one doc read or the discovery search may take (#9193).
pub const DOC_READ_TIMEOUT_SECS: u64 = 10;

// ─── Changed files at the PR head (#9195) ────────────────────────────────────
// Each file is shown whole or named as left out; none is cut to fit. The
// `Not shown:` list comes off the top of the budget (Architect ruling D).

/// Default byte budget for full changed-file text (#9195, ruling Q5).
///
/// Why: about 30k tokens, under the 160,000-character diff cap.
pub const DEFAULT_CHANGED_FILES_BUDGET: usize = 120_000;
/// Largest byte budget a caller may ask for; a larger one is clamped and the
/// clamp is named in the ledger (#9195, ruling Q5).
pub const MAX_CHANGED_FILES_BUDGET: usize = 400_000;
/// Most Contents API reads for `changed_files` in one review (#9195, ruling C).
pub const MAX_CHANGED_FILE_FETCHES: usize = 60;
/// Most changed-file reads in flight at once (#9195, amendment 7).
pub const CHANGED_FILE_FETCH_CONCURRENCY: usize = 8;
/// Longest path the `Not shown:` list and a ledger item id carry (#9195,
/// amendment 4; precedent `MAX_ISSUE_DOC_LINE_CHARS`).
pub const MAX_CHANGED_FILE_PATH_CHARS: usize = 512;

// ─── Changed-symbol call graph (#9196) ───────────────────────────────────────
// Architect ruling Q5: constants, no budget parameter. AC2: capped per symbol
// and in total, each cut marked.

/// Most changed symbols one review asks the call graph about (#9196).
pub const MAX_SYMBOL_CONTEXT_SYMBOLS: usize = 12;
/// Most callers, callees or test lines one symbol's block lists, each (#9196).
pub const MAX_SYMBOL_EDGES: usize = 6;
/// Most characters of one symbol's block; a longer one is cut and marked.
pub const MAX_SYMBOL_BLOCK_CHARS: usize = 2_500;
/// Most characters of the whole symbol section; a symbol past it is left out
/// whole and named (#9196).
pub const MAX_SYMBOL_SECTION_CHARS: usize = 24_000;
/// Seconds one call-chain read may take (#9196).
pub const SYMBOL_CALL_TIMEOUT_SECS: u64 = 10;
/// Seconds the whole call-chain phase may take; a symbol not started by then
/// is left out (#9196).
pub const SYMBOL_PHASE_DEADLINE_SECS: u64 = 30;
/// Most call-chain reads in flight at once (#9196).
pub const SYMBOL_CALL_CONCURRENCY: usize = 4;
/// Most bytes of one call-chain report the parser reads (#9196).
pub const MAX_SYMBOL_REPORT_BYTES: usize = 262_144;
/// Most symbols the `Not shown:` list and the ledger name one by one; the
/// rest fold into one line and one item (#9196, precedent
/// `MAX_ISSUE_DOCS_LISTED`).
pub const MAX_SYMBOLS_LISTED: usize = 64;

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
            ("PR_HIGH_CONFIDENCE_THRESHOLD", PR_HIGH_CONFIDENCE_THRESHOLD),
            ("VERIFY_REFUTED_CONFIDENCE", VERIFY_REFUTED_CONFIDENCE),
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
            BLOCK_VERDICT_MIN_CONFIDENCE <= PR_HIGH_CONFIDENCE_THRESHOLD,
            "block_verdict must be <= pr_high_confidence_threshold"
        );
        // A refuted finding must be demoted strictly below the include-in-review
        // gate so it can never resurface or drive a verdict.
        const _: () = assert!(
            VERIFY_REFUTED_CONFIDENCE < FIX_ISSUE_MIN_CONFIDENCE,
            "refuted confidence must be below the review-include gate"
        );
    }

    #[test]
    fn review_version_is_tr_prefixed() {
        assert!(
            REVIEW_VERSION.starts_with("tr-"),
            "REVIEW_VERSION must start with 'tr-'"
        );
    }

    /// Regression guard: `MAX_DIFF_CHARS` must be 160_000 (the cap raised from
    /// 60_000 in #624 to give the DiffAnalyzer noise filter enough headroom).
    ///
    /// Why: a silent regression to the old 60 K cap would make the DiffAnalyzer
    /// pointless — noise-filtered diffs would still be truncated at ~15 K tokens.
    /// What: asserts the exact numeric value; changing it must fail this test so
    /// the change is explicit and intentional.
    /// Test: this test itself.
    #[test]
    fn max_diff_chars_is_160k() {
        assert_eq!(
            MAX_DIFF_CHARS, 160_000,
            "MAX_DIFF_CHARS must be 160_000 (raised from 60_000 in #624)"
        );
        // Also assert it comfortably fits below the reviewer model's context window.
        // 200 K tokens × ~4 chars/token ≈ 800 K chars → 160 K is well within range.
        const {
            assert!(
                MAX_DIFF_CHARS < 400_000,
                "MAX_DIFF_CHARS must remain well below the 200 K-token context window"
            )
        };
    }

    /// Regression guard: `truncate_diff` must cut at a hunk boundary and not
    /// exceed the cap significantly (covers the new 160 K value end-to-end).
    ///
    /// Why: ensures the `truncate_diff` implementation respects the updated cap.
    /// What: builds a diff longer than MAX_DIFF_CHARS, asserts the truncation
    /// marker appears and the result does not greatly exceed MAX_DIFF_CHARS.
    /// Test: this test itself.
    #[test]
    fn max_diff_chars_truncation_consistent() {
        use crate::pipeline::diff::truncate_diff;
        let over = "a".repeat(MAX_DIFF_CHARS + 5_000);
        let result = truncate_diff(&over);
        assert!(
            result.contains("[DIFF TRUNCATED"),
            "truncated diff must contain the marker"
        );
        assert!(
            result.len() <= MAX_DIFF_CHARS + 300,
            "truncated result must not greatly exceed the cap: len={}",
            result.len()
        );
    }
}
