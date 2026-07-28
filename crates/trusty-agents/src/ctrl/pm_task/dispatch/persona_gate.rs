//! Pure per-turn gating helpers for the persona-chat dispatch path.
//!
//! Why: `persona.rs` sits exactly at the 500-SLOC production cap enforced by
//! `scripts/check_line_cap.sh`, so #4171 could not add its tier gate there
//! without a split. These four items were the natural extraction: they are
//! the only PURE functions in that file — no I/O, no LLM, no registry
//! construction — and they are precisely the ones the unit tests exercise, so
//! moving them puts the testable logic in one place and leaves `persona.rs`
//! as the assembly point it is. Moved VERBATIM (same names, same signatures,
//! same behavior); `persona_tests.rs` reaches them unchanged through its
//! `use super::*`, because `persona.rs` re-imports them.
//! What: `filter_persona_tool_names` (allow-glob + RBAC-tier + scope gate),
//! `filter_persona_tool_names_for_tier` (that gate plus #4171's L0-only
//! session-state strip), `persona_max_turns`, and `persona_allowed_tools`.
//! Test: `persona_tests.rs` — `filter_persona_tool_names_*`,
//! `persona_max_turns_*`, `persona_allowed_tools_*`; #4171's own coverage
//! lives in `tools::session_state::tests`.

use crate::tools::registry::scope::{Scope, ScopePattern, agent_can_use};

use super::super::helpers::match_any_glob;

/// Narrow a persona's full candidate tool-name list down to what it may
/// actually reach on this turn (#3285).
///
/// Why: `run_pm_task_with_persona` now registers the FULL delegation/CTRL/
/// filesystem/search/shell/MCP tool surface into every persona's registry —
/// tool parity with the session path. Without a second, independent gate a
/// persona would suddenly gain every tool the PM has, regardless of what its
/// TOML declares. This function is that gate: it is the single place that
/// decides which of the registered tools are actually advertised to the LLM,
/// so it is pulled out as a pure function precisely so the allow/scope
/// security property can be pinned by fast unit tests instead of only being
/// exercised end-to-end.
/// What: Keeps a name only when it matches at least one glob in `patterns`
/// (a persona's `[tools].allow` list — see `match_any_glob`) AND is present
/// in `allowed_by_tier` (the RBAC-tier filter derived from
/// `registry.filter_tools_for_user`) AND, for tools that declare an OpenRPC
/// scope (#3208; see `tool_scopes`), that scope is covered by at least one
/// of the persona's own declared `[tools].scopes` patterns
/// (`agent_scope_patterns`). A tool absent from `tool_scopes` (the trait
/// default — every in-process tool) is not part of the scoped surface and
/// passes this third gate unconditionally; a tool that DOES declare a scope
/// is denied by default when `agent_scope_patterns` is empty (fail closed —
/// `agent_can_use` denies all on an empty pattern list). Order-preserving;
/// all three gates must pass independently, mirroring how
/// `registry`+`allowed_tools` combine in `chat_with_tools_gated`.
/// Test: `filter_persona_tool_names_respects_allow_globs`,
/// `filter_persona_tool_names_new_delegation_tools_require_explicit_allow`,
/// `filter_persona_tool_names_delegation_tools_surface_when_allowed`,
/// `filter_persona_tool_names_respects_rbac_tier`,
/// `filter_persona_tool_names_respects_declared_scopes`,
/// `filter_persona_tool_names_denies_scoped_tool_when_agent_declares_no_scopes`.
pub(super) fn filter_persona_tool_names(
    all_names: Vec<String>,
    patterns: &[String],
    allowed_by_tier: &std::collections::HashSet<String>,
    tool_scopes: &std::collections::HashMap<String, String>,
    agent_scope_patterns: &[ScopePattern],
) -> Vec<String> {
    all_names
        .into_iter()
        .filter(|name| {
            match_any_glob(name, patterns)
                && allowed_by_tier.contains(name)
                && match tool_scopes.get(name) {
                    Some(scope) => agent_can_use(agent_scope_patterns, &Scope::new(scope.clone())),
                    None => true,
                }
        })
        .collect()
}

/// Default `max_turns` ceiling for a tool-using persona chat turn when
/// `[llm].persona_max_turns` is not set in the agent's TOML.
///
/// Why: named separately from the literal so the demo-day rationale (raised
/// from a hardcoded 4 after a persona doing two sequential tool calls plus a
/// summary exhausted that budget) is documented once, at the definition, not
/// re-explained at every read site.
pub(super) const DEFAULT_PERSONA_TOOL_MAX_TURNS: u32 = 8;

/// Effective `max_turns` ceiling for a persona chat turn.
///
/// Why: `run_pm_task_with_persona` is a separate call site from the general
/// subagent tool loop (`agents::in_process_runner` et al., which already
/// honor `LlmParams::max_turns`) — it hardcoded its own ceiling (2 turns with
/// no tools, 4 with tools) since #254 and never consulted config at all. A
/// persona running a couple of sequential tool calls (e.g. grep twice, then
/// summarize) routinely exhausted the 4-turn budget and failed with
/// `chat_with_tools exceeded max_turns`. Pulled into its own function
/// (mirroring `filter_persona_tool_names` above) so the default-vs-override
/// behavior is unit-testable without a live LLM call.
/// What: No tools -> 2 (unchanged current behavior). With tools ->
/// `llm.persona_max_turns` when set, else [`DEFAULT_PERSONA_TOOL_MAX_TURNS`]
/// (8, raised from the prior hardcoded 4).
/// Test: `persona_max_turns_no_tools_is_two`,
/// `persona_max_turns_with_tools_defaults_to_eight`,
/// `persona_max_turns_with_tools_honors_config_override`.
pub(super) fn persona_max_turns(has_tools: bool, llm: &crate::agents::LlmParams) -> u32 {
    if !has_tools {
        return 2;
    }
    llm.persona_max_turns
        .unwrap_or(DEFAULT_PERSONA_TOOL_MAX_TURNS)
}

/// Compute the `allowed_tools` argument passed to `chat_with_tools_gated` for
/// a persona whose surface is gated by a declared `[tools].allow` list
/// (#3208 regression guard).
///
/// Why: `ToolRegistry::dispatch_gated`'s allowlist semantics treat `None` as
/// "no restriction — every registered tool is callable", which is CORRECT
/// for the coding-agent session path that never sets an allow list. A
/// persona reaching this helper, however, has ALWAYS opted into restriction
/// by declaring `[tools].allow` — the caller only reaches this branch in
/// that case (see the `if let Some(patterns) = persona_cfg.tools.allow`
/// split in `run_pm_task_with_persona`). If every candidate tool got
/// filtered away (an allow-glob that matches nothing, or an RBAC tier / scope
/// check that denies everything), collapsing that to `None` would silently
/// REOPEN the persona's full registered tool surface — delegate_to_agent,
/// run_bash, move_file, create_dir, and everything else registered above —
/// the exact opposite of "deny by default". Always returning `Some(names)`,
/// even when empty, keeps `dispatch_gated` denying every call in that case
/// (an empty `Some` list matches nothing).
/// What: Identity wrapper — deliberately never returns `None`.
/// Test: `persona_allowed_tools_denies_dispatch_when_empty`,
/// `persona_allowed_tools_permits_dispatch_when_non_empty`.
pub(super) fn persona_allowed_tools(names: Vec<String>) -> Option<Vec<String>> {
    Some(names)
}
/// `filter_persona_tool_names` plus the L0-only session-state strip (#4171,
/// epic #4167).
///
/// Why: the three gates in `filter_persona_tool_names` all answer "what did
/// this persona DECLARE?". #4171 needs a fourth that answers a different
/// question — "what is this persona's TIER allowed to hold at all?" — and it
/// must be the last word, because the trusty-mpm-proxied `session_list` /
/// `session_status` / `project_list` / `console_metrics` executors are
/// registered into EVERY persona's registry (`tools::mcp_service_tools`,
/// `tools::mcp_live`) and `system_status` natively, so until now the L1
/// black-box posture held them back only by their ABSENCE from each
/// persona's `[tools].allow` — a convention any personalization overlay could
/// union away. Wrapping rather than extending `filter_persona_tool_names`
/// keeps that function's signature (and its eight existing call sites in
/// `persona_tests.rs`) untouched while giving the composition one name the
/// dispatch path calls.
/// What: applies `filter_persona_tool_names`, then
/// `tools::session_state::retain_tier_permitted`, which is DENY-ONLY — it can
/// remove a name, never add one, so an L0 persona still gets exactly what it
/// declares. Fail-closed on an indeterminate tier: `AgentInfo::tier()`
/// (#4168/#4200) resolves an absent, blank or unrecognized `tier = …` to
/// `AgentTier::L1Standard`, which is the stripping arm.
/// Test: `l1_persona_declaring_session_state_tools_gets_none`,
/// `l0_persona_declaring_session_state_tools_gets_them`,
/// `indeterminate_tier_fails_closed_to_no_session_state_tools`,
/// `persona_tier_gate_strips_session_state_for_l1`,
/// `persona_tier_gate_keeps_session_state_for_l0`.
pub(super) fn filter_persona_tool_names_for_tier(
    all_names: Vec<String>,
    patterns: &[String],
    allowed_by_tier: &std::collections::HashSet<String>,
    tool_scopes: &std::collections::HashMap<String, String>,
    agent_scope_patterns: &[ScopePattern],
    tier: crate::agents::AgentTier,
) -> Vec<String> {
    crate::tools::session_state::retain_tier_permitted(
        filter_persona_tool_names(
            all_names,
            patterns,
            allowed_by_tier,
            tool_scopes,
            agent_scope_patterns,
        ),
        tier,
    )
}
