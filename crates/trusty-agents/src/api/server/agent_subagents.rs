//! `GET /api/agents/:name/subagents` — the Sub-agents configuration section
//! (#4029, epic #4021 OQ-5): the UNION of the two delegation mechanisms an
//! agent may reach, each labelled by kind, never flattened together.
//!
//! Why: OQ-5 was resolved by the owner on 2026-07-26 — *"Sub-agents is its own
//! configuration section"* — and on 2026-07-27 the owner further required that
//! the section show BOTH delegation mechanisms, which the specs had never
//! reconciled. Neither was exposed over any API route: `parse_agent_toml`
//! (`super::projects`) enumerates twelve `[agent]`/`[tools]` fields and
//! `subagents` is not among them, so a Svelte pane alone could only have
//! guessed. The two mechanisms are genuinely different things with different
//! enforcement points, different failure polarities and a non-overlapping
//! target vocabulary, so the payload keeps them apart:
//!
//! - **`in_product`** — `delegate_to_agent` (`crate::tools::delegate`),
//!   trusty-agents → trusty-agents, in-process peer/worker delegation. Its
//!   target set is NOT per-agent config at all: it is the hardcoded
//!   `runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES` role allowlist
//!   applied to whatever resolves out of `agents::agents_dir_candidates()`,
//!   further narrowed by #4169's one-directional L0/L1 tier gate. The role
//!   `documentation` exists ONLY here — it is in no other allow-set in the
//!   codebase.
//! - **`cross_product`** — `dispatch_task` (`crate::tools::pm_bridge`),
//!   trusty-agents → trusty-code, out-of-process specialist dispatch. This one
//!   DOES have a per-agent TOML binding: the optional `[subagents]` section
//!   (`agents::config::SubagentsConfig`), intersected with the bridge's own
//!   `NON_CODING_TARGETS` floor by `SubagentAllowSet` — fail-closed per OQ-7,
//!   with an absent section granting NOTHING.
//!
//! **Honesty rules this route holds to.** Every one exists because the
//! alternative is a config pane that advertises a capability the enforcement
//! layer refuses:
//!
//! - **No second copy of any gate.** Reachability is computed by calling the
//!   SAME code the enforcement points call: `SubagentAllowSet::resolve` for the
//!   cross-product half (so the pane can never disagree with the bridge — the
//!   OQ-7 requirement stated in #4029), and, for the in-product half, the same
//!   `AgentConfig::by_name_in` resolution + `ASSISTANT_ALLOWED_DELEGATE_ROLES`
//!   membership + `AgentInfo::tier()` comparison that
//!   `DelegateToAgentTool::execute`'s pre-flight check performs, in that order.
//! - **The tier gate is reflected, not re-litigated (#4169, epic #4167).** A
//!   target that resolves to `AgentTier::L0Orchestration` is reported
//!   `reachable: false` for any delegator that is not itself L0. This route
//!   changes no gate and grants nothing; it only refuses to advertise what the
//!   gate would refuse.
//! - **Registered ≠ granted.** `delegate_to_agent` is only registered for
//!   `role == ASSISTANT_TIER_ROLE` (`build_registry_for_agent`), and even then
//!   dispatch is gated by the agent's own resolved tool patterns. Both facts
//!   are reported separately (`tool_registered` / `tool_granted`) rather than
//!   collapsed into one green tick.
//! - **Fail-closed on unresolvable config.** When the agent's own config cannot
//!   be resolved the response is `200` with `resolved: false`, a `config_error`
//!   and EMPTY target lists — never a guess assembled from a partial parse. An
//!   agent whose config does not resolve does not run, so "it may delegate to
//!   nothing" is the truthful answer as well as the safe one.
//! - **The roster is not dumped.** Only role-ELIGIBLE agents become target
//!   cards; everything else is counted (`role_excluded_count`), never named,
//!   preserving the #3052 PR A CRITICAL-2 posture that the delegation surface
//!   does not enumerate internal agent ids.
//!
//! What: [`agent_subagents_route`] is the axum shim; [`subagents_at`] is the
//! testable core. Unlike its four sibling section routes this one resolves
//! through `AgentConfig::by_name_in` rather than a partial `toml::from_str`,
//! because the gate it must mirror does exactly that — which also means this
//! route DOES see `extends` (the documented gap in `/skills`, `/knowledge` and
//! `/permissions` is absent here).
//!
//! DOC-57 (`docs/specs/agent-config-five-sections.md`) still documents five
//! sections and is amended by #4182, deliberately NOT by this change.
//! Test: `super::tests::agent_subagents`.

use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use super::agent_patch::resolve_agent_paths;
use super::state::AppState;
use crate::agents::{AgentConfig, AgentTier};
use crate::ctrl::pm_task::match_any_glob;
use crate::runtime::tool_registry::{ASSISTANT_ALLOWED_DELEGATE_ROLES, ASSISTANT_TIER_ROLE};
use crate::skills::manifest::{SkillCatalog, effective_tool_patterns};
use crate::tools::cross_product::{NON_CODING_TARGETS, SubagentAllowSet, TargetDenied};

/// The in-product delegation tool this section reports on.
const DELEGATE_TOOL: &str = "delegate_to_agent";
/// The cross-product delegation tool this section reports on.
const DISPATCH_TOOL: &str = "dispatch_task";

/// `GET /api/agents/:name/subagents` — HTTP entry point.
///
/// Why/What: see the module doc. Resolution roots mirror the sibling section
/// routes (`agent_knowledge_route`, `agent_skills_route`): the agent search
/// path is `agents::agents_dir_candidates()` — the exact multi-tier list
/// `delegate_to_agent`'s pre-flight check and the real spawn both use — and the
/// skill-source root is the project CWD.
/// Test: `super::tests::agent_subagents::subagents_route_is_wired_into_router`.
pub(super) async fn agent_subagents_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    subagents_at(
        &crate::agents::agents_dir_candidates(),
        &name,
        &project_root,
    )
    .await
}

/// Core sub-agent resolution against explicit dirs/roots.
///
/// Why: Same testability rationale as `agent_knowledge::knowledge_at` — the
/// union logic, the tier interaction and the fail-closed cross-product default
/// must be exercisable against a `tempfile::TempDir` roster without an HTTP
/// server or the developer's real `~/.trusty-agents/agents/`.
/// What: `400` for an invalid name, `404` for an unknown agent. Anything else
/// is `200`: an unresolvable config degrades to `resolved: false` plus empty
/// target lists and a `config_error` (see the module doc's fail-closed rule),
/// never a `500` and never a partial-parse guess.
/// Test: `subagents_route_reports_both_mechanisms_for_an_assistant`,
/// `subagents_route_hides_l0_target_from_l1_delegator`,
/// `subagents_route_shows_l0_target_to_l0_delegator`,
/// `subagents_route_cross_product_denies_everything_without_the_section`,
/// `subagents_route_unknown_agent_404`,
/// `subagents_route_rejects_traversal_name`,
/// `subagents_route_degrades_on_malformed_toml`.
pub(super) async fn subagents_at(
    dirs: &[PathBuf],
    name: &str,
    project_root: &std::path::Path,
) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid agent name" })),
        )
            .into_response();
    }
    if resolve_agent_paths(dirs, name).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown agent", "name": name })),
        )
            .into_response();
    }

    // Fail-closed: the gate this route mirrors resolves through
    // `by_name_in`, so an agent this call cannot resolve is an agent that
    // cannot delegate anywhere (it cannot even run). Reporting empty sets plus
    // the parse error is both the honest and the safe answer — see the module
    // doc. Deliberately NOT a partial `toml::from_str` fallback: reading
    // `[subagents]` out of a file whose `[agent]` table failed to parse would
    // report grants against an identity we could not establish.
    let cfg = match AgentConfig::by_name_in(dirs, name) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(agent = name, error = %format!("{e:#}"), "subagents_at: config did not resolve");
            return (
                StatusCode::OK,
                Json(json!({
                    "resolved": false,
                    "in_product": empty_in_product(),
                    "cross_product": empty_cross_product(),
                    "config_error": format!("{e:#}"),
                })),
            )
                .into_response();
        }
    };

    // The agent's resolved tool-pattern surface, computed exactly as the
    // Knowledge/Skills panes compute it (`[tools].allow` ∪ expanded
    // `[skills].allow`), so "is this delegation tool actually granted" is
    // answered by the same matcher the dispatch gate uses rather than by
    // eyeballing the allow-list.
    let catalog =
        SkillCatalog::builtin().with_authored(crate::skills::manifest::authored::load_from_paths(
            &crate::skills::sources::SkillSourceRegistry::load(project_root).resolved_paths(),
        ));
    let (patterns, _unresolved) = effective_tool_patterns(
        cfg.tools.allow.as_ref(),
        cfg.skills.allow.as_ref(),
        &catalog,
    );
    let grants = |tool: &str| patterns.as_deref().is_some_and(|p| match_any_glob(tool, p));

    let in_product = in_product_surface(dirs, name, &cfg, grants(DELEGATE_TOOL)).await;
    let cross_product =
        cross_product_surface(cfg.subagents.allowed.as_deref(), grants(DISPATCH_TOOL));

    (
        StatusCode::OK,
        Json(json!({
            "resolved": true,
            "in_product": in_product,
            "cross_product": cross_product,
        })),
    )
        .into_response()
}

/// The `in_product` half: `delegate_to_agent`'s reachable target set for this
/// agent, reflecting BOTH the role allowlist and #4169's tier gate.
///
/// Why: this mechanism has no per-agent config to read, so the only honest
/// source is the gate's own inputs. Every candidate is resolved with
/// `AgentConfig::by_name_in` — the identical call
/// `DelegateToAgentTool::execute` makes — and then subjected to the identical
/// two checks in the identical order: `ASSISTANT_ALLOWED_DELEGATE_ROLES`
/// membership on the target's resolved `role`, and the tier comparison
/// (`target == L0Orchestration && delegator != L0Orchestration` ⇒ refused).
/// A candidate that fails to resolve is reported as such rather than dropped,
/// because the gate refuses those too (`Err` ⇒ `rejected`) and a target that
/// silently vanished from the pane looks identical to one that was never
/// declared.
/// What: `{mechanism, tool, tool_registered, tool_granted, delegator_tier,
/// allowed_roles, targets, role_excluded_count, unresolved}`. `targets` carries
/// only role-eligible agents (see the module doc's no-roster-dump rule), each
/// with `reachable` plus a `reason` when refused. The agent itself is excluded:
/// self-delegation is not a configuration surface.
/// Test: `subagents_route_reports_both_mechanisms_for_an_assistant`,
/// `subagents_route_hides_l0_target_from_l1_delegator`,
/// `subagents_route_shows_l0_target_to_l0_delegator`,
/// `subagents_route_reports_tool_not_registered_for_a_worker_role`,
/// `subagents_route_reports_unresolvable_target_rather_than_dropping_it`.
async fn in_product_surface(
    dirs: &[PathBuf],
    self_name: &str,
    cfg: &AgentConfig,
    tool_granted: bool,
) -> Value {
    let delegator_tier = cfg.agent.tier();
    let mut targets: Vec<Value> = Vec::new();
    let mut unresolved: Vec<Value> = Vec::new();
    let mut role_excluded_count: usize = 0;

    for entry in super::projects::scan_agent_catalog(dirs).await {
        let Some(candidate) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        if candidate.eq_ignore_ascii_case(self_name) {
            continue;
        }
        match AgentConfig::by_name_in(dirs, candidate) {
            Ok(target) => {
                if !ASSISTANT_ALLOWED_DELEGATE_ROLES.contains(&target.agent.role.as_str()) {
                    role_excluded_count += 1;
                    continue;
                }
                let target_tier = target.agent.tier();
                let tier_blocked = target_tier == AgentTier::L0Orchestration
                    && delegator_tier != AgentTier::L0Orchestration;
                // ADR-0023: the KIND predicate the gate now enforces, read
                // from the SAME function `DelegateToAgentTool::execute` calls
                // — not a second copy of the comparison. Without this the
                // pane would advertise every peer assistant as reachable
                // while the gate refuses it, which is the drift this module's
                // doc exists to forbid. Checked FIRST, matching the order
                // `execute` reports refusals in.
                let kind_blocked = crate::agents::delegation::kind_refuses_delegation(
                    &cfg.agent.role,
                    &target.agent.role,
                );
                targets.push(json!({
                    "name": target.agent.name,
                    "display_name": target.agent.display_label(),
                    "role": target.agent.role,
                    "tier": target_tier.wire_label(),
                    "reachable": !(kind_blocked || tier_blocked),
                    "reason": if kind_blocked {
                        Some(
                            "refused by the delegation kind rule: this target is a peer \
                             assistant, and assistants communicate with each other rather \
                             than delegating to one another (ADR-0023). Delegation targets \
                             are sub-agents (specialists)."
                                .to_string(),
                        )
                    } else if tier_blocked {
                        Some(format!(
                            "refused by the L0/L1 delegation gate: this agent is tier \
                             {} and may not delegate into an L0-orchestration target \
                             (#4169) — privilege cannot be acquired via delegation",
                            delegator_tier.wire_label(),
                        ))
                    } else {
                        None
                    },
                }));
            }
            Err(e) => unresolved.push(json!({
                "name": candidate,
                "reason": format!(
                    "config did not resolve, so the delegation pre-flight check \
                     refuses it: {e:#}"
                ),
            })),
        }
    }
    targets.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["name"].as_str().unwrap_or_default())
    });

    json!({
        "mechanism": "in_product",
        "tool": DELEGATE_TOOL,
        // `build_registry_for_agent` routes ONLY `role == "assistant"` into
        // `build_assistant_tier_registry`, the one branch that registers
        // `delegate_to_agent` with the role allowlist and the tier gate. A
        // worker-role agent never gets the tool at all, so reporting its
        // theoretical targets as reachable would be a fabrication.
        "tool_registered": cfg.agent.role == ASSISTANT_TIER_ROLE,
        "tool_granted": tool_granted,
        "delegator_tier": delegator_tier.wire_label(),
        "allowed_roles": ASSISTANT_ALLOWED_DELEGATE_ROLES,
        "targets": targets,
        "role_excluded_count": role_excluded_count,
        "unresolved": unresolved,
    })
}

/// The `cross_product` half: `dispatch_task`'s reachable specialist set,
/// resolved through the bridge's OWN allow-set (OQ-7).
///
/// Why: #4029's test requirement is explicit — the pane must show "exactly the
/// cross-product targets it is permitted to call, sourced from the same
/// allow-set the bridge enforces … never a client-side list that can drift from
/// what the bridge actually allows." So this calls
/// [`SubagentAllowSet::resolve`] itself rather than re-implementing the
/// intersection. Every floor target is listed with its grant state (the
/// `/skills` precedent: report the whole vocabulary, not only the granted
/// subset) so a denied specialist carries a reason instead of being invisible.
/// What: `{mechanism, tool, tool_granted, declares_allowed, bridge_floor,
/// targets, rejected}`. `declares_allowed` distinguishes "no `[subagents]`
/// section" from "an empty one" for display purposes only — both grant nothing
/// (`SubagentsConfig`'s doc comment makes that non-load-bearing). `rejected`
/// carries declared names the bridge floor refuses, which is the one way a
/// hand-written `[subagents].allowed` can silently do nothing.
/// The #4169 tier gate is deliberately NOT applied here: it lives in
/// `delegate_to_agent`, has no counterpart in the tcode bridge, and inventing
/// one on this read path would report a restriction nothing enforces.
///
/// Takes the declared list rather than the whole `AgentConfig` so the union
/// logic is unit-testable without constructing a full config (which requires
/// `[agent]`/`[llm]`/`[system_prompt]` tables that have nothing to do with
/// delegation).
/// Test: `cross_product_surface_denies_everything_when_nothing_is_declared`,
/// `cross_product_surface_grants_only_the_declared_floor_target`,
/// `cross_product_surface_rejects_a_declared_coding_target`,
/// `subagents_route_cross_product_denies_everything_without_the_section`,
/// `subagents_route_cross_product_grants_a_declared_floor_target`.
fn cross_product_surface(declared: Option<&[String]>, tool_granted: bool) -> Value {
    let allow_set = SubagentAllowSet::from_allowed(declared);

    let targets: Vec<Value> = NON_CODING_TARGETS
        .iter()
        .map(|floor_name| {
            let outcome = allow_set.resolve(floor_name);
            json!({
                "name": floor_name,
                "granted": outcome.is_ok(),
                "reason": match &outcome {
                    Ok(_) => None,
                    // The bridge's own `TargetDenied: Display` is deliberately
                    // generic ("not available to this agent") so a black-boxed
                    // persona cannot probe the floor list. This is an
                    // operator-facing config pane that already reports
                    // `bridge_floor` in full, so it states the actionable
                    // reason instead — the DECISION still comes from
                    // `resolve()` above, only the wording is local.
                    Err(TargetDenied::NotGranted(_)) => Some(
                        "not listed in this agent's [subagents].allowed — absent or \
                         unlisted denies (OQ-7, fail-closed)"
                            .to_string(),
                    ),
                    Err(other) => Some(other.to_string()),
                },
            })
        })
        .collect();

    let rejected: Vec<Value> = declared
        .unwrap_or_default()
        .iter()
        .filter_map(|raw| {
            let normalized = raw.trim().to_ascii_lowercase();
            match allow_set.resolve(raw) {
                Ok(_) => None,
                // `NotGranted` is unreachable for a name this agent itself
                // declared, so anything left is the floor refusing it.
                Err(TargetDenied::Blank) => Some(json!({
                    "name": raw,
                    "reason": TargetDenied::Blank.to_string(),
                })),
                Err(_) => Some(json!({
                    "name": normalized,
                    "reason": "not a permitted non-coding specialist — the bridge floor \
                               (NON_CODING_TARGETS) hard-denies it regardless of this \
                               declaration (OQ-7)",
                })),
            }
        })
        .collect();

    json!({
        "mechanism": "cross_product",
        "tool": DISPATCH_TOOL,
        "tool_granted": tool_granted,
        "declares_allowed": declared.is_some(),
        "bridge_floor": NON_CODING_TARGETS,
        "targets": targets,
        "rejected": rejected,
    })
}

/// The `in_product` payload for an agent whose config did not resolve — empty,
/// with the mechanism vocabulary still present so the pane renders its shell.
fn empty_in_product() -> Value {
    json!({
        "mechanism": "in_product",
        "tool": DELEGATE_TOOL,
        "tool_registered": false,
        "tool_granted": false,
        "delegator_tier": AgentTier::default().wire_label(),
        "allowed_roles": ASSISTANT_ALLOWED_DELEGATE_ROLES,
        "targets": Vec::<Value>::new(),
        "role_excluded_count": 0,
        "unresolved": Vec::<Value>::new(),
    })
}

/// The `cross_product` payload for an agent whose config did not resolve —
/// every floor target denied, matching the fail-closed default.
fn empty_cross_product() -> Value {
    json!({
        "mechanism": "cross_product",
        "tool": DISPATCH_TOOL,
        "tool_granted": false,
        "declares_allowed": false,
        "bridge_floor": NON_CODING_TARGETS,
        "targets": NON_CODING_TARGETS
            .iter()
            .map(|n| json!({
                "name": n,
                "granted": false,
                "reason": "this agent's config could not be resolved, so no grant could \
                           be read (fail-closed)",
            }))
            .collect::<Vec<Value>>(),
        "rejected": Vec::<Value>::new(),
    })
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// The empty/degraded payloads must agree with the real ones on the
    /// mechanism vocabulary — a pane keying off `mechanism`/`bridge_floor`
    /// would otherwise blank out on a config error instead of rendering the
    /// denial.
    #[test]
    fn empty_payloads_carry_the_same_vocabulary_and_deny_everything() {
        let ip = empty_in_product();
        assert_eq!(ip["mechanism"], "in_product");
        assert_eq!(ip["tool"], DELEGATE_TOOL);
        assert_eq!(ip["tool_registered"], false);
        assert_eq!(ip["delegator_tier"], "l1", "fail-closed to the L1 default");
        assert!(ip["targets"].as_array().unwrap().is_empty());

        let cp = empty_cross_product();
        assert_eq!(cp["mechanism"], "cross_product");
        assert_eq!(cp["tool"], DISPATCH_TOOL);
        let targets = cp["targets"].as_array().unwrap();
        assert_eq!(targets.len(), NON_CODING_TARGETS.len());
        assert!(targets.iter().all(|t| t["granted"] == false));
    }

    /// `cross_product_surface` must derive its grant decision from
    /// `SubagentAllowSet::resolve`, so an absent `[subagents]` section denies
    /// every floor target — the OQ-7 deny-by-default posture, asserted at the
    /// payload level rather than only inside the bridge's own tests.
    #[test]
    fn cross_product_surface_denies_everything_when_nothing_is_declared() {
        let body = cross_product_surface(None, false);
        assert_eq!(body["declares_allowed"], false);
        assert_eq!(body["tool_granted"], false);
        let targets = body["targets"].as_array().unwrap();
        assert_eq!(targets.len(), NON_CODING_TARGETS.len());
        for t in targets {
            assert_eq!(t["granted"], false, "{t:?}");
            assert!(
                t["reason"].as_str().unwrap().contains("fail-closed"),
                "{t:?}"
            );
        }
        assert!(body["rejected"].as_array().unwrap().is_empty());
    }

    /// A declared floor target is granted; the OTHER floor target is not — the
    /// intersection must be per-name, not "declares anything ⇒ grants all".
    #[test]
    fn cross_product_surface_grants_only_the_declared_floor_target() {
        let declared = vec!["research".to_string()];
        let body = cross_product_surface(Some(&declared), true);
        assert_eq!(body["declares_allowed"], true);
        assert_eq!(body["tool_granted"], true);
        let targets = body["targets"].as_array().unwrap();
        let research = targets.iter().find(|t| t["name"] == "research").unwrap();
        assert_eq!(research["granted"], true);
        assert_eq!(research["reason"], Value::Null);
        let ticketing = targets.iter().find(|t| t["name"] == "ticketing").unwrap();
        assert_eq!(ticketing["granted"], false);
        assert!(
            ticketing["reason"]
                .as_str()
                .unwrap()
                .contains("[subagents].allowed")
        );
    }

    /// A permissive config naming a CODING target must never widen the floor —
    /// the same invariant `non_coding_floor_rejects_a_coding_target_even_when_config_allows_it`
    /// pins inside the bridge, asserted here at the surface that reports it.
    #[test]
    fn cross_product_surface_rejects_a_declared_coding_target() {
        let declared = vec!["engineer".to_string(), "research".to_string()];
        let body = cross_product_surface(Some(&declared), true);
        let targets = body["targets"].as_array().unwrap();
        assert!(
            targets.iter().all(|t| t["name"] != "engineer"),
            "a non-floor name must never become a target card: {targets:?}"
        );
        let rejected = body["rejected"].as_array().unwrap();
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert_eq!(rejected[0]["name"], "engineer");
        assert!(
            rejected[0]["reason"]
                .as_str()
                .unwrap()
                .contains("NON_CODING_TARGETS")
        );
    }

    /// `documentation` is in `ASSISTANT_ALLOWED_DELEGATE_ROLES` and in NO other
    /// allow-set in the codebase — in particular it is not a cross-product
    /// target. If the two halves were ever flattened this assertion is the one
    /// that breaks, which is why the payload keeps them apart.
    #[test]
    fn the_two_mechanisms_have_a_disjoint_target_vocabulary() {
        assert!(ASSISTANT_ALLOWED_DELEGATE_ROLES.contains(&"documentation"));
        assert!(!NON_CODING_TARGETS.contains(&"documentation"));
        for floor in NON_CODING_TARGETS {
            assert!(
                !ASSISTANT_ALLOWED_DELEGATE_ROLES.contains(floor),
                "{floor} appears in both allow-sets; the payload's kind labelling \
                 would become ambiguous"
            );
        }
    }
}
