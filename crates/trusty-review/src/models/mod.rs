//! Core data models for trusty-review.
//!
//! Why: defines the shared data shapes used by all pipeline stages and
//! stored in the review log.  Keeping them in a dedicated module ensures
//! a single authoritative definition and prevents type drift between the
//! pipeline, the LLM provider layer, and the store.
//! What: exposes `Verdict`, `Effort`, `VerifyOutcome`, `Finding`
//! (FixSuggestion), and `ReviewResult` — all serde-serialisable.
//! Test: `verdict_serde_roundtrip`, `review_result_serde_roundtrip`,
//! `finding_confidence_clamping`, and `finding_source_citation_roundtrip`
//! in this module.

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
/// failures are distinguishable in logs.  As of #1876 it also covers transient
/// verifier errors (rate limit, transport, upstream 5xx) — NOT just
/// config/lifecycle failures — because both are "unable to verify", a
/// structurally different outcome from a clean model REFUTED (see
/// `pipeline::verify::verify_one`).  Only config/lifecycle errors also emit the
/// `verification_model_error` alarm signal; transient errors are expected
/// operational noise and do not alarm.
/// Test: `verify_outcome_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyOutcome {
    /// Verifier LLM confirmed the finding is real.
    Confirmed,
    /// Verifier LLM refuted the finding.
    Refuted,
    /// Verifier call failed with a config/lifecycle error (see spec REV-340) or
    /// a transient error (rate limit / transport / upstream 5xx, #1876). The
    /// finding is excluded from the verdict floor like a refutation, but
    /// `rederive_verdict` treats it as "unable to verify" rather than a clean
    /// model REFUTED — it preserves `primary_verdict` instead of collapsing the
    /// review to APPROVE. Only config/lifecycle errors also emit the
    /// `verification_model_error` alarm.
    ErrorRefuted {
        /// Human-readable error class (e.g. `"ModelNotFound"`, `"AccessDenied"`,
        /// `"Transport"`, `"RateLimited"`, `"Upstream"`).
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
/// What: a three-variant enum serialised kebab-case (`"correctness"` /
/// `"method-conformance"` / `"test-coverage"`).  It is `#[serde(default)]` (→
/// `Correctness`) wherever it appears so pre-#1359 fixtures and LLM responses that
/// omit `category` still deserialise unchanged (back-compat — every legacy finding
/// is a correctness finding).  The `TestCoverage` variant was added in #1418.
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
    /// An unmet test-plan / acceptance-criteria item: the diff lacks evidence of
    /// test coverage for an AC item from the linked ticket or PR test-plan section
    /// (#1418).  Informational — never drives BLOCK or REQUEST_CHANGES alone.
    TestCoverage,
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
    /// Brief "what goes wrong if unaddressed" — the failure mechanism (#1416).
    ///
    /// Why: a finding that states the failure consequence (not just the issue) is
    /// far more actionable — it tells the author *why it matters* and helps them
    /// prioritise.  Kept as its own field so the renderer can place it distinctly
    /// (a `_Why it matters:_` line) rather than burying it in the description.
    /// `#[serde(default)]` (empty string) keeps every pre-#1416 finding parsing.
    #[serde(default)]
    pub consequence: String,
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
    /// Exact spec/ticket/test-plan source grounding this finding (#1419).
    ///
    /// Why: citing the precise source (e.g. `"IMPL-2026-05-009 WP-9"`,
    /// `"PRD §4.2"`) makes a finding authoritative — it demonstrates a
    /// prescribed intent was violated, not just an LLM inference.  The LLM
    /// populates this when the context snippet it used carries a source label
    /// (see `prompt_templates.rs`).  `None` when the finding rests on general
    /// code reasoning rather than a cited spec/ticket snippet.
    /// `#[serde(default, skip_serializing_if)]` keeps every pre-#1419 finding
    /// deserialising unchanged (backward-compatible `None` default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_citation: Option<String>,
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
            consequence: String::new(),
            suggestion: suggestion.into(),
            suggested_replacement: None,
            confidence: confidence.clamp(0.0, 1.0),
            effort,
            category: FindingCategory::Correctness,
            source_citation: None,
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
    /// Authoritative integer count of `findings`, mirroring `findings.len()`
    /// (#1877).
    ///
    /// Why: a downstream shadow-eval consumer reported implausible counts like
    /// 5120/4794 for a "findings count" on ordinary PRs — a markdown-string
    /// character count masquerading as a findings count, most likely because the
    /// consumer computed `len()` over a serialised text blob (e.g. `review_body`
    /// or the full MCP `content[0].text` envelope) instead of parsing the
    /// structured `findings` array.  Exposing the count as its own typed integer
    /// field removes any need for a consumer to derive it, so this specific class
    /// of bug (deriving a count from the wrong field/type) cannot recur.
    /// `#[serde(default)]` keeps every pre-#1877 serialised result (e.g.
    /// persisted `DedupStore` records) deserialising with `0` rather than
    /// failing.
    /// What: kept in sync with `findings.len()` at the two canonical pipeline
    /// exit points — `abort_dry` (early-abort paths) and `finalize_review`
    /// (completed paths, unified + map-reduce) — so every `ReviewResult` a
    /// caller observes carries the correct count regardless of exit path.
    /// Test: `review_result_serde_roundtrip`, `findings_count_matches_len_on_abort`,
    /// `findings_count_matches_len_on_completed_review` (runner_tests.rs).
    #[serde(default)]
    pub findings_count: usize,
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
    /// Indices (into `findings`) of the findings that were placed inline (#1414).
    ///
    /// Why: the summary body must render exactly the findings that did NOT land
    /// inline.  Deriving that set by matching `(file, line)` against
    /// `inline_comments` is lossy — two distinct findings at the same anchor
    /// collide and one is silently omitted from *both* the inline set and the
    /// summary (data loss).  Carrying the authoritative inline index set from the
    /// `InlinePlan` lets the renderer partition by finding identity, so no finding
    /// is ever dropped.  `#[serde(default)]` + skip-if-empty keeps pre-existing
    /// serialised results deserialising unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_finding_indices: Vec<usize>,
    /// Count of low-severity nits suppressed past the inline cap (#1420).
    ///
    /// Why: the nit-volume cap rolls overflow nits into one summary line rather
    /// than spamming inline comments; the count must reach the body renderer (and
    /// the dry-run / MCP response) so the suppression is acknowledged, not silent.
    /// `#[serde(default)]` + skip-if-zero keeps pre-#1420 results unchanged.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub suppressed_nits: usize,
    /// True when a zero-findings APPROVE was flagged as a suspiciously
    /// "shallow" clean review (#1877): a large diff reviewed with far fewer
    /// output tokens than its size would plausibly require for a substantive
    /// pass.
    ///
    /// Why: `pricerator#637` returned A+/0-findings in 2.6s for $0.011 on a
    /// 300+-line PR — a real (non-error, non-dedup-short-circuit) LLM call that
    /// nonetheless looks too cheap/fast to have substantively reviewed the diff.
    /// Silently crowning that A+ masks the signal from downstream consumers
    /// (shadow-eval, dashboards, humans) who would otherwise treat it as a
    /// confidently-reviewed clean pass. Flagging it — and capping the grade at
    /// `B-` (see `letter_grade::cap_shallow_review_grade`) — makes the
    /// uncertainty visible without touching the genuinely fail-CLOSED paths
    /// (LLM/transport error, truncated output, oversized diff — all correctly
    /// UNKNOWN already).
    /// What: set via `letter_grade::is_shallow_clean_review` in the unified
    /// runner path, using `verdict`, `findings.is_empty()`, the rendered diff
    /// length, and `output_tokens`. `#[serde(default)]` keeps pre-#1877 results
    /// deserialising to `false`. NOT yet wired into the map-reduce path — see
    /// the NOTE in `runner_mapreduce::fold_reduced_into_result` — because that
    /// path does not currently aggregate per-chunk token telemetry onto the
    /// result, so this always stays `false` for map-reduce reviews today.
    /// Test: `shallow_clean_review_flags_large_diff_low_tokens`,
    /// `shallow_clean_review_false_for_small_diff` (letter_grade.rs),
    /// `run_review_flags_shallow_clean_review_on_large_diff` (runner_tests.rs).
    #[serde(default, skip_serializing_if = "is_false")]
    pub shallow_clean_review: bool,

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
            findings_count: 0,
            inline_comments: Vec::new(),
            inline_finding_indices: Vec::new(),
            suppressed_nits: 0,
            shallow_clean_review: false,
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

/// Serde skip-predicate: true when a `usize` is zero (#1420).
///
/// Why: `#[serde(skip_serializing_if)]` needs a path to a predicate; this keeps
/// `suppressed_nits: 0` out of serialised results so pre-#1420 logs are unchanged.
/// What: returns `*n == 0`.
/// Test: covered transitively by `review_result_serde_roundtrip`.
fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

/// Serde skip-predicate: true when a `bool` is `false` (#1877).
///
/// Why: `#[serde(skip_serializing_if)]` needs a path to a predicate; this keeps
/// `shallow_clean_review: false` out of serialised results (the common case) so
/// existing consumers/logs are unaffected unless the flag actually fires.
/// What: returns `!*b`.
/// Test: covered transitively by `review_result_serde_roundtrip`.
fn is_false(b: &bool) -> bool {
    !*b
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
// Tests extracted to tests.rs to keep this file under the 500-line cap (#1877).

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
