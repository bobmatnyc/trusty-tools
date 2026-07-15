//! Unit tests for the pre-spawn conformance FRONT gate (AC-13..AC-17).
//!
//! Why: the gate's correctness is safety-critical (it decides whether a session
//! auto-spawns or escalates) and must be verifiable offline — no GitHub, no
//! network. These tests mock the ISR seam and assert every matrix row, the
//! fail-open paths, the stricter-wins composition, and the #1269 degraded branch.
//! What: a `MockGate` returning a fixed `ResolvedIntent`, plus pure-function
//! assertions over `front_gate_disposition` / `stricter_of` / `run_front_gate`.
//! Test: this IS the test module.

use super::*;

use async_trait::async_trait;
use trusty_common::intent_source::{Method, MethodKind, Precedence, ResolvedIntent, TicketRef};

use crate::driver::correlation::{ScopeCheck, SessionCorrelation};
use crate::driver::disposition::DispositionReason;
use crate::driver::policy::{
    ActionContext, ChangeClass, CiStatus, Disposition, GuardrailSignals, ReviewVerdict,
};

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// A `ConformanceGate` that returns a pre-baked `ResolvedIntent` (no network).
struct MockGate(ResolvedIntent);

#[async_trait]
impl ConformanceGate for MockGate {
    async fn resolve(
        &self,
        _ticket_id: &str,
        _owner: &str,
        _repo: &str,
        _task: &str,
    ) -> ResolvedIntent {
        self.0.clone()
    }
}

fn method(text: &str) -> Method {
    Method {
        text: text.to_string(),
        kind: MethodKind::Approach,
        source_excerpt: text.to_string(),
    }
}

fn ticket_ref() -> TicketRef {
    TicketRef {
        id: "#1360".to_string(),
        title: "front gate".to_string(),
        url: None,
        backend: "github".to_string(),
    }
}

/// A resolved intent where the TICKET prescribes `m` (precedence: Ticket).
fn ticket_intent(m: &str) -> ResolvedIntent {
    ResolvedIntent {
        ticket: Some(ticket_ref()),
        ticket_method: Some(method(m)),
        spec_section: None,
        spec_method: None,
        precedence_winner: Precedence::Ticket,
        conflict: false,
        stale_spec: false,
        unresolved: None,
    }
}

/// A representative `AutoAccept` disposition for composition tests.
///
/// The `reason` argument is retained for call-site readability but does not
/// affect the composition logic under test (`stricter_of*` only inspect the
/// `AutoAccept`/`Escalate` discriminant), so every auto-accept maps to one
/// representative [`DispositionReason`].
fn auto(_reason: &str) -> Disposition {
    Disposition::AutoAccept {
        reason: DispositionReason::ConformanceMatch,
    }
}

/// A representative `Escalate` disposition for composition tests.
///
/// The `reason` string is mapped to a real [`DispositionReason`] deterministically
/// (T4 text → `T4Escalate`, otherwise `ConformanceDivergence`) so equality
/// assertions between the two escalation sources stay meaningful — a `T4`
/// escalation and a conformance-divergence escalation render distinct values.
fn esc(reason: &str) -> Disposition {
    let mapped = if reason.contains("T4") {
        DispositionReason::T4Escalate
    } else {
        DispositionReason::ConformanceDivergence {
            prescribed: "prescribed".to_string(),
            planned: "planned".to_string(),
        }
    };
    Disposition::Escalate { reason: mapped }
}

// ── front_gate_disposition: matrix rows ─────────────────────────────────────────

#[test]
fn gap_auto_accepts() {
    // M3: neither ticket nor spec prescribes a method.
    let intent = ResolvedIntent::none();
    let d = front_gate_disposition(&intent, None);
    assert!(d.is_auto_accept(), "gap must auto-accept (M3)");
}

#[test]
fn unresolved_auto_accepts() {
    // Fail-open (AC-17): ISR could not resolve.
    let intent = ResolvedIntent::unresolved("ticket fetch failed: 404");
    let d = front_gate_disposition(&intent, Some(&method("use cursor pagination")));
    assert!(
        d.is_auto_accept(),
        "unresolved must auto-accept (fail-open)"
    );
    let reason = match d {
        Disposition::AutoAccept { reason } => reason.to_string(),
        Disposition::Escalate { .. } => unreachable!(),
    };
    assert!(reason.contains(NO_INTENT_SOURCE));
}

#[test]
fn planned_matching_prescribed_auto_accepts() {
    // M1 agree: planned method matches the ticket method (whitespace/case-insensitive).
    let intent = ticket_intent("use cursor pagination");
    let d = front_gate_disposition(&intent, Some(&method("Use   Cursor  Pagination")));
    assert!(d.is_auto_accept(), "matching method must auto-accept");
}

#[test]
fn absent_plan_auto_accepts() {
    // Pre-work the plan is implicit; an absent planned method is conformant
    // (escalate-only-on-divergence, §5.1).
    let intent = ticket_intent("use cursor pagination");
    let d = front_gate_disposition(&intent, None);
    assert!(d.is_auto_accept(), "absent plan must auto-accept");
}

#[test]
fn divergence_escalates() {
    // M5: planned method contradicts the explicit ticket method.
    let intent = ticket_intent("use cursor-based pagination");
    let d = front_gate_disposition(&intent, Some(&method("use offset pagination")));
    assert!(!d.is_auto_accept(), "divergence must escalate (M5)");
    let reason = match d {
        Disposition::Escalate { reason } => reason.to_string(),
        Disposition::AutoAccept { .. } => unreachable!(),
    };
    assert!(
        reason.contains("cursor"),
        "reason names the prescribed method"
    );
    assert!(reason.contains("offset"), "reason names the planned method");
}

#[test]
fn conflict_follows_ticket() {
    // M4: ticket and spec conflict → ticket wins (precedence applied by ISR).
    // A plan matching the TICKET method auto-accepts; the stale spec is advisory.
    let mut intent = ticket_intent("use cursor pagination");
    intent.spec_section = None;
    intent.spec_method = Some(method("use offset pagination"));
    intent.conflict = true;
    intent.stale_spec = true;
    // precedence_winner stays Ticket (the ISR sets this); prescribed = ticket method.
    let d = front_gate_disposition(&intent, Some(&method("use cursor pagination")));
    assert!(
        d.is_auto_accept(),
        "M4 conforms to the ticket; stale spec advisory"
    );
}

#[test]
fn spec_only_divergence_escalates() {
    // M2/M5: ticket silent, spec prescribes; plan diverges from the spec method.
    let intent = ResolvedIntent {
        ticket: Some(ticket_ref()),
        ticket_method: None,
        spec_section: None,
        spec_method: Some(method("use cursor pagination")),
        precedence_winner: Precedence::Spec,
        conflict: false,
        stale_spec: false,
        unresolved: None,
    };
    let d = front_gate_disposition(&intent, Some(&method("use offset pagination")));
    assert!(!d.is_auto_accept(), "spec divergence must escalate (M2/M5)");
}

// ── stricter_of: composition (AC-16) ────────────────────────────────────────────

#[test]
fn stricter_wins_both_clear() {
    let d = stricter_of(auto("conformance ok"), auto("autonomy ok"));
    assert!(d.is_auto_accept());
}

#[test]
fn stricter_wins_autonomy_escalates() {
    // Conformance would auto-accept, but a T4/destructive autonomy escalation
    // must NOT be lowered (AC-16).
    let d = stricter_of(auto("conformance ok"), esc("T4: destructive"));
    assert!(!d.is_auto_accept(), "tier escalation must not be lowered");
    assert_eq!(d, esc("T4: destructive"));
}

#[test]
fn stricter_wins_conformance_escalates() {
    let d = stricter_of(esc("conformance divergence"), auto("autonomy ok"));
    assert!(!d.is_auto_accept());
    assert_eq!(d, esc("conformance divergence"));
}

#[test]
fn stricter_wins_both_escalate_prefers_autonomy() {
    let d = stricter_of(esc("conformance"), esc("T4"));
    assert_eq!(
        d,
        esc("T4"),
        "tier safety authority cited when both escalate"
    );
}

// ── stricter_of_with_source: escalation-source tagging (#1360 review) ────────────

#[test]
fn source_both_clear_is_neither() {
    let (d, src) = stricter_of_with_source(auto("conformance ok"), auto("autonomy ok"));
    assert!(d.is_auto_accept());
    assert_eq!(src, EscalationSource::Neither);
}

#[test]
fn source_conformance_only_is_conformance() {
    let (d, src) = stricter_of_with_source(esc("conformance divergence"), auto("autonomy ok"));
    assert_eq!(d, esc("conformance divergence"));
    assert_eq!(src, EscalationSource::Conformance);
}

#[test]
fn source_autonomy_only_is_autonomy() {
    let (d, src) = stricter_of_with_source(auto("conformance ok"), esc("T4: destructive"));
    assert_eq!(d, esc("T4: destructive"));
    assert_eq!(src, EscalationSource::Autonomy);
}

#[test]
fn source_both_escalate_is_autonomy() {
    // Autonomy wins outright when both escalate; the source is Autonomy so the
    // (possibly irrelevant) conformance-derived proposed_default is suppressed.
    let (d, src) = stricter_of_with_source(esc("conformance divergence"), esc("T4"));
    assert_eq!(d, esc("T4"), "tier authority wins");
    assert_eq!(src, EscalationSource::Autonomy);
}

// ── run_front_gate: orchestration (AC-13, AC-14, AC-16, AC-17) ──────────────────

fn gate_with(intent: ResolvedIntent) -> MockGate {
    MockGate(intent)
}

#[tokio::test]
async fn run_non_ticketed_auto_accepts() {
    // AC-17: non-ticketed work → AutoAccept (fail-open), gate never runs ISR.
    let gate = gate_with(ResolvedIntent::none());
    let out = run_front_gate(
        &gate,
        "owner",
        "repo",
        "implement the parser with no ticket reference",
        auto("autonomy ok"),
        HeadlessApproval::Headless,
    )
    .await;
    assert!(out.may_spawn(), "non-ticketed work auto-accepts");
    assert!(out.proposed_default.is_none());
}

#[tokio::test]
async fn run_isr_failure_auto_accepts() {
    // AC-17: ISR failure → AutoAccept (fail-open). Task IS ticketed.
    let gate = gate_with(ResolvedIntent::unresolved("ticket fetch failed"));
    let out = run_front_gate(
        &gate,
        "owner",
        "repo",
        "Closes #1360 — use offset pagination",
        auto("autonomy ok"),
        HeadlessApproval::Headless,
    )
    .await;
    assert!(out.may_spawn(), "ISR failure auto-accepts (fail-open)");
}

#[tokio::test]
async fn run_clean_match_auto_accepts() {
    // AC-13: ticket method honoured (no divergent plan) → AutoAccept → spawn.
    let gate = gate_with(ticket_intent("use cursor pagination"));
    let out = run_front_gate(
        &gate,
        "owner",
        "repo",
        "Closes #1360: implement the feature",
        auto("autonomy ok"),
        HeadlessApproval::Headless,
    )
    .await;
    assert!(out.may_spawn(), "clean match auto-accepts (AC-13)");
    assert!(out.escalation_reason().is_none());
}

#[tokio::test]
async fn run_divergence_escalates_and_proposes_default() {
    // AC-14: planned method diverges → Escalate, proposed_default = ticket method.
    let gate = gate_with(ticket_intent("use cursor-based pagination"));
    let out = run_front_gate(
        &gate,
        "owner",
        "repo",
        "Fixes #1360: implement listing.\nuse offset pagination for the listing endpoint",
        auto("autonomy ok"),
        HeadlessApproval::OperatorConfirm,
    )
    .await;
    assert!(!out.may_spawn(), "divergence escalates (AC-14)");
    assert!(out.escalation_reason().is_some());
    assert_eq!(
        out.proposed_default.as_deref(),
        Some("use cursor-based pagination"),
        "proposed_default is the conformant ticket method"
    );
}

#[tokio::test]
async fn run_autonomy_only_escalation_has_no_proposed_default() {
    // #1360 review: conformance is CLEAN (plan matches ticket method) but the
    // autonomy tier escalates (T4). proposed_default must be None — surfacing the
    // conformance method here would mislead the human (reason says "T4", default
    // would say "use cursor pagination", which is unrelated to why we stopped).
    let gate = gate_with(ticket_intent("use cursor pagination"));
    let out = run_front_gate(
        &gate,
        "owner",
        "repo",
        // Plan agrees with the ticket method, so conformance auto-accepts; the
        // autonomy disposition supplied below is the lone escalation source.
        "Closes #1360: implement listing.\nuse cursor pagination",
        esc("T4: irreversible or security-sensitive operation; human confirmation required"),
        HeadlessApproval::OperatorConfirm,
    )
    .await;
    assert!(!out.may_spawn(), "autonomy T4 escalates");
    assert!(
        out.escalation_reason().unwrap().contains("T4"),
        "reason is the autonomy tier reason"
    );
    assert!(
        out.proposed_default.is_none(),
        "autonomy-only escalation must not surface a conformance proposed_default"
    );
}

#[tokio::test]
async fn run_both_escalate_reason_is_autonomy_and_default_not_conformance() {
    // #1360 review: BOTH paths escalate — conformance diverges AND autonomy is T4.
    // The autonomy tier wins (safety authority), so the reason is the autonomy
    // reason and proposed_default is NOT the (now-irrelevant) conformance method.
    let gate = gate_with(ticket_intent("use cursor-based pagination"));
    let out = run_front_gate(
        &gate,
        "owner",
        "repo",
        // Plan diverges from the ticket method → conformance escalates too.
        "Fixes #1360: implement listing.\nuse offset pagination for the endpoint",
        esc("T4: irreversible or security-sensitive operation; human confirmation required"),
        HeadlessApproval::OperatorConfirm,
    )
    .await;
    assert!(!out.may_spawn(), "both escalate");
    assert!(
        out.escalation_reason().unwrap().contains("T4"),
        "autonomy reason wins when both escalate"
    );
    assert_ne!(
        out.proposed_default.as_deref(),
        Some("use cursor-based pagination"),
        "must not surface the conformance method when autonomy is the escalation source"
    );
    assert!(
        out.proposed_default.is_none(),
        "autonomy-sourced escalation carries no proposed_default"
    );
}

#[tokio::test]
async fn run_stricter_wins_autonomy_escalation_survives_conformance_auto_accept() {
    // AC-16: conformance would auto-accept (clean match) but autonomy escalates
    // (T4); the composed outcome must escalate.
    let gate = gate_with(ticket_intent("use cursor pagination"));
    let out = run_front_gate(
        &gate,
        "owner",
        "repo",
        "Closes #1360: implement the feature",
        esc("T4: irreversible or security-sensitive operation"),
        HeadlessApproval::Headless,
    )
    .await;
    assert!(
        !out.may_spawn(),
        "tier escalation survives conformance auto-accept"
    );
    assert!(out.escalation_reason().unwrap().contains("T4"));
}

#[tokio::test]
async fn degraded_branch_marks_operator_confirm() {
    // #1269 degraded path: the outcome carries the operator-confirm approval mode
    // so the spawn path withholds the spawn and surfaces pending_decision via CLI.
    let gate = gate_with(ticket_intent("use cursor-based pagination"));
    let out = run_front_gate(
        &gate,
        "owner",
        "repo",
        "Fixes #1360: implement listing.\nuse offset pagination",
        auto("autonomy ok"),
        HeadlessApproval::OperatorConfirm,
    )
    .await;
    assert_eq!(out.approval, HeadlessApproval::OperatorConfirm);
    assert!(
        !out.may_spawn(),
        "escalation in degraded mode still withholds spawn"
    );
}

#[test]
fn current_capability_is_degraded() {
    // Until #1269 lands, the production path runs operator-confirm.
    assert_eq!(
        HeadlessApproval::current(),
        HeadlessApproval::OperatorConfirm
    );
}

// ── autonomy_disposition: first production caller of evaluate_autonomy_tier ──────

fn correlated() -> SessionCorrelation {
    SessionCorrelation::new()
        .with_worktree("/repo/wt")
        .with_issue_id(1360)
}

fn ctx(decision: &str, class: ChangeClass) -> ActionContext {
    ActionContext {
        pending_decision: decision.to_string(),
        change_class: class,
        correlation: correlated(),
        prior_rejections: 0,
    }
}

/// Pre-work guardrails are unknown/unavailable (the work isn't done yet).
fn prework_signals() -> GuardrailSignals {
    GuardrailSignals {
        review: ReviewVerdict::Unavailable,
        ci: CiStatus::Unknown,
        search_consistent: true,
        memory_consistent: true,
        scope: ScopeCheck::InScope,
    }
}

#[test]
fn autonomy_disposition_destructive_escalates() {
    // The engine's FIRST production caller: a destructive task escalates (T4).
    let c = ctx(
        "decommission the production index",
        ChangeClass::Destructive,
    );
    let d = autonomy_disposition(&c, &prework_signals());
    assert!(
        !d.is_auto_accept(),
        "destructive work escalates via the tier engine"
    );
}

#[test]
fn autonomy_disposition_style_only_auto_accepts() {
    // A T1 style-only change with no objecting guardrail auto-accepts.
    let c = ctx("apply rustfmt", ChangeClass::StyleOnly);
    let d = autonomy_disposition(&c, &prework_signals());
    assert!(d.is_auto_accept(), "T1 style-only auto-accepts pre-work");
}

#[test]
fn autonomy_disposition_empty_decision_fails_open() {
    // An empty decision cannot be classified → fail-open AutoAccept (never block
    // the gate on its own composition input).
    let c = ctx("   ", ChangeClass::Standard);
    let d = autonomy_disposition(&c, &prework_signals());
    assert!(d.is_auto_accept(), "empty decision fails open");
}

#[test]
fn prework_autonomy_ordinary_task_auto_accepts() {
    // An ordinary ticketed task auto-accepts pre-work (no destructive op).
    let d = prework_autonomy_disposition("Closes #1360: implement the listing endpoint");
    assert!(d.is_auto_accept(), "ordinary pre-work task auto-accepts");
}

#[test]
fn prework_autonomy_destructive_task_escalates() {
    // A destructive task escalates pre-spawn (T4), independent of conformance.
    let d = prework_autonomy_disposition("Closes #1360: drop table sessions to reset state");
    assert!(
        !d.is_auto_accept(),
        "destructive pre-work task escalates (T4)"
    );
}

#[test]
fn prework_autonomy_empty_task_fails_open() {
    let d = prework_autonomy_disposition("   ");
    assert!(d.is_auto_accept(), "empty task fails open");
}

#[test]
fn prescribed_method_follows_precedence() {
    let mut intent = ticket_intent("use cursor pagination");
    intent.spec_method = Some(method("use offset pagination"));
    // Ticket wins → prescribed is the ticket method.
    let d = front_gate_disposition(&intent, Some(&method("use offset pagination")));
    assert!(
        !d.is_auto_accept(),
        "diverges from ticket (the precedence winner)"
    );
}
