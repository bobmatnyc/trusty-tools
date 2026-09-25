//! Tests for the reclaim verdict's decision line (#8109).
//!
//! Why: the operator greps the rendered sentence, so the assertions are on the
//! sentence rather than on the variant.

use super::*;

/// 🔴 #8109: a refusal's decision line names the gate that refused and why —
/// for an agent-held tree as much as for any other.
#[test]
fn a_decision_line_names_the_gate_and_the_reason() {
    let blocked = ReclaimVerdict::blocked(ReclaimGate::PrState, "no pull request for this branch");
    assert_eq!(
        blocked.decision(),
        "refused at gate 5 (pull-request state) — no pull request for this branch"
    );
    let held = ReclaimVerdict::blocked_by_agent(ReclaimGate::AgentOwnership, "agent a1 holds it");
    assert_eq!(
        held.decision(),
        "refused at gate 4 (agent ownership) — agent a1 holds it"
    );
}

/// 🔴 #8109: a grant's decision line names its landing evidence, so a reclaim
/// is as auditable as a refusal — and a landed-content grant never reads as a
/// pull request.
#[test]
fn a_decision_line_for_a_grant_names_its_landing_evidence() {
    assert_eq!(
        ReclaimVerdict::Reclaimable { pr: 8109 }.decision(),
        "reclaimable — landing evidence is PR #8109"
    );
    let landed = ReclaimVerdict::ReclaimableLandedContent {
        base: "origin/main".to_string(),
    }
    .decision();
    assert!(landed.contains("already on origin/main"), "{landed}");
    assert!(!landed.contains("PR #"), "{landed}");
}
