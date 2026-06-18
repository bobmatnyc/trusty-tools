//! Core data models for trusty-review.
//!
//! Why: defines the shared data shapes used by all pipeline stages and
//! stored in the review log.  Keeping them in a dedicated module ensures
//! a single authoritative definition and prevents type drift between the
//! pipeline, the LLM provider layer, and the store.
//! What: exposes `Verdict`, `Effort`, `VerifyOutcome`, `Finding`
//! (FixSuggestion), and `ReviewResult` — all serde-serialisable.
//! Test: `verdict_serde_roundtrip`, `review_result_serde_roundtrip`,
//! and `finding_confidence_clamping` in this module.

use serde::{Deserialize, Serialize};

pub mod status;
pub use status::ReviewStatus;

use crate::config::constants::REVIEW_VERSION;

// ─── Verdict ──────────────────────────────────────────────────────────────────

/// Review verdict tier aligned to the duetto-code-intelligence board grades.
///
/// Why: the pipeline maps LLM output to one of these tiers to drive the
/// GitHub review action (approve vs request-changes vs block) and the
/// notification payload.  Using the same string tokens as the calibration
/// board enables clean round-trips through JSON without any conversion.
/// What: serialises to EXACT board strings — `"APPROVE"`, `"APPROVE*"`,
/// `"REQUEST_CHANGES"`, `"BLOCK"`, `"UNKNOWN"`.  Deserialises the same.
/// Display prints the same strings.
///
/// # Fail-safe policy (fail-CLOSED since #1241)
/// When the pipeline encounters a genuine parse/transport error or a truncated
/// model response, the fail-safe default is `Unknown` (fail-CLOSED).  Ticket
/// #1241 supersedes spec REV-130's original fail-OPEN `Approve`: a silent APPROVE
/// on unparseable/truncated output posts a green check for a review that never
/// happened, which is a safety hole.  `Unknown` surfaces a clear "could not
/// review" state and is never treated as a merge-approval downstream.  The model
/// *itself* also emits `Unknown` when it judges the diff too truncated to assess,
/// so the board can distinguish "clean review" from "could not assess".
///
/// Test: `verdict_serde_roundtrip`, `verdict_display`,
/// `verdict_unknown_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verdict {
    /// No significant concerns; the reviewer is satisfied.
    Approve,
    /// Approve with non-blocking advisory notes (a concern worth noting, but
    /// not blocking). Board grade: `APPROVE*`.
    #[serde(rename = "APPROVE*")]
    ApproveWithReservations,
    /// Reviewer requests at least one real correctness/logic change before merge.
    RequestChanges,
    /// Critical issue — build-breaking, data-corrupting, or auth-bypassing.
    /// Must not merge without explicit bypass.
    Block,
    /// The diff was too truncated or contained insufficient context for the
    /// model to assess.  Not a clean APPROVE — the reviewer could not form an
    /// opinion.  Board grade: `UNKNOWN`.
    Unknown,
}

impl Verdict {
    /// Ordinal severity for this verdict (higher = more severe).
    ///
    /// Why: the grade-derivation (`grade.rs`) and verification re-derivation
    /// (`verify.rs`) paths both need a total order over verdicts to compute the
    /// stricter-of / less-severe-of two verdicts.  Before #1357 each module
    /// carried its OWN private `verdict_ord` with an identical mapping — two
    /// copies of the same ordinal table is a maintenance hazard (drift between
    /// them would silently corrupt the floor/ceiling logic).  Centralising the
    /// mapping here makes `Verdict` the single source of truth.
    /// What: returns `APPROVE=0 < APPROVE*=1 < REQUEST_CHANGES=2 < BLOCK=3`.
    /// `Unknown=4` is a terminal state (the diff was unassessable); callers
    /// short-circuit it before any ordinal comparison, so it never participates
    /// in normal flow — the value is defined only so the match is exhaustive.
    /// Test: `verdict_ordinal_is_monotonic` in this module; exercised
    /// transitively by `grade.rs::stricter_of` and `verify.rs::verdict_min`.
    pub fn ordinal(&self) -> u8 {
        match self {
            Verdict::Approve => 0,
            Verdict::ApproveWithReservations => 1,
            Verdict::RequestChanges => 2,
            Verdict::Block => 3,
            Verdict::Unknown => 4,
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Approve => write!(f, "APPROVE"),
            Verdict::ApproveWithReservations => write!(f, "APPROVE*"),
            Verdict::RequestChanges => write!(f, "REQUEST_CHANGES"),
            Verdict::Block => write!(f, "BLOCK"),
            Verdict::Unknown => write!(f, "UNKNOWN"),
        }
    }
}

// ─── Effort ───────────────────────────────────────────────────────────────────

/// Estimated remediation effort for a finding.
///
/// Why: only Low/Medium findings are eligible for tracker-issue filing
/// (spec §07 REV-605, source-analysis §2.3).
/// What: serialised as lowercase.
/// Test: `effort_serde_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Small change, typically under an hour.
    Low,
    /// Moderate change, typically a few hours.
    Medium,
    /// Large refactoring or cross-cutting change.
    High,
}

impl Effort {
    /// Returns `true` if this effort level is eligible for tracker-issue filing.
    ///
    /// Why: spec REV-605 restricts issue filing to Low/Medium; High efforts
    /// are noted in the review but not filed as issues automatically.
    /// What: returns `true` for `Low` and `Medium`.
    /// Test: `effort_issue_eligibility`.
    pub fn is_issue_eligible(&self) -> bool {
        matches!(self, Effort::Low | Effort::Medium)
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Effort::Low => write!(f, "low"),
            Effort::Medium => write!(f, "medium"),
            Effort::High => write!(f, "high"),
        }
    }
}

// ─── Verification outcome ─────────────────────────────────────────────────────

/// Outcome of the per-finding verification round.
///
/// Why: the pipeline needs to distinguish "verified true", "refuted by LLM",
/// "refuted because of error", etc., to compute the correct verdict and emit
/// the right alarm (spec §07 REV-606, source-analysis §12.1).
/// What: `ErrorRefuted` carries the error class string so config/lifecycle
/// failures are distinguishable in logs.
/// Test: `verify_outcome_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyOutcome {
    /// Verifier LLM confirmed the finding is real.
    Confirmed,
    /// Verifier LLM refuted the finding.
    Refuted,
    /// Verifier call failed with a config/lifecycle error (see spec REV-340).
    /// The finding is treated as refuted but an alarm is emitted.
    ErrorRefuted {
        /// Human-readable error class (e.g. `"ModelNotFound"`, `"AccessDenied"`).
        error_class: String,
    },
    /// Verifier call failed due to truncation / context-length overrun.
    TruncationRefuted,
    /// The finding was below the verification confidence threshold; not sent
    /// to the verifier.
    Skipped,
}

// ─── Finding category ──────────────────────────────────────────────────────────

/// The axis a finding speaks to: code correctness vs. intent/method conformance.
///
/// Why: the intent/method-conformance back gate (DOC-15, `SPEC-CONFORMANCE-02`,
/// #1359) adds a second, distinct *kind* of finding — one that flags a diff
/// **contradicting an explicit method the ticket/spec stated**, separate from a
/// correctness bug.  The two must be distinguishable so the verdict floor can
/// treat them differently: a conformance divergence caps at `REQUEST_CHANGES`
/// and NEVER drives `BLOCK` (BLOCK is reserved for correctness/safety), whereas a
/// correctness finding floors normally.  Threading a category through the finding
/// model is the minimal way to carry that distinction from the LLM JSON all the
/// way to `grade::severity_floor`.
/// What: a two-variant enum serialised kebab-case (`"correctness"` /
/// `"method-conformance"`).  It is `#[serde(default)]` (→ `Correctness`) wherever
/// it appears so pre-#1359 fixtures and LLM responses that omit `category` still
/// deserialise unchanged (back-compat — every legacy finding is a correctness
/// finding).
/// Test: `finding_category_serde_roundtrip`,
/// `finding_defaults_category_correctness`, and the parser/grade tests that
/// assert the conformance cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FindingCategory {
    /// A correctness / logic / safety finding (the default, legacy behaviour).
    #[default]
    Correctness,
    /// An intent/method-conformance divergence: the diff contradicts an explicit
    /// method the ticket or spec prescribed (`SPEC-CONFORMANCE-02`, M5).
    MethodConformance,
}

// ─── Finding ──────────────────────────────────────────────────────────────────

/// A single code-review finding / fix suggestion.
///
/// Why: the pipeline extracts findings from the LLM review body and attaches
/// metadata (confidence, effort, file location) so downstream stages (issue
/// filing, verification) can process them uniformly.
/// What: a direct port of `FixSuggestion` from spec §07 REV-602.  The
/// `verified` and `issue_eligible` fields are transient pipeline state; they
/// are serialised for the review log but not required on deserialisation.
/// Test: `finding_confidence_clamping`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Changed file path this finding refers to.
    pub file: String,
    /// Optional line number within the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Finding type / category (e.g. `"security"`, `"logic-error"`).
    pub kind: String,
    /// Human-readable description of the issue.
    pub description: String,
    /// Proposed fix or remediation suggestion (prose).
    pub suggestion: String,
    /// Exact replacement code for a committable GitHub `suggestion` block (#1415).
    ///
    /// Why: a one-click-appliable GitHub ```suggestion block must contain the
    /// *exact* replacement lines for the anchored location — not prose.  Keeping
    /// it in a dedicated field (separate from the prose `suggestion`) lets the
    /// renderer emit a committable block only when the model provides concrete
    /// replacement code, and degrade to prose otherwise.  `#[serde(default)]` +
    /// skip-if-none keeps every pre-#1415 finding deserialising with `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_replacement: Option<String>,
    /// Confidence score in `[0.0, 1.0]`.  Clamped at construction time.
    pub confidence: f32,
    /// Estimated remediation effort.
    pub effort: Effort,
    /// Which axis this finding speaks to (correctness vs. method-conformance).
    ///
    /// Why: the back gate (#1359) caps `MethodConformance` findings at
    /// `REQUEST_CHANGES` in the verdict floor.  `#[serde(default)]` keeps every
    /// pre-#1359 serialised finding deserialising as `Correctness`.
    #[serde(default)]
    pub category: FindingCategory,
    // ── Transient pipeline state ──────────────────────────────────────────
    /// Verification outcome; `None` before the verification round.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<VerifyOutcome>,
    /// Whether this finding is eligible for tracker-issue filing.
    #[serde(default)]
    pub issue_eligible: bool,
}

impl Finding {
    /// Construct a finding, clamping `confidence` to `[0.0, 1.0]`.
    ///
    /// Why: spec REV-607 requires clamping rather than erroring to defend
    /// against malformed LLM JSON.
    /// What: any out-of-range value is silently clamped.
    /// Test: `finding_confidence_clamping`.
    pub fn new(
        file: impl Into<String>,
        kind: impl Into<String>,
        description: impl Into<String>,
        suggestion: impl Into<String>,
        confidence: f32,
        effort: Effort,
    ) -> Self {
        Self {
            file: file.into(),
            line: None,
            kind: kind.into(),
            description: description.into(),
            suggestion: suggestion.into(),
            suggested_replacement: None,
            confidence: confidence.clamp(0.0, 1.0),
            effort,
            category: FindingCategory::Correctness,
            verified: None,
            issue_eligible: false,
        }
    }

    /// Set this finding's category, returning the finding for chaining.
    ///
    /// Why: `Finding::new` defaults to `Correctness` so every existing call site
    /// is unchanged; the back-gate parser (#1359) needs to override the category
    /// for an LLM-emitted `method-conformance` finding without a new constructor
    /// arity.  A small builder keeps construction ergonomic and the default safe.
    /// What: overwrites `self.category` and returns `self`.
    /// Test: `finding_with_category_sets_method_conformance`.
    #[must_use]
    pub fn with_category(mut self, category: FindingCategory) -> Self {
        self.category = category;
        self
    }
}

// ─── InlineCommentOut ───────────────────────────────────────────────────────────

/// A serialisable inline review comment anchored to a PR diff line (#1414).
///
/// Why: the runner builds the inline-comment set from the findings + diff, and it
/// must be carried on the `ReviewResult` so (a) the live poster can attach it to
/// the GitHub review and (b) the dry-run / MCP response can preview exactly what
/// would be posted.  It is a flat, owned projection (no borrows, serde-friendly)
/// distinct from the posting-layer's internal `InlineComment` type.
/// What: `path` + `line` are the new-side anchor; `body` is the rendered markdown.
/// Test: `inline_comment_out_serde_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineCommentOut {
    /// Changed file path (new side), e.g. `src/db.rs`.
    pub path: String,
    /// New-side line number the comment anchors to.
    pub line: u32,
    /// Rendered inline-comment markdown body.
    pub body: String,
}

// ─── ReviewResult ─────────────────────────────────────────────────────────────

/// The complete output of a PR review pass.
///
/// Why: every piece of review output is captured in one struct so it can be
/// serialised to the review log, compared between models, and diffed across
/// pipeline versions.
/// What: a subset of spec §07 REV-600 fields covering the MVP verdict loop.
/// The full field set (JIRA/APEX/Confluence context, multi-pass tokens, etc.)
/// will be added in later stages.
/// Test: `review_result_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResult {
    // ── PR identity ───────────────────────────────────────────────────────
    /// GitHub organisation.
    pub owner: String,
    /// GitHub repository name.
    pub repo: String,
    /// PR number.
    pub pr_number: u64,
    /// PR title.
    pub pr_title: String,
    /// GitHub PR URL.
    pub pr_url: String,

    // ── Review output ─────────────────────────────────────────────────────
    /// Full LLM review markdown.
    pub review_body: String,
    /// Normalised verdict.
    pub verdict: Verdict,
    /// Letter grade (e.g. "B+", "F"); `None` only for legacy pre-#732 results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade: Option<String>,
    /// Extracted findings.
    pub findings: Vec<Finding>,
    /// Per-line inline review comments that were (or, in dry-run, would be)
    /// posted to the PR diff (#1414).
    ///
    /// Why: when posting findings as inline per-line comments, the dry-run /MCP
    /// response must show the exact set of would-be inline comments so a caller
    /// can preview what live mode would attach to the diff.  Storing them on the
    /// result is the single source of truth for both the live POST and the
    /// dry-run preview.  `#[serde(default)]` keeps every pre-#1414 serialised
    /// result deserialising with an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_comments: Vec<InlineCommentOut>,

    // ── Telemetry ─────────────────────────────────────────────────────────
    /// Model id used for the main reviewer call.
    pub model: String,
    /// Input token count for the reviewer call.
    pub input_tokens: u32,
    /// Output token count for the reviewer call.
    pub output_tokens: u32,
    /// Estimated USD cost for the reviewer call.
    pub cost_estimate_usd: f64,
    /// Wall-clock latency of the reviewer call in milliseconds.
    pub latency_ms: u64,

    // ── Pipeline flags ────────────────────────────────────────────────────
    /// Run status — `Completed` (authoritative), `Skipped` (required context
    /// dep down; no verdict), or `Degraded` (opted-in context-free, #590).
    #[serde(default)]
    pub status: ReviewStatus,
    /// True if this is a dry run (no GitHub comment posted).
    pub dry_run: bool,
    /// True if the review comment was posted to GitHub.
    pub posted: bool,
    /// ISO-8601 timestamp of the review.
    pub timestamp: String,
    /// Non-None if the pipeline encountered an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    // ── Dedup ─────────────────────────────────────────────────────────────
    /// Head commit SHA; used as the dedup key.
    pub head_sha: String,

    // ── Versioning ────────────────────────────────────────────────────────
    /// Pipeline version string (e.g. `"tr-0.1"`).
    pub review_version: String,
}

impl ReviewResult {
    /// Construct a `ReviewResult` skeleton with sensible defaults.
    ///
    /// Why: most fields are filled in by pipeline stages; a builder avoids
    /// long constructor signatures and eases struct evolution.
    /// What: sets `review_version`, `dry_run = true`, `posted = false`, timestamps.
    /// Test: `review_result_serde_roundtrip`.
    pub fn new(
        owner: impl Into<String>,
        repo: impl Into<String>,
        pr_number: u64,
        pr_title: impl Into<String>,
        pr_url: impl Into<String>,
    ) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            pr_number,
            pr_title: pr_title.into(),
            pr_url: pr_url.into(),
            review_body: String::new(),
            verdict: Verdict::Unknown,
            grade: None,
            findings: Vec::new(),
            inline_comments: Vec::new(),
            model: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cost_estimate_usd: 0.0,
            latency_ms: 0,
            status: ReviewStatus::Completed,
            dry_run: true,
            posted: false,
            timestamp: chrono_now(),
            error: None,
            head_sha: String::new(),
            review_version: REVIEW_VERSION.to_string(),
        }
    }

    /// Copy telemetry fields from an `LlmResponse` onto this result.
    ///
    /// Why: the pipeline applies response fields after a successful LLM call.
    /// What: copies model, tokens, cost, latency, and the raw review body text.
    /// Test: covered transitively by pipeline tests.
    pub fn apply_llm_response(&mut self, resp: &crate::llm::LlmResponse) {
        self.model = resp.model.clone();
        self.input_tokens = resp.input_tokens;
        self.output_tokens = resp.output_tokens;
        self.cost_estimate_usd = resp.cost_usd;
        self.latency_ms = resp.latency_ms;
        self.review_body = resp.text.clone();
    }
}

/// Return a simple ISO-8601 UTC timestamp without depending on chrono.
///
/// Why: we want `ReviewResult` to have a timestamp but don't want to add
/// `chrono` as an unconditional dep just for Stage 1.  This helper uses the
/// system clock and formats a basic ISO-8601 string.
/// What: formats as `YYYY-MM-DDTHH:MM:SSZ` using `std::time::SystemTime`.
/// Test: `timestamp_format_is_iso8601`.
fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Convert epoch seconds to date-time components.
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Simplified Gregorian calendar for the year 2000+ range.
    let (year, month, day) = epoch_days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn epoch_days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Reference: https://en.wikipedia.org/wiki/Julian_day (simplified)
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify serde round-trip for all five board grades including APPROVE*.
    ///
    /// Why: `APPROVE*` contains an asterisk which is unusual for JSON enum
    /// values; a regression would silently produce the wrong board grade.
    /// What: serialises each variant, asserts the exact board string, then
    /// deserialises and asserts equality.
    /// Test: this test itself; no network.
    #[test]
    fn verdict_serde_roundtrip() {
        let cases = [
            (Verdict::Approve, "\"APPROVE\""),
            (Verdict::ApproveWithReservations, "\"APPROVE*\""),
            (Verdict::RequestChanges, "\"REQUEST_CHANGES\""),
            (Verdict::Block, "\"BLOCK\""),
            (Verdict::Unknown, "\"UNKNOWN\""),
        ];
        for (v, expected_json) in cases {
            let json = serde_json::to_string(&v).unwrap();
            assert_eq!(json, expected_json, "serialise mismatch for {v:?}");
            let back: Verdict = serde_json::from_str(&json).unwrap();
            assert_eq!(back, v, "deserialise mismatch for {expected_json}");
        }
    }

    /// Verify Display prints the exact board strings.
    ///
    /// Why: the compare table and Markdown log use `verdict.to_string()`;
    /// any mismatch would show the wrong grade to users.
    /// What: asserts Display output for all five variants matches board strings.
    /// Test: this test itself.
    #[test]
    fn verdict_display() {
        assert_eq!(Verdict::Approve.to_string(), "APPROVE");
        assert_eq!(Verdict::ApproveWithReservations.to_string(), "APPROVE*");
        assert_eq!(Verdict::RequestChanges.to_string(), "REQUEST_CHANGES");
        assert_eq!(Verdict::Block.to_string(), "BLOCK");
        assert_eq!(Verdict::Unknown.to_string(), "UNKNOWN");
    }

    /// Verify UNKNOWN round-trips correctly (board-grade special case).
    ///
    /// Why: UNKNOWN is emitted when the diff is too truncated to assess; it
    /// must survive a serde round-trip so the calibration board sees the
    /// correct grade.
    /// What: serialises `Unknown`, asserts `"UNKNOWN"`, deserialises back.
    /// Test: this test itself.
    #[test]
    fn verdict_unknown_round_trip() {
        let json = serde_json::to_string(&Verdict::Unknown).unwrap();
        assert_eq!(json, "\"UNKNOWN\"");
        let back: Verdict = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Verdict::Unknown);
    }

    /// Verify the single-source-of-truth verdict ordinal is strictly monotonic.
    ///
    /// Why: #1357 collapsed two duplicate `verdict_ord` tables (grade.rs +
    /// verify.rs) into `Verdict::ordinal`.  This test pins the ordering both
    /// call sites depend on so a future reorder can't silently invert the
    /// floor/ceiling comparisons.
    /// What: asserts APPROVE < APPROVE* < REQUEST_CHANGES < BLOCK < UNKNOWN.
    /// Test: this test itself.
    #[test]
    fn verdict_ordinal_is_monotonic() {
        assert!(Verdict::Approve.ordinal() < Verdict::ApproveWithReservations.ordinal());
        assert!(Verdict::ApproveWithReservations.ordinal() < Verdict::RequestChanges.ordinal());
        assert!(Verdict::RequestChanges.ordinal() < Verdict::Block.ordinal());
        assert!(Verdict::Block.ordinal() < Verdict::Unknown.ordinal());
        // Pin the exact values the comparison logic relies on.
        assert_eq!(Verdict::Approve.ordinal(), 0);
        assert_eq!(Verdict::Block.ordinal(), 3);
    }

    #[test]
    fn effort_serde_roundtrip() {
        let json = serde_json::to_string(&Effort::Low).unwrap();
        assert_eq!(json, "\"low\"");
        let back: Effort = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Effort::Low);
    }

    #[test]
    fn effort_issue_eligibility() {
        assert!(Effort::Low.is_issue_eligible());
        assert!(Effort::Medium.is_issue_eligible());
        assert!(!Effort::High.is_issue_eligible());
    }

    #[test]
    fn finding_confidence_clamping() {
        let f_over = Finding::new("src/lib.rs", "bug", "desc", "fix", 1.5, Effort::Low);
        assert!(
            (f_over.confidence - 1.0_f32).abs() < f32::EPSILON,
            "over 1.0 should clamp to 1.0"
        );

        let f_under = Finding::new("src/lib.rs", "bug", "desc", "fix", -0.1, Effort::Low);
        assert!(
            (f_under.confidence - 0.0_f32).abs() < f32::EPSILON,
            "under 0.0 should clamp to 0.0"
        );

        let f_mid = Finding::new("src/lib.rs", "bug", "desc", "fix", 0.85, Effort::Medium);
        assert!((f_mid.confidence - 0.85_f32).abs() < f32::EPSILON);
    }

    /// `FindingCategory` round-trips via its kebab-case wire form.
    ///
    /// Why: the LLM emits and the review log persists the category as a string;
    /// the back gate (#1359) keys verdict-floor behaviour off it, so the wire
    /// shape must be stable.
    /// What: serialises both variants, asserts the exact tokens, deserialises back.
    /// Test: this test itself.
    #[test]
    fn finding_category_serde_roundtrip() {
        assert_eq!(
            serde_json::to_string(&FindingCategory::Correctness).unwrap(),
            "\"correctness\""
        );
        assert_eq!(
            serde_json::to_string(&FindingCategory::MethodConformance).unwrap(),
            "\"method-conformance\""
        );
        let back: FindingCategory = serde_json::from_str("\"method-conformance\"").unwrap();
        assert_eq!(back, FindingCategory::MethodConformance);
        assert_eq!(FindingCategory::default(), FindingCategory::Correctness);
    }

    /// A `Finding` constructed via `new` defaults to the `Correctness` category.
    ///
    /// Why: every existing call site must keep producing correctness findings
    /// (back-compat); only the back-gate parser opts into `MethodConformance`.
    /// What: builds a finding via `new`, asserts the category.
    /// Test: this test itself.
    #[test]
    fn finding_defaults_category_correctness() {
        let f = Finding::new("src/lib.rs", "bug", "desc", "fix", 0.9, Effort::Low);
        assert_eq!(f.category, FindingCategory::Correctness);
    }

    /// `with_category` overrides the default category.
    ///
    /// Why: the back-gate parser threads `MethodConformance` onto an LLM finding
    /// via this builder.
    /// What: chains `with_category`, asserts the override took effect.
    /// Test: this test itself.
    #[test]
    fn finding_with_category_sets_method_conformance() {
        let f = Finding::new("src/lib.rs", "bug", "desc", "fix", 0.9, Effort::Medium)
            .with_category(FindingCategory::MethodConformance);
        assert_eq!(f.category, FindingCategory::MethodConformance);
    }

    /// A serialised finding WITHOUT a `category` field still deserialises (the
    /// `#[serde(default)]` back-compat guarantee for pre-#1359 fixtures).
    ///
    /// Why: AC requires existing finding fixtures (no `category` key) to keep
    /// deserialising, defaulting to `Correctness`.
    /// What: deserialises a category-less JSON object, asserts the default.
    /// Test: this test itself.
    #[test]
    fn finding_without_category_field_defaults_correctness() {
        let json = r#"{
            "file": "src/main.rs",
            "kind": "logic-error",
            "description": "off-by-one",
            "suggestion": "use <=",
            "confidence": 0.7,
            "effort": "medium"
        }"#;
        let f: Finding = serde_json::from_str(json).expect("legacy finding must deserialise");
        assert_eq!(f.category, FindingCategory::Correctness);
        assert_eq!(f.kind, "logic-error");
    }

    /// `InlineCommentOut` survives a serde round-trip (#1414).
    ///
    /// Why: the dry-run / MCP response serialises `ReviewResult.inline_comments`;
    /// the projection must round-trip so callers can preview would-be comments.
    /// What: serialises an `InlineCommentOut`, deserialises it, asserts equality.
    /// Test: this test itself.
    #[test]
    fn inline_comment_out_serde_roundtrip() {
        let c = InlineCommentOut {
            path: "src/db.rs".to_string(),
            line: 42,
            body: "**security** — SQL injection".to_string(),
        };
        let json = serde_json::to_string(&c).expect("serialise");
        let back: InlineCommentOut = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, c);
    }

    /// A pre-#1414 `ReviewResult` JSON (no `inline_comments`) still deserialises.
    ///
    /// Why: `inline_comments` is `#[serde(default)]` so older review logs keep
    /// loading with an empty inline-comment list.
    /// What: builds a result, serialises with no comments (skipped when empty),
    /// re-parses, asserts the field defaults to empty.
    /// Test: this test itself.
    #[test]
    fn review_result_without_inline_comments_defaults_empty() {
        let result = ReviewResult::new("o", "r", 1, "t", "u");
        let json = serde_json::to_string(&result).expect("serialise");
        assert!(
            !json.contains("inline_comments"),
            "empty inline_comments is skipped in serialisation"
        );
        let back: ReviewResult = serde_json::from_str(&json).expect("deserialise");
        assert!(back.inline_comments.is_empty());
    }

    #[test]
    fn review_result_serde_roundtrip() {
        let mut result = ReviewResult::new(
            "acme",
            "backend",
            42,
            "Add feature X",
            "https://github.com/acme/backend/pull/42",
        );
        result.verdict = Verdict::RequestChanges;
        result.review_version = "tr-0.1".to_string();
        result.findings.push(Finding::new(
            "src/main.rs",
            "security",
            "SQL injection risk",
            "Use parameterised query",
            0.92,
            Effort::Medium,
        ));

        let json = serde_json::to_string(&result).expect("serialise");
        let back: ReviewResult = serde_json::from_str(&json).expect("deserialise");

        assert_eq!(back.owner, "acme");
        assert_eq!(back.repo, "backend");
        assert_eq!(back.pr_number, 42);
        assert_eq!(back.verdict, Verdict::RequestChanges);
        assert_eq!(back.review_version, "tr-0.1");
        assert_eq!(back.findings.len(), 1);
        assert_eq!(back.findings[0].kind, "security");
        assert!((back.findings[0].confidence - 0.92_f32).abs() < f32::EPSILON);
        assert!(back.dry_run, "dry_run defaults to true");
        assert!(!back.posted, "posted defaults to false");
    }

    #[test]
    fn timestamp_format_is_iso8601() {
        let ts = chrono_now();
        // Basic format check: YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20, "timestamp should be 20 chars: {ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert_eq!(&ts[19..20], "Z");
    }

    #[test]
    fn verify_outcome_serde() {
        let confirmed = VerifyOutcome::Confirmed;
        let json = serde_json::to_string(&confirmed).unwrap();
        assert_eq!(json, "\"confirmed\"");

        let error_refuted = VerifyOutcome::ErrorRefuted {
            error_class: "ModelNotFound".to_string(),
        };
        let json = serde_json::to_string(&error_refuted).unwrap();
        let back: VerifyOutcome = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, VerifyOutcome::ErrorRefuted { error_class } if error_class == "ModelNotFound")
        );
    }
}
