//! Delegation authority as a function of agent KIND (ADR-0024).
//!
//! The governing decision is ADR-0024, "Assistants Are Level-0 Delegators;
//! Sub-Agents Are In-Process, Single-Edge Leaves That Never Delegate" — cited
//! by number AND title because it is not on `main` yet (it lands with PR
//! #4243; it was drafted as ADR-0023 and renumbered when the worktree-
//! authority ADR took that slot, so an older draft reference to "ADR-0023"
//! elsewhere means a DIFFERENT document).
//!
//! Why: the shipped delegation gate encoded its rule against TIER
//! (`delegate.rs`: `target_tier == L0Orchestration && delegator_tier !=
//! L0Orchestration`), which is a PROXY for kind. It worked only while tier
//! happened to coincide with kind — every assistant was `L1Standard` and `L0`
//! was an empty tier, so "assistant delegating to assistant" and "L1
//! delegating to L1" were the same event and no L0-vs-L0 edge could exist.
//! ADR-0024 decision 3 breaks that coincidence deliberately (every assistant
//! becomes L0), and a total order over tiers is structurally incapable of
//! forbidding an edge WITHIN a rank, for ANY numbering — so no renumbering
//! fixes it and any future check leaning on tier values reintroduces the same
//! defect under new labels. This module holds the rule expressed against the
//! attribute it is actually about.
//! What: [`is_assistant_kind`] and [`kind_refuses_delegation`] — the ONE
//! definition of the kind predicate in this crate. Both the enforcement point
//! (`tools::delegate::DelegateToAgentTool::execute`) and the reporting surface
//! that must agree with it (`api::server::agent_subagents::in_product_surface`)
//! call these rather than re-deriving the comparison, per the crate's "no
//! second copy of any gate" principle: a route that advertises a target the
//! gate refuses is the same class of drift as a gate a call site forgot.
//! Test: `assistant_kind_is_read_from_role_not_tier`,
//! `kind_refuses_assistant_to_assistant`,
//! `kind_permits_assistant_to_sub_agent`,
//! `kind_ignores_non_assistant_sources`; the behavioral coverage lives in
//! `tools::delegate`'s tests (`delegate_assistant_to_assistant_is_refused*`).

use crate::runtime::tool_registry::ASSISTANT_TIER_ROLE;

/// Is `role` the assistant KIND?
///
/// Why: ADR-0024 predicate 1 fixes `KIND` as the EXISTING `agent.role` field —
/// specifically `role == ASSISTANT_TIER_ROLE` — and not a new attribute.
/// Deliberately NOT `AgentInfo::kind`: that field is picker-grouping metadata
/// whose own doc comment forbids exactly this use ("Conflating any of the
/// three would silently change dispatch/security behavior as a side effect of
/// a display change"), and it defaults to `"assistant"` for every agent
/// including workers, so it cannot discriminate anything. `role` is already
/// the load-bearing security discriminator on this path — it selects the
/// tool-registry branch (`build_registry_for_agent`) and drives
/// `ASSISTANT_ALLOWED_DELEGATE_ROLES` — so reading it here adds no new
/// meaning to an already-overloaded field, it reuses the meaning it has.
/// What: exact match against `ASSISTANT_TIER_ROLE`. Every other role —
/// `engineer`, `qa`, `researcher`, `documentation`, `ops`, `planner`
/// (sub-agents), and `orchestrator`/`controller` (`pm`/`ctrl`, outside this
/// model entirely) — is not the assistant kind.
/// Test: `assistant_kind_is_read_from_role_not_tier`.
pub(crate) fn is_assistant_kind(role: &str) -> bool {
    role == ASSISTANT_TIER_ROLE
}

/// Does the KIND rule refuse a delegation edge from `source_role` to
/// `target_role`? (ADR-0024 predicates 1 + 2.)
///
/// Why: "assistants communicate with each other, but never delegate" (owner,
/// 2026-07-28). Expressed on the edge rather than on ranks, this is the ONLY
/// formulation that survives the tier inversion: it refuses an assistant peer
/// edge whether both endpoints are L1 (today), both L0 (after the inversion),
/// or split across a renumbering nobody has proposed yet, because it never
/// reads a tier at all.
/// What: `true` — refuse — exactly when BOTH endpoints are the assistant kind.
/// A non-assistant SOURCE is outside this rule's population entirely, which is
/// deliberate and is ADR-0024 predicate 1's explicit scope caveat: `pm`/`ctrl`
/// -as-orchestrator (role `orchestrator`/`controller`) delegate today through
/// separately-trusted, pre-existing paths this rule does not revisit, so their
/// edges are never refused here. A non-assistant TARGET (a sub-agent) is the
/// permitted case predicate 2 names. This function does NOT evaluate the tier
/// gate — that stays where it is, as an independently-computed
/// defense-in-depth layer over a DIFFERENT config field, so a defect in one
/// layer does not open the graph.
/// Test: `kind_refuses_assistant_to_assistant`,
/// `kind_permits_assistant_to_sub_agent`, `kind_ignores_non_assistant_sources`.
pub(crate) fn kind_refuses_delegation(source_role: &str, target_role: &str) -> bool {
    is_assistant_kind(source_role) && is_assistant_kind(target_role)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The discriminator is `role`, and nothing about it is derivable from a
    /// tier value — the property that makes every caller of this module
    /// renumbering-proof.
    #[test]
    fn assistant_kind_is_read_from_role_not_tier() {
        assert!(is_assistant_kind("assistant"));
        for sub_agent in [
            "engineer",
            "qa",
            "researcher",
            "documentation",
            "ops",
            "planner",
        ] {
            assert!(!is_assistant_kind(sub_agent), "{sub_agent} is a sub-agent");
        }
        for orchestrator in ["orchestrator", "controller"] {
            assert!(
                !is_assistant_kind(orchestrator),
                "{orchestrator} is outside the assistant/sub-agent model"
            );
        }
    }

    /// ADR-0024 predicate 2, the peer prohibition.
    #[test]
    fn kind_refuses_assistant_to_assistant() {
        assert!(kind_refuses_delegation("assistant", "assistant"));
    }

    /// The permitted edge: assistant -> sub-agent.
    #[test]
    fn kind_permits_assistant_to_sub_agent() {
        for sub_agent in [
            "engineer",
            "qa",
            "researcher",
            "documentation",
            "ops",
            "planner",
        ] {
            assert!(
                !kind_refuses_delegation("assistant", sub_agent),
                "assistant -> {sub_agent} is the whole point of delegation"
            );
        }
    }

    /// Predicate 1's scope caveat: `pm`/`ctrl` are not governed by this rule
    /// and must not be newly blocked by it.
    #[test]
    fn kind_ignores_non_assistant_sources() {
        for source in ["orchestrator", "controller", "engineer"] {
            assert!(!kind_refuses_delegation(source, "assistant"));
            assert!(!kind_refuses_delegation(source, "engineer"));
        }
    }
}
