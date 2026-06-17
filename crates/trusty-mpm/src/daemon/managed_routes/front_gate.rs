//! Pre-spawn intent/method-conformance FRONT gate (DOC-15 C3, #1360).
//!
//! # Spec References
//!
//! - [`SPEC-CONFORMANCE-02~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-02~draft) (§5.1 FRONT gate)
//! - [`SPEC-CONFORMANCE-01~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-01~draft) (§4 decision matrix)
//!
//! Why: a managed session begins *coding a ticket* the moment
//! [`crate::daemon::managed_routes::spawn_managed`] calls `adapter.spawn`. The
//! FRONT gate is the only seam that owns BOTH the task text (the intent) and the
//! spawn trigger, so it is where we can check the planned method against the
//! ticket+spec intent *before* any code is written — when redirection is
//! cheapest (spec §5.1 Rationale). The gate realises the ~80% auto / ~20%
//! escalate operating model: the default is auto-proceed; escalation is the
//! exception, reserved for a genuine method divergence (spec G2).
//!
//! What: this module is the pure, testable core of the gate. It exposes
//! - [`ConformanceGate`] — the seam over the shared ISR
//!   (`trusty_common::intent_source`), so the spawn path can resolve a
//!   `ResolvedIntent` without coupling to a live GitHub client (mocked in tests);
//! - [`HeadlessApproval`] — the #1269 capability flag that selects the headless
//!   auto-spawn path vs. the degraded operator-confirm path;
//! - [`front_gate_disposition`] — the pure matrix→`Disposition` mapping, composed
//!   STRICTER-WINS with `evaluate_autonomy_tier` so conformance can never *lower*
//!   a tier escalation (spec §5.1, AC-16);
//! - [`run_front_gate`] — the async orchestration the spawn path calls.
//!
//! Fail-open everywhere (spec §4.2, AC-17): non-ticketed work, an unresolved
//! ISR, or an empty planned/prescribed method all yield `AutoAccept` — the gate
//! must NEVER be the reason work stalls.
//!
//! Test: `super::front_gate::tests` covers the matrix rows (agree/gap/divergence),
//! fail-open (non-ticketed + ISR failure), the stricter-wins composition, and the
//! #1269 degraded branch — all without network access (AC-13..AC-17).

use async_trait::async_trait;

use trusty_common::intent_source::backend_fetcher::{BackendTicketFetcher, FsSpecLookup};
use trusty_common::intent_source::{
    EnvTokenResolver, IntentQuery, Method, Precedence, ResolvedIntent, extract_ticket_id,
    heuristic_method,
};

use crate::driver::correlation::{ScopeCheck, SessionCorrelation};
use crate::driver::policy::{
    ActionContext, AutonomyDecision, ChangeClass, CiStatus, Disposition, GuardrailSignals,
    ReviewVerdict, evaluate_autonomy_tier,
};

/// The fail-open reason used when the gate has no intent to conform to.
///
/// Why: non-ticketed work, an unresolved ISR, and a clean gap all collapse to
/// the same outcome — auto-proceed — and the spec fixes the reason text
/// ("no intent source", §5.1). Centralising the literal keeps every call site
/// and every test asserting on one string.
/// What: the canonical `AutoAccept` reason for the fail-open / no-intent path.
/// Test: `tests::non_ticketed_auto_accepts`, `tests::isr_failure_auto_accepts`.
pub const NO_INTENT_SOURCE: &str = "no intent source";

/// Whether the fully-headless auto-spawn approval path is available (#1269).
///
/// Why: spec §5.1 — the fully-headless auto-spawn path is **soft-blocked by
/// #1269**. The gate must still land and function: on an escalation it degrades
/// to the CLI-owned launch / operator-confirm path rather than blocking the
/// whole gate on #1269. Modelling the capability as an explicit two-state value
/// (rather than a bare `bool`) keeps the degradation branch self-documenting at
/// every call site.
/// What: `Headless` — the headless approval channel is live (post-#1269);
/// `OperatorConfirm` — degrade an escalation to operator confirmation (the
/// `pending_decision` is written and surfaced through the CLI/activity channels,
/// and a human resolves it via `POST …/answer`).
/// Test: `tests::degraded_branch_marks_operator_confirm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadlessApproval {
    /// The headless auto-spawn approval path is available (post-#1269).
    Headless,
    /// Degraded path: escalation requires operator confirmation (pre-#1269).
    OperatorConfirm,
}

impl HeadlessApproval {
    /// The capability the gate runs under until #1269 lands.
    ///
    /// Why: #1269 soft-blocks the headless auto-spawn path; until it merges, the
    /// production spawn path must use the degraded operator-confirm branch so the
    /// gate lands and functions today (spec §5.1 Rationale, scope item 6).
    /// What: returns [`HeadlessApproval::OperatorConfirm`].
    /// Test: `tests::current_capability_is_degraded`.
    #[must_use]
    pub fn current() -> Self {
        // #1269 not yet landed: degrade to operator-confirm.
        HeadlessApproval::OperatorConfirm
    }
}

/// The outcome of running the FRONT gate against a spawn.
///
/// Why: the spawn path needs more than a yes/no — on an escalation it must write
/// the `pending_decision`/`proposed_default` and withhold the spawn, and it must
/// know whether it is on the headless or the degraded operator-confirm path so it
/// can log/surface accordingly (spec §5.1).
/// What: carries the composed [`Disposition`], plus — when the disposition is
/// `Escalate` — the `proposed_default` (the conformant method to surface to the
/// human) and the approval mode in force.
/// Test: `tests::*` assert on the disposition and the escalation fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontGateOutcome {
    /// The composed disposition (stricter of conformance vs. autonomy).
    pub disposition: Disposition,
    /// The conformant method to propose to the human, when escalating.
    pub proposed_default: Option<String>,
    /// Which approval path is in force (headless vs. degraded operator-confirm).
    pub approval: HeadlessApproval,
}

impl FrontGateOutcome {
    /// Whether the gate cleared and the spawn may proceed.
    ///
    /// Why: the spawn path branches on this to call `adapter.spawn` or withhold
    /// it; a helper keeps the call site declarative.
    /// What: returns `true` when the composed disposition is `AutoAccept`.
    /// Test: asserted across `tests::*`.
    #[must_use]
    pub fn may_spawn(&self) -> bool {
        self.disposition.is_auto_accept()
    }

    /// The escalation reason text, when the gate escalated.
    ///
    /// Why: the spawn path writes this into `SessionRecord.pending_decision` so it
    /// surfaces through every existing channel (activity API, CLI, supervisor).
    /// What: returns the `Escalate` reason, or `None` on auto-accept.
    /// Test: `tests::divergence_escalates`.
    #[must_use]
    pub fn escalation_reason(&self) -> Option<&str> {
        match &self.disposition {
            Disposition::Escalate { reason } => Some(reason),
            Disposition::AutoAccept { .. } => None,
        }
    }
}

/// Seam over the shared intent-source resolver (ISR).
///
/// Why: `run_front_gate` must resolve a `ResolvedIntent` from a ticket-id, but
/// the production resolver fetches a GitHub issue over the network. Modelling the
/// resolution as a trait lets the spawn path run the real ISR while unit tests
/// inject a deterministic mock — no network, fully offline (spec §5.1 inputs,
/// CLAUDE.md no-network-in-tests).
/// What: one async method mapping `(ticket_id, owner, repo, task)` to a
/// `ResolvedIntent`. Implementors MUST be fail-open: any failure resolves to
/// [`ResolvedIntent::unresolved`], never an `Err` (the trait is infallible by
/// design so the gate can never block on resolver failure — spec §4.2).
/// Test: `tests` use a `MockGate`; the production impl is [`IsrConformanceGate`].
#[async_trait]
pub trait ConformanceGate: Send + Sync {
    /// Resolve the intent for a ticketed task.
    ///
    /// Why: the FRONT gate compares the *planned method* (derived from `task`)
    /// against the ticket/spec method the ISR resolves (spec §5.1).
    /// What: returns a `ResolvedIntent`; fail-open (`unresolved`) on any failure.
    /// Test: `tests` via `MockGate`.
    async fn resolve(&self, ticket_id: &str, owner: &str, repo: &str, task: &str)
    -> ResolvedIntent;
}

/// Production [`ConformanceGate`] backed by the shared ISR.
///
/// Why: the real gate must reuse `trusty_common::intent_source` — the single
/// shared resolver both conformance gates call (spec G4) — rather than
/// re-implement ticket fetch + precedence. This wraps it behind the
/// `BackendTicketFetcher` (PAT/App-token via the pluggable resolver) and a
/// filesystem spec lookup rooted at the session's workspace.
/// What: holds the repo root used for SLD spec resolution; `resolve` builds an
/// `IntentQuery::Ticket` (pre-work `changed_files` is empty, so the spec axis is
/// keyed off the ticket body's links only — spec §6.1) and calls
/// `resolve_default`, which is itself fail-open.
/// Test: covered by `tests::production_gate_*` (offline: a non-fetchable ticket
/// resolves to `unresolved`, the fail-open contract — no network is exercised).
pub struct IsrConformanceGate {
    repo_root: std::path::PathBuf,
}

impl IsrConformanceGate {
    /// Construct a production gate rooted at `repo_root` for spec resolution.
    ///
    /// Why: SLD spec refs are repo-relative (`docs/specs/…`); the loader needs
    /// the checkout root to resolve them (spec §6.4).
    /// What: stores the root used to build the `FsSpecLookup`.
    /// Test: `tests::production_gate_*`.
    #[must_use]
    pub fn new(repo_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }
}

#[async_trait]
impl ConformanceGate for IsrConformanceGate {
    async fn resolve(
        &self,
        ticket_id: &str,
        owner: &str,
        repo: &str,
        _task: &str,
    ) -> ResolvedIntent {
        let fetcher: BackendTicketFetcher = BackendTicketFetcher::new(Box::new(EnvTokenResolver));
        let spec_lookup = FsSpecLookup::new(self.repo_root.clone());
        let query = IntentQuery::Ticket {
            ticket_id: ticket_id.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            changed_files: Vec::new(),
        };
        // `resolve_default` is itself fail-open — it returns `unresolved` on any
        // fetch/parse failure and never `Err`s (spec §4.2).
        trusty_common::intent_source::resolve_default(query, &fetcher, &spec_lookup).await
    }
}

/// The method the resolved intent prescribes, under precedence (ticket > spec).
///
/// Why: the FRONT gate compares the planned method against the *winning* method,
/// not against both axes — precedence has already been applied centrally by the
/// ISR (spec §6.1). A small accessor keeps [`front_gate_disposition`] declarative.
/// What: returns the ticket method when the ticket won, the spec method when the
/// spec won, and `None` for a gap.
/// Test: `tests::prescribed_method_follows_precedence`.
fn prescribed_method(intent: &ResolvedIntent) -> Option<&Method> {
    match intent.precedence_winner {
        Precedence::Ticket => intent.ticket_method.as_ref(),
        Precedence::Spec => intent.spec_method.as_ref(),
        Precedence::None => None,
    }
}

/// Lowercase + collapse whitespace, for method-text comparison.
///
/// Why: trivial formatting differences between the planned method (from the task
/// prose) and the prescribed method (from the ticket/spec) must not register as a
/// divergence — that would manufacture a false escalation (spec §4.3 fail-open
/// bias). Mirrors the ISR's own `normalise` so FRONT and the resolver agree on
/// what "the same method" means.
/// What: lowercases, splits on whitespace, and rejoins single-spaced.
/// Test: `tests::planned_matching_prescribed_auto_accepts`.
fn normalise(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map the resolved intent + planned method onto a conformance `Disposition`.
///
/// Why: this is the FRONT half of the normative decision matrix (spec §4.1),
/// pre-work. It is a pure function of `(intent, planned_method)` so it is fully
/// unit-testable offline.
/// What: implements the matrix rows for FRONT —
/// - `unresolved` / non-ticketed / gap (no prescribed method) → `AutoAccept`
///   (M3, fail-open §4.2);
/// - a prescribed method exists and the planned method is absent OR agrees with
///   it → `AutoAccept` (M1/M2 agree). Pre-work the plan is implicit, so an absent
///   planned method is treated as conformant (escalate-only-on-divergence, §5.1);
/// - a prescribed method exists and the planned method *diverges* from it →
///   `Escalate` (M1/M2/M5 divergence). The reason names both methods so a human
///   can triage instantly (`first_failing_signal` style, §5.1).
///
/// Note (M4): a ticket/spec **conflict** is NOT an escalation here — the ISR has
/// already applied ticket > spec, so the prescribed method is the *ticket*
/// method and the gate conforms to it; the stale spec is advisory only (spec
/// §4.1 M4, §5.1). The conflict therefore only matters insofar as the planned
/// method diverges from the *ticket* method.
/// Test: `tests::gap_auto_accepts`, `tests::planned_matching_prescribed_auto_accepts`,
/// `tests::divergence_escalates`, `tests::conflict_follows_ticket`.
#[must_use]
pub fn front_gate_disposition(
    intent: &ResolvedIntent,
    planned_method: Option<&Method>,
) -> Disposition {
    // Fail-open: ISR could not resolve → no intent to conform to (spec §4.2).
    if let Some(reason) = &intent.unresolved {
        return Disposition::AutoAccept {
            reason: format!("{NO_INTENT_SOURCE} (unresolved: {reason})"),
        };
    }

    let Some(prescribed) = prescribed_method(intent) else {
        // M3 gap: neither ticket nor spec prescribes a method — nothing to
        // conform to.
        return Disposition::AutoAccept {
            reason: format!("{NO_INTENT_SOURCE} (no prescribed method; gap)"),
        };
    };

    match planned_method {
        // M1/M2 agree (or no explicit plan pre-work → treat as conformant): the
        // planned approach matches the prescribed method.
        None => Disposition::AutoAccept {
            reason: "conformance: no divergent plan; ticket/spec method honoured".to_string(),
        },
        Some(planned) if normalise(&planned.text) == normalise(&prescribed.text) => {
            Disposition::AutoAccept {
                reason: "conformance: planned method matches the ticket/spec method".to_string(),
            }
        }
        // M1/M2/M5 divergence: the planned method contradicts an explicit
        // ticket/spec method — escalate before any code is written (spec §5.1).
        Some(planned) => Disposition::Escalate {
            reason: format!(
                "conformance divergence: ticket/spec specifies \"{}\"; plan uses \"{}\"",
                prescribed.text.trim(),
                planned.text.trim()
            ),
        },
    }
}

/// Which input won the stricter-wins composition (the escalation source).
///
/// Why: the FRONT gate's `proposed_default` is only meaningful when the
/// *conformance* path is what escalated — it names the conformant method to
/// adopt. When the *autonomy* tier is the escalation source (e.g. a T4
/// destructive task), surfacing the conformance-derived method is misleading: the
/// reason reads "T4: destructive" while `proposed_default` says "use cursor
/// pagination", which has nothing to do with why we stopped (#1360 review). The
/// composition must therefore report which side won so the caller can scope
/// `proposed_default` to the conformance case only.
/// What: `Conformance` — the conformance disposition is the (sole) escalation
/// source; `Autonomy` — the autonomy tier escalated (it wins outright when both
/// escalate, since the tier engine is the authoritative safety floor); `Neither`
/// — both auto-accepted, no escalation.
/// Test: `tests::stricter_wins_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationSource {
    /// Both inputs auto-accepted; the composition did not escalate.
    Neither,
    /// The conformance disposition is the escalation source.
    Conformance,
    /// The autonomy tier is the escalation source (wins when both escalate).
    Autonomy,
}

/// Compose two dispositions STRICTER-WINS, reporting the escalation source.
///
/// Why: spec §5.1 / AC-16 — the conformance disposition is *combined* with the
/// standard `evaluate_autonomy_tier` decision by taking the stricter outcome. The
/// gate must NEVER *lower* a tier escalation to auto-accept (a T4/destructive
/// action still escalates regardless of conformance). Equally, a conformance
/// divergence must escalate even when the autonomy tier would auto-accept.
/// Beyond the disposition, the caller needs to know WHICH side escalated so it can
/// scope `proposed_default` to the conformance-only case (#1360 review): a
/// `proposed_default` derived from conformance is meaningless when the autonomy
/// tier is the reason we stopped.
/// What: returns the stricter `Disposition` plus an [`EscalationSource`] tag —
/// `Escalate` if EITHER input escalates (preferring the autonomy reason when both
/// escalate, since the tier engine is the established safety authority, and
/// tagging the source `Autonomy` in that case); `AutoAccept` (tagged `Neither`)
/// only when BOTH auto-accept.
/// Test: `tests::stricter_wins_*`, `tests::run_*_proposed_default_*`.
#[must_use]
pub fn stricter_of_with_source(
    conformance: Disposition,
    autonomy: Disposition,
) -> (Disposition, EscalationSource) {
    match (&conformance, &autonomy) {
        // Both clear → auto-accept (cite conformance, the gate's own reason).
        (Disposition::AutoAccept { .. }, Disposition::AutoAccept { .. }) => {
            (conformance, EscalationSource::Neither)
        }
        // Autonomy escalates → autonomy wins (tier safety is authoritative; it
        // must never be lowered by conformance). It wins even when conformance
        // ALSO escalates, so the source is `Autonomy` in both sub-cases.
        (_, Disposition::Escalate { .. }) => (autonomy, EscalationSource::Autonomy),
        // Only conformance escalates → conformance wins.
        (Disposition::Escalate { .. }, _) => (conformance, EscalationSource::Conformance),
    }
}

/// Compose two dispositions STRICTER-WINS (escalate beats auto-accept).
///
/// Why: the disposition-only projection of [`stricter_of_with_source`], for call
/// sites and tests that do not need the escalation-source tag.
/// What: returns the stricter `Disposition`, discarding the source tag.
/// Test: `tests::stricter_wins_*`.
#[must_use]
pub fn stricter_of(conformance: Disposition, autonomy: Disposition) -> Disposition {
    stricter_of_with_source(conformance, autonomy).0
}

/// Run the FRONT gate for a spawn, producing a [`FrontGateOutcome`].
///
/// Why: the single entry point [`crate::daemon::managed_routes::spawn_managed`]
/// calls between record-creation and `adapter.spawn`. It ties together ticket
/// extraction, ISR resolution, the conformance disposition, and the stricter-wins
/// composition with autonomy — and selects the headless vs. degraded path.
/// What: in order — (1) extract a ticket-id from `task`; non-ticketed →
/// `AutoAccept` fail-open (AC-17); (2) resolve the intent via `gate` (fail-open);
/// (3) derive the planned method from `task` (heuristic, no network); (4) map to a
/// conformance `Disposition` ([`front_gate_disposition`]); (5) compose
/// stricter-wins with `autonomy` ([`stricter_of`]); (6) wrap with the
/// `proposed_default` (the prescribed method) and the `approval` mode. The gate
/// is infallible by design (every failure is fail-open) so it can never block.
/// Test: `tests::run_*` (AC-13, AC-14, AC-16, AC-17).
pub async fn run_front_gate(
    gate: &dyn ConformanceGate,
    owner: &str,
    repo: &str,
    task: &str,
    autonomy: Disposition,
    approval: HeadlessApproval,
) -> FrontGateOutcome {
    // (1) Extract a ticket-id from the task prose. Non-ticketed → fail-open.
    let Some(ticket_id) = extract_ticket_id(task) else {
        let disposition = stricter_of(
            Disposition::AutoAccept {
                reason: format!("{NO_INTENT_SOURCE} (non-ticketed work)"),
            },
            autonomy,
        );
        return FrontGateOutcome {
            disposition,
            proposed_default: None,
            approval,
        };
    };

    // (2) Resolve intent (fail-open by the trait contract).
    let intent = gate.resolve(&ticket_id, owner, repo, task).await;

    // (3) Derive the planned method from the task prose (heuristic, offline).
    let planned = heuristic_method(task);

    // (4) Map to a conformance disposition; (5) compose stricter-wins, keeping the
    // escalation source so we can scope `proposed_default` correctly.
    let conformance = front_gate_disposition(&intent, planned.as_ref());
    let prescribed = prescribed_method(&intent).map(|m| m.text.trim().to_string());
    let (disposition, source) = stricter_of_with_source(conformance, autonomy);

    // `proposed_default` carries the conformant method to adopt — it is ONLY
    // meaningful when the CONFORMANCE path is the escalation source. When the
    // autonomy tier escalates (e.g. T4 destructive), the conformance-derived
    // method is unrelated to why we stopped, so surfacing it would mislead the
    // human (the reason would say "T4: destructive" while the default says "use
    // cursor pagination"). Scope it to `Conformance` only; `None` otherwise
    // (#1360 review).
    let proposed_default = match source {
        EscalationSource::Conformance => prescribed,
        EscalationSource::Autonomy | EscalationSource::Neither => None,
    };

    FrontGateOutcome {
        disposition,
        proposed_default,
        approval,
    }
}

/// Build the autonomy [`Disposition`] the FRONT gate composes against.
///
/// Why: the gate must compose with `evaluate_autonomy_tier` (spec §5.1), but
/// pre-work most guardrail signals (review, CI) are not yet available — the work
/// has not been done. The honest pre-work signal set is "unknown", which the
/// autonomy engine already treats conservatively. This helper builds an
/// `ActionContext`/`GuardrailSignals` pair from what IS known pre-work (the task
/// text as the decision, the caller-supplied change class) and runs the engine,
/// so the FRONT gate gives `evaluate_autonomy_tier` its FIRST production caller
/// (spec §2.2 — the engine was built+tested with zero callers).
/// What: runs `evaluate_autonomy_tier` with an unavailable/unknown guardrail set
/// and the supplied [`ActionContext`]; maps the inevitable `EmptyDecision` error
/// (empty task) to a fail-open `AutoAccept` so the gate never blocks on its own
/// composition step.
/// Test: `tests::autonomy_disposition_*`.
#[must_use]
pub fn autonomy_disposition(ctx: &ActionContext, signals: &GuardrailSignals) -> Disposition {
    match evaluate_autonomy_tier(ctx, signals) {
        Ok(decision) => decision_into_disposition(decision),
        // An empty decision text cannot be classified; fail open rather than
        // block the gate on its own composition input (spec §4.2).
        Err(_) => Disposition::AutoAccept {
            reason: format!("{NO_INTENT_SOURCE} (autonomy input empty)"),
        },
    }
}

/// Build the PRE-WORK autonomy disposition for a task about to be spawned.
///
/// Why: the FRONT gate runs *before* any code exists, so the post-work guardrails
/// (`review`, `ci`) are not yet meaningful — the work has not been done. The only
/// autonomy dimension that applies pre-spawn is the tier/destructive classifier:
/// a task that names an irreversible operation (`delete`, `drop table`,
/// `decommission`, …) must escalate before the agent is even launched (T4),
/// while ordinary work proceeds. Feeding the engine `StyleOnly` + a pre-work
/// signal set (review `Unavailable`, CI `Unknown` — NOT `Reject`/`Red`) yields
/// exactly that: auto-accept normally, escalate on destructive text. This is the
/// engine's FIRST production caller (spec §2.2).
/// What: constructs an [`ActionContext`] from the task text (the `pending_decision`
/// the destructive-keyword scan reads) under `ChangeClass::StyleOnly`, plus a
/// neutral pre-work [`GuardrailSignals`], and runs `evaluate_autonomy_tier`. Maps
/// the empty-task error to a fail-open `AutoAccept`.
///
/// Why `ChangeClass::StyleOnly` is the safe pre-work default: the destructive
/// scan is NOT gated by the change class. `classify_tier` first derives a base
/// tier from the class (StyleOnly → T1) and then *unconditionally* bumps to T4 if
/// `ActionContext::mentions_destructive_op()` matches a destructive keyword —
/// `if ctx.mentions_destructive_op() { base.max(T4) }` runs independently of
/// `change_class`. So picking the *lowest* base tier (StyleOnly/T1) cannot
/// suppress a destructive escalation: a "drop table" task is forced to T4
/// regardless. This is exactly the pre-work intent — ordinary work auto-accepts
/// (T1), destructive text escalates (T4). The passing test
/// `tests::prework_autonomy_destructive_task_escalates` ("drop table sessions …")
/// pins this behaviour; if the keyword scan were ever gated on `change_class`,
/// switch this default to `ChangeClass::Standard` as the conservative floor.
/// Test: `tests::prework_autonomy_*` (esp. `prework_autonomy_destructive_task_escalates`).
#[must_use]
pub fn prework_autonomy_disposition(task: &str) -> Disposition {
    let ctx = ActionContext {
        pending_decision: task.to_string(),
        // Pre-spawn we cannot classify the diff; the destructive-keyword scan in
        // `evaluate_autonomy_tier` (`classify_tier`) forces T4 on irreversible
        // task text INDEPENDENT of this class, so StyleOnly is the safe default —
        // it sets the lowest base tier without suppressing destructive escalation.
        change_class: ChangeClass::StyleOnly,
        correlation: SessionCorrelation::new(),
        prior_rejections: 0,
    };
    let signals = GuardrailSignals {
        // Pre-work: not yet reviewed / no CI run. `Unavailable`/`Unknown` are NOT
        // `Reject`/`Red`, so a T1 task auto-accepts; a destructive task is forced
        // to T4 and escalates regardless.
        review: ReviewVerdict::Unavailable,
        ci: CiStatus::Unknown,
        search_consistent: true,
        memory_consistent: true,
        scope: ScopeCheck::InScope,
    };
    autonomy_disposition(&ctx, &signals)
}

/// Project an [`AutonomyDecision`] onto its [`Disposition`].
///
/// Why: [`stricter_of`] composes two `Disposition`s; the autonomy engine returns
/// the richer `AutonomyDecision` (tier + disposition). This narrows it to the
/// disposition the composition needs.
/// What: returns `decision.disposition`.
/// Test: `tests::autonomy_disposition_*`.
fn decision_into_disposition(decision: AutonomyDecision) -> Disposition {
    decision.disposition
}

#[cfg(test)]
mod tests;
