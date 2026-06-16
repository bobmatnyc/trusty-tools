//! The verification gate helpers + the prohibition (SP1–SP7) guard (§3.2, §3.5).
//!
//! Why: two SM invariants live here so they are auditable in one place. (1) The
//! VERIFICATION GATE (§3.5): a goal cannot be claimed `Done` without observed
//! evidence on every linked task — this is enforced in the goal store
//! ([`SmGoalStore::close`](crate::core::sm::SmGoalStore)), and the loop feeds it
//! evidence captured by [`super::observe`]; this module documents that wiring and
//! provides the redirect text. (2) The PROHIBITION GUARD (§3.2): the SM must never
//! do real work itself. Structurally the loop has NO edit/read/build/test tools —
//! the only actions are session control + goals + memory — so SP1–SP5 are enforced
//! by CONSTRUCTION. On top of that, if the model's decision is a direct-work
//! ATTEMPT ([`SmDecision::DoWork`](super::SmDecision)), the engine refuses it and
//! redirects to "launch a session" via [`redirect_direct_work`].
//! What: [`DIRECT_WORK_REDIRECT`] (the canonical redirect preamble) and
//! [`redirect_direct_work`] (build the operator-facing refusal + redirect).
//! Test: `verify_tests.rs` covers the redirect text; the gate itself is covered
//! by the goal-store tests + `delegate_tests.rs::delegate_gate_blocks_without_evidence`.

/// The canonical preamble for refusing a direct-work attempt (§3.2 SP1–SP5).
///
/// Why: the SM has "no hands of its own"; when it attempts to do work directly the
/// engine must give a CONSISTENT, operator-facing refusal that names the rule and
/// the required behaviour (launch a session). A single constant keeps that wording
/// stable across call sites and tests.
/// What: the fixed refusal/redirect preamble string.
/// Test: `verify_tests.rs::redirect_mentions_prohibition_and_launch`.
pub const DIRECT_WORK_REDIRECT: &str = "The session manager does not do work itself (prohibitions SP1-SP5): it \
     delegates every unit of work to a launched session. Rather than doing this \
     directly, launch a session scoped to the task";

/// Build the operator-facing refusal + redirect for a direct-work attempt.
///
/// Why: the SP guard — when the model decides to do the work itself instead of
/// delegating, the loop REFUSES and redirects. Surfacing the attempted work in the
/// message makes the redirect actionable ("you tried to X; instead launch a
/// session to X") and auditable (it is also recorded as a goal note by the caller).
/// What: returns [`DIRECT_WORK_REDIRECT`] followed by the attempted-work `summary`
/// (when non-blank), as a single operator-facing sentence.
/// Test: `verify_tests.rs::redirect_includes_attempted_work`,
/// `redirect_handles_empty_summary`.
pub fn redirect_direct_work(summary: &str) -> String {
    let summary = summary.trim();
    if summary.is_empty() {
        format!("{DIRECT_WORK_REDIRECT}.")
    } else {
        format!("{DIRECT_WORK_REDIRECT}: {summary}.")
    }
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod verify_tests;
