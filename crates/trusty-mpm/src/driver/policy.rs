//! Autonomy policy for the session-manager driver.
//!
//! Why: the driver (the calling agentic process that operates trusty-mpm) needs
//! a *structured*, *auditable*, *non-LLM* rule set to decide whether a proposed
//! action may be auto-accepted or must be escalated to a human. Targeting ~80%
//! auto-accept / ~20% escalation is only safe if the auto-accept gate is driven
//! by hard signals (trusty-review verdict, green CI, search/memory consistency,
//! in-scope validation) rather than by reading pane state with a classifier. A
//! subtly-wrong harness must never be able to auto-merge bad code, so every
//! decision here is a pure function over explicit signal structs.
//! What: defines the [`AutonomyTier`] T1–T4 model, the [`ActionContext`] /
//! [`GuardrailSignals`] inputs, the pure guardrail predicates, and
//! [`evaluate_autonomy_tier`] — the single entry point that maps a proposed
//! action plus its guardrail signals onto a tier and an [`AutonomyDecision`]
//! (auto-accept vs. escalate, with a reason).
//! Test: the `tests` module exercises every tier path, every guardrail, and the
//! safety rule that destructive actions always escalate — all without any LLM
//! or network call.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::driver::correlation::{ScopeCheck, SessionCorrelation};

/// Words in a `pending_decision` that mark an irreversible / destructive action.
///
/// Why: T4 (always-escalate) is anchored on a deny-list of operations a human
/// must confirm. Centralizing the list keeps the classifier honest and testable.
/// What: lowercase substrings matched case-insensitively against the decision text.
/// Test: `destructive_keyword_detection`.
const DESTRUCTIVE_KEYWORDS: &[&str] = &[
    "delete",
    "drop table",
    "drop database",
    "push --force",
    "force-push",
    "force push",
    "decommission",
    "rm -rf",
    "truncate",
    "revoke",
    "rotate secret",
    "rotate key",
    "wipe",
];

/// Verdict returned by trusty-review for a diff or PR.
///
/// Why: the code-review signal is the strongest guardrail; modeling it as a
/// typed enum (rather than a string) makes the gate logic exhaustive and prevents
/// silent typos like `"approved"` vs `"APPROVE"`.
/// What: three variants — `Approve` (no correctness findings), `Reject`
/// (correctness findings present), and `Unavailable` (review not run / errored).
/// Test: `review_verdict_gates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// trusty-review approved the change with no correctness findings.
    Approve,
    /// trusty-review found correctness issues; the change must not auto-accept.
    Reject,
    /// No review result is available (not run, errored, or timed out).
    Unavailable,
}

/// CI / test-suite status for the change under consideration.
///
/// Why: a green test suite is a non-negotiable input to the auto-accept gate;
/// modeling `Unknown` explicitly forces the policy to treat "we don't know" as
/// not-green rather than silently passing.
/// What: `Green` (all required checks passed), `Red` (a required check failed),
/// `Unknown` (no status available yet).
/// Test: `ci_status_gates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiStatus {
    /// All required checks are green.
    Green,
    /// At least one required check failed.
    Red,
    /// No CI status is available.
    Unknown,
}

/// Structured, non-LLM guardrail signals consulted by the auto-accept gate.
///
/// Why: bundling the four hard signals (review, CI, search consistency, memory
/// consistency) plus the scope check into one struct makes
/// [`evaluate_autonomy_tier`] a pure function of explicit inputs — no hidden
/// global state, no I/O — so the whole policy is unit-testable offline.
/// What: carries the [`ReviewVerdict`], [`CiStatus`], two boolean consistency
/// flags sourced from trusty-search / trusty-memory, and the [`ScopeCheck`]
/// produced by [`SessionCorrelation::validate_in_scope`].
/// Test: constructed in every `evaluate_*` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardrailSignals {
    /// trusty-review verdict on the diff / PR.
    pub review: ReviewVerdict,
    /// CI / test-suite status.
    pub ci: CiStatus,
    /// trusty-search found no conflicting implementation (`true` = consistent).
    pub search_consistent: bool,
    /// trusty-memory surfaced no blocking prior decision (`true` = consistent).
    pub memory_consistent: bool,
    /// Result of validating the change against the session's correlation.
    pub scope: ScopeCheck,
}

impl GuardrailSignals {
    /// Whether every structured guardrail is favorable.
    ///
    /// Why: the T2 auto-accept gate requires ALL signals green; this collapses
    /// the conjunction into one auditable predicate.
    /// What: returns `true` only when review is `Approve`, CI is `Green`, both
    /// consistency flags are set, and scope is `InScope`.
    /// Test: `all_clear_requires_every_signal`.
    pub fn all_clear(&self) -> bool {
        self.review == ReviewVerdict::Approve
            && self.ci == CiStatus::Green
            && self.search_consistent
            && self.memory_consistent
            && self.scope.is_in_scope()
    }
}

/// Classification of the change's blast radius, supplied by the caller.
///
/// Why: tier selection depends on *what kind* of change is proposed — a style-only
/// tweak (T1) is categorically lower-risk than an architecture-touching, cross-crate
/// change (T3). The driver computes this from diff metadata (files touched, crates
/// spanned) before calling the policy; the policy itself does not parse diffs.
/// What: four variants ordered by escalating risk, mirroring the documented
/// T1–T4 model in `SESSION_MANAGER_DRIVER_AGENT.md` §4.
/// Test: `change_class_orders_tiers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClass {
    /// Trivial / formatting / comment-only change.
    StyleOnly,
    /// Standard feature or bugfix confined to one crate.
    Standard,
    /// Architecture-touching or cross-crate change.
    Architectural,
    /// Irreversible or security-sensitive operation.
    Destructive,
}

/// The proposed action plus the context needed to tier it.
///
/// Why: [`evaluate_autonomy_tier`] needs both the human-readable decision text
/// (to scan for destructive keywords) and the structured change classification +
/// session correlation. Bundling them keeps the call site clean and the function
/// signature stable as more context fields accrue.
/// What: the `pending_decision` text surfaced by the harness, the caller-computed
/// [`ChangeClass`], the session [`SessionCorrelation`], and the count of prior
/// rejections of this decision in the session.
/// Test: constructed in every `evaluate_*` test.
#[derive(Debug, Clone)]
pub struct ActionContext {
    /// The `pending_decision` text surfaced by the session harness.
    pub pending_decision: String,
    /// Caller-computed change classification (blast radius).
    pub change_class: ChangeClass,
    /// The session's artifact correlation (scope anchor).
    pub correlation: SessionCorrelation,
    /// How many times this proposed default was already rejected in-session.
    pub prior_rejections: u32,
}

impl ActionContext {
    /// Whether the pending-decision text names an irreversible operation.
    ///
    /// Why: even a change the caller classified as `Standard` must be forced to
    /// T4 if its decision text contains a destructive keyword — defense in depth
    /// against a mis-classification upstream.
    /// What: case-insensitive substring match against [`DESTRUCTIVE_KEYWORDS`].
    /// Test: `destructive_keyword_detection`.
    pub fn mentions_destructive_op(&self) -> bool {
        let lowered = self.pending_decision.to_lowercase();
        DESTRUCTIVE_KEYWORDS.iter().any(|kw| lowered.contains(kw))
    }
}

/// Autonomy tier governing how a proposed action is handled.
///
/// Why: the driver maps every proposed action onto one of four tiers so its
/// behavior is predictable and auditable rather than ad-hoc. The tiers mirror the
/// unicorn-factory tiered-PR-autonomy model documented in
/// `docs/trusty-mpm/spec/SESSION_MANAGER_DRIVER_AGENT.md` §4.
///
/// Escalating order: `T1` < `T2` < `T3` < `T4`. The `Ord` derive follows variant
/// declaration order, so a higher tier compares greater — callers may take the
/// `max` of two tiers to pick the more cautious one.
///
/// What:
/// - **T1 — observe / style-only**: trivial change, auto-accepted without the full
///   guardrail battery.
/// - **T2 — guarded auto-accept**: standard feature/bugfix, auto-accepted only when
///   ALL structured guardrails are green.
/// - **T3 — fallback-escalate**: architecture-touching / cross-crate change;
///   auto-accept requires an explicit trusty-review APPROVE *and* a clean scope,
///   otherwise it escalates.
/// - **T4 — human-escalate**: irreversible / security-sensitive / destructive
///   operation; ALWAYS escalates regardless of guardrails.
///
/// Test: `tier_ordering`, and every `evaluate_*` test asserts the chosen tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AutonomyTier {
    /// T1 — trivial / style-only; auto-accept without the full guardrail battery.
    T1,
    /// T2 — standard change; auto-accept only when all guardrails are green.
    T2,
    /// T3 — architecture-touching; auto-accept needs explicit review APPROVE + scope.
    T3,
    /// T4 — irreversible / security-sensitive; always escalate to a human.
    T4,
}

impl AutonomyTier {
    /// Short stable label for logging / serialization (`"T1"`..`"T4"`).
    ///
    /// Why: callers log tiers into audit trails and structured events; a stable
    /// string avoids depending on `Debug` formatting.
    /// What: returns the canonical uppercase tier label.
    /// Test: `tier_labels`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::T1 => "T1",
            Self::T2 => "T2",
            Self::T3 => "T3",
            Self::T4 => "T4",
        }
    }
}

/// The disposition of a proposed action after applying the policy.
///
/// Why: the driver needs more than a tier — it needs the *action to take*
/// (auto-accept or escalate) and a machine- and human-readable *reason*, so the
/// decision can be logged, surfaced to the human on escalation, and audited later.
/// What: an enum with [`Disposition::AutoAccept`] and [`Disposition::Escalate`],
/// each carrying a reason string; wrapped together with the chosen tier in
/// [`AutonomyDecision`].
/// Test: matched in every `evaluate_*` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// The proposed default may be auto-accepted.
    AutoAccept {
        /// Why the gate cleared (which guardrails were favorable).
        reason: String,
    },
    /// The decision must be escalated to a human.
    Escalate {
        /// Why the gate did not clear / why human review is required.
        reason: String,
    },
}

impl Disposition {
    /// Whether this disposition is an auto-accept.
    ///
    /// Why: callers frequently branch on the boolean; this avoids a verbose match.
    /// What: returns `true` for [`Disposition::AutoAccept`].
    /// Test: asserted across `evaluate_*` tests.
    pub fn is_auto_accept(&self) -> bool {
        matches!(self, Disposition::AutoAccept { .. })
    }
}

/// The full policy decision: the tier plus the disposition.
///
/// Why: this is the single value [`evaluate_autonomy_tier`] returns so callers get
/// the tier (for telemetry) and the disposition (for action) atomically.
/// What: pairs an [`AutonomyTier`] with a [`Disposition`].
/// Test: returned and asserted in every `evaluate_*` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutonomyDecision {
    /// The tier the action was classified into.
    pub tier: AutonomyTier,
    /// The resulting disposition (auto-accept vs. escalate) with its reason.
    pub disposition: Disposition,
}

impl AutonomyDecision {
    /// Convenience: whether this decision auto-accepts.
    ///
    /// Why: terse call-site checks without reaching into `disposition`.
    /// What: delegates to [`Disposition::is_auto_accept`].
    /// Test: asserted across `evaluate_*` tests.
    pub fn is_auto_accept(&self) -> bool {
        self.disposition.is_auto_accept()
    }
}

/// Errors that can arise while evaluating the autonomy policy.
///
/// Why: library code must surface structured, matchable errors instead of
/// panicking; the policy can refuse to decide when its inputs are incoherent
/// (e.g. an empty decision text), and the caller must handle that explicitly.
/// What: a `thiserror` enum; currently one variant for an empty pending decision.
/// Test: `empty_decision_is_error`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    /// The pending-decision text was empty / whitespace-only, so it cannot be
    /// classified for destructive keywords.
    #[error("pending decision text is empty; cannot evaluate autonomy policy")]
    EmptyDecision,
}

/// Classify the base tier from the caller-supplied change class, then escalate it
/// if the decision text names a destructive operation.
///
/// Why: factored out of [`evaluate_autonomy_tier`] so the tier-selection logic is
/// independently testable and the keyword-override (defense-in-depth) is explicit.
/// What: maps [`ChangeClass`] onto its base tier and bumps to T4 when
/// [`ActionContext::mentions_destructive_op`] is true.
/// Test: `base_tier_for_class`, `destructive_text_forces_t4`.
fn classify_tier(ctx: &ActionContext) -> AutonomyTier {
    let base = match ctx.change_class {
        ChangeClass::StyleOnly => AutonomyTier::T1,
        ChangeClass::Standard => AutonomyTier::T2,
        ChangeClass::Architectural => AutonomyTier::T3,
        ChangeClass::Destructive => AutonomyTier::T4,
    };
    if ctx.mentions_destructive_op() {
        base.max(AutonomyTier::T4)
    } else {
        base
    }
}

/// Evaluate the autonomy policy for a proposed action.
///
/// Why: this is the single, pure entry point the driver calls for every
/// `pending_decision`. It encodes the documented T1–T4 model so the auto-accept /
/// escalate choice is deterministic, auditable, and free of any LLM or network
/// dependency — satisfying the safety rule that the pane classifier must never be
/// the approval gate.
///
/// What: returns an [`AutonomyDecision`] (tier + disposition). Algorithm: (1)
/// reject empty decision text with [`PolicyError::EmptyDecision`]; (2) classify
/// the tier from the change class, forcing T4 on destructive text; (3) apply
/// per-tier gating —
///
/// - **T1**: auto-accept (style-only) — but still escalate if a guardrail is
///   actively `Reject`/`Red` (a "trivial" change that fails review isn't trivial).
/// - **T2**: auto-accept iff ALL guardrails are clear.
/// - **T3**: auto-accept iff review is `Approve` AND scope is `InScope` AND CI is
///   not `Red`. Otherwise escalate.
/// - **T4**: always escalate.
///
/// Regardless of tier, the policy escalates when the same decision was rejected
/// before (`prior_rejections > 0`).
///
/// Test: the `tests` module covers each branch.
pub fn evaluate_autonomy_tier(
    ctx: &ActionContext,
    signals: &GuardrailSignals,
) -> Result<AutonomyDecision, PolicyError> {
    if ctx.pending_decision.trim().is_empty() {
        return Err(PolicyError::EmptyDecision);
    }

    let tier = classify_tier(ctx);

    // A previously-rejected decision always escalates, regardless of tier — the
    // human already pushed back once, so the driver must not re-auto-accept.
    if ctx.prior_rejections > 0 {
        return Ok(AutonomyDecision {
            tier,
            disposition: Disposition::Escalate {
                reason: format!(
                    "decision previously rejected {} time(s); re-escalating to human",
                    ctx.prior_rejections
                ),
            },
        });
    }

    let disposition = match tier {
        AutonomyTier::T1 => gate_t1(signals),
        AutonomyTier::T2 => gate_t2(signals),
        AutonomyTier::T3 => gate_t3(signals),
        AutonomyTier::T4 => Disposition::Escalate {
            reason: "T4: irreversible or security-sensitive operation; human confirmation required"
                .to_string(),
        },
    };

    Ok(AutonomyDecision { tier, disposition })
}

/// T1 gate: style-only changes auto-accept unless a guardrail actively objects.
///
/// Why: even a "trivial" change must not auto-accept if trusty-review actually
/// rejected it or CI is red — a formatter that breaks the build is not trivial.
/// What: escalates on `Reject` / `Red`, otherwise auto-accepts.
/// Test: `evaluate_t1_auto_accepts`, `evaluate_t1_escalates_on_red`.
fn gate_t1(signals: &GuardrailSignals) -> Disposition {
    if signals.review == ReviewVerdict::Reject {
        Disposition::Escalate {
            reason: "T1: trusty-review returned REJECT on a style-only change".to_string(),
        }
    } else if signals.ci == CiStatus::Red {
        Disposition::Escalate {
            reason: "T1: CI is red on a style-only change".to_string(),
        }
    } else {
        Disposition::AutoAccept {
            reason: "T1: style-only change with no objecting guardrail".to_string(),
        }
    }
}

/// T2 gate: standard changes auto-accept only when ALL guardrails are clear.
///
/// Why: the documented ~80% auto-accept target rests on T2 — the common case —
/// passing the full structured battery (review APPROVE, green CI, search & memory
/// consistency, in-scope).
/// What: auto-accepts on [`GuardrailSignals::all_clear`], otherwise escalates with
/// a reason naming the first failing signal.
/// Test: `evaluate_t2_auto_accepts`, `evaluate_t2_escalates_*`.
fn gate_t2(signals: &GuardrailSignals) -> Disposition {
    if signals.all_clear() {
        Disposition::AutoAccept {
            reason: "T2: all structured guardrails green (review APPROVE, CI green, search+memory consistent, in-scope)".to_string(),
        }
    } else {
        Disposition::Escalate {
            reason: format!(
                "T2: guardrail not satisfied: {}",
                first_failing_signal(signals)
            ),
        }
    }
}

/// T3 gate: architecture-touching changes need explicit APPROVE + clean scope.
///
/// Why: cross-crate / architectural changes carry more blast radius, so the bar is
/// higher — a bare green CI is not enough; trusty-review must explicitly APPROVE and
/// the work must be in-scope. CI must not be actively red.
/// What: auto-accepts iff review is `Approve` AND scope is `InScope` AND CI is not
/// `Red`; otherwise escalates.
/// Test: `evaluate_t3_auto_accepts`, `evaluate_t3_escalates_*`.
fn gate_t3(signals: &GuardrailSignals) -> Disposition {
    let approved = signals.review == ReviewVerdict::Approve;
    let in_scope = signals.scope.is_in_scope();
    let ci_ok = signals.ci != CiStatus::Red;
    if approved && in_scope && ci_ok {
        Disposition::AutoAccept {
            reason: "T3: architecture-touching change with explicit trusty-review APPROVE and in-scope validation".to_string(),
        }
    } else {
        Disposition::Escalate {
            reason: format!(
                "T3: requires explicit APPROVE + in-scope + non-red CI; got {}",
                first_failing_signal(signals)
            ),
        }
    }
}

/// Name the first unfavorable structured signal, for human-readable escalation.
///
/// Why: an escalation message that says merely "guardrail failed" wastes the
/// human's time; naming the specific signal makes triage instant.
/// What: returns a static description of the first failing signal in a fixed
/// priority order (review → CI → search → memory → scope), or "none" when all clear.
/// Test: `first_failing_signal_priority`.
fn first_failing_signal(signals: &GuardrailSignals) -> &'static str {
    if signals.review != ReviewVerdict::Approve {
        "trusty-review did not APPROVE"
    } else if signals.ci != CiStatus::Green {
        "CI not green"
    } else if !signals.search_consistent {
        "trusty-search found a conflicting implementation"
    } else if !signals.memory_consistent {
        "trusty-memory surfaced a blocking prior decision"
    } else if !signals.scope.is_in_scope() {
        "change is out-of-scope or session is uncorrelated"
    } else {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn correlated() -> SessionCorrelation {
        SessionCorrelation::new()
            .with_worktree("/repo/wt")
            .with_issue_id(1204)
    }

    fn ctx(decision: &str, class: ChangeClass) -> ActionContext {
        ActionContext {
            pending_decision: decision.to_string(),
            change_class: class,
            correlation: correlated(),
            prior_rejections: 0,
        }
    }

    fn all_clear_signals() -> GuardrailSignals {
        GuardrailSignals {
            review: ReviewVerdict::Approve,
            ci: CiStatus::Green,
            search_consistent: true,
            memory_consistent: true,
            scope: ScopeCheck::InScope,
        }
    }

    #[test]
    fn tier_ordering() {
        assert!(AutonomyTier::T1 < AutonomyTier::T2);
        assert!(AutonomyTier::T2 < AutonomyTier::T3);
        assert!(AutonomyTier::T3 < AutonomyTier::T4);
        assert_eq!(AutonomyTier::T2.max(AutonomyTier::T4), AutonomyTier::T4);
    }

    #[test]
    fn tier_labels() {
        assert_eq!(AutonomyTier::T1.label(), "T1");
        assert_eq!(AutonomyTier::T4.label(), "T4");
    }

    #[test]
    fn destructive_keyword_detection() {
        let c = ctx(
            "Proceed to delete the staging table?",
            ChangeClass::Standard,
        );
        assert!(c.mentions_destructive_op());
        let c2 = ctx("Apply the formatting change?", ChangeClass::StyleOnly);
        assert!(!c2.mentions_destructive_op());
        let c3 = ctx(
            "Should I push --force to the branch?",
            ChangeClass::Standard,
        );
        assert!(c3.mentions_destructive_op());
    }

    #[test]
    fn base_tier_for_class() {
        assert_eq!(
            classify_tier(&ctx("ok", ChangeClass::StyleOnly)),
            AutonomyTier::T1
        );
        assert_eq!(
            classify_tier(&ctx("ok", ChangeClass::Standard)),
            AutonomyTier::T2
        );
        assert_eq!(
            classify_tier(&ctx("ok", ChangeClass::Architectural)),
            AutonomyTier::T3
        );
        assert_eq!(
            classify_tier(&ctx("ok", ChangeClass::Destructive)),
            AutonomyTier::T4
        );
    }

    #[test]
    fn destructive_text_forces_t4() {
        // Caller classified it as a Standard change, but the text says "delete".
        let c = ctx("delete the old index", ChangeClass::Standard);
        assert_eq!(classify_tier(&c), AutonomyTier::T4);
    }

    #[test]
    fn empty_decision_is_error() {
        let c = ctx("   ", ChangeClass::Standard);
        let res = evaluate_autonomy_tier(&c, &all_clear_signals());
        assert_eq!(res, Err(PolicyError::EmptyDecision));
    }

    #[test]
    fn evaluate_t1_auto_accepts() {
        let c = ctx("apply rustfmt", ChangeClass::StyleOnly);
        let d = evaluate_autonomy_tier(&c, &all_clear_signals()).expect("ok");
        assert_eq!(d.tier, AutonomyTier::T1);
        assert!(d.is_auto_accept());
    }

    #[test]
    fn evaluate_t1_escalates_on_red() {
        let c = ctx("apply rustfmt", ChangeClass::StyleOnly);
        let mut s = all_clear_signals();
        s.ci = CiStatus::Red;
        let d = evaluate_autonomy_tier(&c, &s).expect("ok");
        assert_eq!(d.tier, AutonomyTier::T1);
        assert!(!d.is_auto_accept());
    }

    #[test]
    fn evaluate_t1_escalates_on_reject() {
        let c = ctx("apply rustfmt", ChangeClass::StyleOnly);
        let mut s = all_clear_signals();
        s.review = ReviewVerdict::Reject;
        let d = evaluate_autonomy_tier(&c, &s).expect("ok");
        assert!(!d.is_auto_accept());
    }

    #[test]
    fn evaluate_t2_auto_accepts() {
        let c = ctx("implement the parser fix", ChangeClass::Standard);
        let d = evaluate_autonomy_tier(&c, &all_clear_signals()).expect("ok");
        assert_eq!(d.tier, AutonomyTier::T2);
        assert!(d.is_auto_accept());
    }

    #[test]
    fn evaluate_t2_escalates_on_review_unavailable() {
        let c = ctx("implement the parser fix", ChangeClass::Standard);
        let mut s = all_clear_signals();
        s.review = ReviewVerdict::Unavailable;
        let d = evaluate_autonomy_tier(&c, &s).expect("ok");
        assert_eq!(d.tier, AutonomyTier::T2);
        assert!(!d.is_auto_accept());
    }

    #[test]
    fn evaluate_t2_escalates_on_inconsistency() {
        let c = ctx("implement the parser fix", ChangeClass::Standard);
        let mut s = all_clear_signals();
        s.memory_consistent = false;
        let d = evaluate_autonomy_tier(&c, &s).expect("ok");
        assert!(!d.is_auto_accept());
    }

    #[test]
    fn evaluate_t2_escalates_out_of_scope() {
        let c = ctx("implement the parser fix", ChangeClass::Standard);
        let mut s = all_clear_signals();
        s.scope = ScopeCheck::OutOfScope {
            stray_paths: vec![PathBuf::from("/etc/passwd")],
            foreign_issue_ids: vec![],
        };
        let d = evaluate_autonomy_tier(&c, &s).expect("ok");
        assert!(!d.is_auto_accept());
    }

    #[test]
    fn evaluate_t3_auto_accepts() {
        let c = ctx("refactor the cross-crate trait", ChangeClass::Architectural);
        let d = evaluate_autonomy_tier(&c, &all_clear_signals()).expect("ok");
        assert_eq!(d.tier, AutonomyTier::T3);
        assert!(d.is_auto_accept());
    }

    #[test]
    fn evaluate_t3_escalates_without_approve() {
        let c = ctx("refactor the cross-crate trait", ChangeClass::Architectural);
        let mut s = all_clear_signals();
        s.review = ReviewVerdict::Unavailable; // not an explicit APPROVE
        let d = evaluate_autonomy_tier(&c, &s).expect("ok");
        assert_eq!(d.tier, AutonomyTier::T3);
        assert!(!d.is_auto_accept());
    }

    #[test]
    fn evaluate_t3_tolerates_unknown_ci_but_not_red() {
        let c = ctx("refactor the cross-crate trait", ChangeClass::Architectural);
        let mut s = all_clear_signals();
        s.ci = CiStatus::Unknown;
        let d = evaluate_autonomy_tier(&c, &s).expect("ok");
        assert!(
            d.is_auto_accept(),
            "T3 tolerates Unknown CI when APPROVE+scope"
        );

        s.ci = CiStatus::Red;
        let d2 = evaluate_autonomy_tier(&c, &s).expect("ok");
        assert!(!d2.is_auto_accept(), "T3 must escalate on red CI");
    }

    #[test]
    fn evaluate_t4_always_escalates() {
        let c = ctx(
            "decommission the production index",
            ChangeClass::Destructive,
        );
        let d = evaluate_autonomy_tier(&c, &all_clear_signals()).expect("ok");
        assert_eq!(d.tier, AutonomyTier::T4);
        assert!(!d.is_auto_accept());
    }

    #[test]
    fn evaluate_destructive_text_escalates_even_when_classified_standard() {
        // Safety rule: destructive keyword forces T4 → always escalate, even with
        // a fully-clear guardrail battery.
        let c = ctx("drop table sessions to reset", ChangeClass::Standard);
        let d = evaluate_autonomy_tier(&c, &all_clear_signals()).expect("ok");
        assert_eq!(d.tier, AutonomyTier::T4);
        assert!(!d.is_auto_accept());
    }

    #[test]
    fn prior_rejection_always_escalates() {
        let mut c = ctx("implement the parser fix", ChangeClass::Standard);
        c.prior_rejections = 1;
        let d = evaluate_autonomy_tier(&c, &all_clear_signals()).expect("ok");
        // tier still computed, but disposition is escalate.
        assert_eq!(d.tier, AutonomyTier::T2);
        assert!(!d.is_auto_accept());
    }

    #[test]
    fn all_clear_requires_every_signal() {
        assert!(all_clear_signals().all_clear());
        let mut s = all_clear_signals();
        s.search_consistent = false;
        assert!(!s.all_clear());
    }

    #[test]
    fn review_verdict_gates() {
        // Reject blocks T2 auto-accept.
        let c = ctx("standard fix", ChangeClass::Standard);
        let mut s = all_clear_signals();
        s.review = ReviewVerdict::Reject;
        assert!(!evaluate_autonomy_tier(&c, &s).unwrap().is_auto_accept());
    }

    #[test]
    fn ci_status_gates() {
        let c = ctx("standard fix", ChangeClass::Standard);
        let mut s = all_clear_signals();
        s.ci = CiStatus::Unknown;
        assert!(!evaluate_autonomy_tier(&c, &s).unwrap().is_auto_accept());
    }

    #[test]
    fn change_class_orders_tiers() {
        assert!(
            classify_tier(&ctx("a", ChangeClass::StyleOnly))
                < classify_tier(&ctx("b", ChangeClass::Standard))
        );
        assert!(
            classify_tier(&ctx("c", ChangeClass::Architectural))
                < classify_tier(&ctx("d", ChangeClass::Destructive))
        );
    }

    #[test]
    fn first_failing_signal_priority() {
        let mut s = all_clear_signals();
        assert_eq!(first_failing_signal(&s), "none");
        s.memory_consistent = false;
        assert_eq!(
            first_failing_signal(&s),
            "trusty-memory surfaced a blocking prior decision"
        );
        s.review = ReviewVerdict::Reject; // higher priority wins
        assert_eq!(first_failing_signal(&s), "trusty-review did not APPROVE");
    }

    #[test]
    fn decision_serde_round_trip_for_tier() {
        let t = AutonomyTier::T3;
        let json = serde_json::to_string(&t).expect("ser");
        assert_eq!(json, "\"T3\"");
        let back: AutonomyTier = serde_json::from_str(&json).expect("de");
        assert_eq!(back, t);
    }
}
