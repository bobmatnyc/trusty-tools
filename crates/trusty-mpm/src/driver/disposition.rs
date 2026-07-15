//! Structured reason for a driver [`crate::driver::policy::Disposition`].
//!
//! Why: the disposition reason used to be a bare `String` built ad-hoc with
//! `format!` at ~12 call sites across the autonomy policy and the conformance
//! FRONT gate. A stringly-typed reason is unmatchable (callers cannot branch on
//! *why* a decision escalated without brittle substring scans), easy to drift
//! (two call sites can emit subtly different wording for the same case), and
//! opaque to future telemetry. Modelling every reason as a data-carrying enum —
//! the anchor of the stringly-typed conversion initiative — makes the reason set
//! exhaustive, refactor-safe, and inspectable, exactly like trusty-review's
//! `Verdict` and this crate's `AutonomyTier`.
//! What: [`DispositionReason`] enumerates every reason the autonomy policy
//! (`T1..T4`, prior-rejection) and the conformance gate (match / divergence /
//! no-intent-source) can produce; [`NoIntentKind`] sub-classifies the fail-open
//! "no intent source" case. `Display` reproduces the exact text each call site
//! emitted before this refactor, *byte-for-byte*, so the wire format
//! (`SessionRecord.pending_decision`, HTTP-visible via CLI/TUI/Slack/Telegram)
//! never changes — the reason is rendered with `.to_string()` only at that edge.
//! Test: `tests` pins the byte-identical rendering of every variant against the
//! literal strings captured from the pre-refactor code.

use std::fmt;

/// The canonical fail-open prefix shared by every [`NoIntentKind`] rendering.
///
/// Why: the conformance gate's `NO_INTENT_SOURCE` const fixes this text ("no
/// intent source", spec §5.1); `Display` must reproduce it verbatim so the
/// gate's own `reason.contains(NO_INTENT_SOURCE)` tests keep passing. Centralising
/// the literal here keeps the byte-identical snapshot honest.
/// What: the string `"no intent source"`.
/// Test: `tests::no_intent_prefix_matches_gate_const`, and every `no_intent_*`
/// snapshot.
const NO_INTENT_SOURCE: &str = "no intent source";

/// Why a fail-open "no intent source" disposition was reached.
///
/// Why: the gate collapses four distinct fail-open situations (unresolved ISR,
/// a ticket/spec gap, non-ticketed work, an empty autonomy input) onto the same
/// `AutoAccept` outcome, but their human-facing text differs in the parenthetical
/// detail. Modelling the detail as an enum keeps each rendering exact while the
/// outer reason stays one variant.
/// What: four variants; `Unresolved` carries the resolver's own reason text.
/// Test: `tests::no_intent_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoIntentKind {
    /// The intent-source resolver could not resolve; carries its reason text.
    Unresolved(String),
    /// Neither ticket nor spec prescribes a method (a gap).
    Gap,
    /// The task carries no ticket reference.
    NonTicketed,
    /// The autonomy-composition input (task text) was empty.
    AutonomyInputEmpty,
}

impl fmt::Display for NoIntentKind {
    /// Why: renders the parenthetical detail of a `no intent source` reason.
    /// What: matches each variant onto its exact pre-refactor wording.
    /// Test: `tests::no_intent_*`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoIntentKind::Unresolved(reason) => write!(f, "unresolved: {reason}"),
            NoIntentKind::Gap => f.write_str("no prescribed method; gap"),
            NoIntentKind::NonTicketed => f.write_str("non-ticketed work"),
            NoIntentKind::AutonomyInputEmpty => f.write_str("autonomy input empty"),
        }
    }
}

/// The structured reason attached to a [`crate::driver::policy::Disposition`].
///
/// Why: replaces the free-form reason `String` so every disposition reason is a
/// typed, matchable value rather than an opaque sentence. This is the anchor
/// example of the stringly-typed conversion initiative: the autonomy policy and
/// the conformance FRONT gate both build reasons, and unifying them here lets
/// callers branch on the cause (and lets telemetry aggregate it) without parsing
/// prose. `Display` still emits the original text verbatim, so the HTTP-visible
/// `pending_decision` wire format is unchanged.
/// What: variants for the autonomy tiers (`T1..T4`, `PriorRejection`) and the
/// conformance gate (`ConformanceMatch`, `ConformanceNoDivergentPlan`,
/// `ConformanceDivergence`, `NoIntentSource`). The `T2GuardrailFailed` /
/// `T3RequiresApprove` variants carry the already-static failing-signal
/// description; `ConformanceDivergence` carries the trimmed prescribed/planned
/// method texts.
/// Test: `tests` asserts the byte-identical rendering of each variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispositionReason {
    // ── Autonomy tier reasons (driver::policy) ──────────────────────────────
    /// T1 auto-accept: style-only change with no objecting guardrail.
    T1StyleOnly,
    /// T1 escalate: trusty-review returned REJECT on a style-only change.
    T1ReviewReject,
    /// T1 escalate: CI is red on a style-only change.
    T1CiRed,
    /// T2 auto-accept: all structured guardrails are green.
    T2AllClear,
    /// T2 escalate: a guardrail was not satisfied; carries the signal text.
    T2GuardrailFailed(&'static str),
    /// T3 auto-accept: architecture-touching change with explicit APPROVE + scope.
    T3Approved,
    /// T3 escalate: requires APPROVE + in-scope + non-red CI; carries the signal.
    T3RequiresApprove(&'static str),
    /// T4 escalate: irreversible / security-sensitive; always escalate.
    T4Escalate,
    /// Escalate: the same decision was rejected before; carries the count.
    PriorRejection(u32),

    // ── Conformance FRONT-gate reasons (daemon::managed_routes::front_gate) ──
    /// Auto-accept fail-open: there is no intent to conform to.
    NoIntentSource(NoIntentKind),
    /// Auto-accept: the planned method matches the ticket/spec method.
    ConformanceMatch,
    /// Auto-accept: no divergent plan; the ticket/spec method is honoured.
    ConformanceNoDivergentPlan,
    /// Escalate: the planned method diverges from the prescribed method.
    ConformanceDivergence {
        /// The ticket/spec-prescribed method text (already trimmed).
        prescribed: String,
        /// The planned method text derived from the task prose (already trimmed).
        planned: String,
    },
}

impl fmt::Display for DispositionReason {
    /// Why: renders each reason to the exact text its call site emitted before
    /// the refactor, so `pending_decision` (and every channel that surfaces it)
    /// is byte-for-byte unchanged.
    /// What: matches every variant onto its original literal / `format!` output.
    /// Test: `tests::*` pins each rendering.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispositionReason::T1StyleOnly => {
                f.write_str("T1: style-only change with no objecting guardrail")
            }
            DispositionReason::T1ReviewReject => {
                f.write_str("T1: trusty-review returned REJECT on a style-only change")
            }
            DispositionReason::T1CiRed => f.write_str("T1: CI is red on a style-only change"),
            DispositionReason::T2AllClear => f.write_str(
                "T2: all structured guardrails green (review APPROVE, CI green, search+memory consistent, in-scope)",
            ),
            DispositionReason::T2GuardrailFailed(signal) => {
                write!(f, "T2: guardrail not satisfied: {signal}")
            }
            DispositionReason::T3Approved => f.write_str(
                "T3: architecture-touching change with explicit trusty-review APPROVE and in-scope validation",
            ),
            DispositionReason::T3RequiresApprove(signal) => {
                write!(f, "T3: requires explicit APPROVE + in-scope + non-red CI; got {signal}")
            }
            DispositionReason::T4Escalate => f.write_str(
                "T4: irreversible or security-sensitive operation; human confirmation required",
            ),
            DispositionReason::PriorRejection(count) => write!(
                f,
                "decision previously rejected {count} time(s); re-escalating to human"
            ),
            DispositionReason::NoIntentSource(kind) => write!(f, "{NO_INTENT_SOURCE} ({kind})"),
            DispositionReason::ConformanceMatch => {
                f.write_str("conformance: planned method matches the ticket/spec method")
            }
            DispositionReason::ConformanceNoDivergentPlan => {
                f.write_str("conformance: no divergent plan; ticket/spec method honoured")
            }
            DispositionReason::ConformanceDivergence {
                prescribed,
                planned,
            } => write!(
                f,
                "conformance divergence: ticket/spec specifies \"{prescribed}\"; plan uses \"{planned}\""
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate's `NO_INTENT_SOURCE` const and our prefix must stay identical, or
    /// the gate's `reason.contains(NO_INTENT_SOURCE)` tests would silently drift.
    #[test]
    fn no_intent_prefix_matches_gate_const() {
        assert_eq!(
            NO_INTENT_SOURCE,
            crate::daemon::managed_routes::front_gate::NO_INTENT_SOURCE
        );
    }

    // ── Byte-identical rendering: autonomy tier reasons ─────────────────────

    #[test]
    fn renders_tier_reasons_byte_identical() {
        assert_eq!(
            DispositionReason::T1StyleOnly.to_string(),
            "T1: style-only change with no objecting guardrail"
        );
        assert_eq!(
            DispositionReason::T1ReviewReject.to_string(),
            "T1: trusty-review returned REJECT on a style-only change"
        );
        assert_eq!(
            DispositionReason::T1CiRed.to_string(),
            "T1: CI is red on a style-only change"
        );
        assert_eq!(
            DispositionReason::T2AllClear.to_string(),
            "T2: all structured guardrails green (review APPROVE, CI green, search+memory consistent, in-scope)"
        );
        assert_eq!(
            DispositionReason::T2GuardrailFailed("trusty-review did not APPROVE").to_string(),
            "T2: guardrail not satisfied: trusty-review did not APPROVE"
        );
        assert_eq!(
            DispositionReason::T3Approved.to_string(),
            "T3: architecture-touching change with explicit trusty-review APPROVE and in-scope validation"
        );
        assert_eq!(
            DispositionReason::T3RequiresApprove("CI not green").to_string(),
            "T3: requires explicit APPROVE + in-scope + non-red CI; got CI not green"
        );
        assert_eq!(
            DispositionReason::T4Escalate.to_string(),
            "T4: irreversible or security-sensitive operation; human confirmation required"
        );
        assert_eq!(
            DispositionReason::PriorRejection(2).to_string(),
            "decision previously rejected 2 time(s); re-escalating to human"
        );
    }

    // ── Byte-identical rendering: conformance reasons ───────────────────────

    #[test]
    fn renders_conformance_reasons_byte_identical() {
        assert_eq!(
            DispositionReason::ConformanceMatch.to_string(),
            "conformance: planned method matches the ticket/spec method"
        );
        assert_eq!(
            DispositionReason::ConformanceNoDivergentPlan.to_string(),
            "conformance: no divergent plan; ticket/spec method honoured"
        );
        assert_eq!(
            DispositionReason::ConformanceDivergence {
                prescribed: "use cursor-based pagination".to_string(),
                planned: "use offset pagination".to_string(),
            }
            .to_string(),
            "conformance divergence: ticket/spec specifies \"use cursor-based pagination\"; plan uses \"use offset pagination\""
        );
    }

    // ── Byte-identical rendering: no-intent-source fail-open reasons ─────────

    #[test]
    fn renders_no_intent_reasons_byte_identical() {
        assert_eq!(
            DispositionReason::NoIntentSource(NoIntentKind::Unresolved(
                "ticket fetch failed: 404".to_string()
            ))
            .to_string(),
            "no intent source (unresolved: ticket fetch failed: 404)"
        );
        assert_eq!(
            DispositionReason::NoIntentSource(NoIntentKind::Gap).to_string(),
            "no intent source (no prescribed method; gap)"
        );
        assert_eq!(
            DispositionReason::NoIntentSource(NoIntentKind::NonTicketed).to_string(),
            "no intent source (non-ticketed work)"
        );
        assert_eq!(
            DispositionReason::NoIntentSource(NoIntentKind::AutonomyInputEmpty).to_string(),
            "no intent source (autonomy input empty)"
        );
    }
}
