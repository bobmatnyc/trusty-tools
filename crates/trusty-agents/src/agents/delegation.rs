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
//! ADR-0024 decision 4 (RATIFIED 2026-07-29) adds a SECOND, independent
//! narrowing to the same edge: the reachable sub-agent set is an editable
//! per-agent configuration whitelist, bounded by a server-owned floor
//! ([`ASSISTANT_REACHABLE_SUBAGENTS`]). The two rules are deliberately NOT
//! collapsed — the kind predicate is a property of the CODE and must keep
//! refusing a peer edge even if a whitelist is misconfigured to name an
//! assistant (the ADR's own conformance checklist says so in as many words),
//! while the whitelist is data an operator curates. Reachability is the
//! conjunction: `!(kind_blocked || tier_blocked || !whitelisted)`.
//!
//! What: [`is_assistant_kind`] and [`kind_refuses_delegation`] — the ONE
//! definition of the kind predicate in this crate — plus
//! [`ASSISTANT_REACHABLE_SUBAGENTS`], the floor decision 4's whitelist narrows.
//! Both the enforcement point
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

/// The server-owned FLOOR of in-process sub-agent NAMES an assistant-kind
/// delegator may ever reach — the ceiling ADR-0024 decision 4's editable
/// whitelist narrows (owner ratification, 2026-07-29).
///
/// Why: the owner's concrete goal for decision 4 is that an assistant persona's
/// reachable sub-agent set becomes exactly `{research, ticketing}` — NO coding
/// agents. Decision 4 also says the set must be "an editable configuration
/// whitelist … not a hand-authored Rust constant", and this constant is NOT
/// that set: it is the FLOOR the editable set is bounded by, the exact
/// counterpart of `tools::cross_product::NON_CODING_TARGETS` for the
/// out-of-process mechanism. Two layers, not one — a config (or a GUI PATCH)
/// that names `engineer` must be refused by CODE, not merely absent from a
/// curated list, which is the ADR's ratified sub-answer (b) ("the write path
/// MUST enforce a server-side floor"). A floor is also what makes the
/// whitelist safe to expose for editing at all.
/// What: NAMES, not roles — this is the vocabulary `delegate_to_agent`'s
/// `agent_name` parameter actually takes and `AgentConfig::by_name_in`
/// resolves, so the whitelist is checked against the same string the caller
/// supplies. `research-agent` (role `researcher`) and `ticketing-agent` (role
/// `ticketing`) are the two bundled non-coding specialists; they are the
/// in-process spellings of the same two capabilities the bridge floor names
/// `research`/`ticketing`. Nothing in the roster is REMOVED to achieve this —
/// every agent definition stays in the catalog, only reachability changes.
/// Deliberately NOT derived from the role allowlist: role eligibility is a
/// coarse pre-filter (`ASSISTANT_ALLOWED_DELEGATE_ROLES`), and deriving one
/// from the other would collapse two independent gates into one.
/// Test: `delegate_floor_rejects_an_engineer_even_when_config_allows_it`,
/// `the_two_floors_share_no_name`,
/// `assistant_reachable_floor_names_resolve_in_the_bundled_roster`.
pub(crate) const ASSISTANT_REACHABLE_SUBAGENTS: &[&str] = &["research-agent", "ticketing-agent"];

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

/// The roles that name the ORCHESTRATOR kind — `pm` and a `ctrl` configured
/// as a controller rather than as a persona.
///
/// Why: these two spellings were previously scattered across prose ("`pm`/
/// `ctrl`-as-orchestrator") and encoded at dispatch as a hardcoded
/// `role != "assistant"` fallthrough, never as a named value. Naming them
/// makes the L0 population a reviewable list instead of the complement of a
/// single equality test. `controller` is included because `ctrl.toml` may
/// declare either spelling (`ctrl::config`'s loader accepts `"controller" |
/// "ctrl"`), and a rule that recognized only one would derive a different
/// tier for the same agent depending on which word its TOML happened to use.
/// What: exactly two values, matched case-sensitively and exactly, mirroring
/// [`is_assistant_kind`]'s discipline — a near-miss like `Orchestrator` is
/// NOT the orchestrator kind and fails closed to
/// [`crate::agents::AgentTier::L1Standard`].
/// Test: `orchestrator_kind_is_exactly_the_two_declared_roles`.
pub(crate) const ORCHESTRATOR_TIER_ROLES: &[&str] = &["orchestrator", "controller"];

/// Is `role` the orchestrator KIND?
///
/// Why: `pm` reached [`crate::agents::AgentTier::L0Orchestration`] by a
/// hardcoded special case at ONE dispatch call site
/// (`ctrl::pm_task::dispatch::history::ctrl_delegate_posture`) while every
/// assistant reached the same tier through the role-derived rule
/// ([`crate::agents::AgentTier::for_kind`]) — two mechanisms for one fact, so
/// "is this agent L0?" answered differently depending on which one a reader
/// or a new call site consulted. This predicate is the orchestrator half of
/// the ONE rule, living beside [`is_assistant_kind`] so both halves of the L0
/// population are defined in the same module.
/// What: membership in [`ORCHESTRATOR_TIER_ROLES`]. Being the orchestrator
/// kind means only that the agent sits OUTSIDE the L0/L1 persona tier model
/// and is not narrowed by it — it does NOT make an orchestrator an assistant:
/// [`kind_refuses_delegation`] still ignores it as a source (predicate 1's
/// scope caveat), and `ASSISTANT_ALLOWED_DELEGATE_ROLES` still excludes it as
/// a target.
/// Test: `orchestrator_kind_is_exactly_the_two_declared_roles`,
/// `assistant_and_orchestrator_kinds_are_disjoint`.
pub(crate) fn is_orchestrator_kind(role: &str) -> bool {
    ORCHESTRATOR_TIER_ROLES.contains(&role)
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

    /// The orchestrator kind is a NAMED list, not "everything that is not an
    /// assistant" — a sub-agent role must never fall into it, because that is
    /// the exact widening the L0 derivation must not acquire.
    #[test]
    fn orchestrator_kind_is_exactly_the_two_declared_roles() {
        for orchestrator in ORCHESTRATOR_TIER_ROLES {
            assert!(is_orchestrator_kind(orchestrator));
        }
        for other in [
            "",
            "assistant",
            "engineer",
            "qa",
            "researcher",
            "documentation",
            "ops",
            "planner",
            "ticketing",
            "analysis",
            "observer",
            // Near-misses fail closed, exactly as the assistant predicate does.
            "Orchestrator",
            "orchestrator-tier",
            "ctrl",
        ] {
            assert!(
                !is_orchestrator_kind(other),
                "role {other:?} must NOT be the orchestrator kind"
            );
        }
    }

    /// The two L0 kinds are disjoint: an orchestrator is L0 because it sits
    /// outside the persona tier model, NOT because it is an assistant — so it
    /// must never pick up the assistant-only peer prohibition or the
    /// assistant-only role allowlist by way of the shared tier value.
    #[test]
    fn assistant_and_orchestrator_kinds_are_disjoint() {
        for orchestrator in ORCHESTRATOR_TIER_ROLES {
            assert!(!is_assistant_kind(orchestrator));
        }
        assert!(!is_orchestrator_kind(ASSISTANT_TIER_ROLE));
    }
}
