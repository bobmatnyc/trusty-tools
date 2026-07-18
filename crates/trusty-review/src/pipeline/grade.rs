//! Severity-anchored, deterministic grade derivation.
//!
//! Why: the calibration run against the duetto code-review board (30 PRs)
//! revealed two systemic problems:
//!   - BLOCK was never emitted (0% detection): the model soft-pedalled critical
//!     issues to APPROVE* instead of escalating to BLOCK.
//!   - REQUEST_CHANGES leaked to APPROVE* 64% of the time: High findings were
//!     under-graded.
//!
//! The fix has these deterministic rules applied in `derive_verdict`:
//!
//! 0. SUBSTANTIVE-FINDING FILTER (#1343, applied first): findings that are
//!    verifier-refuted (`Refuted` / `ErrorRefuted` / `TruncationRefuted`) are always
//!    excluded from ALL floor logic.  Otherwise a finding is excluded when it carries
//!    `confidence < 0.50` (`FLOOR_COUNT_MIN_CONFIDENCE`) UNLESS it is `Effort::High`
//!    — a non-refuted High-effort (critical/high severity) finding is retained even
//!    at low confidence so it still drives the BLOCK floor (0.3.12 safety-net fix,
//!    PR #1350).  A `confidence:0.1` Medium or a refuted finding is noise, never
//!    evidence, and must never harden the verdict; an uncertain *critical* finding
//!    keeps its place at the floor.
//!
//! 1. LOW-CONFIDENCE OVERRIDE: if ALL substantive findings have confidence
//!    ≤ 0.65 AND none are `High`-effort, force APPROVE — overriding even a
//!    model-proposed APPROVE* downward.  Prevents APPROVE* over-fire on
//!    clean PRs with speculative low-confidence findings.
//!
//! 2. SEVERITY FLOOR: take the stricter of (model-proposed, severity-derived).
//!    As of #1015, Medium findings only count when `confidence > 0.80`
//!    (`FLOOR_MIN_CONFIDENCE`); advisory-tier Medium findings (0.66–0.80)
//!    must not force REQUEST_CHANGES on PRs the model judged clean.  As of
//!    #1876, a SINGLE confident Medium finding is enough to floor to
//!    REQUEST_CHANGES (previously this required ≥2) — see item 3 below for why
//!    the count threshold was dropped rather than merely lowered.
//!
//! 3. SOURCE-OF-TRUTH RECONCILIATION (#1343 → removed by #1876 → REINTRODUCED,
//!    NARROWED, by #1897): when the model's own verdict is a clean `APPROVE`, a
//!    *count-based* REQUEST_CHANGES floor used to be capped at APPROVE* — the
//!    theory being that "≥2 weakly grounded Mediums" was a heuristic weaker than
//!    the model's own holistic judgment.  A recalibration shadow-eval (#1876,
//!    n=473) found this made the reviewer dangerously lenient: verdict agreement
//!    was only 61.3%, and 89% of reference-reviewer REQUEST_CHANGES cases were
//!    silently downgraded to APPROVE.  #1876 therefore dropped the count
//!    requirement entirely — ONE finding that clears `FLOOR_MIN_CONFIDENCE`
//!    (0.80) floored to REQUEST_CHANGES unconditionally, with no cap.  A
//!    FOLLOW-UP shadow-eval (#1897, 26 paired PRs) found THIS overcorrected the
//!    other way: 47% of reference-APPROVE PRs were newly escalated to
//!    REQUEST_CHANGES/BLOCK, driven largely by a single Medium finding just
//!    above `FLOOR_MIN_CONFIDENCE` (e.g. 0.81) from a differently-calibrated
//!    reviewer model, on diffs the reference reviewer read as clean.  The fix
//!    is a NARROWED cap — NOT a revert of #1876: a REQUEST_CHANGES floor is
//!    capped back to APPROVE* only when it rests SOLELY on one Medium finding
//!    in the marginal confidence band (`FLOOR_MIN_CONFIDENCE` < c <
//!    `SOLO_MEDIUM_ESCALATION_CONFIDENCE`, i.e. 0.80–0.90), with no
//!    High-effort finding present (disqualified or not) and no confident
//!    conformance divergence independently justifying escalation.  ≥2
//!    confident Mediums, a solo Medium clearing the higher solo-escalation bar
//!    (`SOLO_MEDIUM_ESCALATION_CONFIDENCE`, 0.90), any High-effort finding, and
//!    a confident method-conformance divergence (`conformance_floor`) all keep
//!    the #1876 RC-recall win and are NEVER capped back down merely because the
//!    model's own verdict was a clean APPROVE — see `derive_verdict_with`'s
//!    reconciliation-cap block and [`FloorResult`] for the mechanics.
//!
//!   | Finding set                                          | Minimum floor   |
//!   |------------------------------------------------------|-----------------|
//!   | `High` effort AND escalation-eligible (#PR84)        | BLOCK           |
//!   | ≥1 `Medium` effort with confidence > 0.80, OR a       | REQUEST_CHANGES |
//!   |   citability-demoted `High` (confidence > 0.80)      |                 |
//!   | Only `Low` effort or no floor-counting findings      | APPROVE         |
//!
//!   #1897 EXCEPTION to the REQUEST_CHANGES row above: when the model's own
//!   verdict is a clean APPROVE and the REQUEST_CHANGES floor rests SOLELY on
//!   ONE genuine `Medium` finding in the marginal confidence band
//!   (`FLOOR_MIN_CONFIDENCE` < c < `SOLO_MEDIUM_ESCALATION_CONFIDENCE`, i.e.
//!   0.80–0.90) — with no `High`-effort finding present at all (disqualified or
//!   not) and no confident conformance divergence — the result is capped to
//!   APPROVE* instead. A ≥2-Medium floor, a solo Medium clearing the 0.90 bar, a
//!   High-effort finding, or a confident conformance finding are all NOT
//!   affected by this exception and still floor to REQUEST_CHANGES exactly as
//!   #1876 intended.
//!
//!   The model can never soften an escalation-eligible Critical/High or a
//!   confident-Medium finding below the floor. #PR84 (citability gate): a `High`
//!   finding drives the BLOCK floor ONLY when it is escalation-eligible — backed
//!   by a `source_citation` OR flagged `code_provable` (a core
//!   algorithmic-correctness bug provable from the diff). A non-citable, non-diff-
//!   provable `High` finding (external framework/library/platform speculation) is
//!   demoted to the advisory Medium tier and can NEVER force BLOCK — see
//!   `is_escalation_eligible` / `drives_block_floor`.
//!
//! ### Every BLOCK-emitting site is gated (adversarial-review follow-up)
//!
//! An initial version of the #PR84 fix gated ONLY `correctness_floor` (the
//! severity floor), which left THREE other paths able to reproduce the exact
//! PR #84 mis-grade unchanged:
//!
//!   1. **Model-proposed / grade-implied BLOCK** — `stricter_of(model_proposed,
//!      floor)` can only RAISE the verdict toward the floor; it could never LOWER
//!      a model self-reported `verdict:"BLOCK"` (or grade `"F"`, via
//!      `derive_verdict_with_grade`'s `effective_model`) that itself rested on a
//!      disqualified High.  **Fixed** in `derive_verdict_with` (this function) —
//!      see the `has_disqualified_high` sanitization below.  Because this is the
//!      single choke point every other verdict-deriving call site funnels
//!      through (`derive_verdict_with_grade`, `mapreduce::reduce`'s worst-chunk
//!      seed, `verify::rederive_verdict`'s baseline), the fix propagates to all
//!      of them automatically.
//!   2. **Map-reduce synthesis floor** (`mapreduce::synthesis::apply_synthesis_floor`)
//!      — a hand-rolled floor for large diffs that does NOT call `derive_verdict`
//!      at all; it tested bare `f.effort == Effort::High`.  **Fixed** — now uses
//!      `drives_block_floor` (Tier 1) with a `correctness_floor`-equivalent
//!      confidence-gated demotion (Tier 1.5) for a disqualified High.
//!   3. **Verification re-derivation** (`verify::rederive_verdict`'s
//!      `any_confirmed_high` baseline selector) — bare `f.effort == Effort::High`
//!      picked path (a) (preserve `primary_verdict` as a hard floor) regardless
//!      of citability.  **Fixed** — now requires `drives_block_floor`; a
//!      confirmed-but-disqualified High falls through to path (a2) instead,
//!      which still independently re-derives via `derive_verdict` (so it is NOT
//!      silently dropped, just no longer treated as an unconditional floor).
//!
//! ### Second-round adversarial-review fixes (grade consistency + downgrade scope)
//!
//! A follow-up review of the four fixes above found two more issues, both fixed:
//!
//!   - **MEDIUM — grade/verdict disagreement after a RULE 2 downgrade.** When
//!     RULE 2 downgrades a self-reported BLOCK, the model's original `grade`
//!     (e.g. `"F"`) was left untouched by `letter_grade::clamp_grade_to_verdict`,
//!     which only clamps a grade that is too OPTIMISTIC for the actual
//!     verdict — a grade too SEVERE (like `F` next to a downgraded
//!     REQUEST_CHANGES) passed through unchanged, rendering a contradictory
//!     "Grade: F — Request Changes" heading.  **Fixed**: `derive_verdict_with_grade`
//!     now calls `letter_grade::reconcile_grade_with_verdict`, which handles
//!     BOTH directions.
//!   - **LOW — downgrade scope was broader than necessary.** The RULE 2
//!     sanitize originally fired on `has_disqualified_high && !has_high` alone,
//!     which could strip an otherwise-valid, Medium-grounded self-reported
//!     BLOCK whenever an UNRELATED disqualified High happened to also be
//!     present.  **Fixed**: the sanitize now additionally requires
//!     `severity_floor` over the substantive set with disqualified Highs
//!     excluded to be `Approve` (no OTHER independent grounds) before
//!     downgrading — see `no_independent_grounds` below. Tested by
//!     `pr84_disqualified_high_does_not_strip_block_grounded_by_independent_medium`
//!     and `pr84_disqualified_high_with_only_low_effort_companion_still_downgrades`
//!     (grade_tests.rs).
//!
//! `Verdict::Unknown` is always preserved (pass-through) — the model has
//! signalled the diff was unassessable and no rule applies.
//!
//! ## Grade integration (#732)
//!
//! `derive_verdict_with_grade` is the new entry point for the full pipeline.
//! It accepts the LLM's model-proposed verdict AND the grade, then:
//!
//!   1. Derives the grade-implied verdict via `letter_grade::verdict_for_grade`.
//!   2. Takes the stricter of (grade-implied, model-proposed) as the new "model input".
//!   3. Applies the existing severity floor via `derive_verdict`.
//!
//! Precedence: final_verdict = severity_floor(max(grade_verdict, model_verdict))
//! This ensures the final verdict is NEVER weaker than either the grade or the
//! severity floor independently demands.
//!
//! What: exposes `derive_verdict` (unchanged; used by verification re-derivation)
//! and `derive_verdict_with_grade` (new entry point for the runner).
//! The `Effort` enum is the existing in-model severity proxy:
//!
//! - `Effort::High`   → Critical or High severity finding
//! - `Effort::Medium` → Medium severity finding
//! - `Effort::Low`    → Low severity finding
//!
//! Test: `grade_critical_high_effort_yields_block`,
//! `grade_two_medium_yields_request_changes`,
//! `grade_model_approve_solo_high_confidence_medium_still_escalates` (#1897,
//! supersedes the pre-#1897 `grade_one_medium_yields_request_changes`
//! expectation at marginal (0.80–0.90) confidence — #1876, in turn, superseded
//! the pre-#1876 `..._yields_approve_star` expectation),
//! `grade_model_approve_single_marginal_medium_caps_at_approve_star` (#1897),
//! `grade_model_approve_solo_medium_at_bar_boundary_escalates` (#1897),
//! `grade_only_low_yields_approve`,
//! `grade_unknown_is_preserved`,
//! `grade_floor_overrides_model_approve`,
//! `grade_model_block_kept_when_no_critical_finding`,
//! `grade_low_confidence_all_medium_yields_approve`,
//! `grade_high_confidence_medium_beats_low_confidence_check`,
//! `grade_advisory_medium_below_floor_threshold_does_not_escalate`,
//! `grade_high_confidence_medium_above_floor_threshold_escalates`,
//! `derive_verdict_with_grade_grade_a_no_findings_approve`,
//! `derive_verdict_with_grade_grade_f_no_findings_block`,
//! `derive_verdict_with_grade_severity_overrides_grade_a`,
//! `floor_excludes_refuted_and_low_confidence_findings` (#1343),
//! `approve_b_plus_survives_refuted_and_low_confidence_findings` (#1343),
//! `grade_model_approve_confident_medium_still_escalates_to_request_changes` and
//! `grade_model_approve_b_plus_confident_medium_escalates_to_request_changes`
//! (#1876 — supersede the pre-#1876
//! `..._caps_medium_count_floor_at_approve_star` / `..._two_high_conf_medium_
//! caps_at_approve_star` expectations),
//! `model_request_changes_review_body_still_surfaces_request_changes` (#1343),
//! `high_effort_finding_still_overrides_approve` (#1343),
//! `low_confidence_high_effort_finding_still_drives_floor` (PR #1350),
//! `refuted_high_effort_finding_is_still_excluded` (PR #1350),
//! `grade_model_approve_with_reservations_marginal_medium_not_capped` (#1897 —
//! the cap protects only a CLEAN model APPROVE, not APPROVE*),
//! `grade_model_approve_marginal_medium_with_high_effort_still_blocks` (#1897),
//! `grade_model_approve_marginal_medium_with_confident_conformance_not_capped`
//! (#1897), `pr84_confident_uncited_high_caps_at_request_changes` (#1897 —
//! confirms a disqualified High disqualifies the cap),
//! `derive_verdict_with_grade_marginal_medium_caps_grade_to_c_plus` (#1897),
//! `injected_solo_medium_escalation_changes_cap_eligibility` (#1597 + #1897).

use std::sync::LazyLock;

use regex::Regex;
use tracing::debug;

use crate::models::{Effort, Finding, FindingCategory, Verdict, VerifyOutcome};
use crate::pipeline::letter_grade::{Grade, reconcile_grade_with_verdict, verdict_for_grade};

/// Citation-grammar pattern a `source_citation` must match to be treated as
/// escalation-eligible (RULE 1 hardening, #PR84 adversarial-review follow-up).
///
/// Why: the original #PR84 fix accepted ANY non-blank `source_citation` string,
/// so a model could re-open the BLOCK floor with a junk/vague string (e.g. "see
/// the docs", "trust me") that carries no verifiable grounding. Requiring the
/// citation to match one of the four grammar shapes the prompt actually teaches
/// (`code:`, `jira:`, `apex:`, `gh:` — see the inline citation grammar in the
/// system prompt) closes that reopening. This is a SHAPE check only — it does
/// NOT cross-reference the cited identifier against the actual diff/context
/// (that would require plumbing the diff/context into `derive_verdict`, a much
/// larger change); flagged as a residual gap for follow-up validation.
/// What: matches any of — a `path:line` reference (e.g. `src/handler.ts:42`), a
/// ticket key (e.g. `IMPL-2026-05-009`, `TICKET-123`), a bare GitHub reference
/// (`#123`), or a spec-section mark (e.g. `PRD § 4.2`).
/// Test: `pr84_junk_citation_does_not_reopen_block`,
/// `high_finding_with_source_citation_still_blocks`.
static CITATION_GRAMMAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\w./-]+\.[A-Za-z0-9]+:\d+|\b[A-Z][A-Z0-9]+-\d+\b|#\d+|§\s*\d")
        .expect("citation grammar regex is a valid literal")
});

// ─── Confidence thresholds ────────────────────────────────────────────────────

/// Confidence threshold below which a finding is considered advisory-only.
///
/// Why: the model sometimes emits speculative Medium-severity findings with very
/// low confidence (e.g. 0.5).  If ALL findings fall below this threshold and
/// none are High-effort, the floor collapses from APPROVE* to APPROVE so we
/// don't over-fire on clean PRs.
/// What: any finding with `confidence > LOW_CONFIDENCE_THRESHOLD` is treated as
/// substantive; those at or below are advisory.
/// Test: `grade_low_confidence_all_medium_yields_approve`.
const LOW_CONFIDENCE_THRESHOLD: f32 = 0.65;

/// Minimum confidence for a Medium-effort finding to count toward the severity
/// floor (closes #1015).
///
/// Why: advisory-tier Medium findings (confidence 0.66–0.80) are often
/// speculative; letting two of them force REQUEST_CHANGES over-escalates clean
/// PRs that the model holistically judged APPROVE/B+.  Raising the floor-count
/// gate ensures only well-grounded Medium findings drive the REQUEST_CHANGES
/// floor, while the LOW_CONFIDENCE_THRESHOLD override still collapses the
/// entire batch when ALL findings are at or below 0.65.
/// What: a Medium finding counts toward the REQUEST_CHANGES floor ONLY when
/// its `confidence > FLOOR_MIN_CONFIDENCE`.  High-effort findings are
/// unaffected — a confirmed Critical/High still → BLOCK regardless of
/// confidence.
/// Test: `grade_advisory_medium_below_floor_threshold_does_not_escalate`,
/// `grade_high_confidence_medium_above_floor_threshold_escalates`.
const FLOOR_MIN_CONFIDENCE: f32 = 0.80;

/// Confidence bar at which a SINGLE Medium-effort finding is, on its own,
/// strong enough evidence to escalate a clean model APPROVE straight to
/// REQUEST_CHANGES (closes #1897 Rank-1 fix).
///
/// Why: the #1897 shadow-eval (26 paired PRs) found #1876's removal of the
/// #1343 reconciliation cap overcorrected — 47% of Bedrock-APPROVE PRs were
/// newly escalated to REQUEST_CHANGES/BLOCK by the daemon, driven largely by a
/// SINGLE Medium finding just above `FLOOR_MIN_CONFIDENCE` (e.g. 0.81) from a
/// differently-calibrated reviewer model, on a diff the reference reviewer
/// read as clean.  A finding that clears this higher "solo-escalation" bar
/// is well-evidenced enough to stand alone against a clean model APPROVE; one
/// that only clears `FLOOR_MIN_CONFIDENCE` is in the marginal band and is
/// instead subject to the narrowed reconciliation cap in `derive_verdict_with`
/// (see the module doc's "#1897 reconciliation cap" section) UNLESS
/// corroborated by a second confident Medium, a High-effort finding, or a
/// confident conformance divergence.
/// What: within `correctness_floor`, a single confident (`confidence >
/// FLOOR_MIN_CONFIDENCE`) `Effort::Medium` finding whose confidence ALSO
/// clears `>= SOLO_MEDIUM_ESCALATION_CONFIDENCE` escalates unconditionally
/// (not cap-eligible), matching the treatment ≥2 confident Mediums already
/// receive.  Only genuine `Effort::Medium` findings are considered here — a
/// citability-demoted High (`is_disqualified_high`) is still High-severity
/// evidence and is excluded from the cap by the "no High-effort" condition
/// regardless of this bar (see `correctness_floor`).
/// Test: `grade_model_approve_solo_high_confidence_medium_still_escalates`,
/// `grade_model_approve_single_marginal_medium_caps_at_approve_star`,
/// `grade_model_approve_solo_medium_at_bar_boundary_escalates`.
const SOLO_MEDIUM_ESCALATION_CONFIDENCE: f32 = 0.90;

/// Confidence floor below which a finding is excluded from verdict hardening
/// entirely (closes #1343).
///
/// Why: the calibration bug in #1343 showed the structured verdict drifting to
/// REQUEST_CHANGES/D+ while the model's own `review_body` said APPROVE/B+, partly
/// driven by speculative findings carrying `confidence: 0.1` (and verifier-refuted
/// findings, which are demoted to 0.10 by `VERIFY_REFUTED_CONFIDENCE`).  Such
/// findings must never count toward any floor — they are noise, not evidence.
/// The 0.50 value is the coin-flip line: below 0.50 a finding is more likely
/// wrong than right, so it must not move the verdict floor.  (Contrast with
/// `LOW_CONFIDENCE_THRESHOLD` = 0.65, the advisory-batch collapse line, and
/// `FLOOR_MIN_CONFIDENCE` = 0.80, the Medium-counts-toward-the-floor line.)
/// What: any finding with `confidence < FLOOR_COUNT_MIN_CONFIDENCE` is dropped
/// from the severity-floor input set (alongside any verifier-refuted finding) —
/// with ONE exception (0.3.12, PR #1350): a non-refuted `Effort::High` finding is
/// retained even below 0.50, so an uncertain-but-critical concern keeps its safety
/// net (see `is_substantive`).
/// Test: `floor_excludes_refuted_and_low_confidence_findings`,
/// `approve_b_plus_survives_refuted_and_low_confidence_findings`,
/// `low_confidence_high_effort_finding_still_drives_floor`.
const FLOOR_COUNT_MIN_CONFIDENCE: f32 = 0.50;

// ─── Calibration env-var overrides (#1597) ────────────────────────────────────

/// Environment variable for overriding [`LOW_CONFIDENCE_THRESHOLD`] at runtime.
///
/// Why: per-deployment tuning of grading strictness without recompiling.  Parsed
/// as `f32`; invalid or out-of-`[0.0, 1.0]` values silently fall back to the
/// compile-time constant (closes #1597).
/// What: when set to a valid `f32` in `[0.0, 1.0]`, overrides the advisory-batch
/// collapse threshold.  Default value when unset: `0.65`.
/// Test: `injected_low_confidence_threshold_changes_verdict`,
/// `thresholds_from_lookup_reads_each_env_key`.
pub const TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD_ENV: &str =
    "TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD";

/// Environment variable for overriding [`FLOOR_MIN_CONFIDENCE`] at runtime.
///
/// Why: per-deployment control over how strictly Medium findings must score to
/// count toward the REQUEST_CHANGES floor (closes #1597).
/// What: when set to a valid `f32` in `[0.0, 1.0]`, overrides the Medium-floor
/// gate.  Default value when unset: `0.80`.
/// Test: `injected_floor_min_confidence_changes_verdict`,
/// `thresholds_from_lookup_reads_each_env_key`.
pub const TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE_ENV: &str = "TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE";

/// Environment variable for overriding [`SOLO_MEDIUM_ESCALATION_CONFIDENCE`] at
/// runtime (closes #1897).
///
/// Why: per-deployment tuning of the solo-Medium escalation bar without
/// recompiling — mirrors the #1597 pattern for the other calibration
/// thresholds.
/// What: when set to a valid `f32` in `[0.0, 1.0]`, overrides the bar at which
/// a single confident Medium finding escalates uncapped. Default value when
/// unset: `0.90`.
/// Test: `injected_solo_medium_escalation_changes_cap_eligibility`,
/// `thresholds_from_lookup_reads_each_env_key`.
pub const TRUSTY_REVIEW_SOLO_MEDIUM_ESCALATION_CONFIDENCE_ENV: &str =
    "TRUSTY_REVIEW_SOLO_MEDIUM_ESCALATION_CONFIDENCE";

/// Environment variable for overriding [`FLOOR_COUNT_MIN_CONFIDENCE`] at runtime.
///
/// Why: per-deployment control over the sub-coin-flip exclusion floor (closes #1597).
/// What: when set to a valid `f32` in `[0.0, 1.0]`, overrides the minimum
/// confidence for a finding to participate in the verdict floor at all.  Default
/// value when unset: `0.50`.
/// Test: `injected_floor_count_min_confidence_changes_verdict`,
/// `thresholds_from_lookup_reads_each_env_key`.
pub const TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE_ENV: &str =
    "TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE";

/// Parse a calibration threshold from an optional raw string, else `default`.
///
/// Why: the parse-and-validate step is the only non-trivial logic in the #1597
/// env-override feature.  Keeping it a PURE function (no `std::env` access) lets
/// it be unit-tested exhaustively without mutating any process-global state —
/// the root-cause fix for the #2688 parallel-test flake, where env writes in one
/// test leaked into `derive_verdict` reads on a parallel test thread.
/// What: trims `raw` and parses it as `f32`, accepting the value only when it is
/// finite and in `[0.0, 1.0]`; every other outcome (absent, non-numeric, or
/// out-of-range) returns `default` silently.
/// Test: `parse_threshold_accepts_valid`,
/// `parse_threshold_rejects_invalid_and_out_of_range`,
/// `parse_threshold_none_is_default`.
fn parse_threshold(raw: Option<&str>, default: f32) -> f32 {
    match raw {
        Some(raw) => match raw.trim().parse::<f32>() {
            Ok(v) if v.is_finite() && (0.0..=1.0).contains(&v) => v,
            _ => default,
        },
        None => default,
    }
}

/// The three calibration thresholds `derive_verdict` consults, resolved once.
///
/// Why: previously each threshold was read from `std::env` on every call
/// (`low_confidence_threshold()` and friends), so `derive_verdict` carried a
/// hidden dependency on process-global env state.  A test that overrode one of
/// the `TRUSTY_REVIEW_*` env vars therefore raced with any parallel test that
/// called `derive_verdict` — the #2688 flake.  Resolving the three values ONCE
/// into an explicit value (from env in production, injected directly in tests)
/// removes the global dependency and gives tests a clean seam to pin thresholds
/// without touching the process environment.
/// What: holds the effective `low_confidence`, `floor_min`, `solo_medium_escalation`,
/// and `floor_count_min` confidence gates; construct via [`Thresholds::from_env`]
/// (production) or, in tests, `Thresholds::defaults` (compile-time constants) /
/// a direct literal.
/// Test: `thresholds_defaults_match_constants`,
/// `thresholds_from_lookup_reads_each_env_key`.
#[derive(Debug, Clone, Copy)]
struct Thresholds {
    /// Advisory-batch collapse line (`LOW_CONFIDENCE_THRESHOLD`, default 0.65).
    low_confidence: f32,
    /// Medium-counts-toward-the-floor line (`FLOOR_MIN_CONFIDENCE`, default 0.80).
    floor_min: f32,
    /// Solo-Medium uncapped-escalation bar (`SOLO_MEDIUM_ESCALATION_CONFIDENCE`,
    /// default 0.90) — closes #1897.
    solo_medium_escalation: f32,
    /// Sub-coin-flip exclusion line (`FLOOR_COUNT_MIN_CONFIDENCE`, default 0.50).
    floor_count_min: f32,
}

impl Thresholds {
    /// The compile-time default thresholds (no env access).
    ///
    /// Why: the calibrated baseline used when no operator override is present, and
    /// the value tests inject when they want the shipped behaviour.  Only tests
    /// construct it directly (production always goes through `from_env`), so it is
    /// `#[cfg(test)]` to stay off the shipped surface and avoid a dead-code warning.
    /// What: returns the three `const` defaults verbatim.
    /// Test: `thresholds_defaults_match_constants`.
    #[cfg(test)]
    fn defaults() -> Self {
        Self {
            low_confidence: LOW_CONFIDENCE_THRESHOLD,
            floor_min: FLOOR_MIN_CONFIDENCE,
            solo_medium_escalation: SOLO_MEDIUM_ESCALATION_CONFIDENCE,
            floor_count_min: FLOOR_COUNT_MIN_CONFIDENCE,
        }
    }

    /// Resolve thresholds via an injected key→value lookup (closes #1597).
    ///
    /// Why: factoring the env-key→field wiring behind a lookup closure lets tests
    /// verify the wiring (which key maps to which field) WITHOUT mutating the
    /// process environment — the #2688 root-cause fix.  Production passes a
    /// closure that reads `std::env`; tests pass a pure in-memory map.
    /// What: for each of the three env keys, parses `lookup(key)` via
    /// [`parse_threshold`], falling back to the matching compile-time constant.
    /// Test: `thresholds_from_lookup_reads_each_env_key`.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            low_confidence: parse_threshold(
                lookup(TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD_ENV).as_deref(),
                LOW_CONFIDENCE_THRESHOLD,
            ),
            floor_min: parse_threshold(
                lookup(TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE_ENV).as_deref(),
                FLOOR_MIN_CONFIDENCE,
            ),
            solo_medium_escalation: parse_threshold(
                lookup(TRUSTY_REVIEW_SOLO_MEDIUM_ESCALATION_CONFIDENCE_ENV).as_deref(),
                SOLO_MEDIUM_ESCALATION_CONFIDENCE,
            ),
            floor_count_min: parse_threshold(
                lookup(TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE_ENV).as_deref(),
                FLOOR_COUNT_MIN_CONFIDENCE,
            ),
        }
    }

    /// Resolve thresholds from the process environment (production entry point).
    ///
    /// Why: the runtime path — reads each `TRUSTY_REVIEW_*` env var so operators
    /// can tune grading strictness without recompiling (#1597).
    /// What: delegates to [`Thresholds::from_lookup`] with a closure over
    /// `std::env::var`.
    /// Test: exercised end-to-end by every `derive_verdict` test (default env) and
    /// by `thresholds_from_lookup_reads_each_env_key` (key→field wiring).
    fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }
}

/// Resolve the current `FLOOR_MIN_CONFIDENCE` calibration threshold from the
/// process environment (adversarial-review follow-up).
///
/// Why: the map-reduce synthesis floor (`mapreduce::synthesis::apply_synthesis_floor`)
/// demotes an ungated High finding to the same confidence-gated REQUEST_CHANGES
/// tier `correctness_floor` Tier 2 gives it — both floors must apply the SAME
/// bar, including any operator override via `TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE`
/// (#1597), or the two floors could disagree at the threshold boundary.
/// What: returns `Thresholds::from_env().floor_min`.
/// Test: exercised transitively by `synthesis_high_confidence_demoted_high_floors_to_request_changes`
/// in `synthesis_tests.rs`.
pub(crate) fn floor_min_confidence() -> f32 {
    Thresholds::from_env().floor_min
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Compute the final review verdict from the model-proposed verdict and findings.
///
/// Why: the calibration run showed the model systematically under-fires
/// (BLOCK=0%, REQUEST_CHANGES=36%).  Applying a deterministic severity-derived
/// FLOOR ensures Critical/High issues are never silently softened to APPROVE*.
///
/// What: two-pass derivation:
///
/// 1. LOW-CONFIDENCE OVERRIDE (ceiling): if ALL findings have confidence ≤ 0.65
///    AND none are High-effort, the entire batch is advisory noise.  The result is
///    forced to APPROVE — overriding even a model-proposed APPROVE* downward.
///    This prevents APPROVE* over-fire on clean PRs with speculative low-confidence
///    findings.
///
/// 2. SEVERITY FLOOR (minimum): outside the override window, compute a floor from
///    the finding severity distribution (see `severity_floor`) and return
///    `max(model_proposed, floor)`.  The model can never soften a Critical/High
///    finding — nor, as of #1876, a single confident Medium finding — below the
///    floor, regardless of the model's own proposed verdict.  EXCEPTION (#1897):
///    see the reconciliation cap below.
///
/// Special case: `Verdict::Unknown` is always returned as-is — the model has
/// determined the diff was unassessable and no floor or override applies.
///
/// ## #1343 reconciliation cap — removed by #1876, REINTRODUCED NARROWED by #1897
/// A prior version of this function capped a *count-based* (≥2 Medium)
/// REQUEST_CHANGES floor at APPROVE* whenever `model_proposed == Approve`, on
/// the theory that a count heuristic was weaker evidence than the model's own
/// holistic APPROVE.  #1876's shadow-eval showed this made the reviewer
/// dangerously lenient (61.3% verdict agreement; 89% of reference
/// REQUEST_CHANGES cases silently downgraded).  `correctness_floor`'s Tier 2
/// stopped needing ≥2 findings — ONE finding that clears `FLOOR_MIN_CONFIDENCE`
/// was treated as, by construction, well-evidenced, so every REQUEST_CHANGES
/// floor became confidence-grounded rather than count-based, and the #1343 cap
/// was removed outright.
///
/// A follow-up shadow-eval (#1897, 26 paired PRs) found that removal
/// overcorrected: 47% of reference-APPROVE PRs were newly escalated to
/// REQUEST_CHANGES/BLOCK, concentrated on PRs where the ONLY floor-driving
/// evidence was a single Medium finding just above `FLOOR_MIN_CONFIDENCE`
/// (e.g. 0.81) — well short of a truly confident escalation, but enough to
/// clear the old binary 0.80 gate.  This function now re-applies a NARROWED
/// cap, in the `derive_verdict_with` implementation below, immediately after
/// computing `stricter_of(model_proposed, floor)`: when `model_proposed` is a
/// clean `Approve`, the result is `RequestChanges`, and
/// `floor_result.cap_eligible` is true (i.e. the floor rests SOLELY on one
/// genuine Medium finding in the marginal 0.80–0.90 confidence band, per
/// `correctness_floor`/[`FloorResult`], with no High-effort finding present
/// and no confident conformance divergence), the result is capped to
/// `ApproveWithReservations`.  ≥2 confident Mediums, a solo Medium ≥
/// `SOLO_MEDIUM_ESCALATION_CONFIDENCE` (0.90), any High-effort finding
/// (disqualified or not), and a confident conformance finding are all outside
/// this narrow exception and keep the #1876 RC-recall win uncapped — see
/// `severity_floor` / `correctness_floor` for the `cap_eligible` computation.
///
/// Test: see module-level test list.
pub fn derive_verdict(model_proposed: Verdict, findings: &[Finding]) -> Verdict {
    derive_verdict_with(model_proposed, findings, &Thresholds::from_env())
}

/// `derive_verdict` with the calibration thresholds supplied explicitly.
///
/// Why: this is the injection seam introduced for #2688.  The public
/// `derive_verdict` resolves thresholds from the process environment; splitting
/// the derivation logic out so it takes an explicit `&Thresholds` lets tests
/// exercise every threshold behaviour by passing a literal struct — with no
/// `std::env` mutation, so nothing can race a parallel test that reads the same
/// env keys.
/// What: applies the two-pass derivation (low-confidence override, then severity
/// floor) using `thresholds` for every confidence gate.  Behaviour is identical
/// to the pre-#2688 code when `thresholds == Thresholds::from_env()`.
/// Test: every `derive_verdict` / `injected_*` test in `grade_tests.rs`.
fn derive_verdict_with(
    model_proposed: Verdict,
    findings: &[Finding],
    thresholds: &Thresholds,
) -> Verdict {
    // UNKNOWN is a special terminal state — preserve it unconditionally.
    if model_proposed == Verdict::Unknown {
        debug!("verdict=UNKNOWN from model — preserving (diff unassessable)");
        return Verdict::Unknown;
    }

    // #1343: exclude refuted and sub-0.50-confidence findings from ALL floor
    // logic.  A verifier-refuted finding (demoted to 0.10) or a speculative
    // `confidence: 0.1` finding is noise, not evidence — it must never harden the
    // verdict above what the model holistically concluded.  This is the source-of-
    // truth reconciliation: the floor only sees substantive findings.
    let substantive: Vec<&Finding> = findings
        .iter()
        .filter(|f| is_substantive(f, thresholds))
        .collect();

    // Low-confidence override (ceiling): if ALL substantive findings are advisory-
    // only (confidence ≤ threshold) AND none are BLOCK-floor-driving, the batch is
    // noise.  Override the model down to APPROVE — this specifically prevents
    // APPROVE* over-fire (Fix 4).  Only an escalation-eligible High finding (cited
    // or diff-provable) escapes this gate: a confirmed bug with low confidence
    // should still BLOCK, not disappear.  #PR84: a non-citable High finding
    // (external framework/platform speculation) is NOT eligible, so it no longer
    // keeps the batch out of the APPROVE override — it is advisory, not a blocker.
    let has_high = substantive.iter().any(|f| drives_block_floor(f));

    // #PR84 RULE 2 (adversarial-review crux fix): a model self-reported — or
    // grade-implied, via `derive_verdict_with_grade`'s `effective_model` —
    // proposed BLOCK is ITSELF subject to the citability gate, not just the
    // floor below.  Without this, `stricter_of(model_proposed, floor)` can only
    // RAISE the verdict from the floor toward the model's proposal; it can never
    // LOWER a model-proposed BLOCK, so a model that self-reports
    // `verdict:"BLOCK"` (or `grade:"F"`) purely on the strength of an uncited,
    // non-diff-provable High finding — the exact PR #84 shape — would pass BLOCK
    // through completely ungated.  Downgrade the proposal to REQUEST_CHANGES when
    // a disqualified (high-severity, NOT escalation-eligible) finding is present,
    // no escalation-eligible High finding is ALSO present to independently
    // justify BLOCK, AND — adversarial-review LOW-severity scope fix — no OTHER
    // (non-disqualified) substantive finding independently grounds at least an
    // advisory-level concern on its own.
    //
    // The third condition matters: `has_disqualified_high && !has_high` ALONE
    // would strip an otherwise-valid self-reported BLOCK whenever a purely
    // UNRELATED disqualified High happens to co-occur with genuine, separate
    // grounds (e.g. a confident Medium finding) — an adversarial review found
    // this could over-fire.  Recomputing `severity_floor` over the substantive
    // set with disqualified Highs removed answers "is there ANY OTHER reason to
    // trust the model's escalation, independent of the disqualified finding?" —
    // if yes (the reduced floor clears APPROVE), the model's BLOCK is preserved
    // exactly as `grade_model_block_kept_when_no_critical_finding` already
    // expects; only a disqualified High with NO other grounds at all (the PR #84
    // shape) triggers the downgrade.
    //
    // Deliberately scoped to "a disqualified High finding is present" — NOT
    // merely "no eligible High finding at all" — so this does not touch:
    //   - `verify::rederive_verdict`'s infra-failure preservation (path c),
    //     which intentionally keeps `primary_verdict` on verifier infra failure
    //     (#726) independent of citability, and is invoked with an EMPTY
    //     `survivors` list when the only finding was excluded as
    //     ErrorRefuted/TruncationRefuted — there is no disqualified High for
    //     this gate to see, so BLOCK correctly survives;
    //   - a model that escalates to BLOCK past a bare Medium finding (no High
    //     at all) — a pre-#PR84 "model knows more" pattern unrelated to High-
    //     severity citability (`grade_model_block_kept_when_no_critical_finding`).
    let has_disqualified_high = substantive.iter().any(|f| is_disqualified_high(f));
    let non_disqualified_substantive: Vec<&Finding> = substantive
        .iter()
        .filter(|f| !is_disqualified_high(f))
        .copied()
        .collect();
    let no_independent_grounds =
        severity_floor(&non_disqualified_substantive, thresholds).verdict == Verdict::Approve;
    let model_proposed = if model_proposed == Verdict::Block
        && has_disqualified_high
        && !has_high
        && no_independent_grounds
    {
        debug!(
            "model-proposed BLOCK rests solely on a disqualified (uncited, \
             non-diff-provable) High finding, with no other independent grounds \
             — downgrading to REQUEST_CHANGES (#PR84 RULE 2)"
        );
        Verdict::RequestChanges
    } else {
        model_proposed
    };

    let threshold = thresholds.low_confidence;
    let all_low_confidence =
        !substantive.is_empty() && substantive.iter().all(|f| f.confidence <= threshold);

    if all_low_confidence && !has_high {
        debug!(
            model_verdict = %model_proposed,
            threshold,
            "low-confidence override: all substantive findings ≤ threshold confidence, no High-effort → APPROVE"
        );
        return Verdict::Approve;
    }

    // Severity floor: take the stricter of model-proposed and severity-derived.
    // `correctness_floor`'s Tier 2 (below) floors to REQUEST_CHANGES on a
    // SINGLE confident (`confidence > FLOOR_MIN_CONFIDENCE`) Medium finding —
    // the same hard-floor treatment already given to High-effort findings
    // (Tier 1, BLOCK) and confident method-conformance divergences
    // (`conformance_floor`, #1359).  As of #1897 the floor also tells the
    // caller whether that REQUEST_CHANGES rests SOLELY on one
    // marginal-confidence Medium (`cap_eligible`) — see the reconciliation cap
    // immediately below.
    let floor_result = severity_floor(&substantive, thresholds);
    let floor = floor_result.verdict.clone();

    let mut final_verdict = stricter_of(model_proposed.clone(), floor.clone());

    // #1897 RECONCILIATION CAP (narrowed successor to the #1343 cap #1876
    // removed): #1876 dropped the ≥2-Medium count requirement so a SINGLE
    // confident Medium finding floors to REQUEST_CHANGES unconditionally. A
    // shadow-eval (#1897, 26 paired PRs) found this overcorrected — 47% of
    // reference-APPROVE PRs were newly escalated, driven by a single Medium
    // just above FLOOR_MIN_CONFIDENCE from a differently-calibrated reviewer
    // model. Restore a narrow APPROVE veto for EXACTLY that marginal case:
    // when the model's own verdict is a clean APPROVE and the RC floor rests
    // SOLELY on one Medium finding whose confidence is in the marginal band
    // (`FLOOR_MIN_CONFIDENCE` < c < `SOLO_MEDIUM_ESCALATION_CONFIDENCE`) —
    // with no High-effort finding present (disqualified or not) and no
    // confident conformance divergence independently justifying
    // REQUEST_CHANGES — cap the result at APPROVE* instead. This is NOT the
    // #1343 cap reborn: it does not touch a ≥2-Medium floor, a single Medium
    // that clears the higher solo-escalation bar, a High-effort finding, or a
    // confident conformance finding — all of those keep the #1876 RC-recall
    // win and escalate exactly as before. `floor_result.cap_eligible` (set by
    // `severity_floor`/`correctness_floor`) is the single source of truth for
    // whether this narrow case applies.
    if model_proposed == Verdict::Approve
        && final_verdict == Verdict::RequestChanges
        && floor_result.cap_eligible
    {
        debug!(
            "#1897: REQUEST_CHANGES floor rests solely on one marginal-confidence \
             Medium finding with a clean model APPROVE and no independent High/\
             conformance grounds — capping at APPROVE* (narrowed reconciliation cap)"
        );
        final_verdict = Verdict::ApproveWithReservations;
    }

    debug!(
        model_verdict = %model_proposed,
        severity_floor = %floor,
        final_verdict = %final_verdict,
        "grade derivation: floor={floor}, model={model_proposed}, final={final_verdict}",
    );

    final_verdict
}

/// Return `true` if a finding is substantive enough to count toward the verdict
/// floor (closes #1343; High-effort safety net restored in 0.3.12 per PR #1350).
///
/// Why: refuted findings and very-low-confidence speculation must never harden
/// the verdict.  The #1343 calibration bug surfaced REQUEST_CHANGES/D+ on PRs the
/// model graded APPROVE/B+ partly because `verified:"refuted"` findings (demoted to
/// 0.10) and raw `confidence:0.1` findings were still fed into the severity floor.
/// BUT the original #1343 predicate dropped EVERY finding below 0.50 confidence —
/// including genuine High-effort (critical/high severity) findings.  That removed
/// the safety net: a real critical bug the model was merely uncertain about (e.g.
/// `confidence:0.45`, `effort:High`) would be excluded from the floor and silently
/// soften to APPROVE.  PR #1350's review flagged this; we restore the net here.
/// What: returns `false` when the finding is any refutation variant
/// (`Refuted` / `ErrorRefuted` / `TruncationRefuted`) — a verifier-refuted finding
/// is disproven evidence and is excluded REGARDLESS of effort.  Otherwise returns
/// `true` when EITHER its `confidence >= FLOOR_COUNT_MIN_CONFIDENCE` (0.50) OR it is
/// `Effort::High` — a non-refuted High-effort finding is retained even at low
/// confidence so it still drives the BLOCK floor / `has_high` path.
/// Test: `floor_excludes_refuted_and_low_confidence_findings`,
/// `low_confidence_high_effort_finding_still_drives_floor`,
/// `refuted_high_effort_finding_is_still_excluded`.
fn is_substantive(f: &Finding, thresholds: &Thresholds) -> bool {
    let refuted = matches!(
        f.verified,
        Some(VerifyOutcome::Refuted)
            | Some(VerifyOutcome::ErrorRefuted { .. })
            | Some(VerifyOutcome::TruncationRefuted)
    );
    // A refuted finding is disproven evidence — always excluded, even high-severity.
    // Otherwise retain it if it clears the confidence floor OR is a high-severity
    // (critical or high) finding: a genuine critical or high-severity concern must
    // keep its place at the verdict floor even when the model is only uncertain
    // about it.
    !refuted && (f.confidence >= thresholds.floor_count_min || is_high_severity(f))
}

/// Return `true` when a finding is critical- or high-severity (closes #1352).
///
/// Why: the verdict-floor guard must single out "a critical/high-severity finding
/// that must never be silently dropped".  Before #1352 the call sites spelled this
/// as the bare check `f.effort == Effort::High`, which made `Effort` do double duty
/// as a severity proxy and left the *intent* (severity, not remediation cost)
/// implicit.  Naming the predicate makes that intent unmistakable at every call
/// site and gives a single place to evolve the severity definition if `Finding`
/// ever grows a dedicated severity axis.
/// What: returns `f.effort == Effort::High`.  In this domain model `Effort` IS the
/// severity proxy — `Effort::High` is defined as "Critical or High severity"
/// (see the module-level mapping and `models::Effort`), `Effort::Medium` as Medium
/// severity, and `Effort::Low` as Low severity.  This is therefore behaviour-
/// equivalent to the previous inline check (verified by the #1343 calibration
/// regression tests, which are unchanged) while reading as the severity question
/// it actually asks.  Should a separate `Severity::{Critical,High}` field land
/// later, this is the one function to update.
/// Test: `is_high_severity_matches_high_effort`,
/// `low_confidence_high_effort_finding_still_drives_floor` (#1350),
/// `floor_excludes_refuted_and_low_confidence_findings` (#1343).
pub(crate) fn is_high_severity(f: &Finding) -> bool {
    f.effort == Effort::High
}

/// Return `true` when a finding clears the citability gate and is therefore
/// eligible to drive the deterministic BLOCK floor (RULE 1, #PR84 calibration).
///
/// Why: PR #84 (`duetto-eve-agents` #84) was graded BLOCK/F on a SINGLE finding
/// the model self-tagged `effort: high` — a FALSE, non-citable claim about
/// Vercel's routing (contradicted by Vercel's docs and disproven in prod).  The
/// floor trusted the model's self-reported effort unconditionally and hard-clamped
/// to BLOCK with no citability check.  The rule: a high/critical finding may
/// escalate toward BLOCK ONLY if it is either (a) backed by a non-empty
/// `source_citation` (code location / spec / doc / ticket) OR (b) a CORE
/// algorithmic-correctness issue argued from the diff itself (a logic/data/security
/// bug provable from the code under review, flagged `code_provable`).  Non-citable
/// speculation about external framework/library/platform behavior is NOT eligible;
/// it is capped at the advisory (Medium) tier and can never force BLOCK.
/// What: returns true iff `source_citation` is present, non-blank, AND matches
/// [`CITATION_GRAMMAR_RE`] (a junk/vague string no longer qualifies — hardened
/// per the adversarial-review follow-up, see the regex doc comment) OR
/// `code_provable` is set.  Deliberately narrow — this is the ONLY relaxation of
/// the BLOCK floor and it must not weaken it beyond the two #PR84 rules.
/// Test: `pr84_uncited_high_finding_does_not_block`,
/// `high_finding_with_source_citation_still_blocks`,
/// `high_finding_code_provable_still_blocks`,
/// `pr84_junk_citation_does_not_reopen_block`.
pub(crate) fn is_escalation_eligible(f: &Finding) -> bool {
    let cited = f
        .source_citation
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .is_some_and(|c| CITATION_GRAMMAR_RE.is_match(c));
    cited || f.code_provable
}

/// Return `true` when a finding drives the deterministic BLOCK floor: it is
/// high-severity AND escalation-eligible (RULE 2, #PR84 calibration).
///
/// Why: the BLOCK floor must not clamp to BLOCK on a high-effort finding that
/// fails the citability gate (uncited AND not core-algorithmic-correctness).
/// Because the floor previously trusted the model's self-reported `effort`
/// unconditionally, a prompt-only change is not self-enforcing — this predicate is
/// the deterministic backstop.  A high finding that is NOT escalation-eligible is
/// demoted to the advisory (Medium) tier by `correctness_floor` rather than
/// driving BLOCK.  Reused verbatim by the map-reduce synthesis floor
/// (`mapreduce::synthesis::apply_synthesis_floor`) and the model-proposed-BLOCK
/// sanitization below, so every BLOCK-emitting site in the pipeline applies the
/// SAME gate (adversarial-review follow-up — see the module doc's "sites gated"
/// list).
/// What: returns `is_high_severity(f) && is_escalation_eligible(f)`.
/// Test: `pr84_uncited_high_finding_does_not_block` and its citation/code-provable
/// controls.
pub(crate) fn drives_block_floor(f: &Finding) -> bool {
    is_high_severity(f) && is_escalation_eligible(f)
}

/// Return `true` when a finding is a "disqualified High" — high-severity but
/// FAILING the citability gate (RULE 1) — the finding class RULE 2 demotes to
/// the advisory tier instead of letting it drive BLOCK.
///
/// Why: this exact predicate was previously inlined at three call sites
/// (`correctness_floor`'s Tier 2, `derive_verdict_with`'s RULE 2 sanitize scope
/// check, and the map-reduce synthesis floor's Tier 1.5) — naming it once keeps
/// all three in agreement and gives a single place to audit the "disqualified"
/// definition (adversarial-review follow-up, Duplicate Elimination).
/// What: returns `is_high_severity(f) && !is_escalation_eligible(f)`.
/// Test: covered transitively by every `#PR84` test in this module and
/// `synthesis_tests.rs`.
pub(crate) fn is_disqualified_high(f: &Finding) -> bool {
    is_high_severity(f) && !is_escalation_eligible(f)
}

// ─── Floor computation ────────────────────────────────────────────────────────

/// Outcome of the deterministic severity-floor computation (closes #1897).
///
/// Why: `derive_verdict_with`'s narrowed reconciliation cap needs to
/// distinguish a well-corroborated REQUEST_CHANGES floor (≥2 confident
/// Mediums, a solo Medium clearing `SOLO_MEDIUM_ESCALATION_CONFIDENCE`, a
/// High-effort finding, or a confident conformance divergence) from one that
/// rests SOLELY on a single marginal-confidence Medium finding — only the
/// latter is eligible to be capped back to APPROVE* when the model's own
/// verdict was a clean APPROVE.  Bundling `cap_eligible` alongside the
/// `verdict` keeps that determination co-located with the tier logic that
/// produces it, rather than re-deriving it from scratch in the caller.
/// What: pairs the computed [`Verdict`] with a `cap_eligible` flag.
/// Test: exercised transitively by every reconciliation-cap test in
/// `grade_tests.rs` (see `derive_verdict_with`'s module-doc test list).
struct FloorResult {
    verdict: Verdict,
    cap_eligible: bool,
}

/// Compute the minimum (floor) verdict from the finding severity distribution.
///
/// Why: the floor is the deterministic component of grade derivation.  It is
/// applied as a lower-bound over the model's own verdict in `derive_verdict`.
/// The low-confidence override is handled separately in `derive_verdict` before
/// this function is called; by the time this is reached, the batch has at least
/// one substantive finding.
///
/// As of #1015, Medium findings only count toward the REQUEST_CHANGES floor
/// when their `confidence > FLOOR_MIN_CONFIDENCE` (0.80).  Advisory-tier Medium
/// findings (confidence 0.66–0.80) are speculative; they must not force
/// REQUEST_CHANGES over-escalation on PRs the model holistically judged clean.
/// High-effort behavior is unchanged: any confirmed High finding still floors
/// to BLOCK regardless of confidence.  As of #1876, a SINGLE confident Medium
/// finding is sufficient to floor to REQUEST_CHANGES (previously ≥2 were
/// required, with exactly 1 capped at the weaker APPROVE*) — see the module doc
/// for the shadow-eval rationale. As of #1897, whether that single-Medium floor
/// is eligible for the narrowed reconciliation cap is also computed here (see
/// [`FloorResult`]).
///
/// What: applies the three-tier rule set:
///
/// 1. Any `High`-effort finding → BLOCK (Critical/High severity)
/// 2. ≥1 `Medium`-effort finding with `confidence > 0.80` → REQUEST_CHANGES
/// 3. Only `Low` / no floor-counting findings → APPROVE
///
/// Test: `grade_two_medium_yields_request_changes`,
/// `grade_model_approve_solo_high_confidence_medium_still_escalates` (#1897,
/// supersedes the pre-#1897 `grade_one_medium_yields_request_changes`
/// expectation at marginal confidence),
/// `grade_advisory_medium_below_floor_threshold_does_not_escalate`,
/// `grade_high_confidence_medium_above_floor_threshold_escalates`.
///
/// As of #1343 the caller pre-filters refuted / sub-0.50-confidence findings via
/// `is_substantive`, so this function only ever sees substantive findings.
///
/// As of #1359 the floor is split by `FindingCategory`: correctness findings run
/// the three-tier rule above, while method-conformance findings run a separate,
/// strictly-weaker rule (see [`conformance_floor`]) that caps at `REQUEST_CHANGES`
/// and never contributes `BLOCK`.  The combined floor is the stricter of the two.
/// A confident conformance finding also makes the combined result cap-INeligible
/// (#1897) — it is independent grounded evidence, mirroring the High-effort
/// exemption.
fn severity_floor(findings: &[&Finding], thresholds: &Thresholds) -> FloorResult {
    if findings.is_empty() {
        return FloorResult {
            verdict: Verdict::Approve,
            cap_eligible: false,
        };
    }

    // #1359: a method-conformance divergence is an *intent overlay*, capped at
    // REQUEST_CHANGES — it must never drive BLOCK (reserved for correctness /
    // safety).  Run the standard floor over the CORRECTNESS findings only, then
    // combine with the conformance floor via `stricter_of`.
    let (conformance, correctness): (Vec<&&Finding>, Vec<&&Finding>) = findings
        .iter()
        .partition(|f| f.category == FindingCategory::MethodConformance);

    let correctness_result = correctness_floor(&correctness, thresholds);
    let conformance_verdict = conformance_floor(&conformance, thresholds);
    let verdict = stricter_of(
        correctness_result.verdict.clone(),
        conformance_verdict.clone(),
    );

    // #1897: a confident conformance divergence is independent grounded
    // evidence — the cap must never fire when one is present, even if the
    // correctness tier alone would have been cap-eligible.
    let cap_eligible = correctness_result.cap_eligible && conformance_verdict == Verdict::Approve;

    FloorResult {
        verdict,
        cap_eligible,
    }
}

/// The three-tier correctness floor (#1876 merged the old Tiers 2/3 — see below;
/// #1897 layers a `cap_eligible` determination back on top — see [`FloorResult`]).
///
/// Why: separating the correctness rule from the conformance rule (#1359) keeps
/// each rule readable and lets the conformance rule be strictly weaker (capped at
/// REQUEST_CHANGES) without touching the correctness tiers.  #1876 additionally
/// collapsed the old count-gated Tier 2/3 split (≥2 Medium → REQUEST_CHANGES,
/// exactly 1 → APPROVE*) into a single confidence-gated tier: a shadow-eval
/// showed requiring a SECOND corroborating Medium finding before escalating was
/// the single largest source of the reviewer under-firing REQUEST_CHANGES.  A
/// FOLLOW-UP shadow-eval (#1897) showed that same single-Medium tier was ALSO
/// the largest source of over-firing on clean PRs — so the tier's OUTPUT VERDICT
/// is unchanged (a single confident Medium still floors to REQUEST_CHANGES), but
/// this function now additionally reports whether that REQUEST_CHANGES is
/// well-corroborated or rests on marginal evidence alone, via `cap_eligible`.
/// What: applies the three-tier rule set over correctness findings:
///   1. Any escalation-eligible `High`-effort finding → BLOCK.  #PR84: a High
///      finding drives BLOCK ONLY when it clears the citability gate
///      (`is_escalation_eligible` — cited OR `code_provable`); a non-citable,
///      non-diff-provable High finding is demoted to Tier 2 (advisory) and can
///      never force BLOCK.
///   2. ≥1 high-confidence (`confidence > FLOOR_MIN_CONFIDENCE`) Medium-effort
///      finding — OR a High finding demoted by the #PR84 citability gate —
///      → REQUEST_CHANGES.  `cap_eligible` (#1897) is set true only when this
///      REQUEST_CHANGES rests on EXACTLY ONE genuine `Effort::Medium` finding
///      whose confidence is in the marginal band
///      (`FLOOR_MIN_CONFIDENCE` < c < `SOLO_MEDIUM_ESCALATION_CONFIDENCE`) AND
///      no `Effort::High` finding (disqualified or not) is present at all —
///      i.e. ≥2 confident Mediums, a solo Medium clearing the higher
///      solo-escalation bar, and any High-effort finding (a disqualified High
///      is stronger evidence than an ordinary Medium even though it cannot
///      itself drive BLOCK) are all NOT cap-eligible.
///   3. Only `Low` / no floor-counting findings → APPROVE
///
/// Test: `grade_two_medium_yields_request_changes`,
///       `grade_model_approve_solo_high_confidence_medium_still_escalates` (#1897),
///       `grade_model_approve_single_marginal_medium_caps_at_approve_star` (#1897),
///       `grade_advisory_medium_below_floor_threshold_does_not_escalate`,
///       `pr84_uncited_high_finding_does_not_block`,
///       `pr84_confident_uncited_high_caps_at_request_changes` (disqualified
///       High present → never cap-eligible),
///       `high_finding_with_source_citation_still_blocks`.
fn correctness_floor(findings: &[&&Finding], thresholds: &Thresholds) -> FloorResult {
    if findings.is_empty() {
        return FloorResult {
            verdict: Verdict::Approve,
            cap_eligible: false,
        };
    }

    // Partition findings by effort tier.  #PR84: a High-effort finding only
    // drives the BLOCK floor when it clears the citability gate (cited OR
    // `code_provable`).  A non-citable, non-diff-provable High finding is external
    // framework/platform speculation — it is demoted to the advisory (Medium) tier
    // below and can never force BLOCK.
    let has_block_high = findings.iter().any(|f| drives_block_floor(f));

    // #1897: ANY High-effort finding (escalation-eligible or disqualified) rules
    // out the reconciliation cap — a disqualified High is still stronger evidence
    // than an ordinary Medium even though citability keeps it out of the BLOCK
    // floor.
    let has_any_high_effort = findings.iter().any(|f| is_high_severity(f));

    // Only count Medium findings whose confidence clears the floor threshold
    // (#1015: advisory-tier Medium findings must not force REQUEST_CHANGES).  A
    // High finding demoted by the #PR84 citability gate (high-severity but NOT
    // escalation-eligible) is treated as a Medium here — it may raise the floor to
    // REQUEST_CHANGES when it is confident, but never to BLOCK.
    let medium_floor = thresholds.floor_min;
    let has_confident_medium = findings.iter().any(|f| {
        let advisory_medium = f.effort == Effort::Medium || is_disqualified_high(f);
        advisory_medium && f.confidence > medium_floor
    });

    // Tier 1: any escalation-eligible High-effort (critical/high severity) → BLOCK.
    if has_block_high {
        return FloorResult {
            verdict: Verdict::Block,
            cap_eligible: false,
        };
    }

    // Tier 2 (#1876): ANY single high-confidence Medium-effort finding →
    // REQUEST_CHANGES.  A finding that clears FLOOR_MIN_CONFIDENCE (0.80) is,
    // by definition, well-evidenced — it no longer needs a second corroborating
    // Medium to escalate, matching the hard-floor treatment already given to
    // High-effort findings (Tier 1).
    if has_confident_medium {
        // #1897: is this REQUEST_CHANGES well-corroborated, or does it rest
        // solely on one marginal-confidence genuine Medium?  Only genuine
        // `Effort::Medium` findings count toward the cap-eligible tally — a
        // disqualified High already ruled the cap out via `has_any_high_effort`.
        let solo_bar = thresholds.solo_medium_escalation;
        let confident_genuine_medium_confidences: Vec<f32> = findings
            .iter()
            .filter(|f| f.effort == Effort::Medium && f.confidence > medium_floor)
            .map(|f| f.confidence)
            .collect();
        let cap_eligible = !has_any_high_effort
            && confident_genuine_medium_confidences.len() == 1
            && confident_genuine_medium_confidences[0] < solo_bar;
        return FloorResult {
            verdict: Verdict::RequestChanges,
            cap_eligible,
        };
    }

    // Tier 3: only Low-effort, no findings, or all-advisory Medium findings.
    FloorResult {
        verdict: Verdict::Approve,
        cap_eligible: false,
    }
}

/// The method-conformance floor (#1359, `SPEC-CONFORMANCE-02` §5.2; AC-8/AC-12).
///
/// Why: a method-conformance divergence is an advisory overlay on top of the
/// correctness review — it must be conservative and bounded.  The spec is
/// explicit: a conformance finding caps the verdict at `REQUEST_CHANGES` and
/// NEVER drives `BLOCK` (BLOCK is reserved for correctness/safety, OQ-5), and a
/// conformance finding must clear `FLOOR_MIN_CONFIDENCE` (0.80) to affect the
/// verdict at all — below that it is advisory only and does NOT raise the floor
/// (the primary false-positive guard, G3).
/// What: returns `REQUEST_CHANGES` when ANY conformance finding clears 0.80
/// confidence (regardless of its `Effort` — even a `High`-effort conformance
/// finding is capped here, never BLOCK); otherwise `APPROVE` (advisory only).
/// Note the caller's `is_substantive` pre-filter has already dropped refuted /
/// sub-0.50 findings, but the 0.80 gate here is stricter and is what AC-12 pins.
/// Test: `conformance_finding_caps_at_request_changes`,
/// `conformance_high_effort_never_blocks`,
/// `conformance_below_floor_confidence_is_advisory`.
fn conformance_floor(findings: &[&&Finding], thresholds: &Thresholds) -> Verdict {
    // Effort is intentionally ignored: the conformance cap is confidence-only (never BLOCK).
    let floor = thresholds.floor_min;
    let any_confident = findings.iter().any(|f| f.confidence >= floor);
    if any_confident {
        // Capped at REQUEST_CHANGES — conformance NEVER drives BLOCK.
        Verdict::RequestChanges
    } else {
        // Below the confidence floor → advisory only, does not raise the floor.
        Verdict::Approve
    }
}

// ─── Verdict ordering ─────────────────────────────────────────────────────────

/// Return the stricter (higher severity) of two verdicts.
///
/// Why: the floor is a MINIMUM; we take `max(model, floor)` using verdict
/// severity ordering so the model can escalate beyond the floor but cannot
/// go below it.
/// What: compares via `Verdict::ordinal` (the single source of truth, #1357):
/// APPROVE(0) < APPROVE*(1) < REQUEST_CHANGES(2) < BLOCK(3).  Unknown(4) is a
/// separate terminal case handled before `stricter_of` is called.
/// Test: `grade_floor_overrides_model_approve`,
/// `grade_model_block_kept_when_no_critical_finding`.
fn stricter_of(a: Verdict, b: Verdict) -> Verdict {
    if b.ordinal() > a.ordinal() { b } else { a }
}

// ─── Grade-aware entry point ──────────────────────────────────────────────────

/// Derive the final verdict using both the LLM's grade AND the severity floor.
///
/// Why: the grade is the LLM's primary quality signal; the severity floor is the
/// deterministic safety net.  Neither alone is sufficient — the grade alone could
/// be too optimistic (e.g. a confident "A" from a model that missed a High-effort
/// finding), and the floor alone ignores the model's holistic quality assessment.
/// Together they guarantee: final_verdict ≥ max(grade_verdict, severity_floor).
///
/// What: three-step derivation:
///   1. `grade_verdict` = `verdict_for_grade(grade)` — the grade's implied verdict.
///   2. `effective_model` = max(grade_verdict, model_proposed) — stricter of the two.
///      This means: if the model wrote APPROVE but its grade implies APPROVE*, the
///      grade wins as the new "model proposal" going into the floor.
///   3. Final = `derive_verdict(effective_model, findings)` — applies the severity
///      floor so a High finding still floors to BLOCK even with grade "A".
///
/// Special case: when `model_proposed == Unknown`, it is preserved unconditionally
/// (the model could not assess the diff; grade/floor do not apply).
///
/// Also returns the final grade, reconciled by `letter_grade::reconcile_grade_with_verdict`
/// (adversarial-review MEDIUM fix — supersedes a bare `clamp_grade_to_verdict` call,
/// which only handled a grade too OPTIMISTIC for the final verdict; RULE 2 above can
/// now make `final_verdict` MILDER than what `grade` implies, e.g. self-reported
/// `grade:"F"` downgraded to REQUEST_CHANGES, so the reconciliation must also cap an
/// over-severe grade DOWN to agree) so the grade and verdict never disagree in the
/// output — EXCEPT for `Verdict::Unknown`, which returns `None` (no letter grade).
///
/// ## UNKNOWN ⇒ no letter grade (#1474)
///
/// `Verdict::Unknown` means "the diff was un-reviewable" (empty/insufficient diff,
/// truncated output, parse failure) — NOT "reviewed and failed".  An un-reviewable
/// change has no quality letter to report, so the grade is `None` (the output field
/// is then omitted) rather than a misleading `F`.  Previously this path hardcoded
/// `Grade::F`, which collapsed "could not review" into "reviewed → critical failure"
/// — e.g. an empty develop→main release PR whose own review_body said grade "A+"
/// surfaced top-level `grade:"F"`.  `F` is reserved for a real BLOCK verdict.
///
/// Test: `derive_verdict_with_grade_grade_a_no_findings_approve`,
/// `derive_verdict_with_grade_grade_f_no_findings_block`,
/// `derive_verdict_with_grade_severity_overrides_grade_a`,
/// `derive_verdict_with_grade_unknown_yields_no_grade`.
pub fn derive_verdict_with_grade(
    model_proposed: Verdict,
    grade: Grade,
    findings: &[Finding],
) -> (Verdict, Option<Grade>) {
    // UNKNOWN is terminal and un-reviewable — preserve the verdict and suppress the
    // letter grade entirely (None ⇒ field omitted).  An un-reviewable diff has no
    // quality grade; emitting F would falsely report a critical failure (#1474).
    if model_proposed == Verdict::Unknown {
        debug!(
            "verdict=UNKNOWN from model — preserving (diff unassessable); grade suppressed (None)"
        );
        return (Verdict::Unknown, None);
    }

    // Step 1: derive the grade's implied verdict.
    let grade_verdict = verdict_for_grade(grade);

    // Step 2: effective model proposal = stricter of (grade-implied, model-proposed).
    let effective_model = stricter_of(model_proposed.clone(), grade_verdict);

    debug!(
        model_verdict = %model_proposed,
        grade = %grade,
        grade_verdict = %effective_model,
        "derive_verdict_with_grade: using effective_model = max(model, grade)",
    );

    // Step 3: apply the severity floor over the effective model proposal.
    let final_verdict = derive_verdict(effective_model, findings);

    // Reconcile the grade so it is consistent with the final verdict IN BOTH
    // DIRECTIONS (adversarial-review MEDIUM fix): too-optimistic (existing
    // `clamp_grade_to_verdict` behaviour) AND too-severe (new — RULE 2 can
    // downgrade `final_verdict` below what `grade` implies).  `final_verdict`
    // is never Unknown here (the Unknown branch returned above), so this always
    // yields a real letter grade.
    let final_grade = reconcile_grade_with_verdict(grade, &final_verdict);

    (final_verdict, Some(final_grade))
}

// ─── Unit tests ─────────────────────────────────────────────────────────────
// Tests extracted to grade_tests.rs to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "grade_tests.rs"]
mod tests;
